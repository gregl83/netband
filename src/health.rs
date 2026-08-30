use std::collections::VecDeque;

use crate::config::TriggerConfig;
use crate::model::{EventKind, MeasurementEvent, Outcome};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HealthSample {
    pub successful: bool,
    pub rtt_ms: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HealthSnapshot {
    pub attempted: usize,
    pub successful: usize,
    pub loss_pct: f64,
    pub p95_rtt_ms: Option<f64>,
    pub sufficient_samples: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DegradationReason {
    Loss,
    Rtt,
    LossAndRtt,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HealthDecision {
    Healthy(HealthSnapshot),
    Degraded {
        snapshot: HealthSnapshot,
        reason: DegradationReason,
    },
    Recovered(HealthSnapshot),
}

impl HealthDecision {
    pub const fn snapshot(self) -> HealthSnapshot {
        match self {
            Self::Healthy(snapshot) | Self::Recovered(snapshot) => snapshot,
            Self::Degraded { snapshot, .. } => snapshot,
        }
    }
}

#[derive(Debug, Clone)]
pub struct HealthWindow {
    config: HealthConfig,
    rounds: VecDeque<Vec<HealthSample>>,
    degraded: bool,
    degradation_reason: Option<DegradationReason>,
    recovery_streak: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct HealthConfig {
    pub window_rounds: usize,
    pub min_samples: usize,
    pub loss_threshold_pct: f64,
    pub rtt_threshold_ms: Option<f64>,
    pub recovery_loss_pct: f64,
    pub recovery_rounds: u32,
}

impl From<&TriggerConfig> for HealthConfig {
    fn from(config: &TriggerConfig) -> Self {
        Self {
            window_rounds: config.window_rounds as usize,
            min_samples: config.min_samples as usize,
            loss_threshold_pct: config.loss_threshold_pct,
            rtt_threshold_ms: config.rtt_threshold_ms,
            recovery_loss_pct: config.recovery_loss_pct,
            recovery_rounds: config.recovery_rounds,
        }
    }
}

impl HealthWindow {
    pub fn new(config: HealthConfig) -> Self {
        Self {
            config,
            rounds: VecDeque::with_capacity(config.window_rounds),
            degraded: false,
            degradation_reason: None,
            recovery_streak: 0,
        }
    }

    pub fn observe_events(&mut self, events: &[MeasurementEvent]) -> HealthDecision {
        let samples = events
            .iter()
            .filter(|event| event.event_kind == EventKind::PingProbe)
            .map(|event| HealthSample {
                successful: event.outcome == Outcome::Success,
                rtt_ms: (event.outcome == Outcome::Success)
                    .then_some(event.rtt_ms)
                    .flatten(),
            })
            .collect();
        self.observe_round(samples)
    }

    pub fn observe_round(&mut self, samples: Vec<HealthSample>) -> HealthDecision {
        let round_snapshot = snapshot(&samples, 0);
        self.rounds.push_back(samples);
        while self.rounds.len() > self.config.window_rounds {
            self.rounds.pop_front();
        }

        let window_samples = self.rounds.iter().flatten().copied().collect::<Vec<_>>();
        let window_snapshot = snapshot(&window_samples, self.config.min_samples);

        if !self.degraded {
            if let Some(reason) = degradation_reason(window_snapshot, self.config) {
                self.degraded = true;
                self.degradation_reason = Some(reason);
                self.recovery_streak = 0;
                return HealthDecision::Degraded {
                    snapshot: window_snapshot,
                    reason,
                };
            }
            return HealthDecision::Healthy(window_snapshot);
        }

        if recovery_round(round_snapshot, self.config) {
            self.recovery_streak += 1;
        } else {
            self.recovery_streak = 0;
        }

        if self.recovery_streak >= self.config.recovery_rounds {
            self.degraded = false;
            self.degradation_reason = None;
            self.recovery_streak = 0;
            HealthDecision::Recovered(window_snapshot)
        } else {
            HealthDecision::Degraded {
                snapshot: window_snapshot,
                reason: self
                    .degradation_reason
                    .expect("a degraded window retains its reason"),
            }
        }
    }

    pub fn rounds_retained(&self) -> usize {
        self.rounds.len()
    }
}

fn snapshot(samples: &[HealthSample], min_samples: usize) -> HealthSnapshot {
    let attempted = samples.len();
    let successful = samples.iter().filter(|sample| sample.successful).count();
    let loss_pct = if attempted == 0 {
        0.0
    } else {
        (attempted - successful) as f64 * 100.0 / attempted as f64
    };
    let mut rtts = samples
        .iter()
        .filter(|sample| sample.successful)
        .filter_map(|sample| sample.rtt_ms)
        .collect::<Vec<_>>();
    rtts.sort_by(f64::total_cmp);
    let p95_rtt_ms = if rtts.is_empty() {
        None
    } else {
        let rank = ((rtts.len() as f64 * 0.95).ceil() as usize).max(1);
        Some(rtts[rank - 1])
    };
    HealthSnapshot {
        attempted,
        successful,
        loss_pct,
        p95_rtt_ms,
        sufficient_samples: attempted >= min_samples,
    }
}

fn degradation_reason(snapshot: HealthSnapshot, config: HealthConfig) -> Option<DegradationReason> {
    if !snapshot.sufficient_samples {
        return None;
    }
    let loss = snapshot.loss_pct >= config.loss_threshold_pct;
    let rtt = config
        .rtt_threshold_ms
        .zip(snapshot.p95_rtt_ms)
        .is_some_and(|(threshold, p95)| p95 >= threshold);
    match (loss, rtt) {
        (true, true) => Some(DegradationReason::LossAndRtt),
        (true, false) => Some(DegradationReason::Loss),
        (false, true) => Some(DegradationReason::Rtt),
        (false, false) => None,
    }
}

fn recovery_round(snapshot: HealthSnapshot, config: HealthConfig) -> bool {
    if snapshot.attempted == 0 || snapshot.loss_pct >= config.recovery_loss_pct {
        return false;
    }
    match config.rtt_threshold_ms {
        Some(threshold) => snapshot.p95_rtt_ms.is_some_and(|p95| p95 < threshold),
        None => true,
    }
}
