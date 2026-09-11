use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

use chrono::Utc;
use netband::config::OutputTarget;
use netband::journal::{Journal, JournalError, JournalSink};
use netband::model::{EventKind, MeasurementEvent, Outcome};
use tempfile::tempdir;

#[test]
fn directory_ownership_and_abrupt_exit_recovery_work_across_processes() {
    const DIRECTORY: &str = "NETBAND_ROTATION_TEST_DIRECTORY";
    const MODE: &str = "NETBAND_ROTATION_TEST_MODE";
    if let Some(directory) = std::env::var_os(DIRECTORY) {
        let target = OutputTarget::Directory(PathBuf::from(directory));
        if std::env::var(MODE).unwrap() == "contender" {
            assert!(matches!(
                Journal::open_at(&target, None, Utc::now()),
                Err(JournalError::Locked(_))
            ));
            return;
        }
        let mut journal = Journal::open_at(&target, None, Utc::now()).unwrap();
        let event = MeasurementEvent::new(
            "process",
            "acknowledged",
            EventKind::PingProbe,
            Outcome::Success,
            Utc::now(),
        );
        journal.append_batch(&[event]).unwrap();
        let mut file = OpenOptions::new()
            .append(true)
            .open(journal.path())
            .unwrap();
        file.write_all(b"incomplete,tail").unwrap();
        file.sync_all().unwrap();
        // Exit without Rust destructors, leaving recovery evidence and a partial row.
        std::process::exit(0);
    }
    let dir = tempdir().unwrap();
    let target = OutputTarget::Directory(dir.path().to_owned());
    let run = |mode: &str| {
        let child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "directory_ownership_and_abrupt_exit_recovery_work_across_processes",
            ])
            .env(DIRECTORY, dir.path())
            .env(MODE, mode)
            .output()
            .unwrap();
        assert!(child.status.success(), "{child:?}");
    };
    let owner = Journal::open_at(&target, None, Utc::now()).unwrap();
    let before = fs::read(owner.path()).unwrap();
    run("contender");
    assert_eq!(fs::read(owner.path()).unwrap(), before);
    drop(owner);
    run("crash");
    let active = fs::read_to_string(dir.path().join(".netband-active")).unwrap();
    let interrupted = dir.path().join(active.trim());
    assert!(
        fs::read(&interrupted)
            .unwrap()
            .ends_with(b"incomplete,tail")
    );
    let restarted = Journal::open_at(&target, None, Utc::now()).unwrap();
    assert_ne!(restarted.path(), interrupted);
    let mut reader = csv::Reader::from_path(&interrupted).unwrap();
    let records = reader.records().collect::<Result<Vec<_>, _>>().unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(&records[0][2], "acknowledged");
}
