use std::collections::HashSet;
use std::fmt::Write as _;
use std::fs;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use sha2::{Digest, Sha256};
use thiserror::Error;
use url::Url;

use crate::cli::{Cli, CommandKind, ConsoleMode, ProviderKind, Verbosity};

const DEFAULT_TARGETS: [&str; 3] = ["1.1.1.1", "8.8.8.8", "9.9.9.9"];
const DEFAULT_LOCATE_URL: &str = "https://locate.measurementlab.net/v2/nearest/ndt/ndt7";
const MLAB_AUP_URL: &str = "https://www.measurementlab.net/aup/";
const MLAB_PRIVACY_URL: &str = "https://www.measurementlab.net/privacy/";

#[derive(Debug, Error)]
#[error("{0}")]
pub struct ConfigError(String);

fn error(message: impl Into<String>) -> ConfigError {
    ConfigError(message.into())
}

#[derive(Debug, Clone)]
pub struct ResolveContext {
    pub stdout_is_terminal: bool,
    pub current_dir: PathBuf,
    pub state_dir: PathBuf,
}

#[derive(Debug, Clone)]
pub struct ResolvedConfig {
    pub command: CommandKind,
    pub console: ConsoleMode,
    pub verbosity: Verbosity,
    pub interfaces: Vec<String>,
    pub output: OutputTarget,
    pub state_file: PathBuf,
    pub ping: PingConfig,
    pub bandwidth: BandwidthConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputTarget {
    File(PathBuf),
    Directory(PathBuf),
}

#[derive(Debug, Clone)]
pub struct PingConfig {
    pub targets: Vec<IpAddr>,
    pub interval: Duration,
    pub timeout: Duration,
}

#[derive(Debug, Clone)]
pub struct BandwidthConfig {
    pub provider: ProviderConfig,
    pub provider_id: String,
    pub automatic_enabled: bool,
    pub daily_max: u32,
    pub min_spacing: Duration,
    pub slot_jitter_pct: u8,
    pub whole_test_timeout: Duration,
    pub shutdown_margin: Duration,
    pub trigger: TriggerConfig,
    pub cooldown: CooldownConfig,
}

#[derive(Debug, Clone)]
pub enum ProviderConfig {
    Mlab(MlabConfig),
    Direct(DirectConfig),
}

#[derive(Debug, Clone)]
pub struct MlabConfig {
    pub locate_url: Url,
    pub policy_accepted: bool,
}

#[derive(Debug, Clone)]
pub struct DirectConfig {
    pub target: Option<String>,
    pub download_url: Url,
    pub upload_url: Url,
    pub tls_server_name: Option<String>,
    pub ca_cert: Option<PathBuf>,
    pub allow_insecure: bool,
}

#[derive(Debug, Clone)]
pub struct TriggerConfig {
    pub window_rounds: u32,
    pub min_samples: u32,
    pub loss_threshold_pct: f64,
    pub rtt_threshold_ms: Option<f64>,
    pub recovery_loss_pct: f64,
    pub recovery_rounds: u32,
    pub pending_ttl: Duration,
}

#[derive(Debug, Clone)]
pub struct CooldownConfig {
    pub initial: Duration,
    pub max: Duration,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    console: Option<ConsoleMode>,
    verbosity: Option<Verbosity>,
    interfaces: Option<Vec<String>>,
    output: Option<PathBuf>,
    output_dir: Option<PathBuf>,
    state_file: Option<PathBuf>,
    ping: Option<FilePing>,
    bandwidth: Option<FileBandwidth>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FilePing {
    targets: Option<Vec<String>>,
    interval: Option<String>,
    timeout: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileBandwidth {
    provider: Option<ProviderKind>,
    daily_max: Option<u32>,
    min_spacing: Option<String>,
    slot_jitter_pct: Option<u8>,
    whole_test_timeout: Option<String>,
    shutdown_margin: Option<String>,
    accept_mlab_policy: Option<bool>,
    trigger: Option<FileTrigger>,
    cooldown: Option<FileCooldown>,
    mlab: Option<FileMlab>,
    direct: Option<FileDirect>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileTrigger {
    window_rounds: Option<u32>,
    min_samples: Option<u32>,
    loss_threshold_pct: Option<f64>,
    rtt_threshold_ms: Option<f64>,
    recovery_loss_pct: Option<f64>,
    recovery_rounds: Option<u32>,
    pending_ttl: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileCooldown {
    initial: Option<String>,
    max: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileMlab {
    locate_url: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileDirect {
    target: Option<String>,
    download_url: Option<String>,
    upload_url: Option<String>,
    tls_server_name: Option<String>,
    ca_cert: Option<PathBuf>,
    allow_insecure: Option<bool>,
}

pub fn resolve(cli: &Cli, context: &ResolveContext) -> Result<ResolvedConfig, ConfigError> {
    let file = load_file(cli.options.config.as_deref())?;
    let command = cli.command_kind();

    let requested_console = cli
        .options
        .console
        .or(file.console)
        .unwrap_or(match command {
            CommandKind::Run | CommandKind::ConfigCheck => ConsoleMode::Auto,
            CommandKind::OncePing | CommandKind::OnceBandwidth => ConsoleMode::Human,
        });
    let console = match requested_console {
        ConsoleMode::Auto if context.stdout_is_terminal => ConsoleMode::Human,
        ConsoleMode::Auto => ConsoleMode::Off,
        mode => mode,
    };

    let interfaces = if cli.options.interfaces.is_empty() {
        file.interfaces.unwrap_or_default()
    } else {
        cli.options.interfaces.clone()
    };
    validate_interfaces(&interfaces)?;

    let file_ping = file.ping.unwrap_or_default();
    let target_strings = if !cli.options.ping_targets.is_empty() {
        cli.options.ping_targets.clone()
    } else if let Some(targets) = file_ping.targets {
        targets
    } else {
        DEFAULT_TARGETS
            .iter()
            .map(|target| (*target).to_owned())
            .collect()
    };
    if target_strings.is_empty() {
        return Err(error("at least one ping target is required"));
    }
    let targets = target_strings
        .iter()
        .map(|target| {
            target
                .parse::<IpAddr>()
                .map_err(|_| error(format!("invalid ping target IP: {target}")))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut unique_targets = HashSet::new();
    for target in &targets {
        if !unique_targets.insert(*target) {
            return Err(error(format!("duplicate ping target: {target}")));
        }
    }
    let interval = parse_duration(
        "ping interval",
        cli.options
            .ping_interval
            .as_deref()
            .or(file_ping.interval.as_deref())
            .unwrap_or("5s"),
    )?;
    let timeout = parse_duration(
        "ping timeout",
        cli.options
            .ping_timeout
            .as_deref()
            .or(file_ping.timeout.as_deref())
            .unwrap_or("2s"),
    )?;

    let output = resolve_output(
        cli.options.output.as_ref(),
        cli.options.output_dir.as_ref(),
        file.output.as_ref(),
        file.output_dir.as_ref(),
        &context.current_dir,
    )?;
    let state_file = make_absolute(
        cli.options
            .state_file
            .as_ref()
            .or(file.state_file.as_ref())
            .cloned()
            .unwrap_or_else(|| context.state_dir.join("scheduler.json")),
        &context.current_dir,
    );

    let bandwidth = resolve_bandwidth(cli, file.bandwidth.unwrap_or_default(), context)?;

    Ok(ResolvedConfig {
        command,
        console,
        verbosity: cli
            .options
            .verbosity
            .or(file.verbosity)
            .unwrap_or(Verbosity::Info),
        interfaces,
        output,
        state_file,
        ping: PingConfig {
            targets,
            interval,
            timeout,
        },
        bandwidth,
    })
}

fn load_file(path: Option<&Path>) -> Result<FileConfig, ConfigError> {
    let Some(path) = path else {
        return Ok(FileConfig::default());
    };
    let contents = fs::read_to_string(path).map_err(|source| {
        error(format!(
            "cannot read config file {}: {source}",
            path.display()
        ))
    })?;
    toml::from_str(&contents).map_err(|source| {
        error(format!(
            "cannot parse config file {}: {source}",
            path.display()
        ))
    })
}

fn resolve_bandwidth(
    cli: &Cli,
    file: FileBandwidth,
    context: &ResolveContext,
) -> Result<BandwidthConfig, ConfigError> {
    let kind = cli
        .options
        .ndt_provider
        .or(file.provider)
        .unwrap_or(ProviderKind::Mlab);
    let daily_max = cli
        .options
        .bandwidth_daily_max
        .or(file.daily_max)
        .unwrap_or(4);
    let min_spacing = parse_duration(
        "bandwidth minimum spacing",
        cli.options
            .bandwidth_min_spacing
            .as_deref()
            .or(file.min_spacing.as_deref())
            .unwrap_or("36m"),
    )?;
    let slot_jitter_pct = cli
        .options
        .bandwidth_slot_jitter_pct
        .or(file.slot_jitter_pct)
        .unwrap_or(50);
    if slot_jitter_pct > 100 {
        return Err(error(
            "bandwidth slot jitter must be between 0 and 100 percent",
        ));
    }
    let whole_test_timeout = parse_duration(
        "bandwidth timeout",
        cli.options
            .bandwidth_timeout
            .as_deref()
            .or(file.whole_test_timeout.as_deref())
            .unwrap_or("55s"),
    )?;
    let shutdown_margin = parse_duration(
        "bandwidth shutdown margin",
        cli.options
            .bandwidth_shutdown_margin
            .as_deref()
            .or(file.shutdown_margin.as_deref())
            .unwrap_or("15s"),
    )?;

    let file_trigger = file.trigger.unwrap_or_default();
    let trigger = TriggerConfig {
        window_rounds: positive_u32(
            "loss window rounds",
            cli.options
                .loss_window_rounds
                .or(file_trigger.window_rounds)
                .unwrap_or(6),
        )?,
        min_samples: positive_u32(
            "loss minimum samples",
            cli.options
                .loss_min_samples
                .or(file_trigger.min_samples)
                .unwrap_or(6),
        )?,
        loss_threshold_pct: percentage(
            "loss threshold",
            cli.options
                .loss_threshold_pct
                .or(file_trigger.loss_threshold_pct)
                .unwrap_or(50.0),
        )?,
        rtt_threshold_ms: optional_positive_f64(
            "RTT threshold",
            cli.options
                .rtt_threshold_ms
                .or(file_trigger.rtt_threshold_ms),
        )?,
        recovery_loss_pct: percentage(
            "recovery loss threshold",
            cli.options
                .recovery_loss_pct
                .or(file_trigger.recovery_loss_pct)
                .unwrap_or(10.0),
        )?,
        recovery_rounds: positive_u32(
            "recovery rounds",
            cli.options
                .recovery_rounds
                .or(file_trigger.recovery_rounds)
                .unwrap_or(3),
        )?,
        pending_ttl: parse_duration(
            "pending trigger lifetime",
            cli.options
                .pending_trigger_ttl
                .as_deref()
                .or(file_trigger.pending_ttl.as_deref())
                .unwrap_or("30m"),
        )?,
    };
    if trigger.recovery_loss_pct >= trigger.loss_threshold_pct {
        return Err(error(
            "recovery loss threshold must be below the trigger loss threshold",
        ));
    }

    let file_cooldown = file.cooldown.unwrap_or_default();
    let cooldown = CooldownConfig {
        initial: parse_duration(
            "initial cooldown",
            cli.options
                .cooldown_initial
                .as_deref()
                .or(file_cooldown.initial.as_deref())
                .unwrap_or("60s"),
        )?,
        max: parse_duration(
            "maximum cooldown",
            cli.options
                .cooldown_max
                .as_deref()
                .or(file_cooldown.max.as_deref())
                .unwrap_or("16m"),
        )?,
    };
    if cooldown.initial > cooldown.max {
        return Err(error("initial cooldown cannot exceed maximum cooldown"));
    }

    let direct_file = file.direct.unwrap_or_default();
    let mlab_file = file.mlab.unwrap_or_default();
    let has_direct_options = cli.options.ndt_target.is_some()
        || cli.options.ndt_download_url.is_some()
        || cli.options.ndt_upload_url.is_some()
        || cli.options.ndt_tls_server_name.is_some()
        || cli.options.ndt_ca_cert.is_some()
        || cli.options.allow_insecure_ndt
        || direct_file.target.is_some()
        || direct_file.download_url.is_some()
        || direct_file.upload_url.is_some()
        || direct_file.tls_server_name.is_some()
        || direct_file.ca_cert.is_some()
        || direct_file.allow_insecure.unwrap_or(false);
    let locate_override = cli.options.mlab_locate_url.clone().or(mlab_file.locate_url);

    let (provider, provider_id, automatic_enabled) = match kind {
        ProviderKind::Mlab => {
            if has_direct_options {
                return Err(error("direct NDT options require --ndt-provider direct"));
            }
            if daily_max > 4 {
                return Err(error("M-Lab permits at most 4 bandwidth runs per UTC day"));
            }
            if min_spacing < Duration::from_secs(36 * 60) {
                return Err(error(
                    "M-Lab bandwidth minimum spacing must be at least 36m",
                ));
            }
            let locate_url = parse_http_url(
                "M-Lab Locate URL",
                locate_override.as_deref().unwrap_or(DEFAULT_LOCATE_URL),
            )?;
            let policy_accepted =
                cli.options.accept_mlab_policy || file.accept_mlab_policy.unwrap_or(false);
            (
                ProviderConfig::Mlab(MlabConfig {
                    locate_url,
                    policy_accepted,
                }),
                "mlab".to_owned(),
                daily_max > 0 && policy_accepted,
            )
        }
        ProviderKind::Direct => {
            if locate_override.is_some() {
                return Err(error(
                    "M-Lab Locate URL does not apply to the direct provider",
                ));
            }
            if cli.options.accept_mlab_policy || file.accept_mlab_policy.unwrap_or(false) {
                return Err(error(
                    "M-Lab policy acknowledgement does not apply to direct provider",
                ));
            }
            let direct = resolve_direct(cli, direct_file, context)?;
            let provider_id = direct_provider_id(&direct);
            (ProviderConfig::Direct(direct), provider_id, daily_max > 0)
        }
    };

    if kind == ProviderKind::Direct {
        let minimum = whole_test_timeout
            .checked_add(shutdown_margin)
            .ok_or_else(|| error("bandwidth timeout plus shutdown margin is too large"))?
            .max(Duration::from_secs(60));
        if min_spacing < minimum {
            return Err(error(format!(
                "direct bandwidth minimum spacing must be at least {}",
                humantime::format_duration(minimum)
            )));
        }
        let capacity = 86_400 / min_spacing.as_secs();
        if u64::from(daily_max) > capacity {
            return Err(error(format!(
                "daily maximum {daily_max} cannot fit in 24h with {} minimum spacing",
                humantime::format_duration(min_spacing)
            )));
        }
    }

    Ok(BandwidthConfig {
        provider,
        provider_id,
        automatic_enabled,
        daily_max,
        min_spacing,
        slot_jitter_pct,
        whole_test_timeout,
        shutdown_margin,
        trigger,
        cooldown,
    })
}

fn resolve_direct(
    cli: &Cli,
    file: FileDirect,
    context: &ResolveContext,
) -> Result<DirectConfig, ConfigError> {
    let target = cli.options.ndt_target.clone().or(file.target);
    let download = cli.options.ndt_download_url.clone().or(file.download_url);
    let upload = cli.options.ndt_upload_url.clone().or(file.upload_url);
    if target.is_some() && (download.is_some() || upload.is_some()) {
        return Err(error(
            "direct target and explicit download/upload URLs are mutually exclusive",
        ));
    }
    if download.is_some() != upload.is_some() {
        return Err(error(
            "direct download and upload URLs must be supplied together",
        ));
    }
    if target.is_none() && download.is_none() {
        return Err(error(
            "direct provider requires a target or download/upload URL pair",
        ));
    }

    let allow_insecure = cli.options.allow_insecure_ndt || file.allow_insecure.unwrap_or(false);
    let (download_url, upload_url) = if let Some(raw_target) = target.as_deref() {
        direct_target_urls(raw_target)?
    } else {
        (
            parse_ndt_url(
                "direct download URL",
                download.as_deref().unwrap(),
                allow_insecure,
            )?,
            parse_ndt_url(
                "direct upload URL",
                upload.as_deref().unwrap(),
                allow_insecure,
            )?,
        )
    };

    for url in [&download_url, &upload_url] {
        if url.scheme() == "ws" && !allow_insecure {
            return Err(error(
                "plain ws:// requires the explicit --allow-insecure-ndt opt-in",
            ));
        }
    }

    let tls_server_name = cli
        .options
        .ndt_tls_server_name
        .clone()
        .or(file.tls_server_name);
    if let Some(name) = tls_server_name.as_deref()
        && !valid_dns_name(name)
    {
        return Err(error(format!("invalid TLS server name: {name}")));
    }
    if tls_server_name.is_some() && download_url.scheme() != "wss" {
        return Err(error("TLS server name requires wss:// direct endpoints"));
    }

    let ca_cert = cli
        .options
        .ndt_ca_cert
        .clone()
        .or(file.ca_cert)
        .map(|path| make_absolute(path, &context.current_dir));
    if ca_cert.is_some() && download_url.scheme() != "wss" {
        return Err(error(
            "a private CA certificate requires wss:// direct endpoints",
        ));
    }

    Ok(DirectConfig {
        target,
        download_url,
        upload_url,
        tls_server_name,
        ca_cert,
        allow_insecure,
    })
}

fn direct_target_urls(target: &str) -> Result<(Url, Url), ConfigError> {
    if target.trim().is_empty()
        || target.contains('/')
        || target.contains('?')
        || target.contains('#')
    {
        return Err(error(format!("invalid direct target: {target}")));
    }
    let base = if let Ok(address) = target.parse::<IpAddr>() {
        match address {
            IpAddr::V4(_) => format!("wss://{address}"),
            IpAddr::V6(_) => format!("wss://[{address}]"),
        }
    } else {
        format!("wss://{target}")
    };
    let parsed =
        Url::parse(&base).map_err(|_| error(format!("invalid direct target: {target}")))?;
    if parsed.host().is_none() || !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(error(format!("invalid direct target: {target}")));
    }
    let mut download = parsed.clone();
    download.set_path("/ndt/v7/download");
    let mut upload = parsed;
    upload.set_path("/ndt/v7/upload");
    Ok((download, upload))
}

fn parse_ndt_url(name: &str, value: &str, allow_insecure: bool) -> Result<Url, ConfigError> {
    let url = Url::parse(value).map_err(|_| error(format!("invalid {name}")))?;
    if url.scheme() != "wss" && url.scheme() != "ws" {
        return Err(error(format!("{name} must use wss:// or ws://")));
    }
    if url.scheme() == "ws" && !allow_insecure {
        return Err(error(format!(
            "{name} uses ws://; pass --allow-insecure-ndt only for a trusted private network"
        )));
    }
    if url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(error(format!(
            "invalid {name}: credentials and fragments are not allowed"
        )));
    }
    Ok(url)
}

fn parse_http_url(name: &str, value: &str) -> Result<Url, ConfigError> {
    let url = Url::parse(value).map_err(|_| error(format!("invalid {name}")))?;
    if !matches!(url.scheme(), "http" | "https") || url.host().is_none() {
        return Err(error(format!("{name} must be an http(s) URL")));
    }
    if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
        return Err(error(format!(
            "invalid {name}: credentials and fragments are not allowed"
        )));
    }
    Ok(url)
}

fn direct_provider_id(direct: &DirectConfig) -> String {
    let mut hasher = Sha256::new();
    hasher.update(endpoint_identity(&direct.download_url));
    hasher.update(b"\n");
    hasher.update(endpoint_identity(&direct.upload_url));
    if let Some(name) = direct.tls_server_name.as_deref() {
        hasher.update(b"\nsni=");
        hasher.update(name.to_ascii_lowercase());
    }
    let digest = hasher.finalize();
    let mut prefix = String::with_capacity(16);
    for byte in &digest[..8] {
        let _ = write!(prefix, "{byte:02x}");
    }
    format!("direct:{prefix}")
}

fn endpoint_identity(url: &Url) -> String {
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    let port = url.port_or_known_default().unwrap_or_default();
    format!("{}://{host}:{port}{}", url.scheme(), url.path())
}

fn resolve_output(
    cli_file: Option<&PathBuf>,
    cli_dir: Option<&PathBuf>,
    file_file: Option<&PathBuf>,
    file_dir: Option<&PathBuf>,
    current_dir: &Path,
) -> Result<OutputTarget, ConfigError> {
    if cli_file.is_none() && cli_dir.is_none() && file_file.is_some() && file_dir.is_some() {
        return Err(error("config cannot set both output and output_dir"));
    }
    if let Some(path) = cli_file {
        return Ok(OutputTarget::File(make_absolute(path.clone(), current_dir)));
    }
    if let Some(path) = cli_dir {
        return Ok(OutputTarget::Directory(make_absolute(
            path.clone(),
            current_dir,
        )));
    }
    if let Some(path) = file_file {
        return Ok(OutputTarget::File(make_absolute(path.clone(), current_dir)));
    }
    if let Some(path) = file_dir {
        return Ok(OutputTarget::Directory(make_absolute(
            path.clone(),
            current_dir,
        )));
    }
    Ok(OutputTarget::Directory(current_dir.to_path_buf()))
}

fn make_absolute(path: PathBuf, current_dir: &Path) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        current_dir.join(path)
    }
}

fn parse_duration(name: &str, value: &str) -> Result<Duration, ConfigError> {
    let duration = humantime::parse_duration(value)
        .map_err(|source| error(format!("invalid {name} {value:?}: {source}")))?;
    if duration.is_zero() {
        return Err(error(format!("{name} must be greater than zero")));
    }
    Ok(duration)
}

fn positive_u32(name: &str, value: u32) -> Result<u32, ConfigError> {
    if value == 0 {
        Err(error(format!("{name} must be greater than zero")))
    } else {
        Ok(value)
    }
}

fn percentage(name: &str, value: f64) -> Result<f64, ConfigError> {
    if value.is_finite() && (0.0..=100.0).contains(&value) {
        Ok(value)
    } else {
        Err(error(format!("{name} must be between 0 and 100 percent")))
    }
}

fn optional_positive_f64(name: &str, value: Option<f64>) -> Result<Option<f64>, ConfigError> {
    match value {
        Some(value) if value.is_finite() && value > 0.0 => Ok(Some(value)),
        Some(_) => Err(error(format!("{name} must be greater than zero"))),
        None => Ok(None),
    }
}

fn validate_interfaces(interfaces: &[String]) -> Result<(), ConfigError> {
    let mut seen = HashSet::new();
    for interface in interfaces {
        if interface.trim().is_empty() {
            return Err(error("network interface name cannot be empty"));
        }
        if !seen.insert(interface) {
            return Err(error(format!("duplicate interface: {interface}")));
        }
    }
    Ok(())
}

fn valid_dns_name(name: &str) -> bool {
    if name.len() > 253 || name.is_empty() || name.parse::<IpAddr>().is_ok() {
        return false;
    }
    name.trim_end_matches('.').split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    })
}

pub fn validate_environment(config: &ResolvedConfig) -> Result<(), ConfigError> {
    if !config.interfaces.is_empty() {
        let available = if_addrs::get_if_addrs()
            .map_err(|source| error(format!("cannot inspect network interfaces: {source}")))?
            .into_iter()
            .map(|interface| interface.name)
            .collect::<HashSet<_>>();
        for interface in &config.interfaces {
            if !available.contains(interface) {
                return Err(error(format!(
                    "network interface does not exist: {interface}"
                )));
            }
        }
    }

    let directory = match &config.output {
        OutputTarget::File(path) => path
            .parent()
            .ok_or_else(|| error(format!("output file has no parent: {}", path.display())))?,
        OutputTarget::Directory(path) => path.as_path(),
    };
    validate_directory("output", directory)?;

    if let ProviderConfig::Direct(direct) = &config.bandwidth.provider
        && let Some(path) = direct.ca_cert.as_deref()
    {
        let metadata = fs::metadata(path).map_err(|source| {
            error(format!(
                "cannot read NDT CA certificate {}: {source}",
                path.display()
            ))
        })?;
        if !metadata.is_file() {
            return Err(error(format!(
                "NDT CA certificate is not a file: {}",
                path.display()
            )));
        }
        fs::File::open(path).map_err(|source| {
            error(format!(
                "cannot read NDT CA certificate {}: {source}",
                path.display()
            ))
        })?;
    }
    Ok(())
}

fn validate_directory(name: &str, directory: &Path) -> Result<(), ConfigError> {
    let metadata = fs::metadata(directory).map_err(|source| {
        error(format!(
            "{name} directory {} is not accessible: {source}",
            directory.display()
        ))
    })?;
    if !metadata.is_dir() {
        return Err(error(format!(
            "{name} path is not a directory: {}",
            directory.display()
        )));
    }
    if metadata.permissions().readonly() {
        return Err(error(format!(
            "{name} directory is read-only: {}",
            directory.display()
        )));
    }
    Ok(())
}

impl ResolvedConfig {
    pub fn summary(&self) -> String {
        let mut summary = String::new();
        let provider_name = match self.bandwidth.provider {
            ProviderConfig::Mlab(_) => "mlab",
            ProviderConfig::Direct(_) => "direct",
        };
        let output = match &self.output {
            OutputTarget::File(path) => format!("file:{}", path.display()),
            OutputTarget::Directory(path) => format!("directory:{}", path.display()),
        };
        let _ = writeln!(summary, "configuration=valid");
        let _ = writeln!(summary, "command={}", command_name(self.command));
        let _ = writeln!(summary, "console={}", enum_name(self.console));
        let _ = writeln!(summary, "verbosity={}", verbosity_name(self.verbosity));
        let _ = writeln!(
            summary,
            "interfaces={}",
            if self.interfaces.is_empty() {
                "default-route".to_owned()
            } else {
                self.interfaces.join(",")
            }
        );
        let _ = writeln!(summary, "output={output}");
        let _ = writeln!(summary, "state_file={}", self.state_file.display());
        let _ = writeln!(
            summary,
            "ping.targets={}",
            self.ping
                .targets
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(",")
        );
        let _ = writeln!(
            summary,
            "ping.interval={}",
            humantime::format_duration(self.ping.interval)
        );
        let _ = writeln!(
            summary,
            "ping.timeout={}",
            humantime::format_duration(self.ping.timeout)
        );
        let _ = writeln!(summary, "bandwidth.provider={provider_name}");
        let _ = writeln!(
            summary,
            "bandwidth.provider_id={}",
            self.bandwidth.provider_id
        );
        let _ = writeln!(
            summary,
            "bandwidth.automatic_enabled={}",
            self.bandwidth.automatic_enabled
        );
        let _ = writeln!(summary, "bandwidth.daily_max={}", self.bandwidth.daily_max);
        let _ = writeln!(
            summary,
            "bandwidth.min_spacing={}",
            humantime::format_duration(self.bandwidth.min_spacing)
        );
        let _ = writeln!(
            summary,
            "bandwidth.slot_jitter_pct={}",
            self.bandwidth.slot_jitter_pct
        );
        let _ = writeln!(
            summary,
            "bandwidth.whole_test_timeout={}",
            humantime::format_duration(self.bandwidth.whole_test_timeout)
        );
        let _ = writeln!(
            summary,
            "bandwidth.shutdown_margin={}",
            humantime::format_duration(self.bandwidth.shutdown_margin)
        );
        let _ = writeln!(
            summary,
            "trigger.window_rounds={}",
            self.bandwidth.trigger.window_rounds
        );
        let _ = writeln!(
            summary,
            "trigger.min_samples={}",
            self.bandwidth.trigger.min_samples
        );
        let _ = writeln!(
            summary,
            "trigger.loss_threshold_pct={}",
            self.bandwidth.trigger.loss_threshold_pct
        );
        let _ = writeln!(
            summary,
            "trigger.rtt_threshold_ms={}",
            self.bandwidth
                .trigger
                .rtt_threshold_ms
                .map(|value| value.to_string())
                .unwrap_or_else(|| "disabled".to_owned())
        );
        let _ = writeln!(
            summary,
            "trigger.recovery_loss_pct={}",
            self.bandwidth.trigger.recovery_loss_pct
        );
        let _ = writeln!(
            summary,
            "trigger.recovery_rounds={}",
            self.bandwidth.trigger.recovery_rounds
        );
        let _ = writeln!(
            summary,
            "trigger.pending_ttl={}",
            humantime::format_duration(self.bandwidth.trigger.pending_ttl)
        );
        let _ = writeln!(
            summary,
            "cooldown.initial={}",
            humantime::format_duration(self.bandwidth.cooldown.initial)
        );
        let _ = writeln!(
            summary,
            "cooldown.max={}",
            humantime::format_duration(self.bandwidth.cooldown.max)
        );
        match &self.bandwidth.provider {
            ProviderConfig::Mlab(mlab) => {
                let _ = writeln!(summary, "mlab.locate_url={}", redact_url(&mlab.locate_url));
                let _ = writeln!(summary, "mlab.policy_accepted={}", mlab.policy_accepted);
                let _ = writeln!(summary, "mlab.acceptable_use={MLAB_AUP_URL}");
                let _ = writeln!(summary, "mlab.privacy={MLAB_PRIVACY_URL}");
            }
            ProviderConfig::Direct(direct) => {
                let _ = writeln!(
                    summary,
                    "direct.target={}",
                    direct.target.as_deref().unwrap_or("explicit-urls")
                );
                let _ = writeln!(
                    summary,
                    "direct.download_url={}",
                    redact_url(&direct.download_url)
                );
                let _ = writeln!(
                    summary,
                    "direct.upload_url={}",
                    redact_url(&direct.upload_url)
                );
                let _ = writeln!(
                    summary,
                    "direct.tls_server_name={}",
                    direct.tls_server_name.as_deref().unwrap_or("default")
                );
                let _ = writeln!(
                    summary,
                    "direct.ca_cert={}",
                    direct
                        .ca_cert
                        .as_ref()
                        .map(|path| path.display().to_string())
                        .unwrap_or_else(|| "system-roots".to_owned())
                );
                let _ = writeln!(summary, "direct.allow_insecure={}", direct.allow_insecure);
            }
        }
        summary
    }
}

fn redact_url(url: &Url) -> String {
    let had_query = url.query().is_some();
    let mut redacted = url.clone();
    redacted.set_query(None);
    redacted.set_fragment(None);
    let mut value = redacted.to_string();
    if had_query {
        value.push_str("?[redacted]");
    }
    value
}

fn enum_name(mode: ConsoleMode) -> &'static str {
    match mode {
        ConsoleMode::Auto => "auto",
        ConsoleMode::Human => "human",
        ConsoleMode::Jsonl => "jsonl",
        ConsoleMode::Off => "off",
    }
}

fn command_name(command: CommandKind) -> &'static str {
    match command {
        CommandKind::Run => "run",
        CommandKind::OncePing => "once-ping",
        CommandKind::OnceBandwidth => "once-bandwidth",
        CommandKind::ConfigCheck => "config-check",
    }
}

fn verbosity_name(verbosity: Verbosity) -> &'static str {
    match verbosity {
        Verbosity::Error => "error",
        Verbosity::Warn => "warn",
        Verbosity::Info => "info",
        Verbosity::Debug => "debug",
        Verbosity::Trace => "trace",
    }
}
