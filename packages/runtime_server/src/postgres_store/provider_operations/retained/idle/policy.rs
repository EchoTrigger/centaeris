//! A continuous-idle grace period measured only from verified observations.
use std::collections::BTreeMap;
use std::time::Duration;

const MAX_IDLE_OBSERVATIONS: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IdleEnvironmentVersion {
    pub environment_id: String,
    pub controller_epoch: i64,
    pub predecessor_operation_id: Option<String>,
}

struct Observation {
    version: IdleEnvironmentVersion,
    since_ms: i64,
    last_seen_ms: i64,
    seen: bool,
}

/// No policy instance means disabled. Restart deliberately starts a fresh grace.
pub(crate) struct IdlePausePolicy {
    timeout_ms: i64,
    observations: BTreeMap<String, Observation>,
}

impl IdlePausePolicy {
    pub(crate) fn new(timeout: Duration) -> Result<Self, String> {
        let timeout_ms =
            i64::try_from(timeout.as_millis()).map_err(|_| "provider_idle_timeout_invalid")?;
        if timeout_ms == 0 {
            return Err("provider_idle_timeout_invalid".into());
        }
        Ok(Self {
            timeout_ms,
            observations: BTreeMap::new(),
        })
    }

    pub(crate) fn observe(
        &mut self,
        version: IdleEnvironmentVersion,
        now_ms: i64,
    ) -> Result<bool, String> {
        if now_ms < 0 || version.controller_epoch < 0 || version.environment_id.is_empty() {
            return Err("provider_idle_observation_invalid".into());
        }
        if let Some(observed) = self.observations.get_mut(&version.environment_id) {
            if observed.version != version || now_ms < observed.last_seen_ms {
                observed.version = version;
                observed.since_ms = now_ms;
            }
            observed.last_seen_ms = now_ms;
            observed.seen = true;
            return Ok(now_ms - observed.since_ms >= self.timeout_ms);
        }
        if self.observations.len() < MAX_IDLE_OBSERVATIONS {
            self.observations.insert(
                version.environment_id.clone(),
                Observation {
                    version,
                    since_ms: now_ms,
                    last_seen_ms: now_ms,
                    seen: true,
                },
            );
        }
        Ok(false)
    }

    pub(crate) fn forget(&mut self, environment_id: &str) {
        self.observations.remove(environment_id);
    }

    /// Called after every complete bounded, cursor-paginated scan.
    pub(crate) fn finish_scan(&mut self) {
        self.observations.retain(|_, value| {
            let seen = value.seen;
            value.seen = false;
            seen
        });
    }
}

#[cfg(test)]
#[path = "policy_tests.rs"]
mod tests;
