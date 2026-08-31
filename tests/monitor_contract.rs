use std::collections::HashSet;
use std::future::Future;
use std::io;
use std::net::IpAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use netband::cli::ConsoleMode;
use netband::console::{Console, ConsoleOff};
use netband::health::HealthConfig;
use netband::journal::{JournalError, JournalSink, OutputCoordinator};
use netband::model::MeasurementEvent;
use netband::monitor::{MonitorError, PingMonitorConfig, cancellation_channel, monitor_ping};
use netband::ping::{
    PingTransport, ProbeAttemptResult, ProbeBinding, ProbeFailure, ProbeReply, ProbeRequest,
};

type ProbeFuture<'a> = Pin<Box<dyn Future<Output = ProbeAttemptResult> + Send + 'a>>;

#[derive(Clone, Copy)]
enum ResultMode {
    Success,
    Alternating,
}

#[derive(Clone)]
struct FakeTransport {
    delay: Duration,
    mode: ResultMode,
    calls: Arc<AtomicUsize>,
    active: Arc<AtomicUsize>,
    max_active: Arc<AtomicUsize>,
}

impl FakeTransport {
    fn new(delay: Duration, mode: ResultMode) -> Self {
        Self {
            delay,
            mode,
            calls: Arc::new(AtomicUsize::new(0)),
            active: Arc::new(AtomicUsize::new(0)),
            max_active: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl PingTransport for FakeTransport {
    fn probe(&self, request: ProbeRequest) -> ProbeFuture<'_> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active.fetch_max(active, Ordering::SeqCst);
            tokio::time::sleep(self.delay).await;
            self.active.fetch_sub(1, Ordering::SeqCst);
            let failed = matches!(self.mode, ResultMode::Alternating) && call % 2 == 1;
            ProbeAttemptResult {
                binding: ProbeBinding {
                    interface: Some("eth-test".into()),
                    source_ip: Some("192.0.2.10".parse().unwrap()),
                },
                sent: true,
                result: if failed {
                    Err(ProbeFailure::Timeout)
                } else {
                    Ok(ProbeReply {
                        target: request.target,
                        identifier: Some(request.identifier),
                        sequence: request.sequence,
                        rtt: Duration::from_millis(10),
                        icmp_type: if request.target.is_ipv4() { 0 } else { 129 },
                        icmp_code: 0,
                    })
                },
            }
        })
    }
}

#[derive(Clone, Default)]
struct RecordingJournal {
    batches: Arc<AtomicUsize>,
    events: Arc<Mutex<Vec<MeasurementEvent>>>,
    fail_on_batch: Option<usize>,
}

impl JournalSink for RecordingJournal {
    fn append_batch(&mut self, events: &[MeasurementEvent]) -> Result<(), JournalError> {
        let batch = self.batches.fetch_add(1, Ordering::SeqCst) + 1;
        if self.fail_on_batch == Some(batch) {
            return Err(JournalError::write(io::Error::other("injected failure")));
        }
        self.events.lock().unwrap().extend_from_slice(events);
        Ok(())
    }
}

fn settings(interval: Duration, targets: usize) -> PingMonitorConfig {
    PingMonitorConfig {
        run_id: "continuous-test".into(),
        targets: (1..=targets)
            .map(|last| IpAddr::V4(std::net::Ipv4Addr::new(192, 0, 2, last as u8)))
            .collect(),
        interval,
        timeout: Duration::from_secs(30),
        identifier: 42,
        health: HealthConfig {
            window_rounds: 6,
            min_samples: 6,
            loss_threshold_pct: 50.0,
            rtt_threshold_ms: None,
            recovery_loss_pct: 10.0,
            recovery_rounds: 3,
        },
    }
}

async fn wait_for_calls(calls: &AtomicUsize, expected: usize) {
    for _ in 0..20 {
        if calls.load(Ordering::SeqCst) >= expected {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!(
        "expected {expected} probes, observed {}",
        calls.load(Ordering::SeqCst)
    );
}

#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn immediate_start_and_regular_ticks_cover_one_logical_hour() {
    let transport = Arc::new(FakeTransport::new(Duration::ZERO, ResultMode::Success));
    let journal = RecordingJournal::default();
    let mut coordinator = OutputCoordinator::new(journal.clone(), ConsoleOff);
    let (shutdown_tx, shutdown) = cancellation_channel();
    let monitor_transport = Arc::clone(&transport);
    let task = tokio::spawn(async move {
        monitor_ping(
            monitor_transport,
            settings(Duration::from_secs(5), 1),
            &mut coordinator,
            shutdown,
        )
        .await
    });

    wait_for_calls(&transport.calls, 1).await;
    for expected in 2..=721 {
        tokio::time::advance(Duration::from_secs(5)).await;
        wait_for_calls(&transport.calls, expected).await;
    }
    shutdown_tx.send(true).unwrap();
    let stats = task.await.unwrap().unwrap();

    assert_eq!(stats.rounds_started, 721);
    assert_eq!(stats.rounds_completed, 721);
    assert_eq!(stats.skipped_ticks, 0);
    assert_eq!(journal.batches.load(Ordering::SeqCst), 721);
    let events = journal.events.lock().unwrap();
    assert_eq!(events.len(), 1_442);
    assert!(events.iter().all(|event| event.run_id == "continuous-test"));
    assert_eq!(
        events
            .iter()
            .map(|event| event.event_id.as_str())
            .collect::<HashSet<_>>()
            .len(),
        events.len()
    );
    assert!(events.as_chunks::<2>().0.iter().all(|pair| {
        pair[0].event_kind == netband::model::EventKind::PingProbe
            && pair[1].event_kind == netband::model::EventKind::PingSummary
    }));
}

#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn slow_rounds_skip_ticks_without_overlap_or_catch_up() {
    let transport = Arc::new(FakeTransport::new(
        Duration::from_secs(12),
        ResultMode::Success,
    ));
    let journal = RecordingJournal::default();
    let mut coordinator = OutputCoordinator::new(journal, ConsoleOff);
    let (shutdown_tx, shutdown) = cancellation_channel();
    let monitor_transport = Arc::clone(&transport);
    let task = tokio::spawn(async move {
        monitor_ping(
            monitor_transport,
            settings(Duration::from_secs(5), 2),
            &mut coordinator,
            shutdown,
        )
        .await
    });

    wait_for_calls(&transport.calls, 2).await;
    tokio::time::advance(Duration::from_secs(5)).await;
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(5)).await;
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(2)).await;
    tokio::task::yield_now().await;
    assert_eq!(transport.calls.load(Ordering::SeqCst), 2);
    tokio::time::advance(Duration::from_secs(3)).await;
    wait_for_calls(&transport.calls, 4).await;
    shutdown_tx.send(true).unwrap();
    tokio::time::advance(Duration::from_secs(12)).await;
    let stats = task.await.unwrap().unwrap();

    assert_eq!(stats.rounds_started, 2);
    assert_eq!(stats.rounds_completed, 2);
    assert_eq!(stats.skipped_ticks, 2);
    assert_eq!(transport.max_active.load(Ordering::SeqCst), 2);
}

#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn mixed_probe_failures_do_not_stop_future_rounds() {
    let transport = Arc::new(FakeTransport::new(Duration::ZERO, ResultMode::Alternating));
    let journal = RecordingJournal::default();
    let mut coordinator = OutputCoordinator::new(journal, ConsoleOff);
    let (shutdown_tx, shutdown) = cancellation_channel();
    let monitor_transport = Arc::clone(&transport);
    let task = tokio::spawn(async move {
        monitor_ping(
            monitor_transport,
            settings(Duration::from_secs(5), 2),
            &mut coordinator,
            shutdown,
        )
        .await
    });

    wait_for_calls(&transport.calls, 2).await;
    for expected in [4, 6, 8] {
        tokio::time::advance(Duration::from_secs(5)).await;
        wait_for_calls(&transport.calls, expected).await;
    }
    shutdown_tx.send(true).unwrap();
    let stats = task.await.unwrap().unwrap();
    assert_eq!(stats.rounds_completed, 4);
    assert_eq!(stats.successful_probes, 4);
    assert_eq!(stats.failed_probes, 4);
}

#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn journal_failure_is_fatal_and_stops_scheduling() {
    let transport = Arc::new(FakeTransport::new(Duration::ZERO, ResultMode::Success));
    let journal = RecordingJournal {
        fail_on_batch: Some(2),
        ..RecordingJournal::default()
    };
    let mut coordinator = OutputCoordinator::new(journal, ConsoleOff);
    let (_shutdown_tx, shutdown) = cancellation_channel();
    let monitor_transport = Arc::clone(&transport);
    let task = tokio::spawn(async move {
        monitor_ping(
            monitor_transport,
            settings(Duration::from_secs(5), 1),
            &mut coordinator,
            shutdown,
        )
        .await
    });

    wait_for_calls(&transport.calls, 1).await;
    tokio::time::advance(Duration::from_secs(5)).await;
    wait_for_calls(&transport.calls, 2).await;
    assert!(matches!(task.await.unwrap(), Err(MonitorError::Journal(_))));
    tokio::time::advance(Duration::from_secs(3_600)).await;
    assert_eq!(transport.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn closed_stdout_disables_console_without_stopping_measurements() {
    let transport = Arc::new(FakeTransport::new(Duration::ZERO, ResultMode::Success));
    let journal = RecordingJournal::default();
    let (writer, reader) = tokio::io::duplex(64);
    drop(reader);
    let console = Console::spawn(ConsoleMode::Human, writer, 8, |_| {});
    let mut coordinator = OutputCoordinator::new(journal.clone(), console);
    let (shutdown_tx, shutdown) = cancellation_channel();
    let monitor_transport = Arc::clone(&transport);
    let task = tokio::spawn(async move {
        let result = monitor_ping(
            monitor_transport,
            settings(Duration::from_secs(5), 1),
            &mut coordinator,
            shutdown,
        )
        .await;
        let (_, console) = coordinator.into_parts();
        (result, console.shutdown(Duration::from_secs(1)).await)
    });

    wait_for_calls(&transport.calls, 1).await;
    for expected in 2..=4 {
        tokio::time::advance(Duration::from_secs(5)).await;
        wait_for_calls(&transport.calls, expected).await;
    }
    shutdown_tx.send(true).unwrap();
    let (result, console_stats) = task.await.unwrap();
    assert_eq!(result.unwrap().rounds_completed, 4);
    assert_eq!(journal.batches.load(Ordering::SeqCst), 4);
    assert!(console_stats.disabled);
}
