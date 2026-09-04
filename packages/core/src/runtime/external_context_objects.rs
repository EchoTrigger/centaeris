use super::*;

pub(super) fn build_tool_evidence_rollup_external_context_object(
    turn_id: &str,
    reports: &[ToolExecutionResult],
) -> Option<ExternalContextObject> {
    if reports.is_empty() {
        return None;
    }
    let total_output_bytes = reports
        .iter()
        .map(|report| report.details.to_string().len())
        .sum::<usize>();
    let evidence_count = reports
        .iter()
        .filter(|report| extract_evidence_object_id(&report.details).is_some())
        .count();
    if reports.len() < TOOL_EVIDENCE_ROLLUP_MIN_REPORTS
        && total_output_bytes < TOOL_EVIDENCE_ROLLUP_BYTES
        && evidence_count < 2
    {
        return None;
    }
    let ok_count = reports
        .iter()
        .filter(|report| report.status == "ok")
        .count();
    let error_count = reports
        .iter()
        .filter(|report| report.status == "error")
        .count();
    let items = reports
        .iter()
        .map(|report| {
            json!({
                "toolCallId": report.tool_call_id,
                "toolName": report.tool_name,
                "status": report.status,
                "summary": summarize_tool_result(report),
                "resultPreview": preview_tool_result(report),
                "evidenceObjectId": extract_evidence_object_id(&report.details),
                "outputBytes": report.details.to_string().len(),
                "outputTruncated": report.details.to_string().len() > CHECKPOINT_TOOL_REPORT_PREVIEW_CHARS,
                "startedAtMs": report.started_at_ms,
                "completedAtMs": report.completed_at_ms,
                "latencyMs": report.latency_ms,
                "transitionReason": report.transition_reason,
            })
        })
        .collect::<Vec<_>>();
    let content = json!({
        "schema": "tool_evidence_rollup_v1",
        "turnId": turn_id,
        "toolCount": reports.len(),
        "okCount": ok_count,
        "errorCount": error_count,
        "evidenceObjectCount": evidence_count,
        "totalOutputBytes": total_output_bytes,
        "items": items,
    })
    .to_string();
    let object_id = format!(
        "external_context:tool_evidence_rollup_{}",
        stable_text_hash(content.as_str())
    );
    Some(ExternalContextObject {
        schema_version: EXTERNAL_CONTEXT_SCHEMA_VERSION.to_string(),
        object_id,
        object_kind: "toolEvidenceRollup".to_string(),
        source_provider_id: "core".to_string(),
        source_tool_name: "tool_evidence_rollup".to_string(),
        title: format!("Tool evidence rollup for {turn_id}"),
        content,
        metadata: json!({
            "schema": "tool_evidence_rollup_metadata_v1",
            "turnId": turn_id,
            "toolCount": reports.len(),
            "okCount": ok_count,
            "errorCount": error_count,
            "evidenceObjectCount": evidence_count,
            "totalOutputBytes": total_output_bytes,
            "rollupReason": if reports.len() >= TOOL_EVIDENCE_ROLLUP_MIN_REPORTS {
                "tool_count"
            } else if total_output_bytes >= TOOL_EVIDENCE_ROLLUP_BYTES {
                "output_bytes"
            } else {
                "evidence_object_count"
            },
        }),
        updated_at_ms: reports
            .iter()
            .map(|report| report.completed_at_ms.max(report.started_at_ms))
            .max()
            .unwrap_or_default(),
    })
}

pub(super) fn extract_external_context_object_from_tool_output(
    details: &Value,
) -> Option<Result<ExternalContextObject, String>> {
    let external_object = details
        .get("result")
        .and_then(|result| result.get("externalObject"))
        .or_else(|| details.get("externalObject"))?;
    let mode = external_object
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !mode.eq_ignore_ascii_case("externalObject") {
        return None;
    }
    let Some(object_value) = external_object.get("object").cloned() else {
        return Some(Err(
            "externalObject output missing external context object".to_string()
        ));
    };
    let object = match serde_json::from_value::<ExternalContextObject>(object_value) {
        Ok(object) => object,
        Err(err) => {
            return Some(Err(format!(
                "decode externalObject external context object failed: {err}"
            )));
        }
    };
    if object.object_id.trim().is_empty() {
        return Some(Err(
            "externalObject external context object_id is required".to_string()
        ));
    }
    if object.source_provider_id.trim().is_empty() {
        return Some(Err(
            "externalObject external context source_provider_id is required".to_string(),
        ));
    }
    if object.source_tool_name.trim().is_empty() {
        return Some(Err(
            "externalObject external context source_tool_name is required".to_string(),
        ));
    }
    Some(Ok(object))
}

pub(super) fn annotate_external_context_store_status(
    details: &Value,
    status: Value,
) -> Result<Value, String> {
    let mut object = details.as_object().cloned().ok_or_else(|| {
        "tool result details must be an object for external context status".to_string()
    })?;
    object.insert("externalContextStore".to_string(), status);
    Ok(Value::Object(object))
}

pub(super) fn annotate_evidence_rollup_store_status(
    details: &Value,
    status: Value,
) -> Result<Value, String> {
    let mut object = details.as_object().cloned().ok_or_else(|| {
        "tool result details must be an object for evidence rollup status".to_string()
    })?;
    object.insert("evidenceRollupStore".to_string(), status);
    Ok(Value::Object(object))
}
