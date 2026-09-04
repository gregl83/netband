use std::collections::HashMap;
use std::future::Future;
use std::net::IpAddr;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{TimeZone, Utc};
use clap::Parser;
use netband::cli::{Cli, ConsoleMode};
use netband::config::{OutputTarget, ResolveContext, resolve};
use netband::model::{ErrorKind, EventKind, LoadPhase, Outcome};
use netband::ping::{
    PingExitStatus, PingRoundRequest, PingTransport, ProbeAttemptResult, ProbeBinding,
    ProbeFailure, ProbeReply, ProbeRequest, execute_ping_once, measure_round,
};
use tempfile::tempdir;
use tokio::io::AsyncReadExt;

type ProbeFuture<'a> = Pin<Box<dyn Future<Output = ProbeAttemptResult> + Send + 'a>>;

#[derive(Clone)]
struct Behavior {
    delay: Duration,
    result: ProbeAttemptResult,
}

#[derive(Clone)]
struct FakeTransport {
    behaviors: Arc<HashMap<IpAddr, Behavior>>,
    active: Arc<AtomicUsize>,
    max_active: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<ProbeRequest>>>,
}

impl FakeTransport {
    fn new(behaviors: impl IntoIterator<Item = (IpAddr, Behavior)>) -> Self {
        Self {
            behaviors: Arc::new(behaviors.into_iter().collect()),
            active: Arc::new(AtomicUsize::new(0)),
            max_active: Arc::new(AtomicUsize::new(0)),
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl PingTransport for FakeTransport {
    fn probe(&self, request: ProbeRequest) -> ProbeFuture<'_> {
        self.requests.lock().unwrap().push(request.clone());
        let behavior = self.behaviors[&request.target].clone();
        Box::pin(async move {
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active.fetch_max(active, Ordering::SeqCst);
            tokio::time::sleep(behavior.delay).await;
            self.active.fetch_sub(1, Ordering::SeqCst);
            behavior.result
        })
    }
}

fn ip(value: &str) -> IpAddr {
    value.parse().unwrap()
}

fn binding(address: &str) -> ProbeBinding {
    ProbeBinding {
        interface: Some("eth-test".into()),
        source_ip: Some(ip(address)),
    }
}

fn success(target: IpAddr, sequence: u16, rtt_ms: u64) -> ProbeAttemptResult {
    ProbeAttemptResult {
        binding: binding("192.0.2.10"),
        sent: true,
        result: Ok(ProbeReply {
            target,
            identifier: None,
            sequence,
            rtt: Duration::from_millis(rtt_ms),
            icmp_type: 0,
            icmp_code: 0,
        }),
    }
}

fn behavior(delay_ms: u64, result: ProbeAttemptResult) -> Behavior {
    Behavior {
        delay: Duration::from_millis(delay_ms),
        result,
    }
}

fn round(targets: Vec<IpAddr>) -> PingRoundRequest {
    PingRoundRequest {
        run_id: "run-test".into(),
        round_number: 0,
        targets,
        timeout: Duration::from_secs(2),
        scheduled_at_utc: Utc
            .with_ymd_and_hms(2026, 8, 30, 12, 0, 0)
            .single()
            .unwrap(),
        identifier: 42,
        load_phase: None,
        load_run_id: None,
    }
}

#[tokio::test(flavor = "current_thread")]
async fn targets_overlap_but_events_remain_in_configuration_order() {
    let targets = vec![ip("192.0.2.1"), ip("192.0.2.2"), ip("192.0.2.3")];
    let transport = Arc::new(FakeTransport::new([
        (targets[0], behavior(30, success(targets[0], 0, 30))),
        (targets[1], behavior(20, success(targets[1], 1, 20))),
        (targets[2], behavior(10, success(targets[2], 2, 10))),
    ]));

    let report = measure_round(Arc::clone(&transport), round(targets.clone()))
        .await
        .unwrap();

    assert_eq!(transport.max_active.load(Ordering::SeqCst), 3);
    assert_eq!(report.successful_targets, 3);
    assert_eq!(report.failed_targets, 0);
    assert_eq!(report.exit_status(), PingExitStatus::Success);
    assert_eq!(report.events.len(), 6);
    for (index, pair) in report.events.as_chunks::<2>().0.iter().enumerate() {
        assert_eq!(pair[0].event_kind, EventKind::PingProbe);
        assert_eq!(pair[1].event_kind, EventKind::PingSummary);
        assert_eq!(
            pair[0].target.as_deref(),
            Some(targets[index].to_string()).as_deref()
        );
        assert_eq!(pair[0].sequence, Some(index as u16));
        assert_eq!(pair[0].outcome, Outcome::Success);
        assert_eq!(pair[0].source_ip, Some(ip("192.0.2.10")));
        assert_eq!(pair[0].interface.as_deref(), Some("eth-test"));
        assert_eq!(pair[1].packets_sent, Some(1));
        assert_eq!(pair[1].packets_received, Some(1));
        assert_eq!(pair[1].packet_loss_pct, Some(0.0));
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a_round_snapshots_load_context_for_probe_and_summary_rows() {
    let target = ip("192.0.2.1");
    let transport = Arc::new(FakeTransport::new([(
        target,
        behavior(1, success(target, 0, 10)),
    )]));
    let mut request = round(vec![target]);
    request.load_phase = Some(LoadPhase::Download);
    request.load_run_id = Some("run-test:bandwidth:0".into());

    let report = measure_round(transport, request).await.unwrap();

    assert_eq!(report.events.len(), 2);
    assert!(report.events.iter().all(|event| {
        event.load_phase == Some(LoadPhase::Download)
            && event.load_run_id.as_deref() == Some("run-test:bandwidth:0")
    }));
}

#[tokio::test(flavor = "current_thread")]
async fn timeout_unreachable_permission_and_cancelled_are_detailed() {
    let targets = vec![
        ip("192.0.2.1"),
        ip("192.0.2.2"),
        ip("192.0.2.3"),
        ip("192.0.2.4"),
    ];
    let failures = [
        ProbeFailure::Timeout,
        ProbeFailure::Unreachable {
            icmp_type: 3,
            icmp_code: 1,
            message: "host unreachable".into(),
        },
        ProbeFailure::PermissionDenied {
            os_error_code: Some(13),
            message: "denied".into(),
        },
        ProbeFailure::Cancelled,
    ];
    let transport = Arc::new(FakeTransport::new(targets.iter().zip(failures).map(
        |(target, failure)| {
            (
                *target,
                behavior(
                    1,
                    ProbeAttemptResult {
                        binding: binding("192.0.2.10"),
                        sent: !matches!(failure, ProbeFailure::PermissionDenied { .. }),
                        result: Err(failure),
                    },
                ),
            )
        },
    )));

    let report = measure_round(transport, round(targets)).await.unwrap();
    assert_eq!(report.failed_targets, 4);
    assert_eq!(report.exit_status(), PingExitStatus::PartialFailure);
    let probes = report
        .events
        .iter()
        .filter(|event| event.event_kind == EventKind::PingProbe)
        .collect::<Vec<_>>();
    assert_eq!(probes[0].outcome, Outcome::Timeout);
    assert_eq!(probes[0].error_kind, Some(ErrorKind::IcmpTimeout));
    assert_eq!(probes[1].outcome, Outcome::Unreachable);
    assert_eq!(probes[1].icmp_type, Some(3));
    assert_eq!(probes[1].icmp_code, Some(1));
    assert_eq!(probes[2].outcome, Outcome::PermissionDenied);
    assert_eq!(probes[2].os_error_code, Some(13));
    assert!(
        probes[2]
            .error_message
            .as_deref()
            .unwrap()
            .contains("ping_group_range")
    );
    assert_eq!(probes[3].outcome, Outcome::Cancelled);
    assert!(probes.iter().all(|event| event.duration_ms.is_some()));
    assert!(probes.iter().all(|event| event.started_at_utc.is_some()));
    assert!(probes.iter().all(|event| event.finished_at_utc.is_some()));
}

#[tokio::test(flavor = "current_thread")]
async fn mismatched_target_identifier_or_sequence_is_a_protocol_failure() {
    let targets = vec![ip("192.0.2.1"), ip("192.0.2.2"), ip("192.0.2.3")];
    let mut wrong_target = success(ip("198.51.100.10"), 0, 1);
    wrong_target.result.as_mut().unwrap().identifier = Some(42);
    let mut wrong_identifier = success(targets[1], 1, 1);
    wrong_identifier.result.as_mut().unwrap().identifier = Some(99);
    let wrong_sequence = success(targets[2], 99, 1);
    let transport = Arc::new(FakeTransport::new([
        (targets[0], behavior(1, wrong_target)),
        (targets[1], behavior(1, wrong_identifier)),
        (targets[2], behavior(1, wrong_sequence)),
    ]));

    let report = measure_round(transport, round(targets)).await.unwrap();
    for probe in report
        .events
        .iter()
        .filter(|event| event.event_kind == EventKind::PingProbe)
    {
        assert_eq!(probe.outcome, Outcome::Error);
        assert_eq!(probe.error_kind, Some(ErrorKind::Protocol));
    }
}

#[tokio::test(flavor = "current_thread")]
async fn an_icmp_error_reply_is_recorded_as_unreachable() {
    let target = ip("192.0.2.1");
    let mut attempt = success(target, 0, 1);
    let reply = attempt.result.as_mut().unwrap();
    reply.icmp_type = 3;
    reply.icmp_code = 1;
    let transport = Arc::new(FakeTransport::new([(target, behavior(1, attempt))]));

    let report = measure_round(transport, round(vec![target])).await.unwrap();
    let probe = &report.events[0];
    assert_eq!(probe.outcome, Outcome::Unreachable);
    assert_eq!(probe.error_kind, Some(ErrorKind::IcmpUnreachable));
    assert_eq!(probe.icmp_type, Some(3));
    assert_eq!(probe.icmp_code, Some(1));
}

fn context(root: PathBuf) -> ResolveContext {
    ResolveContext {
        stdout_is_terminal: false,
        current_dir: root.clone(),
        state_dir: root.join("state"),
    }
}

async fn execute_mode(mode: ConsoleMode) -> (String, Vec<csv::StringRecord>, PingExitStatus) {
    let dir = tempdir().unwrap();
    let output = dir.path().join("ping.csv");
    let cli = Cli::try_parse_from([
        "netband",
        "--output",
        output.to_str().unwrap(),
        "--ping-target",
        "192.0.2.1",
        "--ping-target",
        "192.0.2.2",
        "--console",
        match mode {
            ConsoleMode::Human => "human",
            ConsoleMode::Jsonl => "jsonl",
            ConsoleMode::Off => "off",
            ConsoleMode::Auto => "auto",
        },
        "once",
        "ping",
    ])
    .unwrap();
    let config = resolve(&cli, &context(dir.path().to_path_buf())).unwrap();
    assert!(matches!(config.output, OutputTarget::File(_)));
    let targets = config.ping.targets.clone();
    let transport = Arc::new(FakeTransport::new([
        (targets[0], behavior(1, success(targets[0], 0, 12))),
        (
            targets[1],
            behavior(
                1,
                ProbeAttemptResult {
                    binding: binding("192.0.2.10"),
                    sent: true,
                    result: Err(ProbeFailure::Timeout),
                },
            ),
        ),
    ]));
    let (writer, mut reader) = tokio::io::duplex(64 * 1024);
    let execution = execute_ping_once(&config, transport, writer).await.unwrap();
    let mut console = String::new();
    reader.read_to_string(&mut console).await.unwrap();
    let records = csv::Reader::from_path(execution.output_path)
        .unwrap()
        .records()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    (console, records, execution.exit_status)
}

#[tokio::test(flavor = "current_thread")]
async fn one_shot_cli_pipeline_records_all_rows_and_separates_console_modes() {
    let (human, human_csv, status) = execute_mode(ConsoleMode::Human).await;
    assert_eq!(status, PingExitStatus::PartialFailure);
    assert_eq!(status.code(), 1);
    assert_eq!(human.lines().count(), 2);
    assert!(human.contains("outcome=success"));
    assert!(human.contains("outcome=timeout"));
    assert_eq!(human_csv.len(), 4);

    let (jsonl, jsonl_csv, _) = execute_mode(ConsoleMode::Jsonl).await;
    assert_eq!(jsonl.lines().count(), 4);
    for line in jsonl.lines() {
        serde_json::from_str::<serde_json::Value>(line).unwrap();
    }
    assert!(jsonl.contains("\"event_kind\":\"ping_probe\""));
    assert!(jsonl.contains("\"error_kind\":\"icmp_timeout\""));
    assert_eq!(jsonl_csv.len(), 4);

    let (off, off_csv, _) = execute_mode(ConsoleMode::Off).await;
    assert!(off.is_empty());
    assert_eq!(off_csv.len(), 4);
}
