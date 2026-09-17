//! Persistence tests grouped by lifecycle, recovery, and commit failures.
use super::*;
use crate::cli::Cli;
use crate::config::{BandwidthConfig, ResolveContext, resolve};
use crate::scheduler::{ManualDecision, Scheduler};
use chrono::{TimeDelta, TimeZone};
use clap::Parser;
use std::cell::Cell;
use tempfile::TempDir;
mod commits;
mod initialization;
mod recovery;

thread_local! {
    static FAULT: Cell<Option<(FileKind, Step, usize)>> = const { Cell::new(None) };
}
pub(super) fn fail_at(kind: FileKind, step: Step) -> bool {
    FAULT.with(|fault| match fault.get() {
        Some((expected_kind, expected_step, skip))
            if (kind, step) == (expected_kind, expected_step) =>
        {
            fault.set(if skip == 0 {
                None
            } else {
                Some((kind, step, skip - 1))
            });
            skip == 0
        }
        _ => false,
    })
}
struct Inject;
impl Inject {
    fn at(kind: FileKind, step: Step, skip: usize) -> Self {
        FAULT.with(|fault| {
            assert!(fault.get().is_none());
            fault.set(Some((kind, step, skip)));
        });
        Self
    }
}
impl Drop for Inject {
    fn drop(&mut self) {
        FAULT.with(|fault| fault.set(None));
    }
}
const STEPS: [Step; 5] = [
    Step::Write,
    Step::PartialWrite,
    Step::Sync,
    Step::Rename,
    Step::DirectorySync,
];
fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 8, 12, 0, 0).unwrap()
}
fn path(root: &TempDir) -> PathBuf {
    root.path().join("scheduler.json")
}
fn config(root: &TempDir) -> BandwidthConfig {
    let cli = Cli::try_parse_from(["netband", "--accept-mlab-policy", "config", "check"]).unwrap();
    resolve(
        &cli,
        &ResolveContext {
            stdout_is_terminal: false,
            current_dir: root.path().to_owned(),
            state_dir: root.path().to_owned(),
        },
    )
    .unwrap()
    .bandwidth
}
fn open(root: &TempDir) -> Result<Scheduler, SchedulerError> {
    Scheduler::open_seeded(path(root), &config(root), now(), 7)
}
fn contents(root: &TempDir) -> BTreeMap<String, Vec<u8>> {
    fs::read_dir(root.path())
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (
                entry.file_name().to_string_lossy().into_owned(),
                fs::read(entry.path()).unwrap(),
            )
        })
        .collect()
}
fn assert_rejected_unchanged(root: &TempDir) {
    let before = contents(root);
    assert!(open(root).is_err());
    assert_eq!(contents(root), before);
}
