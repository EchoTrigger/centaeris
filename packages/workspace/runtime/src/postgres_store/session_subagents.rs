//! Authorized presentation reads of Core's durable subagent jobs and task history.
use centaeris_core::runtime::driver_user_message_id;
use centaeris_core::runtime::subagent::{
    subagent_work_packet_runtime_binding, SubagentWorkPacketEnvelope, SUBAGENT_RUN_JOB_KIND,
};
use centaeris_core::session::history::SessionHistoryRecordV1;
use centaeris_core::session::reliability::{RuntimeJobRecord, RuntimeJobStatus};
use centaeris_core::session::state::{MessageRole, ModelMessageSemanticsV1, ModelToolCallStateV1};
use postgres::{GenericClient, Transaction};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::PostgresRuntimeStore;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SessionSubagentsReadRequest {
    pub schema: String,
    pub session_id: String,
    pub workspace_id: String,
    pub owner_id: i64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SessionSubagentReadRequest {
    pub schema: String,
    pub session_id: String,
    pub workspace_id: String,
    pub owner_id: i64,
    pub runtime_job_id: String,
}

fn valid_scope(session_id: &str, workspace_id: &str, owner_id: i64) -> bool {
    !session_id.trim().is_empty() && !workspace_id.trim().is_empty() && owner_id > 0
}

impl SessionSubagentsReadRequest {
    pub(crate) fn validate(&self) -> bool {
        self.schema == "runtime.session.subagents.read.v1"
            && valid_scope(&self.session_id, &self.workspace_id, self.owner_id)
    }
}

impl SessionSubagentReadRequest {
    pub(crate) fn validate(&self) -> bool {
        self.schema == "runtime.session.subagent.read.v1"
            && valid_scope(&self.session_id, &self.workspace_id, self.owner_id)
            && !self.runtime_job_id.trim().is_empty()
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionSubagent {
    pub runtime_job_id: String,
    pub call_id: String,
    pub child_session_id: String,
    #[serde(skip)]
    child_turn_id: String,
    pub parent_agent_run_id: String,
    pub parent_turn_id: String,
    pub description: String,
    pub prompt: String,
    pub status: RuntimeJobStatus,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionSubagents {
    pub schema: &'static str,
    pub session_id: String,
    pub agents: Vec<SessionSubagent>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionSubagentMessage {
    message_id: String,
    role: &'static str,
    content: String,
    created_at_ms: i64,
    reasoning_content: Option<String>,
    tool_calls: Vec<ModelToolCallStateV1>,
    tool_call_id: Option<String>,
    tool_name: Option<String>,
    result_state: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionSubagentDetail {
    pub schema: &'static str,
    pub session_id: String,
    pub agent: SessionSubagent,
    pub messages: Vec<SessionSubagentMessage>,
}

fn lock_scope(
    tx: &mut Transaction<'_>,
    session_id: &str,
    workspace_id: &str,
    owner_id: i64,
) -> Result<bool, String> {
    tx.query_opt(
        "SELECT id FROM public.app_core_session WHERE id=$1 AND workspace_id=$2 AND owner_id::bigint=$3 AND purpose='interactive' AND status='active' AND \"purgedAt\" IS NULL FOR SHARE",
        &[&session_id, &workspace_id, &owner_id],
    )
    .map(|row| row.is_some())
    .map_err(|error| format!("lock subagent parent Session failed: {error}"))
}

fn present_agent(
    job: &RuntimeJobRecord,
    root_session_id: &str,
    content: Value,
    metadata: Value,
) -> Result<SessionSubagent, String> {
    if job.job_kind != SUBAGENT_RUN_JOB_KIND || job.session_id.as_deref() != Some(root_session_id) {
        return Err("session_subagent_job_scope_invalid".into());
    }
    let envelope = SubagentWorkPacketEnvelope {
        ref_id: job
            .payload_ref
            .clone()
            .ok_or("session_subagent_packet_missing")?,
        content_json: content,
    };
    let binding = subagent_work_packet_runtime_binding(&envelope, job)?;
    let packet = envelope
        .content_json
        .get("workPacket")
        .unwrap_or(&envelope.content_json);
    let call_id = metadata
        .get("toolCallId")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or("session_subagent_call_id_missing")?;
    for (field, expected) in [
        ("schema", "agent_tool_work_packet_v1"),
        ("sessionId", root_session_id),
        ("parentTurnId", binding.parent_turn_id.as_str()),
        ("childTurnId", binding.child_turn_id.as_str()),
        ("childSessionId", binding.child_session_id.as_str()),
        ("subagentId", binding.subagent_id.as_str()),
        ("runtimeJobId", job.job_id.as_str()),
    ] {
        if metadata.get(field).and_then(Value::as_str) != Some(expected) {
            return Err(format!(
                "session_subagent_metadata_binding_invalid: {field}"
            ));
        }
    }
    if packet
        .get("run_context")
        .and_then(|value| value.get("sessionId"))
        .and_then(Value::as_str)
        != Some(root_session_id)
        || binding.child_session_id == root_session_id
    {
        return Err("session_subagent_packet_scope_invalid".into());
    }
    let prompt = packet
        .get("task_brief")
        .and_then(|value| value.get("objective"))
        .and_then(Value::as_str)
        .ok_or("session_subagent_prompt_missing")?
        .to_string();
    Ok(SessionSubagent {
        runtime_job_id: job.job_id.clone(),
        call_id: call_id.to_string(),
        child_session_id: binding.child_session_id,
        child_turn_id: binding.child_turn_id,
        parent_agent_run_id: binding.parent_agent_run_id,
        parent_turn_id: binding.parent_turn_id,
        description: binding.description,
        prompt,
        status: job.status.clone(),
        created_at_ms: job.created_at_ms,
        updated_at_ms: job.updated_at_ms,
    })
}

fn load_agent(
    tx: &mut impl GenericClient,
    job: &RuntimeJobRecord,
    root_session_id: &str,
) -> Result<SessionSubagent, String> {
    let reference = job
        .payload_ref
        .as_deref()
        .ok_or("session_subagent_packet_missing")?;
    let row = tx.query_opt(
        "SELECT schema_version,object_kind,source_provider_id,source_tool_name,content,metadata_json FROM runtime.external_context_objects WHERE object_id=$1",
        &[&reference],
    ).map_err(|error| format!("load subagent WorkPacket failed: {error}"))?
        .ok_or("session_subagent_packet_missing")?;
    if row.get::<_, String>(0) != "external_context.v1"
        || row.get::<_, String>(1) != "subagent_work_packet"
        || row.get::<_, String>(2) != "centaeris.core"
        || !matches!(
            row.get::<_, String>(3).as_str(),
            "agent" | "spawn_agent" | "followup_task"
        )
    {
        return Err("session_subagent_packet_source_invalid".into());
    }
    let content = serde_json::from_str(&row.get::<_, String>(4))
        .map_err(|error| format!("decode subagent WorkPacket failed: {error}"))?;
    let metadata = serde_json::from_str(&row.get::<_, String>(5))
        .map_err(|error| format!("decode subagent WorkPacket metadata failed: {error}"))?;
    let agent = present_agent(job, root_session_id, content, metadata)?;
    if tx
        .query_opt(
            "SELECT id FROM public.app_core_session WHERE id=$1",
            &[&agent.child_session_id],
        )
        .map_err(|error| format!("check subagent child Session scope failed: {error}"))?
        .is_some()
    {
        return Err("session_subagent_child_scope_invalid".into());
    }
    Ok(agent)
}

fn present_messages(history: Vec<SessionHistoryRecordV1>) -> Vec<SessionSubagentMessage> {
    let mut messages = Vec::with_capacity(history.len());
    for record in history {
        // Prompt context may replay another task's input after compaction.
        // Only the selected task's original user request belongs in this view.
        let role = match record.entry.message.role {
            MessageRole::User if record.entry.is_original_user_request() => "user",
            MessageRole::Assistant => "assistant",
            MessageRole::Tool => "tool",
            MessageRole::User | MessageRole::System => continue,
        };
        let message = record.entry.message;
        let mut presented = SessionSubagentMessage {
            message_id: message.message_id,
            role,
            content: message.content,
            created_at_ms: message.created_at_ms,
            reasoning_content: None,
            tool_calls: vec![],
            tool_call_id: None,
            tool_name: None,
            result_state: None,
        };
        match record.entry.model_semantics {
            ModelMessageSemanticsV1::Plain => {}
            ModelMessageSemanticsV1::Assistant {
                reasoning_content,
                tool_calls,
            } => {
                presented.reasoning_content = reasoning_content;
                presented.tool_calls = tool_calls;
            }
            ModelMessageSemanticsV1::ToolResult {
                tool_call_id,
                tool_name,
                result_state,
                ..
            } => {
                presented.tool_call_id = Some(tool_call_id);
                presented.tool_name = Some(tool_name);
                presented.result_state = Some(result_state);
            }
        }
        messages.push(presented);
    }
    messages
}

impl PostgresRuntimeStore {
    pub(crate) fn read_session_subagents(
        &self,
        request: &SessionSubagentsReadRequest,
    ) -> Result<Option<SessionSubagents>, String> {
        self.with_client(|client| {
            let mut tx = client.transaction().map_err(|error| error.to_string())?;
            if !lock_scope(&mut tx, &request.session_id, &request.workspace_id, request.owner_id)? {
                return Ok(None);
            }
            let rows = tx.query(
                "SELECT job_id FROM runtime.runtime_jobs WHERE session_id=$1 AND job_kind=$2 ORDER BY created_at_ms,job_id",
                &[&request.session_id, &SUBAGENT_RUN_JOB_KIND],
            ).map_err(|error| format!("list Session subagent jobs failed: {error}"))?;
            let mut agents = Vec::with_capacity(rows.len());
            for row in rows {
                let job_id: String = row.get(0);
                let job = super::reliability::load_job(&mut tx, &job_id)?
                    .ok_or("session_subagent_job_missing")?;
                agents.push(load_agent(&mut tx, &job, &request.session_id)?);
            }
            tx.commit().map_err(|error| error.to_string())?;
            Ok(Some(SessionSubagents {
                schema: "workspace.session.subagents.v1",
                session_id: request.session_id.clone(),
                agents,
            }))
        })
    }

    pub(crate) fn read_session_subagent(
        &self,
        request: &SessionSubagentReadRequest,
    ) -> Result<Option<SessionSubagentDetail>, String> {
        self.with_client(|client| {
            let mut tx = client.transaction().map_err(|error| error.to_string())?;
            if !lock_scope(
                &mut tx,
                &request.session_id,
                &request.workspace_id,
                request.owner_id,
            )? {
                return Ok(None);
            }
            let Some(job) = super::reliability::load_job(&mut tx, &request.runtime_job_id)? else {
                return Ok(None);
            };
            if job.session_id.as_deref() != Some(request.session_id.as_str())
                || job.job_kind != SUBAGENT_RUN_JOB_KIND
            {
                return Ok(None);
            }
            let agent = load_agent(&mut tx, &job, &request.session_id)?;
            let input_id = driver_user_message_id(&agent.child_session_id, &agent.child_turn_id);
            let history = super::runtime::session_history::load_original_user_request_history(
                &mut tx,
                &agent.child_session_id,
                &input_id,
            )?;
            tx.commit().map_err(|error| error.to_string())?;
            Ok(Some(SessionSubagentDetail {
                schema: "workspace.session.subagent.v1",
                session_id: request.session_id.clone(),
                agent,
                messages: present_messages(history),
            }))
        })
    }
}

#[cfg(test)]
mod tests;
