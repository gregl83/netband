use std::future::Future;
use std::net::IpAddr;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use clap::Parser;
use futures_util::{SinkExt, StreamExt};
use netband::cli::Cli;
use netband::config::{ResolveContext, resolve};
use netband::console::ConsoleOff;
use netband::health::HealthConfig;
use netband::journal::{JournalError, JournalSink, OutputCoordinator};
use netband::model::{EventKind, LoadPhase, MeasurementEvent, Outcome, TriggerReason};
use netband::monitor::{PingMonitorConfig, cancellation_channel, monitor_adaptive};
use netband::ping::{
    PingTransport, ProbeAttemptResult, ProbeBinding, ProbeFailure, ProbeReply, ProbeRequest,
};
use netband::scheduler::Scheduler;
use tempfile::tempdir;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio_tungstenite::accept_hdr_async;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::HeaderValue;

const PROTOCOL: &str = "net.measurementlab.ndt.v7";
type ProbeFuture<'a> = Pin<Box<dyn Future<Output = ProbeAttemptResult> + Send + 'a>>;

#[derive(Clone)]
struct DegradedTransport {
    active: Arc<AtomicUsize>,
    calls: Arc<AtomicUsize>,
    bandwidth_active: Arc<AtomicUsize>,
    overlap: Arc<AtomicBool>,
    recover_after_first_round: bool,
}

impl PingTransport for DegradedTransport {
    fn probe(&self, request: ProbeRequest) -> ProbeFuture<'_> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            self.active.fetch_add(1, Ordering::SeqCst);
            if self.bandwidth_active.load(Ordering::SeqCst) != 0 {
                self.overlap.store(true, Ordering::SeqCst);
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
            self.active.fetch_sub(1, Ordering::SeqCst);
            ProbeAttemptResult {
                binding: ProbeBinding {
                    interface: None,
                    source_ip: Some("192.0.2.10".parse().unwrap()),
                },
                sent: true,
                result: if call.is_multiple_of(2) || (self.recover_after_first_round && call >= 2) {
                    Ok(ProbeReply {
                        target: request.target,
                        identifier: Some(request.identifier),
                        sequence: request.sequence,
                        rtt: Duration::from_millis(10),
                        icmp_type: 0,
                        icmp_code: 0,
                    })
                } else {
                    Err(ProbeFailure::Timeout)
                },
            }
        })
    }
}

#[derive(Clone, Default)]
struct RecordingJournal {
    events: Arc<Mutex<Vec<MeasurementEvent>>>,
}

impl JournalSink for RecordingJournal {
    fn append_batch(&mut self, events: &[MeasurementEvent]) -> Result<(), JournalError> {
        self.events.lock().unwrap().extend_from_slice(events);
        Ok(())
    }
}

#[allow(clippy::result_large_err)]
fn accept_protocol(_request: &Request, mut response: Response) -> Result<Response, ErrorResponse> {
    response
        .headers_mut()
        .insert("sec-websocket-protocol", HeaderValue::from_static(PROTOCOL));
    Ok(response)
}

async fn ndt_server(
    listener: TcpListener,
    bandwidth_active: Arc<AtomicUsize>,
    max_bandwidth_active: Arc<AtomicUsize>,
    shutdown: watch::Sender<bool>,
    post_test_delay: Duration,
) {
    let (download, _) = listener.accept().await.unwrap();
    let active = bandwidth_active.fetch_add(1, Ordering::SeqCst) + 1;
    max_bandwidth_active.fetch_max(active, Ordering::SeqCst);
    let mut download = accept_hdr_async(download, accept_protocol).await.unwrap();
    for _ in 0..8 {
        download
            .send(Message::Binary(vec![1_u8; 16 * 1024].into()))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    download.close(None).await.unwrap();

    let (upload, _) = listener.accept().await.unwrap();
    let mut upload = accept_hdr_async(upload, accept_protocol).await.unwrap();
    let mut bytes = 0_usize;
    let upload_window = tokio::time::sleep(Duration::from_millis(80));
    tokio::pin!(upload_window);
    loop {
        tokio::select! {
            _ = &mut upload_window => break,
            message = upload.next() => {
                let Some(message) = message else {
                    break;
                };
                if let Message::Binary(payload) = message.unwrap() {
                    bytes += payload.len();
                }
            }
        }
    }
    assert!(bytes >= 16 * 1024);
    let _ = upload.close(None).await;
    bandwidth_active.fetch_sub(1, Ordering::SeqCst);
    tokio::time::sleep(post_test_delay).await;
    let _ = shutdown.send(true);
}

fn resolved(root: PathBuf, address: std::net::SocketAddr) -> netband::config::ResolvedConfig {
    let download = format!("ws://{address}/ndt/v7/download");
    let upload = format!("ws://{address}/ndt/v7/upload");
    let cli = Cli::try_parse_from([
        "netband",
        "--ndt-provider",
        "direct",
        "--ndt-download-url",
        &download,
        "--ndt-upload-url",
        &upload,
        "--allow-insecure-ndt",
        "--ping-target",
        "192.0.2.1",
        "--ping-target",
        "192.0.2.2",
        "--ping-interval",
        "20ms",
        "--loss-window-rounds",
        "1",
        "--loss-min-samples",
        "2",
        "--bandwidth-timeout",
        "2s",
        "run",
    ])
    .unwrap();
    resolve(
        &cli,
        &ResolveContext {
            stdout_is_terminal: false,
            current_dir: root.clone(),
            state_dir: root.join("state"),
        },
    )
    .unwrap()
}

#[tokio::test]
async fn degraded_round_drains_before_one_triggered_bandwidth_attempt() {
    let root = tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config = resolved(root.path().to_path_buf(), listener.local_addr().unwrap());
    let active = Arc::new(AtomicUsize::new(0));
    let calls = Arc::new(AtomicUsize::new(0));
    let bandwidth_active = Arc::new(AtomicUsize::new(0));
    let max_bandwidth_active = Arc::new(AtomicUsize::new(0));
    let overlap = Arc::new(AtomicBool::new(false));
    let transport = Arc::new(DegradedTransport {
        active: Arc::clone(&active),
        calls,
        bandwidth_active: Arc::clone(&bandwidth_active),
        overlap: Arc::clone(&overlap),
        recover_after_first_round: false,
    });
    let journal = RecordingJournal::default();
    let mut coordinator = OutputCoordinator::new(journal.clone(), ConsoleOff);
    let (shutdown_tx, shutdown) = cancellation_channel();
    let server = tokio::spawn(ndt_server(
        listener,
        bandwidth_active,
        Arc::clone(&max_bandwidth_active),
        shutdown_tx,
        Duration::from_millis(100),
    ));
    let scheduler =
        Scheduler::open(&config.state_file, &config.bandwidth, chrono::Utc::now()).unwrap();
    let settings = PingMonitorConfig {
        run_id: "adaptive-run".into(),
        targets: vec![
            IpAddr::V4("192.0.2.1".parse().unwrap()),
            IpAddr::V4("192.0.2.2".parse().unwrap()),
        ],
        interval: Duration::from_millis(20),
        timeout: Duration::from_secs(1),
        identifier: 42,
        health: HealthConfig {
            window_rounds: 1,
            min_samples: 2,
            loss_threshold_pct: 50.0,
            rtt_threshold_ms: None,
            recovery_loss_pct: 10.0,
            recovery_rounds: 3,
        },
    };

    let stats = tokio::time::timeout(
        Duration::from_secs(5),
        monitor_adaptive(
            &config,
            transport,
            settings,
            scheduler,
            &mut coordinator,
            shutdown,
        ),
    )
    .await
    .unwrap()
    .unwrap();
    server.await.unwrap();

    assert!(
        stats.rounds_completed > 1,
        "pings must resume after bandwidth"
    );
    assert_eq!(stats.bandwidth_attempts, 1);
    assert!(overlap.load(Ordering::SeqCst));
    assert_eq!(max_bandwidth_active.load(Ordering::SeqCst), 1);
    let events = journal.events.lock().unwrap();
    assert!(events.iter().any(|event| {
        event.event_kind == EventKind::PingProbe && event.outcome == Outcome::Timeout
    }));
    assert!(events.iter().any(|event| {
        event.event_kind == EventKind::Scheduler
            && event.trigger_reason == Some(TriggerReason::PingLoss)
            && event.outcome == Outcome::Scheduled
    }));
    assert!(events.iter().any(|event| {
        event.event_kind == EventKind::Bandwidth
            && event.trigger_reason == Some(TriggerReason::PingLoss)
            && event.daily_runs_used == Some(1)
            && event.outcome == Outcome::Success
    }));
    let loaded = events
        .iter()
        .filter(|event| event.event_kind == EventKind::PingProbe && event.load_run_id.is_some())
        .collect::<Vec<_>>();
    assert!(
        loaded
            .iter()
            .any(|event| event.load_phase == Some(LoadPhase::Download))
    );
    assert!(
        loaded
            .iter()
            .any(|event| event.load_phase == Some(LoadPhase::Upload))
    );
    assert!(
        loaded
            .iter()
            .all(|event| { event.load_run_id.as_deref() == Some("adaptive-run:bandwidth:0") })
    );
    assert!(events.iter().all(|event| {
        event.load_phase.is_none()
            || matches!(
                event.event_kind,
                EventKind::PingProbe | EventKind::PingSummary
            )
    }));
    assert!(events.iter().any(|event| {
        event.event_kind == EventKind::PingProbe
            && event.load_run_id.is_none()
            && event.sequence.is_some_and(|sequence| sequence > 1)
    }));
}

#[tokio::test]
async fn loaded_successes_are_durable_but_do_not_rearm_the_health_trigger() {
    let root = tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config = resolved(root.path().to_path_buf(), listener.local_addr().unwrap());
    let active = Arc::new(AtomicUsize::new(0));
    let bandwidth_active = Arc::new(AtomicUsize::new(0));
    let overlap = Arc::new(AtomicBool::new(false));
    let transport = Arc::new(DegradedTransport {
        active,
        calls: Arc::new(AtomicUsize::new(0)),
        bandwidth_active: Arc::clone(&bandwidth_active),
        overlap,
        recover_after_first_round: true,
    });
    let journal = RecordingJournal::default();
    let mut coordinator = OutputCoordinator::new(journal.clone(), ConsoleOff);
    let (shutdown_tx, shutdown) = cancellation_channel();
    let server = tokio::spawn(ndt_server(
        listener,
        bandwidth_active,
        Arc::new(AtomicUsize::new(0)),
        shutdown_tx,
        Duration::from_millis(1),
    ));
    let scheduler =
        Scheduler::open(&config.state_file, &config.bandwidth, chrono::Utc::now()).unwrap();
    let settings = PingMonitorConfig {
        run_id: "health-isolation-run".into(),
        targets: vec![
            IpAddr::V4("192.0.2.1".parse().unwrap()),
            IpAddr::V4("192.0.2.2".parse().unwrap()),
        ],
        interval: Duration::from_millis(20),
        timeout: Duration::from_secs(1),
        identifier: 43,
        health: HealthConfig {
            window_rounds: 1,
            min_samples: 2,
            loss_threshold_pct: 50.0,
            rtt_threshold_ms: None,
            recovery_loss_pct: 10.0,
            recovery_rounds: 3,
        },
    };

    let stats = monitor_adaptive(
        &config,
        transport,
        settings,
        scheduler,
        &mut coordinator,
        shutdown,
    )
    .await
    .unwrap();
    server.await.unwrap();

    assert_eq!(stats.bandwidth_attempts, 1);
    let events = journal.events.lock().unwrap();
    let loaded_successes = events
        .iter()
        .filter(|event| {
            event.event_kind == EventKind::PingProbe
                && event.load_run_id.is_some()
                && event.outcome == Outcome::Success
        })
        .count();
    assert!(loaded_successes >= 6);
    drop(events);

    let reopened =
        Scheduler::open(&config.state_file, &config.bandwidth, chrono::Utc::now()).unwrap();
    assert!(
        reopened.snapshot().trigger_latched,
        "only unloaded recovery rounds may rearm the trigger"
    );
}
