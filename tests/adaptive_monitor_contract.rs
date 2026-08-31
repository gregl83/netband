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
use netband::model::{EventKind, MeasurementEvent, Outcome, TriggerReason};
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
}

impl PingTransport for DegradedTransport {
    fn probe(&self, request: ProbeRequest) -> ProbeFuture<'_> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            self.active.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(5)).await;
            self.active.fetch_sub(1, Ordering::SeqCst);
            ProbeAttemptResult {
                binding: ProbeBinding {
                    interface: None,
                    source_ip: Some("192.0.2.10".parse().unwrap()),
                },
                sent: true,
                result: if call.is_multiple_of(2) {
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
    ping_active: Arc<AtomicUsize>,
    overlap: Arc<AtomicBool>,
    shutdown: watch::Sender<bool>,
) {
    let (download, _) = listener.accept().await.unwrap();
    overlap.store(ping_active.load(Ordering::SeqCst) != 0, Ordering::SeqCst);
    let mut download = accept_hdr_async(download, accept_protocol).await.unwrap();
    download
        .send(Message::Binary(vec![1_u8; 16 * 1024].into()))
        .await
        .unwrap();
    download.close(None).await.unwrap();

    let (upload, _) = listener.accept().await.unwrap();
    overlap.fetch_or(ping_active.load(Ordering::SeqCst) != 0, Ordering::SeqCst);
    let mut upload = accept_hdr_async(upload, accept_protocol).await.unwrap();
    let mut bytes = 0_usize;
    while let Some(message) = upload.next().await {
        if let Message::Binary(payload) = message.unwrap() {
            bytes += payload.len();
            if bytes >= 16 * 1024 {
                break;
            }
        }
    }
    upload.close(None).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
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
    let overlap = Arc::new(AtomicBool::new(false));
    let transport = Arc::new(DegradedTransport {
        active: Arc::clone(&active),
        calls,
    });
    let journal = RecordingJournal::default();
    let mut coordinator = OutputCoordinator::new(journal.clone(), ConsoleOff);
    let (shutdown_tx, shutdown) = cancellation_channel();
    let server = tokio::spawn(ndt_server(
        listener,
        active,
        Arc::clone(&overlap),
        shutdown_tx,
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
    assert!(!overlap.load(Ordering::SeqCst));
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
}
