use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::model::prepared_prompt::{ModelMessageRoleV1, ModelMessageV1};
use crate::runtime::contracts::RuntimeProcessState;
use crate::tool::ModelToolChoice;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallEnvelope {
    pub id: String,
    pub name: String,
    pub args_json: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerateResult {
    pub content: String,
    pub tool_calls: Vec<ToolCallEnvelope>,
    pub reasoning_content: Option<String>,
    pub input_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
    pub prompt_cache_hit_tokens: Option<i64>,
    pub prompt_cache_miss_tokens: Option<i64>,
}

pub mod prepared_prompt;
pub mod prompt;
mod protocol_adapters;
pub mod provider_polling;
mod provider_registry;
mod settings;
mod store;
mod transport;

pub use protocol_adapters::{
    validate_provider_tool_call_arguments, AnthropicMessagesModelClient,
    OpenAiCompatibleModelClient, OpenAiResponsesModelClient,
};
pub use provider_registry::*;
pub use settings::*;
pub use store::EmptyModelSessionConfigStore;
pub use transport::{JsonHttpFuture, JsonHttpRequest, JsonHttpResponse, JsonHttpTransport};

use provider_registry::{is_retryable_model_client_error_kind, map_transport_model_client_error};
use transport::{
    execute_json_model_response_with_retries, execute_sse_with_retries,
    map_attempted_transport_error, SseAttemptEvent, SseAttemptProgress,
};
