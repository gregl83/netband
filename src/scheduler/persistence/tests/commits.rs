use super::*;
#[test]
fn reservation_commit_failures_never_return_admission_or_refund_written_records() {
    for kind in [
        FileKind::Ledger,
        FileKind::Checkpoint,
        FileKind::Backup,
        FileKind::Snapshot,
    ] {
        for step in STEPS {
            if kind == FileKind::Ledger && matches!(step, Step::Rename | Step::DirectorySync) {
                continue;
            }
            let root = TempDir::new().unwrap();
            let mut scheduler = open(&root).unwrap();
            let fault = Inject::at(kind, step, 0);
            assert!(scheduler.reserve_run(now()).is_err(), "{kind:?}/{step:?}");
            drop(fault);
            let failed = contents(&root);
            assert!(scheduler.reserve_run(now() + TimeDelta::hours(1)).is_err());
            assert!(scheduler.flush().is_err());
            assert_eq!(
                contents(&root),
                failed,
                "poisoned scheduler wrote more data"
            );
            drop(scheduler);
            if kind == FileKind::Ledger && step == Step::PartialWrite {
                assert_rejected_unchanged(&root);
            } else {
                let recovered = open(&root).unwrap();
                let expected = usize::from(!(kind == FileKind::Ledger && step == Step::Write));
                assert_eq!(
                    recovered.snapshot().runs.len(),
                    expected,
                    "{kind:?}/{step:?}"
                );
            }
        }
    }
}
#[test]
fn ordinary_ping_state_flushes_do_not_append_accounting_records() {
    let root = TempDir::new().unwrap();
    let mut scheduler = open(&root).unwrap();
    let ledger = path(&root).with_extension("accounting.jsonl");
    let before = fs::read(&ledger).unwrap();
    scheduler
        .preflight_manual("test", now() + TimeDelta::minutes(1))
        .unwrap();
    scheduler.flush().unwrap();
    assert_eq!(fs::read(&ledger).unwrap(), before);
}
#[test]
fn missing_ledger_during_reservation_is_fatal_and_not_recreated() {
    let root = TempDir::new().unwrap();
    let mut scheduler = open(&root).unwrap();
    let ledger = path(&root).with_extension("accounting.jsonl");
    fs::remove_file(&ledger).unwrap();
    assert!(scheduler.reserve_run(now()).is_err());
    assert!(!ledger.exists());
    assert!(scheduler.flush().is_err());
}

#[test]
fn failed_cooldown_commit_replays_complete_evidence_or_refuses_recovery() {
    for kind in [
        FileKind::Ledger,
        FileKind::Checkpoint,
        FileKind::Backup,
        FileKind::Snapshot,
    ] {
        for step in STEPS {
            if kind == FileKind::Ledger
                && matches!(step, Step::Write | Step::Rename | Step::DirectorySync)
            {
                continue;
            }
            let root = TempDir::new().unwrap();
            let mut scheduler = open(&root).unwrap();
            let deadline = now() + TimeDelta::days(2);
            scheduler.state_mut().cooldown_until_utc = Some(deadline);
            scheduler.state_mut().backoff_step = 1;
            let fault = Inject::at(kind, step, 0);
            assert!(scheduler.persist().is_err());
            drop(fault);
            drop(scheduler);
            if kind == FileKind::Ledger && step == Step::PartialWrite {
                assert_rejected_unchanged(&root);
            } else {
                let mut recovered = open(&root).unwrap();
                assert_eq!(recovered.snapshot().cooldown_until_utc, Some(deadline));
                assert!(matches!(
                    recovered.preflight_manual("test", now()).unwrap(),
                    ManualDecision::Blocked(_)
                ));
            }
        }
    }
}

#[test]
fn failed_recovery_checkpoint_update_is_retryable_without_refunding() {
    for step in STEPS {
        let root = TempDir::new().unwrap();
        let mut scheduler = open(&root).unwrap();
        let checkpoint = fs::read(path(&root).with_extension("initialized")).unwrap();
        scheduler.reserve_run(now()).unwrap();
        drop(scheduler);
        fs::write(path(&root).with_extension("initialized"), checkpoint).unwrap();
        fs::copy(path(&root).with_extension("bak"), path(&root)).unwrap();
        let fault = Inject::at(FileKind::Checkpoint, step, 0);
        assert!(open(&root).is_err());
        drop(fault);
        assert_eq!(open(&root).unwrap().snapshot().runs, vec![now()]);
    }
}

#[test]
fn live_file_replacement_and_truncation_fail_before_appending_a_reservation() {
    for extension in ["json", "initialized", "accounting.jsonl"] {
        let root = TempDir::new().unwrap();
        let mut scheduler = open(&root).unwrap();
        fs::write(path(&root).with_extension(extension), b"changed").unwrap();
        let before = contents(&root);
        assert!(scheduler.reserve_run(now()).is_err());
        assert_eq!(contents(&root), before);
    }
}

#[test]
fn invalid_in_memory_state_and_sequence_overflow_do_not_write() {
    for overflow in [false, true] {
        let root = TempDir::new().unwrap();
        let mut scheduler = open(&root).unwrap();
        if overflow {
            scheduler.persistence.checkpoint.sequence = u64::MAX;
            fs::write(
                path(&root).with_extension("initialized"),
                json(&scheduler.persistence.checkpoint),
            )
            .unwrap();
        } else {
            scheduler.state_mut().rng_state = 0;
        }
        let before = contents(&root);
        assert!(scheduler.reserve_run(now()).is_err());
        assert_eq!(contents(&root), before);
    }
}
