use crate::agent_run_authorization::WorkspaceAgentRunAuthorization;
use crate::agent_run_lifecycle_job_id;
use crate::environment_authorization::EnvironmentAuthorization;
use centaeris_core::session::RuntimeJobLeaseFence;
use sha2::{Digest, Sha256};

pub(crate) struct ControllerAuthorityRequest<'a> {
    pub environment_authorization: &'a EnvironmentAuthorization,
    pub signing_key: &'a [u8],
    pub workspace_id: &'a str,
    pub lease: &'a RuntimeJobLeaseFence,
    pub purpose: ControllerPurpose,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ControllerPurpose {
    Execute,
    Cleanup,
}
impl ControllerPurpose {
    fn as_str(self) -> &'static str {
        match self {
            Self::Execute => "execute",
            Self::Cleanup => "cleanup",
        }
    }
}

pub(crate) struct VerifiedOriginalGrant {
    pub authorization: WorkspaceAgentRunAuthorization,
    pub authorization_digest: String,
    pub controller_epoch: i64,
}

// Match API's Workspace → membership → Session → Environment → Run → grant
// order. A legitimate heartbeat briefly owns the job exclusively. Bound that
// wait by the existing request/SQL budget, then verify authority with a fresh
// database clock. Keep the SHARE pins through the physical operation.
pub(crate) fn verify_controller_authority(
    tx: &mut postgres::Transaction<'_>,
    r: &ControllerAuthorityRequest<'_>,
) -> Result<VerifiedOriginalGrant, String> {
    let b = &r.environment_authorization.payload;
    b.digest()?;
    let c = b
        .controller
        .as_ref()
        .ok_or("retained controller required")?;
    if c.controller_purpose != r.purpose.as_str()
        || r.lease.job_id != agent_run_lifecycle_job_id(&b.agent_run_id)?
        || r.lease.job_kind != "agent_run.lifecycle"
    {
        return Err("retained controller identity rejected".into());
    }
    let mut hash = Sha256::new();
    hash.update(b"workspace:environment-controller-lease:v1\0");
    hash.update(r.lease.lease_owner.as_bytes());
    if c.controller_lease_digest != format!("sha256:{:x}", hash.finalize()) {
        return Err("retained controller lease digest rejected".into());
    }
    let initial=tx.query_opt("SELECT workspace_id,membership_ref,user_id::text FROM public.app_core_agentrun WHERE id=$1", &[&b.agent_run_id]).map_err(|e|e.to_string())?.ok_or("retained Run missing")?;
    let tenant: String = initial.get(0);
    let member: String = initial.get(1);
    let user: String = initial.get(2);
    if tenant != r.workspace_id {
        return Err("retained tenant rejected".into());
    }
    let workspace = tx
        .query_opt(
            "SELECT status FROM public.app_core_workspace WHERE id=$1 FOR SHARE",
            &[&tenant],
        )
        .map_err(|e| e.to_string())?
        .ok_or("retained workspace missing")?;
    let membership=tx.query_opt("SELECT 1 FROM public.app_core_workspacemembership WHERE id=$1 AND workspace_id=$2 AND user_id::text=$3 AND role IN('owner','admin','member') FOR SHARE", &[&member,&tenant,&user]).map_err(|e|e.to_string())?;
    if r.purpose == ControllerPurpose::Execute
        && (workspace.get::<_, String>(0) != "active" || membership.is_none())
    {
        return Err("retained membership rejected".into());
    }
    let session=tx.query_opt("SELECT workspace_id,owner_id::text,agent_id,current_environment_id,\"deletedAt\" IS NULL FROM public.app_core_session WHERE id=$1 FOR SHARE", &[&b.session_id]).map_err(|e|e.to_string())?.ok_or("retained Session missing")?;
    if session.get::<_, String>(0) != tenant
        || session.get::<_, String>(1) != user
        || (r.purpose == ControllerPurpose::Execute
            && (session.get::<_, Option<String>>(3).as_deref() != Some(&b.environment_id)
                || !session.get::<_, bool>(4)))
    {
        return Err("retained Session rejected".into());
    }
    let env=tx.query_opt("SELECT session_id,controller_run_id,controller_epoch,controller_lease_owner,controller_purpose,controller_released,environment_profile::text,environment_profile_digest FROM public.app_core_sessionenvironment WHERE id=$1 FOR SHARE", &[&b.environment_id]).map_err(|e|e.to_string())?.ok_or("retained environment missing")?;
    if env.get::<_, String>(0) != b.session_id
        || env.get::<_, Option<String>>(1).as_deref() != Some(&b.agent_run_id)
        || env.get::<_, i64>(2) != c.controller_epoch
        || env.get::<_, String>(3) != r.lease.lease_owner
        || env.get::<_, String>(4) != c.controller_purpose
        || env.get::<_, bool>(5)
    {
        return Err("retained controller fence rejected".into());
    }
    let run=tx.query_opt("SELECT session_id,environment_id,workspace_id,user_id::text,membership_ref,status,\"modelConfig_id\",\"thinkingMode\",\"preAdmissionCancelledAt\" IS NULL FROM public.app_core_agentrun WHERE id=$1 FOR SHARE", &[&b.agent_run_id]).map_err(|e|e.to_string())?.ok_or("retained Run missing")?;
    if let Some(retirement) = tx
        .query_opt(
            "SELECT state FROM runtime.environment_retirements WHERE environment_id=$1",
            &[&b.environment_id],
        )
        .map_err(|e| e.to_string())?
    {
        if r.purpose == ControllerPurpose::Execute || retirement.get::<_, String>(0) != "destroyed"
        {
            return Err("retained environment retiring; controller operation rejected".into());
        }
    }
    if run.get::<_, String>(0) != b.session_id
        || run.get::<_, Option<String>>(1).as_deref() != Some(&b.environment_id)
        || run.get::<_, String>(2) != tenant
        || run.get::<_, String>(3) != user
        || run.get::<_, String>(4) != member
        || (r.purpose == ControllerPurpose::Execute
            && !matches!(run.get::<_, String>(5).as_str(), "queued" | "running"))
    {
        return Err("retained Run binding rejected".into());
    }
    // Session remains locked against Core record writers. Status is a delayed
    // presentation projection and cannot reopen already committed termination.
    let terminal=tx.query_one("SELECT EXISTS(SELECT 1 FROM public.app_core_sessionevent WHERE workspace_id=$1 AND session_id=$2 AND agent_run_id=$3 AND session_level=false AND payload->>'type' IN('agent_run_completed','agent_run_failed','agent_run_interrupted'))", &[&tenant,&b.session_id,&b.agent_run_id]).map_err(|e|e.to_string())?.get::<_,bool>(0);
    let pre_admission_failure = super::pre_admission_failure::has_pre_admission_failure(
        tx,
        &tenant,
        &b.session_id,
        &b.agent_run_id,
        &b.agent_run_authorization_digest,
    )?;
    let terminal = !run.get::<_, bool>(8) || terminal || pre_admission_failure;
    if r.purpose == ControllerPurpose::Execute && terminal {
        return Err("retained Core terminal execution rejected".into());
    }
    if r.purpose == ControllerPurpose::Cleanup && !terminal {
        return Err("retained cleanup requires Core terminal".into());
    }
    let grant=tx.query_opt("SELECT digest,payload::text,signature FROM public.app_core_agentrunauthorization WHERE agent_run_id=$1 FOR SHARE", &[&b.agent_run_id]).map_err(|e|e.to_string())?.ok_or("retained original grant missing")?;
    let digest: String = grant.get(0);
    let original: WorkspaceAgentRunAuthorization =
        serde_json::from_str(&grant.get::<_, String>(1)).map_err(|e| e.to_string())?;
    original.checked_binding(&b.agent_run_id)?.verify(
        &digest,
        r.signing_key,
        &grant.get::<_, String>(2),
    )?;
    r.environment_authorization
        .verify(&original, &digest, r.signing_key)?;
    if original.workspace_id != tenant
        || original.user_id != user
        || original.agent_id != session.get::<_, String>(2)
        || original.model_config_ref != run.get::<_, String>(6)
        || original.thinking_mode != run.get::<_, Option<String>>(7).filter(|s| !s.is_empty())
    {
        return Err("retained original grant binding rejected".into());
    }
    let bound_profile: crate::agent_run_authorization::EnvironmentProfile = serde_json::from_str(
        &env.get::<_, Option<String>>(6)
            .ok_or("environment_profile_missing")?,
    )
    .map_err(|_| "environment_profile_invalid")?;
    if bound_profile != original.environment_profile
        || env.get::<_, Option<String>>(7).as_deref() != Some(&original.environment_profile_digest)
        || bound_profile.digest()? != original.environment_profile_digest
    {
        return Err("environment_profile_binding_rejected".into());
    }
    pin_lifecycle_job(tx, &r.lease.job_id)?;
    // This is a separate statement after lock acquisition. A predicate evaluated
    // before a pure lock wait cannot prove that an unchanged lease is unexpired.
    let job=tx.query_opt("SELECT 1 FROM runtime.runtime_jobs j JOIN runtime.execution_job_tenants t ON t.job_id=j.job_id WHERE j.job_id=$1 AND j.job_kind='agent_run.lifecycle' AND j.status IN('leased','running') AND j.lease_owner=$2 AND j.lease_expires_at_ms>(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint AND j.session_id=$3 AND j.idempotency_key=$4 AND j.payload_ref=$5 AND t.workspace_id=$6", &[&r.lease.job_id,&r.lease.lease_owner,&b.session_id,&format!("{}:{digest}",r.lease.job_id),&format!("record:agent_run:{}",b.agent_run_id),&tenant]).map_err(|e|format!("retained job fence busy or rejected: {e}"))?;
    if job.is_none() {
        return Err("retained job fence rejected".into());
    }
    Ok(VerifiedOriginalGrant {
        authorization: original,
        authorization_digest: digest,
        controller_epoch: c.controller_epoch,
    })
}

fn pin_lifecycle_job(tx: &mut postgres::Transaction<'_>, job_id: &str) -> Result<(), String> {
    let timeouts = tx
        .query_one(
            "SELECT current_setting('lock_timeout'),current_setting('statement_timeout'),(SELECT setting::bigint FROM pg_settings WHERE name='lock_timeout'),(SELECT setting::bigint FROM pg_settings WHERE name='statement_timeout')",
            &[],
        )
        .map_err(|error| error.to_string())?;
    let old_lock: String = timeouts.get(0);
    let old_statement: String = timeouts.get(1);
    let remaining = super::remaining_request_time()?
        .unwrap_or(super::DEFAULT_POSTGRES_CHECKOUT_TIMEOUT)
        .min(super::DEFAULT_POSTGRES_CHECKOUT_TIMEOUT);
    let mut milliseconds = remaining.as_millis().max(1);
    for configured_ms in [timeouts.get::<_, i64>(2), timeouts.get::<_, i64>(3)] {
        if configured_ms > 0 {
            milliseconds = milliseconds.min(configured_ms as u128);
        }
    }
    tx.query_one(
        "SELECT set_config('lock_timeout',$1,true),set_config('statement_timeout',$1,true)",
        &[&format!("{milliseconds}ms")],
    )
    .map_err(|error| error.to_string())?;
    let pinned = tx.query_opt(
        "SELECT j.job_id FROM runtime.runtime_jobs j JOIN runtime.execution_job_tenants t ON t.job_id=j.job_id WHERE j.job_id=$1 FOR SHARE OF j,t",
        &[&job_id],
    ).map_err(|error| format!("retained job fence busy or rejected: {error}"))?;
    // The statement limit also bounds combined waits for the job and tenant.
    // Restore on success; a SQL failure rolls back transaction-local changes.
    tx.query_one(
        "SELECT set_config('lock_timeout',$1,true),set_config('statement_timeout',$2,true)",
        &[&old_lock, &old_statement],
    )
    .map_err(|error| error.to_string())?;
    if pinned.is_none() {
        return Err("retained job fence rejected".into());
    }
    Ok(())
}
