use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::runtime::contracts::{current_timestamp_ms, RuntimeProcessState};
use crate::runtime::event::{
    project_runtime_event, RuntimeEventProjection, RuntimeEventVisibility, RUNTIME_EVENT_VERSION,
};
use crate::runtime::TurnUpdate;

static STREAM_EVENT_COUNTER: AtomicU64 = AtomicU64::new(1);

#[expect(
    clippy::too_many_arguments,
    reason = "runtime event projection keeps exact protocol fields explicit"
)]
fn live_event(
    event_type: &str,
    session_id: String,
    turn_id: String,
    task_id: String,
    status: &str,
    tool_name: Option<String>,
    process_state: Option<RuntimeProcessState>,
    payload: Value,
) -> RuntimeEventProjection {
    let at_ms = current_timestamp_ms();
    let sequence = STREAM_EVENT_COUNTER.fetch_add(1, Ordering::Relaxed);
    RuntimeEventProjection {
        event_id: format!("runtime:{event_type}:{turn_id}:{task_id}:{at_ms}:{sequence}"),
        version: RUNTIME_EVENT_VERSION.to_string(),
        event_type: event_type.to_string(),
        at_ms,
        session_id,
        parent_task_id: turn_id.clone(),
        turn_id,
        task_id,
        status: status.to_string(),
        visibility: RuntimeEventVisibility::User,
        tool_name,
        process_state,
        payload,
        meta: json!({"source": "core.agent_runtime"}),
    }
}

pub fn project_turn_update(update: TurnUpdate) -> Result<Option<Value>, String> {
    let event = match update {
        TurnUpdate::ModelRequestStart {
            session_id,
            turn_id,
            purpose,
            context_token_estimate,
            message,
            process_state,
            elapsed_ms,
            initial_content,
        } => live_event(
            "ModelRequestStart",
            session_id,
            turn_id.clone(),
            turn_id,
            "running",
            None,
            Some(process_state),
            json!({
                "message": message,
                "elapsedMs": elapsed_ms,
                "initialContent": initial_content,
                "purpose": purpose.as_str(),
                "contextTokenEstimate": context_token_estimate,
            }),
        ),
        TurnUpdate::Token {
            session_id,
            turn_id,
            content,
        } => live_event(
            "ModelTextDelta",
            session_id,
            turn_id.clone(),
            turn_id,
            "running",
            None,
            None,
            json!({"delta": content}),
        ),
        TurnUpdate::ReplaceContent {
            session_id,
            turn_id,
            content,
        } => live_event(
            "ModelTextReplace",
            session_id,
            turn_id.clone(),
            turn_id,
            "running",
            None,
            None,
            json!({"content": content}),
        ),
        TurnUpdate::Status {
            session_id,
            turn_id,
            message,
            process_state,
        } => live_event(
            "ModelStatus",
            session_id,
            turn_id.clone(),
            turn_id,
            "running",
            None,
            Some(process_state),
            json!({"message": message}),
        ),
        TurnUpdate::ToolCallPreparing {
            session_id,
            turn_id,
            name,
            process_state,
        } => live_event(
            "ToolCallPreparing",
            session_id,
            turn_id.clone(),
            turn_id,
            "running",
            Some(name),
            Some(process_state),
            json!({}),
        ),
        TurnUpdate::ToolCallReady {
            session_id,
            turn_id,
            call_id,
            provider_item_id,
            name,
            process_state,
            args_json,
            args_preview,
        } => live_event(
            "ToolCallReady",
            session_id,
            turn_id,
            call_id.clone(),
            "running",
            Some(name),
            Some(process_state),
            json!({
                "callId": call_id,
                "providerItemId": provider_item_id,
                "argsJson": args_json,
                "argsPreview": args_preview,
            }),
        ),
        TurnUpdate::RuntimeError {
            session_id,
            turn_id,
            message,
            reason,
            retryable,
            process_state,
        } => live_event(
            "RuntimeError",
            session_id,
            turn_id.clone(),
            turn_id,
            "error",
            None,
            Some(process_state),
            json!({"message": message, "reason": reason, "retryable": retryable}),
        ),
        TurnUpdate::RuntimeEvent { event } => event,
        TurnUpdate::ModelDone { .. } => return Ok(None),
    };
    project_runtime_event(&event).map(Some)
}

pub fn extract_final_answer_from_stream_items(items: &[Value]) -> Option<String> {
    items.iter().rev().find_map(|item| {
        if !matches!(
            item.get("type").and_then(Value::as_str),
            Some("runtime_event" | "session_event")
        ) {
            return None;
        }
        let event = item.get("event")?;
        match event.get("type").and_then(Value::as_str)? {
            "Final" => event
                .pointer("/payload/content")
                .and_then(Value::as_str)
                .map(str::to_string),
            _ => None,
        }
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HeadlessTranscriptLine {
    pub kind: String,
    pub section: String,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub status: Option<String>,
    pub text: Option<String>,
    pub source_item_id: Option<String>,
    pub source_event_id: Option<String>,
    pub subagent_id: Option<String>,
    pub tool_group_id: Option<String>,
    pub event_type: Option<String>,
    pub indent: u8,
}

fn string_field(item: &Value, field: &str) -> Option<String> {
    item.get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn compact_text(text: &str) -> String {
    const MAX_CHARS: usize = 220;
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= MAX_CHARS {
        return compact;
    }
    compact
        .chars()
        .take(MAX_CHARS.saturating_sub(1))
        .collect::<String>()
        + "…"
}

fn headless_line(item: &Value) -> Option<HeadlessTranscriptLine> {
    if !matches!(
        item.get("type").and_then(Value::as_str),
        Some("runtime_event" | "session_event")
    ) {
        return None;
    }
    let event = item.get("event")?;
    let event_type = string_field(event, "type")?;
    let event_id = string_field(event, "id");
    let status = string_field(event, "status");
    let payload = event.get("payload").unwrap_or(&Value::Null);
    match event_type.as_str() {
        "ModelTextDelta" => text_line(
            payload.get("delta").and_then(Value::as_str)?,
            status,
            event_id,
            event_type,
        ),
        "ModelTextReplace" | "Final" => text_line(
            payload.get("content").and_then(Value::as_str)?,
            status,
            event_id,
            event_type,
        ),
        "ToolCall" | "ToolResult" => Some(HeadlessTranscriptLine {
            kind: "tool_group".to_string(),
            section: "tool".to_string(),
            title: string_field(payload, "summary")
                .or_else(|| string_field(event, "toolName"))
                .or_else(|| string_field(payload, "toolName")),
            summary: string_field(payload, "resultPreview"),
            status,
            text: None,
            source_item_id: None,
            source_event_id: event_id,
            subagent_id: None,
            tool_group_id: string_field(payload, "callId")
                .or_else(|| string_field(event, "taskId")),
            event_type: Some(event_type),
            indent: 0,
        }),
        "SubagentSpawned" | "SubagentProgress" | "SubagentResult" | "SubagentFailed"
        | "SubagentCancelled" | "SubagentToolGroup" => Some(HeadlessTranscriptLine {
            kind: "subagent_group".to_string(),
            section: "subagent".to_string(),
            title: string_field(payload, "title").or_else(|| string_field(payload, "role")),
            summary: string_field(payload, "summary")
                .or_else(|| string_field(payload, "description")),
            status,
            text: None,
            source_item_id: None,
            source_event_id: event_id,
            subagent_id: string_field(payload, "subagentId"),
            tool_group_id: string_field(payload, "toolGroupId"),
            event_type: Some(event_type),
            indent: 1,
        }),
        _ => None,
    }
}

fn text_line(
    text: &str,
    status: Option<String>,
    event_id: Option<String>,
    event_type: String,
) -> Option<HeadlessTranscriptLine> {
    Some(HeadlessTranscriptLine {
        kind: "assistant_text".to_string(),
        section: "final".to_string(),
        title: None,
        summary: None,
        status,
        text: Some(compact_text(text)),
        source_item_id: None,
        source_event_id: event_id,
        subagent_id: None,
        tool_group_id: None,
        event_type: Some(event_type),
        indent: 0,
    })
}

pub fn headless_transcript_lines_from_stream_items(items: &[Value]) -> Vec<HeadlessTranscriptLine> {
    items.iter().filter_map(headless_line).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_runtime_projection_fails_instead_of_fabricating_error_payload() {
        let update = TurnUpdate::RuntimeEvent {
            event: RuntimeEventProjection {
                event_id: "event-1".to_string(),
                version: "banana".to_string(),
                event_type: "Status".to_string(),
                at_ms: 1,
                session_id: "session-1".to_string(),
                turn_id: "turn-1".to_string(),
                task_id: "task-1".to_string(),
                parent_task_id: "turn-1".to_string(),
                status: "running".to_string(),
                visibility: RuntimeEventVisibility::User,
                tool_name: None,
                process_state: None,
                payload: json!({}),
                meta: json!({}),
            },
        };
        assert!(project_turn_update(update).is_err());
    }

    #[test]
    fn model_request_start_projects_context_state() {
        let item = project_turn_update(TurnUpdate::ModelRequestStart {
            session_id: "session-1".to_string(),
            turn_id: "turn-1".to_string(),
            purpose: crate::runtime::ModelRequestPurposeV1::Main,
            context_token_estimate: 42,
            message: None,
            process_state: RuntimeProcessState::Thinking,
            elapsed_ms: 0,
            initial_content: String::new(),
        })
        .expect("project request start")
        .expect("request start payload");

        assert_eq!(item["event"]["payload"]["purpose"], "main");
        assert_eq!(item["event"]["payload"]["contextTokenEstimate"], 42);
    }

    #[test]
    fn final_projection_is_reused_by_headless_consumer() {
        let item = project_runtime_event(&live_event(
            "Final",
            "session-1".to_string(),
            "turn-1".to_string(),
            "task-1".to_string(),
            "done",
            None,
            None,
            json!({"content": "done"}),
        ))
        .expect("project final");
        assert_eq!(
            extract_final_answer_from_stream_items(std::slice::from_ref(&item)).as_deref(),
            Some("done")
        );
        assert_eq!(
            headless_transcript_lines_from_stream_items(&[item])[0]
                .text
                .as_deref(),
            Some("done")
        );
    }
}
