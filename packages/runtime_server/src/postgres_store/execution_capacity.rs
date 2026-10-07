use std::collections::HashMap;

use centaeris_core::session::reliability::{
    ClaimDueRuntimeJobsRequest, RuntimeJobRecord, RuntimeJobStorePort,
    ScheduleRuntimeJobDisposition, ScheduleRuntimeJobRequest, ScheduleRuntimeJobResult,
    AGENT_RUN_LIFECYCLE_JOB_KIND,
};

use super::reliability::{insert_job, load_job, load_job_by_key};
use super::PostgresRuntimeStore;
use crate::execution_capacity::ExecutionCapacity;

// Only active execution leases participate. Historical terminal jobs never
// enter the aggregation. An expired lease occupies capacity until reconciled.
pub(super) const ACTIVE_EXECUTIONS: &str = "WITH active AS MATERIALIZED (
    SELECT t.workspace_id FROM runtime_jobs j
    LEFT JOIN execution_job_tenants t ON t.job_id=j.job_id
    WHERE j.job_kind='agent_run.lifecycle' AND j.status IN ('leased','running')
), tenant_usage AS MATERIALIZED (
    SELECT workspace_id,COUNT(*) AS used FROM active GROUP BY workspace_id
)";

impl PostgresRuntimeStore {
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
        let limits = ExecutionCapacity::from_env()?;
        self.with_client(|client| {
            let mut tx = client.transaction().map_err(|error| error.to_string())?;
            // All hosted lifecycle claims share this transaction lock across
            // replicas. Contention yields immediately rather than queuing SQL.
            if !tx.query_one("SELECT pg_try_advisory_xact_lock(731946,2)", &[])
                .map_err(|error| error.to_string())?.get::<_, bool>(0) {
                return Err("execution claim busy".to_string());
            }
            let rows = tx.query(&format!("{ACTIVE_EXECUTIONS} SELECT workspace_id,used FROM tenant_usage"), &[])
                .map_err(|error| error.to_string())?;
            let mut used = 0usize;
            let mut tenants = HashMap::<String, usize>::new();
            for row in rows {
                let count = usize::try_from(row.get::<_, i64>(1)).map_err(|error| error.to_string())?;
                used = used.saturating_add(count);
                if let Some(tenant) = row.get::<_, Option<String>>(0) { tenants.insert(tenant, count); }
            }
            let available = limits.global.saturating_sub(used).min(req.limit);
            if available == 0 { return Ok(Vec::new()); }
            let full = tenants.iter().filter(|(_, used)| **used >= limits.tenant)
                .map(|(tenant, _)| tenant.clone()).collect::<Vec<_>>();
            let now = tx.query_one("SELECT (EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint", &[])
                .map_err(|error| error.to_string())?.get::<_, i64>(0);
            let candidates = tx.query("SELECT j.job_id,t.workspace_id FROM runtime_jobs j
                JOIN execution_job_tenants t ON t.job_id=j.job_id
                WHERE j.status='queued' AND j.job_kind='agent_run.lifecycle' AND j.run_at_ms<=$1
                AND ($2::text IS NULL OR j.job_id=$2) AND ($3::text IS NULL OR j.session_id=$3)
                AND NOT(t.workspace_id=ANY($4))
                ORDER BY j.run_at_ms,j.created_at_ms,j.job_id FOR UPDATE OF j SKIP LOCKED LIMIT $5",
                &[&now, &req.job_id, &req.session_id, &full, &(available as i64)])
                .map_err(|error| error.to_string())?;
            let until = now.checked_add(i64::try_from(req.lease_ms).map_err(|error| error.to_string())?)
                .ok_or("execution lease deadline overflow")?;
            let mut jobs = Vec::new();
            for row in candidates {
                let id: String = row.get(0);
                let tenant: String = row.get(1);
                let tenant_used = tenants.entry(tenant).or_default();
                if *tenant_used >= limits.tenant { continue; }
                tx.execute("UPDATE runtime_jobs SET status='leased',lease_owner=gen_random_uuid()::text,lease_expires_at_ms=$1,updated_at_ms=$2,heartbeat_at_ms=$2 WHERE job_id=$3 AND status='queued'",
                    &[&until,&now,&id]).map_err(|error| error.to_string())?;
                *tenant_used += 1;
                jobs.push(load_job(&mut tx, &id)?.ok_or("claimed execution job missing")?);
            }
            tx.commit().map_err(|error| error.to_string())?;
            Ok(jobs)
        })
    }
}
