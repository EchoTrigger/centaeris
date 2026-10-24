//! A fresh resume ACK wakes business jobs without manufacturing another Job owner.
use super::*;

pub(super) fn resumed_jobs(
    tx: &mut postgres::Transaction<'_>,
    operation: &ProviderEnvironmentPowerOperation,
    target: &ProviderVerifiedTarget,
    acknowledged_at_ms: i64,
) -> Result<(), String> {
    // The Environment remains pinned by the ACK transaction. Job locks follow
    // the existing Environment -> Run -> Job direction; yield owns no Env pin.
    let rows=tx.query(
        "SELECT j.job_id,j.session_id,r.id,g.digest,g.payload::text FROM runtime.runtime_jobs j JOIN public.app_core_agentrun r ON j.job_id='agent_run.lifecycle:'||r.id JOIN public.app_core_agentrunauthorization g ON g.agent_run_id=r.id JOIN runtime.execution_job_tenants t ON t.job_id=j.job_id WHERE j.job_kind='agent_run.lifecycle' AND j.status IN('queued','leased','running') AND r.environment_id=$1 AND r.session_id=$2 AND r.workspace_id=$3 AND j.session_id=r.session_id AND j.payload_ref='record:agent_run:'||r.id AND j.idempotency_key=j.job_id||':'||g.digest AND t.workspace_id=r.workspace_id ORDER BY j.job_id FOR UPDATE OF j",
        &[&target.identity.environment_id,&target.identity.session_id,&target.identity.workspace_id],
    ).map_err(|e|e.to_string())?;
    let mut events = Vec::new();
    let mut sessions = Vec::new();
    let mut jobs = Vec::new();
    let mut payloads = Vec::new();
    for row in rows {
        let job: String = row.get(0);
        let session: String = row.get(1);
        let run: String = row.get(2);
        let digest: String = row.get(3);
        let original: crate::agent_run_authorization::WorkspaceAgentRunAuthorization =
            match serde_json::from_str(&row.get::<_, String>(4)) {
                Ok(value) => value,
                Err(_) => continue,
            };
        if original
            .checked_binding(&run)
            .and_then(|binding| binding.verify_digest_binding(&digest))
            .is_err()
            || original.session_id != target.identity.session_id
            || original.workspace_id != target.identity.workspace_id
            || original.environment_profile_digest != target.identity.profile_digest
            || original.environment_profile.digest().as_deref()
                != Ok(target.identity.profile_digest.as_str())
        {
            continue;
        }
        events.push(format!(
            "runtime_job_wake:{job}:provider_power:{}",
            operation.request.operation_id
        ));
        payloads.push(serde_json::json!({"schema":"runtime.provider_environment.power.wake.v1","jobId":job,
            "environmentId":target.identity.environment_id,"sourceOperationId":operation.request.operation_id,
            "wokenAtMs":acknowledged_at_ms,"transitionReason":"provider_environment_resumed"}).to_string());
        sessions.push(session);
        jobs.push(job);
    }
    if jobs.is_empty() {
        return Ok(());
    }
    // One event insert and one due update cover the entire Environment. Active
    // owners keep their lease; existing yield consumes the pending wake later.
    tx.execute("INSERT INTO runtime.runtime_events(event_id,session_id,task_id,event_type,at_ms,visibility,payload_json) SELECT v.event_id,v.session_id,v.job_id,'runtime_job_wake_requested',$5,'internal',v.payload_json FROM unnest($1::text[],$2::text[],$3::text[],$4::text[]) AS v(event_id,session_id,job_id,payload_json)",
        &[&events,&sessions,&jobs,&payloads,&acknowledged_at_ms]).map_err(|e|e.to_string())?;
    tx.execute("UPDATE runtime.runtime_jobs SET run_at_ms=GREATEST($2,COALESCE(lease_reclaim_not_before_ms,$2)),updated_at_ms=$2 WHERE job_id=ANY($1::text[]) AND status='queued' AND run_at_ms>GREATEST($2,COALESCE(lease_reclaim_not_before_ms,$2))",&[&jobs,&acknowledged_at_ms]).map_err(|e|e.to_string())?;
    Ok(())
}

/// A positive pause ACK releases provider residency, not another Job owner.
/// Lock selected Jobs, then recheck in a fresh statement to fence cancellation
/// and successful creation committed while the first statement waited.
pub(super) fn capacity_jobs(
    tx: &mut postgres::Transaction<'_>,
    operation: &ProviderEnvironmentPowerOperation,
    acknowledged_at_ms: i64,
) -> Result<(), String> {
    let predicate=format!("j.job_kind='agent_run.lifecycle' AND j.status IN('queued','leased','running') AND r.status IN('queued','running') AND j.session_id=r.session_id AND j.payload_ref='record:agent_run:'||r.id AND j.idempotency_key=j.job_id||':'||g.digest AND t.workspace_id=r.workspace_id AND g.payload->>'environmentProfileDigest'=e.environment_profile_digest AND e.environment_profile->>'providerConfigDigest'=$1 AND NOT ({}) AND NOT ({}) AND (y.reason='e2b_environment_capacity_wait' OR (j.status IN('leased','running') AND COALESCE(y.reason,'')<>'question_wait' AND EXISTS(SELECT 1 FROM runtime.provider_creation_operations c WHERE c.provider='e2b' AND c.provider_config_digest=$1 AND c.profile_digest=e.environment_profile_digest AND c.environment_id=e.id AND c.agent_run_id=r.id AND c.authorization_digest=g.digest AND c.processing_job_id=j.job_id AND c.dispatch_state='prepared' AND c.settlement_id IS NULL)))",crate::postgres_store::lifecycle_control::PENDING,crate::postgres_store::lifecycle_control::TERMINAL);
    let selection=format!("SELECT j.job_id,j.session_id,r.id,g.digest,g.payload::text,e.id,e.environment_profile_digest FROM runtime.runtime_jobs j JOIN public.app_core_agentrun r ON j.job_id='agent_run.lifecycle:'||r.id JOIN public.app_core_agentrunauthorization g ON g.agent_run_id=r.id JOIN runtime.execution_job_tenants t ON t.job_id=j.job_id JOIN public.app_core_sessionenvironment e ON e.id=r.environment_id LEFT JOIN LATERAL (SELECT z.payload_json::jsonb->>'transitionReason' AS reason FROM runtime.runtime_events z WHERE z.task_id=j.job_id AND z.event_type='runtime_job_yielded' ORDER BY z.at_ms DESC,z.event_id DESC LIMIT 1) y ON true WHERE {predicate}");
    let ids: Vec<String> = tx
        .query(
            &format!("{selection} ORDER BY j.job_id LIMIT 64 FOR UPDATE OF j"),
            &[&operation.request.provider_config_digest],
        )
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|row| row.get(0))
        .collect();
    if ids.is_empty() {
        return Ok(());
    }
    let rows = tx
        .query(
            &format!("{selection} AND j.job_id=ANY($2::text[]) ORDER BY j.job_id LIMIT 64"),
            &[&operation.request.provider_config_digest, &ids],
        )
        .map_err(|e| e.to_string())?;
    let mut events = Vec::new();
    let mut sessions = Vec::new();
    let mut jobs = Vec::new();
    let mut payloads = Vec::new();
    for row in rows {
        let job: String = row.get(0);
        let session: String = row.get(1);
        let run: String = row.get(2);
        let digest: String = row.get(3);
        let environment: String = row.get(5);
        let profile_digest: String = row.get(6);
        let original: crate::agent_run_authorization::WorkspaceAgentRunAuthorization =
            match serde_json::from_str(&row.get::<_, String>(4)) {
                Ok(value) => value,
                Err(_) => continue,
            };
        if original
            .checked_binding(&run)
            .and_then(|binding| binding.verify_digest_binding(&digest))
            .is_err()
            || original.session_id != session
            || original.environment_profile_digest != profile_digest
            || original.environment_profile.digest().as_deref() != Ok(profile_digest.as_str())
        {
            continue;
        }
        events.push(format!(
            "runtime_job_wake:{job}:provider_capacity:{}",
            operation.request.operation_id
        ));
        payloads.push(serde_json::json!({"schema":"runtime.provider_environment.power.wake.v1","jobId":job,"environmentId":environment,"sourceOperationId":operation.request.operation_id,"wokenAtMs":acknowledged_at_ms,"transitionReason":"capacity_released"}).to_string());
        sessions.push(session);
        jobs.push(job);
    }
    if jobs.is_empty() {
        return Ok(());
    }
    tx.execute("INSERT INTO runtime.runtime_events(event_id,session_id,task_id,event_type,at_ms,visibility,payload_json) SELECT v.event_id,v.session_id,v.job_id,'runtime_job_wake_requested',$5,'internal',v.payload_json FROM unnest($1::text[],$2::text[],$3::text[],$4::text[]) AS v(event_id,session_id,job_id,payload_json)",&[&events,&sessions,&jobs,&payloads,&acknowledged_at_ms]).map_err(|e|e.to_string())?;
    tx.execute("UPDATE runtime.runtime_jobs SET run_at_ms=GREATEST($2,COALESCE(lease_reclaim_not_before_ms,$2)),updated_at_ms=$2 WHERE job_id=ANY($1::text[]) AND status='queued' AND run_at_ms>GREATEST($2,COALESCE(lease_reclaim_not_before_ms,$2))",&[&jobs,&acknowledged_at_ms]).map_err(|e|e.to_string())?;
    Ok(())
}
