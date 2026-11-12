//! Interactive Session erasure follows durable execution and release receipts.
use std::collections::{BTreeSet, VecDeque};

use centaeris_core::runtime::subagent::{
    subagent_work_packet_runtime_binding, SubagentWorkPacketEnvelope, SUBAGENT_RUN_JOB_KIND,
};
use centaeris_core::session::reliability::RuntimeJobRecord;
use postgres::{GenericClient, Transaction};
use serde::Deserialize;
use serde_json::Value;

use super::super::residency::retirement::RetirementRequest;
use super::super::PostgresRuntimeStore;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SessionPurgeRequest {
    pub schema: String,
    pub session_id: String,
    pub workspace_id: String,
    pub owner_id: i64,
}

impl SessionPurgeRequest {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.schema != "runtime.session.purge.v1"
            || self.session_id.trim().is_empty()
            || self.workspace_id.trim().is_empty()
            || self.owner_id <= 0
        {
            return Err("session_purge_request_invalid".into());
        }
        Ok(())
    }
}

fn child_session(
    job: &RuntimeJobRecord,
    content_json: Value,
    root_session_id: &str,
) -> Result<String, String> {
    let envelope = SubagentWorkPacketEnvelope {
        ref_id: job.payload_ref.clone().unwrap_or_default(),
        content_json,
    };
    let binding = subagent_work_packet_runtime_binding(&envelope, job)?;
    let packet = envelope
        .content_json
        .get("workPacket")
        .unwrap_or(&envelope.content_json);
    if packet
        .get("run_context")
        .and_then(|context| context.get("sessionId"))
        .and_then(Value::as_str)
        != Some(root_session_id)
        || binding.child_session_id == root_session_id
    {
        return Err("session_purge_child_binding_rejected".into());
    }
    Ok(binding.child_session_id)
}

/// Inspect the entire tree before erasing anything. The root Session lock is also
/// the dispatch boundary; job locks settle any already-committing job transition.
fn collect_session_tree(
    tx: &mut Transaction<'_>,
    root_session_id: &str,
) -> Result<(BTreeSet<String>, BTreeSet<String>), String> {
    let mut sessions = BTreeSet::from([root_session_id.to_string()]);
    let mut pending = VecDeque::from([root_session_id.to_string()]);
    let mut object_ids = BTreeSet::new();
    while let Some(session_id) = pending.pop_front() {
        let jobs = tx
            .query(
                "SELECT job_id FROM runtime.runtime_jobs WHERE session_id=$1 ORDER BY job_id FOR UPDATE",
                &[&session_id],
            )
            .map_err(|error| format!("lock Session purge jobs failed: {error}"))?;
        for row in jobs {
            let job_id: String = row.get(0);
            let job = super::super::reliability::load_job(tx, &job_id)?
                .ok_or("session_purge_job_missing")?;
            if !job.status.is_terminal()
                || (job.job_kind
                    == centaeris_core::session::reliability::AGENT_RUN_LIFECYCLE_JOB_KIND
                    && job.status
                        != centaeris_core::session::reliability::RuntimeJobStatus::Succeeded)
            {
                return Err("session_purge_liabilities_pending".into());
            }
            object_ids.extend(job.output_refs.iter().cloned());
            if job.job_kind != SUBAGENT_RUN_JOB_KIND {
                continue;
            }
            let reference = job
                .payload_ref
                .as_deref()
                .ok_or("session_purge_work_packet_missing")?;
            let content = match serde_json::from_str::<Value>(reference) {
                Ok(value) => value,
                Err(_) => {
                    object_ids.insert(reference.to_string());
                    let row = tx
                        .query_opt(
                            "SELECT content FROM runtime.external_context_objects WHERE object_id=$1",
                            &[&reference],
                        )
                        .map_err(|error| format!("load Session purge WorkPacket failed: {error}"))?
                        .ok_or("session_purge_work_packet_missing")?;
                    serde_json::from_str::<Value>(&row.get::<_, String>(0)).map_err(|error| {
                        format!("decode Session purge WorkPacket failed: {error}")
                    })?
                }
            };
            let child = child_session(&job, content, root_session_id)?;
            if sessions.insert(child.clone()) {
                // A child runtime identity never grants erasure of another
                // platform Session, even if a malformed packet names one.
                if tx
                    .query_opt(
                        "SELECT id FROM public.app_core_session WHERE id=$1",
                        &[&child],
                    )
                    .map_err(|error| format!("check Session purge child scope failed: {error}"))?
                    .is_some()
                {
                    return Err("session_purge_child_scope_rejected".into());
                }
                pending.push_back(child);
            }
        }
    }
    Ok((sessions, object_ids))
}

fn require_released_resources(tx: &mut impl GenericClient, session_id: &str) -> Result<(), String> {
    let pending: bool = tx.query_one(
        r#"SELECT
          EXISTS(SELECT 1 FROM public.app_core_agentrun r WHERE r.session_id=$1 AND r.status IN('queued','running'))
          OR EXISTS(SELECT 1 FROM public.app_core_sessionenvironment WHERE session_id=$1 AND NOT controller_released)
          OR EXISTS(SELECT 1 FROM public.app_core_agentrun r WHERE r.session_id=$1
           AND NOT (r."preAdmissionCancelledAt" IS NOT NULL AND NOT EXISTS(
            SELECT 1 FROM runtime.execution_instances i WHERE i.agent_run_id=r.id))
           AND NOT EXISTS(SELECT 1 FROM runtime.agent_run_cleanup_receipts c
            JOIN public.app_core_agentrunenvironmentrelease p
             ON p.agent_run_id=c.agent_run_id AND p.environment_id=c.environment_id
              AND p.authorization_digest=c.authorization_digest
            JOIN public.app_core_agentrunauthorization g ON g.agent_run_id=r.id AND g.digest=c.authorization_digest
            WHERE c.agent_run_id=r.id AND c.environment_id=r.environment_id
             AND c.cleanup_complete
             AND p.controller_epoch>=c.controller_epoch))"#,
        &[&session_id],
    ).map_err(|error| format!("inspect Session purge release receipts failed: {error}"))?.get(0);
    if pending {
        return Err("session_purge_liabilities_pending".into());
    }
    Ok(())
}

fn require_settled_resources(tx: &mut impl GenericClient, session_id: &str) -> Result<(), String> {
    require_released_resources(tx, session_id)?;
    let pending: bool = tx.query_one(
        r#"SELECT
          EXISTS(SELECT 1 FROM runtime.resident_sandboxes WHERE environment_id IN (
           SELECT id FROM public.app_core_sessionenvironment WHERE session_id=$1))
          OR EXISTS(SELECT 1 FROM runtime.execution_instances i
           WHERE i.session_id=$1 AND i.allocation_execution_id=i.execution_id
            AND NOT EXISTS(SELECT 1 FROM runtime.environment_creation_attempts a
             WHERE a.allocation_execution_id=i.execution_id AND a.instance_id=i.instance_id AND a.state='notDispatched')
            AND NOT EXISTS(SELECT 1 FROM runtime.runtime_jobs j
             WHERE j.job_id='environment.host_retire:'||i.instance_id AND j.idempotency_key=i.instance_id AND j.status='succeeded'))
          OR EXISTS(SELECT 1 FROM public.app_core_agentrun r
           JOIN runtime.agent_run_cleanup_receipts c ON c.agent_run_id=r.id AND c.environment_id=r.environment_id
           WHERE r.session_id=$1 AND c.allocation_kind='instance' AND NOT c.physical_removed
            AND NOT EXISTS(SELECT 1 FROM runtime.execution_instances i
             JOIN runtime.environment_retirements t
              ON t.allocation_execution_id=i.allocation_execution_id AND t.instance_id=i.instance_id
               AND t.environment_id=i.environment_id AND t.host_id=i.host_id
             WHERE i.agent_run_id=r.id AND i.session_id=r.session_id AND i.environment_id=r.environment_id
              AND i.instance_id=c.instance_id AND t.state='destroyed'
              AND EXISTS(SELECT 1 FROM runtime.runtime_jobs j WHERE j.job_id='environment.host_retire:'||i.instance_id
               AND j.idempotency_key=i.instance_id AND j.status='succeeded')))"#,
        &[&session_id],
    ).map_err(|error| format!("inspect Session purge physical receipts failed: {error}"))?.get(0);
    if pending {
        return Err("session_purge_liabilities_pending".into());
    }
    Ok(())
}

fn lock_purge_scope(tx: &mut Transaction<'_>, request: &SessionPurgeRequest) -> Result<(), String> {
    tx.query_opt(
        "SELECT id FROM public.app_core_workspace WHERE id=$1 AND status='active' FOR UPDATE",
        &[&request.workspace_id],
    )
    .map_err(|error| format!("lock Session purge Workspace failed: {error}"))?
    .ok_or("session_purge_scope_rejected")?;
    tx.query_opt(
        "SELECT id FROM public.app_core_workspacemembership WHERE workspace_id=$1 AND user_id::bigint=$2 FOR UPDATE",
        &[&request.workspace_id, &request.owner_id],
    ).map_err(|error| format!("lock Session purge membership failed: {error}"))?
        .ok_or("session_purge_scope_rejected")?;
    tx.query_opt(
        "SELECT id FROM public.app_core_session WHERE id=$1 AND workspace_id=$2 AND owner_id::bigint=$3 AND purpose='interactive' AND status IN('active','deleted') AND \"purgeRequestedAt\" IS NOT NULL FOR UPDATE",
        &[&request.session_id, &request.workspace_id, &request.owner_id],
    ).map_err(|error| format!("lock Session purge ownership failed: {error}"))?
        .ok_or("session_purge_scope_rejected")?;
    Ok(())
}

fn erased_capture_event(mut wire: Value) -> Result<Value, String> {
    crate::postgres_store::decode_session_payload(&mut wire)?;
    if wire.get("type").and_then(Value::as_str) != Some("tool_result") {
        return Err("session_purge_capture_event_rejected".into());
    }
    let payload = &wire["payload"];
    wire["payload"] = serde_json::json!({
        "callId":payload["callId"],"toolName":payload["toolName"],
        "resultState":payload["resultState"],"latencyMs":payload["latencyMs"],
        "modelContent":"","fullOutputPath":null,"outputStartByte":null,
        "outputByteLength":0,"outputComplete":true,"summary":"purged",
        "operations":[],"modelInputImages":[],
    });
    centaeris_core::session::parse_wire_record(&wire).map_err(|error| error.to_string())?;
    Ok(wire)
}

fn erase_platform_body(tx: &mut Transaction<'_>, session_id: &str) -> Result<(), String> {
    // A capture still being uploaded may hold this event's FK KEY SHARE before
    // taking the Session publication lock. Retain the target and update only its
    // body, so that uploader can reach the final owner check and roll back.
    let captured = tx.query(
        "SELECT e.\"eventId\",e.payload::text FROM public.app_core_sessionevent e WHERE e.session_id=$1 AND e.payload->>'type'='tool_result'",
        &[&session_id],
    ).map_err(|error| format!("lock Session capture tombstones failed: {error}"))?;
    for row in captured {
        let event_id: String = row.get(0);
        let wire = serde_json::from_str(&row.get::<_, String>(1))
            .map_err(|error| format!("decode Session capture tombstone failed: {error}"))?;
        let wire = erased_capture_event(wire)?.to_string();
        tx.execute(
            "UPDATE public.app_core_sessionevent SET projects_to_agent_run_stream=false,payload=$2::text::jsonb WHERE \"eventId\"=$1",
            &[&event_id,&wire],
        ).map_err(|error| format!("erase Session capture preview failed: {error}"))?;
    }
    for sql in [
        "DELETE FROM public.app_core_transcriptoutputchunk WHERE capture_id IN(SELECT \"eventId\" FROM public.app_core_sessionevent WHERE session_id=$1)",
        "UPDATE public.app_core_transcriptoutputcapture SET \"purgedAt\"=COALESCE(\"purgedAt\",transaction_timestamp()) WHERE event_id IN(SELECT \"eventId\" FROM public.app_core_sessionevent WHERE session_id=$1)",
        "DELETE FROM public.app_core_materialresultsnapshot WHERE call_id IN(SELECT \"eventId\" FROM public.app_core_sessionevent WHERE session_id=$1)",
        "DELETE FROM public.app_core_materialevidencereceipt WHERE call_id IN(SELECT \"eventId\" FROM public.app_core_sessionevent WHERE session_id=$1)",
        "UPDATE public.app_core_runmaterialadmission SET origin_call_id=NULL WHERE origin_call_id IN(SELECT \"eventId\" FROM public.app_core_sessionevent WHERE session_id=$1)",
        "DELETE FROM public.app_core_sessioncitationprojection WHERE session_id=$1",
        "DELETE FROM public.app_core_sessionevent e WHERE session_id=$1 AND payload->>'type' NOT IN('agent_run_completed','agent_run_failed','agent_run_interrupted','tool_result')",
        "UPDATE public.app_core_sessionevent SET payload=jsonb_set(payload-'__centaerisSessionStorage','{payload,message}','\"purged\"'::jsonb,false) WHERE session_id=$1 AND payload->>'type' IN('agent_run_failed','agent_run_interrupted')",
        "UPDATE public.app_core_agentrun SET prompt='',agent_instructions='' WHERE session_id=$1",
        "UPDATE public.app_core_modelrunlog SET error='' WHERE \"agentRunId\" IN(SELECT id FROM public.app_core_agentrun WHERE session_id=$1)",
    ] {
        tx.execute(sql, &[&session_id])
            .map_err(|error| format!("erase platform Session body failed: {error}"))?;
    }
    Ok(())
}

impl PostgresRuntimeStore {
    /// Freeze only already-released retained allocations. The existing retirement
    /// protocol persists this exact request and fences a newly acquired controller.
    pub(crate) fn preflight_interactive_session_purge(
        &self,
        request: &SessionPurgeRequest,
    ) -> Result<Vec<RetirementRequest>, String> {
        request.validate()?;
        self.with_client(|client| {
            let mut tx=client.transaction().map_err(|error|error.to_string())?;
            lock_purge_scope(&mut tx,request)?;
            require_released_resources(&mut tx,&request.session_id)?;
            collect_session_tree(&mut tx,&request.session_id)?;
            let rows=tx.query(
                "SELECT s.environment_id,s.instance_id,s.host_id,e.controller_epoch,s.execution_id,s.state,COALESCE(i.allocation_mode='retained' AND s.allocation_mode='retained' AND i.allocation_execution_id=i.execution_id,false),t.instance_id,t.host_id,t.allocation_execution_id,t.request_json FROM runtime.resident_sandboxes s JOIN public.app_core_sessionenvironment e ON e.id=s.environment_id LEFT JOIN runtime.execution_instances i ON i.execution_id=s.execution_id AND i.instance_id=s.instance_id AND i.environment_id=s.environment_id AND i.host_id=s.host_id AND i.session_id=e.session_id LEFT JOIN runtime.environment_retirements t ON t.environment_id=e.id WHERE e.session_id=$1 ORDER BY s.environment_id,s.instance_id FOR UPDATE OF s",
                &[&request.session_id],
            ).map_err(|error|format!("inspect Session purge retirement targets failed: {error}"))?;
            let mut retirements=Vec::new();
            for row in rows {
                let environment_id:String=row.get(0);
                let instance_id:String=row.get(1);
                let host_id:String=row.get(2);
                let execution_id:String=row.get(4);
                let state:String=row.get(5);
                if !row.get::<_,bool>(6) || !matches!(state.as_str(),"resident"|"releasing") {
                    return Err("session_purge_liabilities_pending".into());
                }
                let retirement=if let Some(body)=row.get::<_,Option<String>>(10) {
                    let persisted:RetirementRequest=serde_json::from_str(&body).map_err(|_|"session_purge_retirement_binding_rejected")?;
                    if row.get::<_,Option<String>>(7).as_deref()!=Some(instance_id.as_str())
                        || row.get::<_,Option<String>>(8).as_deref()!=Some(host_id.as_str())
                        || row.get::<_,Option<String>>(9).as_deref()!=Some(execution_id.as_str())
                        || persisted.environment_id!=environment_id || persisted.instance_id!=instance_id || persisted.host_id!=host_id {
                        return Err("session_purge_retirement_binding_rejected".into());
                    }
                    persisted
                } else {
                    RetirementRequest {schema:"runtime.environment.retire.v1".into(),operation_id:format!("session-delete:{instance_id}"),environment_id,instance_id,host_id,expected_controller_epoch:row.get(3),acknowledge_tmpfs_loss:true}
                };
                retirement.validate()?;
                retirements.push(retirement);
            }
            tx.commit().map_err(|error|error.to_string())?;
            Ok(retirements)
        })
    }

    pub(crate) fn purge_interactive_session(
        &self,
        request: &SessionPurgeRequest,
    ) -> Result<Vec<String>, String> {
        request.validate()?;
        self.with_client(|client| {
            let mut tx = client.transaction().map_err(|error| error.to_string())?;
            super::super::resource_commit::require_durable_resource_commit(&mut tx)?;
            lock_purge_scope(&mut tx,request)?;
            require_settled_resources(&mut tx, &request.session_id)?;
            let (sessions, object_ids) = collect_session_tree(&mut tx, &request.session_id)?;
            for session in &sessions {
                super::delete_session_records(&mut tx, session)?;
            }
            for object_id in object_ids {
                tx.execute(
                    "DELETE FROM runtime.external_context_objects o WHERE o.object_id=$1 AND NOT EXISTS(SELECT 1 FROM runtime.external_context_links l WHERE l.object_id=o.object_id) AND NOT EXISTS(SELECT 1 FROM runtime.runtime_jobs j WHERE j.payload_ref=o.object_id OR j.output_refs_json::jsonb ? o.object_id)",
                    &[&object_id],
                ).map_err(|error| format!("erase Session purge orphan object failed: {error}"))?;
            }
            erase_platform_body(&mut tx, &request.session_id)?;
            tx.execute(
                "UPDATE public.app_core_session SET status='deleted',title='',\"archivedAt\"=NULL,\"deletedAt\"=COALESCE(\"deletedAt\",transaction_timestamp()),\"deletedBy_id\"=owner_id,\"purgedAt\"=COALESCE(\"purgedAt\",transaction_timestamp()),\"updatedAt\"=transaction_timestamp() WHERE id=$1",
                &[&request.session_id],
            ).map_err(|error| format!("commit Session purge tombstone failed: {error}"))?;
            tx.commit().map_err(|error| error.to_string())?;
            Ok(sessions.into_iter().collect())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use centaeris_core::runtime::subagent::{build_subagent_run_job, SubagentRunJobRequest};

    fn packet(root: &str, child: &str) -> Value {
        let contract = centaeris_core::tool::list_tool_contracts()
            .into_iter()
            .find(|contract| contract.name == "read")
            .unwrap();
        serde_json::json!({
            "run_context":{"schema":"agent_run_context_v1","sessionId":root,"branchId":child,"turnId":"child-turn","agentRunId":"child-run","agentRef":{"agentId":"child","agentRunId":"child-run"},"parentAgentRef":{"agentId":"main","agentRunId":"parent-run"},"parentTurnId":"parent-turn","depth":1,"cwd":std::env::current_dir().unwrap().display().to_string(),"cancellationScopeId":"child-scope","parentCancellationScopeId":"parent-scope","cancelOnParentCancel":true,"createdAtMs":1},
            "task_brief":{"task_id":null,"objective":"Child task","success_criteria":[],"constraints":[],"output_hint":null},
            "hot_view":{"summary":"","recent_message_ids":[],"state_kv":{}},"object_refs":[],
            "allowedTools":["read"],"delegatedToolContracts":[{"name":"read","providerId":contract.provider_id,"contractDigest":contract.contract_digest().unwrap(),"concurrencySafe":contract.concurrency_safe}],"writablePathPrefixes":[],
            "output_contract":{"response_mode":"summary","expected_sections":[],"require_artifact_refs":false,"max_summary_chars":null},"parent_checkpoint_id":null,"context_mode":"Borrow"
        })
    }

    fn job() -> RuntimeJobRecord {
        build_subagent_run_job(SubagentRunJobRequest {
            session_id: "root".into(),
            parent_turn_id: "parent-turn".into(),
            tool_call_id: "call".into(),
            subagent_id: "child".into(),
            work_packet_ref: "packet".into(),
            checkpoint_id: None,
            run_at_ms: 1,
            created_at_ms: 1,
            max_retries: 0,
        })
    }

    #[test]
    fn purge_child_identity_requires_core_binding_and_root_scope() {
        let job = job();
        assert_eq!(
            child_session(&job, packet("root", "child"), "root").unwrap(),
            "child"
        );
        assert!(child_session(&job, packet("foreign-root", "child"), "root").is_err());
        assert!(child_session(&job, packet("root", "root"), "root").is_err());
        let mut wrong_turn = job.clone();
        wrong_turn.branch_id = Some("other-turn".into());
        assert!(child_session(&wrong_turn, packet("root", "child"), "root").is_err());
    }

    #[test]
    fn purge_contract_rejects_unknown_fields_and_wrong_schema() {
        let valid = serde_json::json!({"schema":"runtime.session.purge.v1","sessionId":"root","workspaceId":"workspace","ownerId":1});
        let request: SessionPurgeRequest = serde_json::from_value(valid.clone()).unwrap();
        request.validate().unwrap();
        let mut wrong = valid.clone();
        wrong["schema"] = Value::String("wrong".into());
        assert!(serde_json::from_value::<SessionPurgeRequest>(wrong)
            .unwrap()
            .validate()
            .is_err());
        let mut unknown = valid;
        unknown["unexpected"] = Value::Bool(true);
        assert!(serde_json::from_value::<SessionPurgeRequest>(unknown).is_err());
    }

    #[test]
    fn capture_tombstone_retains_identity_with_a_valid_empty_tool_result() {
        let wire = serde_json::json!({
            "schemaVersion":"session.event.v1","eventVersion":1,"eventId":"event:result",
            "type":"tool_result","sequence":1,"sessionId":"root","agentRunId":"run",
            "turnId":"turn","createdAtMs":1,"payload":{
                "callId":"call","toolName":"read","resultState":"successWithOutput",
                "modelContent":"private\u{0000}output","fullOutputPath":null,"outputStartByte":null,
                "outputByteLength":14,"outputComplete":true,"summary":"private preview",
                "operations":[],"modelInputImages":[],"latencyMs":2,
            },
        });
        let stored = crate::postgres_store::session_payload::encode(&wire).unwrap();
        let erased = erased_capture_event(stored).unwrap();
        assert_eq!(erased["eventId"], wire["eventId"]);
        assert_eq!(
            erased["payload"],
            serde_json::json!({
                "callId":"call","toolName":"read","resultState":"successWithOutput","latencyMs":2,
                "modelContent":"","fullOutputPath":null,"outputStartByte":null,"outputByteLength":0,
                "outputComplete":true,"summary":"purged","operations":[],"modelInputImages":[],
            })
        );
        assert!(erased
            .as_object()
            .unwrap()
            .get("__centaerisSessionStorage")
            .is_none());
        centaeris_core::session::parse_wire_record(&erased).unwrap();
    }
}
