use super::*;
pub(crate) mod control;
pub(crate) mod finalization;
pub(crate) mod retained_cleanup;
pub(super) use control::advance as advance_control;

pub(super) fn question_response(
    state: HttpServerState,
    request: Request<Bytes>,
) -> Result<Response<Body>, String> {
    let request = match serde_json::from_slice::<crate::contract::AgentRunQuestionResponseRequest>(
        request.body(),
    )
    .map_err(|error| error.to_string())
    .and_then(|value| {
        let key = env::var("AGENT_RUN_AUTHORIZATION_SIGNING_KEY")
            .map_err(|_| "AGENT_RUN_AUTHORIZATION_SIGNING_KEY is required".to_string())?;
        value.validate(key.as_bytes())
    }) {
        Ok(value) => value,
        Err(error) => return json_error_response(400, &error),
    };
    let response = request.response();
    match state.job_store.respond_to_question(
        &request.agent_run_control.authorization.session_id,
        &request.job_id,
        &response,
        now_ms()?,
    ) {
        Ok(disposition) => Ok(http_response(
            200,
            "application/json",
            serde_json::to_vec(&json!({
                "schema":"runtime.agent_run.question_response.result.v1",
                "agentRunId":response.agent_run_id,"continuationId":response.continuation_id,
                "operationId":response.operation_id,"disposition":disposition,
            }))
            .map_err(|error| error.to_string())?,
        )),
        Err(error) => {
            eprintln!("question response rejected: {error}");
            json_error_response(409, "question_response_rejected")
        }
    }
}

pub(super) fn cleanup_receipt(
    state: HttpServerState,
    request: Request<Bytes>,
) -> Result<Response<Body>, String> {
    cleanup_receipt_response(request.into_body().as_ref(), |query| {
        state.job_store.get_cleanup_receipt(query)
    })
}

pub(super) fn agent_run_api_purge(
    state: HttpServerState,
    request: Request<Bytes>,
) -> Result<Response<Body>, String> {
    let parsed = serde_json::from_slice::<
        crate::postgres_store::agent_run_api_purge::AgentRunApiPurgeRequest,
    >(request.body());
    let query = match parsed.and_then(|query| {
        let key = env::var("AGENT_RUN_AUTHORIZATION_SIGNING_KEY")
            .map_err(|_| serde::de::Error::custom("signing key unavailable"))?;
        query
            .validate(key.as_bytes())
            .map_err(serde::de::Error::custom)?;
        Ok(query)
    }) {
        Ok(query) => query,
        Err(_) => return json_error_response(400, "agent_run_api_purge_request_invalid"),
    };
    match state
        .job_store
        .purge_agent_run_api_session(&query.agent_run_control)
    {
        Ok(()) => {
            state
                .tool_layers
                .lock()
                .map_err(|_| "Agent Run API runtime context lock poisoned")?
                .remove(&query.agent_run_control.authorization.session_id);
            Ok(http_response(
                200,
                "application/json",
                serde_json::to_vec(&json!({
                    "schema":"runtime.agent_run.api_purged.v1",
                    "agentRunId":query.agent_run_control.agent_run_id,
                    "sessionId":query.agent_run_control.authorization.session_id,
                    "purged":true,
                }))
                .map_err(|e| e.to_string())?,
            ))
        }
        Err(error) => {
            eprintln!("Agent Run API content purge pending: {error}");
            json_error_response(409, "agent_run_api_purge_pending")
        }
    }
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
    let run = envelope.agent_run_cleanup.agent_run_id.clone();
    match crate::agent_run_cleanup::teardown(
        state,
        envelope.job_id,
        envelope.lease_owner,
        *envelope.agent_run_cleanup,
    ) {
        Ok(receipt) => Ok(http_response(
            200,
            "application/json",
            serde_json::to_vec(&json!({
                "schema": "runtime.agent_run.teardown.result.v1", "agentRunId": run,
                "status": "cleanupConfirmed", "cleanupReceiptId": receipt,
            }))
            .map_err(|error| error.to_string())?,
        )),
        Err(error) => error.response(),
    }
}

pub(super) fn step(
    state: HttpServerState,
    request: Request<Bytes>,
) -> Result<Response<Body>, String> {
    let body = request.into_body();
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
    let run = agent_run_step.agent_run_start.agent_run_id.clone();
    match crate::agent_run_execution::step(state, agent_run_step) {
        Ok(outcome) => step_response(&run, outcome),
        Err(error) => error.response(),
    }
}

#[cfg(test)]
fn cleanup_failure_response(error: &str) -> Result<Response<Body>, String> {
    crate::agent_run_cleanup::failure(error).response()
}

#[cfg(test)]
fn execution_control_failure_response(
    run: &str,
    error: &str,
) -> Option<Result<Response<Body>, String>> {
    crate::agent_run_execution::control_failure(run, error).map(|failure| failure.response())
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
