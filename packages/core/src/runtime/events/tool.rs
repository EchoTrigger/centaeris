use super::*;

pub(crate) fn build_runtime_event_tool_result_events(
    session_id: &str,
    turn_id: &str,
    tool_results: &[ToolExecutionResult],
    tool_operations_json: Option<&str>,
) -> Result<Vec<RuntimeEventProjection>, String> {
    if tool_results.is_empty() {
        return Ok(vec![]);
    }

    let operations_by_call_id = parse_tool_operations_by_call_id(tool_operations_json);
    let mut events = Vec::with_capacity(tool_results.len());

    for report in tool_results {
        let model_input_images =
            tool_context_writer::tool_result_model_input_image_sources(&report.details)?;
        let capture = crate::tool::layer::tool_result_capture(report);
        let process_state = RuntimeProcessState::from_tool_name(report.tool_name.as_str());
        let at_ms = report.completed_at_ms;
        let event_id = stable_session_event_id(
            "tool_result",
            &[session_id, turn_id, report.tool_call_id.as_str()],
        );

        let mut payload = json!({
            "callId": report.tool_call_id,
            "summary": summarize_tool_result(report),
            "resultState": report.result_state().as_str(),
            "resultPreview": preview_tool_result(report),
            "modelContent": report.content,
            "fullOutputPath": capture.full_output_path,
            "outputStartByte": capture.output_start_byte,
            "outputByteLength": capture.output_byte_length,
            "outputComplete": capture.output_complete,
            "modelInputImages": model_input_images,
            "latencyMs": report.latency_ms.max(0),
        });
        if let Some(operation_items) = operations_by_call_id.get(report.tool_call_id.as_str()) {
            if let Some(object) = payload.as_object_mut() {
                object.insert(
                    "operations".to_string(),
                    Value::Array(operation_items.clone()),
                );
            }
        }
        if let Some(file_fact) = extract_file_fact(&report.details) {
            if let Some(object) = payload.as_object_mut() {
                object.insert("fileFact".to_string(), file_fact);
            }
        }
        if let Some(network_diagnostics) = extract_network_diagnostics(&report.details) {
            if let Some(object) = payload.as_object_mut() {
                object.insert("networkDiagnostics".to_string(), network_diagnostics);
            }
        }
        let hint_lines = extract_tool_result_hint_lines(report);
        if !hint_lines.is_empty() {
            if let Some(object) = payload.as_object_mut() {
                object.insert(
                    "hintLines".to_string(),
                    Value::Array(hint_lines.into_iter().map(Value::String).collect()),
                );
            }
        }

        let event = typed_runtime_event(json!({
            "id": event_id,
            "version": "v1",
            "type": "ToolResult",
            "at": at_ms,
            "sessionId": session_id,
            "turnId": turn_id,
            "taskId": report.tool_call_id,
            "parentTaskId": turn_id,
            "status": event_status_from_tool_status(report.status.as_str()),
            "visibility": "user",
            "toolName": report.tool_name,
            "processState": process_state.as_str(),
            "payload": payload,
            "meta": {
                "source": "core.agent_runtime",
                "transitionReason": report.transition_reason,
            }
        }));

        events.push(event);

        if let Some(event) =
            build_runtime_event_tool_evidence_ref_event(session_id, turn_id, report, at_ms)
        {
            events.push(event);
        }
    }

    Ok(events)
}

fn extract_network_diagnostics(details: &Value) -> Option<Value> {
    let diagnostics = details
        .get("runtimeDiagnostics")?
        .as_array()?
        .iter()
        .filter(|item| item.get("source").and_then(Value::as_str) == Some("networkProxy"))
        .take(16)
        .cloned()
        .collect::<Vec<_>>();
    (!diagnostics.is_empty()).then_some(Value::Array(diagnostics))
}

fn extract_file_fact(details: &Value) -> Option<Value> {
    let fact = details.get("fileFact")?;
    let schema = fact.get("schema").and_then(Value::as_str)?;
    if schema.starts_with("file_") && schema.ends_with("_fact_v1") {
        Some(fact.clone())
    } else {
        None
    }
}

fn build_runtime_event_tool_evidence_ref_event(
    session_id: &str,
    turn_id: &str,
    report: &ToolExecutionResult,
    at_ms: i64,
) -> Option<RuntimeEventProjection> {
    let details_json = report.details.to_string();
    let evidence_object_id = extract_evidence_object_id(&report.details);
    let evidence_rollup_object_id = extract_evidence_rollup_object_id(&report.details);
    let output_truncated = details_json.len() > CHECKPOINT_TOOL_REPORT_PREVIEW_CHARS;
    if evidence_object_id.is_none() && evidence_rollup_object_id.is_none() && !output_truncated {
        return None;
    }

    let event_id = stable_session_event_id(
        "tool_evidence_refs",
        &[session_id, turn_id, report.tool_call_id.as_str()],
    );
    let event = typed_runtime_event(json!({
        "id": event_id,
        "version": "v1",
        "type": "ToolEvidenceRefs",
        "at": at_ms,
        "sessionId": session_id,
        "turnId": turn_id,
        "taskId": report.tool_call_id,
        "parentTaskId": turn_id,
        "status": "done",
        "visibility": "internal",
        "toolName": report.tool_name,
        "payload": {
            "callId": report.tool_call_id,
            "evidenceObjectId": evidence_object_id,
            "evidenceRollupObjectId": evidence_rollup_object_id,
            "outputBytes": details_json.len(),
            "outputTruncated": output_truncated,
        },
        "meta": {
            "source": "core.agent_runtime",
            "renderSurface": "internal",
            "transitionReason": report.transition_reason,
        }
    }));

    Some(event)
}

pub(crate) fn build_runtime_event_tool_call_events(
    session_id: &str,
    turn_id: &str,
    tool_calls: &[ToolCallEnvelope],
    permission_preview: Option<&HashMap<String, PermissionDecision>>,
) -> Result<Vec<RuntimeEventProjection>, String> {
    if tool_calls.is_empty() {
        return Ok(vec![]);
    }

    let mut events = Vec::with_capacity(tool_calls.len());
    for call in tool_calls {
        let at_ms = now_ms();
        let at_ms_identity = at_ms.to_string();
        let event_id = stable_session_event_id(
            "tool_call",
            &[
                session_id,
                turn_id,
                call.id.as_str(),
                at_ms_identity.as_str(),
            ],
        );
        let process_state = RuntimeProcessState::from_tool_name(call.name.as_str());
        let mut payload_map = serde_json::Map::new();
        payload_map.insert("callId".to_string(), Value::String(call.id.clone()));
        if call.name == "bash" {
            let input = serde_json::from_str::<Value>(call.args_json.as_str())
                .map_err(|error| format!("semantic tool_call input is invalid JSON: {error}"))?;
            let input = input
                .as_object()
                .ok_or_else(|| "semantic tool_call input must be an object".to_string())?;
            let (display_target, command, description) =
                super::super::bash_tool_call_display(input)?;
            payload_map.insert("displayTarget".to_string(), Value::String(display_target));
            payload_map.insert("command".to_string(), Value::String(command));
            if let Some(description) = description {
                payload_map.insert("description".to_string(), Value::String(description));
            }
        }
        let event = typed_runtime_event(json!({
            "id": event_id,
            "version": "v1",
            "type": "ToolCall",
            "at": at_ms,
            "sessionId": session_id,
            "turnId": turn_id,
            "taskId": call.id,
            "parentTaskId": turn_id,
            "status": "running",
            "visibility": "user",
            "toolName": call.name,
            "processState": process_state.as_str(),
            "payload": Value::Object(payload_map),
            "meta": {
                "source": "core.agent_runtime",
                "permissionReason": permission_preview
                    .and_then(|items| items.get(call.id.as_str()))
                    .map(|decision| decision.reason.clone()),
                "permissionDecision": permission_preview
                    .and_then(|items| items.get(call.id.as_str()))
                    .map(PermissionDecision::audit_json),
            }
        }));

        events.push(event);
    }
    Ok(events)
}

pub(crate) fn build_runtime_event_tool_progress_event(
    session_id: &str,
    turn_id: &str,
    event: &ToolBatchExecutorEvent,
) -> RuntimeEventProjection {
    let at_ms = now_ms();
    let (task_id, tool_name, stage, status, parallel_group) = match event {
        ToolBatchExecutorEvent::Queued {
            tool_call_id,
            tool_name,
            ..
        } => (
            tool_call_id.as_str(),
            tool_name.as_str(),
            "queued",
            "running",
            None,
        ),
        ToolBatchExecutorEvent::Started {
            tool_call_id,
            tool_name,
            parallel_group,
            ..
        } => (
            tool_call_id.as_str(),
            tool_name.as_str(),
            "executing",
            "running",
            Some(parallel_group.as_str()),
        ),
        ToolBatchExecutorEvent::Finished {
            tool_call_id,
            tool_name,
            status,
            parallel_group,
            ..
        } => (
            tool_call_id.as_str(),
            tool_name.as_str(),
            "finished",
            event_status_from_tool_status(status.as_str()),
            Some(parallel_group.as_str()),
        ),
    };
    let process_state = RuntimeProcessState::from_tool_name(tool_name);
    let at_ms_identity = at_ms.to_string();
    let event_id = stable_session_event_id(
        "tool_progress",
        &[session_id, turn_id, task_id, stage, at_ms_identity.as_str()],
    );
    let mut payload = json!({
        "callId": task_id,
        "stage": stage,
        "message": format!("{} {}", tool_name, stage),
    });
    if let Some(group) = parallel_group {
        if let Some(object) = payload.as_object_mut() {
            object.insert(
                "parallelGroup".to_string(),
                Value::String(group.to_string()),
            );
        }
    }
    typed_runtime_event(json!({
        "id": event_id,
        "version": "v1",
        "type": "ToolProgress",
        "at": at_ms,
        "sessionId": session_id,
        "turnId": turn_id,
        "taskId": task_id,
        "parentTaskId": turn_id,
        "status": status,
        "visibility": "user",
        "toolName": tool_name,
        "processState": process_state.as_str(),
        "payload": payload,
        "meta": {
            "source": "core.tool_batch_executor",
        }
    }))
}

pub(crate) fn build_runtime_event_question_required_event(
    session_id: &str,
    turn_id: &str,
    task_id: &str,
    payload: Value,
) -> RuntimeEventProjection {
    build_runtime_event_wait_required_event(
        session_id,
        turn_id,
        task_id,
        "QuestionRequired",
        payload,
    )
}

fn build_runtime_event_wait_required_event(
    session_id: &str,
    turn_id: &str,
    task_id: &str,
    event_type: &str,
    payload: Value,
) -> RuntimeEventProjection {
    let at_ms = now_ms();
    let at_ms_identity = at_ms.to_string();
    let event_id = stable_session_event_id(
        "wait_required",
        &[
            session_id,
            turn_id,
            task_id,
            event_type,
            at_ms_identity.as_str(),
        ],
    );
    typed_runtime_event(json!({
        "id": event_id,
        "version": "v1",
        "type": event_type,
        "at": at_ms,
        "sessionId": session_id,
        "turnId": turn_id,
        "taskId": task_id,
        "parentTaskId": turn_id,
        "status": "running",
        "visibility": "user",
        "payload": payload,
        "meta": {
            "source": "core.agent_runtime",
        }
    }))
}
