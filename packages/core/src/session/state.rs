use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize};

use crate::runtime::contracts::{JsonMap, RuntimeAgentRunIdentityV1, TimestampMs};

pub const COMPLETED_TURN_PROJECTION_SCHEMA_V1: &str = "runtime.completed_turn.v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompletedTurnProjectionV1 {
    pub schema: String,
    pub agent_run_id: String,
    pub execution_id: String,
    pub authorization_digest: String,
    pub completion_reason: String,
    pub final_turn_id: String,
    pub expected_tool_call_ids: Vec<String>,
}

impl CompletedTurnProjectionV1 {
    pub fn new(
        agent_run_identity: &RuntimeAgentRunIdentityV1,
        completion_reason: String,
        final_turn_id: String,
        expected_tool_call_ids: Vec<String>,
    ) -> Result<Self, String> {
        let projection = Self {
            schema: COMPLETED_TURN_PROJECTION_SCHEMA_V1.to_string(),
            agent_run_id: agent_run_identity.agent_run_id.clone(),
            execution_id: agent_run_identity.execution_id.clone(),
            authorization_digest: agent_run_identity.authorization_digest.clone(),
            completion_reason,
            final_turn_id,
            expected_tool_call_ids,
        };
        projection.validate()?;
        Ok(projection)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema != COMPLETED_TURN_PROJECTION_SCHEMA_V1 {
            return Err("completed_turn_projection_schema_invalid".to_string());
        }
        RuntimeAgentRunIdentityV1 {
            agent_run_id: self.agent_run_id.clone(),
            execution_id: self.execution_id.clone(),
            authorization_digest: self.authorization_digest.clone(),
        }
        .validate()?;
        if !matches!(
            self.completion_reason.as_str(),
            "finalized" | "terminal_tool"
        ) {
            return Err("completed_turn_projection_completion_reason_invalid".to_string());
        }
        if self.final_turn_id.trim().is_empty() {
            return Err("completed_turn_projection_final_turn_id_required".to_string());
        }
        for tool_call_id in &self.expected_tool_call_ids {
            if tool_call_id.trim().is_empty() {
                return Err("completed_turn_projection_tool_call_id_required".to_string());
            }
        }
        if self
            .expected_tool_call_ids
            .windows(2)
            .any(|pair| pair[0] >= pair[1])
        {
            return Err("completed_turn_projection_tool_call_ids_not_strictly_sorted".to_string());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum MessageRole {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub message_id: String,
    pub role: MessageRole,
    pub content: String,
    pub created_at_ms: TimestampMs,
    pub metadata: JsonMap,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelToolCallStateV1 {
    pub id: String,
    pub name: String,
    pub args_json: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum ModelMessageSemanticsV1 {
    Plain,
    Assistant {
        reasoning_content: Option<String>,
        tool_calls: Vec<ModelToolCallStateV1>,
    },
    ToolResult {
        tool_call_id: String,
        tool_name: String,
        status: String,
        result_state: String,
        error_kind: Option<String>,
        object_refs: Vec<String>,
        transition_reason: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionStateSnapshot {
    pub session_id: String,
    pub messages: Vec<ChatMessage>,
    pub context_window: Vec<ChatMessage>,
    pub model_semantics: BTreeMap<String, ModelMessageSemanticsV1>,
    #[serde(
        rename = "completedTurn",
        deserialize_with = "deserialize_completed_turn"
    )]
    pub completed_turn: Option<CompletedTurnProjectionV1>,
    pub updated_at_ms: TimestampMs,
    pub metadata: JsonMap,
}

fn deserialize_completed_turn<'de, D>(
    deserializer: D,
) -> Result<Option<CompletedTurnProjectionV1>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::deserialize(deserializer)
}

impl SessionStateSnapshot {
    pub fn new(session_id: String, now_ms: TimestampMs) -> Self {
        Self {
            session_id,
            messages: vec![],
            context_window: vec![],
            model_semantics: BTreeMap::new(),
            completed_turn: None,
            updated_at_ms: now_ms,
            metadata: JsonMap::new(),
        }
    }

    pub fn model_semantics_for(
        &self,
        message_id: &str,
    ) -> Result<&ModelMessageSemanticsV1, String> {
        self.model_semantics
            .get(message_id)
            .ok_or_else(|| format!("model_message_semantics_missing: messageId={message_id}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_snapshot_requires_typed_runtime_state() {
        let snapshot = SessionStateSnapshot::new("chat-state".to_string(), 1);
        let mut value = serde_json::to_value(snapshot).expect("snapshot json");
        value
            .as_object_mut()
            .expect("snapshot object")
            .remove("model_semantics");
        assert!(serde_json::from_value::<SessionStateSnapshot>(value.clone()).is_err());

        let object = value.as_object_mut().expect("snapshot object");
        object.insert("model_semantics".to_string(), serde_json::json!({}));
        object.remove("completedTurn");
        assert!(serde_json::from_value::<SessionStateSnapshot>(value).is_err());
    }
}
