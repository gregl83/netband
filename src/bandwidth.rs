use std::collections::HashSet;
use std::future::{Future, poll_fn};
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll, ready};
use std::time::{Duration, Instant};

use chrono::{DateTime, SecondsFormat, Utc};
use futures_util::{Sink, SinkExt, Stream, StreamExt};
use rustls::pki_types::{CertificateDer, ServerName, pem::PemObject};
use rustls::{ClientConfig, RootCertStore};
use serde::Deserialize;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpSocket, TcpStream};
use tokio::sync::watch;
use tokio_rustls::TlsConnector;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::{HOST, SEC_WEBSOCKET_PROTOCOL, USER_AGENT};
use tokio_tungstenite::tungstenite::{Error as WebSocketError, Message};
use tokio_tungstenite::{WebSocketStream, client_async_with_config};
use url::Url;

use crate::config::ResolvedConfig;
use crate::console::{Console, ConsoleDiagnostic, ConsoleStats};
use crate::journal::{Journal, JournalError, OutputCoordinator};
use crate::model::{
    ErrorKind, EventKind, LoadPhase, MeasurementEvent, Outcome, ProviderKind, RequestStage,
    TriggerReason,
};
use crate::provider::{
    EndpointCandidate, FailureDisposition, RequestFailure, USER_AGENT as NETBAND_USER_AGENT,
    parse_retry_after_value, resolve_endpoints, retry_until,
};
use crate::scheduler::{BandwidthOpportunity, ManualDecision, Scheduler, SchedulerError};

const NDT7_SUBPROTOCOL: &str = "net.measurementlab.ndt.v7";
const MAX_RESOLVED_ADDRESSES: usize = 8;
const MAX_MESSAGE_SIZE: usize = 1 << 24;
const INITIAL_UPLOAD_MESSAGE_SIZE: usize = 1 << 13;
const MAX_UPLOAD_MESSAGE_SIZE: usize = 1 << 20;
const UPLOAD_SCALING_FRACTION: u64 = 16;
const UPLOAD_DURATION: Duration = Duration::from_secs(10);
const UPLOAD_CLOSE_TIMEOUT: Duration = Duration::from_secs(2);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const CONSOLE_CAPACITY: usize = 256;
const CONSOLE_SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(250);

trait IoStream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T> IoStream for T where T: AsyncRead + AsyncWrite + Unpin + Send {}
type BoxStream = Box<dyn IoStream>;
type NdtSocket = WebSocketStream<BoxStream>;
pub type ConnectFuture<'a> = Pin<Box<dyn Future<Output = io::Result<TcpStream>> + Send + 'a>>;
pub type ResolveFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Vec<SocketAddr>, String>> + Send + 'a>>;

pub trait TcpConnector: Send + Sync {
    fn connect<'a>(&'a self, remote: SocketAddr, interface: Option<&'a str>) -> ConnectFuture<'a>;
}

pub trait AddressResolver: Send + Sync {
    fn resolve<'a>(&'a self, host: &'a str, port: u16) -> ResolveFuture<'a>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SystemTcpConnector;
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemAddressResolver;

impl TcpConnector for SystemTcpConnector {
    fn connect<'a>(&'a self, remote: SocketAddr, interface: Option<&'a str>) -> ConnectFuture<'a> {
        Box::pin(connect_tcp(remote, interface))
    }
}

impl AddressResolver for SystemAddressResolver {
    fn resolve<'a>(&'a self, host: &'a str, port: u16) -> ResolveFuture<'a> {
        Box::pin(resolve_addresses(host, port))
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TcpMetrics {
    pub min_rtt_ms: Option<f64>,
    pub rtt_ms: Option<f64>,
    pub retransmissions: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct DirectionMeasurement {
    pub bytes: u64,
    pub elapsed: Duration,
    pub remote_ip: IpAddr,
    pub source_ip: IpAddr,
    pub metrics: TcpMetrics,
}

#[derive(Debug)]
struct DirectionResult {
    measurement: Option<DirectionMeasurement>,
    failures: Vec<RequestFailure>,
}

#[derive(Debug)]
struct CandidateResult {
    download: Option<DirectionMeasurement>,
    upload: Option<DirectionMeasurement>,
    failures: Vec<RequestFailure>,
    server: String,
    provider_id: String,
    provider_kind: ProviderKind,
    terminal_outcome: Option<Outcome>,
}

#[derive(Debug)]
pub struct BandwidthReport {
    pub events: Vec<MeasurementEvent>,
    pub outcome: Outcome,
    pub reserved: bool,
    pub reservation_error: Option<String>,
}

impl BandwidthReport {
    pub const fn exit_code(&self) -> u8 {
        if matches!(self.outcome, Outcome::Success) {
            0
        } else {
            1
        }
    }
}

#[derive(Debug)]
pub struct BandwidthExecution {
    pub output_path: std::path::PathBuf,
    pub report: BandwidthReport,
    pub console_stats: ConsoleStats,
}

#[derive(Debug, Error)]
pub enum BandwidthCommandError {
    #[error(transparent)]
    Journal(#[from] JournalError),
    #[error(transparent)]
    Scheduler(#[from] SchedulerError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionReservation {
    Untracked,
    Reserved { daily_runs_used: u32 },
}

pub trait ReservationGate {
    fn reserve(&mut self, started_at: DateTime<Utc>) -> Result<AdmissionReservation, String>;
}

#[derive(Debug, Default)]
struct UntrackedReservation;

impl ReservationGate for UntrackedReservation {
    fn reserve(&mut self, _started_at: DateTime<Utc>) -> Result<AdmissionReservation, String> {
        Ok(AdmissionReservation::Untracked)
    }
}

pub fn cancellation_channel() -> (watch::Sender<bool>, watch::Receiver<bool>) {
    watch::channel(false)
}

pub async fn execute_bandwidth_once<W>(
    config: &ResolvedConfig,
    console_writer: W,
    shutdown: watch::Receiver<bool>,
) -> Result<BandwidthExecution, BandwidthCommandError>
where
    W: AsyncWrite + Unpin + Send + 'static,
{
    let started_at = Utc::now();
    let run_number = next_run_number();
    let run_id = format!(
        "{}-{}-{run_number}",
        started_at.to_rfc3339_opts(SecondsFormat::Nanos, true),
        std::process::id()
    );
    let (journal, output_path) = Journal::open_at(&config.output, started_at)?;
    let console = Console::spawn(
        config.console,
        console_writer,
        CONSOLE_CAPACITY,
        trace_console_diagnostic,
    );
    let mut scheduler = Scheduler::open(&config.state_file, &config.bandwidth, started_at)?;
    let mut report = match scheduler.preflight_manual(&run_id, started_at)? {
        ManualDecision::Allowed => {
            let opportunity = BandwidthOpportunity {
                reason: TriggerReason::Manual,
                scheduled_at_utc: started_at,
                interface: config.interfaces.first().cloned(),
            };
            let mut report =
                measure_bandwidth_with_gate(config, &run_id, shutdown, &mut scheduler).await;
            if report.reservation_error.is_none() {
                let scheduler_events =
                    scheduler.finish_attempt(&run_id, Utc::now(), opportunity, &mut report)?;
                report.events.extend(scheduler_events);
            }
            report
        }
        ManualDecision::Blocked(event) => BandwidthReport {
            events: vec![*event],
            outcome: Outcome::Suppressed,
            reserved: false,
            reservation_error: None,
        },
    };
    scheduler.flush()?;
    let reservation_error = report.reservation_error.take();
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
    if let Some(message) = reservation_error {
        return Err(BandwidthCommandError::Scheduler(SchedulerError::Admission(
            message,
        )));
    }
    Ok(BandwidthExecution {
        output_path,
        report,
        console_stats,
    })
}

pub async fn measure_bandwidth(
    config: &ResolvedConfig,
    run_id: &str,
    shutdown: watch::Receiver<bool>,
) -> BandwidthReport {
    let mut reservation = UntrackedReservation;
    measure_bandwidth_with_network_and_gate(
        config,
        run_id,
        shutdown,
        &SystemTcpConnector,
        &SystemAddressResolver,
        &mut reservation,
    )
    .await
}

pub async fn measure_bandwidth_with_gate<G: ReservationGate>(
    config: &ResolvedConfig,
    run_id: &str,
    shutdown: watch::Receiver<bool>,
    reservation: &mut G,
) -> BandwidthReport {
    measure_bandwidth_with_network_and_gate(
        config,
        run_id,
        shutdown,
        &SystemTcpConnector,
        &SystemAddressResolver,
        reservation,
    )
    .await
}

pub async fn measure_bandwidth_with_gate_and_phase<G: ReservationGate>(
    config: &ResolvedConfig,
    run_id: &str,
    shutdown: watch::Receiver<bool>,
    reservation: &mut G,
    phase: watch::Sender<LoadPhase>,
) -> BandwidthReport {
    measure_bandwidth_with_network_and_gate_observed(
        config,
        run_id,
        shutdown,
        &SystemTcpConnector,
        &SystemAddressResolver,
        reservation,
        Some(&phase),
    )
    .await
}

pub async fn measure_bandwidth_with_connector<C: TcpConnector>(
    config: &ResolvedConfig,
    run_id: &str,
    shutdown: watch::Receiver<bool>,
    connector: &C,
) -> BandwidthReport {
    let mut reservation = UntrackedReservation;
    measure_bandwidth_with_network_and_gate(
        config,
        run_id,
        shutdown,
        connector,
        &SystemAddressResolver,
        &mut reservation,
    )
    .await
}

pub async fn measure_bandwidth_with_network<C: TcpConnector, R: AddressResolver>(
    config: &ResolvedConfig,
    run_id: &str,
    shutdown: watch::Receiver<bool>,
    connector: &C,
    resolver: &R,
) -> BandwidthReport {
    let mut reservation = UntrackedReservation;
    measure_bandwidth_with_network_and_gate(
        config,
        run_id,
        shutdown,
        connector,
        resolver,
        &mut reservation,
    )
    .await
}

pub async fn measure_bandwidth_with_network_and_gate<
    C: TcpConnector,
    R: AddressResolver,
    G: ReservationGate,
>(
    config: &ResolvedConfig,
    run_id: &str,
    shutdown: watch::Receiver<bool>,
    connector: &C,
    resolver: &R,
    reservation: &mut G,
) -> BandwidthReport {
    measure_bandwidth_with_network_and_gate_observed(
        config,
        run_id,
        shutdown,
        connector,
        resolver,
        reservation,
        None,
    )
    .await
}

async fn measure_bandwidth_with_network_and_gate_observed<
    C: TcpConnector,
    R: AddressResolver,
    G: ReservationGate,
>(
    config: &ResolvedConfig,
    run_id: &str,
    mut shutdown: watch::Receiver<bool>,
    connector: &C,
    resolver: &R,
    reservation: &mut G,
    phase: Option<&watch::Sender<LoadPhase>>,
) -> BandwidthReport {
    report_phase(phase, LoadPhase::Setup);
    let interface = config.interfaces.first().map(String::as_str);
    let whole_timeout = tokio::time::sleep(config.bandwidth.whole_test_timeout);
    tokio::pin!(whole_timeout);
    let resolution = tokio::select! {
        _ = &mut whole_timeout => {
            return command_failure_report(
                config,
                run_id,
                interface,
                Outcome::Timeout,
                ErrorKind::Timeout,
                format!(
                    "NDT7 test timed out after {}",
                    humantime::format_duration(config.bandwidth.whole_test_timeout)
                ),
            );
        }
        _ = cancellation_requested(&mut shutdown) => {
            return command_failure_report(
                config,
                run_id,
                interface,
                Outcome::Cancelled,
                ErrorKind::Cancelled,
                "NDT7 test was cancelled".to_owned(),
            );
        }
        resolution = resolve_endpoints(&config.bandwidth, interface) => resolution,
    };
    let mut failures = resolution.failures;
    if let Some(terminal) = resolution.terminal {
        let outcome = terminal.outcome;
        failures.push(terminal);
        return report_from_result(ReportInput {
            run_id,
            interface,
            provider_id: &config.bandwidth.provider_id,
            provider_kind: provider_kind(config),
            server: None,
            failures,
            download: None,
            upload: None,
            outcome,
        });
    }

    let reservation = match reservation.reserve(Utc::now()) {
        Ok(reservation) => reservation,
        Err(message) => {
            let mut report = command_failure_report(
                config,
                run_id,
                interface,
                Outcome::Error,
                ErrorKind::Internal,
                format!("cannot persist bandwidth reservation: {message}"),
            );
            report.reservation_error = Some(message);
            return report;
        }
    };

    let test = run_candidates(
        &resolution.candidates,
        interface,
        connector,
        resolver,
        phase,
    );
    let candidate = tokio::select! {
        _ = &mut whole_timeout => {
            return apply_admission(command_failure_report(
                config,
                run_id,
                interface,
                Outcome::Timeout,
                ErrorKind::Timeout,
                format!(
                    "NDT7 test timed out after {}",
                    humantime::format_duration(config.bandwidth.whole_test_timeout)
                ),
            ), reservation);
        }
        _ = cancellation_requested(&mut shutdown) => {
            return apply_admission(command_failure_report(
                config,
                run_id,
                interface,
                Outcome::Cancelled,
                ErrorKind::Cancelled,
                "NDT7 test was cancelled".to_owned(),
            ), reservation);
        },
        result = test => result,
    };
    failures.extend(candidate.failures);
    let outcome = candidate.terminal_outcome.unwrap_or_else(|| {
        match (candidate.download.is_some(), candidate.upload.is_some()) {
            (true, true) => Outcome::Success,
            (true, false) | (false, true) => Outcome::Partial,
            (false, false) => Outcome::Error,
        }
    });
    let report = report_from_result(ReportInput {
        run_id,
        interface,
        provider_id: &candidate.provider_id,
        provider_kind: candidate.provider_kind,
        server: Some(candidate.server),
        failures,
        download: candidate.download,
        upload: candidate.upload,
        outcome,
    });
    apply_admission(report, reservation)
}

fn apply_admission(
    mut report: BandwidthReport,
    reservation: AdmissionReservation,
) -> BandwidthReport {
    if let AdmissionReservation::Reserved { daily_runs_used } = reservation {
        report.reserved = true;
        for event in &mut report.events {
            event.daily_runs_used = Some(daily_runs_used);
        }
    }
    report
}

fn command_failure_report(
    config: &ResolvedConfig,
    run_id: &str,
    interface: Option<&str>,
    outcome: Outcome,
    error_kind: ErrorKind,
    message: String,
) -> BandwidthReport {
    let mut failure = RequestFailure::simple(RequestStage::Connect, error_kind, message, None, 1);
    failure.outcome = outcome;
    report_from_result(ReportInput {
        run_id,
        interface,
        provider_id: &config.bandwidth.provider_id,
        provider_kind: provider_kind(config),
        server: None,
        failures: vec![failure],
        download: None,
        upload: None,
        outcome,
    })
}

async fn run_candidates<C: TcpConnector, R: AddressResolver>(
    candidates: &[EndpointCandidate],
    interface: Option<&str>,
    connector: &C,
    resolver: &R,
    phase: Option<&watch::Sender<LoadPhase>>,
) -> CandidateResult {
    let mut all_failures = Vec::new();
    for candidate in candidates {
        report_phase(phase, LoadPhase::Setup);
        let download = run_download(candidate, interface, connector, resolver, phase).await;
        let download_retry = download.measurement.is_none()
            && download
                .failures
                .last()
                .is_some_and(|failure| failure.disposition == FailureDisposition::TryNextTarget);
        let provider_stop = download.failures.last().and_then(provider_stop_outcome);
        all_failures.extend(download.failures);
        if let Some(outcome) = provider_stop {
            return failed_candidate(candidate, all_failures, outcome);
        }
        if download_retry {
            continue;
        }

        report_phase(phase, LoadPhase::Setup);
        let upload = run_upload(candidate, interface, connector, resolver, phase).await;
        let provider_stop = upload.failures.last().and_then(provider_stop_outcome);
        all_failures.extend(upload.failures);
        let outcome = provider_stop;
        return CandidateResult {
            download: download.measurement,
            upload: upload.measurement,
            failures: all_failures,
            server: candidate.logical_server.clone(),
            provider_id: candidate.provider_id.clone(),
            provider_kind: candidate.provider_kind,
            terminal_outcome: outcome,
        };
    }
    let candidate = candidates
        .first()
        .expect("endpoint resolution returned candidates");
    failed_candidate(candidate, all_failures, Outcome::NoCapacity)
}

fn failed_candidate(
    candidate: &EndpointCandidate,
    failures: Vec<RequestFailure>,
    outcome: Outcome,
) -> CandidateResult {
    CandidateResult {
        download: None,
        upload: None,
        failures,
        server: candidate.logical_server.clone(),
        provider_id: candidate.provider_id.clone(),
        provider_kind: candidate.provider_kind,
        terminal_outcome: Some(outcome),
    }
}

fn provider_stop_outcome(failure: &RequestFailure) -> Option<Outcome> {
    (failure.disposition == FailureDisposition::ProviderWide).then_some(failure.outcome)
}

async fn run_download<C: TcpConnector, R: AddressResolver>(
    candidate: &EndpointCandidate,
    interface: Option<&str>,
    connector: &C,
    resolver: &R,
    phase: Option<&watch::Sender<LoadPhase>>,
) -> DirectionResult {
    let connected = match connect_websocket(
        &candidate.download_url,
        candidate,
        interface,
        connector,
        resolver,
        1,
    )
    .await
    {
        Ok(connected) => connected,
        Err(failures) => {
            return DirectionResult {
                measurement: None,
                failures,
            };
        }
    };
    let ConnectedSocket {
        mut socket,
        remote_ip,
        source_ip,
        mut failures,
    } = connected;
    report_phase(phase, LoadPhase::Download);
    let started = Instant::now();
    let mut bytes = 0_u64;
    let mut metrics = TcpMetrics::default();
    let mut completed = false;
    while let Some(message) = socket.next().await {
        match message {
            Ok(Message::Binary(payload)) => bytes = bytes.saturating_add(payload.len() as u64),
            Ok(Message::Text(text)) => update_metrics(&mut metrics, text.as_ref()),
            Ok(Message::Ping(payload)) => {
                if let Err(error) = socket.send(Message::Pong(payload)).await {
                    failures.push(stream_failure(
                        candidate,
                        RequestStage::Download,
                        remote_ip,
                        source_ip,
                        error.to_string(),
                        websocket_os_error(&error),
                    ));
                    break;
                }
            }
            Ok(Message::Close(_)) => {
                completed = true;
                break;
            }
            Ok(_) => {}
            Err(error) => {
                failures.push(stream_failure(
                    candidate,
                    RequestStage::Download,
                    remote_ip,
                    source_ip,
                    error.to_string(),
                    websocket_os_error(&error),
                ));
                break;
            }
        }
    }
    let elapsed = started.elapsed();
    report_phase(phase, LoadPhase::Setup);
    if bytes == 0 {
        if failures.is_empty() {
            failures.push(stream_failure(
                candidate,
                RequestStage::Download,
                remote_ip,
                source_ip,
                "download ended without measurement bytes".to_owned(),
                None,
            ));
        }
        return DirectionResult {
            measurement: None,
            failures,
        };
    }
    if !completed && failures.is_empty() {
        failures.push(stream_failure(
            candidate,
            RequestStage::Download,
            remote_ip,
            source_ip,
            "download connection ended without a close frame".to_owned(),
            None,
        ));
    }
    DirectionResult {
        measurement: Some(DirectionMeasurement {
            bytes,
            elapsed,
            remote_ip,
            source_ip,
            metrics,
        }),
        failures,
    }
}

async fn run_upload<C: TcpConnector, R: AddressResolver>(
    candidate: &EndpointCandidate,
    interface: Option<&str>,
    connector: &C,
    resolver: &R,
    phase: Option<&watch::Sender<LoadPhase>>,
) -> DirectionResult {
    let connected = match connect_websocket(
        &candidate.upload_url,
        candidate,
        interface,
        connector,
        resolver,
        1,
    )
    .await
    {
        Ok(connected) => connected,
        Err(failures) => {
            return DirectionResult {
                measurement: None,
                failures,
            };
        }
    };
    let ConnectedSocket {
        socket,
        remote_ip,
        source_ip,
        mut failures,
    } = connected;
    report_phase(phase, LoadPhase::Upload);
    let UploadTransfer {
        bytes,
        elapsed,
        metrics,
        error,
    } = transfer_upload(socket).await;
    report_phase(phase, LoadPhase::Setup);
    if let Some(error) = error {
        let os_error = match &error {
            UploadError::Transport(error) => websocket_os_error(error),
            _ => None,
        };
        failures.push(stream_failure(
            candidate,
            RequestStage::Upload,
            remote_ip,
            source_ip,
            error.to_string(),
            os_error,
        ));
    }
    if bytes == 0 {
        if failures.is_empty() {
            failures.push(stream_failure(
                candidate,
                RequestStage::Upload,
                remote_ip,
                source_ip,
                "upload ended without measurement bytes".to_owned(),
                None,
            ));
        }
        return DirectionResult {
            measurement: None,
            failures,
        };
    }
    DirectionResult {
        measurement: Some(DirectionMeasurement {
            bytes,
            elapsed,
            remote_ip,
            source_ip,
            metrics,
        }),
        failures,
    }
}

fn report_phase(phase: Option<&watch::Sender<LoadPhase>>, value: LoadPhase) {
    if let Some(phase) = phase {
        phase.send_replace(value);
    }
}

#[derive(Debug, Error)]
enum UploadError {
    #[error(transparent)]
    Transport(#[from] WebSocketError),
    #[error("upload connection ended without a close frame")]
    MissingClose,
    #[error("upload close handshake timed out after 2s")]
    CloseTimeout,
}

#[derive(Debug)]
struct UploadTransfer {
    bytes: u64,
    elapsed: Duration,
    metrics: TcpMetrics,
    error: Option<UploadError>,
}

enum UploadProgress {
    Incoming(Option<Message>),
    Accepted,
    Flushed,
}

// Poll both directions without awaiting a write inside the receive path. The
// caller retains flush state across incoming messages, so only one bulk frame
// can be pending and an accepted frame is never replayed or counted twice.
fn poll_upload_io<S>(
    socket: &mut WebSocketStream<S>,
    payload: Option<&Message>,
    flushing: bool,
    context: &mut Context<'_>,
) -> Poll<Result<UploadProgress, WebSocketError>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    if let Poll::Ready(incoming) = Pin::new(&mut *socket).poll_next(context) {
        return Poll::Ready(incoming.transpose().map(UploadProgress::Incoming));
    }
    if flushing {
        ready!(Pin::new(socket).poll_flush(context))?;
        return Poll::Ready(Ok(UploadProgress::Flushed));
    }
    if let Some(payload) = payload {
        ready!(Pin::new(&mut *socket).poll_ready(context))?;
        Pin::new(socket).start_send(payload.clone())?;
        return Poll::Ready(Ok(UploadProgress::Accepted));
    }
    Poll::Pending
}

async fn transfer_upload<S>(mut socket: WebSocketStream<S>) -> UploadTransfer
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let started = tokio::time::Instant::now();
    let mut deadline = started + UPLOAD_DURATION;
    // None while Uploading; frozen on transition to Closing.
    let mut elapsed = None;
    let mut bytes = 0_u64;
    let mut metrics = TcpMetrics::default();
    let mut payload = Message::Binary(upload_payload().into());
    let close = Message::Close(None);
    let mut flushing = false;
    let mut close_queued = false;
    let mut peer_closed = false;

    let error = loop {
        let outgoing = if elapsed.is_none() {
            Some(&payload)
        } else if !close_queued {
            Some(&close)
        } else {
            None
        };
        let progress = tokio::select! {
            biased;
            _ = tokio::time::sleep_until(deadline) => {
                if elapsed.is_some() {
                    break Some(UploadError::CloseTimeout);
                }
                // The rate describes local payload acceptance during the active
                // window, not delivery of the buffered tail or handshake waiting.
                elapsed = Some(started.elapsed());
                deadline = tokio::time::Instant::now() + UPLOAD_CLOSE_TIMEOUT;
                continue;
            }
            progress = poll_fn(|context| poll_upload_io(&mut socket, outgoing, flushing, context)) => progress,
        };
        match progress {
            Ok(UploadProgress::Incoming(Some(Message::Text(text)))) => {
                update_metrics(&mut metrics, text.as_ref());
            }
            Ok(UploadProgress::Incoming(Some(Message::Close(_)))) => {
                if elapsed.is_none() {
                    elapsed = Some(started.elapsed());
                    deadline = tokio::time::Instant::now() + UPLOAD_CLOSE_TIMEOUT;
                }
                peer_closed = true;
                // Tungstenite queues the reply automatically, behind any partial
                // bulk frame. Keep driving it until the peer closes the transport.
                close_queued = true;
                flushing = true;
            }
            Ok(UploadProgress::Incoming(Some(_))) => {}
            Ok(UploadProgress::Incoming(None)) => {
                break (!peer_closed).then_some(UploadError::MissingClose);
            }
            Ok(UploadProgress::Accepted) => {
                flushing = true;
                if elapsed.is_none() {
                    bytes = bytes.saturating_add(payload.len() as u64);
                } else {
                    close_queued = true;
                }
            }
            Ok(UploadProgress::Flushed) => {
                flushing = false;
                if elapsed.is_none() {
                    let next_size = next_upload_message_size(payload.len(), bytes);
                    if next_size != payload.len() {
                        payload = Message::Binary(upload_payload_with_size(next_size).into());
                    }
                }
            }
            Err(error) => break Some(UploadError::Transport(error)),
        }
    };
    UploadTransfer {
        bytes,
        elapsed: elapsed.unwrap_or_else(|| started.elapsed()),
        metrics,
        error,
    }
}

struct ConnectedSocket {
    socket: NdtSocket,
    remote_ip: IpAddr,
    source_ip: IpAddr,
    failures: Vec<RequestFailure>,
}

async fn connect_websocket<C: TcpConnector, R: AddressResolver>(
    url: &Url,
    candidate: &EndpointCandidate,
    interface: Option<&str>,
    connector: &C,
    resolver: &R,
    attempt: u32,
) -> Result<ConnectedSocket, Vec<RequestFailure>> {
    let host = match url.host_str() {
        Some(host) => host,
        None => {
            return Err(vec![RequestFailure::simple(
                RequestStage::Dns,
                ErrorKind::Dns,
                "NDT7 URL has no host",
                Some(url.to_string()),
                attempt,
            )]);
        }
    };
    let port = url.port_or_known_default().unwrap_or(443);
    let addresses = match resolver.resolve(host, port).await {
        Ok(addresses) => addresses,
        Err(message) => {
            return Err(vec![RequestFailure::simple(
                RequestStage::Dns,
                ErrorKind::Dns,
                message,
                Some(url.to_string()),
                attempt,
            )]);
        }
    };
    let mut failures = Vec::new();
    for (address_index, remote) in addresses.into_iter().enumerate() {
        let request_attempt = attempt + address_index as u32;
        let tcp = match connector.connect(remote, interface).await {
            Ok(tcp) => tcp,
            Err(error) => {
                let mut failure = RequestFailure::simple(
                    RequestStage::Connect,
                    ErrorKind::Connect,
                    format!("TCP connect failed: {error}"),
                    Some(url.to_string()),
                    request_attempt,
                );
                failure.remote_ip = Some(remote.ip());
                failure.os_error_code = error.raw_os_error();
                failures.push(failure);
                continue;
            }
        };
        let source_ip = tcp
            .local_addr()
            .map(|address| address.ip())
            .unwrap_or_else(|_| {
                if remote.is_ipv4() {
                    IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED)
                } else {
                    IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED)
                }
            });
        let stream = match wrap_stream(tcp, url, candidate).await {
            Ok(stream) => stream,
            Err(message) => {
                let mut failure = RequestFailure::simple(
                    RequestStage::Tls,
                    ErrorKind::Tls,
                    message,
                    Some(url.to_string()),
                    request_attempt,
                );
                failure.source_ip = Some(source_ip);
                failure.remote_ip = Some(remote.ip());
                failures.push(failure);
                continue;
            }
        };
        let request = match websocket_request(url, candidate) {
            Ok(request) => request,
            Err(message) => {
                failures.push(RequestFailure::simple(
                    RequestStage::WebsocketHandshake,
                    ErrorKind::WebsocketHandshake,
                    message,
                    Some(url.to_string()),
                    request_attempt,
                ));
                return Err(failures);
            }
        };
        let websocket_config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
            .max_message_size(Some(MAX_MESSAGE_SIZE))
            .max_frame_size(Some(MAX_MESSAGE_SIZE));
        match client_async_with_config(request, stream, Some(websocket_config)).await {
            Ok((socket, response)) => {
                let selected = response
                    .headers()
                    .get(SEC_WEBSOCKET_PROTOCOL)
                    .and_then(|value| value.to_str().ok());
                if selected != Some(NDT7_SUBPROTOCOL) {
                    failures.push(handshake_failure(
                        candidate,
                        url,
                        remote.ip(),
                        source_ip,
                        request_attempt,
                        None,
                        "server did not select the NDT7 WebSocket subprotocol".to_owned(),
                    ));
                    return Err(failures);
                }
                return Ok(ConnectedSocket {
                    socket,
                    remote_ip: remote.ip(),
                    source_ip,
                    failures,
                });
            }
            Err(WebSocketError::Http(response)) => {
                let status = response.status().as_u16();
                let retry_header = response.headers().get("retry-after");
                let retry_after = retry_header
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| parse_retry_after_value(value, std::time::SystemTime::now()));
                let retry_detail = match (retry_header.is_some(), retry_after.is_some()) {
                    (true, true) => "Retry-After parsed",
                    (true, false) => "Retry-After malformed",
                    (false, _) => "Retry-After missing",
                };
                failures.push(handshake_failure(
                    candidate,
                    url,
                    remote.ip(),
                    source_ip,
                    request_attempt,
                    Some((status, retry_after)),
                    format!("WebSocket handshake returned HTTP {status}; {retry_detail}"),
                ));
                return Err(failures);
            }
            Err(error) => {
                failures.push(handshake_failure(
                    candidate,
                    url,
                    remote.ip(),
                    source_ip,
                    request_attempt,
                    None,
                    format!("WebSocket handshake failed: {error}"),
                ));
            }
        }
    }
    Err(failures)
}

async fn resolve_addresses(host: &str, port: u16) -> Result<Vec<SocketAddr>, String> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(vec![SocketAddr::new(ip, port)]);
    }
    let resolved = tokio::net::lookup_host((host, port))
        .await
        .map_err(|error| format!("DNS resolution failed: {error}"))?;
    let mut unique = HashSet::new();
    let addresses = resolved
        .filter(|address| unique.insert(*address))
        .take(MAX_RESOLVED_ADDRESSES)
        .collect::<Vec<_>>();
    if addresses.is_empty() {
        Err("DNS resolution returned no addresses".to_owned())
    } else {
        Ok(addresses)
    }
}

async fn connect_tcp(remote: SocketAddr, interface: Option<&str>) -> io::Result<TcpStream> {
    let socket = if remote.is_ipv4() {
        TcpSocket::new_v4()?
    } else {
        TcpSocket::new_v6()?
    };
    if let Some(interface) = interface {
        bind_socket(&socket, interface, remote.is_ipv4())?;
    }
    tokio::time::timeout(CONNECT_TIMEOUT, socket.connect(remote))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "TCP connect timed out"))?
}

#[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
fn bind_socket(socket: &TcpSocket, interface: &str, ipv4: bool) -> io::Result<()> {
    socket.bind_device(Some(interface.as_bytes()))?;
    let source = if_addrs::get_if_addrs()?
        .into_iter()
        .find(|address| address.name == interface && address.ip().is_ipv4() == ipv4)
        .map(|address| address.ip())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::AddrNotAvailable,
                format!("interface {interface} has no matching source address"),
            )
        })?;
    socket.bind(SocketAddr::new(source, 0))
}

#[cfg(not(any(target_os = "android", target_os = "fuchsia", target_os = "linux")))]
fn bind_socket(_socket: &TcpSocket, interface: &str, _ipv4: bool) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        format!("binding TCP to interface {interface} is unsupported"),
    ))
}

async fn wrap_stream(
    tcp: TcpStream,
    url: &Url,
    candidate: &EndpointCandidate,
) -> Result<BoxStream, String> {
    match url.scheme() {
        "ws" if candidate.allow_insecure => Ok(Box::new(tcp)),
        "wss" => {
            let roots = tls_roots(candidate.ca_cert.as_deref())?;
            let config = ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth();
            let name = candidate
                .tls_server_name
                .as_deref()
                .or_else(|| url.host_str())
                .ok_or_else(|| "TLS endpoint has no server name".to_owned())?
                .to_owned();
            let server_name = ServerName::try_from(name)
                .map_err(|error| format!("invalid TLS server name: {error}"))?;
            TlsConnector::from(Arc::new(config))
                .connect(server_name, tcp)
                .await
                .map(|stream| Box::new(stream) as BoxStream)
                .map_err(|error| format!("TLS handshake failed: {error}"))
        }
        scheme => Err(format!("unsupported WebSocket scheme: {scheme}")),
    }
}

fn tls_roots(ca_cert: Option<&std::path::Path>) -> Result<RootCertStore, String> {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    if let Some(path) = ca_cert {
        let contents = std::fs::read(path)
            .map_err(|error| format!("cannot read private CA {}: {error}", path.display()))?;
        let mut found = false;
        for certificate in CertificateDer::pem_slice_iter(&contents) {
            let certificate = certificate
                .map_err(|error| format!("invalid private CA {}: {error}", path.display()))?;
            roots
                .add(certificate)
                .map_err(|error| format!("invalid private CA {}: {error}", path.display()))?;
            found = true;
        }
        if !found {
            return Err(format!(
                "private CA {} contains no certificates",
                path.display()
            ));
        }
    }
    Ok(roots)
}

fn websocket_request(
    url: &Url,
    candidate: &EndpointCandidate,
) -> Result<tokio_tungstenite::tungstenite::http::Request<()>, String> {
    let mut request = url
        .as_str()
        .into_client_request()
        .map_err(|error| format!("invalid WebSocket request: {error}"))?;
    request.headers_mut().insert(
        SEC_WEBSOCKET_PROTOCOL,
        NDT7_SUBPROTOCOL.parse().expect("static protocol is valid"),
    );
    request.headers_mut().insert(
        USER_AGENT,
        NETBAND_USER_AGENT
            .parse()
            .expect("static user agent is valid"),
    );
    if let Some(server_name) = candidate.tls_server_name.as_deref() {
        let host = match url.port() {
            Some(port) if port != 443 => format!("{server_name}:{port}"),
            _ => server_name.to_owned(),
        };
        request
            .headers_mut()
            .insert(HOST, host.parse().map_err(|_| "invalid TLS Host header")?);
    }
    Ok(request)
}

fn handshake_failure(
    candidate: &EndpointCandidate,
    url: &Url,
    remote_ip: IpAddr,
    source_ip: IpAddr,
    attempt: u32,
    response: Option<(u16, Option<Duration>)>,
    message: String,
) -> RequestFailure {
    let (http_status, retry_after) = response.unzip();
    let (outcome, disposition) = classify_handshake_status(candidate.provider_kind, http_status);
    RequestFailure {
        stage: RequestStage::WebsocketHandshake,
        outcome,
        error_kind: ErrorKind::WebsocketHandshake,
        message,
        server: Some(url.to_string()),
        source_ip: Some(source_ip),
        remote_ip: Some(remote_ip),
        os_error_code: None,
        attempt,
        http_status,
        retry_after: retry_after.flatten(),
        disposition,
    }
}

pub fn classify_handshake_status(
    provider_kind: ProviderKind,
    http_status: Option<u16>,
) -> (Outcome, FailureDisposition) {
    match http_status {
        Some(429) => (Outcome::RateLimited, FailureDisposition::ProviderWide),
        Some(503) if provider_kind == ProviderKind::Mlab => {
            (Outcome::NoCapacity, FailureDisposition::TryNextTarget)
        }
        Some(503) => (Outcome::NoCapacity, FailureDisposition::Terminal),
        _ => (Outcome::Error, FailureDisposition::Terminal),
    }
}

fn stream_failure(
    candidate: &EndpointCandidate,
    stage: RequestStage,
    remote_ip: IpAddr,
    source_ip: IpAddr,
    message: String,
    os_error_code: Option<i32>,
) -> RequestFailure {
    RequestFailure {
        stage,
        outcome: Outcome::Error,
        error_kind: match stage {
            RequestStage::Download => ErrorKind::DownloadFailed,
            RequestStage::Upload => ErrorKind::UploadFailed,
            _ => ErrorKind::Io,
        },
        message,
        server: Some(candidate.logical_server.clone()),
        source_ip: Some(source_ip),
        remote_ip: Some(remote_ip),
        os_error_code,
        attempt: 1,
        http_status: None,
        retry_after: None,
        disposition: FailureDisposition::Terminal,
    }
}

fn websocket_os_error(error: &WebSocketError) -> Option<i32> {
    match error {
        WebSocketError::Io(error) => error.raw_os_error(),
        _ => None,
    }
}

struct ReportInput<'a> {
    run_id: &'a str,
    interface: Option<&'a str>,
    provider_id: &'a str,
    provider_kind: ProviderKind,
    server: Option<String>,
    failures: Vec<RequestFailure>,
    download: Option<DirectionMeasurement>,
    upload: Option<DirectionMeasurement>,
    outcome: Outcome,
}

fn report_from_result(input: ReportInput<'_>) -> BandwidthReport {
    let ReportInput {
        run_id,
        interface,
        provider_id,
        provider_kind,
        server,
        failures,
        download,
        upload,
        outcome,
    } = input;
    let now = Utc::now();
    let mut events = failures
        .iter()
        .enumerate()
        .map(|(index, failure)| {
            failure_event(
                run_id,
                index,
                interface,
                provider_id,
                provider_kind,
                failure,
                now,
            )
        })
        .collect::<Vec<_>>();
    let mut bandwidth = MeasurementEvent::new(
        run_id,
        format!("{run_id}:bandwidth"),
        EventKind::Bandwidth,
        outcome,
        now,
    );
    bandwidth.started_at_utc = download
        .as_ref()
        .or(upload.as_ref())
        .map(|measurement| now - chrono_duration(measurement.elapsed));
    bandwidth.interface = interface.map(str::to_owned);
    bandwidth.source_ip = upload
        .as_ref()
        .or(download.as_ref())
        .map(|measurement| measurement.source_ip);
    bandwidth.trigger_reason = Some(TriggerReason::Manual);
    bandwidth.provider_id = Some(provider_id.to_owned());
    bandwidth.provider_kind = Some(provider_kind);
    bandwidth.server = server;
    bandwidth.remote_ip = upload
        .as_ref()
        .or(download.as_ref())
        .map(|measurement| measurement.remote_ip);
    bandwidth.download_mbps = download
        .as_ref()
        .and_then(|measurement| throughput_mbps(measurement.bytes, measurement.elapsed));
    bandwidth.upload_mbps = upload
        .as_ref()
        .and_then(|measurement| throughput_mbps(measurement.bytes, measurement.elapsed));
    bandwidth.bytes_received = download.as_ref().map(|measurement| measurement.bytes);
    bandwidth.bytes_sent = upload.as_ref().map(|measurement| measurement.bytes);
    bandwidth.duration_ms = Some(
        download.as_ref().map_or(0.0, |measurement| {
            measurement.elapsed.as_secs_f64() * 1_000.0
        }) + upload.as_ref().map_or(0.0, |measurement| {
            measurement.elapsed.as_secs_f64() * 1_000.0
        }),
    );
    let download_metrics = download
        .as_ref()
        .map(|measurement| measurement.metrics)
        .unwrap_or_default();
    let upload_metrics = upload
        .as_ref()
        .map(|measurement| measurement.metrics)
        .unwrap_or_default();
    bandwidth.tcp_min_rtt_ms = upload_metrics.min_rtt_ms.or(download_metrics.min_rtt_ms);
    bandwidth.tcp_rtt_ms = upload_metrics.rtt_ms.or(download_metrics.rtt_ms);
    bandwidth.tcp_retransmissions = upload_metrics
        .retransmissions
        .or(download_metrics.retransmissions);
    if outcome != Outcome::Success {
        bandwidth.error_kind = failures.last().map(|failure| failure.error_kind);
        bandwidth.error_message = failures.last().map(|failure| failure.message.clone());
    }
    events.push(bandwidth);
    BandwidthReport {
        events: events.into_iter().map(|event| event.sanitized()).collect(),
        outcome,
        reserved: false,
        reservation_error: None,
    }
}

fn failure_event(
    run_id: &str,
    index: usize,
    interface: Option<&str>,
    provider_id: &str,
    provider_kind: ProviderKind,
    failure: &RequestFailure,
    now: DateTime<Utc>,
) -> MeasurementEvent {
    let mut event = MeasurementEvent::new(
        run_id,
        format!("{run_id}:request-failure:{index}"),
        EventKind::RequestFailure,
        failure.outcome,
        now,
    );
    event.interface = interface.map(str::to_owned);
    event.provider_id = Some(provider_id.to_owned());
    event.provider_kind = Some(provider_kind);
    event.server.clone_from(&failure.server);
    event.source_ip = failure.source_ip;
    event.remote_ip = failure.remote_ip;
    event.request_stage = Some(failure.stage);
    event.request_attempt = Some(failure.attempt);
    event.http_status = failure.http_status;
    event.retry_after_ms = failure
        .retry_after
        .and_then(|duration| u64::try_from(duration.as_millis()).ok());
    event.rate_limit_until_utc = retry_until(now, failure.retry_after);
    event.error_kind = Some(failure.error_kind);
    event.os_error_code = failure.os_error_code;
    event.error_message = Some(failure.message.clone());
    event
}

pub fn throughput_mbps(bytes: u64, elapsed: Duration) -> Option<f64> {
    let seconds = elapsed.as_secs_f64();
    if seconds <= 0.0 {
        return None;
    }
    let value = bytes as f64 * 8.0 / seconds / 1_000_000.0;
    value.is_finite().then_some(value)
}

fn update_metrics(metrics: &mut TcpMetrics, text: &str) {
    if let Ok(measurement) = serde_json::from_str::<WireMeasurement>(text)
        && let Some(tcp) = measurement.tcp_info
    {
        metrics.min_rtt_ms = tcp.min_rtt.map(|value| value as f64 / 1_000.0);
        metrics.rtt_ms = tcp.rtt.map(|value| value as f64 / 1_000.0);
        metrics.retransmissions = tcp.bytes_retrans;
    }
}

fn next_upload_message_size(current_size: usize, total_bytes: u64) -> usize {
    // Follow the adaptive sizing algorithm from the NDT7 specification appendix.
    if current_size >= MAX_UPLOAD_MESSAGE_SIZE
        || current_size as u64 >= total_bytes / UPLOAD_SCALING_FRACTION
    {
        current_size
    } else {
        current_size.saturating_mul(2).min(MAX_UPLOAD_MESSAGE_SIZE)
    }
}

fn upload_payload() -> Vec<u8> {
    upload_payload_with_size(INITIAL_UPLOAD_MESSAGE_SIZE)
}

fn upload_payload_with_size(size: usize) -> Vec<u8> {
    let mut payload = vec![0_u8; size];
    let mut state = Utc::now().timestamp_nanos_opt().unwrap_or_default() as u64 | 1;
    for byte in &mut payload {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        *byte = state as u8;
    }
    payload
}

fn chrono_duration(duration: Duration) -> chrono::Duration {
    chrono::Duration::from_std(duration).unwrap_or(chrono::Duration::MAX)
}

fn provider_kind(config: &ResolvedConfig) -> ProviderKind {
    match config.bandwidth.provider {
        crate::config::ProviderConfig::Mlab(_) => ProviderKind::Mlab,
        crate::config::ProviderConfig::Direct(_) => ProviderKind::Direct,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use futures_util::{SinkExt, StreamExt};
    use tokio::io::{DuplexStream, duplex};
    use tokio::time::{Instant, advance};
    use tokio_tungstenite::WebSocketStream;
    use tokio_tungstenite::tungstenite::{Message, protocol::Role};

    use super::{
        INITIAL_UPLOAD_MESSAGE_SIZE, MAX_UPLOAD_MESSAGE_SIZE, UploadError,
        next_upload_message_size, transfer_upload, upload_payload,
    };

    async fn upload_pair() -> (WebSocketStream<DuplexStream>, WebSocketStream<DuplexStream>) {
        let (client, server) = duplex(64);
        (
            WebSocketStream::from_raw_socket(client, Role::Client, None).await,
            WebSocketStream::from_raw_socket(server, Role::Server, None).await,
        )
    }

    #[test]
    fn upload_payload_starts_at_the_ndt7_initial_size() {
        assert_eq!(upload_payload().len(), INITIAL_UPLOAD_MESSAGE_SIZE);
    }

    #[test]
    fn upload_message_size_follows_ndt7_growth_boundaries_and_cap() {
        let threshold = (INITIAL_UPLOAD_MESSAGE_SIZE * 16) as u64;
        assert_eq!(
            next_upload_message_size(INITIAL_UPLOAD_MESSAGE_SIZE, threshold),
            INITIAL_UPLOAD_MESSAGE_SIZE
        );
        assert_eq!(
            next_upload_message_size(INITIAL_UPLOAD_MESSAGE_SIZE, threshold + 15),
            INITIAL_UPLOAD_MESSAGE_SIZE
        );
        assert_eq!(
            next_upload_message_size(INITIAL_UPLOAD_MESSAGE_SIZE, threshold + 16),
            INITIAL_UPLOAD_MESSAGE_SIZE * 2
        );

        let half_max = MAX_UPLOAD_MESSAGE_SIZE / 2;
        assert_eq!(
            next_upload_message_size(half_max, (half_max * 16) as u64 + 16),
            MAX_UPLOAD_MESSAGE_SIZE
        );
        assert_eq!(
            next_upload_message_size(MAX_UPLOAD_MESSAGE_SIZE, u64::MAX),
            MAX_UPLOAD_MESSAGE_SIZE
        );
    }

    #[tokio::test(start_paused = true)]
    async fn stalled_upload_stops_at_twelve_seconds_without_counting_cleanup_time() {
        let (client, _server) = upload_pair().await;
        let started = Instant::now();
        let result = transfer_upload(client).await;
        assert_eq!(started.elapsed(), Duration::from_secs(12));
        assert_eq!(result.elapsed, Duration::from_secs(10));
        assert_eq!(result.bytes, INITIAL_UPLOAD_MESSAGE_SIZE as u64);
        assert!(matches!(result.error, Some(UploadError::CloseTimeout)));
    }

    #[tokio::test(start_paused = true)]
    async fn incoming_messages_do_not_requeue_or_recount_a_blocked_payload() {
        let (client, mut server) = upload_pair().await;
        let upload = tokio::spawn(transfer_upload(client));
        tokio::task::yield_now().await;
        advance(Duration::from_secs(1)).await;
        server.send(Message::Ping(vec![1].into())).await.unwrap();
        for _ in 0..20 {
            server
                .send(Message::Text(r#"{"TCPInfo":{"RTT":2500}}"#.into()))
                .await
                .unwrap();
        }
        server.close(None).await.unwrap();
        let result = upload.await.unwrap();
        assert_eq!(result.bytes, INITIAL_UPLOAD_MESSAGE_SIZE as u64);
        assert_eq!(result.elapsed, Duration::from_secs(1));
        assert_eq!(result.metrics.rtt_ms, Some(2.5));
        assert!(matches!(result.error, Some(UploadError::CloseTimeout)));
    }

    #[tokio::test(start_paused = true)]
    async fn upload_automatically_replies_to_ping_and_preserves_partial_frame_on_close() {
        let (client, mut server) = upload_pair().await;
        let upload = tokio::spawn(transfer_upload(client));
        server.send(Message::Ping(vec![7].into())).await.unwrap();
        let mut received = 0;
        loop {
            match server.next().await.unwrap().unwrap() {
                Message::Binary(payload) => received += payload.len(),
                Message::Pong(payload) => {
                    assert_eq!(&payload[..], &[7]);
                    break;
                }
                other => panic!("unexpected message: {other:?}"),
            }
        }
        server.close(None).await.unwrap();
        loop {
            match server.next().await.unwrap().unwrap() {
                Message::Binary(payload) => received += payload.len(),
                Message::Close(_) => break,
                _ => {}
            }
        }
        drop(server);
        let result = upload.await.unwrap();
        assert!(result.error.is_none(), "{result:?}");
        assert_eq!(result.bytes, received as u64);
    }

    #[tokio::test(start_paused = true)]
    async fn abrupt_disconnect_retains_transport_error_and_accepted_bytes() {
        let (client, server) = upload_pair().await;
        let upload = tokio::spawn(transfer_upload(client));
        tokio::task::yield_now().await;
        drop(server);
        let result = upload.await.unwrap();
        assert_eq!(result.bytes, INITIAL_UPLOAD_MESSAGE_SIZE as u64);
        assert!(matches!(result.error, Some(UploadError::Transport(_))));
    }

    #[tokio::test(start_paused = true)]
    async fn deadline_drains_only_the_pending_payload_and_excludes_close_waiting() {
        let (client, mut server) = upload_pair().await;
        let started = Instant::now();
        let upload = tokio::spawn(transfer_upload(client));
        let Message::Binary(first) = server.next().await.unwrap().unwrap() else {
            panic!("expected initial payload");
        };
        let mut received = first.len();
        advance(Duration::from_secs(10)).await;
        loop {
            match server.next().await.unwrap().unwrap() {
                Message::Binary(payload) => received += payload.len(),
                Message::Close(_) => break,
                other => panic!("unexpected message: {other:?}"),
            }
        }
        // Receiving Close queues the reply, but deliberately delay flushing it.
        advance(Duration::from_secs(1)).await;
        server.flush().await.unwrap();
        drop(server);
        let result = upload.await.unwrap();
        assert!(result.error.is_none(), "{result:?}");
        assert_eq!(result.elapsed, Duration::from_secs(10));
        assert_eq!(started.elapsed(), Duration::from_secs(11));
        assert_eq!(result.bytes, received as u64);
        assert!(received <= 2 * INITIAL_UPLOAD_MESSAGE_SIZE);
    }

    #[tokio::test(start_paused = true)]
    async fn immediate_peer_close_does_not_invent_measurement_bytes() {
        let (client, mut server) = upload_pair().await;
        server.close(None).await.unwrap();
        let upload = tokio::spawn(transfer_upload(client));
        assert!(matches!(
            server.next().await.unwrap().unwrap(),
            Message::Close(_)
        ));
        drop(server);
        let result = upload.await.unwrap();
        assert!(result.error.is_none(), "{result:?}");
        assert_eq!(result.bytes, 0);
    }
}

async fn cancellation_requested(shutdown: &mut watch::Receiver<bool>) {
    while !*shutdown.borrow() {
        if shutdown.changed().await.is_err() {
            return;
        }
    }
}

fn trace_console_diagnostic(diagnostic: ConsoleDiagnostic) {
    tracing::warn!(?diagnostic, "bandwidth console diagnostic");
}

fn next_run_number() -> u64 {
    static RUN_NUMBER: AtomicU64 = AtomicU64::new(1);
    RUN_NUMBER.fetch_add(1, Ordering::Relaxed)
}

#[derive(Debug, Deserialize)]
struct WireMeasurement {
    #[serde(rename = "TCPInfo")]
    tcp_info: Option<WireTcpInfo>,
}

#[derive(Debug, Deserialize)]
struct WireTcpInfo {
    #[serde(rename = "MinRTT")]
    min_rtt: Option<u64>,
    #[serde(rename = "RTT")]
    rtt: Option<u64>,
    #[serde(rename = "BytesRetrans")]
    bytes_retrans: Option<u64>,
}
