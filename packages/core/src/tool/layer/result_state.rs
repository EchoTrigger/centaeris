use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::ToolExecutionResult;
use crate::tool::ToolFailureKind;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ToolResultState {
    SuccessWithOutput,
    SuccessNoOutput,
    SuccessNoMatches,
    Failed,
    Denied,
    Aborted,
}

impl ToolResultState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SuccessWithOutput => "successWithOutput",
            Self::SuccessNoOutput => "successNoOutput",
            Self::SuccessNoMatches => "successNoMatches",
            Self::Failed => "failed",
            Self::Denied => "denied",
            Self::Aborted => "aborted",
        }
    }

    pub fn from_result(report: &ToolExecutionResult) -> Self {
        let details = &report.details;
        if is_denied_tool_result(report) {
            return Self::Denied;
        }
        if is_aborted_tool_result(report) {
            return Self::Aborted;
        }
        if !is_success_status(report.status.as_str()) {
            return Self::Failed;
        }
        if is_successful_zero_match_result(report, Some(details)) {
            return Self::SuccessNoMatches;
        }
        if report.content.trim().is_empty() || has_no_model_visible_output(details) {
            return Self::SuccessNoOutput;
        }
        Self::SuccessWithOutput
    }

    pub fn is_success(self) -> bool {
        matches!(
            self,
            Self::SuccessWithOutput | Self::SuccessNoOutput | Self::SuccessNoMatches
        )
    }
}

impl ToolExecutionResult {
    pub fn result_state(&self) -> ToolResultState {
        ToolResultState::from_result(self)
    }
}

fn is_success_status(status: &str) -> bool {
    status == "ok"
}

fn is_denied_tool_result(report: &ToolExecutionResult) -> bool {
    if matches!(
        report.error.as_ref().map(|error| &error.kind),
        Some(ToolFailureKind::PermissionDenied)
    ) {
        return true;
    }
    let status = report.status.trim().to_ascii_lowercase();
    matches!(status.as_str(), "blocked" | "denied" | "skipped")
}

fn is_aborted_tool_result(report: &ToolExecutionResult) -> bool {
    if matches!(
        report.error.as_ref().map(|error| &error.kind),
        Some(ToolFailureKind::Cancelled)
    ) {
        return true;
    }
    let status = report.status.trim().to_ascii_lowercase();
    matches!(
        status.as_str(),
        "cancelled" | "canceled" | "aborted" | "discarded"
    )
}

fn is_successful_zero_match_result(report: &ToolExecutionResult, parsed: Option<&Value>) -> bool {
    let Some(value) = parsed else {
        return false;
    };
    if !is_search_like_tool_result(report, value) {
        return false;
    }
    if has_non_empty_stderr(value) || value.get("timedOut").and_then(Value::as_bool) == Some(true) {
        return false;
    }
    let exit_code = value.get("exitCode").and_then(Value::as_i64);
    if exit_code.is_some_and(|code| code != 0 && code != 1) {
        return false;
    }
    if ["matches", "results", "items"].iter().any(|key| {
        value
            .get(*key)
            .and_then(Value::as_array)
            .map(|items| items.is_empty())
            .unwrap_or(false)
    }) {
        return true;
    }
    if [
        "matchCount",
        "matchesCount",
        "totalMatches",
        "resultCount",
        "count",
    ]
    .iter()
    .any(|key| value.get(*key).and_then(Value::as_u64) == Some(0))
    {
        return true;
    }
    false
}

fn is_search_like_tool_result(report: &ToolExecutionResult, value: &Value) -> bool {
    if report
        .tool_name
        .trim()
        .to_ascii_lowercase()
        .contains("search")
    {
        return true;
    }
    value
        .get("schema")
        .and_then(Value::as_str)
        .map(|schema| schema.to_ascii_lowercase().contains("search"))
        .unwrap_or(false)
        || value.get("matches").is_some()
}

fn has_no_model_visible_output(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(item) => item.trim().is_empty(),
        Value::Array(items) => items.is_empty(),
        Value::Object(map) => {
            if map.is_empty() {
                return true;
            }
            map.iter().all(|(key, value)| {
                is_low_signal_empty_output_key(key.as_str()) || value_is_empty_output(value)
            })
        }
        _ => false,
    }
}

fn is_low_signal_empty_output_key(key: &str) -> bool {
    matches!(
        key,
        "schema"
            | "status"
            | "executed"
            | "exitCode"
            | "exit_code"
            | "timedOut"
            | "durationMs"
            | "latencyMs"
            | "command"
            | "cmd"
            | "cwd"
            | "stdout"
            | "stderr"
            | "stdoutTail"
            | "stderrTail"
            | "stdout_tail"
            | "stderr_tail"
            | "stdoutChars"
            | "stderrChars"
    )
}

fn value_is_empty_output(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(item) => item.trim().is_empty(),
        Value::Array(items) => items.is_empty(),
        Value::Object(map) => map.is_empty(),
        _ => false,
    }
}

fn has_non_empty_stderr(value: &Value) -> bool {
    value
        .get("stderr")
        .or_else(|| value.get("stderrTail"))
        .or_else(|| value.get("stderr_tail"))
        .and_then(Value::as_str)
        .map(|item| !item.trim().is_empty())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::ToolErrorInfo;

    fn failed_result(error_kind: ToolFailureKind, transition_reason: &str) -> ToolExecutionResult {
        ToolExecutionResult {
            tool_call_id: "call_banana".to_string(),
            tool_name: "read".to_string(),
            status: "error".to_string(),
            content: "failed".to_string(),
            details: Value::Null,
            facts: Vec::new(),
            error: Some(ToolErrorInfo::new(error_kind, "failed", "Failed")),
            started_at_ms: 0,
            completed_at_ms: 0,
            latency_ms: 0,
            parallel_group: None,
            transition_reason: Some(transition_reason.to_string()),
        }
    }

    #[test]
    fn result_state_uses_typed_error_kind_instead_of_reason_text() {
        assert_eq!(
            failed_result(ToolFailureKind::HostUnavailable, "approval_like_text").result_state(),
            ToolResultState::Failed
        );
        assert_eq!(
            failed_result(ToolFailureKind::PermissionDenied, "banana").result_state(),
            ToolResultState::Denied
        );
        assert_eq!(
            failed_result(ToolFailureKind::Cancelled, "banana").result_state(),
            ToolResultState::Aborted
        );
    }
}
