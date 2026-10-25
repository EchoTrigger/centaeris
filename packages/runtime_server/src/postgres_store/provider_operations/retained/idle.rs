//! Bounded observations for the opt-in retained-environment rotation policy.
use super::power::{self, ProviderEnvironmentPowerOperation, ProviderEnvironmentPowerRequest};
use super::*;

pub(crate) mod policy;
pub(crate) const PAGE_SIZE: usize = 64;

pub(crate) struct RotationCandidate {
    pub environment_id: String,
    pub controller_epoch: i64,
    pub creation_attempt_id: String,
    pub sandbox_id: String,
    pub latest: Option<ProviderEnvironmentPowerOperation>,
    pub has_demand: bool,
}
pub(crate) struct RotationPage {
    pub candidates: Vec<RotationCandidate>,
    pub next: Option<String>,
    pub now_ms: i64,
    pub resume_capacity_available: bool,
}

// The existing lifecycle Job carries both durable waiting and cancellation.
// No second pending-work queue is created by the rotation policy.
pub(in crate::postgres_store) fn demand_predicate() -> String {
    format!("j.job_kind='agent_run.lifecycle' AND j.status IN('queued','leased','running') AND r.status IN('queued','running') AND j.session_id=r.session_id AND j.payload_ref='record:agent_run:'||r.id AND j.idempotency_key=j.job_id||':'||g.digest AND t.workspace_id=r.workspace_id AND g.payload->>'environmentProfileDigest'=e.environment_profile_digest AND NOT ({}) AND NOT ({}) AND (SELECT y.payload_json::jsonb->>'transitionReason' FROM runtime.runtime_events y WHERE y.task_id=j.job_id AND y.event_type='runtime_job_yielded' ORDER BY y.at_ms DESC,y.event_id DESC LIMIT 1) IN('environment_power_pending','e2b_environment_capacity_wait')",crate::postgres_store::lifecycle_control::PENDING,crate::postgres_store::lifecycle_control::TERMINAL)
}
pub(super) fn demand_from() -> &'static str {
    "runtime.runtime_jobs j JOIN public.app_core_agentrun r ON j.job_id='agent_run.lifecycle:'||r.id JOIN public.app_core_agentrunauthorization g ON g.agent_run_id=r.id JOIN runtime.execution_job_tenants t ON t.job_id=j.job_id"
}

/// Call under the Environment pin. Job cancellation takes the same Job lock.
/// Lock first, then re-evaluate in a fresh statement so a cancellation that
/// committed during lock acquisition is visible before the provider is invoked.
pub(super) fn pin_resume_demand(
    tx: &mut postgres::Transaction<'_>,
    environment_id: &str,
) -> Result<Option<power::automatic_resume::AutomaticResumeDemand>, String> {
    let rows=tx.query(&format!("SELECT j.job_id FROM {} JOIN public.app_core_sessionenvironment e ON e.id=r.environment_id WHERE r.environment_id=$1 AND {} ORDER BY j.job_id LIMIT $2 FOR SHARE OF j",demand_from(),demand_predicate()),&[&environment_id,&(PAGE_SIZE as i64)]).map_err(|e|e.to_string())?;
    let ids: Vec<String> = rows.into_iter().map(|row| row.get(0)).collect();
    if ids.is_empty() {
        return Ok(None);
    }
    tx.query_opt(&format!("SELECT j.job_id,r.id,g.digest FROM {} JOIN public.app_core_sessionenvironment e ON e.id=r.environment_id WHERE r.environment_id=$1 AND j.job_id=ANY($2::text[]) AND {} ORDER BY j.job_id LIMIT 1",demand_from(),demand_predicate()),&[&environment_id,&ids]).map(|row|row.map(|row|power::automatic_resume::AutomaticResumeDemand {schema:"runtime.providerEnvironment.automaticResumeDemand.v1".into(),job_id:row.get(0),agent_run_id:row.get(1),authorization_digest:row.get(2)})).map_err(|e|e.to_string())
}

impl PostgresRuntimeStore {
    /// Cursor ordering bounds each scan and avoids concentrating observation IO
    /// on the first environments. The policy drops disappeared targets on wrap.
    pub(crate) fn provider_rotation_candidates(
        &self,
        config: &str,
        profile_digest: &str,
        after: Option<&str>,
        running_limit: Option<u32>,
    ) -> Result<RotationPage, String> {
        self.cleanup_view().with_client(|db| {
            super::super::capacity::validate_limit(running_limit)?;
            let resume_capacity_available = match running_limit {
                Some(limit) => super::super::capacity::active_count(db, config)? < i64::from(limit),
                None => true,
            };
            let rows=db.query("SELECT e.id,e.controller_epoch,c.attempt_id,c.verified_sandbox_id,p.operation_id,p.environment_id,p.operation_sequence,p.predecessor_operation_id,p.request_digest,p.request_json,p.action,p.dispatch_state,p.ack_json,p.automatic_demand_json,p.not_dispatched_at_ms,(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint FROM runtime.provider_environment_bindings b JOIN runtime.provider_creation_operations c ON c.attempt_id=b.creation_attempt_id AND c.environment_id=b.environment_id JOIN public.app_core_sessionenvironment e ON e.id=b.environment_id LEFT JOIN LATERAL (SELECT * FROM runtime.provider_environment_power_operations p WHERE p.environment_id=e.id ORDER BY p.operation_sequence DESC LIMIT 1) p ON true WHERE c.provider_config_digest=$1 AND c.profile_digest=$2 AND e.environment_profile_digest=$2 AND c.dispatch_state='dispatched' AND c.verified_sandbox_id IS NOT NULL AND c.settlement_id IS NULL AND e.controller_released=true AND NOT EXISTS(SELECT 1 FROM runtime.provider_environment_retirements d WHERE d.environment_id=e.id) AND ($3::text IS NULL OR e.id>$3) ORDER BY e.id LIMIT $4",&[&config,&profile_digest,&after,&((PAGE_SIZE+1) as i64)]).map_err(|e|e.to_string())?;
            let more=rows.len()>PAGE_SIZE;
            let now_ms=rows.first().map(|row|row.get(15)).unwrap_or(crate::now_ms()?);
            let mut candidates=Vec::new();
            for row in rows.into_iter().take(PAGE_SIZE) {
                let environment_id:String=row.get(0);
                let latest=row.get::<_,Option<String>>(4).map(|id|power::decode_observation(&row,&id,5)).transpose()?.flatten();
                if latest.as_ref().is_some_and(|value|value.request.environment_id!=environment_id || value.request.creation_attempt_id!=row.get::<_,String>(2) || value.request.sandbox_id!=row.get::<_,String>(3) || value.request.provider_config_digest!=config) {
                    return Err("provider_rotation_power_binding_rejected".into());
                }
                candidates.push(RotationCandidate {environment_id,controller_epoch:row.get(1),creation_attempt_id:row.get(2),sandbox_id:row.get(3),latest,has_demand:false});
            }
            let ids: Vec<_> = candidates.iter().map(|candidate| candidate.environment_id.clone()).collect();
            if !ids.is_empty() {
                // A cheap live-Job hint avoids planning the full authorization and
                // wait-history predicate for pages with no work. It never grants
                // execution; the full predicate and dispatch pins remain authoritative.
                let live: Vec<String> = db.query("SELECT DISTINCT r.environment_id FROM public.app_core_agentrun r JOIN runtime.runtime_jobs j ON j.job_id='agent_run.lifecycle:'||r.id WHERE r.environment_id=ANY($1::text[]) AND r.status IN('queued','running') AND j.job_kind='agent_run.lifecycle' AND j.status IN('queued','leased','running')", &[&ids])
                    .map_err(|error| error.to_string())?.into_iter().map(|row| row.get(0)).collect();
                if !live.is_empty() {
                    let demanding: std::collections::BTreeSet<String> = db.query(&format!("SELECT DISTINCT r.environment_id FROM {} JOIN public.app_core_sessionenvironment e ON e.id=r.environment_id WHERE r.environment_id=ANY($1::text[]) AND {}", demand_from(), demand_predicate()), &[&live])
                        .map_err(|error| error.to_string())?.into_iter().map(|row| row.get(0)).collect();
                    for candidate in &mut candidates {
                        candidate.has_demand = demanding.contains(&candidate.environment_id);
                    }
                }
            }
            let next=more.then(||candidates.last().expect("nonempty bounded page").environment_id.clone());
            Ok(RotationPage {candidates,next,now_ms,resume_capacity_available})
        })
    }
}

pub(crate) fn request(
    candidate: &RotationCandidate,
    config: &str,
    action: power::ProviderEnvironmentPowerAction,
) -> ProviderEnvironmentPowerRequest {
    ProviderEnvironmentPowerRequest {
        schema: "runtime.provider_environment.power.v1".into(),
        operation_id: format!("environment-power-{}", uuid::Uuid::new_v4()),
        action,
        environment_id: candidate.environment_id.clone(),
        creation_attempt_id: candidate.creation_attempt_id.clone(),
        sandbox_id: candidate.sandbox_id.clone(),
        provider_config_digest: config.into(),
        expected_controller_epoch: candidate.controller_epoch,
        predecessor_operation_id: candidate
            .latest
            .as_ref()
            .map(|value| value.request.operation_id.clone()),
    }
}
