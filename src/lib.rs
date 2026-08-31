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
pub mod shutdown;

use std::io::IsTerminal;
use std::process::ExitCode;

use directories::ProjectDirs;

use crate::cli::{Cli, CommandKind};
use crate::config::{ResolveContext, resolve, validate_environment};

const EXIT_CONFIGURATION: u8 = 2;
const EXIT_PERMISSION: u8 = 3;
const EXIT_DURABLE_STATE: u8 = 4;
const EXIT_INTERNAL: u8 = 5;
const EXIT_FORCED_SHUTDOWN: u8 = 6;

struct CommandCompletion<T> {
    result: T,
    shutdown_requested: bool,
}

pub async fn run(cli: Cli) -> ExitCode {
    let current_dir = match std::env::current_dir() {
        Ok(path) => path,
        Err(error) => {
            eprintln!("configuration error: cannot determine current directory: {error}");
            return ExitCode::from(EXIT_CONFIGURATION);
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
            return ExitCode::from(EXIT_CONFIGURATION);
        }
    };
    diagnostics::init(config.verbosity);
    if config.command != CommandKind::ConfigCheck {
        tracing::info!(configuration = %config.summary().trim(), "Netband starting");
    }

    match config.command {
        CommandKind::ConfigCheck => {
            print!("{}", config.summary());
            ExitCode::SUCCESS
        }
        CommandKind::OncePing => run_once_ping(&config).await,
        CommandKind::Run => run_monitor(&config).await,
        CommandKind::OnceBandwidth => run_once_bandwidth(&config).await,
    }
}

async fn run_once_ping(config: &config::ResolvedConfig) -> ExitCode {
    if config.interfaces.len() > 1 {
        tracing::error!(
            "once ping accepts at most one selected interface; use run for fair multi-interface monitoring"
        );
        return ExitCode::from(EXIT_CONFIGURATION);
    }
    let transport = std::sync::Arc::new(ping::SurgePingTransport::new(
        config.interfaces.first().map(String::as_str),
        &config.ping.targets,
    ));
    let (shutdown_sender, _shutdown_receiver) = monitor::cancellation_channel();
    match supervise_command(
        config.shutdown_grace,
        shutdown_sender,
        ping::execute_ping_once(config, transport, console::service_stdout()),
    )
    .await
    {
        Ok(CommandCompletion {
            result: Ok(execution),
            shutdown_requested,
        }) => {
            tracing::debug!(
                output = %execution.output_path.display(),
                console_dropped = execution.console_stats.dropped_events,
                "one-shot ping complete"
            );
            if shutdown_requested {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(execution.exit_status.code())
            }
        }
        Ok(CommandCompletion {
            result: Err(error), ..
        }) => {
            tracing::error!(%error, "one-shot ping failed");
            ExitCode::from(ping_error_code(&error))
        }
        Err(code) => ExitCode::from(code),
    }
}

async fn run_monitor(config: &config::ResolvedConfig) -> ExitCode {
    let (shutdown_sender, shutdown) = monitor::cancellation_channel();
    let completion = if !config.interfaces.is_empty() {
        supervise_command(
            config.shutdown_grace,
            shutdown_sender,
            monitor::execute_multi_interface_monitor(config, console::service_stdout(), shutdown),
        )
        .await
    } else {
        let transport = std::sync::Arc::new(ping::SurgePingTransport::new(
            config.interfaces.first().map(String::as_str),
            &config.ping.targets,
        ));
        supervise_command(
            config.shutdown_grace,
            shutdown_sender,
            monitor::execute_ping_monitor(config, transport, console::service_stdout(), shutdown),
        )
        .await
    };
    match completion {
        Ok(CommandCompletion {
            result: Ok(execution),
            ..
        }) => {
            tracing::debug!(
                output = %execution.output_path.display(),
                rounds = execution.monitor_stats.rounds_completed,
                skipped_ticks = execution.monitor_stats.skipped_ticks,
                console_dropped = execution.console_stats.dropped_events,
                "continuous ping monitoring stopped"
            );
            ExitCode::SUCCESS
        }
        Ok(CommandCompletion {
            result: Err(error), ..
        }) => {
            tracing::error!(%error, "continuous ping monitoring failed");
            ExitCode::from(monitor_error_code(&error))
        }
        Err(code) => ExitCode::from(code),
    }
}

async fn run_once_bandwidth(config: &config::ResolvedConfig) -> ExitCode {
    if config.interfaces.len() > 1 {
        tracing::error!(
            "once bandwidth accepts at most one selected interface; use run for fair multi-interface monitoring"
        );
        return ExitCode::from(EXIT_CONFIGURATION);
    }
    let (shutdown_sender, shutdown) = bandwidth::cancellation_channel();
    match supervise_command(
        config.shutdown_grace,
        shutdown_sender,
        bandwidth::execute_bandwidth_once(config, console::service_stdout(), shutdown),
    )
    .await
    {
        Ok(CommandCompletion {
            result: Ok(execution),
            shutdown_requested,
        }) => {
            tracing::debug!(
                output = %execution.output_path.display(),
                outcome = ?execution.report.outcome,
                console_dropped = execution.console_stats.dropped_events,
                "one-shot bandwidth measurement complete"
            );
            if shutdown_requested {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(execution.report.exit_code())
            }
        }
        Ok(CommandCompletion {
            result: Err(error), ..
        }) => {
            tracing::error!(%error, "one-shot bandwidth measurement failed");
            ExitCode::from(bandwidth_error_code(&error))
        }
        Err(code) => ExitCode::from(code),
    }
}

async fn supervise_command<T, F>(
    grace: std::time::Duration,
    shutdown_sender: tokio::sync::watch::Sender<bool>,
    operation: F,
) -> Result<CommandCompletion<T>, u8>
where
    F: std::future::Future<Output = T>,
{
    match shutdown::supervise(grace, shutdown_sender, operation).await {
        Ok(shutdown::Supervised::Completed(result)) => Ok(CommandCompletion {
            result,
            shutdown_requested: false,
        }),
        Ok(shutdown::Supervised::Graceful { result, signal }) => {
            tracing::info!(signal = signal.as_str(), "graceful shutdown complete");
            Ok(CommandCompletion {
                result,
                shutdown_requested: true,
            })
        }
        Ok(shutdown::Supervised::Forced(reason)) => {
            tracing::error!(?reason, "forced shutdown complete");
            Err(EXIT_FORCED_SHUTDOWN)
        }
        Err(error) => {
            tracing::error!(%error, "cannot install operating-system signal handlers");
            Err(EXIT_INTERNAL)
        }
    }
}

fn journal_error_code(error: &journal::JournalError) -> u8 {
    if error.is_permission_denied() {
        EXIT_PERMISSION
    } else {
        EXIT_DURABLE_STATE
    }
}

fn scheduler_error_code(error: &scheduler::SchedulerError) -> u8 {
    if error.is_permission_denied() {
        EXIT_PERMISSION
    } else {
        EXIT_DURABLE_STATE
    }
}

fn ping_error_code(error: &ping::PingCommandError) -> u8 {
    match error {
        ping::PingCommandError::Journal(error) => journal_error_code(error),
        ping::PingCommandError::Round(_) => EXIT_INTERNAL,
    }
}

fn monitor_error_code(error: &monitor::MonitorError) -> u8 {
    match error {
        monitor::MonitorError::Journal(error) => journal_error_code(error),
        monitor::MonitorError::Scheduler(error) => scheduler_error_code(error),
        monitor::MonitorError::Round(_) | monitor::MonitorError::Task(_) => EXIT_INTERNAL,
    }
}

fn bandwidth_error_code(error: &bandwidth::BandwidthCommandError) -> u8 {
    match error {
        bandwidth::BandwidthCommandError::Journal(error) => journal_error_code(error),
        bandwidth::BandwidthCommandError::Scheduler(error) => scheduler_error_code(error),
    }
}
