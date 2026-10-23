//! Monotonic, opt-in stage boundaries; timing never changes operation authority.
use crate::observations::power::{PowerMetric, PowerObservation, PowerOutcome};
use std::time::Instant;

pub(super) struct PowerStage<'a> {
    observation: &'a PowerObservation,
    metric: PowerMetric,
    started: Option<Instant>,
    outcome: PowerOutcome,
}

impl<'a> PowerStage<'a> {
    pub(super) fn new(observation: &'a PowerObservation, metric: PowerMetric) -> Self {
        Self {
            observation,
            metric,
            started: observation.is_enabled().then(Instant::now),
            outcome: PowerOutcome::Error,
        }
    }

    pub(super) fn finish(mut self, outcome: PowerOutcome) {
        self.outcome = outcome;
    }
}

impl Drop for PowerStage<'_> {
    fn drop(&mut self) {
        if let Some(started) = self.started {
            self.observation
                .elapsed(self.metric, started.elapsed(), self.outcome);
        }
    }
}
