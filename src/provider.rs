use std::collections::HashMap;
use std::net::IpAddr;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

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
    pub stage: RequestStage,
    pub outcome: Outcome,
    pub error_kind: ErrorKind,
    pub message: String,
    pub server: Option<String>,
    pub source_ip: Option<IpAddr>,
    pub remote_ip: Option<IpAddr>,
    pub os_error_code: Option<i32>,
    pub attempt: u32,
    pub http_status: Option<u16>,
    pub retry_after: Option<Duration>,
    pub disposition: FailureDisposition,
}

impl RequestFailure {
    pub fn simple(
        stage: RequestStage,
        error_kind: ErrorKind,
        message: impl Into<String>,
        server: Option<String>,
        attempt: u32,
    ) -> Self {
        Self {
            stage,
            outcome: Outcome::Error,
            error_kind,
            message: message.into(),
            server,
            source_ip: None,
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
    match &config.provider {
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
    }
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

    let status = response.status();
    let retry_after_present = response
        .headers()
        .contains_key(reqwest::header::RETRY_AFTER);
    let retry_after = parse_retry_after(response.headers(), SystemTime::now());
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
            stage: RequestStage::Locate,
            outcome,
            error_kind: ErrorKind::HttpStatus,
            message: format!("{base_message}; {retry_detail}"),
            server: Some(mlab.locate_url.to_string()),
            source_ip: None,
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
            failures.push(missing_url_failure(&result.machine, DOWNLOAD_KEY, attempt));
            continue;
        };
        let Some(upload) = result.urls.get(UPLOAD_KEY) else {
            failures.push(missing_url_failure(&result.machine, UPLOAD_KEY, attempt));
            continue;
        };
        let parsed = Url::parse(download)
            .ok()
            .zip(Url::parse(upload).ok())
            .filter(|(download, upload)| download.scheme() == "wss" && upload.scheme() == "wss");
        let Some((download_url, upload_url)) = parsed else {
            failures.push(RequestFailure::simple(
                RequestStage::Locate,
                ErrorKind::Protocol,
                "Locate candidate has invalid or insecure NDT7 URLs",
                Some(result.machine),
                attempt,
            ));
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

fn missing_url_failure(machine: &str, key: &str, attempt: u32) -> RequestFailure {
    let mut failure = RequestFailure::simple(
        RequestStage::Locate,
        ErrorKind::Protocol,
        format!("Locate candidate is missing {key}"),
        Some(machine.to_owned()),
        attempt,
    );
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

pub fn parse_retry_after(
    headers: &reqwest::header::HeaderMap,
    now: SystemTime,
) -> Option<Duration> {
    let value = headers.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?;
    parse_retry_after_value(value, now)
}

pub fn parse_retry_after_value(value: &str, now: SystemTime) -> Option<Duration> {
    if let Ok(seconds) = value.trim().parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    httpdate::parse_http_date(value)
        .ok()?
        .duration_since(now)
        .ok()
}

pub fn retry_until(now: DateTime<Utc>, delay: Option<Duration>) -> Option<DateTime<Utc>> {
    let delay = chrono::Duration::from_std(delay?).ok()?;
    now.checked_add_signed(delay)
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
