use std::net::IpAddr;

use chrono::{DateTime, NaiveDate, SecondsFormat, Utc};
use serde::{Deserialize, Serialize, Serializer};
use url::Url;

macro_rules! id_type {
    ($name:ident) => {
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(uuid::Uuid);

        impl $name {
            pub fn new() -> Self {
                Self(uuid::Uuid::new_v4())
            }
        }
        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }
        impl From<&$name> for $name {
            fn from(id: &$name) -> Self {
                *id
            }
        }
        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(f)
            }
        }
        impl std::str::FromStr for $name {
            type Err = uuid::Error;
            fn from_str(value: &str) -> Result<Self, Self::Err> {
                value.parse().map(Self)
            }
        }
    };
}
id_type!(RunId);
id_type!(EventId);
id_type!(RequestId);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunKind {
    Session,
    PingRound,
    Bandwidth,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SchedulerAction {
    TriggerPending,
    TriggerMergedWithDeferred,
    TriggerCancelled,
    TriggerExpired,
    DeferredExpired,
    BandwidthStart,
    RateLimit,
    ClockRollback,
    Suppressed,
    Deferred,
    InterfaceRecovered,
    InterfaceRetry,
    BandwidthSuppressed,
    BandwidthInterfaceSkipped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SchedulerReason {
    HealthDegraded,
    HealthRecovered,
    TriggerTtlExpired,
    DayLimit,
    AttemptLimit,
    OpportunityReady,
    ProviderRateLimit,
    ClockRollback,
    DailyCap,
    ProviderCooldown,
    MinimumSpacing,
    InterfaceAvailable,
    InterfaceUnavailable,
    TriggerInterfaceMissing,
    TriggerInterfaceBackoff,
    TriggerInterfaceUnavailable,
    NoHealthyInterface,
}

pub const SCHEMA_VERSION: u8 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    RunStarted,
    RunFinished,
    PingProbe,
    Bandwidth,
    RequestFailure,
    Scheduler,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Started,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoadPhase {
    Setup,
    Download,
    Upload,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    Mlab,
    Direct,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestDirection {
    Download,
    Upload,
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
    // Event and run identity.
    pub schema_version: u8,
    pub event_id: EventId,
    pub event_kind: EventKind,
    pub event_sequence: Option<u64>,
    pub run_id: RunId,
    pub parent_run_id: Option<RunId>,
    pub root_run_id: RunId,
    pub run_kind: RunKind,

    // Timing.
    #[serde(serialize_with = "serialize_optional_timestamp")]
    pub scheduled_at_utc: Option<DateTime<Utc>>,
    #[serde(serialize_with = "serialize_optional_timestamp")]
    pub requested_at_utc: Option<DateTime<Utc>>,
    #[serde(serialize_with = "serialize_optional_timestamp")]
    pub started_at_utc: Option<DateTime<Utc>>,
    #[serde(serialize_with = "serialize_optional_timestamp")]
    pub finished_at_utc: Option<DateTime<Utc>>,
    pub elapsed_ms: Option<f64>,

    // Result and explanation.
    pub outcome: Outcome,
    pub message: Option<String>,

    // Session provenance.
    pub command: Option<String>,
    pub netband_version: Option<String>,
    pub process_id: Option<u32>,

    // Shared network context.
    pub interface: Option<String>,
    pub connection_details: Option<serde_json::Map<String, serde_json::Value>>,

    // Provider context.
    pub provider_id: Option<String>,
    pub provider_kind: Option<ProviderKind>,
    pub server_name: Option<String>,

    // Scheduling and accounting.
    pub scheduler_action: Option<SchedulerAction>,
    pub scheduler_reason: Option<SchedulerReason>,
    pub trigger_reason: Option<TriggerReason>,
    #[serde(serialize_with = "serialize_optional_timestamp")]
    pub scheduler_not_before_utc: Option<DateTime<Utc>>,
    pub provider_accounting_date: Option<NaiveDate>,
    pub provider_daily_starts: Option<u32>,
    pub bandwidth_start_reserved: Option<bool>,

    // Ping.
    pub ping_target_ip: Option<String>,
    pub ping_local_ip: Option<IpAddr>,
    pub ping_sequence: Option<u16>,
    pub ping_packets_sent: Option<u32>,
    pub ping_packets_received: Option<u32>,
    pub ping_rtt_ms: Option<f64>,
    pub ping_icmp_type: Option<u8>,
    pub ping_icmp_code: Option<u8>,

    // Concurrent load.
    pub load_run_id: Option<RunId>,
    pub load_phase: Option<LoadPhase>,

    // Request.
    pub request_id: Option<RequestId>,
    pub request_direction: Option<RequestDirection>,
    pub request_stage: Option<RequestStage>,
    pub request_url: Option<String>,
    pub request_local_ip: Option<IpAddr>,
    pub request_remote_ip: Option<IpAddr>,
    pub request_http_status: Option<u16>,
    pub request_retry_after_ms: Option<u64>,
    #[serde(serialize_with = "serialize_optional_timestamp")]
    pub request_retry_at_utc: Option<DateTime<Utc>>,

    // Download.
    pub download_request_id: Option<RequestId>,
    pub download_local_ip: Option<IpAddr>,
    pub download_remote_ip: Option<IpAddr>,
    pub download_bytes: Option<u64>,
    pub download_measurement_duration_ms: Option<f64>,
    pub download_mbps: Option<f64>,
    pub download_server_tcp_min_rtt_ms: Option<f64>,
    pub download_server_tcp_rtt_ms: Option<f64>,
    pub download_server_tcp_retransmitted_bytes: Option<u64>,

    // Upload.
    pub upload_request_id: Option<RequestId>,
    pub upload_local_ip: Option<IpAddr>,
    pub upload_remote_ip: Option<IpAddr>,
    pub upload_bytes: Option<u64>,
    pub upload_measurement_duration_ms: Option<f64>,
    pub upload_mbps: Option<f64>,
    pub upload_server_tcp_min_rtt_ms: Option<f64>,
    pub upload_server_tcp_rtt_ms: Option<f64>,
    pub upload_server_tcp_retransmitted_bytes: Option<u64>,

    // Error details.
    pub error_kind: Option<ErrorKind>,
    pub os_error_code: Option<i32>,
}

impl MeasurementEvent {
    pub fn new(
        run_id: impl Into<RunId>,
        event_kind: EventKind,
        outcome: Outcome,
        finished_at_utc: DateTime<Utc>,
    ) -> Self {
        let run_id = run_id.into();
        Self {
            schema_version: SCHEMA_VERSION,
            event_id: EventId::new(),
            event_kind,
            event_sequence: None,
            run_id,
            parent_run_id: None,
            root_run_id: run_id,
            run_kind: match event_kind {
                EventKind::PingProbe => RunKind::PingRound,
                EventKind::Bandwidth | EventKind::RequestFailure => RunKind::Bandwidth,
                _ => RunKind::Session,
            },
            scheduled_at_utc: None,
            requested_at_utc: None,
            started_at_utc: None,
            finished_at_utc: Some(finished_at_utc),
            elapsed_ms: None,
            outcome,
            message: None,
            command: None,
            netband_version: None,
            process_id: None,
            interface: None,
            connection_details: None,
            provider_id: None,
            provider_kind: None,
            server_name: None,
            scheduler_action: None,
            scheduler_reason: None,
            trigger_reason: None,
            scheduler_not_before_utc: None,
            provider_accounting_date: None,
            provider_daily_starts: None,
            bandwidth_start_reserved: None,
            ping_target_ip: None,
            ping_local_ip: None,
            ping_sequence: None,
            ping_packets_sent: None,
            ping_packets_received: None,
            ping_rtt_ms: None,
            ping_icmp_type: None,
            ping_icmp_code: None,
            load_run_id: None,
            load_phase: None,
            request_id: None,
            request_direction: None,
            request_stage: None,
            request_url: None,
            request_local_ip: None,
            request_remote_ip: None,
            request_http_status: None,
            request_retry_after_ms: None,
            request_retry_at_utc: None,
            download_request_id: None,
            download_local_ip: None,
            download_remote_ip: None,
            download_bytes: None,
            download_measurement_duration_ms: None,
            download_mbps: None,
            download_server_tcp_min_rtt_ms: None,
            download_server_tcp_rtt_ms: None,
            download_server_tcp_retransmitted_bytes: None,
            upload_request_id: None,
            upload_local_ip: None,
            upload_remote_ip: None,
            upload_bytes: None,
            upload_measurement_duration_ms: None,
            upload_mbps: None,
            upload_server_tcp_min_rtt_ms: None,
            upload_server_tcp_rtt_ms: None,
            upload_server_tcp_retransmitted_bytes: None,
            error_kind: None,
            os_error_code: None,
        }
    }

    pub fn sanitized(&self) -> Self {
        let mut event = self.clone();
        event.server_name = event.server_name.as_deref().map(sanitize_message);
        event.request_url = event.request_url.as_deref().map(sanitize_endpoint);
        event.message = event.message.as_deref().map(sanitize_message);
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
