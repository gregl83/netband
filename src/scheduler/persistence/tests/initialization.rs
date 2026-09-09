use super::*;
#[test]
fn new_state_has_a_checkpoint_and_empty_accounting_before_first_start() {
    let root = TempDir::new().unwrap();
    let mut scheduler = open(&root).unwrap();
    let checkpoint: Checkpoint = parse(
        &path(&root),
        &fs::read(path(&root).with_extension("initialized")).unwrap(),
    )
    .unwrap();
    let log = read_log(
        &path(&root),
        &fs::read(path(&root).with_extension("accounting.jsonl")).unwrap(),
    )
    .unwrap();
    assert_eq!(checkpoint.sequence, 1);
    assert!(scheduler.snapshot().runs.is_empty());
    assert!(log.states[0].providers["mlab"].runs.is_empty());
    scheduler.flush().unwrap();
    drop(scheduler);
    let mut scheduler = open(&root).unwrap();
    assert_eq!(scheduler.reserve_run(now()).unwrap().daily_runs_used, 1);
    drop(scheduler);
    assert_eq!(open(&root).unwrap().snapshot().runs, vec![now()]);
}
#[test]
fn interrupted_initialization_resumes_or_rejects_an_incomplete_log() {
    for kind in [FileKind::Ledger, FileKind::Checkpoint, FileKind::Snapshot] {
        for step in STEPS {
            for skip in 0..=usize::from(kind != FileKind::Snapshot) {
                if kind == FileKind::Ledger
                    && skip == 1
                    && matches!(step, Step::Rename | Step::DirectorySync)
                {
                    continue;
                }
                let root = TempDir::new().unwrap();
                let fault = Inject::at(kind, step, skip);
                assert!(open(&root).is_err(), "{kind:?}/{step:?}/{skip}");
                drop(fault);
                if kind == FileKind::Ledger && step == Step::PartialWrite && skip == 1 {
                    assert_rejected_unchanged(&root);
                } else {
                    let mut scheduler =
                        open(&root).unwrap_or_else(|e| panic!("{kind:?}/{step:?}/{skip}: {e}"));
                    assert_eq!(scheduler.reserve_run(now()).unwrap().daily_runs_used, 1);
                }
            }
        }
    }
}
#[test]
fn orphan_artifacts_are_not_treated_as_a_new_installation() {
    for extension in ["bak", "initialized", "accounting.jsonl"] {
        let root = TempDir::new().unwrap();
        File::create(path(&root).with_extension("lock")).unwrap();
        fs::write(path(&root).with_extension(extension), b"broken").unwrap();
        assert_rejected_unchanged(&root);
    }
}
#[test]
fn competing_owner_is_rejected_without_changing_accounting() {
    let root = TempDir::new().unwrap();
    let scheduler = open(&root).unwrap();
    let before = contents(&root);
    assert!(matches!(open(&root), Err(SchedulerError::Locked(_))));
    assert_eq!(contents(&root), before);
    drop(scheduler);
    open(&root).unwrap();
}

#[test]
fn conflicting_state_filenames_are_rejected_before_creating_files() {
    for extension in ["lock", "bak", "initialized"] {
        let root = TempDir::new().unwrap();
        assert!(
            Scheduler::open_seeded(
                path(&root).with_extension(extension),
                &config(&root),
                now(),
                7
            )
            .is_err()
        );
        assert!(contents(&root).is_empty());
    }
}
