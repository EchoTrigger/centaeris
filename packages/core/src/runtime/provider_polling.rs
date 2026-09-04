use super::*;

pub(super) fn build_provider_poll_runtime_job(
    session_id: &str,
    turn_id: &str,
    agent_run_identity: &RuntimeAgentRunIdentityV1,
    report: &ToolExecutionResult,
    pending_poll: &DynamicToolPendingPoll,
) -> Result<RuntimeJobRecord, String> {
    agent_run_identity.validate()?;
    let payload = ProviderPollingRuntimePayload {
        provider_id: pending_poll.provider_id.clone(),
        tool_name: pending_poll.tool_name.clone(),
        poll_key: pending_poll.spec.poll_key.clone(),
        poll_args: pending_poll.spec.poll_args.clone(),
        source_agent_run_id: agent_run_identity.agent_run_id.clone(),
        source_turn_id: turn_id.to_string(),
        source_tool_call_id: report.tool_call_id.clone(),
        lease_ms: pending_poll.spec.lease_ms.unwrap_or(30_000),
    };
    let job_id = build_provider_poll_runtime_job_id(
        session_id,
        agent_run_identity.agent_run_id.as_str(),
        turn_id,
        report.tool_call_id.as_str(),
        pending_poll.provider_id.as_str(),
        pending_poll.tool_name.as_str(),
        pending_poll.spec.poll_key.as_str(),
    );
    let payload_json = build_provider_poll_payload_ref(&payload)?;
    let provider_idempotency_key = pending_poll
        .spec
        .idempotency_key
        .clone()
        .unwrap_or_else(|| pending_poll.spec.poll_key.clone());
    let idempotency_key = format!(
        "provider.poll:{session_id}:{}:{}:{}:{provider_idempotency_key}",
        agent_run_identity.agent_run_id, pending_poll.provider_id, pending_poll.tool_name
    );
    Ok(RuntimeJobRecord {
        job_id,
        job_kind: PROVIDER_POLL_RUNTIME_JOB_KIND.to_string(),
        status: RuntimeJobStatus::Queued,
        run_at_ms: pending_poll
            .spec
            .next_poll_at_ms
            .unwrap_or(report.completed_at_ms),
        lease_owner: None,
        lease_expires_at_ms: None,
        heartbeat_at_ms: None,
        retry_count: 0,
        max_retries: pending_poll.spec.max_poll_attempts.unwrap_or(30),
        backoff_policy: RuntimeBackoffPolicy::default(),
        idempotency_key,
        session_id: Some(session_id.to_string()),
        branch_id: None,
        checkpoint_id: None,
        payload_ref: Some(payload_json),
        output_refs: vec![],
        last_error: None,
        created_at_ms: report.completed_at_ms,
        updated_at_ms: report.completed_at_ms,
    })
}
