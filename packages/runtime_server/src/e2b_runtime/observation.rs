//! Typed read-only provider observations; execution and retirement stay elsewhere.
use super::*;
use crate::environment_authorization::provider_maintenance::ProviderMaintenanceTarget;
use crate::postgres_store::environment_observation::ProviderEnvironmentObservation;
use crate::postgres_store::provider_operations::retained::power::{
    ProviderEnvironmentPowerAcknowledgement, ProviderEnvironmentPowerAction,
    ProviderEnvironmentPowerDispatchState, ProviderEnvironmentPowerOperation,
};
use std::collections::BTreeMap;

impl E2bRuntime {
    pub(crate) fn observe_environment(
        &self,
        target: &ProviderEnvironmentObservation,
    ) -> Result<Value, String> {
        let mut process: Option<Arc<SpoolingAdapterProcess>> = None;
        observe_with(target, &self.config.profile, |operation, payload| {
            if process.is_none() {
                let launched = self
                    .launcher
                    .launch(&self.config, E2bAdapterLane::Ordinary)
                    .map_err(|_| "provider_environment_observation_unavailable".to_string())?;
                require_capabilities(
                    &launched,
                    E2bAdapterLane::Ordinary,
                    self.config.request_timeout,
                )
                .map_err(|_| "provider_environment_observation_unavailable".to_string())?;
                process = Some(launched);
            }
            let process = process.as_ref().expect("adapter process launched");
            let response = process
                .call(
                    &uuid::Uuid::new_v4().to_string(),
                    operation,
                    payload,
                    self.config.request_timeout,
                )
                .map_err(|_| "provider_environment_observation_unavailable".to_string())?;
            if response.outcome != AdapterOutcome::Succeeded {
                return Err("provider_environment_observation_unavailable".into());
            }
            response
                .result
                .ok_or_else(|| "provider_environment_observation_unavailable".to_string())
        })
    }
}

/// The acknowledged pause stays in force while a provably unsent Resume is the
/// latest power fact; a dispatched fact without acknowledgement proves nothing.
fn pause_holds(power: Option<&ProviderEnvironmentPowerOperation>) -> bool {
    power.is_some_and(|operation| match &operation.acknowledgement {
        Some(ProviderEnvironmentPowerAcknowledgement::Paused { .. }) => true,
        None if operation.request.action == ProviderEnvironmentPowerAction::Resume
            && operation.dispatch_state == ProviderEnvironmentPowerDispatchState::NotDispatched =>
        {
            true
        }
        _ => false,
    })
}
fn power_outcome_unknown(power: Option<&ProviderEnvironmentPowerOperation>) -> bool {
    power.is_some_and(|operation| {
        operation.acknowledgement.is_none()
            && operation.dispatch_state == ProviderEnvironmentPowerDispatchState::Dispatched
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Attached {
    sandbox_id: String,
    template_id: String,
    metadata: BTreeMap<String, String>,
    state: String,
    cpu_count: i64,
    memory_mi_b: i64,
}

fn observe_with(
    target: &ProviderEnvironmentObservation,
    configured: &EnvironmentProfile,
    mut call: impl FnMut(&str, Value) -> Result<Value, String>,
) -> Result<Value, String> {
    if configured
        .digest()
        .map_err(|_| "provider_environment_observation_profile_rejected")?
        != target.profile_digest
    {
        return Err("provider_environment_observation_profile_rejected".into());
    }
    let EnvironmentProfile::E2b(profile) = &target.profile else {
        return Err("provider_environment_observation_profile_rejected".into());
    };
    let mut state = "unallocated";
    let mut provider_target = Value::Null;
    let mut resource_holding = 0u8;
    let mut resource_facts = Value::Null;
    if matches!(
        target
            .creation
            .as_ref()
            .map(|operation| operation.dispatch_state),
        Some(ProviderCreationDispatchState::Dispatched)
    ) {
        if let Some(provider) = &target.provider_target {
            provider_target = serde_json::to_value(ProviderMaintenanceTarget {
                provider: "e2b".into(),
                creation_attempt_id: provider.identity.attempt_id.clone(),
                instance_id: provider.identity.instance_id.clone(),
                sandbox_id: provider.sandbox_id.clone(),
                verification_digest: provider.verification_digest.clone(),
                provider_config_digest: provider.identity.provider_config_digest.clone(),
            })
            .map_err(|_| "provider_environment_observation_target_rejected")?;
            if target.removed {
                state = "destroyed";
            } else if pause_holds(target.power.as_ref()) {
                state = "paused";
                resource_holding = 1;
            } else if power_outcome_unknown(target.power.as_ref()) {
                state = "unknown";
            } else {
                let metadata = expected_metadata(&provider.identity);
                let reply = call(
                    "attach",
                    json!({"sandboxId":provider.sandbox_id,
                    "expectedTemplateId":profile.template_ref.template_id,"expectedMetadata":metadata,
                    "workspaceRoot":profile.workspace_root,"workspaceUser":profile.user,
                    "includeResources":true}),
                )?;
                let attached: Attached = serde_json::from_value(reply)
                    .map_err(|_| "provider_environment_observation_target_rejected")?;
                if attached.sandbox_id != provider.sandbox_id
                    || attached.template_id != profile.template_ref.template_id
                    || attached.metadata != metadata
                    || attached.state != "running"
                    || attached.cpu_count <= 0
                    || attached.memory_mi_b <= 0
                {
                    return Err("provider_environment_observation_target_rejected".into());
                }
                state = "resident";
                resource_holding = 1;
                resource_facts =
                    json!({"cpuCount":attached.cpu_count,"memoryMiB":attached.memory_mi_b});
            }
        } else {
            state = "unknown";
        }
    }
    Ok(
        json!({"schema":"runtime.provider_environment.observe.result.v1","state":state,
        "sessionId":target.session_id,"environmentId":target.environment_id,
        "profile":target.profile,"profileDigest":target.profile_digest,
        "controllerEpoch":target.controller_epoch,"controllerOwner":target.controller_owner,
        "providerTarget":provider_target,"templateRef":profile.template_ref,
        "resourceFacts":resource_facts,"resourceHolding":resource_holding}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::postgres_store::provider_operations::{
        cleanup::ProviderVerifiedTarget, ProviderCreationDispatchState, ProviderCreationIdentity,
        ProviderCreationOperation,
    };
    use centaeris_core::execution::EnvironmentTaskOwner;
    fn target() -> ProviderEnvironmentObservation {
        let mut value: Value = serde_json::from_str(include_str!(
            "../../../../tests/workspace/fixtures/e2b_environment_profile/v1/valid.json"
        ))
        .unwrap();
        value["lifetime"] = json!("retained");
        value["workspaceInitialization"] = json!("taskImage");
        value["workspaceRoot"] = json!("/app");
        value["user"] = json!("root");
        let profile: EnvironmentProfile = serde_json::from_value(value).unwrap();
        let digest = profile.digest().unwrap();
        let EnvironmentProfile::E2b(ref p) = profile else {
            unreachable!()
        };
        let identity = ProviderCreationIdentity {
            attempt_id: "attempt".into(),
            execution_id: "execution".into(),
            instance_id: format!("instance_{}", "a".repeat(64)),
            environment_id: "environment".into(),
            session_id: "session".into(),
            workspace_id: "workspace".into(),
            owner: EnvironmentTaskOwner::AgentRun {
                run_id: "origin-run".into(),
            },
            authorization_digest: format!("sha256:{}", "b".repeat(64)),
            profile_digest: digest.clone(),
            provider_config_digest: p.provider_config_digest.clone(),
            request_digest: format!("sha256:{}", "c".repeat(64)),
            idempotency_key: "create-key".into(),
            processing_job_id: Some("origin-job".into()),
        };
        let provider = ProviderVerifiedTarget {
            identity: identity.clone(),
            sandbox_id: "sandbox".into(),
            verification_digest: format!("sha256:{}", "d".repeat(64)),
        };
        ProviderEnvironmentObservation {
            session_id: "session".into(),
            environment_id: "environment".into(),
            profile,
            profile_digest: digest,
            controller_epoch: 3,
            controller_owner: None,
            creation: Some(ProviderCreationOperation {
                identity,
                dispatch_state: ProviderCreationDispatchState::Dispatched,
                reported_sandbox_id: Some("sandbox".into()),
                recorded_at_ms: 0,
            }),
            provider_target: Some(provider),
            power: None,
            removed: false,
        }
    }
    #[test]
    fn resident_observation_uses_exact_provider_binding_and_fresh_sdk_resources() {
        let target = target();
        let mut calls = Vec::new();
        let provider = target.provider_target.as_ref().unwrap();
        let metadata = expected_metadata(&provider.identity);
        let EnvironmentProfile::E2b(ref profile) = target.profile else {
            unreachable!()
        };
        let result=observe_with(&target,&target.profile,|operation,payload| {
            calls.push((operation.to_string(),payload));
            Ok(json!({"sandboxId":provider.sandbox_id,"templateId":profile.template_ref.template_id,"metadata":metadata,"state":"running","cpuCount":4,"memoryMiB":4096}))
        }).unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "attach");
        assert_eq!(calls[0].1["includeResources"], true);
        assert_eq!(calls[0].1["expectedMetadata"], json!(metadata));
        let typed =
            crate::environment_authorization::provider_maintenance::ProviderMaintenanceTarget {
                provider: "e2b".into(),
                creation_attempt_id: provider.identity.attempt_id.clone(),
                instance_id: provider.identity.instance_id.clone(),
                sandbox_id: provider.sandbox_id.clone(),
                verification_digest: provider.verification_digest.clone(),
                provider_config_digest: provider.identity.provider_config_digest.clone(),
            };
        assert_eq!(
            result,
            json!({"schema":"runtime.provider_environment.observe.result.v1","state":"resident","sessionId":"session","environmentId":"environment","profile":target.profile,"profileDigest":target.profile_digest,"controllerEpoch":3,"controllerOwner":null,"providerTarget":typed,"templateRef":profile.template_ref,"resourceFacts":{"cpuCount":4,"memoryMiB":4096},"resourceHolding":1})
        );
    }
    #[test]
    fn unallocated_observation_does_not_contact_provider_or_invent_physical_identity() {
        let mut target = target();
        target.creation = None;
        target.provider_target = None;
        let result = observe_with(&target, &target.profile, |_, _| {
            panic!("unallocated provider observation contacted SDK")
        })
        .unwrap();
        assert_eq!(result["state"], "unallocated");
        assert_eq!(result["providerTarget"], Value::Null);
        assert_eq!(result["resourceFacts"], Value::Null);
        assert_eq!(result["resourceHolding"], 0);
    }
}
