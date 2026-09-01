use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use chrono::{TimeZone, Utc};
use netband::cli::ConsoleMode;
use netband::console::{Console, ConsoleDiagnostic, ConsoleSink, human_line, render_jsonl};
use netband::journal::{Journal, OutputCoordinator};
use netband::model::{
    ErrorKind, EventKind, MeasurementEvent, Outcome, ProviderKind, RequestStage, TriggerReason,
};
use tokio::io::{AsyncReadExt, AsyncWrite};

#[cfg(target_os = "linux")]
#[test]
fn service_stdout_does_not_change_inherited_descriptor_flags() {
    fn stdout_flags() -> String {
        std::fs::read_to_string("/proc/self/fdinfo/1")
            .unwrap()
            .lines()
            .find(|line| line.starts_with("flags:"))
            .unwrap()
            .to_owned()
    }

    let before = stdout_flags();
    let writer = netband::console::service_stdout();
    let after = stdout_flags();
    drop(writer);

    assert_eq!(after, before);
}

fn event(kind: EventKind, outcome: Outcome) -> MeasurementEvent {
    MeasurementEvent::new(
        "run-1",
        "event-1",
        kind,
        outcome,
        Utc.with_ymd_and_hms(2026, 8, 30, 12, 0, 2)
            .single()
            .unwrap(),
    )
}

#[test]
fn human_output_is_concise_and_omits_internal_events() {
    let mut ping = event(EventKind::PingSummary, Outcome::Timeout);
    ping.interface = Some("eth0".into());
    ping.target = Some("1.1.1.1".into());
    ping.packets_sent = Some(1);
    ping.packets_received = Some(0);
    ping.packet_loss_pct = Some(100.0);
    ping.error_message = Some("request timed out".into());
    let line = human_line(&ping).unwrap();
    assert_eq!(
        line,
        "2026-08-30T12:00:02.000Z ping interface=eth0 target=1.1.1.1 outcome=timeout rtt_ms=- loss_pct=100 reason=\"request timed out\"\n"
    );
    assert!(!line.contains("\u{1b}["));

    let mut bandwidth = event(EventKind::Bandwidth, Outcome::Partial);
    bandwidth.provider_kind = Some(ProviderKind::Direct);
    bandwidth.server = Some("wss://ndt.example.net/down?token=secret".into());
    bandwidth.download_mbps = Some(100.25);
    bandwidth.upload_mbps = None;
    bandwidth.error_message = Some("upload failed".into());
    let line = human_line(&bandwidth).unwrap();
    assert!(line.contains("provider=direct"));
    assert!(line.contains("server=wss://ndt.example.net/down?[redacted]"));
    assert!(line.contains("download_mbps=100.25 upload_mbps=-"));
    assert!(!line.contains("secret"));

    assert!(human_line(&event(EventKind::RequestFailure, Outcome::Error)).is_none());
    assert!(human_line(&event(EventKind::Scheduler, Outcome::Deferred)).is_none());
}

#[test]
fn jsonl_is_versioned_flat_and_sanitized() {
    let mut request = event(EventKind::RequestFailure, Outcome::RateLimited);
    request.provider_kind = Some(ProviderKind::Mlab);
    request.server = Some("https://locate.example/nearest?access_token=secret".into());
    request.request_stage = Some(RequestStage::Locate);
    request.http_status = Some(429);
    request.retry_after_ms = Some(60_000);
    request.trigger_reason = Some(TriggerReason::PingLoss);
    request.error_kind = Some(ErrorKind::HttpStatus);
    request.error_message = Some("rate limited".into());

    let line = render_jsonl(&request).unwrap();
    assert!(line.ends_with('\n'));
    assert_eq!(line.lines().count(), 1);
    assert!(!line.contains("secret"));
    assert!(!line.contains("\u{1b}["));
    let value: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["event_kind"], "request_failure");
    assert_eq!(value["request_stage"], "locate");
    assert_eq!(value["rtt_ms"], serde_json::Value::Null);
    assert_eq!(value["server"], "https://locate.example/nearest?[redacted]");
}

#[test]
fn diagnostic_text_and_endpoint_credentials_are_sanitized() {
    let mut request = event(EventKind::RequestFailure, Outcome::Error);
    request.server = Some("https://user:password@example.test/path?token=server-secret".into());
    request.error_message = Some("access_token=first api_key=second token=third key=fourth".into());

    let line = render_jsonl(&request).unwrap();
    for secret in [
        "user",
        "password",
        "server-secret",
        "first",
        "second",
        "third",
        "fourth",
    ] {
        assert!(!line.contains(secret));
    }
    assert!(line.contains("[redacted]"));
}

#[tokio::test(flavor = "current_thread")]
async fn worker_writes_jsonl_and_drains_on_shutdown() {
    let (writer, mut reader) = tokio::io::duplex(16 * 1024);
    let console = Console::spawn(ConsoleMode::Jsonl, writer, 8, |_| {});
    console.offer(&event(EventKind::PingProbe, Outcome::Success));
    console.offer(&event(EventKind::PingSummary, Outcome::Success));
    let stats = console.shutdown(Duration::from_secs(1)).await;
    assert!(!stats.disabled);
    assert_eq!(stats.dropped_events, 0);

    let mut output = String::new();
    reader.read_to_string(&mut output).await.unwrap();
    assert_eq!(output.lines().count(), 2);
}

#[tokio::test(flavor = "current_thread")]
async fn async_writer_recovers_after_pipe_backpressure_clears() {
    let measurement = event(EventKind::PingProbe, Outcome::Success);
    let expected_line = render_jsonl(&measurement).unwrap();
    assert!(expected_line.len() > 64);

    let (writer, mut reader) = tokio::io::duplex(64);
    let console = Console::spawn(ConsoleMode::Jsonl, writer, 8, |_| {});
    console.offer(&measurement);
    console.offer(&measurement);

    tokio::time::sleep(Duration::from_millis(10)).await;
    assert!(!console.stats().disabled);

    let reader_task = tokio::spawn(async move {
        let mut output = String::new();
        reader.read_to_string(&mut output).await.unwrap();
        output
    });
    let stats = console.shutdown(Duration::from_secs(1)).await;
    let output = reader_task.await.unwrap();

    assert!(!stats.disabled);
    assert_eq!(stats.dropped_events, 0);
    assert_eq!(output, expected_line.repeat(2));
}

struct PendingWriter;

impl AsyncWrite for PendingWriter {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        _buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Poll::Pending
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Pending
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

#[tokio::test(flavor = "current_thread")]
async fn full_queue_drops_only_console_events_and_shutdown_is_bounded() {
    let diagnostics = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&diagnostics);
    let console = Console::spawn(ConsoleMode::Jsonl, PendingWriter, 1, move |diagnostic| {
        captured.lock().unwrap().push(diagnostic);
    });
    console.offer(&event(EventKind::PingProbe, Outcome::Success));
    tokio::task::yield_now().await;
    console.offer(&event(EventKind::PingProbe, Outcome::Success));
    console.offer(&event(EventKind::PingProbe, Outcome::Success));
    console.offer(&event(EventKind::PingProbe, Outcome::Success));

    let started = Instant::now();
    let stats = console.shutdown(Duration::from_millis(30)).await;
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(stats.dropped_events, 2);
    assert!(diagnostics.lock().unwrap().iter().any(|diagnostic| {
        matches!(
            diagnostic,
            ConsoleDiagnostic::QueueFull { dropped_events: 1 }
        )
    }));
}

#[tokio::test(flavor = "current_thread")]
async fn blocked_console_drops_do_not_remove_durable_csv_rows() {
    let journal = Journal::from_writer(Vec::new()).unwrap();
    let console = Console::spawn(ConsoleMode::Jsonl, PendingWriter, 1, |_| {});
    let mut coordinator = OutputCoordinator::new(journal, console);
    for index in 0..10 {
        let mut measurement = event(EventKind::PingProbe, Outcome::Success);
        measurement.event_id = format!("event-{index}");
        coordinator.publish_batch(&[measurement]).unwrap();
    }

    let (journal, console) = coordinator.into_parts();
    let bytes = journal.into_inner().unwrap();
    let mut reader = csv::Reader::from_reader(bytes.as_slice());
    assert_eq!(reader.records().count(), 10);
    let stats = console.shutdown(Duration::from_millis(30)).await;
    assert!(stats.dropped_events > 0);
}

struct BrokenWriter;

impl AsyncWrite for BrokenWriter {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        _buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Poll::Ready(Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed")))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Err(io::Error::other("flush failed")))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

struct OtherWriteFailure;

impl AsyncWrite for OtherWriteFailure {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        _buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Poll::Ready(Err(io::Error::other("write failed")))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

struct FlushFailure;

impl AsyncWrite for FlushFailure {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Err(io::Error::other("flush failed")))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

#[tokio::test(flavor = "current_thread")]
async fn broken_stdout_disables_console_once_without_payload_diagnostics() {
    let diagnostics = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&diagnostics);
    let console = Console::spawn(ConsoleMode::Human, BrokenWriter, 8, move |diagnostic| {
        captured.lock().unwrap().push(diagnostic);
    });
    let mut sensitive = event(EventKind::PingSummary, Outcome::Error);
    sensitive.target = Some("access-token-must-not-appear".into());
    console.offer(&sensitive);
    tokio::task::yield_now().await;
    console.offer(&sensitive);
    let stats = console.shutdown(Duration::from_secs(1)).await;

    assert!(stats.disabled);
    let diagnostics = diagnostics.lock().unwrap();
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(
        diagnostics
            .iter()
            .filter(|diagnostic| matches!(diagnostic, ConsoleDiagnostic::WriterDisabled { .. }))
            .count(),
        1
    );
    assert!(!format!("{diagnostics:?}").contains("access-token-must-not-appear"));
}

#[tokio::test(flavor = "current_thread")]
async fn non_broken_write_and_flush_failures_are_classified() {
    for (writer, expected) in [
        (
            Box::new(OtherWriteFailure) as Box<dyn AsyncWrite + Unpin + Send>,
            netband::console::ConsoleFailure::Write,
        ),
        (
            Box::new(FlushFailure) as Box<dyn AsyncWrite + Unpin + Send>,
            netband::console::ConsoleFailure::Flush,
        ),
    ] {
        let diagnostics = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&diagnostics);
        let console = Console::spawn(ConsoleMode::Jsonl, writer, 2, move |diagnostic| {
            captured.lock().unwrap().push(diagnostic);
        });
        console.offer(&event(EventKind::PingProbe, Outcome::Success));
        let stats = console.shutdown(Duration::from_secs(1)).await;
        assert!(stats.disabled);
        assert!(
            diagnostics
                .lock()
                .unwrap()
                .contains(&ConsoleDiagnostic::WriterDisabled { reason: expected })
        );
    }
}
