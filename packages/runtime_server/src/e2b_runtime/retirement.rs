//! Independently authorized provider retirement; a Run never supplies authority.
use super::*;
use crate::e2b_run_registry::E2bEnvironmentBinding;
use crate::postgres_store::provider_operations::cleanup::ProviderRemovalEvidence;
use crate::postgres_store::provider_operations::retained::retirement::{
    ProviderEnvironmentRetirementRequest, ProviderEnvironmentRetirementResult,
};

impl E2bRuntime {
    /// Pure binding check for taking an existing live guard before Store pins.
    /// Maintenance never constructs an execution context or a Run authority.
    pub(crate) fn retirement_binding(
        &self,
        request: &ProviderEnvironmentRetirementRequest,
    ) -> Result<E2bEnvironmentBinding, E2bRuntimeError> {
        request
            .validate()
            .map_err(|_| E2bRuntimeError::Rejected("e2b_retirement_request_invalid"))?;
        let EnvironmentProfile::E2b(profile) = self.config.profile() else {
            return Err(E2bRuntimeError::Rejected("e2b_retirement_profile_rejected"));
        };
        if profile.lifetime != "retained"
            || request.provider_config_digest != self.config.provider_config_digest
        {
            return Err(E2bRuntimeError::Rejected("e2b_retirement_binding_rejected"));
        }
        Ok(E2bEnvironmentBinding {
            environment_id: request.environment_id.clone(),
            profile_digest: self
                .config
                .profile()
                .digest()
                .map_err(|_| E2bRuntimeError::Rejected("e2b_retirement_profile_rejected"))?,
            provider_config_digest: self.config.provider_config_digest.clone(),
        })
    }

    pub(crate) fn retire(
        &self,
        request: &ProviderEnvironmentRetirementRequest,
    ) -> Result<ProviderEnvironmentRetirementResult, E2bRuntimeError> {
        let binding = self.retirement_binding(request)?;
        // The Store's durable intent, immutable verified target and current
        // Environment pins authorize this exact physical callback.
        // Replaying a settled retirement never launches the cleanup adapter.
        let mut lane = cleanup::CleanupLane::new(self);
        self.store
            .retire_provider_environment(request, &self.config.provider_config_digest, |target| {
                remove_verified(target, request, &binding, |id| lane.remove(id))
            })
            .map_err(|_| E2bRuntimeError::Unknown("e2b_retirement_unconfirmed"))
    }
}

fn remove_verified(
    target: &ProviderVerifiedTarget,
    request: &ProviderEnvironmentRetirementRequest,
    binding: &E2bEnvironmentBinding,
    remove: impl FnOnce(&str) -> Result<ProviderRemovalEvidence, String>,
) -> Result<ProviderRemovalEvidence, String> {
    request
        .validate()
        .map_err(|_| "e2b_retirement_request_invalid")?;
    if target.identity.attempt_id != request.creation_attempt_id
        || target.sandbox_id != request.sandbox_id
        || target.identity.environment_id != request.environment_id
        || binding.environment_id != request.environment_id
        || target.identity.profile_digest != binding.profile_digest
        || target.identity.provider_config_digest != binding.provider_config_digest
        || request.provider_config_digest != binding.provider_config_digest
        || target.verification_digest.is_empty()
    {
        return Err("e2b_retirement_target_rejected".into());
    }
    // No fresh GET: the target was durably verified before this operation.
    // A second exact kill can report NotFound after a lost first kill reply.
    let evidence = remove(&target.sandbox_id)?;
    let (ProviderRemovalEvidence::Deleted { sandbox_id }
    | ProviderRemovalEvidence::NotFound { sandbox_id }) = &evidence;
    if sandbox_id != &target.sandbox_id {
        return Err("e2b_retirement_removal_identity_rejected".into());
    }
    Ok(evidence)
}

#[cfg(test)]
mod tests {
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

    fn request(target: &ProviderVerifiedTarget) -> ProviderEnvironmentRetirementRequest {
        ProviderEnvironmentRetirementRequest {
            schema: "runtime.provider_environment.retire.v1".into(),
            operation_id: "retire-environment".into(),
            environment_id: target.identity.environment_id.clone(),
            creation_attempt_id: target.identity.attempt_id.clone(),
            sandbox_id: target.sandbox_id.clone(),
            provider_config_digest: target.identity.provider_config_digest.clone(),
            expected_controller_epoch: 2,
            acknowledge_data_loss: true,
        }
    }

    #[test]
    fn retirement_rejects_changed_request_or_verified_binding_before_delete() {
        for changed in [
            "sandbox",
            "attempt",
            "environment",
            "profile",
            "provider",
            "verification",
            "acknowledgement",
        ] {
            let mut target = target();
            let mut request = request(&target);
            let mut binding = binding(&target);
            match changed {
                "sandbox" => request.sandbox_id = "foreign-sandbox".into(),
                "attempt" => request.creation_attempt_id = "foreign-attempt".into(),
                "environment" => request.environment_id = "other-environment".into(),
                "profile" => binding.profile_digest = format!("sha256:{}", "f".repeat(64)),
                "provider" => binding.provider_config_digest = format!("sha256:{}", "f".repeat(64)),
                "verification" => target.verification_digest.clear(),
                "acknowledgement" => request.acknowledge_data_loss = false,
                _ => unreachable!(),
            }
            let called = Cell::new(false);
            let result = remove_verified(&target, &request, &binding, |id| {
                called.set(true);
                Ok(ProviderRemovalEvidence::Deleted {
                    sandbox_id: id.into(),
                })
            });
            assert!(result.is_err(), "{changed}");
            assert!(
                !called.get(),
                "foreign {changed} reached the provider mutation"
            );
        }
    }

    #[test]
    fn retirement_accepts_only_exact_typed_removal_and_preserves_unknown() {
        let target = target();
        for absent in [false, true] {
            let result = remove_verified(&target, &request(&target), &binding(&target), |id| {
                assert_eq!(id, target.sandbox_id);
                Ok(if absent {
                    ProviderRemovalEvidence::NotFound {
                        sandbox_id: id.into(),
                    }
                } else {
                    ProviderRemovalEvidence::Deleted {
                        sandbox_id: id.into(),
                    }
                })
            })
            .unwrap();
            match result {
                ProviderRemovalEvidence::Deleted { sandbox_id }
                | ProviderRemovalEvidence::NotFound { sandbox_id } => {
                    assert_eq!(sandbox_id, target.sandbox_id);
                }
            }
        }
        assert!(
            remove_verified(&target, &request(&target), &binding(&target), |_| {
                Err("provider outcome unknown".into())
            })
            .is_err()
        );
        assert!(
            remove_verified(&target, &request(&target), &binding(&target), |_| {
                Ok(ProviderRemovalEvidence::Deleted {
                    sandbox_id: "foreign-sandbox".into(),
                })
            })
            .is_err()
        );
    }

    #[test]
    fn lost_kill_reply_replays_only_the_same_verified_id_and_accepts_typed_not_found() {
        let target = target();
        let calls = std::cell::RefCell::new(Vec::new());
        assert!(
            remove_verified(&target, &request(&target), &binding(&target), |id| {
                calls.borrow_mut().push(("kill", id.to_owned()));
                Err("lost provider acknowledgement".into())
            })
            .is_err()
        );
        let result = remove_verified(&target, &request(&target), &binding(&target), |id| {
            calls.borrow_mut().push(("kill", id.to_owned()));
            Ok(ProviderRemovalEvidence::NotFound {
                sandbox_id: id.into(),
            })
        })
        .unwrap();
        assert!(
            matches!(result, ProviderRemovalEvidence::NotFound { sandbox_id } if sandbox_id == target.sandbox_id)
        );
        assert_eq!(
            *calls.borrow(),
            vec![
                ("kill", target.sandbox_id.clone()),
                ("kill", target.sandbox_id)
            ]
        );
    }
}
