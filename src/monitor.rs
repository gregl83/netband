use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use chrono::{SecondsFormat, Utc};
use thiserror::Error;
use tokio::io::AsyncWrite;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::{Instant, MissedTickBehavior};

use crate::bandwidth::measure_bandwidth_with_gate;
use crate::config::ResolvedConfig;
use crate::console::{Console, ConsoleStats};
use crate::health::{DegradationReason, HealthConfig, HealthDecision, HealthWindow};
use crate::journal::{Journal, JournalError, JournalSink, OutputCoordinator};
use crate::ping::{
    PingRoundError, PingRoundReport, PingRoundRequest, PingTransport, measure_round,
};
use crate::scheduler::{Scheduler, SchedulerError};

const CONSOLE_CAPACITY: usize = 256;
const CONSOLE_SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(250);

#[derive(Debug, Clone)]
pub struct PingMonitorConfig {
    pub run_id: String,
    pub targets: Vec<IpAddr>,
    pub interval: Duration,
    pub timeout: Duration,
    pub identifier: u16,
    pub health: HealthConfig,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MonitorStats {
    pub rounds_started: u64,
    pub rounds_completed: u64,
    pub skipped_ticks: u64,
    pub successful_probes: u64,
    pub failed_probes: u64,
    pub bandwidth_attempts: u64,
}

#[derive(Debug)]
pub struct PingMonitorExecution {
    pub output_path: std::path::PathBuf,
    pub monitor_stats: MonitorStats,
    pub console_stats: ConsoleStats,
}

#[derive(Debug, Error)]
pub enum MonitorError {
    #[error(transparent)]
    Journal(#[from] JournalError),
    #[error(transparent)]
    Round(#[from] PingRoundError),
    #[error("ping scheduler task failed: {0}")]
    Task(#[from] tokio::task::JoinError),
    #[error(transparent)]
    Scheduler(#[from] SchedulerError),
}

pub fn cancellation_channel() -> (watch::Sender<bool>, watch::Receiver<bool>) {
    watch::channel(false)
}

pub async fn execute_ping_monitor<T, W>(
    config: &ResolvedConfig,
    transport: Arc<T>,
    console_writer: W,
    shutdown: watch::Receiver<bool>,
) -> Result<PingMonitorExecution, MonitorError>
where
    T: PingTransport,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let started_at = Utc::now();
    let run_number = next_monitor_number();
    let settings = PingMonitorConfig {
        run_id: format!(
            "{}-{}-{run_number}",
            started_at.to_rfc3339_opts(SecondsFormat::Nanos, true),
            std::process::id()
        ),
        targets: config.ping.targets.clone(),
        interval: config.ping.interval,
        timeout: config.ping.timeout,
        identifier: (u64::from(std::process::id()) ^ run_number) as u16,
        health: HealthConfig::from(&config.bandwidth.trigger),
    };
    let (journal, output_path) = Journal::open_at(&config.output, started_at)?;
    let console = Console::spawn(
        config.console,
        console_writer,
        CONSOLE_CAPACITY,
        |diagnostic| tracing::warn!(?diagnostic, "ping console diagnostic"),
    );
    let mut coordinator = OutputCoordinator::new(journal, console);
    let result = if config.bandwidth.automatic_enabled {
        let scheduler = Scheduler::open(&config.state_file, &config.bandwidth, started_at)?;
        monitor_adaptive(
            config,
            transport,
            settings,
            scheduler,
            &mut coordinator,
            shutdown,
        )
        .await
    } else {
        monitor_ping(transport, settings, &mut coordinator, shutdown).await
    };
    let (journal, console) = coordinator.into_parts();
    drop(journal);
    let console_stats = console.shutdown(CONSOLE_SHUTDOWN_TIMEOUT).await;
    let monitor_stats = result?;
    Ok(PingMonitorExecution {
        output_path,
        monitor_stats,
        console_stats,
    })
}

pub async fn monitor_ping<T, J, C>(
    transport: Arc<T>,
    config: PingMonitorConfig,
    coordinator: &mut OutputCoordinator<J, C>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<MonitorStats, MonitorError>
where
    T: PingTransport,
    J: JournalSink,
    C: crate::console::ConsoleSink,
{
    let mut stats = MonitorStats::default();
    let mut health = HealthWindow::new(config.health);
    if *shutdown.borrow() {
        return Ok(stats);
    }

    let mut ticker = tokio::time::interval_at(Instant::now() + config.interval, config.interval);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut active = Some(start_round(&transport, &config, 0));
    stats.rounds_started = 1;
    let mut next_round = 1_u64;

    loop {
        if let Some(task) = active.as_mut() {
            tokio::select! {
                biased;
                result = task => {
                    active = None;
                    let _ = complete_round(result, coordinator, &mut health, &mut stats)?;
                }
                cancelled = cancellation_requested(&mut shutdown) => {
                    if cancelled {
                        let task = active.take().expect("active round exists");
                        let _ = complete_round(task.await, coordinator, &mut health, &mut stats)?;
                        break;
                    }
                }
                scheduled = ticker.tick() => {
                    stats.skipped_ticks += 1;
                    tracing::warn!(
                        skipped_ticks = stats.skipped_ticks,
                        scheduled_at = ?scheduled,
                        "ping round still active; skipping interval tick"
                    );
                }
            }
        } else {
            tokio::select! {
                cancelled = cancellation_requested(&mut shutdown) => {
                    if cancelled {
                        break;
                    }
                }
                _ = ticker.tick() => {
                    active = Some(start_round(&transport, &config, next_round));
                    stats.rounds_started += 1;
                    next_round = next_round.wrapping_add(1);
                }
            }
        }
    }

    Ok(stats)
}

pub async fn monitor_adaptive<T, J, C>(
    resolved: &ResolvedConfig,
    transport: Arc<T>,
    config: PingMonitorConfig,
    mut scheduler: Scheduler,
    coordinator: &mut OutputCoordinator<J, C>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<MonitorStats, MonitorError>
where
    T: PingTransport,
    J: JournalSink,
    C: crate::console::ConsoleSink,
{
    let mut stats = MonitorStats::default();
    let mut health = HealthWindow::new(config.health);
    let mut ticker = tokio::time::interval_at(Instant::now(), config.interval);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut active: Option<JoinHandle<Result<PingRoundReport, PingRoundError>>> = None;
    let mut next_round = 0_u64;
    let mut latest_round_has_success = false;
    let mut bandwidth_number = 0_u64;

    loop {
        if *shutdown.borrow() {
            if let Some(task) = active.take() {
                let _ = complete_round(task.await, coordinator, &mut health, &mut stats)?;
            }
            break;
        }

        if let Some(task) = active.as_mut() {
            tokio::select! {
                biased;
                result = task => {
                    active = None;
                    let (decision, has_success) =
                        complete_round(result, coordinator, &mut health, &mut stats)?;
                    latest_round_has_success = has_success;
                    let events = scheduler.observe_health(&config.run_id, Utc::now(), decision)?;
                    coordinator.publish_batch(&events)?;
                }
                cancelled = cancellation_requested(&mut shutdown) => {
                    if cancelled {
                        let task = active.take().expect("active round exists");
                        let _ = complete_round(task.await, coordinator, &mut health, &mut stats)?;
                        break;
                    }
                }
                scheduled = ticker.tick() => {
                    stats.skipped_ticks += 1;
                    tracing::warn!(
                        skipped_ticks = stats.skipped_ticks,
                        scheduled_at = ?scheduled,
                        "ping round still active; skipping interval tick"
                    );
                }
            }
            continue;
        }

        let action = scheduler.poll(&config.run_id, Utc::now(), latest_round_has_success)?;
        coordinator.publish_batch(&action.events)?;
        if let Some(opportunity) = action.opportunity {
            let bandwidth_run_id = format!("{}:bandwidth:{bandwidth_number}", config.run_id);
            bandwidth_number = bandwidth_number.wrapping_add(1);
            let mut report = measure_bandwidth_with_gate(
                resolved,
                &bandwidth_run_id,
                shutdown.clone(),
                &mut scheduler,
            )
            .await;
            stats.bandwidth_attempts += 1;
            let reservation_error = report.reservation_error.clone();
            if reservation_error.is_none() {
                let events = scheduler.finish_attempt(
                    &config.run_id,
                    Utc::now(),
                    opportunity,
                    &mut report,
                )?;
                report.events.extend(events);
            }
            coordinator.publish_batch(&report.events)?;
            if let Some(message) = reservation_error {
                return Err(MonitorError::Scheduler(SchedulerError::Admission(message)));
            }
            ticker.reset_at(Instant::now() + config.interval);
            continue;
        }

        tokio::select! {
            cancelled = cancellation_requested(&mut shutdown) => {
                if cancelled {
                    break;
                }
            }
            _ = ticker.tick() => {
                active = Some(start_round(&transport, &config, next_round));
                stats.rounds_started += 1;
                next_round = next_round.wrapping_add(1);
            }
        }
    }
    Ok(stats)
}

fn start_round<T: PingTransport>(
    transport: &Arc<T>,
    config: &PingMonitorConfig,
    round_number: u64,
) -> JoinHandle<Result<PingRoundReport, PingRoundError>> {
    let request = PingRoundRequest {
        run_id: config.run_id.clone(),
        round_number,
        targets: config.targets.clone(),
        timeout: config.timeout,
        scheduled_at_utc: Utc::now(),
        identifier: config.identifier,
    };
    let transport = Arc::clone(transport);
    tokio::spawn(async move { measure_round(transport, request).await })
}

fn complete_round<J, C>(
    result: Result<Result<PingRoundReport, PingRoundError>, tokio::task::JoinError>,
    coordinator: &mut OutputCoordinator<J, C>,
    health: &mut HealthWindow,
    stats: &mut MonitorStats,
) -> Result<(HealthDecision, bool), MonitorError>
where
    J: JournalSink,
    C: crate::console::ConsoleSink,
{
    let report = result??;
    coordinator.publish_batch(&report.events)?;
    let decision = health.observe_events(&report.events);
    trace_health(decision);
    stats.rounds_completed += 1;
    stats.successful_probes += report.successful_targets as u64;
    stats.failed_probes += report.failed_targets as u64;
    Ok((decision, report.successful_targets > 0))
}

fn trace_health(decision: HealthDecision) {
    let snapshot = decision.snapshot();
    match decision {
        HealthDecision::Healthy(_) => tracing::debug!(
            attempted = snapshot.attempted,
            loss_pct = snapshot.loss_pct,
            p95_rtt_ms = snapshot.p95_rtt_ms,
            sufficient_samples = snapshot.sufficient_samples,
            "ping health evaluated"
        ),
        HealthDecision::Degraded { reason, .. } => tracing::warn!(
            reason = degradation_reason_text(reason),
            attempted = snapshot.attempted,
            loss_pct = snapshot.loss_pct,
            p95_rtt_ms = snapshot.p95_rtt_ms,
            "ping health degraded"
        ),
        HealthDecision::Recovered(_) => tracing::info!(
            attempted = snapshot.attempted,
            loss_pct = snapshot.loss_pct,
            p95_rtt_ms = snapshot.p95_rtt_ms,
            "ping health recovered"
        ),
    }
}

const fn degradation_reason_text(reason: DegradationReason) -> &'static str {
    match reason {
        DegradationReason::Loss => "loss",
        DegradationReason::Rtt => "rtt",
        DegradationReason::LossAndRtt => "loss_and_rtt",
    }
}

async fn cancellation_requested(shutdown: &mut watch::Receiver<bool>) -> bool {
    if *shutdown.borrow() {
        return true;
    }
    shutdown.changed().await.is_err() || *shutdown.borrow()
}

fn next_monitor_number() -> u64 {
    static MONITOR_NUMBER: AtomicU64 = AtomicU64::new(1);
    MONITOR_NUMBER.fetch_add(1, Ordering::Relaxed)
}
