use super::*;

pub struct OpenAiResponsesModelClient<T: JsonHttpTransport> {
    registry: ModelProviderRegistry,
    pub(super) transport: T,
}

impl<T: JsonHttpTransport> OpenAiResponsesModelClient<T> {
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
        if resolved.provider.info.wire_api != WireApi::OpenAiResponses {
            return Err(ModelClientError::new(
                ModelClientErrorKind::InvalidRequest,
                format!(
                    "openai-responses adapter does not support wire_api={:?}",
                    resolved.provider.info.wire_api
                ),
                false,
            ));
        }
        let base_url = resolved.effective_api_base.ok_or_else(|| {
            ModelClientError::new(
                ModelClientErrorKind::InvalidRequest,
                "provider api_base is required for openai-responses adapter",
                false,
            )
        })?;
        let url = format!("{}/responses", base_url.trim_end_matches('/'));
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
        let payload = OpenAiResponsesRequest {
            model: resolved.session_config.model.clone(),
            prompt_cache_key: build_openai_responses_prompt_cache_key(
                request,
                resolved.session_config.model.as_str(),
            ),
            prompt_cache_retention: request.provider_prompt_cache_retention.clone(),
            instructions: request
                .prepared_prompt
                .system_prompt
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string),
            input: build_openai_responses_input(request)?,
            stream,
            max_output_tokens: prepared_prompt_max_output_tokens(
                request,
                &resolved.session_config,
            )?,
            reasoning: build_openai_responses_reasoning(
                resolved.session_config.thinking_mode.as_deref(),
            )?,
            tools: build_openai_responses_tools(request),
            tool_choice: build_openai_responses_tool_choice(request),
        };
        let body_json = serde_json::to_string(&payload).map_err(|err| {
            ModelClientError::new(
                ModelClientErrorKind::InvalidRequest,
                format!("serialize openai-responses request failed: {err}"),
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
            return Err(map_openai_responses_http_error(
                response.status_code,
                response.body_json.as_str(),
            ));
        }
        let parsed: OpenAiResponsesResponse = serde_json::from_str(response.body_json.as_str())
            .map_err(|err| {
                ModelClientError::new(
                    ModelClientErrorKind::Provider,
                    format!("parse openai-responses response failed: {err}"),
                    false,
                )
            })?;
        let raw_truncated_tool_calls = parsed
            .output
            .as_deref()
            .map(truncated_openai_responses_tool_calls)
            .unwrap_or_default();
        if parsed
            .incomplete_details
            .as_ref()
            .and_then(|details| details.reason.as_deref())
            == Some("max_output_tokens")
        {
            return Err(reject_openai_responses_finish_reason(
                parsed
                    .incomplete_details
                    .as_ref()
                    .and_then(|details| details.reason.as_deref()),
            )
            .expect_err("max_output_tokens must be rejected")
            .with_truncated_tool_calls(raw_truncated_tool_calls));
        }
        reject_openai_responses_finish_reason(
            parsed
                .incomplete_details
                .as_ref()
                .and_then(|details| details.reason.as_deref())
                .or(parsed.status.as_deref()),
        )?;
        let provider_request_id = parsed
            .id
            .clone()
            .or_else(|| response.headers.get("x-request-id").cloned());
        let tool_calls = extract_openai_responses_tool_calls(parsed.output.as_deref())?;
        validate_provider_tool_call_arguments("openai-responses", &tool_calls)?;
        Ok(ModelClientResponse {
            generate_result: GenerateResult {
                content: extract_openai_responses_text(
                    parsed.output_text.clone(),
                    parsed.output.as_deref(),
                ),
                tool_calls,
                reasoning_content: extract_openai_responses_reasoning(parsed.output.as_deref()),
                input_tokens: parsed.usage.as_ref().and_then(|usage| usage.input_tokens),
                total_tokens: parsed.usage.as_ref().and_then(|usage| {
                    usage
                        .total_tokens
                        .or_else(|| match (usage.input_tokens, usage.output_tokens) {
                            (Some(input_tokens), Some(output_tokens)) => {
                                Some(input_tokens + output_tokens)
                            }
                            _ => None,
                        })
                }),
                prompt_cache_hit_tokens: parsed
                    .usage
                    .as_ref()
                    .and_then(openai_responses_cached_tokens),
                prompt_cache_miss_tokens: parsed
                    .usage
                    .as_ref()
                    .and_then(openai_responses_cache_miss_tokens),
            },
            provider_request_id,
            provider_latency_ms: None,
            provider_attempts: 1,
        })
    }
}

impl<T: JsonHttpTransport> ModelClient for OpenAiResponsesModelClient<T> {
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
            let mut stream_state = OpenAiResponsesStreamState::default();
            let mut pending_completion_events = Vec::<ModelClientStreamEvent>::new();
            let mut attempt_has_visible_content = false;
            let attempted =
                execute_sse_with_retries(&self.transport, &http_request, &mut |event| {
                    let chunk = match event {
                        SseAttemptEvent::Start { attempt } => {
                            stream_state = OpenAiResponsesStreamState::default();
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
                            OpenAiResponsesStreamUpdate::Token { content } => {
                                attempt_has_visible_content = true;
                                sink(ModelClientStreamEvent::Token { content });
                            }
                            OpenAiResponsesStreamUpdate::Status {
                                message,
                                process_state,
                            }
                            | OpenAiResponsesStreamUpdate::ProcessStatus {
                                message,
                                process_state,
                            } => sink(ModelClientStreamEvent::Status {
                                message,
                                process_state,
                            }),
                            OpenAiResponsesStreamUpdate::ToolCallPreparing { name } => {
                                sink(ModelClientStreamEvent::ToolCallPreparing { name });
                            }
                            OpenAiResponsesStreamUpdate::ToolCallReady {
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
                            OpenAiResponsesStreamUpdate::Done { finish_reason } => {
                                progress.terminal = true;
                                progress.terminal_error =
                                    reject_openai_responses_finish_reason(finish_reason.as_deref())
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
                return Err(map_openai_responses_http_error(
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
                            format!("serialize openai-responses completion failed: {err}"),
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

pub(super) fn build_openai_responses_prompt_cache_key(
    request: &ModelClientRequest,
    model: &str,
) -> Option<String> {
    let seed = request.provider_prompt_cache_key.as_deref()?.trim();
    if seed.is_empty() {
        return None;
    }
    Some(format!(
        "centaeris-pcache-v1:{}",
        stable_model_client_hash(
            serde_json::json!({
                "schema": "openai_responses_prompt_cache_key_v1",
                "seed": seed,
                "model": model,
            })
            .to_string()
            .as_str()
        )
    ))
}

pub(super) fn stable_model_client_hash(value: &str) -> String {
    let mut hash = 1469598103934665603u64;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(1099511628211);
    }
    format!("{hash:016x}")
}

pub(super) fn openai_responses_input_tokens(usage: &OpenAiResponsesUsage) -> Option<i64> {
    usage.input_tokens
}

pub(super) fn openai_responses_cached_tokens(usage: &OpenAiResponsesUsage) -> Option<i64> {
    usage
        .input_tokens_details
        .as_ref()
        .and_then(|details| details.cached_tokens)
        .or_else(|| {
            usage
                .prompt_tokens_details
                .as_ref()
                .and_then(|details| details.cached_tokens)
        })
}

pub(super) fn openai_responses_cache_miss_tokens(usage: &OpenAiResponsesUsage) -> Option<i64> {
    let input_tokens = openai_responses_input_tokens(usage)?;
    let cached_tokens = openai_responses_cached_tokens(usage)?;
    Some(input_tokens.saturating_sub(cached_tokens))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum OpenAiResponsesInputItem {
    Message {
        role: String,
        content: Vec<OpenAiResponsesInputContentItem>,
    },
    FunctionCall {
        call_id: String,
        name: String,
        arguments: String,
    },
    FunctionCallOutput {
        call_id: String,
        output: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum OpenAiResponsesInputContentItem {
    InputText { text: String },
    InputImage { image_url: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(super) struct OpenAiResponsesRequest {
    model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt_cache_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt_cache_retention: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    instructions: Option<String>,
    input: Vec<OpenAiResponsesInputItem>,
    #[serde(default)]
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_output_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning: Option<OpenAiResponsesReasoning>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    tools: Vec<OpenAiResponsesToolDefinition>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<OpenAiResponsesToolChoice>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct OpenAiResponsesReasoning {
    effort: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct OpenAiResponsesResponse {
    id: Option<String>,
    status: Option<String>,
    incomplete_details: Option<OpenAiResponsesIncompleteDetails>,
    output_text: Option<String>,
    output: Option<Vec<Value>>,
    usage: Option<OpenAiResponsesUsage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct OpenAiResponsesIncompleteDetails {
    reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct OpenAiResponsesUsage {
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    total_tokens: Option<i64>,
    input_tokens_details: Option<OpenAiTokenDetails>,
    prompt_tokens_details: Option<OpenAiTokenDetails>,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct OpenAiResponsesErrorEnvelope {
    error: Option<OpenAiResponsesErrorObject>,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct OpenAiResponsesErrorObject {
    message: Option<String>,
    code: Option<String>,
    #[serde(rename = "type")]
    error_type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(super) struct OpenAiResponsesToolDefinition {
    #[serde(rename = "type")]
    tool_type: String,
    name: String,
    description: String,
    parameters: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub(super) enum OpenAiResponsesToolChoice {
    Mode(String),
    Function {
        #[serde(rename = "type")]
        tool_type: String,
        name: String,
    },
}

#[derive(Debug, Default)]
pub(super) struct OpenAiResponsesStreamState {
    completed_response: Option<OpenAiResponsesResponse>,
    accumulated_output_text: String,
    tool_calls: Vec<ToolCallEnvelope>,
    usage: Option<OpenAiResponsesUsage>,
    provider_request_id: Option<String>,
    function_calls_by_item: HashMap<String, OpenAiResponsesStreamFunctionCallState>,
}

#[derive(Debug, Default)]
pub(super) struct OpenAiResponsesStreamFunctionCallState {
    call_id: Option<String>,
    name: Option<String>,
    arguments: String,
    preparing_emitted: bool,
}

pub(super) enum OpenAiResponsesStreamUpdate {
    Token {
        content: String,
    },
    Status {
        message: Option<String>,
        process_state: RuntimeProcessState,
    },
    ProcessStatus {
        message: Option<String>,
        process_state: RuntimeProcessState,
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

pub(super) fn build_openai_responses_input(
    request: &ModelClientRequest,
) -> Result<Vec<OpenAiResponsesInputItem>, ModelClientError> {
    request
        .prepared_prompt
        .validate()
        .map_err(invalid_openai_compatible_request)?;
    let mut input = vec![];
    for item in &request.prepared_prompt.messages {
        if item
            .reasoning_content
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        {
            return Err(invalid_openai_compatible_request(format!(
                "openai_responses_reasoning_history_unsupported: messageId={}",
                item.message_id
            )));
        }
        let role = match item.role {
            ModelMessageRoleV1::System => "system",
            ModelMessageRoleV1::User => "user",
            ModelMessageRoleV1::Assistant => "assistant",
            ModelMessageRoleV1::Tool => {
                let call_id = item.tool_call_id.as_deref().ok_or_else(|| {
                    invalid_openai_compatible_request(format!(
                        "openai_responses_tool_result_missing_call_id: messageId={}",
                        item.message_id
                    ))
                })?;
                input.push(OpenAiResponsesInputItem::FunctionCallOutput {
                    call_id: call_id.to_string(),
                    output: item.content.clone(),
                });
                continue;
            }
        };
        let content = provider_input_parts(request, item)?
            .into_iter()
            .map(|part| match part {
                ProviderInputPart::Text(text) => {
                    OpenAiResponsesInputContentItem::InputText { text }
                }
                ProviderInputPart::Image {
                    content_type,
                    data_base64,
                } => OpenAiResponsesInputContentItem::InputImage {
                    image_url: image_data_url(content_type.as_str(), data_base64.as_str()),
                },
            })
            .collect::<Vec<_>>();
        if !content.is_empty() {
            input.push(OpenAiResponsesInputItem::Message {
                role: role.to_string(),
                content,
            });
        }
        for tool_call in &item.tool_calls {
            input.push(OpenAiResponsesInputItem::FunctionCall {
                call_id: tool_call.id.clone(),
                name: tool_call.name.clone(),
                arguments: tool_call.args_json.clone(),
            });
        }
    }
    Ok(input)
}

pub(super) fn build_openai_responses_reasoning(
    thinking_mode: Option<&str>,
) -> Result<Option<OpenAiResponsesReasoning>, ModelClientError> {
    let effort = thinking_mode
        .map(str::trim)
        .filter(|value| !value.is_empty() && *value != "default");
    let Some(effort) = effort else {
        return Ok(None);
    };
    if !matches!(effort, "none" | "low" | "medium" | "high" | "xhigh" | "max") {
        return Err(ModelClientError::new(
            ModelClientErrorKind::InvalidRequest,
            format!(
                "unsupported OpenAI Responses effort={effort}; expected none, low, medium, high, xhigh, or max"
            ),
            false,
        ));
    }
    Ok(Some(OpenAiResponsesReasoning {
        effort: effort.to_string(),
    }))
}

pub(super) fn build_openai_responses_tools(
    request: &ModelClientRequest,
) -> Vec<OpenAiResponsesToolDefinition> {
    request
        .prepared_prompt
        .tool_definitions
        .iter()
        .map(|tool| OpenAiResponsesToolDefinition {
            tool_type: "function".to_string(),
            name: tool.name.clone(),
            description: tool.description.clone(),
            parameters: tool.input_schema.clone(),
        })
        .collect()
}

pub(super) fn build_openai_responses_tool_choice(
    request: &ModelClientRequest,
) -> Option<OpenAiResponsesToolChoice> {
    if request.prepared_prompt.tool_definitions.is_empty() {
        return None;
    }
    match &request.prepared_prompt.tool_choice {
        ModelToolChoice::None => Some(OpenAiResponsesToolChoice::Mode("none".to_string())),
        ModelToolChoice::Auto => Some(OpenAiResponsesToolChoice::Mode("auto".to_string())),
        ModelToolChoice::Required => Some(OpenAiResponsesToolChoice::Mode("required".to_string())),
        ModelToolChoice::Specific { name } => Some(OpenAiResponsesToolChoice::Function {
            tool_type: "function".to_string(),
            name: name.clone(),
        }),
    }
}

impl OpenAiResponsesStreamState {
    fn truncated_tool_calls(&self) -> Vec<TruncatedToolCall> {
        let terminal_calls = self
            .completed_response
            .as_ref()
            .and_then(|response| response.output.as_deref())
            .map(truncated_openai_responses_tool_calls)
            .unwrap_or_default();
        if !terminal_calls.is_empty() {
            return terminal_calls;
        }
        self.function_calls_by_item
            .values()
            .map(|call| {
                truncated_tool_call(
                    call.call_id.as_deref(),
                    call.name.as_deref(),
                    call.arguments.as_str(),
                )
            })
            .chain(self.tool_calls.iter().map(|call| {
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
    ) -> Result<Vec<OpenAiResponsesStreamUpdate>, ModelClientError> {
        let parsed = parse_sse_json_frame("openai-responses", chunk, attempt)?;
        let event_type = parsed
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        Ok(match event_type {
            MODEL_PROVIDER_WAITING_STREAM_EVENT_TYPE => {
                vec![OpenAiResponsesStreamUpdate::ProcessStatus {
                    message: None,
                    process_state: RuntimeProcessState::ProviderWaiting,
                }]
            }
            "response.created" => {
                self.provider_request_id = parsed
                    .get("response")
                    .and_then(|response| response.get("id"))
                    .and_then(Value::as_str)
                    .map(ToString::to_string);
                vec![OpenAiResponsesStreamUpdate::Status {
                    message: None,
                    process_state: RuntimeProcessState::Thinking,
                }]
            }
            "response.output_text.delta" => {
                let delta = parsed
                    .get("delta")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if delta.is_empty() {
                    vec![]
                } else {
                    self.accumulated_output_text.push_str(delta);
                    vec![OpenAiResponsesStreamUpdate::Token {
                        content: delta.to_string(),
                    }]
                }
            }
            "response.output_item.added" => {
                if let Some(item) = parsed.get("item").and_then(Value::as_object) {
                    if item.get("type").and_then(Value::as_str) == Some("function_call") {
                        let item_id = item
                            .get("id")
                            .and_then(Value::as_str)
                            .map(ToString::to_string)
                            .unwrap_or_else(|| {
                                item.get("call_id")
                                    .and_then(Value::as_str)
                                    .map(ToString::to_string)
                                    .unwrap_or_else(|| {
                                        format!(
                                            "missing-item-id:{}",
                                            self.function_calls_by_item.len() + 1
                                        )
                                    })
                            });
                        let entry = self
                            .function_calls_by_item
                            .entry(item_id.clone())
                            .or_default();
                        entry.call_id = item
                            .get("call_id")
                            .and_then(Value::as_str)
                            .map(ToString::to_string)
                            .or_else(|| entry.call_id.clone());
                        entry.name = item
                            .get("name")
                            .and_then(Value::as_str)
                            .map(ToString::to_string)
                            .or_else(|| entry.name.clone());
                        if let Some(arguments) = item.get("arguments") {
                            entry.arguments = normalize_tool_arguments_value(arguments);
                        }
                        if !entry.preparing_emitted {
                            if let Some(name) = entry
                                .name
                                .as_deref()
                                .filter(|name| is_bounded_canonical_tool_name(name))
                            {
                                entry.preparing_emitted = true;
                                return Ok(vec![OpenAiResponsesStreamUpdate::ToolCallPreparing {
                                    name: name.to_string(),
                                }]);
                            }
                        }
                        return Ok(vec![]);
                    }
                }
                vec![]
            }
            "response.function_call_arguments.delta" => {
                let item_id = parsed
                    .get("item_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if item_id.is_empty() {
                    return Ok(vec![]);
                }
                let delta = parsed
                    .get("delta")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let entry = self
                    .function_calls_by_item
                    .entry(item_id.to_string())
                    .or_default();
                entry.arguments.push_str(delta);
                vec![]
            }
            "response.function_call_arguments.done" | "response.output_item.done" => {
                if let Some(item) = parsed.get("item").and_then(Value::as_object) {
                    if item.get("type").and_then(Value::as_str) == Some("function_call") {
                        let item_id = item
                            .get("id")
                            .and_then(Value::as_str)
                            .map(ToString::to_string)
                            .unwrap_or_default();
                        let mut call_state = self
                            .function_calls_by_item
                            .remove(item_id.as_str())
                            .unwrap_or_default();
                        let preparing_emitted = call_state.preparing_emitted;
                        call_state.name = item
                            .get("name")
                            .and_then(Value::as_str)
                            .map(ToString::to_string)
                            .or(call_state.name);
                        call_state.call_id = item
                            .get("call_id")
                            .and_then(Value::as_str)
                            .map(ToString::to_string)
                            .or(call_state.call_id);
                        let args_json = normalize_stream_tool_arguments(
                            call_state.arguments,
                            item.get("arguments"),
                        );
                        call_state.arguments = args_json.clone();
                        let Ok((call_id, name)) = required_provider_tool_call_identity(
                            "openai-responses",
                            call_state.call_id.as_deref(),
                            call_state.name.as_deref(),
                        ) else {
                            self.function_calls_by_item.insert(item_id, call_state);
                            return Ok(vec![]);
                        };
                        self.tool_calls.push(ToolCallEnvelope {
                            id: call_id.clone(),
                            name: name.clone(),
                            args_json: args_json.clone(),
                        });
                        let args_preview = preview_tool_arguments_json(args_json.as_str());
                        let should_emit_preparing =
                            !preparing_emitted && is_bounded_canonical_tool_name(name.as_str());
                        let mut updates =
                            Vec::with_capacity(if should_emit_preparing { 2 } else { 1 });
                        if should_emit_preparing {
                            updates.push(OpenAiResponsesStreamUpdate::ToolCallPreparing {
                                name: name.clone(),
                            });
                        }
                        updates.push(OpenAiResponsesStreamUpdate::ToolCallReady {
                            call_id,
                            provider_item_id: if item_id.is_empty() {
                                None
                            } else {
                                Some(item_id)
                            },
                            name,
                            args_json,
                            args_preview,
                        });
                        return Ok(updates);
                    }
                }
                vec![]
            }
            "response.completed" => {
                if let Some(response) = parsed.get("response").cloned() {
                    if let Ok(completed) =
                        serde_json::from_value::<OpenAiResponsesResponse>(response)
                    {
                        self.completed_response = Some(completed);
                    }
                }
                vec![OpenAiResponsesStreamUpdate::Done {
                    finish_reason: Some("completed".to_string()),
                }]
            }
            "response.incomplete" => {
                let response = parsed.get("response").cloned();
                let finish_reason = response
                    .as_ref()
                    .and_then(|response| response.get("incomplete_details"))
                    .and_then(|details| details.get("reason"))
                    .and_then(Value::as_str)
                    .unwrap_or("incomplete")
                    .to_string();
                if let Some(response) = response {
                    if let Ok(incomplete) =
                        serde_json::from_value::<OpenAiResponsesResponse>(response)
                    {
                        self.completed_response = Some(incomplete);
                    }
                }
                vec![OpenAiResponsesStreamUpdate::Done {
                    finish_reason: Some(finish_reason),
                }]
            }
            _ => vec![],
        })
    }

    fn validate_terminal_tool_calls(&self) -> Result<(), ModelClientError> {
        if !self.function_calls_by_item.is_empty() {
            return Err(invalid_provider_tool_call_identity("openai-responses"));
        }
        let tool_calls = match self.completed_response.as_ref() {
            Some(response) => extract_openai_responses_tool_calls(response.output.as_deref())?,
            None => self.tool_calls.clone(),
        };
        validate_retryable_provider_tool_call_arguments("openai-responses", tool_calls.as_slice())
    }

    fn into_response(self) -> OpenAiResponsesResponse {
        if let Some(response) = self.completed_response {
            return response;
        }
        let output = if self.tool_calls.is_empty() && self.accumulated_output_text.trim().is_empty()
        {
            None
        } else {
            let mut items = vec![];
            if !self.accumulated_output_text.trim().is_empty() {
                items.push(json!({
                    "type": "message",
                    "content": [
                        {
                            "type": "output_text",
                            "text": self.accumulated_output_text,
                        }
                    ]
                }));
            }
            for tool_call in self.tool_calls {
                items.push(json!({
                    "type": "function_call",
                    "call_id": tool_call.id,
                    "name": tool_call.name,
                    "arguments": tool_call.args_json,
                }));
            }
            Some(items)
        };
        OpenAiResponsesResponse {
            id: self.provider_request_id,
            status: Some("completed".to_string()),
            incomplete_details: None,
            output_text: None,
            output,
            usage: self.usage,
        }
    }
}

fn reject_openai_responses_finish_reason(
    finish_reason: Option<&str>,
) -> Result<(), ModelClientError> {
    match finish_reason {
        Some("max_output_tokens") => Err(ModelClientError {
            kind: ModelClientErrorKind::ProviderResponseInterrupted,
            message: "provider stopped generation at the output-token limit before producing a complete response".to_string(),
            retryable: false,
            provider_code: Some("incomplete_output_token_limit".to_string()),
            provider_attempts: 0,
            truncated_tool_calls: Vec::new(),
        }),
        Some("incomplete") => Err(ModelClientError {
            kind: ModelClientErrorKind::ProviderResponseInterrupted,
            message: "openai-responses provider returned an incomplete response".to_string(),
            retryable: false,
            provider_code: Some("incomplete_response".to_string()),
            provider_attempts: 0,
            truncated_tool_calls: Vec::new(),
        }),
        _ => Ok(()),
    }
}

pub(super) fn extract_openai_responses_text(
    output_text: Option<String>,
    output: Option<&[Value]>,
) -> String {
    if let Some(text) = output_text
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return text.to_string();
    }
    let mut segments = vec![];
    for item in output.unwrap_or(&[]) {
        let Some(map) = item.as_object() else {
            continue;
        };
        let item_type = map.get("type").and_then(Value::as_str).unwrap_or_default();
        if item_type == "message" {
            if let Some(content_items) = map.get("content").and_then(Value::as_array) {
                for content_item in content_items {
                    if let Some(text) = extract_openai_responses_content_text(content_item) {
                        segments.push(text);
                    }
                }
            }
            continue;
        }
        if let Some(text) = extract_openai_responses_content_text(item) {
            segments.push(text);
        }
    }
    segments.join("\n")
}

pub(super) fn extract_openai_responses_reasoning(output: Option<&[Value]>) -> Option<String> {
    let mut segments = vec![];
    for item in output.unwrap_or(&[]) {
        let Some(map) = item.as_object() else {
            continue;
        };
        if map.get("type").and_then(Value::as_str) != Some("reasoning") {
            continue;
        }
        if let Some(summary_items) = map.get("summary").and_then(Value::as_array) {
            for summary_item in summary_items {
                if let Some(text) = summary_item.get("text").and_then(Value::as_str) {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        segments.push(trimmed.to_string());
                    }
                }
            }
        } else if let Some(text) = map.get("text").and_then(Value::as_str) {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                segments.push(trimmed.to_string());
            }
        }
    }
    if segments.is_empty() {
        None
    } else {
        Some(segments.join("\n"))
    }
}

pub(super) fn extract_openai_responses_tool_calls(
    output: Option<&[Value]>,
) -> Result<Vec<ToolCallEnvelope>, ModelClientError> {
    let mut tool_calls = vec![];
    for item in output.unwrap_or(&[]) {
        let Some(map) = item.as_object() else {
            continue;
        };
        if map.get("type").and_then(Value::as_str) != Some("function_call") {
            continue;
        }
        let (id, name) = required_provider_tool_call_identity(
            "openai-responses",
            map.get("call_id").and_then(Value::as_str),
            map.get("name").and_then(Value::as_str),
        )?;
        let args_json = map
            .get("arguments")
            .map(normalize_tool_arguments_value)
            .unwrap_or_else(|| "{}".to_string());
        tool_calls.push(ToolCallEnvelope {
            id,
            name,
            args_json,
        });
    }
    Ok(tool_calls)
}

fn truncated_openai_responses_tool_calls(output: &[Value]) -> Vec<TruncatedToolCall> {
    output
        .iter()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("function_call"))
        .map(|item| {
            let args = item
                .get("arguments")
                .map(normalize_tool_arguments_value)
                .unwrap_or_default();
            truncated_tool_call(
                item.get("call_id").and_then(Value::as_str),
                item.get("name").and_then(Value::as_str),
                args.as_str(),
            )
        })
        .collect()
}

pub(super) fn extract_openai_responses_content_text(item: &Value) -> Option<String> {
    match item {
        Value::String(text) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        }
        Value::Object(map) => {
            let item_type = map.get("type").and_then(Value::as_str).unwrap_or_default();
            if matches!(item_type, "output_text" | "input_text" | "text") {
                return map
                    .get("text")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToString::to_string);
            }
            if item_type == "refusal" {
                return map
                    .get("refusal")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToString::to_string);
            }
            map.get("text")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string)
        }
        _ => None,
    }
}

pub(super) fn map_openai_responses_http_error(
    status_code: u16,
    body_json: &str,
) -> ModelClientError {
    let parsed = serde_json::from_str::<OpenAiResponsesErrorEnvelope>(body_json).ok();
    let provider_code = parsed
        .as_ref()
        .and_then(|item| item.error.as_ref())
        .and_then(|error| error.code.clone().or_else(|| error.error_type.clone()));
    let message = parsed
        .as_ref()
        .and_then(|item| item.error.as_ref())
        .and_then(|error| error.message.clone())
        .unwrap_or_else(|| format!("openai-responses request failed with status {status_code}"));
    let kind = match status_code {
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
