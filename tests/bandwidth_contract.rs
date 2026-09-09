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
    measure_bandwidth, measure_bandwidth_with_gate_and_phase, measure_bandwidth_with_network,
    measure_bandwidth_with_network_and_gate, throughput_mbps,
};
use netband::cli::{Cli, ConsoleMode};
use netband::config::{OutputTarget, ResolveContext, resolve};
use netband::model::{ErrorKind, EventKind, LoadPhase, Outcome, ProviderKind, RequestStage};
use netband::provider::FailureDisposition;
use rcgen::{CertifiedKey, generate_simple_self_signed};
use rustls::ServerConfig;
use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use tempfile::tempdir;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpSocket};
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::accept_hdr_async;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::HeaderValue;

const PROTOCOL: &str = "net.measurementlab.ndt.v7";
const METRICS: &str = r#"{"TCPInfo":{"MinRTT":1200,"RTT":2500,"BytesRetrans":7}}"#;

fn assert_report_timestamps(report: &netband::bandwidth::BandwidthReport) {
    let bandwidth = report.events.last().unwrap();
    let start = bandwidth.started_at_utc.expect("attempt start");
    let finish = bandwidth.finished_at_utc.expect("attempt finish");
    assert!(start <= finish);
    let mut previous_finish = start;
    for event in &report.events[..report.events.len() - 1] {
        let request_start = event.started_at_utc.expect("request start");
        let request_finish = event.finished_at_utc.expect("request finish");
        assert!(start <= request_start && request_start <= request_finish);
        assert!(previous_finish <= request_finish && request_finish <= finish);
        previous_finish = request_finish;
    }
}

struct TimedResolver {
    address: std::net::SocketAddr,
    fail: bool,
    boundaries: Mutex<Vec<(chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>)>>,
}

impl AddressResolver for TimedResolver {
    fn resolve<'a>(&'a self, _: &'a str, _: u16) -> ResolveFuture<'a> {
        Box::pin(async move {
            let start = chrono::Utc::now();
            tokio::time::sleep(Duration::from_millis(50)).await;
            let finish = chrono::Utc::now();
            self.boundaries.lock().unwrap().push((start, finish));
            if self.fail {
                Err("timed DNS failure".to_owned())
            } else {
                Ok(vec![self.address])
            }
        })
    }
}

#[tokio::test]
async fn attempt_and_request_timestamps_include_setup_on_success_and_failure() {
    for fail in [false, true] {
        let (address, server) = successful_server().await;
        let dir = tempdir().unwrap();
        let config = direct_config(dir.path(), address, "5s");
        let resolver = TimedResolver {
            address,
            fail,
            boundaries: Mutex::new(Vec::new()),
        };
        let (_sender, shutdown) = cancellation_channel();
        let before = chrono::Utc::now();
        let report = measure_bandwidth_with_network(
            &config,
            "timed",
            shutdown,
            &netband::bandwidth::SystemTcpConnector,
            &resolver,
        )
        .await;
        let after = chrono::Utc::now();
        assert_report_timestamps(&report);
        let bandwidth = report.events.last().unwrap();
        let boundaries = resolver.boundaries.lock().unwrap().clone();
        assert!(before <= bandwidth.started_at_utc.unwrap());
        assert!(bandwidth.started_at_utc.unwrap() <= boundaries[0].0);
        assert!(boundaries.last().unwrap().1 <= bandwidth.finished_at_utc.unwrap());
        assert!(bandwidth.finished_at_utc.unwrap() <= after);
        if fail {
            assert_eq!(report.outcome, Outcome::Error);
            let failure = &report.events[0];
            assert_eq!(failure.request_stage, Some(RequestStage::Dns));
            assert!(failure.started_at_utc.unwrap() <= boundaries[0].0);
            assert!(failure.finished_at_utc.unwrap() >= boundaries[0].1);
            assert!(bandwidth.download_mbps.is_none());
            server.abort();
        } else {
            assert_eq!(report.outcome, Outcome::Success);
            assert_eq!(boundaries.len(), 2);
            server.await.unwrap();
        }
    }
}

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

#[tokio::test]
async fn download_replies_to_ping_with_the_same_payload() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let mut download = accept_hdr_async(listener.accept().await.unwrap().0, accept_protocol)
            .await
            .unwrap();
        let payload = vec![0, 1, 127, 128, 255];
        download
            .send(Message::Ping(payload.clone().into()))
            .await
            .unwrap();
        let reply = tokio::time::timeout(Duration::from_secs(2), download.next())
            .await
            .expect("download must drive the automatic Pong without more incoming data")
            .unwrap()
            .unwrap();
        assert_eq!(reply, Message::Pong(payload.into()));
        download
            .send(Message::Binary(vec![3; 1024].into()))
            .await
            .unwrap();
        download.close(None).await.unwrap();
        serve_upload(listener.accept().await.unwrap().0).await;
    });
    let dir = tempdir().unwrap();
    let config = direct_config(dir.path(), address, "5s");
    let (_shutdown_tx, shutdown) = cancellation_channel();
    let report = measure_bandwidth(&config, "download-pong", shutdown).await;
    assert_eq!(report.outcome, Outcome::Success);
    assert_eq!(report.events.last().unwrap().bytes_received, Some(1024));
    server.await.unwrap();
}

#[tokio::test]
async fn download_continues_while_pong_writes_are_backpressured() {
    const MESSAGES: u64 = 4096;
    const PAYLOAD_SIZE: u64 = 1024;

    struct SmallSendBuffer;
    impl TcpConnector for SmallSendBuffer {
        fn connect<'a>(
            &'a self,
            remote: std::net::SocketAddr,
            _interface: Option<&str>,
        ) -> ConnectFuture<'a> {
            Box::pin(async move {
                let socket = TcpSocket::new_v4()?;
                socket.set_send_buffer_size(1024)?;
                socket.connect(remote).await
            })
        }
    }

    let listener = TcpSocket::new_v4().unwrap();
    listener.set_recv_buffer_size(1024).unwrap();
    listener.bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let listener = listener.listen(8).unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let mut download = accept_hdr_async(listener.accept().await.unwrap().0, accept_protocol)
            .await
            .unwrap();
        // Never drain the client's Pongs. Their volume exceeds both TCP buffers,
        // while binary data must continue flowing in the opposite direction.
        for _ in 0..MESSAGES {
            download
                .feed(Message::Ping(vec![7; 125].into()))
                .await
                .unwrap();
            download
                .feed(Message::Binary(vec![3; PAYLOAD_SIZE as usize].into()))
                .await
                .unwrap();
        }
        download.close(None).await.unwrap();
        // Keep the download transport open and unread until the client has
        // completed download and started upload; dropping it would unblock writes.
        serve_upload(listener.accept().await.unwrap().0).await;
        drop(download);
    });
    let dir = tempdir().unwrap();
    let config = direct_config(dir.path(), address, "5s");
    let (_shutdown_tx, shutdown) = cancellation_channel();
    let report = measure_bandwidth_with_network(
        &config,
        "download-pong-backpressure",
        shutdown,
        &SmallSendBuffer,
        &netband::bandwidth::SystemAddressResolver,
    )
    .await;
    assert_eq!(report.outcome, Outcome::Success, "{:?}", report.events);
    assert_eq!(
        report.events.last().unwrap().bytes_received,
        Some(MESSAGES * PAYLOAD_SIZE)
    );
    server.await.unwrap();
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
    let CertifiedKey { cert, signing_key } =
        generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
    let ca_path = root.join("local-ca.pem");
    std::fs::write(&ca_path, cert.pem()).unwrap();
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(signing_key.serialize_der()));
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
    assert_report_timestamps(&report);
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
async fn upload_stops_after_ten_seconds_and_completes_the_close_handshake() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        serve_download(listener.accept().await.unwrap().0).await;
        // Connection setup must not consume the ten-second measurement window.
        tokio::time::sleep(Duration::from_millis(100)).await;
        let mut socket = accept_hdr_async(listener.accept().await.unwrap().0, accept_protocol)
            .await
            .unwrap();
        let started = tokio::time::Instant::now();
        let mut largest = 0;
        while let Some(message) = socket.next().await {
            match message.unwrap() {
                Message::Binary(payload) => largest = largest.max(payload.len()),
                Message::Close(_) => {
                    socket.flush().await.unwrap();
                    return (started.elapsed(), largest);
                }
                _ => {}
            }
        }
        panic!("client disconnected without completing the close handshake");
    });
    let dir = tempdir().unwrap();
    let config = direct_config(dir.path(), address, "14s");
    let (_shutdown_tx, shutdown) = cancellation_channel();
    let report = measure_bandwidth(&config, "upload-deadline", shutdown).await;
    assert_eq!(report.outcome, Outcome::Success);
    assert_eq!(report.events.len(), 1, "{:?}", report.events);
    let (elapsed, largest) = server.await.unwrap();
    assert!(elapsed >= Duration::from_secs(10));
    assert!(elapsed < Duration::from_secs(12));
    assert_eq!(largest, 1 << 20, "outbound payloads must cap at 1 MiB");
}

#[tokio::test]
async fn upload_acknowledges_peer_close_before_disconnecting() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        serve_download(listener.accept().await.unwrap().0).await;
        let mut socket = accept_hdr_async(listener.accept().await.unwrap().0, accept_protocol)
            .await
            .unwrap();
        assert!(matches!(
            socket.next().await.unwrap().unwrap(),
            Message::Binary(_)
        ));
        socket.close(None).await.unwrap();
        loop {
            match socket.next().await {
                Some(Ok(Message::Close(_))) => break,
                Some(Ok(_)) => {}
                other => panic!("expected close acknowledgement, got {other:?}"),
            }
        }
    });
    let dir = tempdir().unwrap();
    let config = direct_config(dir.path(), address, "5s");
    let (_shutdown_tx, shutdown) = cancellation_channel();
    let report = measure_bandwidth(&config, "upload-peer-close", shutdown).await;
    assert_eq!(report.outcome, Outcome::Success);
    assert_eq!(report.events.len(), 1, "{:?}", report.events);
    server.await.unwrap();
}

#[tokio::test]
async fn upload_reads_control_messages_while_bulk_writes_are_blocked() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (done_tx, done_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        serve_download(listener.accept().await.unwrap().0).await;
        let mut socket = accept_hdr_async(listener.accept().await.unwrap().0, accept_protocol)
            .await
            .unwrap();
        // Leave the receive side undrained so the client's bulk writes back up.
        tokio::time::sleep(Duration::from_millis(200)).await;
        socket.send(Message::Ping(vec![1].into())).await.unwrap();
        socket.send(Message::Text(METRICS.into())).await.unwrap();
        socket.close(None).await.unwrap();
        let _ = done_rx.await;
    });
    let dir = tempdir().unwrap();
    let config = direct_config(dir.path(), address, "15s");
    let (_shutdown_tx, shutdown) = cancellation_channel();
    let report = tokio::time::timeout(
        Duration::from_secs(4),
        measure_bandwidth(&config, "upload-backpressure", shutdown),
    )
    .await
    .expect("peer Close must start bounded cleanup even behind a blocked Pong write");
    assert_report_timestamps(&report);
    let bandwidth = report.events.last().unwrap();
    assert!(bandwidth.bytes_sent.unwrap() > 0);
    assert_eq!(bandwidth.tcp_rtt_ms, Some(2.5), "read metrics after Ping");
    assert!(
        report.events.iter().any(|event| {
            event.request_stage == Some(RequestStage::Upload)
                && event.error_kind == Some(ErrorKind::UploadFailed)
                && event.outcome == Outcome::Error
                && event.os_error_code.is_none()
                && event.error_message.as_deref()
                    == Some("upload close handshake timed out after 2s")
        }),
        "{:?}",
        report.events
    );
    let _ = done_tx.send(());
    server.await.unwrap();
}

#[tokio::test]
async fn upload_cleanup_retains_load_phase_and_obeys_outer_limits() {
    for (cancel, timeout, outcome) in [
        (true, "15s", Outcome::Cancelled),
        (false, "300ms", Outcome::Timeout),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (closing_tx, closing_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            serve_download(listener.accept().await.unwrap().0).await;
            let mut socket = accept_hdr_async(listener.accept().await.unwrap().0, accept_protocol)
                .await
                .unwrap();
            assert!(matches!(
                socket.next().await.unwrap().unwrap(),
                Message::Binary(_)
            ));
            socket.close(None).await.unwrap();
            closing_tx.send(()).unwrap();
            let _ = release_rx.await;
        });
        let dir = tempdir().unwrap();
        let config = direct_config(dir.path(), address, timeout);
        let (shutdown_tx, shutdown) = cancellation_channel();
        let (phase_tx, phase_rx) = tokio::sync::watch::channel(LoadPhase::Setup);
        let calls = Arc::new(AtomicUsize::new(0));
        let mut gate = RecordingGate {
            reserved: Arc::new(AtomicBool::new(false)),
            calls: Arc::clone(&calls),
        };
        let task = tokio::spawn(async move {
            measure_bandwidth_with_gate_and_phase(
                &config,
                "upload-cleanup-cancel",
                shutdown,
                &mut gate,
                phase_tx,
            )
            .await
        });
        closing_rx.await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            !task.is_finished(),
            "cleanup must remain part of the bandwidth future"
        );
        assert_eq!(*phase_rx.borrow(), LoadPhase::Upload);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        if cancel {
            shutdown_tx.send(true).unwrap();
        }
        let report = tokio::time::timeout(Duration::from_millis(500), task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(report.outcome, outcome);
        assert_eq!(report.exit_code(), 1);
        assert!(report.reserved);
        assert!(
            report
                .events
                .iter()
                .all(|event| event.daily_runs_used == Some(1))
        );
        assert_eq!(
            report
                .events
                .iter()
                .filter(|event| event.event_kind == EventKind::Bandwidth)
                .count(),
            1
        );
        assert_report_timestamps(&report);
        let bandwidth = report.events.last().unwrap();
        assert_eq!(bandwidth.bytes_received, Some(16 * 1024));
        assert!(bandwidth.bytes_sent.unwrap() >= 8192);
        assert!(bandwidth.download_mbps.unwrap() > 0.0);
        assert!(bandwidth.upload_mbps.unwrap() > 0.0);
        let terminal = &report.events[report.events.len() - 2];
        assert_eq!(terminal.request_stage, Some(RequestStage::Upload));
        assert_eq!(terminal.outcome, outcome);
        let _ = release_tx.send(());
        server.await.unwrap();
    }
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
    assert_report_timestamps(&report);
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
async fn tls_download_preserves_large_messages_and_replies_to_ping() {
    let dir = tempdir().unwrap();
    let (ca_path, server_config) = tls_material(dir.path());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let acceptor = TlsAcceptor::from(server_config);
    let server = tokio::spawn(async move {
        let tls = acceptor
            .accept(listener.accept().await.unwrap().0)
            .await
            .unwrap();
        let mut download = accept_hdr_async(tls, accept_protocol).await.unwrap();
        download
            .send(Message::Binary(vec![3; 1024 * 1024].into()))
            .await
            .unwrap();
        download.send(Message::Text(METRICS.into())).await.unwrap();
        let payload = vec![0, 127, 128, 255];
        download
            .send(Message::Ping(payload.clone().into()))
            .await
            .unwrap();
        let reply = tokio::time::timeout(Duration::from_secs(1), download.next())
            .await
            .expect("TLS read buffering must not prevent automatic Pong delivery")
            .unwrap()
            .unwrap();
        assert_eq!(reply, Message::Pong(payload.into()));
        download
            .send(Message::Binary(vec![5; 8192].into()))
            .await
            .unwrap();
        download.close(None).await.unwrap();
        let tls = acceptor
            .accept(listener.accept().await.unwrap().0)
            .await
            .unwrap();
        serve_upload(tls).await;
    });
    let config = tls_direct_config(dir.path(), address, &ca_path, "localhost");
    let (_shutdown_tx, shutdown) = cancellation_channel();
    let report = measure_bandwidth(&config, "tls-buffered-download", shutdown).await;
    server.await.unwrap();
    assert_eq!(report.outcome, Outcome::Success);
    assert_eq!(
        report.events.last().unwrap().bytes_received,
        Some(1024 * 1024 + 8192)
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

struct InterruptNetwork {
    stage: RequestStage,
    after_download: bool,
    resolutions: AtomicUsize,
    connections: AtomicUsize,
    address: std::net::SocketAddr,
    ready: Arc<tokio::sync::Notify>,
}

impl AddressResolver for InterruptNetwork {
    fn resolve<'a>(&'a self, _host: &'a str, _port: u16) -> ResolveFuture<'a> {
        Box::pin(async move {
            let call = self.resolutions.fetch_add(1, Ordering::SeqCst);
            if self.stage == RequestStage::Dns && call == usize::from(self.after_download) {
                self.ready.notify_one();
                return std::future::pending().await;
            }
            // An injected failed address tests retention of earlier diagnostics.
            Ok(vec!["127.0.0.1:0".parse().unwrap(), self.address])
        })
    }
}

impl TcpConnector for InterruptNetwork {
    fn connect<'a>(
        &'a self,
        remote: std::net::SocketAddr,
        interface: Option<&'a str>,
    ) -> ConnectFuture<'a> {
        Box::pin(async move {
            if remote.port() == 0 {
                return Err(std::io::Error::other("injected address failure"));
            }
            let call = self.connections.fetch_add(1, Ordering::SeqCst);
            if self.stage == RequestStage::Connect && call == usize::from(self.after_download) {
                self.ready.notify_one();
                return std::future::pending().await;
            }
            netband::bandwidth::SystemTcpConnector
                .connect(remote, interface)
                .await
        })
    }
}

#[tokio::test]
async fn interruption_preserves_completed_directions_diagnostics_and_admission() {
    for after_download in [false, true] {
        for stage in [
            RequestStage::Dns,
            RequestStage::Connect,
            RequestStage::Tls,
            RequestStage::WebsocketHandshake,
            if after_download {
                RequestStage::Upload
            } else {
                RequestStage::Download
            },
        ] {
            for cancel in [false, true] {
                let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                let address = listener.local_addr().unwrap();
                let ready = Arc::new(tokio::sync::Notify::new());
                let server_ready = Arc::clone(&ready);
                let server = tokio::spawn(async move {
                    if after_download {
                        serve_download(listener.accept().await.unwrap().0).await;
                    }
                    if matches!(stage, RequestStage::Dns | RequestStage::Connect) {
                        return;
                    }
                    let (mut stream, _) = listener.accept().await.unwrap();
                    if matches!(stage, RequestStage::Tls | RequestStage::WebsocketHandshake) {
                        let mut byte = [0];
                        stream.read_exact(&mut byte).await.unwrap();
                        server_ready.notify_one();
                        // EOF proves cancellation dropped the connection, with no detached I/O.
                        let _ = stream.read_to_end(&mut Vec::new()).await;
                    } else {
                        let mut socket = accept_hdr_async(stream, accept_protocol).await.unwrap();
                        if after_download {
                            assert!(matches!(
                                socket.next().await.unwrap().unwrap(),
                                Message::Binary(_)
                            ));
                        } else {
                            socket.send(Message::Ping(vec![1].into())).await.unwrap();
                            assert!(matches!(
                                socket.next().await.unwrap().unwrap(),
                                Message::Pong(_)
                            ));
                        }
                        server_ready.notify_one();
                        while let Some(Ok(_)) = socket.next().await {}
                    }
                });
                let dir = tempdir().unwrap();
                let mut config =
                    direct_config(dir.path(), address, if cancel { "5s" } else { "500ms" });
                if stage == RequestStage::Tls {
                    let netband::config::ProviderConfig::Direct(direct) =
                        &mut config.bandwidth.provider
                    else {
                        unreachable!();
                    };
                    let url = if after_download {
                        &mut direct.upload_url
                    } else {
                        &mut direct.download_url
                    };
                    url.set_scheme("wss").unwrap();
                }
                let network = InterruptNetwork {
                    stage,
                    after_download,
                    address,
                    ready: Arc::clone(&ready),
                    resolutions: AtomicUsize::new(0),
                    connections: AtomicUsize::new(0),
                };
                let calls = Arc::new(AtomicUsize::new(0));
                let mut gate = RecordingGate {
                    reserved: Arc::new(AtomicBool::new(false)),
                    calls: Arc::clone(&calls),
                };
                let (shutdown_tx, shutdown) = cancellation_channel();
                let task = tokio::spawn(async move {
                    measure_bandwidth_with_network_and_gate(
                        &config,
                        "interrupted",
                        shutdown,
                        &network,
                        &network,
                        &mut gate,
                    )
                    .await
                });
                tokio::time::timeout(Duration::from_secs(2), ready.notified())
                    .await
                    .unwrap();
                let stage_observed = chrono::Utc::now();
                if cancel {
                    shutdown_tx.send(true).unwrap();
                }
                let report = tokio::time::timeout(Duration::from_secs(2), task)
                    .await
                    .unwrap()
                    .unwrap();
                let outcome = if cancel {
                    Outcome::Cancelled
                } else {
                    Outcome::Timeout
                };
                assert_eq!(
                    report.outcome, outcome,
                    "{stage:?}, after download: {after_download}"
                );
                assert_eq!(report.exit_code(), 1);
                assert!(report.reserved);
                assert_eq!(calls.load(Ordering::SeqCst), 1);
                assert!(
                    report
                        .events
                        .iter()
                        .all(|event| event.daily_runs_used == Some(1))
                );
                assert_eq!(
                    report
                        .events
                        .iter()
                        .filter(|event| event.event_kind == EventKind::Bandwidth)
                        .count(),
                    1
                );
                assert_report_timestamps(&report);
                let bandwidth = report.events.last().unwrap();
                assert_eq!(
                    bandwidth.bytes_received,
                    after_download.then_some(16 * 1024)
                );
                assert_eq!(bandwidth.download_mbps.is_some(), after_download);
                assert!(bandwidth.upload_mbps.is_none());
                assert!(bandwidth.bytes_sent.is_none());
                if after_download {
                    assert!(bandwidth.download_mbps.unwrap() > 0.0);
                    assert!(bandwidth.duration_ms.unwrap() > 0.0);
                    assert!(bandwidth.tcp_rtt_ms.is_some());
                }
                let terminal = &report.events[report.events.len() - 2];
                assert_eq!(terminal.event_kind, EventKind::RequestFailure);
                assert_eq!(terminal.request_stage, Some(stage));
                assert_eq!(terminal.outcome, outcome);
                assert!(terminal.started_at_utc.unwrap() <= stage_observed);
                assert!(terminal.finished_at_utc.unwrap() >= stage_observed);
                assert_eq!(terminal.finished_at_utc, bandwidth.finished_at_utc);
                if after_download || stage != RequestStage::Dns {
                    assert!(report.events.iter().any(|event| {
                        event
                            .error_message
                            .as_deref()
                            .is_some_and(|message| message.contains("injected address failure"))
                    }));
                }
                tokio::time::timeout(Duration::from_secs(1), server)
                    .await
                    .unwrap()
                    .unwrap();
            }
        }
    }
}

#[tokio::test]
async fn reservation_failure_and_prior_cancellation_never_start_connections() {
    struct RejectGate(usize);
    impl ReservationGate for RejectGate {
        fn reserve(
            &mut self,
            _: chrono::DateTime<chrono::Utc>,
        ) -> Result<AdmissionReservation, String> {
            self.0 += 1;
            Err("injected reservation failure".to_owned())
        }
    }
    for cancel in [false, true] {
        let dir = tempdir().unwrap();
        let address = "127.0.0.1:443".parse().unwrap();
        let config = direct_config(dir.path(), address, "1s");
        let connector = RecordingConnector::default();
        let mut gate = RejectGate(0);
        let (shutdown_tx, shutdown) = cancellation_channel();
        if cancel {
            shutdown_tx.send(true).unwrap();
        }
        let report = measure_bandwidth_with_network_and_gate(
            &config,
            "not-admitted",
            shutdown,
            &connector,
            &FixedResolver(vec![address]),
            &mut gate,
        )
        .await;
        assert!(connector.calls.lock().unwrap().is_empty());
        assert_eq!(gate.0, usize::from(!cancel));
        assert!(!report.reserved);
        assert_eq!(
            report.outcome,
            if cancel {
                Outcome::Cancelled
            } else {
                Outcome::Error
            }
        );
        assert_eq!(
            report.reservation_error.as_deref(),
            (!cancel).then_some("injected reservation failure")
        );
        assert_eq!(report.events.len(), 2);
        assert!(
            report
                .events
                .iter()
                .all(|event| event.daily_runs_used.is_none())
        );
        assert_report_timestamps(&report);
        let bandwidth = report.events.last().unwrap();
        assert!(bandwidth.download_mbps.is_none());
        assert!(bandwidth.upload_mbps.is_none());
    }
}
