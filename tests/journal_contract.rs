use std::fs;
use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use chrono::{TimeZone, Utc};
use netband::config::OutputTarget;
use netband::console::ConsoleSink;
use netband::journal::{CSV_HEADER, Journal, JournalError, JournalSink, OutputCoordinator};
use netband::model::{
    ErrorKind, EventKind, MeasurementEvent, Outcome, ProviderKind, RequestStage, TriggerReason,
};
use tempfile::tempdir;

fn timestamp(second: u32) -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 30, 12, 0, second)
        .single()
        .unwrap()
        + chrono::TimeDelta::milliseconds(123)
}

fn event(kind: EventKind, outcome: Outcome, id: &str) -> MeasurementEvent {
    MeasurementEvent::new("run-1", id, kind, outcome, timestamp(2))
}

fn fixture_events() -> Vec<MeasurementEvent> {
    let mut ping_failure = event(EventKind::PingProbe, Outcome::Timeout, "event-1");
    ping_failure.scheduled_at_utc = Some(timestamp(0));
    ping_failure.started_at_utc = Some(timestamp(0));
    ping_failure.interface = Some("eth0".into());
    ping_failure.source_ip = Some("192.0.2.10".parse().unwrap());
    ping_failure.target = Some("1.1.1.1".into());
    ping_failure.sequence = Some(7);
    ping_failure.duration_ms = Some(2_000.125);
    ping_failure.icmp_type = Some(3);
    ping_failure.icmp_code = Some(1);
    ping_failure.os_error_code = Some(10060);
    ping_failure.error_kind = Some(ErrorKind::IcmpTimeout);
    ping_failure.error_message = Some("réseau, timeout\nsecond line".into());

    let mut ping_summary = event(EventKind::PingSummary, Outcome::Success, "event-2");
    ping_summary.interface = Some("eth0".into());
    ping_summary.target = Some("8.8.8.8".into());
    ping_summary.rtt_ms = Some(12.5);
    ping_summary.packets_sent = Some(1);
    ping_summary.packets_received = Some(1);
    ping_summary.packet_loss_pct = Some(0.0);

    let mut bandwidth = event(EventKind::Bandwidth, Outcome::Partial, "event-3");
    bandwidth.trigger_reason = Some(TriggerReason::Manual);
    bandwidth.interface = Some("eth0".into());
    bandwidth.provider_id = Some("direct:abc".into());
    bandwidth.provider_kind = Some(ProviderKind::Direct);
    bandwidth.server = Some("wss://ndt.example.net/ndt/v7?access_token=secret".into());
    bandwidth.remote_ip = Some("203.0.113.20".parse().unwrap());
    bandwidth.duration_ms = Some(10_500.0);
    bandwidth.download_mbps = Some(123.456789);
    bandwidth.bytes_received = Some(123_456_789);
    bandwidth.error_kind = Some(ErrorKind::UploadFailed);
    bandwidth.error_message = Some("upload stream closed".into());

    let mut locate = event(EventKind::RequestFailure, Outcome::RateLimited, "event-4");
    locate.provider_id = Some("mlab".into());
    locate.provider_kind = Some(ProviderKind::Mlab);
    locate.server = Some("https://locate.measurementlab.net/v2/nearest?token=secret".into());
    locate.request_stage = Some(RequestStage::Locate);
    locate.request_attempt = Some(2);
    locate.http_status = Some(429);
    locate.retry_after_ms = Some(60_000);
    locate.rate_limit_until_utc = Some(timestamp(3));
    locate.error_kind = Some(ErrorKind::HttpStatus);
    locate.error_message = Some("capacity unavailable".into());

    let mut websocket = event(EventKind::RequestFailure, Outcome::Error, "event-5");
    websocket.provider_id = Some("direct:abc".into());
    websocket.provider_kind = Some(ProviderKind::Direct);
    websocket.server = Some("wss://ndt.example.net/custom/upload?key=secret".into());
    websocket.request_stage = Some(RequestStage::WebsocketHandshake);
    websocket.request_attempt = Some(1);
    websocket.http_status = Some(503);
    websocket.error_kind = Some(ErrorKind::WebsocketHandshake);
    websocket.error_message = Some("upstream unavailable".into());

    let mut deferred = event(EventKind::Scheduler, Outcome::Deferred, "event-6");
    deferred.trigger_reason = Some(TriggerReason::PingLoss);
    deferred.provider_id = Some("mlab".into());
    deferred.provider_kind = Some(ProviderKind::Mlab);
    deferred.rate_limit_until_utc = Some(timestamp(3));
    deferred.daily_runs_used = Some(2);
    deferred.error_kind = Some(ErrorKind::ProviderCooldown);
    deferred.error_message = Some("provider cooldown active".into());

    let mut suppressed = event(EventKind::Scheduler, Outcome::Suppressed, "event-7");
    suppressed.trigger_reason = Some(TriggerReason::Scheduled);
    suppressed.provider_id = Some("mlab".into());
    suppressed.provider_kind = Some(ProviderKind::Mlab);
    suppressed.daily_runs_used = Some(4);
    suppressed.error_kind = Some(ErrorKind::DailyCap);
    suppressed.error_message = Some("daily maximum reached".into());

    vec![
        ping_failure,
        ping_summary,
        bandwidth,
        locate,
        websocket,
        deferred,
        suppressed,
    ]
}

#[test]
fn fixture_events_produce_byte_stable_v1_csv() {
    let mut journal = Journal::from_writer(Vec::new()).unwrap();
    journal.append_batch(&fixture_events()).unwrap();
    let bytes = journal.into_inner().unwrap();
    let text = String::from_utf8(bytes).unwrap();

    let expected = include_str!("fixtures/v1-events.csv")
        .replace('\n', "\r\n")
        .replace("{{LF}}", "\n");
    assert_eq!(text, expected);
    assert!(text.starts_with(CSV_HEADER));
    assert!(text.contains("\"réseau, timeout\nsecond line\""));
    assert!(!text.contains("secret"));
}

#[test]
fn explicit_files_append_with_one_header_and_reject_mismatch() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("events.csv");
    let output = OutputTarget::File(path.clone());

    let (mut first, opened_path) = Journal::open_at(&output, timestamp(0)).unwrap();
    assert_eq!(opened_path, path);
    first.append_batch(&[fixture_events()[0].clone()]).unwrap();
    drop(first);

    let (mut second, _) = Journal::open_at(&output, timestamp(1)).unwrap();
    second.append_batch(&[fixture_events()[1].clone()]).unwrap();
    drop(second);

    let contents = fs::read_to_string(&path).unwrap();
    assert_eq!(contents.matches(CSV_HEADER).count(), 1);
    assert_eq!(contents.lines().count(), 4); // The quoted diagnostic contains a newline.

    let bad_path = dir.path().join("bad.csv");
    fs::write(&bad_path, "wrong,header\r\nexisting,data\r\n").unwrap();
    let before = fs::read(&bad_path).unwrap();
    let error = Journal::open_at(&OutputTarget::File(bad_path.clone()), timestamp(0)).unwrap_err();
    assert!(error.to_string().contains("header"));
    assert_eq!(fs::read(bad_path).unwrap(), before);

    let bare_header = dir.path().join("bare-header.csv");
    fs::write(&bare_header, CSV_HEADER).unwrap();
    let (mut journal, _) =
        Journal::open_at(&OutputTarget::File(bare_header.clone()), timestamp(0)).unwrap();
    journal
        .append_batch(&[fixture_events()[1].clone()])
        .unwrap();
    drop(journal);
    let contents = fs::read_to_string(bare_header).unwrap();
    assert!(contents.starts_with(&format!("{CSV_HEADER}\r\n1,")));
}

#[test]
fn explicit_output_lock_rejects_competing_writer_and_releases_on_drop() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("locked.csv");
    let output = OutputTarget::File(path.clone());

    let (first, _) = Journal::open_at(&output, timestamp(0)).unwrap();
    assert!(matches!(
        Journal::open_at(&output, timestamp(1)),
        Err(JournalError::Locked(locked)) if locked == path
    ));
    drop(first);

    let (mut restarted, _) = Journal::open_at(&output, timestamp(2)).unwrap();
    restarted
        .append_batch(&[fixture_events()[0].clone()])
        .unwrap();
    drop(restarted);
    assert_eq!(
        fs::read_to_string(path)
            .unwrap()
            .matches(CSV_HEADER)
            .count(),
        1
    );
}

#[test]
fn directory_output_creates_a_timestamped_file() {
    let dir = tempdir().unwrap();
    let (journal, path) = Journal::open_at(
        &OutputTarget::Directory(dir.path().to_path_buf()),
        timestamp(0),
    )
    .unwrap();
    drop(journal);

    assert_eq!(
        path.file_name().unwrap(),
        "netband-20260830T120000.123Z.csv"
    );
    assert_eq!(
        fs::read_to_string(path).unwrap(),
        format!("{CSV_HEADER}\r\n")
    );
}

#[derive(Clone)]
struct TraceJournal {
    trace: Arc<Mutex<Vec<&'static str>>>,
    fail: bool,
}

impl JournalSink for TraceJournal {
    fn append_batch(
        &mut self,
        _events: &[MeasurementEvent],
    ) -> Result<(), netband::journal::JournalError> {
        self.trace.lock().unwrap().push("journal");
        if self.fail {
            Err(netband::journal::JournalError::write(io::Error::other(
                "fixture failure",
            )))
        } else {
            Ok(())
        }
    }
}

#[derive(Clone)]
struct TraceConsole(Arc<Mutex<Vec<&'static str>>>);

impl ConsoleSink for TraceConsole {
    fn offer(&self, _event: &MeasurementEvent) {
        self.0.lock().unwrap().push("console");
    }
}

#[test]
fn coordinator_flushes_journal_before_console_and_stops_on_failure() {
    let trace = Arc::new(Mutex::new(Vec::new()));
    let mut coordinator = OutputCoordinator::new(
        TraceJournal {
            trace: Arc::clone(&trace),
            fail: false,
        },
        TraceConsole(Arc::clone(&trace)),
    );
    coordinator.publish_batch(&fixture_events()[..2]).unwrap();
    assert_eq!(*trace.lock().unwrap(), ["journal", "console", "console"]);

    trace.lock().unwrap().clear();
    let mut failing = OutputCoordinator::new(
        TraceJournal {
            trace: Arc::clone(&trace),
            fail: true,
        },
        TraceConsole(Arc::clone(&trace)),
    );
    assert!(failing.publish_batch(&fixture_events()[..1]).is_err());
    assert_eq!(*trace.lock().unwrap(), ["journal"]);
}

struct FailingWriter;

impl Write for FailingWriter {
    fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("disk full"))
    }

    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::other("disk full"))
    }
}

#[test]
fn writer_failures_are_fatal() {
    let error = Journal::from_writer(FailingWriter).unwrap_err();
    assert!(error.to_string().contains("disk full"));
}
