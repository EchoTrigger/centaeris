use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::runtime::contracts::RuntimeProcessState;
use crate::session::SESSION_EVENT_ID_MAX_BYTES;

pub const RUNTIME_EVENT_VERSION: &str = "v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuntimeEventVisibility {
    User,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeEventProjection {
    #[serde(rename = "id")]
    pub event_id: String,
    pub version: String,
    #[serde(rename = "type")]
    pub event_type: String,
    #[serde(rename = "at")]
    pub at_ms: i64,
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub turn_id: String,
    pub task_id: String,
    pub parent_task_id: String,
    pub status: String,
    pub visibility: RuntimeEventVisibility,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process_state: Option<RuntimeProcessState>,
    pub payload: Value,
    pub meta: Value,
}

impl RuntimeEventProjection {
    pub fn validate(&self) -> Result<(), String> {
        for (name, value) in [
            ("id", self.event_id.as_str()),
            ("type", self.event_type.as_str()),
            ("sessionId", self.session_id.as_str()),
            ("turnId", self.turn_id.as_str()),
            ("taskId", self.task_id.as_str()),
            ("parentTaskId", self.parent_task_id.as_str()),
            ("status", self.status.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(format!("runtime_event {name} is required"));
            }
        }
        if self.version != RUNTIME_EVENT_VERSION {
            return Err(format!(
                "runtime_event version mismatch: expected {RUNTIME_EVENT_VERSION}, got {}",
                self.version
            ));
        }
        if self.event_id.len() > SESSION_EVENT_ID_MAX_BYTES {
            return Err(format!(
                "runtime_event id exceeds {SESSION_EVENT_ID_MAX_BYTES} bytes"
            ));
        }
        if !matches!(
            self.event_type.as_str(),
            "ModelRequestStart"
                | "ModelTextDelta"
                | "ModelTextReplace"
                | "ModelStatus"
                | "Status"
                | "Final"
                | "ToolCallPreparing"
                | "ToolCallReady"
                | "ToolCall"
                | "ToolProgress"
                | "ToolResult"
                | "ToolEvidenceRefs"
                | "PermissionRequired"
                | "QuestionRequired"
                | "RuntimeError"
                | "PromptCompaction"
                | "AgentRunInterventionChanged"
                | "RuntimeWaitChanged"
                | "SubagentSpawned"
                | "SubagentProgress"
                | "SubagentToolGroup"
                | "SubagentResult"
                | "SubagentFailed"
                | "SubagentCancelled"
                | "AgentRunStarted"
                | "UserMessage"
                | "TurnSupplement"
                | "ExternalEvidenceRef"
                | "Citation"
                | "Artifact"
                | "CheckpointRef"
                | "Tombstone"
                | "FileFact"
                | "AgentRunCompleted"
                | "AgentRunFailed"
                | "AgentRunInterrupted"
        ) {
            return Err(format!(
                "unsupported runtime_event type: {}",
                self.event_type
            ));
        }
        if self.at_ms <= 0 {
            return Err("runtime_event at must be positive unix milliseconds".to_string());
        }
        if !self.payload.is_object() || !self.meta.is_object() {
            return Err("runtime_event payload and meta must be objects".to_string());
        }
        Ok(())
    }
}

pub fn project_runtime_event(event: &RuntimeEventProjection) -> Result<Value, String> {
    event.validate()?;
    Ok(serde_json::json!({
        "type": "runtime_event",
        "event": event,
    }))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubagentSessionReadiness {
    pub parent_session_id: String,
    pub parent_turn_id: String,
    pub child_session_id: String,
    pub child_turn_id: String,
    pub runtime_job_id: String,
    pub title: String,
    pub at_ms: i64,
}

pub fn subagent_session_readiness(
    event: &RuntimeEventProjection,
) -> Result<Option<SubagentSessionReadiness>, String> {
    event.validate()?;
    if event.event_type != "SubagentSpawned" {
        return Ok(None);
    }
    let required = |field: &str| {
        event
            .payload
            .get(field)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .ok_or_else(|| format!("SubagentSpawned requires {field}"))
    };
    Ok(Some(SubagentSessionReadiness {
        parent_session_id: event.session_id.clone(),
        parent_turn_id: event.turn_id.clone(),
        child_session_id: required("childSessionId")?,
        child_turn_id: required("childTurnId")?,
        runtime_job_id: required("runtimeJobId")?,
        title: event
            .payload
            .get("description")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .unwrap_or(required("subagentId")?),
        at_ms: event.at_ms,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_event_projection_is_typed_and_exact() {
        let event = RuntimeEventProjection {
            event_id: "event-1".to_string(),
            version: RUNTIME_EVENT_VERSION.to_string(),
            event_type: "Status".to_string(),
            at_ms: 1,
            session_id: "session-1".to_string(),
            turn_id: "turn-1".to_string(),
            task_id: "task-1".to_string(),
            parent_task_id: "turn-1".to_string(),
            status: "running".to_string(),
            visibility: RuntimeEventVisibility::User,
            tool_name: None,
            process_state: Some(RuntimeProcessState::Thinking),
            payload: serde_json::json!({"message": "working"}),
            meta: serde_json::json!({"source": "core.agent_runtime"}),
        };
        let projected = project_runtime_event(&event).expect("project runtime event");
        assert_eq!(projected["type"], "runtime_event");
        assert_eq!(projected["event"]["visibility"], "user");

        let mut unknown = projected["event"].clone();
        unknown["banana"] = Value::Bool(true);
        assert!(serde_json::from_value::<RuntimeEventProjection>(unknown).is_err());

        let mut unsupported = projected["event"].clone();
        unsupported["type"] = Value::String("banana".to_string());
        assert!(
            serde_json::from_value::<RuntimeEventProjection>(unsupported)
                .expect("shape remains exact")
                .validate()
                .is_err()
        );
    }
}
