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
mod tls;

use std::io::IsTerminal;
use std::process::ExitCode;

use directories::{BaseDirs, ProjectDirs};

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
    let state_dir = default_state_dir(&current_dir);
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

fn default_state_dir(current_dir: &std::path::Path) -> std::path::PathBuf {
    if let Some(state_dir) = ProjectDirs::from("dev", "netband", "netband")
        .and_then(|dirs| dirs.state_dir().map(std::path::Path::to_path_buf))
    {
        return state_dir;
    }
    BaseDirs::new()
        .map(|dirs| dirs.data_local_dir().join("netband").join("state"))
        .unwrap_or_else(|| current_dir.join(".netband").join("state"))
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

#[cfg(test)]
mod state_directory_tests {
    use super::*;

    #[test]
    fn platform_state_directory_is_not_relative_to_the_working_directory() {
        let work = tempfile::tempdir().unwrap();
        let actual = default_state_dir(work.path());
        assert!(actual.is_absolute());
        assert!(!actual.starts_with(work.path()));

        #[cfg(windows)]
        assert_eq!(
            actual,
            BaseDirs::new()
                .unwrap()
                .data_local_dir()
                .join("netband")
                .join("state")
        );
    }
}

#[cfg(test)]
mod orchestration_tests {
    use super::*;
    use std::io;

    #[test]
    fn journal_failures_have_consistent_exit_codes_across_commands() {
        for case in 0..6 {
            let make = || match case {
                0 => journal::JournalError::Io(io::Error::from(io::ErrorKind::PermissionDenied)),
                1 => journal::JournalError::Io(io::Error::other("write failed")),
                2 => journal::JournalError::Header("results.csv".into()),
                3 => journal::JournalError::Corrupt("results.csv".into()),
                4 => journal::JournalError::Locked("results.csv".into()),
                _ => journal::JournalError::Csv(csv::Error::from(io::Error::other("CSV failed"))),
            };
            let expected = if case == 0 { 3 } else { 4 };
            assert_eq!(ping_error_code(&make().into()), expected);
            assert_eq!(monitor_error_code(&make().into()), expected);
            assert_eq!(bandwidth_error_code(&make().into()), expected);
        }
    }

    #[test]
    fn scheduler_failures_have_consistent_exit_codes_across_commands() {
        for case in 0..6 {
            let make = || match case {
                0 | 1 => scheduler::SchedulerError::Io {
                    path: "scheduler.json".into(),
                    source: io::Error::from(if case == 0 {
                        io::ErrorKind::PermissionDenied
                    } else {
                        io::ErrorKind::Other
                    }),
                },
                2 => scheduler::SchedulerError::Locked("scheduler.lock".into()),
                3 => scheduler::SchedulerError::Corrupt {
                    path: "scheduler.json".into(),
                    message: "fixture".into(),
                },
                4 => scheduler::SchedulerError::UnsupportedSchema(99),
                _ => scheduler::SchedulerError::Admission("fixture".into()),
            };
            let expected = if case == 0 { 3 } else { 4 };
            assert_eq!(monitor_error_code(&make().into()), expected);
            assert_eq!(bandwidth_error_code(&make().into()), expected);
        }
        assert_eq!(ping_error_code(&ping::PingRoundError::NoTargets.into()), 5);
        assert_eq!(
            monitor_error_code(&ping::PingRoundError::NoTargets.into()),
            5
        );
    }

    #[tokio::test]
    async fn task_failure_maps_to_internal_error() {
        let error = tokio::spawn(async { panic!("fixture task failure") })
            .await
            .unwrap_err();
        assert_eq!(monitor_error_code(&error.into()), 5);
    }

    #[tokio::test]
    async fn normal_completion_preserves_result_without_requesting_shutdown() {
        for result in [Ok(()), Err("operation error")] {
            let (sender, receiver) = tokio::sync::watch::channel(false);
            let completion =
                supervise_command(std::time::Duration::from_secs(1), sender, async { result })
                    .await
                    .unwrap();
            assert_eq!(completion.result, result);
            assert!(!completion.shutdown_requested);
            assert!(!*receiver.borrow());
        }
    }

    #[tokio::test]
    async fn one_shot_commands_reject_multiple_interfaces_before_execution() {
        use clap::Parser;
        let root = tempfile::tempdir().unwrap();
        let cli = Cli::try_parse_from(["netband", "config", "check"]).unwrap();
        let mut config = resolve(
            &cli,
            &ResolveContext {
                stdout_is_terminal: false,
                current_dir: root.path().to_owned(),
                state_dir: root.path().join("state"),
            },
        )
        .unwrap();
        config.interfaces = vec!["fixture-a".into(), "fixture-b".into()];
        assert_eq!(run_once_ping(&config).await, ExitCode::from(2));
        assert_eq!(run_once_bandwidth(&config).await, ExitCode::from(2));
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }
}
