use super::*;

pub(crate) fn build_runtime_event_agent_run_intervention_changed(
    session_id: &str,
    turn_id: &str,
    change: &AgentRunInterventionChangedV1,
) -> Result<RuntimeEventProjection, String> {
    change.validate()?;
    build_typed_runtime_state_event(
        session_id,
        turn_id,
        format!(
            "agent_run_intervention:{}:{}",
            change.intervention_id,
            match change.status {
                AgentRunInterventionStatusV1::Requested => "requested",
                AgentRunInterventionStatusV1::Applied => "applied",
                AgentRunInterventionStatusV1::SatisfiedByFinal => "satisfied_by_final",
            }
        ),
        "AgentRunInterventionChanged",
        serde_json::to_value(change)
            .map_err(|error| format!("serialize AgentRun intervention event failed: {error}"))?,
        RuntimeProcessState::Synthesizing,
    )
}

pub(crate) fn build_runtime_event_runtime_wait_changed(
    session_id: &str,
    turn_id: &str,
    change: &RuntimeWaitChangedV1,
) -> Result<RuntimeEventProjection, String> {
    change.validate()?;
    build_typed_runtime_state_event(
        session_id,
        turn_id,
        format!(
            "runtime_wait:{}:{}",
            change.continuation_id,
            match change.status {
                RuntimeWaitStatusV1::Waiting => "waiting",
                RuntimeWaitStatusV1::Resumed => "resumed",
                RuntimeWaitStatusV1::Abandoned => "abandoned",
            }
        ),
        "RuntimeWaitChanged",
        serde_json::to_value(change)
            .map_err(|error| format!("serialize runtime wait event failed: {error}"))?,
        RuntimeProcessState::Waiting,
    )
}

fn build_typed_runtime_state_event(
    session_id: &str,
    turn_id: &str,
    event_id: String,
    event_type: &str,
    payload: Value,
    process_state: RuntimeProcessState,
) -> Result<RuntimeEventProjection, String> {
    let at_ms = payload
        .get("atMs")
        .and_then(Value::as_i64)
        .ok_or_else(|| format!("{event_type} payload is missing atMs"))?;
    let event_id = stable_session_event_id(
        "runtime_state",
        &[session_id, turn_id, event_type, event_id.as_str()],
    );
    let event = typed_runtime_event(json!({
        "id": event_id,
        "version": "v1",
        "type": event_type,
        "at": at_ms,
        "sessionId": session_id,
        "turnId": turn_id,
        "taskId": turn_id,
        "parentTaskId": turn_id,
        "status": "running",
        "visibility": "user",
        "processState": process_state.as_str(),
        "payload": payload,
        "meta": {
            "source": "core.agent_runtime",
        },
    }));
    Ok(event)
}

#[expect(
    clippy::too_many_arguments,
    reason = "status projection keeps exact event fields explicit"
)]
pub(crate) fn build_runtime_event_status_event(
    session_id: &str,
    turn_id: &str,
    task_id: &str,
    parent_task_id: &str,
    message: &str,
    status: &str,
    stage: Option<StatusStage>,
    transition_reason: Option<&str>,
) -> RuntimeEventProjection {
    let at_ms = now_ms();
    let process_state = process_state_for_status_event(stage, status, transition_reason).as_str();
    let at_ms_identity = at_ms.to_string();
    let event_id = stable_session_event_id(
        "status",
        &[session_id, turn_id, task_id, at_ms_identity.as_str()],
    );

    let mut payload = json!({
        "message": message,
    });
    if let Some(stage) = stage {
        if let Some(object) = payload.as_object_mut() {
            object.insert(
                "stage".to_string(),
                Value::String(stage.as_str().to_string()),
            );
        }
    }

    let mut meta = json!({
        "source": "core.agent_runtime",
    });
    if let Some(reason) = transition_reason {
        if let Some(object) = meta.as_object_mut() {
            object.insert(
                "transitionReason".to_string(),
                Value::String(reason.to_string()),
            );
        }
    }
    typed_runtime_event(json!({
        "id": event_id,
        "version": "v1",
        "type": "Status",
        "at": at_ms,
        "sessionId": session_id,
        "turnId": turn_id,
        "taskId": task_id,
        "parentTaskId": parent_task_id,
        "status": normalize_session_event_status(status),
        "visibility": "user",
        "processState": process_state,
        "payload": payload,
        "meta": meta,
    }))
}

fn process_state_for_status_event(
    stage: Option<StatusStage>,
    status: &str,
    transition_reason: Option<&str>,
) -> RuntimeProcessState {
    if matches!(normalize_session_event_status(status), "error" | "failed") {
        return RuntimeProcessState::Reviewing;
    }
    match stage {
        Some(StatusStage::QuestionWait) => RuntimeProcessState::Waiting,
        Some(StatusStage::ModelProcessSummary) => RuntimeProcessState::Synthesizing,
        None => match transition_reason.unwrap_or_default() {
            "await_question" => RuntimeProcessState::Waiting,
            _ => RuntimeProcessState::Thinking,
        },
    }
}

pub(crate) fn build_runtime_event_prompt_compaction_event(
    session_id: &str,
    turn_id: &str,
    task_id: &str,
    status: &str,
    summary: &str,
    detail: Option<&str>,
    payload: Value,
) -> RuntimeEventProjection {
    let at_ms = now_ms();
    let at_ms_identity = at_ms.to_string();
    let event_id = stable_session_event_id(
        "prompt_compaction",
        &[session_id, turn_id, task_id, at_ms_identity.as_str()],
    );
    typed_runtime_event(json!({
        "id": event_id,
        "version": "v1",
        "type": "PromptCompaction",
        "at": at_ms,
        "sessionId": session_id,
        "turnId": turn_id,
        "taskId": task_id,
        "parentTaskId": turn_id,
        "status": normalize_session_event_status(status),
        "visibility": "internal",
        "processState": RuntimeProcessState::Compressing.as_str(),
        "payload": {
            "summary": compact_text(summary, 240),
            "detail": detail.map(|item| compact_text(item, 600)),
            "compaction": payload,
        },
        "meta": {
            "source": "core.agent_runtime",
            "protocol": "prompt_compaction.v1",
            "compactLabel": compact_text(summary, 80),
        },
    }))
}

pub(crate) fn build_runtime_event_final_event(
    session_id: &str,
    turn_id: &str,
    content: &str,
    generate_result: Option<&GenerateResult>,
) -> RuntimeEventProjection {
    let at_ms = now_ms();
    let at_ms_identity = at_ms.to_string();
    let event_id =
        stable_session_event_id("final", &[session_id, turn_id, at_ms_identity.as_str()]);
    let model_usage = generate_result.and_then(build_model_usage_event_meta);
    typed_runtime_event(json!({
        "id": event_id,
        "version": "v1",
        "type": "Final",
        "at": at_ms,
        "sessionId": session_id,
        "turnId": turn_id,
        "taskId": turn_id,
        "parentTaskId": turn_id,
        "status": "done",
        "visibility": "user",
        "processState": RuntimeProcessState::Synthesizing.as_str(),
        "payload": {
            "content": content,
        },
        "meta": {
            "source": "core.agent_runtime",
            "modelUsage": model_usage,
        }
    }))
}

fn build_model_usage_event_meta(generate_result: &GenerateResult) -> Option<Value> {
    if generate_result.input_tokens.is_none()
        && generate_result.total_tokens.is_none()
        && generate_result.prompt_cache_hit_tokens.is_none()
        && generate_result.prompt_cache_miss_tokens.is_none()
    {
        return None;
    }
    let prompt_cache_total_tokens = match (
        generate_result.prompt_cache_hit_tokens,
        generate_result.prompt_cache_miss_tokens,
    ) {
        (Some(hit), Some(miss)) => Some(hit + miss),
        _ => None,
    };
    let prompt_cache_hit_rate = match (
        generate_result.prompt_cache_hit_tokens,
        prompt_cache_total_tokens,
    ) {
        (Some(hit), Some(total)) if total > 0 => Some(hit as f64 / total as f64),
        _ => None,
    };
    Some(json!({
        "inputTokens": generate_result.input_tokens,
        "totalTokens": generate_result.total_tokens,
        "promptCacheHitTokens": generate_result.prompt_cache_hit_tokens,
        "promptCacheMissTokens": generate_result.prompt_cache_miss_tokens,
        "promptCacheTotalTokens": prompt_cache_total_tokens,
        "promptCacheHitRate": prompt_cache_hit_rate,
    }))
}
