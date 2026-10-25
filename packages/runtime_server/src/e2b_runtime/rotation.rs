//! Opt-in automatic full pause and demand-driven restore of retained environments.
use super::*;
use crate::e2b_run_registry::E2bRunRegistry;
use crate::postgres_store::provider_operations::retained::{
    idle,
    power::{
        ProviderEnvironmentPowerAction as Action,
        ProviderEnvironmentPowerDispatchState as Dispatch, ProviderEnvironmentPowerRequest,
    },
};
use idle::policy::{IdleEnvironmentVersion, IdlePausePolicy};

mod lifecycle;
pub(crate) use lifecycle::RotationTask;

const SCAN_INTERVAL: Duration = Duration::from_millis(250);

struct RotationState {
    idle: IdlePausePolicy,
    cursor: Option<String>,
    last_attempted: Option<String>,
}

impl RotationState {
    fn new(timeout: Duration) -> Result<Self, String> {
        Ok(Self {
            idle: IdlePausePolicy::new(timeout)?,
            cursor: None,
            last_attempted: None,
        })
    }
    fn choose(
        &mut self,
        candidate: &idle::RotationCandidate,
        now_ms: i64,
        config: &str,
    ) -> Result<Option<ProviderEnvironmentPowerRequest>, String> {
        if let Some(previous) = &candidate.latest {
            if previous.dispatch_state == Dispatch::NotDispatched {
                self.idle.forget(&candidate.environment_id);
                return Ok(candidate
                    .has_demand
                    .then(|| idle::request(candidate, config, Action::Resume)));
            }
            if previous.acknowledgement.is_none() {
                self.idle.forget(&candidate.environment_id);
                // A committed dispatch with no ACK is supervised, never replayed.
                // A prepared automatic resume is revisited so cancellation can end it before any SDK send.
                return Ok((previous.dispatch_state == Dispatch::Prepared
                    && (previous.request.action == Action::Pause
                        || previous.automatic_demand.is_some()))
                .then(|| previous.request.clone()));
            }
            if previous.request.action == Action::Pause {
                self.idle.forget(&candidate.environment_id);
                return Ok(candidate
                    .has_demand
                    .then(|| idle::request(candidate, config, Action::Resume)));
            }
        }
        if candidate.has_demand {
            self.idle.forget(&candidate.environment_id);
            return Ok(None);
        }
        let ready = self.idle.observe(
            IdleEnvironmentVersion {
                environment_id: candidate.environment_id.clone(),
                controller_epoch: candidate.controller_epoch,
                predecessor_operation_id: candidate
                    .latest
                    .as_ref()
                    .map(|value| value.request.operation_id.clone()),
            },
            now_ms,
        )?;
        Ok(ready.then(|| idle::request(candidate, config, Action::Pause)))
    }
    fn choose_page(
        &mut self,
        candidates: &[idle::RotationCandidate],
        now_ms: i64,
        config: &str,
        resume_capacity_available: bool,
    ) -> Result<Option<ProviderEnvironmentPowerRequest>, String> {
        let mut choices = Vec::new();
        for candidate in candidates {
            if let Some(request) = self.choose(candidate, now_ms, config)? {
                let existing = candidate
                    .latest
                    .as_ref()
                    .is_some_and(|previous| previous.request.operation_id == request.operation_id);
                // This snapshot only suppresses new, certainly full-budget attempts.
                // A prepared reservation must still advance or observe cancellation.
                if request.action == Action::Resume && !existing && !resume_capacity_available {
                    continue;
                }
                choices.push(request);
            }
        }
        choices.sort_by_key(|request| {
            // Release mature idle residency before attempting a budgeted resume.
            // Rotate ties so one fenced/contended low-ID target cannot dominate.
            (
                request.action != Action::Pause,
                self.last_attempted
                    .as_ref()
                    .is_some_and(|id| request.environment_id <= *id),
                request.environment_id.clone(),
            )
        });
        let chosen = choices.into_iter().next();
        if let Some(request) = &chosen {
            self.last_attempted = Some(request.environment_id.clone());
        }
        Ok(chosen)
    }
    fn tick(&mut self, provider: &E2bRuntime, registry: &E2bRunRegistry) -> Result<(), String> {
        let digest = provider.config.profile.digest()?;
        let page = provider.store.provider_rotation_candidates(
            &provider.config.provider_config_digest,
            &digest,
            self.cursor.as_deref(),
            provider.config.running_limit,
        )?;
        let chosen = self.choose_page(
            &page.candidates,
            page.now_ms,
            &provider.config.provider_config_digest,
            page.resume_capacity_available,
        )?;
        self.cursor = page.next;
        if self.cursor.is_none() {
            self.idle.finish_scan();
        }
        if let Some(request) = chosen {
            let binding = provider
                .power_binding(&request)
                .map_err(|_| "e2b_rotation_binding_unavailable")?;
            let result = with_power_environment(registry, &binding, |context| {
                provider.power_guarded(&request, true, || {
                    *context = None;
                    Ok(())
                })
            });
            if matches!(result,Ok(ref result) if result.operation_status=="acknowledged") {
                self.idle.forget(&request.environment_id);
            }
            // Capacity contention and a concurrent controller are ordinary waits.
            // The next bounded observation decides whether another operation is due.
        }
        Ok(())
    }
}

pub(crate) fn start(
    provider: Arc<E2bRuntime>,
    registry: Arc<E2bRunRegistry>,
    runtime: &tokio::runtime::Runtime,
) -> Option<RotationTask> {
    let timeout = provider.config.idle_pause_timeout?;
    let state = RotationState::new(timeout).ok()?;
    Some(RotationTask::spawn(
        runtime,
        state,
        move |state| state.tick(&provider, &registry),
        SCAN_INTERVAL,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::postgres_store::provider_operations::retained::power::{
        ProviderEnvironmentPowerAcknowledgement as Ack, ProviderEnvironmentPowerOperation,
    };
    pub(super) fn candidate() -> idle::RotationCandidate {
        idle::RotationCandidate {
            environment_id: "environment".into(),
            controller_epoch: 2,
            creation_attempt_id: "attempt".into(),
            sandbox_id: "sandbox".into(),
            latest: None,
            has_demand: false,
        }
    }
    pub(super) fn previous(
        candidate: &idle::RotationCandidate,
        action: Action,
        dispatch: Dispatch,
        ack: bool,
    ) -> ProviderEnvironmentPowerOperation {
        let mut request = idle::request(candidate, &format!("sha256:{}", "a".repeat(64)), action);
        request.operation_id = "power-current".into();
        if action == Action::Resume {
            request.predecessor_operation_id = Some("power-previous".into());
        }
        ProviderEnvironmentPowerOperation {automatic_demand:(action==Action::Resume).then(||crate::postgres_store::provider_operations::retained::power::automatic_resume::AutomaticResumeDemand {schema:"runtime.providerEnvironment.automaticResumeDemand.v1".into(),job_id:"agent_run.lifecycle:waiting-run".into(),agent_run_id:"waiting-run".into(),authorization_digest:format!("sha256:{}","a".repeat(64))}),not_dispatched_at_ms:(dispatch==Dispatch::NotDispatched).then_some(1000),request,sequence:1,dispatch_state:dispatch,acknowledgement:ack.then(||match action {
            Action::Pause => Ack::Paused {sandbox_id:candidate.sandbox_id.clone(),changed:true,requested_mode:"full".into()},
            Action::Resume => Ack::Resumed {sandbox_id:candidate.sandbox_id.clone()},
        })}
    }
    #[test]
    fn grace_and_current_demand_control_pause_without_eager_resume() {
        let mut state = RotationState::new(Duration::from_millis(100)).unwrap();
        let mut candidate = candidate();
        assert!(state.choose(&candidate, 1000, "config").unwrap().is_none());
        assert!(state.choose(&candidate, 1099, "config").unwrap().is_none());
        assert_eq!(
            state
                .choose(&candidate, 1100, "config")
                .unwrap()
                .unwrap()
                .action,
            Action::Pause
        );
        candidate.has_demand = true;
        assert!(state.choose(&candidate, 1200, "config").unwrap().is_none());
        candidate.has_demand = false;
        assert!(state.choose(&candidate, 1300, "config").unwrap().is_none());
    }
    #[test]
    fn paused_environment_resumes_only_for_current_demand_and_keeps_predecessor() {
        let mut state = RotationState::new(Duration::from_millis(100)).unwrap();
        let mut candidate = candidate();
        candidate.latest = Some(previous(
            &candidate,
            Action::Pause,
            Dispatch::Dispatched,
            true,
        ));
        assert!(state.choose(&candidate, 1000, "config").unwrap().is_none());
        candidate.has_demand = true;
        let request = state.choose(&candidate, 1001, "config").unwrap().unwrap();
        assert_eq!(request.action, Action::Resume);
        assert_eq!(
            request.predecessor_operation_id,
            Some("power-current".into())
        );
        assert_eq!(request.sandbox_id, "sandbox");
    }
    #[test]
    fn lost_sdk_reply_never_replays_and_unsent_resume_keeps_its_control_plane_identity() {
        let mut state = RotationState::new(Duration::from_millis(1)).unwrap();
        let mut candidate = candidate();
        candidate.has_demand = true;
        for action in [Action::Pause, Action::Resume] {
            candidate.latest = Some(previous(&candidate, action, Dispatch::Dispatched, false));
            assert!(state.choose(&candidate, 1000, "config").unwrap().is_none());
        }
        candidate.latest = Some(previous(
            &candidate,
            Action::Resume,
            Dispatch::Prepared,
            false,
        ));
        assert_eq!(
            state
                .choose(&candidate, 1000, "config")
                .unwrap()
                .unwrap()
                .operation_id,
            "power-current"
        );
        candidate.has_demand = false;
        assert_eq!(
            state
                .choose(&candidate, 1001, "config")
                .unwrap()
                .unwrap()
                .operation_id,
            "power-current"
        );
    }
}

#[cfg(test)]
mod fairness_tests {
    use super::tests::{candidate, previous};
    use super::*;
    #[test]
    fn mature_idle_release_precedes_lower_id_resume_under_one_running_budget() {
        let mut state = RotationState::new(Duration::from_millis(100)).unwrap();
        let mut waiting = candidate();
        waiting.environment_id = "a-paused-demand".into();
        waiting.has_demand = true;
        waiting.latest = Some(previous(
            &waiting,
            Action::Pause,
            Dispatch::Dispatched,
            true,
        ));
        let mut running = candidate();
        running.environment_id = "z-idle-running".into();
        let first = state
            .choose_page(&[waiting, running], 1000, "config", true)
            .unwrap()
            .unwrap();
        assert_eq!(first.action, Action::Resume);
        let mut waiting = candidate();
        waiting.environment_id = "a-paused-demand".into();
        waiting.has_demand = true;
        waiting.latest = Some(previous(
            &waiting,
            Action::Pause,
            Dispatch::Dispatched,
            true,
        ));
        let mut running = candidate();
        running.environment_id = "z-idle-running".into();
        let ready = state
            .choose_page(&[waiting, running], 1100, "config", true)
            .unwrap()
            .unwrap();
        assert_eq!(ready.action, Action::Pause);
        assert_eq!(ready.environment_id, "z-idle-running");
    }
    #[test]
    fn equal_priority_attempts_rotate_and_a_page_produces_at_most_one_operation() {
        let mut state = RotationState::new(Duration::from_millis(1)).unwrap();
        let candidates: Vec<_> = (0..idle::PAGE_SIZE)
            .map(|index| {
                let mut value = candidate();
                value.environment_id = format!("environment-{index:03}");
                value
            })
            .collect();
        assert!(state
            .choose_page(&candidates, 1000, "config", true)
            .unwrap()
            .is_none());
        let first = state
            .choose_page(&candidates, 1001, "config", true)
            .unwrap()
            .unwrap();
        let second = state
            .choose_page(&candidates, 1002, "config", true)
            .unwrap()
            .unwrap();
        assert_eq!(first.environment_id, "environment-000");
        assert_eq!(second.environment_id, "environment-001");
    }
}

#[cfg(test)]
mod capacity_selection_tests {
    use super::tests::{candidate, previous};
    use super::*;

    #[test]
    fn full_running_budget_skips_new_resume_until_capacity_returns() {
        let mut state = RotationState::new(Duration::from_millis(100)).unwrap();
        let mut waiting = candidate();
        waiting.has_demand = true;
        waiting.latest = Some(previous(
            &waiting,
            Action::Pause,
            Dispatch::Dispatched,
            true,
        ));
        for now in (1000..5000).step_by(250) {
            assert!(
                state
                    .choose_page(std::slice::from_ref(&waiting), now, "config", false)
                    .unwrap()
                    .is_none(),
                "a full budget must not prepare another fresh resume on every scan"
            );
        }
        let chosen = state
            .choose_page(std::slice::from_ref(&waiting), 5000, "config", true)
            .unwrap()
            .expect("capacity release must admit the waiting demand");
        assert_eq!(chosen.action, Action::Resume);
        assert_eq!(
            chosen.predecessor_operation_id.as_deref(),
            Some("power-current")
        );
    }

    #[test]
    fn full_budget_still_revisits_prepared_resume_for_cancellation() {
        let mut state = RotationState::new(Duration::from_millis(100)).unwrap();
        let mut waiting = candidate();
        waiting.latest = Some(previous(
            &waiting,
            Action::Resume,
            Dispatch::Prepared,
            false,
        ));
        let chosen = state
            .choose_page(&[waiting], 1000, "config", false)
            .unwrap()
            .expect("the existing reservation must still be supervised");
        assert_eq!(chosen.operation_id, "power-current");
    }
}
