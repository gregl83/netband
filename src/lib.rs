pub mod bandwidth;
pub mod cli;
pub mod config;
pub mod console;
pub mod diagnostics;
pub mod health;
pub mod interfaces;
pub mod journal;
pub mod model;
pub mod monitor;
pub mod ping;
pub mod provider;
pub mod scheduler;

use std::io::IsTerminal;
use std::process::ExitCode;

use directories::ProjectDirs;

use crate::cli::{Cli, CommandKind};
use crate::config::{ResolveContext, resolve, validate_environment};

pub async fn run(cli: Cli) -> ExitCode {
    let current_dir = match std::env::current_dir() {
        Ok(path) => path,
        Err(error) => {
            eprintln!("configuration error: cannot determine current directory: {error}");
            return ExitCode::from(2);
        }
    };
    let state_dir = ProjectDirs::from("dev", "netband", "netband")
        .and_then(|dirs| dirs.state_dir().map(std::path::Path::to_path_buf))
        .unwrap_or_else(|| current_dir.join(".netband-state"));
    let context = ResolveContext {
        stdout_is_terminal: std::io::stdout().is_terminal(),
        current_dir,
        state_dir,
    };

    let config = match resolve(&cli, &context).and_then(|config| {
        validate_environment(&config)?;
        Ok(config)
    }) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("configuration error: {error}");
            return ExitCode::from(2);
        }
    };
    diagnostics::init(config.verbosity);

    match config.command {
        CommandKind::ConfigCheck => {
            print!("{}", config.summary());
            ExitCode::SUCCESS
        }
        CommandKind::OncePing => {
            if config.interfaces.len() > 1 {
                tracing::error!(
                    "once ping accepts at most one selected interface; use run for fair multi-interface monitoring"
                );
                return ExitCode::from(3);
            }
            let transport = std::sync::Arc::new(ping::SurgePingTransport::new(
                config.interfaces.first().map(String::as_str),
                &config.ping.targets,
            ));
            match ping::execute_ping_once(&config, transport, tokio::io::stdout()).await {
                Ok(execution) => {
                    tracing::debug!(
                        output = %execution.output_path.display(),
                        console_dropped = execution.console_stats.dropped_events,
                        "one-shot ping complete"
                    );
                    ExitCode::from(execution.exit_status.code())
                }
                Err(error) => {
                    tracing::error!(%error, "one-shot ping failed");
                    ExitCode::from(4)
                }
            }
        }
        CommandKind::Run => {
            let (_shutdown_sender, shutdown) = monitor::cancellation_channel();
            let execution = if !config.interfaces.is_empty() {
                monitor::execute_multi_interface_monitor(&config, tokio::io::stdout(), shutdown)
                    .await
            } else {
                let transport = std::sync::Arc::new(ping::SurgePingTransport::new(
                    config.interfaces.first().map(String::as_str),
                    &config.ping.targets,
                ));
                monitor::execute_ping_monitor(&config, transport, tokio::io::stdout(), shutdown)
                    .await
            };
            match execution {
                Ok(execution) => {
                    tracing::debug!(
                        output = %execution.output_path.display(),
                        rounds = execution.monitor_stats.rounds_completed,
                        skipped_ticks = execution.monitor_stats.skipped_ticks,
                        console_dropped = execution.console_stats.dropped_events,
                        "continuous ping monitoring stopped"
                    );
                    ExitCode::SUCCESS
                }
                Err(error) => {
                    tracing::error!(%error, "continuous ping monitoring failed");
                    ExitCode::from(4)
                }
            }
        }
        CommandKind::OnceBandwidth => {
            if config.interfaces.len() > 1 {
                tracing::error!(
                    "once bandwidth accepts at most one selected interface; use run for fair multi-interface monitoring"
                );
                return ExitCode::from(3);
            }
            let (_shutdown_sender, shutdown) = bandwidth::cancellation_channel();
            match bandwidth::execute_bandwidth_once(&config, tokio::io::stdout(), shutdown).await {
                Ok(execution) => {
                    tracing::debug!(
                        output = %execution.output_path.display(),
                        outcome = ?execution.report.outcome,
                        console_dropped = execution.console_stats.dropped_events,
                        "one-shot bandwidth measurement complete"
                    );
                    ExitCode::from(execution.report.exit_code())
                }
                Err(error) => {
                    tracing::error!(%error, "one-shot bandwidth measurement failed");
                    ExitCode::from(4)
                }
            }
        }
    }
}
