//! Independent maintenance control over a retained provider Environment.
use super::*;
use crate::e2b_run_registry::{E2bEnvironmentBinding, E2bRunRegistry};
use crate::e2b_runtime::{E2bPreparedHost, E2bRuntimeError};
use crate::postgres_store::provider_operations::retained::power::{
    ProviderEnvironmentPowerRequest, ProviderEnvironmentPowerResult,
};
use std::sync::Arc;

fn with_power_environment(
    registry: &E2bRunRegistry,
    binding: &E2bEnvironmentBinding,
    advance: impl FnOnce(
        &mut Option<Arc<E2bPreparedHost>>,
    ) -> Result<ProviderEnvironmentPowerResult, E2bRuntimeError>,
) -> Result<ProviderEnvironmentPowerResult, E2bRuntimeError> {
    let slot = registry
        .existing_environment(binding)
        .map_err(|_| E2bRuntimeError::Rejected("e2b_power_binding_rejected"))?;
    // Preserve the same local-guard -> durable-Environment-pin order as Run steps.
    // Maintenance after restart never reserves ordinary live-context capacity.
    let mut held = slot
        .as_ref()
        .map(|slot| {
            slot.try_enter()
                .map_err(|_| E2bRuntimeError::Unavailable("e2b_power_context_unavailable"))?
                .ok_or(E2bRuntimeError::Unavailable(
                    "e2b_environment_already_active",
                ))
        })
        .transpose()?;
    let mut absent = None;
    let context = held.as_deref_mut().unwrap_or(&mut absent);
    // The callback invalidates this handle only when the Store supplies a fresh
    // dispatch permit. Historical ACK replay must preserve a successor's handle.
    let result = advance(context)?;
    if result.environment_id != binding.environment_id {
        return Err(E2bRuntimeError::Unknown(
            "e2b_power_result_binding_rejected",
        ));
    }
    Ok(result)
}

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
        with_power_environment(&state.e2b_runs, &binding, |context| {
            provider.power(&request, || {
                *context = None;
                Ok(())
            })
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
        with_power_environment(&registry, &binding, |context| {
            assert!(context.is_none());
            Ok(result())
        })
        .unwrap();
        assert!(registry.existing_environment(&binding).unwrap().is_none());
    }
    #[test]
    fn maintenance_keeps_existing_guard_through_dispatch_and_replay() {
        let registry = E2bRunRegistry::default();
        let binding = binding();
        let slot = registry.environment(binding.clone()).unwrap().unwrap();
        let advanced = Cell::new(false);
        with_power_environment(&registry, &binding, |context| {
            assert!(context.is_none());
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
}
