use super::*;
use crate::runtime::contracts::TimestampMs;
use crate::runtime::subagent::{
    SubagentLifecycleStatus, SubagentSchedulerEvent, SubagentSchedulerEventKind,
};
use crate::session::store::AgentRuntimeSnapshotStorePort;

const SUBAGENT_RESULT_PROJECTION_SCHEMA: &str = "subagent_result_projection_v1";
const MAX_SUBAGENT_RESULT_ITEMS: usize = 64;
const MAX_SUBAGENT_TITLE_CHARS: usize = 180;
const MAX_SUBAGENT_DESCRIPTION_CHARS: usize = 800;
const MAX_SUBAGENT_SUMMARY_CHARS: usize = 1_200;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct SubagentResultProjectionV1 {
    schema: String,
    items: Vec<SubagentResultProjectionItemV1>,
    recorded_at_ms: TimestampMs,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct SubagentResultProjectionItemV1 {
    subagent_run_ref: String,
    subagent_id: String,
    child_session_ref: String,
    parent_turn_id: String,
    work_packet_ref: Option<String>,
    title: String,
    description: Option<String>,
    status: String,
    bounded_summary: String,
    result_ref: Option<String>,
    output_refs: Vec<String>,
    started_at_ms: Option<TimestampMs>,
    finished_at_ms: Option<TimestampMs>,
}

pub fn persist_subagent_result_projection_from_scheduler_events<S>(
    store: &S,
    parent_session_id: &str,
    events: &[SubagentSchedulerEvent],
) -> Result<usize, String>
where
    S: AgentRuntimeSnapshotStorePort + Clone,
{
    let terminal_items = events
        .iter()
        .filter_map(|event| build_projection_item(parent_session_id, event))
        .collect::<Vec<_>>();
    if terminal_items.is_empty() {
        return Ok(0);
    }

    let session_manager = crate::session::manager::SessionManager::new(store.clone());
    let mut session = session_manager.load_or_create_session(parent_session_id)?;
    let written = merge_subagent_result_projection_items(&mut session, terminal_items)?;
    if written > 0 {
        session_manager.save_session(&session)?;
    }
    Ok(written)
}

fn build_projection_item(
    _parent_session_id: &str,
    event: &SubagentSchedulerEvent,
) -> Option<SubagentResultProjectionItemV1> {
    if !matches!(
        event.kind,
        SubagentSchedulerEventKind::Succeeded
            | SubagentSchedulerEventKind::Failed
            | SubagentSchedulerEventKind::Cancelled
    ) {
        return None;
    }
    let result_ref = event.result_ref.as_deref().and_then(normalized_non_empty);
    let output_refs = result_ref.iter().cloned().collect::<Vec<_>>();
    let title = event
        .description
        .as_deref()
        .and_then(normalized_non_empty)
        .unwrap_or_else(|| subagent_projection_default_title(&event.kind).to_string());
    Some(SubagentResultProjectionItemV1 {
        subagent_run_ref: format!("runtime_job:{}", event.job_id),
        subagent_id: compact_subagent_projection_text(
            event.subagent_id.as_str(),
            MAX_SUBAGENT_TITLE_CHARS,
        ),
        child_session_ref: format!("session:{}", event.child_session_id),
        parent_turn_id: event.parent_turn_id.clone(),
        work_packet_ref: event
            .work_packet_ref
            .as_deref()
            .and_then(normalized_non_empty),
        title: compact_subagent_projection_text(title.as_str(), MAX_SUBAGENT_TITLE_CHARS),
        description: event
            .description
            .as_deref()
            .and_then(normalized_non_empty)
            .map(|item| {
                compact_subagent_projection_text(item.as_str(), MAX_SUBAGENT_DESCRIPTION_CHARS)
            }),
        status: subagent_projection_status(&event.status).to_string(),
        bounded_summary: compact_subagent_projection_text(
            event.summary.as_str(),
            MAX_SUBAGENT_SUMMARY_CHARS,
        ),
        result_ref,
        output_refs,
        started_at_ms: event.started_at_ms,
        finished_at_ms: event.completed_at_ms.or(Some(event.at_ms)),
    })
}

fn merge_subagent_result_projection_items(
    session: &mut SessionStateSnapshot,
    new_items: Vec<SubagentResultProjectionItemV1>,
) -> Result<usize, String> {
    let mut projection = read_subagent_result_projection(session)?;
    let mut written = 0usize;
    for item in new_items {
        if let Some(existing) = projection
            .items
            .iter_mut()
            .find(|existing| existing.subagent_run_ref == item.subagent_run_ref)
        {
            if *existing != item {
                *existing = item;
                written = written.saturating_add(1);
            }
        } else {
            projection.items.push(item);
            written = written.saturating_add(1);
        }
    }
    projection.items.sort_by(|left, right| {
        right
            .finished_at_ms
            .unwrap_or_default()
            .cmp(&left.finished_at_ms.unwrap_or_default())
            .then_with(|| right.subagent_run_ref.cmp(&left.subagent_run_ref))
    });
    projection.items.truncate(MAX_SUBAGENT_RESULT_ITEMS);
    projection.recorded_at_ms = now_ms();
    let payload = serde_json::to_string(&projection)
        .map_err(|err| format!("serialize subagent result projection failed: {err}"))?;
    session
        .metadata
        .insert(SUBAGENT_RESULT_PROJECTION_META_KEY.to_string(), payload);
    Ok(written)
}

fn read_subagent_result_projection(
    session: &SessionStateSnapshot,
) -> Result<SubagentResultProjectionV1, String> {
    let Some(raw) = session
        .metadata
        .get(SUBAGENT_RESULT_PROJECTION_META_KEY)
        .map(String::as_str)
        .map(str::trim)
        .filter(|item| !item.is_empty())
    else {
        return Ok(empty_subagent_result_projection());
    };
    let projection = serde_json::from_str::<SubagentResultProjectionV1>(raw)
        .map_err(|err| format!("decode subagent result projection failed: {err}"))?;
    if projection.schema != SUBAGENT_RESULT_PROJECTION_SCHEMA {
        return Err(format!(
            "unsupported subagent result projection schema: {}",
            projection.schema
        ));
    }
    Ok(projection)
}

fn empty_subagent_result_projection() -> SubagentResultProjectionV1 {
    SubagentResultProjectionV1 {
        schema: SUBAGENT_RESULT_PROJECTION_SCHEMA.to_string(),
        items: vec![],
        recorded_at_ms: now_ms(),
    }
}

fn normalized_non_empty(value: &str) -> Option<String> {
    let normalized = value.trim();
    if normalized.is_empty() {
        None
    } else {
        Some(normalized.to_string())
    }
}

fn subagent_projection_status(status: &SubagentLifecycleStatus) -> &'static str {
    match status {
        SubagentLifecycleStatus::Succeeded => "succeeded",
        SubagentLifecycleStatus::Failed => "failed",
        SubagentLifecycleStatus::Cancelled => "cancelled",
        SubagentLifecycleStatus::Queued => "queued",
        SubagentLifecycleStatus::Leased => "leased",
        SubagentLifecycleStatus::Running => "running",
        SubagentLifecycleStatus::Waiting => "waiting",
    }
}

fn subagent_projection_default_title(kind: &SubagentSchedulerEventKind) -> &'static str {
    match kind {
        SubagentSchedulerEventKind::Succeeded => "Subagent completed",
        SubagentSchedulerEventKind::Failed => "Subagent failed",
        SubagentSchedulerEventKind::Cancelled => "Subagent cancelled",
        SubagentSchedulerEventKind::Claimed => "Subagent claimed work",
        SubagentSchedulerEventKind::Running => "Subagent running",
        SubagentSchedulerEventKind::Requeued => "Subagent requeued",
    }
}

fn compact_subagent_projection_text(value: &str, max_chars: usize) -> String {
    let mut result = String::new();
    for (index, character) in value.trim().chars().enumerate() {
        if index >= max_chars {
            result.push_str("...");
            return result;
        }
        result.push(character);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::contracts::JsonMap;

    #[test]
    fn subagent_result_projection_writes_terminal_refs_without_trace() {
        let mut session = SessionStateSnapshot::new("chat-parent".to_string(), 1);
        session.metadata = JsonMap::new();
        let written = merge_subagent_result_projection_items(
            &mut session,
            vec![build_projection_item(
                "chat-parent",
                &SubagentSchedulerEvent {
                    kind: SubagentSchedulerEventKind::Succeeded,
                    subagent_id: "agent-123".to_string(),
                    child_session_id: "session-agent-123".to_string(),
                    parent_turn_id: "turn-parent".to_string(),
                    job_id: "subagent.run:123".to_string(),
                    work_packet_ref: Some("external_context:subagent_work_packet:123".to_string()),
                    result_ref: Some("external_context:subagent_result:123".to_string()),
                    worker_id: Some("worker".to_string()),
                    status: SubagentLifecycleStatus::Succeeded,
                    summary: "Completed with bounded findings.".to_string(),
                    description: Some("Research bounded result projection".to_string()),
                    started_at_ms: Some(10),
                    completed_at_ms: Some(20),
                    at_ms: 20,
                },
            )
            .expect("terminal projection item")],
        )
        .expect("merge projection");

        assert_eq!(written, 1);
        let raw = session
            .metadata
            .get(SUBAGENT_RESULT_PROJECTION_META_KEY)
            .expect("projection metadata");
        let value = serde_json::from_str::<Value>(raw).expect("projection json");
        assert_eq!(
            value.get("schema").and_then(Value::as_str),
            Some("subagent_result_projection_v1")
        );
        let item = value
            .get("items")
            .and_then(Value::as_array)
            .and_then(|items| items.first())
            .expect("projection item");
        assert_eq!(
            item.get("childSessionRef").and_then(Value::as_str),
            Some("session:session-agent-123")
        );
        assert_eq!(
            item.get("resultRef").and_then(Value::as_str),
            Some("external_context:subagent_result:123")
        );
        let serialized = serde_json::to_string(&value).expect("serialize projection");
        assert!(!serialized.contains("workPacket\":"));
        assert!(!serialized.contains("toolCalls"));
        assert!(!serialized.contains("stdout"));
    }
}
