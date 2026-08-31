use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, NaiveDate, TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::bandwidth::{AdmissionReservation, BandwidthReport, ReservationGate};
use crate::config::{BandwidthConfig, ProviderConfig};
use crate::health::{DegradationReason, HealthDecision};
use crate::model::{
    ErrorKind, EventKind, MeasurementEvent, Outcome, ProviderKind, RequestStage, TriggerReason,
};

const STATE_SCHEMA_VERSION: u8 = 1;
const DAY_SECONDS: i64 = 86_400;
const MAX_RATE_LIMIT_ATTEMPTS: u8 = 5;
const DEFAULT_INTERFACE_KEY: &str = "";

#[derive(Debug, Error)]
pub enum SchedulerError {
    #[error("scheduler state I/O failed for {path}: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error("scheduler state is corrupt at {path}: {message}")]
    Corrupt { path: PathBuf, message: String },
    #[error("scheduler state schema {0} is unsupported")]
    UnsupportedSchema(u8),
    #[error("bandwidth admission rejected: {0}")]
    Admission(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SchedulerStore {
    schema_version: u8,
    providers: BTreeMap<String, ProviderState>,
}

impl Default for SchedulerStore {
    fn default() -> Self {
        Self {
            schema_version: STATE_SCHEMA_VERSION,
            providers: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProviderState {
    day_utc: NaiveDate,
    slots: Vec<DateTime<Utc>>,
    runs: Vec<DateTime<Utc>>,
    last_started_at_utc: Option<DateTime<Utc>>,
    cooldown_until_utc: Option<DateTime<Utc>>,
    backoff_step: u8,
    deferred: Option<DeferredOpportunity>,
    #[serde(default)]
    interface_triggers: BTreeMap<String, InterfaceTriggerState>,
    policy_daily_max: u32,
    policy_min_spacing_ms: u64,
    policy_slot_jitter_pct: u8,
    rng_state: u64,
    last_observed_utc: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DeferredOpportunity {
    reason: TriggerReason,
    #[serde(default)]
    interface: Option<String>,
    created_at_utc: DateTime<Utc>,
    expires_at_utc: DateTime<Utc>,
    rate_limit_attempts: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PendingTrigger {
    reason: TriggerReason,
    created_at_utc: DateTime<Utc>,
    expires_at_utc: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct InterfaceTriggerState {
    pending: Option<PendingTrigger>,
    latched: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ReservationLedgerEntry {
    provider_id: String,
    started_at_utc: DateTime<Utc>,
}

#[derive(Debug, Clone)]
struct SchedulerPolicy {
    provider_id: String,
    provider_kind: ProviderKind,
    daily_max: u32,
    min_spacing: Duration,
    slot_jitter_pct: u8,
    pending_ttl: Duration,
    cooldown_initial: Duration,
    cooldown_max: Duration,
}

impl From<&BandwidthConfig> for SchedulerPolicy {
    fn from(config: &BandwidthConfig) -> Self {
        Self {
            provider_id: config.provider_id.clone(),
            provider_kind: match config.provider {
                ProviderConfig::Mlab(_) => ProviderKind::Mlab,
                ProviderConfig::Direct(_) => ProviderKind::Direct,
            },
            daily_max: config.daily_max,
            min_spacing: config.min_spacing,
            slot_jitter_pct: config.slot_jitter_pct,
            pending_ttl: config.trigger.pending_ttl,
            cooldown_initial: config.cooldown.initial,
            cooldown_max: config.cooldown.max,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchedulerSnapshot {
    pub provider_id: String,
    pub day_utc: NaiveDate,
    pub slots: Vec<DateTime<Utc>>,
    pub runs: Vec<DateTime<Utc>>,
    pub last_started_at_utc: Option<DateTime<Utc>>,
    pub cooldown_until_utc: Option<DateTime<Utc>>,
    pub backoff_step: u8,
    pub deferred_attempts: Option<u8>,
    pub pending_trigger: Option<TriggerReason>,
    pub trigger_latched: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SchedulerAction {
    pub opportunity: Option<BandwidthOpportunity>,
    pub events: Vec<MeasurementEvent>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BandwidthOpportunity {
    pub reason: TriggerReason,
    pub scheduled_at_utc: DateTime<Utc>,
    pub interface: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ManualDecision {
    Allowed,
    Blocked(Box<MeasurementEvent>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reservation {
    pub daily_runs_used: u32,
}

#[derive(Debug)]
pub struct Scheduler {
    path: PathBuf,
    store: SchedulerStore,
    policy: SchedulerPolicy,
    event_number: u64,
}

impl Scheduler {
    pub fn open(
        path: impl Into<PathBuf>,
        config: &BandwidthConfig,
        now: DateTime<Utc>,
    ) -> Result<Self, SchedulerError> {
        Self::open_seeded(path, config, now, random_seed())
    }

    pub fn open_seeded(
        path: impl Into<PathBuf>,
        config: &BandwidthConfig,
        now: DateTime<Utc>,
        seed: u64,
    ) -> Result<Self, SchedulerError> {
        let path = path.into();
        let store = load_store(&path)?;
        if store.schema_version != STATE_SCHEMA_VERSION {
            return Err(SchedulerError::UnsupportedSchema(store.schema_version));
        }
        let mut scheduler = Self {
            path,
            store,
            policy: SchedulerPolicy::from(config),
            event_number: 0,
        };
        let mut changed = scheduler.reconcile(now, seed);
        let ledger_changed = scheduler.merge_reservation_ledger()?;
        if ledger_changed {
            scheduler.replan_remaining(now);
            changed = true;
        }
        if changed || !scheduler.path.exists() {
            scheduler.persist()?;
        }
        Ok(scheduler)
    }

    pub fn snapshot(&self) -> SchedulerSnapshot {
        let state = self.state();
        SchedulerSnapshot {
            provider_id: self.policy.provider_id.clone(),
            day_utc: state.day_utc,
            slots: state.slots.clone(),
            runs: state
                .runs
                .iter()
                .copied()
                .filter(|run| run.date_naive() == state.day_utc)
                .collect(),
            last_started_at_utc: state.last_started_at_utc,
            cooldown_until_utc: state.cooldown_until_utc,
            backoff_step: state.backoff_step,
            deferred_attempts: state
                .deferred
                .as_ref()
                .map(|deferred| deferred.rate_limit_attempts),
            pending_trigger: state
                .interface_triggers
                .get(DEFAULT_INTERFACE_KEY)
                .and_then(|trigger| trigger.pending.as_ref())
                .map(|trigger| trigger.reason),
            trigger_latched: state
                .interface_triggers
                .get(DEFAULT_INTERFACE_KEY)
                .is_some_and(|trigger| trigger.latched),
        }
    }

    pub fn observe_health(
        &mut self,
        run_id: &str,
        now: DateTime<Utc>,
        decision: HealthDecision,
    ) -> Result<Vec<MeasurementEvent>, SchedulerError> {
        self.observe_interface_health(run_id, now, None, decision)
    }

    pub fn observe_interface_health(
        &mut self,
        run_id: &str,
        now: DateTime<Utc>,
        interface: Option<&str>,
        decision: HealthDecision,
    ) -> Result<Vec<MeasurementEvent>, SchedulerError> {
        let mut events = self.advance_clock(run_id, now)?;
        match decision {
            HealthDecision::Degraded { reason, snapshot }
                if !self.interface_trigger(interface).latched =>
            {
                let trigger_reason = match reason {
                    DegradationReason::Rtt => TriggerReason::PingRtt,
                    DegradationReason::Loss | DegradationReason::LossAndRtt => {
                        TriggerReason::PingLoss
                    }
                };
                let expires_at = add_duration(now, self.policy.pending_ttl);
                let interface_name = interface.map(str::to_owned);
                let has_deferred = self.state().deferred.is_some();
                if has_deferred {
                    let state = self.state_mut();
                    let deferred = state.deferred.as_mut().expect("deferred exists");
                    deferred.reason = trigger_reason;
                    deferred.interface.clone_from(&interface_name);
                }
                let trigger = self.interface_trigger_mut(interface);
                trigger.latched = true;
                trigger.pending = if has_deferred {
                    None
                } else {
                    Some(PendingTrigger {
                        reason: trigger_reason,
                        created_at_utc: now,
                        expires_at_utc: expires_at,
                    })
                };
                self.persist()?;
                let decision_text = if has_deferred {
                    "trigger_merged_with_deferred"
                } else {
                    "trigger_pending"
                };
                events.push(self.event(
                    run_id,
                    now,
                    Outcome::Deferred,
                    trigger_reason,
                    EventContext::for_interface(interface),
                    format!(
                        "decision={decision_text} loss_pct={:.3} p95_rtt_ms={} expires_at={}",
                        snapshot.loss_pct,
                        snapshot
                            .p95_rtt_ms
                            .map_or_else(|| "none".to_owned(), |value| format!("{value:.3}")),
                        expires_at.to_rfc3339()
                    ),
                ));
            }
            HealthDecision::Recovered(_) => {
                let trigger = self.interface_trigger(interface).clone();
                let pending = trigger.pending;
                let was_latched = trigger.latched;
                if pending.is_some() || was_latched {
                    let trigger = self.interface_trigger_mut(interface);
                    trigger.pending = None;
                    trigger.latched = false;
                    self.persist()?;
                }
                if let Some(pending) = pending {
                    events.push(self.event(
                        run_id,
                        now,
                        Outcome::Expired,
                        pending.reason,
                        EventContext::for_interface(interface),
                        "decision=trigger_cancelled reason=health_recovered".to_owned(),
                    ));
                }
            }
            _ => {}
        }
        Ok(events)
    }

    pub fn poll(
        &mut self,
        run_id: &str,
        now: DateTime<Utc>,
        latest_round_has_success: bool,
    ) -> Result<SchedulerAction, SchedulerError> {
        let health = BTreeMap::from([(DEFAULT_INTERFACE_KEY.to_owned(), latest_round_has_success)]);
        self.poll_interfaces(run_id, now, &health)
    }

    pub fn poll_interfaces(
        &mut self,
        run_id: &str,
        now: DateTime<Utc>,
        latest_success: &BTreeMap<String, bool>,
    ) -> Result<SchedulerAction, SchedulerError> {
        let mut events = self.advance_clock(run_id, now)?;
        if now < self.state().last_observed_utc {
            return Ok(SchedulerAction {
                opportunity: None,
                events,
            });
        }

        let expired = self
            .state()
            .interface_triggers
            .iter()
            .filter_map(|(key, state)| {
                state
                    .pending
                    .as_ref()
                    .filter(|trigger| now >= trigger.expires_at_utc)
                    .cloned()
                    .map(|trigger| (key.clone(), trigger))
            })
            .collect::<Vec<_>>();
        if !expired.is_empty() {
            for (key, _) in &expired {
                if let Some(trigger) = self.state_mut().interface_triggers.get_mut(key) {
                    trigger.pending = None;
                }
            }
            self.persist()?;
            for (key, trigger) in expired {
                events.push(self.event(
                    run_id,
                    now,
                    Outcome::Expired,
                    trigger.reason,
                    EventContext::for_interface(interface_from_key(&key)),
                    "decision=trigger_expired reason=ttl".to_owned(),
                ));
            }
        }

        if let Some(deferred) = self.state().deferred.clone() {
            if now >= deferred.expires_at_utc
                || deferred.rate_limit_attempts >= MAX_RATE_LIMIT_ATTEMPTS
            {
                self.state_mut().deferred = None;
                self.persist()?;
                events.push(self.event(
                    run_id,
                    now,
                    Outcome::Expired,
                    deferred.reason,
                    EventContext::new(None, self.state().cooldown_until_utc),
                    "decision=deferred_expired reason=day_or_attempt_limit".to_owned(),
                ));
            } else if self
                .state()
                .cooldown_until_utc
                .is_none_or(|deadline| now >= deadline)
            {
                return Ok(SchedulerAction {
                    opportunity: Some(BandwidthOpportunity {
                        reason: deferred.reason,
                        scheduled_at_utc: deferred.created_at_utc,
                        interface: deferred.interface,
                    }),
                    events,
                });
            }
        }

        let due_slots = self
            .state()
            .slots
            .iter()
            .copied()
            .filter(|slot| *slot <= now)
            .collect::<Vec<_>>();
        let due_slot = due_slots.first().copied();
        let pending = self.oldest_eligible_pending_trigger(latest_success, due_slot.is_some());
        let reason = pending.as_ref().map(|(_, trigger)| trigger.reason);
        let pending_interface = pending
            .as_ref()
            .and_then(|(key, _)| interface_from_key(key).map(str::to_owned));
        let scheduled_at = pending
            .as_ref()
            .map(|(_, trigger)| trigger.created_at_utc)
            .or(due_slot);

        let Some(scheduled_at) = scheduled_at else {
            return Ok(SchedulerAction {
                opportunity: None,
                events,
            });
        };
        let reason = reason.unwrap_or(TriggerReason::Scheduled);
        if let Some(blocked) = self.block_reason(now) {
            if due_slot.is_some() {
                self.remove_due_slots(now);
            }
            let terminal = matches!(blocked.kind, ErrorKind::DailyCap)
                || add_duration(now, self.policy.min_spacing) >= day_end(now.date_naive());
            if terminal && pending.is_some() {
                self.interface_trigger_mut(pending_interface.as_deref())
                    .pending = None;
            }
            self.persist()?;
            events.push(
                self.event(
                    run_id,
                    now,
                    if terminal {
                        Outcome::Suppressed
                    } else {
                        Outcome::Deferred
                    },
                    reason,
                    EventContext::new(Some(blocked.kind), blocked.cooldown_until)
                        .with_interface(pending_interface.as_deref()),
                    blocked.message,
                ),
            );
            return Ok(SchedulerAction {
                opportunity: None,
                events,
            });
        }

        if due_slot.is_some() {
            self.remove_due_slots(now);
        }
        if reason != TriggerReason::Scheduled
            && let Some(first) = self.state().slots.first().copied()
        {
            self.remove_slot(first);
        }
        self.persist()?;
        events.push(self.event(
            run_id,
            now,
            Outcome::Scheduled,
            reason,
            EventContext::NONE,
            "decision=bandwidth_start".to_owned(),
        ));
        Ok(SchedulerAction {
            opportunity: Some(BandwidthOpportunity {
                reason,
                scheduled_at_utc: scheduled_at,
                interface: pending_interface,
            }),
            events,
        })
    }

    pub fn preflight_manual(
        &mut self,
        run_id: &str,
        now: DateTime<Utc>,
    ) -> Result<ManualDecision, SchedulerError> {
        let mut clock_events = self.advance_clock(run_id, now)?;
        if let Some(event) = clock_events.pop()
            && now < self.state().last_observed_utc
        {
            return Ok(ManualDecision::Blocked(Box::new(event)));
        }
        match self.block_reason(now) {
            Some(blocked) => Ok(ManualDecision::Blocked(Box::new(self.event(
                run_id,
                now,
                Outcome::Suppressed,
                TriggerReason::Manual,
                EventContext::new(Some(blocked.kind), blocked.cooldown_until),
                blocked.message,
            )))),
            None => Ok(ManualDecision::Allowed),
        }
    }

    pub fn reserve_run(&mut self, now: DateTime<Utc>) -> Result<Reservation, SchedulerError> {
        self.reconcile_day(now);
        if let Some(blocked) = self.block_reason(now) {
            return Err(SchedulerError::Admission(format!(
                "reservation rejected after preflight: {}",
                blocked.message
            )));
        }
        append_reservation(&self.path, &self.policy.provider_id, now)?;
        let daily_runs_used = {
            let state = self.state_mut();
            state.runs.push(now);
            state.runs.sort_unstable();
            state.last_started_at_utc = Some(now);
            state.last_observed_utc = state.last_observed_utc.max(now);
            runs_on_day(state, now.date_naive())
        };
        self.persist()?;
        Ok(Reservation { daily_runs_used })
    }

    pub fn finish_attempt(
        &mut self,
        run_id: &str,
        now: DateTime<Utc>,
        opportunity: BandwidthOpportunity,
        report: &mut BandwidthReport,
    ) -> Result<Vec<MeasurementEvent>, SchedulerError> {
        let mut events = Vec::new();
        let reserved = report.reserved;
        let rate_limit = rate_limit_from_report(report, self.policy.provider_kind);
        if let Some(rate_limit) = rate_limit {
            let cooldown = rate_limit
                .retry_after
                .unwrap_or_else(|| self.next_backoff());
            let deadline = add_duration(now, cooldown);
            let day_deadline = day_end(now.date_naive());
            let deadline = deadline.min(day_deadline);
            {
                let state = self.state_mut();
                state.cooldown_until_utc = Some(deadline);
                if rate_limit.retry_after.is_some() {
                    state.backoff_step = state.backoff_step.saturating_add(1).min(4);
                }
                if !reserved {
                    let attempts = state
                        .deferred
                        .as_ref()
                        .map_or(1, |deferred| deferred.rate_limit_attempts.saturating_add(1));
                    state.deferred = (attempts < MAX_RATE_LIMIT_ATTEMPTS
                        && deadline < day_deadline)
                        .then_some(DeferredOpportunity {
                            reason: opportunity.reason,
                            interface: opportunity.interface.clone(),
                            created_at_utc: opportunity.scheduled_at_utc,
                            expires_at_utc: day_deadline,
                            rate_limit_attempts: attempts,
                        });
                } else {
                    state.deferred = None;
                }
            }
            self.interface_trigger_mut(opportunity.interface.as_deref())
                .pending = None;
            self.replan_remaining(now);
            self.persist()?;
            events.push(self.event(
                run_id,
                now,
                if reserved {
                    Outcome::Suppressed
                } else {
                    Outcome::Deferred
                },
                opportunity.reason,
                EventContext::new(Some(ErrorKind::ProviderCooldown), Some(deadline))
                    .with_interface(opportunity.interface.as_deref()),
                format!(
                    "decision=rate_limit stage={} status={} retry_after={} reserved={} deferred_attempts={}",
                    stage_text(rate_limit.stage),
                    rate_limit
                        .status
                        .map_or_else(|| "none".to_owned(), |value| value.to_string()),
                    rate_limit
                        .retry_after
                        .map_or_else(|| "fallback".to_owned(), |value| format!("{}ms", value.as_millis())),
                    reserved,
                    self.state()
                        .deferred
                        .as_ref()
                        .map_or(0, |deferred| deferred.rate_limit_attempts)
                ),
            ));
        } else {
            let successful_request = report.events.iter().any(|event| {
                event.event_kind == EventKind::Bandwidth
                    && (event.remote_ip.is_some()
                        || matches!(event.outcome, Outcome::Success | Outcome::Partial))
            });
            let state = self.state_mut();
            state.deferred = None;
            if successful_request {
                state.cooldown_until_utc = None;
                state.backoff_step = 0;
            }
            self.interface_trigger_mut(opportunity.interface.as_deref())
                .pending = None;
            if opportunity.reason != TriggerReason::Scheduled {
                self.replan_remaining(now);
            } else if reserved {
                let eligible = add_duration(now, self.policy.min_spacing);
                self.state_mut().slots.retain(|slot| *slot >= eligible);
            }
            self.persist()?;
        }

        let used = runs_on_day(self.state(), now.date_naive());
        for event in &mut report.events {
            event.trigger_reason = Some(opportunity.reason);
            event.scheduled_at_utc = Some(opportunity.scheduled_at_utc);
            event.daily_runs_used = Some(used);
        }
        Ok(events)
    }

    fn reconcile(&mut self, now: DateTime<Utc>, seed: u64) -> bool {
        if !self.store.providers.contains_key(&self.policy.provider_id) {
            let day = now.date_naive();
            let mut state = ProviderState {
                day_utc: day,
                slots: Vec::new(),
                runs: Vec::new(),
                last_started_at_utc: None,
                cooldown_until_utc: None,
                backoff_step: 0,
                deferred: None,
                interface_triggers: BTreeMap::new(),
                policy_daily_max: self.policy.daily_max,
                policy_min_spacing_ms: duration_millis(self.policy.min_spacing),
                policy_slot_jitter_pct: self.policy.slot_jitter_pct,
                rng_state: seed.max(1),
                last_observed_utc: now,
            };
            state.slots = plan_full_day(&mut state, &self.policy, now);
            self.store
                .providers
                .insert(self.policy.provider_id.clone(), state);
            return true;
        }
        let before = serde_json::to_vec(self.state()).expect("provider state serializes");
        self.reconcile_day(now);
        let policy_changed = self.state().policy_daily_max != self.policy.daily_max
            || self.state().policy_min_spacing_ms != duration_millis(self.policy.min_spacing)
            || self.state().policy_slot_jitter_pct != self.policy.slot_jitter_pct;
        if policy_changed {
            let daily_max = self.policy.daily_max;
            let min_spacing_ms = duration_millis(self.policy.min_spacing);
            let slot_jitter_pct = self.policy.slot_jitter_pct;
            let state = self.state_mut();
            state.policy_daily_max = daily_max;
            state.policy_min_spacing_ms = min_spacing_ms;
            state.policy_slot_jitter_pct = slot_jitter_pct;
            if daily_max == 0 {
                state.slots.clear();
                state.interface_triggers.clear();
                state.deferred = None;
            } else {
                self.replan_remaining(now);
            }
        }
        self.state_mut().slots.retain(|slot| *slot > now);
        if now >= self.state().last_observed_utc {
            self.state_mut().last_observed_utc = now;
        }
        let after = serde_json::to_vec(self.state()).expect("provider state serializes");
        before != after
    }

    fn advance_clock(
        &mut self,
        run_id: &str,
        now: DateTime<Utc>,
    ) -> Result<Vec<MeasurementEvent>, SchedulerError> {
        let previous = self.state().last_observed_utc;
        if now < previous {
            return Ok(vec![self.event(
                run_id,
                now,
                Outcome::Suppressed,
                TriggerReason::Scheduled,
                EventContext::new(Some(ErrorKind::Internal), None),
                format!(
                    "decision=clock_rollback previous={} current={}",
                    previous.to_rfc3339(),
                    now.to_rfc3339()
                ),
            )]);
        }
        self.reconcile_day(now);
        self.state_mut().last_observed_utc = now;
        self.persist()?;
        Ok(Vec::new())
    }

    fn reconcile_day(&mut self, now: DateTime<Utc>) {
        let day = now.date_naive();
        if day <= self.state().day_utc {
            return;
        }
        let policy = self.policy.clone();
        let state = self.state_mut();
        state.day_utc = day;
        state.runs.retain(|run| run.date_naive() >= day);
        state.deferred = None;
        state.interface_triggers.clear();
        state.cooldown_until_utc = state.cooldown_until_utc.filter(|deadline| *deadline > now);
        state.slots = plan_full_day(state, &policy, now);
    }

    fn replan_remaining(&mut self, now: DateTime<Utc>) {
        let policy = self.policy.clone();
        let state = self.state_mut();
        let used = runs_on_day(state, now.date_naive());
        let remaining = policy.daily_max.saturating_sub(used);
        state.slots = plan_interval(state, &policy, now, day_end(now.date_naive()), remaining);
    }

    fn next_backoff(&mut self) -> Duration {
        let policy = self.policy.clone();
        let state = self.state_mut();
        let multiplier = 1_u32 << state.backoff_step.min(4);
        let base = policy
            .cooldown_initial
            .checked_mul(multiplier)
            .unwrap_or(policy.cooldown_max)
            .min(policy.cooldown_max);
        state.backoff_step = state.backoff_step.saturating_add(1).min(4);
        jitter_duration(state, base)
    }

    fn merge_reservation_ledger(&mut self) -> Result<bool, SchedulerError> {
        let entries = load_reservations(&self.path)?;
        let provider_id = self.policy.provider_id.clone();
        let state = self.state_mut();
        let before = state.runs.len();
        for entry in entries
            .into_iter()
            .filter(|entry| entry.provider_id == provider_id)
        {
            if !state.runs.contains(&entry.started_at_utc) {
                state.runs.push(entry.started_at_utc);
            }
            state.last_started_at_utc = state.last_started_at_utc.max(Some(entry.started_at_utc));
        }
        state.runs.sort_unstable();
        Ok(state.runs.len() != before)
    }

    fn block_reason(&self, now: DateTime<Utc>) -> Option<BlockReason> {
        let state = self.state();
        if self.policy.daily_max == 0
            || runs_on_day(state, now.date_naive()) >= self.policy.daily_max
        {
            return Some(BlockReason {
                kind: ErrorKind::DailyCap,
                cooldown_until: None,
                message: format!(
                    "decision=suppressed reason=daily_cap used={} maximum={}",
                    runs_on_day(state, now.date_naive()),
                    self.policy.daily_max
                ),
            });
        }
        if let Some(deadline) = state.cooldown_until_utc
            && now < deadline
        {
            return Some(BlockReason {
                kind: ErrorKind::ProviderCooldown,
                cooldown_until: Some(deadline),
                message: format!(
                    "decision=deferred reason=provider_cooldown until={}",
                    deadline.to_rfc3339()
                ),
            });
        }
        if let Some(last) = state.last_started_at_utc {
            let eligible = add_duration(last, self.policy.min_spacing);
            if now < eligible {
                return Some(BlockReason {
                    kind: ErrorKind::ProviderCooldown,
                    cooldown_until: Some(eligible),
                    message: format!(
                        "decision=deferred reason=minimum_spacing until={}",
                        eligible.to_rfc3339()
                    ),
                });
            }
        }
        None
    }

    fn remove_slot(&mut self, slot: DateTime<Utc>) {
        self.state_mut()
            .slots
            .retain(|candidate| *candidate != slot);
    }

    fn remove_due_slots(&mut self, now: DateTime<Utc>) {
        self.state_mut().slots.retain(|candidate| *candidate > now);
    }

    fn interface_trigger(&self, interface: Option<&str>) -> InterfaceTriggerState {
        self.state()
            .interface_triggers
            .get(interface_key(interface))
            .cloned()
            .unwrap_or_default()
    }

    fn interface_trigger_mut(&mut self, interface: Option<&str>) -> &mut InterfaceTriggerState {
        self.state_mut()
            .interface_triggers
            .entry(interface_key(interface).to_owned())
            .or_default()
    }

    fn oldest_eligible_pending_trigger(
        &self,
        latest_success: &BTreeMap<String, bool>,
        allow_without_success: bool,
    ) -> Option<(String, PendingTrigger)> {
        self.state()
            .interface_triggers
            .iter()
            .filter_map(|(key, state)| {
                if !allow_without_success && !latest_success.get(key).copied().unwrap_or(false) {
                    return None;
                }
                state
                    .pending
                    .as_ref()
                    .cloned()
                    .map(|pending| (key.clone(), pending))
            })
            .min_by_key(|(_, pending)| pending.created_at_utc)
    }

    fn state(&self) -> &ProviderState {
        self.store
            .providers
            .get(&self.policy.provider_id)
            .expect("active provider state exists")
    }

    fn state_mut(&mut self) -> &mut ProviderState {
        self.store
            .providers
            .get_mut(&self.policy.provider_id)
            .expect("active provider state exists")
    }

    fn event(
        &mut self,
        run_id: &str,
        now: DateTime<Utc>,
        outcome: Outcome,
        reason: TriggerReason,
        context: EventContext,
        message: String,
    ) -> MeasurementEvent {
        let number = self.event_number;
        self.event_number = self.event_number.wrapping_add(1);
        let mut event = MeasurementEvent::new(
            run_id,
            format!("{run_id}:scheduler:{number}"),
            EventKind::Scheduler,
            outcome,
            now,
        );
        event.trigger_reason = Some(reason);
        event.provider_id = Some(self.policy.provider_id.clone());
        event.provider_kind = Some(self.policy.provider_kind);
        event.interface = context.interface;
        event.rate_limit_until_utc = context.cooldown_until;
        event.daily_runs_used = Some(runs_on_day(self.state(), now.date_naive()));
        event.error_kind = context.error_kind;
        event.error_message = Some(message);
        event
    }

    fn persist(&self) -> Result<(), SchedulerError> {
        persist_store(&self.path, &self.store)
    }
}

impl ReservationGate for Scheduler {
    fn reserve(&mut self, started_at: DateTime<Utc>) -> Result<AdmissionReservation, String> {
        self.reserve_run(started_at)
            .map(|reservation| AdmissionReservation::Reserved {
                daily_runs_used: reservation.daily_runs_used,
            })
            .map_err(|error| error.to_string())
    }
}

#[derive(Debug)]
struct BlockReason {
    kind: ErrorKind,
    cooldown_until: Option<DateTime<Utc>>,
    message: String,
}

#[derive(Debug, Clone)]
struct EventContext {
    error_kind: Option<ErrorKind>,
    cooldown_until: Option<DateTime<Utc>>,
    interface: Option<String>,
}

impl EventContext {
    const NONE: Self = Self {
        error_kind: None,
        cooldown_until: None,
        interface: None,
    };

    const fn new(error_kind: Option<ErrorKind>, cooldown_until: Option<DateTime<Utc>>) -> Self {
        Self {
            error_kind,
            cooldown_until,
            interface: None,
        }
    }

    fn for_interface(interface: Option<&str>) -> Self {
        Self::NONE.with_interface(interface)
    }

    fn with_interface(mut self, interface: Option<&str>) -> Self {
        self.interface = interface.map(str::to_owned);
        self
    }
}

fn interface_key(interface: Option<&str>) -> &str {
    interface.unwrap_or(DEFAULT_INTERFACE_KEY)
}

fn interface_from_key(key: &str) -> Option<&str> {
    (!key.is_empty()).then_some(key)
}

#[derive(Debug, Clone, Copy)]
struct RateLimit {
    stage: RequestStage,
    status: Option<u16>,
    retry_after: Option<Duration>,
}

fn rate_limit_from_report(
    report: &BandwidthReport,
    provider_kind: ProviderKind,
) -> Option<RateLimit> {
    report.events.iter().find_map(|event| {
        if event.event_kind != EventKind::RequestFailure {
            return None;
        }
        let status = event.http_status;
        let stage = event.request_stage?;
        let rate_limited = event.outcome == Outcome::RateLimited
            || status == Some(429)
            || (stage == RequestStage::Locate
                && provider_kind == ProviderKind::Mlab
                && matches!(status, Some(204 | 503)))
            || (stage == RequestStage::WebsocketHandshake
                && provider_kind == ProviderKind::Direct
                && status == Some(503));
        rate_limited.then_some(RateLimit {
            stage,
            status,
            retry_after: event.retry_after_ms.map(Duration::from_millis),
        })
    })
}

fn plan_full_day(
    state: &mut ProviderState,
    policy: &SchedulerPolicy,
    now: DateTime<Utc>,
) -> Vec<DateTime<Utc>> {
    let start = day_start(now.date_naive());
    plan_interval(
        state,
        policy,
        start,
        day_end(now.date_naive()),
        policy.daily_max,
    )
    .into_iter()
    .filter(|slot| *slot > now)
    .collect()
}

fn plan_interval(
    state: &mut ProviderState,
    policy: &SchedulerPolicy,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    count: u32,
) -> Vec<DateTime<Utc>> {
    if count == 0 || start >= end {
        return Vec::new();
    }
    let earliest = state
        .last_started_at_utc
        .map(|last| add_duration(last, policy.min_spacing))
        .unwrap_or(start)
        .max(state.cooldown_until_utc.unwrap_or(start))
        .max(start);
    if earliest >= end {
        return Vec::new();
    }
    let total_ms = (end - earliest).num_milliseconds().max(1);
    let mut slots = Vec::new();
    for index in 0..count {
        let stratum_start = total_ms * i64::from(index) / i64::from(count);
        let stratum_end = total_ms * i64::from(index + 1) / i64::from(count);
        let width = (stratum_end - stratum_start).max(1);
        let jitter_width = width * i64::from(policy.slot_jitter_pct) / 100;
        let middle_start = stratum_start + (width - jitter_width) / 2;
        let sampled = if jitter_width == 0 {
            middle_start
        } else {
            middle_start
                + i64::try_from(next_random(state) % u64::try_from(jitter_width).unwrap_or(1))
                    .unwrap_or(0)
        };
        let candidate = earliest + TimeDelta::milliseconds(sampled);
        if slots
            .last()
            .is_none_or(|previous| candidate >= add_duration(*previous, policy.min_spacing))
            && candidate < end
        {
            slots.push(candidate);
        }
    }
    slots
}

fn jitter_duration(state: &mut ProviderState, base: Duration) -> Duration {
    let millis = duration_millis(base);
    let spread = millis / 5;
    if spread == 0 {
        return base;
    }
    let offset = next_random(state) % (spread.saturating_mul(2).saturating_add(1));
    Duration::from_millis(millis.saturating_sub(spread).saturating_add(offset))
}

fn next_random(state: &mut ProviderState) -> u64 {
    let mut value = state.rng_state.max(1);
    value ^= value << 13;
    value ^= value >> 7;
    value ^= value << 17;
    state.rng_state = value;
    value
}

fn random_seed() -> u64 {
    let mut bytes = [0_u8; 8];
    if getrandom::fill(&mut bytes).is_ok() {
        return u64::from_le_bytes(bytes).max(1);
    }
    let fallback =
        Utc::now().timestamp_nanos_opt().unwrap_or_default() as u64 ^ u64::from(std::process::id());
    fallback.max(1)
}

fn runs_on_day(state: &ProviderState, day: NaiveDate) -> u32 {
    state
        .runs
        .iter()
        .filter(|run| run.date_naive() == day)
        .count()
        .try_into()
        .unwrap_or(u32::MAX)
}

fn day_start(day: NaiveDate) -> DateTime<Utc> {
    day.and_hms_opt(0, 0, 0)
        .expect("midnight is valid")
        .and_utc()
}

fn day_end(day: NaiveDate) -> DateTime<Utc> {
    day_start(day) + TimeDelta::seconds(DAY_SECONDS)
}

fn add_duration(value: DateTime<Utc>, duration: Duration) -> DateTime<Utc> {
    value
        .checked_add_signed(TimeDelta::from_std(duration).unwrap_or(TimeDelta::MAX))
        .unwrap_or(DateTime::<Utc>::MAX_UTC)
}

fn duration_millis(duration: Duration) -> u64 {
    duration.as_millis().try_into().unwrap_or(u64::MAX)
}

fn stage_text(stage: RequestStage) -> &'static str {
    match stage {
        RequestStage::Locate => "locate",
        RequestStage::Dns => "dns",
        RequestStage::Connect => "connect",
        RequestStage::Tls => "tls",
        RequestStage::WebsocketHandshake => "websocket_handshake",
        RequestStage::Download => "download",
        RequestStage::Upload => "upload",
    }
}

fn load_store(path: &Path) -> Result<SchedulerStore, SchedulerError> {
    if !path.exists() {
        let backup = backup_path(path);
        if backup.exists() {
            let bytes = fs::read(&backup).map_err(|source| SchedulerError::Io {
                path: backup.clone(),
                source,
            })?;
            return parse_store(path, &bytes);
        }
        if marker_path(path).exists() {
            return Err(SchedulerError::Corrupt {
                path: path.to_path_buf(),
                message: "initialized state is missing".to_owned(),
            });
        }
        return Ok(SchedulerStore::default());
    }
    let bytes = fs::read(path).map_err(|source| SchedulerError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    parse_store(path, &bytes)
}

fn parse_store(path: &Path, bytes: &[u8]) -> Result<SchedulerStore, SchedulerError> {
    serde_json::from_slice(bytes).map_err(|source| SchedulerError::Corrupt {
        path: path.to_path_buf(),
        message: source.to_string(),
    })
}

fn persist_store(path: &Path, store: &SchedulerStore) -> Result<(), SchedulerError> {
    let parent = path.parent().ok_or_else(|| SchedulerError::Corrupt {
        path: path.to_path_buf(),
        message: "state file has no parent".to_owned(),
    })?;
    fs::create_dir_all(parent).map_err(|source| SchedulerError::Io {
        path: parent.to_path_buf(),
        source,
    })?;
    let bytes = serde_json::to_vec_pretty(store).expect("scheduler state serializes");
    let temporary = temporary_path(path);
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temporary)
        .map_err(|source| SchedulerError::Io {
            path: temporary.clone(),
            source,
        })?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|source| SchedulerError::Io {
            path: temporary.clone(),
            source,
        })?;
    drop(file);

    let backup = backup_path(path);
    if path.exists() {
        let _ = fs::remove_file(&backup);
        fs::rename(path, &backup).map_err(|source| SchedulerError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    }
    if let Err(source) = fs::rename(&temporary, path) {
        let _ = fs::rename(&backup, path);
        return Err(SchedulerError::Io {
            path: path.to_path_buf(),
            source,
        });
    }
    let marker = marker_path(path);
    if !marker.exists() {
        File::create(&marker)
            .and_then(|file| file.sync_all())
            .map_err(|source| SchedulerError::Io {
                path: marker,
                source,
            })?;
    }
    Ok(())
}

fn append_reservation(
    state_path: &Path,
    provider_id: &str,
    started_at_utc: DateTime<Utc>,
) -> Result<(), SchedulerError> {
    let path = reservation_path(state_path);
    let entry = ReservationLedgerEntry {
        provider_id: provider_id.to_owned(),
        started_at_utc,
    };
    let mut bytes = serde_json::to_vec(&entry).expect("reservation entry serializes");
    bytes.push(b'\n');
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|source| SchedulerError::Io {
            path: path.clone(),
            source,
        })?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|source| SchedulerError::Io { path, source })
}

fn load_reservations(state_path: &Path) -> Result<Vec<ReservationLedgerEntry>, SchedulerError> {
    let path = reservation_path(state_path);
    let contents = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(SchedulerError::Io { path, source }),
    };
    contents
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(index, line)| {
            serde_json::from_str(line).map_err(|source| SchedulerError::Corrupt {
                path: path.clone(),
                message: format!("reservation ledger line {}: {source}", index + 1),
            })
        })
        .collect()
}

fn temporary_path(path: &Path) -> PathBuf {
    path.with_extension(format!("tmp.{}", std::process::id()))
}

fn backup_path(path: &Path) -> PathBuf {
    path.with_extension("bak")
}

fn marker_path(path: &Path) -> PathBuf {
    path.with_extension("initialized")
}

fn reservation_path(path: &Path) -> PathBuf {
    path.with_extension("reservations.jsonl")
}
