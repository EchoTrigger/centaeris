use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct OpenAiTokenDetails {
    pub(super) cached_tokens: Option<i64>,
}

pub(super) fn parse_sse_json_frame(
    wire_api: &str,
    frame: &str,
    attempt: u32,
) -> Result<Value, ModelClientError> {
    serde_json::from_str(frame).map_err(|error| ModelClientError {
        kind: ModelClientErrorKind::ProviderResponseInterrupted,
        message: format!(
            "malformed_sse_frame: wireApi={wire_api} frameBytes={} frameSha256=sha256:{:x} jsonLine={} jsonColumn={} attempt={attempt}",
            frame.len(),
            Sha256::digest(frame.as_bytes()),
            error.line(),
            error.column(),
        ),
        retryable: true,
        provider_code: Some("malformed_sse_frame".to_string()),
        provider_attempts: 0,
        truncated_tool_calls: Vec::new(),
    })
}

pub(super) fn prepared_prompt_max_output_tokens(
    request: &ModelClientRequest,
    session_config: &ModelSessionConfig,
) -> Result<Option<u32>, ModelClientError> {
    request
        .prepared_prompt
        .validate()
        .map_err(invalid_openai_compatible_request)?;
    if let Some(configured_max_output_tokens) = session_config.max_output_tokens {
        if request.prepared_prompt.max_output_tokens > configured_max_output_tokens {
            return Err(invalid_openai_compatible_request(format!(
                "prepared_prompt_max_output_tokens_exceeded: prepared={} model={configured_max_output_tokens}",
                request.prepared_prompt.max_output_tokens
            )));
        }
    }
    Ok(Some(request.prepared_prompt.max_output_tokens))
}
