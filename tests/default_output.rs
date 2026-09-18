//! Automatic journal storage must not interfere with another invocation.
//! Process-spawning checks live in default_output_process.rs to avoid inherited locks.
use clap::Parser;
use netband::cli::Cli;
use netband::config::{ResolveContext, resolve, validate_environment};
use netband::journal::{Journal, JournalWriter};
use tempfile::tempdir;

#[test]
fn default_storage_is_lazy_unique_and_keeps_directory_ownership() {
    let root = tempdir().unwrap();
    let context = ResolveContext {
        current_dir: root.path().to_owned(),
        data_dir: root.path().join("data"),
        state_dir: root.path().join("state"),
        stdout_is_terminal: false,
    };
    let config = |args: &[&str]| {
        let cli = Cli::try_parse_from(args).unwrap();
        let config = resolve(&cli, &context).unwrap();
        validate_environment(&config).unwrap();
        config
    };
    let run = config(&["netband", "--no-bandwidth", "run"]);
    let ping = config(&["netband", "once", "ping"]);
    let bandwidth = config(&["netband", "--accept-mlab-policy", "once", "bandwidth"]);
    let check = config(&["netband", "config", "check"]);
    assert!(check.summary().contains("data/journals/run"));
    assert!(ping.summary().contains("data/journals/once"));
    assert!(
        !context.state_dir.exists(),
        "validation must not create files"
    );

    assert!(!context.data_dir.exists());
    let now = chrono::Utc::now();
    let monitor = Journal::open_at(&run.output, None, now).unwrap();
    assert!(
        monitor
            .path()
            .starts_with(context.data_dir.join("journals/run"))
    );
    assert!(Journal::open_at(&run.output, None, now).is_err());
    let first = Journal::open_at(&ping.output, None, now).unwrap();
    let second = Journal::open_at(&bandwidth.output, None, now).unwrap();
    assert_ne!(first.path(), second.path());
    assert!(
        first
            .path()
            .starts_with(context.data_dir.join("journals/once"))
    );
    assert!(
        JournalWriter::open_at(
            &netband::config::OutputTarget::File(first.path().to_owned()),
            now
        )
        .is_err()
    );
    let first_path = first.path().to_owned();
    let contents = std::fs::read(&first_path).unwrap();
    drop(first);
    let third = Journal::open_at(&ping.output, None, now).unwrap();
    assert_ne!(third.path(), first_path);
    assert_eq!(std::fs::read(first_path).unwrap(), contents);
    for entry in std::fs::read_dir(context.data_dir.join("journals/once")).unwrap() {
        assert_eq!(entry.unwrap().path().extension().unwrap(), "csv");
    }
    drop(monitor);
    Journal::open_at(&run.output, None, now)
        .expect("directory ownership must be released after drop");
    assert!(!context.state_dir.exists());
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
}
