//! Automatic journal storage must not interfere with another invocation.
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
    assert!(check.summary().contains("state/journals/run"));
    assert!(ping.summary().contains("state/journals/once"));
    assert!(
        !context.state_dir.exists(),
        "validation must not create files"
    );

    let now = chrono::Utc::now();
    let monitor = Journal::open_at(&run.output, None, now).unwrap();
    assert!(
        monitor
            .path()
            .starts_with(context.state_dir.join("journals/run"))
    );
    assert!(Journal::open_at(&run.output, None, now).is_err());
    let first = Journal::open_at(&ping.output, None, now).unwrap();
    let second = Journal::open_at(&bandwidth.output, None, now).unwrap();
    assert_ne!(first.path(), second.path());
    assert!(
        first
            .path()
            .starts_with(context.state_dir.join("journals/once"))
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
    for entry in std::fs::read_dir(context.state_dir.join("journals/once")).unwrap() {
        assert_eq!(entry.unwrap().path().extension().unwrap(), "csv");
    }
    drop(monitor);
    assert!(Journal::open_at(&run.output, None, now).is_ok());
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
}

#[test]
fn config_check_reports_xdg_paths_without_creating_them() {
    let root = tempdir().unwrap();
    let state = root.path().join("xdg-state");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_netband"))
        .current_dir(root.path())
        .env("XDG_STATE_HOME", &state)
        .args(["config", "check"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains(&state.join("netband/journals/run").display().to_string())
    );
    assert!(!state.exists());
}

#[test]
fn once_reports_its_file_on_stderr_without_cluttering_cwd_or_jsonl() {
    let root = tempdir().unwrap();
    let state = root.path().join("xdg-state");
    let mut paths = Vec::new();
    for mode in ["jsonl", "off"] {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_netband"))
            .current_dir(root.path())
            .env("XDG_STATE_HOME", &state)
            .args([
                "--console",
                mode,
                "--verbosity",
                "error",
                "--ping-target",
                "127.0.0.1",
                "--ping-timeout",
                "20ms",
                "once",
                "ping",
            ])
            .output()
            .unwrap();
        // A sandbox may prohibit ICMP; the result and journal must still be produced.
        assert!(
            matches!(output.status.code(), Some(0 | 1)),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stderr = String::from_utf8(output.stderr).unwrap();
        let path = std::path::PathBuf::from(
            stderr
                .lines()
                .find_map(|line| line.strip_prefix("Writing results to "))
                .expect(&stderr),
        );
        assert!(path.is_absolute());
        assert!(path.starts_with(state.join("netband/journals/once")));
        assert!(path.is_file());
        paths.push(path);
        let stdout = String::from_utf8(output.stdout).unwrap();
        if mode == "off" {
            assert!(stdout.is_empty());
        } else {
            assert!(!stdout.is_empty());
            for line in stdout.lines() {
                serde_json::from_str::<serde_json::Value>(line).unwrap();
            }
        }
    }
    assert_ne!(paths[0], paths[1]);
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
}
