//! Independent maintenance control over a retained provider Environment.
use super::*;
#[cfg(test)]
use crate::e2b_run_registry::{E2bEnvironmentBinding, E2bRunRegistry};
use crate::e2b_runtime::E2bRuntimeError;
use crate::postgres_store::provider_operations::retained::power::ProviderEnvironmentPowerRequest;
#[cfg(test)]
use std::sync::Arc;

use crate::e2b_runtime::with_power_environment;
#[cfg(test)]
use crate::postgres_store::provider_operations::retained::power::ProviderEnvironmentPowerResult;

pub(super) fn power(
    state: HttpServerState,
    request: Request<Bytes>,
) -> Result<Response<Body>, String> {
    if let Err((status, code)) = super::environment_maintenance::body_authority(&request) {
        return json_error_response(status, code);
    }
    let request: ProviderEnvironmentPowerRequest = match serde_json::from_slice(request.body()) {
        Ok(value) => value,
        Err(_) => return json_error_response(400, "environment_power_request_invalid"),
    };
    if request.validate().is_err() {
        return json_error_response(400, "environment_power_request_invalid");
    }
    let Some(provider) = state.e2b_runtime.as_ref() else {
        return json_error_response(503, "e2b_power_not_configured");
    };
    let result = provider.power_binding(&request).and_then(|binding| {
        with_power_environment(&state.e2b_runs, &binding, |before_dispatch| {
            provider.power(&request, before_dispatch)
        })
    });
    match result {
        Ok(result) => Ok(http_response(
            if result.operation_status == "acknowledged" {
                200
            } else {
                202
            },
            "application/json",
            serde_json::to_vec(&result).map_err(|_| "e2b_power_response_unavailable")?,
        )),
        Err(E2bRuntimeError::Rejected(code)) => json_error_response(409, code),
        Err(E2bRuntimeError::Unavailable(code) | E2bRuntimeError::Unknown(code)) => {
            json_error_response(503, code)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::postgres_store::provider_operations::retained::power::ProviderEnvironmentPowerAction;
    use std::cell::Cell;

    fn binding() -> E2bEnvironmentBinding {
        E2bEnvironmentBinding {
            environment_id: "environment".into(),
            profile_digest: "profile".into(),
            provider_config_digest: "provider".into(),
        }
    }
    fn result() -> ProviderEnvironmentPowerResult {
        ProviderEnvironmentPowerResult {
            schema: "runtime.provider_environment.power.result.v1",
            operation_id: "pause".into(),
            action: ProviderEnvironmentPowerAction::Pause,
            environment_id: "environment".into(),
            creation_attempt_id: "creation".into(),
            sandbox_id: "sandbox".into(),
            sequence: 1,
            operation_status: "acknowledged",
            requested_mode: Some("full"),
        }
    }
    #[test]
    fn maintenance_without_live_slot_advances_without_allocating_ordinary_capacity() {
        let registry = E2bRunRegistry::default();
        let binding = binding();
        with_power_environment(&registry, &binding, |_| Ok(result())).unwrap();
        assert!(registry.existing_environment(&binding).unwrap().is_none());
    }
    #[test]
    fn maintenance_keeps_existing_guard_through_dispatch_and_replay() {
        let registry = E2bRunRegistry::default();
        let binding = binding();
        let slot = registry.environment(binding.clone()).unwrap().unwrap();
        let advanced = Cell::new(false);
        with_power_environment(&registry, &binding, |_| {
            assert!(slot.try_enter().unwrap().is_none());
            advanced.set(true);
            Ok(result())
        })
        .unwrap();
        assert!(advanced.get());
        assert!(Arc::ptr_eq(
            &slot,
            &registry.existing_environment(&binding).unwrap().unwrap()
        ));
        assert!(slot.try_enter().unwrap().is_some());
    }
    #[test]
    fn maintenance_rejects_active_or_foreign_environment_before_durable_dispatch() {
        let registry = E2bRunRegistry::default();
        let binding = binding();
        let slot = registry.environment(binding.clone()).unwrap().unwrap();
        let held = slot.try_enter().unwrap().unwrap();
        let calls = Cell::new(0);
        let attempt = || {
            calls.set(calls.get() + 1);
            Ok(result())
        };
        assert!(with_power_environment(&registry, &binding, |_| attempt()).is_err());
        drop(held);
        let foreign = E2bEnvironmentBinding {
            profile_digest: "foreign".into(),
            ..binding
        };
        assert!(with_power_environment(&registry, &foreign, |_| attempt()).is_err());
        assert_eq!(calls.get(), 0);
    }

    #[test]
    fn sequential_acknowledged_pauses_release_capacity_for_more_than_sixty_four_environments() {
        let registry = E2bRunRegistry::default();
        for index in 0..65 {
            let binding = E2bEnvironmentBinding {
                environment_id: format!("environment-{index}"),
                ..binding()
            };
            registry
                .environment(binding.clone())
                .unwrap()
                .unwrap_or_else(|| panic!("paused history blocked environment {index}"));
            with_power_environment(&registry, &binding, |before_dispatch| {
                before_dispatch().unwrap();
                let mut paused = result();
                paused.environment_id = binding.environment_id.clone();
                Ok(paused)
            })
            .unwrap();
        }
    }

    #[test]
    fn unconfirmed_pause_preserves_the_existing_registration() {
        for status in ["pending", "prepared", "notDispatched", "unknown"] {
            let registry = E2bRunRegistry::default();
            let binding = binding();
            registry.environment(binding.clone()).unwrap().unwrap();
            let outcome = with_power_environment(&registry, &binding, |before_dispatch| {
                before_dispatch().unwrap();
                if status == "unknown" {
                    return Err(E2bRuntimeError::Unknown("lost acknowledgement"));
                }
                let mut paused = result();
                paused.operation_status = status;
                Ok(paused)
            });
            assert_eq!(outcome.is_err(), status == "unknown");
            assert!(
                registry.existing_environment(&binding).unwrap().is_some(),
                "{status}"
            );
        }
    }

    #[test]
    fn historical_pause_ack_does_not_release_an_empty_successor_registration() {
        let registry = E2bRunRegistry::default();
        let binding = binding();
        registry.environment(binding.clone()).unwrap().unwrap();
        with_power_environment(&registry, &binding, |_| Ok(result())).unwrap();
        assert!(registry.existing_environment(&binding).unwrap().is_some());
    }

    #[test]
    fn fresh_pause_preserves_a_prefetched_slot_until_its_other_holder_finishes() {
        let registry = E2bRunRegistry::default();
        let binding = binding();
        let prefetched = registry.environment(binding.clone()).unwrap().unwrap();
        with_power_environment(&registry, &binding, |before_dispatch| {
            before_dispatch().unwrap();
            Ok(result())
        })
        .unwrap();
        let successor = registry.environment(binding).unwrap().unwrap();
        let held = prefetched.try_enter().unwrap().unwrap();
        assert!(
            successor.try_enter().unwrap().is_none(),
            "one environment acquired two independent guards"
        );
        drop(held);
        assert!(successor.try_enter().unwrap().is_some());
    }

    #[test]
    fn fresh_pause_preserves_a_weak_slot_that_can_be_upgraded() {
        let registry = E2bRunRegistry::default();
        let binding = binding();
        let slot = registry.environment(binding.clone()).unwrap().unwrap();
        let weak = Arc::downgrade(&slot);
        drop(slot);
        with_power_environment(&registry, &binding, |before_dispatch| {
            before_dispatch().unwrap();
            Ok(result())
        })
        .unwrap();
        let original = weak
            .upgrade()
            .expect("weak holder retains the original registration");
        let successor = registry.environment(binding).unwrap().unwrap();
        let held = original.try_enter().unwrap().unwrap();
        assert!(successor.try_enter().unwrap().is_none());
        drop(held);
    }

    #[test]
    fn resume_after_released_pause_uses_authority_callback_without_reserving_a_live_slot() {
        let registry = E2bRunRegistry::default();
        let binding = binding();
        registry.environment(binding.clone()).unwrap().unwrap();
        with_power_environment(&registry, &binding, |before_dispatch| {
            before_dispatch().unwrap();
            Ok(result())
        })
        .unwrap();
        assert!(registry.existing_environment(&binding).unwrap().is_none());
        let resumed = Cell::new(false);
        with_power_environment(&registry, &binding, |before_dispatch| {
            before_dispatch().unwrap();
            resumed.set(true);
            let mut receipt = result();
            receipt.action = ProviderEnvironmentPowerAction::Resume;
            receipt.requested_mode = None;
            Ok(receipt)
        })
        .unwrap();
        assert!(resumed.get());
        assert!(registry.existing_environment(&binding).unwrap().is_none());
        assert!(registry.environment(binding).unwrap().is_some());
    }
}
