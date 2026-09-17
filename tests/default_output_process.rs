//! Keep child-process spawning separate from immediate lock-release tests.
//! A fork can inherit another thread's locked descriptors until exec.
use tempfile::tempdir;

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
