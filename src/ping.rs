use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};
use std::num::NonZeroU32;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use chrono::{DateTime, SecondsFormat, Utc};
use thiserror::Error;
use tokio::io::AsyncWrite;

use crate::config::ResolvedConfig;
use crate::console::{Console, ConsoleDiagnostic, ConsoleStats};
use crate::journal::{Journal, JournalError, OutputCoordinator};
use crate::model::{ErrorKind, EventKind, LoadPhase, MeasurementEvent, Outcome};

const CONSOLE_CAPACITY: usize = 256;
const CONSOLE_SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(250);
const ICMP_PAYLOAD: [u8; 56] = [0; 56];

pub type ProbeFuture<'a> = Pin<Box<dyn Future<Output = ProbeAttemptResult> + Send + 'a>>;

pub trait PingTransport: Send + Sync + 'static {
    fn probe(&self, request: ProbeRequest) -> ProbeFuture<'_>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeRequest {
    pub target: IpAddr,
    pub identifier: u16,
    pub sequence: u16,
    pub timeout: Duration,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProbeBinding {
    pub interface: Option<String>,
    pub source_ip: Option<IpAddr>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeReply {
    pub target: IpAddr,
    pub identifier: Option<u16>,
    pub sequence: u16,
    pub rtt: Duration,
    pub icmp_type: u8,
    pub icmp_code: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeFailure {
    Timeout,
    Unreachable {
        icmp_type: u8,
        icmp_code: u8,
        message: String,
    },
    PermissionDenied {
        os_error_code: Option<i32>,
        message: String,
    },
    Cancelled,
    Io {
        os_error_code: Option<i32>,
        message: String,
    },
    Protocol {
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeAttemptResult {
    pub binding: ProbeBinding,
    pub sent: bool,
    pub result: Result<ProbeReply, ProbeFailure>,
}

#[derive(Debug, Clone)]
pub struct PingRoundRequest {
    pub run_id: String,
    pub round_number: u64,
    pub targets: Vec<IpAddr>,
    pub timeout: Duration,
    pub scheduled_at_utc: DateTime<Utc>,
    pub identifier: u16,
    pub load_phase: Option<LoadPhase>,
    pub load_run_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PingRoundReport {
    pub events: Vec<MeasurementEvent>,
    pub successful_targets: usize,
    pub failed_targets: usize,
}

impl PingRoundReport {
    pub fn exit_status(&self) -> PingExitStatus {
        if self.failed_targets == 0 {
            PingExitStatus::Success
        } else {
            PingExitStatus::PartialFailure
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PingExitStatus {
    Success,
    PartialFailure,
}

impl PingExitStatus {
    pub const fn code(self) -> u8 {
        match self {
            Self::Success => 0,
            Self::PartialFailure => 1,
        }
    }
}

#[derive(Debug)]
pub struct PingExecution {
    pub exit_status: PingExitStatus,
    pub output_path: std::path::PathBuf,
    pub console_stats: ConsoleStats,
}

#[derive(Debug, Error)]
pub enum PingRoundError {
    #[error("a ping round requires at least one target")]
    NoTargets,
    #[error("duplicate ping target: {0}")]
    DuplicateTarget(IpAddr),
    #[error("a ping round supports at most 65536 targets")]
    TooManyTargets,
}

#[derive(Debug, Error)]
pub enum PingCommandError {
    #[error(transparent)]
    Journal(#[from] JournalError),
    #[error(transparent)]
    Round(#[from] PingRoundError),
}

pub async fn measure_round<T>(
    transport: Arc<T>,
    request: PingRoundRequest,
) -> Result<PingRoundReport, PingRoundError>
where
    T: PingTransport + ?Sized,
{
    validate_round_request(&request)?;

    let mut tasks = Vec::with_capacity(request.targets.len());
    for (index, target) in request.targets.iter().copied().enumerate() {
        let sequence = request
            .round_number
            .wrapping_mul(request.targets.len() as u64)
            .wrapping_add(index as u64) as u16;
        let transport = Arc::clone(&transport);
        let context = MeasurementContext {
            run_id: request.run_id.clone(),
            round_number: request.round_number,
            scheduled_at: request.scheduled_at_utc,
            load_phase: request.load_phase,
            load_run_id: request.load_run_id.clone(),
        };
        let probe_request = ProbeRequest {
            target,
            identifier: request.identifier,
            sequence,
            timeout: request.timeout,
        };
        tasks.push((
            target,
            sequence,
            tokio::spawn(async move { measure_target(transport, context, probe_request).await }),
        ));
    }

    let mut events = Vec::with_capacity(request.targets.len() * 2);
    let mut successful_targets = 0;
    let mut failed_targets = 0;
    for (target, sequence, task) in tasks {
        let measurement = match task.await {
            Ok(measurement) => measurement,
            Err(error) => internal_failure(
                &request,
                target,
                sequence,
                format!("ping task failed: {error}"),
            ),
        };
        if measurement.success {
            successful_targets += 1;
        } else {
            failed_targets += 1;
        }
        events.extend(measurement.events);
    }

    Ok(PingRoundReport {
        events,
        successful_targets,
        failed_targets,
    })
}

fn validate_round_request(request: &PingRoundRequest) -> Result<(), PingRoundError> {
    if request.targets.is_empty() {
        return Err(PingRoundError::NoTargets);
    }
    if request.targets.len() > usize::from(u16::MAX) + 1 {
        return Err(PingRoundError::TooManyTargets);
    }
    let mut unique = HashSet::new();
    for target in &request.targets {
        if !unique.insert(*target) {
            return Err(PingRoundError::DuplicateTarget(*target));
        }
    }
    Ok(())
}

pub async fn execute_ping_once<T, W>(
    config: &ResolvedConfig,
    transport: Arc<T>,
    console_writer: W,
) -> Result<PingExecution, PingCommandError>
where
    T: PingTransport,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let scheduled_at = Utc::now();
    let run_number = next_run_number();
    let run_id = run_id(scheduled_at, run_number);
    let identifier = (u64::from(std::process::id()) ^ run_number) as u16;
    let round_request = PingRoundRequest {
        run_id,
        round_number: 0,
        targets: config.ping.targets.clone(),
        timeout: config.ping.timeout,
        scheduled_at_utc: scheduled_at,
        identifier,
        load_phase: None,
        load_run_id: None,
    };
    validate_round_request(&round_request)?;
    let (journal, output_path) = Journal::open_at(&config.output, scheduled_at)?;
    let console = Console::spawn(
        config.console,
        console_writer,
        CONSOLE_CAPACITY,
        trace_console_diagnostic,
    );
    let report = measure_round(transport, round_request).await?;
    let exit_status = report.exit_status();

    let mut coordinator = OutputCoordinator::new(journal, console);
    let publish_result = coordinator.publish_batch(&report.events);
    let flush_result = coordinator.flush();
    if flush_result.is_ok() {
        tracing::info!(path = %output_path.display(), "measurement journal flushed");
    }
    let (journal, console) = coordinator.into_parts();
    drop(journal);
    let console_stats = console.shutdown(CONSOLE_SHUTDOWN_TIMEOUT).await;
    publish_result?;
    flush_result?;

    Ok(PingExecution {
        exit_status,
        output_path,
        console_stats,
    })
}

fn trace_console_diagnostic(diagnostic: ConsoleDiagnostic) {
    match diagnostic {
        ConsoleDiagnostic::QueueFull { dropped_events } => tracing::warn!(
            dropped_events,
            "console queue full; ping presentation dropped"
        ),
        ConsoleDiagnostic::WriterDisabled { reason } => tracing::warn!(
            ?reason,
            "console writer disabled; ping journal remains authoritative"
        ),
    }
}

struct TargetMeasurement {
    events: [MeasurementEvent; 2],
    success: bool,
}

struct MeasurementContext {
    run_id: String,
    round_number: u64,
    scheduled_at: DateTime<Utc>,
    load_phase: Option<LoadPhase>,
    load_run_id: Option<String>,
}

async fn measure_target<T: PingTransport + ?Sized>(
    transport: Arc<T>,
    context: MeasurementContext,
    request: ProbeRequest,
) -> TargetMeasurement {
    let started_at = Utc::now();
    let started = Instant::now();
    let attempt = transport.probe(request.clone()).await;
    let duration = started.elapsed();
    let finished_at = Utc::now();
    build_measurement(
        &context,
        started_at,
        finished_at,
        duration,
        request,
        attempt,
    )
}

fn build_measurement(
    context: &MeasurementContext,
    started_at: DateTime<Utc>,
    finished_at: DateTime<Utc>,
    duration: Duration,
    request: ProbeRequest,
    attempt: ProbeAttemptResult,
) -> TargetMeasurement {
    let run_id = &context.run_id;
    let validated = attempt
        .result
        .and_then(|reply| validate_reply(&request, reply));
    let success = validated.is_ok();
    let (outcome, rtt, icmp_type, icmp_code, error_kind, os_error_code, error_message) =
        match validated {
            Ok(reply) => (
                Outcome::Success,
                Some(reply.rtt),
                Some(reply.icmp_type),
                Some(reply.icmp_code),
                None,
                None,
                None,
            ),
            Err(failure) => failure_fields(failure, request.timeout),
        };

    let target = request.target.to_string();
    let mut probe = MeasurementEvent::new(
        run_id,
        format!(
            "{run_id}:ping-round:{}:ping-probe:{}",
            context.round_number, request.sequence
        ),
        EventKind::PingProbe,
        outcome,
        finished_at,
    );
    apply_common(
        &mut probe,
        context,
        started_at,
        &attempt.binding,
        &target,
        request.sequence,
    );
    probe.duration_ms = Some(duration.as_secs_f64() * 1_000.0);
    probe.rtt_ms = rtt.map(|value| value.as_secs_f64() * 1_000.0);
    probe.icmp_type = icmp_type;
    probe.icmp_code = icmp_code;
    probe.os_error_code = os_error_code;
    probe.error_kind = error_kind;
    probe.error_message = error_message.clone();

    let mut summary = MeasurementEvent::new(
        run_id,
        format!(
            "{run_id}:ping-round:{}:ping-summary:{}",
            context.round_number, request.sequence
        ),
        EventKind::PingSummary,
        outcome,
        finished_at,
    );
    apply_common(
        &mut summary,
        context,
        started_at,
        &attempt.binding,
        &target,
        request.sequence,
    );
    summary.duration_ms = probe.duration_ms;
    summary.rtt_ms = probe.rtt_ms;
    summary.packets_sent = Some(u32::from(attempt.sent));
    summary.packets_received = Some(u32::from(success));
    summary.packet_loss_pct = Some(if success { 0.0 } else { 100.0 });
    summary.os_error_code = os_error_code;
    summary.error_kind = error_kind;
    summary.error_message = error_message;

    TargetMeasurement {
        events: [probe, summary],
        success,
    }
}

fn apply_common(
    event: &mut MeasurementEvent,
    context: &MeasurementContext,
    started_at: DateTime<Utc>,
    binding: &ProbeBinding,
    target: &str,
    sequence: u16,
) {
    event.scheduled_at_utc = Some(context.scheduled_at);
    event.started_at_utc = Some(started_at);
    event.interface.clone_from(&binding.interface);
    event.source_ip = binding.source_ip;
    event.load_phase = context.load_phase;
    event.load_run_id.clone_from(&context.load_run_id);
    event.target = Some(target.to_owned());
    event.sequence = Some(sequence);
}

fn validate_reply(request: &ProbeRequest, reply: ProbeReply) -> Result<ProbeReply, ProbeFailure> {
    if reply.target != request.target {
        return Err(ProbeFailure::Protocol {
            message: format!(
                "reply target mismatch: expected {}, received {}",
                request.target, reply.target
            ),
        });
    }
    if let Some(identifier) = reply.identifier
        && identifier != request.identifier
    {
        return Err(ProbeFailure::Protocol {
            message: format!(
                "reply identifier mismatch: expected {}, received {identifier}",
                request.identifier
            ),
        });
    }
    if reply.sequence != request.sequence {
        return Err(ProbeFailure::Protocol {
            message: format!(
                "reply sequence mismatch: expected {}, received {}",
                request.sequence, reply.sequence
            ),
        });
    }
    let echo_reply_type = match request.target {
        IpAddr::V4(_) => 0,
        IpAddr::V6(_) => 129,
    };
    if reply.icmp_type != echo_reply_type || reply.icmp_code != 0 {
        return Err(ProbeFailure::Unreachable {
            icmp_type: reply.icmp_type,
            icmp_code: reply.icmp_code,
            message: format!(
                "ICMP error reply type {} code {}",
                reply.icmp_type, reply.icmp_code
            ),
        });
    }
    Ok(reply)
}

type FailureFields = (
    Outcome,
    Option<Duration>,
    Option<u8>,
    Option<u8>,
    Option<ErrorKind>,
    Option<i32>,
    Option<String>,
);

fn failure_fields(failure: ProbeFailure, timeout: Duration) -> FailureFields {
    match failure {
        ProbeFailure::Timeout => (
            Outcome::Timeout,
            None,
            None,
            None,
            Some(ErrorKind::IcmpTimeout),
            None,
            Some(format!(
                "ping timed out after {}",
                humantime::format_duration(timeout)
            )),
        ),
        ProbeFailure::Unreachable {
            icmp_type,
            icmp_code,
            message,
        } => (
            Outcome::Unreachable,
            None,
            Some(icmp_type),
            Some(icmp_code),
            Some(ErrorKind::IcmpUnreachable),
            None,
            Some(message),
        ),
        ProbeFailure::PermissionDenied {
            os_error_code,
            message,
        } => (
            Outcome::PermissionDenied,
            None,
            None,
            None,
            Some(ErrorKind::PermissionDenied),
            os_error_code,
            Some(format!(
                "{message}; on Linux, allow this service user's group in net.ipv4.ping_group_range, or grant only cap_net_raw when datagram ICMP is unavailable"
            )),
        ),
        ProbeFailure::Cancelled => (
            Outcome::Cancelled,
            None,
            None,
            None,
            Some(ErrorKind::Cancelled),
            None,
            Some("ping was cancelled".to_owned()),
        ),
        ProbeFailure::Io {
            os_error_code,
            message,
        } => (
            Outcome::Error,
            None,
            None,
            None,
            Some(ErrorKind::Io),
            os_error_code,
            Some(message),
        ),
        ProbeFailure::Protocol { message } => (
            Outcome::Error,
            None,
            None,
            None,
            Some(ErrorKind::Protocol),
            None,
            Some(message),
        ),
    }
}

fn internal_failure(
    request: &PingRoundRequest,
    target: IpAddr,
    sequence: u16,
    message: String,
) -> TargetMeasurement {
    let now = Utc::now();
    let context = MeasurementContext {
        run_id: request.run_id.clone(),
        round_number: request.round_number,
        scheduled_at: request.scheduled_at_utc,
        load_phase: request.load_phase,
        load_run_id: request.load_run_id.clone(),
    };
    build_measurement(
        &context,
        now,
        now,
        Duration::ZERO,
        ProbeRequest {
            target,
            identifier: 0,
            sequence,
            timeout: Duration::ZERO,
        },
        ProbeAttemptResult {
            binding: ProbeBinding::default(),
            sent: false,
            result: Err(ProbeFailure::Protocol { message }),
        },
    )
}

fn next_run_number() -> u64 {
    static RUN_NUMBER: AtomicU64 = AtomicU64::new(1);
    RUN_NUMBER.fetch_add(1, Ordering::Relaxed)
}

fn run_id(started_at: DateTime<Utc>, run_number: u64) -> String {
    format!(
        "{}-{}-{run_number}",
        started_at.to_rfc3339_opts(SecondsFormat::Nanos, true),
        std::process::id()
    )
}

#[derive(Clone)]
enum TargetTransport {
    Ready {
        client: Arc<surge_ping::Client>,
        binding: ProbeBinding,
        scope_id: u32,
    },
    Failed {
        binding: ProbeBinding,
        failure: ProbeFailure,
    },
}

pub struct SurgePingTransport {
    targets: HashMap<IpAddr, TargetTransport>,
}

impl SurgePingTransport {
    pub fn new(interface: Option<&str>, targets: &[IpAddr]) -> Self {
        let interfaces = if_addrs::get_if_addrs();
        let mut clients: HashMap<IpAddr, Result<Arc<surge_ping::Client>, ProbeFailure>> =
            HashMap::new();
        let mut configured = HashMap::new();

        for target in targets {
            let source = resolve_source(*target, interface, interfaces.as_deref());
            let target_transport = match source {
                Ok(source) => {
                    let binding = ProbeBinding {
                        interface: interface.map(str::to_owned),
                        source_ip: Some(source.address),
                    };
                    let client = clients
                        .entry(source.address)
                        .or_insert_with(|| create_client(source.address, interface, source.index))
                        .clone();
                    match client {
                        Ok(client) => TargetTransport::Ready {
                            client,
                            binding,
                            scope_id: source.index.unwrap_or(0),
                        },
                        Err(failure) => TargetTransport::Failed { binding, failure },
                    }
                }
                Err(failure) => TargetTransport::Failed {
                    binding: ProbeBinding {
                        interface: interface.map(str::to_owned),
                        source_ip: None,
                    },
                    failure,
                },
            };
            configured.insert(*target, target_transport);
        }
        Self {
            targets: configured,
        }
    }
}

impl PingTransport for SurgePingTransport {
    fn probe(&self, request: ProbeRequest) -> ProbeFuture<'_> {
        let target = self.targets.get(&request.target).cloned();
        Box::pin(async move {
            let Some(target) = target else {
                return ProbeAttemptResult {
                    binding: ProbeBinding::default(),
                    sent: false,
                    result: Err(ProbeFailure::Protocol {
                        message: "target was not configured in the ICMP transport".to_owned(),
                    }),
                };
            };
            let TargetTransport::Ready {
                client,
                binding,
                scope_id,
            } = target
            else {
                let TargetTransport::Failed { binding, failure } = target else {
                    unreachable!()
                };
                return ProbeAttemptResult {
                    binding,
                    sent: false,
                    result: Err(failure),
                };
            };

            let mut pinger = client
                .pinger(
                    request.target,
                    surge_ping::PingIdentifier(request.identifier),
                )
                .await;
            pinger.timeout(request.timeout);
            if scope_id != 0
                && matches!(request.target, IpAddr::V6(address) if address.is_unicast_link_local())
            {
                pinger.scope_id(scope_id);
            }
            match pinger
                .ping(surge_ping::PingSequence(request.sequence), &ICMP_PAYLOAD)
                .await
            {
                Ok((packet, rtt)) => {
                    let identifier = pinger.ident.map(|_| packet.get_identifier().into_u16());
                    let sequence = packet.get_sequence().into_u16();
                    let (icmp_type, icmp_code) = packet_type_code(&packet);
                    ProbeAttemptResult {
                        binding,
                        sent: true,
                        result: Ok(ProbeReply {
                            target: request.target,
                            identifier,
                            sequence,
                            rtt,
                            icmp_type,
                            icmp_code,
                        }),
                    }
                }
                Err(error) => {
                    let (failure, sent) = map_surge_error(error);
                    ProbeAttemptResult {
                        binding,
                        sent,
                        result: Err(failure),
                    }
                }
            }
        })
    }
}

#[derive(Debug, Clone, Copy)]
struct SourceSelection {
    address: IpAddr,
    index: Option<u32>,
}

fn resolve_source(
    target: IpAddr,
    interface: Option<&str>,
    interfaces: Result<&[if_addrs::Interface], &io::Error>,
) -> Result<SourceSelection, ProbeFailure> {
    if let Some(interface) = interface {
        let interfaces = interfaces.map_err(map_io_reference)?;
        let named = interfaces
            .iter()
            .filter(|candidate| candidate.name == interface)
            .collect::<Vec<_>>();
        if named.is_empty() {
            return Err(ProbeFailure::Io {
                os_error_code: None,
                message: format!("network interface does not exist: {interface}"),
            });
        }
        if !named.iter().any(|candidate| candidate.is_oper_up()) {
            return Err(ProbeFailure::Io {
                os_error_code: None,
                message: format!("network interface is not up: {interface}"),
            });
        }
        let selected = named
            .iter()
            .copied()
            .filter(|candidate| same_family(candidate.ip(), target))
            .min_by_key(|candidate| source_rank(candidate.ip(), target))
            .ok_or_else(|| ProbeFailure::Io {
                os_error_code: None,
                message: format!(
                    "network interface {interface} has no address matching target {target}"
                ),
            })?;
        return Ok(SourceSelection {
            address: selected.ip(),
            index: selected.index,
        });
    }

    if matches!(target, IpAddr::V6(address) if address.is_unicast_link_local()) {
        return Err(ProbeFailure::Io {
            os_error_code: None,
            message: "an interface is required for an IPv6 link-local ping target".to_owned(),
        });
    }
    let bind = match target {
        IpAddr::V4(_) => SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
        IpAddr::V6(_) => SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0),
    };
    let socket = UdpSocket::bind(bind).map_err(map_io)?;
    socket
        .connect(SocketAddr::new(target, 33434))
        .map_err(map_io)?;
    let address = socket.local_addr().map_err(map_io)?.ip();
    Ok(SourceSelection {
        address,
        index: None,
    })
}

fn same_family(left: IpAddr, right: IpAddr) -> bool {
    matches!(
        (left, right),
        (IpAddr::V4(_), IpAddr::V4(_)) | (IpAddr::V6(_), IpAddr::V6(_))
    )
}

fn source_rank(source: IpAddr, target: IpAddr) -> u8 {
    match (source, target) {
        (IpAddr::V6(source), IpAddr::V6(target))
            if source.is_unicast_link_local() == target.is_unicast_link_local() =>
        {
            0
        }
        (IpAddr::V4(source), IpAddr::V4(target))
            if source.is_loopback() == target.is_loopback() =>
        {
            0
        }
        _ => 1,
    }
}

fn create_client(
    source: IpAddr,
    interface: Option<&str>,
    interface_index: Option<u32>,
) -> Result<Arc<surge_ping::Client>, ProbeFailure> {
    let kind = match source {
        IpAddr::V4(_) => surge_ping::ICMP::V4,
        IpAddr::V6(_) => surge_ping::ICMP::V6,
    };
    let mut builder = surge_ping::Config::builder()
        .kind(kind)
        .bind(SocketAddr::new(source, 0));
    if let Some(interface) = interface {
        builder = builder.interface(interface);
    }
    if let Some(index) = interface_index.and_then(NonZeroU32::new) {
        builder = builder.interface_index(index);
    }
    surge_ping::Client::new(&builder.build())
        .map(Arc::new)
        .map_err(map_io)
}

fn packet_type_code(packet: &surge_ping::IcmpPacket) -> (u8, u8) {
    match packet {
        surge_ping::IcmpPacket::V4(packet) => (packet.get_icmp_type().0, packet.get_icmp_code().0),
        surge_ping::IcmpPacket::V6(packet) => {
            (packet.get_icmpv6_type().0, packet.get_icmpv6_code().0)
        }
    }
}

fn map_surge_error(error: surge_ping::SurgeError) -> (ProbeFailure, bool) {
    match error {
        surge_ping::SurgeError::Timeout { .. } => (ProbeFailure::Timeout, true),
        surge_ping::SurgeError::IOError(error) => (map_io(error), false),
        surge_ping::SurgeError::NetworkError => (
            ProbeFailure::Io {
                os_error_code: None,
                message: "ICMP receive task stopped before a reply arrived".to_owned(),
            },
            true,
        ),
        surge_ping::SurgeError::ClientDestroyed => (ProbeFailure::Cancelled, false),
        other => (
            ProbeFailure::Protocol {
                message: other.to_string(),
            },
            false,
        ),
    }
}

fn map_io(error: io::Error) -> ProbeFailure {
    let os_error_code = error.raw_os_error();
    let message = error.to_string();
    if error.kind() == io::ErrorKind::PermissionDenied {
        ProbeFailure::PermissionDenied {
            os_error_code,
            message,
        }
    } else {
        ProbeFailure::Io {
            os_error_code,
            message,
        }
    }
}

fn map_io_reference(error: &io::Error) -> ProbeFailure {
    let os_error_code = error.raw_os_error();
    let message = error.to_string();
    if error.kind() == io::ErrorKind::PermissionDenied {
        ProbeFailure::PermissionDenied {
            os_error_code,
            message,
        }
    } else {
        ProbeFailure::Io {
            os_error_code,
            message,
        }
    }
}
