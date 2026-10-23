//! Internal maintenance authority is independent of Run execute/cleanup grants.
use super::*;
use crate::e2b_run_registry::{E2bEnvironmentBinding, E2bRunRegistry};
use crate::e2b_runtime::E2bRuntimeError;
use crate::postgres_store::provider_operations::retained::retirement::{
    ProviderEnvironmentRetirementRequest, ProviderEnvironmentRetirementResult,
};
use crate::postgres_store::residency::retirement::{
    RetirementError, RetirementRequest, RetirementTarget,
};

#[derive(Deserialize)]
#[serde(untagged)]
enum RetirementEnvelope {
    Provider(ProviderEnvironmentRetirementRequest),
    Legacy(RetirementRequest),
}

impl RetirementEnvelope {
    fn decode(body: &[u8]) -> Result<Self, ()> {
        let request: Self = serde_json::from_slice(body).map_err(|_| ())?;
        match &request {
            Self::Provider(request) => request.validate(),
            Self::Legacy(request) => request.validate(),
        }
        .map_err(|_| ())?;
        Ok(request)
    }
}

fn with_retirement_environment(
    registry: &E2bRunRegistry,
    binding: &E2bEnvironmentBinding,
    retire: impl FnOnce() -> Result<ProviderEnvironmentRetirementResult, E2bRuntimeError>,
) -> Result<ProviderEnvironmentRetirementResult, E2bRuntimeError> {
    let slot = registry
        .existing_environment(binding)
        .map_err(|_| E2bRuntimeError::Rejected("e2b_retirement_binding_rejected"))?;
    // The existing Environment guard precedes all durable retirement pins.
    // A restart may have no live slot; maintenance must not allocate one.
    let mut context = if let Some(slot) = slot.as_ref() {
        Some(
            slot.try_enter()
                .map_err(|_| E2bRuntimeError::Unavailable("e2b_retirement_context_unavailable"))?
                .ok_or(E2bRuntimeError::Unavailable(
                    "e2b_environment_already_active",
                ))?,
        )
    } else {
        None
    };
    let result = retire()?;
    if result.environment_id != binding.environment_id {
        return Err(E2bRuntimeError::Unknown(
            "e2b_retirement_result_binding_rejected",
        ));
    }
    if result.status == "removed" {
        if let Some(context) = context.as_mut() {
            **context = None;
        }
        if let Some(slot) = slot.as_ref() {
            registry
                .remove_environment(binding, slot)
                .map_err(|_| E2bRuntimeError::Unknown("e2b_retirement_registry_unconfirmed"))?;
        }
    }
    Ok(result)
}

pub(super) fn maintenance_authorized(
    configured: Option<&str>,
    internal: Option<&str>,
    supplied: Option<&str>,
) -> bool {
    configured.is_some_and(|key| key.len() >= 32 && Some(key) != internal && supplied == Some(key))
}

pub(super) fn retire(
    state: HttpServerState,
    request: Request<Bytes>,
) -> Result<Response<Body>, String> {
    if let Err((status, code)) = super::environment_maintenance::body_authority(&request) {
        return json_error_response(status, code);
    }
    let request = match RetirementEnvelope::decode(request.body()) {
        Ok(r) => r,
        Err(_) => return json_error_response(400, "environment_retirement_request_invalid"),
    };
    let request = match request {
        RetirementEnvelope::Provider(request) => return retire_provider(state, request),
        RetirementEnvelope::Legacy(request) => request,
    };
    // Host identity comes from the same local daemon used for allocation.
    let host = crate::docker_execution_host::resident_host_context().map(|(id, _)| id);
    if host.as_ref() != Ok(&request.host_id) {
        return json_error_response(503, "environment_retirement_host_unavailable");
    }
    match state
        .job_store
        .retire_retained_instance(&request, retire_physical)
    {
        Ok(result) => Ok(http_response(
            200,
            "application/json",
            serde_json::to_vec(&result).map_err(|e| e.to_string())?,
        )),
        Err(error) => retirement_error_response(error),
    }
}

fn retire_provider(
    state: HttpServerState,
    request: ProviderEnvironmentRetirementRequest,
) -> Result<Response<Body>, String> {
    let Some(provider) = state.e2b_runtime.as_ref() else {
        return json_error_response(503, "e2b_retirement_not_configured");
    };
    let result = provider.retirement_binding(&request).and_then(|binding| {
        with_retirement_environment(&state.e2b_runs, &binding, || provider.retire(&request))
    });
    match result {
        Ok(result) => Ok(http_response(
            if result.status == "pending" { 202 } else { 200 },
            "application/json",
            serde_json::to_vec(&result).map_err(|_| "e2b_retirement_response_unavailable")?,
        )),
        Err(E2bRuntimeError::Rejected(code)) => json_error_response(409, code),
        Err(E2bRuntimeError::Unavailable(code) | E2bRuntimeError::Unknown(code)) => {
            json_error_response(503, code)
        }
    }
}

fn retirement_error_response(error: RetirementError) -> Result<Response<Body>, String> {
    let code = match error {
        RetirementError::ControllerEpochConflict => "environment_retirement_epoch_conflict",
        RetirementError::Rejected(_) => "environment_retirement_rejected",
    };
    json_error_response(409, code)
}

fn physical_identity(facts: &serde_json::Value, target: &RetirementTarget) -> bool {
    let labels = &facts["Config"]["Labels"];
    labels["centaeris.allocation_owner"]
        .as_str()
        .and_then(|v| {
            serde_json::from_str::<centaeris_core::execution::EnvironmentTaskOwner>(v).ok()
        })
        .as_ref()
        == Some(&target.allocation_owner)
        && labels["centaeris.agent_run_id"].as_str() == target.allocation_agent_run_id.as_deref()
        && facts["Id"].as_str() == Some(target.container_id.as_str())
        && facts["Image"].as_str() == Some(target.image_digest.as_str())
        && [
            ("centaeris.managed", "true"),
            ("centaeris.environment_id", target.environment_id.as_str()),
            ("centaeris.instance_id", target.instance_id.as_str()),
            (
                "centaeris.execution_id",
                target.allocation_execution_id.as_str(),
            ),
            (
                "centaeris.environment_authorization_digest",
                target.binding_digest.as_str(),
            ),
        ]
        .iter()
        .all(|(name, value)| labels[*name].as_str() == Some(*value))
}

fn retire_physical(target: &RetirementTarget) -> Result<bool, String> {
    retire_observed(target, crate::docker_engine::inspect, |id| {
        crate::docker_engine::remove(id, false)
    })
}

fn retire_observed(
    target: &RetirementTarget,
    mut inspect: impl FnMut(&str) -> Result<Option<serde_json::Value>, String>,
    remove: impl FnOnce(&str) -> Result<(), String>,
) -> Result<bool, String> {
    if let Some(facts) = inspect(&target.container_id)? {
        if !physical_identity(&facts, target) {
            return Err("retirement physical identity rejected".into());
        }
        // Docker force-removes this exact container and its instance-owned tree.
        // A lost mutation ACK is resolved only by an independent inspect.
        let _ack = remove(&target.container_id);
    }
    Ok(inspect(&target.container_id)?.is_none())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn provider_binding() -> E2bEnvironmentBinding {
        E2bEnvironmentBinding {
            environment_id: "environment".into(),
            profile_digest: "frozen-profile".into(),
            provider_config_digest: "frozen-provider".into(),
        }
    }

    fn provider_result(status: &'static str) -> ProviderEnvironmentRetirementResult {
        ProviderEnvironmentRetirementResult {
            schema: "runtime.provider_environment.retire.result.v1",
            operation_id: "retire-environment".into(),
            environment_id: "environment".into(),
            creation_attempt_id: "original-attempt".into(),
            sandbox_id: "original-sandbox".into(),
            status,
        }
    }

    #[test]
    fn provider_and_legacy_retirement_wires_remain_exact_distinct_variants() {
        let provider = json!({"schema":"runtime.provider_environment.retire.v1",
            "operationId":"retirement", "environmentId":"environment",
            "creationAttemptId":"original-attempt", "sandboxId":"original-sandbox",
            "providerConfigDigest":format!("sha256:{}", "c".repeat(64)),
            "expectedControllerEpoch":2, "acknowledgeDataLoss":true});
        let legacy = json!({"schema":"runtime.environment.retire.v1",
            "operationId":"retirement", "environmentId":"environment", "instanceId":"instance",
            "hostId":"host", "expectedControllerEpoch":2, "acknowledgeTmpfsLoss":true});
        assert!(matches!(
            RetirementEnvelope::decode(&serde_json::to_vec(&provider).unwrap()),
            Ok(RetirementEnvelope::Provider(_))
        ));
        assert!(matches!(
            RetirementEnvelope::decode(&serde_json::to_vec(&legacy).unwrap()),
            Ok(RetirementEnvelope::Legacy(_))
        ));
        for (source, field, value) in [
            (&provider, "schema", json!("runtime.environment.retire.v1")),
            (
                &legacy,
                "schema",
                json!("runtime.provider_environment.retire.v1"),
            ),
            (&provider, "reason", json!("unsupported")),
            (&provider, "acknowledgeTmpfsLoss", json!(true)),
            (&legacy, "sandboxId", json!("original-sandbox")),
            (&provider, "acknowledgeDataLoss", json!(false)),
        ] {
            let mut changed = source.clone();
            changed[field] = value;
            assert!(RetirementEnvelope::decode(&serde_json::to_vec(&changed).unwrap()).is_err());
        }
        let duplicated = serde_json::to_string(&provider).unwrap().replacen(
            "\"operationId\":\"retirement\"",
            "\"operationId\":\"retirement\",\"operationId\":\"other\"",
            1,
        );
        assert!(RetirementEnvelope::decode(duplicated.as_bytes()).is_err());
    }

    #[test]
    fn active_or_foreign_environment_rejects_retirement_before_store_pins() {
        let registry = E2bRunRegistry::default();
        let binding = provider_binding();
        let slot = registry.environment(binding.clone()).unwrap().unwrap();
        let _guard = slot.try_enter().unwrap().unwrap();
        for foreign in [false, true] {
            let mut requested = binding.clone();
            if foreign {
                requested.profile_digest = "different-profile".into();
            }
            let called = Cell::new(false);
            let result = with_retirement_environment(&registry, &requested, || {
                called.set(true);
                Ok(provider_result("removed"))
            });
            assert!(result.is_err());
            assert!(
                !called.get(),
                "busy or foreign context reached durable/physical work"
            );
        }
    }

    #[test]
    fn maintenance_without_live_context_does_not_allocate_an_environment_slot() {
        let registry = E2bRunRegistry::default();
        let binding = provider_binding();
        with_retirement_environment(&registry, &binding, || {
            assert!(registry.existing_environment(&binding).unwrap().is_none());
            Ok(provider_result("pending"))
        })
        .unwrap();
        assert!(registry.existing_environment(&binding).unwrap().is_none());
    }

    #[test]
    fn only_confirmed_retirement_removes_the_exact_existing_environment_slot() {
        let registry = E2bRunRegistry::default();
        let binding = provider_binding();
        let original = registry.environment(binding.clone()).unwrap().unwrap();
        with_retirement_environment(&registry, &binding, || Ok(provider_result("pending")))
            .unwrap();
        assert!(Arc::ptr_eq(
            &original,
            &registry.existing_environment(&binding).unwrap().unwrap()
        ));
        with_retirement_environment(&registry, &binding, || Ok(provider_result("removed")))
            .unwrap();
        assert!(registry.existing_environment(&binding).unwrap().is_none());
    }
    #[test]
    fn unaccepted_epoch_conflict_has_a_distinct_bounded_http_error() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            for (error, code) in [
                (
                    RetirementError::ControllerEpochConflict,
                    "environment_retirement_epoch_conflict",
                ),
                (
                    RetirementError::Rejected("environment_retirement_epoch_conflict".into()),
                    "environment_retirement_rejected",
                ),
                (
                    RetirementError::Rejected("private store diagnostics".into()),
                    "environment_retirement_rejected",
                ),
            ] {
                let response = retirement_error_response(error).unwrap();
                assert_eq!(response.status().as_u16(), 409);
                assert_eq!(response.headers()["content-type"], "application/json");
                let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
                assert_eq!(
                    serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
                    json!({"error": code})
                );
            }
        });
    }

    fn target() -> RetirementTarget {
        RetirementTarget {
            allocation_execution_id: "execution".into(),
            allocation_agent_run_id: Some("origin".into()),
            allocation_owner: centaeris_core::execution::EnvironmentTaskOwner::AgentRun {
                run_id: "origin".into(),
            },
            environment_id: "environment".into(),
            instance_id: "instance".into(),
            container_id: "physical".into(),
            image_digest: "sha256:image".into(),
            binding_digest: "sha256:binding".into(),
        }
    }
    fn facts(t: &RetirementTarget) -> serde_json::Value {
        json!({"Id":t.container_id,"Image":t.image_digest,"Config":{"Labels":{"centaeris.managed":"true","centaeris.environment_id":t.environment_id,"centaeris.instance_id":t.instance_id,"centaeris.execution_id":t.allocation_execution_id,"centaeris.agent_run_id":t.allocation_agent_run_id,"centaeris.allocation_owner":serde_json::to_string(&t.allocation_owner).unwrap(),"centaeris.environment_authorization_digest":t.binding_digest}}})
    }
    #[test]
    fn run_token_never_confers_maintenance_authority() {
        let key = "maintenance-key-with-at-least-32-bytes";
        assert!(!maintenance_authorized(None, Some("run"), Some(key)));
        assert!(!maintenance_authorized(Some(key), Some(key), Some(key)));
        assert!(!maintenance_authorized(Some(key), Some("run"), Some("run")));
        assert!(maintenance_authorized(Some(key), Some("run"), Some(key)));
    }
    #[test]
    fn lost_delete_ack_is_resolved_by_exact_absence() {
        let t = target();
        let calls = Cell::new(0);
        assert!(retire_observed(
            &t,
            |id| {
                assert_eq!(id, "physical");
                let n = calls.get();
                calls.set(n + 1);
                Ok(if n == 0 { Some(facts(&t)) } else { None })
            },
            |id| {
                assert_eq!(id, "physical");
                Err("lost ack".into())
            }
        )
        .unwrap());
        assert_eq!(calls.get(), 2);
    }
    #[test]
    fn unobservable_or_foreign_instance_is_never_success_or_removed() {
        let t = target();
        let removed = Cell::new(false);
        let mut foreign = facts(&t);
        foreign["Config"]["Labels"]["centaeris.instance_id"] = json!("other");
        assert!(retire_observed(
            &t,
            |_| Ok(Some(foreign.clone())),
            |_| {
                removed.set(true);
                Ok(())
            }
        )
        .is_err());
        assert!(!removed.get());
        let calls = Cell::new(0);
        assert!(retire_observed(
            &t,
            |_| {
                let n = calls.get();
                calls.set(n + 1);
                if n == 0 {
                    Ok(Some(facts(&t)))
                } else {
                    Err("daemon unavailable".into())
                }
            },
            |_| Ok(())
        )
        .is_err());
        assert!(!retire_observed(&t, |_| Ok(Some(facts(&t))), |_| Ok(())).unwrap());
    }
}
