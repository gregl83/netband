use crate::cli::Verbosity;
use crate::model::{EventKind, MeasurementEvent, Outcome};

pub fn init(verbosity: Verbosity) {
    let level = match verbosity {
        Verbosity::Error => tracing::Level::ERROR,
        Verbosity::Warn => tracing::Level::WARN,
        Verbosity::Info => tracing::Level::INFO,
        Verbosity::Debug => tracing::Level::DEBUG,
        Verbosity::Trace => tracing::Level::TRACE,
    };
    let _ = tracing_subscriber::fmt()
        .with_max_level(level)
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .with_target(false)
        .without_time()
        .try_init();
}

pub fn record_events(events: &[MeasurementEvent]) {
    for event in events {
        let event = event.sanitized();
        match event.event_kind {
            EventKind::RequestFailure => tracing::warn!(
                outcome = ?event.outcome,
                provider_id = event.provider_id.as_deref().unwrap_or("-"),
                provider_kind = ?event.provider_kind,
                server_name = event.server_name.as_deref().unwrap_or("-"),
                request_url = event.request_url.as_deref().unwrap_or("-"),
                remote_ip = ?event.remote_ip,
                stage = ?event.request_stage,
                http_status = ?event.http_status,
                cooldown_until = ?event.rate_limit_until_utc,
                error = event.error_message.as_deref().unwrap_or("-"),
                "network request failed"
            ),
            EventKind::PingProbe if event.outcome != Outcome::Success => tracing::warn!(
                outcome = ?event.outcome,
                interface = event.interface.as_deref().unwrap_or("default-route"),
                local_ip = ?event.local_ip,
                target = event.target.as_deref().unwrap_or("-"),
                error_kind = ?event.error_kind,
                error = event.error_message.as_deref().unwrap_or("-"),
                "ping probe failed"
            ),
            EventKind::Scheduler => tracing::info!(
                outcome = ?event.outcome,
                provider_id = event.provider_id.as_deref().unwrap_or("-"),
                provider_kind = ?event.provider_kind,
                interface = event.interface.as_deref().unwrap_or("default-route"),
                trigger = ?event.trigger_reason,
                daily_bandwidth_starts = ?event.daily_bandwidth_starts,
                cooldown_until = ?event.rate_limit_until_utc,
                decision = event.error_message.as_deref().unwrap_or("-"),
                "scheduler decision"
            ),
            EventKind::Bandwidth => tracing::info!(
                outcome = ?event.outcome,
                provider_id = event.provider_id.as_deref().unwrap_or("-"),
                provider_kind = ?event.provider_kind,
                interface = event.interface.as_deref().unwrap_or("default-route"),
                server_name = event.server_name.as_deref().unwrap_or("-"),
                download_remote_ip = ?event.download_remote_ip,
                upload_remote_ip = ?event.upload_remote_ip,
                "bandwidth measurement finished"
            ),
            EventKind::PingProbe => {}
        }
    }
}
