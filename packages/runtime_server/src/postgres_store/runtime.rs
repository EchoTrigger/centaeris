use postgres::{GenericClient, Row, Transaction};
use sha2::{Digest, Sha256};

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex};

use centaeris_core::runtime::contracts::{
    CheckpointKindV1, CheckpointRecord, EventVisibility, RuntimeEvent,
};
use centaeris_core::session::reliability::AGENT_RUN_LIFECYCLE_JOB_KIND;
use centaeris_core::session::store::{
    AgentRuntimeSnapshotStorePort, RuntimeStore, RuntimeStoreError, SessionDataStorePort,
};
use centaeris_core::session::transcript::TranscriptProjectionStorePort;
use centaeris_core::session::{
    parse_wire_record, reduce_event, reduce_events, rewrite_last_user_tail_tombstone,
    session_record_projects_to_agent_run_stream, validate_sequenced_session_records,
    wire_record_value, CommittedSessionRecord, RewriteLastUserTailRequest, RuntimeJobLeaseFence,
    SequencedSessionRecord, SessionCommitReceipt, SessionLogFuture, SessionLogPort,
    SessionProjection, SessionRecordType, RUNTIME_JOB_LEASE_FENCE_REJECTED,
};

use super::{
    run_postgres_blocking, AgentRunExecutionControlState, PostgresConnectionPool,
    PostgresRuntimeStore,
};

pub(crate) fn validate_hosted_native_input(
    tx: &mut impl GenericClient,
    start: &crate::contract::AgentRunStart,
    input: &centaeris_core::session::host_event_input::HostEventInput,
) -> Result<(), String> {
    input.validate(&start.authorization.session_id)?;
    let row = tx.query_opt("SELECT a.notice_id,a.input_binding::text,n.payload::text,o.command,o.\"agentRunId\",o.\"turnId\",o.\"sessionId\",o.user_id::text,o.workspace_id,s.prompt FROM app_core_agentworkconsumeattempt a JOIN app_core_agentrun r ON r.id=a.coordinator_run_id JOIN auth_user u ON u.id=r.user_id JOIN app_core_workspace ws ON ws.id=r.workspace_id JOIN app_core_session coord ON coord.id=r.session_id JOIN app_core_agent ag ON ag.id=coord.agent_id JOIN app_core_agentworkreturn n ON n.id=a.notice_id JOIN app_core_agentworksession w ON w.session_id=n.work_id JOIN app_core_agentrun s ON s.id=w.source_run_id JOIN app_core_agentrun c ON c.id=n.child_run_id JOIN app_core_session child ON child.id=c.session_id JOIN app_core_agent child_agent ON child_agent.id=child.agent_id JOIN app_core_agentrunauthorization z ON z.agent_run_id=r.id JOIN app_core_hostedoperationreceipt o ON o.id=a.operation_id WHERE a.id=$1 AND r.id=$2 AND r.session_id=$3 AND r.turn_id=$5 AND z.digest=$4 AND r.user_id::text=$7 AND r.workspace_id=$8 AND u.is_active AND ws.status='active' AND coord.status='active' AND ag.status='active' AND ag.id=$6 AND coord.owner_id=r.user_id AND ag.owner_id=r.user_id AND coord.workspace_id=r.workspace_id AND ag.workspace_id=r.workspace_id AND ag.definition_id IS NULL AND r.definition_version_id IS NULL AND r.acting_app_id IS NULL AND r.app_delegation_id IS NULL AND EXISTS(SELECT 1 FROM app_core_agentcoordinationsession b WHERE b.agent_id=ag.id AND b.session_id=coord.id) AND w.coordination_session_id=r.session_id AND s.workspace_id=r.workspace_id AND c.workspace_id=r.workspace_id AND s.user_id=r.user_id AND c.user_id=r.user_id AND s.membership_ref=r.membership_ref AND c.membership_ref=r.membership_ref AND child.owner_id=r.user_id AND child.workspace_id=r.workspace_id AND child.status='active' AND child_agent.status='active' AND child_agent.id=$6 AND c.session_id=w.session_id AND s.session_id=w.coordination_session_id AND EXISTS(SELECT 1 FROM app_core_workspacemembership m WHERE m.id=r.membership_ref AND m.workspace_id=r.workspace_id AND m.user_id=r.user_id AND m.role IN('owner','admin','member'))",
        &[&input.input_id,&start.agent_run_id,&start.authorization.session_id,&start.authorization_digest,&start.turn_id,&start.authorization.agent_id,&start.authorization.user_id,&start.authorization.workspace_id]).map_err(|e| e.to_string())?.ok_or("host_event_admission_missing")?;
    let binding: serde_json::Value =
        serde_json::from_str(&row.get::<_, String>(1)).map_err(|e| e.to_string())?;
    let notice: serde_json::Value =
        serde_json::from_str(&row.get::<_, String>(2)).map_err(|e| e.to_string())?;
    if let crate::contract::AgentRunStartInitialInput::HostEvent {
        input: initial,
        notice_id,
        ..
    } = &start.initial_input
    {
        if initial.input_id == input.input_id
            && (*notice_id != row.get::<_, String>(0) || start.prompt != row.get::<_, String>(9))
        {
            return Err("host_event_admission_binding_mismatch".into());
        }
    }
    let expected = serde_json::json!({"schema":"workspace.agent_work.consume_binding.v1",
        "agentRunId":start.agent_run_id,"turnId":start.turn_id,"sessionId":start.authorization.session_id,
        "authorizationDigest":start.authorization_digest,"initialInput":{"type":"hostEvent","attemptId":input.input_id,
        "noticeId":row.get::<_,String>(0),"input":input}});
    let operation_run: Option<String> = row.get(4);
    let operation_turn: Option<String> = row.get(5);
    if binding != expected
        || serde_json::from_str::<serde_json::Value>(&input.content).map_err(|e| e.to_string())?
            != notice
        || notice["source"].as_str() != Some(input.source.as_str())
        || row.get::<_, String>(3) != "consumeWorkReturn"
        || operation_run
            .as_deref()
            .is_some_and(|run| run != start.agent_run_id)
        || operation_turn
            .as_deref()
            .is_some_and(|turn| turn != start.turn_id)
        || operation_run.is_some() != operation_turn.is_some()
        || row.get::<_, String>(6) != start.authorization.session_id
        || row.get::<_, String>(7) != start.authorization.user_id
        || row.get::<_, String>(8) != start.authorization.workspace_id
    {
        return Err("host_event_admission_binding_mismatch".into());
    }
    Ok(())
}

const AGENT_RUN_CANCEL_REQUESTED_EVENT_SCHEMA: &str = "runtime.agent_run.cancel_requested.v1";

fn is_successful_work_confirmation(event: &centaeris_core::session::SessionLogRecord) -> bool {
    event.event_type == SessionRecordType::ToolResult
        && event.payload["toolName"] == "confirm_work_return"
        && event.payload["resultState"] == "successWithOutput"
}

fn confirmation_receipt(
    event: &centaeris_core::session::SessionLogRecord,
) -> Result<crate::agent_work::WorkConfirmationRecord, String> {
    serde_json::from_str(
        event.payload["modelContent"]
            .as_str()
            .ok_or("work_confirmation_receipt_missing")?,
    )
    .map_err(|_| "work_confirmation_receipt_invalid".into())
}

fn lock_work_confirmation_scope(
    tx: &mut Transaction<'_>,
    start: &crate::contract::AgentRunStart,
    events: &[SequencedSessionRecord],
) -> Result<(), String> {
    // Only a successful confirm result enters this branch. Take the existing
    // Workspace -> membership -> Agent -> Session -> job order before the
    // ordinary append locks; never add upstream locks after Session/job.
    let workspace = tx
        .query_opt(
            "SELECT id FROM app_core_workspace WHERE id=$1 AND status='active' FOR UPDATE",
            &[&start.authorization.workspace_id],
        )
        .map_err(|e| e.to_string())?;
    if workspace.is_none() {
        return Err("work_confirmation_authority_rejected".into());
    }
    let member = tx.query_opt("SELECT m.id FROM app_core_workspacemembership m JOIN app_core_agentrun r ON r.membership_ref=m.id WHERE r.id=$1 AND m.workspace_id=r.workspace_id AND m.user_id=r.user_id AND m.role IN('owner','admin','member') FOR UPDATE OF m", &[&start.agent_run_id]).map_err(|e| e.to_string())?;
    if member.is_none() {
        return Err("work_confirmation_authority_rejected".into());
    }
    let agent = tx.query_opt("SELECT id FROM app_core_agent WHERE id=$1 AND workspace_id=$2 AND owner_id::text=$3 AND status='active' AND definition_id IS NULL FOR UPDATE", &[&start.authorization.agent_id, &start.authorization.workspace_id, &start.authorization.user_id]).map_err(|e| e.to_string())?;
    if agent.is_none() {
        return Err("work_confirmation_authority_rejected".into());
    }
    let user = tx
        .query_opt(
            "SELECT id FROM auth_user WHERE id::text=$1 AND is_active FOR SHARE",
            &[&start.authorization.user_id],
        )
        .map_err(|e| e.to_string())?;
    if user.is_none() {
        return Err("work_confirmation_authority_rejected".into());
    }
    let mut children = BTreeSet::new();
    for item in events
        .iter()
        .filter(|item| is_successful_work_confirmation(&item.event))
    {
        let receipt = confirmation_receipt(&item.event)?;
        let child = tx.query_opt("SELECT c.session_id FROM app_core_agentworkconsumeattempt a JOIN app_core_agentworkreturn n ON n.id=a.notice_id JOIN app_core_agentrun c ON c.id=n.child_run_id WHERE a.id=$1 AND a.notice_id=$2 AND a.coordinator_run_id=$3", &[&receipt.attempt_id, &receipt.notice_id, &start.agent_run_id]).map_err(|e| e.to_string())?.ok_or("work_confirmation_owner_rejected")?;
        children.insert(child.get::<_, String>(0));
    }
    for child in children {
        if tx.query_opt("SELECT id FROM app_core_session WHERE id=$1 AND status='active' AND \"purgedAt\" IS NULL FOR SHARE", &[&child]).map_err(|e| e.to_string())?.is_none() {
            return Err("work_confirmation_child_read_rejected".into());
        }
    }
    Ok(())
}

fn validate_work_confirmation_commit(
    tx: &mut impl GenericClient,
    start: &crate::contract::AgentRunStart,
    events: &[SequencedSessionRecord],
    index: usize,
) -> Result<(), String> {
    let event = &events[index].event;
    let call_id = event.payload["callId"]
        .as_str()
        .filter(|id| !id.trim().is_empty())
        .ok_or("work_confirmation_call_missing")?;
    let turn = event
        .turn_id
        .as_deref()
        .ok_or("work_confirmation_turn_missing")?;
    let staged_call = events[..index].iter().rev().find(|item| {
        item.event.event_type == SessionRecordType::ToolCall
            && item.event.turn_id.as_deref() == Some(turn)
            && item.event.payload["callId"].as_str() == Some(call_id)
    });
    let call = if let Some(item) = staged_call {
        item.event.payload.clone()
    } else {
        let row = tx.query_opt("SELECT (payload->'payload')::text FROM app_core_sessionevent WHERE session_id=$1 AND agent_run_id=$2 AND NOT session_level AND projects_to_agent_run_stream AND payload->>'type'='tool_call' AND payload->>'turnId'=$3 AND payload->'payload'->>'callId'=$4 ORDER BY sequence DESC LIMIT 1", &[&start.authorization.session_id, &start.agent_run_id, &turn, &call_id]).map_err(|e| e.to_string())?.ok_or("work_confirmation_call_missing")?;
        serde_json::from_str(&row.get::<_, String>(0)).map_err(|e| e.to_string())?
    };
    let args: crate::agent_work::WorkConfirmationArgs =
        serde_json::from_value(call["normalizedInput"].clone())
            .map_err(|_| "work_confirmation_input_invalid")?;
    args.validate()?;
    let contract = centaeris_core::tool::DynamicToolRegistry::from_contracts(vec![
        crate::agent_work::work_confirmation_tool_contract(),
    ])?
    .find_contract("confirm_work_return")
    .ok_or("work_confirmation_contract_missing")?;
    if call["toolName"] != "confirm_work_return"
        || call["providerId"] != crate::agent_work::PROVIDER_ID
        || call["toolContractDigest"].as_str() != Some(contract.contract_digest()?.as_str())
    {
        return Err("work_confirmation_call_identity_rejected".into());
    }
    let row = tx.query_opt("SELECT a.input_binding::text,n.payload::text,n.fact_kind,n.fact_ref,jsonb_build_object('agentId',ag.id,'coordinationSessionId',r.session_id,'userId',r.user_id,'workspaceId',r.workspace_id,'sourceAgentRunId',s.id,'sourceTurnId',w.source_turn_id,'sourceToolCallId',w.source_call_id,'sourceEventId',w.source_event_id,'operationId',wo.\"operationId\",'workSessionId',c.session_id,'workAgentRunId',c.id)::text FROM app_core_agentworkconsumeattempt a JOIN app_core_agentworkreturn n ON n.id=a.notice_id JOIN app_core_agentworksession w ON w.session_id=n.work_id JOIN app_core_agentrun s ON s.id=w.source_run_id JOIN app_core_agentrun c ON c.id=n.child_run_id JOIN app_core_hostedoperationreceipt wo ON wo.id=w.operation_id JOIN app_core_agentrun r ON r.id=a.coordinator_run_id JOIN app_core_session coord ON coord.id=r.session_id JOIN app_core_agent ag ON ag.id=coord.agent_id WHERE a.id=$1 AND a.notice_id=$2 AND r.id=$3 AND r.status IN('queued','running') AND s.session_id=r.session_id AND c.session_id=w.session_id AND s.membership_ref=r.membership_ref AND c.membership_ref=r.membership_ref AND s.definition_version_id IS NULL AND c.definition_version_id IS NULL AND s.acting_app_id IS NULL AND c.acting_app_id IS NULL AND s.app_delegation_id IS NULL AND c.app_delegation_id IS NULL AND wo.command='submitMessage' AND wo.\"agentRunId\"=c.id AND wo.\"sessionId\"=c.session_id AND wo.\"turnId\"=c.turn_id AND wo.user_id=r.user_id AND wo.workspace_id=r.workspace_id FOR SHARE OF a,n,w,wo", &[&args.attempt_id,&args.notice_id,&start.agent_run_id]).map_err(|e| e.to_string())?.ok_or("work_confirmation_owner_rejected")?;
    let binding: serde_json::Value =
        serde_json::from_str(&row.get::<_, String>(0)).map_err(|e| e.to_string())?;
    let notice: serde_json::Value =
        serde_json::from_str(&row.get::<_, String>(1)).map_err(|e| e.to_string())?;
    let input = serde_json::from_value(binding["initialInput"]["input"].clone())
        .map_err(|e| e.to_string())?;
    validate_hosted_native_input(tx, start, &input)?;
    let mut identity: serde_json::Value =
        serde_json::from_str(&row.get::<_, String>(4)).map_err(|e| e.to_string())?;
    identity["factKind"] = serde_json::json!(row.get::<_, String>(2));
    identity["factRef"] = serde_json::json!(row.get::<_, String>(3));
    let mut hash = Sha256::new();
    hash.update(b"workspace.agent_work.return_fact.v1\0");
    hash.update(serde_json::to_vec(&notice["terminal"]).map_err(|e| e.to_string())?);
    identity["factDigest"] = serde_json::json!(format!("sha256:{:x}", hash.finalize()));
    if notice["identity"] != identity
        || notice["terminal"]["kind"] != identity["factKind"]
        || notice["terminal"]["factRef"] != identity["factRef"]
    {
        return Err("work_confirmation_source_identity_rejected".into());
    }
    let receipt = confirmation_receipt(event)?;
    if receipt.schema != "workspace.agent_work.return_confirm.validated.v1"
        || receipt.agent_id != start.authorization.agent_id
        || receipt.session_id != start.authorization.session_id
        || receipt.agent_run_id != start.agent_run_id
        || receipt.tool_call_id != call_id
        || receipt.authorization_digest != start.authorization_digest
        || receipt.input_digest != args.digest()?
        || receipt.notice_id != args.notice_id
        || receipt.attempt_id != args.attempt_id
        || receipt.notice_identity != identity
    {
        return Err("work_confirmation_receipt_identity_rejected".into());
    }
    Ok(())
}
const MODEL_OBSERVATION_CONTENT_DIGEST_DOMAIN: &[u8] = b"centaeris.model_observation_content.v1\0";
const MODEL_OBSERVATION_MANIFEST_DIGEST_DOMAIN: &[u8] =
    b"centaeris.model_observation_manifest.v1\0";

fn load_latest_wait_model_request_id<C: GenericClient>(
    client: &mut C,
    session: &str,
    run: &str,
    turn: &str,
) -> Result<Option<String>, String> {
    client
        .query_opt(
            "SELECT payload->'payload'->>'requestId' FROM app_core_sessionevent WHERE session_id=$1 AND agent_run_id=$2 AND payload->>'turnId'=$3 AND payload->>'type'='model_request_started' AND payload->'payload'->>'purpose'='main' ORDER BY sequence DESC LIMIT 1",
            &[&session, &run, &turn],
        )
        .map_err(|error| error.to_string())
        .map(|row| row.map(|row| row.get(0)))
}

fn validate_wait_handoff_source(
    tx: &mut Transaction<'_>,
    run: &str,
    handoff: &crate::wait_recovery::WaitHandoff,
) -> Result<(), String> {
    let wait = &handoff.wait_checkpoint;
    let current = load_checkpoint(tx, &wait.session_id, &wait.turn_id)?
        .ok_or("wait handoff checkpoint missing")?;
    if current != *wait {
        return Err("wait handoff checkpoint changed".into());
    }
    let source_sequence: i32 = tx
        .query_one(
            "SELECT COALESCE(MAX(sequence),0) FROM app_core_sessionevent WHERE session_id=$1",
            &[&wait.session_id],
        )
        .map_err(|e| e.to_string())?
        .get(0);
    if u64::try_from(source_sequence).ok() != Some(handoff.source_session_sequence) {
        return Err("wait handoff source position changed".into());
    }
    let snapshot: String = tx.query_opt("SELECT snapshot_json FROM runtime.session_runtime_snapshots WHERE session_id=$1 FOR UPDATE", &[&wait.session_id]).map_err(|e|e.to_string())?.ok_or("wait handoff Core snapshot missing")?.get(0);
    if serde_json::from_str::<serde_json::Value>(&snapshot).map_err(|e| e.to_string())?
        != serde_json::from_str::<serde_json::Value>(&handoff.snapshot_json)
            .map_err(|e| e.to_string())?
    {
        return Err("wait handoff Core snapshot changed".into());
    }
    let anchor = load_latest_wait_model_request_id(tx, &wait.session_id, run, &wait.turn_id)?
        .ok_or("wait handoff model request missing")?;
    if anchor != handoff.model_request_id {
        return Err("wait handoff model request changed".into());
    }
    let open: i64 = tx.query_one("SELECT COUNT(*) FROM app_core_sessionevent c WHERE c.session_id=$1 AND c.agent_run_id=$2 AND c.payload->>'type'='tool_call' AND NOT EXISTS(SELECT 1 FROM app_core_sessionevent r WHERE r.session_id=c.session_id AND r.agent_run_id=c.agent_run_id AND r.payload->>'type' IN('tool_result','tool_call_closure') AND r.payload->'payload'->>'callId'=c.payload->'payload'->>'callId')", &[&wait.session_id,&run]).map_err(|e|e.to_string())?.get(0);
    if open != 0 {
        return Err("wait handoff requires closed tool ledger".into());
    }
    let payload: centaeris_core::runtime::contracts::RuntimeAwaitJobCheckpointV1 =
        serde_json::from_str(&wait.payload_json).map_err(|e| e.to_string())?;
    for source in payload.waits {
        let matches: bool = tx
            .query_opt(
                "SELECT job_kind=$2 AND session_id=$3 FROM runtime.runtime_jobs WHERE job_id=$1",
                &[&source.job_id, &source.job_kind, &wait.session_id],
            )
            .map_err(|e| e.to_string())?
            .is_some_and(|row| row.get::<_, Option<bool>>(0) == Some(true));
        let receipts: i64 = tx.query_one("SELECT COUNT(DISTINCT payload->>'type') FROM app_core_sessionevent WHERE session_id=$1 AND agent_run_id=$2 AND payload->'payload'->>'callId'=$3 AND payload->'payload'->>'toolName'=$4 AND payload->>'type' IN('tool_call','tool_result')", &[&wait.session_id,&run,&source.tool_call_id,&source.source_tool_name]).map_err(|e|e.to_string())?.get(0);
        if !matches || receipts != 2 {
            return Err("wait handoff source job or receipt binding mismatch".into());
        }
    }
    Ok(())
}

impl PostgresRuntimeStore {
    pub(crate) fn load_wait_handoff(
        &self,
        checkpoint_id: &str,
    ) -> Result<Option<crate::wait_recovery::WaitHandoff>, String> {
        self.with_client(|client| {
            let Some(row) = client.query_opt(
                "SELECT checkpoint_id,kind,session_id,turn_id,status,done_reason,updated_at_ms,payload_json,wait_handoff_json FROM runtime.checkpoints WHERE checkpoint_id=$1 AND kind='recovery'", &[&checkpoint_id],
            ).map_err(|e|e.to_string())? else { return Ok(None); };
            let Some(value) = row.get::<_,Option<String>>(8) else { return Ok(None); };
            let handoff:crate::wait_recovery::WaitHandoff=serde_json::from_str(&value).map_err(|e|e.to_string())?;
            let checkpoint=row_to_checkpoint(&row)?;
            let payload:centaeris_core::runtime::contracts::RuntimeRecoveryCheckpointV1=serde_json::from_str(&checkpoint.payload_json).map_err(|e|e.to_string())?;
            handoff.validate(&payload)?;
            let reference=client.query_opt(
                "SELECT sequence,payload::text FROM app_core_sessionevent WHERE session_id=$1 AND agent_run_id=$2 AND payload->>'type'='checkpoint_ref' AND payload->'payload'->>'checkpointId'=$3",
                &[&payload.session_id,&payload.agent_run_id,&checkpoint_id],
            ).map_err(|e|e.to_string())?.ok_or("wait recovery checkpoint reference missing")?;
            if u64::try_from(reference.get::<_,i32>(0)).ok()!=Some(payload.session_sequence) {
                return Err("wait recovery checkpoint reference position mismatch".into());
            }
            let wire=serde_json::from_str(&reference.get::<_,String>(1)).map_err(|e|e.to_string())?;
            let event=parse_wire_record(&wire).map_err(|e|e.to_string())?.event;
            crate::wait_recovery::WaitHandoff::validate_reference(&checkpoint,&event)?;
            Ok(Some(handoff))
        })
    }

    pub(crate) fn wait_handoff_checkpoint(
        &self,
        key: &str,
    ) -> Result<Option<CheckpointRecord>, String> {
        self.with_client(|client| optional_row(client.query_opt(
            "SELECT checkpoint_id,kind,session_id,turn_id,status,done_reason,updated_at_ms,payload_json FROM runtime.checkpoints WHERE wait_handoff_json::jsonb->>'handoffKey'=$1", &[&key]),
            row_to_checkpoint, "load wait handoff"))
    }

    pub(crate) fn latest_model_request_id(
        &self,
        session: &str,
        run: &str,
        turn: &str,
    ) -> Result<String, String> {
        self.with_client(|client| {
            load_latest_wait_model_request_id(client, session, run, turn)?
                .ok_or_else(|| "wait handoff has no real model request".into())
        })
    }
    /// Idempotent repair for terminal reads/cancellation, including failures
    /// committed before terminal waiter consumption was wired in.
    pub fn repair_terminal_runtime_job_waits(
        &self,
        session_id: &str,
        agent_run_id: &str,
    ) -> Result<(), String> {
        self.with_client(|client| {
            let mut tx = client.transaction().map_err(|e| e.to_string())?;
            tx.query_opt("SELECT id FROM app_core_session WHERE id=$1 FOR UPDATE", &[&session_id])
                .map_err(|e| e.to_string())?.ok_or("terminal wait session missing")?;
            let terminal = tx.query_opt(
                "SELECT \"createdAtMs\" FROM app_core_sessionevent WHERE session_id=$1 AND agent_run_id=$2 AND payload->>'type' IN ('agent_run_completed','agent_run_failed','agent_run_interrupted') ORDER BY sequence DESC LIMIT 1",
                &[&session_id, &agent_run_id]).map_err(|e| e.to_string())?.ok_or("terminal wait cleanup requires durable terminal")?;
            consume_terminal_runtime_job_waits(&mut tx, session_id, agent_run_id, terminal.get(0))?;
            tx.commit().map_err(|e| e.to_string())
        })
    }
    pub fn load_recovery_checkpoint_by_id(
        &self,
        checkpoint_id: &str,
    ) -> Result<Option<CheckpointRecord>, String> {
        self.with_client(|client| optional_row(
            client.query_opt(
                "SELECT checkpoint_id,kind,session_id,turn_id,status,done_reason,updated_at_ms,payload_json FROM checkpoints WHERE checkpoint_id=$1",
                &[&checkpoint_id],
            ),
            row_to_checkpoint,
            "load referenced recovery checkpoint",
        ))
    }

    pub fn request_agent_run_cancellation(
        &self,
        agent_run_id: &str,
        session_id: &str,
        authorization_digest: &str,
        requested_at_ms: i64,
    ) -> Result<bool, String> {
        let job_id =
            centaeris_core::session::reliability::agent_run_lifecycle_job_id(agent_run_id)?;
        self.with_execution_control_client(|client| {
            let mut tx = client
                .transaction()
                .map_err(|error| format!("begin AgentRun cancellation request failed: {error}"))?;
            let row = tx
                .query_opt(
                    "SELECT job_kind,status,session_id,payload_ref,idempotency_key FROM runtime_jobs WHERE job_id=$1 FOR UPDATE",
                    &[&job_id],
                )
                .map_err(|error| format!("load AgentRun cancellation job failed: {error}"))?
                .ok_or_else(|| "AgentRun cancellation job not found".to_string())?;
            let status = row.get::<_, String>(1);
            if row.get::<_, String>(0) != AGENT_RUN_LIFECYCLE_JOB_KIND
                || row.get::<_, Option<String>>(2).as_deref() != Some(session_id)
                || row.get::<_, Option<String>>(3).as_deref()
                    != Some(format!("record:agent_run:{agent_run_id}").as_str())
                || row.get::<_, String>(4)
                    != format!("agent_run.lifecycle:{agent_run_id}:{authorization_digest}")
            {
                return Err("AgentRun cancellation job binding mismatch".to_string());
            }
            if !matches!(status.as_str(), "queued" | "leased" | "running") {
                return Err(format!("AgentRun cancellation job is terminal: {status}"));
            }
            let event_id = format!("agent_run_cancel_requested:{agent_run_id}");
            let payload = serde_json::json!({
                "schema": AGENT_RUN_CANCEL_REQUESTED_EVENT_SCHEMA,
                "agentRunId": agent_run_id,
                "authorizationDigest": authorization_digest,
                "transitionReason": "agent_run_cancel_requested",
            })
            .to_string();
            let inserted = tx
                .execute(
                    "INSERT INTO runtime_events(event_id,session_id,task_id,event_type,at_ms,visibility,payload_json) VALUES($1,$2,$3,$4,$5,'internal',$6) ON CONFLICT(event_id) DO NOTHING",
                    &[&event_id, &session_id, &agent_run_id, &AGENT_RUN_CANCEL_REQUESTED_EVENT_SCHEMA, &requested_at_ms, &payload],
                )
                .map_err(|error| format!("append AgentRun cancellation request failed: {error}"))?
                == 1;
            let stored = tx
                .query_one(
                    "SELECT session_id,task_id,event_type,payload_json FROM runtime_events WHERE event_id=$1",
                    &[&event_id],
                )
                .map_err(|error| format!("load AgentRun cancellation request failed: {error}"))?;
            if stored.get::<_, String>(0) != session_id
                || stored.get::<_, Option<String>>(1).as_deref() != Some(agent_run_id)
                || stored.get::<_, String>(2) != AGENT_RUN_CANCEL_REQUESTED_EVENT_SCHEMA
                || stored.get::<_, String>(3) != payload
            {
                return Err("AgentRun cancellation request idempotency conflict".to_string());
            }
            if status == "queued" {
                tx.execute(
                    "UPDATE runtime_jobs SET run_at_ms=LEAST(run_at_ms,$1),updated_at_ms=$1 WHERE job_id=$2 AND status='queued'",
                    &[&requested_at_ms, &job_id],
                )
                .map_err(|error| format!("wake cancelled AgentRun lifecycle failed: {error}"))?;
            }
            tx.execute(
                "UPDATE runtime_turn_supplement_queues SET revision=revision+1,accepting=0,entries_json='[]',closed_reason='agent_run_cancel_requested',updated_at_ms=$1 WHERE agent_run_id=$2 AND accepting=1",
                &[&requested_at_ms, &agent_run_id],
            )
            .map_err(|error| format!("close cancelled AgentRun supplement queue failed: {error}"))?;
            tx.commit()
                .map_err(|error| format!("commit AgentRun cancellation request failed: {error}"))?;
            Ok(inserted)
        })
    }

    pub fn agent_run_execution_control_state(
        &self,
        agent_run_id: &str,
        lifecycle_job_id: &str,
        lifecycle_lease_owner: &str,
        now_ms: i64,
    ) -> Result<AgentRunExecutionControlState, String> {
        let event_id = format!("agent_run_cancel_requested:{agent_run_id}");
        self.with_execution_control_client(|client| {
            let row = client
                .query_one(
                    "SELECT EXISTS(SELECT 1 FROM runtime_events WHERE event_id=$1 AND event_type=$2),EXISTS(SELECT 1 FROM runtime_jobs WHERE job_id=$3 AND status='running' AND lease_owner=$4 AND lease_expires_at_ms>$5)",
                    &[&event_id, &AGENT_RUN_CANCEL_REQUESTED_EVENT_SCHEMA, &lifecycle_job_id, &lifecycle_lease_owner, &now_ms],
                )
                .map_err(|error| format!("query AgentRun execution control state failed: {error}"))?;
            Ok(AgentRunExecutionControlState {
                cancellation_requested: row.get(0),
                lifecycle_lease_current: row.get(1),
            })
        })
    }
}

impl RuntimeStore for PostgresRuntimeStore {
    fn list_runtime_job_waiters(
        &self,
        source_job_id: Option<&str>,
        after: Option<&centaeris_core::session::store::RuntimeJobWaiterCursor>,
        limit: usize,
    ) -> Result<Vec<centaeris_core::session::store::RuntimeJobWaiter>, RuntimeStoreError> {
        use centaeris_core::session::store::{RuntimeJobWaiter, RuntimeJobWaiterCursor};
        self.with_client(|client| {
            let checkpoint = after.map_or("", |cursor| cursor.checkpoint_id.as_str());
            let call = after.map_or("", |cursor| cursor.tool_call_id.as_str());
            let limit = to_i64(limit)?;
            let base = "SELECT checkpoint_id,tool_call_id,source_job_id,source_job_kind,session_id,agent_run_id FROM runtime_job_waiters WHERE (checkpoint_id,tool_call_id)>($1,$2)";
            let rows = match source_job_id {
                Some(source) => client.query(&format!("{base} AND source_job_id=$4 ORDER BY checkpoint_id,tool_call_id LIMIT $3"), &[&checkpoint, &call, &limit, &source]),
                None => client.query(&format!("{base} ORDER BY checkpoint_id,tool_call_id LIMIT $3"), &[&checkpoint, &call, &limit]),
            }.map_err(|error| format!("list runtime job waiters failed: {error}"))?;
            Ok(rows.iter().map(|row| RuntimeJobWaiter {
                cursor: RuntimeJobWaiterCursor { checkpoint_id: row.get(0), tool_call_id: row.get(1) },
                source_job_id: row.get(2), source_job_kind: row.get(3), session_id: row.get(4), agent_run_id: row.get(5),
            }).collect())
        }).map_err(RuntimeStoreError::backend)
    }

    fn save_checkpoint(&self, checkpoint: CheckpointRecord) -> Result<(), RuntimeStoreError> {
        self.with_client(|client| {
            let mut tx = client
                .transaction()
                .map_err(|error| format!("begin checkpoint save failed: {error}"))?;
            save_checkpoint(&mut tx, &checkpoint)?;
            tx.commit()
                .map_err(|error| format!("commit checkpoint save failed: {error}"))
        })
        .map_err(RuntimeStoreError::backend)
    }

    fn load_latest_checkpoint(
        &self,
        session_id: &str,
    ) -> Result<Option<CheckpointRecord>, RuntimeStoreError> {
        self.with_client(|client| optional_row(client.query_opt("SELECT checkpoint_id,kind,session_id,turn_id,status,done_reason,updated_at_ms,payload_json FROM checkpoints WHERE session_id=$1 AND kind<>'recovery' ORDER BY updated_at_ms DESC,checkpoint_id DESC LIMIT 1", &[&session_id]), row_to_checkpoint, "load latest Postgres checkpoint"))
            .map_err(RuntimeStoreError::backend)
    }

    fn load_checkpoint_by_turn(
        &self,
        session_id: &str,
        turn_id: &str,
    ) -> Result<Option<CheckpointRecord>, RuntimeStoreError> {
        self.with_client(|client| optional_row(client.query_opt("SELECT checkpoint_id,kind,session_id,turn_id,status,done_reason,updated_at_ms,payload_json FROM checkpoints WHERE session_id=$1 AND turn_id=$2 AND kind<>'recovery'", &[&session_id,&turn_id]), row_to_checkpoint, "load Postgres checkpoint"))
            .map_err(RuntimeStoreError::backend)
    }

    fn list_checkpoints(
        &self,
        session_id: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<CheckpointRecord>, RuntimeStoreError> {
        self.with_client(|client| rows(client.query("SELECT checkpoint_id,kind,session_id,turn_id,status,done_reason,updated_at_ms,payload_json FROM checkpoints WHERE session_id=$1 ORDER BY updated_at_ms DESC,checkpoint_id DESC LIMIT $2 OFFSET $3", &[&session_id,&to_i64(limit)?,&to_i64(offset)?]), row_to_checkpoint, "list Postgres checkpoints"))
            .map_err(RuntimeStoreError::backend)
    }

    fn list_waiting_runtime_job_checkpoints(
        &self,
        after: Option<&centaeris_core::session::store::RuntimeJobWaitCheckpointCursor>,
        limit: usize,
    ) -> Result<Vec<CheckpointRecord>, RuntimeStoreError> {
        self.with_client(|client| rows(
            client.query(
                "SELECT checkpoint_id,kind,session_id,turn_id,status,done_reason,updated_at_ms,payload_json FROM checkpoints WHERE kind='wait' AND status='waiting' AND done_reason='runtime_job' AND ($1::text IS NULL OR (session_id,turn_id)>($1::text,$2::text)) ORDER BY session_id,turn_id LIMIT $3",
                &[
                    &after.map(|cursor| cursor.session_id.as_str()),
                    &after.map(|cursor| cursor.turn_id.as_str()),
                    &to_i64(limit)?,
                ],
            ),
            row_to_checkpoint,
            "list waiting Postgres runtime job checkpoints",
        ))
        .map_err(RuntimeStoreError::backend)
    }

    fn append_event(&self, event: RuntimeEvent) -> Result<(), RuntimeStoreError> {
        self.with_client(|client| append_runtime_event(client, &event))
            .map_err(RuntimeStoreError::backend)
    }

    fn append_event_idempotent(&self, event: RuntimeEvent) -> Result<(), RuntimeStoreError> {
        self.with_client(|client| append_runtime_event_idempotent(client, &event).map(|_| ()))
            .map_err(RuntimeStoreError::backend)
    }

    fn list_events(
        &self,
        session_id: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<RuntimeEvent>, RuntimeStoreError> {
        self.with_client(|client| rows(client.query("SELECT event_id,session_id,task_id,event_type,at_ms,visibility,payload_json FROM runtime_events WHERE session_id=$1 ORDER BY at_ms ASC,event_id ASC LIMIT $2 OFFSET $3", &[&session_id,&to_i64(limit)?,&to_i64(offset)?]), row_to_event, "list Postgres runtime events"))
            .map_err(RuntimeStoreError::backend)
    }
}

impl SessionDataStorePort for PostgresRuntimeStore {
    fn delete_session_data(&self, session_id: &str) -> Result<(), String> {
        let session_id = session_id.trim();
        if session_id.is_empty() {
            return Err("sessionId is required".to_string());
        }
        self.with_client(|client| {
            let mut tx = client
                .transaction()
                .map_err(|error| format!("begin Postgres delete session data failed: {error}"))?;
            let object_ids = tx
                .query(
                    "SELECT DISTINCT object_id FROM external_context_links WHERE session_id=$1",
                    &[&session_id],
                )
                .map_err(|error| format!("query Postgres session external object ids failed: {error}"))?
                .into_iter()
                .map(|row| row.get::<_, String>(0))
                .collect::<Vec<_>>();
            for table in [
                "transcript_projection_heads",
                "runtime_events",
                "session_runtime_snapshots",
                "checkpoints",
                "dead_letters",
                "resource_claims",
                "runtime_jobs",
                "external_context_links",
                "model_observation_contents",
                "model_observation_manifests",
            ] {
                tx.execute(
                    format!("DELETE FROM {table} WHERE session_id=$1").as_str(),
                    &[&session_id],
                )
                .map_err(|error| {
                    format!("delete Postgres session data from {table} failed: {error}")
                })?;
            }
            for object_id in object_ids {
                tx.execute(
                    "DELETE FROM external_context_objects WHERE object_id=$1 AND NOT EXISTS (SELECT 1 FROM external_context_links WHERE object_id=$1)",
                    &[&object_id],
                )
                .map_err(|error| {
                    format!("delete Postgres orphan external context object failed: {error}")
                })?;
            }
            tx.commit()
                .map_err(|error| format!("commit Postgres delete session data failed: {error}"))
        })
    }
}

impl AgentRuntimeSnapshotStorePort for PostgresRuntimeStore {
    fn load_agent_runtime_snapshot(&self, session_id: &str) -> Result<Option<String>, String> {
        self.with_client(|client| {
            client
                .query_opt(
                    "SELECT snapshot_json FROM session_runtime_snapshots WHERE session_id=$1",
                    &[&session_id],
                )
                .map(|row| row.map(|item| item.get(0)))
                .map_err(|error| format!("load Postgres session snapshot failed: {error}"))
        })
    }

    fn save_agent_runtime_snapshot(
        &self,
        session_id: &str,
        snapshot: &str,
        updated_at_ms: i64,
    ) -> Result<(), String> {
        self.with_client(|client| client.execute("INSERT INTO session_runtime_snapshots(session_id,snapshot_json,updated_at_ms) VALUES($1,$2,$3) ON CONFLICT(session_id) DO UPDATE SET snapshot_json=excluded.snapshot_json,updated_at_ms=excluded.updated_at_ms", &[&session_id,&snapshot,&updated_at_ms]).map(|_| ()).map_err(|error| format!("save Postgres session snapshot failed: {error}")))
    }
}

pub(super) fn save_checkpoint<C: postgres::GenericClient>(
    client: &mut C,
    item: &CheckpointRecord,
) -> Result<(), String> {
    let waiters = centaeris_core::session::store::runtime_job_waiters(item)?;
    if item.kind == CheckpointKindV1::Recovery {
        let inserted = client.execute(
            "INSERT INTO runtime.checkpoints(checkpoint_id,kind,session_id,turn_id,status,done_reason,updated_at_ms,payload_json) VALUES($1,$2,$3,$4,$5,$6,$7,$8) ON CONFLICT(checkpoint_id) DO NOTHING",
            &[&item.checkpoint_id,&item.kind.as_str(),&item.session_id,&item.turn_id,&item.status,&item.done_reason,&item.updated_at_ms,&item.payload_json],
        ).map_err(|error| format!("save Postgres recovery checkpoint failed: {error}"))?;
        if inserted == 0 {
            let existing = client
                .query_one(
                    "SELECT checkpoint_id,kind,session_id,turn_id,status,done_reason,updated_at_ms,payload_json FROM runtime.checkpoints WHERE checkpoint_id=$1",
                    &[&item.checkpoint_id],
                )
                .map_err(|error| format!("load Postgres recovery checkpoint failed: {error}"))?;
            if row_to_checkpoint(&existing)? != *item {
                return Err(format!(
                    "recovery_checkpoint_idempotency_conflict: checkpointId={}",
                    item.checkpoint_id
                ));
            }
        }
        return Ok(());
    }
    client.execute("INSERT INTO checkpoints(checkpoint_id,kind,session_id,turn_id,status,done_reason,updated_at_ms,payload_json) VALUES($1,$2,$3,$4,$5,$6,$7,$8) ON CONFLICT(checkpoint_id) DO UPDATE SET kind=excluded.kind,session_id=excluded.session_id,turn_id=excluded.turn_id,status=excluded.status,done_reason=excluded.done_reason,updated_at_ms=excluded.updated_at_ms,payload_json=excluded.payload_json", &[&item.checkpoint_id,&item.kind.as_str(),&item.session_id,&item.turn_id,&item.status,&item.done_reason,&item.updated_at_ms,&item.payload_json]).map_err(|error| format!("save Postgres checkpoint failed: {error}"))?;
    client
        .execute(
            "DELETE FROM runtime_job_waiters WHERE checkpoint_id=$1",
            &[&item.checkpoint_id],
        )
        .map_err(|error| format!("replace checkpoint waiters failed: {error}"))?;
    for waiter in waiters {
        client.execute("INSERT INTO runtime_job_waiters(checkpoint_id,tool_call_id,source_job_id,source_job_kind,session_id,agent_run_id) VALUES($1,$2,$3,$4,$5,$6)",
            &[&waiter.cursor.checkpoint_id, &waiter.cursor.tool_call_id, &waiter.source_job_id, &waiter.source_job_kind, &waiter.session_id, &waiter.agent_run_id])
            .map_err(|error| format!("save checkpoint waiter failed: {error}"))?;
    }
    Ok(())
}

pub(super) fn append_runtime_event<C: postgres::GenericClient>(
    client: &mut C,
    item: &RuntimeEvent,
) -> Result<(), String> {
    client.execute("INSERT INTO runtime_events(event_id,session_id,task_id,event_type,at_ms,visibility,payload_json) VALUES($1,$2,$3,$4,$5,$6,$7)", &[&item.event_id,&item.session_id,&item.task_id,&item.event_type,&item.at_ms,&visibility_to_db(&item.visibility),&item.payload_json]).map(|_| ()).map_err(|error| format!("append Postgres runtime event failed: {error}"))
}

pub(super) fn append_runtime_event_idempotent<C: postgres::GenericClient>(
    client: &mut C,
    item: &RuntimeEvent,
) -> Result<bool, String> {
    let inserted = client.execute(
        "INSERT INTO runtime_events(event_id,session_id,task_id,event_type,at_ms,visibility,payload_json) VALUES($1,$2,$3,$4,$5,$6,$7) ON CONFLICT(event_id) DO NOTHING",
        &[&item.event_id,&item.session_id,&item.task_id,&item.event_type,&item.at_ms,&visibility_to_db(&item.visibility),&item.payload_json],
    ).map_err(|error| format!("append idempotent Postgres runtime event failed: {error}"))?;
    if inserted == 1 {
        return Ok(true);
    }
    let existing = client
        .query_opt(
            "SELECT event_id,session_id,task_id,event_type,at_ms,visibility,payload_json FROM runtime_events WHERE event_id=$1",
            &[&item.event_id],
        )
        .map_err(|error| format!("load idempotent Postgres runtime event failed: {error}"))?
        .map(|row| row_to_event(&row))
        .transpose()?
        .ok_or_else(|| format!("idempotent Postgres runtime event disappeared: {}", item.event_id))?;
    if existing != *item {
        return Err(format!(
            "runtime_event_idempotency_conflict: eventId={}",
            item.event_id
        ));
    }
    Ok(false)
}

pub(super) fn load_checkpoint<C: postgres::GenericClient>(
    client: &mut C,
    session_id: &str,
    turn_id: &str,
) -> Result<Option<CheckpointRecord>, String> {
    optional_row(
        client.query_opt(
            "SELECT checkpoint_id,kind,session_id,turn_id,status,done_reason,updated_at_ms,payload_json FROM checkpoints WHERE session_id=$1 AND turn_id=$2 AND kind<>'recovery'",
            &[&session_id, &turn_id],
        ),
        row_to_checkpoint,
        "load Postgres checkpoint",
    )
}

fn optional_row<T>(
    result: Result<Option<Row>, postgres::Error>,
    decode: fn(&Row) -> Result<T, String>,
    label: &str,
) -> Result<Option<T>, String> {
    result
        .map_err(|error| format!("{label} failed: {error}"))?
        .map(|row| decode(&row))
        .transpose()
}

fn rows<T>(
    result: Result<Vec<Row>, postgres::Error>,
    decode: fn(&Row) -> Result<T, String>,
    label: &str,
) -> Result<Vec<T>, String> {
    result
        .map_err(|error| format!("{label} failed: {error}"))?
        .iter()
        .map(decode)
        .collect()
}

pub(super) fn to_i64(value: usize) -> Result<i64, String> {
    i64::try_from(value).map_err(|_| "pagination value exceeds i64".to_string())
}

fn row_to_checkpoint(row: &Row) -> Result<CheckpointRecord, String> {
    Ok(CheckpointRecord {
        checkpoint_id: row.get(0),
        kind: CheckpointKindV1::parse(row.get::<_, String>(1).as_str())?,
        session_id: row.get(2),
        turn_id: row.get(3),
        status: row.get(4),
        done_reason: row.get(5),
        updated_at_ms: row.get(6),
        payload_json: row.get(7),
    })
}
pub(super) fn row_to_event(row: &Row) -> Result<RuntimeEvent, String> {
    Ok(RuntimeEvent {
        event_id: row.get(0),
        session_id: row.get(1),
        task_id: row.get(2),
        event_type: row.get(3),
        at_ms: row.get(4),
        visibility: visibility_from_db(row.get::<_, String>(5).as_str())?,
        payload_json: row.get(6),
    })
}

fn visibility_to_db(value: &EventVisibility) -> &'static str {
    match value {
        EventVisibility::User => "user",
        EventVisibility::Internal => "internal",
    }
}
fn visibility_from_db(value: &str) -> Result<EventVisibility, String> {
    match value {
        "user" => Ok(EventVisibility::User),
        "internal" => Ok(EventVisibility::Internal),
        _ => Err(format!("invalid event visibility: {value}")),
    }
}

#[derive(Clone)]
pub struct PostgresSessionLog {
    connections: Arc<PostgresConnectionPool>,
    workspace_id: String,
    session_id: String,
    prompt: String,
    accepted_start: Option<crate::contract::AgentRunStart>,
    projection_store: Option<PostgresRuntimeStore>,
    agent_run_state: Arc<Mutex<HashMap<String, AgentRunAppendState>>>,
    /// Clones share incremental state and serialize append validation for this
    /// AgentRun, while each transaction holds a pooled connection only until commit.
    append_lock: Arc<Mutex<()>>,
}

impl std::fmt::Debug for PostgresSessionLog {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PostgresSessionLog")
            .field("database_url", &"[REDACTED]")
            .field("workspace_id", &self.workspace_id)
            .field("session_id", &self.session_id)
            .finish()
    }
}

/// AgentRun 级增量 append 状态：避免每次 append 全量重放整个 AgentRun 日志。
/// 缓存只存在于本进程；进程重启后 miss 自动全量重载，不损失正确性。
#[derive(Clone, Debug)]
struct AgentRunAppendState {
    last_sequence: u64,
    projection: SessionProjection,
    seen_event_ids: HashSet<String>,
    open_tool_call_ids: HashMap<String, (String, String, String)>,
    agent_run_started_count: usize,
    user_message_count: usize,
    initial_host_input_id: Option<String>,
    assistant_message_count: usize,
    last_assistant_status: Option<String>,
    terminal_seen: bool,
    first_event_type: Option<SessionRecordType>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ModelObservationContent {
    digest: String,
    kind: String,
    content_json: String,
    content_bytes: i64,
    first_seen_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ModelObservationReference {
    kind: String,
    content_digest: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ModelObservationChange {
    index: usize,
    reference: ModelObservationReference,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ModelObservationManifest {
    digest: String,
    parent_digest: Option<String>,
    observation_count: usize,
    changes: Vec<ModelObservationChange>,
    manifest_json: String,
    manifest_bytes: i64,
    first_seen_at_ms: i64,
}

#[derive(Clone, Debug)]
struct PreparedSessionRow {
    record: CommittedSessionRecord,
    agent_run_id: String,
    agent_run_sequence: Option<i32>,
    session_level: bool,
    projects_to_agent_run_stream: bool,
    payload: String,
    commit_payload: String,
}

#[derive(Clone, Debug)]
struct PreparedSessionAppend {
    rows: Vec<PreparedSessionRow>,
    contents: Vec<ModelObservationContent>,
    manifests: Vec<ModelObservationManifest>,
}

/// One admission-time event. Session-level closures keep the originating run id
/// but carry no run sequence; new-run first-batch records keep their run
/// sequence. `agent_run_id` is per row, not per batch.
#[derive(Clone, Debug)]
struct AdmissionEntry {
    event: centaeris_core::session::SessionLogRecord,
    agent_run_id: String,
    agent_run_sequence: Option<i32>,
    session_level: bool,
}

fn sha256_json(domain: &[u8], json: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(domain);
    digest.update(json.as_bytes());
    format!("sha256:{:x}", digest.finalize())
}

fn compact_model_observation(
    observation: &serde_json::Value,
    first_seen_at_ms: i64,
) -> Result<(ModelObservationContent, ModelObservationReference), String> {
    let kind = observation
        .get("kind")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "model observation storage kind is required".to_string())?
        .to_string();
    if !observation.is_object() {
        return Err("model observation storage payload is invalid".to_string());
    }
    let content_json = serde_json::to_string(observation)
        .map_err(|error| format!("encode model observation content failed: {error}"))?;
    let content_digest = sha256_json(MODEL_OBSERVATION_CONTENT_DIGEST_DOMAIN, &content_json);
    let content_bytes = i64::try_from(content_json.len())
        .map_err(|_| "model observation content size overflow".to_string())?;
    Ok((
        ModelObservationContent {
            digest: content_digest.clone(),
            kind: kind.clone(),
            content_json,
            content_bytes,
            first_seen_at_ms,
        },
        ModelObservationReference {
            kind,
            content_digest,
        },
    ))
}

fn compact_model_request_wire(
    wire: &mut serde_json::Value,
    first_seen_at_ms: i64,
    parent_digest: Option<String>,
    parent_references: &[ModelObservationReference],
) -> Result<
    (
        Vec<ModelObservationContent>,
        ModelObservationManifest,
        Vec<ModelObservationReference>,
    ),
    String,
> {
    let observations = wire
        .pointer("/payload/observations")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .ok_or_else(|| "model request storage observations are required".to_string())?;
    let mut contents = Vec::with_capacity(observations.len());
    let mut known_content_digests = parent_references
        .iter()
        .map(|reference| reference.content_digest.clone())
        .collect::<HashSet<_>>();
    let references = observations
        .iter()
        .map(|observation| {
            let (content, reference) = compact_model_observation(observation, first_seen_at_ms)?;
            if known_content_digests.insert(content.digest.clone()) {
                contents.push(content);
            }
            Ok(reference)
        })
        .collect::<Result<Vec<_>, String>>()?;
    let manifest = build_model_observation_manifest(
        parent_digest,
        parent_references,
        references.as_slice(),
        first_seen_at_ms,
    )?;
    let payload = wire
        .get_mut("payload")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| "model request storage payload is invalid".to_string())?;
    payload.insert(
        "observations".to_string(),
        serde_json::json!({"manifestDigest": manifest.digest}),
    );
    Ok((contents, manifest, references))
}

fn build_model_observation_manifest(
    parent_digest: Option<String>,
    parent: &[ModelObservationReference],
    references: &[ModelObservationReference],
    first_seen_at_ms: i64,
) -> Result<ModelObservationManifest, String> {
    let changes = references
        .iter()
        .enumerate()
        .filter(|(index, reference)| parent.get(*index) != Some(*reference))
        .map(|(index, reference)| ModelObservationChange {
            index,
            reference: reference.clone(),
        })
        .collect::<Vec<_>>();
    let manifest_json = serde_json::to_string(&serde_json::json!({
        "parentDigest": parent_digest,
        "observationCount": references.len(),
        "changes": changes.iter().map(|change| serde_json::json!({
            "index": change.index,
            "kind": change.reference.kind,
            "contentDigest": change.reference.content_digest,
        })).collect::<Vec<_>>(),
    }))
    .map_err(|error| format!("encode model observation manifest failed: {error}"))?;
    Ok(ModelObservationManifest {
        digest: sha256_json(
            MODEL_OBSERVATION_MANIFEST_DIGEST_DOMAIN,
            manifest_json.as_str(),
        ),
        parent_digest,
        observation_count: references.len(),
        changes,
        manifest_bytes: i64::try_from(manifest_json.len())
            .map_err(|_| "model observation manifest size overflow".to_string())?,
        manifest_json,
        first_seen_at_ms,
    })
}

fn model_request_manifest_key(
    wire: &serde_json::Value,
) -> Result<Option<(String, String)>, String> {
    if wire.get("type").and_then(serde_json::Value::as_str) != Some("model_request_started") {
        return Ok(None);
    }
    let session_id = wire
        .get("sessionId")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "stored model request sessionId is required".to_string())?;
    let manifest = wire
        .pointer("/payload/observations")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| "stored model request observation manifest is required".to_string())?;
    if manifest.keys().map(String::as_str).collect::<BTreeSet<_>>()
        != BTreeSet::from(["manifestDigest"])
    {
        return Err("stored model request observation manifest fields mismatch".to_string());
    }
    let digest = manifest
        .get("manifestDigest")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "stored model request observation manifestDigest is required".to_string())?;
    validate_model_observation_digest(digest)?;
    Ok(Some((session_id.to_string(), digest.to_string())))
}

fn validate_model_observation_digest(digest: &str) -> Result<(), String> {
    if digest.strip_prefix("sha256:").is_none_or(|hex| {
        hex.len() != 64
            || !hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    }) {
        return Err("model observation storage digest is invalid".to_string());
    }
    Ok(())
}

fn decode_model_observation_manifest(
    digest: String,
    parent_column: Option<String>,
    manifest_json: String,
    manifest_bytes: i64,
    first_seen_at_ms: i64,
) -> Result<ModelObservationManifest, String> {
    validate_model_observation_digest(digest.as_str())?;
    if manifest_bytes != i64::try_from(manifest_json.len()).unwrap_or(-1)
        || sha256_json(
            MODEL_OBSERVATION_MANIFEST_DIGEST_DOMAIN,
            manifest_json.as_str(),
        ) != digest
    {
        return Err("stored model observation manifest digest mismatch".to_string());
    }
    let value = serde_json::from_str::<serde_json::Value>(manifest_json.as_str())
        .map_err(|error| format!("decode stored model observation manifest failed: {error}"))?;
    if serde_json::to_string(&value)
        .map_err(|error| format!("encode stored model observation manifest failed: {error}"))?
        != manifest_json
    {
        return Err("stored model observation manifest is not canonical JSON".to_string());
    }
    let object = value
        .as_object()
        .ok_or_else(|| "stored model observation manifest is invalid".to_string())?;
    if object.keys().map(String::as_str).collect::<BTreeSet<_>>()
        != BTreeSet::from(["changes", "observationCount", "parentDigest"])
    {
        return Err("stored model observation manifest fields mismatch".to_string());
    }
    let parent_digest = match object.get("parentDigest") {
        Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(value)) => {
            validate_model_observation_digest(value)?;
            if value == &digest {
                return Err("stored model observation manifest self-parent".to_string());
            }
            Some(value.clone())
        }
        _ => return Err("stored model observation manifest parentDigest is invalid".to_string()),
    };
    if parent_digest != parent_column {
        return Err("stored model observation manifest parent binding mismatch".to_string());
    }
    let observation_count = object
        .get("observationCount")
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| "stored model observation manifest count is invalid".to_string())?;
    let change_values = object
        .get("changes")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "stored model observation manifest changes are invalid".to_string())?;
    let mut changes = Vec::with_capacity(change_values.len());
    let mut previous_index = None;
    for change in change_values {
        let object = change
            .as_object()
            .ok_or_else(|| "stored model observation manifest change is invalid".to_string())?;
        if object.keys().map(String::as_str).collect::<BTreeSet<_>>()
            != BTreeSet::from(["contentDigest", "index", "kind"])
        {
            return Err("stored model observation manifest change fields mismatch".to_string());
        }
        let index = object
            .get("index")
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .filter(|index| *index < observation_count)
            .ok_or_else(|| {
                "stored model observation manifest change index is invalid".to_string()
            })?;
        if previous_index.is_some_and(|previous| index <= previous) {
            return Err("stored model observation manifest changes are not ordered".to_string());
        }
        previous_index = Some(index);
        let kind = object
            .get("kind")
            .and_then(serde_json::Value::as_str)
            .filter(|kind| {
                matches!(
                    *kind,
                    "system_prompt"
                        | "message"
                        | "input_image"
                        | "tool_catalog"
                        | "compaction_prompt"
                        | "input_uptake"
                        | "required_completion_delivery"
                )
            })
            .ok_or_else(|| {
                "stored model observation manifest change kind is invalid".to_string()
            })?;
        let content_digest = object
            .get("contentDigest")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                "stored model observation manifest contentDigest is invalid".to_string()
            })?;
        validate_model_observation_digest(content_digest)?;
        changes.push(ModelObservationChange {
            index,
            reference: ModelObservationReference {
                kind: kind.to_string(),
                content_digest: content_digest.to_string(),
            },
        });
    }
    if parent_digest.is_none()
        && changes
            .iter()
            .enumerate()
            .any(|(index, change)| change.index != index)
    {
        return Err("root model observation manifest append range has a hole".to_string());
    }
    Ok(ModelObservationManifest {
        digest,
        parent_digest,
        observation_count,
        changes,
        manifest_json,
        manifest_bytes,
        first_seen_at_ms,
    })
}

fn apply_model_observation_manifest(
    manifest: &ModelObservationManifest,
    mut references: Vec<ModelObservationReference>,
) -> Result<Vec<ModelObservationReference>, String> {
    if manifest.parent_digest.is_none() && !references.is_empty() {
        return Err("root model observation manifest has a parent state".to_string());
    }
    references.truncate(manifest.observation_count);
    for change in &manifest.changes {
        if change.index < references.len() {
            if references[change.index] == change.reference {
                return Err("model observation manifest contains a redundant change".to_string());
            }
            references[change.index] = change.reference.clone();
        } else if change.index == references.len() {
            references.push(change.reference.clone());
        } else {
            return Err("model observation manifest append range has a hole".to_string());
        }
    }
    if references.len() != manifest.observation_count {
        return Err("model observation manifest count mismatch".to_string());
    }
    Ok(references)
}

fn resolve_model_observation_manifest(
    session_id: &str,
    digest: &str,
    manifests: &HashMap<(String, String), ModelObservationManifest>,
    cache: &mut HashMap<(String, String), Vec<ModelObservationReference>>,
) -> Result<Vec<ModelObservationReference>, String> {
    let root = (session_id.to_string(), digest.to_string());
    if let Some(references) = cache.get(&root) {
        return Ok(references.clone());
    }
    let mut chain = Vec::new();
    let mut visiting = HashSet::new();
    let mut next = Some(root.clone());
    let mut references = Vec::new();
    while let Some(key) = next {
        if let Some(cached) = cache.get(&key) {
            references = cached.clone();
            break;
        }
        if !visiting.insert(key.clone()) {
            return Err("stored model observation manifest parent cycle".to_string());
        }
        let manifest = manifests
            .get(&key)
            .ok_or_else(|| "stored model observation manifest is missing".to_string())?
            .clone();
        next = manifest
            .parent_digest
            .as_ref()
            .map(|parent| (session_id.to_string(), parent.clone()));
        chain.push(manifest);
    }
    for manifest in chain.into_iter().rev() {
        references = apply_model_observation_manifest(&manifest, references)?;
    }
    cache.insert(root.clone(), references);
    cache
        .get(&root)
        .cloned()
        .ok_or_else(|| "stored model observation manifest is missing".to_string())
}

fn load_model_observation_manifests<C: GenericClient>(
    client: &mut C,
    roots: &BTreeSet<(String, String)>,
) -> Result<HashMap<(String, String), ModelObservationManifest>, String> {
    if roots.is_empty() {
        return Ok(HashMap::new());
    }
    let session_ids = roots
        .iter()
        .map(|(session_id, _)| session_id.clone())
        .collect::<Vec<_>>();
    let manifest_digests = roots
        .iter()
        .map(|(_, digest)| digest.clone())
        .collect::<Vec<_>>();
    client
        .query(
            "WITH RECURSIVE requested(session_id,manifest_digest) AS (SELECT * FROM UNNEST($1::text[],$2::text[])), chain(session_id,manifest_digest,parent_digest,manifest_json,manifest_bytes,first_seen_at_ms) AS (SELECT manifest.session_id,manifest.manifest_digest,manifest.parent_digest,manifest.manifest_json,manifest.manifest_bytes,manifest.first_seen_at_ms FROM runtime.model_observation_manifests manifest JOIN requested USING(session_id,manifest_digest) UNION SELECT parent.session_id,parent.manifest_digest,parent.parent_digest,parent.manifest_json,parent.manifest_bytes,parent.first_seen_at_ms FROM runtime.model_observation_manifests parent JOIN chain child ON parent.session_id=child.session_id AND parent.manifest_digest=child.parent_digest) SELECT session_id,manifest_digest,parent_digest,manifest_json,manifest_bytes,first_seen_at_ms FROM chain",
            &[&session_ids, &manifest_digests],
        )
        .map_err(|error| format!("load model observation manifests failed: {error}"))?
        .into_iter()
        .map(|row| {
            let session_id = row.get::<_, String>(0);
            let digest = row.get::<_, String>(1);
            let manifest = decode_model_observation_manifest(
                digest.clone(),
                row.get(2),
                row.get(3),
                row.get(4),
                row.get(5),
            )?;
            Ok(((session_id, digest), manifest))
        })
        .collect::<Result<HashMap<_, _>, String>>()
}

fn load_latest_model_observation_state<C: GenericClient>(
    client: &mut C,
    session_id: &str,
) -> Result<(Option<String>, Vec<ModelObservationReference>), String> {
    let Some(row) = client
        .query_opt(
            "SELECT payload::text FROM app_core_sessionevent WHERE session_id=$1 AND payload->>'type'='model_request_started' ORDER BY sequence DESC LIMIT 1",
            &[&session_id],
        )
        .map_err(|error| format!("load latest model request failed: {error}"))?
    else {
        return Ok((None, Vec::new()));
    };
    let mut wire = serde_json::from_str::<serde_json::Value>(row.get::<_, String>(0).as_str())
        .map_err(|error| format!("decode latest stored model request failed: {error}"))?;
    let (stored_session_id, root_digest) = model_request_manifest_key(&wire)?
        .ok_or_else(|| "latest stored model request manifest is missing".to_string())?;
    if stored_session_id != session_id {
        return Err("stored model observation manifest session binding mismatch".to_string());
    }
    // A later append must not silently recreate missing ancestor content.
    hydrate_session_wire_values(client, std::slice::from_mut(&mut wire))?;
    let references = wire
        .pointer("/payload/observations")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "hydrated model request observations are missing".to_string())?
        .iter()
        .map(|observation| {
            compact_model_observation(observation, 0).map(|(_, reference)| reference)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok((Some(root_digest), references))
}

pub(crate) fn hydrate_session_wire_values<C: GenericClient>(
    client: &mut C,
    wires: &mut [serde_json::Value],
) -> Result<(), String> {
    for wire in wires.iter_mut() {
        super::session_payload::decode(wire)?;
    }
    let roots = wires
        .iter()
        .map(model_request_manifest_key)
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flatten()
        .collect::<BTreeSet<_>>();
    if roots.is_empty() {
        return Ok(());
    }
    let manifests = load_model_observation_manifests(client, &roots)?;
    let mut manifest_cache = HashMap::new();
    let mut references_by_root = HashMap::new();
    let mut kind_keys = BTreeSet::new();
    for wire in wires.iter() {
        let Some((session_id, digest)) = model_request_manifest_key(wire)? else {
            continue;
        };
        if references_by_root.contains_key(&(session_id.clone(), digest.clone())) {
            continue;
        }
        let references = resolve_model_observation_manifest(
            session_id.as_str(),
            digest.as_str(),
            &manifests,
            &mut manifest_cache,
        )?;
        for reference in &references {
            kind_keys.insert((
                session_id.clone(),
                reference.kind.clone(),
                reference.content_digest.clone(),
            ));
        }
        references_by_root.insert((session_id.clone(), digest.clone()), references);
    }
    let keys = kind_keys
        .iter()
        .map(|(session_id, _, digest)| (session_id.clone(), digest.clone()))
        .collect::<BTreeSet<_>>();
    let content_session_ids = keys
        .iter()
        .map(|(session_id, _)| session_id.clone())
        .collect::<Vec<_>>();
    let content_digests = keys
        .iter()
        .map(|(_, digest)| digest.clone())
        .collect::<Vec<_>>();
    let rows = client
        .query(
            "SELECT content.session_id,content.content_digest,content.kind,content.content_json,content.content_bytes,content.first_seen_at_ms FROM runtime.model_observation_contents content JOIN UNNEST($1::text[],$2::text[]) requested(session_id,content_digest) USING(session_id,content_digest)",
            &[&content_session_ids, &content_digests],
        )
        .map_err(|error| format!("load model observation contents failed: {error}"))?;
    let contents = rows
        .into_iter()
        .map(|row| {
            let session_id = row.get::<_, String>(0);
            let content = ModelObservationContent {
                digest: row.get(1),
                kind: row.get(2),
                content_json: row.get(3),
                content_bytes: row.get(4),
                first_seen_at_ms: row.get(5),
            };
            ((session_id, content.digest.clone()), content)
        })
        .collect::<HashMap<_, _>>();
    if contents.len() != keys.len() {
        return Err("stored model observation content set is incomplete".to_string());
    }
    if kind_keys.iter().any(|(session_id, kind, digest)| {
        contents
            .get(&(session_id.clone(), digest.clone()))
            .is_none_or(|content| content.kind != *kind)
    }) {
        return Err("stored model observation content kind conflict".to_string());
    }
    for wire in wires {
        let Some((session_id, digest)) = model_request_manifest_key(wire)? else {
            continue;
        };
        let references = references_by_root
            .get(&(session_id.clone(), digest))
            .ok_or_else(|| "stored model observation manifest is missing".to_string())?;
        hydrate_model_request_wire(wire, references, &contents)?;
    }
    Ok(())
}

fn hydrate_model_request_wire(
    wire: &mut serde_json::Value,
    references: &[ModelObservationReference],
    contents: &HashMap<(String, String), ModelObservationContent>,
) -> Result<(), String> {
    let (session_id, _) = model_request_manifest_key(wire)?
        .ok_or_else(|| "stored model request manifest is missing".to_string())?;
    let observations = references
        .iter()
        .map(|reference| {
            let content = contents
                .get(&(session_id.clone(), reference.content_digest.clone()))
                .ok_or_else(|| "stored model observation content is missing".to_string())?;
            if content.kind != reference.kind
                || content.content_bytes != i64::try_from(content.content_json.len()).unwrap_or(-1)
                || sha256_json(
                    MODEL_OBSERVATION_CONTENT_DIGEST_DOMAIN,
                    content.content_json.as_str(),
                ) != reference.content_digest
            {
                return Err("stored model observation content binding mismatch".to_string());
            }
            let observation = serde_json::from_str::<serde_json::Value>(
                content.content_json.as_str(),
            )
            .map_err(|error| format!("decode stored model observation content failed: {error}"))?;
            if observation.get("kind").and_then(serde_json::Value::as_str)
                != Some(reference.kind.as_str())
                || !observation.is_object()
                || observation.get("contentDigest").is_some()
            {
                return Err("stored model observation content fields mismatch".to_string());
            }
            Ok(observation)
        })
        .collect::<Result<Vec<_>, String>>()?;
    wire.pointer_mut("/payload/observations")
        .ok_or_else(|| "stored model request observations are missing".to_string())?
        .clone_from(&serde_json::Value::Array(observations));
    Ok(())
}

impl PostgresSessionLog {
    pub(crate) fn append_wait_handoff(
        &self,
        agent_run_id: &str,
        events: &[SequencedSessionRecord],
        checkpoint: &CheckpointRecord,
        fence: &RuntimeJobLeaseFence,
        handoff: &crate::wait_recovery::WaitHandoff,
    ) -> Result<SessionCommitReceipt, String> {
        run_postgres_blocking(|| {
            self.append_transaction(
                agent_run_id,
                events,
                Some(fence),
                None,
                Some(checkpoint),
                Some(handoff),
            )
        })
    }
    /// A hosted terminal receipt for cancellation before Session admission.
    /// It is committed under the same Session lock as admission, with a current
    /// lifecycle lease and a durable cancellation request. No model history is written.
    pub fn commit_pre_admission_cancellation(
        &self,
        agent_run_id: &str,
        authorization_digest: &str,
        fence: &RuntimeJobLeaseFence,
    ) -> Result<(), String> {
        run_postgres_blocking(|| {
            self.connections.with_client(|client| {
            let mut tx = client.transaction().map_err(|e| e.to_string())?;
            let run = tx.query_opt(
                "SELECT session_id FROM app_core_agentrun WHERE id=$1 FOR UPDATE",
                &[&agent_run_id],
            ).map_err(|e| e.to_string())?.ok_or("pre-admission run missing")?;
            if run.get::<_, String>(0) != self.session_id {
                return Err("pre-admission session mismatch".into());
            }
            tx.query_opt("SELECT id FROM app_core_session WHERE id=$1 AND workspace_id=$2 FOR UPDATE",
                &[&self.session_id, &self.workspace_id]).map_err(|e| e.to_string())?
                .ok_or("pre-admission session missing")?;
            let expected_job = format!("agent_run.lifecycle:{agent_run_id}");
            if fence.job_id != expected_job || fence.job_kind != AGENT_RUN_LIFECYCLE_JOB_KIND {
                return Err(RUNTIME_JOB_LEASE_FENCE_REJECTED.into());
            }
            tx.query_opt(
                "SELECT job_id FROM runtime.runtime_jobs WHERE job_id=$1 AND job_kind=$2 AND status='running' AND lease_owner=$3 AND session_id=$4 AND idempotency_key=$5 AND lease_expires_at_ms>(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint FOR UPDATE",
                &[&fence.job_id, &fence.job_kind, &fence.lease_owner, &self.session_id,
                  &format!("{expected_job}:{authorization_digest}")],
            ).map_err(|e| e.to_string())?.ok_or(RUNTIME_JOB_LEASE_FENCE_REJECTED)?;
            let requested: bool = tx.query_one(
                "SELECT EXISTS(SELECT 1 FROM runtime.runtime_events WHERE event_id=$1 AND session_id=$2 AND task_id=$3 AND event_type=$4 AND payload_json::jsonb->>'authorizationDigest'=$5)",
                &[&format!("agent_run_cancel_requested:{agent_run_id}"), &self.session_id,
                  &agent_run_id, &AGENT_RUN_CANCEL_REQUESTED_EVENT_SCHEMA, &authorization_digest],
            ).map_err(|e| e.to_string())?.get(0);
            let admitted: bool = tx.query_one(
                "SELECT EXISTS(SELECT 1 FROM app_core_sessionevent WHERE agent_run_id=$1 AND NOT session_level)",
                &[&agent_run_id],
            ).map_err(|e| e.to_string())?.get(0);
            if !requested || admitted {
                return Err("pre-admission cancellation evidence mismatch".into());
            }
            tx.execute("UPDATE app_core_agentrun SET \"preAdmissionCancelledAt\"=COALESCE(\"preAdmissionCancelledAt\",clock_timestamp()) WHERE id=$1", &[&agent_run_id])
                .map_err(|e| e.to_string())?;
            tx.commit().map_err(|e| e.to_string())
        })
        })
    }

    pub(crate) fn new(
        connections: Arc<PostgresConnectionPool>,
        workspace_id: String,
        session_id: String,
        prompt: String,
    ) -> Self {
        Self {
            connections,
            workspace_id,
            session_id,
            prompt,
            accepted_start: None,
            projection_store: None,
            agent_run_state: Arc::new(Mutex::new(HashMap::new())),
            append_lock: Arc::new(Mutex::new(())),
        }
    }

    pub(crate) fn new_with_projection_store(
        projection_store: PostgresRuntimeStore,
        connections: Arc<PostgresConnectionPool>,
        workspace_id: String,
        session_id: String,
        prompt: String,
    ) -> Self {
        let mut log = Self::new(connections, workspace_id, session_id, prompt);
        log.projection_store = Some(projection_store);
        log
    }

    pub(crate) fn with_agent_run_start(
        mut self,
        start: &crate::contract::AgentRunStart,
    ) -> Result<Self, String> {
        self.accepted_start = Some(start.clone());
        self.connections
            .with_client(|client| self.validate_input_binding(client, &start.agent_run_id))?;
        Ok(self)
    }

    fn validate_input_binding(
        &self,
        tx: &mut impl GenericClient,
        run_id: &str,
    ) -> Result<(), String> {
        let Some(start) = &self.accepted_start else {
            return Ok(());
        };
        let initial: Option<String> = tx
            .query_opt(
                "SELECT initial_host_id FROM app_core_agentinputqueue WHERE agent_run_id=$1",
                &[&run_id],
            )
            .map_err(|e| e.to_string())?
            .and_then(|row| row.get(0));
        match &start.initial_input {
            crate::contract::AgentRunStartInitialInput::HostEvent {
                attempt_id, input, ..
            } => {
                if initial.as_deref() != Some(attempt_id.as_str()) {
                    return Err("host_event_admission_binding_mismatch".into());
                }
                validate_hosted_native_input(tx, start, input)
            }
            _ if initial.is_some() => Err("host_event_admission_kind_mismatch".into()),
            _ => Ok(()),
        }
    }
}

impl SessionLogPort for PostgresSessionLog {
    fn append_session_records<'a>(
        &'a self,
        agent_run_id: &'a str,
        events: &'a [SequencedSessionRecord],
    ) -> SessionLogFuture<'a> {
        let store = self.clone();
        let agent_run_id = agent_run_id.to_string();
        let events = events.to_vec();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                store.append(agent_run_id.as_str(), events.as_slice())
            })
            .await
            .map_err(|error| format!("join Postgres session log append failed: {error}"))?
        })
    }

    fn append_session_records_with_runtime_job_lease<'a>(
        &'a self,
        agent_run_id: &'a str,
        events: &'a [SequencedSessionRecord],
        fence: &'a RuntimeJobLeaseFence,
    ) -> SessionLogFuture<'a> {
        let store = self.clone();
        let agent_run_id = agent_run_id.to_string();
        let events = events.to_vec();
        let fence = fence.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                store.append_transaction(
                    agent_run_id.as_str(),
                    events.as_slice(),
                    Some(&fence),
                    None,
                    None,
                    None,
                )
            })
            .await
            .map_err(|error| format!("join fenced Postgres session log append failed: {error}"))?
        })
    }
}

impl PostgresSessionLog {
    fn append(
        &self,
        agent_run_id: &str,
        events: &[SequencedSessionRecord],
    ) -> Result<SessionCommitReceipt, String> {
        self.append_transaction(agent_run_id, events, None, None, None, None)
    }

    pub fn append_session_records_with_runtime_job_lease_blocking(
        &self,
        agent_run_id: &str,
        events: &[SequencedSessionRecord],
        fence: &RuntimeJobLeaseFence,
    ) -> Result<SessionCommitReceipt, String> {
        // The blocking boundary permits sync Postgres inside Tokio without
        // creating another thread for a multi-thread Runtime blocking caller.
        run_postgres_blocking(|| {
            self.append_transaction(agent_run_id, events, Some(fence), None, None, None)
        })
    }

    pub fn append_recovery_checkpoint_with_runtime_job_lease_blocking(
        &self,
        agent_run_id: &str,
        events: &[SequencedSessionRecord],
        checkpoint: &CheckpointRecord,
        fence: &RuntimeJobLeaseFence,
    ) -> Result<SessionCommitReceipt, String> {
        run_postgres_blocking(|| {
            self.append_transaction(
                agent_run_id,
                events,
                Some(fence),
                None,
                Some(checkpoint),
                None,
            )
        })
    }

    pub fn append_rewritten_session_records_with_runtime_job_lease_blocking(
        &self,
        agent_run_id: &str,
        events: &[SequencedSessionRecord],
        rewrite: &RewriteLastUserTailRequest,
        fence: &RuntimeJobLeaseFence,
    ) -> Result<SessionCommitReceipt, String> {
        run_postgres_blocking(|| {
            self.append_transaction(agent_run_id, events, Some(fence), Some(rewrite), None, None)
        })
    }

    fn append_transaction(
        &self,
        agent_run_id: &str,
        events: &[SequencedSessionRecord],
        fence: Option<&RuntimeJobLeaseFence>,
        rewrite: Option<&RewriteLastUserTailRequest>,
        checkpoint: Option<&CheckpointRecord>,
        handoff: Option<&crate::wait_recovery::WaitHandoff>,
    ) -> Result<SessionCommitReceipt, String> {
        validate_sequenced_session_records(events)?;
        let _append_guard = self
            .append_lock
            .lock()
            .map_err(|_| "session record append lock poisoned".to_string())?;
        let result = self.connections.with_client(|client| {
            let mut tx = client
                .transaction()
                .map_err(|error| crate::session_delivery::postgres_error("begin session log append failed", error))?;
            if events.iter().any(|item| is_successful_work_confirmation(&item.event)) {
                if fence.is_none() { return Err("work_confirmation_lease_required".into()); }
                let start = self.accepted_start.as_ref().ok_or("work_confirmation_admission_missing")?;
                lock_work_confirmation_scope(&mut tx, start, events)?;
            }
            let session = tx
                .query_opt(
                    "SELECT workspace_id FROM app_core_session WHERE id=$1 FOR UPDATE",
                    &[&self.session_id],
                )
                .map_err(|error| crate::session_delivery::postgres_error("lock chat session failed", error))?
                .ok_or_else(|| "chat session not found".to_string())?;
            if session.get::<_, String>(0) != self.workspace_id {
                return Err("chat session workspace binding mismatch".to_string());
            }
            self.validate_input_binding(&mut tx, agent_run_id)?;
            if let Some(fence) = fence {
                let lease_is_current = tx
                    .query_opt(
                        "SELECT 1 FROM runtime.runtime_jobs WHERE job_id=$1 AND job_kind=$2 AND status='running' AND lease_owner=$3 AND session_id=$4 AND lease_expires_at_ms>(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint FOR UPDATE",
                        &[&fence.job_id, &fence.job_kind, &fence.lease_owner, &self.session_id],
                    )
                    .map_err(|error| crate::session_delivery::postgres_error("lock runtime job lease fence failed", error))?
                    .is_some();
                if !lease_is_current {
                    return Err(RUNTIME_JOB_LEASE_FENCE_REJECTED.to_string());
                }
            }
            if let Some(checkpoint) = checkpoint {
                if checkpoint.kind != CheckpointKindV1::Recovery
                    || checkpoint.session_id != self.session_id
                    || checkpoint.status != "committed"
                    || checkpoint.done_reason.is_some()
                {
                    return Err("recovery checkpoint binding mismatch".to_string());
                }
                save_checkpoint(&mut tx, checkpoint)?;
            }
            if let Some(handoff) = handoff {
                let checkpoint = checkpoint.ok_or("wait handoff requires recovery checkpoint")?;
                let payload = serde_json::from_str::<centaeris_core::runtime::contracts::RuntimeRecoveryCheckpointV1>(&checkpoint.payload_json).map_err(|e|e.to_string())?;
                handoff.validate(&payload)?;
                if events.len() == 1 {
                    crate::wait_recovery::WaitHandoff::validate_reference(checkpoint, &events[0].event)?;
                }
                if payload.agent_run_id != agent_run_id || checkpoint.turn_id != handoff.wait_checkpoint.turn_id
                    || events.len()!=1 || events[0].event.event_type!=SessionRecordType::CheckpointRef
                    || events[0].event.payload["checkpointId"].as_str()!=Some(checkpoint.checkpoint_id.as_str())
                { return Err("wait handoff Session binding mismatch".into()); }
                let serialized = serde_json::to_string(handoff).map_err(|e|e.to_string())?;
                let stored: Option<String> = tx.query_one("SELECT wait_handoff_json FROM runtime.checkpoints WHERE checkpoint_id=$1 FOR UPDATE", &[&checkpoint.checkpoint_id]).map_err(|e|e.to_string())?.get(0);
                match stored {
                    Some(value) if value!=serialized => return Err("wait handoff idempotency conflict".into()),
                    Some(_) => {},
                    None => { tx.execute("UPDATE runtime.checkpoints SET wait_handoff_json=$2 WHERE checkpoint_id=$1", &[&checkpoint.checkpoint_id,&serialized]).map_err(|e|e.to_string())?; }
                }
            }
            if let Some(receipt) = self.load_idempotent_batch(&mut tx, agent_run_id, events)? {
                consume_waits_for_terminal_batch(&mut tx, &self.session_id, agent_run_id, events)?;
                tx.commit()
                    .map_err(|error| crate::session_delivery::postgres_error("commit idempotent session append failed", error))?;
                return Ok(receipt);
            }
            if let Some(handoff) = handoff {
                validate_wait_handoff_source(&mut tx, agent_run_id, handoff)?;
            }
            let mut records_to_append = events.to_vec();
            let tombstoned_event_ids = if let Some(rewrite) = rewrite {
                if events.first().is_none_or(|item| item.sequence != 2) {
                    return Err(
                        "rewrite initial records must start at AgentRun sequence 2".to_string()
                    );
                }
                let existing = load_session_records(&mut tx, self.session_id.as_str())?;
                let tombstone = rewrite_last_user_tail_tombstone(
                    existing.as_slice(),
                    self.session_id.as_str(),
                    rewrite.target_message_id.as_str(),
                    rewrite.expected_tail_message_id.as_str(),
                    rewrite.new_turn_id.as_str(),
                    rewrite.new_agent_run_id.as_str(),
                    rewrite.created_at_ms,
                )?;
                let targets = tombstone.payload["targetEventIds"]
                    .as_array()
                    .expect("validated tombstone targets")
                    .iter()
                    .map(|value| {
                        value
                            .as_str()
                            .expect("validated tombstone target")
                            .to_string()
                    })
                    .collect::<Vec<_>>();
                records_to_append.insert(
                    0,
                    SequencedSessionRecord {
                        sequence: 1,
                        event: tombstone,
                    },
                );
                let mut whole_session = existing;
                whole_session.extend(records_to_append.iter().map(|item| item.event.clone()));
                reduce_events(self.session_id.as_str(), whole_session.iter())?;
                targets
            } else {
                Vec::new()
            };
            validate_sequenced_session_records(records_to_append.as_slice())?;
            // 普通 AgentRun 只增量校验；rewrite 已在上面重放整个 active Session。
            let validated =
                self.apply_batch_incremental(&mut tx, agent_run_id, records_to_append.as_slice());
            if let Err(error) = validated {
                self.agent_run_state
                    .lock()
                    .map_err(|_| "session record AgentRun state lock poisoned".to_string())?
                    .remove(agent_run_id);
                return Err(error);
            }
            let next_session_sequence = tx
                .query_one(
                    "SELECT COALESCE(MAX(sequence), 0) FROM app_core_sessionevent WHERE session_id=$1",
                    &[&self.session_id],
                )
                .map_err(|error| crate::session_delivery::postgres_error("load session sequence failed", error))?
                .get::<_, i32>(0);
            let observation_state = if records_to_append
                    .iter()
                    .any(|item| item.event.event_type == SessionRecordType::ModelRequestStarted)
                {
                    load_latest_model_observation_state(&mut tx, self.session_id.as_str())?
                } else {
                    (None, Vec::new())
                };
            let prepared = self.prepare_session_append(
                &mut tx,
                agent_run_id,
                records_to_append.as_slice(),
                next_session_sequence,
                observation_state,
            )?;
            self.insert_session_append(&mut tx, &prepared)?;
            consume_waits_for_terminal_batch(&mut tx, &self.session_id, agent_run_id, events)?;
            let records = prepared.rows.into_iter().map(|row| row.record).collect();
            if !tombstoned_event_ids.is_empty() {
                tx.execute(
                    "UPDATE app_core_sessionevent SET projects_to_agent_run_stream=FALSE WHERE session_id=$1 AND \"eventId\"=ANY($2)",
                    &[&self.session_id, &tombstoned_event_ids],
                )
                .map_err(|error| crate::session_delivery::postgres_error("update tombstoned session projection failed", error))?;
            }
            tx.commit()
                .map_err(|error| crate::session_delivery::postgres_error("commit session log append failed", error))?;
            Ok(SessionCommitReceipt { records })
        });
        if result.is_err() {
            self.agent_run_state
                .lock()
                .map_err(|_| "session record AgentRun state lock poisoned".to_string())?
                .remove(agent_run_id);
        }
        let receipt = result?;
        if let (Some(store), Some(source_high_water)) = (
            self.projection_store.as_ref(),
            receipt.records.iter().map(|record| record.sequence).max(),
        ) {
            let current =
                store.load_or_initialize_current_transcript_generation(self.session_id.as_str());
            let projection = current.as_ref().map_err(Clone::clone).and_then(|current| {
                store.catch_up_transcript_projection(
                    self.session_id.as_str(),
                    current.projection_generation.as_str(),
                    source_high_water,
                )
            });
            if let Err(error) = projection {
                eprintln!(
                    "transcript projection catch-up deferred after source commit: sessionId={} error={error}",
                    self.session_id
                );
                if let Ok(current) = current {
                    let invalidated = store
                        .load_transcript_projection_head(
                            self.session_id.as_str(),
                            current.projection_generation.as_str(),
                        )
                        .ok()
                        .flatten()
                        .is_some_and(|head| head.invalidation_reason.is_some());
                    if invalidated {
                        if let Err(rebuild_error) = store.schedule_transcript_generation_rebuild(
                            self.session_id.as_str(),
                            source_high_water,
                            current.projection_generation.as_str(),
                        ) {
                            eprintln!(
                                "transcript generation rebuild scheduling failed after source commit: sessionId={} error={rebuild_error}",
                                self.session_id
                            );
                        }
                    }
                }
            }
        }
        Ok(receipt)
    }

    fn load_idempotent_batch(
        &self,
        tx: &mut Transaction<'_>,
        agent_run_id: &str,
        events: &[SequencedSessionRecord],
    ) -> Result<Option<SessionCommitReceipt>, String> {
        let event_ids = events
            .iter()
            .map(|item| item.event.event_id.clone())
            .collect::<Vec<_>>();
        let rows = tx
            .query(
                "SELECT \"eventId\",workspace_id,session_id,agent_run_id,sequence,agent_run_sequence,projects_to_agent_run_stream,payload::text FROM app_core_sessionevent WHERE \"eventId\"=ANY($1)",
                &[&event_ids],
            )
            .map_err(|error| format!("query session record batch ids failed: {error}"))?;
        if rows.is_empty() {
            return Ok(None);
        }
        if rows.len() != events.len() {
            return Err("session record batch partially overlaps committed facts".to_string());
        }
        let mut wires = rows
            .iter()
            .map(|row| {
                serde_json::from_str::<serde_json::Value>(row.get::<_, String>(7).as_str())
                    .map_err(|error| format!("decode stored session record failed: {error}"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        hydrate_session_wire_values(tx, wires.as_mut_slice())?;
        let mut existing = rows
            .into_iter()
            .zip(wires)
            .map(|(row, wire)| {
                (
                    row.get::<_, String>(0),
                    (
                        row.get::<_, String>(1),
                        row.get::<_, String>(2),
                        row.get::<_, String>(3),
                        row.get::<_, i32>(4),
                        row.get::<_, i32>(5),
                        row.get::<_, bool>(6),
                        wire,
                    ),
                )
            })
            .collect::<HashMap<_, _>>();
        let mut records = Vec::with_capacity(events.len());
        for item in events {
            let row = existing
                .remove(item.event.event_id.as_str())
                .ok_or_else(|| "session record batch contains duplicate event ids".to_string())?;
            let stored = parse_wire_record(&row.6).map_err(|error| error.to_string())?;
            let sequence = u64::try_from(row.3)
                .map_err(|_| "stored session sequence is invalid".to_string())?;
            let same = row.0 == self.workspace_id
                && row.1 == self.session_id
                && row.2 == agent_run_id
                && stored.sequence == sequence
                && row.4
                    == i32::try_from(item.sequence)
                        .map_err(|_| "session record AgentRun sequence overflow".to_string())?
                && row.5 == session_record_projects_to_agent_run_stream(item.event.event_type)
                && stored.event == item.event;
            if !same {
                return Err(format!(
                    "session record idempotency conflict: {}",
                    item.event.event_id
                ));
            }
            records.push(CommittedSessionRecord {
                sequence,
                event: item.event.clone(),
            });
        }
        Ok(Some(SessionCommitReceipt { records }))
    }

    fn apply_batch_incremental(
        &self,
        tx: &mut Transaction<'_>,
        agent_run_id: &str,
        events: &[SequencedSessionRecord],
    ) -> Result<(), String> {
        let mut state_map = self
            .agent_run_state
            .lock()
            .map_err(|_| "session record AgentRun state lock poisoned".to_string())?;
        if !state_map.contains_key(agent_run_id) {
            let loaded = self.load_agent_run_state(tx, agent_run_id)?;
            state_map.insert(agent_run_id.to_string(), loaded);
        }
        let state = state_map
            .get_mut(agent_run_id)
            .ok_or_else(|| "session record AgentRun state is missing".to_string())?;
        let first_sequence = events
            .first()
            .map(|item| item.sequence)
            .ok_or_else(|| "session record batch must not be empty".to_string())?;
        if state.last_sequence != 0 && first_sequence != state.last_sequence + 1 {
            return Err("session record sequence gap".to_string());
        }
        let first_batch = state.last_sequence == 0;
        apply_batch_to_state(
            state,
            self.session_id.as_str(),
            events.iter().map(|item| &item.event),
        )?;
        if first_batch
            && (state.first_event_type != Some(SessionRecordType::AgentRunStarted)
                || state.agent_run_started_count != 1)
        {
            return Err("session record log must start with one agent_run_started".to_string());
        }
        state.last_sequence += events.len() as u64;
        Ok(())
    }

    fn load_agent_run_state(
        &self,
        tx: &mut Transaction<'_>,
        agent_run_id: &str,
    ) -> Result<AgentRunAppendState, String> {
        use centaeris_core::session::SessionRecordType as Type;
        let rows = tx
            .query(
                "SELECT payload::text FROM app_core_sessionevent WHERE agent_run_id=$1 AND session_level=false ORDER BY agent_run_sequence",
                &[&agent_run_id],
            )
            .map_err(|error| format!("load session AgentRun state failed: {error}"))?;
        let mut state = AgentRunAppendState {
            last_sequence: rows.len() as u64,
            projection: SessionProjection::default(),
            seen_event_ids: HashSet::new(),
            open_tool_call_ids: HashMap::new(),
            agent_run_started_count: 0,
            user_message_count: 0,
            initial_host_input_id: self.accepted_start.as_ref().and_then(|start| {
                match &start.initial_input {
                    crate::contract::AgentRunStartInitialInput::HostEvent { input, .. } => {
                        Some(input.input_id.clone())
                    }
                    _ => None,
                }
            }),
            assistant_message_count: 0,
            last_assistant_status: None,
            terminal_seen: false,
            first_event_type: None,
        };
        let mut wires = rows
            .iter()
            .map(|row| {
                serde_json::from_str(row.get::<_, String>(0).as_str())
                    .map_err(|error| format!("decode session AgentRun state failed: {error}"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        hydrate_session_wire_values(tx, wires.as_mut_slice())?;
        let mut events = Vec::with_capacity(rows.len());
        for value in wires {
            events.push(
                parse_wire_record(&value)
                    .map_err(|error| error.to_string())?
                    .event,
            );
        }
        apply_batch_to_state(&mut state, self.session_id.as_str(), events.iter())?;
        if !rows.is_empty() {
            if state.first_event_type != Some(Type::AgentRunStarted) {
                return Err("session record log must start with one agent_run_started".to_string());
            }
            if state.agent_run_started_count != 1 {
                return Err("session record log must contain one agent_run_started".to_string());
            }
        }
        Ok(state)
    }

    fn prepare_session_append(
        &self,
        tx: &mut impl GenericClient,
        agent_run_id: &str,
        events: &[SequencedSessionRecord],
        mut next_session_sequence: i32,
        (mut parent_digest, mut parent_references): (
            Option<String>,
            Vec<ModelObservationReference>,
        ),
    ) -> Result<PreparedSessionAppend, String> {
        let mut rows = Vec::with_capacity(events.len());
        let mut contents = BTreeMap::<String, ModelObservationContent>::new();
        let mut manifests = BTreeMap::<String, ModelObservationManifest>::new();
        for (index, item) in events.iter().enumerate() {
            let event = &item.event;
            if is_successful_work_confirmation(event) {
                let start = self
                    .accepted_start
                    .as_ref()
                    .ok_or("work_confirmation_admission_missing")?;
                validate_work_confirmation_commit(tx, start, events, index)?;
            }
            let turn_id = event.turn_id.as_deref().unwrap_or_default();
            if event.session_id != self.session_id
                || turn_id.trim().is_empty()
                || event.agent_run_id.as_deref() != Some(agent_run_id)
            {
                return Err("session record AgentRun/Session binding mismatch".to_string());
            }
            if event.event_type == SessionRecordType::AgentRunStarted
                && event
                    .payload
                    .get("userObjective")
                    .and_then(serde_json::Value::as_str)
                    != Some(self.prompt.as_str())
            {
                return Err("agent_run_started objective does not match prompt".to_string());
            }
            if event.event_type == SessionRecordType::UserMessage {
                if self.accepted_start.as_ref().is_some_and(|start| {
                    matches!(
                        start.initial_input,
                        crate::contract::AgentRunStartInitialInput::HostEvent { .. }
                    )
                }) {
                    return Err("host_event_admission_kind_mismatch".into());
                }
                let expected_message_id = format!("message:{turn_id}:user");
                if self
                    .accepted_start
                    .as_ref()
                    .is_some_and(|start| start.turn_id != turn_id)
                    || event
                        .payload
                        .get("messageId")
                        .and_then(serde_json::Value::as_str)
                        != Some(expected_message_id.as_str())
                    || event
                        .payload
                        .get("text")
                        .and_then(serde_json::Value::as_str)
                        != Some(self.prompt.as_str())
                {
                    return Err("user message does not match accepted prompt".to_string());
                }
            }
            if event.event_type == SessionRecordType::HostEventInput {
                let start = self
                    .accepted_start
                    .as_ref()
                    .ok_or("host_event_admission_missing")?;
                let input =
                    serde_json::from_value(event.payload.clone()).map_err(|e| e.to_string())?;
                validate_hosted_native_input(tx, start, &input)?;
            }
            if event.event_type == SessionRecordType::AssistantMessage {
                let expected_message_id = format!("message:{turn_id}:assistant");
                if event
                    .payload
                    .get("messageId")
                    .and_then(serde_json::Value::as_str)
                    != Some(expected_message_id.as_str())
                {
                    return Err("assistant message id does not match AgentRun".to_string());
                }
            }
            let agent_run_sequence = i32::try_from(item.sequence)
                .map_err(|_| "session record AgentRun sequence overflow".to_string())?;
            next_session_sequence = next_session_sequence
                .checked_add(1)
                .ok_or_else(|| "session record sequence overflow".to_string())?;
            let sequence = u64::try_from(next_session_sequence)
                .map_err(|_| "session record sequence overflow".to_string())?;
            let mut wire = wire_record_value(&SequencedSessionRecord {
                sequence,
                event: event.clone(),
            })
            .map_err(|error| error.to_string())?;
            if event.event_type == SessionRecordType::ModelRequestStarted {
                let (new_contents, manifest, references) = compact_model_request_wire(
                    &mut wire,
                    event.created_at_ms,
                    parent_digest,
                    parent_references.as_slice(),
                )?;
                parent_digest = Some(manifest.digest.clone());
                parent_references = references;
                manifests.insert(manifest.digest.clone(), manifest);
                for content in new_contents {
                    if let Some(existing) = contents.get(content.digest.as_str()) {
                        if existing.kind != content.kind
                            || existing.content_json != content.content_json
                            || existing.content_bytes != content.content_bytes
                        {
                            return Err("model observation content digest conflict".to_string());
                        }
                    } else {
                        contents.insert(content.digest.clone(), content);
                    }
                }
            }
            let payload = serde_json::to_string(&super::session_payload::encode(&wire)?)
                .map_err(|error| format!("serialize session record failed: {error}"))?;
            let commit_payload = serde_json::json!({
                "sessionRecordId": event.event_id,
                "sessionRecordType": event.event_type.as_str(),
                "sequence": sequence,
                "agentRunSequence": item.sequence,
            })
            .to_string();
            rows.push(PreparedSessionRow {
                record: CommittedSessionRecord {
                    sequence,
                    event: event.clone(),
                },
                agent_run_id: agent_run_id.to_string(),
                agent_run_sequence: Some(agent_run_sequence),
                session_level: false,
                projects_to_agent_run_stream: session_record_projects_to_agent_run_stream(
                    event.event_type,
                ),
                payload,
                commit_payload,
            });
        }
        Ok(PreparedSessionAppend {
            rows,
            contents: contents.into_values().collect(),
            manifests: manifests.into_values().collect(),
        })
    }

    fn insert_session_append(
        &self,
        tx: &mut Transaction<'_>,
        prepared: &PreparedSessionAppend,
    ) -> Result<(), String> {
        if !prepared.manifests.is_empty() {
            let digests = prepared
                .manifests
                .iter()
                .map(|manifest| manifest.digest.clone())
                .collect::<Vec<_>>();
            let parents = prepared
                .manifests
                .iter()
                .map(|manifest| manifest.parent_digest.clone())
                .collect::<Vec<_>>();
            let json = prepared
                .manifests
                .iter()
                .map(|manifest| manifest.manifest_json.clone())
                .collect::<Vec<_>>();
            let bytes = prepared
                .manifests
                .iter()
                .map(|manifest| manifest.manifest_bytes)
                .collect::<Vec<_>>();
            let timestamps = prepared
                .manifests
                .iter()
                .map(|manifest| manifest.first_seen_at_ms)
                .collect::<Vec<_>>();
            tx.execute(
                "INSERT INTO runtime.model_observation_manifests(session_id,manifest_digest,parent_digest,manifest_json,manifest_bytes,first_seen_at_ms) SELECT $1,batch.manifest_digest,batch.parent_digest,batch.manifest_json,batch.manifest_bytes,batch.first_seen_at_ms FROM UNNEST($2::text[],$3::text[],$4::text[],$5::bigint[],$6::bigint[]) batch(manifest_digest,parent_digest,manifest_json,manifest_bytes,first_seen_at_ms) ON CONFLICT(session_id,manifest_digest) DO NOTHING",
                &[&self.session_id, &digests, &parents, &json, &bytes, &timestamps],
            ).map_err(|error| crate::session_delivery::postgres_error("append model observation manifests failed", error))?;
            let stored = tx.query(
                "SELECT manifest_digest,parent_digest,manifest_json,manifest_bytes FROM runtime.model_observation_manifests WHERE session_id=$1 AND manifest_digest=ANY($2)",
                &[&self.session_id, &digests],
            ).map_err(|error| crate::session_delivery::postgres_error("verify model observation manifests failed", error))?
                .into_iter().map(|row| (row.get::<_, String>(0), (
                    row.get::<_, Option<String>>(1), row.get::<_, String>(2), row.get::<_, i64>(3),
                ))).collect::<HashMap<_, _>>();
            if stored.len() != prepared.manifests.len()
                || prepared.manifests.iter().any(|manifest| {
                    stored.get(manifest.digest.as_str())
                        != Some(&(
                            manifest.parent_digest.clone(),
                            manifest.manifest_json.clone(),
                            manifest.manifest_bytes,
                        ))
                })
            {
                return Err("model observation manifest digest conflict".to_string());
            }
        }
        if !prepared.contents.is_empty() {
            let digests = prepared
                .contents
                .iter()
                .map(|content| content.digest.clone())
                .collect::<Vec<_>>();
            let kinds = prepared
                .contents
                .iter()
                .map(|content| content.kind.clone())
                .collect::<Vec<_>>();
            let content_json = prepared
                .contents
                .iter()
                .map(|content| content.content_json.clone())
                .collect::<Vec<_>>();
            let content_bytes = prepared
                .contents
                .iter()
                .map(|content| content.content_bytes)
                .collect::<Vec<_>>();
            let first_seen_at_ms = prepared
                .contents
                .iter()
                .map(|content| content.first_seen_at_ms)
                .collect::<Vec<_>>();
            tx.execute(
                "INSERT INTO runtime.model_observation_contents(session_id,content_digest,kind,content_json,content_bytes,first_seen_at_ms) SELECT $1,batch.content_digest,batch.kind,batch.content_json,batch.content_bytes,batch.first_seen_at_ms FROM UNNEST($2::text[],$3::text[],$4::text[],$5::bigint[],$6::bigint[]) batch(content_digest,kind,content_json,content_bytes,first_seen_at_ms) ON CONFLICT(session_id,content_digest) DO NOTHING",
                &[&self.session_id, &digests, &kinds, &content_json, &content_bytes, &first_seen_at_ms],
            )
            .map_err(|error| crate::session_delivery::postgres_error("append model observation contents failed", error))?;
            let stored = tx
                .query(
                    "SELECT content_digest,kind,content_json,content_bytes FROM runtime.model_observation_contents WHERE session_id=$1 AND content_digest=ANY($2)",
                    &[&self.session_id, &digests],
                )
                .map_err(|error| crate::session_delivery::postgres_error("verify model observation contents failed", error))?
                .into_iter()
                .map(|row| {
                    (
                        row.get::<_, String>(0),
                        (
                            row.get::<_, String>(1),
                            row.get::<_, String>(2),
                            row.get::<_, i64>(3),
                        ),
                    )
                })
                .collect::<HashMap<_, _>>();
            if stored.len() != prepared.contents.len()
                || prepared.contents.iter().any(|content| {
                    stored.get(content.digest.as_str())
                        != Some(&(
                            content.kind.clone(),
                            content.content_json.clone(),
                            content.content_bytes,
                        ))
                })
            {
                return Err("model observation content digest conflict".to_string());
            }
        }
        let event_ids = prepared
            .rows
            .iter()
            .map(|row| row.record.event.event_id.clone())
            .collect::<Vec<_>>();
        let row_agent_run_ids = prepared
            .rows
            .iter()
            .map(|row| row.agent_run_id.clone())
            .collect::<Vec<_>>();
        let sequences = prepared
            .rows
            .iter()
            .map(|row| i32::try_from(row.record.sequence))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| "session record sequence overflow".to_string())?;
        let agent_run_sequences = prepared
            .rows
            .iter()
            .map(|row| row.agent_run_sequence)
            .collect::<Vec<_>>();
        let session_levels = prepared
            .rows
            .iter()
            .map(|row| row.session_level)
            .collect::<Vec<_>>();
        let projections = prepared
            .rows
            .iter()
            .map(|row| row.projects_to_agent_run_stream)
            .collect::<Vec<_>>();
        let payloads = prepared
            .rows
            .iter()
            .map(|row| row.payload.clone())
            .collect::<Vec<_>>();
        let created_at_ms = prepared
            .rows
            .iter()
            .map(|row| row.record.event.created_at_ms)
            .collect::<Vec<_>>();
        let inserted = tx
            .execute(
                "INSERT INTO app_core_sessionevent(\"eventId\",workspace_id,session_id,agent_run_id,sequence,agent_run_sequence,session_level,projects_to_agent_run_stream,payload,\"createdAtMs\",\"insertedAt\") SELECT batch.event_id,$1,$2,batch.agent_run_id,batch.sequence,batch.agent_run_sequence,batch.session_level,batch.projects_to_agent_run_stream,batch.payload::jsonb,batch.created_at_ms,clock_timestamp() FROM UNNEST($3::text[],$4::text[],$5::integer[],$6::integer[],$7::boolean[],$8::boolean[],$9::text[],$10::bigint[]) batch(event_id,agent_run_id,sequence,agent_run_sequence,session_level,projects_to_agent_run_stream,payload,created_at_ms)",
                &[
                    &self.workspace_id,
                    &self.session_id,
                    &event_ids,
                    &row_agent_run_ids,
                    &sequences,
                    &agent_run_sequences,
                    &session_levels,
                    &projections,
                    &payloads,
                    &created_at_ms,
                ],
            )
            .map_err(|error| crate::session_delivery::postgres_error("append session record batch failed", error))?;
        if inserted != prepared.rows.len() as u64 {
            return Err("append session record batch count mismatch".to_string());
        }
        let commit_event_ids = event_ids
            .iter()
            .map(|event_id| format!("session_record_commit:{event_id}"))
            .collect::<Vec<_>>();
        let commit_payloads = prepared
            .rows
            .iter()
            .map(|row| row.commit_payload.clone())
            .collect::<Vec<_>>();
        let inserted = tx
            .execute(
                "INSERT INTO runtime.runtime_events(event_id,session_id,task_id,event_type,at_ms,visibility,payload_json) SELECT batch.event_id,$1,batch.agent_run_id,'session_record_committed',batch.created_at_ms,'internal',batch.payload_json FROM UNNEST($2::text[],$3::text[],$4::bigint[],$5::text[]) batch(event_id,agent_run_id,created_at_ms,payload_json)",
                &[
                    &self.session_id,
                    &commit_event_ids,
                    &row_agent_run_ids,
                    &created_at_ms,
                    &commit_payloads,
                ],
            )
            .map_err(|error| crate::session_delivery::postgres_error("append session runtime commit batch failed", error))?;
        if inserted != prepared.rows.len() as u64 {
            return Err("append session runtime commit batch count mismatch".to_string());
        }
        Ok(())
    }

    fn prepare_admission_append(
        &self,
        entries: &[AdmissionEntry],
        mut next_session_sequence: i32,
    ) -> Result<PreparedSessionAppend, String> {
        let mut rows = Vec::with_capacity(entries.len());
        for entry in entries {
            centaeris_core::session::validate_event_shape(&entry.event)?;
            next_session_sequence = next_session_sequence
                .checked_add(1)
                .ok_or_else(|| "session record sequence overflow".to_string())?;
            let sequence = u64::try_from(next_session_sequence)
                .map_err(|_| "session record sequence overflow".to_string())?;
            let wire = wire_record_value(&SequencedSessionRecord {
                sequence,
                event: entry.event.clone(),
            })
            .map_err(|error| error.to_string())?;
            let payload = serde_json::to_string(&super::session_payload::encode(&wire)?)
                .map_err(|error| format!("serialize session record failed: {error}"))?;
            let commit_payload = serde_json::json!({
                "sessionRecordId": entry.event.event_id,
                "sessionRecordType": entry.event.event_type.as_str(),
                "sequence": sequence,
                "agentRunSequence": entry.agent_run_sequence,
            })
            .to_string();
            rows.push(PreparedSessionRow {
                record: CommittedSessionRecord {
                    sequence,
                    event: entry.event.clone(),
                },
                agent_run_id: entry.agent_run_id.clone(),
                agent_run_sequence: entry.agent_run_sequence,
                session_level: entry.session_level,
                projects_to_agent_run_stream: session_record_projects_to_agent_run_stream(
                    entry.event.event_type,
                ),
                payload,
                commit_payload,
            });
        }
        Ok(PreparedSessionAppend {
            rows,
            contents: Vec::new(),
            manifests: Vec::new(),
        })
    }

    fn load_admission_idempotent_batch(
        &self,
        tx: &mut Transaction<'_>,
        entries: &[AdmissionEntry],
    ) -> Result<Option<SessionCommitReceipt>, String> {
        let event_ids = entries
            .iter()
            .map(|entry| entry.event.event_id.clone())
            .collect::<Vec<_>>();
        let rows = tx
            .query(
                "SELECT \"eventId\",workspace_id,session_id,agent_run_id,sequence,agent_run_sequence,session_level,payload::text FROM app_core_sessionevent WHERE \"eventId\"=ANY($1)",
                &[&event_ids],
            )
            .map_err(|error| crate::session_delivery::postgres_error("query admission batch ids failed", error))?;
        if rows.is_empty() {
            return Ok(None);
        }
        if rows.len() != entries.len() {
            return Err(
                "new user turn admission batch partially overlaps committed facts".to_string(),
            );
        }
        let mut existing = rows
            .into_iter()
            .map(|row| {
                (
                    row.get::<_, String>(0),
                    (
                        row.get::<_, String>(1),
                        row.get::<_, String>(2),
                        row.get::<_, String>(3),
                        row.get::<_, i32>(4),
                        row.get::<_, Option<i32>>(5),
                        row.get::<_, bool>(6),
                        row.get::<_, String>(7),
                    ),
                )
            })
            .collect::<HashMap<_, _>>();
        let mut records = Vec::with_capacity(entries.len());
        for entry in entries {
            let row = existing
                .remove(entry.event.event_id.as_str())
                .ok_or_else(|| {
                    "new user turn admission batch contains duplicate event ids".to_string()
                })?;
            let mut wire = serde_json::from_str::<serde_json::Value>(row.6.as_str())
                .map_err(|error| format!("decode stored session record failed: {error}"))?;
            super::session_payload::decode(&mut wire)?;
            let stored = parse_wire_record(&wire).map_err(|error| error.to_string())?;
            let sequence = u64::try_from(row.3)
                .map_err(|_| "stored session sequence is invalid".to_string())?;
            let stored_run_sequence = row.4;
            let same = row.0 == self.workspace_id
                && row.1 == self.session_id
                && row.2 == entry.agent_run_id
                && stored_run_sequence == entry.agent_run_sequence
                && row.5 == entry.session_level
                && stored.sequence == sequence
                && stored.event == entry.event;
            if !same {
                return Err(format!(
                    "new user turn admission idempotency conflict: {}",
                    entry.event.event_id
                ));
            }
            records.push(CommittedSessionRecord {
                sequence,
                event: entry.event.clone(),
            });
        }
        Ok(Some(SessionCommitReceipt { records }))
    }

    /// Session-level atomic admission: commits the recovery closures for an
    /// older terminal AgentRun and the new AgentRun's first batch in one
    /// transaction. Ordinary appends stay single-run; only this entry may mix
    /// records owned by different runs.
    pub fn append_new_user_turn_admission_blocking(
        &self,
        new_agent_run_id: &str,
        new_run_events: &[SequencedSessionRecord],
        closure_events: &[centaeris_core::session::SessionLogRecord],
        plan: &centaeris_core::runtime::contracts::NewUserTurnClosurePlanV1,
        fence: &RuntimeJobLeaseFence,
    ) -> Result<SessionCommitReceipt, String> {
        validate_sequenced_session_records(new_run_events)?;
        if plan.session_id != self.session_id || plan.trigger_agent_run_id != new_agent_run_id {
            return Err("new user turn admission plan binding mismatch".to_string());
        }
        if closure_events.len() != plan.closures.len() {
            return Err("new user turn admission closure count mismatch".to_string());
        }
        let _append_guard = self
            .append_lock
            .lock()
            .map_err(|_| "session record append lock poisoned".to_string())?;
        self.connections.with_client(|client| {
            let mut tx = client
                .transaction()
                .map_err(|error| crate::session_delivery::postgres_error("begin session admission failed", error))?;
            let session = tx
                .query_opt(
                    "SELECT workspace_id FROM app_core_session WHERE id=$1 FOR UPDATE",
                    &[&self.session_id],
                )
                .map_err(|error| crate::session_delivery::postgres_error("lock chat session failed", error))?
                .ok_or_else(|| "chat session not found".to_string())?;
            if session.get::<_, String>(0) != self.workspace_id {
                return Err("chat session workspace binding mismatch".to_string());
            }
            let lease_is_current = tx
                .query_opt(
                    "SELECT 1 FROM runtime.runtime_jobs WHERE job_id=$1 AND job_kind=$2 AND status='running' AND lease_owner=$3 AND session_id=$4 AND lease_expires_at_ms>(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint FOR UPDATE",
                    &[&fence.job_id, &fence.job_kind, &fence.lease_owner, &self.session_id],
                )
                .map_err(|error| crate::session_delivery::postgres_error("lock runtime job lease fence failed", error))?
                .is_some();
            if !lease_is_current {
                return Err(RUNTIME_JOB_LEASE_FENCE_REJECTED.to_string());
            }
            let cancelled = tx.query_opt(
                "SELECT 1 FROM app_core_agentrun WHERE id=$1 AND \"preAdmissionCancelledAt\" IS NOT NULL",
                &[&new_agent_run_id],
            ).map_err(|e| e.to_string())?.is_some();
            if cancelled {
                return Err("new user turn admission rejected after cancellation".into());
            }
            self.validate_input_binding(&mut tx, new_agent_run_id)?;
            let mut entries = Vec::with_capacity(closure_events.len() + new_run_events.len());
            for closure_event in closure_events {
                entries.push(AdmissionEntry {
                    event: closure_event.clone(),
                    agent_run_id: closure_event
                        .agent_run_id
                        .clone()
                        .ok_or_else(|| "admission closure has no owning AgentRun".to_string())?,
                    agent_run_sequence: None,
                    session_level: true,
                });
            }
            for item in new_run_events {
                entries.push(AdmissionEntry {
                    event: item.event.clone(),
                    agent_run_id: new_agent_run_id.to_string(),
                    agent_run_sequence: Some(
                        i32::try_from(item.sequence)
                            .map_err(|_| "session record AgentRun sequence overflow".to_string())?,
                    ),
                    session_level: false,
                });
            }
            // Re-delivery of an already committed batch must return the stored
            // receipt before the head-comparison, which necessarily advanced.
            if let Some(receipt) = self.load_admission_idempotent_batch(&mut tx, &entries)? {
                tx.commit()
                    .map_err(|error| crate::session_delivery::postgres_error("commit idempotent admission failed", error))?;
                return Ok(receipt);
            }
            let head = tx
                .query_one(
                    "SELECT COALESCE(MAX(sequence), 0) FROM app_core_sessionevent WHERE session_id=$1",
                    &[&self.session_id],
                )
                .map_err(|error| crate::session_delivery::postgres_error("load session head failed", error))?
                .get::<_, i32>(0);
            if u64::try_from(head).map_err(|_| "session head is invalid".to_string())?
                != plan.expected_session_sequence
            {
                return Err("new user turn admission plan is stale: session head changed".to_string());
            }
            for precondition in &plan.evidence_preconditions {
                if precondition.receipt_present {
                    let receipt_event_id = precondition
                        .receipt_event_id
                        .as_deref()
                        .ok_or_else(|| "admission evidence receipt id is missing".to_string())?;
                    let exists = tx
                        .query_opt(
                            "SELECT 1 FROM runtime.runtime_events WHERE event_id=$1",
                            &[&receipt_event_id],
                        )
                        .map_err(|error| crate::session_delivery::postgres_error("verify admission receipt failed", error))?
                        .is_some();
                    if !exists {
                        return Err("new user turn admission evidence receipt is missing".to_string());
                    }
                } else if let Some(expected) = precondition.expected_receipt_event_id.as_deref() {
                    let appeared = tx
                        .query_opt(
                            "SELECT 1 FROM runtime.runtime_events WHERE event_id=$1",
                            &[&expected],
                        )
                        .map_err(|error| crate::session_delivery::postgres_error("verify admission evidence failed", error))?
                        .is_some();
                    if appeared {
                        return Err(
                            "new user turn admission evidence advanced after planning".to_string()
                        );
                    }
                }
            }
            for (closure_event, closure) in closure_events.iter().zip(plan.closures.iter()) {
                if closure_event.event_type != SessionRecordType::ToolCallClosure
                    || closure_event.session_id != self.session_id
                {
                    return Err("new user turn admission closure identity mismatch".to_string());
                }
                let closure_run_id = closure_event
                    .agent_run_id
                    .as_deref()
                    .ok_or_else(|| "admission closure has no owning AgentRun".to_string())?;
                let payload = closure_event
                    .payload
                    .as_object()
                    .ok_or_else(|| "admission closure payload must be an object".to_string())?;
                if closure_run_id != closure.agent_run_id
                    || payload.get("callId").and_then(serde_json::Value::as_str)
                        != Some(closure.call.id.as_str())
                    || payload.get("recovery").and_then(serde_json::Value::as_str)
                        != Some(closure.recovery.as_str())
                    || payload
                        .get("triggerAgentRunId")
                        .and_then(serde_json::Value::as_str)
                        != Some(new_agent_run_id)
                {
                    return Err(
                        "new user turn admission closure does not match its plan".to_string()
                    );
                }
                let terminal = tx
                    .query_opt(
                        "SELECT 1 FROM app_core_sessionevent WHERE session_id=$1 AND agent_run_id=$2 AND session_level=false AND payload->>'type' IN ('agent_run_completed','agent_run_failed','agent_run_interrupted') LIMIT 1",
                        &[&self.session_id, &closure_run_id],
                    )
                    .map_err(|error| crate::session_delivery::postgres_error("verify closure owner terminal state failed", error))?
                    .is_some();
                if !terminal {
                    return Err(
                        "new user turn admission closure owner is not terminal".to_string()
                    );
                }
            }
            for item in new_run_events {
                if item.event.agent_run_id.as_deref() != Some(new_agent_run_id) {
                    return Err("new user turn admission batch AgentRun mismatch".to_string());
                }
            }
            let prepared = self.prepare_admission_append(&entries, head)?;
            self.insert_session_append(&mut tx, &prepared)?;
            consume_waits_for_terminal_batch(
                &mut tx,
                &self.session_id,
                new_agent_run_id,
                new_run_events,
            )?;
            tx.commit()
                .map_err(|error| crate::session_delivery::postgres_error("commit session admission failed", error))?;
            Ok(SessionCommitReceipt {
                records: prepared.rows.into_iter().map(|row| row.record).collect(),
            })
        })
    }
}

fn consume_waits_for_terminal_batch(
    tx: &mut Transaction<'_>,
    session_id: &str,
    run_id: &str,
    events: &[SequencedSessionRecord],
) -> Result<(), String> {
    if let Some(terminal) = events.iter().find(|item| {
        matches!(
            item.event.event_type,
            SessionRecordType::AgentRunCompleted
                | SessionRecordType::AgentRunFailed
                | SessionRecordType::AgentRunInterrupted
        )
    }) {
        consume_terminal_runtime_job_waits(tx, session_id, run_id, terminal.event.created_at_ms)?;
    }
    Ok(())
}

fn consume_terminal_runtime_job_waits(
    tx: &mut Transaction<'_>,
    session_id: &str,
    run_id: &str,
    at_ms: i64,
) -> Result<(), String> {
    // Index by owner, deduplicate checkpoint IDs before reading their payloads.
    // No session-history scan and no dependency on the source job's outcome.
    let rows = tx.query(
        "SELECT c.checkpoint_id,c.kind,c.session_id,c.turn_id,c.status,c.done_reason,c.updated_at_ms,c.payload_json FROM runtime.checkpoints c WHERE c.checkpoint_id IN (SELECT w.checkpoint_id FROM runtime.runtime_job_waiters w WHERE w.agent_run_id=$1 AND w.session_id=$2) ORDER BY c.checkpoint_id FOR UPDATE OF c",
        &[&run_id, &session_id]).map_err(|e| format!("load terminal wait checkpoints failed: {e}"))?;
    for row in rows {
        let checkpoint = row_to_checkpoint(&row)?;
        let request = centaeris_core::runtime::terminal_wait::abandon_terminal_runtime_job_wait(
            checkpoint, session_id, run_id, at_ms,
        )?;
        for event in &request.events {
            append_runtime_event_idempotent(tx, event)?;
        }
        let deleted = tx
            .execute(
                "DELETE FROM runtime.checkpoints WHERE checkpoint_id=$1 AND kind='wait'",
                &[&request.checkpoint.checkpoint_id],
            )
            .map_err(|e| format!("consume terminal wait failed: {e}"))?;
        if deleted != 1 {
            return Err("terminal wait checkpoint delete mismatch".into());
        }
        // Source relationships are removed by the checkpoint foreign key.
    }
    Ok(())
}

fn apply_batch_to_state<'a>(
    state: &mut AgentRunAppendState,
    session_id: &str,
    events: impl IntoIterator<Item = &'a centaeris_core::session::SessionLogRecord>,
) -> Result<(), String> {
    use centaeris_core::session::SessionRecordType as Type;
    for event in events {
        if state.terminal_seen {
            return Err("session record terminal must be last".to_string());
        }
        if state.first_event_type.is_none() && event.event_type != Type::Tombstone {
            state.first_event_type = Some(event.event_type);
        }
        match event.event_type {
            Type::AgentRunStarted => {
                state.agent_run_started_count += 1;
                if state.agent_run_started_count > 1 {
                    return Err("session record log must contain one agent_run_started".to_string());
                }
            }
            Type::UserMessage => {
                state.user_message_count += 1;
                if state.user_message_count > 1 {
                    return Err("session record log contains duplicate user messages".to_string());
                }
            }
            Type::HostEventInput
                if event
                    .payload
                    .get("inputId")
                    .and_then(serde_json::Value::as_str)
                    == state.initial_host_input_id.as_deref() =>
            {
                state.user_message_count += 1;
                if state.user_message_count > 1 {
                    return Err("session record log contains duplicate initial inputs".into());
                }
            }
            Type::AssistantMessage => {
                state.assistant_message_count += 1;
                if state.user_message_count != 1 {
                    return Err("assistant_message requires one user_message".to_string());
                }
                let status = event
                    .payload
                    .get("status")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                state.last_assistant_status = Some(status.to_string());
            }
            Type::AgentRunCompleted | Type::AgentRunFailed | Type::AgentRunInterrupted => {
                if state.user_message_count != 1 {
                    return Err("terminal session record requires one user message".to_string());
                }
                if matches!(
                    event.event_type,
                    Type::AgentRunFailed | Type::AgentRunInterrupted
                ) && state.assistant_message_count == 0
                {
                    state.terminal_seen = true;
                    continue;
                }
                if state.assistant_message_count == 0 {
                    return Err(
                        "completed or failed session record requires an assistant message"
                            .to_string(),
                    );
                }
                let status_matches_terminal = if event.event_type == Type::AgentRunCompleted {
                    state.last_assistant_status.as_deref() == Some("done")
                } else {
                    matches!(
                        state.last_assistant_status.as_deref(),
                        Some("done" | "error")
                    )
                };
                if !status_matches_terminal {
                    return Err("assistant and terminal session states mismatch".to_string());
                }
                state.terminal_seen = true;
            }
            _ => {}
        }
        reduce_event(
            session_id,
            &mut state.projection,
            &mut state.seen_event_ids,
            &mut state.open_tool_call_ids,
            event,
        )?;
    }
    Ok(())
}

fn load_session_records(
    tx: &mut Transaction<'_>,
    session_id: &str,
) -> Result<Vec<centaeris_core::session::SessionLogRecord>, String> {
    let rows = tx.query(
        "SELECT sequence,payload::text FROM app_core_sessionevent WHERE session_id=$1 ORDER BY sequence",
        &[&session_id],
    )
    .map_err(|error| format!("load rewrite Session records failed: {error}"))?;
    let mut wires = rows
        .iter()
        .map(|row| {
            serde_json::from_str::<serde_json::Value>(row.get::<_, String>(1).as_str())
                .map_err(|error| format!("decode rewrite Session record failed: {error}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    hydrate_session_wire_values(tx, wires.as_mut_slice())?;
    rows.into_iter()
        .zip(wires)
        .enumerate()
        .map(|(index, (row, value))| {
            if row.get::<_, i32>(0)
                != i32::try_from(index + 1)
                    .map_err(|_| "rewrite Session sequence overflow".to_string())?
            {
                return Err("rewrite Session sequence is not contiguous".to_string());
            }
            let record = parse_wire_record(&value)
                .map_err(|error| format!("parse rewrite Session record failed: {error}"))?;
            if record.event.session_id != session_id {
                return Err("rewrite Session record binding mismatch".to_string());
            }
            Ok(record.event)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use centaeris_core::session::{SessionLogRecord, SESSION_EVENT_SCHEMA_VERSION};
    use serde_json::json;

    fn state() -> AgentRunAppendState {
        AgentRunAppendState {
            last_sequence: 0,
            projection: SessionProjection::default(),
            seen_event_ids: HashSet::new(),
            open_tool_call_ids: HashMap::new(),
            agent_run_started_count: 0,
            user_message_count: 0,
            initial_host_input_id: None,
            assistant_message_count: 0,
            last_assistant_status: None,
            terminal_seen: false,
            first_event_type: None,
        }
    }

    fn event(
        event_type: SessionRecordType,
        event_id: &str,
        turn_id: Option<&str>,
        agent_run_id: Option<&str>,
        payload: serde_json::Value,
    ) -> SessionLogRecord {
        SessionLogRecord {
            schema_version: SESSION_EVENT_SCHEMA_VERSION.to_string(),
            event_version: centaeris_core::session::SESSION_EVENT_VERSION,
            event_type,
            event_id: event_id.to_string(),
            session_id: "session_1".to_string(),
            turn_id: turn_id.map(str::to_string),
            agent_run_id: agent_run_id.map(str::to_string),
            created_at_ms: 1,
            payload,
        }
    }

    #[test]
    fn model_request_completion_delivery_storage_round_trips_core_state() {
        let observation = centaeris_core::runtime::ModelObservationV1::RequiredCompletionDelivery {
            state: centaeris_core::runtime::contracts::RequiredCompletionDeliveryV1 {
                schema: "runtime.completion_delivery.v1".into(),
                agent_run_id: "agent_run_delivery".into(),
                authorization_digest: format!("sha256:{}", "a".repeat(64)),
                tool_name: "send_message".into(),
                input_key: format!("sha256:{}", "b".repeat(64)),
                turn_ids: vec!["turn_delivery".into()],
                delivered_call_id: None,
                repair_attempted: false,
                draft: None,
            },
        };
        let mut stored = json!({"type":"model_request_started", "sessionId":"session_1",
            "payload":{"purpose":"main", "requestId":"request_delivery", "observations":[observation]}});
        let original = stored.clone();
        let (unique, manifest, references) =
            compact_model_request_wire(&mut stored, 1, None, &[]).unwrap();
        let decoded = decode_model_observation_manifest(
            manifest.digest.clone(),
            manifest.parent_digest.clone(),
            manifest.manifest_json.clone(),
            manifest.manifest_bytes,
            manifest.first_seen_at_ms,
        )
        .unwrap();
        assert_eq!(
            apply_model_observation_manifest(&decoded, Vec::new()).unwrap(),
            references
        );
        let contents = unique
            .into_iter()
            .map(|content| (("session_1".to_string(), content.digest.clone()), content))
            .collect();
        hydrate_model_request_wire(&mut stored, &references, &contents).unwrap();
        assert_eq!(stored, original);
    }

    #[test]
    fn model_request_input_uptake_storage_round_trips_exact_input_ids() {
        let mut stored = json!({"type":"model_request_started", "sessionId":"session_1",
            "payload":{"purpose":"main", "requestId":"request_uptake", "observations":[
                {"kind":"input_uptake", "inputIds":["original-input", "active-input"]}]}});
        let original = stored.clone();
        let (unique, manifest, references) =
            compact_model_request_wire(&mut stored, 1, None, &[]).unwrap();
        let decoded = decode_model_observation_manifest(
            manifest.digest.clone(),
            manifest.parent_digest.clone(),
            manifest.manifest_json.clone(),
            manifest.manifest_bytes,
            manifest.first_seen_at_ms,
        )
        .unwrap();
        assert_eq!(
            apply_model_observation_manifest(&decoded, Vec::new()).unwrap(),
            references
        );
        let contents = unique
            .into_iter()
            .map(|content| (("session_1".to_string(), content.digest.clone()), content))
            .collect::<HashMap<_, _>>();
        hydrate_model_request_wire(&mut stored, &references, &contents).unwrap();
        assert_eq!(stored, original);
    }

    #[test]
    fn model_request_storage_manifest_round_trips_without_exposing_content() {
        let mut stored = json!({
            "type": "model_request_started",
            "sessionId": "session_1",
            "payload": {
                "requestId": "request_1",
                "observations": [
                    {"kind": "system_prompt", "content": "secret system prompt"},
                    {"kind": "message", "message": {
                        "messageId": "message_1",
                        "role": "user",
                        "content": "secret context"
                    }}
                ]
            }
        });
        let original = stored.clone();
        let (unique, manifest, references) =
            compact_model_request_wire(&mut stored, 1, None, &[]).expect("compact request");

        let encoded = stored.to_string();
        assert!(!encoded.contains("secret system prompt"));
        assert!(!encoded.contains("secret context"));
        assert_eq!(
            stored["payload"]["observations"],
            json!({"manifestDigest": manifest.digest})
        );
        assert_eq!(
            apply_model_observation_manifest(&manifest, Vec::new()).unwrap(),
            references
        );

        let contents = unique
            .into_iter()
            .map(|content| (("session_1".to_string(), content.digest.clone()), content))
            .collect::<HashMap<_, _>>();
        hydrate_model_request_wire(&mut stored, &references, &contents).expect("hydrate request");
        assert_eq!(stored, original);
    }

    #[test]
    fn model_request_storage_rejects_digest_kind_conflicts() {
        let mut stored = json!({
            "type": "model_request_started",
            "sessionId": "session_1",
            "payload": {
                "observations": [{"kind": "system_prompt", "content": "prompt"}]
            }
        });
        let (mut unique, _, references) =
            compact_model_request_wire(&mut stored, 1, None, &[]).expect("compact request");
        let mut content = unique.remove(0);
        content.kind = "tool_catalog".to_string();
        let contents =
            HashMap::from([(("session_1".to_string(), content.digest.clone()), content)]);

        assert!(
            hydrate_model_request_wire(&mut stored, &references, &contents)
                .expect_err("kind conflict must fail")
                .contains("binding mismatch")
        );
    }

    #[test]
    #[ignore = "performance/release gate"]
    fn model_observation_manifest_growth_and_validation_through_4095_observations() {
        let mut contents = BTreeMap::new();
        let mut manifests = HashMap::new();
        let mut observations = vec![
            json!({"kind": "system_prompt", "content": "stable system"}),
            json!({"kind": "message", "message": {"messageId": "runtime-context", "role": "user", "content": "runtime context 0"}}),
            json!({"kind": "message", "message": {"messageId": "stable-context", "role": "user", "content": "stable prior context"}}),
        ];
        let mut references = observations
            .iter()
            .map(|value| compact_model_observation(value, 0).unwrap().1)
            .collect::<Vec<_>>();
        for index in [0, 2] {
            let (content, _) = compact_model_observation(&observations[index], 0).unwrap();
            contents.insert(content.digest.clone(), content);
        }
        let mut parent = Vec::new();
        let mut parent_digest = None;
        let mut legacy_refs = 0usize;
        let mut root_bytes = 0usize;
        let mut submitted_cas_content_bytes = 0usize;
        let mut curve = Vec::new();
        for round in 1..=2046usize {
            observations[1] = json!({"kind": "message", "message": {
                "messageId": "runtime-context", "role": "user", "content": format!("runtime context {round}")
            }});
            let (content, reference) =
                compact_model_observation(&observations[1], round as i64).unwrap();
            contents.insert(content.digest.clone(), content);
            references[1] = reference;
            for role in ["user", "assistant"] {
                let observation = json!({"kind": "message", "message": {
                    "messageId": format!("{round}-{role}"), "role": role,
                    "content": format!("{round}:{role}:{}", "m".repeat(64)),
                }});
                let (content, reference) =
                    compact_model_observation(&observation, round as i64).unwrap();
                contents.insert(content.digest.clone(), content);
                references.push(reference);
                observations.push(observation);
            }
            let mut wire = json!({"type": "model_request_started", "sessionId": "session-growth",
                "payload": {"requestId": format!("request-{round}"), "observations": observations}});
            let (new_contents, manifest, prepared_references) =
                compact_model_request_wire(&mut wire, round as i64, parent_digest, &parent)
                    .unwrap();
            assert_eq!(prepared_references, references);
            assert_eq!(new_contents.len(), if round == 1 { 5 } else { 3 });
            submitted_cas_content_bytes += new_contents
                .iter()
                .map(|content| content.content_json.len())
                .sum::<usize>();
            assert_eq!(manifest.changes.len(), if round == 1 { 5 } else { 3 });
            let stored = decode_model_observation_manifest(
                manifest.digest.clone(),
                manifest.parent_digest.clone(),
                manifest.manifest_json.clone(),
                manifest.manifest_bytes,
                manifest.first_seen_at_ms,
            )
            .unwrap();
            assert_eq!(stored, manifest);
            let wire_bytes = wire.to_string().len();
            assert!(wire_bytes < 240);
            root_bytes += wire_bytes;
            legacy_refs += references.len();
            parent_digest = Some(manifest.digest.clone());
            manifests.insert(
                ("session-growth".to_string(), manifest.digest.clone()),
                manifest,
            );
            parent = references.clone();
            if [20, 81, 512, 2046].contains(&round) {
                let resolved = resolve_model_observation_manifest(
                    "session-growth",
                    parent_digest.as_deref().unwrap(),
                    &manifests,
                    &mut HashMap::new(),
                )
                .unwrap();
                assert_eq!(resolved, references);
                let content_map = contents
                    .values()
                    .cloned()
                    .map(|content| {
                        (
                            ("session-growth".to_string(), content.digest.clone()),
                            content,
                        )
                    })
                    .collect();
                let mut hydrated = wire;
                hydrate_model_request_wire(&mut hydrated, &resolved, &content_map).unwrap();
                assert_eq!(hydrated["payload"]["observations"], json!(observations));
                let manifest_refs = manifests
                    .values()
                    .map(|manifest| manifest.changes.len())
                    .sum::<usize>();
                let manifest_bytes = manifests
                    .values()
                    .map(|manifest| manifest.manifest_json.len())
                    .sum::<usize>();
                let content_bytes = contents
                    .values()
                    .map(|content| content.content_json.len())
                    .sum::<usize>();
                assert_eq!(manifest_refs, 3 * round + 2);
                assert_eq!(contents.len(), 3 * round + 2);
                assert_eq!(submitted_cas_content_bytes, content_bytes);
                curve.push(json!({"rounds": round, "observationCount": references.len(),
                    "legacyFullRefs": legacy_refs, "manifestRefs": manifest_refs, "manifestNodes": manifests.len(),
                    "manifestBytes": manifest_bytes, "uniqueContents": contents.len(), "contentBytes": content_bytes,
                    "submittedCasContentBytes": submitted_cas_content_bytes,
                    "eventRootBytes": root_bytes, "physicalPayloadBytes": manifest_bytes + content_bytes + root_bytes}));
            }
        }
        assert!(resolve_model_observation_manifest(
            "session-other",
            parent_digest.as_deref().unwrap(),
            &manifests,
            &mut HashMap::new()
        )
        .is_err());
        let small = curve[1]["physicalPayloadBytes"].as_u64().unwrap();
        let large = curve[3]["physicalPayloadBytes"].as_u64().unwrap();
        assert!(large * 81 * 100 < small * 2046 * 115);
        println!(
            "RUNTIME_01_ARTIFACT {}",
            json!({"gate": "postgres_manifest_linear_growth",
            "measurement": "production_serialization_synthetic_workload_not_database_io",
            "workload": "early_runtime_context_replaced_and_two_tail_observations_appended_per_round", "curve": curve})
        );
    }

    #[test]
    fn model_observation_manifest_rejects_malformed_deltas_and_missing_ancestors() {
        let (_, reference) =
            compact_model_observation(&json!({"kind": "system_prompt", "content": "prompt"}), 0)
                .unwrap();
        let root = build_model_observation_manifest(None, &[], std::slice::from_ref(&reference), 0)
            .unwrap();
        let child = build_model_observation_manifest(
            Some(root.digest.clone()),
            std::slice::from_ref(&reference),
            &[],
            0,
        )
        .unwrap();
        assert!(
            apply_model_observation_manifest(&child, vec![reference.clone()])
                .unwrap()
                .is_empty()
        );
        let mut manifests =
            HashMap::from([(("session".to_string(), child.digest.clone()), child.clone())]);
        assert!(resolve_model_observation_manifest(
            "session",
            &child.digest,
            &manifests,
            &mut HashMap::new()
        )
        .unwrap_err()
        .contains("missing"));
        let mut cyclic_root = root.clone();
        cyclic_root.parent_digest = Some(child.digest.clone());
        manifests.insert(("session".to_string(), root.digest.clone()), cyclic_root);
        assert!(resolve_model_observation_manifest(
            "session",
            &child.digest,
            &manifests,
            &mut HashMap::new()
        )
        .unwrap_err()
        .contains("cycle"));
        let change =
            json!({"index": 0, "kind": reference.kind, "contentDigest": reference.content_digest});
        for invalid in [
            json!({"parentDigest": null, "observationCount": 2, "changes": [change.clone(), change.clone()]}),
            json!({"parentDigest": null, "observationCount": 0, "changes": [change.clone()]}),
            json!({"parentDigest": null, "observationCount": 1, "changes": [change], "extra": true}),
        ] {
            let raw = invalid.to_string();
            assert!(decode_model_observation_manifest(
                sha256_json(MODEL_OBSERVATION_MANIFEST_DIGEST_DOMAIN, &raw),
                None,
                raw.clone(),
                raw.len() as i64,
                0
            )
            .is_err());
        }
        assert!(decode_model_observation_manifest(
            root.digest.clone(),
            None,
            "{}".to_string(),
            2,
            0
        )
        .unwrap_err()
        .contains("digest mismatch"));
        let mut hole = child.clone();
        hole.observation_count = 2;
        assert!(
            apply_model_observation_manifest(&hole, vec![reference.clone()])
                .unwrap_err()
                .contains("count mismatch")
        );
        hole.observation_count = 3;
        hole.changes = vec![ModelObservationChange {
            index: 2,
            reference: reference.clone(),
        }];
        assert!(
            apply_model_observation_manifest(&hole, vec![reference.clone()])
                .unwrap_err()
                .contains("hole")
        );
        let mut redundant = build_model_observation_manifest(
            Some(root.digest),
            std::slice::from_ref(&reference),
            std::slice::from_ref(&reference),
            0,
        )
        .unwrap();
        redundant.changes = vec![ModelObservationChange {
            index: 0,
            reference: reference.clone(),
        }];
        assert!(
            apply_model_observation_manifest(&redundant, vec![reference])
                .unwrap_err()
                .contains("redundant")
        );
    }

    #[test]
    fn twenty_round_model_request_storage_amplification_is_quantified() {
        fn bytes(value: &serde_json::Value) -> usize {
            serde_json::to_vec(value)
                .expect("encode metric value")
                .len()
        }
        fn commit_bytes(event_id: &str, event_type: &str, sequence: usize) -> usize {
            bytes(&json!({
                "sessionRecordId": event_id,
                "sessionRecordType": event_type,
                "sequence": sequence,
                "agentRunSequence": sequence,
            }))
        }

        let system_prompt = "s".repeat(16 * 1024);
        let tool_catalog = json!({
            "kind": "tool_catalog",
            "toolDefinitions": [{
                "name": "search_laws",
                "description": "d".repeat(12 * 1024),
                "inputSchema": {"type": "object"}
            }]
        });
        let mut context = Vec::new();
        let mut unique_contents = BTreeMap::<String, ModelObservationContent>::new();
        let mut old_logical_events = 0usize;
        let mut new_logical_events = 0usize;
        let mut old_payload_bytes = 0usize;
        let mut new_payload_bytes = 0usize;
        let mut parent_digest = None;
        let mut parent_references = Vec::new();
        let mut manifest_bytes = 0usize;
        let mut manifest_refs = 0usize;
        let mut submitted_cas_content_bytes = 0usize;
        for round in 1..=20usize {
            for role in ["user", "assistant"] {
                context.push(json!({
                    "kind": "message",
                    "message": {
                        "messageId": format!("message_{round}_{role}"),
                        "role": role,
                        "content": format!("{round}:{role}:{}", "m".repeat(512))
                    }
                }));
            }
            let observations = std::iter::once(json!({
                "kind": "system_prompt",
                "content": system_prompt.clone(),
            }))
            .chain(context.iter().cloned())
            .chain(std::iter::once(tool_catalog.clone()))
            .collect::<Vec<_>>();
            let request_id = format!("request_{round}");
            let mut old_sequence = old_logical_events + 1;
            for (index, observation) in observations.iter().enumerate() {
                let mut payload = observation.as_object().expect("observation object").clone();
                payload.insert(
                    "observationId".to_string(),
                    json!(format!("{request_id}:observation:{index}")),
                );
                let event_id = format!("event_{request_id}_observation_{index}");
                old_payload_bytes += bytes(&json!({
                    "type": "model_observation",
                    "eventId": event_id,
                    "sessionId": "session_1",
                    "payload": payload,
                }));
                old_payload_bytes += commit_bytes(&event_id, "model_observation", old_sequence);
                old_sequence += 1;
            }
            let old_request_id = format!("event_{request_id}");
            let old_request = json!({
                "type": "model_request_started",
                "eventId": old_request_id,
                "sessionId": "session_1",
                "payload": {"requestId": request_id}
            });
            let checkpoint_id = format!("event_checkpoint_{round}");
            let checkpoint = json!({
                "type": "checkpoint_ref",
                "eventId": checkpoint_id,
                "sessionId": "session_1",
                "payload": {"checkpointId": format!("checkpoint_{round}")}
            });
            old_payload_bytes += bytes(&old_request)
                + commit_bytes(&old_request_id, "model_request_started", old_sequence);
            old_payload_bytes += bytes(&checkpoint)
                + commit_bytes(&checkpoint_id, "checkpoint_ref", old_sequence + 1);
            old_logical_events += observations.len() + 2;

            let new_request_id = format!("event_request_{round}");
            let mut new_request = json!({
                "type": "model_request_started",
                "eventId": new_request_id,
                "sessionId": "session_1",
                "payload": {
                    "requestId": format!("request_{round}"),
                    "observations": observations,
                }
            });
            let (contents, manifest, references) = compact_model_request_wire(
                &mut new_request,
                round as i64,
                parent_digest,
                &parent_references,
            )
            .expect("compact");
            manifest_bytes += manifest.manifest_json.len();
            manifest_refs += manifest.changes.len();
            parent_digest = Some(manifest.digest);
            parent_references = references;
            submitted_cas_content_bytes += contents
                .iter()
                .map(|content| content.content_json.len())
                .sum::<usize>();
            for content in contents {
                unique_contents
                    .entry(content.digest.clone())
                    .or_insert(content);
            }
            let new_sequence = new_logical_events + 1;
            new_payload_bytes += bytes(&new_request)
                + commit_bytes(&new_request_id, "model_request_started", new_sequence);
            new_payload_bytes += bytes(&checkpoint)
                + commit_bytes(&checkpoint_id, "checkpoint_ref", new_sequence + 1);
            new_logical_events += 2;
        }
        new_payload_bytes += unique_contents
            .values()
            .map(|content| content.content_json.len())
            .sum::<usize>();
        new_payload_bytes += manifest_bytes;
        let old_physical_rows = old_logical_events * 2;
        let new_physical_rows = new_logical_events * 2 + unique_contents.len() + 20;
        let audited_42_request_old_model_records = 2_161usize;
        let audited_42_request_new_model_records = 42usize;

        println!(
            "RUNTIME_01_ARTIFACT {}",
            json!({
                "gate": "postgres_storage_projection_20_round",
                "measurement": "production_serialization_synthetic_workload_not_database_io",
                "rounds": 20,
                "oldLogicalEvents": old_logical_events, "newLogicalEvents": new_logical_events,
                "unchangedCheckpointEvents": 20,
                "oldModelEvents": old_logical_events - 20, "newModelEvents": new_logical_events - 20,
                "uniqueContents": unique_contents.len(), "manifestNodes": 20, "manifestRefs": manifest_refs,
                "submittedCasContentBytes": submitted_cas_content_bytes,
                "oldPhysicalRows": old_physical_rows, "newPhysicalRows": new_physical_rows,
                "oldPayloadBytes": old_payload_bytes, "newPayloadBytes": new_payload_bytes,
            })
        );
        assert_eq!(old_logical_events, 500);
        assert_eq!(new_logical_events, 40);
        assert_eq!(unique_contents.len(), 42);
        assert_eq!(old_physical_rows, 1_000);
        assert_eq!(new_physical_rows, 142);
        assert!(new_payload_bytes * 5 < old_payload_bytes);
        assert!(
            audited_42_request_new_model_records * 100 < audited_42_request_old_model_records * 2
        );
    }

    #[test]
    fn accepts_failed_agent_run_without_assistant_message() {
        let events = [
            event(
                SessionRecordType::AgentRunStarted,
                "event_1",
                Some("turn_1"),
                Some("task_1"),
                json!({"userObjective": "test"}),
            ),
            event(
                SessionRecordType::UserMessage,
                "event_2",
                Some("turn_1"),
                None,
                json!({"messageId": "message_1", "text": "test", "attachments": []}),
            ),
            event(
                SessionRecordType::AgentRunFailed,
                "event_3",
                Some("turn_1"),
                None,
                json!({"reasonType": "runtime_error", "message": "sandbox unavailable"}),
            ),
        ];
        let mut state = state();

        apply_batch_to_state(&mut state, "session_1", events.iter()).expect("failed turn");

        assert!(state.terminal_seen);
    }

    #[test]
    fn accepts_workspace_finalization_failure_after_sealed_assistant() {
        let events = [
            event(
                SessionRecordType::AgentRunStarted,
                "event_1",
                Some("turn_1"),
                Some("task_1"),
                json!({"userObjective": "test"}),
            ),
            event(
                SessionRecordType::UserMessage,
                "event_2",
                Some("turn_1"),
                None,
                json!({"messageId": "message_1", "text": "test", "attachments": []}),
            ),
            event(
                SessionRecordType::AssistantMessage,
                "event_3",
                Some("turn_1"),
                Some("task_1"),
                json!({
                    "messageId": "message:turn_1:assistant",
                    "modelMarkdown": "completed text",
                    "artifactRefs": [],
                    "status": "done",
                }),
            ),
            event(
                SessionRecordType::AgentRunFailed,
                "event_4",
                Some("turn_1"),
                Some("task_1"),
                json!({"reasonType": "runtime_error", "message": "snapshot collect failed"}),
            ),
        ];
        let mut state = state();

        apply_batch_to_state(&mut state, "session_1", events.iter())
            .expect("workspace finalization failure after sealed text");

        assert!(state.terminal_seen);
        assert_eq!(state.last_assistant_status.as_deref(), Some("done"));
    }
}
