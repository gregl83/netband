use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use clap::Parser;
use futures_util::{SinkExt, StreamExt};
use netband::bandwidth::{
    AddressResolver, AdmissionReservation, ConnectFuture, ReservationGate, ResolveFuture,
    TcpConnector, cancellation_channel, classify_handshake_status, execute_bandwidth_once,
    measure_bandwidth, measure_bandwidth_with_network, measure_bandwidth_with_network_and_gate,
    throughput_mbps,
};
use netband::cli::{Cli, ConsoleMode};
use netband::config::{OutputTarget, ResolveContext, resolve};
use netband::model::{EventKind, Outcome, ProviderKind, RequestStage};
use netband::provider::FailureDisposition;
use rcgen::{CertifiedKey, generate_simple_self_signed};
use rustls::ServerConfig;
use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use tempfile::tempdir;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::accept_hdr_async;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::HeaderValue;

const PROTOCOL: &str = "net.measurementlab.ndt.v7";
const METRICS: &str = r#"{"TCPInfo":{"MinRTT":1200,"RTT":2500,"BytesRetrans":7}}"#;

fn context(root: PathBuf) -> ResolveContext {
    ResolveContext {
        stdout_is_terminal: false,
        current_dir: root.clone(),
        state_dir: root.join("state"),
    }
}

fn direct_config(
    root: &std::path::Path,
    address: std::net::SocketAddr,
    timeout: &str,
) -> netband::config::ResolvedConfig {
    let download = format!("ws://{address}/custom/download?access_token=download-secret");
    let upload = format!("ws://{address}/custom/upload?access_token=upload-secret");
    let cli = Cli::try_parse_from([
        "netband",
        "--ndt-provider",
        "direct",
        "--ndt-download-url",
        &download,
        "--ndt-upload-url",
        &upload,
        "--allow-insecure-ndt",
        "--bandwidth-timeout",
        timeout,
        "once",
        "bandwidth",
    ])
    .unwrap();
    resolve(&cli, &context(root.to_path_buf())).unwrap()
}

#[allow(clippy::result_large_err)]
fn accept_protocol(_request: &Request, mut response: Response) -> Result<Response, ErrorResponse> {
    response
        .headers_mut()
        .insert("sec-websocket-protocol", HeaderValue::from_static(PROTOCOL));
    Ok(response)
}

async fn serve_download<S>(stream: S)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut socket = accept_hdr_async(stream, accept_protocol).await.unwrap();
    socket
        .send(Message::Binary(vec![3_u8; 16 * 1024].into()))
        .await
        .unwrap();
    socket.send(Message::Text(METRICS.into())).await.unwrap();
    socket.close(None).await.unwrap();
}

async fn serve_upload<S>(stream: S)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut socket = accept_hdr_async(stream, accept_protocol).await.unwrap();
    let mut bytes = 0;
    while let Some(message) = socket.next().await {
        match message.unwrap() {
            Message::Binary(payload) => {
                bytes += payload.len();
                if bytes >= 16 * 1024 {
                    break;
                }
            }
            Message::Ping(payload) => socket.send(Message::Pong(payload)).await.unwrap(),
            _ => {}
        }
    }
    socket.send(Message::Text(METRICS.into())).await.unwrap();
    socket.close(None).await.unwrap();
}

async fn successful_server() -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (download, _) = listener.accept().await.unwrap();
        serve_download(download).await;

        let (upload, _) = listener.accept().await.unwrap();
        serve_upload(upload).await;
    });
    (address, task)
}

async fn upload_size_server(
    frame_count: usize,
) -> (std::net::SocketAddr, tokio::task::JoinHandle<Vec<usize>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (download, _) = listener.accept().await.unwrap();
        serve_download(download).await;

        let (upload, _) = listener.accept().await.unwrap();
        let mut socket = accept_hdr_async(upload, accept_protocol).await.unwrap();
        let mut sizes = Vec::with_capacity(frame_count);
        while sizes.len() < frame_count {
            match socket.next().await.unwrap().unwrap() {
                Message::Binary(payload) => sizes.push(payload.len()),
                Message::Ping(payload) => socket.send(Message::Pong(payload)).await.unwrap(),
                _ => {}
            }
        }
        socket.send(Message::Text(METRICS.into())).await.unwrap();
        socket.close(None).await.unwrap();
        sizes
    });
    (address, task)
}

fn tls_material(root: &std::path::Path) -> (PathBuf, Arc<ServerConfig>) {
    let CertifiedKey { cert, key_pair } =
        generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
    let ca_path = root.join("local-ca.pem");
    std::fs::write(&ca_path, cert.pem()).unwrap();
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key_pair.serialize_der()));
    let server = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert.der().clone()], key)
        .unwrap();
    (ca_path, Arc::new(server))
}

fn tls_direct_config(
    root: &std::path::Path,
    address: std::net::SocketAddr,
    ca_path: &std::path::Path,
    server_name: &str,
) -> netband::config::ResolvedConfig {
    let cli = Cli::try_parse_from([
        "netband",
        "--ndt-provider",
        "direct",
        "--ndt-target",
        &address.to_string(),
        "--ndt-tls-server-name",
        server_name,
        "--ndt-ca-cert",
        ca_path.to_str().unwrap(),
        "--bandwidth-timeout",
        "2s",
        "once",
        "bandwidth",
    ])
    .unwrap();
    resolve(&cli, &context(root.to_path_buf())).unwrap()
}

async fn partial_server() -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (download, _) = listener.accept().await.unwrap();
        let mut download = accept_hdr_async(download, accept_protocol).await.unwrap();
        download
            .send(Message::Binary(vec![1_u8; 8 * 1024].into()))
            .await
            .unwrap();
        download.close(None).await.unwrap();

        let (mut upload, _) = listener.accept().await.unwrap();
        let mut request = vec![0_u8; 8192];
        let _ = upload.read(&mut request).await.unwrap();
        upload
            .write_all(
                b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
    });
    (address, task)
}

#[test]
fn throughput_uses_decimal_megabits_and_handles_boundaries() {
    assert_eq!(
        throughput_mbps(1_000_000, Duration::from_secs(1)),
        Some(8.0)
    );
    assert_eq!(throughput_mbps(1, Duration::ZERO), None);
    assert!(
        throughput_mbps(u64::MAX, Duration::from_nanos(1))
            .unwrap()
            .is_finite()
    );
}

#[tokio::test]
async fn direct_download_and_upload_produce_attributed_bandwidth_result() {
    let (address, server) = successful_server().await;
    let dir = tempdir().unwrap();
    let config = direct_config(dir.path(), address, "5s");
    let (_shutdown_tx, shutdown) = cancellation_channel();
    let report = measure_bandwidth(&config, "run-success", shutdown).await;
    server.await.unwrap();

    assert_eq!(report.outcome, Outcome::Success);
    assert_eq!(report.exit_code(), 0);
    let bandwidth = report.events.last().unwrap();
    assert_eq!(bandwidth.event_kind, EventKind::Bandwidth);
    assert_eq!(
        bandwidth.provider_id.as_deref(),
        Some(config.bandwidth.provider_id.as_str())
    );
    assert!(bandwidth.remote_ip.is_some());
    assert_eq!(bandwidth.bytes_received, Some(16 * 1024));
    assert!(bandwidth.bytes_sent.unwrap() >= 16 * 1024);
    assert!(bandwidth.download_mbps.unwrap() > 0.0);
    assert!(bandwidth.upload_mbps.unwrap() > 0.0);
    assert_eq!(bandwidth.tcp_min_rtt_ms, Some(1.2));
    assert_eq!(bandwidth.tcp_rtt_ms, Some(2.5));
    assert_eq!(bandwidth.tcp_retransmissions, Some(7));
    assert!(!format!("{bandwidth:?}").contains("download-secret"));
}

#[tokio::test]
async fn upload_messages_scale_at_ndt7_boundaries() {
    let (address, server) = upload_size_server(26).await;
    let dir = tempdir().unwrap();
    let config = direct_config(dir.path(), address, "5s");
    let (_shutdown_tx, shutdown) = cancellation_channel();
    let report = measure_bandwidth(&config, "run-upload-scaling", shutdown).await;
    let sizes = server.await.unwrap();

    assert_eq!(report.outcome, Outcome::Success);
    assert!(sizes[..17].iter().all(|size| *size == 8 * 1024));
    assert!(sizes[17..25].iter().all(|size| *size == 16 * 1024));
    assert_eq!(sizes[25], 32 * 1024);
}

#[tokio::test]
async fn upload_handshake_failure_preserves_partial_download() {
    let (address, server) = partial_server().await;
    let dir = tempdir().unwrap();
    let config = direct_config(dir.path(), address, "5s");
    let (_shutdown_tx, shutdown) = cancellation_channel();
    let report = measure_bandwidth(&config, "run-partial", shutdown).await;
    server.await.unwrap();

    assert_eq!(report.outcome, Outcome::Partial);
    assert_eq!(report.exit_code(), 1);
    assert!(report.events.iter().any(|event| {
        event.event_kind == EventKind::RequestFailure
            && event.request_stage == Some(RequestStage::WebsocketHandshake)
            && event.http_status == Some(500)
    }));
    let bandwidth = report.events.last().unwrap();
    assert!(bandwidth.download_mbps.is_some());
    assert!(bandwidth.upload_mbps.is_none());
    assert!(!format!("{:?}", report.events).contains("upload-secret"));
}

#[test]
fn handshake_status_separates_provider_limits_from_target_fallback() {
    assert_eq!(
        classify_handshake_status(ProviderKind::Mlab, Some(429)),
        (Outcome::RateLimited, FailureDisposition::ProviderWide)
    );
    assert_eq!(
        classify_handshake_status(ProviderKind::Mlab, Some(503)),
        (Outcome::NoCapacity, FailureDisposition::TryNextTarget)
    );
    assert_eq!(
        classify_handshake_status(ProviderKind::Direct, Some(503)),
        (Outcome::NoCapacity, FailureDisposition::Terminal)
    );
}

#[tokio::test]
async fn whole_test_timeout_and_cancellation_always_write_bandwidth_result() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (download, _) = listener.accept().await.unwrap();
        let _download = accept_hdr_async(download, accept_protocol).await.unwrap();
        tokio::time::sleep(Duration::from_secs(2)).await;
    });
    let dir = tempdir().unwrap();
    let config = direct_config(dir.path(), address, "20ms");
    let (_shutdown_tx, shutdown) = cancellation_channel();
    let timed_out = measure_bandwidth(&config, "run-timeout", shutdown).await;
    server.abort();
    assert_eq!(timed_out.outcome, Outcome::Timeout);
    assert_eq!(
        timed_out.events.last().unwrap().event_kind,
        EventKind::Bandwidth
    );

    let (shutdown_tx, shutdown) = cancellation_channel();
    shutdown_tx.send(true).unwrap();
    let cancelled = measure_bandwidth(&config, "run-cancelled", shutdown).await;
    assert_eq!(cancelled.outcome, Outcome::Cancelled);
    assert_eq!(cancelled.events.last().unwrap().outcome, Outcome::Cancelled);
}

#[tokio::test]
async fn provider_wide_handshake_rate_limit_stops_before_upload() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = vec![0_u8; 8192];
        let _ = socket.read(&mut request).await.unwrap();
        socket
            .write_all(b"HTTP/1.1 429 Too Many Requests\r\nRetry-After: 60\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(50), listener.accept())
                .await
                .is_err(),
            "provider-wide rate limit must stop target/upload fallback"
        );
    });
    let dir = tempdir().unwrap();
    let config = direct_config(dir.path(), address, "1s");
    let (_shutdown_tx, shutdown) = cancellation_channel();
    let report = measure_bandwidth(&config, "run-limited", shutdown).await;
    server.await.unwrap();
    assert_eq!(report.outcome, Outcome::RateLimited);
    let failure = &report.events[0];
    assert_eq!(failure.http_status, Some(429));
    assert_eq!(failure.retry_after_ms, Some(60_000));
    assert!(failure.rate_limit_until_utc.is_some());
}

#[derive(Default)]
struct RecordingConnector {
    calls: Mutex<Vec<(std::net::SocketAddr, Option<String>)>>,
}

struct FixedResolver(Vec<std::net::SocketAddr>);

impl AddressResolver for FixedResolver {
    fn resolve<'a>(&'a self, _host: &'a str, _port: u16) -> ResolveFuture<'a> {
        let addresses = self.0.clone();
        Box::pin(async move { Ok(addresses) })
    }
}

impl TcpConnector for RecordingConnector {
    fn connect<'a>(
        &'a self,
        remote: std::net::SocketAddr,
        interface: Option<&'a str>,
    ) -> ConnectFuture<'a> {
        self.calls
            .lock()
            .unwrap()
            .push((remote, interface.map(str::to_owned)));
        Box::pin(async { Err(std::io::Error::other("injected connect failure")) })
    }
}

struct RecordingGate {
    reserved: Arc<AtomicBool>,
    calls: Arc<AtomicUsize>,
}

impl ReservationGate for RecordingGate {
    fn reserve(
        &mut self,
        _started_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<AdmissionReservation, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.reserved.store(true, Ordering::SeqCst);
        Ok(AdmissionReservation::Reserved { daily_runs_used: 1 })
    }
}

struct ReservationCheckingConnector {
    reserved: Arc<AtomicBool>,
    calls: Arc<AtomicUsize>,
}

impl TcpConnector for ReservationCheckingConnector {
    fn connect<'a>(
        &'a self,
        _remote: std::net::SocketAddr,
        _interface: Option<&'a str>,
    ) -> ConnectFuture<'a> {
        assert!(
            self.reserved.load(Ordering::SeqCst),
            "the allowance must be persisted before connection I/O"
        );
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Err(std::io::Error::other("injected connect failure")) })
    }
}

#[tokio::test]
async fn daily_allowance_is_reserved_once_before_the_first_ndt_connection() {
    let dir = tempdir().unwrap();
    let config = direct_config(dir.path(), "127.0.0.1:443".parse().unwrap(), "1s");
    let reserved = Arc::new(AtomicBool::new(false));
    let gate_calls = Arc::new(AtomicUsize::new(0));
    let connector_calls = Arc::new(AtomicUsize::new(0));
    let mut gate = RecordingGate {
        reserved: Arc::clone(&reserved),
        calls: Arc::clone(&gate_calls),
    };
    let connector = ReservationCheckingConnector {
        reserved,
        calls: Arc::clone(&connector_calls),
    };
    let resolver = FixedResolver(vec!["192.0.2.10:443".parse().unwrap()]);
    let (_shutdown_tx, shutdown) = cancellation_channel();
    let report = measure_bandwidth_with_network_and_gate(
        &config,
        "run-reservation",
        shutdown,
        &connector,
        &resolver,
        &mut gate,
    )
    .await;

    assert_eq!(gate_calls.load(Ordering::SeqCst), 1);
    assert_eq!(connector_calls.load(Ordering::SeqCst), 2);
    assert!(report.reserved);
    assert!(
        report
            .events
            .iter()
            .all(|event| event.daily_runs_used == Some(1))
    );
}

#[tokio::test]
async fn every_direct_connection_receives_the_selected_interface() {
    let dir = tempdir().unwrap();
    let mut config = direct_config(dir.path(), "127.0.0.1:443".parse().unwrap(), "1s");
    config.interfaces = vec!["eth-injected".to_owned()];
    let connector = RecordingConnector::default();
    let (_shutdown_tx, shutdown) = cancellation_channel();
    let addresses = vec![
        "192.0.2.10:443".parse().unwrap(),
        "192.0.2.11:443".parse().unwrap(),
    ];
    let resolver = FixedResolver(addresses.clone());
    let report =
        measure_bandwidth_with_network(&config, "run-binding", shutdown, &connector, &resolver)
            .await;

    assert_eq!(report.outcome, Outcome::Error);
    let calls = connector.calls.lock().unwrap();
    assert_eq!(
        calls
            .iter()
            .map(|(address, _)| *address)
            .collect::<Vec<_>>(),
        [addresses.clone(), addresses].concat(),
        "each direction must try the bounded address list in order"
    );
    assert!(
        calls
            .iter()
            .all(|(_, interface)| interface.as_deref() == Some("eth-injected"))
    );
    assert!(
        report
            .events
            .iter()
            .filter(|event| event.event_kind == EventKind::RequestFailure)
            .all(|event| event.interface.as_deref() == Some("eth-injected"))
    );
}

#[tokio::test]
async fn ip_connect_uses_separate_tls_name_and_private_ca_without_disabling_validation() {
    let dir = tempdir().unwrap();
    let (ca_path, server_config) = tls_material(dir.path());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let acceptor = TlsAcceptor::from(server_config);
    let server = tokio::spawn(async move {
        let (download, _) = listener.accept().await.unwrap();
        serve_download(acceptor.accept(download).await.unwrap()).await;
        let (upload, _) = listener.accept().await.unwrap();
        serve_upload(acceptor.accept(upload).await.unwrap()).await;
    });
    let config = tls_direct_config(dir.path(), address, &ca_path, "localhost");
    let (_shutdown_tx, shutdown) = cancellation_channel();
    let report = measure_bandwidth(&config, "run-tls", shutdown).await;
    server.await.unwrap();
    assert_eq!(report.outcome, Outcome::Success);
    assert_eq!(
        report.events.last().unwrap().remote_ip.unwrap().to_string(),
        "127.0.0.1"
    );

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (_, mismatch_server_config) = tls_material(dir.path());
    let acceptor = TlsAcceptor::from(mismatch_server_config);
    let mismatch_server = tokio::spawn(async move {
        for _ in 0..2 {
            let (stream, _) = listener.accept().await.unwrap();
            assert!(acceptor.accept(stream).await.is_err());
        }
    });
    let mismatch = tls_direct_config(dir.path(), address, &ca_path, "wrong.example");
    let (_shutdown_tx, shutdown) = cancellation_channel();
    let report = measure_bandwidth(&mismatch, "run-mismatch", shutdown).await;
    mismatch_server.await.unwrap();
    assert_eq!(report.outcome, Outcome::Error);
    assert!(report.events.iter().any(|event| {
        event.event_kind == EventKind::RequestFailure
            && event.request_stage == Some(RequestStage::Tls)
    }));
}

async fn execute_mode(mode: ConsoleMode) -> (String, Vec<csv::StringRecord>) {
    let (address, server) = successful_server().await;
    let dir = tempdir().unwrap();
    let mut config = direct_config(dir.path(), address, "5s");
    let output = dir.path().join("bandwidth.csv");
    config.output = OutputTarget::File(output.clone());
    config.console = mode;
    let (writer, mut reader) = tokio::io::duplex(64 * 1024);
    let (_shutdown_tx, shutdown) = cancellation_channel();
    let execution = execute_bandwidth_once(&config, writer, shutdown)
        .await
        .unwrap();
    server.await.unwrap();
    assert_eq!(execution.report.outcome, Outcome::Success);
    let mut console = String::new();
    reader.read_to_string(&mut console).await.unwrap();
    let records = csv::Reader::from_path(output)
        .unwrap()
        .records()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    (console, records)
}

#[tokio::test]
async fn one_shot_pipeline_keeps_csv_authoritative_across_console_modes() {
    let (human, human_csv) = execute_mode(ConsoleMode::Human).await;
    assert_eq!(human.lines().count(), 1);
    assert!(human.contains("bandwidth"));
    assert!(human.contains("outcome=success"));
    assert_eq!(human_csv.last().unwrap().get(8), Some("bandwidth"));

    let (jsonl, jsonl_csv) = execute_mode(ConsoleMode::Jsonl).await;
    assert_eq!(jsonl.lines().count(), jsonl_csv.len());
    assert!(
        jsonl
            .lines()
            .all(|line| serde_json::from_str::<serde_json::Value>(line).is_ok())
    );
    assert!(jsonl.contains("\"event_kind\":\"bandwidth\""));

    let (off, off_csv) = execute_mode(ConsoleMode::Off).await;
    assert!(off.is_empty());
    assert_eq!(off_csv.last().unwrap().get(8), Some("bandwidth"));
}
