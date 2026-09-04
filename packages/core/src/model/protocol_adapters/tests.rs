use super::openai_responses::build_openai_responses_reasoning;
use super::{
    built_in_model_profile, built_in_model_profiles, built_in_model_providers,
    is_bounded_canonical_tool_name, AnthropicMessagesModelClient, AuthSpec, JsonHttpFuture,
    JsonHttpRequest, JsonHttpResponse, JsonHttpTransport, ModelClient, ModelProviderInfo,
    ModelProviderKind, ModelProviderRegistry, ModelSessionConfig, OpenAiCompatibleModelClient,
    OpenAiResponsesModelClient, WireApi,
};
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use crate::model::prepared_prompt::{
    project_session_messages_to_model_messages, ModelInputImageV1, ModelMessageV1, PreparedPromptV1,
};

#[test]
fn provider_adapters_preserve_inline_image_order() {
    let mut request = build_model_request("custom.openai_compatible");
    request.prepared_prompt.messages[0].content = "before [Image #1] after".to_string();
    request
        .prepared_prompt
        .set_input_images(vec![ModelInputImageV1 {
            message_id: "msg-1".to_string(),
            content_type: "image/png".to_string(),
            placeholder: "[Image #1]".to_string(),
            data_base64: "aW1hZ2U=".to_string(),
        }])
        .expect("inline image prompt");

    let openai = super::build_openai_compatible_messages(&request).expect("openai messages");
    assert_eq!(
        openai[1].content,
        json!([
            {"type": "text", "text": "before "},
            {"type": "image_url", "image_url": {"url": "data:image/png;base64,aW1hZ2U="}},
            {"type": "text", "text": " after"}
        ])
    );

    let responses = super::build_openai_responses_input(&request).expect("responses input");
    assert_eq!(
        serde_json::to_value(&responses).expect("responses json")[0]["content"],
        json!([
            {"type": "input_text", "text": "before "},
            {"type": "input_image", "image_url": "data:image/png;base64,aW1hZ2U="},
            {"type": "input_text", "text": " after"}
        ])
    );

    let (_, anthropic) =
        super::anthropic_messages::build_anthropic_messages(&request).expect("anthropic input");
    assert_eq!(
        serde_json::to_value(&anthropic).expect("anthropic json")[0]["content"],
        json!([
            {"type": "text", "text": "before "},
            {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "aW1hZ2U="}},
            {"type": "text", "text": " after"}
        ])
    );
}
use crate::model::{
    ModelClientError, ModelClientErrorKind, ModelClientRequest, ModelClientStreamEvent,
    DEFAULT_MODEL_MAX_RETRIES, MODEL_PROVIDER_WAITING_STREAM_EVENT_TYPE,
};
use crate::runtime::contracts::RuntimeProcessState;
use crate::session::state::{
    ChatMessage, MessageRole, ModelMessageSemanticsV1, ModelToolCallStateV1, SessionStateSnapshot,
};
use crate::tool::{ModelToolChoice, ModelToolDefinition};
use serde_json::{json, Value};

#[test]
fn preparing_tool_name_requires_bounded_lower_snake_case() {
    assert!(is_bounded_canonical_tool_name("web_search"));
    assert!(!is_bounded_canonical_tool_name(" read "));
    assert!(!is_bounded_canonical_tool_name("write__file"));
    assert!(!is_bounded_canonical_tool_name(&"a".repeat(129)));
}

#[tokio::test]
async fn built_in_provider_registry_contains_expected_defaults() {
    let providers = built_in_model_providers();
    let openai_provider = providers
        .get("openai.default")
        .expect("openai provider should exist");
    assert_eq!(
        openai_provider.capability_profile.max_context_tokens,
        Some(272_000)
    );
    assert!(providers.contains_key("anthropic.default"));
    let kimi_provider = providers
        .get("kimi.default")
        .expect("kimi provider should exist");
    assert_eq!(
        kimi_provider.base_url.as_deref(),
        Some("https://api.moonshot.cn/v1")
    );
    assert_eq!(
        kimi_provider.auth,
        AuthSpec::BearerEnv {
            env_key: "KIMI_API_KEY".to_string(),
        }
    );
    let deepseek_provider = providers
        .get("deepseek.default")
        .expect("deepseek provider should exist");
    assert_eq!(deepseek_provider.provider_kind, ModelProviderKind::DeepSeek);
    assert_eq!(deepseek_provider.wire_api, WireApi::OpenAiChatCompletions);
    assert_eq!(
        deepseek_provider.base_url.as_deref(),
        Some("https://api.deepseek.com")
    );
    assert!(providers.contains_key("custom.openai_compatible"));
    assert!(providers.contains_key("local.default"));
}

#[test]
fn built_in_model_profiles_are_canonical_and_bounded() {
    let profiles = built_in_model_profiles();
    assert_eq!(
        profiles.len(),
        centaeris_model_catalog::model_catalog()
            .providers
            .iter()
            .map(|provider| provider.models.len())
            .sum::<usize>()
    );
    let deepseek = built_in_model_profile("deepseek.default", "deepseek-v4-pro")
        .expect("DeepSeek V4 Pro profile");
    assert_eq!(deepseek.context_tokens, 1_000_000);
    assert_eq!(deepseek.max_output_tokens, 384_000);
    assert_eq!(deepseek.thinking_mode.as_deref(), Some("high"));
    assert_eq!(deepseek.thinking_modes, ["high", "max"]);
    let kimi = built_in_model_profile("kimi.default", "kimi-k3").expect("Kimi K3 profile");
    assert_eq!(kimi.context_tokens, 1_048_576);
    assert_eq!(kimi.max_output_tokens, 131_072);
    assert_eq!(kimi.thinking_mode.as_deref(), Some("high"));
    assert!(built_in_model_profile("deepseek.default", "deepseek-v4-fast").is_none());
    assert!(built_in_model_profile("deepseek.default", "DEEPSEEK-V4-PRO").is_none());
}

#[tokio::test]
async fn model_provider_registry_prefers_user_defined_provider() {
    let custom_provider = ModelProviderInfo {
        provider_key: "openai.default".to_string(),
        name: "Override OpenAI".to_string(),
        provider_kind: ModelProviderKind::Custom,
        base_url: Some("http://localhost:4000/v1".to_string()),
        wire_api: WireApi::OpenAiResponses,
        auth: AuthSpec::None,
        http_headers: HashMap::new(),
        env_http_headers: HashMap::new(),
        default_timeout_ms: Some(30_000),
        default_max_retries: Some(1),
        default_retry_backoff_ms: Some(200),
        capability_profile: Default::default(),
        metadata: HashMap::new(),
    };
    let registry = ModelProviderRegistry::new().with_user_defined(HashMap::from([(
        custom_provider.provider_key.clone(),
        custom_provider.clone(),
    )]));

    let resolved = registry
        .resolve("openai.default")
        .expect("user-defined provider should resolve");
    assert_eq!(resolved.info.name, "Override OpenAI");
    assert_eq!(
        resolved.info.base_url.as_deref(),
        Some("http://localhost:4000/v1")
    );
}

#[tokio::test]
async fn resolve_session_config_uses_provider_defaults_when_session_is_zeroed() {
    let registry = ModelProviderRegistry::new();
    let session_config = ModelSessionConfig {
        provider_kind: ModelProviderKind::OpenAi,
        provider_id: "openai.default".to_string(),
        model: "gpt-5.4-mini".to_string(),
        api_base: None,
        timeout_ms: 0,
        max_retries: 0,
        retry_backoff_ms: 0,
        ..Default::default()
    };

    let resolved = registry
        .resolve_session_config(&session_config)
        .expect("session config should resolve");

    assert_eq!(
        resolved.effective_api_base.as_deref(),
        Some("https://api.openai.com/v1")
    );
    assert_eq!(resolved.effective_timeout_ms, 60_000);
    assert_eq!(resolved.effective_max_retries, DEFAULT_MODEL_MAX_RETRIES);
    assert_eq!(resolved.effective_retry_backoff_ms, 600);
}

#[derive(Debug)]
struct MockJsonHttpTransport {
    requests: Mutex<Vec<JsonHttpRequest>>,
    next_responses: Mutex<VecDeque<Result<JsonHttpResponse, String>>>,
    sse_chunks: Mutex<VecDeque<Vec<String>>>,
}

impl MockJsonHttpTransport {
    fn with_response(response: Result<JsonHttpResponse, String>) -> Self {
        Self {
            requests: Mutex::new(vec![]),
            next_responses: Mutex::new(VecDeque::from([response])),
            sse_chunks: Mutex::new(VecDeque::new()),
        }
    }

    fn with_responses(responses: Vec<Result<JsonHttpResponse, String>>) -> Self {
        Self {
            requests: Mutex::new(vec![]),
            next_responses: Mutex::new(VecDeque::from(responses)),
            sse_chunks: Mutex::new(VecDeque::new()),
        }
    }

    fn with_sse(response: Result<JsonHttpResponse, String>, chunks: Vec<&str>) -> Self {
        Self {
            requests: Mutex::new(vec![]),
            next_responses: Mutex::new(VecDeque::from([response])),
            sse_chunks: Mutex::new(VecDeque::from([chunks
                .into_iter()
                .map(ToString::to_string)
                .collect()])),
        }
    }

    fn with_sse_response_chunks(
        responses: Vec<Result<JsonHttpResponse, String>>,
        chunks_by_response: Vec<Vec<String>>,
    ) -> Self {
        Self {
            requests: Mutex::new(vec![]),
            next_responses: Mutex::new(VecDeque::from(responses)),
            sse_chunks: Mutex::new(VecDeque::from(chunks_by_response)),
        }
    }

    fn take_requests(&self) -> Vec<JsonHttpRequest> {
        self.requests.lock().expect("requests lock").clone()
    }
}

impl JsonHttpTransport for MockJsonHttpTransport {
    fn execute_json<'a>(&'a self, request: &'a JsonHttpRequest) -> JsonHttpFuture<'a> {
        Box::pin(async move {
            self.requests
                .lock()
                .expect("requests lock")
                .push(request.clone());
            self.next_responses
                .lock()
                .expect("response lock")
                .pop_front()
                .expect("mock response should exist")
        })
    }

    fn execute_sse<'a>(
        &'a self,
        request: &'a JsonHttpRequest,
        on_data: &'a mut (dyn FnMut(String) + Send),
    ) -> JsonHttpFuture<'a> {
        Box::pin(async move {
            self.requests
                .lock()
                .expect("requests lock")
                .push(request.clone());
            let chunks = self
                .sse_chunks
                .lock()
                .expect("sse chunks lock")
                .pop_front()
                .unwrap_or_default();
            for chunk in chunks.iter() {
                on_data(chunk.clone());
            }
            self.next_responses
                .lock()
                .expect("response lock")
                .pop_front()
                .expect("mock response should exist")
        })
    }
}

impl JsonHttpTransport for &MockJsonHttpTransport {
    fn execute_json<'a>(&'a self, request: &'a JsonHttpRequest) -> JsonHttpFuture<'a> {
        (**self).execute_json(request)
    }

    fn execute_sse<'a>(
        &'a self,
        request: &'a JsonHttpRequest,
        on_data: &'a mut (dyn FnMut(String) + Send),
    ) -> JsonHttpFuture<'a> {
        (**self).execute_sse(request, on_data)
    }
}

fn test_model_messages(messages: Vec<ChatMessage>) -> Vec<ModelMessageV1> {
    let mut session = SessionStateSnapshot::new("chat-model-test".to_string(), 0);
    for message in &messages {
        let semantics = message
            .metadata
            .get("test_model_semantics_json")
            .map(|raw| {
                serde_json::from_str::<ModelMessageSemanticsV1>(raw)
                    .expect("test model semantics must decode")
            })
            .unwrap_or(ModelMessageSemanticsV1::Plain);
        session
            .model_semantics
            .insert(message.message_id.clone(), semantics);
    }
    session.messages = messages;
    project_session_messages_to_model_messages(&session, session.messages.as_slice())
        .expect("test messages must project to model messages")
}

fn build_model_request(provider_id: &str) -> ModelClientRequest {
    ModelClientRequest {
        session_id: "chat-model".to_string(),
        turn_id: "turn-model".to_string(),
        loop_index: 0,
        provider_prompt_cache_key: None,
        provider_prompt_cache_retention: None,
        system_prompt_manifest_json: None,
        compression_stats_json: None,
        context_token_estimate: 32,
        prepared_prompt: PreparedPromptV1::new(
            Some("You are Centaeris.".to_string()),
            test_model_messages(vec![ChatMessage {
                message_id: "msg-1".to_string(),
                role: MessageRole::User,
                content: "Summarize the change.".to_string(),
                created_at_ms: 1,
                metadata: HashMap::new(),
            }]),
            vec![ModelToolDefinition {
                name: "read".to_string(),
                description: "Read file content for grounded reasoning.".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" }
                    },
                    "required": ["path"],
                    "additionalProperties": true
                }),
            }],
            ModelToolChoice::Auto,
            512,
        )
        .expect("test prepared prompt"),
        session_config: ModelSessionConfig {
            provider_kind: ModelProviderKind::Custom,
            provider_id: provider_id.to_string(),
            model: "gpt-4o-mini".to_string(),
            api_base: Some("http://localhost:4000/v1".to_string()),
            timeout_ms: 30_000,
            max_retries: 1,
            retry_backoff_ms: 200,
            max_output_tokens: Some(512),
            thinking_mode: None,
            metadata: HashMap::new(),
        },
    }
}

fn assert_output_limit_tool_identity(
    error: &ModelClientError,
    call_id: Option<&str>,
    tool_name: Option<&str>,
    raw_args: &str,
) {
    assert_eq!(
        error.kind,
        ModelClientErrorKind::ProviderResponseInterrupted
    );
    assert_eq!(
        error.provider_code.as_deref(),
        Some("incomplete_output_token_limit")
    );
    assert!(!error.retryable);
    assert_eq!(error.provider_attempts, 1);
    assert_eq!(error.truncated_tool_calls.len(), 1);
    assert_eq!(error.truncated_tool_calls[0].call_id.as_deref(), call_id);
    assert_eq!(
        error.truncated_tool_calls[0].tool_name.as_deref(),
        tool_name
    );
    assert_eq!(error.truncated_tool_calls[0].args_bytes, raw_args.len());
    assert!(error.truncated_tool_calls[0]
        .args_sha256
        .starts_with("sha256:"));
}

fn deepseek_test_registry() -> ModelProviderRegistry {
    let mut provider = built_in_model_providers()
        .remove("deepseek.default")
        .expect("DeepSeek provider");
    provider.auth = AuthSpec::None;
    ModelProviderRegistry::new()
        .with_user_defined(HashMap::from([(provider.provider_key.clone(), provider)]))
}

fn kimi_test_registry() -> ModelProviderRegistry {
    let mut built_ins = built_in_model_providers();
    let providers = ["kimi.default", "kimi-code.default"]
        .into_iter()
        .map(|provider_id| {
            let mut provider = built_ins.remove(provider_id).expect("Kimi provider");
            provider.auth = AuthSpec::None;
            (provider.provider_key.clone(), provider)
        })
        .collect();
    ModelProviderRegistry::new().with_user_defined(providers)
}

fn build_kimi_request(model: &str, thinking_mode: Option<&str>) -> ModelClientRequest {
    let provider_id = if matches!(model, "k3" | "kimi-for-coding") {
        "kimi-code.default"
    } else {
        "kimi.default"
    };
    let mut request = build_model_request(provider_id);
    request.session_config.provider_kind = ModelProviderKind::Kimi;
    request.session_config.model = model.to_string();
    request.session_config.max_output_tokens = Some(131_072);
    request.session_config.thinking_mode = thinking_mode.map(str::to_string);
    request.prepared_prompt.max_output_tokens = 131_072;
    request
}

fn build_deepseek_request(thinking_mode: Option<&str>) -> ModelClientRequest {
    let mut request = build_model_request("deepseek.default");
    request.session_config.provider_kind = ModelProviderKind::DeepSeek;
    request.session_config.model = "deepseek-v4-pro".to_string();
    request.session_config.thinking_mode = thinking_mode.map(str::to_string);
    request
}

fn anthropic_test_registry() -> ModelProviderRegistry {
    let provider = ModelProviderInfo {
        provider_key: "anthropic.test".to_string(),
        name: "Anthropic Test".to_string(),
        provider_kind: ModelProviderKind::Anthropic,
        base_url: Some("http://localhost:4000/v1".to_string()),
        wire_api: WireApi::AnthropicMessages,
        auth: AuthSpec::StaticHeader {
            header_name: "x-api-key".to_string(),
            value: "test-key".to_string(),
        },
        http_headers: HashMap::from([("anthropic-version".to_string(), "2023-06-01".to_string())]),
        env_http_headers: HashMap::new(),
        default_timeout_ms: Some(30_000),
        default_max_retries: Some(1),
        default_retry_backoff_ms: Some(200),
        capability_profile: Default::default(),
        metadata: HashMap::new(),
    };
    ModelProviderRegistry::new()
        .with_user_defined(HashMap::from([(provider.provider_key.clone(), provider)]))
}

fn build_anthropic_request() -> ModelClientRequest {
    let mut request = build_model_request("anthropic.test");
    request.session_config.provider_kind = ModelProviderKind::Anthropic;
    request.session_config.model = "claude-test".to_string();
    request
}

#[tokio::test]
async fn anthropic_messages_projects_request_and_parses_tool_result() {
    let transport = MockJsonHttpTransport::with_response(Ok(JsonHttpResponse {
        status_code: 200,
        headers: HashMap::from([("request-id".to_string(), "req-anthropic".to_string())]),
        body_json: json!({
            "id": "msg-anthropic",
            "content": [
                { "type": "text", "text": "I will read it." },
                {
                    "type": "tool_use",
                    "id": "tool-1",
                    "name": "read",
                    "input": { "path": "README.md" }
                }
            ],
            "stop_reason": "tool_use",
            "usage": {
                "input_tokens": 12,
                "output_tokens": 6,
                "cache_read_input_tokens": 3
            }
        })
        .to_string(),
    }));
    let client = AnthropicMessagesModelClient::new(anthropic_test_registry(), &transport);

    let response = client
        .generate(&build_anthropic_request())
        .await
        .expect("anthropic message response");

    assert_eq!(response.generate_result.content, "I will read it.");
    assert_eq!(response.generate_result.tool_calls.len(), 1);
    assert_eq!(response.generate_result.tool_calls[0].id, "tool-1");
    assert_eq!(
        response.generate_result.tool_calls[0].args_json,
        r#"{"path":"README.md"}"#
    );
    assert_eq!(response.generate_result.input_tokens, Some(12));
    assert_eq!(response.generate_result.total_tokens, Some(18));
    assert_eq!(response.generate_result.prompt_cache_hit_tokens, Some(3));
    assert_eq!(response.generate_result.prompt_cache_miss_tokens, Some(9));

    let requests = transport.take_requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].url, "http://localhost:4000/v1/messages");
    assert_eq!(
        requests[0].headers.get("x-api-key"),
        Some(&"test-key".to_string())
    );
    assert_eq!(
        requests[0].headers.get("anthropic-version"),
        Some(&"2023-06-01".to_string())
    );
    let body: Value =
        serde_json::from_str(requests[0].body_json.as_str()).expect("anthropic request JSON");
    assert_eq!(body["system"], "You are Centaeris.");
    assert_eq!(body["messages"][0]["role"], "user");
    assert_eq!(body["tools"][0]["input_schema"]["type"], "object");
    assert_eq!(body["tool_choice"]["type"], "auto");
    assert!(body.get("output_config").is_none());
    assert_eq!(body["thinking"]["type"], "adaptive");
}

#[tokio::test]
async fn anthropic_messages_rejects_unknown_effort_before_transport() {
    let transport = MockJsonHttpTransport::with_response(Err("must not send".to_string()));
    let client = AnthropicMessagesModelClient::new(anthropic_test_registry(), &transport);
    let mut request = build_anthropic_request();
    request.session_config.thinking_mode = Some("banana".to_string());

    let error = client
        .generate(&request)
        .await
        .expect_err("unknown effort must fail");

    assert_eq!(error.kind, ModelClientErrorKind::InvalidRequest);
    assert!(transport.take_requests().is_empty());
}

#[tokio::test]
async fn anthropic_output_limit_preserves_only_tool_identity_and_argument_diagnostics() {
    let raw_args = r#"{"path":"unfinished"#;
    let transport = MockJsonHttpTransport::with_response(Ok(JsonHttpResponse {
        status_code: 200,
        headers: HashMap::new(),
        body_json: json!({
            "id": "msg-capped",
            "content": [{
                "type": "tool_use",
                "name": "read",
                "input": raw_args,
            }],
            "stop_reason": "max_tokens",
        })
        .to_string(),
    }));
    let client = AnthropicMessagesModelClient::new(anthropic_test_registry(), &transport);

    let error = client
        .generate(&build_anthropic_request())
        .await
        .expect_err("capped Anthropic response must enter runtime recovery");

    assert_eq!(error.truncated_tool_calls.len(), 1);
    assert_eq!(error.truncated_tool_calls[0].call_id, None);
    assert_eq!(
        error.truncated_tool_calls[0].tool_name.as_deref(),
        Some("read")
    );
    assert_eq!(error.truncated_tool_calls[0].args_bytes, raw_args.len());
    assert!(error.truncated_tool_calls[0]
        .args_sha256
        .starts_with("sha256:"));
}

#[tokio::test]
async fn anthropic_non_stream_output_limit_preserves_complete_tool_identity() {
    let raw_args = r#"{"path":"unfinished"#;
    let transport = MockJsonHttpTransport::with_response(Ok(JsonHttpResponse {
        status_code: 200,
        headers: HashMap::new(),
        body_json: json!({
            "id": "msg-capped",
            "content": [{
                "type": "tool_use",
                "id": "call-capped",
                "name": "read",
                "input": raw_args,
            }],
            "stop_reason": "max_tokens",
        })
        .to_string(),
    }));
    let client = AnthropicMessagesModelClient::new(anthropic_test_registry(), &transport);

    let error = client
        .generate(&build_anthropic_request())
        .await
        .expect_err("capped Anthropic response must enter runtime recovery");

    assert_output_limit_tool_identity(&error, Some("call-capped"), Some("read"), raw_args);
}

#[tokio::test]
async fn anthropic_stream_output_limit_preserves_complete_tool_identity_without_ready() {
    let raw_args = r#"{"path":"unfinished"#;
    let transport = MockJsonHttpTransport::with_sse(
        Ok(JsonHttpResponse {
            status_code: 200,
            headers: HashMap::new(),
            body_json: String::new(),
        }),
        vec![
            r#"{"type":"message_start","message":{"id":"msg-capped"}}"#,
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"partial"}}"#,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"call-capped","name":"read","input":{}}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"unfinished"}}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"max_tokens"}}"#,
            r#"{"type":"message_stop"}"#,
        ],
    );
    let client = AnthropicMessagesModelClient::new(anthropic_test_registry(), &transport);
    let mut events = Vec::new();

    let error = client
        .generate_stream(&build_anthropic_request(), &mut |event| events.push(event))
        .await
        .expect_err("capped Anthropic stream must enter runtime recovery");

    assert_output_limit_tool_identity(&error, Some("call-capped"), Some("read"), raw_args);
    assert!(!events.iter().any(|event| matches!(
        event,
        ModelClientStreamEvent::ToolCallReady { .. } | ModelClientStreamEvent::Done { .. }
    )));
}

#[tokio::test]
async fn anthropic_stream_output_limit_does_not_synthesize_tool_identity() {
    let raw_args = r#"{"path":"unfinished"#;
    let transport = MockJsonHttpTransport::with_sse(
        Ok(JsonHttpResponse {
            status_code: 200,
            headers: HashMap::new(),
            body_json: String::new(),
        }),
        vec![
            r#"{"type":"message_start","message":{"id":"msg-capped"}}"#,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","input":{}}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"unfinished"}}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"max_tokens"}}"#,
            r#"{"type":"message_stop"}"#,
        ],
    );
    let client = AnthropicMessagesModelClient::new(anthropic_test_registry(), &transport);
    let mut events = Vec::new();

    let error = client
        .generate_stream(&build_anthropic_request(), &mut |event| events.push(event))
        .await
        .expect_err("capped Anthropic stream must enter runtime recovery");

    assert_output_limit_tool_identity(&error, None, None, raw_args);
    assert!(!events.iter().any(|event| matches!(
        event,
        ModelClientStreamEvent::ToolCallReady { .. } | ModelClientStreamEvent::Done { .. }
    )));
}

#[tokio::test]
async fn anthropic_messages_stream_accumulates_tool_json_before_completion() {
    let transport = MockJsonHttpTransport::with_sse(
        Ok(JsonHttpResponse {
            status_code: 200,
            headers: HashMap::new(),
            body_json: String::new(),
        }),
        vec![
            r#"{"type":"message_start","message":{"id":"msg-stream","usage":{"input_tokens":10,"output_tokens":1}}}"#,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"tool-stream","name":"read","input":{}}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"README"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":".md\"}"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"input_tokens":10,"output_tokens":4}}"#,
            r#"{"type":"message_stop"}"#,
        ],
    );
    let client = AnthropicMessagesModelClient::new(anthropic_test_registry(), &transport);
    let mut events = Vec::new();

    let response = client
        .generate_stream(&build_anthropic_request(), &mut |event| events.push(event))
        .await
        .expect("anthropic stream response");

    assert_eq!(response.provider_request_id.as_deref(), Some("msg-stream"));
    assert_eq!(response.generate_result.tool_calls.len(), 1);
    assert_eq!(
        response.generate_result.tool_calls[0].args_json,
        r#"{"path":"README.md"}"#
    );
    let preparing_index = events
        .iter()
        .position(|event| {
            matches!(
                event,
                ModelClientStreamEvent::ToolCallPreparing { name } if name == "read"
            )
        })
        .expect("anthropic should announce the tool before its arguments complete");
    let ready_index = events
        .iter()
        .position(|event| {
            matches!(
                event,
                ModelClientStreamEvent::ToolCallReady { call_id, name, .. }
                    if call_id == "tool-stream" && name == "read"
            )
        })
        .expect("anthropic should publish the completed tool call");
    assert!(preparing_index < ready_index);
    assert!(events.iter().any(|event| matches!(
        event,
        ModelClientStreamEvent::ToolCallReady { call_id, name, .. }
            if call_id == "tool-stream" && name == "read"
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        ModelClientStreamEvent::Done { finish_reason: Some(reason) }
            if reason == "tool_use"
    )));
}

fn test_hash_json(value: &Value) -> String {
    let mut hash = 1469598103934665603u64;
    for byte in value.to_string().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(1099511628211);
    }
    format!("{hash:016x}")
}

fn common_openai_message_prefix_len(
    left: &[super::OpenAiCompatibleChatMessage],
    right: &[super::OpenAiCompatibleChatMessage],
) -> usize {
    left.iter()
        .zip(right.iter())
        .take_while(|(left, right)| left == right)
        .count()
}

fn hash_openai_message_prefix(
    messages: &[super::OpenAiCompatibleChatMessage],
    prefix_len: usize,
) -> String {
    test_hash_json(
        &serde_json::to_value(messages.iter().take(prefix_len).collect::<Vec<_>>())
            .expect("serialize message prefix"),
    )
}

fn assistant_semantics_metadata(
    reasoning_content: Option<&str>,
    tool_calls: &[(&str, &str, &str)],
) -> HashMap<String, String> {
    let mut metadata = HashMap::new();
    metadata.insert(
        "test_model_semantics_json".to_string(),
        serde_json::to_string(&ModelMessageSemanticsV1::Assistant {
            reasoning_content: reasoning_content.map(str::to_string),
            tool_calls: tool_calls
                .iter()
                .map(|(id, name, args_json)| ModelToolCallStateV1 {
                    id: (*id).to_string(),
                    name: (*name).to_string(),
                    args_json: (*args_json).to_string(),
                })
                .collect::<Vec<_>>(),
        })
        .expect("serialize assistant semantics"),
    );
    metadata
}

fn tool_result_semantics_metadata(
    tool_call_id: &str,
    tool_name: &str,
    _status: &str,
) -> HashMap<String, String> {
    let mut metadata = HashMap::new();
    metadata.insert(
        "test_model_semantics_json".to_string(),
        serde_json::to_string(&ModelMessageSemanticsV1::ToolResult {
            tool_call_id: tool_call_id.to_string(),
            tool_name: tool_name.to_string(),
            status: "ok".to_string(),
            result_state: "success_with_output".to_string(),
            error_kind: None,
            object_refs: vec![],
            transition_reason: None,
        })
        .expect("serialize tool result semantics"),
    );
    metadata
}

fn openai_provider_params_hash(request: &ModelClientRequest, stream: bool) -> String {
    test_hash_json(&serde_json::json!({
        "model": request.session_config.model,
        "stream": stream,
        "maxTokens": request.session_config.max_output_tokens,
        "toolChoice": super::build_openai_chat_tool_choice(request),
        "providerKind": request.session_config.provider_kind,
        "providerId": request.session_config.provider_id,
        "thinkingMode": request.session_config.thinking_mode,
    }))
}

#[tokio::test]
async fn openai_responses_prompt_cache_key_is_model_scoped_and_serialized() {
    let transport = MockJsonHttpTransport::with_response(Ok(JsonHttpResponse {
        status_code: 200,
        headers: HashMap::new(),
        body_json:
            "{\"id\":\"resp_cache_key\",\"output_text\":\"done\",\"usage\":{\"input_tokens\":12,\"output_tokens\":1,\"total_tokens\":13}}"
                .to_string(),
    }));
    let client = OpenAiResponsesModelClient::new(ModelProviderRegistry::new(), transport);
    std::env::set_var("OPENAI_API_KEY", "test-openai-key");

    let mut request = build_model_request("openai.default");
    request.provider_prompt_cache_key =
        Some("centaeris-provider-pcache-seed-v1:testseed".to_string());
    request.provider_prompt_cache_retention = Some("24h".to_string());

    client
        .generate(&request)
        .await
        .expect("generate should succeed");

    let requests = client.transport.take_requests();
    let body =
        serde_json::from_str::<Value>(requests[0].body_json.as_str()).expect("parse request body");
    let prompt_cache_key = body
        .get("prompt_cache_key")
        .and_then(Value::as_str)
        .expect("prompt cache key");
    assert!(prompt_cache_key.starts_with("centaeris-pcache-v1:"));
    assert_eq!(
        body.get("prompt_cache_retention").and_then(Value::as_str),
        Some("24h")
    );
    assert!(!prompt_cache_key.contains("chat-model"));
    assert!(!prompt_cache_key.contains("turn-model"));
    assert!(!prompt_cache_key.contains("Summarize"));
}

#[tokio::test]
async fn openai_compatible_chat_completions_does_not_send_responses_prompt_cache_key() {
    let transport = MockJsonHttpTransport::with_response(Ok(JsonHttpResponse {
        status_code: 200,
        headers: HashMap::new(),
        body_json: "{\"id\":\"chat_cache_key\",\"choices\":[{\"message\":{\"content\":\"ok\"}}],\"usage\":{\"prompt_tokens\":8,\"total_tokens\":9}}".to_string(),
    }));
    let client = OpenAiCompatibleModelClient::new(ModelProviderRegistry::new(), transport);
    let mut request = build_model_request("custom.openai_compatible");
    request.provider_prompt_cache_key =
        Some("centaeris-provider-pcache-seed-v1:testseed".to_string());
    request.provider_prompt_cache_retention = Some("24h".to_string());

    client
        .generate(&request)
        .await
        .expect("generate should succeed");

    let requests = client.transport.take_requests();
    let body =
        serde_json::from_str::<Value>(requests[0].body_json.as_str()).expect("parse request body");
    assert!(body.get("prompt_cache_key").is_none());
    assert!(body.get("prompt_cache_retention").is_none());
}

#[test]
fn openai_compatible_chat_messages_merge_leading_system_messages() {
    let mut request = build_model_request("custom.openai_compatible");
    request.prepared_prompt.system_prompt = Some("# Harness".to_string());
    request.prepared_prompt.messages = test_model_messages(vec![
        ChatMessage {
            message_id: "msg-compact".to_string(),
            role: MessageRole::System,
            content: "# Summary\n\nContinue from the compacted context.".to_string(),
            created_at_ms: 1,
            metadata: HashMap::new(),
        },
        ChatMessage {
            message_id: "msg-user".to_string(),
            role: MessageRole::User,
            content: "Continue.".to_string(),
            created_at_ms: 2,
            metadata: HashMap::new(),
        },
    ]);

    let messages =
        super::build_openai_compatible_messages(&request).expect("build merged messages");

    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role, "system");
    assert_eq!(
        messages[0].content,
        "# Harness\n\n# Summary\n\nContinue from the compacted context."
    );
    assert_eq!(messages[1].role, "user");
    assert_eq!(messages[1].content, "Continue.");
}

#[test]
fn openai_compatible_chat_messages_reject_system_after_conversation() {
    let mut request = build_model_request("custom.openai_compatible");
    request.prepared_prompt.messages = test_model_messages(vec![
        ChatMessage {
            message_id: "msg-user".to_string(),
            role: MessageRole::User,
            content: "Start.".to_string(),
            created_at_ms: 1,
            metadata: HashMap::new(),
        },
        ChatMessage {
            message_id: "msg-system-late".to_string(),
            role: MessageRole::System,
            content: "late system context".to_string(),
            created_at_ms: 2,
            metadata: HashMap::new(),
        },
    ]);

    let error = super::build_openai_compatible_messages(&request)
        .expect_err("system content after conversation must fail");

    assert_eq!(error.kind, ModelClientErrorKind::InvalidRequest);
    assert!(error
        .message
        .contains("system message msg-system-late appears after conversation content"));
}

#[test]
fn provider_adapters_accept_anchored_lifecycle_user_context_before_tool_continuation() {
    let mut request = build_model_request("custom.openai_compatible");
    request.prepared_prompt.messages = test_model_messages(vec![
        ChatMessage {
            message_id: "msg-old-user".to_string(),
            role: MessageRole::User,
            content: "Earlier request.".to_string(),
            created_at_ms: 1,
            metadata: HashMap::new(),
        },
        ChatMessage {
            message_id: "msg-old-assistant".to_string(),
            role: MessageRole::Assistant,
            content: "Earlier answer.".to_string(),
            created_at_ms: 2,
            metadata: HashMap::new(),
        },
        ChatMessage {
            message_id: "msg-lifecycle-context".to_string(),
            role: MessageRole::User,
            content: "[Lifecycle hook context]\nverified receipt".to_string(),
            created_at_ms: 3,
            metadata: HashMap::new(),
        },
        ChatMessage {
            message_id: "msg-current-user".to_string(),
            role: MessageRole::User,
            content: "Current request.".to_string(),
            created_at_ms: 4,
            metadata: HashMap::new(),
        },
        ChatMessage {
            message_id: "msg-assistant-tool".to_string(),
            role: MessageRole::Assistant,
            content: String::new(),
            created_at_ms: 5,
            metadata: assistant_semantics_metadata(
                None,
                &[("call-read", "read", "{\"path\":\"README.md\"}")],
            ),
        },
        ChatMessage {
            message_id: "msg-tool-result".to_string(),
            role: MessageRole::Tool,
            content: "README".to_string(),
            created_at_ms: 6,
            metadata: tool_result_semantics_metadata("call-read", "read", "ok"),
        },
    ]);

    let openai = super::build_openai_compatible_messages(&request)
        .expect("OpenAI-compatible continuation projection");
    assert_eq!(
        openai
            .iter()
            .map(|message| message.role.as_str())
            .collect::<Vec<_>>(),
        vec![
            "system",
            "user",
            "assistant",
            "user",
            "user",
            "assistant",
            "tool"
        ]
    );
    super::anthropic_messages::build_anthropic_messages(&request)
        .expect("Anthropic continuation projection");
}

#[tokio::test]
async fn deepseek_chat_messages_reuse_previous_turn_prefix() {
    let stable_system_prompt = "You are Centaeris.";
    let first_user = "What's the highest mountain in the world?";
    let first_assistant = "The highest mountain in the world is Mount Everest.";
    let second_user = "What is the second?";

    let mut round1_request = build_model_request("custom.openai_compatible");
    round1_request.prepared_prompt.system_prompt = Some(stable_system_prompt.to_string());
    round1_request.prepared_prompt.messages = test_model_messages(vec![ChatMessage {
        message_id: "msg-user-1".to_string(),
        role: MessageRole::User,
        content: first_user.to_string(),
        created_at_ms: 1,
        metadata: HashMap::new(),
    }]);

    let round1_messages =
        super::build_openai_compatible_messages(&round1_request).expect("round1 messages");

    let mut round2_request = build_model_request("custom.openai_compatible");
    round2_request.prepared_prompt.system_prompt = Some(stable_system_prompt.to_string());
    round2_request.prepared_prompt.messages = test_model_messages(vec![
        ChatMessage {
            message_id: "msg-user-1".to_string(),
            role: MessageRole::User,
            content: first_user.to_string(),
            created_at_ms: 1,
            metadata: HashMap::new(),
        },
        ChatMessage {
            message_id: "msg-assistant-1".to_string(),
            role: MessageRole::Assistant,
            content: first_assistant.to_string(),
            created_at_ms: 2,
            metadata: HashMap::new(),
        },
        ChatMessage {
            message_id: "msg-user-2".to_string(),
            role: MessageRole::User,
            content: second_user.to_string(),
            created_at_ms: 3,
            metadata: HashMap::new(),
        },
    ]);

    let round2_messages =
        super::build_openai_compatible_messages(&round2_request).expect("round2 messages");

    assert_eq!(
        &round2_messages[..round1_messages.len()],
        round1_messages.as_slice()
    );
    assert_eq!(
        serde_json::to_vec(&round2_messages[..round1_messages.len()])
            .expect("serialize round2 reused prefix"),
        serde_json::to_vec(round1_messages.as_slice()).expect("serialize round1 prefix"),
        "unchanged DeepSeek message prefixes must remain byte-identical"
    );
    assert_eq!(round1_messages[0].role, "system");
    assert_eq!(round1_messages[0].content, stable_system_prompt);
    assert_eq!(round1_messages[1].role, "user");
    assert_eq!(round1_messages[1].content, first_user);
    assert_eq!(round2_messages[2].role, "assistant");
    assert_eq!(round2_messages[2].content, first_assistant);
    assert_eq!(round2_messages[3].role, "user");
    assert_eq!(round2_messages[3].content, second_user);
}

#[test]
fn deepseek_thinking_request_uses_native_protocol_and_auto_tool_default() {
    let client = OpenAiCompatibleModelClient::new(
        deepseek_test_registry(),
        MockJsonHttpTransport::with_response(Err("unused transport".to_string())),
    );
    let high_request = build_deepseek_request(Some("high"));
    let high_http_request = client
        .build_http_request(&high_request, true)
        .expect("build DeepSeek thinking request");
    let high_body = serde_json::from_str::<Value>(high_http_request.body_json.as_str())
        .expect("parse DeepSeek request");

    assert_eq!(
        high_body.pointer("/thinking/type").and_then(Value::as_str),
        Some("enabled")
    );
    assert_eq!(
        high_body.get("reasoning_effort").and_then(Value::as_str),
        Some("high")
    );
    assert!(high_body.get("tool_choice").is_none());
    assert_eq!(
        high_body
            .get("tools")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(1)
    );

    let disabled_request = build_deepseek_request(Some("disabled"));
    let disabled_error = client
        .build_http_request(&disabled_request, false)
        .expect_err("catalog does not expose disabled DeepSeek reasoning");
    assert!(disabled_error
        .message
        .contains("expected low, high, or max"));
}

#[test]
fn deepseek_ten_tool_rounds_preserve_reasoning_content() {
    let client = OpenAiCompatibleModelClient::new(
        deepseek_test_registry(),
        MockJsonHttpTransport::with_response(Err("unused transport".to_string())),
    );
    let mut history = vec![ChatMessage {
        message_id: "msg-user-root".to_string(),
        role: MessageRole::User,
        content: "Inspect the project in ten bounded rounds.".to_string(),
        created_at_ms: 1,
        metadata: HashMap::new(),
    }];
    for round in 0..10 {
        let call_id = format!("call-round-{round}");
        let path = format!("src/round-{round}.rs");
        let args_json = json!({ "path": path }).to_string();
        history.push(ChatMessage {
            message_id: format!("msg-assistant-round-{round}"),
            role: MessageRole::Assistant,
            content: format!("collect round {round}"),
            created_at_ms: i64::from(round * 2 + 2),
            metadata: assistant_semantics_metadata(
                Some(format!("reasoning round {round}").as_str()),
                &[(call_id.as_str(), "read", args_json.as_str())],
            ),
        });
        history.push(ChatMessage {
            message_id: format!("msg-tool-round-{round}"),
            role: MessageRole::Tool,
            content: json!({ "path": path, "status": "ok" }).to_string(),
            created_at_ms: i64::from(round * 2 + 3),
            metadata: tool_result_semantics_metadata(call_id.as_str(), "read", "ok"),
        });
    }

    let mut request = build_deepseek_request(Some("high"));
    request.prepared_prompt.messages = test_model_messages(history);
    let http_request = client
        .build_http_request(&request, true)
        .expect("build ten-round DeepSeek request");
    let body = serde_json::from_str::<Value>(http_request.body_json.as_str())
        .expect("parse ten-round DeepSeek request");
    let assistant_messages = body
        .get("messages")
        .and_then(Value::as_array)
        .expect("DeepSeek messages")
        .iter()
        .filter(|message| message.get("role").and_then(Value::as_str) == Some("assistant"))
        .collect::<Vec<_>>();

    assert_eq!(assistant_messages.len(), 10);
    for (round, message) in assistant_messages.iter().enumerate() {
        assert_eq!(
            message.get("reasoning_content").and_then(Value::as_str),
            Some(format!("reasoning round {round}").as_str())
        );
    }
    assert_eq!(
        body.pointer("/thinking/type").and_then(Value::as_str),
        Some("enabled")
    );
}

#[test]
fn deepseek_thinking_request_rejects_forced_tool_choice() {
    let client = OpenAiCompatibleModelClient::new(
        deepseek_test_registry(),
        MockJsonHttpTransport::with_response(Err("unused transport".to_string())),
    );
    let mut request = build_deepseek_request(Some("max"));
    request.prepared_prompt.tool_choice = ModelToolChoice::Required;

    let error = client
        .build_http_request(&request, true)
        .expect_err("forced tool choice must fail loudly in DeepSeek thinking mode");

    assert_eq!(error.kind, ModelClientErrorKind::InvalidRequest);
    assert!(!error.retryable);
    assert!(error.message.contains("forced tool_choice"));
}

#[test]
fn deepseek_thinking_request_rejects_unknown_mode() {
    let client = OpenAiCompatibleModelClient::new(
        deepseek_test_registry(),
        MockJsonHttpTransport::with_response(Err("unused transport".to_string())),
    );
    let request = build_deepseek_request(Some("banana"));

    let error = client
        .build_http_request(&request, true)
        .expect_err("unknown thinking mode must fail loudly");

    assert_eq!(error.kind, ModelClientErrorKind::InvalidRequest);
    assert!(error.message.contains("expected low, high, or max"));
}

#[test]
fn kimi_models_use_their_native_output_and_thinking_fields() {
    let client = OpenAiCompatibleModelClient::new(
        kimi_test_registry(),
        MockJsonHttpTransport::with_response(Err("unused transport".to_string())),
    );

    let k3 = client
        .build_http_request(&build_kimi_request("kimi-k3", Some("max")), true)
        .expect("build Kimi K3 request");
    let k3_body =
        serde_json::from_str::<Value>(k3.body_json.as_str()).expect("parse Kimi K3 request");
    assert_eq!(
        k3_body.get("max_completion_tokens").and_then(Value::as_u64),
        Some(131_072)
    );
    assert!(k3_body.get("max_tokens").is_none());
    assert_eq!(
        k3_body.get("reasoning_effort").and_then(Value::as_str),
        Some("max")
    );
    assert!(k3_body.get("thinking").is_none());

    let k27 = client
        .build_http_request(
            &build_kimi_request("kimi-for-coding", Some("preserved")),
            true,
        )
        .expect("build Kimi K2.7 Code request");
    let k27_body = serde_json::from_str::<Value>(k27.body_json.as_str())
        .expect("parse Kimi K2.7 Code request");
    assert_eq!(
        k27_body.pointer("/thinking/type").and_then(Value::as_str),
        Some("enabled")
    );
    assert_eq!(
        k27_body.pointer("/thinking/keep").and_then(Value::as_str),
        Some("all")
    );
    assert!(k27_body.get("reasoning_effort").is_none());
}

#[test]
fn kimi_preserved_thinking_models_reject_forced_tool_choice() {
    let client = OpenAiCompatibleModelClient::new(
        kimi_test_registry(),
        MockJsonHttpTransport::with_response(Err("unused transport".to_string())),
    );
    let mut request = build_kimi_request("kimi-for-coding", Some("preserved"));
    request.prepared_prompt.tool_choice = ModelToolChoice::Required;

    let error = client
        .build_http_request(&request, true)
        .expect_err("Kimi K2.7 Code forced tool choice must fail loudly");
    assert_eq!(error.kind, ModelClientErrorKind::InvalidRequest);
    assert!(error
        .message
        .contains("does not support forced tool_choice"));
}

#[tokio::test]
async fn openai_compatible_chat_messages_keep_dynamic_context_after_reusable_prefix() {
    let stable_system_prompt = "You are Centaeris.";
    let first_user = "Inspect the prompt pipeline.";
    let first_assistant = "The prompt pipeline starts with a system prompt.";
    let tool_result = r#"{"path":"core/src/runtime.rs","status":"ok"}"#;
    let second_assistant = "The request builder keeps context messages in order.";
    let current_user = "Audit the cache-friendly request snapshot.";

    let mut request = build_model_request("custom.openai_compatible");
    request.prepared_prompt.system_prompt = Some(stable_system_prompt.to_string());
    request.prepared_prompt.messages = test_model_messages(vec![
        ChatMessage {
            message_id: "msg-user-1".to_string(),
            role: MessageRole::User,
            content: first_user.to_string(),
            created_at_ms: 1,
            metadata: HashMap::new(),
        },
        ChatMessage {
            message_id: "msg-assistant-1".to_string(),
            role: MessageRole::Assistant,
            content: first_assistant.to_string(),
            created_at_ms: 2,
            metadata: assistant_semantics_metadata(
                Some("Inspect the manifest before acting."),
                &[(
                    "call-read-1",
                    "read",
                    r#"{"path":"core/src/runtime.rs"}"#,
                )],
            ),
        },
        ChatMessage {
            message_id: "msg-tool-1".to_string(),
            role: MessageRole::Tool,
            content: tool_result.to_string(),
            created_at_ms: 3,
            metadata: tool_result_semantics_metadata("call-read-1", "read", "ok"),
        },
        ChatMessage {
            message_id: "msg-assistant-2".to_string(),
            role: MessageRole::Assistant,
            content: second_assistant.to_string(),
            created_at_ms: 4,
            metadata: HashMap::new(),
        },
        ChatMessage {
            message_id: "msg-execution-context".to_string(),
            role: MessageRole::User,
            content: "<environment_context>\n  <cwd>D:/Projects/Centaeris</cwd>\n  <bash>bash (Git for Windows)</bash>\n</environment_context>".to_string(),
            created_at_ms: 5,
            metadata: HashMap::new(),
        },
        ChatMessage {
            message_id: "msg-user-2".to_string(),
            role: MessageRole::User,
            content: current_user.to_string(),
            created_at_ms: 6,
            metadata: HashMap::new(),
        },
    ]);

    let messages = super::build_openai_compatible_messages(&request).expect("messages");
    let reusable_prefix = vec![
        super::OpenAiCompatibleChatMessage {
            role: "system".to_string(),
            content: Value::String(stable_system_prompt.to_string()),
            tool_calls: None,
            reasoning_content: None,
            tool_call_id: None,
        },
        super::OpenAiCompatibleChatMessage {
            role: "user".to_string(),
            content: Value::String(first_user.to_string()),
            tool_calls: None,
            reasoning_content: None,
            tool_call_id: None,
        },
        super::OpenAiCompatibleChatMessage {
            role: "assistant".to_string(),
            content: Value::String(first_assistant.to_string()),
            tool_calls: Some(vec![super::OpenAiCompatibleRequestToolCall {
                id: "call-read-1".to_string(),
                tool_type: "function".to_string(),
                function: super::OpenAiCompatibleToolFunction {
                    name: "read".to_string(),
                    arguments: r#"{"path":"core/src/runtime.rs"}"#.to_string(),
                },
            }]),
            reasoning_content: Some("Inspect the manifest before acting.".to_string()),
            tool_call_id: None,
        },
        super::OpenAiCompatibleChatMessage {
            role: "tool".to_string(),
            content: Value::String(tool_result.to_string()),
            tool_calls: None,
            reasoning_content: None,
            tool_call_id: Some("call-read-1".to_string()),
        },
        super::OpenAiCompatibleChatMessage {
            role: "assistant".to_string(),
            content: Value::String(second_assistant.to_string()),
            tool_calls: None,
            reasoning_content: None,
            tool_call_id: None,
        },
    ];
    assert_eq!(
        &messages[..reusable_prefix.len()],
        reusable_prefix.as_slice()
    );

    let snapshot = serde_json::to_value(&messages).expect("serialize messages snapshot");
    assert_eq!(
        snapshot,
        serde_json::json!([
            {"role": "system", "content": stable_system_prompt},
            {"role": "user", "content": first_user},
            {
                "role": "assistant",
                "content": first_assistant,
                "tool_calls": [{
                    "id": "call-read-1",
                    "type": "function",
                    "function": {
                        "name": "read",
                        "arguments": "{\"path\":\"core/src/runtime.rs\"}"
                    }
                }],
                "reasoning_content": "Inspect the manifest before acting."
            },
            {"role": "tool", "content": tool_result, "tool_call_id": "call-read-1"},
            {"role": "assistant", "content": second_assistant},
            {"role": "user", "content": "<environment_context>\n  <cwd>D:/Projects/Centaeris</cwd>\n  <bash>bash (Git for Windows)</bash>\n</environment_context>"},
            {"role": "user", "content": current_user}
        ])
    );
}

#[tokio::test]
async fn openai_compatible_two_round_cache_audit_hashes_final_messages_and_provider_params() {
    let stable_system_prompt = "You are Centaeris.";
    let first_user = "Inspect cache behavior.";
    let first_assistant = "Cache behavior depends on stable request prefixes.";
    let second_user = "Continue the cache audit.";

    let mut round1 = build_model_request("custom.openai_compatible");
    round1.prepared_prompt.system_prompt = Some(stable_system_prompt.to_string());
    round1.prepared_prompt.messages = test_model_messages(vec![ChatMessage {
        message_id: "msg-user-1".to_string(),
        role: MessageRole::User,
        content: first_user.to_string(),
        created_at_ms: 1,
        metadata: HashMap::new(),
    }]);

    let mut round2 = build_model_request("custom.openai_compatible");
    round2.prepared_prompt.system_prompt = Some(stable_system_prompt.to_string());
    round2.prepared_prompt.messages = test_model_messages(vec![
        ChatMessage {
            message_id: "msg-user-1".to_string(),
            role: MessageRole::User,
            content: first_user.to_string(),
            created_at_ms: 1,
            metadata: HashMap::new(),
        },
        ChatMessage {
            message_id: "msg-assistant-1".to_string(),
            role: MessageRole::Assistant,
            content: first_assistant.to_string(),
            created_at_ms: 2,
            metadata: HashMap::new(),
        },
        ChatMessage {
            message_id: "msg-user-2".to_string(),
            role: MessageRole::User,
            content: second_user.to_string(),
            created_at_ms: 3,
            metadata: HashMap::new(),
        },
    ]);

    let round1_messages =
        super::build_openai_compatible_messages(&round1).expect("round1 messages");
    let round2_messages =
        super::build_openai_compatible_messages(&round2).expect("round2 messages");
    let prefix_len = common_openai_message_prefix_len(&round1_messages, &round2_messages);
    let round1_prefix_hash = hash_openai_message_prefix(&round1_messages, prefix_len);
    let round2_prefix_hash = hash_openai_message_prefix(&round2_messages, prefix_len);
    let round1_tools_hash =
        test_hash_json(&serde_json::to_value(super::build_openai_chat_tools(&round1)).unwrap());
    let round2_tools_hash =
        test_hash_json(&serde_json::to_value(super::build_openai_chat_tools(&round2)).unwrap());
    let round1_provider_params_hash = openai_provider_params_hash(&round1, true);
    let round2_provider_params_hash = openai_provider_params_hash(&round2, true);

    assert_eq!(
        prefix_len,
        round1_messages.len(),
        "round2 should fully reuse round1 chat/completions message prefix"
    );
    assert_eq!(round1_prefix_hash, round2_prefix_hash);
    assert_eq!(
        round1_tools_hash, round2_tools_hash,
        "serialized tool schema should stay stable across adjacent turns"
    );
    assert_eq!(
        round1_provider_params_hash, round2_provider_params_hash,
        "provider params should not drift across adjacent turns"
    );
    let final_messages_break = prefix_len < round1_messages.len();
    let tool_definitions_changed = round1_tools_hash != round2_tools_hash;
    let provider_params_changed = round1_provider_params_hash != round2_provider_params_hash;

    eprintln!(
        "openai_compatible_two_round_cache_audit\n{}",
        serde_json::json!({
            "round1": {
                "finalMessageCount": round1_messages.len(),
                "toolDefinitionsHash": round1_tools_hash,
                "providerParamsHash": round1_provider_params_hash,
            },
            "round2": {
                "finalMessageCount": round2_messages.len(),
                "toolDefinitionsHash": round2_tools_hash,
                "providerParamsHash": round2_provider_params_hash,
            },
            "comparison": {
                "finalMessagesCommonPrefixLen": prefix_len,
                "finalMessagesCommonPrefixHash": round1_prefix_hash,
                "toolDefinitionsChanged": tool_definitions_changed,
                "providerParamsChanged": provider_params_changed,
                "cacheBreakDetection": {
                    "schema": "prompt_cache_break_detection_v1",
                    "hasBreak": final_messages_break
                        || tool_definitions_changed
                        || provider_params_changed,
                    "messages": {
                        "break": final_messages_break,
                        "reason": if final_messages_break {
                            Some("previous_request_messages_not_fully_reused")
                        } else {
                            None
                        },
                        "commonPrefixLen": prefix_len,
                        "previousMessageCount": round1_messages.len(),
                        "currentMessageCount": round2_messages.len(),
                    },
                    "tools": {
                        "break": tool_definitions_changed,
                        "reason": if tool_definitions_changed {
                            Some("tool_definitions_changed")
                        } else {
                            None
                        },
                        "leftHash": round1_tools_hash,
                        "rightHash": round2_tools_hash,
                    },
                    "provider": {
                        "break": provider_params_changed,
                        "reason": if provider_params_changed {
                            Some("provider_params_changed")
                        } else {
                            None
                        },
                        "leftHash": round1_provider_params_hash,
                        "rightHash": round2_provider_params_hash,
                    },
                },
            }
        })
    );
}

#[tokio::test]
async fn openai_compatible_client_builds_chat_completions_request() {
    let transport = MockJsonHttpTransport::with_response(Ok(JsonHttpResponse {
        status_code: 200,
        headers: HashMap::new(),
        body_json:
            "{\"id\":\"resp_1\",\"choices\":[{\"message\":{\"content\":\"ok\"}}],\"usage\":{\"prompt_tokens\":10,\"total_tokens\":20}}"
                .to_string(),
    }));
    let client = OpenAiCompatibleModelClient::new(ModelProviderRegistry::new(), transport);

    std::env::set_var("OPENAI_API_KEY", "test-openai-key");
    let request = build_model_request("custom.openai_compatible");
    let response = client
        .generate(&request)
        .await
        .expect("generate should succeed");

    assert_eq!(response.generate_result.content, "ok");
    let requests = client.transport.take_requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[0].url, "http://localhost:4000/v1/chat/completions");
    assert_eq!(
        requests[0].headers.get("content-type").map(String::as_str),
        Some("application/json")
    );
    let body: Value = serde_json::from_str(requests[0].body_json.as_str())
        .expect("openai completions request JSON");
    assert!(body.get("tools").is_some());
    assert!(body.get("reasoning_effort").is_none());
}

#[tokio::test]
async fn prepared_prompt_output_limit_may_be_below_model_capability() {
    let transport = MockJsonHttpTransport::with_response(Ok(JsonHttpResponse {
        status_code: 200,
        headers: HashMap::new(),
        body_json:
            "{\"id\":\"resp_output_limit\",\"choices\":[{\"message\":{\"content\":\"ok\"}}]}"
                .to_string(),
    }));
    let client = OpenAiCompatibleModelClient::new(ModelProviderRegistry::new(), transport);
    let mut request = build_model_request("custom.openai_compatible");
    request.prepared_prompt.max_output_tokens = 128;

    client
        .generate(&request)
        .await
        .expect("request-local output limit below model capability is valid");

    let requests = client.transport.take_requests();
    let body =
        serde_json::from_str::<Value>(requests[0].body_json.as_str()).expect("parse request body");
    assert_eq!(body.get("max_tokens").and_then(Value::as_u64), Some(128));
}

#[tokio::test]
async fn prepared_prompt_output_limit_above_model_capability_fails() {
    let client = OpenAiCompatibleModelClient::new(
        ModelProviderRegistry::new(),
        MockJsonHttpTransport::with_response(Err("must not send".to_string())),
    );
    let mut request = build_model_request("custom.openai_compatible");
    request.prepared_prompt.max_output_tokens = 513;

    let error = client
        .generate(&request)
        .await
        .expect_err("request-local output limit above model capability must fail");

    assert_eq!(error.kind, ModelClientErrorKind::InvalidRequest);
    assert!(error
        .message
        .contains("prepared_prompt_max_output_tokens_exceeded"));
    assert!(client.transport.take_requests().is_empty());
}

#[tokio::test]
async fn openai_compatible_client_retries_json_network_errors_before_success() {
    let transport = MockJsonHttpTransport::with_responses(vec![
        Err("network timeout".to_string()),
        Ok(JsonHttpResponse {
            status_code: 200,
            headers: HashMap::new(),
            body_json: "{\"id\":\"resp_retry\",\"choices\":[{\"message\":{\"content\":\"ok after retry\"}}]}".to_string(),
        }),
    ]);
    let client = OpenAiCompatibleModelClient::new(ModelProviderRegistry::new(), transport);
    let mut request = build_model_request("custom.openai_compatible");
    request.session_config.max_retries = 1;
    request.session_config.retry_backoff_ms = 0;

    let response = client
        .generate(&request)
        .await
        .expect("generate should retry and succeed");

    assert_eq!(response.generate_result.content, "ok after retry");
    assert_eq!(response.provider_attempts, 2);
    assert_eq!(client.transport.take_requests().len(), 2);
}

#[tokio::test]
async fn openai_compatible_client_retries_reasoning_only_empty_final_response() {
    let transport = MockJsonHttpTransport::with_responses(vec![
        Ok(JsonHttpResponse {
            status_code: 200,
            headers: HashMap::new(),
            body_json: json!({
                "id": "resp_empty",
                "choices": [{
                    "message": {
                        "content": "",
                        "reasoning_content": "analysis stopped before the answer"
                    },
                    "finish_reason": "stop"
                }]
            })
            .to_string(),
        }),
        Ok(JsonHttpResponse {
            status_code: 200,
            headers: HashMap::new(),
            body_json: json!({
                "id": "resp_recovered",
                "choices": [{
                    "message": {"content": "recovered answer"},
                    "finish_reason": "stop"
                }]
            })
            .to_string(),
        }),
    ]);
    let client = OpenAiCompatibleModelClient::new(ModelProviderRegistry::new(), transport);
    let mut request = build_model_request("custom.openai_compatible");
    request.session_config.max_retries = 1;
    request.session_config.retry_backoff_ms = 0;

    let response = client
        .generate(&request)
        .await
        .expect("reasoning-only empty final response should retry");

    assert_eq!(response.generate_result.content, "recovered answer");
    assert_eq!(response.provider_attempts, 2);
    assert_eq!(client.transport.take_requests().len(), 2);
}

#[tokio::test]
async fn openai_compatible_stream_retries_partial_uncommitted_response() {
    let transport = MockJsonHttpTransport::with_sse_response_chunks(
        vec![
            Err("stream timeout".to_string()),
            Ok(JsonHttpResponse {
                status_code: 200,
                headers: HashMap::new(),
                body_json: String::new(),
            }),
        ],
        vec![
            vec![
                "{\"id\":\"chatcmpl_1\",\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}"
                    .to_string(),
            ],
            vec![
                "{\"id\":\"chatcmpl_2\",\"choices\":[{\"delta\":{\"content\":\"done\"},\"finish_reason\":\"stop\"}]}"
                    .to_string(),
            ],
        ],
    );
    let client = OpenAiCompatibleModelClient::new(ModelProviderRegistry::new(), transport);
    let mut request = build_model_request("custom.openai_compatible");
    request.session_config.max_retries = 1;
    request.session_config.retry_backoff_ms = 0;
    let mut events = vec![];

    let response = client
        .generate_stream(&request, &mut |event| events.push(event))
        .await
        .expect("partial response is not committed and should retry");

    assert_eq!(response.generate_result.content, "done");
    assert_eq!(response.provider_attempts, 2);
    assert_eq!(client.transport.take_requests().len(), 2);
    assert!(events.iter().any(|event| {
        matches!(event, ModelClientStreamEvent::Token { content } if content == "hello")
    }));
    assert!(events.iter().any(|event| {
        matches!(event, ModelClientStreamEvent::ReplaceContent { content } if content.is_empty())
    }));
    assert!(events.iter().any(|event| {
        matches!(
            event,
            ModelClientStreamEvent::Status {
                process_state: RuntimeProcessState::Retrying,
                ..
            }
        )
    }));
}

#[tokio::test]
async fn openai_compatible_client_serializes_tool_context_as_native_tool_message() {
    let transport = MockJsonHttpTransport::with_response(Ok(JsonHttpResponse {
        status_code: 200,
        headers: HashMap::new(),
        body_json: "{\"id\":\"resp_tool_ctx\",\"choices\":[{\"message\":{\"content\":\"ok\"}}]}"
            .to_string(),
    }));
    let client = OpenAiCompatibleModelClient::new(ModelProviderRegistry::new(), transport);
    std::env::set_var("OPENAI_API_KEY", "test-openai-key");
    let mut request = build_model_request("custom.openai_compatible");
    request.prepared_prompt.messages = test_model_messages(vec![
        ChatMessage {
            message_id: "msg-user-1".to_string(),
            role: MessageRole::User,
            content: "Run pwd.".to_string(),
            created_at_ms: 0,
            metadata: HashMap::new(),
        },
        ChatMessage {
            message_id: "msg-assistant-tool-1".to_string(),
            role: MessageRole::Assistant,
            content: String::new(),
            created_at_ms: 1,
            metadata: assistant_semantics_metadata(
                Some(""),
                &[("call_1", "bash", "{\"command\":\"pwd\"}")],
            ),
        },
        ChatMessage {
            message_id: "msg-tool-1".to_string(),
            role: MessageRole::Tool,
            content: "{\"stdout\":\"tool output\"}".to_string(),
            created_at_ms: 2,
            metadata: tool_result_semantics_metadata("call_1", "bash", "ok"),
        },
    ]);

    client
        .generate(&request)
        .await
        .expect("generate should succeed");

    let requests = client.transport.take_requests();
    let body = serde_json::from_str::<serde_json::Value>(requests[0].body_json.as_str())
        .expect("parse chat completions body");
    let messages = body
        .get("messages")
        .and_then(serde_json::Value::as_array)
        .expect("messages array");
    assert_eq!(
        messages,
        &vec![
            serde_json::json!({
                "role": "system",
                "content": "You are Centaeris."
            }),
            serde_json::json!({
                "role": "user",
                "content": "Run pwd."
            }),
            serde_json::json!({
                "role": "assistant",
                "content": "",
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {
                        "name": "bash",
                        "arguments": "{\"command\":\"pwd\"}",
                    }
                }],
                "reasoning_content": ""
            }),
            serde_json::json!({
                "role": "tool",
                "content": "{\"stdout\":\"tool output\"}",
                "tool_call_id": "call_1"
            })
        ]
    );
}

#[tokio::test]
async fn openai_compatible_client_fails_tool_context_without_call_identity() {
    let transport = MockJsonHttpTransport::with_response(Ok(JsonHttpResponse {
        status_code: 200,
        headers: HashMap::new(),
        body_json: "{\"id\":\"resp_tool_ctx\",\"choices\":[{\"message\":{\"content\":\"ok\"}}]}"
            .to_string(),
    }));
    let client = OpenAiCompatibleModelClient::new(ModelProviderRegistry::new(), transport);
    std::env::set_var("OPENAI_API_KEY", "test-openai-key");
    let mut request = build_model_request("custom.openai_compatible");
    request.prepared_prompt.messages.push(ModelMessageV1 {
        message_id: "msg-tool-1".to_string(),
        role: crate::model::prepared_prompt::ModelMessageRoleV1::Tool,
        content: "{\"stdout\":\"tool output\"}".to_string(),
        tool_calls: vec![],
        tool_call_id: None,
        reasoning_content: None,
    });

    let error = client
        .generate(&request)
        .await
        .expect_err("tool context without call identity must fail");

    assert_eq!(error.kind, ModelClientErrorKind::InvalidRequest);
    assert!(error
        .message
        .contains("prepared_prompt_tool_result_missing_call_id"));
}

#[tokio::test]
async fn openai_compatible_client_fails_assistant_tool_call_without_tool_result() {
    let transport = MockJsonHttpTransport::with_response(Ok(JsonHttpResponse {
        status_code: 200,
        headers: HashMap::new(),
        body_json: "{\"id\":\"resp_tool_ctx\",\"choices\":[{\"message\":{\"content\":\"ok\"}}]}"
            .to_string(),
    }));
    let client = OpenAiCompatibleModelClient::new(ModelProviderRegistry::new(), transport);
    std::env::set_var("OPENAI_API_KEY", "test-openai-key");
    let mut request = build_model_request("custom.openai_compatible");
    request.prepared_prompt.messages = test_model_messages(vec![
        ChatMessage {
            message_id: "msg-user-1".to_string(),
            role: MessageRole::User,
            content: "Run pwd.".to_string(),
            created_at_ms: 0,
            metadata: HashMap::new(),
        },
        ChatMessage {
            message_id: "msg-assistant-tool-1".to_string(),
            role: MessageRole::Assistant,
            content: String::new(),
            created_at_ms: 1,
            metadata: assistant_semantics_metadata(
                Some(""),
                &[("call_1", "bash", "{\"command\":\"pwd\"}")],
            ),
        },
    ]);

    let error = client
        .generate(&request)
        .await
        .expect_err("assistant tool call without result must fail");

    assert_eq!(error.kind, ModelClientErrorKind::InvalidRequest);
    assert!(error.message.contains(
        "prepared_prompt_tool_pairing_invalid: assistant tool call call_1 has no result"
    ));
}

#[tokio::test]
async fn openai_compatible_client_fails_mismatched_tool_call_id() {
    let transport = MockJsonHttpTransport::with_response(Ok(JsonHttpResponse {
        status_code: 200,
        headers: HashMap::new(),
        body_json: "{\"id\":\"resp_tool_ctx\",\"choices\":[{\"message\":{\"content\":\"ok\"}}]}"
            .to_string(),
    }));
    let client = OpenAiCompatibleModelClient::new(ModelProviderRegistry::new(), transport);
    std::env::set_var("OPENAI_API_KEY", "test-openai-key");
    let mut request = build_model_request("custom.openai_compatible");
    request.prepared_prompt.messages = test_model_messages(vec![
        ChatMessage {
            message_id: "msg-user-1".to_string(),
            role: MessageRole::User,
            content: "Run pwd.".to_string(),
            created_at_ms: 0,
            metadata: HashMap::new(),
        },
        ChatMessage {
            message_id: "msg-assistant-tool-1".to_string(),
            role: MessageRole::Assistant,
            content: String::new(),
            created_at_ms: 1,
            metadata: assistant_semantics_metadata(
                Some(""),
                &[("call_1", "bash", "{\"command\":\"pwd\"}")],
            ),
        },
        ChatMessage {
            message_id: "msg-tool-1".to_string(),
            role: MessageRole::Tool,
            content: "{\"stdout\":\"tool output\"}".to_string(),
            created_at_ms: 2,
            metadata: tool_result_semantics_metadata("call_2", "bash", "ok"),
        },
    ]);

    let error = client
        .generate(&request)
        .await
        .expect_err("mismatched tool_call_id must fail");

    assert_eq!(error.kind, ModelClientErrorKind::InvalidRequest);
    assert!(error
        .message
        .contains("prepared_prompt_tool_pairing_invalid: messageId=msg-tool-1 toolCallId=call_2 expected=call_1"));
}

#[test]
fn model_client_rejects_invalid_tool_call_identity() {
    for call_id in [None, Some(""), Some("  ")] {
        let error = super::required_provider_tool_call_identity(
            "openai-compatible",
            call_id,
            Some("file_read"),
        )
        .expect_err("invalid call id must loud-fail");
        assert_eq!(
            error.provider_code.as_deref(),
            Some("invalid_tool_call_identity")
        );
    }
    assert_eq!(
        super::required_provider_tool_call_identity(
            "openai-compatible",
            Some("call_ok"),
            Some("file_read"),
        )
        .expect("exact identity"),
        ("call_ok".to_string(), "file_read".to_string())
    );
}

#[tokio::test]
async fn openai_compatible_client_parses_tool_calls() {
    let transport = MockJsonHttpTransport::with_response(Ok(JsonHttpResponse {
        status_code: 200,
        headers: HashMap::new(),
        body_json: "{\"id\":\"resp_2\",\"choices\":[{\"message\":{\"content\":\"\",\"reasoning_content\":\"\",\"tool_calls\":[{\"id\":\"call_1\",\"function\":{\"name\":\"file_read\",\"arguments\":\"{\\\"path\\\":\\\"README.md\\\"}\"}}]}}],\"usage\":{\"prompt_tokens\":11,\"total_tokens\":25,\"prompt_cache_hit_tokens\":7,\"prompt_cache_miss_tokens\":4}}".to_string(),
    }));
    let client = OpenAiCompatibleModelClient::new(ModelProviderRegistry::new(), transport);
    let request = build_model_request("custom.openai_compatible");

    let response = client
        .generate(&request)
        .await
        .expect("generate should succeed");

    assert_eq!(response.generate_result.tool_calls.len(), 1);
    assert_eq!(
        response.generate_result.reasoning_content.as_deref(),
        Some("")
    );
    assert_eq!(response.generate_result.tool_calls[0].id, "call_1");
    assert_eq!(response.generate_result.tool_calls[0].name, "file_read");
    assert_eq!(
        response.generate_result.tool_calls[0].args_json,
        "{\"path\":\"README.md\"}"
    );
    assert_eq!(response.generate_result.input_tokens, Some(11));
    assert_eq!(response.generate_result.total_tokens, Some(25));
    assert_eq!(response.generate_result.prompt_cache_hit_tokens, Some(7));
    assert_eq!(response.generate_result.prompt_cache_miss_tokens, Some(4));
}

#[tokio::test]
async fn openai_compatible_output_limit_does_not_synthesize_tool_identity() {
    let transport = MockJsonHttpTransport::with_response(Ok(JsonHttpResponse {
        status_code: 200,
        headers: HashMap::new(),
        body_json: json!({
            "id": "resp-capped",
            "choices": [{
                "message": {
                    "content": "partial",
                    "tool_calls": [{
                        "id": "",
                        "function": {"name": "", "arguments": "{\"path\":\"unfinished"}
                    }]
                },
                "finish_reason": "length"
            }]
        })
        .to_string(),
    }));
    let client = OpenAiCompatibleModelClient::new(ModelProviderRegistry::new(), transport);

    let error = client
        .generate(&build_model_request("custom.openai_compatible"))
        .await
        .expect_err("capped response must enter runtime recovery");

    assert_eq!(error.truncated_tool_calls.len(), 1);
    assert_eq!(error.truncated_tool_calls[0].call_id, None);
    assert_eq!(error.truncated_tool_calls[0].tool_name, None);
    assert!(error.truncated_tool_calls[0]
        .args_sha256
        .starts_with("sha256:"));
}

#[tokio::test]
async fn openai_compatible_non_stream_output_limit_preserves_complete_tool_identity() {
    let raw_args = r#"{"path":"unfinished"#;
    let transport = MockJsonHttpTransport::with_response(Ok(JsonHttpResponse {
        status_code: 200,
        headers: HashMap::new(),
        body_json: json!({
            "id": "resp-capped",
            "choices": [{
                "message": {
                    "content": "partial",
                    "tool_calls": [{
                        "id": "call-capped",
                        "function": {"name": "read", "arguments": raw_args}
                    }]
                },
                "finish_reason": "length"
            }]
        })
        .to_string(),
    }));
    let client = OpenAiCompatibleModelClient::new(ModelProviderRegistry::new(), transport);

    let error = client
        .generate(&build_model_request("custom.openai_compatible"))
        .await
        .expect_err("capped response must enter runtime recovery");

    assert_output_limit_tool_identity(&error, Some("call-capped"), Some("read"), raw_args);
}

#[tokio::test]
async fn openai_compatible_stream_output_limit_preserves_complete_tool_identity_without_ready() {
    let raw_args = r#"{"path":"unfinished"#;
    let transport = MockJsonHttpTransport::with_sse(
        Ok(JsonHttpResponse {
            status_code: 200,
            headers: HashMap::new(),
            body_json: String::new(),
        }),
        vec![
            r#"{"id":"chatcmpl-capped","choices":[{"delta":{"content":"partial"}}]}"#,
            r#"{"id":"chatcmpl-capped","choices":[{"delta":{"tool_calls":[{"index":0,"id":"call-capped","function":{"name":"read","arguments":"{\"path\":\"unfinished"}}]}}]}"#,
            r#"{"id":"chatcmpl-capped","choices":[{"delta":{},"finish_reason":"length"}]}"#,
        ],
    );
    let client = OpenAiCompatibleModelClient::new(ModelProviderRegistry::new(), transport);
    let mut events = Vec::new();

    let error = client
        .generate_stream(
            &build_model_request("custom.openai_compatible"),
            &mut |event| events.push(event),
        )
        .await
        .expect_err("capped stream must enter runtime recovery");

    assert_output_limit_tool_identity(&error, Some("call-capped"), Some("read"), raw_args);
    assert!(!events.iter().any(|event| matches!(
        event,
        ModelClientStreamEvent::ToolCallReady { .. } | ModelClientStreamEvent::Done { .. }
    )));
}

#[tokio::test]
async fn openai_compatible_stream_output_limit_does_not_synthesize_tool_identity() {
    let raw_args = r#"{"path":"unfinished"#;
    let transport = MockJsonHttpTransport::with_sse(
        Ok(JsonHttpResponse {
            status_code: 200,
            headers: HashMap::new(),
            body_json: String::new(),
        }),
        vec![
            r#"{"id":"chatcmpl-capped","choices":[{"delta":{"tool_calls":[{"index":0,"id":"","function":{"name":"","arguments":"{\"path\":\"unfinished"}}]}}]}"#,
            r#"{"id":"chatcmpl-capped","choices":[{"delta":{},"finish_reason":"max_tokens"}]}"#,
        ],
    );
    let client = OpenAiCompatibleModelClient::new(ModelProviderRegistry::new(), transport);
    let mut events = Vec::new();

    let error = client
        .generate_stream(
            &build_model_request("custom.openai_compatible"),
            &mut |event| events.push(event),
        )
        .await
        .expect_err("capped stream must enter runtime recovery");

    assert_output_limit_tool_identity(&error, None, None, raw_args);
    assert!(!events.iter().any(|event| matches!(
        event,
        ModelClientStreamEvent::ToolCallReady { .. } | ModelClientStreamEvent::Done { .. }
    )));
}

#[tokio::test]
async fn openai_compatible_client_normalizes_vllm_reasoning() {
    let transport = MockJsonHttpTransport::with_response(Ok(JsonHttpResponse {
        status_code: 200,
        headers: HashMap::new(),
        body_json: json!({
            "id": "resp_vllm_reasoning",
            "choices": [{
                "message": {
                    "content": "final answer",
                    "reasoning": "inspect the request"
                }
            }]
        })
        .to_string(),
    }));
    let client = OpenAiCompatibleModelClient::new(ModelProviderRegistry::new(), transport);

    let response = client
        .generate(&build_model_request("custom.openai_compatible"))
        .await
        .expect("vLLM reasoning should parse");

    assert_eq!(response.generate_result.content, "final answer");
    assert_eq!(
        response.generate_result.reasoning_content.as_deref(),
        Some("inspect the request")
    );
}

#[tokio::test]
async fn openai_compatible_client_rejects_conflicting_reasoning_fields() {
    let transport = MockJsonHttpTransport::with_response(Ok(JsonHttpResponse {
        status_code: 200,
        headers: HashMap::new(),
        body_json: json!({
            "choices": [{
                "message": {
                    "content": "final answer",
                    "reasoning": "vllm",
                    "reasoning_content": "alternate"
                }
            }]
        })
        .to_string(),
    }));
    let client = OpenAiCompatibleModelClient::new(ModelProviderRegistry::new(), transport);

    let error = client
        .generate(&build_model_request("custom.openai_compatible"))
        .await
        .expect_err("conflicting reasoning fields must fail loudly");

    assert_eq!(error.kind, ModelClientErrorKind::Provider);
    assert!(error
        .message
        .contains("both reasoning and reasoning_content"));
}

#[tokio::test]
async fn openai_compatible_client_rejects_malformed_tool_arguments() {
    let malformed_response = JsonHttpResponse {
        status_code: 200,
        headers: HashMap::new(),
        body_json: json!({
            "id": "resp_malformed_tool",
            "choices": [{
                "finish_reason": "tool_calls",
                "message": {
                    "content": "",
                    "tool_calls": [{
                        "id": "call_malformed",
                        "function": {
                            "name": "write",
                            "arguments": "{\"path\":\"report.py\",\"content\":\"unterminated"
                        }
                    }]
                }
            }]
        })
        .to_string(),
    };
    let transport = MockJsonHttpTransport::with_responses(vec![
        Ok(malformed_response.clone()),
        Ok(malformed_response),
    ]);
    let client = OpenAiCompatibleModelClient::new(ModelProviderRegistry::new(), transport);
    let request = build_model_request("custom.openai_compatible");

    let error = client
        .generate(&request)
        .await
        .expect_err("malformed tool arguments must fail at the provider boundary");

    assert_eq!(error.kind, ModelClientErrorKind::Provider);
    assert_eq!(
        error.provider_code.as_deref(),
        Some("malformed_tool_call_arguments")
    );
    assert!(!error.retryable);
    assert_eq!(error.provider_attempts, 2);
    assert!(error.message.contains("callId=call_malformed"));
    assert!(error.message.contains("toolName=write"));
}

#[tokio::test]
async fn openai_compatible_client_retries_malformed_tool_arguments_before_commit() {
    let malformed = JsonHttpResponse {
        status_code: 200,
        headers: HashMap::new(),
        body_json: json!({
            "id": "resp_malformed_tool",
            "choices": [{
                "finish_reason": "tool_calls",
                "message": {
                    "content": "",
                    "tool_calls": [{
                        "id": "call_malformed",
                        "function": {
                            "name": "edit",
                            "arguments": "{\"path\":\"report.py\",\"edits\":["
                        }
                    }]
                }
            }]
        })
        .to_string(),
    };
    let valid = JsonHttpResponse {
        status_code: 200,
        headers: HashMap::new(),
        body_json: json!({
            "id": "resp_valid_tool",
            "choices": [{
                "finish_reason": "tool_calls",
                "message": {
                    "content": "",
                    "tool_calls": [{
                        "id": "call_valid",
                        "function": {
                            "name": "read",
                            "arguments": "{\"path\":\"report.py\"}"
                        }
                    }]
                }
            }]
        })
        .to_string(),
    };
    let transport = MockJsonHttpTransport::with_responses(vec![Ok(malformed), Ok(valid)]);
    let client = OpenAiCompatibleModelClient::new(ModelProviderRegistry::new(), transport);
    let request = build_model_request("custom.openai_compatible");

    let response = client
        .generate(&request)
        .await
        .expect("retry should succeed");

    assert_eq!(response.provider_attempts, 2);
    assert_eq!(response.generate_result.tool_calls.len(), 1);
    assert_eq!(response.generate_result.tool_calls[0].id, "call_valid");
    assert_eq!(client.transport.take_requests().len(), 2);
}

#[tokio::test]
async fn openai_compatible_client_preserves_multiple_tool_calls_in_one_response() {
    let transport = MockJsonHttpTransport::with_response(Ok(JsonHttpResponse {
        status_code: 200,
        headers: HashMap::new(),
        body_json: json!({
            "id": "resp_multi_tool",
            "choices": [{
                "finish_reason": "tool_calls",
                "message": {
                    "content": "",
                    "reasoning_content": "inspect both manifests",
                    "tool_calls": [
                        {
                            "id": "call_cargo",
                            "function": {
                                "name": "read",
                                "arguments": "{\"path\":\"Cargo.toml\"}"
                            }
                        },
                        {
                            "id": "call_package",
                            "function": {
                                "name": "read",
                                "arguments": "{\"path\":\"package.json\"}"
                            }
                        }
                    ]
                }
            }]
        })
        .to_string(),
    }));
    let client = OpenAiCompatibleModelClient::new(ModelProviderRegistry::new(), transport);
    let request = build_model_request("custom.openai_compatible");

    let response = client
        .generate(&request)
        .await
        .expect("multiple tool calls should parse");

    assert_eq!(response.generate_result.tool_calls.len(), 2);
    assert_eq!(response.generate_result.tool_calls[0].id, "call_cargo");
    assert_eq!(response.generate_result.tool_calls[1].id, "call_package");
    assert_eq!(
        response.generate_result.reasoning_content.as_deref(),
        Some("inspect both manifests")
    );
}

#[tokio::test]
async fn openai_compatible_client_derives_input_tokens_from_deepseek_cache_usage() {
    // 真实 DeepSeek 命中率 smoke 需要先配置 DEEPSEEK_API_KEY 环境变量并允许网络访问；
    // 默认单测只验证 provider usage 字段解析，避免 CI 依赖外部 API。
    let transport = MockJsonHttpTransport::with_response(Ok(JsonHttpResponse {
        status_code: 200,
        headers: HashMap::new(),
        body_json: "{\"id\":\"resp_cache\",\"choices\":[{\"message\":{\"content\":\"ok\"}}],\"usage\":{\"total_tokens\":30,\"prompt_cache_hit_tokens\":21,\"prompt_cache_miss_tokens\":3}}".to_string(),
    }));
    let client = OpenAiCompatibleModelClient::new(ModelProviderRegistry::new(), transport);
    let request = build_model_request("custom.openai_compatible");

    let response = client
        .generate(&request)
        .await
        .expect("generate should succeed");

    assert_eq!(response.generate_result.content, "ok");
    assert_eq!(response.generate_result.input_tokens, Some(24));
    assert_eq!(response.generate_result.total_tokens, Some(30));
    assert_eq!(response.generate_result.prompt_cache_hit_tokens, Some(21));
    assert_eq!(response.generate_result.prompt_cache_miss_tokens, Some(3));
}

#[tokio::test]
async fn openai_compatible_client_parses_standard_cached_tokens() {
    let transport = MockJsonHttpTransport::with_response(Ok(JsonHttpResponse {
        status_code: 200,
        headers: HashMap::new(),
        body_json: "{\"id\":\"chatcmpl_cache\",\"choices\":[{\"message\":{\"content\":\"ok\"}}],\"usage\":{\"prompt_tokens\":42,\"total_tokens\":50,\"prompt_tokens_details\":{\"cached_tokens\":32}}}".to_string(),
    }));
    let client = OpenAiCompatibleModelClient::new(ModelProviderRegistry::new(), transport);
    let request = build_model_request("custom.openai_compatible");

    let response = client
        .generate(&request)
        .await
        .expect("generate should succeed");

    assert_eq!(response.generate_result.prompt_cache_hit_tokens, Some(32));
    assert_eq!(response.generate_result.prompt_cache_miss_tokens, Some(10));
}

#[tokio::test]
async fn openai_compatible_client_projects_completed_tool_call_only() {
    let transport = MockJsonHttpTransport::with_sse(
        Ok(JsonHttpResponse {
            status_code: 200,
            headers: HashMap::new(),
            body_json: String::new(),
        }),
        vec![
            "{\"id\":\"chatcmpl_1\",\"choices\":[{\"delta\":{\"reasoning\":\"inspect the manifest\",\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"bash\",\"arguments\":\"\"}}]}}]}",
            "{\"id\":\"chatcmpl_1\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"command\\\":\"}}]}}]}",
            "{\"id\":\"chatcmpl_1\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"rg --files -g README.md\\\"}\"}}]}}]}",
            "{\"id\":\"chatcmpl_1\",\"choices\":[],\"usage\":{\"prompt_tokens\":42,\"total_tokens\":50,\"prompt_cache_hit_tokens\":40,\"prompt_cache_miss_tokens\":2}}",
            "{\"id\":\"chatcmpl_1\",\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}",
        ],
    );
    let client = OpenAiCompatibleModelClient::new(ModelProviderRegistry::new(), transport);
    let request = build_model_request("custom.openai_compatible");
    let mut events = vec![];

    let response = client
        .generate_stream(&request, &mut |event| events.push(event))
        .await
        .expect("stream generate should succeed");

    assert_eq!(response.generate_result.tool_calls.len(), 1);
    assert_eq!(
        response.generate_result.reasoning_content.as_deref(),
        Some("inspect the manifest")
    );
    assert_eq!(response.generate_result.tool_calls[0].id, "call_1");
    assert_eq!(response.generate_result.input_tokens, Some(42));
    assert_eq!(response.generate_result.prompt_cache_hit_tokens, Some(40));
    assert_eq!(response.generate_result.prompt_cache_miss_tokens, Some(2));
    assert_eq!(events.len(), 4);
    assert!(matches!(
        &events[0],
        ModelClientStreamEvent::Status {
            message: None,
            process_state: RuntimeProcessState::Thinking,
        }
    ));
    assert!(matches!(
        &events[1],
        ModelClientStreamEvent::ToolCallPreparing { name } if name == "bash"
    ));
    assert!(matches!(
        &events[2],
        ModelClientStreamEvent::ToolCallReady {
            call_id,
            args_json,
            ..
        } if call_id == "call_1" && args_json == "{\"command\":\"rg --files -g README.md\"}"
    ));
    assert!(matches!(events[3], ModelClientStreamEvent::Done { .. }));
}

#[tokio::test]
async fn openai_compatible_stream_projects_reasoning_as_one_thinking_status() {
    let transport = MockJsonHttpTransport::with_sse(
        Ok(JsonHttpResponse {
            status_code: 200,
            headers: HashMap::new(),
            body_json: String::new(),
        }),
        vec![
            r#"{"choices":[{"delta":{"reasoning":"inspect "}}]}"#,
            r#"{"choices":[{"delta":{"reasoning":"the request"}}]}"#,
            r#"{"choices":[{"delta":{"content":"done"}}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
        ],
    );
    let client = OpenAiCompatibleModelClient::new(ModelProviderRegistry::new(), transport);
    let mut events = vec![];

    let response = client
        .generate_stream(
            &build_model_request("custom.openai_compatible"),
            &mut |event| events.push(event),
        )
        .await
        .expect("stream generate should succeed");

    assert_eq!(
        response.generate_result.reasoning_content.as_deref(),
        Some("inspect the request")
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    ModelClientStreamEvent::Status {
                        message: None,
                        process_state: RuntimeProcessState::Thinking,
                    }
                )
            })
            .count(),
        1
    );
}

#[tokio::test]
async fn openai_compatible_stream_rejects_reasoning_field_switch() {
    let transport = MockJsonHttpTransport::with_sse(
        Ok(JsonHttpResponse {
            status_code: 200,
            headers: HashMap::new(),
            body_json: String::new(),
        }),
        vec![
            r#"{"choices":[{"delta":{"reasoning":"inspect"}}]}"#,
            r#"{"choices":[{"delta":{"reasoning_content":"alternate"}}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
        ],
    );
    let client = OpenAiCompatibleModelClient::new(ModelProviderRegistry::new(), transport);

    let error = client
        .generate_stream(
            &build_model_request("custom.openai_compatible"),
            &mut |_| {},
        )
        .await
        .expect_err("reasoning field switch must fail loudly");

    assert_eq!(error.kind, ModelClientErrorKind::Provider);
    assert!(error.message.contains("changed reasoning field names"));
}

#[tokio::test]
async fn openai_compatible_stream_rejects_noncanonical_tool_identity_before_execution() {
    let transport = MockJsonHttpTransport::with_sse(
        Ok(JsonHttpResponse {
            status_code: 200,
            headers: HashMap::new(),
            body_json: String::new(),
        }),
        vec![
            r#"{"id":"chatcmpl_exact","choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_exact","function":{"name":" read ","arguments":"{\"path\":\"README.md\"}"}}]}}]}"#,
            r#"{"id":"chatcmpl_exact","choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
        ],
    );
    let client = OpenAiCompatibleModelClient::new(ModelProviderRegistry::new(), transport);
    let request = build_model_request("custom.openai_compatible");
    let mut events = Vec::new();
    let error = client
        .generate_stream(&request, &mut |event| events.push(event))
        .await
        .expect_err("noncanonical provider tool identity must loud-fail");

    assert_eq!(
        error.provider_code.as_deref(),
        Some("invalid_tool_call_identity")
    );
    assert!(!events
        .iter()
        .any(|event| matches!(event, ModelClientStreamEvent::ToolCallPreparing { .. })));
    assert!(!events
        .iter()
        .any(|event| matches!(event, ModelClientStreamEvent::ToolCallReady { .. })));
}

#[tokio::test]
async fn openai_compatible_malformed_tool_arguments_do_not_commit_completion_events() {
    let malformed_chunks = vec![
        json!({
            "id": "chatcmpl_malformed",
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_malformed",
                        "function": {
                            "name": "write",
                            "arguments": "{\"path\":\"report.py\",\"content\":\"unterminated"
                        }
                    }]
                }
            }]
        })
        .to_string(),
        json!({
            "id": "chatcmpl_malformed",
            "choices": [{"delta": {}, "finish_reason": "tool_calls"}]
        })
        .to_string(),
    ];
    let transport = MockJsonHttpTransport::with_sse_response_chunks(
        vec![
            Ok(JsonHttpResponse {
                status_code: 200,
                headers: HashMap::new(),
                body_json: String::new(),
            }),
            Ok(JsonHttpResponse {
                status_code: 200,
                headers: HashMap::new(),
                body_json: String::new(),
            }),
        ],
        vec![malformed_chunks.clone(), malformed_chunks],
    );
    let client = OpenAiCompatibleModelClient::new(ModelProviderRegistry::new(), transport);
    let request = build_model_request("custom.openai_compatible");
    let mut events = vec![];

    let error = client
        .generate_stream(&request, &mut |event| events.push(event))
        .await
        .expect_err("malformed terminal tool arguments must not commit");

    assert_eq!(
        error.provider_code.as_deref(),
        Some("malformed_tool_call_arguments")
    );
    assert_eq!(error.provider_attempts, 2);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                event,
                ModelClientStreamEvent::ToolCallPreparing { name } if name == "write"
            ))
            .count(),
        2,
        "each retry attempt should restore the bounded preparing status"
    );
    assert!(events.iter().all(|event| matches!(
        event,
        ModelClientStreamEvent::ToolCallPreparing { name } if name == "write"
    ) || matches!(
        event,
        ModelClientStreamEvent::Status {
            process_state: RuntimeProcessState::Retrying,
            ..
        }
    )));
    assert!(!events.iter().any(|event| matches!(
        event,
        ModelClientStreamEvent::ToolCallReady { .. } | ModelClientStreamEvent::Done { .. }
    )));
}

#[tokio::test]
async fn openai_compatible_stream_retry_replaces_partial_text_and_commits_only_valid_tools() {
    let response = || {
        Ok(JsonHttpResponse {
            status_code: 200,
            headers: HashMap::new(),
            body_json: String::new(),
        })
    };
    let malformed_chunks = vec![
        json!({
            "id": "chatcmpl_first",
            "choices": [{"delta": {
                "content": "draft",
                "tool_calls": [{
                    "index": 0,
                    "id": "call_bad",
                    "function": {"name": "edit", "arguments": "{\"path\":\"a.py\",\"edits\":["}
                }]
            }}]
        })
        .to_string(),
        json!({
            "id": "chatcmpl_first",
            "choices": [{"delta": {}, "finish_reason": "tool_calls"}]
        })
        .to_string(),
    ];
    let valid_chunks = vec![
        json!({
            "id": "chatcmpl_second",
            "choices": [{"delta": {"tool_calls": [{
                "index": 0,
                "id": "call_good",
                "function": {"name": "read", "arguments": "{\"path\":\"a.py\"}"}
            }]}}]
        })
        .to_string(),
        json!({
            "id": "chatcmpl_second",
            "choices": [{"delta": {}, "finish_reason": "tool_calls"}]
        })
        .to_string(),
    ];
    let transport = MockJsonHttpTransport::with_sse_response_chunks(
        vec![response(), response()],
        vec![malformed_chunks, valid_chunks],
    );
    let client = OpenAiCompatibleModelClient::new(ModelProviderRegistry::new(), transport);
    let request = build_model_request("custom.openai_compatible");
    let mut events = Vec::new();

    let result = client
        .generate_stream(&request, &mut |event| events.push(event))
        .await
        .expect("second complete response should succeed");

    assert_eq!(result.provider_attempts, 2);
    assert_eq!(result.generate_result.tool_calls[0].id, "call_good");
    assert!(events.iter().any(|event| matches!(
        event,
        ModelClientStreamEvent::ReplaceContent { content } if content.is_empty()
    )));
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, ModelClientStreamEvent::ToolCallReady { .. }))
            .count(),
        1
    );
    assert!(events.iter().any(|event| matches!(
        event,
        ModelClientStreamEvent::ToolCallReady { call_id, .. } if call_id == "call_good"
    )));
}

#[tokio::test]
async fn openai_compatible_fragmented_tool_arguments_emit_bounded_events() {
    let mut chunks = vec![json!({
        "id": "chatcmpl_fragmented",
        "choices": [{
            "delta": {
                "tool_calls": [{
                    "index": 0,
                    "id": "call_fragmented",
                    "function": {
                        "name": "bash",
                        "arguments": "{\"command\":\""
                    }
                }]
            }
        }]
    })
    .to_string()];
    chunks.extend((0..10_000).map(|_| {
        json!({
            "id": "chatcmpl_fragmented",
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "function": {"arguments": "a"}
                    }]
                }
            }]
        })
        .to_string()
    }));
    chunks.push(
        json!({
            "id": "chatcmpl_fragmented",
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "function": {"arguments": "\"}"}
                    }]
                }
            }]
        })
        .to_string(),
    );
    chunks.push(
        json!({
            "id": "chatcmpl_fragmented",
            "choices": [{"delta": {}, "finish_reason": "tool_calls"}]
        })
        .to_string(),
    );
    let transport = MockJsonHttpTransport::with_sse_response_chunks(
        vec![Ok(JsonHttpResponse {
            status_code: 200,
            headers: HashMap::new(),
            body_json: String::new(),
        })],
        vec![chunks],
    );
    let client = OpenAiCompatibleModelClient::new(ModelProviderRegistry::new(), transport);
    let request = build_model_request("custom.openai_compatible");
    let mut events = vec![];

    let response = client
        .generate_stream(&request, &mut |event| events.push(event))
        .await
        .expect("fragmented tool arguments should complete");

    assert_eq!(response.generate_result.tool_calls.len(), 1);
    assert_eq!(response.generate_result.tool_calls[0].id, "call_fragmented");
    assert_eq!(events.len(), 3);
    assert!(matches!(
        &events[0],
        ModelClientStreamEvent::ToolCallPreparing { name } if name == "bash"
    ));
    assert!(matches!(
        &events[1],
        ModelClientStreamEvent::ToolCallReady { call_id, .. }
            if call_id == "call_fragmented"
    ));
    assert!(matches!(events[2], ModelClientStreamEvent::Done { .. }));
}

#[tokio::test]
async fn openai_compatible_stream_projects_provider_waiting_status() {
    let provider_waiting = serde_json::json!({
        "type": MODEL_PROVIDER_WAITING_STREAM_EVENT_TYPE,
        "processState": RuntimeProcessState::ProviderWaiting.as_str(),
    })
    .to_string();
    let transport = MockJsonHttpTransport::with_sse(
        Ok(JsonHttpResponse {
            status_code: 200,
            headers: HashMap::new(),
            body_json: String::new(),
        }),
        vec![
            provider_waiting.as_str(),
            "{\"id\":\"chatcmpl_1\",\"choices\":[{\"delta\":{\"content\":\"done\"},\"finish_reason\":\"stop\"}]}",
        ],
    );
    let client = OpenAiCompatibleModelClient::new(ModelProviderRegistry::new(), transport);
    let request = build_model_request("custom.openai_compatible");
    let mut events = vec![];

    let response = client
        .generate_stream(&request, &mut |event| events.push(event))
        .await
        .expect("stream generate should succeed");

    assert_eq!(response.generate_result.content, "done");
    assert!(events.iter().any(|event| matches!(
        event,
        ModelClientStreamEvent::Status {
            message,
            process_state,
        } if message.is_none()
            && *process_state == RuntimeProcessState::ProviderWaiting
    )));
}

#[tokio::test]
async fn openai_compatible_stream_retries_after_provider_waiting_only_failure() {
    let provider_waiting = serde_json::json!({
        "type": MODEL_PROVIDER_WAITING_STREAM_EVENT_TYPE,
        "processState": RuntimeProcessState::ProviderWaiting.as_str(),
    })
    .to_string();
    let transport = MockJsonHttpTransport::with_sse_response_chunks(
        vec![
            Err("stream timeout after provider keep-alive".to_string()),
            Ok(JsonHttpResponse {
                status_code: 200,
                headers: HashMap::new(),
                body_json: String::new(),
            }),
        ],
        vec![
            vec![provider_waiting.clone()],
            vec![
                provider_waiting,
                "{\"id\":\"chatcmpl_1\",\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}".to_string(),
            ],
        ],
    );
    let client = OpenAiCompatibleModelClient::new(ModelProviderRegistry::new(), transport);
    let mut request = build_model_request("custom.openai_compatible");
    request.session_config.max_retries = 1;
    request.session_config.retry_backoff_ms = 0;
    let mut events = vec![];

    let response = client
        .generate_stream(&request, &mut |event| events.push(event))
        .await
        .expect("provider waiting heartbeat should not prevent retry");

    assert_eq!(response.generate_result.content, "ok");
    assert_eq!(client.transport.take_requests().len(), 2);
}

#[tokio::test]
async fn openai_compatible_stream_discards_tentative_tool_events_before_retry() {
    let first_attempt_chunks = vec![
        json!({
            "id": "chatcmpl_first",
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_first",
                        "function": {"name": "bash", "arguments": "{\"command\":\"touch first\"}"}
                    }]
                }
            }]
        })
        .to_string(),
        json!({
            "id": "chatcmpl_first",
            "choices": [{"delta": {}, "finish_reason": "tool_calls"}]
        })
        .to_string(),
    ];
    let second_attempt_chunks = vec![
        json!({
            "id": "chatcmpl_second",
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_second",
                        "function": {"name": "read", "arguments": "{\"path\":\"Cargo.toml\"}"}
                    }]
                }
            }]
        })
        .to_string(),
        json!({
            "id": "chatcmpl_second",
            "choices": [{"delta": {}, "finish_reason": "tool_calls"}]
        })
        .to_string(),
    ];
    let transport = MockJsonHttpTransport::with_sse_response_chunks(
        vec![
            Err("socket connection was closed".to_string()),
            Ok(JsonHttpResponse {
                status_code: 200,
                headers: HashMap::new(),
                body_json: "transport summary must not replace adapter state".to_string(),
            }),
        ],
        vec![first_attempt_chunks, second_attempt_chunks],
    );
    let client = OpenAiCompatibleModelClient::new(ModelProviderRegistry::new(), transport);
    let mut request = build_model_request("custom.openai_compatible");
    request.session_config.max_retries = 1;
    request.session_config.retry_backoff_ms = 0;
    let mut events = Vec::new();

    let response = client
        .generate_stream(&request, &mut |event| events.push(event))
        .await
        .expect("retry should use only the completed second response");

    assert_eq!(client.transport.take_requests().len(), 2);
    assert_eq!(response.provider_attempts, 2);
    assert_eq!(response.generate_result.tool_calls.len(), 1);
    assert_eq!(response.generate_result.tool_calls[0].id, "call_second");
    assert!(events.iter().any(|event| matches!(
        event,
        ModelClientStreamEvent::ToolCallReady { call_id, .. } if call_id == "call_second"
    )));
    assert!(!events.iter().any(|event| matches!(
        event,
        ModelClientStreamEvent::ToolCallReady { call_id, .. }
            if call_id == "call_first"
    )));
}

#[tokio::test]
async fn openai_compatible_stream_retries_clean_early_end_before_terminal_event() {
    let transport = MockJsonHttpTransport::with_sse_response_chunks(
        vec![
            Ok(JsonHttpResponse {
                status_code: 200,
                headers: HashMap::new(),
                body_json: String::new(),
            }),
            Ok(JsonHttpResponse {
                status_code: 200,
                headers: HashMap::new(),
                body_json: String::new(),
            }),
        ],
        vec![
            Vec::new(),
            vec![json!({
                "id": "chatcmpl_terminal",
                "choices": [{"delta": {"content": "ok"}, "finish_reason": "stop"}]
            })
            .to_string()],
        ],
    );
    let client = OpenAiCompatibleModelClient::new(ModelProviderRegistry::new(), transport);
    let mut request = build_model_request("custom.openai_compatible");
    request.session_config.max_retries = 1;
    request.session_config.retry_backoff_ms = 0;

    let response = client
        .generate_stream(&request, &mut |_| {})
        .await
        .expect("clean early end without output should retry");

    assert_eq!(response.generate_result.content, "ok");
    assert_eq!(response.provider_attempts, 2);
    assert_eq!(client.transport.take_requests().len(), 2);
}

#[tokio::test]
async fn openai_compatible_stream_retries_reasoning_only_empty_final_response() {
    let transport = MockJsonHttpTransport::with_sse_response_chunks(
        vec![
            Ok(JsonHttpResponse {
                status_code: 200,
                headers: HashMap::new(),
                body_json: String::new(),
            }),
            Ok(JsonHttpResponse {
                status_code: 200,
                headers: HashMap::new(),
                body_json: String::new(),
            }),
        ],
        vec![
            vec![
                json!({
                    "id": "chatcmpl_empty",
                    "choices": [{"delta": {"reasoning_content": "partial analysis"}}]
                })
                .to_string(),
                json!({
                    "id": "chatcmpl_empty",
                    "choices": [{"delta": {}, "finish_reason": "stop"}]
                })
                .to_string(),
            ],
            vec![json!({
                "id": "chatcmpl_recovered",
                "choices": [{
                    "delta": {"content": "recovered answer"},
                    "finish_reason": "stop"
                }]
            })
            .to_string()],
        ],
    );
    let client = OpenAiCompatibleModelClient::new(ModelProviderRegistry::new(), transport);
    let mut request = build_model_request("custom.openai_compatible");
    request.session_config.max_retries = 1;
    request.session_config.retry_backoff_ms = 0;
    let mut events = Vec::new();

    let response = client
        .generate_stream(&request, &mut |event| events.push(event))
        .await
        .expect("reasoning-only empty final stream should retry");

    assert_eq!(response.generate_result.content, "recovered answer");
    assert_eq!(response.provider_attempts, 2);
    assert_eq!(client.transport.take_requests().len(), 2);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, ModelClientStreamEvent::Done { .. }))
            .count(),
        1
    );
    assert!(events.iter().any(|event| matches!(
        event,
        ModelClientStreamEvent::Status {
            process_state: RuntimeProcessState::Retrying,
            ..
        }
    )));
}

#[tokio::test]
async fn openai_compatible_stream_loud_fails_after_empty_final_retry_exhaustion() {
    let empty_attempt = || {
        vec![
            json!({
                "id": "chatcmpl_empty",
                "choices": [{"delta": {"reasoning_content": "partial analysis"}}]
            })
            .to_string(),
            json!({
                "id": "chatcmpl_empty",
                "choices": [{"delta": {}, "finish_reason": "stop"}]
            })
            .to_string(),
        ]
    };
    let transport = MockJsonHttpTransport::with_sse_response_chunks(
        vec![
            Ok(JsonHttpResponse {
                status_code: 200,
                headers: HashMap::new(),
                body_json: String::new(),
            }),
            Ok(JsonHttpResponse {
                status_code: 200,
                headers: HashMap::new(),
                body_json: String::new(),
            }),
        ],
        vec![empty_attempt(), empty_attempt()],
    );
    let client = OpenAiCompatibleModelClient::new(ModelProviderRegistry::new(), transport);
    let mut request = build_model_request("custom.openai_compatible");
    request.session_config.max_retries = 1;
    request.session_config.retry_backoff_ms = 0;
    let mut events = Vec::new();

    let error = client
        .generate_stream(&request, &mut |event| events.push(event))
        .await
        .expect_err("repeated empty final responses must fail loudly");

    assert_eq!(
        error.kind,
        ModelClientErrorKind::ProviderResponseInterrupted
    );
    assert_eq!(error.provider_code.as_deref(), Some("empty_final_response"));
    assert_eq!(error.provider_attempts, 2);
    assert!(error.message.contains("finish_reason=stop"));
    assert!(!events
        .iter()
        .any(|event| matches!(event, ModelClientStreamEvent::Done { .. })));
}

#[tokio::test]
async fn openai_compatible_client_maps_provider_queue_error() {
    let rate_limit_response = || JsonHttpResponse {
        status_code: 429,
        headers: HashMap::new(),
        body_json: "{\"error\":{\"message\":\"rate limited\",\"code\":\"rate_limit_exceeded\"}}"
            .to_string(),
    };
    let transport = MockJsonHttpTransport::with_responses(vec![
        Ok(rate_limit_response()),
        Ok(rate_limit_response()),
    ]);
    let client = OpenAiCompatibleModelClient::new(ModelProviderRegistry::new(), transport);
    let mut request = build_model_request("custom.openai_compatible");
    request.session_config.max_retries = 1;
    request.session_config.retry_backoff_ms = 0;

    let error = client
        .generate(&request)
        .await
        .expect_err("generate should return rate limit error");
    assert_eq!(error.kind, ModelClientErrorKind::ProviderBusyOrRateLimited);
    assert_eq!(error.kind.as_str(), "provider_busy_or_rate_limited");
    assert!(error.retryable);
    assert_eq!(error.provider_code.as_deref(), Some("rate_limit_exceeded"));
}

#[tokio::test]
async fn openai_compatible_client_maps_auth_and_provider_failures() {
    let auth_error = super::map_openai_compatible_http_error(
        401,
        "{\"error\":{\"message\":\"invalid api key\",\"code\":\"invalid_api_key\"}}",
    );
    assert_eq!(auth_error.kind, ModelClientErrorKind::AuthFailed);
    assert_eq!(auth_error.kind.as_str(), "auth_failed");
    assert!(!auth_error.retryable);
    assert_eq!(auth_error.provider_code.as_deref(), Some("invalid_api_key"));

    let provider_error = super::map_openai_compatible_http_error(
        500,
        "{\"error\":{\"message\":\"provider overloaded\",\"code\":\"server_error\"}}",
    );
    assert_eq!(
        provider_error.kind,
        ModelClientErrorKind::ProviderUnavailable
    );
    assert_eq!(provider_error.kind.as_str(), "provider_unavailable");
    assert!(provider_error.retryable);
}

#[tokio::test]
async fn deepseek_insufficient_system_resource_finish_is_retryable() {
    let response = || {
        Ok(JsonHttpResponse {
            status_code: 200,
            headers: HashMap::new(),
            body_json: json!({
                "id": "deepseek_resource_error",
                "choices": [{
                    "finish_reason": "insufficient_system_resource",
                    "message": {"content": ""}
                }]
            })
            .to_string(),
        })
    };
    let transport = MockJsonHttpTransport::with_responses(vec![response(), response()]);
    let client = OpenAiCompatibleModelClient::new(deepseek_test_registry(), transport);
    let request = build_deepseek_request(Some("high"));

    let error = client
        .generate(&request)
        .await
        .expect_err("resource exhaustion must not be accepted as a completed answer");

    assert_eq!(error.kind, ModelClientErrorKind::ProviderUnavailable);
    assert!(error.retryable);
    assert_eq!(
        error.provider_code.as_deref(),
        Some("insufficient_system_resource")
    );
    assert_eq!(error.provider_attempts, 2);
}

#[tokio::test]
async fn deepseek_stream_insufficient_system_resource_finish_is_retryable() {
    let response = || {
        Ok(JsonHttpResponse {
            status_code: 200,
            headers: HashMap::new(),
            body_json: String::new(),
        })
    };
    let resource_chunk =
        r#"{"id":"deepseek_resource_stream","choices":[{"delta":{},"finish_reason":"insufficient_system_resource"}]}"#
            .to_string();
    let transport = MockJsonHttpTransport::with_sse_response_chunks(
        vec![response(), response()],
        vec![vec![resource_chunk.clone()], vec![resource_chunk]],
    );
    let client = OpenAiCompatibleModelClient::new(deepseek_test_registry(), transport);
    let request = build_deepseek_request(Some("max"));
    let mut events = Vec::new();

    let error = client
        .generate_stream(&request, &mut |event| events.push(event))
        .await
        .expect_err("stream resource exhaustion must be retryable");

    assert_eq!(error.kind, ModelClientErrorKind::ProviderUnavailable);
    assert!(error.retryable);
    assert_eq!(
        error.provider_code.as_deref(),
        Some("insufficient_system_resource")
    );
    assert_eq!(error.provider_attempts, 2);
    assert!(
        events.iter().all(|event| matches!(
            event,
            ModelClientStreamEvent::Status {
                process_state: RuntimeProcessState::Retrying,
                ..
            }
        )),
        "failed response must not publish completion events"
    );
}

#[test]
fn deepseek_http_resource_code_overrides_status_classification() {
    let error = super::map_openai_compatible_http_error(
        400,
        r#"{"error":{"message":"inference resources exhausted","code":"insufficient_system_resource"}}"#,
    );

    assert_eq!(error.kind, ModelClientErrorKind::ProviderUnavailable);
    assert!(error.retryable);
    assert_eq!(
        error.provider_code.as_deref(),
        Some("insufficient_system_resource")
    );
}

#[tokio::test]
async fn openai_responses_client_maps_provider_queue_error() {
    let error = super::map_openai_responses_http_error(
        429,
        "{\"error\":{\"message\":\"too many requests\",\"type\":\"rate_limit_exceeded\"}}",
    );

    assert_eq!(error.kind, ModelClientErrorKind::ProviderBusyOrRateLimited);
    assert_eq!(error.kind.as_str(), "provider_busy_or_rate_limited");
    assert!(error.retryable);
    assert_eq!(error.provider_code.as_deref(), Some("rate_limit_exceeded"));
}

#[tokio::test]
async fn openai_compatible_client_maps_response_body_interrupt() {
    let transport = MockJsonHttpTransport::with_responses(vec![
        Err("read http body failed: error decoding response body".to_string()),
        Err("read http body failed: error decoding response body".to_string()),
    ]);
    let client = OpenAiCompatibleModelClient::new(ModelProviderRegistry::new(), transport);
    let request = build_model_request("custom.openai_compatible");

    let error = client
        .generate(&request)
        .await
        .expect_err("generate should classify response body interruption");

    assert_eq!(
        error.kind,
        ModelClientErrorKind::ProviderResponseInterrupted
    );
    assert_eq!(error.kind.as_str(), "provider_response_interrupted");
    assert!(error.retryable);
}

#[tokio::test]
async fn openai_compatible_client_retries_retryable_http_status_before_success() {
    let transport = MockJsonHttpTransport::with_responses(vec![
        Ok(JsonHttpResponse {
            status_code: 429,
            headers: HashMap::new(),
            body_json: "{\"error\":{\"message\":\"rate limited\"}}".to_string(),
        }),
        Ok(JsonHttpResponse {
            status_code: 200,
            headers: HashMap::new(),
            body_json:
                "{\"id\":\"resp_status_retry\",\"choices\":[{\"message\":{\"content\":\"ok\"}}]}"
                    .to_string(),
        }),
    ]);
    let client = OpenAiCompatibleModelClient::new(ModelProviderRegistry::new(), transport);
    let mut request = build_model_request("custom.openai_compatible");
    request.session_config.max_retries = 1;
    request.session_config.retry_backoff_ms = 0;

    let response = client
        .generate(&request)
        .await
        .expect("status retry should succeed");

    assert_eq!(response.generate_result.content, "ok");
    assert_eq!(client.transport.take_requests().len(), 2);
}

#[tokio::test]
async fn openai_responses_client_builds_responses_request() {
    let transport = MockJsonHttpTransport::with_response(Ok(JsonHttpResponse {
        status_code: 200,
        headers: HashMap::new(),
        body_json: "{\"id\":\"resp_3\",\"output_text\":\"done\",\"usage\":{\"input_tokens\":12,\"output_tokens\":6,\"total_tokens\":18}}".to_string(),
    }));
    let client = OpenAiResponsesModelClient::new(ModelProviderRegistry::new(), transport);
    std::env::set_var("OPENAI_API_KEY", "test-openai-key");

    let mut request = build_model_request("openai.default");
    request.session_config.thinking_mode = Some("high".to_string());
    let response = client
        .generate(&request)
        .await
        .expect("generate should succeed");

    assert_eq!(response.generate_result.content, "done");
    let requests = client.transport.take_requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].url, "http://localhost:4000/v1/responses");
    assert!(requests[0]
        .body_json
        .contains("\"instructions\":\"You are Centaeris.\""));
    assert!(requests[0].body_json.contains("\"tool_choice\":\"auto\""));
    assert!(requests[0]
        .body_json
        .contains("\"reasoning\":{\"effort\":\"high\"}"));
}

#[test]
fn openai_responses_rejects_unknown_effort() {
    let error =
        build_openai_responses_reasoning(Some("banana")).expect_err("unknown effort must fail");
    assert_eq!(error.kind, ModelClientErrorKind::InvalidRequest);
}

#[tokio::test]
async fn openai_responses_client_serializes_tool_context_as_function_output() {
    let transport = MockJsonHttpTransport::with_response(Ok(JsonHttpResponse {
        status_code: 200,
        headers: HashMap::new(),
        body_json:
            "{\"id\":\"resp_tool_ctx\",\"output_text\":\"done\",\"usage\":{\"input_tokens\":4,\"output_tokens\":1,\"total_tokens\":5}}"
                .to_string(),
    }));
    let client = OpenAiResponsesModelClient::new(ModelProviderRegistry::new(), transport);
    std::env::set_var("OPENAI_API_KEY", "test-openai-key");
    let mut request = build_model_request("openai.default");
    request.prepared_prompt.messages.push(ModelMessageV1 {
        message_id: "msg-assistant-1".to_string(),
        role: crate::model::prepared_prompt::ModelMessageRoleV1::Assistant,
        content: String::new(),
        tool_calls: vec![crate::model::prepared_prompt::ModelToolCallV1 {
            id: "call_1".to_string(),
            name: "bash".to_string(),
            args_json: "{\"command\":\"pwd\"}".to_string(),
        }],
        tool_call_id: None,
        reasoning_content: None,
    });
    request.prepared_prompt.messages.push(ModelMessageV1 {
        message_id: "msg-tool-1".to_string(),
        role: crate::model::prepared_prompt::ModelMessageRoleV1::Tool,
        content: "{\"stdout\":\"tool output\"}".to_string(),
        tool_calls: vec![],
        tool_call_id: Some("call_1".to_string()),
        reasoning_content: None,
    });

    client
        .generate(&request)
        .await
        .expect("generate should succeed");

    let requests = client.transport.take_requests();
    let body = serde_json::from_str::<serde_json::Value>(requests[0].body_json.as_str())
        .expect("parse responses body");
    let input = body
        .get("input")
        .and_then(serde_json::Value::as_array)
        .expect("input array");
    assert!(input.iter().any(|item| {
        item.get("type").and_then(serde_json::Value::as_str) == Some("function_call")
            && item.get("call_id").and_then(serde_json::Value::as_str) == Some("call_1")
            && item.get("name").and_then(serde_json::Value::as_str) == Some("bash")
            && item.get("arguments").and_then(serde_json::Value::as_str)
                == Some("{\"command\":\"pwd\"}")
    }));
    assert!(input.iter().any(|item| {
        item.get("type").and_then(serde_json::Value::as_str) == Some("function_call_output")
            && item.get("call_id").and_then(serde_json::Value::as_str) == Some("call_1")
            && item.get("output").and_then(serde_json::Value::as_str)
                == Some("{\"stdout\":\"tool output\"}")
    }));
}

#[tokio::test]
async fn openai_responses_client_parses_message_and_tool_calls() {
    let transport = MockJsonHttpTransport::with_response(Ok(JsonHttpResponse {
        status_code: 200,
        headers: HashMap::from([("x-request-id".to_string(), "req_123".to_string())]),
        body_json: "{\"output\":[{\"type\":\"reasoning\",\"summary\":[{\"text\":\"Need to inspect the file first.\"}]},{\"type\":\"function_call\",\"call_id\":\"call_7\",\"name\":\"file_read\",\"arguments\":\"{\\\"path\\\":\\\"README.md\\\"}\"},{\"type\":\"message\",\"content\":[{\"type\":\"output_text\",\"text\":\"I will inspect the file.\"}]}],\"usage\":{\"input_tokens\":14,\"output_tokens\":9,\"input_tokens_details\":{\"cached_tokens\":6}}}".to_string(),
    }));
    let client = OpenAiResponsesModelClient::new(ModelProviderRegistry::new(), transport);
    std::env::set_var("OPENAI_API_KEY", "test-openai-key");
    let request = build_model_request("openai.default");

    let response = client
        .generate(&request)
        .await
        .expect("generate should succeed");

    assert_eq!(response.provider_request_id.as_deref(), Some("req_123"));
    assert_eq!(response.generate_result.content, "I will inspect the file.");
    assert_eq!(
        response.generate_result.reasoning_content.as_deref(),
        Some("Need to inspect the file first.")
    );
    assert_eq!(response.generate_result.tool_calls.len(), 1);
    assert_eq!(response.generate_result.tool_calls[0].id, "call_7");
    assert_eq!(response.generate_result.tool_calls[0].name, "file_read");
    assert_eq!(
        response.generate_result.tool_calls[0].args_json,
        "{\"path\":\"README.md\"}"
    );
    assert_eq!(response.generate_result.input_tokens, Some(14));
    assert_eq!(response.generate_result.total_tokens, Some(23));
    assert_eq!(response.generate_result.prompt_cache_hit_tokens, Some(6));
    assert_eq!(response.generate_result.prompt_cache_miss_tokens, Some(8));
}

#[tokio::test]
async fn openai_responses_client_rejects_malformed_tool_arguments() {
    let malformed_response = JsonHttpResponse {
        status_code: 200,
        headers: HashMap::new(),
        body_json: json!({
            "id": "resp_malformed_tool",
            "output": [{
                "type": "function_call",
                "call_id": "call_malformed",
                "name": "write",
                "arguments": "{\"path\":\"report.py\",\"content\":\"unterminated"
            }]
        })
        .to_string(),
    };
    let transport = MockJsonHttpTransport::with_responses(vec![
        Ok(malformed_response.clone()),
        Ok(malformed_response),
    ]);
    let client = OpenAiResponsesModelClient::new(ModelProviderRegistry::new(), transport);
    std::env::set_var("OPENAI_API_KEY", "test-openai-key");
    let request = build_model_request("openai.default");

    let error = client
        .generate(&request)
        .await
        .expect_err("malformed responses tool arguments must fail at the provider boundary");

    assert_eq!(error.kind, ModelClientErrorKind::Provider);
    assert_eq!(
        error.provider_code.as_deref(),
        Some("malformed_tool_call_arguments")
    );
    assert!(!error.retryable);
    assert_eq!(error.provider_attempts, 2);
}

#[tokio::test]
async fn openai_responses_client_streams_text_deltas() {
    let transport = MockJsonHttpTransport::with_sse(
        Ok(JsonHttpResponse {
            status_code: 200,
            headers: HashMap::new(),
            body_json: String::new(),
        }),
        vec![
            "{\"type\":\"response.created\",\"response\":{\"id\":\"resp_stream_1\"}}",
            "{\"type\":\"response.output_text.delta\",\"delta\":\"Hello\"}",
            "{\"type\":\"response.output_text.delta\",\"delta\":\" world\"}",
            "{\"type\":\"response.completed\",\"response\":{\"id\":\"resp_stream_1\",\"output_text\":\"Hello world\",\"usage\":{\"input_tokens\":9,\"output_tokens\":2,\"total_tokens\":11}}}",
        ],
    );
    let client = OpenAiResponsesModelClient::new(ModelProviderRegistry::new(), transport);
    std::env::set_var("OPENAI_API_KEY", "test-openai-key");
    let request = build_model_request("openai.default");
    let mut events = vec![];

    let response = client
        .generate_stream(&request, &mut |event| events.push(event))
        .await
        .expect("stream generate should succeed");

    assert_eq!(response.generate_result.content, "Hello world");
    assert_eq!(events.len(), 4);
    assert!(matches!(events[0], ModelClientStreamEvent::Status { .. }));
    assert!(matches!(
        events[1],
        ModelClientStreamEvent::Token { ref content } if content == "Hello"
    ));
    assert!(matches!(
        events[2],
        ModelClientStreamEvent::Token { ref content } if content == " world"
    ));
    assert!(matches!(events[3], ModelClientStreamEvent::Done { .. }));
}

#[tokio::test]
async fn openai_responses_non_stream_output_limit_preserves_complete_tool_identity() {
    let raw_args = r#"{"path":"unfinished"#;
    let transport = MockJsonHttpTransport::with_response(Ok(JsonHttpResponse {
        status_code: 200,
        headers: HashMap::new(),
        body_json: json!({
            "id": "resp-capped",
            "status": "incomplete",
            "incomplete_details": {"reason": "max_output_tokens"},
            "output_text": "partial",
            "output": [{
                "type": "function_call",
                "call_id": "call-capped",
                "name": "read",
                "arguments": raw_args,
            }],
        })
        .to_string(),
    }));
    let client = OpenAiResponsesModelClient::new(ModelProviderRegistry::new(), transport);
    std::env::set_var("OPENAI_API_KEY", "test-openai-key");

    let error = client
        .generate(&build_model_request("openai.default"))
        .await
        .expect_err("capped Responses response must enter runtime recovery");

    assert_output_limit_tool_identity(&error, Some("call-capped"), Some("read"), raw_args);
}

#[tokio::test]
async fn openai_responses_non_stream_output_limit_does_not_synthesize_tool_identity() {
    let raw_args = r#"{"path":"unfinished"#;
    let transport = MockJsonHttpTransport::with_response(Ok(JsonHttpResponse {
        status_code: 200,
        headers: HashMap::new(),
        body_json: json!({
            "id": "resp-capped",
            "status": "incomplete",
            "incomplete_details": {"reason": "max_output_tokens"},
            "output": [{
                "type": "function_call",
                "arguments": raw_args,
            }],
        })
        .to_string(),
    }));
    let client = OpenAiResponsesModelClient::new(ModelProviderRegistry::new(), transport);
    std::env::set_var("OPENAI_API_KEY", "test-openai-key");

    let error = client
        .generate(&build_model_request("openai.default"))
        .await
        .expect_err("capped Responses response must enter runtime recovery");

    assert_output_limit_tool_identity(&error, None, None, raw_args);
}

#[tokio::test]
async fn openai_responses_output_limit_preserves_partial_stream_for_runtime_recovery() {
    let transport = MockJsonHttpTransport::with_sse(
        Ok(JsonHttpResponse {
            status_code: 200,
            headers: HashMap::new(),
            body_json: String::new(),
        }),
        vec![
            r#"{"type":"response.created","response":{"id":"resp_incomplete"}}"#,
            r#"{"type":"response.output_text.delta","delta":"partial"}"#,
            r#"{"type":"response.incomplete","response":{"id":"resp_incomplete","status":"incomplete","incomplete_details":{"reason":"max_output_tokens"},"output_text":"partial"}}"#,
        ],
    );
    let client = OpenAiResponsesModelClient::new(ModelProviderRegistry::new(), transport);
    std::env::set_var("OPENAI_API_KEY", "test-openai-key");
    let request = build_model_request("openai.default");
    let mut events = vec![];

    let error = client
        .generate_stream(&request, &mut |event| events.push(event))
        .await
        .expect_err("incomplete response must enter runtime recovery");

    assert_eq!(
        error.provider_code.as_deref(),
        Some("incomplete_output_token_limit")
    );
    assert_eq!(error.provider_attempts, 1);
    assert!(events.iter().any(|event| matches!(
        event,
        ModelClientStreamEvent::Token { content } if content == "partial"
    )));
    assert!(!events
        .iter()
        .any(|event| matches!(event, ModelClientStreamEvent::Done { .. })));
}

#[tokio::test]
async fn openai_responses_output_limit_does_not_synthesize_tool_identity() {
    let transport = MockJsonHttpTransport::with_sse(
        Ok(JsonHttpResponse {
            status_code: 200,
            headers: HashMap::new(),
            body_json: String::new(),
        }),
        vec![
            r#"{"type":"response.created","response":{"id":"resp_incomplete_tool"}}"#,
            r#"{"type":"response.output_item.added","item":{"id":"item_1","type":"function_call","name":"bash","arguments":""}}"#,
            r#"{"type":"response.function_call_arguments.delta","item_id":"item_1","delta":"{\"command\":\"unfinished"}"#,
            r#"{"type":"response.output_item.done","item":{"id":"item_1","type":"function_call","name":"bash","arguments":"{\"command\":\"unfinished"}}"#,
            r#"{"type":"response.incomplete","response":{"id":"resp_incomplete_tool","status":"incomplete","incomplete_details":{"reason":"max_output_tokens"},"output":[{"type":"function_call","name":"bash","arguments":"{\"command\":\"unfinished"}]}}"#,
        ],
    );
    let client = OpenAiResponsesModelClient::new(ModelProviderRegistry::new(), transport);
    std::env::set_var("OPENAI_API_KEY", "test-openai-key");
    let mut events = vec![];

    let error = client
        .generate_stream(&build_model_request("openai.default"), &mut |event| {
            events.push(event)
        })
        .await
        .expect_err("capped response must enter runtime recovery");

    assert_eq!(error.truncated_tool_calls.len(), 1);
    assert_eq!(error.truncated_tool_calls[0].call_id, None);
    assert_eq!(
        error.truncated_tool_calls[0].tool_name.as_deref(),
        Some("bash")
    );
    assert!(!events
        .iter()
        .any(|event| matches!(event, ModelClientStreamEvent::ToolCallReady { .. })));
}

#[tokio::test]
async fn openai_responses_stream_output_limit_preserves_complete_tool_identity_without_ready() {
    let raw_args = r#"{"path":"unfinished"#;
    let transport = MockJsonHttpTransport::with_sse(
        Ok(JsonHttpResponse {
            status_code: 200,
            headers: HashMap::new(),
            body_json: String::new(),
        }),
        vec![
            r#"{"type":"response.created","response":{"id":"resp-capped"}}"#,
            r#"{"type":"response.output_item.added","item":{"id":"item-1","type":"function_call","call_id":"call-capped","name":"read","arguments":""}}"#,
            r#"{"type":"response.function_call_arguments.delta","item_id":"item-1","delta":"{\"path\":\"unfinished"}"#,
            r#"{"type":"response.incomplete","response":{"id":"resp-capped","status":"incomplete","incomplete_details":{"reason":"max_output_tokens"},"output":[{"type":"function_call","call_id":"call-capped","name":"read","arguments":"{\"path\":\"unfinished"}]}}"#,
        ],
    );
    let client = OpenAiResponsesModelClient::new(ModelProviderRegistry::new(), transport);
    std::env::set_var("OPENAI_API_KEY", "test-openai-key");
    let mut events = Vec::new();

    let error = client
        .generate_stream(&build_model_request("openai.default"), &mut |event| {
            events.push(event)
        })
        .await
        .expect_err("capped Responses stream must enter runtime recovery");

    assert_output_limit_tool_identity(&error, Some("call-capped"), Some("read"), raw_args);
    assert!(!events.iter().any(|event| matches!(
        event,
        ModelClientStreamEvent::ToolCallReady { .. } | ModelClientStreamEvent::Done { .. }
    )));
}

#[tokio::test]
async fn openai_responses_client_projects_completed_tool_call_only() {
    let transport = MockJsonHttpTransport::with_sse(
        Ok(JsonHttpResponse {
            status_code: 200,
            headers: HashMap::new(),
            body_json: String::new(),
        }),
        vec![
            "{\"type\":\"response.created\",\"response\":{\"id\":\"resp_stream_tool\"}}",
            "{\"type\":\"response.output_item.added\",\"item\":{\"id\":\"item_1\",\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"bash\",\"arguments\":\"\"}}",
            "{\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"item_1\",\"delta\":\"{\\\"command\\\":\"}",
            "{\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"item_1\",\"delta\":\"\\\"rg --files -g README.md\\\"}\"}",
            "{\"type\":\"response.output_item.done\",\"item\":{\"id\":\"item_1\",\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"bash\",\"arguments\":\"{\\\"command\\\":\\\"rg --files -g README.md\\\"}\"}}",
            "{\"type\":\"response.completed\",\"response\":{\"id\":\"resp_stream_tool\",\"output\":[{\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"bash\",\"arguments\":\"{\\\"command\\\":\\\"rg --files -g README.md\\\"}\"}],\"usage\":{\"input_tokens\":9,\"output_tokens\":2,\"total_tokens\":11}}}",
        ],
    );
    let client = OpenAiResponsesModelClient::new(ModelProviderRegistry::new(), transport);
    std::env::set_var("OPENAI_API_KEY", "test-openai-key");
    let request = build_model_request("openai.default");
    let mut events = vec![];

    let response = client
        .generate_stream(&request, &mut |event| events.push(event))
        .await
        .expect("stream generate should succeed");

    assert_eq!(response.generate_result.tool_calls.len(), 1);
    assert_eq!(response.generate_result.tool_calls[0].id, "call_1");
    assert_eq!(events.len(), 4);
    assert!(matches!(events[0], ModelClientStreamEvent::Status { .. }));
    assert!(matches!(
        &events[1],
        ModelClientStreamEvent::ToolCallPreparing { name } if name == "bash"
    ));
    assert!(matches!(
        &events[2],
        ModelClientStreamEvent::ToolCallReady {
            call_id,
            args_json,
            ..
        } if call_id == "call_1" && args_json == "{\"command\":\"rg --files -g README.md\"}"
    ));
    assert!(matches!(events[3], ModelClientStreamEvent::Done { .. }));
}

#[tokio::test]
async fn openai_compatible_malformed_sse_frame_invalidates_attempt_and_replaces_partial_text() {
    let response = || {
        Ok(JsonHttpResponse {
            status_code: 200,
            headers: HashMap::new(),
            body_json: String::new(),
        })
    };
    let transport = MockJsonHttpTransport::with_sse_response_chunks(
        vec![response(), response()],
        vec![
            vec![
                r#"{"id":"bad-attempt","choices":[{"delta":{"content":"draft"}}]}"#.to_string(),
                r#"{"banana":"#.to_string(),
                r#"{"id":"bad-attempt","choices":[{"delta":{"content":"ignored"},"finish_reason":"stop"}]}"#.to_string(),
            ],
            vec![r#"{"id":"good-attempt","choices":[{"delta":{"content":"recovered"},"finish_reason":"stop"}]}"#.to_string()],
        ],
    );
    let client = OpenAiCompatibleModelClient::new(ModelProviderRegistry::new(), transport);
    let mut request = build_model_request("custom.openai_compatible");
    request.session_config.retry_backoff_ms = 0;
    let mut events = Vec::new();

    let response = client
        .generate_stream(&request, &mut |event| events.push(event))
        .await
        .expect("second SSE attempt should replace the malformed attempt");

    assert_eq!(response.provider_attempts, 2);
    assert_eq!(response.generate_result.content, "recovered");
    assert!(events.iter().any(|event| matches!(
        event,
        ModelClientStreamEvent::ReplaceContent { content } if content.is_empty()
    )));
    assert!(!events.iter().any(|event| matches!(
        event,
        ModelClientStreamEvent::Token { content } if content == "ignored"
    )));
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, ModelClientStreamEvent::Done { .. }))
            .count(),
        1
    );
}

#[tokio::test]
async fn openai_responses_malformed_sse_frame_loud_fails_without_terminal_commit() {
    let response = || {
        Ok(JsonHttpResponse {
            status_code: 200,
            headers: HashMap::new(),
            body_json: String::new(),
        })
    };
    let bad_attempt = || {
        vec![
            r#"{"type":"response.output_text.delta","delta":"draft"}"#.to_string(),
            r#"{"banana":"#.to_string(),
            r#"{"type":"response.completed","response":{"id":"ignored","output_text":"ignored"}}"#
                .to_string(),
        ]
    };
    let transport = MockJsonHttpTransport::with_sse_response_chunks(
        vec![response(), response()],
        vec![bad_attempt(), bad_attempt()],
    );
    let client = OpenAiResponsesModelClient::new(ModelProviderRegistry::new(), transport);
    std::env::set_var("OPENAI_API_KEY", "test-openai-key");
    let mut request = build_model_request("openai.default");
    request.session_config.retry_backoff_ms = 0;
    let mut events = Vec::new();

    let error = client
        .generate_stream(&request, &mut |event| events.push(event))
        .await
        .expect_err("malformed SSE frames must exhaust the existing retry budget");

    assert_eq!(
        error.kind,
        ModelClientErrorKind::ProviderResponseInterrupted
    );
    assert_eq!(error.provider_code.as_deref(), Some("malformed_sse_frame"));
    assert_eq!(error.provider_attempts, 2);
    assert!(!error.retryable);
    assert!(error.message.contains("wireApi=openai-responses"));
    assert!(error.message.contains("frameBytes="));
    assert!(error.message.contains("frameSha256=sha256:"));
    assert!(error.message.contains("jsonLine=1"));
    assert!(error.message.contains("jsonColumn="));
    assert!(error.message.contains("attempt=2"));
    assert!(!error.message.contains(r#"{"banana":"#));
    assert!(!events.iter().any(|event| matches!(
        event,
        ModelClientStreamEvent::ToolCallReady { .. } | ModelClientStreamEvent::Done { .. }
    )));
}

#[tokio::test]
async fn anthropic_malformed_sse_frame_invalidates_attempt_and_replaces_partial_text() {
    let response = || {
        Ok(JsonHttpResponse {
            status_code: 200,
            headers: HashMap::new(),
            body_json: String::new(),
        })
    };
    let transport = MockJsonHttpTransport::with_sse_response_chunks(
        vec![response(), response()],
        vec![
            vec![
                r#"{"type":"message_start","message":{"id":"bad-attempt","usage":{"input_tokens":1,"output_tokens":0}}}"#.to_string(),
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"draft"}}"#.to_string(),
                r#"{"banana":"#.to_string(),
                r#"{"type":"message_stop"}"#.to_string(),
            ],
            vec![
                r#"{"type":"message_start","message":{"id":"good-attempt","usage":{"input_tokens":1,"output_tokens":0}}}"#.to_string(),
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"recovered"}}"#.to_string(),
                r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"input_tokens":1,"output_tokens":1}}"#.to_string(),
                r#"{"type":"message_stop"}"#.to_string(),
            ],
        ],
    );
    let client = AnthropicMessagesModelClient::new(anthropic_test_registry(), &transport);
    let mut request = build_anthropic_request();
    request.session_config.retry_backoff_ms = 0;
    let mut events = Vec::new();

    let response = client
        .generate_stream(&request, &mut |event| events.push(event))
        .await
        .expect("second Anthropic SSE attempt should replace the malformed attempt");

    assert_eq!(response.provider_attempts, 2);
    assert_eq!(response.generate_result.content, "recovered");
    assert!(events.iter().any(|event| matches!(
        event,
        ModelClientStreamEvent::ReplaceContent { content } if content.is_empty()
    )));
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, ModelClientStreamEvent::Done { .. }))
            .count(),
        1
    );
}
