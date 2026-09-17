use netband::journal::CSV_HEADER;
use netband::model::{EventKind, MeasurementEvent, Outcome, RunId};
use serde::Deserializer;
use serde::de::{IgnoredAny, MapAccess, Visitor};

const FAMILIES: &[&str] = &[
    "schema_version,event_id,event_kind,event_sequence,run_id,parent_run_id,run_kind",
    "scheduled_at_utc,started_at_utc,finished_at_utc,elapsed_ms",
    "outcome,message",
    "command,netband_version,process_id",
    "interface,connection_details",
    "provider_id,provider_kind,server_name",
    "scheduler_action,scheduler_reason,trigger_reason,scheduler_not_before_utc,provider_daily_starts",
    "ping_target_ip,ping_local_ip,ping_sequence,ping_packets_sent,ping_packets_received,ping_rtt_ms,ping_icmp_type,ping_icmp_code",
    "load_run_id,load_phase",
    "request_id,request_direction,request_stage,request_url,request_local_ip,request_remote_ip,request_http_status,request_retry_after_ms,request_retry_at_utc",
    "download_request_id,download_local_ip,download_remote_ip,download_bytes,download_measurement_duration_ms,download_mbps,download_server_tcp_min_rtt_ms,download_server_tcp_rtt_ms,download_server_tcp_retransmitted_bytes",
    "upload_request_id,upload_local_ip,upload_remote_ip,upload_bytes,upload_measurement_duration_ms,upload_mbps,upload_server_tcp_min_rtt_ms,upload_server_tcp_rtt_ms,upload_server_tcp_retransmitted_bytes",
    "error_kind,os_error_code",
];

struct Keys;
impl<'de> Visitor<'de> for Keys {
    type Value = Vec<String>;
    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("an event object")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut keys = Vec::new();
        while let Some(key) = map.next_key::<String>()? {
            keys.push(key);
            map.next_value::<IgnoredAny>()?;
        }
        Ok(keys)
    }
}

#[test]
fn csv_and_json_use_the_same_complete_family_order() {
    let expected = FAMILIES.join(",");
    assert_eq!(expected.split(',').count(), 65);
    assert_eq!(CSV_HEADER, expected);
    let event = MeasurementEvent::new(
        RunId::new(),
        EventKind::PingProbe,
        Outcome::Success,
        chrono::Utc::now(),
    );
    let json = serde_json::to_string(&event).unwrap();
    let keys = serde_json::Deserializer::from_str(&json)
        .deserialize_map(Keys)
        .unwrap();
    assert_eq!(keys.join(","), expected);
}

#[test]
fn removed_fields_are_absent_and_family_specific_values_start_unavailable() {
    let event = MeasurementEvent::new(
        RunId::new(),
        EventKind::PingProbe,
        Outcome::Success,
        chrono::Utc::now(),
    );
    let json = serde_json::to_value(event).unwrap();
    for removed in [
        "local_ip",
        "remote_ip",
        "target",
        "sequence",
        "rtt_ms",
        "packet_loss_pct",
        "error_message",
        "rate_limit_until_utc",
        "http_status",
        "retry_after_ms",
        "daily_bandwidth_starts",
    ] {
        assert!(json.get(removed).is_none(), "obsolete column: {removed}");
    }
    for field in [
        "scheduler_reason",
        "ping_local_ip",
        "request_local_ip",
        "request_remote_ip",
        "request_retry_at_utc",
        "scheduler_not_before_utc",
    ] {
        assert!(
            json.get(field).is_some_and(serde_json::Value::is_null),
            "missing optional column: {field}"
        );
    }
}
