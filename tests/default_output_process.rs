//! Keep child-process spawning separate from immediate lock-release tests.
//! A fork can inherit another thread's locked descriptors until exec.
use tempfile::tempdir;

#[test]
fn config_check_reports_xdg_paths_without_creating_them() {
    let root = tempdir().unwrap();
    let state = root.path().join("xdg-state");
    let data = root.path().join("xdg-data");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_netband"))
        .current_dir(root.path())
        .env("XDG_STATE_HOME", &state)
        .env("XDG_DATA_HOME", &data)
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
            .contains(&data.join("netband/journals/run").display().to_string())
    );
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains(&state.join("netband/scheduler.json").display().to_string())
    );
    assert!(!state.exists());
    assert!(!data.exists());
}

#[test]
fn once_reports_its_file_on_stderr_without_cluttering_cwd_or_jsonl() {
    let root = tempdir().unwrap();
    let state = root.path().join("xdg-state");
    let data = root.path().join("xdg-data");
    let mut paths = Vec::new();
    for mode in ["jsonl", "off"] {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_netband"))
            .current_dir(root.path())
            .env("XDG_STATE_HOME", &state)
            .env("XDG_DATA_HOME", &data)
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
        assert!(path.starts_with(data.join("netband/journals/once")));
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
    assert!(!state.exists());
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
}

#[test]
fn home_defaults_and_explicit_paths_keep_data_and_state_independent() {
    let root = tempdir().unwrap();
    let check = |options: &[&str]| {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_netband"))
            .current_dir(root.path())
            .env("HOME", root.path())
            .env_remove("XDG_DATA_HOME")
            .env_remove("XDG_STATE_HOME")
            .args(options)
            .args(["config", "check"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    };
    let data = root.path().join(".local/share/netband/journals/run");
    let state = root.path().join(".local/state/netband/scheduler.json");
    let defaults = check(&[]);
    assert!(defaults.contains(&data.display().to_string()));
    assert!(defaults.contains(&state.display().to_string()));
    let changed_state = check(&["--state-file", "custom.json"]);
    assert!(changed_state.contains(&data.display().to_string()));
    assert!(changed_state.contains(&root.path().join("custom.json").display().to_string()));
    let changed_output = check(&["--output", "custom.csv"]);
    assert!(changed_output.contains(&state.display().to_string()));
    assert!(changed_output.contains(&root.path().join("custom.csv").display().to_string()));
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}
