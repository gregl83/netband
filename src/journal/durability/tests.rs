use std::cell::RefCell;
use std::fs;
use std::path::PathBuf;

use chrono::Utc;
use tempfile::tempdir;

use super::*;
use crate::config::OutputTarget;
use crate::journal::{Journal, JournalWriter};
use crate::model::{EventKind, MeasurementEvent, Outcome, RunId};

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Sync {
    Data,
    Directory(PathBuf),
}

#[derive(Default)]
struct Trace {
    operations: Vec<Sync>,
    fail_directory: Option<PathBuf>,
}

thread_local! {
    static TRACE: RefCell<Trace> = RefCell::default();
}

pub(super) fn record(operation: Sync) -> io::Result<()> {
    TRACE.with_borrow_mut(|trace| {
        let fail = matches!(&operation, Sync::Directory(path) if Some(path) == trace.fail_directory.as_ref());
        trace.operations.push(operation);
        if fail {
            trace.fail_directory = None;
            return Err(io::Error::other("injected directory sync failure"));
        }
        Ok(())
    })
}

fn reset(fail_directory: Option<PathBuf>) {
    TRACE.with_borrow_mut(|trace| {
        *trace = Trace {
            operations: Vec::new(),
            fail_directory,
        }
    });
}

fn operations() -> Vec<Sync> {
    TRACE.with_borrow_mut(|trace| std::mem::take(&mut trace.operations))
}

fn event() -> MeasurementEvent {
    MeasurementEvent::new(
        RunId::new(),
        EventKind::PingProbe,
        Outcome::Success,
        Utc::now(),
    )
}

#[test]
fn fixed_csv_syncs_its_entry_after_data_and_not_on_each_batch() {
    let root = tempdir().unwrap();
    let target = OutputTarget::File(root.path().join("results.csv"));
    for _ in 0..2 {
        reset(None);
        let (mut journal, _) = JournalWriter::open_at(&target, Utc::now()).unwrap();
        assert_eq!(
            operations(),
            [Sync::Data, Sync::Directory(root.path().to_owned())]
        );
        journal.append_batch(&[event()]).unwrap();
        assert_eq!(operations(), [Sync::Data]);
    }
}

#[test]
fn one_shot_syncs_new_csv_entry_after_data_and_keeps_prior_results() {
    let root = tempdir().unwrap();
    let directory = root.path().join("data/journals/once");
    let target = OutputTarget::AutomaticFile(directory.clone());
    let mut paths = Vec::new();
    for _ in 0..2 {
        reset(None);
        let mut journal = Journal::open_at(&target, None, Utc::now()).unwrap();
        let trace = operations();
        assert!(
            trace.ends_with(&[Sync::Data, Sync::Directory(directory.clone())]),
            "{trace:?}"
        );
        for ancestor in directory.ancestors() {
            assert!(
                trace.contains(&Sync::Directory(ancestor.to_owned())),
                "missing {} in {trace:?}",
                ancestor.display()
            );
        }
        paths.push(journal.path().to_owned());
        journal.append_batch_at(&[event()], Utc::now()).unwrap();
        assert_eq!(operations(), [Sync::Data]);
    }
    assert_ne!(paths[0], paths[1]);
    for path in paths {
        assert_eq!(csv::Reader::from_path(path).unwrap().records().count(), 1);
    }
}

#[test]
fn directory_sync_failure_rejects_fixed_output_and_retry_preserves_rows() {
    let root = tempdir().unwrap();
    let target = OutputTarget::File(root.path().join("results.csv"));
    reset(None);
    let (mut journal, path) = JournalWriter::open_at(&target, Utc::now()).unwrap();
    journal.append_batch(&[event()]).unwrap();
    drop(journal);
    let before = fs::read(&path).unwrap();
    reset(Some(root.path().to_owned()));
    let error = JournalWriter::open_at(&target, Utc::now()).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("injected directory sync failure")
    );
    assert_eq!(fs::read(&path).unwrap(), before);
    reset(None);
    let (mut journal, _) = JournalWriter::open_at(&target, Utc::now()).unwrap();
    assert!(operations().contains(&Sync::Directory(root.path().to_owned())));
    journal.append_batch(&[event()]).unwrap();
    assert_eq!(csv::Reader::from_path(path).unwrap().records().count(), 2);
}

#[test]
fn automatic_directory_sync_failures_block_creation_and_retry_syncs_existing_ancestors() {
    for rotating in [false, true] {
        for failed_component in ["", "data", "data/journals", "data/journals/output"] {
            let root = tempdir().unwrap();
            let directory = root.path().join("data/journals/output");
            let target = if rotating {
                OutputTarget::AutomaticDirectory(directory.clone())
            } else {
                OutputTarget::AutomaticFile(directory.clone())
            };
            reset(Some(root.path().join(failed_component)));
            let error = Journal::open_at(&target, None, Utc::now()).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("injected directory sync failure")
            );
            assert!(directory.is_dir());
            assert_eq!(fs::read_dir(&directory).unwrap().count(), 0);
            reset(None);
            let mut journal = Journal::open_at(&target, None, Utc::now()).unwrap();
            let trace = operations();
            for ancestor in directory.ancestors() {
                assert!(
                    trace.contains(&Sync::Directory(ancestor.to_owned())),
                    "missing {} in {trace:?}",
                    ancestor.display()
                );
            }
            journal.append_batch_at(&[event()], Utc::now()).unwrap();
            assert_eq!(operations(), [Sync::Data]);
        }
    }
}
