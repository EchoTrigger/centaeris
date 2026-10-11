use super::*;

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
    let agent_run_id = request.agent_run_start.agent_run_id.clone();
    let terminal_state =
        match load_existing_terminal_state(job_store.as_ref(), &request.agent_run_start) {
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
            request.agent_run_start.authorization.session_id.as_str(),
            request.agent_run_start.authorization_digest.as_str(),
            now_ms()?,
        ) {
            match load_existing_terminal_state(job_store.as_ref(), &request.agent_run_start) {
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
        agent_run_id: request.agent_run_start.agent_run_id.clone(),
        lifecycle_job_id: request.job_id,
        session_id: request.agent_run_start.authorization.session_id.clone(),
        authorization_digest: request.agent_run_start.authorization_digest.clone(),
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
        "agentRunId": request.agent_run_start.agent_run_id,
        "sessionId": request.agent_run_start.authorization.session_id,
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
    let HttpServerState {
        runtime,
        store,
        job_store,
        tool_layers,
        ..
    } = state;
    let teardown = match serde_json::from_slice::<AgentRunTeardownRequest>(body.as_ref())
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
    if let Err(error) = validate_agent_run_lifecycle_job(
        teardown.job_id.as_str(),
        teardown.lease_owner.as_str(),
        &teardown.agent_run_start,
        job_store.as_ref(),
    ) {
        return json_error_response(409, error.as_str());
    }
    if load_existing_terminal_state(job_store.as_ref(), &teardown.agent_run_start)?.is_none() {
        return json_error_response(409, "agent_run_not_terminal");
    }
    if let Err(error) = job_store.close_turn_supplement_queue(CloseTurnSupplementQueueRequest {
        agent_run_id: teardown.agent_run_start.agent_run_id.clone(),
        lifecycle_job_id: teardown.job_id.clone(),
        session_id: teardown.agent_run_start.authorization.session_id.clone(),
        authorization_digest: teardown.agent_run_start.authorization_digest.clone(),
        lease_owner: Some(teardown.lease_owner.clone()),
        reason: "agent_run_terminal".to_string(),
        closed_at_ms: now_ms()?,
    }) {
        eprintln!("turn supplement terminal cleanup failed: {error}");
        return json_error_response(500, "agent_run_supplement_cleanup_failed");
    }
    if let Err(error) = runtime.block_on(terminal_subagents::settle_terminal_parent_subagent_jobs(
        store.as_ref(),
        &teardown.agent_run_start.authorization.session_id,
        &teardown.agent_run_start.agent_run_id,
        now_ms()?,
    )) {
        failure_diagnostics::report(
            &teardown.agent_run_start.agent_run_id,
            "terminal_subagent_cleanup",
            &error,
        );
        // The parent terminal fact remains committed. A retry repeats only
        // teardown and retains the context until child cancellation commits.
        return json_error_response(503, "terminal_subagent_cleanup_unavailable");
    }
    if let Err(error) = DockerExecutionHostRunner::teardown(
        teardown.agent_run_start.agent_run_id.as_str(),
        job_store.as_ref(),
        &RuntimeJobLeaseFence {
            job_id: teardown.job_id.clone(),
            job_kind: AGENT_RUN_LIFECYCLE_JOB_KIND.into(),
            lease_owner: teardown.lease_owner.clone(),
        },
    ) {
        eprintln!("sandbox teardown failed: {error}");
        return json_error_response(500, "sandbox_teardown_failed");
    }
    let mut contexts = tool_layers
        .lock()
        .map_err(|_| "provider poll tool layer registry lock poisoned".to_string())?;
    let session_id = teardown.agent_run_start.authorization.session_id.as_str();
    if contexts
        .get(session_id)
        .is_some_and(|context| context.agent_run_id == teardown.agent_run_start.agent_run_id)
    {
        contexts.remove(session_id);
    }
    let response = serde_json::to_vec(&json!({
        "schema": "runtime.agent_run.teardown.result.v1",
        "agentRunId": teardown.agent_run_start.agent_run_id,
        "status": "removed",
    }))
    .map_err(|error| format!("encode AgentRun teardown response failed: {error}"))?;
    Ok(http_response(200, "application/json", response))
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
