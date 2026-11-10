//! Shared Session requests, receipts and durable runtime projections.

use crate::OperationId;
use serde::{Deserialize, Serialize};

/// A canonical host notification delivered to native callers.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionEvent {
    pub method: String,
    pub params: serde_json::Value,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionCreateCommandRequest {
    pub operation_id: OperationId,
    pub title: Option<String>,
    pub cwd: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PersistedChatMessageResponse {
    pub id: String,
    pub session_id: String,
    pub turn_id: String,
    pub role: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_data: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionDataResponse {
    pub id: String,
    pub title: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub session_kind: String,
    pub parent_session_id: Option<String>,
    pub runtime_job_id: Option<String>,
    pub messages: Vec<PersistedChatMessageResponse>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentInputResponse {
    pub session_id: String,
    pub agent_run_id: String,
    pub turn_id: String,
    pub stream_items: Vec<serde_json::Value>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSupplementResponse {
    pub accepted: bool,
    pub session_id: String,
    pub agent_run_id: String,
    pub queued_count: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentAnswerNowResponse {
    pub accepted: bool,
    pub disposition: &'static str,
    pub session_id: String,
    pub agent_run_id: String,
    pub intervention_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentContextCompactResponse {
    pub session_id: String,
    pub compacted: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LocalImageInputRequest {
    pub placeholder: String,
    pub local_path: String,
}

/// An answer to a pending question in this Session.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QuestionAnswer {
    pub question_id: String,
    pub answers: Option<Vec<String>>,
    pub answer_text: Option<String>,
    pub auto_continue_after_resume_wait: Option<bool>,
}

/// Input to a bound Session. The SDK supplies its Session identity.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionPrompt {
    pub operation_id: OperationId,
    pub message: String,
    pub tail_policy: Option<String>,
    pub rewrite_target_message_id: Option<String>,
    pub rewrite_expected_tail_message_id: Option<String>,
    pub auto_continue_after_resume_wait: Option<bool>,
    #[serde(default)]
    pub attachments: Vec<LocalImageInputRequest>,
}

impl SessionPrompt {
    pub fn new(operation_id: OperationId, message: String) -> Self {
        Self {
            operation_id,
            message,
            tail_policy: None,
            rewrite_target_message_id: None,
            rewrite_expected_tail_message_id: None,
            auto_continue_after_resume_wait: None,
            attachments: Vec::new(),
        }
    }
}

/// Durable runtime state exposed to SDK consumers.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRun {
    pub agent_run_id: String,
    pub session_id: String,
    pub turn_id: String,
    pub agent_run_kind: String,
    pub cwd: Option<String>,
    pub status: String,
    pub started_at_ms: i64,
    pub updated_at_ms: i64,
    pub completed_at_ms: Option<i64>,
    pub last_event_at_ms: Option<i64>,
    pub stall_reason: Option<String>,
    pub watchdog: Option<serde_json::Value>,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunSubscription {
    pub agent_run: Option<AgentRun>,
    pub viewer_id: String,
    pub transition_reason: String,
    pub attached_viewer_count: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelReceipt {
    pub agent_run: Option<AgentRun>,
    pub cancel_accepted: bool,
}
