use super::*;

pub struct OpenAiCompatibleModelClient<T: JsonHttpTransport> {
    registry: ModelProviderRegistry,
    pub(super) transport: T,
}

impl<T: JsonHttpTransport> OpenAiCompatibleModelClient<T> {
    pub fn new(registry: ModelProviderRegistry, transport: T) -> Self {
        Self {
            registry,
            transport,
        }
    }

    pub(super) fn build_http_request(
        &self,
        request: &ModelClientRequest,
        stream: bool,
    ) -> Result<JsonHttpRequest, ModelClientError> {
        let resolved = self
            .registry
            .resolve_session_config(&request.session_config)
            .map_err(|message| {
                ModelClientError::new(ModelClientErrorKind::InvalidRequest, message, false)
            })?;
        validate_provider_image_capability(&resolved.provider.info, request)?;
        if resolved.provider.info.wire_api != WireApi::OpenAiChatCompletions {
            return Err(ModelClientError::new(
                ModelClientErrorKind::InvalidRequest,
                format!(
                    "openai-compatible adapter does not support wire_api={:?}",
                    resolved.provider.info.wire_api
                ),
                false,
            ));
        }
        let base_url = resolved.effective_api_base.ok_or_else(|| {
            ModelClientError::new(
                ModelClientErrorKind::InvalidRequest,
                "provider api_base is required for openai-compatible adapter",
                false,
            )
        })?;
        let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
        request
            .prepared_prompt
            .validate()
            .map_err(invalid_openai_compatible_request)?;
        let mut headers = resolved.provider.headers.clone();
        headers.extend(
            resolved
                .provider
                .info
                .resolve_auth_headers()
                .map_err(|message| {
                    ModelClientError::new(ModelClientErrorKind::AuthFailed, message, false)
                })?,
        );
        headers.insert("content-type".to_string(), "application/json".to_string());
        let (thinking, reasoning_effort) = build_openai_compatible_thinking(
            &resolved.provider.info.provider_kind,
            resolved.session_config.model.as_str(),
            resolved.session_config.thinking_mode.as_deref(),
        )?;
        let (tools, tool_choice) = build_openai_compatible_tool_projection(
            &resolved.provider.info.provider_kind,
            resolved.session_config.model.as_str(),
            request,
        )?;
        let max_output_tokens =
            prepared_prompt_max_output_tokens(request, &resolved.session_config)?;
        let (max_tokens, max_completion_tokens) =
            if resolved.provider.info.provider_kind == ModelProviderKind::Kimi {
                (None, max_output_tokens)
            } else {
                (max_output_tokens, None)
            };
        let payload = OpenAiCompatibleChatCompletionsRequest {
            model: resolved.session_config.model.clone(),
            messages: build_openai_compatible_messages(request)?,
            stream,
            stream_options: stream.then_some(OpenAiCompatibleStreamOptions {
                include_usage: true,
            }),
            max_tokens,
            max_completion_tokens,
            tools,
            tool_choice,
            thinking,
            reasoning_effort,
        };
        let body_json = serde_json::to_string(&payload).map_err(|err| {
            ModelClientError::new(
                ModelClientErrorKind::InvalidRequest,
                format!("serialize openai-compatible request failed: {err}"),
                false,
            )
        })?;
        Ok(JsonHttpRequest {
            method: "POST".to_string(),
            url,
            headers,
            timeout_ms: resolved.effective_timeout_ms,
            sse_idle_timeout_ms: DEFAULT_MODEL_SSE_IDLE_TIMEOUT_MS,
            max_retries: resolved.effective_max_retries,
            retry_backoff_ms: resolved.effective_retry_backoff_ms,
            body_json,
        })
    }

    fn parse_http_response(
        &self,
        response: JsonHttpResponse,
    ) -> Result<ModelClientResponse, ModelClientError> {
        if response.status_code >= 400 {
            return Err(map_openai_compatible_http_error(
                response.status_code,
                response.body_json.as_str(),
            ));
        }
        let parsed: OpenAiCompatibleChatCompletionsResponse =
            serde_json::from_str(response.body_json.as_str()).map_err(|err| {
                ModelClientError::new(
                    ModelClientErrorKind::Provider,
                    format!("parse openai-compatible response failed: {err}"),
                    false,
                )
            })?;
        let choice = parsed.choices.into_iter().next().ok_or_else(|| {
            ModelClientError::new(
                ModelClientErrorKind::Provider,
                "openai-compatible response contained no choices",
                false,
            )
        })?;
        let finish_reason = choice.finish_reason;
        let raw_tool_calls = choice.message.tool_calls.clone();
        if matches!(finish_reason.as_deref(), Some("length" | "max_tokens")) {
            return Err(
                reject_openai_compatible_finish_reason(finish_reason.as_deref())
                    .expect_err("output limit finish reason must be rejected")
                    .with_truncated_tool_calls(truncated_openai_compatible_tool_calls(
                        raw_tool_calls,
                    )),
            );
        }
        reject_openai_compatible_finish_reason(finish_reason.as_deref())?;
        let tool_calls = extract_openai_compatible_tool_calls(raw_tool_calls)?;
        let provider_request_id = parsed
            .id
            .or_else(|| response.headers.get("x-request-id").cloned());
        let usage = parsed.usage.as_ref();
        let reasoning_content = normalize_openai_compatible_reasoning(
            choice.message.reasoning,
            choice.message.reasoning_content,
        )?;
        validate_provider_tool_call_arguments("openai-compatible", &tool_calls)?;
        let content = extract_openai_compatible_text(choice.message.content);
        reject_openai_compatible_empty_final_response(
            content.as_str(),
            !tool_calls.is_empty(),
            reasoning_content.is_some(),
            finish_reason.as_deref(),
        )?;
        Ok(ModelClientResponse {
            generate_result: GenerateResult {
                content,
                tool_calls,
                reasoning_content,
                input_tokens: usage.and_then(openai_compatible_input_tokens),
                total_tokens: usage.and_then(|usage| usage.total_tokens),
                prompt_cache_hit_tokens: usage.and_then(openai_compatible_cached_tokens),
                prompt_cache_miss_tokens: usage.and_then(openai_compatible_cache_miss_tokens),
            },
            provider_request_id,
            provider_latency_ms: None,
            provider_attempts: 1,
        })
    }
}

impl<T: JsonHttpTransport> ModelClient for OpenAiCompatibleModelClient<T> {
    fn generate<'a>(
        &'a self,
        request: &'a ModelClientRequest,
    ) -> ModelClientFuture<'a, ModelClientResponse> {
        Box::pin(async move {
            let http_request = self.build_http_request(request, false)?;
            execute_json_model_response_with_retries(&self.transport, &http_request, |response| {
                self.parse_http_response(response)
            })
            .await
        })
    }

    fn generate_stream<'a>(
        &'a self,
        request: &'a ModelClientRequest,
        sink: &'a mut (dyn FnMut(ModelClientStreamEvent) + Send),
    ) -> ModelClientFuture<'a, ModelClientResponse> {
        Box::pin(async move {
            let http_request = self.build_http_request(request, true)?;
            let mut stream_state = OpenAiCompatibleStreamState::default();
            let mut pending_completion_events = Vec::<ModelClientStreamEvent>::new();
            let mut attempt_has_visible_content = false;
            let attempted =
                execute_sse_with_retries(&self.transport, &http_request, &mut |event| {
                    let chunk = match event {
                        SseAttemptEvent::Start { attempt } => {
                            stream_state = OpenAiCompatibleStreamState::default();
                            pending_completion_events.clear();
                            if attempt > 0 {
                                if attempt_has_visible_content {
                                    sink(ModelClientStreamEvent::ReplaceContent {
                                        content: String::new(),
                                    });
                                }
                                sink(ModelClientStreamEvent::Status {
                                    message: None,
                                    process_state: RuntimeProcessState::Retrying,
                                });
                            }
                            attempt_has_visible_content = false;
                            return SseAttemptProgress::default();
                        }
                        SseAttemptEvent::Data { attempt, frame } => (attempt, frame),
                    };
                    let (attempt, chunk) = chunk;
                    let mut progress = SseAttemptProgress::default();
                    let updates = match stream_state.consume_chunk(chunk.as_str(), attempt) {
                        Ok(updates) => updates,
                        Err(error) => {
                            return SseAttemptProgress {
                                terminal: true,
                                terminal_error: Some(error),
                            };
                        }
                    };
                    for update in updates {
                        match update {
                            OpenAiCompatibleStreamUpdate::Status {
                                message,
                                process_state,
                            } => sink(ModelClientStreamEvent::Status {
                                message,
                                process_state,
                            }),
                            OpenAiCompatibleStreamUpdate::Token { content } => {
                                attempt_has_visible_content = true;
                                sink(ModelClientStreamEvent::Token { content });
                            }
                            OpenAiCompatibleStreamUpdate::ToolCallPreparing { name } => {
                                sink(ModelClientStreamEvent::ToolCallPreparing { name });
                            }
                            OpenAiCompatibleStreamUpdate::ToolCallReady {
                                call_id,
                                provider_item_id,
                                name,
                                args_json,
                                args_preview,
                            } => pending_completion_events.push(
                                ModelClientStreamEvent::ToolCallReady {
                                    call_id,
                                    provider_item_id,
                                    name,
                                    args_json,
                                    args_preview,
                                },
                            ),
                            OpenAiCompatibleStreamUpdate::Done { finish_reason } => {
                                progress.terminal = true;
                                progress.terminal_error = reject_openai_compatible_finish_reason(
                                    finish_reason.as_deref(),
                                )
                                .err()
                                .map(|error| {
                                    if error.provider_code.as_deref()
                                        == Some("incomplete_output_token_limit")
                                    {
                                        error.with_truncated_tool_calls(
                                            stream_state.truncated_tool_calls(),
                                        )
                                    } else {
                                        error
                                    }
                                })
                                .or_else(|| stream_state.validate_terminal_tool_calls().err())
                                .or_else(|| {
                                    stream_state
                                        .validate_terminal_response(finish_reason.as_deref())
                                        .err()
                                });
                                pending_completion_events
                                    .push(ModelClientStreamEvent::Done { finish_reason });
                            }
                        }
                    }
                    progress
                })
                .await
                .map_err(map_attempted_transport_error)?;
            let response = attempted.response;
            if response.status_code >= 400 {
                return Err(map_openai_compatible_http_error(
                    response.status_code,
                    response.body_json.as_str(),
                )
                .with_provider_attempts(attempted.attempts));
            }
            let parsed = stream_state.into_response();
            let mut parsed_response = self
                .parse_http_response(JsonHttpResponse {
                    status_code: response.status_code,
                    headers: response.headers,
                    body_json: serde_json::to_string(&parsed).map_err(|err| {
                        ModelClientError::new(
                            ModelClientErrorKind::Provider,
                            format!("serialize openai-compatible stream completion failed: {err}"),
                            false,
                        )
                    })?,
                })
                .map_err(|error| error.with_provider_attempts(attempted.attempts))?;
            parsed_response.provider_attempts = attempted.attempts;
            for event in pending_completion_events {
                sink(event);
            }
            Ok(parsed_response)
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct OpenAiCompatibleChatMessage {
    pub(super) role: String,
    pub(super) content: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) tool_calls: Option<Vec<OpenAiCompatibleRequestToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) reasoning_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) tool_call_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct OpenAiCompatibleRequestToolCall {
    pub(super) id: String,
    #[serde(rename = "type")]
    pub(super) tool_type: String,
    pub(super) function: OpenAiCompatibleToolFunction,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(super) struct OpenAiCompatibleChatCompletionsRequest {
    model: String,
    messages: Vec<OpenAiCompatibleChatMessage>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<OpenAiCompatibleStreamOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_completion_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    tools: Vec<OpenAiChatToolDefinition>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<OpenAiChatToolChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<OpenAiCompatibleThinkingRequest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct OpenAiCompatibleStreamOptions {
    include_usage: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct OpenAiCompatibleThinkingRequest {
    #[serde(rename = "type")]
    thinking_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    keep: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct OpenAiCompatibleChatCompletionsResponse {
    id: Option<String>,
    choices: Vec<OpenAiCompatibleChoice>,
    usage: Option<OpenAiCompatibleUsage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct OpenAiCompatibleChoice {
    message: OpenAiCompatibleAssistantMessage,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct OpenAiCompatibleAssistantMessage {
    content: Option<Value>,
    tool_calls: Option<Vec<OpenAiCompatibleToolCall>>,
    reasoning: Option<String>,
    reasoning_content: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct OpenAiCompatibleToolCall {
    id: Option<String>,
    function: OpenAiCompatibleToolFunction,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct OpenAiCompatibleToolFunction {
    pub(super) name: String,
    pub(super) arguments: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(super) struct OpenAiChatToolDefinition {
    #[serde(rename = "type")]
    tool_type: String,
    function: OpenAiChatToolFunctionDefinition,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(super) struct OpenAiChatToolFunctionDefinition {
    name: String,
    description: String,
    parameters: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub(super) enum OpenAiChatToolChoice {
    Mode(String),
    Function {
        #[serde(rename = "type")]
        tool_type: String,
        function: OpenAiChatToolChoiceFunction,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct OpenAiChatToolChoiceFunction {
    name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct OpenAiCompatibleUsage {
    prompt_tokens: Option<i64>,
    total_tokens: Option<i64>,
    prompt_tokens_details: Option<OpenAiTokenDetails>,
    prompt_cache_hit_tokens: Option<i64>,
    prompt_cache_miss_tokens: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct OpenAiCompatibleErrorEnvelope {
    error: Option<OpenAiCompatibleErrorObject>,
}

pub(super) fn openai_compatible_input_tokens(usage: &OpenAiCompatibleUsage) -> Option<i64> {
    usage.prompt_tokens.or_else(|| {
        match (
            usage.prompt_cache_hit_tokens,
            usage.prompt_cache_miss_tokens,
        ) {
            (Some(hit), Some(miss)) => Some(hit + miss),
            _ => None,
        }
    })
}

pub(super) fn openai_compatible_cached_tokens(usage: &OpenAiCompatibleUsage) -> Option<i64> {
    usage
        .prompt_cache_hit_tokens
        .or_else(|| usage.prompt_tokens_details.as_ref()?.cached_tokens)
}

pub(super) fn openai_compatible_cache_miss_tokens(usage: &OpenAiCompatibleUsage) -> Option<i64> {
    usage.prompt_cache_miss_tokens.or_else(|| {
        let input_tokens = openai_compatible_input_tokens(usage)?;
        let cached_tokens = openai_compatible_cached_tokens(usage)?;
        Some(input_tokens.saturating_sub(cached_tokens))
    })
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct OpenAiCompatibleErrorObject {
    message: Option<String>,
    code: Option<String>,
    #[serde(rename = "type")]
    error_type: Option<String>,
}

#[derive(Debug, Default)]
pub(super) struct OpenAiCompatibleStreamState {
    provider_request_id: Option<String>,
    content: String,
    reasoning_content: Option<String>,
    reasoning_field: Option<&'static str>,
    thinking_status_emitted: bool,
    usage: Option<OpenAiCompatibleUsage>,
    tool_calls_by_index: HashMap<usize, OpenAiCompatibleStreamToolCallState>,
    completed_tool_calls: Vec<ToolCallEnvelope>,
    finish_reason: Option<String>,
}

#[derive(Debug, Default)]
pub(super) struct OpenAiCompatibleStreamToolCallState {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
    preparing_emitted: bool,
}

impl OpenAiCompatibleStreamState {
    fn truncated_tool_calls(&self) -> Vec<TruncatedToolCall> {
        let mut calls = self.tool_calls_by_index.iter().collect::<Vec<_>>();
        calls.sort_unstable_by_key(|(index, _)| **index);
        calls
            .into_iter()
            .map(|(_, call)| {
                truncated_tool_call(
                    call.id.as_deref(),
                    call.name.as_deref(),
                    call.arguments.as_str(),
                )
            })
            .collect()
    }
}

pub(super) enum OpenAiCompatibleStreamUpdate {
    Status {
        message: Option<String>,
        process_state: RuntimeProcessState,
    },
    Token {
        content: String,
    },
    ToolCallPreparing {
        name: String,
    },
    ToolCallReady {
        call_id: String,
        provider_item_id: Option<String>,
        name: String,
        args_json: String,
        args_preview: String,
    },
    Done {
        finish_reason: Option<String>,
    },
}

pub(super) fn build_openai_compatible_messages(
    request: &ModelClientRequest,
) -> Result<Vec<OpenAiCompatibleChatMessage>, ModelClientError> {
    let mut system_parts = request
        .prepared_prompt
        .system_prompt
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| vec![value.to_string()])
        .unwrap_or_default();
    let mut messages = vec![];
    let mut pending_tool_call_ids = Vec::<String>::new();
    for item in &request.prepared_prompt.messages {
        if !pending_tool_call_ids.is_empty() && !matches!(item.role, ModelMessageRoleV1::Tool) {
            return Err(invalid_openai_compatible_request(format!(
                "assistant tool call {} must be followed by tool result before message {}",
                pending_tool_call_ids[0], item.message_id
            )));
        }
        match item.role {
            ModelMessageRoleV1::System => {
                if !messages.is_empty() {
                    return Err(invalid_openai_compatible_request(format!(
                        "openai-compatible system message {} appears after conversation content",
                        item.message_id
                    )));
                }
                let trimmed = item.content.trim();
                if !trimmed.is_empty() {
                    system_parts.push(trimmed.to_string());
                }
            }
            ModelMessageRoleV1::User => {
                let parts = provider_input_parts(request, item)?;
                if !parts.is_empty() {
                    messages.push(OpenAiCompatibleChatMessage {
                        role: "user".to_string(),
                        content: if parts.len() == 1 {
                            match &parts[0] {
                                ProviderInputPart::Text(text) => Value::String(text.clone()),
                                ProviderInputPart::Image { .. } => openai_chat_parts(&parts),
                            }
                        } else {
                            openai_chat_parts(&parts)
                        },
                        tool_calls: None,
                        reasoning_content: None,
                        tool_call_id: None,
                    });
                }
            }
            ModelMessageRoleV1::Assistant => {
                let message = build_openai_compatible_assistant_message(item);
                if let Some(tool_calls) = message.tool_calls.as_ref() {
                    pending_tool_call_ids.extend(tool_calls.iter().map(|call| call.id.clone()));
                }
                if message
                    .content
                    .as_str()
                    .is_some_and(|content| !content.trim().is_empty())
                    || message.tool_calls.is_some()
                    || message.reasoning_content.is_some()
                {
                    messages.push(message);
                }
            }
            ModelMessageRoleV1::Tool => {
                let tool_call_id = item.tool_call_id.as_deref().ok_or_else(|| {
                    invalid_openai_compatible_request(format!(
                        "tool message {} is missing toolCallId",
                        item.message_id
                    ))
                })?;
                let expected_call_id = pending_tool_call_ids.first().ok_or_else(|| {
                    invalid_openai_compatible_request(format!(
                        "tool message {} has no preceding assistant tool call",
                        item.message_id
                    ))
                })?;
                if expected_call_id != tool_call_id {
                    return Err(invalid_openai_compatible_request(format!(
                        "tool message {} has toolCallId={} but expected {}",
                        item.message_id, tool_call_id, expected_call_id
                    )));
                }
                pending_tool_call_ids.remove(0);
                messages.push(OpenAiCompatibleChatMessage {
                    role: "tool".to_string(),
                    content: Value::String(item.content.clone()),
                    tool_calls: None,
                    reasoning_content: None,
                    tool_call_id: Some(tool_call_id.to_string()),
                });
            }
        }
    }
    if let Some(call_id) = pending_tool_call_ids.first() {
        return Err(invalid_openai_compatible_request(format!(
            "assistant tool call {call_id} has no following tool result"
        )));
    }
    if !system_parts.is_empty() {
        messages.insert(
            0,
            openai_compatible_text_message("system", system_parts.join("\n\n").as_str()),
        );
    }
    Ok(messages)
}

pub(super) fn openai_compatible_text_message(
    role: &str,
    content: &str,
) -> OpenAiCompatibleChatMessage {
    OpenAiCompatibleChatMessage {
        role: role.to_string(),
        content: Value::String(content.to_string()),
        tool_calls: None,
        reasoning_content: None,
        tool_call_id: None,
    }
}

pub(super) fn build_openai_compatible_assistant_message(
    message: &ModelMessageV1,
) -> OpenAiCompatibleChatMessage {
    OpenAiCompatibleChatMessage {
        role: "assistant".to_string(),
        content: Value::String(message.content.clone()),
        tool_calls: Some(
            message
                .tool_calls
                .iter()
                .cloned()
                .map(|call| OpenAiCompatibleRequestToolCall {
                    id: call.id,
                    tool_type: "function".to_string(),
                    function: OpenAiCompatibleToolFunction {
                        name: call.name,
                        arguments: call.args_json,
                    },
                })
                .collect(),
        )
        .filter(|items: &Vec<OpenAiCompatibleRequestToolCall>| !items.is_empty()),
        reasoning_content: message.reasoning_content.clone(),
        tool_call_id: None,
    }
}

fn openai_chat_parts(parts: &[ProviderInputPart]) -> Value {
    Value::Array(
        parts
            .iter()
            .map(|part| match part {
                ProviderInputPart::Text(text) => json!({"type": "text", "text": text}),
                ProviderInputPart::Image {
                    content_type,
                    data_base64,
                } => json!({
                    "type": "image_url",
                    "image_url": {"url": image_data_url(content_type, data_base64)},
                }),
            })
            .collect(),
    )
}

pub(super) fn invalid_openai_compatible_request(message: String) -> ModelClientError {
    ModelClientError::new(ModelClientErrorKind::InvalidRequest, message, false)
}

pub(super) fn build_openai_compatible_thinking(
    provider_kind: &ModelProviderKind,
    model: &str,
    thinking_mode: Option<&str>,
) -> Result<(Option<OpenAiCompatibleThinkingRequest>, Option<String>), ModelClientError> {
    let mode = thinking_mode
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase);
    match provider_kind {
        ModelProviderKind::DeepSeek => match mode.as_deref() {
            None => Ok((None, None)),
            Some(effort @ ("low" | "high" | "max")) => Ok((
                Some(OpenAiCompatibleThinkingRequest {
                    thinking_type: "enabled".to_string(),
                    keep: None,
                }),
                Some(effort.to_string()),
            )),
            Some(mode) => Err(ModelClientError::new(
                ModelClientErrorKind::InvalidRequest,
                format!(
                    "unsupported DeepSeek thinkingMode={mode}; expected low, high, or max"
                ),
                false,
            )),
        },
        ModelProviderKind::Kimi if matches!(model, "kimi-k3" | "k3") => match mode.as_deref() {
            None => Ok((None, None)),
            Some(effort @ ("low" | "high" | "max")) => {
                Ok((None, Some(effort.to_string())))
            }
            Some(mode) => Err(ModelClientError::new(
                ModelClientErrorKind::InvalidRequest,
                format!("unsupported Kimi K3 thinkingMode={mode}; expected low, high, or max"),
                false,
            )),
        },
        ModelProviderKind::Kimi if model == "kimi-for-coding" => match mode.as_deref() {
            None | Some("preserved") => Ok((
                Some(OpenAiCompatibleThinkingRequest {
                    thinking_type: "enabled".to_string(),
                    keep: Some("all".to_string()),
                }),
                None,
            )),
            Some(mode) => Err(ModelClientError::new(
                ModelClientErrorKind::InvalidRequest,
                format!(
                    "unsupported Kimi K2.7 Code thinkingMode={mode}; preserved thinking is required"
                ),
                false,
            )),
        },
        ModelProviderKind::Zai => match mode.as_deref() {
            Some("none") => Ok((
                Some(OpenAiCompatibleThinkingRequest {
                    thinking_type: "disabled".to_string(),
                    keep: None,
                }),
                None,
            )),
            Some(effort @ ("low" | "high" | "max")) => Ok((
                Some(OpenAiCompatibleThinkingRequest {
                    thinking_type: "enabled".to_string(),
                    keep: None,
                }),
                Some(effort.to_string()),
            )),
            Some(mode) => Err(ModelClientError::new(
                ModelClientErrorKind::InvalidRequest,
                format!("unsupported Z.AI thinkingMode={mode}; expected none, low, high, or max"),
                false,
            )),
            None => Ok((None, None)),
        },
        _ => match mode.as_deref() {
            None => Ok((None, None)),
            Some("none") => Ok((None, Some("none".to_string()))),
            Some(effort @ ("low" | "medium" | "high" | "xhigh" | "max")) => {
                Ok((None, Some(effort.to_string())))
            }
            Some(mode) => Err(ModelClientError::new(
                ModelClientErrorKind::InvalidRequest,
                format!(
                    "unsupported OpenAI reasoning effort={mode}; expected low, medium, high, xhigh, or max"
                ),
                false,
            )),
        },
    }
}

pub(super) fn build_openai_compatible_tool_projection(
    provider_kind: &ModelProviderKind,
    model: &str,
    request: &ModelClientRequest,
) -> Result<(Vec<OpenAiChatToolDefinition>, Option<OpenAiChatToolChoice>), ModelClientError> {
    let tools = build_openai_chat_tools(request);
    if provider_kind == &ModelProviderKind::Kimi
        && model == "kimi-for-coding"
        && matches!(
            request.prepared_prompt.tool_choice,
            ModelToolChoice::Required | ModelToolChoice::Specific { .. }
        )
    {
        return Err(ModelClientError::new(
            ModelClientErrorKind::InvalidRequest,
            format!("{model} does not support forced tool_choice; use auto or none"),
            false,
        ));
    }
    let deepseek_thinking_enabled = provider_kind == &ModelProviderKind::DeepSeek;
    if !deepseek_thinking_enabled {
        return Ok((tools, build_openai_chat_tool_choice(request)));
    }

    match &request.prepared_prompt.tool_choice {
        ModelToolChoice::None => Ok((Vec::new(), None)),
        ModelToolChoice::Auto => Ok((tools, None)),
        ModelToolChoice::Required | ModelToolChoice::Specific { .. } => Err(ModelClientError::new(
            ModelClientErrorKind::InvalidRequest,
            "DeepSeek reasoning does not support forced tool_choice; use auto or none",
            false,
        )),
    }
}

pub(super) fn reject_openai_compatible_finish_reason(
    finish_reason: Option<&str>,
) -> Result<(), ModelClientError> {
    match finish_reason {
        Some("insufficient_system_resource") => Err(ModelClientError {
            kind: ModelClientErrorKind::ProviderUnavailable,
            message: "provider stopped generation because system resources were insufficient"
                .to_string(),
            retryable: true,
            provider_code: Some("insufficient_system_resource".to_string()),
            provider_attempts: 0,
            truncated_tool_calls: Vec::new(),
        }),
        Some("length" | "max_tokens") => Err(ModelClientError {
            kind: ModelClientErrorKind::ProviderResponseInterrupted,
            message: "provider stopped generation at the output-token limit before producing a complete response"
                .to_string(),
            retryable: false,
            provider_code: Some("incomplete_output_token_limit".to_string()),
            provider_attempts: 0,
            truncated_tool_calls: Vec::new(),
        }),
        _ => Ok(()),
    }
}

fn reject_openai_compatible_empty_final_response(
    content: &str,
    has_tool_calls: bool,
    has_reasoning_content: bool,
    finish_reason: Option<&str>,
) -> Result<(), ModelClientError> {
    if !content.trim().is_empty() || has_tool_calls {
        return Ok(());
    }
    Err(ModelClientError {
        kind: ModelClientErrorKind::ProviderResponseInterrupted,
        message: format!(
            "empty_final_response: openai-compatible provider returned an empty final response (finish_reason={}, reasoning_content_present={has_reasoning_content})",
            finish_reason.unwrap_or("missing")
        ),
        retryable: true,
        provider_code: Some("empty_final_response".to_string()),
        provider_attempts: 0,
        truncated_tool_calls: Vec::new(),
    })
}

pub(super) fn build_openai_chat_tools(
    request: &ModelClientRequest,
) -> Vec<OpenAiChatToolDefinition> {
    request
        .prepared_prompt
        .tool_definitions
        .iter()
        .map(|tool| OpenAiChatToolDefinition {
            tool_type: "function".to_string(),
            function: OpenAiChatToolFunctionDefinition {
                name: tool.name.clone(),
                description: tool.description.clone(),
                parameters: tool.input_schema.clone(),
            },
        })
        .collect()
}

pub(super) fn build_openai_chat_tool_choice(
    request: &ModelClientRequest,
) -> Option<OpenAiChatToolChoice> {
    if request.prepared_prompt.tool_definitions.is_empty() {
        return None;
    }
    match &request.prepared_prompt.tool_choice {
        ModelToolChoice::None => Some(OpenAiChatToolChoice::Mode("none".to_string())),
        ModelToolChoice::Auto => Some(OpenAiChatToolChoice::Mode("auto".to_string())),
        ModelToolChoice::Required => Some(OpenAiChatToolChoice::Mode("required".to_string())),
        ModelToolChoice::Specific { name } => Some(OpenAiChatToolChoice::Function {
            tool_type: "function".to_string(),
            function: OpenAiChatToolChoiceFunction { name: name.clone() },
        }),
    }
}

impl OpenAiCompatibleStreamState {
    fn consume_chunk(
        &mut self,
        chunk: &str,
        attempt: u32,
    ) -> Result<Vec<OpenAiCompatibleStreamUpdate>, ModelClientError> {
        let parsed = parse_sse_json_frame("openai-completions", chunk, attempt)?;
        if parsed.get("type").and_then(Value::as_str)
            == Some(MODEL_PROVIDER_WAITING_STREAM_EVENT_TYPE)
        {
            return Ok(vec![OpenAiCompatibleStreamUpdate::Status {
                message: None,
                process_state: RuntimeProcessState::ProviderWaiting,
            }]);
        }
        if let Some(id) = parsed
            .get("id")
            .and_then(Value::as_str)
            .map(ToString::to_string)
        {
            self.provider_request_id = Some(id);
        }
        if let Some(usage) = parsed.get("usage") {
            if !usage.is_null() {
                if let Ok(parsed_usage) =
                    serde_json::from_value::<OpenAiCompatibleUsage>(usage.clone())
                {
                    self.usage = Some(parsed_usage);
                }
            }
        }
        let mut updates = vec![];
        let choices = parsed
            .get("choices")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for choice in choices {
            let delta = choice.get("delta").unwrap_or(&Value::Null);
            if let Some(content) = delta.get("content").and_then(Value::as_str) {
                if !content.is_empty() {
                    self.content.push_str(content);
                    updates.push(OpenAiCompatibleStreamUpdate::Token {
                        content: content.to_string(),
                    });
                }
            }
            let parse_reasoning = |field: &'static str| match delta.get(field) {
                None | Some(Value::Null) => Ok(None),
                Some(Value::String(value)) => Ok(Some(value.clone())),
                Some(_) => Err(ModelClientError::new(
                    ModelClientErrorKind::Provider,
                    format!("openai-compatible response field {field} must be a string or null"),
                    false,
                )),
            };
            let reasoning = parse_reasoning("reasoning")?;
            let alternate_reasoning = parse_reasoning("reasoning_content")?;
            let reasoning_field = match (reasoning.is_some(), alternate_reasoning.is_some()) {
                (true, false) => Some("reasoning"),
                (false, true) => Some("reasoning_content"),
                _ => None,
            };
            if let Some(reasoning_content) =
                normalize_openai_compatible_reasoning(reasoning, alternate_reasoning)?
            {
                if self
                    .reasoning_field
                    .is_some_and(|field| Some(field) != reasoning_field)
                {
                    return Err(ModelClientError::new(
                        ModelClientErrorKind::Provider,
                        "openai-compatible stream changed reasoning field names",
                        false,
                    ));
                }
                self.reasoning_field = reasoning_field;
                let accumulated = self.reasoning_content.get_or_insert_with(String::new);
                accumulated.push_str(reasoning_content.as_str());
                if !self.thinking_status_emitted {
                    self.thinking_status_emitted = true;
                    updates.push(OpenAiCompatibleStreamUpdate::Status {
                        message: None,
                        process_state: RuntimeProcessState::Thinking,
                    });
                }
            }
            if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
                for tool_call in tool_calls {
                    let index = tool_call
                        .get("index")
                        .and_then(Value::as_u64)
                        .unwrap_or(self.tool_calls_by_index.len() as u64)
                        as usize;
                    let entry = self.tool_calls_by_index.entry(index).or_default();
                    if let Some(id) = tool_call.get("id").and_then(Value::as_str) {
                        if !id.is_empty() {
                            entry.id = Some(id.to_string());
                        }
                    }
                    if let Some(function) = tool_call.get("function") {
                        if let Some(name) = function.get("name").and_then(Value::as_str) {
                            if !name.is_empty() {
                                entry.name = Some(name.to_string());
                                if !entry.preparing_emitted && is_bounded_canonical_tool_name(name)
                                {
                                    entry.preparing_emitted = true;
                                    updates.push(OpenAiCompatibleStreamUpdate::ToolCallPreparing {
                                        name: name.to_string(),
                                    });
                                }
                            }
                        }
                        if let Some(arguments) = function.get("arguments").and_then(Value::as_str) {
                            entry.arguments.push_str(arguments);
                        }
                    }
                }
            }
            if let Some(finish_reason) = choice.get("finish_reason").and_then(Value::as_str) {
                self.finish_reason = Some(finish_reason.to_string());
                if finish_reason == "tool_calls" {
                    updates.extend(self.flush_ready_tool_calls()?);
                }
                updates.push(OpenAiCompatibleStreamUpdate::Done {
                    finish_reason: Some(finish_reason.to_string()),
                });
            }
        }
        Ok(updates)
    }

    fn flush_ready_tool_calls(
        &mut self,
    ) -> Result<Vec<OpenAiCompatibleStreamUpdate>, ModelClientError> {
        let mut indexes = self.tool_calls_by_index.keys().copied().collect::<Vec<_>>();
        indexes.sort_unstable();
        let mut updates = vec![];
        for index in indexes {
            let Some(state) = self.tool_calls_by_index.remove(&index) else {
                continue;
            };
            let (call_id, name) = required_provider_tool_call_identity(
                "openai-compatible",
                state.id.as_deref(),
                state.name.as_deref(),
            )?;
            let args_json = normalize_stream_tool_arguments(state.arguments, None);
            let args_preview = preview_tool_arguments_json(args_json.as_str());
            self.completed_tool_calls.push(ToolCallEnvelope {
                id: call_id.clone(),
                name: name.clone(),
                args_json: args_json.clone(),
            });
            updates.push(OpenAiCompatibleStreamUpdate::ToolCallReady {
                call_id,
                provider_item_id: Some(index.to_string()),
                name,
                args_json,
                args_preview,
            });
        }
        Ok(updates)
    }

    fn validate_terminal_tool_calls(&self) -> Result<(), ModelClientError> {
        if !self.tool_calls_by_index.is_empty() {
            return Err(invalid_provider_tool_call_identity("openai-compatible"));
        }
        validate_retryable_provider_tool_call_arguments(
            "openai-compatible",
            self.completed_tool_calls.as_slice(),
        )
    }

    fn validate_terminal_response(
        &self,
        finish_reason: Option<&str>,
    ) -> Result<(), ModelClientError> {
        reject_openai_compatible_empty_final_response(
            self.content.as_str(),
            !self.completed_tool_calls.is_empty() || !self.tool_calls_by_index.is_empty(),
            self.reasoning_content.is_some(),
            finish_reason,
        )
    }

    fn into_response(self) -> OpenAiCompatibleChatCompletionsResponse {
        let tool_calls = if self.completed_tool_calls.is_empty() {
            None
        } else {
            Some(
                self.completed_tool_calls
                    .into_iter()
                    .map(|item| OpenAiCompatibleToolCall {
                        id: Some(item.id),
                        function: OpenAiCompatibleToolFunction {
                            name: item.name,
                            arguments: item.args_json,
                        },
                    })
                    .collect(),
            )
        };
        OpenAiCompatibleChatCompletionsResponse {
            id: self.provider_request_id,
            choices: vec![OpenAiCompatibleChoice {
                message: OpenAiCompatibleAssistantMessage {
                    content: if self.content.is_empty() {
                        None
                    } else {
                        Some(Value::String(self.content))
                    },
                    tool_calls,
                    reasoning: None,
                    reasoning_content: self.reasoning_content,
                },
                finish_reason: self.finish_reason,
            }],
            usage: self.usage,
        }
    }
}

fn normalize_openai_compatible_reasoning(
    reasoning: Option<String>,
    reasoning_content: Option<String>,
) -> Result<Option<String>, ModelClientError> {
    match (reasoning, reasoning_content) {
        (Some(_), Some(_)) => Err(ModelClientError::new(
            ModelClientErrorKind::Provider,
            "openai-compatible response contains both reasoning and reasoning_content",
            false,
        )),
        (Some(value), None) | (None, Some(value)) => Ok(Some(value)),
        (None, None) => Ok(None),
    }
}

pub(super) fn extract_openai_compatible_text(content: Option<Value>) -> String {
    match content {
        Some(Value::String(text)) => text,
        Some(Value::Array(items)) => items
            .into_iter()
            .filter_map(|item| match item {
                Value::String(text) => Some(text),
                Value::Object(map) => map
                    .get("text")
                    .and_then(|value| value.as_str())
                    .map(|text| text.to_string()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Some(Value::Object(map)) => map
            .get("text")
            .and_then(|value| value.as_str())
            .map(|text| text.to_string())
            .unwrap_or_default(),
        _ => String::new(),
    }
}

pub(super) fn extract_openai_compatible_tool_calls(
    tool_calls: Option<Vec<OpenAiCompatibleToolCall>>,
) -> Result<Vec<ToolCallEnvelope>, ModelClientError> {
    tool_calls
        .unwrap_or_default()
        .into_iter()
        .map(|item| {
            let (id, name) = required_provider_tool_call_identity(
                "openai-compatible",
                item.id.as_deref(),
                Some(item.function.name.as_str()),
            )?;
            Ok(ToolCallEnvelope {
                id,
                name,
                args_json: normalize_tool_arguments_json(item.function.arguments),
            })
        })
        .collect()
}

fn truncated_openai_compatible_tool_calls(
    tool_calls: Option<Vec<OpenAiCompatibleToolCall>>,
) -> Vec<TruncatedToolCall> {
    tool_calls
        .unwrap_or_default()
        .into_iter()
        .map(|item| {
            truncated_tool_call(
                item.id.as_deref(),
                Some(item.function.name.as_str()),
                item.function.arguments.as_str(),
            )
        })
        .collect()
}

pub(super) fn map_openai_compatible_http_error(
    status_code: u16,
    body_json: &str,
) -> ModelClientError {
    let parsed = serde_json::from_str::<OpenAiCompatibleErrorEnvelope>(body_json).ok();
    let provider_code = parsed
        .as_ref()
        .and_then(|item| item.error.as_ref())
        .and_then(|error| error.code.clone().or_else(|| error.error_type.clone()));
    let message = parsed
        .as_ref()
        .and_then(|item| item.error.as_ref())
        .and_then(|error| error.message.clone())
        .unwrap_or_else(|| format!("openai-compatible request failed with status {status_code}"));
    let kind = if provider_code.as_deref() == Some("insufficient_system_resource") {
        ModelClientErrorKind::ProviderUnavailable
    } else {
        match status_code {
            400 => ModelClientErrorKind::InvalidRequest,
            401 | 403 => ModelClientErrorKind::AuthFailed,
            404 => {
                if provider_code.as_deref() == Some("model_not_found")
                    || message.to_ascii_lowercase().contains("model")
                {
                    ModelClientErrorKind::ModelUnavailable
                } else {
                    ModelClientErrorKind::InvalidRequest
                }
            }
            408 => ModelClientErrorKind::Timeout,
            429 => ModelClientErrorKind::ProviderBusyOrRateLimited,
            500..=599 => ModelClientErrorKind::ProviderUnavailable,
            _ => ModelClientErrorKind::Unknown,
        }
    };
    let retryable = is_retryable_model_client_error_kind(kind);
    ModelClientError {
        kind,
        message,
        retryable,
        provider_code,
        provider_attempts: 0,
        truncated_tool_calls: Vec::new(),
    }
}
