use super::*;
#[test]
fn stale_backup_replays_reservations_and_spacing_idempotently() {
    let root = TempDir::new().unwrap();
    let mut scheduler = open(&root).unwrap();
    scheduler.reserve_run(now()).unwrap();
    drop(scheduler);
    fs::copy(path(&root).with_extension("bak"), path(&root)).unwrap();
    for _ in 0..2 {
        let mut recovered = open(&root).unwrap();
        assert_eq!(recovered.snapshot().runs, vec![now()]);
        assert_eq!(recovered.snapshot().last_started_at_utc, Some(now()));
        assert!(matches!(
            recovered.preflight_manual("test", now()).unwrap(),
            ManualDecision::Blocked(_)
        ));
    }
}
#[test]
fn missing_empty_truncated_and_corrupt_evidence_fail_without_mutation() {
    for extension in ["initialized", "accounting.jsonl"] {
        for damage in ["missing", "empty", "partial", "invalid", "suffix"] {
            let root = TempDir::new().unwrap();
            let mut scheduler = open(&root).unwrap();
            scheduler.reserve_run(now()).unwrap();
            drop(scheduler);
            fs::copy(path(&root).with_extension("bak"), path(&root)).unwrap();
            let file = path(&root).with_extension(extension);
            let bytes = fs::read(&file).unwrap();
            match damage {
                "missing" => fs::remove_file(&file).unwrap(),
                "empty" => fs::write(&file, b"").unwrap(),
                "partial" => fs::write(&file, &bytes[..bytes.len() / 2]).unwrap(),
                "invalid" => fs::write(&file, b"invalid\n").unwrap(),
                _ if extension == "accounting.jsonl" => {
                    let previous_end = bytes[..bytes.len() - 1]
                        .iter()
                        .rposition(|b| *b == b'\n')
                        .unwrap();
                    fs::write(&file, &bytes[..=previous_end]).unwrap();
                }
                _ => fs::write(&file, b"{}\n").unwrap(),
            }
            assert_rejected_unchanged(&root);
        }
    }
}
#[test]
fn mismatched_installations_and_snapshot_accounting_are_rejected() {
    for extension in ["initialized", "accounting.jsonl", "json"] {
        let root = TempDir::new().unwrap();
        let other = TempDir::new().unwrap();
        open(&root).unwrap();
        open(&other).unwrap();
        fs::copy(
            path(&other).with_extension(extension),
            path(&root).with_extension(extension),
        )
        .unwrap();
        assert_rejected_unchanged(&root);
    }
    let root = TempDir::new().unwrap();
    open(&root).unwrap();
    let mut state: SchedulerStore =
        parse_store(&path(&root), &fs::read(path(&root)).unwrap()).unwrap();
    state.providers.get_mut("mlab").unwrap().cooldown_until_utc = Some(now());
    fs::write(path(&root), json(&state)).unwrap();
    assert_rejected_unchanged(&root);
}
#[test]
fn malformed_or_missing_primary_requires_explicit_backup_restoration() {
    for missing in [false, true] {
        let root = TempDir::new().unwrap();
        let mut scheduler = open(&root).unwrap();
        scheduler.reserve_run(now()).unwrap();
        drop(scheduler);
        if missing {
            fs::remove_file(path(&root)).unwrap();
        } else {
            fs::write(path(&root), b"not json").unwrap();
        }
        assert_rejected_unchanged(&root);
        fs::copy(path(&root).with_extension("bak"), path(&root)).unwrap();
        assert_eq!(open(&root).unwrap().snapshot().runs.len(), 1);
    }
}

fn record_cooldown(scheduler: &mut Scheduler, time: DateTime<Utc>, seconds: Option<u64>) {
    use crate::bandwidth::BandwidthReport;
    use crate::model::{EventKind, MeasurementEvent, Outcome, RequestStage, TriggerReason};
    use crate::scheduler::BandwidthOpportunity;
    let mut event = MeasurementEvent::new(
        "test",
        "retry",
        EventKind::RequestFailure,
        Outcome::RateLimited,
        time,
    );
    event.request_stage = Some(RequestStage::Locate);
    event.http_status = Some(429);
    event.retry_after_ms = seconds.map(|seconds| seconds * 1000);
    event.rate_limit_until_utc = seconds.map(|seconds| time + TimeDelta::seconds(seconds as i64));
    let mut report = BandwidthReport {
        events: vec![event],
        outcome: Outcome::RateLimited,
        reserved: false,
        reservation_error: None,
    };
    scheduler
        .finish_attempt(
            "test",
            time,
            BandwidthOpportunity {
                reason: TriggerReason::Scheduled,
                scheduled_at_utc: time,
                interface: None,
            },
            &mut report,
        )
        .unwrap();
}

#[test]
fn stale_backup_preserves_provider_cooldown_across_midnight_and_restart() {
    for retry in [Some(300), Some(2 * 86400), None] {
        let root = TempDir::new().unwrap();
        let before_midnight = Utc.with_ymd_and_hms(2026, 9, 8, 23, 59, 59).unwrap();
        let mut scheduler =
            Scheduler::open_seeded(path(&root), &config(&root), before_midnight, 7).unwrap();
        let backup = fs::read(path(&root)).unwrap();
        record_cooldown(&mut scheduler, before_midnight, retry);
        let deadline = scheduler.snapshot().cooldown_until_utc.unwrap();
        if let Some(seconds) = retry {
            assert_eq!(
                deadline,
                before_midnight + TimeDelta::seconds(seconds as i64)
            );
        }
        assert!(deadline > before_midnight + TimeDelta::seconds(1));
        drop(scheduler);
        fs::write(path(&root), backup).unwrap();
        for time in [
            before_midnight + TimeDelta::seconds(2),
            deadline - TimeDelta::seconds(1),
        ] {
            let mut recovered =
                Scheduler::open_seeded(path(&root), &config(&root), time, 7).unwrap();
            assert_eq!(recovered.snapshot().cooldown_until_utc, Some(deadline));
            assert!(matches!(
                recovered.preflight_manual("test", time).unwrap(),
                ManualDecision::Blocked(_)
            ));
        }
        let mut recovered =
            Scheduler::open_seeded(path(&root), &config(&root), deadline, 7).unwrap();
        assert!(matches!(
            recovered.preflight_manual("test", deadline).unwrap(),
            ManualDecision::Allowed
        ));
    }
}

#[test]
fn replay_restores_all_providers_and_preserves_duplicate_start_timestamps() {
    let root = TempDir::new().unwrap();
    let mut first_config = config(&root);
    first_config.force_limits = true;
    let mut first = Scheduler::open_seeded(path(&root), &first_config, now(), 7).unwrap();
    let backup = fs::read(path(&root)).unwrap();
    for expected in 1..=4 {
        assert_eq!(first.reserve_run(now()).unwrap().daily_runs_used, expected);
    }
    drop(first);
    let mut second_config = first_config.clone();
    second_config.provider_id = "other".to_owned();
    let mut second = Scheduler::open_seeded(path(&root), &second_config, now(), 7).unwrap();
    second.reserve_run(now()).unwrap();
    drop(second);
    fs::write(path(&root), backup).unwrap();
    let mut first = Scheduler::open_seeded(path(&root), &first_config, now(), 7).unwrap();
    assert_eq!(first.snapshot().runs.len(), 4);
    assert!(first.reserve_run(now()).is_err());
    drop(first);
    let second = Scheduler::open_seeded(path(&root), &second_config, now(), 7).unwrap();
    assert_eq!(second.snapshot().runs, vec![now()]);
}

#[test]
fn ledger_record_corruption_is_detected_even_beyond_the_checkpoint() {
    for uncheckpointed in [false, true] {
        for damage in [
            "digest",
            "sequence",
            "chain",
            "state",
            "duplicate",
            "blank",
            "schema",
        ] {
            let root = TempDir::new().unwrap();
            let mut scheduler = open(&root).unwrap();
            let checkpoint = fs::read(path(&root).with_extension("initialized")).unwrap();
            let snapshot = fs::read(path(&root)).unwrap();
            scheduler.reserve_run(now()).unwrap();
            drop(scheduler);
            if uncheckpointed {
                fs::write(path(&root).with_extension("initialized"), checkpoint).unwrap();
                fs::write(path(&root), snapshot).unwrap();
            }
            let ledger = path(&root).with_extension("accounting.jsonl");
            let text = fs::read_to_string(&ledger).unwrap();
            let mut lines: Vec<_> = text.lines().map(str::to_owned).collect();
            let last = lines.last_mut().unwrap();
            let mut record: Record = parse(&ledger, last.as_bytes()).unwrap();
            match damage {
                "digest" => record.digest = "0".repeat(64),
                "sequence" => record.sequence += 1,
                "chain" => record.previous_digest = "0".repeat(64),
                "state" => record.state.providers.get_mut("mlab").unwrap().runs.clear(),
                "schema" => record.state.schema_version = 99,
                _ => {}
            }
            *last = String::from_utf8(json(&record)).unwrap();
            if damage == "duplicate" {
                lines.push(lines.last().unwrap().clone());
            }
            if damage == "blank" {
                lines.push(String::new());
            }
            fs::write(ledger, format!("{}\n", lines.join("\n"))).unwrap();
            assert_rejected_unchanged(&root);
        }
    }
}

#[test]
fn unsupported_schemas_and_checkpoint_variants_fail_closed() {
    for target in ["snapshot", "checkpoint", "header"] {
        for field in ["version", "identity", "sequence", "digest"] {
            let root = TempDir::new().unwrap();
            open(&root).unwrap();
            let file = path(&root).with_extension(match target {
                "snapshot" => "json",
                "checkpoint" => "initialized",
                _ => "accounting.jsonl",
            });
            let bytes = fs::read(&file).unwrap();
            if target == "snapshot" {
                let mut state: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                match field {
                    "version" => state["schema_version"] = 99.into(),
                    "identity" => state["checkpoint"] = serde_json::Value::Null,
                    "sequence" => state["checkpoint"]["sequence"] = u64::MAX.into(),
                    _ => state["providers"]["mlab"]["backoff_step"] = 99.into(),
                }
                fs::write(file, json(&state)).unwrap();
            } else if target == "checkpoint" {
                let mut checkpoint: Checkpoint = parse(&file, &bytes).unwrap();
                match field {
                    "version" => checkpoint.version = 99,
                    "identity" => checkpoint.installation = "0".repeat(32),
                    "sequence" => checkpoint.sequence = u64::MAX,
                    _ => checkpoint.digest = "0".repeat(64),
                }
                fs::write(file, json(&checkpoint)).unwrap();
            } else {
                let end = bytes.iter().position(|b| *b == b'\n').unwrap();
                let mut header: Header = parse(&file, &bytes[..end]).unwrap();
                if field == "version" {
                    header.version = 99;
                } else {
                    header.installation = "invalid".to_owned();
                }
                let mut changed = json(&header);
                changed.extend_from_slice(&bytes[end..]);
                fs::write(file, changed).unwrap();
            }
            assert_rejected_unchanged(&root);
        }
    }
}

#[test]
fn io_errors_are_reported_without_reinitializing_state() {
    for extension in ["json", "initialized", "accounting.jsonl", "lock"] {
        let root = TempDir::new().unwrap();
        open(&root).unwrap();
        let file = path(&root).with_extension(extension);
        fs::remove_file(&file).unwrap();
        fs::create_dir(&file).unwrap();
        assert!(matches!(open(&root), Err(SchedulerError::Io { .. })));
        assert!(file.is_dir());
    }
}

#[test]
fn retry_exhaustion_survives_replay_when_deadline_and_backoff_are_unchanged() {
    let root = TempDir::new().unwrap();
    let mut scheduler = open(&root).unwrap();
    for _ in 0..4 {
        record_cooldown(&mut scheduler, now(), Some(0));
    }
    assert_eq!(scheduler.snapshot().deferred_attempts, Some(4));
    let backup = fs::read(path(&root)).unwrap();
    let before = scheduler.snapshot();
    record_cooldown(&mut scheduler, now(), Some(0));
    assert_eq!(
        scheduler.snapshot().cooldown_until_utc,
        before.cooldown_until_utc
    );
    assert_eq!(scheduler.snapshot().backoff_step, before.backoff_step);
    assert!(scheduler.snapshot().deferred_attempts.is_none());
    drop(scheduler);
    fs::write(path(&root), backup).unwrap();
    assert!(open(&root).unwrap().snapshot().deferred_attempts.is_none());
}
