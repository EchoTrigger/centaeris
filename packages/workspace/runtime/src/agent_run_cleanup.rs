//! Fenced cleanup shared by native lifecycle execution and HTTP.
use crate::agent_run_execution::Failure;
use crate::*;

pub(crate) fn teardown(
    state: HttpServerState,
    job_id: String,
    lease_owner: String,
    cleanup: crate::contract::AgentRunCleanup,
) -> Result<String, Failure> {
    use crate::postgres_store::cleanup_receipts::{CleanupAuthorityRequest, CleanupTarget};
    use centaeris_core::execution::{
        EnvironmentTaskControllerPurpose, EnvironmentTaskFence, EnvironmentTaskOwner,
    };
    let signing_key = match env::var("AGENT_RUN_AUTHORIZATION_SIGNING_KEY") {
        Ok(key) => key,
        Err(_) => {
            return unavailable(
                &cleanup.agent_run_id,
                "teardown_signing_key",
                "cleanup_signing_key_unavailable",
                "retained_cleanup_unavailable",
            )
        }
    };
    let cleanup = match cleanup.validate(signing_key.as_bytes()) {
        Ok(cleanup) => cleanup,
        Err(_) => {
            return Err(Failure::rejected(
                400,
                "agent_run_cleanup_authority_invalid",
            ))
        }
    };
    let lease = RuntimeJobLeaseFence {
        job_id,
        job_kind: AGENT_RUN_LIFECYCLE_JOB_KIND.into(),
        lease_owner,
    };
    if state
        .job_store
        .check_cleanup_authority(&cleanup, signing_key.as_bytes(), &lease)
        .is_err()
    {
        return Err(Failure::rejected(
            409,
            "agent_run_cleanup_authority_rejected",
        ));
    }
    // The short pin above has ended: these operations use separate connections.
    // Physical cleanup below pins and rechecks the current lease and epoch.
    if let Err(error) =
        state
            .job_store
            .close_turn_supplement_queue(CloseTurnSupplementQueueRequest {
                agent_run_id: cleanup.agent_run_id.clone(),
                lifecycle_job_id: lease.job_id.clone(),
                session_id: cleanup.authorization.session_id.clone(),
                authorization_digest: cleanup.authorization_digest.clone(),
                lease_owner: Some(lease.lease_owner.clone()),
                reason: "agent_run_terminal".into(),
                closed_at_ms: now_ms()?,
            })
    {
        return unavailable(
            &cleanup.agent_run_id,
            "teardown_supplement_queue",
            &error.to_string(),
            "agent_run_supplement_cleanup_failed",
        );
    }
    if let Err(error) =
        state
            .runtime
            .block_on(terminal_subagents::settle_terminal_parent_subagent_jobs(
                state.store.as_ref(),
                &cleanup.authorization.session_id,
                &cleanup.agent_run_id,
                now_ms()?,
            ))
    {
        return unavailable(
            &cleanup.agent_run_id,
            "teardown_terminal_subagents",
            &error,
            "terminal_subagent_cleanup_unavailable",
        );
    }
    let host_context = || {
        #[cfg(test)]
        if crate::environment_runtime_tests::owns_run(&cleanup.agent_run_id) {
            return crate::environment_runtime_tests::owned_host_context();
        }
        crate::docker_execution_host::resident_host_context()
    };
    let host_id = match host_context() {
        Ok((host_id, _)) => host_id,
        Err(error) => {
            return unavailable(
                &cleanup.agent_run_id,
                "teardown_host_context",
                &error,
                "retained_cleanup_unavailable",
            )
        }
    };
    let authority = CleanupAuthorityRequest {
        cleanup: &cleanup,
        signing_key: signing_key.as_bytes(),
        lease: &lease,
        host_id: &host_id,
    };
    let inventory = |preparing: &[crate::resident_capacity::PreparingResidentSandbox]| {
        #[cfg(test)]
        if crate::environment_runtime_tests::owns_run(&cleanup.agent_run_id) {
            return crate::environment_runtime_tests::owned_inventory();
        }
        crate::docker_execution_host::observed_resident_sandboxes_excluding(preparing)
    };
    #[cfg(test)]
    let owned_fixture = crate::environment_runtime_tests::owns_run(&cleanup.agent_run_id);
    #[cfg(not(test))]
    let owned_fixture = false;
    let recovered = if cleanup.environment_authorization.payload.execution_mode == "perRun"
        && !owned_fixture
        && crate::docker_execution_host::OciRuntime::from_environment()?
            == crate::docker_execution_host::OciRuntime::Runsc
    {
        state
            .job_store
            .with_cleanup_target(
                &authority,
                crate::resident_capacity::FilteredResidentInventory(inventory),
                |target, _| crate::docker_execution_host::stopped_recovery::prepare(target),
            )
            .and_then(|prepared| {
                prepared
                    .flatten()
                    .map(|prepared| {
                        let api_url = env::var("API_INTERNAL_URL")
                            .map_err(|_| "API_INTERNAL_URL is required")?;
                        let api_token = env::var("INTERNAL_API_TOKEN")
                            .map_err(|_| "INTERNAL_API_TOKEN is required")?;
                        prepared.publish(&api_url, &api_token, &cleanup, &lease)
                    })
                    .transpose()
            })
    } else {
        Ok(None)
    };
    let recovery = match recovered {
        Ok(value) => value,
        Err(error) => {
            failure_diagnostics::report_cleanup(
                &cleanup.agent_run_id,
                "teardown_workspace_preservation",
                &error,
            );
            return Err(failure(&error));
        }
    };
    let physical_cleanup =
        |target: &CleanupTarget,
         verified: &crate::postgres_store::controller_authority::VerifiedOriginalGrant| {
            if let Some(recovery) = recovery.as_ref() {
                let CleanupTarget::Instance {
                    allocation,
                    origin_authorization,
                    ..
                } = target
                else {
                    return Err("cleanup_recovery_target_changed".into());
                };
                return crate::docker_execution_host::runsc_power::remove_stopped_after_preservation(
                allocation, &origin_authorization.environment_profile, || match &recovery.evidence {
                    crate::postgres_store::cleanup_preservation::CleanupRecoveryEvidence::Preserved { .. }
                    | crate::postgres_store::cleanup_preservation::CleanupRecoveryEvidence::PreAdmissionFailed => Ok(()),
                    crate::postgres_store::cleanup_preservation::CleanupRecoveryEvidence::Absent => Err("stopped_workspace_preservation_required".into()),
                },
            );
            }
            match target {
                CleanupTarget::NeverAllocated
                | CleanupTarget::Destroyed { .. }
                | CleanupTarget::Unmaterialized { .. } => {
                    Ok(crate::postgres_store::cleanup_receipts::CleanupPhysicalResult::Retained)
                }
                CleanupTarget::Instance {
                    allocation,
                    resources,
                    origin_authorization,
                    ..
                } => {
                    let fence = EnvironmentTaskFence {
                        environment_instance_id: allocation.instance_id.clone(),
                        controller_owner: EnvironmentTaskOwner::AgentRun {
                            run_id: cleanup.agent_run_id.clone(),
                        },
                        controller_epoch: verified
                            .controller_epoch
                            .try_into()
                            .map_err(|_| "cleanup epoch invalid")?,
                        controller_purpose: EnvironmentTaskControllerPurpose::Cleanup,
                    };
                    let per_run =
                        cleanup.environment_authorization.payload.execution_mode == "perRun";

                    #[cfg(test)]
                    if crate::environment_runtime_tests::owns_run(&cleanup.agent_run_id) {
                        return crate::environment_runtime_tests::cleanup_owned_physical_execution(
                            allocation, *resources, &fence, per_run,
                        );
                    }
                    // A prior release may have completed before its final DB receipt.
                    // Only the exact recorded container's independent absence closes it.
                    if per_run
                        && allocation
                            .container_id
                            .as_deref()
                            .is_some_and(|id| matches!(crate::docker_engine::inspect(id), Ok(None)))
                    {
                        return crate::docker_execution_host::remove_cleaned_environment(
                            allocation,
                        );
                    }
                    crate::http::agent_runs::retained_cleanup::confirm_run_cleanup(
                        &crate::http::agent_runs::retained_cleanup::DockerCleanupHost {
                            cleanup: &cleanup,
                            lease: &lease,
                            allocation,
                            resources: *resources,
                            origin_authorization: origin_authorization.as_ref(),
                        },
                        &fence,
                    )?;
                    if per_run {
                        crate::docker_execution_host::remove_cleaned_environment(allocation)
                    } else {
                        Ok(crate::postgres_store::cleanup_receipts::CleanupPhysicalResult::Retained)
                    }
                }
            }
        };
    let result = if let Some(recovery) = recovery.as_ref() {
        state.job_store.with_recovery_cleanup_authority(
            &authority,
            crate::resident_capacity::FilteredResidentInventory(inventory),
            recovery,
            physical_cleanup,
        )
    } else {
        state.job_store.with_cleanup_authority(
            &authority,
            crate::resident_capacity::FilteredResidentInventory(inventory),
            physical_cleanup,
        )
    };
    let receipt = match result {
        Ok(receipt) => receipt,
        Err(error) => {
            failure_diagnostics::report_cleanup(
                &cleanup.agent_run_id,
                "teardown_cleanup_authority",
                &error,
            );
            return Err(failure(&error));
        }
    };
    let mut contexts = state
        .tool_layers
        .lock()
        .map_err(|_| "provider poll tool layer registry lock poisoned".to_string())?;
    let session_id = cleanup.authorization.session_id.as_str();
    if contexts
        .get(session_id)
        .is_some_and(|context| context.agent_run_id == cleanup.agent_run_id)
    {
        contexts.remove(session_id);
    }
    drop(contexts);
    Ok(receipt.receipt_id)
}

fn unavailable<T>(
    run: &str,
    stage: &'static str,
    error: &str,
    code: &'static str,
) -> Result<T, Failure> {
    failure_diagnostics::report_cleanup(run, stage, error);
    Err(Failure::rejected(503, code))
}

pub(crate) fn failure(error: &str) -> Failure {
    if error == "environment_host_busy" || error == crate::resident_capacity::RESIDENT_CAPACITY_WAIT
    {
        Failure::rejected(409, error)
    } else {
        Failure::rejected(503, "retained_cleanup_unconfirmed")
    }
}
