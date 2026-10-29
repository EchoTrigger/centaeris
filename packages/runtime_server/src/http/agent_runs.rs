use super::*;
mod control;
pub(crate) mod retained_cleanup;
pub(super) use control::advance as advance_control;

pub(super) fn cleanup_receipt(
    state: HttpServerState,
    request: Request<Bytes>,
) -> Result<Response<Body>, String> {
    cleanup_receipt_response(request.into_body().as_ref(), |query| {
        state.job_store.get_cleanup_receipt(query)
    })
}

fn cleanup_receipt_response(
    body: &[u8],
    read: impl FnOnce(
        &crate::postgres_store::cleanup_receipts::CleanupReceiptQuery,
    )
        -> Result<Option<crate::postgres_store::cleanup_receipts::CleanupReceipt>, String>,
) -> Result<Response<Body>, String> {
    let query = match serde_json::from_slice::<
        crate::postgres_store::cleanup_receipts::CleanupReceiptQuery,
    >(body)
    {
        Ok(query) if query.validate().is_ok() => query,
        _ => return json_error_response(400, "cleanup_receipt_query_invalid"),
    };
    match read(&query) {
        Ok(Some(receipt)) => Ok(http_response(
            200,
            "application/json",
            serde_json::to_vec(&receipt).map_err(|_| "cleanup receipt response encoding failed")?,
        )),
        Ok(None) => json_error_response(404, "cleanup_receipt_not_found"),
        Err(_) => json_error_response(503, "cleanup_receipt_unavailable"),
    }
}

pub(super) fn cancel(
    state: HttpServerState,
    request: Request<Bytes>,
) -> Result<Response<Body>, String> {
    let body = request.into_body();
    let job_store = state.job_store;
    let request = match serde_json::from_slice::<AgentRunCancelRequest>(body.as_ref())
        .map_err(|error| format!("invalid json: {error}"))
        .and_then(|request| {
            let signing_key = env::var("AGENT_RUN_AUTHORIZATION_SIGNING_KEY")
                .map_err(|_| "AGENT_RUN_AUTHORIZATION_SIGNING_KEY is required".to_string())?;
            request.validate(signing_key.as_bytes())
        }) {
        Ok(value) => value,
        Err(error) => {
            return json_error_response(400, error.as_str());
        }
    };
    let agent_run_id = request.agent_run_control.agent_run_id.clone();
    let terminal_state = match load_terminal_state_for_binding(
        job_store.as_ref(),
        &request.agent_run_control.agent_run_id,
        &request.agent_run_control.authorization,
        &request.agent_run_control.authorization_digest,
    ) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("load AgentRun cancellation terminal state failed: {error}");
            return json_error_response(500, "agent_run_cancel_state_unavailable");
        }
    };
    let (disposition, terminal_state) = if terminal_state.is_some() {
        ("terminal", terminal_state)
    } else {
        if let Err(error) = job_store.request_agent_run_cancellation(
            agent_run_id.as_str(),
            request.agent_run_control.authorization.session_id.as_str(),
            request.agent_run_control.authorization_digest.as_str(),
            now_ms()?,
        ) {
            match load_terminal_state_for_binding(
                job_store.as_ref(),
                &request.agent_run_control.agent_run_id,
                &request.agent_run_control.authorization,
                &request.agent_run_control.authorization_digest,
            ) {
                Ok(Some(state)) => ("terminal", Some(state)),
                Ok(None) => {
                    eprintln!("request AgentRun cancellation failed: {error}");
                    return json_error_response(409, "agent_run_cancel_rejected");
                }
                Err(reload_error) => {
                    eprintln!(
                        "reload AgentRun cancellation terminal state failed: {reload_error}; requestError={error}"
                    );
                    return json_error_response(500, "agent_run_cancel_state_unavailable");
                }
            }
        } else {
            ("requested", None)
        }
    };
    let response = serde_json::to_vec(&json!({
        "schema": "runtime.agent_run.cancel.result.v1",
        "agentRunId": agent_run_id,
        "disposition": disposition,
        "terminalState": terminal_state,
    }))
    .map_err(|error| format!("encode AgentRun cancel response failed: {error}"))?;
    Ok(http_response(200, "application/json", response))
}

pub(super) fn supplement(
    state: HttpServerState,
    request: Request<Bytes>,
) -> Result<Response<Body>, String> {
    let body = request.into_body();
    let job_store = state.job_store;
    let request = match serde_json::from_slice::<AgentRunSupplementRequest>(body.as_ref())
        .map_err(|error| format!("invalid json: {error}"))
        .and_then(|request| {
            let signing_key = env::var("AGENT_RUN_AUTHORIZATION_SIGNING_KEY")
                .map_err(|_| "AGENT_RUN_AUTHORIZATION_SIGNING_KEY is required".to_string())?;
            request.validate(signing_key.as_bytes())
        }) {
        Ok(value) => value,
        Err(error) => {
            return json_error_response(400, error.as_str());
        }
    };
    let result = job_store.enqueue_turn_supplement(EnqueueTurnSupplementRequest {
        agent_run_id: request.agent_run_control.agent_run_id.clone(),
        lifecycle_job_id: request.job_id,
        session_id: request.agent_run_control.authorization.session_id.clone(),
        authorization_digest: request.agent_run_control.authorization_digest.clone(),
        supplement_id: request.supplement_id.clone(),
        message: request.message,
        created_at_ms: now_ms()?,
    });
    let result = match result {
        Ok(value) => value,
        Err(error) => {
            let status = turn_supplement_http_status(&error);
            if status == 500 {
                eprintln!("AgentRun supplement admission failed: {error}");
            }
            return json_error_response(status, error.to_string().as_str());
        }
    };
    let disposition = match result.disposition {
        EnqueueTurnSupplementDisposition::Accepted => "accepted",
        EnqueueTurnSupplementDisposition::Duplicate => "duplicate",
    };
    let response = serde_json::to_vec(&json!({
        "schema": "runtime.agent_run.supplement.result.v1",
        "accepted": true,
        "disposition": disposition,
        "agentRunId": request.agent_run_control.agent_run_id,
        "sessionId": request.agent_run_control.authorization.session_id,
        "supplementId": request.supplement_id,
        "queuedCount": result.queued_count,
        "queueRevision": result.revision,
    }))
    .map_err(|error| format!("encode AgentRun supplement response failed: {error}"))?;
    Ok(http_response(202, "application/json", response))
}

pub(super) fn teardown(
    state: HttpServerState,
    request: Request<Bytes>,
) -> Result<Response<Body>, String> {
    let body = request.into_body();
    let envelope =
        match serde_json::from_slice::<crate::contract::AgentRunTeardownEnvelope>(body.as_ref()) {
            Ok(envelope) => envelope,
            Err(_) => return json_error_response(400, "agent_run_teardown_request_invalid"),
        };
    if envelope.schema != crate::contract::AGENT_RUN_TEARDOWN_SCHEMA
        || envelope.job_id.trim().is_empty()
        || envelope.lease_owner.len() < 16
        || envelope.lease_owner.len() > 160
        || envelope.lease_owner.chars().any(char::is_control)
    {
        return json_error_response(400, "agent_run_teardown_request_invalid");
    }
    teardown_owned_tasks(
        state,
        envelope.job_id,
        envelope.lease_owner,
        *envelope.agent_run_cleanup,
    )
}

fn teardown_owned_tasks(
    state: HttpServerState,
    job_id: String,
    lease_owner: String,
    cleanup: crate::contract::AgentRunCleanup,
) -> Result<Response<Body>, String> {
    use crate::postgres_store::cleanup_receipts::{CleanupAuthorityRequest, CleanupTarget};
    use centaeris_core::execution::{
        EnvironmentTaskControllerPurpose, EnvironmentTaskFence, EnvironmentTaskOwner,
    };
    let signing_key = match env::var("AGENT_RUN_AUTHORIZATION_SIGNING_KEY") {
        Ok(key) => key,
        Err(_) => {
            return cleanup_unavailable_response(
                &cleanup.agent_run_id,
                "teardown_signing_key",
                "cleanup_signing_key_unavailable",
                "retained_cleanup_unavailable",
            )
        }
    };
    let cleanup = match cleanup.validate(signing_key.as_bytes()) {
        Ok(cleanup) => cleanup,
        Err(_) => return json_error_response(400, "agent_run_cleanup_authority_invalid"),
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
        return json_error_response(409, "agent_run_cleanup_authority_rejected");
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
        return cleanup_unavailable_response(
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
        return cleanup_unavailable_response(
            &cleanup.agent_run_id,
            "teardown_terminal_subagents",
            &error,
            "terminal_subagent_cleanup_unavailable",
        );
    }
    let host_context = || {
        #[cfg(test)]
        if crate::native_consumption_tests::owns_run(&cleanup.agent_run_id) {
            return crate::native_consumption_tests::owned_host_context();
        }
        crate::docker_execution_host::resident_host_context()
    };
    let host_id = match host_context() {
        Ok((host_id, _)) => host_id,
        Err(error) => {
            return cleanup_unavailable_response(
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
        if crate::native_consumption_tests::owns_run(&cleanup.agent_run_id) {
            return crate::native_consumption_tests::owned_inventory();
        }
        crate::docker_execution_host::observed_resident_sandboxes_excluding(preparing)
    };
    #[cfg(test)]
    let native_fixture = crate::native_consumption_tests::owns_run(&cleanup.agent_run_id);
    #[cfg(not(test))]
    let native_fixture = false;
    let recovered = if cleanup.environment_authorization.payload.execution_mode == "perRun"
        && !native_fixture
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
            return cleanup_failure_response(&error);
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
                    crate::postgres_store::cleanup_preservation::CleanupRecoveryEvidence::Preserved { .. } => Ok(()),
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
                    if crate::native_consumption_tests::owns_run(&cleanup.agent_run_id) {
                        return crate::native_consumption_tests::cleanup_owned_physical_execution(
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
                    retained_cleanup::confirm_run_cleanup(
                        &retained_cleanup::DockerCleanupHost {
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
            return cleanup_failure_response(&error);
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
    Ok(http_response(
        200,
        "application/json",
        serde_json::to_vec(&json!({
            "schema":"runtime.agent_run.teardown.result.v1", "agentRunId":cleanup.agent_run_id,
            "status":"cleanupConfirmed", "cleanupReceiptId":receipt.receipt_id,
        }))
        .map_err(|_| "cleanup response encoding failed")?,
    ))
}

pub(super) fn step(
    state: HttpServerState,
    request: Request<Bytes>,
) -> Result<Response<Body>, String> {
    let body = request.into_body();
    let HttpServerState {
        runtime,
        store,
        job_store,
        tool_layers,
        ..
    } = state;
    let agent_run_step = match serde_json::from_slice::<AgentRunStepRequest>(body.as_ref())
        .map_err(|error| format!("invalid json: {error}"))
        .and_then(|request| {
            let signing_key = env::var("AGENT_RUN_AUTHORIZATION_SIGNING_KEY")
                .map_err(|_| "AGENT_RUN_AUTHORIZATION_SIGNING_KEY is required".to_string())?;
            request.validate(signing_key.as_bytes())
        }) {
        Ok(value) => value,
        Err(error) => {
            return json_error_response(400, error.as_str());
        }
    };
    if job_store
        .require_execute_lease(&agent_run_step.job_id, &agent_run_step.lease_owner)
        .is_err()
    {
        return json_error_response(409, "agent_run_execute_lease_required");
    }
    if let Err(error) = validate_agent_run_lifecycle_job(
        agent_run_step.job_id.as_str(),
        agent_run_step.lease_owner.as_str(),
        &agent_run_step.agent_run_start,
        job_store.as_ref(),
    ) {
        return json_error_response(409, error.as_str());
    }
    let agent_run_id = agent_run_step.agent_run_start.agent_run_id.clone();
    let failure_agent_run_start = agent_run_step.agent_run_start.clone();
    let failure_job_id = agent_run_step.job_id.clone();
    let failure_lease_owner = agent_run_step.lease_owner.clone();
    let failure_runtime = runtime.clone();
    let failure_store = store.clone();
    let failure_job_store = job_store.clone();
    let execution = catch_agent_run_step_panic(|| {
        execute_agent_run(
            agent_run_step.agent_run_start,
            runtime,
            store,
            job_store,
            agent_run_step.job_id,
            agent_run_step.lease_owner,
            tool_layers.clone(),
        )
    });
    if let Ok(Err(error)) = &execution {
        if let Some(response) = execution_control_failure_response(&agent_run_id, error) {
            return response;
        }
    }
    let outcome = match execution {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(error)) if error == "agent_run_lifecycle_lease_lost" => {
            return json_error_response(409, error.as_str());
        }
        Ok(Err(error)) if session_delivery::pending(&error) => {
            failure_diagnostics::report(agent_run_id.as_str(), "session_record_commit", &error);
            session_delivery_waiting()
        }
        Ok(Err(error)) => match terminalize_agent_run_failure(
            failure_runtime.as_ref(),
            failure_store.as_ref(),
            failure_job_store.as_ref(),
            &failure_agent_run_start,
            failure_job_id.as_str(),
            failure_lease_owner.as_str(),
            AgentRunFailure {
                internal_error: error.as_str(),
                transition_reason: "runtime_internal_error",
            },
        ) {
            Ok(outcome) => outcome,
            Err(terminal_error) => {
                let retryable = terminal_error == "runtime_completed_projection_requires_recovery";
                eprintln!(
                    "AgentRun failure terminalization failed: {terminal_error}; rootCause={error}"
                );
                return agent_run_step_failure_response(
                    agent_run_id.as_str(),
                    if retryable {
                        "recovery_required"
                    } else {
                        "runtime_internal"
                    },
                    retryable,
                    "runtime_failure_terminalization_failed",
                );
            }
        },
        Err(error) => {
            match catch_agent_run_step_panic(|| {
                terminalize_agent_run_failure(
                    failure_runtime.as_ref(),
                    failure_store.as_ref(),
                    failure_job_store.as_ref(),
                    &failure_agent_run_start,
                    failure_job_id.as_str(),
                    failure_lease_owner.as_str(),
                    AgentRunFailure {
                        internal_error: error.as_str(),
                        transition_reason: "runtime_panic",
                    },
                )
            }) {
                Ok(Ok(outcome)) => outcome,
                Ok(Err(terminal_error)) => {
                    let retryable =
                        terminal_error == "runtime_completed_projection_requires_recovery";
                    eprintln!(
                        "AgentRun panic terminalization failed: {terminal_error}; rootCause={error}"
                    );
                    return agent_run_step_failure_response(
                        agent_run_id.as_str(),
                        if retryable {
                            "recovery_required"
                        } else {
                            "runtime_panic"
                        },
                        retryable,
                        "runtime_failure_terminalization_failed",
                    );
                }
                Err(terminal_error) => {
                    eprintln!(
                        "AgentRun panic terminalization panicked: {terminal_error}; rootCause={error}"
                    );
                    return agent_run_step_failure_response(
                        agent_run_id.as_str(),
                        "runtime_panic",
                        false,
                        "runtime_failure_terminalization_failed",
                    );
                }
            }
        }
    };
    step_response(&agent_run_id, outcome)
}

fn cleanup_unavailable_response(
    agent_run_id: &str,
    stage: &'static str,
    error: &str,
    public_code: &'static str,
) -> Result<Response<Body>, String> {
    failure_diagnostics::report_cleanup(agent_run_id, stage, error);
    json_error_response(503, public_code)
}

fn cleanup_failure_response(error: &str) -> Result<Response<Body>, String> {
    if error == "environment_host_busy" || error == crate::resident_capacity::RESIDENT_CAPACITY_WAIT
    {
        json_error_response(409, error)
    } else {
        json_error_response(503, "retained_cleanup_unconfirmed")
    }
}

fn execution_control_failure_response(
    agent_run_id: &str,
    error: &str,
) -> Option<Result<Response<Body>, String>> {
    error
        .starts_with("run_execution_control_probe_failed:")
        .then(|| {
            agent_run_step_failure_response(
                agent_run_id,
                "execution_control_unavailable",
                true,
                "execution_control_unavailable",
            )
            .map(|mut response| {
                *response.status_mut() = axum::http::StatusCode::SERVICE_UNAVAILABLE;
                response
            })
        })
}

fn step_response(
    agent_run_id: &str,
    outcome: AgentRunStepOutcome,
) -> Result<Response<Body>, String> {
    outcome.validate()?;
    let mut response = json!({
        "schema": "runtime.agent_run.step.result.v1",
        "agentRunId": agent_run_id,
        "disposition": outcome.disposition,
        "terminalState": outcome.terminal_state,
        "transitionReason": outcome.transition_reason,
    });
    if let Some(retry_at_ms) = outcome.retry_at_ms {
        response["retryAtMs"] = json!(retry_at_ms);
    }
    let response = serde_json::to_vec(&response)
        .map_err(|error| format!("encode AgentRun step response failed: {error}"))?;
    Ok(http_response(200, "application/json", response))
}

#[cfg(test)]
mod cleanup_receipt_http_tests {
    use super::*;
    #[test]
    fn host_admission_busy_cleanup_is_an_exact_conflict_without_a_false_receipt() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        for (reason, expected_status, expected_code) in [
            ("environment_host_busy", 409, "environment_host_busy"),
            (
                "execution_resident_capacity_wait",
                409,
                "execution_resident_capacity_wait",
            ),
            (
                "execution_resident_capacity_wait:detail",
                503,
                "retained_cleanup_unconfirmed",
            ),
            (
                "execution_host_preparation_wait",
                503,
                "retained_cleanup_unconfirmed",
            ),
            (
                "cleanup_container_identity_missing",
                503,
                "retained_cleanup_unconfirmed",
            ),
            (
                "cleanup_physical_identity_rejected",
                503,
                "retained_cleanup_unconfirmed",
            ),
            (
                "cleanup_physical_absence_unconfirmed",
                503,
                "retained_cleanup_unconfirmed",
            ),
            (
                "cleanup_physical_release_unconfirmed",
                503,
                "retained_cleanup_unconfirmed",
            ),
            ("host_busy", 503, "retained_cleanup_unconfirmed"),
            (
                "environment_host_busy:unknown",
                503,
                "retained_cleanup_unconfirmed",
            ),
            (
                "environment_host_transport_unavailable",
                503,
                "retained_cleanup_unconfirmed",
            ),
        ] {
            let response = cleanup_failure_response(reason).unwrap();
            assert_eq!(response.status(), expected_status);
            let body = runtime
                .block_on(axum::body::to_bytes(response.into_body(), 4096))
                .unwrap();
            let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(value, json!({"error": expected_code}));
        }
    }

    #[test]
    fn unavailable_fresh_control_returns_retryable_503_before_terminalization() {
        let response = execution_control_failure_response(
            "run-a",
            "run_execution_control_probe_failed:offline",
        )
        .expect("a control uncertainty must bypass terminalization")
        .unwrap();
        assert_eq!(response.status(), 503);
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let bytes = runtime
            .block_on(axum::body::to_bytes(response.into_body(), 4096))
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["retryable"], true);
        assert_eq!(value["failureClass"], "execution_control_unavailable");
        assert!(execution_control_failure_response(
            "run-a",
            "private error mentions run_execution_control_probe_failed:offline"
        )
        .is_none());
    }
    #[test]
    fn current_step_response_matches_the_worker_contract() {
        let response = step_response(
            "run-a",
            AgentRunStepOutcome {
                disposition: "terminal",
                terminal_state: Some("completed"),
                transition_reason: "runtime_session_terminal_committed".into(),
                retry_at_ms: None,
            },
        )
        .unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let bytes = runtime
            .block_on(axum::body::to_bytes(response.into_body(), 4096))
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["schema"], "runtime.agent_run.step.result.v1");
        assert_eq!(value["terminalState"], "completed");
    }
    #[test]
    fn receipt_query_rejects_unsupported_schema_and_unknown_fields_before_storage() {
        let good = json!({"schema":"runtime.agent_run.cleanup_receipt.get.v1","agentRunId":"run-1",
            "authorizationDigest":format!("sha256:{}","a".repeat(64)),"cleanupReceiptId":"receipt-1"});
        for mutate in ["schema", "extra"] {
            let mut request = good.clone();
            request[mutate] = json!("unsupported");
            let response = cleanup_receipt_response(&serde_json::to_vec(&request).unwrap(), |_| {
                panic!("invalid request read storage")
            })
            .unwrap();
            assert_eq!(response.status(), 400);
        }
        let missing =
            cleanup_receipt_response(&serde_json::to_vec(&good).unwrap(), |_| Ok(None)).unwrap();
        assert_eq!(missing.status(), 404);
        let unknown = cleanup_receipt_response(&serde_json::to_vec(&good).unwrap(), |_| {
            Err("storage offline".into())
        })
        .unwrap();
        assert_eq!(unknown.status(), 503);
    }
}
