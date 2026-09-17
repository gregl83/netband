mod support;
use netband::health::{
    DegradationReason, HealthConfig, HealthDecision, HealthSample, HealthWindow,
};

fn config() -> HealthConfig {
    HealthConfig {
        window_rounds: 3,
        min_samples: 4,
        loss_threshold_pct: 50.0,
        rtt_threshold_ms: None,
        recovery_loss_pct: 10.0,
        recovery_rounds: 2,
    }
}

fn ok(rtt_ms: f64) -> HealthSample {
    HealthSample {
        successful: true,
        rtt_ms: Some(rtt_ms),
    }
}

fn failed() -> HealthSample {
    HealthSample {
        successful: false,
        rtt_ms: Some(9999.0),
    }
}

#[test]
fn minimum_samples_and_loss_boundary_are_exact() {
    let mut window = HealthWindow::new(config());
    let decision = window.observe_round(vec![ok(1.0), failed()]);
    assert!(matches!(decision, HealthDecision::Healthy(snapshot) if !snapshot.sufficient_samples));

    let decision = window.observe_round(vec![ok(2.0), failed()]);
    assert!(matches!(
        decision,
        HealthDecision::Degraded {
            reason: DegradationReason::Loss,
            snapshot,
        } if snapshot.attempted == 4 && snapshot.loss_pct == 50.0
    ));
}

#[test]
fn old_rounds_are_evicted_and_failed_rtt_values_are_ignored() {
    let mut window = HealthWindow::new(HealthConfig {
        min_samples: 1,
        loss_threshold_pct: 90.0,
        ..config()
    });
    window.observe_round(vec![ok(10.0)]);
    window.observe_round(vec![ok(20.0)]);
    window.observe_round(vec![failed()]);
    let decision = window.observe_round(vec![ok(30.0)]);
    let snapshot = decision.snapshot();

    assert_eq!(window.rounds_retained(), 3);
    assert_eq!(snapshot.attempted, 3);
    assert_eq!(snapshot.successful, 2);
    assert_eq!(snapshot.p95_rtt_ms, Some(30.0));
}

#[test]
fn optional_p95_rtt_uses_nearest_rank_and_combines_reasons() {
    let mut window = HealthWindow::new(HealthConfig {
        min_samples: 20,
        loss_threshold_pct: 5.0,
        rtt_threshold_ms: Some(19.0),
        ..config()
    });
    let mut samples = (1..=19).map(|value| ok(value as f64)).collect::<Vec<_>>();
    samples.push(HealthSample {
        successful: true,
        rtt_ms: Some(100.0),
    });
    let decision = window.observe_round(samples);
    assert!(matches!(
        decision,
        HealthDecision::Degraded {
            reason: DegradationReason::Rtt,
            snapshot,
        } if snapshot.p95_rtt_ms == Some(19.0)
    ));

    let mut combined = HealthWindow::new(HealthConfig {
        min_samples: 2,
        rtt_threshold_ms: Some(10.0),
        ..config()
    });
    assert!(matches!(
        combined.observe_round(vec![ok(10.0), failed()]),
        HealthDecision::Degraded {
            reason: DegradationReason::LossAndRtt,
            ..
        }
    ));
}

#[test]
fn total_outage_degrades_and_recovery_requires_consecutive_rounds() {
    let mut window = HealthWindow::new(HealthConfig {
        min_samples: 2,
        ..config()
    });
    assert!(matches!(
        window.observe_round(vec![failed(), failed()]),
        HealthDecision::Degraded { .. }
    ));
    assert!(matches!(
        window.observe_round(vec![ok(5.0), ok(6.0)]),
        HealthDecision::Degraded { .. }
    ));
    assert!(matches!(
        window.observe_round(vec![ok(5.0), failed()]),
        HealthDecision::Degraded { .. }
    ));
    assert!(matches!(
        window.observe_round(vec![ok(5.0), ok(6.0)]),
        HealthDecision::Degraded { .. }
    ));
    assert!(matches!(
        window.observe_round(vec![ok(5.0), ok(6.0)]),
        HealthDecision::Recovered(_)
    ));
    assert!(matches!(
        window.observe_round(vec![ok(5.0), ok(6.0)]),
        HealthDecision::Healthy(_)
    ));
}

#[test]
fn journal_context_does_not_change_ping_health_or_recovery() {
    use netband::model::{EventKind, MeasurementEvent, Outcome};
    let mut plain = HealthWindow::new(config());
    let mut mixed = HealthWindow::new(config());
    for outcome in [
        Outcome::Timeout,
        Outcome::Success,
        Outcome::Success,
        Outcome::Success,
    ] {
        let mut probe = MeasurementEvent::new(
            support::id::<netband::model::RunId>("run"),
            EventKind::PingProbe,
            outcome,
            chrono::Utc::now(),
        );
        probe.ping_rtt_ms = Some(12.0);
        let mut events = vec![probe.clone()];
        for kind in [
            EventKind::RunStarted,
            EventKind::RunFinished,
            EventKind::Bandwidth,
            EventKind::RequestFailure,
            EventKind::Scheduler,
        ] {
            let mut context = MeasurementEvent::new(
                support::id::<netband::model::RunId>("run"),
                kind,
                Outcome::Error,
                chrono::Utc::now(),
            );
            context.ping_rtt_ms = Some(9999.0);
            events.push(context);
        }
        assert_eq!(
            plain.observe_events(&[probe]),
            mixed.observe_events(&events)
        );
        assert_eq!(plain.rounds_retained(), mixed.rounds_retained());
    }
}
