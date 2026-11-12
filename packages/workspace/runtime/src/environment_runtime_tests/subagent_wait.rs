//! Owned read-only WorkPackets exercise native wait ownership without changing
//! the production agent tool's default write-capable delegation policy.
use crate::*;
use centaeris_core::runtime::keys::external_context as keys;
use centaeris_core::runtime::subagent::{build_subagent_run_job, SubagentRunJobRequest};
use centaeris_core::session::external_context::{
    ExternalContextObject, EXTERNAL_CONTEXT_SCHEMA_VERSION,
};
use centaeris_core::session::store::{
    RuntimeStoreTransactionPort, UpsertExternalContextAndScheduleJobRequest,
};

pub(super) fn seed(
    store: &PostgresRuntimeStore,
    contexts: &ToolLayerRegistry,
    run: &str,
) -> Result<serde_json::Value, String> {
    if !super::owns_run(run) {
        return Err("owned subagent parent unknown".into());
    }
    let (session, turn) = store.with_client(|client| {
        let row = client.query_one(
            "SELECT session_id,payload->>'turnId' FROM public.app_core_sessionevent WHERE agent_run_id=$1 AND payload->>'type'='model_request_started' ORDER BY sequence DESC LIMIT 1",
            &[&run],
        ).map_err(|error| error.to_string())?;
        Ok((row.get::<_, String>(0), row.get::<_, String>(1)))
    })?;
    let (cwd, contract) = {
        let contexts = contexts.lock().map_err(|_| "owned context lock poisoned")?;
        let context = contexts
            .get(&session)
            .ok_or("owned parent context missing")?;
        if context.agent_run_id != run {
            return Err("owned parent context mismatch".into());
        }
        (
            context
                .tool_layer
                .cwd()
                .ok_or("owned parent cwd missing")?
                .display()
                .to_string(),
            context.tool_layer.tool_contract("read")?,
        )
    };
    let mut references = Vec::new();
    for index in 0..3 {
        let id = format!("native-child-{index}");
        let child = format!("{session}-child-{index}");
        let child_turn = format!("{turn}-child-{index}");
        let packet_ref = keys::subagent_work_packet_ref(&child);
        let now = now_ms()?;
        let job = build_subagent_run_job(SubagentRunJobRequest {
            session_id: session.clone(),
            parent_turn_id: turn.clone(),
            tool_call_id: format!("start-{id}"),
            subagent_id: id.clone(),
            work_packet_ref: packet_ref.clone(),
            checkpoint_id: None,
            run_at_ms: now,
            created_at_ms: now,
            max_retries: 0,
        });
        let packet = json!({
            "run_context":{"schema":"agent_run_context_v1","sessionId":session,"branchId":child,
                "turnId":child_turn,"agentRunId":job.job_id,"agentRef":{"agentId":id,"agentRunId":job.job_id},
                "parentAgentRef":{"agentId":"main-agent","agentRunId":run},"parentTurnId":turn,
                "depth":1,"cwd":cwd,"cancellationScopeId":format!("cancel:{}",job.job_id),
                "parentCancellationScopeId":format!("cancel:{run}"),"cancelOnParentCancel":true,"createdAtMs":now},
            "task_brief":{"task_id":id,"objective":id,"success_criteria":[],"constraints":[],"output_hint":id},
            "hot_view":{"summary":"","recent_message_ids":[],"state_kv":{}},"object_refs":[],
            "allowedTools":["read"],"delegatedToolContracts":[{"name":"read","providerId":contract.provider_id,
                "contractDigest":contract.contract_digest()?,"concurrencySafe":contract.concurrency_safe}],"writablePathPrefixes":[],
            "output_contract":{"response_mode":"summary","expected_sections":[],"require_artifact_refs":false,"max_summary_chars":null},
            "parent_checkpoint_id":null,"context_mode":"Borrow",
        });
        subagent_work_packet_runtime_binding(
            &centaeris_core::runtime::subagent::SubagentWorkPacketEnvelope {
                ref_id: packet_ref.clone(),
                content_json: packet.clone(),
            },
            &job,
        )?;
        let result_ref = keys::subagent_result_ref(&job.job_id);
        let reference = json!({"schema":"task_output_ref_v1","kind":"agent",
            "runtime_job_id":job.job_id,"child_session_id":child,"result_ref":result_ref});
        store.upsert_external_context_and_schedule_job(UpsertExternalContextAndScheduleJobRequest {
            object: ExternalContextObject {
                schema_version: EXTERNAL_CONTEXT_SCHEMA_VERSION.into(),object_id:packet_ref,
                object_kind:"subagent_work_packet".into(),source_provider_id:"centaeris.core".into(),source_tool_name:"agent".into(),
                title:id.clone(),content:packet.to_string(),updated_at_ms:now,
                metadata:json!({"schema":"agent_tool_work_packet_v1","sessionId":session,
                    "parentTurnId":turn,"childTurnId":child_turn,"toolCallId":format!("start-{id}"),
                    "subagentId":id,"runtimeJobId":job.job_id,"childSessionId":child,"resultRef":result_ref}),
            }, job,
        })?;
        references.push(reference);
    }
    Ok(json!(references))
}
