use centaeris_core::model::prepared_prompt::PreparedPromptV1;
use centaeris_core::model::{
    validate_provider_tool_call_arguments, ModelClient, ModelClientError, ModelClientErrorKind,
    ModelClientFuture, ModelClientRequest, ModelClientResponse, ModelClientStreamEvent,
};
use centaeris_core::model::{GenerateResult, ToolCallEnvelope};
use centaeris_core::runtime::contracts::RuntimeProcessState;
use serde::{Deserialize, Serialize};

/// SSE 帧解码器：累积字节缓冲，容忍 chunk 边界切开多字节 UTF-8 字符。
/// 末尾不完整序列等待后续 chunk；非法字节序列 loud-fail。
struct SseFrameDecoder {
    buffer: Vec<u8>,
}

impl SseFrameDecoder {
    fn new() -> Self {
        Self { buffer: Vec::new() }
    }

    /// 追加 chunk，返回本批解析出的完整帧（`data: ` 行已去前缀并按行拼接）。
    fn push(&mut self, chunk: &[u8]) -> Result<Vec<String>, String> {
        self.buffer.extend_from_slice(chunk);
        let mut frames = Vec::new();
        loop {
            let frame_end = {
                let text = match std::str::from_utf8(self.buffer.as_slice()) {
                    Ok(text) => text,
                    Err(error) if error.error_len().is_none() => break,
                    Err(error) => return Err(format!("stream is not UTF-8: {error}")),
                };
                match text.find("\n\n") {
                    Some(end) => end,
                    None => break,
                }
            };
            let frame = std::str::from_utf8(&self.buffer[..frame_end])
                .map_err(|error| format!("frame is not UTF-8: {error}"))?
                .to_string();
            self.buffer.drain(..frame_end + 2);
            let data = frame
                .lines()
                .filter_map(|line| line.strip_prefix("data: "))
                .collect::<Vec<_>>()
                .join("\n");
            if !data.is_empty() {
                frames.push(data);
            }
        }
        Ok(frames)
    }

    /// 流结束检查：残留非空（不完整帧或不完整 UTF-8）时返回 Err。
    fn finish(&self) -> Result<(), String> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        match std::str::from_utf8(self.buffer.as_slice()) {
            Ok(text) if !text.trim().is_empty() => {
                Err("stream ended with an incomplete frame".to_string())
            }
            Ok(_) => Ok(()),
            Err(error) => Err(format!("stream ended with incomplete UTF-8: {error}")),
        }
    }
}

#[derive(Clone)]
pub struct ApiModelClient {
    api_internal_url: String,
    internal_api_token: String,
    agent_run_id: String,
    model_config_ref: String,
    authorization_ref: String,
    authorization_digest: String,
    thinking_mode: Option<String>,
    model_max_output_tokens: u32,
    client: reqwest::Client,
}

pub struct ApiModelClientConfig {
    pub api_internal_url: String,
    pub internal_api_token: String,
    pub agent_run_id: String,
    pub model_config_ref: String,
    pub authorization_ref: String,
    pub authorization_digest: String,
    pub thinking_mode: Option<String>,
    pub model_max_output_tokens: u32,
}

impl ApiModelClient {
    pub fn new(config: ApiModelClientConfig) -> Self {
        Self {
            api_internal_url: config.api_internal_url,
            internal_api_token: config.internal_api_token,
            agent_run_id: config.agent_run_id,
            model_config_ref: config.model_config_ref,
            authorization_ref: config.authorization_ref,
            authorization_digest: config.authorization_digest,
            thinking_mode: config.thinking_mode,
            model_max_output_tokens: config.model_max_output_tokens,
            client: reqwest::Client::new(),
        }
    }

    fn prepared_prompt_max_output_tokens(
        &self,
        request: &ModelClientRequest,
    ) -> Result<u32, ModelClientError> {
        request.prepared_prompt.validate().map_err(|error| {
            ModelClientError::new(
                ModelClientErrorKind::InvalidRequest,
                format!("prepared prompt invalid: {error}"),
                false,
            )
        })?;
        if request.prepared_prompt.max_output_tokens > self.model_max_output_tokens {
            return Err(ModelClientError::new(
                ModelClientErrorKind::InvalidRequest,
                format!(
                    "prepared prompt output limit exceeded: prepared={} model={}",
                    request.prepared_prompt.max_output_tokens, self.model_max_output_tokens
                ),
                false,
            ));
        }
        Ok(request.prepared_prompt.max_output_tokens)
    }

    fn model_run_request<'a>(
        &'a self,
        request: &'a ModelClientRequest,
    ) -> Result<ModelRunRequest<'a>, ModelClientError> {
        Ok(ModelRunRequest {
            schema: "api.model.run.v1",
            agent_run_id: self.agent_run_id.as_str(),
            model_config_ref: self.model_config_ref.as_str(),
            authorization_ref: self.authorization_ref.as_str(),
            authorization_digest: self.authorization_digest.as_str(),
            thinking_mode: self.thinking_mode.as_deref(),
            max_output_tokens: self.prepared_prompt_max_output_tokens(request)?,
            prepared_prompt: &request.prepared_prompt,
        })
    }

    async fn call_api(
        &self,
        request: &ModelClientRequest,
    ) -> Result<ModelClientResponse, ModelClientError> {
        let model_run_request = self.model_run_request(request)?;
        let url = format!(
            "{}/internal/model-runs",
            self.api_internal_url.trim_end_matches('/')
        );
        let response = self
            .client
            .post(url)
            .header("Content-Type", "application/json")
            .header("X-Internal-Token", self.internal_api_token.as_str())
            .json(&model_run_request)
            .send()
            .await
            .map_err(|error| {
                ModelClientError::new(
                    ModelClientErrorKind::Network,
                    format!("api model run request failed: {error}"),
                    true,
                )
            })?;
        let status = response.status();
        let body = response.text().await.map_err(|error| {
            ModelClientError::new(
                ModelClientErrorKind::Network,
                format!("read api model run response failed: {error}"),
                true,
            )
        })?;
        if !status.is_success() {
            return Err(ModelClientError::new(
                ModelClientErrorKind::Provider,
                format!("api model run returned {}: {body}", status.as_u16()),
                false,
            ));
        }
        let payload = serde_json::from_str::<ModelRunResponse>(body.as_str()).map_err(|error| {
            ModelClientError::new(
                ModelClientErrorKind::Provider,
                format!("parse api model run response failed: {error}"),
                false,
            )
        })?;
        model_response(payload)
    }

    async fn call_api_with_retries(
        &self,
        request: &ModelClientRequest,
    ) -> Result<ModelClientResponse, ModelClientError> {
        let max_retries = request.session_config.max_retries;
        for retry in 0..=max_retries {
            match self.call_api(request).await {
                Ok(mut response) => {
                    response.provider_attempts = retry.saturating_add(1);
                    return Ok(response);
                }
                Err(mut error) => {
                    error.provider_attempts = retry.saturating_add(1);
                    if !error.retryable || retry == max_retries {
                        if retry == max_retries {
                            error.retryable = false;
                        }
                        return Err(error);
                    }
                    sleep_before_retry(
                        retry.saturating_add(1),
                        request.session_config.retry_backoff_ms,
                    )
                    .await;
                }
            }
        }
        unreachable!("model retry loop always returns")
    }

    async fn call_api_stream(
        &self,
        request: &ModelClientRequest,
        sink: &mut (dyn FnMut(ModelClientStreamEvent) + Send),
    ) -> Result<ModelClientResponse, ModelClientError> {
        let model_run_request = self.model_run_request(request)?;
        let url = format!(
            "{}/internal/model-runs",
            self.api_internal_url.trim_end_matches('/')
        );
        let mut response = self
            .client
            .post(url)
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream")
            .header("X-Internal-Token", self.internal_api_token.as_str())
            .json(&model_run_request)
            .send()
            .await
            .map_err(|error| stream_error("api model stream request failed", error))?;
        if !response.status().is_success() {
            return Err(ModelClientError::new(
                ModelClientErrorKind::Provider,
                format!("api model stream returned {}", response.status().as_u16()),
                false,
            ));
        }
        let mut decoder = SseFrameDecoder::new();
        let mut streamed_text = String::new();
        let mut result = None;
        let mut terminal_received = false;
        loop {
            let chunk = match response.chunk().await {
                Ok(Some(chunk)) => chunk,
                Ok(None) => break,
                Err(_) if terminal_received => break,
                Err(error) => return Err(stream_error("read api model stream failed", error)),
            };
            let frames = decoder.push(chunk.as_ref()).map_err(|message| {
                ModelClientError::new(
                    ModelClientErrorKind::ProviderResponseInterrupted,
                    format!("api model stream {message}"),
                    false,
                )
            })?;
            for data in frames {
                let event =
                    serde_json::from_str::<serde_json::Value>(data.as_str()).map_err(|error| {
                        ModelClientError::new(
                            ModelClientErrorKind::ProviderResponseInterrupted,
                            format!("decode api model stream event failed: {error}"),
                            false,
                        )
                    })?;
                if terminal_received {
                    return Err(ModelClientError::new(
                        ModelClientErrorKind::ProviderResponseInterrupted,
                        "api model stream emitted an event after result",
                        false,
                    ));
                }
                if event.get("schema").and_then(serde_json::Value::as_str)
                    != Some("api.model.stream.v1")
                {
                    return Err(ModelClientError::new(
                        ModelClientErrorKind::ProviderResponseInterrupted,
                        "api model stream schema mismatch",
                        false,
                    ));
                }
                match event.get("type").and_then(serde_json::Value::as_str) {
                    Some("error") => {
                        if event.as_object().is_none_or(|fields| fields.len() != 4) {
                            return Err(ModelClientError::new(
                                ModelClientErrorKind::Provider,
                                "api model failure fields mismatch",
                                false,
                            ));
                        }
                        let failure = serde_json::from_value::<ModelStreamFailure>(event).map_err(
                            |error| {
                                ModelClientError::new(
                                    ModelClientErrorKind::Provider,
                                    format!("invalid api model failure: {error}"),
                                    false,
                                )
                            },
                        )?;
                        return Err(hosted_model_error(failure)?);
                    }
                    Some("reasoning") => {
                        if event.as_object().is_none_or(|fields| fields.len() != 3) {
                            return Err(ModelClientError::new(
                                ModelClientErrorKind::ProviderResponseInterrupted,
                                "api reasoning fields mismatch",
                                false,
                            ));
                        }
                        let text = event
                            .get("text")
                            .and_then(serde_json::Value::as_str)
                            .ok_or_else(|| {
                                ModelClientError::new(
                                    ModelClientErrorKind::ProviderResponseInterrupted,
                                    "api reasoning text is invalid",
                                    false,
                                )
                            })?;
                        sink(ModelClientStreamEvent::Reasoning {
                            text: text.to_string(),
                        });
                    }
                    Some("delta") => {
                        let delta = event
                            .get("delta")
                            .and_then(serde_json::Value::as_str)
                            .ok_or_else(|| {
                                ModelClientError::new(
                                    ModelClientErrorKind::ProviderResponseInterrupted,
                                    "api model stream delta is missing",
                                    false,
                                )
                            })?;
                        streamed_text.push_str(delta);
                        sink(ModelClientStreamEvent::Token {
                            content: delta.to_string(),
                        });
                    }
                    Some("result") => {
                        let mut event = event;
                        let fields = event.as_object_mut().expect("validated result object");
                        fields.remove("schema");
                        fields.remove("type");
                        result = Some(serde_json::from_value::<ModelRunResponse>(event).map_err(
                            |error| {
                                ModelClientError::new(
                                    ModelClientErrorKind::ProviderResponseInterrupted,
                                    format!("decode api model stream result failed: {error}"),
                                    false,
                                )
                            },
                        )?);
                        terminal_received = true;
                    }
                    _ => {
                        return Err(ModelClientError::new(
                            ModelClientErrorKind::ProviderResponseInterrupted,
                            "api model stream event type is unsupported",
                            false,
                        ));
                    }
                }
            }
        }
        decoder.finish().map_err(|message| {
            ModelClientError::new(
                ModelClientErrorKind::ProviderResponseInterrupted,
                format!("api model stream {message}"),
                true,
            )
        })?;
        let payload = result.ok_or_else(|| {
            ModelClientError::new(
                ModelClientErrorKind::ProviderResponseInterrupted,
                "api model stream ended without result",
                true,
            )
        })?;
        if payload.text != streamed_text {
            return Err(ModelClientError::new(
                ModelClientErrorKind::ProviderResponseInterrupted,
                "api model stream text/result mismatch",
                false,
            ));
        }
        let response = model_response(payload)?;
        for call in &response.generate_result.tool_calls {
            sink(ModelClientStreamEvent::ToolCallReady {
                call_id: call.id.clone(),
                provider_item_id: None,
                name: call.name.clone(),
                args_json: call.args_json.clone(),
                args_preview: String::new(),
            });
        }
        Ok(response)
    }

    async fn call_api_stream_with_retries(
        &self,
        request: &ModelClientRequest,
        sink: &mut (dyn FnMut(ModelClientStreamEvent) + Send),
    ) -> Result<ModelClientResponse, ModelClientError> {
        let max_retries = request.session_config.max_retries;
        for retry in 0..=max_retries {
            let mut attempt_has_visible_content = false;
            let mut attempt_has_reasoning = false;
            let result = {
                let mut attempt_sink = |event| {
                    if matches!(&event, ModelClientStreamEvent::Reasoning { text } if !text.is_empty())
                    {
                        attempt_has_reasoning = true;
                    }
                    if matches!(&event, ModelClientStreamEvent::Token { content } if !content.is_empty())
                    {
                        attempt_has_visible_content = true;
                    }
                    sink(event);
                };
                self.call_api_stream(request, &mut attempt_sink).await
            };
            match result {
                Ok(mut response) => {
                    response.provider_attempts = retry.saturating_add(1);
                    return Ok(response);
                }
                Err(mut error) => {
                    error.provider_attempts = retry.saturating_add(1);
                    if !error.retryable || retry == max_retries {
                        if retry == max_retries {
                            error.retryable = false;
                        }
                        return Err(error);
                    }
                    if attempt_has_visible_content {
                        sink(ModelClientStreamEvent::ReplaceContent {
                            content: String::new(),
                        });
                    }
                    if attempt_has_reasoning {
                        sink(ModelClientStreamEvent::Reasoning {
                            text: String::new(),
                        });
                    }
                    sink(ModelClientStreamEvent::Status {
                        message: None,
                        process_state: RuntimeProcessState::Retrying,
                    });
                    sleep_before_retry(
                        retry.saturating_add(1),
                        request.session_config.retry_backoff_ms,
                    )
                    .await;
                }
            }
        }
        unreachable!("model stream retry loop always returns")
    }
}

impl ModelClient for ApiModelClient {
    fn generate<'a>(
        &'a self,
        request: &'a ModelClientRequest,
    ) -> ModelClientFuture<'a, ModelClientResponse> {
        Box::pin(async move { self.call_api_with_retries(request).await })
    }

    fn generate_stream<'a>(
        &'a self,
        request: &'a ModelClientRequest,
        sink: &'a mut (dyn FnMut(ModelClientStreamEvent) + Send),
    ) -> ModelClientFuture<'a, ModelClientResponse> {
        Box::pin(async move {
            let response = self.call_api_stream_with_retries(request, sink).await?;
            sink(ModelClientStreamEvent::Done {
                finish_reason: Some("stop".to_string()),
            });
            Ok(response)
        })
    }
}

async fn sleep_before_retry(attempt: u32, retry_backoff_ms: u64) {
    let delay_ms = retry_backoff_ms.saturating_mul(u64::from(attempt));
    if delay_ms > 0 {
        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
    }
}

fn stream_error(label: &str, error: reqwest::Error) -> ModelClientError {
    ModelClientError::new(
        ModelClientErrorKind::ProviderResponseInterrupted,
        format!("{label}: {error}"),
        true,
    )
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ModelStreamFailure {
    schema: String,
    #[serde(rename = "type")]
    event_type: String,
    reason_type: String,
    http_status: Option<u16>,
}

fn canonical_failure_reason(reason: &str) -> bool {
    !reason.is_empty()
        && reason.len() <= 128
        && reason
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

pub(super) fn hosted_model_failure_reason(error: &str) -> Option<&str> {
    let (_, reason) = error.rsplit_once("hosted_model_error: ")?;
    canonical_failure_reason(reason).then_some(reason)
}

fn hosted_model_error(failure: ModelStreamFailure) -> Result<ModelClientError, ModelClientError> {
    if failure.schema != "api.model.stream.v1"
        || failure.event_type != "error"
        || !canonical_failure_reason(&failure.reason_type)
        || failure
            .http_status
            .is_some_and(|status| !(400..=599).contains(&status))
    {
        return Err(ModelClientError::new(
            ModelClientErrorKind::Provider,
            "invalid api model failure",
            false,
        ));
    }
    let (kind, retryable) = match failure.reason_type.as_str() {
        "provider_stream_interrupted" => (ModelClientErrorKind::ProviderResponseInterrupted, true),
        "provider_timeout" => (ModelClientErrorKind::Timeout, true),
        "provider_request_rejected" if failure.http_status == Some(408) => {
            (ModelClientErrorKind::Timeout, true)
        }
        "provider_unreachable" => (ModelClientErrorKind::Network, true),
        "provider_rate_limited" => (ModelClientErrorKind::ProviderBusyOrRateLimited, true),
        "provider_unavailable" if failure.http_status.is_some_and(|status| status >= 500) => {
            (ModelClientErrorKind::ProviderUnavailable, true)
        }
        "provider_unavailable" => (ModelClientErrorKind::ModelUnavailable, false),
        "provider_authentication_failed"
        | "provider_secret_unavailable"
        | "provider_secret_decryption_failed" => (ModelClientErrorKind::AuthFailed, false),
        reason
            if reason.starts_with("model_quota_")
                || reason.starts_with("prepared_prompt_")
                || reason == "provider_request_rejected"
                || reason == "model_thinking_mode_unsupported" =>
        {
            (ModelClientErrorKind::InvalidRequest, false)
        }
        "model_run_cancelled" => (ModelClientErrorKind::Cancelled, false),
        _ => (ModelClientErrorKind::Provider, false),
    };
    let mut error = ModelClientError::new(
        kind,
        format!("hosted_model_error: {}", failure.reason_type),
        retryable,
    );
    error.provider_code = Some(failure.reason_type);
    Ok(error)
}

fn model_response(payload: ModelRunResponse) -> Result<ModelClientResponse, ModelClientError> {
    let tool_calls = payload
        .tool_calls
        .into_iter()
        .map(|call| ToolCallEnvelope {
            id: call.id,
            name: call.name,
            args_json: call.args_json,
        })
        .collect::<Vec<_>>();
    validate_provider_tool_call_arguments("api.model.run.v1", &tool_calls)?;
    Ok(ModelClientResponse {
        generate_result: GenerateResult {
            content: payload.text,
            tool_calls,
            continuation_reasoning_content: payload.continuation_reasoning_content,
            reasoning_content: payload.reasoning_content,
            input_tokens: payload.usage.as_ref().and_then(|usage| usage.prompt_tokens),
            total_tokens: payload.usage.as_ref().and_then(|usage| usage.total_tokens),
            prompt_cache_hit_tokens: payload
                .usage
                .as_ref()
                .and_then(|usage| usage.prompt_cache_hit_tokens),
            prompt_cache_miss_tokens: payload
                .usage
                .as_ref()
                .and_then(|usage| usage.prompt_cache_miss_tokens),
        },
        provider_request_id: None,
        provider_latency_ms: None,
        provider_attempts: 1,
    })
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ModelRunRequest<'a> {
    schema: &'static str,
    agent_run_id: &'a str,
    model_config_ref: &'a str,
    authorization_ref: &'a str,
    authorization_digest: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking_mode: Option<&'a str>,
    max_output_tokens: u32,
    prepared_prompt: &'a PreparedPromptV1,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ModelRunResponse {
    text: String,
    reasoning_content: Option<String>,
    continuation_reasoning_content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<ModelRunToolCall>,
    usage: Option<ModelRunUsage>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModelRunToolCall {
    id: String,
    name: String,
    args_json: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
struct ModelRunUsage {
    prompt_tokens: Option<i64>,
    total_tokens: Option<i64>,
    prompt_cache_hit_tokens: Option<i64>,
    prompt_cache_miss_tokens: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use centaeris_core::model::prepared_prompt::{ModelMessageRoleV1, ModelMessageV1};
    use centaeris_core::model::ModelSessionConfig;
    use centaeris_core::tool::ModelToolChoice;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn read_http_request(stream: &mut tokio::net::TcpStream) {
        let mut request = Vec::new();
        let mut buffer = [0_u8; 4_096];
        loop {
            let read = stream.read(&mut buffer).await.expect("read request");
            if read == 0 {
                return;
            }
            request.extend_from_slice(&buffer[..read]);
            let Some(header_end) = request.windows(4).position(|window| window == b"\r\n\r\n")
            else {
                continue;
            };
            let headers = String::from_utf8_lossy(&request[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(str::trim)
                        .map(str::to_string)
                })
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or_default();
            if request.len() >= header_end + 4 + content_length {
                return;
            }
        }
    }

    #[test]
    fn sse_decoder_tolerates_chunk_boundaries_cutting_multibyte_utf8() {
        let mut decoder = SseFrameDecoder::new();
        // "中" = E4 B8 AD；构造 chunk 边界恰好切开它：
        // chunk1 以 E4 B8 结尾（残缺），chunk2 以 AD 开头补全。
        let frame =
            "data: {\"schema\":\"api.model.stream.v1\",\"type\":\"delta\",\"delta\":\"中\"}\n\n";
        let bytes = frame.as_bytes();
        let cut = bytes
            .iter()
            .position(|byte| *byte == 0xAD)
            .expect("cut point");
        let chunk1 = &bytes[..cut];
        let chunk2 = &bytes[cut..];
        assert!(
            std::str::from_utf8(chunk1).is_err(),
            "chunk1 must cut the multibyte character"
        );
        assert_eq!(
            decoder.push(chunk1).expect("partial chunk"),
            Vec::<String>::new(),
            "incomplete UTF-8 must wait for the next chunk"
        );
        let frames = decoder.push(chunk2).expect("completing chunk");
        assert_eq!(frames.len(), 1);
        let event: serde_json::Value =
            serde_json::from_str(frames[0].as_str()).expect("decoded frame");
        assert_eq!(event["delta"], "中");
        decoder.finish().expect("clean finish");
    }

    #[test]
    fn sse_decoder_rejects_invalid_utf8_and_incomplete_tail() {
        let mut decoder = SseFrameDecoder::new();
        let error = decoder
            .push(&[0x41, 0xFF, 0x42])
            .expect_err("invalid byte must fail");
        assert!(error.contains("not UTF-8"));

        let mut decoder = SseFrameDecoder::new();
        assert_eq!(
            decoder
                .push(b"data: {\"type\":\"delta\"}")
                .expect("no frame yet"),
            Vec::<String>::new(),
            "frame without \\n\\n terminator waits"
        );
        let error = decoder.finish().expect_err("unterminated tail must fail");
        assert!(error.contains("incomplete frame"));
    }

    #[test]
    fn sse_decoder_splits_multiple_frames_in_one_chunk_and_drops_empty() {
        let mut decoder = SseFrameDecoder::new();
        let chunk = b"data: {\"a\":1}\n\ndata: {\"b\":2}\n\n";
        let frames = decoder.push(chunk).expect("frames");
        assert_eq!(
            frames,
            vec![r#"{"a":1}"#.to_string(), r#"{"b":2}"#.to_string()]
        );
        decoder.finish().expect("clean finish");
    }

    fn model_request() -> ModelClientRequest {
        ModelClientRequest {
            session_id: "chat-1".to_string(),
            turn_id: "turn-1".to_string(),
            loop_index: 0,
            provider_prompt_cache_key: None,
            provider_prompt_cache_retention: None,
            system_prompt_manifest_json: None,
            compression_stats_json: None,
            context_token_estimate: 16,
            prepared_prompt: PreparedPromptV1::new(
                Some("system".to_string()),
                vec![ModelMessageV1 {
                    message_id: "user-1".to_string(),
                    role: ModelMessageRoleV1::User,
                    content: "hello".to_string(),
                    tool_calls: vec![],
                    tool_call_id: None,
                    reasoning_content: None,
                }],
                vec![],
                ModelToolChoice::None,
                1_024,
            )
            .expect("prepared prompt"),
            session_config: ModelSessionConfig::default(),
        }
    }

    fn model_client(
        api_internal_url: String,
        thinking_mode: Option<String>,
        model_max_output_tokens: u32,
    ) -> ApiModelClient {
        ApiModelClient::new(ApiModelClientConfig {
            api_internal_url,
            internal_api_token: "token".to_string(),
            agent_run_id: "agent-run-1".to_string(),
            model_config_ref: "model-1".to_string(),
            authorization_ref: "authorization-1".to_string(),
            authorization_digest: "sha256:authorization".to_string(),
            thinking_mode,
            model_max_output_tokens,
        })
    }

    #[test]
    fn model_run_wire_body_contains_only_the_prepared_prompt_contract() {
        let client = model_client("http://localhost".to_string(), None, 1_024);
        let request = model_request();
        let value = serde_json::to_value(
            client
                .model_run_request(&request)
                .expect("model run request"),
        )
        .expect("serialize model run request");
        let object = value.as_object().expect("wire object");

        assert_eq!(object.len(), 7);
        assert_eq!(value["schema"], "api.model.run.v1");
        assert!(object.get("thinkingMode").is_none());
        assert_eq!(value["preparedPrompt"]["schema"], "prepared_prompt.v1");
        assert!(object.get("userMessage").is_none());
        assert!(object.get("contextMessages").is_none());
        assert!(object.get("systemPrompt").is_none());
        assert!(object.get("toolDefinitions").is_none());
        assert!(object.get("toolChoice").is_none());
    }

    #[test]
    fn model_run_wire_body_accepts_output_limit_below_model_capability() {
        let client = model_client(
            "http://localhost".to_string(),
            Some("high".to_string()),
            2_048,
        );
        let model_request = model_request();
        let request = client
            .model_run_request(&model_request)
            .expect("request-local output limit below model capability is valid");
        assert_eq!(request.max_output_tokens, 1_024);
        assert_eq!(request.thinking_mode, Some("high"));
    }

    #[test]
    fn model_run_wire_body_rejects_output_limit_above_model_capability() {
        let client = model_client("http://localhost".to_string(), None, 512);
        let error = client
            .model_run_request(&model_request())
            .expect_err("output limit above model capability must fail");
        assert_eq!(error.kind, ModelClientErrorKind::InvalidRequest);
        assert!(error.message.contains("output limit exceeded"));
    }

    #[test]
    fn model_response_rejects_malformed_tool_arguments_before_commit() {
        let error = model_response(ModelRunResponse {
            text: String::new(),
            continuation_reasoning_content: None,
            reasoning_content: None,
            tool_calls: vec![ModelRunToolCall {
                id: "call-1".to_string(),
                name: "bash".to_string(),
                args_json: r#"{"command":"unterminated""#.to_string(),
            }],
            usage: None,
        })
        .expect_err("malformed tool arguments must fail loudly");

        assert_eq!(error.kind, ModelClientErrorKind::Provider);
        assert_eq!(
            error.provider_code.as_deref(),
            Some("malformed_tool_call_arguments")
        );
        assert!(!error.retryable);
    }

    #[test]
    fn model_response_preserves_reasoning_content() {
        let payload = serde_json::from_value::<ModelRunResponse>(serde_json::json!({
            "text": "final answer",
            "reasoningContent": "inspect request",
            "toolCalls": [],
            "usage": null
        }))
        .expect("model run response wire payload");
        let response = model_response(payload).expect("model response");

        assert_eq!(
            response.generate_result.reasoning_content.as_deref(),
            Some("inspect request")
        );
        assert!(response
            .generate_result
            .continuation_reasoning_content
            .is_none());
        let payload = serde_json::from_value::<ModelRunResponse>(serde_json::json!({
            "text": "answer", "reasoningContent": "display", "continuationReasoningContent": "provider continuation", "toolCalls": [], "usage": null
        })).unwrap();
        let result = model_response(payload).unwrap().generate_result;
        assert_eq!(result.reasoning_content.as_deref(), Some("display"));
        assert_eq!(
            result.continuation_reasoning_content.as_deref(),
            Some("provider continuation")
        );
    }

    #[test]
    fn model_response_rejects_reasoning_field_aliases_and_wrong_types() {
        for field in [
            "reasoning_content",
            "continuation_reasoning_content",
            "unknown",
        ] {
            let mut payload = serde_json::json!({"text":"done", "toolCalls":[], "usage":null});
            payload[field] = serde_json::json!("not canonical");
            assert!(
                serde_json::from_value::<ModelRunResponse>(payload).is_err(),
                "accepted {field}"
            );
        }
        for field in ["reasoningContent", "continuationReasoningContent"] {
            let mut payload = serde_json::json!({"text":"done", "toolCalls":[], "usage":null});
            payload[field] = serde_json::json!(42);
            assert!(
                serde_json::from_value::<ModelRunResponse>(payload).is_err(),
                "accepted wrong type for {field}"
            );
        }
    }

    #[tokio::test]
    async fn api_model_stream_configuration_failure_stops_after_one_attempt() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut connection, _) = listener.accept().await.unwrap();
            read_http_request(&mut connection).await;
            let body = "data: {\"schema\":\"api.model.stream.v1\",\"type\":\"error\",\"reasonType\":\"model_quota_domain_required\",\"httpStatus\":null}\n\n";
            connection.write_all(format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(), body
            ).as_bytes()).await.unwrap();
            connection.shutdown().await.unwrap();
        });
        let client = model_client(format!("http://{address}"), None, 1_024);
        let mut request = model_request();
        request.session_config.max_retries = 5;
        request.session_config.retry_backoff_ms = 0;
        let mut events = Vec::new();
        let error = client
            .generate_stream(&request, &mut |event| events.push(event))
            .await
            .unwrap_err();
        tokio::time::timeout(std::time::Duration::from_secs(5), server)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(error.kind, ModelClientErrorKind::InvalidRequest);
        assert_eq!(
            error.provider_code.as_deref(),
            Some("model_quota_domain_required")
        );
        assert_eq!(error.provider_attempts, 1);
        assert!(!error.retryable);
        assert!(events.is_empty());
    }

    #[test]
    fn hosted_model_errors_preserve_failure_facts_and_reject_malformed_frames() {
        for (reason, status, kind, retryable) in [
            (
                "model_quota_domain_disabled",
                None,
                ModelClientErrorKind::InvalidRequest,
                false,
            ),
            (
                "model_quota_backend_unavailable",
                None,
                ModelClientErrorKind::InvalidRequest,
                false,
            ),
            (
                "provider_authentication_failed",
                Some(401),
                ModelClientErrorKind::AuthFailed,
                false,
            ),
            (
                "provider_request_rejected",
                Some(408),
                ModelClientErrorKind::Timeout,
                true,
            ),
            (
                "provider_unavailable",
                None,
                ModelClientErrorKind::ModelUnavailable,
                false,
            ),
            (
                "provider_unavailable",
                Some(503),
                ModelClientErrorKind::ProviderUnavailable,
                true,
            ),
            (
                "provider_stream_interrupted",
                None,
                ModelClientErrorKind::ProviderResponseInterrupted,
                true,
            ),
        ] {
            let failure = serde_json::from_value(serde_json::json!({
                "schema": "api.model.stream.v1", "type": "error", "reasonType": reason, "httpStatus": status,
            })).unwrap();
            let error = hosted_model_error(failure).unwrap();
            assert_eq!(error.kind, kind);
            assert_eq!(error.retryable, retryable);
            assert_eq!(hosted_model_failure_reason(&error.message), Some(reason));
        }
        for (field, value) in [
            ("reason_type", serde_json::json!("wrong")),
            ("retryable", serde_json::json!(true)),
        ] {
            let mut failure = serde_json::json!({"schema":"api.model.stream.v1","type":"error","reasonType":"model_adapter_failed","httpStatus":null});
            failure[field] = value;
            assert!(serde_json::from_value::<ModelStreamFailure>(failure).is_err());
        }
        assert!(
            hosted_model_failure_reason("hosted_model_error: secret response with spaces")
                .is_none()
        );
    }

    #[tokio::test]
    async fn api_model_stream_retries_interrupted_body_and_replaces_partial_content() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let address = listener.local_addr().expect("test server address");
        let server = tokio::spawn(async move {
            let (mut first, _) = listener.accept().await.expect("first request");
            read_http_request(&mut first).await;
            let partial = b"data: {\"schema\":\"api.model.stream.v1\",\"type\":\"reasoning\",\"text\":\"first attempt\"}\n\ndata: {\"schema\":\"api.model.stream.v1\",\"type\":\"delta\",\"delta\":\"partial\"}\n\n";
            first
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
                )
                .await
                .expect("first response headers");
            first
                .write_all(format!("{:X}\r\n", partial.len()).as_bytes())
                .await
                .expect("first chunk size");
            first.write_all(partial).await.expect("first chunk");
            first.write_all(b"\r\n").await.expect("first chunk end");
            first.shutdown().await.expect("interrupt first response");

            let (mut second, _) = listener.accept().await.expect("retry request");
            read_http_request(&mut second).await;
            let body = concat!(
                "data: {\"schema\":\"api.model.stream.v1\",\"type\":\"reasoning\",\"text\":\"second attempt\"}\n\n",
                "data: {\"schema\":\"api.model.stream.v1\",\"type\":\"delta\",\"delta\":\"recovered\"}\n\n",
                "data: {\"schema\":\"api.model.stream.v1\",\"type\":\"result\",\"text\":\"recovered\",\"reasoningContent\":null,\"toolCalls\":[],\"usage\":null}\n\n"
            );
            second
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(), body
                    )
                    .as_bytes(),
                )
                .await
                .expect("retry response");
            second.shutdown().await.expect("complete retry response");
        });

        let client = model_client(format!("http://{address}"), None, 1_024);
        let mut request = model_request();
        request.session_config.max_retries = 1;
        request.session_config.retry_backoff_ms = 0;
        let mut events = Vec::new();
        let response = client
            .generate_stream(&request, &mut |event| events.push(event))
            .await
            .expect("retry should recover the model stream");
        server.await.expect("test server");

        assert_eq!(response.provider_attempts, 2);
        let reasoning: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                ModelClientStreamEvent::Reasoning { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(reasoning, ["first attempt", "", "second attempt"]);
        let events: Vec<_> = events
            .iter()
            .filter(|event| !matches!(event, ModelClientStreamEvent::Reasoning { .. }))
            .collect();
        assert!(
            matches!(&events[0], ModelClientStreamEvent::Token { content } if content == "partial")
        );
        assert!(
            matches!(&events[1], ModelClientStreamEvent::ReplaceContent { content } if content.is_empty())
        );
        assert!(matches!(
            &events[2],
            ModelClientStreamEvent::Status {
                process_state: RuntimeProcessState::Retrying,
                ..
            }
        ));
        assert!(
            matches!(&events[3], ModelClientStreamEvent::Token { content } if content == "recovered")
        );
        assert!(matches!(&events[4], ModelClientStreamEvent::Done { .. }));
    }
}
