pub mod cli;
pub mod config;
pub mod console;
pub mod diagnostics;
pub mod journal;
pub mod model;

use std::io::IsTerminal;
use std::process::ExitCode;

use directories::ProjectDirs;

use crate::cli::{Cli, CommandKind};
use crate::config::{ResolveContext, resolve, validate_environment};

pub fn run(cli: Cli) -> ExitCode {
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
        CommandKind::Run | CommandKind::OncePing | CommandKind::OnceBandwidth => {
            tracing::error!("measurement command is not implemented yet; configuration is valid");
            ExitCode::from(3)
        }
    }
}
