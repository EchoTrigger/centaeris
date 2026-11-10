//! Durable native Host cleanup follows committed physical-removal facts.
use super::{reliability, PostgresRuntimeStore};
use centaeris_core::session::reliability::{
    ClaimDueRuntimeJobsRequest, CompleteRuntimeJobRequest, FailRuntimeJobRequest,
    RuntimeBackoffPolicy, RuntimeJobFailureDisposition, RuntimeJobRecord, RuntimeJobStatus,
    RuntimeJobStorePort, StartRuntimeJobRequest,
};
use hosted_execution::host_supervisor::HostLaunchTarget;
use postgres::GenericClient;
use sha2::{Digest, Sha256};

const BACKFILL_LIMIT: i64 = 64;
const ATTEMPTS_PER_PASS: usize = 4;

fn kind(host: &str) -> String {
    format!(
        "environment.host_retire:{:x}",
        Sha256::digest(host.as_bytes())
    )
}

fn target(
    db: &mut impl GenericClient,
    host: &str,
    instance: &str,
) -> Result<Option<HostLaunchTarget>, String> {
    let Some(row) = db.query_opt(
        "SELECT environment_id,profile_digest,image_digest FROM runtime.execution_instances i WHERE host_id=$1 AND instance_id=$2 AND allocation_execution_id=execution_id AND NOT EXISTS(SELECT 1 FROM runtime.resident_sandboxes s WHERE s.instance_id=i.instance_id)",
        &[&host, &instance],
    ).map_err(|_| "host_retirement_fact_unavailable")? else { return Ok(None) };
    let environment: String = row.get(0);
    let containers = db.query(
        "SELECT removed_container_id FROM runtime.agent_run_cleanup_receipts WHERE instance_id=$1 AND environment_id=$2 AND cleanup_complete AND physical_removed UNION SELECT r.container_id FROM runtime.environment_retirements r JOIN runtime.execution_instances i ON i.execution_id=r.allocation_execution_id AND i.instance_id=r.instance_id AND i.environment_id=r.environment_id AND i.host_id=r.host_id WHERE r.instance_id=$1 AND r.environment_id=$2 AND r.host_id=$3 AND r.state='destroyed'",
        &[&instance, &environment, &host],
    ).map_err(|_| "host_retirement_fact_unavailable")?;
    if containers.is_empty() {
        return Ok(None);
    }
    if containers.len() != 1 {
        return Err("host_retirement_fact_conflict".into());
    }
    Ok(Some(HostLaunchTarget {
        environment_instance_id: instance.into(),
        container_id: containers[0]
            .get::<_, Option<String>>(0)
            .ok_or("host_retirement_fact_conflict")?,
        environment_id: environment,
        profile_digest: row
            .get::<_, Option<String>>(1)
            .ok_or("host_retirement_fact_conflict")?,
        image_digest: row.get(2),
    }))
}

/// Called inside the same durable transaction that releases the physical holding.
pub(super) fn enqueue(
    db: &mut impl GenericClient,
    host: &str,
    instance: &str,
) -> Result<(), String> {
    let target = target(db, host, instance)?.ok_or("host_retirement_fact_unconfirmed")?;
    enqueue_target(db, host, target)
}

fn enqueue_target(
    db: &mut impl GenericClient,
    host: &str,
    target: HostLaunchTarget,
) -> Result<(), String> {
    let instance = target.environment_instance_id.as_str();
    let payload = serde_json::to_string(&target).map_err(|_| "host_retirement_target_invalid")?;
    ensure_job(db, host, instance, Some(payload))
}

fn ensure_job(
    db: &mut impl GenericClient,
    host: &str,
    instance: &str,
    payload: Option<String>,
) -> Result<(), String> {
    let now = crate::now_ms()?;
    let job = RuntimeJobRecord {
        job_id: format!("environment.host_retire:{instance}"),
        job_kind: kind(host),
        status: RuntimeJobStatus::Queued,
        run_at_ms: now,
        lease_owner: None,
        lease_expires_at_ms: None,
        heartbeat_at_ms: None,
        retry_count: 0,
        max_retries: u32::MAX,
        backoff_policy: RuntimeBackoffPolicy::default(),
        idempotency_key: instance.into(),
        session_id: None,
        branch_id: None,
        checkpoint_id: None,
        payload_ref: payload,
        output_refs: vec![],
        last_error: None,
        created_at_ms: now,
        updated_at_ms: now,
    };
    reliability::insert_job(db, &job)?;
    let existing = reliability::load_job(db, &job.job_id)?.ok_or("host_retirement_job_missing")?;
    if existing.job_kind != job.job_kind
        || existing.idempotency_key != job.idempotency_key
        || matches!((&existing.payload_ref, &job.payload_ref), (Some(existing), Some(expected)) if existing != expected)
    {
        return Err("host_retirement_job_conflict".into());
    }
    Ok(())
}

impl PostgresRuntimeStore {
    /// One keyset traversal per process startup, never a recurring history sweep.
    /// New completions enqueue atomically, including anchors passed by this cursor.
    pub(crate) fn backfill_host_retirements(
        &self,
        host: &str,
        after: &str,
    ) -> Result<Option<String>, String> {
        self.with_client(|db| {
            let mut tx = db.transaction().map_err(|_| "host_retirement_store_unavailable")?;
            super::resource_commit::require_durable_resource_commit(&mut tx)?;
            let rows = tx.query("SELECT instance_id,EXISTS(SELECT 1 FROM runtime.agent_run_cleanup_receipts c WHERE c.instance_id=i.instance_id AND c.environment_id=i.environment_id AND c.cleanup_complete AND c.physical_removed) OR EXISTS(SELECT 1 FROM runtime.environment_retirements r WHERE r.instance_id=i.instance_id AND r.environment_id=i.environment_id AND r.allocation_execution_id=i.execution_id AND r.host_id=i.host_id AND r.state='destroyed') FROM runtime.execution_instances i WHERE host_id=$1 AND allocation_execution_id=execution_id AND instance_id>$2 ORDER BY instance_id LIMIT $3", &[&host,&after,&BACKFILL_LIMIT]).map_err(|_| "host_retirement_fact_unavailable")?;
            for row in &rows {
                if !row.get::<_,bool>(1) { continue; }
                let instance: String = row.get(0);
                let mut item = tx.transaction().map_err(|_| "host_retirement_store_unavailable")?;
                match enqueue(&mut item,host,&instance) {
                    Ok(()) => item.commit().map_err(|_| "host_retirement_store_unavailable")?,
                    Err(_) => {
                        item.rollback().map_err(|_| "host_retirement_store_unavailable")?;
                        // Preserve the obligation without guessing a target. An
                        // existing non-null target is never overwritten here.
                        ensure_job(&mut tx,host,&instance,None)?;
                    }
                }
            }
            let cursor = (rows.len() == BACKFILL_LIMIT as usize).then(|| rows.last().expect("full page").get(0));
            tx.commit().map_err(|_| "host_retirement_store_unavailable")?;
            Ok(cursor)
        })
    }

    pub(crate) fn process_host_retirements(
        &self,
        host: &str,
        mut retire: impl FnMut(HostLaunchTarget) -> Result<(), String>,
    ) -> Result<usize, String> {
        let job_kind = kind(host);
        let now = crate::now_ms()?;
        // Recover only this Host's obligations. The generic finite lease-reclaim
        // policy must never turn physical cleanup into a forgotten dead letter.
        self.with_client(|db| {
            db.execute("UPDATE runtime.runtime_jobs SET status='queued',run_at_ms=$1,lease_owner=NULL,lease_expires_at_ms=NULL,lease_reclaim_not_before_ms=NULL,updated_at_ms=$1 WHERE job_id IN(SELECT job_id FROM runtime.runtime_jobs WHERE job_kind=$2 AND (status IN('failed','cancelled','dead_lettered') OR (status IN('leased','running') AND lease_expires_at_ms<=$1)) ORDER BY job_id FOR UPDATE SKIP LOCKED LIMIT 64)", &[&now,&job_kind]).map_err(|_| "host_retirement_store_unavailable")?;
            Ok(())
        })?;
        let mut attempted = 0;
        for _ in 0..ATTEMPTS_PER_PASS {
            let Some(job) = self
                .claim_due_runtime_jobs(ClaimDueRuntimeJobsRequest {
                    now_ms: crate::now_ms()?,
                    worker_id: job_kind.clone(),
                    job_id: None,
                    job_kind: Some(job_kind.clone()),
                    session_id: None,
                    limit: 1,
                    lease_ms: 90_000,
                })?
                .pop()
            else {
                break;
            };
            let owner = job.lease_owner.ok_or("host_retirement_lease_missing")?;
            self.start_runtime_job(StartRuntimeJobRequest {
                job_id: job.job_id.clone(),
                lease_owner: owner.clone(),
                started_at_ms: crate::now_ms()?,
            })?;
            let outcome = (|| {
                let expected = self.with_client(|db| {
                    let mut tx = db.transaction().map_err(|_| "host_retirement_store_unavailable")?;
                    super::resource_commit::require_durable_resource_commit(&mut tx)?;
                    let current = target(&mut tx,host,&job.idempotency_key)?.ok_or("host_retirement_fact_unconfirmed")?;
                    current.validate().map_err(|_| "host_retirement_target_invalid")?;
                    if let Some(payload) = &job.payload_ref {
                        let expected: HostLaunchTarget = serde_json::from_str(payload).map_err(|_| "host_retirement_target_invalid")?;
                        if expected != current { return Err("host_retirement_fact_unconfirmed".into()); }
                    } else {
                        let payload = serde_json::to_string(&current).map_err(|_| "host_retirement_target_invalid")?;
                        let changed = tx.execute("UPDATE runtime.runtime_jobs SET payload_ref=$1 WHERE job_id=$2 AND lease_owner=$3 AND status='running' AND lease_expires_at_ms>$4 AND payload_ref IS NULL", &[&payload,&job.job_id,&owner,&crate::now_ms()?]).map_err(|_| "host_retirement_store_unavailable")?;
                        if changed != 1 { return Err("host_retirement_lease_rejected".into()); }
                    }
                    tx.commit().map_err(|_| "host_retirement_store_unavailable")?;
                    Ok(current)
                })?;
                retire(expected)
            })();
            let finished = crate::now_ms()?;
            match outcome {
                Ok(()) => self.complete_runtime_job(CompleteRuntimeJobRequest {
                    job_id: job.job_id,
                    lease_owner: owner,
                    output_refs: vec![],
                    completed_at_ms: finished,
                })?,
                Err(_) => self.fail_runtime_job(FailRuntimeJobRequest {
                    job_id: job.job_id,
                    lease_owner: owner,
                    failed_at_ms: finished,
                    last_error: "environment_host_retirement_pending".into(),
                    next_run_at_ms: Some(finished.saturating_add(10_000)),
                    disposition: RuntimeJobFailureDisposition::RetryScheduled,
                })?,
            }
            attempted += 1;
        }
        Ok(attempted)
    }
}
