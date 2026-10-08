use super::*;
use crate::runtime::contracts::TimestampMs;
use crate::runtime::subagent::{
    SubagentLifecycleStatus, SubagentSchedulerEvent, SubagentSchedulerEventKind,
};
use crate::session::reliability::RuntimeJobStorePort;
use crate::session::store::AgentRuntimeSnapshotStorePort;

const SUBAGENT_RESULT_PROJECTION_SCHEMA: &str = "subagent_result_projection_v1";
const MAX_SUBAGENT_RESULT_ITEMS: usize = 64;
const MAX_SUBAGENT_TITLE_CHARS: usize = 180;
const MAX_SUBAGENT_DESCRIPTION_CHARS: usize = 800;
const MAX_SUBAGENT_SUMMARY_CHARS: usize = 1_200;
const SNAPSHOT_CAS_ATTEMPTS: usize = 16;

pub fn validate_wait_recovery_replay<S: RuntimeJobStorePort>(
    store: &S,
    sealed: &SessionStateSnapshot,
    current: &SessionStateSnapshot,
) -> Result<(), String> {
    validate_projection(store, sealed)?;
    validate_projection(store, current)?;
    let mut semantic_sealed = sealed.clone();
    let mut semantic_current = current.clone();
    // Only Core's verified, derived terminal-child projection may evolve while
    // the same wait is sealed. All other state, including private wait metadata,
    // remains part of the exact replay contract.
    semantic_sealed
        .metadata
        .remove(SUBAGENT_RESULT_PROJECTION_META_KEY);
    semantic_current
        .metadata
        .remove(SUBAGENT_RESULT_PROJECTION_META_KEY);
    if serde_json::to_value(semantic_sealed).map_err(|e| e.to_string())?
        != serde_json::to_value(semantic_current).map_err(|e| e.to_string())?
    {
        return Err("wait recovery changed Core state".into());
    }
    Ok(())
}

pub fn reconstruct_wait_recovery_snapshot<S: RuntimeJobStorePort>(
    store: &S,
    sealed: &SessionStateSnapshot,
) -> Result<SessionStateSnapshot, String> {
    validate_projection(store, sealed)?;
    let mut restored = sealed.clone();
    let retained = read_subagent_result_projection(sealed)?;
    let mut items = Vec::new();
    let mut offset = 0;
    const PAGE_SIZE: usize = 256;
    loop {
        let jobs =
            store.list_runtime_jobs(crate::session::reliability::ListRuntimeJobsRequest {
                statuses: vec![
                    RuntimeJobStatus::Succeeded,
                    RuntimeJobStatus::Failed,
                    RuntimeJobStatus::DeadLettered,
                    RuntimeJobStatus::Cancelled,
                ],
                job_kind: Some(super::subagent::SUBAGENT_RUN_JOB_KIND.into()),
                session_id: Some(sealed.session_id.clone()),
                branch_id: None,
                limit: PAGE_SIZE,
                offset,
            })?;
        let count = jobs.len();
        for job in jobs {
            let item = authoritative_projection_item(&sealed.session_id, &job)?;
            items.push(
                retained
                    .items
                    .iter()
                    .find(|old| old.subagent_run_ref == item.subagent_run_ref)
                    .cloned()
                    .unwrap_or(item),
            );
            items.sort_by(|left, right| {
                right
                    .finished_at_ms
                    .unwrap_or_default()
                    .cmp(&left.finished_at_ms.unwrap_or_default())
                    .then_with(|| right.subagent_run_ref.cmp(&left.subagent_run_ref))
            });
            items.truncate(MAX_SUBAGENT_RESULT_ITEMS);
        }
        if count < PAGE_SIZE {
            break;
        }
        offset += count;
    }
    if !items.is_empty() {
        merge_subagent_result_projection_items(&mut restored, items)?;
    }
    Ok(restored)
}

pub fn restore_wait_recovery_snapshot<S: RuntimeJobStorePort + AgentRuntimeSnapshotStorePort>(
    store: &S,
    sealed: &SessionStateSnapshot,
) -> Result<SessionStateSnapshot, String> {
    let mut first_observed = None;
    for _ in 0..SNAPSHOT_CAS_ATTEMPTS {
        let raw = store.load_agent_runtime_snapshot(&sealed.session_id)?;
        let mut basis = sealed.clone();
        if let Some(raw) = &raw {
            let current: SessionStateSnapshot =
                serde_json::from_str(raw).map_err(|e| e.to_string())?;
            if current.session_id != sealed.session_id {
                return Err("wait recovery snapshot session mismatch".into());
            }
            validate_projection(store, &current)?;
            if let Some(first) = &first_observed {
                validate_wait_recovery_replay(store, first, &current)?;
            } else {
                // The caller has authorized rollback to an immutable recovery
                // checkpoint. Retain that first observation to detect unrelated
                // advancement during a CAS retry, rather than overwriting it.
                first_observed = Some(current.clone());
            }
            let retained = read_subagent_result_projection(&current)?;
            if !retained.items.is_empty() {
                merge_subagent_result_projection_items(&mut basis, retained.items)?;
            }
        }
        if first_observed.is_none() {
            first_observed = Some(sealed.clone());
        }
        let snapshot = reconstruct_wait_recovery_snapshot(store, &basis)?;
        if store.compare_and_save_agent_runtime_snapshot(
            &sealed.session_id,
            raw.as_deref(),
            &serde_json::to_string(&snapshot).map_err(|e| e.to_string())?,
            now_ms(),
        )? {
            return Ok(snapshot);
        }
    }
    Err("wait recovery snapshot concurrent update".into())
}

fn authoritative_projection_item(
    parent: &str,
    job: &RuntimeJobRecord,
) -> Result<SubagentResultProjectionItemV1, String> {
    if job.job_kind != super::subagent::SUBAGENT_RUN_JOB_KIND
        || job.session_id.as_deref() != Some(parent)
        || !job.status.is_terminal()
    {
        return Err("subagent projection ownership or terminal mismatch".into());
    }
    let kind = match job.status {
        RuntimeJobStatus::Succeeded => SubagentSchedulerEventKind::Succeeded,
        RuntimeJobStatus::Failed | RuntimeJobStatus::DeadLettered => {
            SubagentSchedulerEventKind::Failed
        }
        RuntimeJobStatus::Cancelled => SubagentSchedulerEventKind::Cancelled,
        _ => return Err("subagent projection job is not terminal".into()),
    };
    let summary = subagent_projection_default_title(&kind);
    let event = super::subagent::scheduler_event_from_job(
        job,
        kind,
        SubagentLifecycleStatus::from(job.status.clone()),
        None,
        summary,
        job.updated_at_ms,
    )?;
    build_projection_item(parent, &event)
        .ok_or_else(|| "subagent projection terminal item missing".into())
}

fn validate_projection<S: RuntimeJobStorePort>(
    store: &S,
    snapshot: &SessionStateSnapshot,
) -> Result<(), String> {
    let projection = read_subagent_result_projection(snapshot)?;
    if projection.items.len() > MAX_SUBAGENT_RESULT_ITEMS {
        return Err("subagent result projection exceeds item limit".into());
    }
    let mut seen = std::collections::HashSet::new();
    for item in &projection.items {
        // The producer appends three dots after its retained character budget.
        if item.title.chars().count() > MAX_SUBAGENT_TITLE_CHARS + 3
            || item
                .description
                .as_ref()
                .is_some_and(|value| value.chars().count() > MAX_SUBAGENT_DESCRIPTION_CHARS + 3)
            || item.bounded_summary.chars().count() > MAX_SUBAGENT_SUMMARY_CHARS + 3
        {
            return Err("subagent projection display bounds exceeded".into());
        }
        let id = item
            .subagent_run_ref
            .strip_prefix("runtime_job:")
            .ok_or("subagent projection runtime job ref invalid")?;
        if !seen.insert(id) {
            return Err("subagent projection duplicate job".into());
        }
        let job = store
            .get_runtime_job(id)?
            .ok_or("subagent projection job missing")?;
        let expected = authoritative_projection_item(&snapshot.session_id, &job)?;
        if item.subagent_id != expected.subagent_id
            || item.child_session_ref != expected.child_session_ref
            || item.parent_turn_id != expected.parent_turn_id
            || item.work_packet_ref != expected.work_packet_ref
            || item.status != expected.status
            || item.result_ref != expected.result_ref
            || item.output_refs != expected.output_refs
        {
            return Err("subagent projection ownership or terminal mismatch".into());
        }
    }
    Ok(())
}

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
    S: AgentRuntimeSnapshotStorePort + RuntimeJobStorePort + Clone,
{
    let mut terminal_items = Vec::new();
    for event in events {
        let Some(item) = build_projection_item(parent_session_id, event) else {
            continue;
        };
        let job = store
            .get_runtime_job(&event.job_id)?
            .ok_or_else(|| format!("subagent projection job missing: {}", event.job_id))?;
        let canonical = super::subagent::scheduler_event_from_job(
            &job,
            event.kind.clone(),
            SubagentLifecycleStatus::from(job.status.clone()),
            event.worker_id.clone(),
            &event.summary,
            job.updated_at_ms,
        )?;
        // Scheduler notifications are hints. Recheck authoritative durable facts
        // before they can populate a Session's result projection.
        if job.job_kind != super::subagent::SUBAGENT_RUN_JOB_KIND
            || job.session_id.as_deref() != Some(parent_session_id)
            || canonical.subagent_id != event.subagent_id
            || canonical.child_session_id != event.child_session_id
            || job.branch_id.as_deref() != Some(event.parent_turn_id.as_str())
            || !job.status.is_terminal()
            || SubagentLifecycleStatus::from(job.status.clone()) != event.status
            || job.output_refs.first() != event.result_ref.as_ref()
            || job.payload_ref != event.work_packet_ref
            || !matches!(
                (&event.kind, &event.status),
                (
                    SubagentSchedulerEventKind::Succeeded,
                    SubagentLifecycleStatus::Succeeded
                ) | (
                    SubagentSchedulerEventKind::Failed,
                    SubagentLifecycleStatus::Failed
                ) | (
                    SubagentSchedulerEventKind::Cancelled,
                    SubagentLifecycleStatus::Cancelled
                )
            )
        {
            return Err(format!(
                "subagent projection ownership or terminal mismatch: {}",
                event.job_id
            ));
        }
        terminal_items.push(item);
    }
    if terminal_items.is_empty() {
        return Ok(0);
    }

    for _ in 0..SNAPSHOT_CAS_ATTEMPTS {
        let raw = store.load_agent_runtime_snapshot(parent_session_id)?;
        let mut session = match &raw {
            Some(raw) => {
                serde_json::from_str::<SessionStateSnapshot>(raw).map_err(|e| e.to_string())?
            }
            None => SessionStateSnapshot::new(parent_session_id.into(), now_ms()),
        };
        let written = merge_subagent_result_projection_items(&mut session, terminal_items.clone())?;
        if written == 0 {
            return Ok(0);
        }
        if store.compare_and_save_agent_runtime_snapshot(
            parent_session_id,
            raw.as_deref(),
            &serde_json::to_string(&session).map_err(|e| e.to_string())?,
            now_ms(),
        )? {
            return Ok(written);
        }
    }
    Err("subagent projection snapshot concurrent update".into())
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
