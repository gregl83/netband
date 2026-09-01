use std::fmt;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use thiserror::Error;
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::cli::ConsoleMode;
use crate::model::{
    EventKind, MeasurementEvent, Outcome, ProviderKind, sanitize_endpoint, sanitize_message,
    timestamp_text,
};

#[derive(Debug, Error)]
pub enum ConsoleRenderError {
    #[error("cannot serialize console JSON: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConsoleDiagnostic {
    QueueFull { dropped_events: u64 },
    WriterDisabled { reason: ConsoleFailure },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsoleFailure {
    Serialization,
    BrokenPipe,
    Write,
    Flush,
    ShutdownTimeout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConsoleStats {
    pub dropped_events: u64,
    pub disabled: bool,
}

pub trait ConsoleSink {
    fn offer(&self, event: &MeasurementEvent);
}

pub fn service_stdout() -> tokio::io::Stdout {
    tokio::io::stdout()
}

#[derive(Debug, Default, Clone, Copy)]
pub struct ConsoleOff;

impl ConsoleSink for ConsoleOff {
    fn offer(&self, _event: &MeasurementEvent) {}
}

struct SharedState {
    dropped_events: AtomicU64,
    disabled: AtomicBool,
    writer_failure_reported: AtomicBool,
}

pub struct Console {
    sender: Option<mpsc::Sender<MeasurementEvent>>,
    shared: Arc<SharedState>,
    worker: Option<JoinHandle<()>>,
    diagnostic: Arc<dyn Fn(ConsoleDiagnostic) + Send + Sync>,
}

impl fmt::Debug for Console {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Console")
            .field("enabled", &self.sender.is_some())
            .finish_non_exhaustive()
    }
}

impl Console {
    pub fn spawn<W, D>(mode: ConsoleMode, writer: W, capacity: usize, diagnostic: D) -> Self
    where
        W: AsyncWrite + Unpin + Send + 'static,
        D: Fn(ConsoleDiagnostic) + Send + Sync + 'static,
    {
        let shared = Arc::new(SharedState {
            dropped_events: AtomicU64::new(0),
            disabled: AtomicBool::new(false),
            writer_failure_reported: AtomicBool::new(false),
        });
        let diagnostic: Arc<dyn Fn(ConsoleDiagnostic) + Send + Sync> = Arc::new(diagnostic);
        if mode == ConsoleMode::Off {
            return Self {
                sender: None,
                shared,
                worker: None,
                diagnostic,
            };
        }

        let (sender, receiver) = mpsc::channel(capacity.max(1));
        let worker_shared = Arc::clone(&shared);
        let worker_diagnostic = Arc::clone(&diagnostic);
        let effective_mode = if mode == ConsoleMode::Jsonl {
            ConsoleMode::Jsonl
        } else {
            ConsoleMode::Human
        };
        let worker = tokio::spawn(run_worker(
            effective_mode,
            writer,
            receiver,
            worker_shared,
            worker_diagnostic,
        ));
        Self {
            sender: Some(sender),
            shared,
            worker: Some(worker),
            diagnostic,
        }
    }

    pub fn stdout(mode: ConsoleMode, capacity: usize) -> Self {
        Self::spawn(
            mode,
            tokio::io::stdout(),
            capacity,
            |diagnostic| match diagnostic {
                ConsoleDiagnostic::QueueFull { dropped_events } => {
                    tracing::warn!(
                        dropped_events,
                        "console queue full; measurement presentation dropped"
                    )
                }
                ConsoleDiagnostic::WriterDisabled { reason } => {
                    tracing::warn!(
                        ?reason,
                        "console writer disabled; durable journal remains active"
                    )
                }
            },
        )
    }

    pub async fn shutdown(mut self, timeout: Duration) -> ConsoleStats {
        self.sender.take();
        if let Some(mut worker) = self.worker.take()
            && tokio::time::timeout(timeout, &mut worker).await.is_err()
        {
            disable_writer(
                &self.shared,
                self.diagnostic.as_ref(),
                ConsoleFailure::ShutdownTimeout,
            );
            worker.abort();
            let _ = worker.await;
        }
        self.stats()
    }

    pub fn stats(&self) -> ConsoleStats {
        ConsoleStats {
            dropped_events: self.shared.dropped_events.load(Ordering::Relaxed),
            disabled: self.shared.disabled.load(Ordering::Relaxed),
        }
    }

    fn record_drop(&self) {
        let dropped_events = self.shared.dropped_events.fetch_add(1, Ordering::Relaxed) + 1;
        if dropped_events == 1 || dropped_events.is_power_of_two() {
            (self.diagnostic)(ConsoleDiagnostic::QueueFull { dropped_events });
        }
    }
}

impl ConsoleSink for Console {
    fn offer(&self, event: &MeasurementEvent) {
        let Some(sender) = self.sender.as_ref() else {
            return;
        };
        if self.shared.disabled.load(Ordering::Acquire) {
            self.shared.dropped_events.fetch_add(1, Ordering::Relaxed);
            return;
        }
        match sender.try_send(event.sanitized()) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => self.record_drop(),
            Err(mpsc::error::TrySendError::Closed(_)) => {
                self.shared.dropped_events.fetch_add(1, Ordering::Relaxed);
                disable_writer(
                    &self.shared,
                    self.diagnostic.as_ref(),
                    ConsoleFailure::Write,
                );
            }
        }
    }
}

async fn run_worker<W>(
    mode: ConsoleMode,
    mut writer: W,
    mut receiver: mpsc::Receiver<MeasurementEvent>,
    shared: Arc<SharedState>,
    diagnostic: Arc<dyn Fn(ConsoleDiagnostic) + Send + Sync>,
) where
    W: AsyncWrite + Unpin,
{
    while let Some(event) = receiver.recv().await {
        let rendered = match mode {
            ConsoleMode::Jsonl => render_jsonl(&event).map(Some),
            ConsoleMode::Human => Ok(human_line(&event)),
            ConsoleMode::Auto | ConsoleMode::Off => Ok(None),
        };
        let text = match rendered {
            Ok(Some(text)) => text,
            Ok(None) => continue,
            Err(_) => {
                disable_writer(&shared, diagnostic.as_ref(), ConsoleFailure::Serialization);
                return;
            }
        };
        if let Err(error) = writer.write_all(text.as_bytes()).await {
            let reason = if error.kind() == io::ErrorKind::BrokenPipe {
                ConsoleFailure::BrokenPipe
            } else {
                ConsoleFailure::Write
            };
            disable_writer(&shared, diagnostic.as_ref(), reason);
            return;
        }
        if writer.flush().await.is_err() {
            disable_writer(&shared, diagnostic.as_ref(), ConsoleFailure::Flush);
            return;
        }
    }
    if writer.flush().await.is_err() {
        disable_writer(&shared, diagnostic.as_ref(), ConsoleFailure::Flush);
    }
}

fn disable_writer(
    shared: &SharedState,
    diagnostic: &dyn Fn(ConsoleDiagnostic),
    reason: ConsoleFailure,
) {
    shared.disabled.store(true, Ordering::Release);
    if !shared.writer_failure_reported.swap(true, Ordering::AcqRel) {
        diagnostic(ConsoleDiagnostic::WriterDisabled { reason });
    }
}

pub fn render_jsonl(event: &MeasurementEvent) -> Result<String, ConsoleRenderError> {
    let mut line = serde_json::to_string(&event.sanitized())?;
    line.push('\n');
    Ok(line)
}

pub fn human_line(event: &MeasurementEvent) -> Option<String> {
    let event = event.sanitized();
    let timestamp = event.finished_at_utc.map(timestamp_text)?;
    let interface = event.interface.as_deref().unwrap_or("default-route");
    let reason = event
        .error_message
        .as_deref()
        .map(|message| format!(" reason=\"{}\"", quote_human(message)))
        .unwrap_or_default();
    match event.event_kind {
        EventKind::PingSummary => Some(format!(
            "{timestamp} ping interface={interface} target={} outcome={} rtt_ms={} loss_pct={}{}\n",
            event.target.as_deref().unwrap_or("-"),
            outcome_name(event.outcome),
            decimal_or_dash(event.rtt_ms),
            decimal_or_dash(event.packet_loss_pct),
            reason,
        )),
        EventKind::Bandwidth => Some(format!(
            "{timestamp} bandwidth interface={interface} provider={} server={} outcome={} download_mbps={} upload_mbps={}{}\n",
            event.provider_kind.map(provider_name).unwrap_or("-"),
            event
                .server
                .as_deref()
                .map(sanitize_endpoint)
                .unwrap_or_else(|| "-".to_owned()),
            outcome_name(event.outcome),
            decimal_or_dash(event.download_mbps),
            decimal_or_dash(event.upload_mbps),
            reason,
        )),
        EventKind::PingProbe | EventKind::RequestFailure | EventKind::Scheduler => None,
    }
}

fn quote_human(value: &str) -> String {
    sanitize_message(value)
        .replace(['\r', '\n'], " ")
        .replace('"', "\\\"")
}

fn decimal_or_dash(value: Option<f64>) -> String {
    value.map_or_else(|| "-".to_owned(), |value| value.to_string())
}

fn outcome_name(outcome: Outcome) -> &'static str {
    match outcome {
        Outcome::Success => "success",
        Outcome::Partial => "partial",
        Outcome::Timeout => "timeout",
        Outcome::Unreachable => "unreachable",
        Outcome::PermissionDenied => "permission_denied",
        Outcome::Cancelled => "cancelled",
        Outcome::Error => "error",
        Outcome::NoCapacity => "no_capacity",
        Outcome::RateLimited => "rate_limited",
        Outcome::Scheduled => "scheduled",
        Outcome::Rescheduled => "rescheduled",
        Outcome::Deferred => "deferred",
        Outcome::Suppressed => "suppressed",
        Outcome::Expired => "expired",
    }
}

fn provider_name(provider: ProviderKind) -> &'static str {
    match provider {
        ProviderKind::Mlab => "mlab",
        ProviderKind::Direct => "direct",
    }
}
