use std::collections::{BTreeMap, HashSet};
use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use chrono::{DateTime, SecondsFormat, Utc};
use thiserror::Error;
use tokio::io::AsyncWrite;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::{Instant, MissedTickBehavior};

use crate::bandwidth::{BandwidthReport, measure_bandwidth_with_gate_and_phase};
use crate::config::ResolvedConfig;
use crate::console::{Console, ConsoleStats};
use crate::health::{DegradationReason, HealthConfig, HealthDecision, HealthWindow};
use crate::interfaces::{FairInterfaceSelector, InterfaceResolver, SystemInterfaceResolver};
use crate::journal::{Journal, JournalError, JournalSink, OutputCoordinator};
use crate::ping::{
    PingRoundError, PingRoundReport, PingRoundRequest, PingTransport, SurgePingTransport,
    measure_round,
};
use crate::scheduler::{BandwidthOpportunity, Scheduler, SchedulerError};
use crate::{
    model::ErrorKind, model::EventKind, model::LoadPhase, model::MeasurementEvent, model::Outcome,
};

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
    pub interface_failures: u64,
}

pub trait PingTransportFactory: Send + Sync {
    fn create(&self, interface: &str, targets: &[IpAddr]) -> Arc<dyn PingTransport>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SystemPingTransportFactory;

impl PingTransportFactory for SystemPingTransportFactory {
    fn create(&self, interface: &str, targets: &[IpAddr]) -> Arc<dyn PingTransport> {
        Arc::new(SurgePingTransport::new(Some(interface), targets))
    }
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
    let flush_result = coordinator.flush();
    if flush_result.is_ok() {
        tracing::info!(path = %output_path.display(), "measurement journal flushed");
    }
    let (journal, console) = coordinator.into_parts();
    drop(journal);
    let console_stats = console.shutdown(CONSOLE_SHUTDOWN_TIMEOUT).await;
    flush_result?;
    let monitor_stats = result?;
    Ok(PingMonitorExecution {
        output_path,
        monitor_stats,
        console_stats,
    })
}

pub async fn execute_multi_interface_monitor<W>(
    config: &ResolvedConfig,
    console_writer: W,
    shutdown: watch::Receiver<bool>,
) -> Result<PingMonitorExecution, MonitorError>
where
    W: AsyncWrite + Unpin + Send + 'static,
{
    execute_multi_interface_monitor_with(
        config,
        &SystemInterfaceResolver,
        &SystemPingTransportFactory,
        console_writer,
        shutdown,
    )
    .await
}

pub async fn execute_multi_interface_monitor_with<R, F, W>(
    config: &ResolvedConfig,
    resolver: &R,
    factory: &F,
    console_writer: W,
    shutdown: watch::Receiver<bool>,
) -> Result<PingMonitorExecution, MonitorError>
where
    R: InterfaceResolver,
    F: PingTransportFactory,
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
    let scheduler = config
        .bandwidth
        .automatic_enabled
        .then(|| Scheduler::open(&config.state_file, &config.bandwidth, started_at))
        .transpose()?;
    let result = monitor_multi_interface(
        config,
        resolver,
        factory,
        settings,
        scheduler,
        &mut coordinator,
        shutdown,
    )
    .await;
    let flush_result = coordinator.flush();
    if flush_result.is_ok() {
        tracing::info!(path = %output_path.display(), "measurement journal flushed");
    }
    let (journal, console) = coordinator.into_parts();
    drop(journal);
    let console_stats = console.shutdown(CONSOLE_SHUTDOWN_TIMEOUT).await;
    flush_result?;
    let monitor_stats = result?;
    Ok(PingMonitorExecution {
        output_path,
        monitor_stats,
        console_stats,
    })
}

struct InterfaceRuntime {
    name: String,
    health: HealthWindow,
    latest_success: bool,
    degraded: bool,
    available: bool,
    retry_at: Option<Instant>,
    backoff_step: u8,
}

impl InterfaceRuntime {
    fn new(name: String, health: HealthConfig) -> Self {
        Self {
            name,
            health: HealthWindow::new(health),
            latest_success: false,
            degraded: false,
            available: true,
            retry_at: None,
            backoff_step: 0,
        }
    }

    fn can_resolve(&self, now: Instant) -> bool {
        self.retry_at.is_none_or(|deadline| now >= deadline)
    }

    fn mark_available(&mut self) -> bool {
        let recovered = !self.available;
        self.available = true;
        self.retry_at = None;
        self.backoff_step = 0;
        recovered
    }

    fn mark_failed(&mut self, now: Instant) -> Duration {
        let seconds = 1_u64 << self.backoff_step.min(6);
        let delay = Duration::from_secs(seconds.min(60));
        self.available = false;
        self.latest_success = false;
        self.retry_at = Some(now + delay);
        self.backoff_step = self.backoff_step.saturating_add(1).min(6);
        delay
    }
}

pub async fn monitor_multi_interface<R, F, J, C>(
    resolved: &ResolvedConfig,
    resolver: &R,
    factory: &F,
    config: PingMonitorConfig,
    mut scheduler: Option<Scheduler>,
    coordinator: &mut OutputCoordinator<J, C>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<MonitorStats, MonitorError>
where
    R: InterfaceResolver,
    F: PingTransportFactory,
    J: JournalSink,
    C: crate::console::ConsoleSink,
{
    let mut stats = MonitorStats::default();
    let mut interfaces = resolved
        .interfaces
        .iter()
        .cloned()
        .map(|name| InterfaceRuntime::new(name, config.health))
        .collect::<Vec<_>>();
    let mut fairness = FairInterfaceSelector::new(&resolved.interfaces);
    let mut ticker = tokio::time::interval_at(Instant::now(), config.interval);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut active: Option<(usize, JoinHandle<Result<PingRoundReport, PingRoundError>>)> = None;
    let mut next_interface = 0_usize;
    let mut next_round = 0_u64;
    let mut bandwidth_number = 0_u64;
    let mut interface_event_number = 0_u64;

    loop {
        if *shutdown.borrow() {
            if let Some((index, task)) = active.take() {
                let report = task.await??;
                complete_interface_round(
                    &config.run_id,
                    &mut interfaces[index],
                    report,
                    scheduler.as_mut(),
                    coordinator,
                    &mut stats,
                )?;
            }
            break;
        }

        if let Some((index, task)) = active.as_mut() {
            tokio::select! {
                biased;
                result = task => {
                    let index = *index;
                    active = None;
                    complete_interface_round(
                        &config.run_id,
                        &mut interfaces[index],
                        result??,
                        scheduler.as_mut(),
                        coordinator,
                        &mut stats,
                    )?;
                }
                cancelled = cancellation_requested(&mut shutdown) => {
                    if cancelled {
                        let (index, task) = active.take().expect("active round exists");
                        complete_interface_round(
                            &config.run_id,
                            &mut interfaces[index],
                            task.await??,
                            scheduler.as_mut(),
                            coordinator,
                            &mut stats,
                        )?;
                        break;
                    }
                }
                scheduled = ticker.tick() => {
                    stats.skipped_ticks += 1;
                    tracing::warn!(
                        skipped_ticks = stats.skipped_ticks,
                        scheduled_at = ?scheduled,
                        interface = %interfaces[*index].name,
                        "ping round still active; skipping interval tick"
                    );
                }
            }
            continue;
        }

        if let Some(scheduler) = scheduler.as_mut() {
            let health = interfaces
                .iter()
                .map(|interface| (interface.name.clone(), interface.latest_success))
                .collect::<BTreeMap<_, _>>();
            let mut action = scheduler.poll_interfaces(&config.run_id, Utc::now(), &health)?;
            if let Some(opportunity) = action.opportunity.take() {
                let (selected, selection_events) = select_bandwidth_interface(
                    &config.run_id,
                    &opportunity,
                    &mut interfaces,
                    resolver,
                    &fairness,
                    &mut interface_event_number,
                    &mut stats,
                );
                if let Some(interface) = selected {
                    for event in &mut action.events {
                        if event.interface.is_none() {
                            event.interface = Some(interface.clone());
                        }
                    }
                    coordinator.publish_batch(&action.events)?;
                    coordinator.publish_batch(&selection_events)?;
                    let mut attempt_config = resolved.clone();
                    attempt_config.interfaces = vec![interface.clone()];
                    let bandwidth_run_id =
                        format!("{}:bandwidth:{bandwidth_number}", config.run_id);
                    bandwidth_number = bandwidth_number.wrapping_add(1);
                    let load_transport = factory.create(&interface, &config.targets);
                    let mut report = measure_bandwidth_while_monitoring(
                        &attempt_config,
                        load_transport,
                        &config,
                        &bandwidth_run_id,
                        scheduler,
                        coordinator,
                        shutdown.clone(),
                        &mut ticker,
                        &mut next_round,
                        &mut stats,
                    )
                    .await?;
                    fairness.record_attempt(&interface);
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
                } else {
                    coordinator.publish_batch(&action.events)?;
                    let mut report = BandwidthReport {
                        events: selection_events,
                        outcome: Outcome::Error,
                        reserved: false,
                        reservation_error: None,
                    };
                    let events = scheduler.finish_attempt(
                        &config.run_id,
                        Utc::now(),
                        opportunity,
                        &mut report,
                    )?;
                    report.events.extend(events);
                    coordinator.publish_batch(&report.events)?;
                }
                continue;
            }
            coordinator.publish_batch(&action.events)?;
        }

        tokio::select! {
            cancelled = cancellation_requested(&mut shutdown) => {
                if cancelled {
                    break;
                }
            }
            _ = ticker.tick() => {
                let index = next_interface;
                next_interface = (next_interface + 1) % interfaces.len();
                let runtime = &mut interfaces[index];
                if !runtime.can_resolve(Instant::now()) {
                    continue;
                }
                match resolver.resolve(&runtime.name) {
                    Ok(_) => {
                        if runtime.mark_available() {
                            let event = interface_event(
                                &config.run_id,
                                &mut interface_event_number,
                                &runtime.name,
                                Outcome::Success,
                                None,
                                None,
                                "decision=interface_recovered".to_owned(),
                            );
                            coordinator.publish_batch(&[event])?;
                        }
                        let transport = factory.create(&runtime.name, &config.targets);
                        active = Some((
                            index,
                            start_dynamic_round(transport, &config, next_round, None, None),
                        ));
                        stats.rounds_started += 1;
                        next_round = next_round.wrapping_add(1);
                    }
                    Err(error) => {
                        let delay = runtime.mark_failed(Instant::now());
                        stats.interface_failures += 1;
                        let event = interface_event(
                            &config.run_id,
                            &mut interface_event_number,
                            &runtime.name,
                            Outcome::Deferred,
                            Some(ErrorKind::Io),
                            Some(Utc::now() + chrono_duration(delay)),
                            format!("decision=interface_retry error={error}"),
                        );
                        coordinator.publish_batch(&[event])?;
                    }
                }
            }
        }
    }
    if let Some(scheduler) = scheduler.as_ref() {
        scheduler.flush()?;
    }
    Ok(stats)
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
    let mut active = Some(start_round(&transport, &config, 0, None, None));
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
                    active = Some(start_round(&transport, &config, next_round, None, None));
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
            let load_transport: Arc<dyn PingTransport> = transport.clone();
            let mut report = measure_bandwidth_while_monitoring(
                resolved,
                load_transport,
                &config,
                &bandwidth_run_id,
                &mut scheduler,
                coordinator,
                shutdown.clone(),
                &mut ticker,
                &mut next_round,
                &mut stats,
            )
            .await?;
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
            continue;
        }

        tokio::select! {
            cancelled = cancellation_requested(&mut shutdown) => {
                if cancelled {
                    break;
                }
            }
            _ = ticker.tick() => {
                active = Some(start_round(&transport, &config, next_round, None, None));
                stats.rounds_started += 1;
                next_round = next_round.wrapping_add(1);
            }
        }
    }
    scheduler.flush()?;
    Ok(stats)
}

fn complete_interface_round<J, C>(
    run_id: &str,
    runtime: &mut InterfaceRuntime,
    report: PingRoundReport,
    scheduler: Option<&mut Scheduler>,
    coordinator: &mut OutputCoordinator<J, C>,
    stats: &mut MonitorStats,
) -> Result<(), MonitorError>
where
    J: JournalSink,
    C: crate::console::ConsoleSink,
{
    coordinator.publish_batch(&report.events)?;
    let decision = runtime.health.observe_events(&report.events);
    trace_health(decision);
    runtime.latest_success = report.successful_targets > 0;
    runtime.degraded = matches!(decision, HealthDecision::Degraded { .. });
    stats.rounds_completed += 1;
    stats.successful_probes += report.successful_targets as u64;
    stats.failed_probes += report.failed_targets as u64;
    if let Some(scheduler) = scheduler {
        let events = scheduler.observe_interface_health(
            run_id,
            Utc::now(),
            Some(&runtime.name),
            decision,
        )?;
        coordinator.publish_batch(&events)?;
    }
    Ok(())
}

fn select_bandwidth_interface<R: InterfaceResolver>(
    run_id: &str,
    opportunity: &BandwidthOpportunity,
    interfaces: &mut [InterfaceRuntime],
    resolver: &R,
    fairness: &FairInterfaceSelector,
    event_number: &mut u64,
    stats: &mut MonitorStats,
) -> (Option<String>, Vec<MeasurementEvent>) {
    let mut events = Vec::new();
    if let Some(requested) = opportunity.interface.as_deref() {
        let Some(runtime) = interfaces
            .iter_mut()
            .find(|interface| interface.name == requested)
        else {
            events.push(interface_event(
                run_id,
                event_number,
                requested,
                Outcome::Error,
                Some(ErrorKind::Io),
                None,
                "decision=bandwidth_suppressed reason=trigger_interface_missing".to_owned(),
            ));
            return (None, events);
        };
        if !runtime.can_resolve(Instant::now()) {
            events.push(interface_event(
                run_id,
                event_number,
                requested,
                Outcome::Deferred,
                Some(ErrorKind::Io),
                runtime.retry_at.map(instant_to_utc),
                "decision=bandwidth_suppressed reason=trigger_interface_backoff".to_owned(),
            ));
            return (None, events);
        }
        return match resolver.resolve(requested) {
            Ok(_) => {
                runtime.mark_available();
                (Some(requested.to_owned()), events)
            }
            Err(error) => {
                let delay = runtime.mark_failed(Instant::now());
                stats.interface_failures += 1;
                events.push(interface_event(
                    run_id,
                    event_number,
                    requested,
                    Outcome::Deferred,
                    Some(ErrorKind::Io),
                    Some(Utc::now() + chrono_duration(delay)),
                    format!(
                        "decision=bandwidth_suppressed reason=trigger_interface_unavailable error={error}"
                    ),
                ));
                (None, events)
            }
        };
    }

    let mut eligible = interfaces
        .iter()
        .filter(|interface| {
            interface.available && !interface.degraded && interface.can_resolve(Instant::now())
        })
        .map(|interface| interface.name.clone())
        .collect::<HashSet<_>>();
    while let Some(selected) = fairness.select(&eligible).map(str::to_owned) {
        let runtime = interfaces
            .iter_mut()
            .find(|interface| interface.name == selected)
            .expect("selected interface exists");
        match resolver.resolve(&selected) {
            Ok(_) => {
                runtime.mark_available();
                return (Some(selected), events);
            }
            Err(error) => {
                let delay = runtime.mark_failed(Instant::now());
                stats.interface_failures += 1;
                events.push(interface_event(
                    run_id,
                    event_number,
                    &selected,
                    Outcome::Deferred,
                    Some(ErrorKind::Io),
                    Some(Utc::now() + chrono_duration(delay)),
                    format!(
                        "decision=bandwidth_interface_skipped reason=unavailable error={error}"
                    ),
                ));
                eligible.remove(&selected);
            }
        }
    }
    events.push(interface_event(
        run_id,
        event_number,
        "unassigned",
        Outcome::Suppressed,
        Some(ErrorKind::Io),
        None,
        "decision=bandwidth_suppressed reason=no_healthy_interface".to_owned(),
    ));
    (None, events)
}

fn interface_event(
    run_id: &str,
    event_number: &mut u64,
    interface: &str,
    outcome: Outcome,
    error_kind: Option<ErrorKind>,
    retry_at: Option<DateTime<Utc>>,
    message: String,
) -> MeasurementEvent {
    let number = *event_number;
    *event_number = number.wrapping_add(1);
    let mut event = MeasurementEvent::new(
        run_id,
        format!("{run_id}:interface:{number}"),
        EventKind::Scheduler,
        outcome,
        Utc::now(),
    );
    event.interface = (interface != "unassigned").then(|| interface.to_owned());
    event.error_kind = error_kind;
    event.rate_limit_until_utc = retry_at;
    event.error_message = Some(message);
    event
}

fn instant_to_utc(deadline: Instant) -> DateTime<Utc> {
    Utc::now() + chrono_duration(deadline.saturating_duration_since(Instant::now()))
}

fn chrono_duration(duration: Duration) -> chrono::Duration {
    chrono::Duration::from_std(duration).unwrap_or(chrono::Duration::MAX)
}

#[allow(clippy::too_many_arguments)]
async fn measure_bandwidth_while_monitoring<J, C>(
    resolved: &ResolvedConfig,
    transport: Arc<dyn PingTransport>,
    ping: &PingMonitorConfig,
    bandwidth_run_id: &str,
    scheduler: &mut Scheduler,
    coordinator: &mut OutputCoordinator<J, C>,
    shutdown: watch::Receiver<bool>,
    ticker: &mut tokio::time::Interval,
    next_round: &mut u64,
    stats: &mut MonitorStats,
) -> Result<BandwidthReport, MonitorError>
where
    J: JournalSink,
    C: crate::console::ConsoleSink,
{
    let (phase_sender, phase_receiver) = watch::channel(LoadPhase::Setup);
    let bandwidth = measure_bandwidth_with_gate_and_phase(
        resolved,
        bandwidth_run_id,
        shutdown,
        scheduler,
        phase_sender,
    );
    tokio::pin!(bandwidth);
    let mut active_ping: Option<JoinHandle<Result<PingRoundReport, PingRoundError>>> = None;

    loop {
        if let Some(task) = active_ping.as_mut() {
            tokio::select! {
                biased;
                report = &mut bandwidth => {
                    let task = active_ping.take().expect("active loaded ping round exists");
                    complete_loaded_round(task.await, coordinator, stats)?;
                    return Ok(report);
                }
                result = task => {
                    active_ping = None;
                    complete_loaded_round(result, coordinator, stats)?;
                }
                scheduled = ticker.tick() => {
                    stats.skipped_ticks += 1;
                    tracing::warn!(
                        skipped_ticks = stats.skipped_ticks,
                        scheduled_at = ?scheduled,
                        load_phase = ?*phase_receiver.borrow(),
                        load_run_id = bandwidth_run_id,
                        "ping round still active during bandwidth test; skipping interval tick"
                    );
                }
            }
        } else {
            tokio::select! {
                biased;
                report = &mut bandwidth => return Ok(report),
                _ = ticker.tick() => {
                    let phase = *phase_receiver.borrow();
                    active_ping = Some(start_dynamic_round(
                        Arc::clone(&transport),
                        ping,
                        *next_round,
                        Some(phase),
                        Some(bandwidth_run_id),
                    ));
                    stats.rounds_started += 1;
                    *next_round = next_round.wrapping_add(1);
                }
            }
        }
    }
}

fn complete_loaded_round<J, C>(
    result: Result<Result<PingRoundReport, PingRoundError>, tokio::task::JoinError>,
    coordinator: &mut OutputCoordinator<J, C>,
    stats: &mut MonitorStats,
) -> Result<(), MonitorError>
where
    J: JournalSink,
    C: crate::console::ConsoleSink,
{
    let report = result??;
    debug_assert!(
        report
            .events
            .iter()
            .all(|event| { event.load_phase.is_some() && event.load_run_id.is_some() })
    );
    coordinator.publish_batch(&report.events)?;
    stats.rounds_completed += 1;
    stats.successful_probes += report.successful_targets as u64;
    stats.failed_probes += report.failed_targets as u64;
    Ok(())
}

fn start_dynamic_round(
    transport: Arc<dyn PingTransport>,
    config: &PingMonitorConfig,
    round_number: u64,
    load_phase: Option<LoadPhase>,
    load_run_id: Option<&str>,
) -> JoinHandle<Result<PingRoundReport, PingRoundError>> {
    let request = PingRoundRequest {
        run_id: config.run_id.clone(),
        round_number,
        targets: config.targets.clone(),
        timeout: config.timeout,
        scheduled_at_utc: Utc::now(),
        identifier: config.identifier,
        load_phase,
        load_run_id: load_run_id.map(str::to_owned),
    };
    tokio::spawn(async move { measure_round(transport, request).await })
}

fn start_round<T: PingTransport>(
    transport: &Arc<T>,
    config: &PingMonitorConfig,
    round_number: u64,
    load_phase: Option<LoadPhase>,
    load_run_id: Option<&str>,
) -> JoinHandle<Result<PingRoundReport, PingRoundError>> {
    let request = PingRoundRequest {
        run_id: config.run_id.clone(),
        round_number,
        targets: config.targets.clone(),
        timeout: config.timeout,
        scheduled_at_utc: Utc::now(),
        identifier: config.identifier,
        load_phase,
        load_run_id: load_run_id.map(str::to_owned),
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
