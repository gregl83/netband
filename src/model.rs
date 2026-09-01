use std::net::IpAddr;

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize, Serializer};
use url::Url;

pub const SCHEMA_VERSION: u8 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    PingProbe,
    PingSummary,
    Bandwidth,
    RequestFailure,
    Scheduler,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Success,
    Partial,
    Timeout,
    Unreachable,
    PermissionDenied,
    Cancelled,
    Error,
    NoCapacity,
    RateLimited,
    Scheduled,
    Rescheduled,
    Deferred,
    Suppressed,
    Expired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerReason {
    Scheduled,
    PingLoss,
    PingRtt,
    Manual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    Mlab,
    Direct,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestStage {
    Locate,
    Dns,
    Connect,
    Tls,
    WebsocketHandshake,
    Download,
    Upload,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    IcmpTimeout,
    IcmpUnreachable,
    PermissionDenied,
    Dns,
    Connect,
    Tls,
    HttpStatus,
    WebsocketHandshake,
    DownloadFailed,
    UploadFailed,
    ProviderCooldown,
    DailyCap,
    Cancelled,
    Timeout,
    Io,
    Protocol,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MeasurementEvent {
    pub schema_version: u8,
    pub run_id: String,
    pub event_id: String,
    #[serde(serialize_with = "serialize_optional_timestamp")]
    pub scheduled_at_utc: Option<DateTime<Utc>>,
    #[serde(serialize_with = "serialize_optional_timestamp")]
    pub started_at_utc: Option<DateTime<Utc>>,
    #[serde(serialize_with = "serialize_optional_timestamp")]
    pub finished_at_utc: Option<DateTime<Utc>>,
    pub interface: Option<String>,
    pub source_ip: Option<IpAddr>,
    pub event_kind: EventKind,
    pub trigger_reason: Option<TriggerReason>,
    pub target: Option<String>,
    pub sequence: Option<u16>,
    pub outcome: Outcome,
    pub duration_ms: Option<f64>,
    pub rtt_ms: Option<f64>,
    pub packets_sent: Option<u32>,
    pub packets_received: Option<u32>,
    pub packet_loss_pct: Option<f64>,
    pub icmp_type: Option<u8>,
    pub icmp_code: Option<u8>,
    pub provider_id: Option<String>,
    pub provider_kind: Option<ProviderKind>,
    pub server: Option<String>,
    pub remote_ip: Option<IpAddr>,
    pub request_stage: Option<RequestStage>,
    pub request_attempt: Option<u32>,
    pub http_status: Option<u16>,
    pub retry_after_ms: Option<u64>,
    #[serde(serialize_with = "serialize_optional_timestamp")]
    pub rate_limit_until_utc: Option<DateTime<Utc>>,
    pub daily_runs_used: Option<u32>,
    pub download_mbps: Option<f64>,
    pub upload_mbps: Option<f64>,
    pub bytes_sent: Option<u64>,
    pub bytes_received: Option<u64>,
    pub tcp_min_rtt_ms: Option<f64>,
    pub tcp_rtt_ms: Option<f64>,
    pub tcp_retransmissions: Option<u64>,
    pub os_error_code: Option<i32>,
    pub error_kind: Option<ErrorKind>,
    pub error_message: Option<String>,
}

impl MeasurementEvent {
    pub fn new(
        run_id: impl Into<String>,
        event_id: impl Into<String>,
        event_kind: EventKind,
        outcome: Outcome,
        finished_at_utc: DateTime<Utc>,
    ) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            run_id: run_id.into(),
            event_id: event_id.into(),
            scheduled_at_utc: None,
            started_at_utc: None,
            finished_at_utc: Some(finished_at_utc),
            interface: None,
            source_ip: None,
            event_kind,
            trigger_reason: None,
            target: None,
            sequence: None,
            outcome,
            duration_ms: None,
            rtt_ms: None,
            packets_sent: None,
            packets_received: None,
            packet_loss_pct: None,
            icmp_type: None,
            icmp_code: None,
            provider_id: None,
            provider_kind: None,
            server: None,
            remote_ip: None,
            request_stage: None,
            request_attempt: None,
            http_status: None,
            retry_after_ms: None,
            rate_limit_until_utc: None,
            daily_runs_used: None,
            download_mbps: None,
            upload_mbps: None,
            bytes_sent: None,
            bytes_received: None,
            tcp_min_rtt_ms: None,
            tcp_rtt_ms: None,
            tcp_retransmissions: None,
            os_error_code: None,
            error_kind: None,
            error_message: None,
        }
    }

    pub fn sanitized(&self) -> Self {
        let mut event = self.clone();
        event.server = event.server.as_deref().map(sanitize_endpoint);
        event.error_message = event.error_message.as_deref().map(sanitize_message);
        event
    }
}

pub fn timestamp_text(timestamp: DateTime<Utc>) -> String {
    timestamp.to_rfc3339_opts(SecondsFormat::Millis, true)
}

pub fn sanitize_endpoint(value: &str) -> String {
    let Ok(mut url) = Url::parse(value) else {
        return sanitize_message(value);
    };
    let had_query = url.query().is_some();
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_query(None);
    url.set_fragment(None);
    let mut sanitized = url.to_string();
    if had_query {
        sanitized.push_str("?[redacted]");
    }
    sanitized
}

pub fn sanitize_message(value: &str) -> String {
    const KEYS: [&str; 5] = [
        "access_token=",
        "api_key=",
        "authorization=",
        "token=",
        "key=",
    ];
    let mut sanitized = value.to_owned();
    for key in KEYS {
        let mut search_from = 0;
        loop {
            let lowercase = sanitized.to_ascii_lowercase();
            let Some(offset) = lowercase[search_from..].find(key) else {
                break;
            };
            let start = search_from + offset;
            let value_start = start + key.len();
            if sanitized[..start]
                .chars()
                .next_back()
                .is_some_and(char::is_alphanumeric)
            {
                search_from = value_start;
                continue;
            }
            let value_end = sanitized[value_start..]
                .find(|character: char| {
                    character.is_whitespace() || matches!(character, '&' | ',' | ';' | '"' | '\'')
                })
                .map_or(sanitized.len(), |offset| value_start + offset);
            sanitized.replace_range(value_start..value_end, "[redacted]");
            search_from = value_start + "[redacted]".len();
        }
    }
    sanitized
}

fn serialize_optional_timestamp<S>(
    timestamp: &Option<DateTime<Utc>>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match timestamp {
        Some(timestamp) => serializer.serialize_some(&timestamp_text(*timestamp)),
        None => serializer.serialize_none(),
    }
}
