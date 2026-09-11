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
    assert_eq!(run.shutdown_grace, Duration::from_secs(30));
    assert_eq!(run.state_file, dir.path().join("state/scheduler.json"));
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

    let explicit_ping_only = resolve(
        &parse(&["netband", "--accept-mlab-policy", "--no-bandwidth", "run"]),
        &context(dir.path().to_path_buf(), true),
    )
    .unwrap();
    assert!(explicit_ping_only.no_bandwidth);
    assert!(!explicit_ping_only.bandwidth.automatic_enabled);
    assert_eq!(explicit_ping_only.console, ConsoleMode::Human);
    assert!(
        explicit_ping_only
            .summary()
            .contains("bandwidth.disabled_by_cli=true")
    );
}

#[test]
fn explicit_state_file_overrides_the_platform_default() {
    let dir = tempdir().unwrap();
    let config = resolve(
        &parse(&[
            "netband",
            "--state-file",
            "portable/state/scheduler.json",
            "config",
            "check",
        ]),
        &context(dir.path().to_path_buf(), false),
    )
    .unwrap();

    assert_eq!(
        config.state_file,
        dir.path().join("portable/state/scheduler.json")
    );
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
            "--shutdown-grace",
            "12s",
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
    assert_eq!(config.shutdown_grace, Duration::from_secs(12));
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

    let duplicate_target = resolve(
        &parse(&[
            "netband",
            "--ping-target",
            "192.0.2.1",
            "--ping-target",
            "192.0.2.1",
            "config",
            "check",
        ]),
        &context(dir.path().to_path_buf(), false),
    )
    .unwrap_err();
    assert!(
        duplicate_target
            .to_string()
            .contains("duplicate ping target")
    );

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

    for extra in [None, Some("--force")] {
        let mut args = vec!["netband"];
        if let Some(flag) = extra {
            args.push(flag);
        }
        args.extend(["once", "bandwidth"]);
        let error = resolve(&parse(&args), &context(dir.path().to_path_buf(), false)).unwrap_err();
        assert!(error.to_string().contains("explicit policy acceptance"));
    }

    let forced = resolve(
        &parse(&[
            "netband",
            "--accept-mlab-policy",
            "--force",
            "once",
            "bandwidth",
        ]),
        &context(dir.path().to_path_buf(), false),
    )
    .unwrap();
    assert!(forced.bandwidth.force_limits);

    let wrong_command = resolve(
        &parse(&["netband", "--force", "once", "ping"]),
        &context(dir.path().to_path_buf(), false),
    )
    .unwrap_err();
    assert!(wrong_command.to_string().contains("once bandwidth"));
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
    let certificate = rcgen::generate_simple_self_signed(vec!["ndt.example.net".to_owned()])
        .unwrap()
        .cert
        .pem();
    std::fs::write(&ca, &certificate).unwrap();
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

    std::fs::write(&ca, format!("{certificate}{certificate}")).unwrap();
    validate_environment(&config).unwrap();
    for contents in [
        String::new(),
        "test fixture".to_owned(),
        "-----BEGIN CERTIFICATE-----\ninvalid!\n-----END CERTIFICATE-----".to_owned(),
        "-----BEGIN CERTIFICATE-----\nAQID\n-----END CERTIFICATE-----".to_owned(),
        format!("{certificate}-----BEGIN CERTIFICATE-----\nAQID\n-----END CERTIFICATE-----"),
    ] {
        std::fs::write(&ca, contents).unwrap();
        assert!(
            validate_environment(&config)
                .unwrap_err()
                .to_string()
                .contains("private CA")
        );
    }
    // Validation reads current contents on each call, including disappearance.
    std::fs::write(&ca, certificate).unwrap();
    validate_environment(&config).unwrap();
    std::fs::remove_file(&ca).unwrap();
    assert!(
        validate_environment(&config)
            .unwrap_err()
            .to_string()
            .contains("private CA")
    );
    std::fs::create_dir(&ca).unwrap();
    assert!(
        validate_environment(&config)
            .unwrap_err()
            .to_string()
            .contains("private CA")
    );
}

#[test]
fn invalid_private_ca_stops_commands_before_output_state_or_network_activity() {
    let root = tempdir().unwrap();
    let ca = root.path().join("invalid.pem");
    std::fs::write(&ca, "not a certificate").unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    for command in [["config", "check"], ["once", "bandwidth"]] {
        let output = root.path().join("results.csv");
        let state = root.path().join("scheduler.json");
        let result = std::process::Command::new(env!("CARGO_BIN_EXE_netband"))
            .args(["--ndt-provider", "direct", "--ndt-target"])
            .arg(listener.local_addr().unwrap().to_string())
            .arg("--ndt-ca-cert")
            .arg(&ca)
            .arg("--output")
            .arg(&output)
            .arg("--state-file")
            .arg(&state)
            .args(command)
            .output()
            .unwrap();
        assert_eq!(result.status.code(), Some(2));
        assert!(!String::from_utf8_lossy(&result.stdout).contains("configuration=valid"));
        assert!(String::from_utf8_lossy(&result.stderr).contains("private CA"));
        assert!(!output.exists());
        assert!(!state.exists());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }
}

#[test]
fn direct_tls_options_apply_when_either_endpoint_is_secure() {
    let root = tempdir().unwrap();
    let ca = root.path().join("ca.pem");
    let certificate = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
    std::fs::write(&ca, certificate.cert.pem()).unwrap();
    for download in ["ws", "wss"] {
        for upload in ["ws", "wss"] {
            for options in 0..4 {
                for allow_insecure in [false, true] {
                    let down = format!("{download}://127.0.0.1/ndt/v7/download");
                    let up = format!("{upload}://127.0.0.1/ndt/v7/upload");
                    let mut args = vec![
                        "netband",
                        "--ndt-provider",
                        "direct",
                        "--ndt-download-url",
                        &down,
                        "--ndt-upload-url",
                        &up,
                    ];
                    if options & 1 != 0 {
                        args.extend(["--ndt-tls-server-name", "localhost"]);
                    }
                    if options & 2 != 0 {
                        args.extend(["--ndt-ca-cert", ca.to_str().unwrap()]);
                    }
                    if allow_insecure {
                        args.push("--allow-insecure-ndt");
                    }
                    args.extend(["config", "check"]);
                    let result = resolve(&parse(&args), &context(root.path().to_path_buf(), false));
                    let plaintext_allowed =
                        allow_insecure || (download == "wss" && upload == "wss");
                    let options_allowed = options == 0 || download == "wss" || upload == "wss";
                    assert_eq!(
                        result.is_ok(),
                        plaintext_allowed && options_allowed,
                        "{download}/{upload}, options={options}, insecure={allow_insecure}"
                    );
                    if let Ok(config) = result {
                        validate_environment(&config).unwrap();
                        let ProviderConfig::Direct(direct) = config.bandwidth.provider else {
                            unreachable!()
                        };
                        assert_eq!(
                            direct.tls_server_name.as_deref(),
                            (options & 1 != 0).then_some("localhost")
                        );
                        assert_eq!(direct.ca_cert.as_ref(), (options & 2 != 0).then_some(&ca));
                    } else {
                        let error = result.unwrap_err().to_string();
                        assert!(error.contains(if plaintext_allowed {
                            "requires wss://"
                        } else {
                            "allow-insecure-ndt"
                        }));
                    }
                }
            }
        }
    }
}

#[test]
fn rotating_output_defaults_precedence_and_validation() {
    let dir = tempdir().unwrap();
    let ctx = context(dir.path().to_owned(), false);
    let defaults = resolve(&parse(&["netband", "run"]), &ctx).unwrap();
    assert_eq!(
        defaults.output,
        OutputTarget::Directory(dir.path().to_owned())
    );
    assert_eq!(defaults.rotate_max_bytes, None);
    assert!(defaults.summary().contains("rotation=daily-utc"));
    let config = dir.path().join("rotation.toml");
    std::fs::write(
        &config,
        "output_dir = 'measurements'\nrotate_max_bytes = 4096\n",
    )
    .unwrap();
    let args = ["netband", "--config", config.to_str().unwrap(), "run"];
    let configured = resolve(&parse(&args), &ctx).unwrap();
    assert_eq!(
        configured.output,
        OutputTarget::Directory(dir.path().join("measurements"))
    );
    assert_eq!(configured.rotate_max_bytes, Some(4096));
    let overridden = resolve(
        &parse(&[
            "netband",
            "--config",
            config.to_str().unwrap(),
            "--output-dir",
            "other",
            "--rotate-max-bytes",
            "8192",
            "run",
        ]),
        &ctx,
    )
    .unwrap();
    assert_eq!(
        overridden.output,
        OutputTarget::Directory(dir.path().join("other"))
    );
    assert_eq!(overridden.rotate_max_bytes, Some(8192));
    assert!(overridden.summary().contains("rotate_max_bytes=8192"));
    assert!(
        resolve(
            &parse(&[
                "netband",
                "--config",
                config.to_str().unwrap(),
                "--output",
                "fixed.csv",
                "run",
            ]),
            &ctx
        )
        .unwrap_err()
        .to_string()
        .contains("requires directory output")
    );
    for text in [
        "rotate_max_bytes = 0",
        "rotate_max_bytes = -1",
        "rotate_max_bytes = '64MiB'",
        "output = 'fixed.csv'\nrotate_max_bytes = 1024",
    ] {
        std::fs::write(&config, text).unwrap();
        assert!(resolve(&parse(&args), &ctx).is_err(), "{text}");
    }
    assert!(resolve(&parse(&["netband", "--rotate-max-bytes", "0", "run"]), &ctx).is_err());
    assert!(
        Cli::try_parse_from([
            "netband",
            "--output",
            "fixed.csv",
            "--rotate-max-bytes",
            "1",
            "run"
        ])
        .is_err()
    );
    assert!(
        Cli::try_parse_from([
            "netband",
            "--rotate-max-bytes",
            "18446744073709551616",
            "run"
        ])
        .is_err()
    );
    assert!(!dir.path().join("measurements").exists());
    assert!(!dir.path().join("fixed.csv").exists());
    assert!(!dir.path().join("state").exists());
}
