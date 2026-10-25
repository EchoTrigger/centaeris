//! Independently authorized provider pause and resume over the exact creation identity.
use super::*;
use crate::e2b_run_registry::{E2bEnvironmentBinding, E2bRunRegistry};
use crate::observations::power::{PowerMetric, PowerObservation};
use crate::postgres_store::provider_operations::retained::power::{
    ProviderEnvironmentPowerAcknowledgement, ProviderEnvironmentPowerAction,
    ProviderEnvironmentPowerRequest, ProviderEnvironmentPowerResult,
};
use std::collections::BTreeMap;

pub(crate) fn with_power_environment(
    registry: &E2bRunRegistry,
    binding: &E2bEnvironmentBinding,
    advance: impl FnOnce(
        &mut (dyn FnMut() -> Result<(), String> + Send),
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
    let mut dispatched = false;
    let result = advance(&mut || {
        *context = None;
        dispatched = true;
        Ok(())
    })?;
    if result.environment_id != binding.environment_id {
        return Err(E2bRuntimeError::Unknown(
            "e2b_power_result_binding_rejected",
        ));
    }
    if dispatched
        && result.action == ProviderEnvironmentPowerAction::Pause
        && result.operation_status == "acknowledged"
    {
        // Keep the guard, but drop this extra reference before checking that no
        // other consumer can still acquire the old slot after its removal.
        drop(slot);
        if let Some(held) = held.take() {
            registry
                .release_paused_environment(binding, held)
                .map_err(|_| E2bRuntimeError::Unknown("e2b_power_registry_unconfirmed"))?;
        }
    }
    Ok(result)
}

impl E2bRuntime {
    /// Pure binding check before acquiring the existing local Environment guard.
    /// This grants no Run or physical dispatch authority.
    pub(crate) fn power_binding(
        &self,
        request: &ProviderEnvironmentPowerRequest,
    ) -> Result<E2bEnvironmentBinding, E2bRuntimeError> {
        request
            .validate()
            .map_err(|_| E2bRuntimeError::Rejected("e2b_power_request_invalid"))?;
        let EnvironmentProfile::E2b(profile) = self.config.profile() else {
            return Err(E2bRuntimeError::Rejected("e2b_power_profile_rejected"));
        };
        if profile.lifetime != "retained"
            || request.provider_config_digest != self.config.provider_config_digest
        {
            return Err(E2bRuntimeError::Rejected("e2b_power_binding_rejected"));
        }
        Ok(E2bEnvironmentBinding {
            environment_id: request.environment_id.clone(),
            profile_digest: self
                .config
                .profile()
                .digest()
                .map_err(|_| E2bRuntimeError::Rejected("e2b_power_profile_rejected"))?,
            provider_config_digest: self.config.provider_config_digest.clone(),
        })
    }

    pub(crate) fn power(
        &self,
        request: &ProviderEnvironmentPowerRequest,
        before_dispatch: impl FnOnce() -> Result<(), String> + Send,
    ) -> Result<ProviderEnvironmentPowerResult, E2bRuntimeError> {
        self.power_guarded(request, false, before_dispatch)
    }
    pub(crate) fn power_guarded(
        &self,
        request: &ProviderEnvironmentPowerRequest,
        automatic_resume: bool,
        before_dispatch: impl FnOnce() -> Result<(), String> + Send,
    ) -> Result<ProviderEnvironmentPowerResult, E2bRuntimeError> {
        let binding = self.power_binding(request)?;
        let EnvironmentProfile::E2b(profile) = self.config.profile() else {
            return Err(E2bRuntimeError::Rejected("e2b_power_profile_rejected"));
        };
        let observation = PowerObservation::new(
            &request.operation_id,
            match request.action {
                ProviderEnvironmentPowerAction::Pause => "pause",
                ProviderEnvironmentPowerAction::Resume => "resume",
            },
        );
        observation.timed(PowerMetric::RuntimePower, || {
            let mut lane = cleanup::CleanupLane::new_observed(self, observation.clone());
            // The Store commits intent and a fresh dispatch CAS before this callback.
            // Historical ACKs and Unknown replays cannot invalidate a newer Run's
            // context or enter the provider. No creator/successor Job is borrowed.
            let result = self
                .store
                .advance_provider_environment_power_guarded(
                    request,
                    &self.config.provider_config_digest,
                    self.config.running_limit,
                    automatic_resume,
                    |target, current| {
                        dispatch_power(
                            target,
                            current,
                            &binding,
                            &profile.template_ref.template_id,
                            profile.timeout_seconds,
                            |operation, payload| {
                                // The HTTP caller already holds the existing Environment
                                // guard. Discard old SDK handles before the fresh mutation,
                                // including mutations whose result will remain Unknown.
                                observation.timed(PowerMetric::ContextDrop, before_dispatch)?;
                                lane.call_payload(operation, payload)
                            },
                        )
                    },
                )
                .map_err(|_| E2bRuntimeError::Unknown("e2b_power_unconfirmed"));
            finish_power_lane(lane, result)
        })
    }
}

// Store success can still represent a dispatched operation whose receipt is unknown.
fn finish_power_lane(
    mut lane: cleanup::CleanupLane<'_>,
    result: Result<ProviderEnvironmentPowerResult, E2bRuntimeError>,
) -> Result<ProviderEnvironmentPowerResult, E2bRuntimeError> {
    if result
        .as_ref()
        .is_ok_and(|result| result.operation_status == "acknowledged")
    {
        lane.allow_reuse();
    }
    result
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PauseReceipt {
    sandbox_id: String,
    requested_mode: String,
    changed: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ResumeReceipt {
    sandbox_id: String,
    template_id: String,
    metadata: BTreeMap<String, String>,
    state: String,
}

fn dispatch_power(
    target: &ProviderVerifiedTarget,
    request: &ProviderEnvironmentPowerRequest,
    binding: &E2bEnvironmentBinding,
    template_id: &str,
    timeout_seconds: u32,
    invoke: impl FnOnce(&str, Value) -> Result<Value, String>,
) -> Result<ProviderEnvironmentPowerAcknowledgement, String> {
    request
        .validate()
        .map_err(|_| "e2b_power_request_invalid")?;
    if target.identity.attempt_id != request.creation_attempt_id
        || target.sandbox_id != request.sandbox_id
        || target.identity.environment_id != request.environment_id
        || binding.environment_id != request.environment_id
        || target.identity.profile_digest != binding.profile_digest
        || target.identity.provider_config_digest != binding.provider_config_digest
        || request.provider_config_digest != binding.provider_config_digest
        || target.verification_digest.is_empty()
        || template_id.is_empty()
        || !(1..=86400).contains(&timeout_seconds)
    {
        return Err("e2b_power_target_rejected".into());
    }
    let metadata = expected_metadata(&target.identity);
    let mut payload = json!({"sandboxId":target.sandbox_id,
        "expectedTemplateId":template_id,"expectedMetadata":metadata});
    match request.action {
        ProviderEnvironmentPowerAction::Pause => {
            payload["mode"] = json!("full");
            let receipt: PauseReceipt = serde_json::from_value(invoke("pause", payload)?)
                .map_err(|_| "e2b_pause_acknowledgement_invalid")?;
            // The pinned SDK maps every HTTP 409 to false. Neither false nor an
            // observed paused state proves that this fresh full request completed.
            if receipt.sandbox_id != target.sandbox_id
                || receipt.requested_mode != "full"
                || !receipt.changed
            {
                return Err("e2b_pause_acknowledgement_unconfirmed".into());
            }
            Ok(ProviderEnvironmentPowerAcknowledgement::Paused {
                sandbox_id: receipt.sandbox_id,
                requested_mode: receipt.requested_mode,
                changed: receipt.changed,
            })
        }
        ProviderEnvironmentPowerAction::Resume => {
            payload["timeoutSeconds"] = json!(timeout_seconds);
            payload["onResume"] = json!("restore");
            let receipt: ResumeReceipt = serde_json::from_value(invoke("connect", payload)?)
                .map_err(|_| "e2b_resume_acknowledgement_invalid")?;
            if receipt.sandbox_id != target.sandbox_id
                || receipt.template_id != template_id
                || receipt.metadata != metadata
                || receipt.state != "running"
            {
                return Err("e2b_resume_acknowledgement_unconfirmed".into());
            }
            Ok(ProviderEnvironmentPowerAcknowledgement::Resumed {
                sandbox_id: receipt.sandbox_id,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    mod reuse_tests;
    use super::*;
    use std::cell::Cell;

    fn target() -> ProviderVerifiedTarget {
        ProviderVerifiedTarget {
            identity: ProviderCreationIdentity {
                attempt_id: "original-attempt".into(),
                execution_id: "original-execution".into(),
                instance_id: "instance".into(),
                environment_id: "environment".into(),
                session_id: "session".into(),
                workspace_id: "workspace".into(),
                owner: centaeris_core::execution::EnvironmentTaskOwner::AgentRun {
                    run_id: "original-run-a".into(),
                },
                authorization_digest: format!("sha256:{}", "a".repeat(64)),
                profile_digest: format!("sha256:{}", "b".repeat(64)),
                provider_config_digest: format!("sha256:{}", "c".repeat(64)),
                request_digest: format!("sha256:{}", "d".repeat(64)),
                idempotency_key: "original-key".into(),
                processing_job_id: Some("original-job-a".into()),
            },
            sandbox_id: "original-sandbox".into(),
            verification_digest: format!("sha256:{}", "e".repeat(64)),
        }
    }
    fn binding(target: &ProviderVerifiedTarget) -> E2bEnvironmentBinding {
        E2bEnvironmentBinding {
            environment_id: target.identity.environment_id.clone(),
            profile_digest: target.identity.profile_digest.clone(),
            provider_config_digest: target.identity.provider_config_digest.clone(),
        }
    }
    fn request(
        target: &ProviderVerifiedTarget,
        action: ProviderEnvironmentPowerAction,
    ) -> ProviderEnvironmentPowerRequest {
        ProviderEnvironmentPowerRequest {
            schema: "runtime.provider_environment.power.v1".into(),
            operation_id: "power-environment".into(),
            action,
            environment_id: target.identity.environment_id.clone(),
            creation_attempt_id: target.identity.attempt_id.clone(),
            sandbox_id: target.sandbox_id.clone(),
            provider_config_digest: target.identity.provider_config_digest.clone(),
            expected_controller_epoch: 2,
            predecessor_operation_id: match action {
                ProviderEnvironmentPowerAction::Pause => None,
                ProviderEnvironmentPowerAction::Resume => Some("original-pause".into()),
            },
        }
    }
    fn resume_receipt(target: &ProviderVerifiedTarget) -> Value {
        json!({"sandboxId":target.sandbox_id,"templateId":"template-fixture",
            "metadata":expected_metadata(&target.identity),"state":"running"})
    }

    #[test]
    fn pause_dispatches_one_full_request_with_original_identity_and_requires_fresh_ack() {
        let target = target();
        let result = dispatch_power(&target, &request(&target, ProviderEnvironmentPowerAction::Pause),
            &binding(&target), "template-fixture", 120, |operation, payload| {
                assert_eq!(operation, "pause");
                assert_eq!(payload, json!({"sandboxId":target.sandbox_id,"mode":"full",
                    "expectedTemplateId":"template-fixture","expectedMetadata":expected_metadata(&target.identity)}));
                Ok(json!({"sandboxId":target.sandbox_id,"requestedMode":"full","changed":true}))
            }).unwrap();
        assert!(
            matches!(result, ProviderEnvironmentPowerAcknowledgement::Paused {
            sandbox_id, requested_mode, changed: true
        } if sandbox_id == target.sandbox_id && requested_mode == "full")
        );
    }

    #[test]
    fn resume_dispatches_one_restore_request_and_verifies_the_exact_running_result() {
        let target = target();
        let result = dispatch_power(&target, &request(&target, ProviderEnvironmentPowerAction::Resume),
            &binding(&target), "template-fixture", 120, |operation, payload| {
                assert_eq!(operation, "connect");
                assert_eq!(payload, json!({"sandboxId":target.sandbox_id,"timeoutSeconds":120,"onResume":"restore",
                    "expectedTemplateId":"template-fixture","expectedMetadata":expected_metadata(&target.identity)}));
                Ok(resume_receipt(&target))
            }).unwrap();
        assert!(
            matches!(result, ProviderEnvironmentPowerAcknowledgement::Resumed { sandbox_id }
            if sandbox_id == target.sandbox_id)
        );
    }

    #[test]
    fn power_rejects_foreign_request_or_verified_target_before_any_provider_call() {
        for changed in [
            "sandbox",
            "attempt",
            "environment",
            "profile",
            "provider",
            "verification",
            "template",
            "timeout",
        ] {
            let mut target = target();
            let mut request = request(&target, ProviderEnvironmentPowerAction::Pause);
            let mut binding = binding(&target);
            let mut template = "template-fixture";
            let mut timeout = 120;
            match changed {
                "sandbox" => request.sandbox_id = "foreign-sandbox".into(),
                "attempt" => request.creation_attempt_id = "foreign-attempt".into(),
                "environment" => request.environment_id = "foreign-environment".into(),
                "profile" => binding.profile_digest = format!("sha256:{}", "f".repeat(64)),
                "provider" => binding.provider_config_digest = format!("sha256:{}", "f".repeat(64)),
                "verification" => target.verification_digest.clear(),
                "template" => template = "",
                "timeout" => timeout = 0,
                _ => unreachable!(),
            }
            let called = Cell::new(false);
            assert!(
                dispatch_power(&target, &request, &binding, template, timeout, |_, _| {
                    called.set(true);
                    Ok(json!({"sandboxId":target.sandbox_id,"requestedMode":"full","changed":true}))
                })
                .is_err(),
                "{changed}"
            );
            assert!(
                !called.get(),
                "foreign {changed} reached the provider mutation"
            );
        }
    }

    #[test]
    fn pause_already_paused_or_malformed_ack_never_establishes_a_fresh_full_snapshot() {
        let target = target();
        for receipt in [
            json!({"sandboxId":target.sandbox_id,"requestedMode":"full","changed":false}),
            json!({"sandboxId":target.sandbox_id,"requestedMode":"filesystem","changed":true}),
            json!({"sandboxId":"foreign-sandbox","requestedMode":"full","changed":true}),
            json!({"sandboxId":target.sandbox_id,"requestedMode":"full","changed":true,"observedMode":"full"}),
            json!({"sandboxId":target.sandbox_id,"requestedMode":"full","changed":1}),
            json!({"sandboxId":target.sandbox_id,"changed":true}),
        ] {
            let called = Cell::new(false);
            assert!(dispatch_power(
                &target,
                &request(&target, ProviderEnvironmentPowerAction::Pause),
                &binding(&target),
                "template-fixture",
                120,
                |operation, _| {
                    assert_eq!(operation, "pause");
                    called.set(true);
                    Ok(receipt)
                }
            )
            .is_err());
            assert!(
                called.get(),
                "the request was valid; its provider acknowledgement must be inspected"
            );
        }
    }

    #[test]
    fn resume_rejects_changed_identity_nonrunning_state_or_extra_preservation_claims() {
        let target = target();
        for changed in [
            "sandboxId",
            "templateId",
            "metadata",
            "state",
            "observedMode",
            "missingState",
        ] {
            let mut receipt = resume_receipt(&target);
            match changed {
                "metadata" => receipt["metadata"]["agentRunId"] = json!("successor-run-b"),
                "state" => receipt["state"] = json!("paused"),
                "observedMode" => receipt["observedMode"] = json!("full"),
                "missingState" => {
                    receipt.as_object_mut().unwrap().remove("state");
                }
                field => receipt[field] = json!("foreign-identity"),
            }
            let called = Cell::new(false);
            assert!(
                dispatch_power(
                    &target,
                    &request(&target, ProviderEnvironmentPowerAction::Resume),
                    &binding(&target),
                    "template-fixture",
                    120,
                    |operation, _| {
                        assert_eq!(operation, "connect");
                        called.set(true);
                        Ok(receipt)
                    }
                )
                .is_err(),
                "{changed}"
            );
            assert!(
                called.get(),
                "{changed} did not reach its single valid request"
            );
        }
    }

    #[test]
    fn power_unknown_preserves_the_single_attempt_without_inspection_or_mutation_retries() {
        let target = target();
        for action in [
            ProviderEnvironmentPowerAction::Pause,
            ProviderEnvironmentPowerAction::Resume,
        ] {
            let calls = Cell::new(0);
            assert!(dispatch_power(
                &target,
                &request(&target, action),
                &binding(&target),
                "template-fixture",
                120,
                |_, payload| {
                    calls.set(calls.get() + 1);
                    assert_eq!(payload["sandboxId"], target.sandbox_id);
                    Err("lost provider acknowledgement".into())
                }
            )
            .is_err());
            assert_eq!(calls.get(), 1);
        }
    }
}
