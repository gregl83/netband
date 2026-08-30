use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};
use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandKind {
    Run,
    OncePing,
    OnceBandwidth,
    ConfigCheck,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConsoleMode {
    Auto,
    Human,
    Jsonl,
    Off,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verbosity {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    Mlab,
    Direct,
}

#[derive(Debug, Parser)]
#[command(
    name = "netband",
    version,
    about = "Measure network latency and bandwidth from the command line"
)]
pub struct Cli {
    #[command(flatten)]
    pub options: Options,

    #[command(subcommand)]
    pub command: Commands,
}

impl Cli {
    pub fn command_kind(&self) -> CommandKind {
        match self.command {
            Commands::Run => CommandKind::Run,
            Commands::Once {
                command: OnceCommand::Ping,
            } => CommandKind::OncePing,
            Commands::Once {
                command: OnceCommand::Bandwidth,
            } => CommandKind::OnceBandwidth,
            Commands::Config {
                command: ConfigCommand::Check,
            } => CommandKind::ConfigCheck,
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Run continuous monitoring in the foreground
    Run,
    /// Run one measurement and exit
    Once {
        #[command(subcommand)]
        command: OnceCommand,
    },
    /// Inspect configuration without performing measurements
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
}

#[derive(Debug, Subcommand)]
pub enum OnceCommand {
    /// Run one ping round; exits 0 when all targets reply and 1 otherwise
    Ping,
    /// Run one bandwidth test
    Bandwidth,
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Resolve and validate the effective configuration
    Check,
}

#[derive(Debug, Default, Args)]
pub struct Options {
    /// Read configuration from this TOML file
    #[arg(long, global = true, value_name = "FILE")]
    pub config: Option<PathBuf>,

    /// Select live stdout presentation
    #[arg(long, global = true, value_enum)]
    pub console: Option<ConsoleMode>,

    /// Restrict measurements to an interface; repeat to add interfaces
    #[arg(long = "interface", global = true, value_name = "NAME", action = clap::ArgAction::Append)]
    pub interfaces: Vec<String>,

    /// Ping an IP address; repeat to replace the default target list
    #[arg(long = "ping-target", global = true, value_name = "IP", action = clap::ArgAction::Append)]
    pub ping_targets: Vec<String>,

    /// Delay between ping rounds, such as 5s
    #[arg(long, global = true, value_name = "DURATION")]
    pub ping_interval: Option<String>,

    /// Per-ping timeout, such as 2s
    #[arg(long, global = true, value_name = "DURATION")]
    pub ping_timeout: Option<String>,

    /// Write measurements to this CSV file
    #[arg(
        long,
        global = true,
        value_name = "FILE",
        conflicts_with = "output_dir"
    )]
    pub output: Option<PathBuf>,

    /// Create a timestamped CSV in this directory
    #[arg(long, global = true, value_name = "DIR")]
    pub output_dir: Option<PathBuf>,

    /// Persist scheduler state at this path
    #[arg(long, global = true, value_name = "FILE")]
    pub state_file: Option<PathBuf>,

    /// Set operational log verbosity
    #[arg(long, global = true, value_enum)]
    pub verbosity: Option<Verbosity>,

    /// Select M-Lab discovery or a directly managed NDT7 server
    #[arg(long, global = true, value_enum)]
    pub ndt_provider: Option<ProviderKind>,

    /// Override the M-Lab Locate URL (intended for local testing)
    #[arg(long, global = true, value_name = "URL")]
    pub mlab_locate_url: Option<String>,

    /// Direct NDT7 host or IP with an optional port
    #[arg(long, global = true, value_name = "HOST[:PORT]")]
    pub ndt_target: Option<String>,

    /// Direct NDT7 download WebSocket URL
    #[arg(long, global = true, value_name = "URL")]
    pub ndt_download_url: Option<String>,

    /// Direct NDT7 upload WebSocket URL
    #[arg(long, global = true, value_name = "URL")]
    pub ndt_upload_url: Option<String>,

    /// Certificate DNS name for a direct IP endpoint
    #[arg(long, global = true, value_name = "DNS_NAME")]
    pub ndt_tls_server_name: Option<String>,

    /// PEM CA bundle for a private direct server
    #[arg(long, global = true, value_name = "FILE")]
    pub ndt_ca_cert: Option<PathBuf>,

    /// Permit plain ws:// only for an explicitly trusted private network
    #[arg(long, global = true)]
    pub allow_insecure_ndt: bool,

    /// Maximum bandwidth runs per UTC day; 0 disables them
    #[arg(long, global = true, value_name = "COUNT")]
    pub bandwidth_daily_max: Option<u32>,

    /// Minimum time between bandwidth runs
    #[arg(long, global = true, value_name = "DURATION")]
    pub bandwidth_min_spacing: Option<String>,

    /// Maximum random displacement within a planned slot
    #[arg(long, global = true, value_name = "PERCENT")]
    pub bandwidth_slot_jitter_pct: Option<u8>,

    /// Whole bandwidth test timeout
    #[arg(long, global = true, value_name = "DURATION")]
    pub bandwidth_timeout: Option<String>,

    /// Time reserved to stop a bandwidth test cleanly
    #[arg(long, global = true, value_name = "DURATION")]
    pub bandwidth_shutdown_margin: Option<String>,

    /// Ping rounds kept in the trigger window
    #[arg(long, global = true, value_name = "ROUNDS")]
    pub loss_window_rounds: Option<u32>,

    /// Minimum probes required before evaluating a trigger
    #[arg(long, global = true, value_name = "COUNT")]
    pub loss_min_samples: Option<u32>,

    /// Packet-loss percentage that requests an early bandwidth run
    #[arg(long, global = true, value_name = "PERCENT")]
    pub loss_threshold_pct: Option<f64>,

    /// Optional p95 RTT threshold in milliseconds
    #[arg(long, global = true, value_name = "MILLISECONDS")]
    pub rtt_threshold_ms: Option<f64>,

    /// Loss percentage required to rearm the trigger
    #[arg(long, global = true, value_name = "PERCENT")]
    pub recovery_loss_pct: Option<f64>,

    /// Consecutive healthy rounds required to rearm
    #[arg(long, global = true, value_name = "ROUNDS")]
    pub recovery_rounds: Option<u32>,

    /// Maximum age of a deferred ping trigger
    #[arg(long, global = true, value_name = "DURATION")]
    pub pending_trigger_ttl: Option<String>,

    /// Initial provider rate-limit cooldown
    #[arg(long, global = true, value_name = "DURATION")]
    pub cooldown_initial: Option<String>,

    /// Maximum provider rate-limit cooldown
    #[arg(long, global = true, value_name = "DURATION")]
    pub cooldown_max: Option<String>,

    /// Acknowledge M-Lab acceptable-use and privacy policies
    #[arg(long, global = true)]
    pub accept_mlab_policy: bool,
}
