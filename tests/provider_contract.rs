use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

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
        parse_retry_after(&headers, chrono::DateTime::<chrono::Utc>::UNIX_EPOCH),
        Some(netband::provider::RetryAfter {
            delay: Duration::from_secs(120),
            deadline: chrono::DateTime::from_timestamp(120, 0).unwrap()
        })
    );

    headers.insert(
        reqwest::header::RETRY_AFTER,
        "Thu, 01 Jan 1970 00:02:00 GMT".parse().unwrap(),
    );
    assert_eq!(
        parse_retry_after(&headers, chrono::DateTime::<chrono::Utc>::UNIX_EPOCH),
        Some(netband::provider::RetryAfter {
            delay: Duration::from_secs(120),
            deadline: chrono::DateTime::from_timestamp(120, 0).unwrap()
        })
    );

    headers.insert(reqwest::header::RETRY_AFTER, "later".parse().unwrap());
    assert_eq!(
        parse_retry_after(&headers, chrono::DateTime::<chrono::Utc>::UNIX_EPOCH),
        None
    );
    headers.remove(reqwest::header::RETRY_AFTER);
    assert_eq!(
        parse_retry_after(&headers, chrono::DateTime::<chrono::Utc>::UNIX_EPOCH),
        None
    );
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

    let before = chrono::Utc::now();
    let resolution = resolve_endpoints(&config.bandwidth, None).await;
    let after = chrono::Utc::now();
    for failure in resolution.failures.iter().chain(resolution.terminal.iter()) {
        assert!(before <= failure.started_at_utc);
        assert!(failure.started_at_utc <= failure.finished_at_utc);
        assert!(failure.finished_at_utc <= after);
    }

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
    let before = chrono::Utc::now();
    let resolution = resolve_endpoints(&config.bandwidth, None).await;
    let after = chrono::Utc::now();
    for failure in resolution.failures.iter().chain(resolution.terminal.iter()) {
        assert!(before <= failure.started_at_utc);
        assert!(failure.started_at_utc <= failure.finished_at_utc);
        assert!(failure.finished_at_utc <= after);
    }
    server.await.unwrap();

    assert_eq!(resolution.candidates.len(), 1);
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].to_ascii_lowercase().contains(&format!(
        "user-agent: netband/{}",
        env!("CARGO_PKG_VERSION")
    )));
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
        let before = chrono::Utc::now();
        let resolution = resolve_endpoints(&config.bandwidth, None).await;
        let after = chrono::Utc::now();
        for failure in resolution.failures.iter().chain(resolution.terminal.iter()) {
            assert!(before <= failure.started_at_utc);
            assert!(failure.started_at_utc <= failure.finished_at_utc);
            assert!(failure.finished_at_utc <= after);
        }
        server.await.unwrap();
        let failure = resolution.terminal.unwrap();
        assert_eq!(failure.outcome, expected);
        assert_eq!(
            failure.retry_after.map(|retry| retry.delay),
            retry.map(Duration::from_secs)
        );
        assert_eq!(
            failure.retry_after.map(|retry| retry.deadline),
            retry
                .map(|seconds| failure.finished_at_utc + chrono::Duration::seconds(seconds as i64))
        );
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
    let before = chrono::Utc::now();
    let resolution = resolve_endpoints(&config.bandwidth, None).await;
    let after = chrono::Utc::now();
    for failure in resolution.failures.iter().chain(resolution.terminal.iter()) {
        assert!(before <= failure.started_at_utc);
        assert!(failure.started_at_utc <= failure.finished_at_utc);
        assert!(failure.finished_at_utc <= after);
    }
    assert_eq!(resolution.candidates.len(), 1);
    assert!(resolution.failures.is_empty());
}

#[tokio::test]
async fn locate_interruption_preserves_stage_without_measurements_or_reservation() {
    for cancel in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = vec![0_u8; 8192];
            assert!(socket.read(&mut request).await.unwrap() > 0);
            ready_tx.send(()).unwrap();
            let _ = socket.read_to_end(&mut Vec::new()).await;
        });
        let dir = tempdir().unwrap();
        let mut config = mlab_config(dir.path(), &format!("http://{address}"));
        config.bandwidth.whole_test_timeout =
            Duration::from_millis(if cancel { 5000 } else { 500 });
        let (shutdown_tx, shutdown) = cancellation_channel();
        let task = tokio::spawn(async move {
            measure_bandwidth(&config, "locate-interrupted", shutdown).await
        });
        tokio::time::timeout(Duration::from_secs(2), ready_rx)
            .await
            .unwrap()
            .unwrap();
        let observed = chrono::Utc::now();
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
        assert_eq!(report.outcome, outcome);
        assert!(!report.reserved);
        assert_eq!(report.events.len(), 2);
        let failure = &report.events[0];
        assert_eq!(failure.request_stage, Some(RequestStage::Locate));
        assert_eq!(failure.outcome, outcome);
        assert!(failure.server.is_some());
        let bandwidth = report.events.last().unwrap();
        assert!(failure.started_at_utc.unwrap() <= observed);
        assert!(failure.finished_at_utc.unwrap() >= observed);
        assert_eq!(failure.started_at_utc, bandwidth.started_at_utc);
        assert_eq!(failure.finished_at_utc, bandwidth.finished_at_utc);
        assert_eq!(bandwidth.outcome, outcome);
        assert!(bandwidth.download_mbps.is_none());
        assert!(bandwidth.upload_mbps.is_none());
        assert!(bandwidth.download_bytes.is_none());
        assert!(bandwidth.upload_bytes.is_none());
        assert!(
            report
                .events
                .iter()
                .all(|event| event.daily_bandwidth_starts.is_none())
        );
        tokio::time::timeout(Duration::from_secs(1), server)
            .await
            .unwrap()
            .unwrap();
    }
}

#[tokio::test]
async fn locate_reports_preserve_absolute_dates_and_invalid_header_absence() {
    for header in [
        "Thu, 01 Jan 1970 00:02:00 GMT",
        "Tue, 01 Jan 2030 00:00:00 GMT",
        "0",
        "later",
    ] {
        let (base, _, server) = http_server(vec![response(
            "429 Too Many Requests",
            &format!("Retry-After: {header}\r\n"),
            "",
        )])
        .await;
        let dir = tempdir().unwrap();
        let config = mlab_config(dir.path(), &base);
        let (_sender, shutdown) = cancellation_channel();
        let report = measure_bandwidth(&config, "retry-date", shutdown).await;
        server.await.unwrap();
        let failure = &report.events[0];
        let received = failure.finished_at_utc.unwrap();
        let expected = netband::provider::parse_retry_after_value(header, received);
        assert_eq!(
            failure.rate_limit_until_utc,
            expected.map(|retry| retry.deadline)
        );
        assert_eq!(
            failure.retry_after_ms,
            expected.map(|retry| retry.delay.as_millis() as u64)
        );
        assert_eq!(failure.outcome, Outcome::RateLimited);
        assert!(!report.reserved);
    }
}
