use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use chrono::{DateTime, TimeDelta, TimeZone, Utc};
use netband::bandwidth::BandwidthReport;
use netband::config::{
    BandwidthConfig, CooldownConfig, DirectConfig, MlabConfig, ProviderConfig, TriggerConfig,
};
use netband::health::{DegradationReason, HealthDecision, HealthSnapshot};
use netband::model::{EventKind, MeasurementEvent, Outcome, RequestStage, TriggerReason};
use netband::scheduler::{BandwidthOpportunity, ManualDecision, Scheduler, SchedulerError};
use tempfile::TempDir;
use url::Url;

fn at(day: u32, hour: u32, minute: u32, second: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, day, hour, minute, second)
        .single()
        .unwrap()
}

fn trigger() -> TriggerConfig {
    TriggerConfig {
        window_rounds: 6,
        min_samples: 6,
        loss_threshold_pct: 50.0,
        rtt_threshold_ms: None,
        recovery_loss_pct: 10.0,
        recovery_rounds: 3,
        pending_ttl: Duration::from_secs(30 * 60),
    }
}

fn mlab() -> BandwidthConfig {
    BandwidthConfig {
        provider: ProviderConfig::Mlab(MlabConfig {
            locate_url: Url::parse("https://locate.measurementlab.net/v2/nearest/ndt/ndt7")
                .unwrap(),
            policy_accepted: true,
        }),
        provider_id: "mlab".into(),
        automatic_enabled: true,
        force_limits: false,
        daily_max: 4,
        min_spacing: Duration::from_secs(36 * 60),
        slot_jitter_pct: 50,
        whole_test_timeout: Duration::from_secs(55),
        shutdown_margin: Duration::from_secs(15),
        trigger: trigger(),
        cooldown: CooldownConfig {
            initial: Duration::from_secs(60),
            max: Duration::from_secs(960),
        },
    }
}

fn direct(provider_id: &str, daily_max: u32, min_spacing: Duration) -> BandwidthConfig {
    BandwidthConfig {
        provider: ProviderConfig::Direct(DirectConfig {
            target: Some("ndt.example.net".into()),
            download_url: Url::parse("wss://ndt.example.net/ndt/v7/download").unwrap(),
            upload_url: Url::parse("wss://ndt.example.net/ndt/v7/upload").unwrap(),
            tls_server_name: None,
            ca_cert: None,
            allow_insecure: false,
        }),
        provider_id: provider_id.into(),
        automatic_enabled: daily_max > 0,
        force_limits: false,
        daily_max,
        min_spacing,
        slot_jitter_pct: 50,
        whole_test_timeout: Duration::from_secs(55),
        shutdown_margin: Duration::from_secs(15),
        trigger: trigger(),
        cooldown: CooldownConfig {
            initial: Duration::from_secs(60),
            max: Duration::from_secs(960),
        },
    }
}

fn degraded(reason: DegradationReason) -> HealthDecision {
    HealthDecision::Degraded {
        snapshot: HealthSnapshot {
            attempted: 6,
            successful: 3,
            loss_pct: 50.0,
            p95_rtt_ms: Some(80.0),
            sufficient_samples: true,
        },
        reason,
    }
}

fn recovered() -> HealthDecision {
    HealthDecision::Recovered(HealthSnapshot {
        attempted: 6,
        successful: 6,
        loss_pct: 0.0,
        p95_rtt_ms: Some(10.0),
        sufficient_samples: true,
    })
}

fn success_report(reserved: bool) -> BandwidthReport {
    let mut bandwidth = MeasurementEvent::new(
        "bandwidth",
        "bandwidth:result",
        EventKind::Bandwidth,
        Outcome::Success,
        at(30, 1, 0, 0),
    );
    bandwidth.remote_ip = Some("192.0.2.1".parse().unwrap());
    BandwidthReport {
        events: vec![bandwidth],
        outcome: Outcome::Success,
        reserved,
        reservation_error: None,
    }
}

fn rate_report(
    stage: RequestStage,
    status: u16,
    retry_after: Option<Duration>,
    reserved: bool,
) -> BandwidthReport {
    let mut failure = MeasurementEvent::new(
        "bandwidth",
        "bandwidth:request",
        EventKind::RequestFailure,
        if status == 204 {
            Outcome::NoCapacity
        } else {
            Outcome::RateLimited
        },
        at(30, 1, 0, 0),
    );
    failure.request_stage = Some(stage);
    failure.http_status = Some(status);
    failure.retry_after_ms = retry_after.map(|delay| delay.as_millis() as u64);
    BandwidthReport {
        events: vec![failure],
        outcome: Outcome::RateLimited,
        reserved,
        reservation_error: None,
    }
}

fn state_path(root: &TempDir) -> PathBuf {
    root.path().join("scheduler.json")
}

#[test]
fn seeded_slots_stay_in_middle_half_and_respect_cap_and_spacing() {
    let now = at(30, 0, 0, 0);
    let mut plans = Vec::new();
    for seed in 1..=64 {
        let root = TempDir::new().unwrap();
        let scheduler = Scheduler::open_seeded(state_path(&root), &mlab(), now, seed).unwrap();
        let slots = scheduler.snapshot().slots;
        assert_eq!(slots.len(), 4);
        for (index, slot) in slots.iter().enumerate() {
            let stratum = now + TimeDelta::hours(index as i64 * 6);
            assert!(*slot >= stratum + TimeDelta::minutes(90));
            assert!(*slot < stratum + TimeDelta::minutes(270));
        }
        assert!(
            slots
                .windows(2)
                .all(|pair| pair[1] - pair[0] >= TimeDelta::minutes(36))
        );
        plans.push(slots);
    }
    assert!(plans.windows(2).any(|pair| pair[0] != pair[1]));
}

#[test]
fn mlab_cap_is_global_and_survives_restart() {
    let root = TempDir::new().unwrap();
    let path = state_path(&root);
    let mut scheduler = Scheduler::open_seeded(&path, &mlab(), at(30, 0, 0, 0), 5).unwrap();
    for hour in 1..=4 {
        scheduler.reserve_run(at(30, hour, 0, 0)).unwrap();
    }
    assert!(matches!(
        scheduler.reserve_run(at(30, 5, 0, 0)),
        Err(SchedulerError::Admission(_))
    ));
    assert!(matches!(
        Scheduler::open_seeded(&path, &mlab(), at(30, 6, 0, 0), 6),
        Err(SchedulerError::Locked(_))
    ));
    drop(scheduler);
    let mut restarted = Scheduler::open_seeded(&path, &mlab(), at(30, 6, 0, 0), 6).unwrap();
    assert_eq!(restarted.snapshot().runs.len(), 4);
    assert!(matches!(
        restarted.preflight_manual("run", at(30, 6, 0, 0)).unwrap(),
        ManualDecision::Blocked(_)
    ));
}

#[test]
fn force_overrides_direct_limits_but_not_the_mlab_hard_cap() {
    let direct_root = TempDir::new().unwrap();
    let direct_path = state_path(&direct_root);
    let now = at(30, 1, 0, 0);
    let direct_config = direct("direct:forced", 1, Duration::from_secs(60));
    let mut scheduler = Scheduler::open_seeded(&direct_path, &direct_config, now, 81).unwrap();
    scheduler.reserve_run(now).unwrap();
    drop(scheduler);

    let mut forced_direct = direct_config;
    forced_direct.force_limits = true;
    let forced_at = now + TimeDelta::seconds(1);
    let mut scheduler =
        Scheduler::open_seeded(&direct_path, &forced_direct, forced_at, 82).unwrap();
    assert_eq!(
        scheduler.preflight_manual("forced", forced_at).unwrap(),
        ManualDecision::Allowed
    );
    assert_eq!(scheduler.reserve_run(forced_at).unwrap().daily_runs_used, 2);

    let mlab_root = TempDir::new().unwrap();
    let mlab_path = state_path(&mlab_root);
    let mut mlab_config = mlab();
    let mut scheduler =
        Scheduler::open_seeded(&mlab_path, &mlab_config, at(30, 0, 0, 0), 83).unwrap();
    for hour in 1..=4 {
        scheduler.reserve_run(at(30, hour, 0, 0)).unwrap();
    }
    drop(scheduler);

    mlab_config.force_limits = true;
    let forced_at = at(30, 5, 0, 0);
    let mut scheduler = Scheduler::open_seeded(&mlab_path, &mlab_config, forced_at, 84).unwrap();
    assert!(matches!(
        scheduler.preflight_manual("forced", forced_at).unwrap(),
        ManualDecision::Blocked(_)
    ));
    assert!(matches!(
        scheduler.reserve_run(forced_at),
        Err(SchedulerError::Admission(_))
    ));
}

#[test]
fn restart_retains_future_slots_and_discards_missed_slots_without_catch_up() {
    let root = TempDir::new().unwrap();
    let path = state_path(&root);
    let now = at(30, 0, 0, 0);
    let first = Scheduler::open_seeded(&path, &mlab(), now, 7)
        .unwrap()
        .snapshot();
    let restart_at = first.slots[1] + TimeDelta::seconds(1);
    let restarted = Scheduler::open_seeded(&path, &mlab(), restart_at, 999)
        .unwrap()
        .snapshot();
    assert_eq!(
        restarted.slots,
        first
            .slots
            .into_iter()
            .filter(|slot| *slot > restart_at)
            .collect::<Vec<_>>()
    );
}

#[test]
fn a_runtime_clock_jump_coalesces_all_missed_slots_without_a_burst() {
    let root = TempDir::new().unwrap();
    let now = at(30, 0, 0, 0);
    let mut scheduler = Scheduler::open_seeded(state_path(&root), &mlab(), now, 9).unwrap();
    let slots = scheduler.snapshot().slots;
    let jumped_to = slots[2] + TimeDelta::seconds(1);
    assert!(
        scheduler
            .poll("run", jumped_to, true)
            .unwrap()
            .opportunity
            .is_some()
    );
    assert!(
        scheduler
            .snapshot()
            .slots
            .iter()
            .all(|slot| *slot > jumped_to)
    );
    assert!(
        scheduler
            .poll("run", jumped_to, true)
            .unwrap()
            .opportunity
            .is_none()
    );
}

#[test]
fn utc_rollover_keeps_cross_midnight_spacing_and_clock_rollback_fails_closed() {
    let root = TempDir::new().unwrap();
    let path = state_path(&root);
    let mut scheduler = Scheduler::open_seeded(&path, &mlab(), at(30, 23, 50, 0), 3).unwrap();
    scheduler.reserve_run(at(30, 23, 50, 0)).unwrap();
    drop(scheduler);

    let next_day = at(31, 0, 5, 0);
    let mut restarted = Scheduler::open_seeded(&path, &mlab(), next_day, 4).unwrap();
    let snapshot = restarted.snapshot();
    assert!(snapshot.runs.is_empty());
    assert!(snapshot.slots.iter().all(|slot| *slot >= at(31, 0, 26, 0)));

    let rollback = restarted.poll("run", at(30, 23, 59, 0), true).unwrap();
    assert!(rollback.opportunity.is_none());
    assert!(rollback.events.iter().any(|event| {
        event
            .error_message
            .as_deref()
            .is_some_and(|message| message.contains("clock_rollback"))
    }));
}

#[test]
fn one_degradation_episode_triggers_once_replans_and_rearms_after_recovery() {
    let root = TempDir::new().unwrap();
    let path = state_path(&root);
    let now = at(30, 1, 0, 0);
    let mut scheduler = Scheduler::open_seeded(&path, &mlab(), now, 17).unwrap();
    let original_slots = scheduler.snapshot().slots;

    let events = scheduler
        .observe_health("run", now, degraded(DegradationReason::Loss))
        .unwrap();
    assert_eq!(events.len(), 1);
    let action = scheduler.poll("run", now, true).unwrap();
    let opportunity = action.opportunity.unwrap();
    assert_eq!(opportunity.reason, TriggerReason::PingLoss);
    scheduler.reserve_run(now).unwrap();
    let mut report = success_report(true);
    scheduler
        .finish_attempt("run", now, opportunity, &mut report)
        .unwrap();
    let after = scheduler.snapshot();
    assert_eq!(after.runs.len(), 1);
    assert!(after.slots.len() <= 3);
    assert_ne!(after.slots, original_slots);
    assert!(after.trigger_latched);
    assert!(after.pending_trigger.is_none());

    scheduler
        .observe_health("run", now, degraded(DegradationReason::Loss))
        .unwrap();
    assert!(scheduler.snapshot().pending_trigger.is_none());
    scheduler.observe_health("run", now, recovered()).unwrap();
    assert!(!scheduler.snapshot().trigger_latched);
    scheduler
        .observe_health("run", now, degraded(DegradationReason::Rtt))
        .unwrap();
    assert_eq!(
        scheduler.snapshot().pending_trigger,
        Some(TriggerReason::PingRtt)
    );
}

#[test]
fn interface_trigger_latches_are_independent_and_keep_origin_attribution() {
    let root = TempDir::new().unwrap();
    let now = at(30, 1, 0, 0);
    let mut scheduler = Scheduler::open_seeded(state_path(&root), &mlab(), now, 19).unwrap();
    scheduler
        .observe_interface_health("run", now, Some("eth-a"), degraded(DegradationReason::Loss))
        .unwrap();
    scheduler
        .observe_interface_health(
            "run",
            now + TimeDelta::seconds(1),
            Some("eth-b"),
            degraded(DegradationReason::Rtt),
        )
        .unwrap();
    let health = BTreeMap::from([("eth-a".to_owned(), true), ("eth-b".to_owned(), true)]);

    let first = scheduler
        .poll_interfaces("run", now + TimeDelta::seconds(2), &health)
        .unwrap()
        .opportunity
        .unwrap();
    assert_eq!(first.interface.as_deref(), Some("eth-a"));
    assert_eq!(first.reason, TriggerReason::PingLoss);
    let mut report = success_report(false);
    scheduler
        .finish_attempt("run", now + TimeDelta::seconds(2), first, &mut report)
        .unwrap();

    let second = scheduler
        .poll_interfaces("run", now + TimeDelta::seconds(3), &health)
        .unwrap()
        .opportunity
        .unwrap();
    assert_eq!(second.interface.as_deref(), Some("eth-b"));
    assert_eq!(second.reason, TriggerReason::PingRtt);
}

#[test]
fn outage_on_one_interface_does_not_block_an_eligible_interface_trigger() {
    let root = TempDir::new().unwrap();
    let now = at(30, 1, 0, 0);
    let mut scheduler = Scheduler::open_seeded(state_path(&root), &mlab(), now, 23).unwrap();
    for (offset, interface) in [(0, "eth-down"), (1, "eth-ready")] {
        scheduler
            .observe_interface_health(
                "run",
                now + TimeDelta::seconds(offset),
                Some(interface),
                degraded(DegradationReason::Loss),
            )
            .unwrap();
    }
    let health = BTreeMap::from([
        ("eth-down".to_owned(), false),
        ("eth-ready".to_owned(), true),
    ]);
    let opportunity = scheduler
        .poll_interfaces("run", now + TimeDelta::seconds(2), &health)
        .unwrap()
        .opportunity
        .unwrap();
    assert_eq!(opportunity.interface.as_deref(), Some("eth-ready"));
}

#[test]
fn expired_trigger_stays_latched_until_recovery_then_rearms() {
    let root = TempDir::new().unwrap();
    let mut scheduler =
        Scheduler::open_seeded(state_path(&root), &mlab(), at(30, 1, 0, 0), 21).unwrap();
    scheduler
        .observe_health("run", at(30, 1, 0, 0), degraded(DegradationReason::Loss))
        .unwrap();
    assert!(
        scheduler
            .poll("run", at(30, 1, 10, 0), false)
            .unwrap()
            .opportunity
            .is_none()
    );
    let expired = scheduler.poll("run", at(30, 1, 30, 1), false).unwrap();
    assert!(expired.opportunity.is_none());
    assert!(expired.events.iter().any(|event| {
        event.outcome == Outcome::Expired
            && event.error_message.as_deref()
                == Some("decision=trigger_expired reason=ttl latch=retained rearm=health_recovery")
    }));
    let after_expiry = scheduler.snapshot();
    assert!(after_expiry.trigger_latched);
    assert!(after_expiry.pending_trigger.is_none());

    let repeated_degradation = scheduler
        .observe_health("run", at(30, 1, 31, 0), degraded(DegradationReason::Loss))
        .unwrap();
    assert!(repeated_degradation.is_empty());
    assert!(scheduler.snapshot().pending_trigger.is_none());

    scheduler
        .observe_health("run", at(30, 1, 32, 0), recovered())
        .unwrap();
    assert!(!scheduler.snapshot().trigger_latched);
    scheduler
        .observe_health("run", at(30, 1, 33, 0), degraded(DegradationReason::Rtt))
        .unwrap();
    assert_eq!(
        scheduler.snapshot().pending_trigger,
        Some(TriggerReason::PingRtt)
    );
}

#[test]
fn locate_rate_limits_persist_cooldown_defer_once_and_use_bounded_backoff() {
    let root = TempDir::new().unwrap();
    let path = state_path(&root);
    let now = at(30, 1, 0, 0);
    let mut scheduler = Scheduler::open_seeded(&path, &mlab(), now, 31).unwrap();
    let opportunity = BandwidthOpportunity {
        reason: TriggerReason::Scheduled,
        scheduled_at_utc: now,
        interface: None,
    };
    let mut report = rate_report(RequestStage::Locate, 429, None, false);
    let events = scheduler
        .finish_attempt("run", now, opportunity, &mut report)
        .unwrap();
    let first = scheduler.snapshot();
    let deadline = first.cooldown_until_utc.unwrap();
    assert!(deadline >= now + TimeDelta::seconds(48));
    assert!(deadline <= now + TimeDelta::seconds(72));
    assert_eq!(first.deferred_attempts, Some(1));
    assert!(
        events[0]
            .error_message
            .as_deref()
            .unwrap()
            .contains("status=429")
    );

    drop(scheduler);
    let mut restarted = Scheduler::open_seeded(&path, &mlab(), now, 999).unwrap();
    assert_eq!(restarted.snapshot().cooldown_until_utc, Some(deadline));
    assert!(
        restarted
            .poll("run", deadline - TimeDelta::seconds(1), true)
            .unwrap()
            .opportunity
            .is_none()
    );
    let retry = restarted.poll("run", deadline, true).unwrap();
    assert!(retry.opportunity.is_some());
    let mut second = rate_report(RequestStage::Locate, 204, None, false);
    restarted
        .finish_attempt("run", deadline, retry.opportunity.unwrap(), &mut second)
        .unwrap();
    let second_delay = restarted.snapshot().cooldown_until_utc.unwrap() - deadline;
    assert!(second_delay >= TimeDelta::seconds(96));
    assert!(second_delay <= TimeDelta::seconds(144));
    assert_eq!(restarted.snapshot().deferred_attempts, Some(2));
}

#[test]
fn retry_after_is_exact_and_post_reservation_limit_is_not_retried() {
    let root = TempDir::new().unwrap();
    let now = at(30, 1, 0, 0);
    let mut scheduler = Scheduler::open_seeded(state_path(&root), &mlab(), now, 37).unwrap();
    scheduler.reserve_run(now).unwrap();
    let opportunity = BandwidthOpportunity {
        reason: TriggerReason::PingLoss,
        scheduled_at_utc: now,
        interface: None,
    };
    let mut report = rate_report(
        RequestStage::WebsocketHandshake,
        429,
        Some(Duration::from_secs(300)),
        true,
    );
    scheduler
        .finish_attempt("run", now, opportunity, &mut report)
        .unwrap();
    let snapshot = scheduler.snapshot();
    assert_eq!(
        snapshot.cooldown_until_utc,
        Some(now + TimeDelta::minutes(5))
    );
    assert_eq!(snapshot.runs.len(), 1);
    assert_eq!(snapshot.deferred_attempts, None);
}

#[test]
fn provider_state_is_independent_and_policy_changes_preserve_used_runs() {
    let root = TempDir::new().unwrap();
    let path = state_path(&root);
    let now = at(30, 1, 0, 0);
    let mut mlab_scheduler = Scheduler::open_seeded(&path, &mlab(), now, 41).unwrap();
    mlab_scheduler.reserve_run(now).unwrap();
    drop(mlab_scheduler);

    let direct_config = direct("direct:canonical", 8, Duration::from_secs(90 * 60));
    let mut direct_scheduler = Scheduler::open_seeded(&path, &direct_config, now, 43).unwrap();
    assert!(direct_scheduler.snapshot().runs.is_empty());
    direct_scheduler.reserve_run(now).unwrap();
    drop(direct_scheduler);

    let mlab_runs = Scheduler::open_seeded(&path, &mlab(), now, 99)
        .unwrap()
        .snapshot()
        .runs
        .len();
    assert_eq!(mlab_runs, 1);
    let lowered = direct("direct:canonical", 1, Duration::from_secs(2 * 60 * 60));
    let lowered = Scheduler::open_seeded(&path, &lowered, now, 100).unwrap();
    assert_eq!(lowered.snapshot().runs.len(), 1);
    assert!(lowered.snapshot().slots.is_empty());
    drop(lowered);

    let disabled = direct("direct:canonical", 0, Duration::from_secs(2 * 60 * 60));
    let mut disabled = Scheduler::open_seeded(&path, &disabled, now, 101).unwrap();
    assert!(disabled.snapshot().slots.is_empty());
    assert!(matches!(
        disabled.preflight_manual("run", now).unwrap(),
        ManualDecision::Blocked(_)
    ));
}

#[test]
fn corrupt_initialized_state_fails_closed() {
    let root = TempDir::new().unwrap();
    let path = state_path(&root);
    Scheduler::open_seeded(&path, &mlab(), at(30, 0, 0, 0), 47).unwrap();
    std::fs::write(&path, b"not json").unwrap();
    assert!(matches!(
        Scheduler::open_seeded(&path, &mlab(), at(30, 1, 0, 0), 48),
        Err(SchedulerError::Corrupt { .. })
    ));
}

#[test]
fn reservation_ledger_recovers_a_run_from_an_interrupted_state_replace() {
    let root = TempDir::new().unwrap();
    let path = state_path(&root);
    let now = at(30, 1, 0, 0);
    let mut scheduler = Scheduler::open_seeded(&path, &mlab(), now, 61).unwrap();
    scheduler.reserve_run(now).unwrap();

    std::fs::remove_file(&path).unwrap();
    drop(scheduler);
    assert!(matches!(
        Scheduler::open_seeded(&path, &mlab(), now, 62),
        Err(SchedulerError::Corrupt { .. })
    ));
    std::fs::copy(path.with_extension("bak"), &path).unwrap();
    let recovered = Scheduler::open_seeded(&path, &mlab(), now, 62)
        .unwrap()
        .snapshot();
    assert_eq!(recovered.runs, vec![now]);
}

#[test]
fn health_trigger_during_cooldown_merges_into_the_single_deferred_retry() {
    let root = TempDir::new().unwrap();
    let now = at(30, 1, 0, 0);
    let mut scheduler = Scheduler::open_seeded(state_path(&root), &mlab(), now, 67).unwrap();
    let scheduled = BandwidthOpportunity {
        reason: TriggerReason::Scheduled,
        scheduled_at_utc: now,
        interface: None,
    };
    let mut limited = rate_report(RequestStage::Locate, 429, None, false);
    scheduler
        .finish_attempt("run", now, scheduled, &mut limited)
        .unwrap();
    let deadline = scheduler.snapshot().cooldown_until_utc.unwrap();

    let events = scheduler
        .observe_health(
            "run",
            now + TimeDelta::seconds(1),
            degraded(DegradationReason::Loss),
        )
        .unwrap();
    assert!(
        events[0]
            .error_message
            .as_deref()
            .unwrap()
            .contains("trigger_merged_with_deferred")
    );
    assert!(scheduler.snapshot().pending_trigger.is_none());
    assert!(
        scheduler
            .poll("run", deadline - TimeDelta::seconds(1), true)
            .unwrap()
            .opportunity
            .is_none()
    );
    let retry = scheduler.poll("run", deadline, true).unwrap();
    assert_eq!(retry.opportunity.unwrap().reason, TriggerReason::PingLoss);
}

#[test]
fn five_consecutive_discovery_limits_expire_the_deferred_opportunity() {
    let root = TempDir::new().unwrap();
    let mut now = at(30, 1, 0, 0);
    let mut scheduler = Scheduler::open_seeded(state_path(&root), &mlab(), now, 71).unwrap();
    let mut opportunity = BandwidthOpportunity {
        reason: TriggerReason::Scheduled,
        scheduled_at_utc: now,
        interface: None,
    };
    for attempt in 1..=5 {
        let mut limited = rate_report(RequestStage::Locate, 503, None, false);
        scheduler
            .finish_attempt("run", now, opportunity.clone(), &mut limited)
            .unwrap();
        if attempt < 5 {
            assert_eq!(scheduler.snapshot().deferred_attempts, Some(attempt));
            now = scheduler.snapshot().cooldown_until_utc.unwrap();
            opportunity = scheduler
                .poll("run", now, true)
                .unwrap()
                .opportunity
                .unwrap();
        }
    }
    assert_eq!(scheduler.snapshot().deferred_attempts, None);
    assert!(
        scheduler
            .poll(
                "run",
                scheduler.snapshot().cooldown_until_utc.unwrap(),
                true
            )
            .unwrap()
            .opportunity
            .is_none()
    );
}

#[test]
fn deterministic_multi_day_simulation_never_exceeds_provider_caps() {
    let root = TempDir::new().unwrap();
    let path = state_path(&root);
    let config = direct("direct:simulation", 12, Duration::from_secs(70 * 60));
    for day in 28..=31 {
        let start = at(day, 0, 0, 0);
        let mut scheduler = Scheduler::open_seeded(&path, &config, start, 53).unwrap();
        while let Some(slot) = scheduler.snapshot().slots.first().copied() {
            let action = scheduler.poll("simulation", slot, true).unwrap();
            let Some(opportunity) = action.opportunity else {
                break;
            };
            scheduler.reserve_run(slot).unwrap();
            let mut report = success_report(true);
            scheduler
                .finish_attempt("simulation", slot, opportunity, &mut report)
                .unwrap();
        }
        let snapshot = scheduler.snapshot();
        assert!(snapshot.runs.len() <= 12);
        assert!(
            snapshot
                .runs
                .windows(2)
                .all(|pair| { pair[1] - pair[0] >= TimeDelta::minutes(70) })
        );
    }
}
