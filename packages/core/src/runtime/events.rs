use std::collections::HashMap;

use serde_json::{json, Value};

use super::status_events::StatusStage;
use super::tool_batch_executor::ToolBatchExecutorEvent;
use super::*;
use crate::runtime::contracts::TimestampMs;
use crate::runtime::event::RuntimeEventProjection;
use crate::runtime::subagent::{
    SubagentLifecycleStatus, SubagentSchedulerEvent, SubagentSchedulerEventKind,
};
pub(super) use crate::session::stable_session_event_id;

mod status;
mod subagent;
mod tool;

fn typed_runtime_event(value: Value) -> RuntimeEventProjection {
    let event = serde_json::from_value::<RuntimeEventProjection>(value)
        .expect("Core runtime event literal must match runtime_event DTO");
    event
        .validate()
        .expect("Core runtime event literal must be valid");
    event
}

pub(super) use self::status::{
    build_runtime_event_agent_run_intervention_changed, build_runtime_event_final_event,
    build_runtime_event_prompt_compaction_event, build_runtime_event_runtime_wait_changed,
    build_runtime_event_status_event,
};
pub(super) use self::subagent::{
    build_runtime_event_subagent_event_from_scheduler_event,
    build_runtime_event_subagent_spawned_from_tool_result,
    build_runtime_event_subagent_tool_group_events_from_tool_results,
};
pub(super) use self::tool::{
    build_runtime_event_question_required_event, build_runtime_event_tool_call_events,
    build_runtime_event_tool_progress_event, build_runtime_event_tool_result_events,
};

fn parse_tool_operations_by_call_id(
    tool_operations_json: Option<&str>,
) -> HashMap<String, Vec<Value>> {
    let Some(parsed) = tool_operations_json.and_then(parse_json_text) else {
        return HashMap::new();
    };
    let Some(items) = parsed.as_array() else {
        return HashMap::new();
    };
    let mut grouped = HashMap::<String, Vec<Value>>::new();
    for item in items {
        let Some(call_id) = item.get("callId").and_then(Value::as_str) else {
            continue;
        };
        grouped
            .entry(call_id.to_string())
            .or_default()
            .push(item.clone());
    }
    grouped
}

#[cfg(test)]
mod tests;
