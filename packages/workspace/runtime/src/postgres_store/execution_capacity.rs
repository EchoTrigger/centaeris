use std::sync::Arc;

use centaeris_core::session::reliability::{
    ClaimDueRuntimeJobsRequest, RuntimeJobRecord, RuntimeJobStorePort,
    ScheduleRuntimeJobDisposition, ScheduleRuntimeJobRequest, ScheduleRuntimeJobResult,
    AGENT_RUN_LIFECYCLE_JOB_KIND,
};

use super::reliability::{insert_job, load_job, load_job_by_key};
use super::PostgresRuntimeStore;

pub(super) struct DispatchHost {
    id: String,
    sandbox_limit: Option<i64>,
}

// A live or unresolved resident can only be advanced on its owning host.
pub(super) fn host_affinity(host: &str) -> String {
    format!(
        "({host}::text IS NULL OR EXISTS(
        SELECT 1 FROM public.app_core_agentrun r
        WHERE j.job_id='agent_run.lifecycle:'||r.id AND j.session_id=r.session_id
        AND NOT EXISTS(SELECT 1 FROM runtime.resident_sandboxes s
            WHERE (s.environment_id=r.environment_id OR s.agent_run_id=r.id) AND s.host_id<>{host})
    ))"
    )
}

pub(super) fn local_resident(host: &str) -> String {
    format!("EXISTS(SELECT 1 FROM public.app_core_agentrun r
        JOIN runtime.resident_sandboxes s ON s.environment_id=r.environment_id OR s.agent_run_id=r.id
        WHERE j.job_id='agent_run.lifecycle:'||r.id AND j.session_id=r.session_id AND s.host_id={host})")
}

pub(super) fn host_start_slots(host: &str, limit: &str, now: &str) -> String {
    format!("SELECT CASE WHEN {limit}::bigint IS NULL THEN NULL ELSE GREATEST({limit}::bigint -
    (SELECT count(*) FROM runtime.resident_sandboxes WHERE host_id={host}) -
    (SELECT count(*) FROM runtime.runtime_jobs active
     JOIN public.app_core_agentrun r ON active.job_id='agent_run.lifecycle:'||r.id AND active.session_id=r.session_id
     WHERE active.lease_host_id={host} AND active.lease_purpose='execute'
       AND active.status IN('leased','running') AND active.lease_expires_at_ms>{now}
       AND NOT EXISTS(SELECT 1 FROM runtime.resident_sandboxes s WHERE s.environment_id=r.environment_id OR s.agent_run_id=r.id)
    ),0) END AS available")
}

impl PostgresRuntimeStore {
    pub(crate) fn with_dispatch_host(
        mut self,
        id: String,
        sandbox_limit: Option<u64>,
    ) -> Result<Self, String> {
        let sandbox_limit = sandbox_limit
            .map(i64::try_from)
            .transpose()
            .map_err(|e| e.to_string())?;
        if id.trim().is_empty() || sandbox_limit.is_some_and(|limit| limit <= 0) {
            return Err("worker dispatch host or sandbox limit invalid".into());
        }
        self.dispatch_host = Some(Arc::new(DispatchHost { id, sandbox_limit }));
        Ok(self)
    }

    pub(super) fn dispatch_host_id(&self) -> Option<&str> {
        self.dispatch_host.as_deref().map(|host| host.id.as_str())
    }

    pub(super) fn dispatch_sandbox_limit(&self) -> Option<i64> {
        self.dispatch_host
            .as_deref()
            .and_then(|host| host.sandbox_limit)
    }

    pub(crate) fn schedule_worker_job(
        &self,
        req: ScheduleRuntimeJobRequest,
        workspace_id: Option<&str>,
    ) -> Result<ScheduleRuntimeJobResult, String> {
        if req.job.job_kind != AGENT_RUN_LIFECYCLE_JOB_KIND {
            if workspace_id.is_some() {
                return Err("unexpected execution tenant".to_string());
            }
            return self.schedule_runtime_job(req);
        }
        let workspace_id = workspace_id
            .filter(|value| !value.trim().is_empty())
            .ok_or("execution tenant is required")?;
        self.with_client(|client| {
            let mut tx = client.transaction().map_err(|error| error.to_string())?;
            let inserted = insert_job(&mut tx, &req.job)?;
            let job = load_job_by_key(&mut tx, &req.job.job_kind, &req.job.idempotency_key)?
                .ok_or("scheduled execution job missing")?;
            if job.job_id != req.job.job_id
                || job.session_id != req.job.session_id
                || job.payload_ref != req.job.payload_ref
            {
                return Err("execution job identity conflict".to_string());
            }
            if inserted {
                tx.execute(
                    "INSERT INTO execution_job_tenants(job_id,workspace_id) VALUES($1,$2)",
                    &[&job.job_id, &workspace_id],
                )
                .map_err(|error| error.to_string())?;
            } else {
                let binding = tx
                    .query_opt(
                        "SELECT workspace_id FROM execution_job_tenants WHERE job_id=$1",
                        &[&job.job_id],
                    )
                    .map_err(|error| error.to_string())?;
                if binding.is_none_or(|row| row.get::<_, String>(0) != workspace_id) {
                    return Err("execution tenant identity conflict".to_string());
                }
            }
            tx.commit().map_err(|error| error.to_string())?;
            Ok(ScheduleRuntimeJobResult {
                disposition: if inserted {
                    ScheduleRuntimeJobDisposition::Inserted
                } else {
                    ScheduleRuntimeJobDisposition::Existing
                },
                job,
            })
        })
    }

    pub(crate) fn claim_worker_jobs(
        &self,
        req: ClaimDueRuntimeJobsRequest,
    ) -> Result<Vec<RuntimeJobRecord>, String> {
        if req.job_kind.as_deref() != Some(AGENT_RUN_LIFECYCLE_JOB_KIND) {
            return self.claim_due_runtime_jobs(req);
        }
        self.with_client(|client| {
            let mut tx = client.transaction().map_err(|error| error.to_string())?;
            let host = self.dispatch_host_id();
            let sandbox_limit = self.dispatch_sandbox_limit();
            // A configured count quota serializes new start reservations on this host.
            let Some(clock) = tx.query_opt("SELECT (EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint WHERE $1::text IS NULL OR $2::bigint IS NULL OR pg_try_advisory_xact_lock(731950,hashtext($1))", &[&host, &sandbox_limit])
                .map_err(|error| error.to_string())? else { return Ok(Vec::new()) };
            let now = clock.get::<_, i64>(0);
            let due=super::capacity_wait::DUE_AT;
            let preparation = super::capacity_wait::preparation_candidate();
            let affinity = host_affinity("$5");
            let local_resident = local_resident("$5");
            let host_slots = host_start_slots("$5", "$6", "$1");
            let candidates = tx.query(&format!("WITH preparation AS MATERIALIZED ({preparation}), host_slots AS MATERIALIZED ({host_slots})
                SELECT j.job_id,CASE WHEN j.run_at_ms>$1 THEN p.generation END, resident.local,host_slots.available FROM runtime_jobs j
                JOIN execution_job_tenants t ON t.job_id=j.job_id
                LEFT JOIN preparation p ON p.job_id=j.job_id
                CROSS JOIN host_slots
                CROSS JOIN LATERAL (SELECT {local_resident} AS local) resident
                WHERE j.status='queued' AND j.job_kind='agent_run.lifecycle' AND {due}<=$1
                AND COALESCE(j.lease_reclaim_not_before_ms,{due})<=$1
                AND ($2::text IS NULL OR j.job_id=$2) AND ($3::text IS NULL OR j.session_id=$3)
                AND {affinity} AND ($5::text IS NULL OR resident.local OR host_slots.available IS NULL OR host_slots.available>0)
                AND NOT({})
                ORDER BY {due},j.created_at_ms,j.job_id FOR UPDATE OF j SKIP LOCKED LIMIT $4", super::lifecycle_control::PENDING),
                &[&now, &req.job_id, &req.session_id, &(req.limit as i64), &host, &sandbox_limit])
                .map_err(|error| error.to_string())?;
            if candidates.is_empty() {
                return Ok(Vec::new());
            }
            if host.is_some() {
                super::resource_commit::require_durable_resource_commit(&mut tx)?;
            }
            let until = now.checked_add(i64::try_from(req.lease_ms).map_err(|error| error.to_string())?)
                .ok_or("execution lease deadline overflow")?;
            let mut jobs = Vec::new();
            let mut slots = candidates[0].get::<_, Option<i64>>(3);
            for row in candidates {
                let id: String = row.get(0);
                let generation: Option<i64> = row.get(1);
                let needs_start = host.is_some() && !row.get::<_, bool>(2);
                if needs_start && slots == Some(0) {
                    continue;
                }
                // The fact is committed with the lease and survives the waiter
                // row being removed by the lease transition.
                let changed = tx.execute(
                    "WITH preparation_claim AS (
                        INSERT INTO runtime_events(event_id,session_id,task_id,event_type,at_ms,visibility,payload_json)
                        SELECT 'host_preparation_claim:'||$4::bigint,COALESCE(session_id,'runtime_job:'||job_id),
                            job_id,'runtime.host_preparation.claimed.v1',$2,'internal',
                            json_build_object('schema','runtime.host_preparation.claimed.v1','jobId',job_id,
                                'generation',$4::bigint,'claimedAtMs',$2::bigint)::text
                        FROM runtime_jobs WHERE job_id=$3 AND $4::bigint IS NOT NULL
                        ON CONFLICT(event_id) DO NOTHING RETURNING event_id
                     )
                     UPDATE runtime_jobs SET run_at_ms=LEAST(run_at_ms,$2),status='leased',
                        lease_owner=gen_random_uuid()::text,lease_expires_at_ms=$1,updated_at_ms=$2,
                        heartbeat_at_ms=$2,lease_purpose='execute',lease_control_stage=NULL,lease_host_id=$5
                     WHERE job_id=$3 AND status='queued'
                     AND ($4::bigint IS NULL OR EXISTS(SELECT 1 FROM preparation_claim))",
                    &[&until, &now, &id, &generation, &host],
                ).map_err(|error| error.to_string())?;
                if changed == 1 {
                    if needs_start { if let Some(remaining) = slots.as_mut() { *remaining -= 1; } }
                    jobs.push(load_job(&mut tx, &id)?.ok_or("claimed execution job missing")?);
                }
            }
            tx.commit().map_err(|error| error.to_string())?;
            Ok(jobs)
        })
    }
}
