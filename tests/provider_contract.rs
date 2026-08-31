use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use clap::Parser;
use netband::bandwidth::{cancellation_channel, measure_bandwidth};
use netband::cli::Cli;
use netband::config::{ResolveContext, resolve};
use netband::model::{ErrorKind, Outcome, RequestStage};
use netband::provider::{parse_locate_candidates, parse_retry_after, resolve_endpoints};
use tempfile::tempdir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use url::Url;

fn context(root: PathBuf) -> ResolveContext {
    ResolveContext {
        stdout_is_terminal: false,
        current_dir: root.clone(),
        state_dir: root.join("state"),
    }
}

#[test]
fn checked_in_locate_fixture_keeps_secure_pairs_and_reports_missing_urls() {
    let body = include_bytes!("fixtures/locate-v2.json");
    let locate = Url::parse("https://locate.example.test/v2/nearest/ndt/ndt7").unwrap();
    let resolution = parse_locate_candidates(body, "mlab", &locate);

    assert!(resolution.terminal.is_none());
    assert_eq!(resolution.candidates.len(), 1);
    assert_eq!(resolution.failures.len(), 1);
    assert_eq!(
        resolution.candidates[0].logical_server,
        "ndt-a.example.test"
    );
    assert_eq!(resolution.candidates[0].download_url.scheme(), "wss");
    assert!(
        resolution.failures[0]
            .message
            .contains("wss:///ndt/v7/upload")
    );

    let malformed = parse_locate_candidates(b"{not-json", "mlab", &locate);
    assert!(malformed.candidates.is_empty());
    assert_eq!(malformed.terminal.unwrap().stage, RequestStage::Locate);
}

#[test]
fn retry_after_supports_delta_http_date_and_invalid_values() {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(reqwest::header::RETRY_AFTER, "120".parse().unwrap());
    assert_eq!(
        parse_retry_after(&headers, SystemTime::UNIX_EPOCH),
        Some(Duration::from_secs(120))
    );

    headers.insert(
        reqwest::header::RETRY_AFTER,
        "Thu, 01 Jan 1970 00:02:00 GMT".parse().unwrap(),
    );
    assert_eq!(
        parse_retry_after(&headers, SystemTime::UNIX_EPOCH),
        Some(Duration::from_secs(120))
    );

    headers.insert(reqwest::header::RETRY_AFTER, "later".parse().unwrap());
    assert_eq!(parse_retry_after(&headers, SystemTime::UNIX_EPOCH), None);
    headers.remove(reqwest::header::RETRY_AFTER);
    assert_eq!(parse_retry_after(&headers, SystemTime::UNIX_EPOCH), None);
}

async fn http_server(
    responses: Vec<String>,
) -> (String, Arc<Mutex<Vec<String>>>, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&requests);
    let task = tokio::spawn(async move {
        for response in responses {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = vec![0_u8; 8192];
            let count = socket.read(&mut request).await.unwrap();
            captured
                .lock()
                .unwrap()
                .push(String::from_utf8_lossy(&request[..count]).into_owned());
            socket.write_all(response.as_bytes()).await.unwrap();
            socket.shutdown().await.unwrap();
        }
    });
    (format!("http://{address}"), requests, task)
}

fn response(status: &str, headers: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n{body}",
        body.len()
    )
}

fn mlab_config(root: &std::path::Path, locate_url: &str) -> netband::config::ResolvedConfig {
    let cli = Cli::try_parse_from([
        "netband",
        "--mlab-locate-url",
        locate_url,
        "--accept-mlab-policy",
        "once",
        "bandwidth",
    ])
    .unwrap();
    resolve(&cli, &context(root.to_path_buf())).unwrap()
}

#[tokio::test]
async fn unconsented_mlab_resolution_stops_before_the_network_boundary() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dir = tempdir().unwrap();
    let cli = Cli::try_parse_from([
        "netband",
        "--mlab-locate-url",
        &format!("http://{}", listener.local_addr().unwrap()),
        "config",
        "check",
    ])
    .unwrap();
    let config = resolve(&cli, &context(dir.path().to_path_buf())).unwrap();

    let resolution = resolve_endpoints(&config.bandwidth, None).await;

    assert!(resolution.candidates.is_empty());
    let failure = resolution.terminal.unwrap();
    assert_eq!(failure.error_kind, ErrorKind::PermissionDenied);
    assert!(failure.message.contains("policy acceptance"));
    assert!(
        tokio::time::timeout(Duration::from_millis(50), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn locate_follows_redirect_identifies_client_and_parses_multiple_results() {
    let fixture = include_str!("fixtures/locate-v2.json");
    let (base, requests, server) = http_server(vec![
        response("302 Found", "Location: /final\r\n", ""),
        response("200 OK", "Content-Type: application/json\r\n", fixture),
    ])
    .await;
    let dir = tempdir().unwrap();
    let config = mlab_config(dir.path(), &format!("{base}/start"));
    let resolution = resolve_endpoints(&config.bandwidth, None).await;
    server.await.unwrap();

    assert_eq!(resolution.candidates.len(), 1);
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(
        requests[0]
            .to_ascii_lowercase()
            .contains("user-agent: netband/0.1.0")
    );
    assert!(requests[1].starts_with("GET /final HTTP/1.1"));
}

#[tokio::test]
async fn locate_statuses_are_provider_wide_and_preserve_retry_details() {
    for (status, expected, retry) in [
        ("204 No Content", Outcome::NoCapacity, None),
        ("429 Too Many Requests", Outcome::RateLimited, Some(60)),
        ("503 Service Unavailable", Outcome::RateLimited, Some(30)),
    ] {
        let retry_header = retry
            .map(|seconds| format!("Retry-After: {seconds}\r\n"))
            .unwrap_or_default();
        let (base, _, server) = http_server(vec![response(status, &retry_header, "")]).await;
        let dir = tempdir().unwrap();
        let config = mlab_config(dir.path(), &base);
        let resolution = resolve_endpoints(&config.bandwidth, None).await;
        server.await.unwrap();
        let failure = resolution.terminal.unwrap();
        assert_eq!(failure.outcome, expected);
        assert_eq!(failure.retry_after, retry.map(Duration::from_secs));
    }
}

#[tokio::test]
async fn direct_provider_never_contacts_locate() {
    let dir = tempdir().unwrap();
    let cli = Cli::try_parse_from([
        "netband",
        "--ndt-provider",
        "direct",
        "--ndt-target",
        "127.0.0.1:443",
        "once",
        "bandwidth",
    ])
    .unwrap();
    let config = resolve(&cli, &context(dir.path().to_path_buf())).unwrap();
    let resolution = resolve_endpoints(&config.bandwidth, None).await;
    assert_eq!(resolution.candidates.len(), 1);
    assert!(resolution.failures.is_empty());
}

#[tokio::test]
async fn locate_is_included_in_the_whole_test_timeout() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = vec![0_u8; 8192];
        let _ = socket.read(&mut request).await.unwrap();
        tokio::time::sleep(Duration::from_secs(2)).await;
    });
    let dir = tempdir().unwrap();
    let cli = Cli::try_parse_from([
        "netband",
        "--mlab-locate-url",
        &format!("http://{address}"),
        "--bandwidth-timeout",
        "20ms",
        "--accept-mlab-policy",
        "once",
        "bandwidth",
    ])
    .unwrap();
    let config = resolve(&cli, &context(dir.path().to_path_buf())).unwrap();
    let (_shutdown_tx, shutdown) = cancellation_channel();
    let report = measure_bandwidth(&config, "run-locate-timeout", shutdown).await;
    server.abort();
    assert_eq!(report.outcome, Outcome::Timeout);
    assert_eq!(report.events.last().unwrap().outcome, Outcome::Timeout);
}
