use super::*;

pub struct AnthropicMessagesModelClient<T: JsonHttpTransport> {
    registry: ModelProviderRegistry,
    transport: T,
}

impl<T: JsonHttpTransport> AnthropicMessagesModelClient<T> {
    pub fn new(registry: ModelProviderRegistry, transport: T) -> Self {
        Self {
            registry,
            transport,
        }
    }

    fn build_http_request(
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
        if resolved.provider.info.wire_api != WireApi::AnthropicMessages {
            return Err(ModelClientError::new(
                ModelClientErrorKind::InvalidRequest,
                format!(
                    "anthropic-messages adapter does not support wire_api={:?}",
                    resolved.provider.info.wire_api
                ),
                false,
            ));
        }
        let base_url = resolved.effective_api_base.ok_or_else(|| {
            ModelClientError::new(
                ModelClientErrorKind::InvalidRequest,
                "provider api_base is required for anthropic-messages adapter",
                false,
            )
        })?;
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
        let (system, messages) = build_anthropic_messages(request)?;
        let effort = resolved
            .session_config
            .thinking_mode
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty() && *value != "default");
        if effort
            .is_some_and(|effort| !matches!(effort, "low" | "medium" | "high" | "xhigh" | "max"))
        {
            return Err(ModelClientError::new(
                ModelClientErrorKind::InvalidRequest,
                format!(
                    "unsupported Anthropic effort={}; expected low, medium, high, xhigh, or max",
                    effort.expect("checked invalid effort")
                ),
                false,
            ));
        }
        let payload = AnthropicMessagesRequest {
            model: resolved.session_config.model.clone(),
            max_tokens: prepared_prompt_max_output_tokens(request, &resolved.session_config)?
                .ok_or_else(|| {
                    ModelClientError::new(
                        ModelClientErrorKind::InvalidRequest,
                        "anthropic-messages requires max_tokens",
                        false,
                    )
                })?,
            stream,
            system,
            messages,
            tools: build_anthropic_tools(request),
            tool_choice: build_anthropic_tool_choice(request),
            thinking: (resolved.provider.info.provider_kind == ModelProviderKind::Anthropic)
                .then_some(AnthropicThinking {
                    thinking_type: "adaptive".to_string(),
                }),
            output_config: effort.map(|effort| AnthropicOutputConfig {
                effort: effort.to_string(),
            }),
        };
        let body_json = serde_json::to_string(&payload).map_err(|error| {
            ModelClientError::new(
                ModelClientErrorKind::InvalidRequest,
                format!("serialize anthropic-messages request failed: {error}"),
                false,
            )
        })?;
        Ok(JsonHttpRequest {
            method: "POST".to_string(),
            url: format!("{}/messages", base_url.trim_end_matches('/')),
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
            return Err(map_anthropic_messages_http_error(
                response.status_code,
                response.body_json.as_str(),
            ));
        }
        let parsed: AnthropicMessageResponse = serde_json::from_str(response.body_json.as_str())
            .map_err(|error| {
                ModelClientError::new(
                    ModelClientErrorKind::Provider,
                    format!("parse anthropic-messages response failed: {error}"),
                    false,
                )
            })?;
        let raw_content = parsed.content.clone();
        if parsed.stop_reason.as_deref() == Some("max_tokens") {
            return Err(reject_anthropic_stop_reason(parsed.stop_reason.as_deref())
                .expect_err("max_tokens must be rejected")
                .with_truncated_tool_calls(truncated_anthropic_tool_calls(
                    raw_content.as_slice(),
                )));
        }
        reject_anthropic_stop_reason(parsed.stop_reason.as_deref())?;
        let tool_calls = extract_anthropic_tool_calls(parsed.content.as_slice())?;
        validate_provider_tool_call_arguments("anthropic-messages", &tool_calls)?;
        let usage = parsed.usage.as_ref();
        Ok(ModelClientResponse {
            generate_result: GenerateResult {
                content: extract_anthropic_text(parsed.content.as_slice()),
                tool_calls,
                reasoning_content: None,
                input_tokens: usage.and_then(|item| item.input_tokens),
                total_tokens: usage.and_then(|item| {
                    match (item.input_tokens, item.output_tokens) {
                        (Some(input), Some(output)) => Some(input + output),
                        _ => None,
                    }
                }),
                prompt_cache_hit_tokens: usage.and_then(|item| item.cache_read_input_tokens),
                prompt_cache_miss_tokens: usage.and_then(anthropic_cache_miss_tokens),
            },
            provider_request_id: parsed
                .id
                .or_else(|| response.headers.get("x-request-id").cloned()),
            provider_latency_ms: None,
            provider_attempts: 1,
        })
    }
}

impl<T: JsonHttpTransport> ModelClient for AnthropicMessagesModelClient<T> {
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
            let mut stream_state = AnthropicMessagesStreamState::default();
            let mut pending_completion_events = Vec::<ModelClientStreamEvent>::new();
            let mut attempt_has_visible_content = false;
            let attempted =
                execute_sse_with_retries(&self.transport, &http_request, &mut |event| {
                    let chunk = match event {
                        SseAttemptEvent::Start { attempt } => {
                            stream_state = AnthropicMessagesStreamState::default();
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
                            AnthropicMessagesStreamUpdate::Status {
                                message,
                                process_state,
                            } => sink(ModelClientStreamEvent::Status {
                                message,
                                process_state,
                            }),
                            AnthropicMessagesStreamUpdate::Token { content } => {
                                attempt_has_visible_content = true;
                                sink(ModelClientStreamEvent::Token { content });
                            }
                            AnthropicMessagesStreamUpdate::ToolCallPreparing { name } => {
                                sink(ModelClientStreamEvent::ToolCallPreparing { name });
                            }
                            AnthropicMessagesStreamUpdate::ToolCallReady {
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
                            AnthropicMessagesStreamUpdate::Error { error } => {
                                progress.terminal = true;
                                progress.terminal_error = Some(error);
                            }
                            AnthropicMessagesStreamUpdate::Done { finish_reason } => {
                                progress.terminal = true;
                                progress.terminal_error =
                                    reject_anthropic_stop_reason(finish_reason.as_deref())
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
                                        .or_else(|| {
                                            stream_state.validate_terminal_tool_calls().err()
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
                return Err(map_anthropic_messages_http_error(
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
                    body_json: serde_json::to_string(&parsed).map_err(|error| {
                        ModelClientError::new(
                            ModelClientErrorKind::Provider,
                            format!(
                                "serialize anthropic-messages stream completion failed: {error}"
                            ),
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(super) struct AnthropicMessagesRequest {
    model: String,
    max_tokens: u32,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    messages: Vec<AnthropicMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    tools: Vec<AnthropicToolDefinition>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<AnthropicToolChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<AnthropicThinking>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_config: Option<AnthropicOutputConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct AnthropicThinking {
    #[serde(rename = "type")]
    thinking_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct AnthropicOutputConfig {
    effort: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(super) struct AnthropicMessage {
    role: String,
    content: Vec<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(super) struct AnthropicToolDefinition {
    name: String,
    description: String,
    input_schema: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum AnthropicToolChoice {
    Auto,
    Any,
    Tool { name: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct AnthropicMessageResponse {
    id: Option<String>,
    content: Vec<Value>,
    stop_reason: Option<String>,
    usage: Option<AnthropicUsage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct AnthropicUsage {
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    cache_creation_input_tokens: Option<i64>,
    cache_read_input_tokens: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct AnthropicErrorEnvelope {
    error: Option<AnthropicErrorObject>,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct AnthropicErrorObject {
    #[serde(rename = "type")]
    error_type: Option<String>,
    message: Option<String>,
}

#[derive(Debug, Default)]
pub(super) struct AnthropicMessagesStreamState {
    provider_request_id: Option<String>,
    content: String,
    completed_tool_calls: Vec<ToolCallEnvelope>,
    tool_calls_by_index: HashMap<usize, AnthropicStreamToolCallState>,
    usage: Option<AnthropicUsage>,
    stop_reason: Option<String>,
}

#[derive(Debug, Default)]
pub(super) struct AnthropicStreamToolCallState {
    id: Option<String>,
    name: Option<String>,
    input_json: String,
    preparing_emitted: bool,
}

pub(super) enum AnthropicMessagesStreamUpdate {
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
    Error {
        error: ModelClientError,
    },
    Done {
        finish_reason: Option<String>,
    },
}

pub(super) fn build_anthropic_messages(
    request: &ModelClientRequest,
) -> Result<(Option<String>, Vec<AnthropicMessage>), ModelClientError> {
    let mut system_parts = request
        .prepared_prompt
        .system_prompt
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| vec![value.to_string()])
        .unwrap_or_default();
    let mut messages = Vec::<AnthropicMessage>::new();
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
                        "anthropic system message {} appears after conversation content",
                        item.message_id
                    )));
                }
                let trimmed = item.content.trim();
                if !trimmed.is_empty() {
                    system_parts.push(trimmed.to_string());
                }
            }
            ModelMessageRoleV1::User => {
                let content = provider_input_parts(request, item)?
                    .into_iter()
                    .map(|part| match part {
                        ProviderInputPart::Text(text) => json!({"type": "text", "text": text}),
                        ProviderInputPart::Image {
                            content_type,
                            data_base64,
                        } => json!({
                            "type": "image",
                            "source": {
                                "type": "base64",
                                "media_type": content_type,
                                "data": data_base64,
                            }
                        }),
                    })
                    .collect::<Vec<_>>();
                if !content.is_empty() {
                    push_anthropic_message(&mut messages, "user", content);
                }
            }
            ModelMessageRoleV1::Assistant => {
                if item
                    .reasoning_content
                    .as_deref()
                    .is_some_and(|value| !value.trim().is_empty())
                {
                    return Err(invalid_openai_compatible_request(format!(
                        "anthropic reasoning history requires provider signatures: messageId={}",
                        item.message_id
                    )));
                }
                let mut content = Vec::<Value>::new();
                let trimmed = item.content.trim();
                if !trimmed.is_empty() {
                    content.push(json!({ "type": "text", "text": trimmed }));
                }
                for tool_call in &item.tool_calls {
                    let input = serde_json::from_str::<Value>(tool_call.args_json.as_str())
                        .map_err(|error| {
                            invalid_openai_compatible_request(format!(
                                "anthropic tool call {} has invalid arguments: {error}",
                                tool_call.id
                            ))
                        })?;
                    content.push(json!({
                        "type": "tool_use",
                        "id": tool_call.id,
                        "name": tool_call.name,
                        "input": input,
                    }));
                    pending_tool_call_ids.push(tool_call.id.clone());
                }
                if !content.is_empty() {
                    push_anthropic_message(&mut messages, "assistant", content);
                }
            }
            ModelMessageRoleV1::Tool => {
                let tool_call_id = item.tool_call_id.as_deref().ok_or_else(|| {
                    invalid_openai_compatible_request(format!(
                        "tool message {} is missing toolCallId",
                        item.message_id
                    ))
                })?;
                let expected = pending_tool_call_ids.first().ok_or_else(|| {
                    invalid_openai_compatible_request(format!(
                        "tool message {} has no preceding assistant tool call",
                        item.message_id
                    ))
                })?;
                if expected != tool_call_id {
                    return Err(invalid_openai_compatible_request(format!(
                        "tool message {} has toolCallId={} but expected {}",
                        item.message_id, tool_call_id, expected
                    )));
                }
                pending_tool_call_ids.remove(0);
                push_anthropic_message(
                    &mut messages,
                    "user",
                    vec![json!({
                        "type": "tool_result",
                        "tool_use_id": tool_call_id,
                        "content": item.content,
                    })],
                );
            }
        }
    }
    if let Some(call_id) = pending_tool_call_ids.first() {
        return Err(invalid_openai_compatible_request(format!(
            "assistant tool call {call_id} has no following tool result"
        )));
    }
    if messages.is_empty() {
        return Err(invalid_openai_compatible_request(
            "anthropic-messages requires at least one non-system message".to_string(),
        ));
    }
    let system = (!system_parts.is_empty()).then(|| system_parts.join("\n\n"));
    Ok((system, messages))
}

pub(super) fn push_anthropic_message(
    messages: &mut Vec<AnthropicMessage>,
    role: &str,
    content: Vec<Value>,
) {
    if let Some(previous) = messages.last_mut().filter(|item| item.role == role) {
        previous.content.extend(content);
    } else {
        messages.push(AnthropicMessage {
            role: role.to_string(),
            content,
        });
    }
}

pub(super) fn build_anthropic_tools(request: &ModelClientRequest) -> Vec<AnthropicToolDefinition> {
    if matches!(request.prepared_prompt.tool_choice, ModelToolChoice::None) {
        return Vec::new();
    }
    request
        .prepared_prompt
        .tool_definitions
        .iter()
        .map(|tool| AnthropicToolDefinition {
            name: tool.name.clone(),
            description: tool.description.clone(),
            input_schema: tool.input_schema.clone(),
        })
        .collect()
}

pub(super) fn build_anthropic_tool_choice(
    request: &ModelClientRequest,
) -> Option<AnthropicToolChoice> {
    if request.prepared_prompt.tool_definitions.is_empty()
        || matches!(request.prepared_prompt.tool_choice, ModelToolChoice::None)
    {
        return None;
    }
    match &request.prepared_prompt.tool_choice {
        ModelToolChoice::None => None,
        ModelToolChoice::Auto => Some(AnthropicToolChoice::Auto),
        ModelToolChoice::Required => Some(AnthropicToolChoice::Any),
        ModelToolChoice::Specific { name } => {
            Some(AnthropicToolChoice::Tool { name: name.clone() })
        }
    }
}

impl AnthropicMessagesStreamState {
    fn truncated_tool_calls(&self) -> Vec<TruncatedToolCall> {
        self.tool_calls_by_index
            .values()
            .map(|call| {
                truncated_tool_call(
                    call.id.as_deref(),
                    call.name.as_deref(),
                    call.input_json.as_str(),
                )
            })
            .chain(self.completed_tool_calls.iter().map(|call| {
                truncated_tool_call(
                    Some(call.id.as_str()),
                    Some(call.name.as_str()),
                    call.args_json.as_str(),
                )
            }))
            .collect()
    }

    fn consume_chunk(
        &mut self,
        chunk: &str,
        attempt: u32,
    ) -> Result<Vec<AnthropicMessagesStreamUpdate>, ModelClientError> {
        let parsed = parse_sse_json_frame("anthropic-messages", chunk, attempt)?;
        let event_type = parsed
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        Ok(match event_type {
            MODEL_PROVIDER_WAITING_STREAM_EVENT_TYPE | "ping" => {
                vec![AnthropicMessagesStreamUpdate::Status {
                    message: None,
                    process_state: RuntimeProcessState::ProviderWaiting,
                }]
            }
            "message_start" => {
                let message = parsed.get("message");
                self.provider_request_id = message
                    .and_then(|item| item.get("id"))
                    .and_then(Value::as_str)
                    .map(ToString::to_string);
                self.usage = message
                    .and_then(|item| item.get("usage"))
                    .and_then(|item| serde_json::from_value::<AnthropicUsage>(item.clone()).ok());
                vec![AnthropicMessagesStreamUpdate::Status {
                    message: None,
                    process_state: RuntimeProcessState::Thinking,
                }]
            }
            "content_block_start" => {
                let Some(index) = parsed.get("index").and_then(Value::as_u64) else {
                    return Ok(Vec::new());
                };
                let Some(content_block) = parsed.get("content_block") else {
                    return Ok(Vec::new());
                };
                if content_block.get("type").and_then(Value::as_str) != Some("tool_use") {
                    return Ok(Vec::new());
                }
                let state = self.tool_calls_by_index.entry(index as usize).or_default();
                state.id = content_block
                    .get("id")
                    .and_then(Value::as_str)
                    .map(ToString::to_string);
                state.name = content_block
                    .get("name")
                    .and_then(Value::as_str)
                    .map(ToString::to_string);
                if let Some(input) = content_block
                    .get("input")
                    .filter(|value| value.as_object().is_some_and(|object| !object.is_empty()))
                {
                    state.input_json = normalize_tool_arguments_value(input);
                }
                if !state.preparing_emitted {
                    if let Some(name) = state
                        .name
                        .as_deref()
                        .filter(|name| is_bounded_canonical_tool_name(name))
                    {
                        state.preparing_emitted = true;
                        return Ok(vec![AnthropicMessagesStreamUpdate::ToolCallPreparing {
                            name: name.to_string(),
                        }]);
                    }
                }
                Vec::new()
            }
            "content_block_delta" => {
                let delta = parsed.get("delta");
                let delta_type = delta
                    .and_then(|item| item.get("type"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                match delta_type {
                    "text_delta" => {
                        let text = delta
                            .and_then(|item| item.get("text"))
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        if text.is_empty() {
                            Vec::new()
                        } else {
                            self.content.push_str(text);
                            vec![AnthropicMessagesStreamUpdate::Token {
                                content: text.to_string(),
                            }]
                        }
                    }
                    "input_json_delta" => {
                        let Some(index) = parsed.get("index").and_then(Value::as_u64) else {
                            return Ok(Vec::new());
                        };
                        let partial_json = delta
                            .and_then(|item| item.get("partial_json"))
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        self.tool_calls_by_index
                            .entry(index as usize)
                            .or_default()
                            .input_json
                            .push_str(partial_json);
                        Vec::new()
                    }
                    _ => Vec::new(),
                }
            }
            "content_block_stop" => {
                let Some(index) = parsed.get("index").and_then(Value::as_u64) else {
                    return Ok(Vec::new());
                };
                self.finish_tool_call(index as usize).into_iter().collect()
            }
            "message_delta" => {
                self.stop_reason = parsed
                    .get("delta")
                    .and_then(|item| item.get("stop_reason"))
                    .and_then(Value::as_str)
                    .map(ToString::to_string);
                if let Some(usage) = parsed
                    .get("usage")
                    .and_then(|item| serde_json::from_value::<AnthropicUsage>(item.clone()).ok())
                {
                    self.usage = Some(usage);
                }
                Vec::new()
            }
            "message_stop" => vec![AnthropicMessagesStreamUpdate::Done {
                finish_reason: self.stop_reason.clone(),
            }],
            "error" => vec![AnthropicMessagesStreamUpdate::Error {
                error: map_anthropic_stream_error(&parsed),
            }],
            _ => Vec::new(),
        })
    }

    fn finish_tool_call(&mut self, index: usize) -> Option<AnthropicMessagesStreamUpdate> {
        let identity = self.tool_calls_by_index.get(&index).and_then(|state| {
            required_provider_tool_call_identity(
                "anthropic-messages",
                state.id.as_deref(),
                state.name.as_deref(),
            )
            .ok()
        })?;
        let state = self.tool_calls_by_index.remove(&index)?;
        let (call_id, name) = identity;
        let args_json = normalize_stream_tool_arguments(state.input_json, None);
        self.completed_tool_calls.push(ToolCallEnvelope {
            id: call_id.clone(),
            name: name.clone(),
            args_json: args_json.clone(),
        });
        Some(AnthropicMessagesStreamUpdate::ToolCallReady {
            call_id,
            provider_item_id: Some(index.to_string()),
            name,
            args_preview: preview_tool_arguments_json(args_json.as_str()),
            args_json,
        })
    }

    fn validate_terminal_tool_calls(&self) -> Result<(), ModelClientError> {
        if !self.tool_calls_by_index.is_empty() {
            return Err(invalid_provider_tool_call_identity("anthropic-messages"));
        }
        validate_retryable_provider_tool_call_arguments(
            "anthropic-messages",
            self.completed_tool_calls.as_slice(),
        )
    }

    fn into_response(mut self) -> AnthropicMessageResponse {
        let mut indexes = self.tool_calls_by_index.keys().copied().collect::<Vec<_>>();
        indexes.sort_unstable();
        for index in indexes {
            let _ = self.finish_tool_call(index);
        }
        let mut content = Vec::new();
        if !self.content.is_empty() {
            content.push(json!({ "type": "text", "text": self.content }));
        }
        for tool_call in self.completed_tool_calls {
            let input = serde_json::from_str::<Value>(tool_call.args_json.as_str())
                .unwrap_or_else(|_| Value::Object(Default::default()));
            content.push(json!({
                "type": "tool_use",
                "id": tool_call.id,
                "name": tool_call.name,
                "input": input,
            }));
        }
        AnthropicMessageResponse {
            id: self.provider_request_id,
            content,
            stop_reason: self.stop_reason,
            usage: self.usage,
        }
    }
}

pub(super) fn extract_anthropic_text(content: &[Value]) -> String {
    content
        .iter()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|item| item.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn extract_anthropic_tool_calls(
    content: &[Value],
) -> Result<Vec<ToolCallEnvelope>, ModelClientError> {
    content
        .iter()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("tool_use"))
        .map(|item| {
            let (id, name) = required_provider_tool_call_identity(
                "anthropic-messages",
                item.get("id").and_then(Value::as_str),
                item.get("name").and_then(Value::as_str),
            )?;
            Ok(ToolCallEnvelope {
                id,
                name,
                args_json: item
                    .get("input")
                    .map(normalize_tool_arguments_value)
                    .unwrap_or_else(|| "{}".to_string()),
            })
        })
        .collect()
}

fn truncated_anthropic_tool_calls(content: &[Value]) -> Vec<TruncatedToolCall> {
    content
        .iter()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("tool_use"))
        .map(|item| {
            let args = item
                .get("input")
                .map(normalize_tool_arguments_value)
                .unwrap_or_default();
            truncated_tool_call(
                item.get("id").and_then(Value::as_str),
                item.get("name").and_then(Value::as_str),
                args.as_str(),
            )
        })
        .collect()
}

pub(super) fn anthropic_cache_miss_tokens(usage: &AnthropicUsage) -> Option<i64> {
    usage.cache_creation_input_tokens.or_else(|| {
        let input_tokens = usage.input_tokens?;
        let cache_read_tokens = usage.cache_read_input_tokens.unwrap_or(0);
        Some(input_tokens.saturating_sub(cache_read_tokens))
    })
}

pub(super) fn reject_anthropic_stop_reason(
    stop_reason: Option<&str>,
) -> Result<(), ModelClientError> {
    match stop_reason {
        Some("max_tokens") => Err(ModelClientError {
            kind: ModelClientErrorKind::ProviderResponseInterrupted,
            message: "provider stopped generation at the output-token limit before producing a complete response".to_string(),
            retryable: false,
            provider_code: Some("incomplete_output_token_limit".to_string()),
            provider_attempts: 0,
            truncated_tool_calls: Vec::new(),
        }),
        _ => Ok(()),
    }
}

pub(super) fn map_anthropic_stream_error(value: &Value) -> ModelClientError {
    let body_json = serde_json::to_string(value).unwrap_or_else(|_| "{}".to_string());
    let status_code = match value
        .get("error")
        .and_then(|item| item.get("type"))
        .and_then(Value::as_str)
    {
        Some("overloaded_error") => 529,
        _ => 400,
    };
    map_anthropic_messages_http_error(status_code, body_json.as_str())
}

pub(super) fn map_anthropic_messages_http_error(
    status_code: u16,
    body_json: &str,
) -> ModelClientError {
    let parsed = serde_json::from_str::<AnthropicErrorEnvelope>(body_json).ok();
    let provider_code = parsed
        .as_ref()
        .and_then(|item| item.error.as_ref())
        .and_then(|item| item.error_type.clone());
    let message = parsed
        .as_ref()
        .and_then(|item| item.error.as_ref())
        .and_then(|item| item.message.clone())
        .unwrap_or_else(|| format!("anthropic-messages request failed with status {status_code}"));
    let kind = match status_code {
        400 => ModelClientErrorKind::InvalidRequest,
        401 | 403 => ModelClientErrorKind::AuthFailed,
        404 => {
            if provider_code.as_deref() == Some("not_found_error")
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
    };
    ModelClientError {
        kind,
        message,
        retryable: is_retryable_model_client_error_kind(kind),
        provider_code,
        provider_attempts: 0,
        truncated_tool_calls: Vec::new(),
    }
}
