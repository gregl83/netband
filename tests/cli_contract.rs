use std::path::Path;
use std::process::{Command, Output};

use tempfile::tempdir;

fn netband(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_netband"))
        .args(args)
        .output()
        .expect("netband should start")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn help_and_version_expose_the_v1_command_contract() {
    let help = netband(&["--help"]);
    assert!(help.status.success());
    let help_text = stdout(&help);
    assert!(help_text.contains("run"));
    assert!(help_text.contains("once"));
    assert!(help_text.contains("config"));
    assert!(help_text.contains("--console"));
    assert!(help_text.contains("--no-bandwidth"));
    assert!(stderr(&help).is_empty());

    let once_help = netband(&["once", "--help"]);
    assert!(once_help.status.success());
    assert!(stdout(&once_help).contains("bandwidth"));
    assert!(stdout(&once_help).contains("ping"));

    let version = netband(&["--version"]);
    assert!(version.status.success());
    assert_eq!(stdout(&version).trim(), "netband 0.1.0");
}

#[test]
fn config_check_prints_stable_defaults_only_to_stdout() {
    let output = netband(&["config", "check"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("configuration=valid"));
    assert!(text.contains("console=off"));
    assert!(text.contains("ping.targets=1.1.1.1,8.8.8.8,9.9.9.9"));
    assert!(text.contains("ping.interval=5s"));
    assert!(text.contains("ping.timeout=2s"));
    assert!(text.contains("bandwidth.provider=mlab"));
    assert!(text.contains("bandwidth.daily_max=4"));
    assert!(text.contains("bandwidth.automatic_enabled=false"));
    assert!(stderr(&output).is_empty());
}

#[test]
fn invalid_configuration_uses_stderr_and_creates_no_output() {
    let dir = tempdir().unwrap();
    let output_path = dir.path().join("must-not-exist.csv");
    let output = netband(&[
        "--output",
        output_path.to_str().unwrap(),
        "--ping-interval",
        "0s",
        "config",
        "check",
    ]);

    assert_eq!(output.status.code(), Some(2));
    assert!(stdout(&output).is_empty());
    assert!(stderr(&output).contains("configuration error"));
    assert!(!Path::new(&output_path).exists());
}

#[test]
fn configured_interfaces_are_checked_without_probing() {
    let output = netband(&[
        "--interface",
        "netband-interface-that-does-not-exist",
        "config",
        "check",
    ]);

    assert_eq!(output.status.code(), Some(2));
    assert!(stdout(&output).is_empty());
    assert!(stderr(&output).contains("network interface"));
}

#[test]
fn tokenized_direct_urls_are_redacted() {
    let output = netband(&[
        "--ndt-provider",
        "direct",
        "--ndt-download-url",
        "wss://ndt.example.net/ndt/v7/download?access_token=download-secret",
        "--ndt-upload-url",
        "wss://ndt.example.net/ndt/v7/upload?access_token=upload-secret",
        "config",
        "check",
    ]);

    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("bandwidth.provider=direct"));
    assert!(text.contains("?[redacted]"));
    assert!(!text.contains("download-secret"));
    assert!(!text.contains("upload-secret"));
}

#[test]
fn malformed_tokenized_urls_are_redacted_from_errors() {
    let secret = "must-not-appear";
    let output = netband(&[
        "--ndt-provider",
        "direct",
        "--ndt-download-url",
        &format!("not a URL?access_token={secret}"),
        "--ndt-upload-url",
        "wss://ndt.example.net/upload",
        "config",
        "check",
    ]);

    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("invalid direct download URL"));
    assert!(!stderr(&output).contains(secret));
}

#[test]
fn automatic_bandwidth_scheduler_reports_phase_unavailable() {
    let scheduled = netband(&["--accept-mlab-policy", "run"]);
    assert_eq!(scheduled.status.code(), Some(3));
    assert!(stdout(&scheduled).is_empty());
    assert!(stderr(&scheduled).contains("--no-bandwidth"));
}
