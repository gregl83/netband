use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{TimeZone, Utc};
use clap::Parser;
use netband::bandwidth::BandwidthReport;
use netband::cli::Cli;
use netband::config::{ProviderConfig, ResolveContext, ResolvedConfig, resolve};
use netband::health::{DegradationReason, HealthDecision, HealthSnapshot};
use netband::journal::CSV_HEADER;
use netband::model::{EventKind, MeasurementEvent, Outcome, RequestStage, TriggerReason};
use netband::scheduler::{ManualDecision, Scheduler};
use tempfile::tempdir;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn load_config(path: &Path) -> ResolvedConfig {
    let cli = Cli::try_parse_from([
        "netband",
        "--config",
        path.to_str().unwrap(),
        "config",
        "check",
    ])
    .unwrap();
    resolve(
        &cli,
        &ResolveContext {
            stdout_is_terminal: false,
            current_dir: root(),
            state_dir: root().join(".netband/state"),
        },
    )
    .unwrap()
}

#[test]
fn checked_in_configs_use_the_real_loader_and_safe_provider_identities() {
    let complete = load_config(&root().join("examples/netband.toml"));
    assert_eq!(complete.ping.targets.len(), 3);
    assert_eq!(complete.bandwidth.daily_max, 4);
    assert_eq!(complete.bandwidth.min_spacing, Duration::from_secs(36 * 60));
    assert!(!complete.bandwidth.automatic_enabled);

    let service = load_config(&root().join("packaging/netband.toml"));
    assert_eq!(service.shutdown_grace, Duration::from_secs(30));
    assert!(!service.bandwidth.automatic_enabled);

    let mlab = load_config(&root().join("examples/mlab.toml"));
    let direct_path = root().join("examples/direct.toml");
    let direct_text = fs::read_to_string(&direct_path).unwrap();
    let direct = load_config(&direct_path);
    assert!(matches!(mlab.bandwidth.provider, ProviderConfig::Mlab(_)));
    assert!(matches!(
        direct.bandwidth.provider,
        ProviderConfig::Direct(_)
    ));
    assert_eq!(mlab.bandwidth.provider_id, "mlab");
    assert!(direct.bandwidth.provider_id.starts_with("direct:"));
    assert_ne!(mlab.bandwidth.provider_id, direct.bandwidth.provider_id);
    assert!(direct_text.contains(".invalid"));
    assert!(!direct_text.contains("access_token"));
    assert!(!direct_text.contains("akamai"));
}

#[test]
fn published_ndt7_validation_dataset_is_complete_and_sanitized() {
    let path = root().join("docs/benchmarks/2026-09-03-akamai.csv");
    let mut reader = csv::Reader::from_path(path).unwrap();
    let headers = reader.headers().unwrap().clone();
    assert_eq!(
        headers.iter().collect::<Vec<_>>(),
        [
            "pair",
            "first_client",
            "netband_download_mbps",
            "netband_upload_mbps",
            "reference_download_mbps",
            "reference_upload_mbps",
            "netband_upload_end",
        ]
    );
    assert!(headers.iter().all(|field| {
        !field.contains("ip") && !field.contains("address") && !field.contains("timestamp")
    }));

    let rows = reader.records().collect::<Result<Vec<_>, _>>().unwrap();
    assert_eq!(rows.len(), 20);
    let mut warning_count = 0;
    for (index, row) in rows.iter().enumerate() {
        let pair = index + 1;
        assert_eq!(row.get(0).unwrap().parse::<usize>().unwrap(), pair);
        let expected_first = if pair % 2 == 1 {
            "netband"
        } else {
            "reference"
        };
        assert_eq!(row.get(1), Some(expected_first));
        for field in 2..=5 {
            let value = row.get(field).unwrap().parse::<f64>().unwrap();
            assert!(value.is_finite() && value > 0.0);
        }
        match row.get(6).unwrap() {
            "clean" => {}
            "broken_pipe" | "connection_reset" => warning_count += 1,
            status => panic!("unexpected upload-end status {status}"),
        }
    }
    assert_eq!(warning_count, 9);

    fn median(mut values: Vec<f64>) -> f64 {
        values.sort_by(f64::total_cmp);
        (values[values.len() / 2 - 1] + values[values.len() / 2]) / 2.0
    }
    let values = |field: usize| {
        rows.iter()
            .map(|row| row.get(field).unwrap().parse::<f64>().unwrap())
            .collect::<Vec<_>>()
    };
    assert_eq!(format!("{:.2}", median(values(2))), "21.84");
    assert_eq!(format!("{:.2}", median(values(3))), "17.97");
    assert_eq!(format!("{:.2}", median(values(4))), "22.24");
    assert_eq!(format!("{:.2}", median(values(5))), "16.62");

    let readme = fs::read_to_string(root().join("README.md")).unwrap();
    let validation = fs::read_to_string(root().join("docs/ndt7-validation.md")).unwrap();
    for documented in ["21.84", "17.97", "22.24", "16.62"] {
        assert!(readme.contains(documented));
        assert!(validation.contains(documented));
    }
    assert!(readme.contains("docs/ndt7-validation.md"));
    assert!(validation.contains("benchmarks/2026-09-03-akamai.csv"));
    assert!(validation.contains("actual server endpoint and client"));
    assert!(validation.contains("ndt.example.com"));
}

#[test]
fn reference_docs_track_every_cli_option_schema_field_and_policy_link() {
    let readme = fs::read_to_string(root().join("README.md")).unwrap();
    let configuration = fs::read_to_string(root().join("docs/configuration.md")).unwrap();
    let data = fs::read_to_string(root().join("docs/data-format.md")).unwrap();
    let privacy = fs::read_to_string(root().join("PRIVACY.md")).unwrap();

    for flag in [
        "--config",
        "--console",
        "--interface",
        "--ping-target",
        "--ping-interval",
        "--ping-timeout",
        "--no-bandwidth",
        "--output",
        "--output-dir",
        "--state-file",
        "--shutdown-grace",
        "--verbosity",
        "--ndt-provider",
        "--mlab-locate-url",
        "--ndt-target",
        "--ndt-download-url",
        "--ndt-upload-url",
        "--ndt-tls-server-name",
        "--ndt-ca-cert",
        "--allow-insecure-ndt",
        "--bandwidth-daily-max",
        "--bandwidth-min-spacing",
        "--bandwidth-slot-jitter-pct",
        "--bandwidth-timeout",
        "--bandwidth-shutdown-margin",
        "--loss-window-rounds",
        "--loss-min-samples",
        "--loss-threshold-pct",
        "--rtt-threshold-ms",
        "--recovery-loss-pct",
        "--recovery-rounds",
        "--pending-trigger-ttl",
        "--cooldown-initial",
        "--cooldown-max",
        "--accept-mlab-policy",
    ] {
        assert!(
            configuration.contains(flag),
            "missing documented flag {flag}"
        );
    }

    assert!(data.contains(CSV_HEADER));
    assert_eq!(CSV_HEADER.split(',').count(), 42);
    for mode in ["auto", "human", "jsonl", "off"] {
        assert!(readme.contains(mode), "README omits console mode {mode}");
    }
    assert!(readme.contains(">events.jsonl 2>netband.log"));
    assert!(privacy.contains("https://www.measurementlab.net/aup/"));
    assert!(privacy.contains("https://www.measurementlab.net/privacy/"));
}

#[test]
fn documented_schedule_trigger_cap_and_cooldown_are_executable() {
    let docs = fs::read_to_string(root().join("docs/scheduling.md")).unwrap();
    let config = load_config(&root().join("examples/mlab.toml"));
    let now = Utc.with_ymd_and_hms(2026, 8, 30, 0, 0, 0).unwrap();
    let work = tempdir().unwrap();
    let state = work.path().join("scheduler.json");
    let mut scheduler = Scheduler::open_seeded(&state, &config.bandwidth, now, 7).unwrap();
    let expected = [
        "2026-08-30T02:54:48.327Z",
        "2026-08-30T10:24:11.652Z",
        "2026-08-30T15:32:43.743Z",
        "2026-08-30T21:16:35.107Z",
    ];
    let slots = scheduler.snapshot().slots;
    assert_eq!(slots.len(), expected.len());
    for (slot, documented) in slots.iter().zip(expected) {
        assert_eq!(
            slot.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            documented
        );
        assert!(docs.contains(documented));
    }

    let trigger_at = Utc.with_ymd_and_hms(2026, 8, 30, 1, 0, 0).unwrap();
    scheduler
        .observe_health(
            "docs",
            trigger_at,
            HealthDecision::Degraded {
                snapshot: HealthSnapshot {
                    attempted: 6,
                    successful: 3,
                    loss_pct: 50.0,
                    p95_rtt_ms: Some(25.0),
                    sufficient_samples: true,
                },
                reason: DegradationReason::Loss,
            },
        )
        .unwrap();
    let opportunity = scheduler
        .poll("docs", trigger_at, true)
        .unwrap()
        .opportunity
        .unwrap();
    assert_eq!(opportunity.reason, TriggerReason::PingLoss);
    scheduler.reserve_run(trigger_at).unwrap();
    let mut success = MeasurementEvent::new(
        "docs",
        "bandwidth:success",
        EventKind::Bandwidth,
        Outcome::Success,
        trigger_at,
    );
    success.remote_ip = Some("192.0.2.1".parse().unwrap());
    let mut report = BandwidthReport {
        events: vec![success],
        outcome: Outcome::Success,
        reserved: true,
        reservation_error: None,
    };
    scheduler
        .finish_attempt("docs", trigger_at, opportunity, &mut report)
        .unwrap();
    assert_eq!(scheduler.snapshot().runs.len(), 1);
    assert_eq!(scheduler.snapshot().slots.len(), 3);

    for hour in 2..=4 {
        scheduler
            .reserve_run(Utc.with_ymd_and_hms(2026, 8, 30, hour, 0, 0).unwrap())
            .unwrap();
    }
    assert!(matches!(
        scheduler
            .preflight_manual("docs", Utc.with_ymd_and_hms(2026, 8, 30, 5, 0, 0).unwrap())
            .unwrap(),
        ManualDecision::Blocked(_)
    ));
    drop(scheduler);

    let cooldown_state = work.path().join("cooldown.json");
    let mut scheduler = Scheduler::open_seeded(cooldown_state, &config.bandwidth, now, 7).unwrap();
    let mut failure = MeasurementEvent::new(
        "docs",
        "request:rate-limit",
        EventKind::RequestFailure,
        Outcome::RateLimited,
        trigger_at,
    );
    failure.request_stage = Some(RequestStage::Locate);
    failure.http_status = Some(429);
    failure.retry_after_ms = Some(120_000);
    let mut report = BandwidthReport {
        events: vec![failure],
        outcome: Outcome::RateLimited,
        reserved: false,
        reservation_error: None,
    };
    scheduler
        .finish_attempt(
            "docs",
            trigger_at,
            netband::scheduler::BandwidthOpportunity {
                reason: TriggerReason::Scheduled,
                scheduled_at_utc: trigger_at,
                interface: None,
            },
            &mut report,
        )
        .unwrap();
    assert_eq!(
        scheduler.snapshot().cooldown_until_utc,
        Some(Utc.with_ymd_and_hms(2026, 8, 30, 1, 2, 0).unwrap())
    );
}
