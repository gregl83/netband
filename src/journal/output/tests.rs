use std::cell::Cell;

use chrono::{TimeDelta, TimeZone};
use tempfile::tempdir;

use super::*;
use crate::journal::CSV_HEADER;
use crate::model::{EventKind, Outcome};

thread_local! {
    static FAILURE: Cell<Option<Step>> = const { Cell::new(None) };
}

pub(super) fn inject(step: Step) -> Result<(), JournalError> {
    FAILURE.with(|failure| {
        if failure.get() == Some(step) {
            failure.set(None);
            Err(io::Error::other(format!("injected {step:?}")).into())
        } else {
            Ok(())
        }
    })
}

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 10, 23, 59, 59).unwrap()
}

fn event(id: &str) -> MeasurementEvent {
    MeasurementEvent::new("run", id, EventKind::PingProbe, Outcome::Success, now())
}

fn segments(directory: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<_> = fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "csv"))
        .collect();
    paths.sort();
    paths
}

fn ids(path: &Path) -> Vec<String> {
    let text = fs::read_to_string(path).unwrap();
    assert!(text.starts_with(CSV_HEADER));
    assert_eq!(text.matches(CSV_HEADER).count(), 1);
    csv::Reader::from_reader(text.as_bytes())
        .records()
        .map(|record| record.unwrap().get(2).unwrap().to_owned())
        .collect()
}

#[test]
fn utc_rotation_uses_write_time_and_handles_idle_and_backward_clocks() {
    let dir = tempdir().unwrap();
    let target = OutputTarget::Directory(dir.path().to_owned());
    let mut output = Journal::open_at(&target, None, now()).unwrap();
    let first = output.path().to_owned();
    output.append_batch_at(&[event("before")], now()).unwrap();
    let next_day = now() + TimeDelta::seconds(1);
    output
        .append_batch_at(&[], next_day + TimeDelta::days(9))
        .unwrap();
    assert_eq!(output.path(), first);
    output.append_batch_at(&[event("after")], next_day).unwrap();
    let second = output.path().to_owned();
    assert_ne!(first, second);
    output
        .append_batch_at(&[event("backward")], now() - TimeDelta::days(3))
        .unwrap();
    output
        .append_batch_at(&[event("same-day")], next_day)
        .unwrap();
    assert_eq!(output.path(), second);
    output
        .append_batch_at(&[event("idle")], next_day + TimeDelta::days(9))
        .unwrap();
    assert_eq!(segments(dir.path()).len(), 3);
    assert_eq!(ids(&first), ["before"]);
    assert_eq!(ids(&second), ["after", "backward", "same-day"]);
    assert_eq!(ids(output.path()), ["idle"]);
}

#[test]
fn size_limits_count_encoded_bytes_and_keep_batches_intact() {
    let mut special = event("a");
    special.error_message = Some("Unicode é, quoted \"value\"\nand newline".into());
    let batch = [special.clone(), event("b")];
    let mut expected = JournalWriter::from_writer(Vec::new()).unwrap();
    expected.append_batch(&batch).unwrap();
    let expected = expected.into_inner().unwrap();
    for limit in [
        1,
        CSV_HEADER.len() as u64,
        expected.len() as u64 - 1,
        expected.len() as u64,
    ] {
        let dir = tempdir().unwrap();
        let mut output = Journal::open_at(
            &OutputTarget::Directory(dir.path().to_owned()),
            Some(limit),
            now(),
        )
        .unwrap();
        let first = output.path().to_owned();
        output.append_batch_at(&batch, now()).unwrap();
        assert_eq!(output.path(), first, "no empty size-triggered segment");
        assert_eq!(fs::read(&first).unwrap(), expected);
        output.append_batch_at(&[event("c")], now()).unwrap();
        assert_ne!(output.path(), first);
        assert_eq!(segments(dir.path()).len(), 2);
        assert_eq!(ids(&first), ["a", "b"]);
        assert_eq!(ids(output.path()), ["c"]);
    }
}

#[test]
fn exact_size_boundary_fits_and_combined_triggers_create_one_segment() {
    let batch = [event("a")];
    let mut encoded = JournalWriter::without_header(Vec::new());
    encoded.append_batch(&batch).unwrap();
    let limit = (CSV_HEADER.len() + 2 + 2 * encoded.into_inner().unwrap().len()) as u64;
    let dir = tempdir().unwrap();
    let mut output = Journal::open_at(
        &OutputTarget::Directory(dir.path().to_owned()),
        Some(limit),
        now(),
    )
    .unwrap();
    let first = output.path().to_owned();
    output.append_batch_at(&batch, now()).unwrap();
    output.append_batch_at(&[event("b")], now()).unwrap();
    assert_eq!(fs::metadata(&first).unwrap().len(), limit);
    assert_eq!(output.path(), first);
    output
        .append_batch_at(&[event("c")], now() + TimeDelta::seconds(1))
        .unwrap();
    assert_eq!(segments(dir.path()).len(), 2);
    assert_eq!(ids(&first), ["a", "b"]);
}

#[test]
fn size_rotation_during_clock_rollback_preserves_date_high_water_mark() {
    let dir = tempdir().unwrap();
    let mut output = Journal::open_at(
        &OutputTarget::Directory(dir.path().to_owned()),
        Some(1),
        now(),
    )
    .unwrap();
    output.append_batch_at(&[event("a")], now()).unwrap();
    output
        .append_batch_at(&[event("b")], now() - TimeDelta::days(1))
        .unwrap();
    assert_eq!(output.directory.as_ref().unwrap().date, now().date_naive());
    output.directory.as_mut().unwrap().max_bytes = None;
    let path = output.path().to_owned();
    output.append_batch_at(&[event("c")], now()).unwrap();
    assert_eq!(output.path(), path);
}

#[test]
fn fixed_mode_preserves_bytes_recovery_and_single_file_across_midnight() {
    let dir = tempdir().unwrap();
    let target = OutputTarget::File(dir.path().join("fixed.csv"));
    let events = [event("a"), event("b")];
    let mut output = Journal::open_at(&target, None, now()).unwrap();
    let path = output.path().to_owned();
    output.append_batch_at(&events[..1], now()).unwrap();
    output
        .append_batch_at(&[], now() + TimeDelta::days(1))
        .unwrap();
    drop(output);
    OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"partial,row")
        .unwrap();
    let mut output = Journal::open_at(&target, None, now() + TimeDelta::days(2)).unwrap();
    output
        .append_batch_at(&events[1..], now() + TimeDelta::days(3))
        .unwrap();
    output.flush().unwrap();
    let mut expected = JournalWriter::from_writer(Vec::new()).unwrap();
    expected.append_batch(&events).unwrap();
    assert_eq!(fs::read(&path).unwrap(), expected.into_inner().unwrap());
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    assert!(Journal::open_at(&target, Some(1), now()).is_err());
    assert!(
        Journal::open_at(
            &OutputTarget::Directory(dir.path().to_owned()),
            Some(0),
            now()
        )
        .is_err()
    );
}

#[test]
fn restart_recovers_only_recorded_segment_and_collision_never_overwrites() {
    let dir = tempdir().unwrap();
    let target = OutputTarget::Directory(dir.path().to_owned());
    let unrelated = dir.path().join("netband-unrelated.csv");
    fs::write(&unrelated, b"unrelated invalid CSV").unwrap();
    let mut output = Journal::open_at(&target, None, now()).unwrap();
    let first = output.path().to_owned();
    output.append_batch_at(&[event("a")], now()).unwrap();
    drop(output);
    let before = fs::read(&first).unwrap();
    OpenOptions::new()
        .append(true)
        .open(&first)
        .unwrap()
        .write_all(b"partial,row")
        .unwrap();
    let mut restarted = Journal::open_at(&target, None, now()).unwrap();
    assert_ne!(restarted.path(), first);
    assert_eq!(fs::read(&first).unwrap(), before);
    restarted.append_batch_at(&[event("b")], now()).unwrap();
    assert_eq!(ids(restarted.path()), ["b"]);
    assert_eq!(fs::read(unrelated).unwrap(), b"unrelated invalid CSV");
}

#[test]
fn directory_and_segment_locks_reject_competitors_without_changing_bytes() {
    let dir = tempdir().unwrap();
    let target = OutputTarget::Directory(dir.path().to_owned());
    let output = Journal::open_at(&target, None, now()).unwrap();
    let path = output.path().to_owned();
    let before = fs::read(&path).unwrap();
    assert!(matches!(
        Journal::open_at(&target, None, now()),
        Err(JournalError::Locked(_))
    ));
    assert!(matches!(
        JournalWriter::open_at(&OutputTarget::File(path.clone()), now()),
        Err(JournalError::Locked(_))
    ));
    assert_eq!(fs::read(&path).unwrap(), before);
    drop(output);
    let (explicit, _) = JournalWriter::open_at(&OutputTarget::File(path.clone()), now()).unwrap();
    assert!(matches!(
        Journal::open_at(&target, None, now()),
        Err(JournalError::Locked(_))
    ));
    drop(explicit);
    Journal::open_at(&target, None, now()).unwrap();
}

#[test]
fn damaged_recovery_evidence_fails_closed() {
    for damage in ["missing", "header", "record", "marker", "traversal"] {
        let dir = tempdir().unwrap();
        let target = OutputTarget::Directory(dir.path().to_owned());
        let output = Journal::open_at(&target, None, now()).unwrap();
        let path = output.path().to_owned();
        drop(output);
        match damage {
            "missing" => fs::remove_file(&path).unwrap(),
            "header" => fs::write(&path, "partial header").unwrap(),
            "record" => fs::write(&path, format!("{CSV_HEADER}\r\nbad,row\r\n")).unwrap(),
            "marker" => fs::write(dir.path().join(ACTIVE), "").unwrap(),
            "traversal" => fs::write(dir.path().join(ACTIVE), "netband-../../other.csv\n").unwrap(),
            _ => unreachable!(),
        }
        let before = fs::read(&path).ok();
        let marker = fs::read(dir.path().join(ACTIVE)).unwrap();
        assert!(Journal::open_at(&target, None, now()).is_err(), "{damage}");
        assert_eq!(fs::read(&path).ok(), before);
        assert_eq!(fs::read(dir.path().join(ACTIVE)).unwrap(), marker);
    }
}

#[test]
fn transition_faults_preserve_acknowledged_rows_and_poison_the_writer() {
    for failure in [
        Step::OldSync,
        Step::NewLock,
        Step::HeaderWrite,
        Step::HeaderSync,
        Step::Publish,
        Step::SegmentDirectorySync,
        Step::MarkerWrite,
        Step::MarkerSync,
        Step::MarkerRename,
        Step::MarkerDirectorySync,
        Step::BatchWrite,
        Step::BatchSync,
    ] {
        let dir = tempdir().unwrap();
        let target = OutputTarget::Directory(dir.path().to_owned());
        let mut output = Journal::open_at(&target, Some(1), now()).unwrap();
        output
            .append_batch_at(&[event("acknowledged")], now())
            .unwrap();
        let acknowledged = output.path().to_owned();
        let before = fs::read(&acknowledged).unwrap();
        FAILURE.with(|slot| slot.set(Some(failure)));
        assert!(
            output
                .append_batch_at(&[event("uncertain")], now())
                .is_err(),
            "{failure:?}"
        );
        assert!(output.append_batch_at(&[event("retry")], now()).is_err());
        assert!(output.flush().is_err());
        assert_eq!(fs::read(&acknowledged).unwrap(), before);
        drop(output);
        let mut restarted = Journal::open_at(&target, None, now()).unwrap();
        restarted
            .append_batch_at(&[event("restarted")], now())
            .unwrap();
        let all: Vec<_> = segments(dir.path())
            .iter()
            .flat_map(|path| ids(path))
            .collect();
        assert_eq!(
            all.iter().filter(|id| *id == "acknowledged").count(),
            1,
            "{failure:?}"
        );
        assert_eq!(all.iter().filter(|id| *id == "restarted").count(), 1);
        assert!(!all.iter().any(|id| id == "retry"));
    }
}

#[test]
fn stale_linked_staging_file_is_unlinked_without_truncating_its_segment() {
    let dir = tempdir().unwrap();
    let target = OutputTarget::Directory(dir.path().to_owned());
    let output = Journal::open_at(&target, None, now()).unwrap();
    let path = output.path().to_owned();
    drop(output);
    fs::hard_link(&path, dir.path().join(PENDING)).unwrap();
    let before = fs::read(&path).unwrap();
    Journal::open_at(&target, None, now()).unwrap();
    assert_eq!(fs::read(path).unwrap(), before);
}

#[cfg(unix)]
#[test]
fn recovery_and_temporary_files_reject_symlinks() {
    use std::os::unix::fs::symlink;
    for name in [LOCK, ACTIVE, PENDING, MARKER_TEMP, "netband-linked.csv"] {
        let dir = tempdir().unwrap();
        let external = tempdir().unwrap();
        let victim = external.path().join("victim");
        fs::write(&victim, b"do not modify").unwrap();
        symlink(&victim, dir.path().join(name)).unwrap();
        if name.ends_with(".csv") {
            fs::write(dir.path().join(ACTIVE), format!("{name}\n")).unwrap();
        }
        assert!(
            Journal::open_at(&OutputTarget::Directory(dir.path().to_owned()), None, now()).is_err()
        );
        assert_eq!(fs::read(victim).unwrap(), b"do not modify");
    }
}

#[test]
fn rotation_and_restart_preserve_scheduler_admission_evidence() {
    use crate::cli::Cli;
    use crate::config::{ResolveContext, resolve};
    use crate::scheduler::{ManualDecision, Scheduler};
    use clap::Parser;

    let dir = tempdir().unwrap();
    let config = resolve(
        &Cli::try_parse_from(["netband", "--accept-mlab-policy", "run"]).unwrap(),
        &ResolveContext {
            stdout_is_terminal: false,
            current_dir: dir.path().to_owned(),
            state_dir: dir.path().to_owned(),
        },
    )
    .unwrap();
    let mut scheduler = Scheduler::open(&config.state_file, &config.bandwidth, now()).unwrap();
    scheduler.reserve_run(now()).unwrap();
    let snapshot = scheduler.snapshot();
    let evidence: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            let bytes = fs::read(&path).unwrap();
            (path, bytes)
        })
        .collect();
    let target = OutputTarget::Directory(dir.path().to_owned());
    for _ in 0..3 {
        let mut output = Journal::open_at(&target, Some(1), now()).unwrap();
        output.append_batch_at(&[event("a")], now()).unwrap();
        output.append_batch_at(&[event("b")], now()).unwrap();
    }
    for (path, before) in evidence {
        assert_eq!(fs::read(path).unwrap(), before);
    }
    assert_eq!(scheduler.snapshot(), snapshot);
    drop(scheduler);
    let mut restarted = Scheduler::open(&config.state_file, &config.bandwidth, now()).unwrap();
    assert_eq!(restarted.snapshot().runs, snapshot.runs);
    assert!(matches!(
        restarted.preflight_manual("next", now()).unwrap(),
        ManualDecision::Blocked(_)
    ));
}

#[test]
fn initialization_faults_leave_only_parseable_csvs_and_can_restart() {
    for failure in [
        Step::NewLock,
        Step::HeaderWrite,
        Step::HeaderSync,
        Step::Publish,
        Step::SegmentDirectorySync,
        Step::MarkerWrite,
        Step::MarkerSync,
        Step::MarkerRename,
        Step::MarkerDirectorySync,
    ] {
        let dir = tempdir().unwrap();
        let target = OutputTarget::Directory(dir.path().to_owned());
        FAILURE.with(|slot| slot.set(Some(failure)));
        assert!(
            Journal::open_at(&target, None, now()).is_err(),
            "{failure:?}"
        );
        for path in segments(dir.path()) {
            assert!(ids(&path).is_empty());
        }
        let mut output = Journal::open_at(&target, None, now()).unwrap();
        output
            .append_batch_at(&[event("recovered")], now())
            .unwrap();
        assert_eq!(ids(output.path()), ["recovered"]);
    }
}

#[test]
fn incomplete_staged_header_never_becomes_an_archive() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join(PENDING), b"schema_ver").unwrap();
    fs::write(dir.path().join(MARKER_TEMP), b"netband-incom").unwrap();
    let output =
        Journal::open_at(&OutputTarget::Directory(dir.path().to_owned()), None, now()).unwrap();
    assert_eq!(segments(dir.path()).len(), 1);
    assert!(ids(output.path()).is_empty());
    assert!(!dir.path().join(PENDING).exists());
    assert!(!dir.path().join(MARKER_TEMP).exists());
}

#[test]
fn marker_staging_preserves_a_locked_explicit_journal() {
    let dir = tempdir().unwrap();
    let path = dir.path().join(MARKER_TEMP);
    let (mut fixed, _) = JournalWriter::open_at(&OutputTarget::File(path.clone()), now()).unwrap();
    fixed.append_batch(&[event("acknowledged")]).unwrap();
    let before = fs::read(&path).unwrap();

    let result = Journal::open_at(&OutputTarget::Directory(dir.path().to_owned()), None, now());

    assert!(
        fs::read(&path).is_ok_and(|bytes| bytes == before),
        "directory initialization must not truncate or rename a locked explicit journal"
    );
    assert!(
        result.is_err(),
        "a locked marker staging path must reject directory initialization"
    );
    assert!(!dir.path().join(ACTIVE).exists());
    fixed.append_batch(&[event("after-rejection")]).unwrap();
    assert_eq!(ids(&path), ["acknowledged", "after-rejection"]);
}

#[test]
fn marker_collision_during_rotation_preserves_both_journals() {
    let dir = tempdir().unwrap();
    let target = OutputTarget::Directory(dir.path().to_owned());
    let mut journal = Journal::open_at(&target, Some(1), now()).unwrap();
    journal
        .append_batch_at(&[event("acknowledged")], now())
        .unwrap();
    let active = journal.path().to_owned();
    let before = fs::read(&active).unwrap();
    let marker = fs::read(dir.path().join(ACTIVE)).unwrap();

    let temporary = dir.path().join(MARKER_TEMP);
    let (mut fixed, _) =
        JournalWriter::open_at(&OutputTarget::File(temporary.clone()), now()).unwrap();
    fixed.append_batch(&[event("explicit")]).unwrap();
    let fixed_before = fs::read(&temporary).unwrap();

    assert!(matches!(
        journal.append_batch_at(&[event("unacknowledged")], now()),
        Err(JournalError::Locked(path)) if path == temporary
    ));
    assert_eq!(fs::read(&active).unwrap(), before);
    assert_eq!(fs::read(dir.path().join(ACTIVE)).unwrap(), marker);
    assert_eq!(fs::read(&temporary).unwrap(), fixed_before);
    assert!(journal.flush().is_err());
    assert!(journal.append_batch_at(&[event("retry")], now()).is_err());
    fixed.append_batch(&[event("after-rejection")]).unwrap();
    assert_eq!(ids(&temporary), ["explicit", "after-rejection"]);
}
