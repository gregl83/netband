use std::collections::HashMap;
use std::net::IpAddr;
use std::path::PathBuf;
use std::time::Duration;

use chrono::{DateTime, Utc};
use reqwest::StatusCode;
use serde::Deserialize;
use url::Url;

use crate::config::{BandwidthConfig, DirectConfig, MlabConfig, ProviderConfig};
use crate::model::{ErrorKind, Outcome, ProviderKind, RequestStage};

pub const USER_AGENT: &str = concat!("netband/", env!("CARGO_PKG_VERSION"));
const DOWNLOAD_KEY: &str = "wss:///ndt/v7/download";
const UPLOAD_KEY: &str = "wss:///ndt/v7/upload";

#[derive(Debug, Clone)]
pub struct EndpointCandidate {
    pub download_url: Url,
    pub upload_url: Url,
    pub logical_server: String,
    pub provider_id: String,
    pub provider_kind: ProviderKind,
    pub tls_server_name: Option<String>,
    pub ca_cert: Option<PathBuf>,
    pub allow_insecure: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureDisposition {
    ProviderWide,
    TryNextTarget,
    Terminal,
}

#[derive(Debug, Clone)]
pub struct RequestFailure {
    pub started_at_utc: DateTime<Utc>,
    pub finished_at_utc: DateTime<Utc>,
    pub stage: RequestStage,
    pub outcome: Outcome,
    pub error_kind: ErrorKind,
    pub message: String,
    pub server_name: Option<String>,
    pub request_url: Option<String>,
    pub local_ip: Option<IpAddr>,
    pub remote_ip: Option<IpAddr>,
    pub os_error_code: Option<i32>,
    pub attempt: u32,
    pub http_status: Option<u16>,
    pub retry_after: Option<RetryAfter>,
    pub disposition: FailureDisposition,
}

impl RequestFailure {
    pub fn simple(
        stage: RequestStage,
        error_kind: ErrorKind,
        message: impl Into<String>,
        request_url: Option<String>,
        attempt: u32,
    ) -> Self {
        let now = Utc::now();
        Self {
            started_at_utc: now,
            finished_at_utc: now,
            stage,
            outcome: Outcome::Error,
            error_kind,
            message: message.into(),
            server_name: None,
            request_url,
            local_ip: None,
            remote_ip: None,
            os_error_code: None,
            attempt,
            http_status: None,
            retry_after: None,
            disposition: FailureDisposition::Terminal,
        }
    }
}

#[derive(Debug)]
pub struct EndpointResolution {
    pub candidates: Vec<EndpointCandidate>,
    pub failures: Vec<RequestFailure>,
    pub terminal: Option<RequestFailure>,
}

pub async fn resolve_endpoints(
    config: &BandwidthConfig,
    interface: Option<&str>,
) -> EndpointResolution {
    let started_at = Utc::now();
    let mut resolution = match &config.provider {
        ProviderConfig::Direct(direct) => resolve_direct(config, direct),
        ProviderConfig::Mlab(mlab) if mlab.policy_accepted => {
            resolve_mlab(config, mlab, interface).await
        }
        ProviderConfig::Mlab(mlab) => EndpointResolution {
            candidates: Vec::new(),
            failures: Vec::new(),
            terminal: Some(RequestFailure::simple(
                RequestStage::Locate,
                ErrorKind::PermissionDenied,
                "M-Lab bandwidth requires explicit policy acceptance",
                Some(mlab.locate_url.to_string()),
                0,
            )),
        },
    };
    for failure in resolution
        .failures
        .iter_mut()
        .chain(resolution.terminal.iter_mut())
    {
        failure.started_at_utc = started_at;
    }
    resolution
}

fn resolve_direct(config: &BandwidthConfig, direct: &DirectConfig) -> EndpointResolution {
    EndpointResolution {
        candidates: vec![EndpointCandidate {
            download_url: direct.download_url.clone(),
            upload_url: direct.upload_url.clone(),
            logical_server: direct.tls_server_name.clone().unwrap_or_else(|| {
                direct
                    .download_url
                    .host_str()
                    .unwrap_or("unknown")
                    .to_owned()
            }),
            provider_id: config.provider_id.clone(),
            provider_kind: ProviderKind::Direct,
            tls_server_name: direct.tls_server_name.clone(),
            ca_cert: direct.ca_cert.clone(),
            allow_insecure: direct.allow_insecure,
        }],
        failures: Vec::new(),
        terminal: None,
    }
}

async fn resolve_mlab(
    config: &BandwidthConfig,
    mlab: &MlabConfig,
    interface: Option<&str>,
) -> EndpointResolution {
    let builder = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .redirect(reqwest::redirect::Policy::limited(5))
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(15));
    let builder = match bind_http_interface(builder, interface) {
        Ok(builder) => builder,
        Err(message) => {
            return terminal_resolution(RequestFailure::simple(
                RequestStage::Locate,
                ErrorKind::Connect,
                message,
                Some(mlab.locate_url.to_string()),
                1,
            ));
        }
    };
    let client = match builder.build() {
        Ok(client) => client,
        Err(error) => {
            return terminal_resolution(RequestFailure::simple(
                RequestStage::Locate,
                ErrorKind::Connect,
                format!("cannot configure Locate client: {error}"),
                Some(mlab.locate_url.to_string()),
                1,
            ));
        }
    };
    let response = match client
        .get(mlab.locate_url.clone())
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            let kind = if error.is_timeout() {
                ErrorKind::Connect
            } else {
                ErrorKind::Io
            };
            return terminal_resolution(RequestFailure::simple(
                RequestStage::Locate,
                kind,
                format!("Locate request failed: {error}"),
                Some(mlab.locate_url.to_string()),
                1,
            ));
        }
    };

    let received_at = Utc::now();
    let status = response.status();
    let retry_after_present = response
        .headers()
        .contains_key(reqwest::header::RETRY_AFTER);
    let retry_after = parse_retry_after(response.headers(), received_at);
    if status != StatusCode::OK {
        let (outcome, base_message) = match status {
            StatusCode::NO_CONTENT => (Outcome::NoCapacity, "Locate returned no capacity"),
            StatusCode::TOO_MANY_REQUESTS => (Outcome::RateLimited, "Locate rate limit"),
            StatusCode::SERVICE_UNAVAILABLE if retry_after.is_some() => (
                Outcome::RateLimited,
                "Locate service requested a retry delay",
            ),
            StatusCode::SERVICE_UNAVAILABLE => (Outcome::Error, "Locate service unavailable"),
            _ => (Outcome::Error, "Locate returned an unexpected status"),
        };
        let retry_detail = match (retry_after_present, retry_after.is_some()) {
            (true, true) => "Retry-After parsed",
            (true, false) => "Retry-After malformed",
            (false, _) => "Retry-After missing",
        };
        return terminal_resolution(RequestFailure {
            started_at_utc: received_at,
            finished_at_utc: received_at,
            stage: RequestStage::Locate,
            outcome,
            error_kind: ErrorKind::HttpStatus,
            message: format!("{base_message}; {retry_detail}"),
            server_name: None,
            request_url: Some(mlab.locate_url.to_string()),
            local_ip: None,
            remote_ip: None,
            os_error_code: None,
            attempt: 1,
            http_status: Some(status.as_u16()),
            retry_after,
            disposition: FailureDisposition::ProviderWide,
        });
    }

    let body = match response.bytes().await {
        Ok(body) => body,
        Err(error) => {
            return terminal_resolution(RequestFailure::simple(
                RequestStage::Locate,
                ErrorKind::Protocol,
                format!("cannot read Locate response: {error}"),
                Some(mlab.locate_url.to_string()),
                1,
            ));
        }
    };
    parse_locate_candidates(&body, &config.provider_id, &mlab.locate_url)
}

pub fn parse_locate_candidates(
    body: &[u8],
    provider_id: &str,
    locate_url: &Url,
) -> EndpointResolution {
    let body = match serde_json::from_slice::<LocateResponse>(body) {
        Ok(body) => body,
        Err(error) => {
            return terminal_resolution(RequestFailure::simple(
                RequestStage::Locate,
                ErrorKind::Protocol,
                format!("invalid Locate response: {error}"),
                Some(locate_url.to_string()),
                1,
            ));
        }
    };
    let mut candidates = Vec::new();
    let mut failures = Vec::new();
    for (index, result) in body.results.into_iter().enumerate() {
        let attempt = index as u32 + 1;
        let Some(download) = result.urls.get(DOWNLOAD_KEY) else {
            failures.push(missing_url_failure(
                &result.machine,
                DOWNLOAD_KEY,
                attempt,
                locate_url,
            ));
            continue;
        };
        let Some(upload) = result.urls.get(UPLOAD_KEY) else {
            failures.push(missing_url_failure(
                &result.machine,
                UPLOAD_KEY,
                attempt,
                locate_url,
            ));
            continue;
        };
        let parsed = Url::parse(download)
            .ok()
            .zip(Url::parse(upload).ok())
            .filter(|(download, upload)| download.scheme() == "wss" && upload.scheme() == "wss");
        let Some((download_url, upload_url)) = parsed else {
            let mut failure = RequestFailure::simple(
                RequestStage::Locate,
                ErrorKind::Protocol,
                "Locate candidate has invalid or insecure NDT7 URLs",
                Some(locate_url.to_string()),
                attempt,
            );
            failure.server_name = Some(result.machine);
            failures.push(failure);
            continue;
        };
        candidates.push(EndpointCandidate {
            download_url,
            upload_url,
            logical_server: result.machine,
            provider_id: provider_id.to_owned(),
            provider_kind: ProviderKind::Mlab,
            tls_server_name: None,
            ca_cert: None,
            allow_insecure: false,
        });
    }
    let terminal = candidates.is_empty().then(|| {
        RequestFailure::simple(
            RequestStage::Locate,
            ErrorKind::Protocol,
            "Locate returned no usable secure NDT7 targets",
            Some(locate_url.to_string()),
            1,
        )
    });
    EndpointResolution {
        candidates,
        failures,
        terminal,
    }
}

fn missing_url_failure(machine: &str, key: &str, attempt: u32, locate_url: &Url) -> RequestFailure {
    let mut failure = RequestFailure::simple(
        RequestStage::Locate,
        ErrorKind::Protocol,
        format!("Locate candidate is missing {key}"),
        Some(locate_url.to_string()),
        attempt,
    );
    failure.server_name = Some(machine.to_owned());
    failure.disposition = FailureDisposition::TryNextTarget;
    failure
}

fn terminal_resolution(failure: RequestFailure) -> EndpointResolution {
    EndpointResolution {
        candidates: Vec::new(),
        failures: Vec::new(),
        terminal: Some(failure),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryAfter {
    pub delay: Duration,
    pub deadline: DateTime<Utc>,
}

pub fn parse_retry_after(
    headers: &reqwest::header::HeaderMap,
    received_at: DateTime<Utc>,
) -> Option<RetryAfter> {
    let value = headers.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?;
    parse_retry_after_value(value, received_at)
}

pub fn parse_retry_after_value(value: &str, received_at: DateTime<Utc>) -> Option<RetryAfter> {
    let value = value.trim();
    if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) {
        let delay = Duration::from_secs(value.parse().ok()?);
        // A valid delay beyond the UTC range must not remove the provider's limit.
        let deadline = chrono::Duration::from_std(delay)
            .ok()
            .and_then(|delay| received_at.checked_add_signed(delay))
            .unwrap_or(DateTime::<Utc>::MAX_UTC);
        return Some(RetryAfter { delay, deadline });
    }
    let deadline = DateTime::<Utc>::from(httpdate::parse_http_date(value).ok()?);
    let delay = (deadline - received_at).to_std().unwrap_or_default();
    Some(RetryAfter { delay, deadline })
}

#[derive(Debug, Deserialize)]
struct LocateResponse {
    #[serde(default)]
    results: Vec<LocateResult>,
}

#[derive(Debug, Deserialize)]
struct LocateResult {
    machine: String,
    #[serde(default)]
    urls: HashMap<String, String>,
}

#[cfg(any(
    target_os = "android",
    target_os = "fuchsia",
    target_os = "illumos",
    target_os = "ios",
    target_os = "linux",
    target_os = "macos",
    target_os = "solaris",
    target_os = "tvos",
    target_os = "visionos",
    target_os = "watchos",
))]
fn bind_http_interface(
    builder: reqwest::ClientBuilder,
    interface: Option<&str>,
) -> Result<reqwest::ClientBuilder, String> {
    Ok(match interface {
        Some(interface) => builder.interface(interface),
        None => builder,
    })
}

#[cfg(not(any(
    target_os = "android",
    target_os = "fuchsia",
    target_os = "illumos",
    target_os = "ios",
    target_os = "linux",
    target_os = "macos",
    target_os = "solaris",
    target_os = "tvos",
    target_os = "visionos",
    target_os = "watchos",
)))]
fn bind_http_interface(
    builder: reqwest::ClientBuilder,
    interface: Option<&str>,
) -> Result<reqwest::ClientBuilder, String> {
    match interface {
        Some(interface) => Err(format!(
            "binding Locate requests to interface {interface} is unsupported"
        )),
        None => Ok(builder),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_deadlines_preserve_dates_and_check_numeric_boundaries() {
        let received = DateTime::from_timestamp(120, 500_000_000).unwrap();
        for seconds in [0, 60, 172_800, u64::MAX] {
            let retry = parse_retry_after_value(&seconds.to_string(), received).unwrap();
            assert_eq!(retry.delay, Duration::from_secs(seconds));
            let expected = if seconds == u64::MAX {
                DateTime::<Utc>::MAX_UTC
            } else {
                received + chrono::Duration::seconds(seconds as i64)
            };
            assert_eq!(retry.deadline, expected);
        }
        assert_eq!(
            parse_retry_after_value("1", DateTime::<Utc>::MAX_UTC)
                .unwrap()
                .deadline,
            DateTime::<Utc>::MAX_UTC
        );
        for (date, seconds, delay_ms) in [
            ("Thu, 01 Jan 1970 00:01:00 GMT", 60, 0),
            ("Thu, 01 Jan 1970 00:02:00 GMT", 120, 0),
            ("Thu, 01 Jan 1970 00:03:00 GMT", 180, 59_500),
        ] {
            let retry = parse_retry_after_value(date, received).unwrap();
            assert_eq!(
                retry.deadline,
                DateTime::from_timestamp(seconds, 0).unwrap()
            );
            assert_eq!(retry.delay, Duration::from_millis(delay_ms));
        }
        for value in ["", "later", "-1", "+1", "1.5", "18446744073709551616"] {
            assert_eq!(parse_retry_after_value(value, received), None, "{value}");
        }
        assert_eq!(
            parse_retry_after_value(" 60 ", received),
            parse_retry_after_value("60", received)
        );
    }
}
