use std::process::Command;

use chrono::Utc;
use netband::config::OutputTarget;
use netband::journal::{Journal, JournalError};
use tempfile::tempdir;

// Keep process spawning separate from tests that close and immediately reopen files:
// a fork can briefly inherit their locked descriptors before exec closes them.
#[test]
fn both_output_modes_reject_another_process() {
    const CHILD_PATH: &str = "NETBAND_JOURNAL_LOCK_TEST_PATH";
    if let Some(path) = std::env::var_os(CHILD_PATH) {
        let path = std::path::PathBuf::from(path);
        assert!(matches!(
            Journal::open_at(&OutputTarget::File(path.clone()), Utc::now()),
            Err(JournalError::Locked(locked)) if locked == path
        ));
        return;
    }

    for timestamped in [false, true] {
        let dir = tempdir().unwrap();
        let output = if timestamped {
            OutputTarget::Directory(dir.path().to_path_buf())
        } else {
            OutputTarget::File(dir.path().join("locked.csv"))
        };
        let (journal, path) = Journal::open_at(&output, Utc::now()).unwrap();
        let before = std::fs::read(&path).unwrap();
        let child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "both_output_modes_reject_another_process"])
            .env(CHILD_PATH, &path)
            .output()
            .unwrap();
        assert!(child.status.success(), "{child:?}");
        assert_eq!(std::fs::read(&path).unwrap(), before);
        drop(journal);
        Journal::open_at(&OutputTarget::File(path), Utc::now()).unwrap();
    }
}
