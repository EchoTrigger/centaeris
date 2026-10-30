use centaeris_core::model::prepared_prompt::PreparedPromptV1;
use centaeris_core::model::TruncatedToolCall;
use centaeris_core::model::{
    validate_provider_tool_call_arguments, ModelClient, ModelClientError, ModelClientErrorKind,
    ModelClientFuture, ModelClientRequest, ModelClientResponse, ModelClientStreamEvent, RetryAfter,
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
    timings: crate::observations::RunTimings,
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
        let timings = crate::observations::RunTimings::new(
            &format!("agent_run.lifecycle:{}", config.agent_run_id),
            &config.agent_run_id,
        );
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
            timings,
        }
    }

    pub fn with_timings(mut self, timings: crate::observations::RunTimings) -> Self {
        self.timings = timings;
        self
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
        attempt: u32,
    ) -> Result<ModelClientResponse, ModelClientError> {
        let model_run_request = self.model_run_request(request)?;
        let mut observation = self.timings.model_http(&request.turn_id, attempt, false);
        let result = self
            .call_api_response(
                model_run_request,
                request.session_config.timeout_ms,
                &mut observation,
            )
            .await;
        match &result {
            Ok(response) => observation.complete(
                response
                    .generate_result
                    .tool_calls
                    .iter()
                    .map(|call| call.id.as_str()),
            ),
            Err(_) => observation.finish(false),
        }
        result
    }

    async fn call_api_response(
        &self,
        model_run_request: ModelRunRequest<'_>,
        timeout_ms: u64,
        observation: &mut crate::observations::ModelHttp,
    ) -> Result<ModelClientResponse, ModelClientError> {
        let url = format!(
            "{}/internal/model-runs",
            self.api_internal_url.trim_end_matches('/')
        );
        let response = self
            .client
            .post(url)
            .timeout(model_response_timeout(timeout_ms))
            .header("Content-Type", "application/json")
            .header("X-Internal-Token", self.internal_api_token.as_str())
            .json(&model_run_request)
            .send()
            .await
            .map_err(|error| {
                ModelClientError::new(
                    if error.is_timeout() {
                        ModelClientErrorKind::Timeout
                    } else if error.is_builder() {
                        ModelClientErrorKind::InvalidRequest
                    } else {
                        ModelClientErrorKind::Network
                    },
                    format!("api model run request failed: {error}"),
                    !error.is_builder(),
                )
            })?;
        observation.headers_received();
        let status = response.status();
        let body = response.text().await.map_err(|error| {
            ModelClientError::new(
                ModelClientErrorKind::Network,
                format!("read api model run response failed: {error}"),
                true,
            )
        })?;
        if !status.is_success() {
            if status == reqwest::StatusCode::BAD_GATEWAY {
                if let Ok(failure) = serde_json::from_str::<ModelHttpFailure>(&body) {
                    if failure.error == "model_run_failed" {
                        return Err(hosted_model_error(ModelStreamFailure {
                            schema: "api.model.stream.v1".to_string(),
                            event_type: "error".to_string(),
                            reason_type: failure.reason_type,
                            http_status: failure.http_status,
                            retry_after: failure.retry_after,
                            truncated_tool_calls: failure.truncated_tool_calls,
                        })?);
                    }
                }
            }
            return Err(ModelClientError::new(
                ModelClientErrorKind::Provider,
                format!("api model run returned {}", status.as_u16()),
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
        let response = model_response(payload)?;
        observation.first_event();
        Ok(response)
    }

    async fn call_api_with_retries(
        &self,
        request: &ModelClientRequest,
    ) -> Result<ModelClientResponse, ModelClientError> {
        let max_retries = request.session_config.max_retries;
        for retry in 0..=max_retries {
            match self.call_api(request, retry.saturating_add(1)).await {
                Ok(mut response) => {
                    response.provider_attempts = retry.saturating_add(1);
                    return Ok(response);
                }
                Err(mut error) => {
                    error.provider_attempts = retry.saturating_add(1);
                    if !error.retryable || retry == max_retries {
                        finish_hosted_failure(&mut error, retry == max_retries);
                        if retry == max_retries {
                            error.retryable = false;
                        }
                        return Err(error);
                    }
                    sleep_before_retry(
                        retry.saturating_add(1),
                        request.session_config.retry_backoff_ms,
                        error.retry_after,
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
        attempt: u32,
    ) -> Result<ModelClientResponse, ModelClientError> {
        let model_run_request = self.model_run_request(request)?;
        let mut observation = self.timings.model_http(&request.turn_id, attempt, true);
        let result = self
            .call_api_stream_response(
                model_run_request,
                request.session_config.timeout_ms,
                sink,
                &mut observation,
            )
            .await;
        if result.is_err() {
            observation.finish(false);
        }
        result
    }

    async fn call_api_stream_response(
        &self,
        model_run_request: ModelRunRequest<'_>,
        timeout_ms: u64,
        sink: &mut (dyn FnMut(ModelClientStreamEvent) + Send),
        observation: &mut crate::observations::ModelHttp,
    ) -> Result<ModelClientResponse, ModelClientError> {
        let url = format!(
            "{}/internal/model-runs",
            self.api_internal_url.trim_end_matches('/')
        );
        let pending = self
            .client
            .post(url)
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream")
            .header("X-Internal-Token", self.internal_api_token.as_str())
            .json(&model_run_request)
            .send();
        let mut response = tokio::time::timeout(model_response_timeout(timeout_ms), pending)
            .await
            .map_err(|_| {
                ModelClientError::new(
                    ModelClientErrorKind::Timeout,
                    "api model stream response headers timed out",
                    true,
                )
            })?
            .map_err(|error| stream_error("api model stream request failed", error))?;
        observation.headers_received();
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
            let received = tokio::time::timeout(
                std::time::Duration::from_millis(
                    centaeris_core::model::DEFAULT_MODEL_SSE_IDLE_TIMEOUT_MS,
                ),
                response.chunk(),
            )
            .await
            .map_err(|_| {
                ModelClientError::new(
                    ModelClientErrorKind::Timeout,
                    "api model stream idle timeout",
                    true,
                )
            })?;
            let chunk = match received {
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
                        let failure = serde_json::from_value::<ModelStreamFailure>(event).map_err(
                            |error| {
                                ModelClientError::new(
                                    ModelClientErrorKind::Provider,
                                    format!("invalid api model failure: {error}"),
                                    false,
                                )
                            },
                        )?;
                        let error = hosted_model_error(failure)?;
                        observation.first_event();
                        return Err(error);
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
                        observation.first_event();
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
                        observation.first_event();
                        observation.text_delta(delta);
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
                        observation.first_event();
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
        observation.complete(
            response
                .generate_result
                .tool_calls
                .iter()
                .map(|call| call.id.as_str()),
        );
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
                self.call_api_stream(request, &mut attempt_sink, retry.saturating_add(1))
                    .await
            };
            match result {
                Ok(mut response) => {
                    response.provider_attempts = retry.saturating_add(1);
                    return Ok(response);
                }
                Err(mut error) => {
                    error.provider_attempts = retry.saturating_add(1);
                    if !error.retryable || retry == max_retries {
                        finish_hosted_failure(&mut error, retry == max_retries);
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
                        error.retry_after,
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

fn model_response_timeout(configured_ms: u64) -> std::time::Duration {
    std::time::Duration::from_millis(if configured_ms == 0 {
        centaeris_core::model::DEFAULT_MODEL_RESPONSE_HEADERS_TIMEOUT_MS
    } else {
        configured_ms
    })
}

async fn sleep_before_retry(attempt: u32, retry_backoff_ms: u64, advice: Option<RetryAfter>) {
    if let Some(advice) = advice {
        advice.wait().await;
        return;
    }
    let delay_ms = retry_backoff_ms.saturating_mul(u64::from(attempt));
    if delay_ms > 0 {
        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
    }
}

fn stream_error(label: &str, error: reqwest::Error) -> ModelClientError {
    let kind = if error.is_timeout() {
        ModelClientErrorKind::Timeout
    } else if error.is_builder() {
        ModelClientErrorKind::InvalidRequest
    } else if error.is_connect() {
        ModelClientErrorKind::Network
    } else {
        ModelClientErrorKind::ProviderResponseInterrupted
    };
    ModelClientError::new(kind, format!("{label}: {error}"), !error.is_builder())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ModelStreamFailure {
    #[serde(default)]
    retry_after: Option<String>,
    schema: String,
    #[serde(rename = "type")]
    event_type: String,
    reason_type: String,
    #[serde(deserialize_with = "required_optional_http_status")]
    http_status: Option<u16>,
    #[serde(default, deserialize_with = "present_truncated_tool_calls")]
    truncated_tool_calls: Option<Vec<ModelTruncatedToolCall>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ModelHttpFailure {
    #[serde(default)]
    retry_after: Option<String>,
    error: String,
    reason_type: String,
    #[serde(deserialize_with = "required_optional_http_status")]
    http_status: Option<u16>,
    #[serde(default, deserialize_with = "present_truncated_tool_calls")]
    truncated_tool_calls: Option<Vec<ModelTruncatedToolCall>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ModelTruncatedToolCall {
    #[serde(deserialize_with = "required_optional_string")]
    call_id: Option<String>,
    #[serde(deserialize_with = "required_optional_string")]
    tool_name: Option<String>,
    args_bytes: usize,
    args_sha256: String,
}

fn required_optional_string<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    Option::<String>::deserialize(deserializer)
}

fn present_truncated_tool_calls<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Vec<ModelTruncatedToolCall>>, D::Error> {
    Vec::<ModelTruncatedToolCall>::deserialize(deserializer).map(Some)
}

fn canonical_failure_reason(reason: &str) -> bool {
    !reason.is_empty()
        && reason.len() <= 128
        && reason
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

// Host-owned diagnostic data crosses the existing Core string-error boundary.
// Only this versioned, sanitized envelope may become a public failure message.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct HostedModelFailure {
    schema: String,
    pub reason_type: String,
    #[serde(deserialize_with = "required_optional_http_status")]
    http_status: Option<u16>,
    provider_attempts: u32,
    retry_exhausted: bool,
}

fn required_optional_http_status<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<u16>, D::Error> {
    Option::<u16>::deserialize(deserializer)
}

impl HostedModelFailure {
    fn encoded(&self) -> String {
        format!(
            "hosted_model_error: {}",
            serde_json::to_string(self).expect("failure contains only scalar fields")
        )
    }

    pub fn public_message(&self) -> String {
        let mut message = if self.retry_exhausted {
            format!(
                "exceeded retry limit after {} attempts",
                self.provider_attempts
            )
        } else if self.http_status.is_some() {
            "model request failed".to_string()
        } else {
            return self.reason_type.clone();
        };
        if let Some(status) = self.http_status {
            let title = reqwest::StatusCode::from_u16(status)
                .ok()
                .and_then(|status| status.canonical_reason())
                .unwrap_or("HTTP Error");
            message.push_str(&format!(
                ", {}status: {status} {title}",
                if self.retry_exhausted { "last " } else { "" }
            ));
        }
        message.push_str(&format!(" · {}", self.reason_type));
        message
    }
}

pub(super) fn hosted_model_failure(error: &str) -> Option<HostedModelFailure> {
    let json = error.strip_prefix("hosted_model_error: ").or_else(|| {
        error
            .strip_prefix("model_client_error(")?
            .split_once("): hosted_model_error: ")
            .map(|(_, json)| json)
    })?;
    let failure: HostedModelFailure = serde_json::from_str(json).ok()?;
    (failure.schema == "hosted.model.failure.v1"
        && canonical_failure_reason(&failure.reason_type)
        && failure.provider_attempts > 0
        && failure
            .http_status
            .is_none_or(|status| (400..=599).contains(&status)))
    .then_some(failure)
}

fn finish_hosted_failure(error: &mut ModelClientError, reached_limit: bool) {
    let failure = hosted_model_failure(&error.message).or_else(|| {
        if error.provider_code.is_some() {
            return None;
        }
        let reason = match error.kind {
            ModelClientErrorKind::Timeout => "provider_timeout",
            ModelClientErrorKind::Network => "provider_unreachable",
            ModelClientErrorKind::ProviderResponseInterrupted => "provider_stream_interrupted",
            _ => return None,
        };
        Some(HostedModelFailure {
            schema: "hosted.model.failure.v1".to_string(),
            reason_type: reason.to_string(),
            http_status: None,
            provider_attempts: error.provider_attempts,
            retry_exhausted: false,
        })
    });
    if let Some(mut failure) = failure {
        failure.provider_attempts = error.provider_attempts;
        failure.retry_exhausted = reached_limit && error.retryable;
        error.provider_code = Some(failure.reason_type.clone());
        error.message = failure.encoded();
    }
}

fn hosted_model_error(failure: ModelStreamFailure) -> Result<ModelClientError, ModelClientError> {
    if failure.schema != "api.model.stream.v1"
        || failure.event_type != "error"
        || !canonical_failure_reason(&failure.reason_type)
        || failure
            .http_status
            .is_some_and(|status| !(400..=599).contains(&status))
        || (failure.reason_type == "incomplete_output_token_limit")
            != failure.truncated_tool_calls.is_some()
        || failure.truncated_tool_calls.as_ref().is_some_and(|calls| {
            calls.iter().any(|call| {
                call.call_id
                    .as_ref()
                    .is_some_and(|id| id.is_empty() || id.trim() != id)
                    || call.tool_name.as_ref().is_some_and(|name| {
                        name.is_empty()
                            || name.len() > 128
                            || name.starts_with('_')
                            || name.ends_with('_')
                            || name.contains("__")
                            || !name.bytes().all(|byte| {
                                byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'
                            })
                    })
                    || call
                        .args_sha256
                        .strip_prefix("sha256:")
                        .is_none_or(|digest| {
                            digest.len() != 64
                                || !digest.bytes().all(|byte| {
                                    byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
                                })
                        })
            })
        })
    {
        return Err(ModelClientError::new(
            ModelClientErrorKind::Provider,
            "invalid api model failure",
            false,
        ));
    }
    let (kind, retryable) = match failure.reason_type.as_str() {
        "incomplete_output_token_limit" => {
            (ModelClientErrorKind::ProviderResponseInterrupted, false)
        }
        "provider_stream_interrupted" | "provider_response_invalid" | "empty_final_response" => {
            (ModelClientErrorKind::ProviderResponseInterrupted, true)
        }
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
            if reason.starts_with("prepared_prompt_")
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
        HostedModelFailure {
            schema: "hosted.model.failure.v1".to_string(),
            reason_type: failure.reason_type.clone(),
            http_status: failure.http_status,
            provider_attempts: 1,
            retry_exhausted: false,
        }
        .encoded(),
        retryable,
    );
    error.retry_after = failure.retry_after.as_deref().and_then(RetryAfter::parse);
    error.provider_code = Some(failure.reason_type);
    error.truncated_tool_calls = failure
        .truncated_tool_calls
        .unwrap_or_default()
        .into_iter()
        .map(|call| TruncatedToolCall {
            call_id: call.call_id,
            tool_name: call.tool_name,
            args_bytes: call.args_bytes,
            args_sha256: call.args_sha256,
        })
        .collect();
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
        transport_timings: None,
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn model_observations_follow_valid_events_nonempty_text_and_full_response() {
        use std::sync::{Arc, Mutex};
        use std::time::{Duration, Instant};
        let base = Instant::now();
        let elapsed = Arc::new(Mutex::new(Duration::ZERO));
        let clock = elapsed.clone();
        let (timings, records) = crate::observations::RunTimings::for_test(
            "agent_run.lifecycle:observed-run",
            "observed-run",
            Arc::new(move || base + *clock.lock().unwrap()),
        );
        timings.request_started("turn-1", "model_request:observed");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (next_text, text_ready) = tokio::sync::oneshot::channel();
        let (next_result, result_ready) = tokio::sync::oneshot::channel();
        let (result_sent, result_received) = tokio::sync::oneshot::channel();
        let (finish_body, finish_ready) = tokio::sync::oneshot::channel();
        let server_clock = elapsed.clone();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            read_http_request(&mut stream).await;
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n").await.unwrap();
            async fn frame(stream: &mut tokio::net::TcpStream, value: serde_json::Value) {
                let body = format!("data: {value}\n\n");
                stream
                    .write_all(format!("{:X}\r\n{body}\r\n", body.len()).as_bytes())
                    .await
                    .unwrap();
            }
            *server_clock.lock().unwrap() = Duration::from_millis(25);
            frame(&mut stream, serde_json::json!({"schema":"api.model.stream.v1","type":"reasoning","text":"private reasoning"})).await;
            text_ready.await.unwrap();
            frame(
                &mut stream,
                serde_json::json!({"schema":"api.model.stream.v1","type":"delta","delta":""}),
            )
            .await;
            frame(&mut stream, serde_json::json!({"schema":"api.model.stream.v1","type":"delta","delta":"private response"})).await;
            result_ready.await.unwrap();
            frame(&mut stream, serde_json::json!({"schema":"api.model.stream.v1","type":"result","text":"private response","reasoningContent":null,"toolCalls":[],"usage":null})).await;
            result_sent.send(()).unwrap();
            finish_ready.await.unwrap();
            stream.write_all(b"0\r\n\r\n").await.unwrap();
            stream.shutdown().await.unwrap();
        });
        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
        let client = model_client(format!("http://{address}"), None, 1_024).with_timings(timings);
        let generation = tokio::spawn(async move {
            client.generate_stream(&model_request(), &mut |event| {
                if matches!(&event, ModelClientStreamEvent::Reasoning { .. }) {
                    event_tx.send("reasoning").unwrap();
                }
                if matches!(&event, ModelClientStreamEvent::Token { content } if !content.is_empty()) {
                    event_tx.send("text").unwrap();
                }
            }).await
        });
        assert_eq!(event_rx.recv().await.unwrap(), "reasoning");
        assert_eq!(records.lock().unwrap().len(), 2);
        assert_eq!(records.lock().unwrap()[1]["phase"], "modelFirstEvent");
        *elapsed.lock().unwrap() = Duration::from_millis(60);
        next_text.send(()).unwrap();
        assert_eq!(event_rx.recv().await.unwrap(), "text");
        assert_eq!(records.lock().unwrap().len(), 3);
        assert_eq!(records.lock().unwrap()[2]["phase"], "modelFirstText");
        *elapsed.lock().unwrap() = Duration::from_millis(200);
        next_result.send(()).unwrap();
        result_received.await.unwrap();
        assert!(!generation.is_finished());
        assert_eq!(records.lock().unwrap().len(), 3);
        *elapsed.lock().unwrap() = Duration::from_millis(260);
        finish_body.send(()).unwrap();
        let response = generation.await.unwrap().unwrap();
        server.await.unwrap();
        assert_eq!(response.generate_result.content, "private response");
        let records = records.lock().unwrap();
        assert_eq!(records.len(), 4);
        assert_eq!(records[0]["phase"], "modelResponseHeaders");
        assert_eq!(records[1]["elapsedMs"], 25.0);
        assert_eq!(records[2]["elapsedMs"], 60.0);
        assert_eq!(records[3]["elapsedMs"], 260.0);
        assert_eq!(records[3]["phase"], "modelHttp");
        assert_eq!(records[3]["outcome"], "ok");
        for record in records.iter() {
            assert_eq!(record["requestId"], "model_request:observed");
            assert_eq!(record["jobId"], "agent_run.lifecycle:observed-run");
            assert!(!record.to_string().contains("private"));
        }
    }

    #[tokio::test]
    async fn api_output_limit_reaches_core_recovery_without_transport_retry_or_ready_tool() {
        for streaming in [false, true] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                read_http_request(&mut stream).await;
                let mut failure = serde_json::json!({
                    "reasonType": "incomplete_output_token_limit", "httpStatus": null,
                    "truncatedToolCalls": [{"callId":"original-call", "toolName":"bash",
                        "argsBytes": 23, "argsSha256": format!("sha256:{}", "a".repeat(64))}],
                });
                let (status, content_type, body) = if streaming {
                    failure["schema"] = "api.model.stream.v1".into();
                    failure["type"] = "error".into();
                    ("200 OK", "text/event-stream", format!(
                        "data: {{\"schema\":\"api.model.stream.v1\",\"type\":\"delta\",\"delta\":\"partial answer\"}}\n\ndata: {failure}\n\n"
                    ))
                } else {
                    failure["error"] = "model_run_failed".into();
                    ("502 Bad Gateway", "application/json", failure.to_string())
                };
                stream.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
                assert!(tokio::time::timeout(std::time::Duration::from_millis(100), listener.accept()).await.is_err(),
                    "output truncation must continue through Core, never retry the original request");
            });
            let client = model_client(format!("http://{address}"), None, 1_024);
            let mut request = model_request();
            request.session_config.max_retries = 2;
            request.session_config.retry_backoff_ms = 0;
            let mut events = vec![];
            let error = if streaming {
                client
                    .generate_stream(&request, &mut |event| events.push(event))
                    .await
                    .unwrap_err()
            } else {
                client.generate(&request).await.unwrap_err()
            };
            assert_eq!(error.provider_attempts, 1);
            assert_eq!(
                error.provider_code.as_deref(),
                Some("incomplete_output_token_limit")
            );
            assert!(!error.retryable);
            assert_eq!(
                error.truncated_tool_calls[0].call_id.as_deref(),
                Some("original-call")
            );
            assert!(events
                .iter()
                .all(|event| !matches!(event, ModelClientStreamEvent::ToolCallReady { .. })));
            let partial = events
                .iter()
                .filter_map(|event| match event {
                    ModelClientStreamEvent::Token { content } => Some(content.as_str()),
                    _ => None,
                })
                .collect::<String>();
            let core_error =
                centaeris_core::runtime::GenerateDriverError::from_model_client(error, partial);
            assert!(core_error.is_output_token_limit());
            assert_eq!(
                core_error.partial_content,
                if streaming { "partial answer" } else { "" }
            );
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn api_empty_final_retries_the_original_request_and_stops_at_existing_limit() {
        for streaming in [false, true] {
            for recover in [false, true] {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let address = listener.local_addr().unwrap();
                let server = tokio::spawn(async move {
                    for attempt in 0..2 {
                        let (mut stream, _) = listener.accept().await.unwrap();
                        read_http_request(&mut stream).await;
                        let success = recover && attempt == 1;
                        let (status, content_type, body) = if streaming {
                            (
                                "200 OK",
                                "text/event-stream",
                                if success {
                                    "data: {\"schema\":\"api.model.stream.v1\",\"type\":\"delta\",\"delta\":\"answer\"}\n\ndata: {\"schema\":\"api.model.stream.v1\",\"type\":\"result\",\"text\":\"answer\",\"toolCalls\":[],\"usage\":null}\n\n".to_string()
                                } else {
                                    "data: {\"schema\":\"api.model.stream.v1\",\"type\":\"error\",\"reasonType\":\"empty_final_response\",\"httpStatus\":null}\n\n".to_string()
                                },
                            )
                        } else if success {
                            (
                                "200 OK",
                                "application/json",
                                "{\"text\":\"answer\",\"toolCalls\":[],\"usage\":null}".to_string(),
                            )
                        } else {
                            ("502 Bad Gateway", "application/json", "{\"error\":\"model_run_failed\",\"reasonType\":\"empty_final_response\",\"httpStatus\":null}".to_string())
                        };
                        stream.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
                    }
                });
                let client = model_client(format!("http://{address}"), None, 1_024);
                let mut request = model_request();
                request.session_config.max_retries = 1;
                request.session_config.retry_backoff_ms = 0;
                let result = if streaming {
                    client.generate_stream(&request, &mut |_| {}).await
                } else {
                    client.generate(&request).await
                };
                if recover {
                    let response = result.unwrap();
                    assert_eq!(response.generate_result.content, "answer");
                    assert_eq!(response.provider_attempts, 2);
                } else {
                    let error = result.unwrap_err();
                    assert_eq!(error.provider_code.as_deref(), Some("empty_final_response"));
                    assert_eq!(error.provider_attempts, 2);
                    assert!(!error.retryable);
                }
                server.await.unwrap();
            }
        }
    }

    #[tokio::test]
    async fn api_output_limit_continues_through_the_real_core_loop_and_pairs_rejected_call() {
        use centaeris_core::execution::*;
        use centaeris_core::runtime::ToolConcurrencyCoordinator;
        use centaeris_core::runtime::{
            AgentRunInitialInput, AgentRunRequest, AgentRunStop, AgentRuntime, AgentRuntimeConfig,
            TurnUpdate,
        };
        use centaeris_core::tool::layer::ToolLayer;
        use centaeris_runtime_sqlite::SqliteRuntimeStore;
        struct RecoveryHost;
        impl ExecutionHostRunner for RecoveryHost {
            fn status(&self, _: &ExecutionPolicy) -> Result<ExecutionHostStatus, ExecutionError> {
                Ok(ExecutionHostStatus::transient_ready(false))
            }
            fn run_file_system_operation(
                &self,
                _: ExecutionFileSystemRequest,
            ) -> Result<ExecutionFileSystemOutput, ExecutionFileSystemError> {
                Err(ExecutionFileSystemError::new(
                    ExecutionFileSystemErrorKind::NotFound,
                    "no instruction file in recovery fixture",
                ))
            }
            fn run_host_command(
                &self,
                _: Option<&str>,
                _: ExecutionCommandRequest,
                _: Option<&ExecutionCancellationProbe>,
            ) -> Result<ExecutionHostCommandOutput, ExecutionError> {
                panic!("a truncated tool call must never execute")
            }
        }
        for incomplete_identity in [false, true] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let mut requests = vec![];
                for attempt in 0..2 {
                    let (mut stream, _) = listener.accept().await.unwrap();
                    requests.push(read_http_request(&mut stream).await);
                    let body = if attempt == 0 {
                        let failure = serde_json::json!({"schema":"api.model.stream.v1", "type":"error",
                            "reasonType":"incomplete_output_token_limit", "httpStatus":null,
                            "truncatedToolCalls":[{"callId":if incomplete_identity {None} else {Some("original-call")},
                                "toolName":"bash", "argsBytes":23, "argsSha256":format!("sha256:{}", "a".repeat(64))}]});
                        format!("data: {{\"schema\":\"api.model.stream.v1\",\"type\":\"delta\",\"delta\":\"partial answer\"}}\n\ndata: {failure}\n\n")
                    } else {
                        "data: {\"schema\":\"api.model.stream.v1\",\"type\":\"delta\",\"delta\":\" done\"}\n\ndata: {\"schema\":\"api.model.stream.v1\",\"type\":\"result\",\"text\":\" done\",\"toolCalls\":[],\"usage\":null}\n\n".to_string()
                    };
                    stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
                }
                requests
            });
            let root = tempfile::tempdir().unwrap();
            let store = SqliteRuntimeStore::new(root.path().join("runtime.sqlite")).unwrap();
            let jobs = std::sync::Arc::new(store.job_store(129).unwrap());
            let binding = std::sync::Arc::new(
                ExecutionHostBinding::new(
                    ExecutionHostMode::Remote,
                    std::sync::Arc::new(RecoveryHost),
                    root.path().to_path_buf(),
                    ExecutionPolicy::workspace_write_no_network(root.path()),
                )
                .unwrap(),
            );
            let tools = ToolLayer::try_new_with_skill_catalog_config_and_execution_host_binding(
                centaeris_core::extension::skills::SkillCatalogLoadConfig::default(),
                binding,
            )
            .unwrap();
            let engine = AgentRuntime::new(
                jobs,
                store,
                tools,
                AgentRuntimeConfig {
                    enable_prompt_compaction: false,
                    model_max_output_tokens: 1_024,
                    ..Default::default()
                },
                ToolConcurrencyCoordinator::new(1),
            );
            let client = model_client(format!("http://{address}"), None, 1_024);
            let mut events = vec![];
            let result = engine
                .process_turn_loop_online_with_model_client_stream_cancellable_and_tool_safe_point_async(
                    AgentRunRequest {
                        session_id: "hosted-output-recovery".into(),
                        initial_turn_id: "original-turn".into(),
                        initial_input: AgentRunInitialInput::UserMessage("finish the task".into()),
                        agent_run_identity: None,
                        runtime_scope: centaeris_core::model::prompt::PromptCompactionScopeV1::main(
                        ),
                        resume_from_turn_id: None,
                        auto_continue_after_resume_wait: None,
                    },
                    &client,
                    &centaeris_core::model::EmptyModelSessionConfigStore::new(),
                    &mut |event| events.push(event),
                    &|| Ok(None),
                    &mut |_| Ok(()),
                )
                .await
                .unwrap();
            assert_eq!(result.stop, AgentRunStop::Finalized);
            assert_eq!(
                result
                    .last_turn
                    .as_ref()
                    .unwrap()
                    .session_snapshot
                    .messages
                    .last()
                    .unwrap()
                    .content,
                "partial answer done"
            );
            assert!(result
                .turn_receipts
                .iter()
                .all(|turn| turn.tool_result_ids.is_empty()));
            assert!(events
                .iter()
                .all(|event| !matches!(event, TurnUpdate::ToolCallReady { .. })));
            let requests = server.await.unwrap();
            assert_eq!(requests.len(), 2);
            assert_eq!(requests[0]["turnId"], serde_json::Value::Null,
                "the hosted API only receives the canonical prepared prompt and existing Run binding");
            let turn_ids: Vec<_> = events
                .iter()
                .filter_map(|event| match event {
                    TurnUpdate::ModelRequestStart { turn_id, .. } => Some(turn_id),
                    _ => None,
                })
                .collect();
            assert_eq!(turn_ids.len(), 2);
            assert_ne!(turn_ids[0], turn_ids[1]);
            let messages = requests[1]["preparedPrompt"]["messages"]
                .as_array()
                .unwrap();
            let calls: Vec<_> = messages
                .iter()
                .filter(|message| {
                    message
                        .get("toolCalls")
                        .and_then(|calls| calls.as_array())
                        .is_some_and(|calls| !calls.is_empty())
                })
                .collect();
            if incomplete_identity {
                assert!(calls.is_empty());
                assert!(messages.last().unwrap()["content"]
                    .as_str()
                    .unwrap()
                    .contains("fresh valid identity"));
            } else {
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0]["toolCalls"][0]["id"], "original-call");
                assert_eq!(calls[0]["toolCalls"][0]["argsJson"], "{}");
                let paired: Vec<_> = messages
                    .iter()
                    .filter(|message| message["toolCallId"] == "original-call")
                    .collect();
                assert_eq!(paired.len(), 1);
                assert!(paired[0]["content"]
                    .as_str()
                    .unwrap()
                    .contains("was not executed"));
            }
        }
    }

    #[test]
    fn api_output_limit_rejects_malformed_recovery_metadata_and_wrong_failure_kind() {
        let valid = serde_json::json!({"schema":"api.model.stream.v1", "type":"error",
            "reasonType":"incomplete_output_token_limit", "httpStatus":null,
            "truncatedToolCalls":[{"callId":"call", "toolName":"bash", "argsBytes":5,
                "argsSha256":format!("sha256:{}", "a".repeat(64))}]});
        for change in [
            "digest",
            "name",
            "wrongReason",
            "missingMetadata",
            "secretBody",
            "nullMetadata",
            "missingStatus",
            "missingCallId",
            "missingToolName",
        ] {
            let mut value = valid.clone();
            match change {
                "digest" => value["truncatedToolCalls"][0]["argsSha256"] = "not-a-digest".into(),
                "name" => value["truncatedToolCalls"][0]["toolName"] = "badTool".into(),
                "wrongReason" => value["reasonType"] = "empty_final_response".into(),
                "missingMetadata" => {
                    value.as_object_mut().unwrap().remove("truncatedToolCalls");
                }
                "secretBody" => {
                    value["truncatedToolCalls"][0]["argsJson"] = "private arguments".into()
                }
                "nullMetadata" => value["truncatedToolCalls"] = serde_json::Value::Null,
                "missingStatus" => {
                    value.as_object_mut().unwrap().remove("httpStatus");
                }
                "missingCallId" => {
                    value["truncatedToolCalls"][0]
                        .as_object_mut()
                        .unwrap()
                        .remove("callId");
                }
                "missingToolName" => {
                    value["truncatedToolCalls"][0]
                        .as_object_mut()
                        .unwrap()
                        .remove("toolName");
                }
                _ => unreachable!(),
            }
            assert!(
                match serde_json::from_value::<ModelStreamFailure>(value) {
                    Ok(failure) => hosted_model_error(failure).is_err(),
                    Err(_) => true,
                },
                "{change}"
            );
        }
    }

    async fn read_http_request(stream: &mut tokio::net::TcpStream) -> serde_json::Value {
        let mut request = Vec::new();
        let mut buffer = [0_u8; 4_096];
        loop {
            let read = stream.read(&mut buffer).await.expect("read request");
            if read == 0 {
                return serde_json::Value::Null;
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
                return serde_json::from_slice(
                    &request[header_end + 4..header_end + 4 + content_length],
                )
                .unwrap();
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

    #[tokio::test(start_paused = true)]
    async fn hosted_retry_after_is_carried_as_one_deadline() {
        let failure: ModelStreamFailure = serde_json::from_value(serde_json::json!({
            "schema": "api.model.stream.v1", "type": "error", "reasonType": "provider_rate_limited",
            "httpStatus": 429, "retryAfter": "5"
        }))
        .unwrap();
        let error = hosted_model_error(failure).unwrap();
        assert!(error.retryable);
        let advice = error.retry_after.expect("upstream advice retained");
        tokio::time::advance(std::time::Duration::from_secs(3)).await;
        let started = tokio::time::Instant::now();
        sleep_before_retry(1, 600, Some(advice)).await;
        assert_eq!(started.elapsed(), std::time::Duration::from_secs(2));
    }

    #[tokio::test]
    async fn hosted_stream_header_timeout_closes_the_pending_request() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = [0; 4096];
            while socket.read(&mut bytes).await.unwrap() != 0 {}
            let _ = socket.shutdown().await;
        });
        let client = model_client(format!("http://{address}"), None, 1024);
        let mut request = model_request();
        request.session_config.timeout_ms = 250;
        request.session_config.max_retries = 0;
        let error = client
            .generate_stream(&request, &mut |_| {})
            .await
            .unwrap_err();
        assert_eq!(error.kind, ModelClientErrorKind::Timeout);
        assert_eq!(error.provider_attempts, 1);
        tokio::time::timeout(std::time::Duration::from_secs(2), server)
            .await
            .unwrap()
            .unwrap();
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
            let body = "data: {\"schema\":\"api.model.stream.v1\",\"type\":\"error\",\"reasonType\":\"provider_request_rejected\",\"httpStatus\":null}\n\n";
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
            Some("provider_request_rejected")
        );
        assert_eq!(error.provider_attempts, 1);
        assert!(!error.retryable);
        assert!(events.is_empty());
        let detail: serde_json::Value =
            serde_json::from_str(error.message.strip_prefix("hosted_model_error: ").unwrap())
                .expect("structured terminal model failure");
        assert_eq!(detail["providerAttempts"], 1);
        assert_eq!(detail["retryExhausted"], false);
    }

    #[test]
    fn hosted_model_errors_preserve_failure_facts_and_reject_malformed_frames() {
        for (reason, status, kind, retryable) in [
            (
                "model_quota_domain_disabled",
                None,
                ModelClientErrorKind::Provider,
                false,
            ),
            (
                "prepared_prompt_invalid",
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
            (
                "provider_response_invalid",
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
            assert_eq!(
                hosted_model_failure(&error.message).map(|failure| failure.reason_type),
                Some(reason.to_string())
            );
        }
        for (field, value) in [
            ("reason_type", serde_json::json!("wrong")),
            ("retryable", serde_json::json!(true)),
        ] {
            let mut failure = serde_json::json!({"schema":"api.model.stream.v1","type":"error","reasonType":"model_adapter_failed","httpStatus":null});
            failure[field] = value;
            assert!(serde_json::from_value::<ModelStreamFailure>(failure).is_err());
        }
        assert!(hosted_model_failure("hosted_model_error: secret response with spaces").is_none());
    }

    #[test]
    fn exhausted_transport_failure_retains_terminal_identity_without_private_text() {
        let mut error = ModelClientError::new(
            ModelClientErrorKind::ProviderResponseInterrupted,
            "private provider body",
            true,
        )
        .with_provider_attempts(2);
        finish_hosted_failure(&mut error, true);
        let detail = hosted_model_failure(&error.message)
            .expect("transport failure must retain attempts and its terminal classification");
        assert_eq!(detail.reason_type, "provider_stream_interrupted");
        assert_eq!(detail.provider_attempts, 2);
        assert!(detail.retry_exhausted);
        assert_eq!(detail.http_status, None);
        assert!(!error.message.contains("private provider body"));
    }

    #[test]
    fn terminal_failure_details_cross_core_conversion_and_reject_corruption() {
        let mut error = hosted_model_error(serde_json::from_value(serde_json::json!({
            "schema":"api.model.stream.v1", "type":"error", "reasonType":"provider_rate_limited", "httpStatus":429,
        })).unwrap()).unwrap();
        error.provider_attempts = 3;
        finish_hosted_failure(&mut error, true);
        error.retryable = false;
        let wrapped: String =
            centaeris_core::runtime::GenerateDriverError::from_model_client(error, String::new())
                .into();
        let failure = hosted_model_failure(&wrapped).expect("Core must preserve the host envelope");
        assert_eq!(failure.http_status, Some(429));
        assert_eq!(failure.provider_attempts, 3);
        assert!(failure.retry_exhausted);
        for (key, value) in [
            ("schema", serde_json::json!("unknown")),
            ("httpStatus", serde_json::json!(200)),
            ("providerAttempts", serde_json::json!(0)),
            ("retryExhausted", serde_json::json!("true")),
            ("unknown", serde_json::json!(true)),
        ] {
            let mut malformed = serde_json::to_value(&failure).unwrap();
            malformed[key] = value;
            assert!(hosted_model_failure(&format!("hosted_model_error: {malformed}")).is_none());
        }
        let mut missing = serde_json::to_value(&failure).unwrap();
        missing.as_object_mut().unwrap().remove("httpStatus");
        assert!(hosted_model_failure(&format!("hosted_model_error: {missing}")).is_none());
        assert!(hosted_model_failure(&format!("{} trailing data", failure.encoded())).is_none());
        assert!(
            hosted_model_failure(&format!("private provider body: {}", failure.encoded()))
                .is_none()
        );
    }

    #[tokio::test]
    async fn api_model_rate_limit_records_actual_attempts_and_exhaustion() {
        for (reason, status, expected_attempts) in [
            ("provider_rate_limited", 429, 2),
            ("provider_authentication_failed", 401, 1),
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                for _ in 0..expected_attempts {
                    let (mut stream, _) =
                        tokio::time::timeout(std::time::Duration::from_secs(5), listener.accept())
                            .await
                            .unwrap()
                            .unwrap();
                    read_http_request(&mut stream).await;
                    let frame = serde_json::json!({"schema":"api.model.stream.v1","type":"error","reasonType":reason,"httpStatus":status});
                    let body = format!("data: {frame}\n\n");
                    stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
                }
            });
            let client = model_client(format!("http://{address}"), None, 1_024);
            let mut request = model_request();
            request.session_config.max_retries = 1;
            request.session_config.retry_backoff_ms = 0;
            let mut events = Vec::new();
            let error = client
                .generate_stream(&request, &mut |event| events.push(event))
                .await
                .unwrap_err();
            let failure = hosted_model_failure(&error.message).unwrap();
            assert_eq!(failure.http_status, Some(status));
            assert_eq!(failure.provider_attempts, expected_attempts);
            assert_eq!(failure.retry_exhausted, status == 429);
            assert!(!error.retryable);
            assert_eq!(
                events
                    .iter()
                    .filter(|event| matches!(
                        event,
                        ModelClientStreamEvent::Status {
                            process_state: RuntimeProcessState::Retrying,
                            ..
                        }
                    ))
                    .count(),
                (expected_attempts - 1) as usize
            );
            server.await.unwrap();
        }
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
    #[tokio::test]
    async fn api_model_stream_recovers_typed_failure_after_reasoning_and_bounds_retries() {
        for recover in [true, false] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                for attempt in 0..2 {
                    let (mut stream, _) =
                        tokio::time::timeout(std::time::Duration::from_secs(5), listener.accept())
                            .await
                            .expect("expected model attempt")
                            .unwrap();
                    read_http_request(&mut stream).await;
                    let body = if recover && attempt == 1 {
                        "data: {\"schema\":\"api.model.stream.v1\",\"type\":\"delta\",\"delta\":\"final answer\"}\n\ndata: {\"schema\":\"api.model.stream.v1\",\"type\":\"result\",\"text\":\"final answer\",\"toolCalls\":[],\"usage\":null}\n\n"
                    } else {
                        "data: {\"schema\":\"api.model.stream.v1\",\"type\":\"reasoning\",\"text\":\"preparing final answer\"}\n\ndata: {\"schema\":\"api.model.stream.v1\",\"type\":\"error\",\"reasonType\":\"provider_response_invalid\",\"httpStatus\":null}\n\n"
                    };
                    stream.write_all(format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body
                    ).as_bytes()).await.unwrap();
                }
            });
            let client = model_client(format!("http://{address}"), None, 1_024);
            let mut request = model_request();
            request.session_config.max_retries = 1;
            request.session_config.retry_backoff_ms = 0;
            let mut events = Vec::new();
            let result = client
                .generate_stream(&request, &mut |event| events.push(event))
                .await;
            if recover {
                let response = result.expect("typed provider failure should recover");
                assert_eq!(response.generate_result.content, "final answer");
                assert_eq!(response.provider_attempts, 2);
                assert!(response.generate_result.tool_calls.is_empty());
            } else {
                let error =
                    result.expect_err("persistent invalid responses must stop at the limit");
                assert_eq!(error.provider_attempts, 2);
                assert!(!error.retryable);
                assert_eq!(
                    error.provider_code.as_deref(),
                    Some("provider_response_invalid")
                );
            }
            assert!(events.iter().any(
                |e| matches!(e, ModelClientStreamEvent::Reasoning { text } if text.is_empty())
            ));
            assert_eq!(
                events
                    .iter()
                    .filter(|e| matches!(
                        e,
                        ModelClientStreamEvent::Status {
                            process_state: RuntimeProcessState::Retrying,
                            ..
                        }
                    ))
                    .count(),
                1
            );
            server.await.unwrap();
        }
    }
}
