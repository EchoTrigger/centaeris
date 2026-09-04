use super::*;

const MAX_PREPARING_TOOL_NAME_BYTES: usize = 128;

pub(super) fn is_bounded_canonical_tool_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_PREPARING_TOOL_NAME_BYTES
        && !value.starts_with('_')
        && !value.ends_with('_')
        && !value.contains("__")
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

pub(super) fn truncated_tool_call(
    call_id: Option<&str>,
    tool_name: Option<&str>,
    args: &str,
) -> TruncatedToolCall {
    TruncatedToolCall {
        call_id: call_id
            .filter(|value| !value.is_empty() && *value == value.trim())
            .map(ToString::to_string),
        tool_name: tool_name
            .filter(|value| is_bounded_canonical_tool_name(value))
            .map(ToString::to_string),
        args_bytes: args.len(),
        args_sha256: format!("sha256:{:x}", Sha256::digest(args.as_bytes())),
    }
}

pub(super) fn invalid_provider_tool_call_identity(wire_api: &str) -> ModelClientError {
    ModelClientError {
        kind: ModelClientErrorKind::Provider,
        message: format!("{wire_api} response contained an invalid tool call identity"),
        retryable: false,
        provider_code: Some("invalid_tool_call_identity".to_string()),
        provider_attempts: 0,
        truncated_tool_calls: Vec::new(),
    }
}

pub(super) fn required_provider_tool_call_identity(
    wire_api: &str,
    call_id: Option<&str>,
    tool_name: Option<&str>,
) -> Result<(String, String), ModelClientError> {
    let call_id = call_id
        .filter(|value| !value.is_empty() && *value == value.trim())
        .ok_or_else(|| invalid_provider_tool_call_identity(wire_api))?;
    let tool_name = tool_name
        .filter(|value| is_bounded_canonical_tool_name(value))
        .ok_or_else(|| invalid_provider_tool_call_identity(wire_api))?;
    Ok((call_id.to_string(), tool_name.to_string()))
}

pub fn validate_provider_tool_call_arguments(
    wire_api: &str,
    tool_calls: &[ToolCallEnvelope],
) -> Result<(), ModelClientError> {
    validate_provider_tool_call_arguments_with_retryability(wire_api, tool_calls, false)
}

pub(super) fn validate_retryable_provider_tool_call_arguments(
    wire_api: &str,
    tool_calls: &[ToolCallEnvelope],
) -> Result<(), ModelClientError> {
    validate_provider_tool_call_arguments_with_retryability(wire_api, tool_calls, true)
}

pub(super) fn validate_provider_tool_call_arguments_with_retryability(
    wire_api: &str,
    tool_calls: &[ToolCallEnvelope],
    retryable: bool,
) -> Result<(), ModelClientError> {
    for call in tool_calls {
        if let Err(error) = serde_json::from_str::<Value>(call.args_json.as_str()) {
            let mut digest = Sha256::new();
            digest.update(call.args_json.as_bytes());
            return Err(ModelClientError {
                kind: ModelClientErrorKind::Provider,
                message: format!(
                    "{wire_api} response contained malformed tool arguments: callId={} toolName={} argsBytes={} argsSha256=sha256:{:x} errorLine={} errorColumn={} error={error}",
                    call.id,
                    call.name,
                    call.args_json.len(),
                    digest.finalize(),
                    error.line(),
                    error.column(),
                ),
                retryable,
                provider_code: Some("malformed_tool_call_arguments".to_string()),
                provider_attempts: 0,
                truncated_tool_calls: Vec::new(),
            });
        }
    }
    Ok(())
}

pub(super) fn normalize_tool_arguments_json(raw: String) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        "{}".to_string()
    } else {
        trimmed.to_string()
    }
}

pub(super) fn normalize_tool_arguments_value(value: &Value) -> String {
    match value {
        Value::String(raw) => normalize_tool_arguments_json(raw.clone()),
        Value::Null => "{}".to_string(),
        _ => serde_json::to_string(value).unwrap_or_else(|_| "{}".to_string()),
    }
}

pub(super) fn normalize_stream_tool_arguments(
    accumulated: String,
    item_arguments: Option<&Value>,
) -> String {
    let trimmed = accumulated.trim();
    if !trimmed.is_empty() && serde_json::from_str::<Value>(trimmed).is_ok() {
        return trimmed.to_string();
    }
    item_arguments
        .map(normalize_tool_arguments_value)
        .unwrap_or_else(|| {
            if trimmed.is_empty() {
                "{}".to_string()
            } else {
                trimmed.to_string()
            }
        })
}

pub(super) fn preview_tool_arguments(raw: &str) -> String {
    let normalized = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= 240 {
        return normalized;
    }
    let mut preview = normalized.chars().take(237).collect::<String>();
    preview.push_str("...");
    preview
}

pub(super) fn preview_tool_arguments_json(raw: &str) -> String {
    serde_json::from_str::<Value>(raw)
        .ok()
        .and_then(|value| serde_json::to_string(&value).ok())
        .map(|item| preview_tool_arguments(item.as_str()))
        .unwrap_or_else(|| preview_tool_arguments(raw))
}
