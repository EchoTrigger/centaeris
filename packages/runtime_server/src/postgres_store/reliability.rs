use std::time::{Duration, Instant};

use postgres::fallible_iterator::FallibleIterator;
use postgres::{Client, GenericClient, Row};

use centaeris_core::session::reliability::{
    AcquireResourceClaimDisposition, AcquireResourceClaimRequest, AcquireResourceClaimResult,
    CancelRuntimeJobRequest, ClaimDueRuntimeJobsRequest, CompleteRuntimeJobRequest,
    CreateDeadLetterDisposition, CreateDeadLetterRequest, CreateDeadLetterResult, DeadLetterRecord,
    DeadLetterReplayPolicy, DeadLetterStatus, DeadLetterStorePort, DismissDeadLetterRequest,
    FailRuntimeJobRequest, ListDeadLettersRequest, ListRuntimeJobsRequest,
    MarkDeadLetterReplayedRequest, MarkDeadLetterReplayingRequest, ReleaseResourceClaimRequest,
    RenewRuntimeJobLeaseRequest, ReplayDeadLetterRequest, ReplayDeadLetterResult,
    ResourceClaimRecord, ResourceClaimStorePort, RuntimeJobFailureDisposition,
    RuntimeJobOutboxPort, RuntimeJobOutboxPublishDisposition, RuntimeJobOutboxRecord,
    RuntimeJobRecord, RuntimeJobStatus, RuntimeJobStorePort, ScheduleRuntimeJobDisposition,
    ScheduleRuntimeJobRequest, ScheduleRuntimeJobResult, StartRuntimeJobRequest,
    WakeRuntimeJobDisposition, WakeRuntimeJobRequest, YieldRuntimeJobRequest,
    RUNTIME_JOB_TERMINAL_EVENT,
};

use super::runtime::to_i64;
use super::PostgresRuntimeStore;

const JOB_COLUMNS:&str="job_id,job_kind,status,run_at_ms,lease_owner,lease_expires_at_ms,retry_count,max_retries,backoff_policy_json,idempotency_key,session_id,branch_id,checkpoint_id,payload_ref,output_refs_json,last_error,created_at_ms,updated_at_ms,heartbeat_at_ms";
const DL_COLUMNS:&str="dead_letter_id,original_job_id,job_kind,status,session_id,branch_id,checkpoint_id,payload_ref,idempotency_key,failure_reason,last_error,attempts,first_failed_at_ms,last_failed_at_ms,replay_policy_json,replayed_job_id,dismissed_by,dismissed_reason,updated_at_ms";

pub(crate) struct RuntimeJobWaitResult {
    pub(crate) ready: bool,
    pub(crate) next_run_at_ms: Option<i64>,
}

impl PostgresRuntimeStore {
    pub(crate) fn get_runtime_job_execution_control(
        &self,
        id: &str,
    ) -> Result<Option<RuntimeJobRecord>, String> {
        self.with_execution_control_client(|client| load_job(client, id))
    }

    pub(crate) fn wait_for_runtime_jobs(
        &self,
        job_kinds: &[String],
        max_wait: Duration,
    ) -> Result<RuntimeJobWaitResult, String> {
        self.with_listener_client(|client| {
            let max_wait = super::remaining_request_time()?
                .map_or(max_wait, |remaining| max_wait.min(remaining));
            client
                .batch_execute("LISTEN runtime_job_ready_v1")
                .map_err(|error| format!("listen for Postgres runtime jobs failed: {error}"))?;
            let deadline = Instant::now() + max_wait;
            loop {
                let (now_ms, next_run_at_ms) = next_runtime_job(client, job_kinds)?;
                if next_run_at_ms.is_some_and(|run_at_ms| run_at_ms <= now_ms) {
                    return Ok(RuntimeJobWaitResult {
                        ready: true,
                        next_run_at_ms,
                    });
                }
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Ok(RuntimeJobWaitResult {
                        ready: false,
                        next_run_at_ms,
                    });
                }
                let until_next_job = next_run_at_ms
                    .map(|run_at_ms| {
                        Duration::from_millis(
                            u64::try_from(run_at_ms.saturating_sub(now_ms).max(1))
                                .unwrap_or(u64::MAX),
                        )
                    })
                    .unwrap_or(remaining);
                // A timeout or disconnect returns no notification. Re-querying either
                // observes due work or exposes the broken connection to the worker.
                let _ = client
                    .notifications()
                    .timeout_iter(remaining.min(until_next_job))
                    .next()
                    .map_err(|error| {
                        format!("wait for Postgres runtime job notification failed: {error}")
                    })?;
            }
        })
    }
}

fn next_runtime_job(
    client: &mut Client,
    job_kinds: &[String],
) -> Result<(i64, Option<i64>), String> {
    let limits = crate::execution_capacity::ExecutionCapacity::from_env()?;
    let row = client
        .query_one(
            &format!("{} SELECT (EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint,MIN(j.run_at_ms)
                FROM runtime_jobs j LEFT JOIN execution_job_tenants t ON t.job_id=j.job_id
                LEFT JOIN tenant_usage u ON u.workspace_id=t.workspace_id
                WHERE j.status='queued' AND j.job_kind=ANY($1)
                AND (j.job_kind<>'agent_run.lifecycle' OR
                    (t.job_id IS NOT NULL AND (SELECT COUNT(*) FROM active)<$2 AND COALESCE(u.used,0)<$3))",
                super::execution_capacity::ACTIVE_EXECUTIONS),
            &[&job_kinds, &(limits.global as i64), &(limits.tenant as i64)],
        )
        .map_err(|error| format!("query next Postgres runtime job failed: {error}"))?;
    Ok((row.get(0), row.get(1)))
}

impl RuntimeJobStorePort for PostgresRuntimeStore {
    fn schedule_runtime_job(
        &self,
        req: ScheduleRuntimeJobRequest,
    ) -> Result<ScheduleRuntimeJobResult, String> {
        self.with_client(|c| {
            let mut tx = c
                .transaction()
                .map_err(|e| format!("begin Postgres job schedule failed: {e}"))?;
            let inserted = insert_job(&mut tx, &req.job)?;
            let job = if inserted {
                req.job
            } else {
                load_job_by_key(&mut tx, &req.job.job_kind, &req.job.idempotency_key)?
                    .ok_or("existing Postgres runtime job missing")?
            };
            tx.commit()
                .map_err(|e| format!("commit Postgres job schedule failed: {e}"))?;
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
    fn get_runtime_job(&self, id: &str) -> Result<Option<RuntimeJobRecord>, String> {
        self.with_client(|c| load_job(c, id))
    }
    fn list_runtime_jobs(
        &self,
        req: ListRuntimeJobsRequest,
    ) -> Result<Vec<RuntimeJobRecord>, String> {
        let statuses = if req.statuses.is_empty() {
            None
        } else {
            Some(
                req.statuses
                    .iter()
                    .map(job_status)
                    .map(str::to_string)
                    .collect::<Vec<_>>(),
            )
        };
        self.with_client(|c|c.query(format!("SELECT {JOB_COLUMNS} FROM runtime_jobs WHERE ($1::text[] IS NULL OR status=ANY($1)) AND ($2::text IS NULL OR job_kind=$2) AND ($3::text IS NULL OR session_id=$3) AND ($4::text IS NULL OR branch_id=$4) ORDER BY run_at_ms,created_at_ms,job_id LIMIT $5 OFFSET $6").as_str(), &[&statuses,&req.job_kind,&req.session_id,&req.branch_id,&to_i64(req.limit)?,&to_i64(req.offset)?]).map_err(|e|format!("list Postgres runtime jobs failed: {e}"))?.iter().map(row_job).collect())
    }
    fn claim_due_runtime_jobs(
        &self,
        req: ClaimDueRuntimeJobsRequest,
    ) -> Result<Vec<RuntimeJobRecord>, String> {
        let until = req
            .now_ms
            .saturating_add(i64::try_from(req.lease_ms).map_err(|_| "lease overflow".to_string())?);
        self.with_client(|c|{let mut tx=c.transaction().map_err(|e|format!("begin Postgres job claim failed: {e}"))?;let ids=tx.query("SELECT job_id FROM runtime_jobs WHERE status='queued' AND run_at_ms<=$1 AND ($2::text IS NULL OR job_id=$2) AND ($3::text IS NULL OR job_kind=$3) AND ($4::text IS NULL OR session_id=$4) ORDER BY run_at_ms,created_at_ms,job_id FOR UPDATE SKIP LOCKED LIMIT $5", &[&req.now_ms,&req.job_id,&req.job_kind,&req.session_id,&to_i64(req.limit)?]).map_err(|e|format!("select Postgres due jobs failed: {e}"))?.iter().map(|r|r.get::<_,String>(0)).collect::<Vec<_>>();if !ids.is_empty(){tx.execute("UPDATE runtime_jobs SET status='leased',lease_owner=gen_random_uuid()::text,lease_expires_at_ms=$1,updated_at_ms=$2,heartbeat_at_ms=$2 WHERE job_id=ANY($3) AND status='queued'",&[&until,&req.now_ms,&ids]).map_err(|e|format!("claim Postgres jobs failed: {e}"))?;}let jobs=load_jobs(&mut tx,&ids)?;tx.commit().map_err(|e|format!("commit Postgres job claim failed: {e}"))?;Ok(jobs)})
    }
    fn start_runtime_job(&self, r: StartRuntimeJobRequest) -> Result<(), String> {
        self.with_client(|c|updated(c.execute("UPDATE runtime_jobs SET status='running',updated_at_ms=$1,heartbeat_at_ms=$1 WHERE job_id=$2 AND status='leased' AND lease_owner=$3 AND lease_expires_at_ms>$1", &[&r.started_at_ms,&r.job_id,&r.lease_owner]),"start runtime job"))
    }
    fn renew_runtime_job_lease(&self, r: RenewRuntimeJobLeaseRequest) -> Result<(), String> {
        let until = r
            .heartbeat_at_ms
            .saturating_add(i64::try_from(r.lease_ms).map_err(|_| "lease overflow".to_string())?);
        self.with_execution_control_client(|c|updated(c.execute("UPDATE runtime_jobs SET heartbeat_at_ms=$1,lease_expires_at_ms=$2,updated_at_ms=$1 WHERE job_id=$3 AND status IN('leased','running') AND lease_owner=$4 AND lease_expires_at_ms>$1", &[&r.heartbeat_at_ms,&until,&r.job_id,&r.lease_owner]),"renew runtime job lease"))
    }
    fn yield_runtime_job(&self, r: YieldRuntimeJobRequest) -> Result<(), String> {
        if r.run_at_ms < r.yielded_at_ms || r.transition_reason.trim().is_empty() {
            return Err("invalid runtime job yield".to_string());
        }
        self.with_client(|client| {
            let mut tx = client
                .transaction()
                .map_err(|error| format!("begin Postgres runtime job yield failed: {error}"))?;
            let row = tx
                .query_opt(
                    "SELECT status,lease_owner,lease_expires_at_ms,run_at_ms,session_id FROM runtime_jobs WHERE job_id=$1 FOR UPDATE",
                    &[&r.job_id],
                )
                .map_err(|error| format!("lock Postgres runtime job for yield failed: {error}"))?
                .ok_or_else(|| format!("yield runtime job not found: {}", r.job_id))?;
            let status: String = row.get(0);
            let lease_owner: Option<String> = row.get(1);
            let lease_expires_at_ms: Option<i64> = row.get(2);
            let _stored_run_at_ms: i64 = row.get(3);
            let session_id: Option<String> = row.get(4);
            let event_id = format!(
                "runtime_job_yield:{}:{}:{}",
                r.job_id, r.lease_owner, r.yielded_at_ms
            );
            let payload = serde_json::json!({
                "schema": "runtime.job.yielded.v1",
                "jobId": r.job_id,
                "leaseOwner": r.lease_owner,
                "yieldedAtMs": r.yielded_at_ms,
                "runAtMs": r.run_at_ms,
                "transitionReason": r.transition_reason,
            })
            .to_string();
            if status == "queued" && lease_owner.is_none() {
                let existing = tx
                    .query_opt(
                        "SELECT event_id,payload_json FROM runtime_events WHERE task_id=$1 AND event_type='runtime_job_yielded' AND at_ms=$2",
                        &[&r.job_id, &r.yielded_at_ms],
                    )
                    .map_err(|error| format!("load yielded runtime event failed: {error}"))?
                    .map(|row| (row.get::<_, String>(0), row.get::<_, String>(1)));
                match existing {
                    Some((existing_id, existing_payload))
                        if existing_id == event_id && existing_payload == payload =>
                    {
                        tx.commit().map_err(|error| format!("commit idempotent Postgres runtime job yield failed: {error}"))?;
                        return Ok(());
                    }
                    Some(_) => {
                        return Err(format!(
                            "yield runtime job idempotency conflict: {}",
                            r.job_id
                        ));
                    }
                    None => {
                        return Err(format!(
                            "yield runtime job lease mismatch or expired: {}",
                            r.job_id
                        ));
                    }
                }
            }
            if !matches!(status.as_str(), "leased" | "running")
                || lease_owner.as_deref() != Some(r.lease_owner.as_str())
                || lease_expires_at_ms.is_none_or(|expires| expires <= r.yielded_at_ms)
            {
                return Err(format!("yield runtime job lease mismatch or expired: {}", r.job_id));
            }
            let pending_wake_ids = tx
                .query(
                    "SELECT wake.event_id FROM runtime_events AS wake WHERE wake.task_id=$1 AND wake.event_type='runtime_job_wake_requested' AND NOT EXISTS(SELECT 1 FROM runtime_events AS consumed WHERE consumed.event_id='runtime_job_wake_consumed:' || substring(wake.event_id FROM char_length('runtime_job_wake:')+1)) ORDER BY wake.at_ms,wake.event_id",
                    &[&r.job_id],
                )
                .map_err(|error| format!("query pending Postgres runtime job wakes failed: {error}"))?
                .into_iter()
                .map(|row| row.get::<_, String>(0))
                .collect::<Vec<_>>();
            let effective_run_at_ms = if pending_wake_ids.is_empty() {
                r.run_at_ms
            } else {
                r.yielded_at_ms
            };
            updated(
                tx.execute(
                    "UPDATE runtime_jobs SET status='queued',run_at_ms=$1,updated_at_ms=$2,lease_owner=NULL,lease_expires_at_ms=NULL,heartbeat_at_ms=NULL WHERE job_id=$3 AND status IN('leased','running') AND lease_owner=$4 AND lease_expires_at_ms>$2",
                    &[&effective_run_at_ms, &r.yielded_at_ms, &r.job_id, &r.lease_owner],
                ),
                "yield runtime job",
            )?;
            let session_id =
                session_id.unwrap_or_else(|| format!("runtime_job:{}", r.job_id));
            tx.execute(
                "INSERT INTO runtime_events(event_id,session_id,task_id,event_type,at_ms,visibility,payload_json) VALUES($1,$2,$3,'runtime_job_yielded',$4,'internal',$5)",
                &[&event_id, &session_id, &r.job_id, &r.yielded_at_ms, &payload],
            )
            .map_err(|error| format!("append Postgres runtime job yield event failed: {error}"))?;
            for wake_event_id in pending_wake_ids {
                let consumed_event_id = wake_event_id.replacen(
                    "runtime_job_wake:",
                    "runtime_job_wake_consumed:",
                    1,
                );
                let consumed_payload = serde_json::json!({
                    "schema": "runtime.job.wake_consumed.v1",
                    "jobId": r.job_id,
                    "wakeEventId": wake_event_id,
                    "consumedAtMs": r.yielded_at_ms,
                    "transitionReason": "yield_observed_pending_wake",
                })
                .to_string();
                tx.execute(
                    "INSERT INTO runtime_events(event_id,session_id,task_id,event_type,at_ms,visibility,payload_json) VALUES($1,$2,$3,'runtime_job_wake_consumed',$4,'internal',$5)",
                    &[&consumed_event_id, &session_id, &r.job_id, &r.yielded_at_ms, &consumed_payload],
                )
                .map_err(|error| format!("consume pending Postgres runtime job wake failed: {error}"))?;
            }
            tx.commit()
                .map_err(|error| format!("commit Postgres runtime job yield failed: {error}"))
        })
    }
    fn wake_runtime_job(
        &self,
        r: WakeRuntimeJobRequest,
    ) -> Result<WakeRuntimeJobDisposition, String> {
        if r.job_id.trim().is_empty()
            || r.source_job_id.trim().is_empty()
            || r.transition_reason.trim().is_empty()
        {
            return Err("invalid runtime job wake".to_string());
        }
        self.with_client(|client| {
            let mut tx = client
                .transaction()
                .map_err(|error| format!("begin Postgres runtime job wake failed: {error}"))?;
            let row = tx
                .query_opt(
                    "SELECT status,run_at_ms,session_id FROM runtime_jobs WHERE job_id=$1 FOR UPDATE",
                    &[&r.job_id],
                )
                .map_err(|error| format!("load Postgres runtime job for wake failed: {error}"))?
                .ok_or_else(|| format!("wake runtime job not found: {}", r.job_id))?;
            let status = row.get::<_, String>(0);
            let run_at_ms = row.get::<_, i64>(1);
            let session_id = row.get::<_, Option<String>>(2);
            let event_id = format!("runtime_job_wake:{}:{}", r.job_id, r.source_job_id);
            let payload = serde_json::json!({
                "schema": "runtime.job.wake.v1",
                "jobId": r.job_id,
                "sourceJobId": r.source_job_id,
                "wokenAtMs": r.woken_at_ms,
                "transitionReason": r.transition_reason,
            })
            .to_string();
            let existing = tx
                .query_opt(
                    "SELECT payload_json FROM runtime_events WHERE event_id=$1",
                    &[&event_id],
                )
                .map_err(|error| format!("load Postgres runtime job wake event failed: {error}"))?
                .map(|row| row.get::<_, String>(0));
            if existing.as_deref().is_some_and(|value| value != payload) {
                return Err(format!(
                    "wake runtime job idempotency conflict: {}",
                    r.job_id
                ));
            }
            if existing.is_none() {
                tx.execute(
                    "INSERT INTO runtime_events(event_id,session_id,task_id,event_type,at_ms,visibility,payload_json) VALUES($1,$2,$3,'runtime_job_wake_requested',$4,'internal',$5)",
                    &[&event_id, &session_id.unwrap_or_else(|| format!("runtime_job:{}", r.job_id)), &r.job_id, &r.woken_at_ms, &payload],
                )
                .map_err(|error| format!("append Postgres runtime job wake event failed: {error}"))?;
            }
            let disposition = match status.as_str() {
                "queued" => {
                    if run_at_ms > r.woken_at_ms {
                        updated(
                            tx.execute(
                                "UPDATE runtime_jobs SET run_at_ms=$1,updated_at_ms=$1 WHERE job_id=$2 AND status='queued' AND run_at_ms>$1",
                                &[&r.woken_at_ms, &r.job_id],
                            ),
                            "wake Postgres runtime job",
                        )?;
                        WakeRuntimeJobDisposition::Woken
                    } else {
                        WakeRuntimeJobDisposition::AlreadyRunnable
                    }
                }
                "leased" | "running" => WakeRuntimeJobDisposition::Active,
                "succeeded" | "failed" | "dead_lettered" | "cancelled" => {
                    WakeRuntimeJobDisposition::Terminal
                }
                other => return Err(format!("wake runtime job unsupported status: {other}")),
            };
            tx.commit()
                .map_err(|error| format!("commit Postgres runtime job wake failed: {error}"))?;
            Ok(disposition)
        })
    }
    fn complete_runtime_job(&self, r: CompleteRuntimeJobRequest) -> Result<(), String> {
        let out = serde_json::to_string(&r.output_refs).map_err(|e| e.to_string())?;
        self.with_client(|client| {
            let mut tx = client
                .transaction()
                .map_err(|e| format!("begin Postgres complete runtime job failed: {e}"))?;
            updated(tx.execute("UPDATE runtime_jobs SET status='succeeded',output_refs_json=$1,updated_at_ms=$2,lease_owner=NULL,lease_expires_at_ms=NULL,last_error=NULL WHERE job_id=$3 AND status IN('leased','running') AND lease_owner=$4 AND lease_expires_at_ms>$2", &[&out,&r.completed_at_ms,&r.job_id,&r.lease_owner]),"complete runtime job")?;
            upsert_job_outbox(&mut tx, r.job_id.as_str(), RUNTIME_JOB_TERMINAL_EVENT)?;
            tx.commit()
                .map_err(|e| format!("commit Postgres complete runtime job failed: {e}"))
        })
    }
    fn fail_runtime_job(&self, r: FailRuntimeJobRequest) -> Result<(), String> {
        let (status, run) = match r.disposition {
            RuntimeJobFailureDisposition::RetryScheduled => (
                "queued",
                r.next_run_at_ms.ok_or("retry requires next_run_at_ms")?,
            ),
            RuntimeJobFailureDisposition::Failed => ("failed", r.failed_at_ms),
            RuntimeJobFailureDisposition::DeadLettered => ("dead_lettered", r.failed_at_ms),
        };
        self.with_client(|client| {
            let mut tx = client
                .transaction()
                .map_err(|e| format!("begin Postgres fail runtime job failed: {e}"))?;
            updated(tx.execute("UPDATE runtime_jobs SET status=$1,run_at_ms=$2,retry_count=retry_count+1,updated_at_ms=$3,lease_owner=NULL,lease_expires_at_ms=NULL,last_error=$4 WHERE job_id=$5 AND status IN('leased','running') AND lease_owner=$6 AND lease_expires_at_ms>$3", &[&status,&run,&r.failed_at_ms,&r.last_error,&r.job_id,&r.lease_owner]),"fail runtime job")?;
            if !matches!(r.disposition, RuntimeJobFailureDisposition::RetryScheduled) {
                upsert_job_outbox(&mut tx, r.job_id.as_str(), RUNTIME_JOB_TERMINAL_EVENT)?;
            }
            tx.commit()
                .map_err(|e| format!("commit Postgres fail runtime job failed: {e}"))
        })
    }
    fn cancel_runtime_job(&self, r: CancelRuntimeJobRequest) -> Result<(), String> {
        let expected = r.expected_status.as_ref().map(job_status);
        self.with_client(|client| {
            let mut tx = client
                .transaction()
                .map_err(|e| format!("begin Postgres cancel runtime job failed: {e}"))?;
            updated(tx.execute("UPDATE runtime_jobs SET status='cancelled',updated_at_ms=$1,lease_owner=NULL,lease_expires_at_ms=NULL,last_error=COALESCE(NULLIF(last_error,''),$2) WHERE job_id=$3 AND status NOT IN('succeeded','failed','dead_lettered','cancelled') AND ($4::text IS NULL OR status=$4) AND ($5::text IS NULL OR (status IN('leased','running') AND lease_owner=$5 AND lease_expires_at_ms>$1))", &[&r.cancelled_at_ms,&r.reason,&r.job_id,&expected,&r.expected_lease_owner]),"cancel runtime job")?;
            upsert_job_outbox(&mut tx, r.job_id.as_str(), RUNTIME_JOB_TERMINAL_EVENT)?;
            tx.commit()
                .map_err(|e| format!("commit Postgres cancel runtime job failed: {e}"))
        })
    }
    fn reclaim_expired_runtime_job_leases(&self, now: i64) -> Result<usize, String> {
        self.with_client(|c|c.execute("UPDATE runtime_jobs SET status='queued',run_at_ms=$1,updated_at_ms=$1,lease_owner=NULL,lease_expires_at_ms=NULL,last_error=CASE WHEN COALESCE(last_error,'')='' AND status='running' THEN 'worker_crashed_reclaimed' WHEN COALESCE(last_error,'')='' THEN 'lease_expired_reclaimed' ELSE last_error END WHERE status IN('leased','running') AND lease_expires_at_ms<=$1", &[&now]).map(|n|n as usize).map_err(|e|format!("reclaim Postgres job leases failed: {e}")))
    }
}

impl RuntimeJobOutboxPort for PostgresRuntimeStore {
    fn list_pending_runtime_job_outbox(
        &self,
        limit: usize,
    ) -> Result<Vec<RuntimeJobOutboxRecord>, String> {
        self.with_client(|client| client.query("SELECT job_id,event_type,published_at_ms,generation FROM runtime_job_outbox WHERE published_at_ms IS NULL ORDER BY job_id,event_type LIMIT $1", &[&to_i64(limit)?]).map_err(|error|format!("list Postgres runtime job outbox failed: {error}"))?.into_iter().map(|row| {
            let generation:i64=row.get(3);
            Ok(RuntimeJobOutboxRecord{job_id:row.get(0),event_type:row.get(1),published_at_ms:row.get(2),generation:u32::try_from(generation).map_err(|_|"outbox generation overflow".to_string())?})
        }).collect())
    }
    fn mark_runtime_job_outbox_published(
        &self,
        job_id: &str,
        event_type: &str,
        generation: u32,
        published_at_ms: i64,
    ) -> Result<RuntimeJobOutboxPublishDisposition, String> {
        self.with_client(|client| {
            let generation = i64::from(generation);
            let updated = client
                .execute(
                    "UPDATE runtime_job_outbox SET published_at_ms=$1 WHERE job_id=$2 AND event_type=$3 AND generation=$4 AND published_at_ms IS NULL",
                    &[&published_at_ms, &job_id, &event_type, &generation],
                )
                .map_err(|error| format!("mark Postgres runtime job outbox published failed: {error}"))?;
            if updated == 1 {
                return Ok(RuntimeJobOutboxPublishDisposition::Published);
            }
            let stored = client
                .query_opt(
                    "SELECT generation,published_at_ms FROM runtime_job_outbox WHERE job_id=$1 AND event_type=$2",
                    &[&job_id, &event_type],
                )
                .map_err(|error| format!("load Postgres runtime job outbox publish state failed: {error}"))?
                .ok_or_else(|| "Postgres runtime job outbox row not found".to_string())?;
            let stored_generation = stored.get::<_, i64>(0);
            let stored_published_at_ms = stored.get::<_, Option<i64>>(1);
            if stored_generation != generation {
                Ok(RuntimeJobOutboxPublishDisposition::Stale)
            } else if stored_published_at_ms.is_some() {
                Ok(RuntimeJobOutboxPublishDisposition::AlreadyPublished)
            } else {
                Err("Postgres runtime job outbox publish CAS failed".to_string())
            }
        })
    }
}

impl ResourceClaimStorePort for PostgresRuntimeStore {
    fn acquire_resource_claim(
        &self,
        r: AcquireResourceClaimRequest,
    ) -> Result<AcquireResourceClaimResult, String> {
        let expires = r
            .now_ms
            .saturating_add(i64::try_from(r.ttl_ms).map_err(|_| "claim ttl overflow".to_string())?);
        self.with_client(|c|{let mut tx=c.transaction().map_err(|e|format!("begin Postgres claim failed: {e}"))?;let old=load_claim_for_update(&mut tx,&r.resource_kind,&r.resource_key)?;let(disposition,claim)=match old{Some(x) if x.expires_at_ms>r.now_ms&&x.owner!=r.owner=>(AcquireResourceClaimDisposition::Conflict,x),Some(mut x) if x.expires_at_ms>r.now_ms=>{tx.execute("UPDATE resource_claims SET expires_at_ms=$1,metadata_json=$2,updated_at_ms=$3 WHERE resource_kind=$4 AND resource_key=$5 AND owner=$6", &[&expires,&r.metadata_json,&r.now_ms,&r.resource_kind,&r.resource_key,&r.owner]).map_err(|e|format!("refresh Postgres claim failed: {e}"))?;x.expires_at_ms=expires;x.metadata_json=r.metadata_json;x.updated_at_ms=r.now_ms;(AcquireResourceClaimDisposition::AlreadyOwned,x)},_=>{tx.execute("INSERT INTO resource_claims(resource_kind,resource_key,owner,owner_kind,session_id,branch_id,expires_at_ms,metadata_json,created_at_ms,updated_at_ms) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$9) ON CONFLICT(resource_kind,resource_key) DO UPDATE SET owner=excluded.owner,owner_kind=excluded.owner_kind,session_id=excluded.session_id,branch_id=excluded.branch_id,expires_at_ms=excluded.expires_at_ms,metadata_json=excluded.metadata_json,updated_at_ms=excluded.updated_at_ms", &[&r.resource_kind,&r.resource_key,&r.owner,&r.owner_kind,&r.session_id,&r.branch_id,&expires,&r.metadata_json,&r.now_ms]).map_err(|e|format!("upsert Postgres claim failed: {e}"))?;(AcquireResourceClaimDisposition::Acquired,load_claim(&mut tx,&r.resource_kind,&r.resource_key)?.ok_or("inserted claim missing")?)} };tx.commit().map_err(|e|format!("commit Postgres claim failed: {e}"))?;Ok(AcquireResourceClaimResult{disposition,claim})})
    }
    fn get_resource_claim(
        &self,
        k: &str,
        key: &str,
    ) -> Result<Option<ResourceClaimRecord>, String> {
        self.with_client(|c| load_claim(c, k, key))
    }
    fn release_resource_claim(&self, r: ReleaseResourceClaimRequest) -> Result<bool, String> {
        self.with_client(|c|c.execute("DELETE FROM resource_claims WHERE resource_kind=$1 AND resource_key=$2 AND owner=$3", &[&r.resource_kind,&r.resource_key,&r.owner]).map(|n|n>0).map_err(|e|format!("release Postgres claim failed: {e}")))
    }
    fn reclaim_expired_resource_claims(&self, now: i64) -> Result<usize, String> {
        self.with_client(|c| {
            c.execute(
                "DELETE FROM resource_claims WHERE expires_at_ms<=$1",
                &[&now],
            )
            .map(|n| n as usize)
            .map_err(|e| format!("reclaim Postgres claims failed: {e}"))
        })
    }
}

impl DeadLetterStorePort for PostgresRuntimeStore {
    fn create_dead_letter(
        &self,
        r: CreateDeadLetterRequest,
    ) -> Result<CreateDeadLetterResult, String> {
        self.with_client(|c| {
            let mut tx = c
                .transaction()
                .map_err(|e| format!("begin Postgres DLQ create failed: {e}"))?;
            let inserted = insert_dead_letter(&mut tx, &r.dead_letter)?;
            let item = if inserted {
                r.dead_letter
            } else {
                load_dead_letter_by_original(&mut tx, &r.dead_letter.original_job_id)?
                    .ok_or("existing dead letter missing")?
            };
            tx.commit()
                .map_err(|e| format!("commit Postgres DLQ create failed: {e}"))?;
            Ok(CreateDeadLetterResult {
                disposition: if inserted {
                    CreateDeadLetterDisposition::Inserted
                } else {
                    CreateDeadLetterDisposition::Existing
                },
                dead_letter: item,
            })
        })
    }
    fn get_dead_letter(&self, id: &str) -> Result<Option<DeadLetterRecord>, String> {
        self.with_client(|c| load_dead_letter(c, id))
    }
    fn list_dead_letters(
        &self,
        r: ListDeadLettersRequest,
    ) -> Result<Vec<DeadLetterRecord>, String> {
        let statuses = if r.statuses.is_empty() {
            None
        } else {
            Some(
                r.statuses
                    .iter()
                    .map(dl_status)
                    .map(str::to_string)
                    .collect::<Vec<_>>(),
            )
        };
        self.with_client(|c|c.query(format!("SELECT {DL_COLUMNS} FROM dead_letters WHERE ($1::text[] IS NULL OR status=ANY($1)) AND ($2::text IS NULL OR job_kind=$2) AND ($3::text IS NULL OR session_id=$3) AND ($4::text IS NULL OR branch_id=$4) ORDER BY last_failed_at_ms DESC,dead_letter_id DESC LIMIT $5 OFFSET $6").as_str(), &[&statuses,&r.job_kind,&r.session_id,&r.branch_id,&to_i64(r.limit)?,&to_i64(r.offset)?]).map_err(|e|format!("list Postgres dead letters failed: {e}"))?.iter().map(row_dead_letter).collect())
    }
    fn mark_dead_letter_replaying(&self, r: MarkDeadLetterReplayingRequest) -> Result<(), String> {
        self.with_client(|c|updated(c.execute("UPDATE dead_letters SET status='replaying',updated_at_ms=$1 WHERE dead_letter_id=$2 AND status='open'", &[&r.updated_at_ms,&r.dead_letter_id]),"mark dead letter replaying"))
    }
    fn mark_dead_letter_replayed(&self, r: MarkDeadLetterReplayedRequest) -> Result<(), String> {
        self.with_client(|c|updated(c.execute("UPDATE dead_letters SET status='replayed',replayed_job_id=$1,updated_at_ms=$2 WHERE dead_letter_id=$3 AND status='replaying'", &[&r.replayed_job_id,&r.updated_at_ms,&r.dead_letter_id]),"mark dead letter replayed"))
    }
    fn replay_dead_letter(
        &self,
        r: ReplayDeadLetterRequest,
    ) -> Result<ReplayDeadLetterResult, String> {
        self.with_client(|c|{let mut tx=c.transaction().map_err(|e|format!("begin Postgres DLQ replay failed: {e}"))?;let dl=load_dead_letter(&mut tx,&r.dead_letter_id)?.ok_or("dead letter not found")?;if dl.status!=DeadLetterStatus::Open||dl.job_kind!=r.replay_job.job_kind||dl.idempotency_key==r.replay_job.idempotency_key{return Err("invalid dead letter replay".to_string())}let inserted=insert_job(&mut tx,&r.replay_job)?;let job=if inserted{r.replay_job}else{load_job_by_key(&mut tx,&dl.job_kind,&r.replay_job.idempotency_key)?.ok_or("existing replay job missing")?};updated(tx.execute("UPDATE dead_letters SET status='replayed',replayed_job_id=$1,updated_at_ms=$2 WHERE dead_letter_id=$3 AND status='open'", &[&job.job_id,&r.replayed_at_ms,&r.dead_letter_id]),"mark replayed")?;tx.commit().map_err(|e|format!("commit Postgres DLQ replay failed: {e}"))?;Ok(ReplayDeadLetterResult{disposition:if inserted{ScheduleRuntimeJobDisposition::Inserted}else{ScheduleRuntimeJobDisposition::Existing},job})})
    }
    fn dismiss_dead_letter(&self, r: DismissDeadLetterRequest) -> Result<(), String> {
        self.with_client(|c|updated(c.execute("UPDATE dead_letters SET status='dismissed',dismissed_by=$1,dismissed_reason=$2,updated_at_ms=$3 WHERE dead_letter_id=$4 AND status='open'", &[&r.dismissed_by,&r.dismissed_reason,&r.updated_at_ms,&r.dead_letter_id]),"dismiss dead letter"))
    }
}

pub(super) fn insert_job<C: GenericClient>(
    c: &mut C,
    j: &RuntimeJobRecord,
) -> Result<bool, String> {
    let backoff = serde_json::to_string(&j.backoff_policy).map_err(|e| e.to_string())?;
    let outputs = serde_json::to_string(&j.output_refs).map_err(|e| e.to_string())?;
    c.execute("INSERT INTO runtime_jobs(job_id,job_kind,status,run_at_ms,lease_owner,lease_expires_at_ms,retry_count,max_retries,backoff_policy_json,idempotency_key,session_id,branch_id,checkpoint_id,payload_ref,output_refs_json,last_error,created_at_ms,updated_at_ms,heartbeat_at_ms) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19) ON CONFLICT DO NOTHING", &[&j.job_id,&j.job_kind,&job_status(&j.status),&j.run_at_ms,&j.lease_owner,&j.lease_expires_at_ms,&i64::from(j.retry_count),&i64::from(j.max_retries),&backoff,&j.idempotency_key,&j.session_id,&j.branch_id,&j.checkpoint_id,&j.payload_ref,&outputs,&j.last_error,&j.created_at_ms,&j.updated_at_ms,&j.heartbeat_at_ms]).map(|n|n>0).map_err(|e|format!("insert Postgres runtime job failed: {e}"))
}
pub(super) fn upsert_job_outbox<C: GenericClient>(
    c: &mut C,
    job_id: &str,
    event_type: &str,
) -> Result<(), String> {
    c.execute("INSERT INTO runtime_job_outbox(job_id,event_type,published_at_ms,generation) VALUES($1,$2,NULL,0) ON CONFLICT(job_id,event_type) DO UPDATE SET published_at_ms=NULL,generation=runtime_job_outbox.generation+1", &[&job_id, &event_type]).map(|_|()).map_err(|error|format!("upsert Postgres runtime job outbox failed: {error}"))
}
pub(super) fn insert_dead_letter<C: GenericClient>(
    c: &mut C,
    d: &DeadLetterRecord,
) -> Result<bool, String> {
    let policy = serde_json::to_string(&d.replay_policy).map_err(|e| e.to_string())?;
    c.execute("INSERT INTO dead_letters(dead_letter_id,original_job_id,job_kind,status,session_id,branch_id,checkpoint_id,payload_ref,idempotency_key,failure_reason,last_error,attempts,first_failed_at_ms,last_failed_at_ms,replay_policy_json,replayed_job_id,dismissed_by,dismissed_reason,updated_at_ms) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19) ON CONFLICT DO NOTHING", &[&d.dead_letter_id,&d.original_job_id,&d.job_kind,&dl_status(&d.status),&d.session_id,&d.branch_id,&d.checkpoint_id,&d.payload_ref,&d.idempotency_key,&d.failure_reason,&d.last_error,&i64::from(d.attempts),&d.first_failed_at_ms,&d.last_failed_at_ms,&policy,&d.replayed_job_id,&d.dismissed_by,&d.dismissed_reason,&d.updated_at_ms]).map(|n|n>0).map_err(|e|format!("insert Postgres dead letter failed: {e}"))
}
pub(super) fn load_job<C: GenericClient>(
    c: &mut C,
    id: &str,
) -> Result<Option<RuntimeJobRecord>, String> {
    c.query_opt(
        format!("SELECT {JOB_COLUMNS} FROM runtime_jobs WHERE job_id=$1").as_str(),
        &[&id],
    )
    .map_err(|e| format!("load Postgres runtime job failed: {e}"))?
    .map(|r| row_job(&r))
    .transpose()
}
pub(super) fn load_job_by_key<C: GenericClient>(
    c: &mut C,
    k: &str,
    key: &str,
) -> Result<Option<RuntimeJobRecord>, String> {
    c.query_opt(
        format!("SELECT {JOB_COLUMNS} FROM runtime_jobs WHERE job_kind=$1 AND idempotency_key=$2")
            .as_str(),
        &[&k, &key],
    )
    .map_err(|e| format!("load Postgres runtime job key failed: {e}"))?
    .map(|r| row_job(&r))
    .transpose()
}
fn load_jobs<C: GenericClient>(c: &mut C, ids: &[String]) -> Result<Vec<RuntimeJobRecord>, String> {
    if ids.is_empty() {
        return Ok(vec![]);
    }
    c.query(format!("SELECT {JOB_COLUMNS} FROM runtime_jobs WHERE job_id=ANY($1) ORDER BY run_at_ms,created_at_ms,job_id").as_str(), &[&ids]).map_err(|e|format!("load claimed Postgres jobs failed: {e}"))?.iter().map(row_job).collect()
}
fn load_claim<C: GenericClient>(
    c: &mut C,
    k: &str,
    key: &str,
) -> Result<Option<ResourceClaimRecord>, String> {
    c.query_opt("SELECT resource_kind,resource_key,owner,owner_kind,session_id,branch_id,expires_at_ms,metadata_json,created_at_ms,updated_at_ms FROM resource_claims WHERE resource_kind=$1 AND resource_key=$2", &[&k,&key]).map_err(|e|format!("load Postgres claim failed: {e}"))?.map(|r|Ok(row_claim(&r))).transpose()
}
fn load_claim_for_update<C: GenericClient>(
    c: &mut C,
    k: &str,
    key: &str,
) -> Result<Option<ResourceClaimRecord>, String> {
    c.query_opt("SELECT resource_kind,resource_key,owner,owner_kind,session_id,branch_id,expires_at_ms,metadata_json,created_at_ms,updated_at_ms FROM resource_claims WHERE resource_kind=$1 AND resource_key=$2 FOR UPDATE", &[&k,&key]).map_err(|e|format!("lock Postgres claim failed: {e}"))?.map(|r|Ok(row_claim(&r))).transpose()
}
pub(super) fn load_dead_letter<C: GenericClient>(
    c: &mut C,
    id: &str,
) -> Result<Option<DeadLetterRecord>, String> {
    c.query_opt(
        format!("SELECT {DL_COLUMNS} FROM dead_letters WHERE dead_letter_id=$1").as_str(),
        &[&id],
    )
    .map_err(|e| format!("load Postgres dead letter failed: {e}"))?
    .map(|r| row_dead_letter(&r))
    .transpose()
}
pub(super) fn load_dead_letter_by_original<C: GenericClient>(
    c: &mut C,
    id: &str,
) -> Result<Option<DeadLetterRecord>, String> {
    c.query_opt(
        format!("SELECT {DL_COLUMNS} FROM dead_letters WHERE original_job_id=$1").as_str(),
        &[&id],
    )
    .map_err(|e| format!("load Postgres dead letter original job failed: {e}"))?
    .map(|r| row_dead_letter(&r))
    .transpose()
}
fn row_job(r: &Row) -> Result<RuntimeJobRecord, String> {
    let retries: i64 = r.get(6);
    let max: i64 = r.get(7);
    Ok(RuntimeJobRecord {
        job_id: r.get(0),
        job_kind: r.get(1),
        status: job_status_from(r.get::<_, String>(2).as_str())?,
        run_at_ms: r.get(3),
        lease_owner: r.get(4),
        lease_expires_at_ms: r.get(5),
        retry_count: u32::try_from(retries).map_err(|_| "retry overflow".to_string())?,
        max_retries: u32::try_from(max).map_err(|_| "max retries overflow".to_string())?,
        backoff_policy: serde_json::from_str(r.get::<_, String>(8).as_str())
            .map_err(|e| e.to_string())?,
        idempotency_key: r.get(9),
        session_id: r.get(10),
        branch_id: r.get(11),
        checkpoint_id: r.get(12),
        payload_ref: r.get(13),
        output_refs: serde_json::from_str(r.get::<_, String>(14).as_str())
            .map_err(|e| e.to_string())?,
        last_error: r.get(15),
        created_at_ms: r.get(16),
        updated_at_ms: r.get(17),
        heartbeat_at_ms: r.get(18),
    })
}
fn row_claim(r: &Row) -> ResourceClaimRecord {
    ResourceClaimRecord {
        resource_kind: r.get(0),
        resource_key: r.get(1),
        owner: r.get(2),
        owner_kind: r.get(3),
        session_id: r.get(4),
        branch_id: r.get(5),
        expires_at_ms: r.get(6),
        metadata_json: r.get(7),
        created_at_ms: r.get(8),
        updated_at_ms: r.get(9),
    }
}
fn row_dead_letter(r: &Row) -> Result<DeadLetterRecord, String> {
    let attempts: i64 = r.get(11);
    Ok(DeadLetterRecord {
        dead_letter_id: r.get(0),
        original_job_id: r.get(1),
        job_kind: r.get(2),
        status: dl_status_from(r.get::<_, String>(3).as_str())?,
        session_id: r.get(4),
        branch_id: r.get(5),
        checkpoint_id: r.get(6),
        payload_ref: r.get(7),
        idempotency_key: r.get(8),
        failure_reason: r.get(9),
        last_error: r.get(10),
        attempts: u32::try_from(attempts).map_err(|_| "attempt overflow".to_string())?,
        first_failed_at_ms: r.get(12),
        last_failed_at_ms: r.get(13),
        replay_policy: serde_json::from_str::<DeadLetterReplayPolicy>(
            r.get::<_, String>(14).as_str(),
        )
        .map_err(|e| e.to_string())?,
        replayed_job_id: r.get(15),
        dismissed_by: r.get(16),
        dismissed_reason: r.get(17),
        updated_at_ms: r.get(18),
    })
}
fn updated(v: Result<u64, postgres::Error>, label: &str) -> Result<(), String> {
    match v.map_err(|e| format!("{label} failed: {e}"))? {
        0 => Err(format!("{label} state mismatch or not found")),
        _ => Ok(()),
    }
}
pub(super) fn job_status(v: &RuntimeJobStatus) -> &'static str {
    match v {
        RuntimeJobStatus::Queued => "queued",
        RuntimeJobStatus::Leased => "leased",
        RuntimeJobStatus::Running => "running",
        RuntimeJobStatus::Succeeded => "succeeded",
        RuntimeJobStatus::Failed => "failed",
        RuntimeJobStatus::DeadLettered => "dead_lettered",
        RuntimeJobStatus::Cancelled => "cancelled",
    }
}
fn job_status_from(v: &str) -> Result<RuntimeJobStatus, String> {
    match v {
        "queued" => Ok(RuntimeJobStatus::Queued),
        "leased" => Ok(RuntimeJobStatus::Leased),
        "running" => Ok(RuntimeJobStatus::Running),
        "succeeded" => Ok(RuntimeJobStatus::Succeeded),
        "failed" => Ok(RuntimeJobStatus::Failed),
        "dead_lettered" => Ok(RuntimeJobStatus::DeadLettered),
        "cancelled" => Ok(RuntimeJobStatus::Cancelled),
        _ => Err(format!("unknown runtime job status: {v}")),
    }
}
pub(super) fn dl_status(v: &DeadLetterStatus) -> &'static str {
    match v {
        DeadLetterStatus::Open => "open",
        DeadLetterStatus::Replaying => "replaying",
        DeadLetterStatus::Replayed => "replayed",
        DeadLetterStatus::Dismissed => "dismissed",
    }
}
fn dl_status_from(v: &str) -> Result<DeadLetterStatus, String> {
    match v {
        "open" => Ok(DeadLetterStatus::Open),
        "replaying" => Ok(DeadLetterStatus::Replaying),
        "replayed" => Ok(DeadLetterStatus::Replayed),
        "dismissed" => Ok(DeadLetterStatus::Dismissed),
        _ => Err(format!("unknown dead letter status: {v}")),
    }
}
