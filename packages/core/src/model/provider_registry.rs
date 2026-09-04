use std::collections::HashMap;
use std::env;
use std::future::Future;
use std::pin::Pin;

use centaeris_model_catalog::{model_catalog, ModelApi, ModelProviderDefinition};
use serde::{Deserialize, Serialize};

use crate::model::prepared_prompt::PreparedPromptV1;
use crate::model::GenerateResult;
use crate::runtime::contracts::RuntimeProcessState;

use super::settings::ModelWireApi;

pub const DEFAULT_MODEL_CONTEXT_TOKENS: u32 = 200_000;
pub const DEFAULT_MODEL_OUTPUT_TOKENS: u32 = 32_768;
pub const DEFAULT_MODEL_RESPONSE_HEADERS_TIMEOUT_MS: u64 = 60_000;
pub const DEFAULT_MODEL_SSE_IDLE_TIMEOUT_MS: u64 = 300_000;
pub const DEFAULT_MODEL_MAX_RETRIES: u32 = 5;
pub const DEFAULT_MODEL_RETRY_BACKOFF_MS: u64 = 600;
pub const PROMPT_COMPACTION_MAX_OUTPUT_TOKENS: u32 = 16_384;
pub const PROMPT_COMPACTION_TRIGGER_HEADROOM_TOKENS: u32 = 32_768;
pub const PROMPT_COMPACTION_USER_REPLAY_TOKENS: u32 = 20_000;

pub fn prompt_compaction_max_output_tokens(
    model_max_output_tokens: u32,
    summary_max_tokens: u32,
) -> u32 {
    model_max_output_tokens.min(summary_max_tokens)
}
pub const MODEL_PROVIDER_WAITING_STREAM_EVENT_TYPE: &str = "provider_waiting";
pub const CUSTOM_OPENAI_COMPATIBLE_PROVIDER_ID: &str = "custom.openai_compatible";
pub const OPENAI_PROVIDER_ID: &str = "openai.default";
pub const ANTHROPIC_PROVIDER_ID: &str = "anthropic.default";
pub const DEEPSEEK_PROVIDER_ID: &str = "deepseek.default";
pub const KIMI_PROVIDER_ID: &str = "kimi.default";
pub const KIMI_CODE_PROVIDER_ID: &str = "kimi-code.default";
pub const ZAI_PROVIDER_ID: &str = "zai.default";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BuiltInModelProfile {
    pub provider_id: String,
    pub model: String,
    pub display_name: String,
    pub context_tokens: u32,
    pub max_output_tokens: u32,
    pub thinking_mode: Option<String>,
    pub thinking_modes: Vec<String>,
    pub supports_vision: bool,
    pub api_override: Option<ModelWireApi>,
    pub api_base_override: Option<String>,
}

pub fn built_in_model_profiles() -> Vec<BuiltInModelProfile> {
    model_catalog()
        .providers
        .iter()
        .flat_map(|provider| {
            provider.models.iter().map(|model| BuiltInModelProfile {
                provider_id: provider.provider_id.clone(),
                model: model.model.clone(),
                display_name: model.display_name.clone(),
                context_tokens: model.context_tokens,
                max_output_tokens: model.max_output_tokens,
                thinking_mode: model.thinking_mode.clone(),
                thinking_modes: model.thinking_modes.clone(),
                supports_vision: model.supports_vision,
                api_override: model.api_override.map(model_wire_api),
                api_base_override: model.api_base_override.clone(),
            })
        })
        .collect()
}

pub fn built_in_model_provider_ids() -> Vec<String> {
    model_catalog()
        .providers
        .iter()
        .map(|provider| provider.provider_id.clone())
        .collect()
}

pub fn built_in_model_profile(provider_id: &str, model: &str) -> Option<BuiltInModelProfile> {
    built_in_model_profiles()
        .into_iter()
        .find(|profile| profile.provider_id == provider_id && profile.model == model)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelProviderKind {
    OpenAi,
    Anthropic,
    Gemini,
    Kimi,
    DeepSeek,
    Zai,
    OpenRouter,
    Local,
    Custom,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WireApi {
    OpenAiResponses,
    OpenAiChatCompletions,
    AnthropicMessages,
    GeminiGenerateContent,
    Custom,
    LocalMock,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AuthSpec {
    None,
    ApiKeyEnv {
        env_key: String,
        header_name: String,
        prefix: Option<String>,
    },
    BearerEnv {
        env_key: String,
    },
    StaticHeader {
        header_name: String,
        value: String,
    },
    CommandToken {
        command: String,
        header_name: String,
        prefix: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityProfile {
    pub supports_streaming: bool,
    pub supports_tool_calls: bool,
    pub supports_structured_output: bool,
    pub supports_reasoning: bool,
    pub supports_vision: bool,
    pub max_context_tokens: Option<u32>,
    pub max_output_tokens: Option<u32>,
}

impl Default for CapabilityProfile {
    fn default() -> Self {
        Self {
            supports_streaming: true,
            supports_tool_calls: false,
            supports_structured_output: false,
            supports_reasoning: false,
            supports_vision: false,
            max_context_tokens: None,
            max_output_tokens: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ModelProviderInfo {
    pub provider_key: String,
    pub name: String,
    pub provider_kind: ModelProviderKind,
    pub base_url: Option<String>,
    pub wire_api: WireApi,
    pub auth: AuthSpec,
    pub http_headers: HashMap<String, String>,
    pub env_http_headers: HashMap<String, String>,
    pub default_timeout_ms: Option<u64>,
    pub default_max_retries: Option<u32>,
    pub default_retry_backoff_ms: Option<u64>,
    pub capability_profile: CapabilityProfile,
    pub metadata: HashMap<String, String>,
}

impl ModelProviderInfo {
    pub fn resolve_http_headers(&self) -> HashMap<String, String> {
        let mut headers = self.http_headers.clone();
        for (header_name, env_key) in &self.env_http_headers {
            if let Ok(value) = env::var(env_key) {
                let trimmed = value.trim();
                if !trimmed.is_empty() {
                    headers.insert(header_name.clone(), trimmed.to_string());
                }
            }
        }
        headers
    }

    pub fn resolve_auth_headers(&self) -> Result<HashMap<String, String>, String> {
        let mut headers = HashMap::new();
        match &self.auth {
            AuthSpec::None => {}
            AuthSpec::ApiKeyEnv {
                env_key,
                header_name,
                prefix,
            } => {
                let value = env::var(env_key)
                    .map_err(|_| format!("missing auth env var for provider: {env_key}"))?;
                let final_value = apply_auth_prefix(value.trim(), prefix.as_deref());
                headers.insert(header_name.clone(), final_value);
            }
            AuthSpec::BearerEnv { env_key } => {
                let value = env::var(env_key)
                    .map_err(|_| format!("missing auth env var for provider: {env_key}"))?;
                headers.insert(
                    "authorization".to_string(),
                    format!("Bearer {}", value.trim()),
                );
            }
            AuthSpec::StaticHeader { header_name, value } => {
                headers.insert(header_name.clone(), value.clone());
            }
            AuthSpec::CommandToken { .. } => {
                return Err("command token auth is not implemented yet".to_string());
            }
        }
        Ok(headers)
    }
}

fn apply_auth_prefix(value: &str, prefix: Option<&str>) -> String {
    match prefix {
        Some(prefix_value) if !prefix_value.trim().is_empty() => {
            format!("{} {}", prefix_value.trim(), value)
        }
        _ => value.to_string(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedModelProvider {
    pub info: ModelProviderInfo,
    pub headers: HashMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedModelSessionConfig {
    pub session_config: ModelSessionConfig,
    pub provider: ResolvedModelProvider,
    pub effective_api_base: Option<String>,
    pub effective_timeout_ms: u64,
    pub effective_max_retries: u32,
    pub effective_retry_backoff_ms: u64,
}

#[derive(Debug, Clone)]
pub struct ModelProviderRegistry {
    built_in: HashMap<String, ModelProviderInfo>,
    user_defined: HashMap<String, ModelProviderInfo>,
}

impl ModelProviderRegistry {
    pub fn new() -> Self {
        Self {
            built_in: built_in_model_providers(),
            user_defined: HashMap::new(),
        }
    }

    pub fn with_user_defined(mut self, providers: HashMap<String, ModelProviderInfo>) -> Self {
        self.user_defined = providers;
        self
    }

    pub fn insert_user_defined(
        &mut self,
        provider: ModelProviderInfo,
    ) -> Option<ModelProviderInfo> {
        self.user_defined
            .insert(provider.provider_key.clone(), provider)
    }

    pub fn get(&self, provider_key: &str) -> Option<&ModelProviderInfo> {
        self.user_defined
            .get(provider_key)
            .or_else(|| self.built_in.get(provider_key))
    }

    pub fn all(&self) -> HashMap<String, ModelProviderInfo> {
        let mut merged = self.built_in.clone();
        merged.extend(self.user_defined.clone());
        merged
    }

    pub fn resolve(&self, provider_key: &str) -> Result<ResolvedModelProvider, String> {
        let info = self
            .get(provider_key)
            .cloned()
            .ok_or_else(|| format!("unknown model provider: {provider_key}"))?;
        let headers = info.resolve_http_headers();
        Ok(ResolvedModelProvider { info, headers })
    }

    pub fn resolve_session_config(
        &self,
        session_config: &ModelSessionConfig,
    ) -> Result<ResolvedModelSessionConfig, String> {
        let provider = self.resolve(session_config.provider_id.as_str())?;
        Ok(ResolvedModelSessionConfig {
            session_config: session_config.clone(),
            effective_api_base: session_config
                .api_base
                .clone()
                .or_else(|| provider.info.base_url.clone()),
            effective_timeout_ms: if session_config.timeout_ms == 0 {
                provider
                    .info
                    .default_timeout_ms
                    .unwrap_or(DEFAULT_MODEL_RESPONSE_HEADERS_TIMEOUT_MS)
            } else {
                session_config.timeout_ms
            },
            effective_max_retries: if session_config.max_retries == 0 {
                provider
                    .info
                    .default_max_retries
                    .unwrap_or(DEFAULT_MODEL_MAX_RETRIES)
            } else {
                session_config.max_retries
            },
            effective_retry_backoff_ms: if session_config.retry_backoff_ms == 0 {
                provider
                    .info
                    .default_retry_backoff_ms
                    .unwrap_or(DEFAULT_MODEL_RETRY_BACKOFF_MS)
            } else {
                session_config.retry_backoff_ms
            },
            provider,
        })
    }
}

impl Default for ModelProviderRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ModelSessionConfig {
    pub provider_kind: ModelProviderKind,
    pub provider_id: String,
    pub model: String,
    pub api_base: Option<String>,
    pub timeout_ms: u64,
    pub max_retries: u32,
    pub retry_backoff_ms: u64,
    pub max_output_tokens: Option<u32>,
    pub thinking_mode: Option<String>,
    pub metadata: HashMap<String, String>,
}

impl Default for ModelSessionConfig {
    fn default() -> Self {
        Self {
            provider_kind: ModelProviderKind::Local,
            provider_id: "local.default".to_string(),
            model: "mock.default".to_string(),
            api_base: None,
            timeout_ms: DEFAULT_MODEL_RESPONSE_HEADERS_TIMEOUT_MS,
            max_retries: DEFAULT_MODEL_MAX_RETRIES,
            retry_backoff_ms: DEFAULT_MODEL_RETRY_BACKOFF_MS,
            max_output_tokens: None,
            thinking_mode: None,
            metadata: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ModelClientRequest {
    pub session_id: String,
    pub turn_id: String,
    pub loop_index: u32,
    pub provider_prompt_cache_key: Option<String>,
    pub provider_prompt_cache_retention: Option<String>,
    pub system_prompt_manifest_json: Option<String>,
    pub compression_stats_json: Option<String>,
    pub context_token_estimate: u32,
    pub prepared_prompt: PreparedPromptV1,
    pub session_config: ModelSessionConfig,
}

#[derive(Debug, Clone)]
pub struct ModelClientResponse {
    pub generate_result: GenerateResult,
    pub provider_request_id: Option<String>,
    pub provider_latency_ms: Option<u64>,
    pub provider_attempts: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TruncatedToolCall {
    pub call_id: Option<String>,
    pub tool_name: Option<String>,
    pub args_bytes: usize,
    pub args_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelClientStreamEvent {
    RequestStart {
        message: Option<String>,
        process_state: RuntimeProcessState,
        elapsed_ms: u64,
    },
    Token {
        content: String,
    },
    ReplaceContent {
        content: String,
    },
    Status {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModelClientErrorKind {
    Timeout,
    AuthFailed,
    ProviderBusyOrRateLimited,
    ProviderUnavailable,
    ProviderResponseInterrupted,
    ModelUnavailable,
    InvalidRequest,
    Network,
    Provider,
    Cancelled,
    Unknown,
}

impl ModelClientErrorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Timeout => "timeout",
            Self::AuthFailed => "auth_failed",
            Self::ProviderBusyOrRateLimited => "provider_busy_or_rate_limited",
            Self::ProviderUnavailable => "provider_unavailable",
            Self::ProviderResponseInterrupted => "provider_response_interrupted",
            Self::ModelUnavailable => "model_unavailable",
            Self::InvalidRequest => "invalid_request",
            Self::Network => "network",
            Self::Provider => "provider",
            Self::Cancelled => "cancelled",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ModelClientError {
    pub kind: ModelClientErrorKind,
    pub message: String,
    pub retryable: bool,
    pub provider_code: Option<String>,
    pub provider_attempts: u32,
    pub truncated_tool_calls: Vec<TruncatedToolCall>,
}

impl ModelClientError {
    pub fn new(kind: ModelClientErrorKind, message: impl Into<String>, retryable: bool) -> Self {
        Self {
            kind,
            message: message.into(),
            retryable,
            provider_code: None,
            provider_attempts: 0,
            truncated_tool_calls: Vec::new(),
        }
    }

    pub fn with_provider_attempts(mut self, provider_attempts: u32) -> Self {
        self.provider_attempts = provider_attempts;
        self
    }

    pub fn with_truncated_tool_calls(mut self, tool_calls: Vec<TruncatedToolCall>) -> Self {
        self.truncated_tool_calls = tool_calls;
        self
    }
}

pub(super) fn is_retryable_model_client_error_kind(kind: ModelClientErrorKind) -> bool {
    matches!(
        kind,
        ModelClientErrorKind::Timeout
            | ModelClientErrorKind::ProviderBusyOrRateLimited
            | ModelClientErrorKind::ProviderUnavailable
            | ModelClientErrorKind::ProviderResponseInterrupted
            | ModelClientErrorKind::Network
            | ModelClientErrorKind::Provider
    )
}

pub(super) fn map_transport_model_client_error(message: String) -> ModelClientError {
    let normalized = message.to_ascii_lowercase();
    let kind = if normalized.contains("read http body failed")
        || normalized.contains("error decoding response body")
        || normalized.contains("response body")
        || normalized.contains("stream ended before a terminal response event")
    {
        ModelClientErrorKind::ProviderResponseInterrupted
    } else if normalized.contains("timeout")
        || normalized.contains("timed out")
        || normalized.contains("deadline has elapsed")
    {
        ModelClientErrorKind::Timeout
    } else {
        ModelClientErrorKind::Network
    };
    ModelClientError::new(kind, message, is_retryable_model_client_error_kind(kind))
}

pub type ModelClientFuture<'a, TValue> =
    Pin<Box<dyn Future<Output = Result<TValue, ModelClientError>> + Send + 'a>>;

pub trait ModelClient: Send + Sync {
    fn generate<'a>(
        &'a self,
        request: &'a ModelClientRequest,
    ) -> ModelClientFuture<'a, ModelClientResponse>;

    fn generate_stream<'a>(
        &'a self,
        request: &'a ModelClientRequest,
        _sink: &'a mut (dyn FnMut(ModelClientStreamEvent) + Send),
    ) -> ModelClientFuture<'a, ModelClientResponse> {
        self.generate(request)
    }
}

pub trait ModelSessionConfigStore: Send + Sync {
    fn get_session_config(&self, session_id: &str) -> Result<Option<ModelSessionConfig>, String>;
}

pub fn built_in_model_providers() -> HashMap<String, ModelProviderInfo> {
    let mut providers = model_catalog()
        .providers
        .iter()
        .map(|provider| (provider.provider_id.clone(), model_provider_info(provider)))
        .collect::<HashMap<_, _>>();
    providers.insert(
        "local.default".to_string(),
        ModelProviderInfo {
            provider_key: "local.default".to_string(),
            name: "Local Mock".to_string(),
            provider_kind: ModelProviderKind::Local,
            base_url: None,
            wire_api: WireApi::LocalMock,
            auth: AuthSpec::None,
            http_headers: HashMap::new(),
            env_http_headers: HashMap::new(),
            default_timeout_ms: Some(10_000),
            default_max_retries: Some(0),
            default_retry_backoff_ms: Some(0),
            capability_profile: CapabilityProfile {
                supports_streaming: false,
                supports_tool_calls: false,
                supports_structured_output: false,
                supports_reasoning: false,
                supports_vision: false,
                max_context_tokens: None,
                max_output_tokens: None,
            },
            metadata: HashMap::new(),
        },
    );
    providers.insert(
        CUSTOM_OPENAI_COMPATIBLE_PROVIDER_ID.to_string(),
        ModelProviderInfo {
            provider_key: CUSTOM_OPENAI_COMPATIBLE_PROVIDER_ID.to_string(),
            name: "Custom OpenAI-Compatible".to_string(),
            provider_kind: ModelProviderKind::Custom,
            base_url: None,
            wire_api: WireApi::OpenAiChatCompletions,
            auth: AuthSpec::None,
            http_headers: HashMap::new(),
            env_http_headers: HashMap::new(),
            default_timeout_ms: Some(60_000),
            default_max_retries: Some(DEFAULT_MODEL_MAX_RETRIES),
            default_retry_backoff_ms: Some(DEFAULT_MODEL_RETRY_BACKOFF_MS),
            capability_profile: CapabilityProfile {
                supports_streaming: true,
                supports_tool_calls: true,
                supports_structured_output: true,
                supports_reasoning: true,
                supports_vision: true,
                max_context_tokens: None,
                max_output_tokens: None,
            },
            metadata: HashMap::new(),
        },
    );
    providers
}

fn model_provider_info(provider: &ModelProviderDefinition) -> ModelProviderInfo {
    ModelProviderInfo {
        provider_key: provider.provider_id.clone(),
        name: provider.display_name.clone(),
        provider_kind: match provider.provider_kind.as_str() {
            "open_ai" => ModelProviderKind::OpenAi,
            "anthropic" => ModelProviderKind::Anthropic,
            "kimi" => ModelProviderKind::Kimi,
            "deep_seek" => ModelProviderKind::DeepSeek,
            "zai" => ModelProviderKind::Zai,
            "custom" => ModelProviderKind::Custom,
            unsupported => panic!("unsupported catalog providerKind={unsupported}"),
        },
        base_url: Some(provider.api_base.clone()),
        wire_api: wire_api(provider.api),
        auth: if provider.credential.header == "authorization"
            && provider.credential.prefix.as_deref() == Some("Bearer")
        {
            AuthSpec::BearerEnv {
                env_key: provider.credential.env.clone(),
            }
        } else {
            AuthSpec::ApiKeyEnv {
                env_key: provider.credential.env.clone(),
                header_name: provider.credential.header.clone(),
                prefix: provider.credential.prefix.clone(),
            }
        },
        http_headers: provider.http_headers.clone(),
        env_http_headers: HashMap::new(),
        default_timeout_ms: Some(DEFAULT_MODEL_RESPONSE_HEADERS_TIMEOUT_MS),
        default_max_retries: Some(DEFAULT_MODEL_MAX_RETRIES),
        default_retry_backoff_ms: Some(DEFAULT_MODEL_RETRY_BACKOFF_MS),
        capability_profile: CapabilityProfile {
            supports_streaming: true,
            supports_tool_calls: true,
            supports_structured_output: provider.api != ModelApi::AnthropicMessages,
            supports_reasoning: provider
                .models
                .iter()
                .any(|model| !model.thinking_modes.is_empty()),
            supports_vision: provider.models.iter().any(|model| model.supports_vision),
            max_context_tokens: provider
                .models
                .iter()
                .map(|model| model.context_tokens)
                .max(),
            max_output_tokens: provider
                .models
                .iter()
                .map(|model| model.max_output_tokens)
                .max(),
        },
        metadata: HashMap::from([("catalogId".to_string(), provider.catalog_id.clone())]),
    }
}

fn wire_api(api: ModelApi) -> WireApi {
    match api {
        ModelApi::OpenAiCompletions => WireApi::OpenAiChatCompletions,
        ModelApi::OpenAiResponses => WireApi::OpenAiResponses,
        ModelApi::AnthropicMessages => WireApi::AnthropicMessages,
    }
}

fn model_wire_api(api: ModelApi) -> ModelWireApi {
    match api {
        ModelApi::OpenAiCompletions => ModelWireApi::OpenAiCompletions,
        ModelApi::OpenAiResponses => ModelWireApi::OpenAiResponses,
        ModelApi::AnthropicMessages => ModelWireApi::AnthropicMessages,
    }
}
