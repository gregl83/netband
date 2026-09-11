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
    assert_eq!(
        service.output,
        netband::config::OutputTarget::Directory(PathBuf::from("/var/lib/netband/measurements"))
    );
    assert_eq!(service.rotate_max_bytes, Some(67_108_864));
    assert!(
        fs::read_to_string(root().join("packaging/netband.service"))
            .unwrap()
            .contains("StateDirectory=netband netband/measurements")
    );
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
    let directory = root().join("docs/benchmarks/2026-09-06-akamai");
    let mut reader = csv::Reader::from_path(directory.join("measurements.csv")).unwrap();
    assert_eq!(
        reader.headers().unwrap().iter().collect::<Vec<_>>(),
        [
            "pair",
            "position",
            "client",
            "exit_code",
            "outcome",
            "download_mbps",
            "upload_mbps",
            "diagnostic",
        ]
    );
    let rows = reader.records().collect::<Result<Vec<_>, _>>().unwrap();
    assert_eq!(rows.len(), 40);
    for (index, row) in rows.iter().enumerate() {
        let pair = index / 2 + 1;
        let first = index % 2 == 0;
        assert_eq!(row[0].parse::<usize>().unwrap(), pair);
        assert_eq!(&row[1], if first { "first" } else { "second" });
        assert_eq!(
            &row[2],
            if (pair % 2 == 1) == first {
                "netband"
            } else {
                "reference"
            }
        );
        row[3].parse::<i32>().unwrap();
        assert!(matches!(
            &row[4],
            "success"
                | "partial"
                | "timeout"
                | "unreachable"
                | "permission_denied"
                | "cancelled"
                | "error"
                | "no_capacity"
                | "rate_limited"
        ));
        for field in 5..=6 {
            if &row[4] == "success" {
                let value = row[field].parse::<f64>().unwrap();
                assert!(value.is_finite() && value > 0.0);
            } else if !row[field].is_empty() {
                assert!(row[field].parse::<f64>().unwrap().is_finite());
            }
        }
        for diagnostic in row[7].split(';').filter(|value| !value.is_empty()) {
            let (kind, code) = diagnostic.split_once(':').unwrap_or((diagnostic, ""));
            assert!(matches!(
                kind,
                "download_failed"
                    | "upload_failed"
                    | "timeout"
                    | "io"
                    | "protocol"
                    | "dns"
                    | "connect"
                    | "tls"
                    | "websocket_handshake"
                    | "http_status"
                    | "cancelled"
                    | "internal"
            ));
            if !code.is_empty() {
                code.parse::<i32>().unwrap();
            }
        }
    }
    let summary: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(directory.join("summary.json")).unwrap()).unwrap();
    let validation = fs::read_to_string(root().join("docs/ndt7-validation.md")).unwrap();
    let readme = fs::read_to_string(root().join("README.md")).unwrap();
    assert!(readme.contains("docs/ndt7-validation.md"));
    assert!(validation.contains("benchmarks/2026-09-06-akamai/measurements.csv"));
    for client in ["netband", "reference"] {
        for (field, direction) in [(5, "download_mbps"), (6, "upload_mbps")] {
            let mut values: Vec<f64> = rows
                .iter()
                .filter(|row| &row[2] == client && &row[3] == "0" && &row[4] == "success")
                .map(|row| row[field].parse().unwrap())
                .collect();
            values.sort_by(f64::total_cmp);
            assert!(!values.is_empty());
            let median = (values[(values.len() - 1) / 2] + values[values.len() / 2]) / 2.0;
            let recorded = &summary["clients"][client][direction];
            assert_eq!(recorded["n"].as_u64().unwrap(), values.len() as u64);
            assert!((recorded["median"].as_f64().unwrap() - median).abs() < 1e-9);
            assert!(
                validation.contains(&format!("{median:.2}")),
                "missing median for {client} {direction}"
            );
        }
    }
    assert!(validation.contains("ndt.example.com"));
}

#[test]
fn measurement_docs_describe_current_behavior_without_old_build_claims() {
    let readme = fs::read_to_string(root().join("README.md")).unwrap();
    let validation = fs::read_to_string(root().join("docs/ndt7-validation.md")).unwrap();
    let release = fs::read_to_string(root().join("docs/release.md")).unwrap();
    for document in [&readme, &validation, &release] {
        for historical in [
            "0.2.0",
            "0.3.0",
            "0.4.0",
            "7531961",
            "revised",
            "Baseline download",
        ] {
            assert!(
                !document.contains(historical),
                "obsolete documentation: {historical}"
            );
        }
    }
    assert!(validation.contains("Upload accepts payloads for ten seconds"));
    assert!(validation.contains("separate two-second allowance"));
    assert!(validation.contains("64 KiB"));
    assert!(validation.contains("twenty pairs"));
    assert!(validation.contains("numerical agreement alone does not prove"));
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
        "--rotate-max-bytes",
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
    assert_eq!(CSV_HEADER.split(',').count(), 52);
    for field in CSV_HEADER.split(',') {
        assert!(
            data.contains(&format!("| `{field}` |")),
            "missing field documentation: {field}"
        );
    }
    let examples = fs::read_to_string(root().join("docs/examples/console.jsonl")).unwrap();
    for line in examples.lines() {
        let row: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(row.as_object().unwrap().len(), 52);
        assert_eq!(row["schema_version"], 1);
        for field in CSV_HEADER.split(',') {
            assert!(row.get(field).is_some(), "missing example field: {field}");
        }
        for (direction, bytes) in [("download", "download_bytes"), ("upload", "upload_bytes")] {
            if let Some(rate) = row[format!("{direction}_mbps")].as_f64() {
                let expected = 8.0 * row[bytes].as_f64().unwrap()
                    / (1000.0 * row[format!("{direction}_duration_ms")].as_f64().unwrap());
                assert!((rate - expected).abs() <= rate.abs() * 1e-12);
            }
        }
    }

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
    success.download_remote_ip = Some("192.0.2.1".parse().unwrap());
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
    failure.rate_limit_until_utc = Some(trigger_at + chrono::TimeDelta::seconds(120));
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
