use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;
use netband::cli::{Cli, CommandKind, ConsoleMode};
use netband::config::{
    OutputTarget, ProviderConfig, ResolveContext, resolve, validate_environment,
};
use tempfile::tempdir;

fn parse(args: &[&str]) -> Cli {
    Cli::try_parse_from(args).expect("CLI should parse")
}

fn context(root: PathBuf, stdout_is_terminal: bool) -> ResolveContext {
    ResolveContext {
        stdout_is_terminal,
        current_dir: root.clone(),
        state_dir: root.join("state"),
    }
}

#[test]
fn defaults_are_typed_and_command_specific() {
    let dir = tempdir().unwrap();
    let run = resolve(
        &parse(&["netband", "run"]),
        &context(dir.path().to_path_buf(), true),
    )
    .unwrap();
    assert_eq!(run.command, CommandKind::Run);
    assert_eq!(run.console, ConsoleMode::Human);
    assert_eq!(run.ping.interval, Duration::from_secs(5));
    assert_eq!(run.ping.timeout, Duration::from_secs(2));
    assert_eq!(
        run.ping.targets,
        [
            IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1)),
            IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)),
            IpAddr::V4(Ipv4Addr::new(9, 9, 9, 9)),
        ]
    );
    assert_eq!(run.bandwidth.daily_max, 4);
    assert_eq!(run.bandwidth.min_spacing, Duration::from_secs(36 * 60));
    assert!(!run.bandwidth.automatic_enabled);

    let piped_run = resolve(
        &parse(&["netband", "run"]),
        &context(dir.path().to_path_buf(), false),
    )
    .unwrap();
    assert_eq!(piped_run.console, ConsoleMode::Off);

    let once = resolve(
        &parse(&["netband", "once", "ping"]),
        &context(dir.path().to_path_buf(), false),
    )
    .unwrap();
    assert_eq!(once.console, ConsoleMode::Human);
}

#[test]
fn cli_overrides_toml_and_replaces_lists() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("netband.toml");
    std::fs::write(
        &file,
        r#"
console = "off"
interfaces = ["from-file"]

[ping]
targets = ["192.0.2.1"]
interval = "30s"
timeout = "4s"

[bandwidth]
daily_max = 2
"#,
    )
    .unwrap();

    let config = resolve(
        &parse(&[
            "netband",
            "--config",
            file.to_str().unwrap(),
            "--console",
            "jsonl",
            "--interface",
            "from-cli",
            "--ping-target",
            "198.51.100.1",
            "--ping-interval",
            "7s",
            "config",
            "check",
        ]),
        &context(dir.path().to_path_buf(), false),
    )
    .unwrap();

    assert_eq!(config.console, ConsoleMode::Jsonl);
    assert_eq!(config.interfaces, ["from-cli"]);
    assert_eq!(
        config.ping.targets,
        ["198.51.100.1".parse::<IpAddr>().unwrap()]
    );
    assert_eq!(config.ping.interval, Duration::from_secs(7));
    assert_eq!(config.ping.timeout, Duration::from_secs(4));
    assert_eq!(config.bandwidth.daily_max, 2);
}

#[test]
fn malformed_toml_and_durations_are_rejected() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("bad.toml");
    std::fs::write(&file, "[ping\ninterval = '5s'").unwrap();
    let error = resolve(
        &parse(&[
            "netband",
            "--config",
            file.to_str().unwrap(),
            "config",
            "check",
        ]),
        &context(dir.path().to_path_buf(), false),
    )
    .unwrap_err();
    assert!(error.to_string().contains("parse"));

    let error = resolve(
        &parse(&["netband", "--ping-timeout", "later", "config", "check"]),
        &context(dir.path().to_path_buf(), false),
    )
    .unwrap_err();
    assert!(error.to_string().contains("ping timeout"));
}

#[test]
fn duplicate_interfaces_and_output_conflicts_are_rejected() {
    let dir = tempdir().unwrap();
    let duplicate = resolve(
        &parse(&[
            "netband",
            "--interface",
            "eth0",
            "--interface",
            "eth0",
            "config",
            "check",
        ]),
        &context(dir.path().to_path_buf(), false),
    )
    .unwrap_err();
    assert!(duplicate.to_string().contains("duplicate interface"));

    let file = dir.path().join("conflict.toml");
    std::fs::write(
        &file,
        "output = 'measurements.csv'\noutput_dir = 'measurements'\n",
    )
    .unwrap();
    let conflict = resolve(
        &parse(&[
            "netband",
            "--config",
            file.to_str().unwrap(),
            "config",
            "check",
        ]),
        &context(dir.path().to_path_buf(), false),
    )
    .unwrap_err();
    assert!(conflict.to_string().contains("output"));
}

#[test]
fn mlab_policy_bounds_and_consent_are_applied() {
    let dir = tempdir().unwrap();
    let too_many = resolve(
        &parse(&["netband", "--bandwidth-daily-max", "5", "config", "check"]),
        &context(dir.path().to_path_buf(), false),
    )
    .unwrap_err();
    assert!(too_many.to_string().contains("at most 4"));

    let too_close = resolve(
        &parse(&[
            "netband",
            "--bandwidth-min-spacing",
            "35m",
            "config",
            "check",
        ]),
        &context(dir.path().to_path_buf(), false),
    )
    .unwrap_err();
    assert!(too_close.to_string().contains("36m"));

    let accepted = resolve(
        &parse(&["netband", "--accept-mlab-policy", "config", "check"]),
        &context(dir.path().to_path_buf(), false),
    )
    .unwrap();
    assert!(accepted.bandwidth.automatic_enabled);
}

#[test]
fn direct_targets_urls_and_tls_are_validated() {
    let dir = tempdir().unwrap();
    for target in [
        "ndt.example.net:443",
        "203.0.113.10:443",
        "[2001:db8::1]:443",
    ] {
        let config = resolve(
            &parse(&[
                "netband",
                "--ndt-provider",
                "direct",
                "--ndt-target",
                target,
                "config",
                "check",
            ]),
            &context(dir.path().to_path_buf(), false),
        )
        .unwrap();
        assert!(matches!(
            config.bandwidth.provider,
            ProviderConfig::Direct(_)
        ));
        assert!(config.bandwidth.automatic_enabled);
    }

    let ip_with_sni = resolve(
        &parse(&[
            "netband",
            "--ndt-provider",
            "direct",
            "--ndt-target",
            "203.0.113.10:443",
            "--ndt-tls-server-name",
            "ndt.example.net",
            "config",
            "check",
        ]),
        &context(dir.path().to_path_buf(), false),
    )
    .unwrap();
    assert!(matches!(
        ip_with_sni.bandwidth.provider,
        ProviderConfig::Direct(_)
    ));

    let insecure = resolve(
        &parse(&[
            "netband",
            "--ndt-provider",
            "direct",
            "--ndt-download-url",
            "ws://127.0.0.1/ndt/v7/download",
            "--ndt-upload-url",
            "ws://127.0.0.1/ndt/v7/upload",
            "config",
            "check",
        ]),
        &context(dir.path().to_path_buf(), false),
    )
    .unwrap_err();
    assert!(insecure.to_string().contains("allow-insecure-ndt"));
}

#[test]
fn direct_configuration_rejects_ambiguity_and_impossible_policy() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("ambiguous.toml");
    std::fs::write(
        &file,
        r#"
[bandwidth]
provider = "direct"

[bandwidth.direct]
target = "ndt.example.net"
download_url = "wss://ndt.example.net/ndt/v7/download"
upload_url = "wss://ndt.example.net/ndt/v7/upload"
"#,
    )
    .unwrap();
    let ambiguous = resolve(
        &parse(&[
            "netband",
            "--config",
            file.to_str().unwrap(),
            "config",
            "check",
        ]),
        &context(dir.path().to_path_buf(), false),
    )
    .unwrap_err();
    assert!(ambiguous.to_string().contains("mutually exclusive"));

    let impossible = resolve(
        &parse(&[
            "netband",
            "--ndt-provider",
            "direct",
            "--ndt-target",
            "ndt.example.net",
            "--bandwidth-daily-max",
            "721",
            "--bandwidth-min-spacing",
            "2m",
            "config",
            "check",
        ]),
        &context(dir.path().to_path_buf(), false),
    )
    .unwrap_err();
    assert!(impossible.to_string().contains("cannot fit"));
}

#[test]
fn provider_identity_ignores_secret_query_values() {
    let dir = tempdir().unwrap();
    let resolve_token = |token: &str| {
        resolve(
            &parse(&[
                "netband",
                "--ndt-provider",
                "direct",
                "--ndt-download-url",
                &format!("wss://ndt.example.net/download?access_token={token}"),
                "--ndt-upload-url",
                &format!("wss://ndt.example.net/upload?access_token={token}"),
                "config",
                "check",
            ]),
            &context(dir.path().to_path_buf(), false),
        )
        .unwrap()
        .bandwidth
        .provider_id
    };

    assert_eq!(resolve_token("one"), resolve_token("two"));
}

#[test]
fn environment_validation_checks_output_parent_without_creating_output() {
    let dir = tempdir().unwrap();
    let output = dir.path().join("measurements.csv");
    let config = resolve(
        &parse(&[
            "netband",
            "--output",
            output.to_str().unwrap(),
            "config",
            "check",
        ]),
        &context(dir.path().to_path_buf(), false),
    )
    .unwrap();

    assert!(matches!(config.output, OutputTarget::File(_)));
    validate_environment(&config).unwrap();
    assert!(!output.exists());
}

#[test]
fn explicit_direct_urls_support_nonstandard_paths_and_insecure_opt_in() {
    let dir = tempdir().unwrap();
    let secure = resolve(
        &parse(&[
            "netband",
            "--ndt-provider",
            "direct",
            "--ndt-download-url",
            "wss://ndt.example.net:8443/custom/down",
            "--ndt-upload-url",
            "wss://ndt.example.net:8443/custom/up",
            "config",
            "check",
        ]),
        &context(dir.path().to_path_buf(), false),
    )
    .unwrap();
    let ProviderConfig::Direct(secure) = secure.bandwidth.provider else {
        panic!("expected direct provider");
    };
    assert_eq!(secure.download_url.path(), "/custom/down");
    assert_eq!(secure.upload_url.path(), "/custom/up");

    let insecure = resolve(
        &parse(&[
            "netband",
            "--ndt-provider",
            "direct",
            "--ndt-download-url",
            "ws://127.0.0.1/custom/down",
            "--ndt-upload-url",
            "ws://127.0.0.1/custom/up",
            "--allow-insecure-ndt",
            "config",
            "check",
        ]),
        &context(dir.path().to_path_buf(), false),
    )
    .unwrap();
    assert!(matches!(
        insecure.bandwidth.provider,
        ProviderConfig::Direct(_)
    ));

    let partial = resolve(
        &parse(&[
            "netband",
            "--ndt-provider",
            "direct",
            "--ndt-download-url",
            "wss://ndt.example.net/down",
            "config",
            "check",
        ]),
        &context(dir.path().to_path_buf(), false),
    )
    .unwrap_err();
    assert!(partial.to_string().contains("supplied together"));
}

#[test]
fn private_ca_is_validated_without_network_access() {
    let dir = tempdir().unwrap();
    let ca = dir.path().join("private-ca.pem");
    std::fs::write(&ca, "test fixture").unwrap();
    let config = resolve(
        &parse(&[
            "netband",
            "--ndt-provider",
            "direct",
            "--ndt-target",
            "203.0.113.10:443",
            "--ndt-tls-server-name",
            "ndt.example.net",
            "--ndt-ca-cert",
            ca.to_str().unwrap(),
            "config",
            "check",
        ]),
        &context(dir.path().to_path_buf(), false),
    )
    .unwrap();
    validate_environment(&config).unwrap();

    std::fs::remove_file(&ca).unwrap();
    let error = validate_environment(&config).unwrap_err();
    assert!(error.to_string().contains("CA certificate"));
}
