use super::*;

pub(super) async fn route_after_generate_with_retry<S>(
    checkpoint_store: &TurnCheckpointStore<S>,
    mut req: RouteGenerateResultRequest,
    stage: &str,
    max_recovery_attempts: u32,
    recovery_policy_trace_json: &mut Vec<String>,
) -> Result<RouteGenerateResultResponse, String>
where
    S: RuntimeStore + RuntimeStoreTransactionPort + Clone + Send + 'static,
{
    let mut retries = 0u32;

    loop {
        merge_recovery_traces(
            &mut req.state.recovery_policy_trace_json,
            recovery_policy_trace_json.as_slice(),
        );

        match checkpoint_store
            .route_after_generate_async(req.clone())
            .await
        {
            Ok(response) => return Ok(response),
            Err(err) => {
                let Some((policy, priority)) = classify_retry_recovery_policy(err.as_str()) else {
                    return Err(err);
                };
                if retries >= max_recovery_attempts {
                    let trace = build_recovery_trace(
                        policy,
                        priority,
                        stage,
                        "retry_exhausted",
                        retries,
                        max_recovery_attempts,
                        err.as_str(),
                    );
                    append_recovery_trace(recovery_policy_trace_json, trace.clone());
                    append_recovery_trace(&mut req.state.recovery_policy_trace_json, trace);
                    return Err(err);
                }

                retries = retries.saturating_add(1);
                let trace = build_recovery_trace(
                    policy,
                    priority,
                    stage,
                    "retry_stage",
                    retries,
                    max_recovery_attempts,
                    err.as_str(),
                );
                append_recovery_trace(recovery_policy_trace_json, trace.clone());
                append_recovery_trace(&mut req.state.recovery_policy_trace_json, trace);
            }
        }
    }
}

pub(super) async fn persist_query_state_with_retry<S>(
    checkpoint_store: &TurnCheckpointStore<S>,
    mut req: PersistQueryStateRequest,
    stage: &str,
    max_recovery_attempts: u32,
    recovery_policy_trace_json: &mut Vec<String>,
) -> Result<SubmitTurnResponse, String>
where
    S: RuntimeStore + RuntimeStoreTransactionPort + Clone + Send + 'static,
{
    let mut retries = 0u32;

    loop {
        merge_recovery_traces(
            &mut req.state.recovery_policy_trace_json,
            recovery_policy_trace_json.as_slice(),
        );

        match checkpoint_store
            .persist_query_state_async(req.clone())
            .await
        {
            Ok(response) => return Ok(response),
            Err(err) => {
                let Some((policy, priority)) = classify_retry_recovery_policy(err.as_str()) else {
                    return Err(err);
                };
                if retries >= max_recovery_attempts {
                    let trace = build_recovery_trace(
                        policy,
                        priority,
                        stage,
                        "retry_exhausted",
                        retries,
                        max_recovery_attempts,
                        err.as_str(),
                    );
                    append_recovery_trace(recovery_policy_trace_json, trace.clone());
                    append_recovery_trace(&mut req.state.recovery_policy_trace_json, trace);
                    return Err(err);
                }

                retries = retries.saturating_add(1);
                let trace = build_recovery_trace(
                    policy,
                    priority,
                    stage,
                    "retry_stage",
                    retries,
                    max_recovery_attempts,
                    err.as_str(),
                );
                append_recovery_trace(recovery_policy_trace_json, trace.clone());
                append_recovery_trace(&mut req.state.recovery_policy_trace_json, trace);
            }
        }
    }
}

pub(super) fn classify_retry_recovery_policy(error_text: &str) -> Option<(&'static str, i32)> {
    if let Some((policy, priority)) = classify_recovery_policy(error_text) {
        return Some((policy, priority));
    }

    let normalized = error_text.to_ascii_lowercase();
    if normalized.contains("database is locked") || normalized.contains("database busy") {
        return Some(("storage_busy", 75));
    }
    if normalized.contains("timeout") || normalized.contains("timed out") {
        return Some(("stage_timeout", 65));
    }
    if normalized.contains("temporarily unavailable")
        || normalized.contains("connection reset")
        || normalized.contains("connection aborted")
    {
        return Some(("transient_io", 60));
    }

    None
}

pub(super) fn build_recovery_trace(
    policy: &str,
    priority: i32,
    stage: &str,
    action: &str,
    retry_count: u32,
    max_recovery_attempts: u32,
    error_text: &str,
) -> String {
    json!({
        "policy": policy,
        "priority": priority,
        "stage": stage,
        "action": action,
        "meta": {
            "retryCount": retry_count,
            "maxRecoveryAttempts": max_recovery_attempts,
            "error": error_text,
        },
        "timestamp": now_ms(),
    })
    .to_string()
}

pub(super) fn append_recovery_trace(traces: &mut Vec<String>, trace: String) {
    if traces.iter().any(|item| item == &trace) {
        return;
    }
    traces.push(trace);
}

pub(super) fn merge_recovery_traces(target: &mut Vec<String>, incoming: &[String]) {
    for item in incoming {
        append_recovery_trace(target, item.clone());
    }
}
