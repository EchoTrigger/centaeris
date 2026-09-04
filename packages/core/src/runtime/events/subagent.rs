use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SubagentEventKind {
    Spawned,
    Progress,
    ToolGroup,
    Result,
    Failed,
    Cancelled,
}

impl SubagentEventKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Spawned => "SubagentSpawned",
            Self::Progress => "SubagentProgress",
            Self::ToolGroup => "SubagentToolGroup",
            Self::Result => "SubagentResult",
            Self::Failed => "SubagentFailed",
            Self::Cancelled => "SubagentCancelled",
        }
    }

    fn default_status(self) -> &'static str {
        match self {
            Self::Spawned => "queued",
            Self::Failed => "error",
            Self::Result | Self::Cancelled => "done",
            Self::Progress | Self::ToolGroup => "running",
        }
    }

    fn process_state(self) -> RuntimeProcessState {
        match self {
            Self::Spawned | Self::Progress | Self::ToolGroup => RuntimeProcessState::Executing,
            Self::Result | Self::Failed | Self::Cancelled => RuntimeProcessState::Reviewing,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct SubagentEventPayload {
    pub subagent_id: String,
    pub parent_turn_id: String,
    pub status: Option<String>,
    pub role: Option<String>,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub description: Option<String>,
    pub message: Option<String>,
    pub session_state: Option<String>,
    pub child_session_id: Option<String>,
    pub child_turn_id: Option<String>,
    pub runtime_job_id: Option<String>,
    pub output_ref: Option<Value>,
    pub task_notification: Option<Value>,
    pub work_packet_ref: Option<String>,
    pub work_packet_summary: Option<Value>,
    pub tool_group_id: Option<String>,
    pub stats: Option<Value>,
    pub details: Option<Value>,
    pub source_event_ids: Vec<String>,
    pub result_envelope: Option<Value>,
    pub produced_refs: Option<Value>,
    pub reason: Option<String>,
    pub retryable: Option<bool>,
    pub started_at_ms: Option<TimestampMs>,
    pub completed_at_ms: Option<TimestampMs>,
}

impl SubagentEventPayload {
    pub(crate) fn new(subagent_id: impl Into<String>, parent_turn_id: impl Into<String>) -> Self {
        Self {
            subagent_id: subagent_id.into(),
            parent_turn_id: parent_turn_id.into(),
            status: None,
            role: None,
            title: None,
            summary: None,
            description: None,
            message: None,
            session_state: None,
            child_session_id: None,
            child_turn_id: None,
            runtime_job_id: None,
            output_ref: None,
            task_notification: None,
            work_packet_ref: None,
            work_packet_summary: None,
            tool_group_id: None,
            stats: None,
            details: None,
            source_event_ids: vec![],
            result_envelope: None,
            produced_refs: None,
            reason: None,
            retryable: None,
            started_at_ms: None,
            completed_at_ms: None,
        }
    }

    fn to_json(&self) -> Value {
        let mut payload = json!({
            "subagentId": self.subagent_id,
            "parentTurnId": self.parent_turn_id,
        });
        let Some(object) = payload.as_object_mut() else {
            return payload;
        };
        insert_non_empty_string(object, "role", self.role.as_deref());
        insert_non_empty_string(object, "title", self.title.as_deref());
        insert_non_empty_string(object, "summary", self.summary.as_deref());
        insert_non_empty_string(object, "description", self.description.as_deref());
        insert_non_empty_string(object, "message", self.message.as_deref());
        insert_non_empty_string(object, "sessionState", self.session_state.as_deref());
        insert_non_empty_string(object, "childSessionId", self.child_session_id.as_deref());
        insert_non_empty_string(object, "childTurnId", self.child_turn_id.as_deref());
        insert_non_empty_string(object, "runtimeJobId", self.runtime_job_id.as_deref());
        insert_non_empty_string(object, "workPacketRef", self.work_packet_ref.as_deref());
        insert_non_empty_string(object, "toolGroupId", self.tool_group_id.as_deref());
        insert_non_empty_string(object, "reason", self.reason.as_deref());
        if let Some(value) = self.work_packet_summary.as_ref() {
            object.insert("workPacketSummary".to_string(), value.clone());
        }
        if let Some(value) = self.output_ref.as_ref() {
            object.insert("outputRef".to_string(), value.clone());
        }
        if let Some(value) = self.task_notification.as_ref() {
            object.insert("taskNotification".to_string(), value.clone());
        }
        if let Some(value) = self.stats.as_ref() {
            object.insert("stats".to_string(), value.clone());
        }
        if let Some(value) = self.details.as_ref() {
            object.insert("details".to_string(), value.clone());
        }
        if !self.source_event_ids.is_empty() {
            object.insert(
                "sourceEventIds".to_string(),
                Value::Array(
                    self.source_event_ids
                        .iter()
                        .filter_map(|item| {
                            let normalized = item.trim();
                            if normalized.is_empty() {
                                None
                            } else {
                                Some(Value::String(normalized.to_string()))
                            }
                        })
                        .collect(),
                ),
            );
        }
        if let Some(value) = self.result_envelope.as_ref() {
            object.insert("resultEnvelope".to_string(), value.clone());
        }
        if let Some(value) = self.produced_refs.as_ref() {
            object.insert("producedRefs".to_string(), value.clone());
        }
        if let Some(value) = self.retryable {
            object.insert("retryable".to_string(), Value::Bool(value));
        }
        if let Some(value) = self.started_at_ms {
            object.insert("startedAtMs".to_string(), Value::Number(value.into()));
        }
        if let Some(value) = self.completed_at_ms {
            object.insert("completedAtMs".to_string(), Value::Number(value.into()));
        }
        payload
    }
}

pub(crate) fn build_runtime_event_subagent_spawned_from_tool_result(
    session_id: &str,
    turn_id: &str,
    parent_task_id: &str,
    report: &ToolExecutionResult,
) -> Result<Option<RuntimeEventProjection>, String> {
    if report.tool_name != "agent" || report.status != "ok" {
        return Ok(None);
    }
    if report.details.get("schema").and_then(Value::as_str) != Some("agent_tool_result_v1") {
        return Err(format!(
            "Agent tool result schema mismatch for callId={}",
            report.tool_call_id
        ));
    }
    let required = |field: &str| {
        report
            .details
            .get(field)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .ok_or_else(|| {
                format!(
                    "Agent tool result requires {field}: callId={}",
                    report.tool_call_id
                )
            })
    };
    let subagent_id = required("subagentId")?;
    let child_session_id = required("childSessionId")?;
    let child_turn_id = required("childTurnId")?;
    let runtime_job_id = required("runtimeJobId")?;
    let work_packet_ref = required("workPacketRef")?;
    let output_ref = report.details.get("outputRef").cloned().ok_or_else(|| {
        format!(
            "Agent tool result requires outputRef: callId={}",
            report.tool_call_id
        )
    })?;
    let mut payload = SubagentEventPayload::new(subagent_id.clone(), turn_id);
    payload.status = Some("queued".to_string());
    payload.role = Some("agent".to_string());
    payload.description = required("description").ok();
    payload.child_session_id = Some(child_session_id);
    payload.child_turn_id = Some(child_turn_id);
    payload.runtime_job_id = Some(runtime_job_id);
    payload.work_packet_ref = Some(work_packet_ref);
    payload.output_ref = Some(output_ref);
    payload.started_at_ms = Some(report.started_at_ms);
    Ok(Some(build_runtime_event_subagent_event_at(
        session_id,
        turn_id,
        subagent_id.as_str(),
        parent_task_id,
        SubagentEventKind::Spawned,
        &payload,
        report.started_at_ms,
    )))
}

#[cfg(test)]
pub(crate) fn build_runtime_event_subagent_event(
    session_id: &str,
    turn_id: &str,
    task_id: &str,
    parent_task_id: &str,
    kind: SubagentEventKind,
    payload: &SubagentEventPayload,
) -> RuntimeEventProjection {
    build_runtime_event_subagent_event_at(
        session_id,
        turn_id,
        task_id,
        parent_task_id,
        kind,
        payload,
        now_ms(),
    )
}

fn build_runtime_event_subagent_event_at(
    session_id: &str,
    turn_id: &str,
    task_id: &str,
    parent_task_id: &str,
    kind: SubagentEventKind,
    payload: &SubagentEventPayload,
    at_ms: TimestampMs,
) -> RuntimeEventProjection {
    let event_type = kind.as_str();
    let status = payload.status.as_deref().unwrap_or(kind.default_status());
    let process_state = kind.process_state();
    let at_ms_text = at_ms.to_string();
    let event_id = stable_session_event_id(
        "subagent",
        &[
            session_id,
            turn_id,
            task_id,
            event_type,
            at_ms_text.as_str(),
            payload.tool_group_id.as_deref().unwrap_or_default(),
        ],
    );
    typed_runtime_event(json!({
        "id": event_id,
        "version": "v1",
        "type": event_type,
        "at": at_ms,
        "sessionId": session_id,
        "turnId": turn_id,
        "taskId": task_id,
        "parentTaskId": parent_task_id,
        "status": status,
        "visibility": "user",
        "processState": process_state.as_str(),
        "payload": payload.to_json(),
        "meta": {
            "source": "core.agent_runtime",
            "protocol": "subagent.v1",
        }
    }))
}

pub(crate) fn build_runtime_event_subagent_event_from_scheduler_event(
    session_id: &str,
    turn_id: &str,
    scheduler_event: &SubagentSchedulerEvent,
) -> RuntimeEventProjection {
    let kind = match scheduler_event.kind {
        SubagentSchedulerEventKind::Claimed
        | SubagentSchedulerEventKind::Running
        | SubagentSchedulerEventKind::Requeued => SubagentEventKind::Progress,
        SubagentSchedulerEventKind::Succeeded => SubagentEventKind::Result,
        SubagentSchedulerEventKind::Failed => SubagentEventKind::Failed,
        SubagentSchedulerEventKind::Cancelled => SubagentEventKind::Cancelled,
    };
    let mut payload = SubagentEventPayload::new(
        scheduler_event.subagent_id.clone(),
        scheduler_event.parent_turn_id.clone(),
    );
    payload.status = Some(subagent_event_status_from_lifecycle(
        &scheduler_event.status,
        kind,
    ));
    payload.role = Some("worker".to_string());
    payload.title = Some(
        scheduler_event
            .description
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| subagent_scheduler_title(&scheduler_event.kind))
            .to_string(),
    );
    payload.summary = Some(compact_text(scheduler_event.summary.as_str(), 240));
    payload.description = scheduler_event
        .description
        .as_deref()
        .map(|value| compact_text(value, 800));
    payload.message = Some(compact_text(scheduler_event.summary.as_str(), 240));
    payload.work_packet_ref = scheduler_event.work_packet_ref.clone();
    payload.session_state = Some(
        match scheduler_event.kind {
            SubagentSchedulerEventKind::Claimed
            | SubagentSchedulerEventKind::Running
            | SubagentSchedulerEventKind::Requeued => "waiting_background",
            SubagentSchedulerEventKind::Succeeded
            | SubagentSchedulerEventKind::Failed
            | SubagentSchedulerEventKind::Cancelled => "attention_pending",
        }
        .to_string(),
    );
    let child_session_id = scheduler_event.child_session_id.clone();
    payload.child_session_id = Some(child_session_id.clone());
    if let Some(result_ref) = scheduler_event
        .result_ref
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        payload.produced_refs = Some(json!([{
            "schema": "task_output_ref_v1",
            "kind": "agent",
            "runtimeJobId": scheduler_event.job_id,
            "childSessionId": child_session_id.clone(),
            "resultRef": result_ref,
        }]));
    }
    if matches!(
        scheduler_event.kind,
        SubagentSchedulerEventKind::Succeeded
            | SubagentSchedulerEventKind::Failed
            | SubagentSchedulerEventKind::Cancelled
    ) {
        let output_ref = scheduler_event
            .result_ref
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|result_ref| {
                json!({
                    "schema": "task_output_ref_v1",
                    "kind": "agent",
                    "runtimeJobId": scheduler_event.job_id,
                    "childSessionId": child_session_id.clone(),
                    "resultRef": result_ref,
                })
            })
            .unwrap_or(Value::Null);
        payload.task_notification = Some(json!({
            "schema": "task_notification_v1",
            "title": subagent_scheduler_title(&scheduler_event.kind),
            "status": subagent_event_status_from_lifecycle(&scheduler_event.status, kind),
            "outputRef": output_ref,
            "childSessionId": child_session_id,
            "workPacketRef": scheduler_event.work_packet_ref.as_deref(),
        }));
    }
    payload.started_at_ms = scheduler_event.started_at_ms;
    payload.completed_at_ms = scheduler_event.completed_at_ms;
    payload.reason = match scheduler_event.kind {
        SubagentSchedulerEventKind::Failed | SubagentSchedulerEventKind::Cancelled => {
            Some(scheduler_event.summary.clone())
        }
        _ => None,
    };
    payload.retryable = match scheduler_event.kind {
        SubagentSchedulerEventKind::Requeued => Some(true),
        SubagentSchedulerEventKind::Failed => Some(false),
        _ => None,
    };

    build_runtime_event_subagent_event_at(
        session_id,
        turn_id,
        scheduler_event.subagent_id.as_str(),
        scheduler_event.parent_turn_id.as_str(),
        kind,
        &payload,
        scheduler_event.at_ms,
    )
}

pub(crate) fn build_runtime_event_subagent_tool_group_events_from_tool_results(
    session_id: &str,
    turn_id: &str,
    tool_results: &[ToolExecutionResult],
    source_events: &[RuntimeEventProjection],
) -> Vec<RuntimeEventProjection> {
    let source_event_ids_by_call_id = source_event_ids_by_tool_call_id(source_events);
    let mut events = vec![];
    for report in tool_results {
        let parsed = &report.details;
        let Some(trace) = parsed
            .get("subagentTrace")
            .or_else(|| parsed.get("subagent_trace"))
        else {
            continue;
        };
        let trace_subagent_id = trace
            .get("subagentId")
            .or_else(|| trace.get("subagent_id"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let Some(tool_groups) = trace
            .get("toolGroups")
            .or_else(|| trace.get("tool_groups"))
            .and_then(Value::as_array)
        else {
            continue;
        };
        for (index, group) in tool_groups.iter().enumerate() {
            let subagent_id = group
                .get("subagentId")
                .or_else(|| group.get("subagent_id"))
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| trace_subagent_id.clone());
            let Some(subagent_id) = subagent_id.filter(|item| !item.trim().is_empty()) else {
                continue;
            };
            let tool_group_id = group
                .get("toolGroupId")
                .or_else(|| group.get("tool_group_id"))
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| {
                    format!(
                        "{}:tool_group:{}",
                        subagent_id,
                        stable_text_hash(format!("{}:{index}", report.tool_call_id).as_str())
                    )
                });
            let mut source_event_ids = group
                .get("sourceEventIds")
                .or_else(|| group.get("source_event_ids"))
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::trim)
                        .filter(|item| !item.is_empty())
                        .map(str::to_string)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            if source_event_ids.is_empty() {
                if let Some(items) = source_event_ids_by_call_id.get(report.tool_call_id.as_str()) {
                    source_event_ids.extend(items.iter().cloned());
                }
            }

            let mut payload = SubagentEventPayload::new(subagent_id.clone(), turn_id);
            payload.status = group
                .get("status")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| {
                    Some(event_status_from_tool_status(report.status.as_str()).to_string())
                });
            payload.role = group
                .get("role")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| Some("worker".to_string()));
            payload.title = group
                .get("title")
                .and_then(Value::as_str)
                .map(|item| compact_text(item, 160));
            payload.summary = group
                .get("summary")
                .and_then(Value::as_str)
                .map(|item| compact_text(item, 240));
            payload.description = group
                .get("description")
                .and_then(Value::as_str)
                .map(|item| compact_text(item, 800))
                .or_else(|| payload.summary.clone());
            payload.message = group
                .get("message")
                .and_then(Value::as_str)
                .map(|item| compact_text(item, 240));
            payload.tool_group_id = Some(tool_group_id.clone());
            payload.stats = group.get("stats").cloned();
            payload.details = group
                .get("details")
                .or_else(|| group.get("items"))
                .or_else(|| group.get("operations"))
                .or_else(|| group.get("toolCalls"))
                .or_else(|| group.get("tool_calls"))
                .cloned();
            payload.source_event_ids = source_event_ids;
            payload.reason = group
                .get("reason")
                .and_then(Value::as_str)
                .map(str::to_string);
            payload.retryable = group.get("retryable").and_then(Value::as_bool);

            events.push(build_runtime_event_subagent_event_at(
                session_id,
                turn_id,
                subagent_id.as_str(),
                turn_id,
                SubagentEventKind::ToolGroup,
                &payload,
                report.completed_at_ms,
            ));
        }
    }
    events
}

fn source_event_ids_by_tool_call_id(
    source_events: &[RuntimeEventProjection],
) -> HashMap<String, Vec<String>> {
    let mut grouped: HashMap<String, Vec<String>> = HashMap::new();
    for event in source_events {
        if !matches!(
            event.event_type.as_str(),
            "ToolCall" | "ToolCallReady" | "ToolProgress" | "ToolResult"
        ) {
            continue;
        }
        let call_id = event
            .payload
            .get("callId")
            .and_then(Value::as_str)
            .or(Some(event.task_id.as_str()));
        let Some(call_id) = call_id else {
            continue;
        };
        grouped
            .entry(call_id.to_string())
            .or_default()
            .push(event.event_id.clone());
    }
    grouped
}

fn subagent_event_status_from_lifecycle(
    status: &SubagentLifecycleStatus,
    kind: SubagentEventKind,
) -> String {
    match (status, kind) {
        (_, SubagentEventKind::Failed) => "error".to_string(),
        (_, SubagentEventKind::Result | SubagentEventKind::Cancelled) => "done".to_string(),
        (SubagentLifecycleStatus::Failed, _) => "error".to_string(),
        (SubagentLifecycleStatus::Succeeded | SubagentLifecycleStatus::Cancelled, _) => {
            "done".to_string()
        }
        _ => "running".to_string(),
    }
}

fn subagent_scheduler_title(kind: &SubagentSchedulerEventKind) -> &'static str {
    match kind {
        SubagentSchedulerEventKind::Claimed => "Subagent claimed work",
        SubagentSchedulerEventKind::Running => "Subagent running",
        SubagentSchedulerEventKind::Succeeded => "Subagent completed",
        SubagentSchedulerEventKind::Failed => "Subagent failed",
        SubagentSchedulerEventKind::Requeued => "Subagent requeued",
        SubagentSchedulerEventKind::Cancelled => "Subagent cancelled",
    }
}

fn insert_non_empty_string(
    object: &mut serde_json::Map<String, Value>,
    key: &str,
    value: Option<&str>,
) {
    let Some(normalized) = value.map(str::trim).filter(|item| !item.is_empty()) else {
        return;
    };
    object.insert(key.to_string(), Value::String(normalized.to_string()));
}
