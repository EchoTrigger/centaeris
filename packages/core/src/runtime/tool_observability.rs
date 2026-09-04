use super::*;

pub(super) fn build_tool_use_summary(tool_results: &[ToolExecutionResult]) -> String {
    let total = tool_results.len();
    let ok = tool_results
        .iter()
        .filter(|item| item.status == "ok")
        .count();
    let error = tool_results
        .iter()
        .filter(|item| item.status == "error")
        .count();
    let skipped = tool_results
        .iter()
        .filter(|item| item.status == "skipped")
        .count();
    let blocked = tool_results
        .iter()
        .filter(|item| item.status == "blocked")
        .count();
    let latency_ms: i64 = tool_results.iter().map(|item| item.latency_ms.max(0)).sum();
    format!(
        "Tools executed: total={total}, ok={ok}, error={error}, skipped={skipped}, blocked={blocked}, latencyMs={latency_ms}"
    )
}

#[derive(Debug, Clone, Serialize)]
struct ToolOperationPayload {
    #[serde(rename = "callId")]
    call_id: String,
    #[serde(rename = "toolName")]
    tool_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    kind: Option<&'static str>,
    status: String,
    #[serde(rename = "resultState")]
    result_state: String,
    #[serde(rename = "path", skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(rename = "startLine", skip_serializing_if = "Option::is_none")]
    start_line: Option<u32>,
    #[serde(rename = "endLine", skip_serializing_if = "Option::is_none")]
    end_line: Option<u32>,
    #[serde(rename = "totalLines", skip_serializing_if = "Option::is_none")]
    total_lines: Option<u32>,
    #[serde(rename = "nextOffset", skip_serializing_if = "Option::is_none")]
    next_offset: Option<u32>,
    #[serde(rename = "truncatedBy", skip_serializing_if = "Option::is_none")]
    truncated_by: Option<String>,
    #[serde(rename = "added", skip_serializing_if = "Option::is_none")]
    added: Option<u32>,
    #[serde(rename = "removed", skip_serializing_if = "Option::is_none")]
    removed: Option<u32>,
    #[serde(rename = "outputPreview", skip_serializing_if = "Option::is_none")]
    output_preview: Option<String>,
    #[serde(rename = "diffPreview", skip_serializing_if = "Option::is_none")]
    diff_preview: Option<String>,
    #[serde(rename = "error", skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(rename = "exitCode", skip_serializing_if = "Option::is_none")]
    exit_code: Option<i32>,
}

pub fn project_tool_operations_json(tool_results: &[ToolExecutionResult]) -> Option<String> {
    if tool_results.is_empty() {
        return None;
    }

    let mut operations: Vec<ToolOperationPayload> = Vec::with_capacity(tool_results.len());
    for report in tool_results {
        let details = &report.details;
        let result_state = report.result_state();
        let operation_succeeded = result_state.is_success();
        let kind = (report.tool_name == "bash").then_some("command");
        let path = match report.tool_name.as_str() {
            "read" | "write" => operation_string(details, "path"),
            "edit" => operation_string(details, "path").or_else(|| edit_operation_path(details)),
            _ => None,
        };
        let start_line = (report.tool_name == "read")
            .then(|| operation_u32(details, "startLine"))
            .flatten();
        let end_line = (report.tool_name == "read")
            .then(|| operation_u32(details, "endLine"))
            .flatten();
        let total_lines = (report.tool_name == "read")
            .then(|| operation_u32(details, "totalLines"))
            .flatten();
        let next_offset = (report.tool_name == "read")
            .then(|| operation_u32(details, "nextOffset"))
            .flatten();
        let truncated_by = (report.tool_name == "read")
            .then(|| operation_string(details, "truncatedBy"))
            .flatten();
        let added = operation_succeeded
            .then(|| match report.tool_name.as_str() {
                "write" | "edit" => operation_u32(details, "addedLines"),
                _ => None,
            })
            .flatten();
        let removed = operation_succeeded
            .then(|| match report.tool_name.as_str() {
                "write" | "edit" => operation_u32(details, "removedLines"),
                _ => None,
            })
            .flatten();
        let output_preview = tool_operation_output_preview(report);
        let diff_preview = (["write", "edit"].contains(&report.tool_name.as_str())
            && operation_succeeded)
            .then(|| operation_string(details, "diffPreview"))
            .flatten()
            .map(|item| compact_multiline_text(item.as_str(), 2_400));
        let exit_code = (report.tool_name == "bash")
            .then(|| operation_i32(details, "exitCode"))
            .flatten();
        let status = report.status.clone();

        operations.push(ToolOperationPayload {
            call_id: report.tool_call_id.clone(),
            tool_name: report.tool_name.clone(),
            kind,
            status,
            result_state: result_state.as_str().to_string(),
            path,
            start_line,
            end_line,
            total_lines,
            next_offset,
            truncated_by,
            added,
            removed,
            output_preview,
            diff_preview,
            error: report
                .error
                .as_ref()
                .map(|item| compact_text(item.user_message.as_str(), 240)),
            exit_code,
        });
    }

    serde_json::to_string(&operations).ok()
}

fn parse_result_envelope(details: &Value) -> Option<ResultEnvelope> {
    if let Some(value) = details.get("resultEnvelope") {
        return serde_json::from_value::<ResultEnvelope>(value.clone()).ok();
    }
    serde_json::from_value::<ResultEnvelope>(details.clone()).ok()
}

pub(super) fn summarize_tool_result(report: &ToolExecutionResult) -> String {
    match report.result_state() {
        crate::tool::layer::ToolResultState::Failed => {
            return report
                .error
                .as_ref()
                .map(|item| compact_text(item.model_message.as_str(), 160))
                .unwrap_or_else(|| format!("{} failed", report.tool_name));
        }
        crate::tool::layer::ToolResultState::Denied => {
            return report
                .error
                .as_ref()
                .map(|item| compact_text(item.model_message.as_str(), 160))
                .unwrap_or_else(|| format!("{} denied", report.tool_name));
        }
        crate::tool::layer::ToolResultState::Aborted => {
            return report
                .error
                .as_ref()
                .map(|item| compact_text(item.model_message.as_str(), 160))
                .unwrap_or_else(|| format!("{} aborted", report.tool_name));
        }
        crate::tool::layer::ToolResultState::SuccessNoMatches => {
            return format!("{} completed successfully. Matches: 0.", report.tool_name);
        }
        crate::tool::layer::ToolResultState::SuccessNoOutput => {
            return format!(
                "{} completed successfully with no output.",
                report.tool_name
            );
        }
        crate::tool::layer::ToolResultState::SuccessWithOutput => {}
    }
    if report.status == "error" {
        return report
            .error
            .as_ref()
            .map(|item| compact_text(item.model_message.as_str(), 160))
            .unwrap_or_else(|| format!("{} failed", report.tool_name));
    }
    if report.tool_name == "read" {
        if let Some(summary) = summarize_file_read_tool_result(&report.details) {
            return summary;
        }
    }
    if let Some(envelope) = parse_result_envelope(&report.details) {
        return compact_text(envelope.summary.as_str(), 160);
    }
    if let Some(summary) = extract_text_from_output(
        &report.details,
        &["summary", "message", "result", "status"],
        160,
    ) {
        return summary;
    }
    if report.content.trim().is_empty() {
        format!("{} completed", report.tool_name)
    } else {
        compact_text(report.content.as_str(), 160)
    }
}

fn summarize_file_read_tool_result(details: &Value) -> Option<String> {
    let path = details.get("path").and_then(Value::as_str)?;
    let (start_line, end_line, total_lines, truncated) = file_read_coverage_fields(details);
    let total = total_lines
        .map(|value| value.to_string())
        .unwrap_or_else(|| "?".to_string());
    let next_hint = if truncated {
        format!("; next offset {}", end_line)
    } else {
        String::new()
    };
    if details
        .get("synthetic")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Some(compact_text(
            format!("Skipped reread of {path} lines {start_line}-{end_line} of {total}{next_hint}")
                .as_str(),
            160,
        ));
    }
    Some(compact_text(
        format!("Read {path} lines {start_line}-{end_line} of {total}{next_hint}").as_str(),
        160,
    ))
}

fn file_read_coverage_fields(parsed: &Value) -> (u64, u64, Option<u64>, bool) {
    let start_line = parsed.get("startLine").and_then(Value::as_u64).unwrap_or(1);
    let end_line = parsed
        .get("endLine")
        .and_then(Value::as_u64)
        .unwrap_or(start_line);
    let total_lines = parsed.get("totalLines").and_then(Value::as_u64);
    let truncated = parsed
        .get("truncated")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    (start_line, end_line, total_lines, truncated)
}

pub(super) fn preview_tool_result(report: &ToolExecutionResult) -> String {
    match report.result_state() {
        crate::tool::layer::ToolResultState::SuccessNoOutput
        | crate::tool::layer::ToolResultState::SuccessNoMatches
        | crate::tool::layer::ToolResultState::Failed
        | crate::tool::layer::ToolResultState::Denied
        | crate::tool::layer::ToolResultState::Aborted => {
            return compact_text(summarize_tool_result(report).as_str(), 260);
        }
        crate::tool::layer::ToolResultState::SuccessWithOutput => {}
    }
    if let Some(envelope) = parse_result_envelope(&report.details) {
        if let Some(finding) = envelope.findings.first() {
            return compact_text(
                format!("{}: {}", envelope.summary, finding.detail).as_str(),
                260,
            );
        }
        return compact_text(envelope.summary.as_str(), 260);
    }
    if let Some(preview) = extract_text_from_output(
        &report.details,
        &["resultPreview", "preview", "output", "content", "message"],
        260,
    ) {
        return preview;
    }
    compact_text(report.content.as_str(), 260)
}

pub(super) fn extract_tool_result_hint_lines(report: &ToolExecutionResult) -> Vec<String> {
    let _ = report;
    Vec::new()
}

fn extract_text_from_output(details: &Value, keys: &[&str], limit: usize) -> Option<String> {
    for key in keys {
        let Some(value) = details.get(*key) else {
            continue;
        };
        if let Some(text) = as_non_empty_string(value) {
            return Some(compact_text(text.as_str(), limit));
        }
    }
    None
}

pub(super) fn extract_evidence_object_id(details: &Value) -> Option<String> {
    let external_object = details
        .get("result")
        .and_then(|result| result.get("externalObject"))
        .or_else(|| details.get("externalObject"));
    if let Some(external_object) = external_object {
        if let Some(object_id) = external_object
            .get("object")
            .and_then(|item| item.get("objectId"))
            .and_then(Value::as_str)
            .or_else(|| {
                external_object
                    .get("pointer")
                    .and_then(|item| item.get("objectId"))
                    .and_then(Value::as_str)
            })
            .map(str::trim)
            .filter(|item| !item.is_empty())
        {
            return Some(object_id.to_string());
        }
    }
    details
        .get("externalContextStore")
        .and_then(|item| item.get("objectId"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(ToString::to_string)
}

pub(super) fn extract_evidence_rollup_object_id(details: &Value) -> Option<String> {
    details
        .get("evidenceRollupStore")
        .and_then(|item| item.get("objectId"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(ToString::to_string)
}

fn operation_string(details: &Value, key: &str) -> Option<String> {
    details
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn edit_operation_path(details: &Value) -> Option<String> {
    details
        .get("operations")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .and_then(|operation| operation_string(operation, "path"))
}

fn operation_u32(details: &Value, key: &str) -> Option<u32> {
    details
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
}

fn operation_i32(details: &Value, key: &str) -> Option<i32> {
    details
        .get(key)
        .and_then(Value::as_i64)
        .and_then(|value| i32::try_from(value).ok())
}

fn as_non_empty_string(value: &Value) -> Option<String> {
    if let Some(raw) = value.as_str() {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
        return None;
    }
    if value.is_null() {
        return None;
    }
    let text = value.to_string();
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(compact_text(trimmed, 180))
}

fn tool_operation_output_preview(report: &ToolExecutionResult) -> Option<String> {
    if let Some(error) = report
        .error
        .as_ref()
        .map(|e| e.model_message.as_str())
        .filter(|item| !item.is_empty())
    {
        return Some(compact_text(error, 1_000));
    }
    if report.tool_name != "bash" {
        return None;
    }
    let stdout = report
        .details
        .get("stdout")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|item| !item.is_empty());
    let stderr = report
        .details
        .get("stderr")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|item| !item.is_empty());
    match (stdout, stderr) {
        (Some(out), Some(err)) => Some(compact_text(format!("{out}\n{err}").as_str(), 1_000)),
        (Some(out), None) => Some(compact_text(out, 1_000)),
        (None, Some(err)) => Some(compact_text(err, 1_000)),
        (None, None) => Some(compact_text(report.content.as_str(), 1_000)),
    }
}
