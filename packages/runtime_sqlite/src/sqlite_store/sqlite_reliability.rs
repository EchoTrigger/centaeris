use super::*;
use centaeris_core::session::reliability::{
    AcquireResourceClaimDisposition, AcquireResourceClaimRequest, AcquireResourceClaimResult,
    CancelRuntimeJobRequest, ClaimDueRuntimeJobsRequest, CompleteRuntimeJobRequest,
    CreateDeadLetterDisposition, CreateDeadLetterRequest, CreateDeadLetterResult, DeadLetterRecord,
    DeadLetterReplayPolicy, DeadLetterStatus, DeadLetterStorePort, DismissDeadLetterRequest,
    FailRuntimeJobRequest, ListDeadLettersRequest, ListRuntimeJobsRequest,
    MarkDeadLetterReplayedRequest, MarkDeadLetterReplayingRequest, ReleaseResourceClaimRequest,
    RenewRuntimeJobLeaseRequest, ReplayDeadLetterRequest, ReplayDeadLetterResult,
    ResourceClaimRecord, ResourceClaimStorePort, RuntimeBackoffPolicy,
    RuntimeJobFailureDisposition, RuntimeJobOutboxPort, RuntimeJobOutboxPublishDisposition,
    RuntimeJobOutboxRecord, RuntimeJobRecord, RuntimeJobStatus, RuntimeJobStorePort,
    ScheduleRuntimeJobDisposition, ScheduleRuntimeJobRequest, ScheduleRuntimeJobResult,
    StartRuntimeJobRequest, WakeRuntimeJobDisposition, WakeRuntimeJobRequest,
    YieldRuntimeJobRequest, RUNTIME_JOB_TERMINAL_EVENT,
};
use rusqlite::types::Value as SqlValue;
use rusqlite::{params, params_from_iter, OptionalExtension};

impl RuntimeJobStorePort for SqliteRuntimeStore {
    fn schedule_runtime_job(
        &self,
        req: ScheduleRuntimeJobRequest,
    ) -> Result<ScheduleRuntimeJobResult, String> {
        self.with_conn(|conn| {
            let tx = conn
                .transaction()
                .map_err(|err| format!("begin schedule_runtime_job transaction failed: {err}"))?;
            let inserted = insert_runtime_job(&tx, &req.job)?;
            let result = if inserted {
                ScheduleRuntimeJobResult {
                    disposition: ScheduleRuntimeJobDisposition::Inserted,
                    job: req.job,
                }
            } else {
                let job = load_runtime_job_by_kind_and_idempotency(
                    &tx,
                    req.job.job_kind.as_str(),
                    req.job.idempotency_key.as_str(),
                )?
                .ok_or_else(|| {
                    format!(
                        "schedule_runtime_job existing row missing job_kind={} idempotency_key={}",
                        req.job.job_kind, req.job.idempotency_key
                    )
                })?;
                ScheduleRuntimeJobResult {
                    disposition: ScheduleRuntimeJobDisposition::Existing,
                    job,
                }
            };
            tx.commit()
                .map_err(|err| format!("commit schedule_runtime_job transaction failed: {err}"))?;
            Ok(result)
        })
    }

    fn get_runtime_job(&self, job_id: &str) -> Result<Option<RuntimeJobRecord>, String> {
        self.with_conn(|conn| load_runtime_job_by_id(conn, job_id))
    }

    fn list_runtime_jobs(
        &self,
        req: ListRuntimeJobsRequest,
    ) -> Result<Vec<RuntimeJobRecord>, String> {
        self.with_conn(|conn| list_runtime_jobs_with_filters(conn, req))
    }

    fn claim_due_runtime_jobs(
        &self,
        req: ClaimDueRuntimeJobsRequest,
    ) -> Result<Vec<RuntimeJobRecord>, String> {
        self.with_conn(|conn| {
            let limit_i64 = to_i64(req.limit)?;
            let lease_ms = i64::try_from(req.lease_ms)
                .map_err(|_| format!("lease_ms overflow: {}", req.lease_ms))?;
            let lease_expires_at_ms = req.now_ms.saturating_add(lease_ms);

            let tx = conn
                .transaction()
                .map_err(|err| format!("begin claim_due_runtime_jobs transaction failed: {err}"))?;

            let due_job_ids = {
                let mut ids = Vec::new();
                let mut conditions = vec![
                    "status = 'queued'".to_string(),
                    "run_at_ms <= ?".to_string(),
                ];
                let mut params_vec = vec![SqlValue::from(req.now_ms)];
                if let Some(job_id) = req
                    .job_id
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                {
                    conditions.push("job_id = ?".to_string());
                    params_vec.push(SqlValue::from(job_id.to_string()));
                }
                if let Some(job_kind) = req
                    .job_kind
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                {
                    conditions.push("job_kind = ?".to_string());
                    params_vec.push(SqlValue::from(job_kind.to_string()));
                }
                if let Some(session_id) = req
                    .session_id
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                {
                    conditions.push("session_id = ?".to_string());
                    params_vec.push(SqlValue::from(session_id.to_string()));
                }
                params_vec.push(SqlValue::from(limit_i64));
                let sql = format!(
                    "
                    SELECT job_id
                    FROM runtime_jobs
                    WHERE {}
                    ORDER BY run_at_ms ASC, created_at_ms ASC, job_id ASC
                    LIMIT ?
                    ",
                    conditions.join(" AND ")
                );
                let mut stmt = tx
                    .prepare(sql.as_str())
                    .map_err(|err| format!("prepare claim_due_runtime_jobs failed: {err}"))?;
                let rows = stmt
                    .query_map(params_from_iter(params_vec.iter()), |row| {
                        row.get::<_, String>(0)
                    })
                    .map_err(|err| format!("query claim_due_runtime_jobs failed: {err}"))?;
                for row in rows {
                    ids.push(row.map_err(|err| {
                        format!("decode claim_due_runtime_jobs id failed: {err}")
                    })?);
                }
                ids
            };

            let mut claimed_ids = Vec::new();
            for job_id in due_job_ids {
                let updated = tx
                    .execute(
                        "
                        UPDATE runtime_jobs
                        SET status = 'leased',
                            lease_owner = ?1,
                            lease_expires_at_ms = ?2,
                            updated_at_ms = ?3,
                            heartbeat_at_ms = ?3
                        WHERE job_id = ?4
                          AND status = 'queued'
                        ",
                        params![
                            req.worker_id.as_str(),
                            lease_expires_at_ms,
                            req.now_ms,
                            job_id.as_str(),
                        ],
                    )
                    .map_err(|err| format!("update claim_due_runtime_jobs failed: {err}"))?;
                if updated > 0 {
                    claimed_ids.push(job_id);
                }
            }

            let claimed = load_runtime_jobs_by_ids(&tx, &claimed_ids)?;
            tx.commit().map_err(|err| {
                format!("commit claim_due_runtime_jobs transaction failed: {err}")
            })?;
            Ok(claimed)
        })
    }

    fn start_runtime_job(&self, req: StartRuntimeJobRequest) -> Result<(), String> {
        self.with_conn(|conn| {
            let updated = conn
                .execute(
                    "
                    UPDATE runtime_jobs
                    SET status = 'running',
                        updated_at_ms = ?1,
                        heartbeat_at_ms = ?1
                    WHERE job_id = ?2
                      AND status = 'leased'
                      AND lease_owner = ?3
                      AND lease_expires_at_ms > ?1
                    ",
                    params![
                        req.started_at_ms,
                        req.job_id.as_str(),
                        req.lease_owner.as_str(),
                    ],
                )
                .map_err(|err| format!("start_runtime_job failed: {err}"))?;
            if updated == 0 {
                return Err(format!(
                    "start_runtime_job lease mismatch or job not found: {}",
                    req.job_id
                ));
            }
            Ok(())
        })
    }

    fn renew_runtime_job_lease(&self, req: RenewRuntimeJobLeaseRequest) -> Result<(), String> {
        let lease_ms = i64::try_from(req.lease_ms)
            .map_err(|_| format!("lease_ms overflow: {}", req.lease_ms))?;
        let lease_expires_at_ms = req.heartbeat_at_ms.saturating_add(lease_ms);
        self.with_conn(|conn| {
            let updated = conn.execute(
                "UPDATE runtime_jobs SET heartbeat_at_ms=?1,lease_expires_at_ms=?2,updated_at_ms=?1 WHERE job_id=?3 AND status IN('leased','running') AND lease_owner=?4 AND lease_expires_at_ms>?1",
                params![req.heartbeat_at_ms, lease_expires_at_ms, req.job_id, req.lease_owner],
            ).map_err(|error| format!("renew_runtime_job_lease failed: {error}"))?;
            if updated != 1 {
                return Err(format!("renew_runtime_job_lease lease mismatch, expired lease, or job not found: {}", req.job_id));
            }
            Ok(())
        })
    }

    fn yield_runtime_job(&self, req: YieldRuntimeJobRequest) -> Result<(), String> {
        if req.run_at_ms < req.yielded_at_ms || req.transition_reason.trim().is_empty() {
            return Err("invalid runtime job yield".to_string());
        }
        self.with_conn(|conn| {
            let tx = conn
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(|error| format!("begin yield_runtime_job failed: {error}"))?;
            let job = tx
                .query_row(
                    "SELECT status,lease_owner,lease_expires_at_ms,run_at_ms,session_id FROM runtime_jobs WHERE job_id=?1",
                    params![req.job_id],
                    |row| Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, Option<String>>(4)?,
                    )),
                )
                .optional()
                .map_err(|error| format!("load runtime job for yield failed: {error}"))?
                .ok_or_else(|| format!("yield_runtime_job not found: {}", req.job_id))?;
            let event_id = format!(
                "runtime_job_yield:{}:{}:{}",
                req.job_id, req.lease_owner, req.yielded_at_ms
            );
            let payload = serde_json::json!({
                "schema": "runtime.job.yielded.v1",
                "jobId": req.job_id,
                "leaseOwner": req.lease_owner,
                "yieldedAtMs": req.yielded_at_ms,
                "runAtMs": req.run_at_ms,
                "transitionReason": req.transition_reason,
            })
            .to_string();
            if job.0 == "queued" && job.1.is_none() {
                let existing = tx
                    .query_row(
                        "SELECT event_id,payload_json FROM runtime_events WHERE task_id=?1 AND event_type='runtime_job_yielded' AND at_ms=?2",
                        params![req.job_id, req.yielded_at_ms],
                        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                    )
                    .optional()
                    .map_err(|error| format!("load yielded runtime event failed: {error}"))?;
                match existing {
                    Some((existing_id, existing_payload))
                        if existing_id == event_id && existing_payload == payload =>
                    {
                        tx.commit().map_err(|error| format!(
                            "commit idempotent yield_runtime_job failed: {error}"
                        ))?;
                        return Ok(());
                    }
                    Some(_) => {
                        return Err(format!(
                            "yield_runtime_job idempotency conflict: {}",
                            req.job_id
                        ));
                    }
                    None => {
                        return Err(format!(
                            "yield_runtime_job lease mismatch or expired: {}",
                            req.job_id
                        ));
                    }
                }
            }
            if !matches!(job.0.as_str(), "leased" | "running")
                || job.1.as_deref() != Some(req.lease_owner.as_str())
                || job.2.is_none_or(|expires| expires <= req.yielded_at_ms)
            {
                return Err(format!("yield_runtime_job lease mismatch or expired: {}", req.job_id));
            }
            let pending_wake_ids = {
                let mut statement = tx
                    .prepare(
                        "SELECT wake.event_id FROM runtime_events AS wake WHERE wake.task_id=?1 AND wake.event_type='runtime_job_wake_requested' AND NOT EXISTS(SELECT 1 FROM runtime_events AS consumed WHERE consumed.event_id='runtime_job_wake_consumed:' || substr(wake.event_id,length('runtime_job_wake:')+1)) ORDER BY wake.at_ms,wake.event_id",
                    )
                    .map_err(|error| format!("prepare pending runtime job wakes failed: {error}"))?;
                let wake_ids = statement
                    .query_map(params![req.job_id], |row| row.get::<_, String>(0))
                    .map_err(|error| format!("query pending runtime job wakes failed: {error}"))?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|error| format!("read pending runtime job wakes failed: {error}"))?;
                wake_ids
            };
            let effective_run_at_ms = if pending_wake_ids.is_empty() {
                req.run_at_ms
            } else {
                req.yielded_at_ms
            };
            let updated = tx
                .execute(
                    "UPDATE runtime_jobs SET status='queued',run_at_ms=?1,updated_at_ms=?2,lease_owner=NULL,lease_expires_at_ms=NULL,heartbeat_at_ms=NULL WHERE job_id=?3 AND status IN('leased','running') AND lease_owner=?4 AND lease_expires_at_ms>?2",
                    params![effective_run_at_ms, req.yielded_at_ms, req.job_id, req.lease_owner],
                )
                .map_err(|error| format!("yield_runtime_job failed: {error}"))?;
            if updated != 1 {
                return Err(format!("yield_runtime_job lease mismatch or expired: {}", req.job_id));
            }
            let session_id = job
                .4
                .unwrap_or_else(|| format!("runtime_job:{}", req.job_id));
            tx.execute(
                "INSERT INTO runtime_events(event_id,session_id,task_id,event_type,at_ms,visibility,payload_json) VALUES(?1,?2,?3,'runtime_job_yielded',?4,'internal',?5)",
                params![event_id, session_id, req.job_id, req.yielded_at_ms, payload],
            )
            .map_err(|error| format!("append runtime job yield event failed: {error}"))?;
            for wake_event_id in pending_wake_ids {
                let consumed_event_id = wake_event_id.replacen(
                    "runtime_job_wake:",
                    "runtime_job_wake_consumed:",
                    1,
                );
                let consumed_payload = serde_json::json!({
                    "schema": "runtime.job.wake_consumed.v1",
                    "jobId": req.job_id,
                    "wakeEventId": wake_event_id,
                    "consumedAtMs": req.yielded_at_ms,
                    "transitionReason": "yield_observed_pending_wake",
                })
                .to_string();
                tx.execute(
                    "INSERT INTO runtime_events(event_id,session_id,task_id,event_type,at_ms,visibility,payload_json) VALUES(?1,?2,?3,'runtime_job_wake_consumed',?4,'internal',?5)",
                    params![consumed_event_id, session_id, req.job_id, req.yielded_at_ms, consumed_payload],
                )
                .map_err(|error| format!("consume pending runtime job wake failed: {error}"))?;
            }
            tx.commit().map_err(|error| format!("commit yield_runtime_job failed: {error}"))
        })
    }

    fn wake_runtime_job(
        &self,
        req: WakeRuntimeJobRequest,
    ) -> Result<WakeRuntimeJobDisposition, String> {
        if req.job_id.trim().is_empty()
            || req.source_job_id.trim().is_empty()
            || req.transition_reason.trim().is_empty()
        {
            return Err("invalid runtime job wake".to_string());
        }
        self.with_conn(|conn| {
            let tx = conn
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(|error| format!("begin wake_runtime_job failed: {error}"))?;
            let (status, run_at_ms, session_id) = tx
                .query_row(
                    "SELECT status,run_at_ms,session_id FROM runtime_jobs WHERE job_id=?1",
                    params![req.job_id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, Option<String>>(2)?,
                        ))
                    },
                )
                .optional()
                .map_err(|error| format!("load runtime job for wake failed: {error}"))?
                .ok_or_else(|| format!("wake_runtime_job not found: {}", req.job_id))?;
            let event_id = format!(
                "runtime_job_wake:{}:{}",
                req.job_id, req.source_job_id
            );
            let payload = serde_json::json!({
                "schema": "runtime.job.wake.v1",
                "jobId": req.job_id,
                "sourceJobId": req.source_job_id,
                "wokenAtMs": req.woken_at_ms,
                "transitionReason": req.transition_reason,
            })
            .to_string();
            let existing = tx
                .query_row(
                    "SELECT payload_json FROM runtime_events WHERE event_id=?1",
                    params![event_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(|error| format!("load runtime job wake event failed: {error}"))?;
            if existing.as_deref().is_some_and(|value| value != payload) {
                return Err(format!(
                    "wake_runtime_job idempotency conflict: {}",
                    req.job_id
                ));
            }
            if existing.is_none() {
                tx.execute(
                    "INSERT INTO runtime_events(event_id,session_id,task_id,event_type,at_ms,visibility,payload_json) VALUES(?1,?2,?3,'runtime_job_wake_requested',?4,'internal',?5)",
                    params![event_id, session_id.clone().unwrap_or_else(|| format!("runtime_job:{}", req.job_id)), req.job_id, req.woken_at_ms, payload],
                )
                .map_err(|error| format!("append runtime job wake event failed: {error}"))?;
            }
            let disposition = match status.as_str() {
                "queued" => {
                    if run_at_ms > req.woken_at_ms {
                        tx.execute(
                            "UPDATE runtime_jobs SET run_at_ms=?1,updated_at_ms=?1 WHERE job_id=?2 AND status='queued' AND run_at_ms>?1",
                            params![req.woken_at_ms, req.job_id],
                        )
                        .map_err(|error| format!("wake runtime job failed: {error}"))?;
                        WakeRuntimeJobDisposition::Woken
                    } else {
                        WakeRuntimeJobDisposition::AlreadyRunnable
                    }
                }
                "leased" | "running" => WakeRuntimeJobDisposition::Active,
                "succeeded" | "failed" | "dead_lettered" | "cancelled" => {
                    WakeRuntimeJobDisposition::Terminal
                }
                other => return Err(format!("wake_runtime_job unsupported status: {other}")),
            };
            tx.commit()
                .map_err(|error| format!("commit wake_runtime_job failed: {error}"))?;
            Ok(disposition)
        })
    }

    fn complete_runtime_job(&self, req: CompleteRuntimeJobRequest) -> Result<(), String> {
        self.with_conn(|conn| {
            let tx = conn
                .transaction()
                .map_err(|err| format!("begin complete_runtime_job transaction failed: {err}"))?;
            let output_refs_json = serde_json::to_string(&req.output_refs).map_err(|err| {
                format!("serialize complete_runtime_job output_refs failed: {err}")
            })?;
            let updated = tx
                .execute(
                    "
                    UPDATE runtime_jobs
                    SET status = 'succeeded',
                        output_refs_json = ?1,
                        updated_at_ms = ?2,
                        lease_owner = NULL,
                        lease_expires_at_ms = NULL,
                        last_error = NULL
                    WHERE job_id = ?3
                      AND status IN ('leased', 'running')
                      AND lease_owner = ?4
                      AND lease_expires_at_ms > ?2
                    ",
                    params![
                        output_refs_json,
                        req.completed_at_ms,
                        req.job_id.as_str(),
                        req.lease_owner.as_str(),
                    ],
                )
                .map_err(|err| format!("complete_runtime_job failed: {err}"))?;
            if updated == 0 {
                return Err(format!(
                    "complete_runtime_job lease mismatch or job not found: {}",
                    req.job_id
                ));
            }
            upsert_runtime_job_outbox_event(&tx, req.job_id.as_str(), RUNTIME_JOB_TERMINAL_EVENT)?;
            tx.commit()
                .map_err(|err| format!("commit complete_runtime_job failed: {err}"))
        })
    }

    fn fail_runtime_job(&self, req: FailRuntimeJobRequest) -> Result<(), String> {
        self.with_conn(|conn| {
            let tx = conn
                .transaction()
                .map_err(|err| format!("begin fail_runtime_job transaction failed: {err}"))?;
            let (status_raw, run_at_ms) = match req.disposition {
                RuntimeJobFailureDisposition::RetryScheduled => (
                    "queued",
                    req.next_run_at_ms.ok_or_else(|| {
                        format!(
                            "fail_runtime_job retry_scheduled requires next_run_at_ms job_id={}",
                            req.job_id
                        )
                    })?,
                ),
                RuntimeJobFailureDisposition::Failed => ("failed", req.failed_at_ms),
                RuntimeJobFailureDisposition::DeadLettered => ("dead_lettered", req.failed_at_ms),
            };

            let updated = tx
                .execute(
                    "
                    UPDATE runtime_jobs
                    SET status = ?1,
                        run_at_ms = ?2,
                        retry_count = retry_count + 1,
                        updated_at_ms = ?3,
                        lease_owner = NULL,
                        lease_expires_at_ms = NULL,
                        last_error = ?4
                    WHERE job_id = ?5
                      AND status IN ('leased', 'running')
                      AND lease_owner = ?6
                      AND lease_expires_at_ms > ?3
                    ",
                    params![
                        status_raw,
                        run_at_ms,
                        req.failed_at_ms,
                        req.last_error.as_str(),
                        req.job_id.as_str(),
                        req.lease_owner.as_str(),
                    ],
                )
                .map_err(|err| format!("fail_runtime_job failed: {err}"))?;
            if updated == 0 {
                return Err(format!(
                    "fail_runtime_job lease mismatch or job not found: {}",
                    req.job_id
                ));
            }
            if !matches!(
                req.disposition,
                RuntimeJobFailureDisposition::RetryScheduled
            ) {
                upsert_runtime_job_outbox_event(
                    &tx,
                    req.job_id.as_str(),
                    RUNTIME_JOB_TERMINAL_EVENT,
                )?;
            }
            tx.commit()
                .map_err(|err| format!("commit fail_runtime_job failed: {err}"))
        })
    }

    fn cancel_runtime_job(&self, req: CancelRuntimeJobRequest) -> Result<(), String> {
        self.with_conn(|conn| {
            let tx = conn
                .transaction()
                .map_err(|err| format!("begin cancel_runtime_job transaction failed: {err}"))?;
            let updated = tx
                .execute(
                    "
                    UPDATE runtime_jobs
                    SET status = 'cancelled',
                        updated_at_ms = ?1,
                        lease_owner = NULL,
                        lease_expires_at_ms = NULL,
                        last_error = CASE
                            WHEN COALESCE(last_error, '') = '' THEN ?2
                            ELSE last_error
                        END
                    WHERE job_id = ?3
                      AND status NOT IN ('succeeded', 'failed', 'dead_lettered', 'cancelled')
                      AND (?4 IS NULL OR status = ?4)
                    ",
                    params![
                        req.cancelled_at_ms,
                        req.reason.as_str(),
                        req.job_id.as_str(),
                        req.expected_status.as_ref().map(runtime_job_status_to_db),
                    ],
                )
                .map_err(|err| format!("cancel_runtime_job failed: {err}"))?;
            if updated == 0 {
                return Err(format!(
                    "cancel_runtime_job not found or already terminal: {}",
                    req.job_id
                ));
            }
            upsert_runtime_job_outbox_event(&tx, req.job_id.as_str(), RUNTIME_JOB_TERMINAL_EVENT)?;
            tx.commit()
                .map_err(|err| format!("commit cancel_runtime_job failed: {err}"))
        })
    }

    fn reclaim_expired_runtime_job_leases(
        &self,
        now_ms: centaeris_core::runtime::contracts::TimestampMs,
    ) -> Result<usize, String> {
        self.with_conn(|conn| {
            let updated = conn
                .execute(
                    "
                    UPDATE runtime_jobs
                    SET status = 'queued',
                        run_at_ms = ?1,
                        updated_at_ms = ?1,
                        lease_owner = NULL,
                        lease_expires_at_ms = NULL,
                        last_error = CASE
                            WHEN COALESCE(last_error, '') = '' AND status = 'running' THEN 'worker_crashed_reclaimed'
                            WHEN COALESCE(last_error, '') = '' THEN 'lease_expired_reclaimed'
                            ELSE last_error
                        END
                    WHERE status IN ('leased', 'running')
                      AND lease_expires_at_ms IS NOT NULL
                      AND lease_expires_at_ms <= ?1
                    ",
                    params![now_ms],
                )
                .map_err(|err| format!("reclaim_expired_runtime_job_leases failed: {err}"))?;
            Ok(updated)
        })
    }
}

impl ResourceClaimStorePort for SqliteRuntimeStore {
    fn acquire_resource_claim(
        &self,
        req: AcquireResourceClaimRequest,
    ) -> Result<AcquireResourceClaimResult, String> {
        self.with_conn(|conn| {
            let tx = conn
                .transaction()
                .map_err(|err| format!("begin acquire_resource_claim transaction failed: {err}"))?;
            let expires_at_ms = req.now_ms.saturating_add(
                i64::try_from(req.ttl_ms)
                    .map_err(|_| format!("resource claim ttl_ms overflow: {}", req.ttl_ms))?,
            );
            let existing =
                load_resource_claim(&tx, req.resource_kind.as_str(), req.resource_key.as_str())?;
            let result = match existing {
                Some(claim) if claim.expires_at_ms > req.now_ms && claim.owner != req.owner => {
                    AcquireResourceClaimResult {
                        disposition: AcquireResourceClaimDisposition::Conflict,
                        claim,
                    }
                }
                Some(mut claim) if claim.expires_at_ms > req.now_ms => {
                    let updated = tx
                        .execute(
                            "
                            UPDATE resource_claims
                            SET expires_at_ms = ?4,
                                metadata_json = ?5,
                                updated_at_ms = ?6
                            WHERE resource_kind = ?1
                              AND resource_key = ?2
                              AND owner = ?3
                            ",
                            params![
                                req.resource_kind.as_str(),
                                req.resource_key.as_str(),
                                req.owner.as_str(),
                                expires_at_ms,
                                req.metadata_json.as_str(),
                                req.now_ms,
                            ],
                        )
                        .map_err(|err| format!("refresh resource claim failed: {err}"))?;
                    if updated == 0 {
                        return Err(format!(
                            "refresh resource claim lost row: kind={} key={}",
                            req.resource_kind, req.resource_key
                        ));
                    }
                    claim.expires_at_ms = expires_at_ms;
                    claim.metadata_json = req.metadata_json;
                    claim.updated_at_ms = req.now_ms;
                    AcquireResourceClaimResult {
                        disposition: AcquireResourceClaimDisposition::AlreadyOwned,
                        claim,
                    }
                }
                _ => {
                    tx.execute(
                        "
                        INSERT INTO resource_claims(
                            resource_kind, resource_key, owner, owner_kind, session_id,
                            branch_id, expires_at_ms, metadata_json, created_at_ms, updated_at_ms
                        )
                        VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)
                        ON CONFLICT(resource_kind, resource_key) DO UPDATE SET
                            owner = excluded.owner,
                            owner_kind = excluded.owner_kind,
                            session_id = excluded.session_id,
                            branch_id = excluded.branch_id,
                            expires_at_ms = excluded.expires_at_ms,
                            metadata_json = excluded.metadata_json,
                            updated_at_ms = excluded.updated_at_ms
                        ",
                        params![
                            req.resource_kind.as_str(),
                            req.resource_key.as_str(),
                            req.owner.as_str(),
                            req.owner_kind.as_str(),
                            req.session_id.as_deref(),
                            req.branch_id.as_deref(),
                            expires_at_ms,
                            req.metadata_json.as_str(),
                            req.now_ms,
                        ],
                    )
                    .map_err(|err| format!("upsert resource claim failed: {err}"))?;
                    let claim = load_resource_claim(
                        &tx,
                        req.resource_kind.as_str(),
                        req.resource_key.as_str(),
                    )?
                    .ok_or_else(|| {
                        format!(
                            "acquire_resource_claim inserted row missing kind={} key={}",
                            req.resource_kind, req.resource_key
                        )
                    })?;
                    AcquireResourceClaimResult {
                        disposition: AcquireResourceClaimDisposition::Acquired,
                        claim,
                    }
                }
            };
            tx.commit().map_err(|err| {
                format!("commit acquire_resource_claim transaction failed: {err}")
            })?;
            Ok(result)
        })
    }

    fn get_resource_claim(
        &self,
        resource_kind: &str,
        resource_key: &str,
    ) -> Result<Option<ResourceClaimRecord>, String> {
        self.with_conn(|conn| load_resource_claim(conn, resource_kind, resource_key))
    }

    fn release_resource_claim(&self, req: ReleaseResourceClaimRequest) -> Result<bool, String> {
        self.with_conn(|conn| {
            let updated = conn
                .execute(
                    "
                    DELETE FROM resource_claims
                    WHERE resource_kind = ?1
                      AND resource_key = ?2
                      AND owner = ?3
                    ",
                    params![
                        req.resource_kind.as_str(),
                        req.resource_key.as_str(),
                        req.owner.as_str(),
                    ],
                )
                .map_err(|err| format!("release_resource_claim failed: {err}"))?;
            let _ = req.released_at_ms;
            Ok(updated > 0)
        })
    }

    fn reclaim_expired_resource_claims(
        &self,
        now_ms: centaeris_core::runtime::contracts::TimestampMs,
    ) -> Result<usize, String> {
        self.with_conn(|conn| {
            let updated = conn
                .execute(
                    "
                    DELETE FROM resource_claims
                    WHERE expires_at_ms <= ?1
                    ",
                    params![now_ms],
                )
                .map_err(|err| format!("reclaim_expired_resource_claims failed: {err}"))?;
            Ok(updated)
        })
    }
}

impl DeadLetterStorePort for SqliteRuntimeStore {
    fn create_dead_letter(
        &self,
        req: CreateDeadLetterRequest,
    ) -> Result<CreateDeadLetterResult, String> {
        self.with_conn(|conn| {
            let tx = conn
                .transaction()
                .map_err(|err| format!("begin create_dead_letter transaction failed: {err}"))?;
            let inserted = insert_dead_letter_record(&tx, &req.dead_letter)?;
            let result = if inserted {
                CreateDeadLetterResult {
                    disposition: CreateDeadLetterDisposition::Inserted,
                    dead_letter: req.dead_letter,
                }
            } else {
                let dead_letter = load_dead_letter_by_original_job(
                    &tx,
                    req.dead_letter.original_job_id.as_str(),
                )?
                .ok_or_else(|| {
                    format!(
                        "create_dead_letter existing row missing original_job_id={}",
                        req.dead_letter.original_job_id
                    )
                })?;
                CreateDeadLetterResult {
                    disposition: CreateDeadLetterDisposition::Existing,
                    dead_letter,
                }
            };
            tx.commit()
                .map_err(|err| format!("commit create_dead_letter transaction failed: {err}"))?;
            Ok(result)
        })
    }

    fn get_dead_letter(&self, dead_letter_id: &str) -> Result<Option<DeadLetterRecord>, String> {
        self.with_conn(|conn| load_dead_letter_by_id(conn, dead_letter_id))
    }

    fn list_dead_letters(
        &self,
        req: ListDeadLettersRequest,
    ) -> Result<Vec<DeadLetterRecord>, String> {
        self.with_conn(|conn| list_dead_letters_with_filters(conn, req))
    }

    fn mark_dead_letter_replaying(
        &self,
        req: MarkDeadLetterReplayingRequest,
    ) -> Result<(), String> {
        self.with_conn(|conn| {
            let updated = conn
                .execute(
                    "
                    UPDATE dead_letters
                    SET status = 'replaying',
                        updated_at_ms = ?2
                    WHERE dead_letter_id = ?1
                      AND status = 'open'
                    ",
                    params![req.dead_letter_id.as_str(), req.updated_at_ms],
                )
                .map_err(|err| format!("mark_dead_letter_replaying failed: {err}"))?;
            if updated == 0 {
                return Err(format!(
                    "mark_dead_letter_replaying not found or not open: {}",
                    req.dead_letter_id
                ));
            }
            Ok(())
        })
    }

    fn mark_dead_letter_replayed(&self, req: MarkDeadLetterReplayedRequest) -> Result<(), String> {
        self.with_conn(|conn| {
            let updated = conn
                .execute(
                    "
                    UPDATE dead_letters
                    SET status = 'replayed',
                        replayed_job_id = ?2,
                        updated_at_ms = ?3
                    WHERE dead_letter_id = ?1
                      AND status = 'replaying'
                    ",
                    params![
                        req.dead_letter_id.as_str(),
                        req.replayed_job_id.as_deref(),
                        req.updated_at_ms,
                    ],
                )
                .map_err(|err| format!("mark_dead_letter_replayed failed: {err}"))?;
            if updated == 0 {
                return Err(format!(
                    "mark_dead_letter_replayed not found or not replaying: {}",
                    req.dead_letter_id
                ));
            }
            Ok(())
        })
    }

    fn replay_dead_letter(
        &self,
        req: ReplayDeadLetterRequest,
    ) -> Result<ReplayDeadLetterResult, String> {
        self.with_conn(|conn| {
            let tx = conn
                .transaction()
                .map_err(|err| format!("begin replay_dead_letter transaction failed: {err}"))?;

            let dead_letter = load_dead_letter_by_id(&tx, req.dead_letter_id.as_str())?
                .ok_or_else(|| format!("replay_dead_letter not found: {}", req.dead_letter_id))?;
            if dead_letter.status != DeadLetterStatus::Open {
                return Err(format!(
                    "replay_dead_letter requires open status: {}",
                    req.dead_letter_id
                ));
            }
            if dead_letter.job_kind != req.replay_job.job_kind {
                return Err(format!(
                    "replay_dead_letter job_kind mismatch: dead_letter={} replay_job={}",
                    dead_letter.job_kind, req.replay_job.job_kind
                ));
            }
            if dead_letter.idempotency_key == req.replay_job.idempotency_key {
                return Err(format!(
                    "replay_dead_letter requires a new runtime job idempotency_key: {}",
                    req.dead_letter_id
                ));
            }

            let inserted = insert_runtime_job(&tx, &req.replay_job)?;
            let disposition = if inserted {
                ScheduleRuntimeJobDisposition::Inserted
            } else {
                ScheduleRuntimeJobDisposition::Existing
            };
            let replay_job = if inserted {
                req.replay_job
            } else {
                load_runtime_job_by_kind_and_idempotency(
                    &tx,
                    req.replay_job.job_kind.as_str(),
                    req.replay_job.idempotency_key.as_str(),
                )?
                .ok_or_else(|| {
                    format!(
                        "replay_dead_letter existing row missing job_kind={} idempotency_key={}",
                        req.replay_job.job_kind, req.replay_job.idempotency_key
                    )
                })?
            };

            let updated = tx
                .execute(
                    "
                    UPDATE dead_letters
                    SET status = 'replayed',
                        replayed_job_id = ?2,
                        updated_at_ms = ?3
                    WHERE dead_letter_id = ?1
                      AND status = 'open'
                    ",
                    params![
                        req.dead_letter_id.as_str(),
                        replay_job.job_id.as_str(),
                        req.replayed_at_ms,
                    ],
                )
                .map_err(|err| format!("mark replay_dead_letter replayed failed: {err}"))?;
            if updated == 0 {
                return Err(format!(
                    "replay_dead_letter not found or not open: {}",
                    req.dead_letter_id
                ));
            }

            tx.commit()
                .map_err(|err| format!("commit replay_dead_letter transaction failed: {err}"))?;
            Ok(ReplayDeadLetterResult {
                disposition,
                job: replay_job,
            })
        })
    }

    fn dismiss_dead_letter(&self, req: DismissDeadLetterRequest) -> Result<(), String> {
        self.with_conn(|conn| {
            let updated = conn
                .execute(
                    "
                    UPDATE dead_letters
                    SET status = 'dismissed',
                        dismissed_by = ?2,
                        dismissed_reason = ?3,
                        updated_at_ms = ?4
                    WHERE dead_letter_id = ?1
                      AND status = 'open'
                    ",
                    params![
                        req.dead_letter_id.as_str(),
                        req.dismissed_by.as_str(),
                        req.dismissed_reason.as_str(),
                        req.updated_at_ms,
                    ],
                )
                .map_err(|err| format!("dismiss_dead_letter failed: {err}"))?;
            if updated == 0 {
                return Err(format!(
                    "dismiss_dead_letter not found or not open: {}",
                    req.dead_letter_id
                ));
            }
            Ok(())
        })
    }
}

impl RuntimeJobOutboxPort for SqliteRuntimeStore {
    fn list_pending_runtime_job_outbox(
        &self,
        limit: usize,
    ) -> Result<Vec<RuntimeJobOutboxRecord>, String> {
        self.with_conn(|conn| {
            let mut statement = conn
                .prepare("SELECT job_id,event_type,published_at_ms,generation FROM runtime_job_outbox WHERE published_at_ms IS NULL ORDER BY job_id,event_type LIMIT ?1")
                .map_err(|error| format!("prepare runtime job outbox list failed: {error}"))?;
            let records = statement
                .query_map(params![to_i64(limit)?], |row| {
                    let generation = row.get::<_, i64>(3)?;
                    Ok(RuntimeJobOutboxRecord {
                        job_id: row.get(0)?,
                        event_type: row.get(1)?,
                        published_at_ms: row.get(2)?,
                        generation: u32::try_from(generation).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(3, generation))?,
                    })
                })
                .map_err(|error| format!("query runtime job outbox failed: {error}"))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| format!("decode runtime job outbox failed: {error}"))?;
            Ok(records)
        })
    }

    fn mark_runtime_job_outbox_published(
        &self,
        job_id: &str,
        event_type: &str,
        generation: u32,
        published_at_ms: i64,
    ) -> Result<RuntimeJobOutboxPublishDisposition, String> {
        self.with_conn(|conn| {
            let updated = conn.execute(
                "UPDATE runtime_job_outbox SET published_at_ms=?1 WHERE job_id=?2 AND event_type=?3 AND generation=?4 AND published_at_ms IS NULL",
                params![published_at_ms, job_id, event_type, i64::from(generation)],
            ).map_err(|error| format!("mark runtime job outbox published failed: {error}"))?;
            if updated == 1 {
                return Ok(RuntimeJobOutboxPublishDisposition::Published);
            }
            let stored = conn
                .query_row(
                    "SELECT generation,published_at_ms FROM runtime_job_outbox WHERE job_id=?1 AND event_type=?2",
                    params![job_id, event_type],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<i64>>(1)?)),
                )
                .optional()
                .map_err(|error| format!("load runtime job outbox publish state failed: {error}"))?
                .ok_or_else(|| "runtime job outbox row not found".to_string())?;
            if stored.0 != i64::from(generation) {
                Ok(RuntimeJobOutboxPublishDisposition::Stale)
            } else if stored.1.is_some() {
                Ok(RuntimeJobOutboxPublishDisposition::AlreadyPublished)
            } else {
                Err("runtime job outbox publish CAS failed".to_string())
            }
        })
    }

    fn requeue_runtime_job_notifications(&self, published_before_ms: i64) -> Result<usize, String> {
        self.with_conn(|conn| conn.execute(
            "UPDATE runtime_job_outbox AS outbox SET published_at_ms=NULL,generation=generation+1 WHERE event_type='runtime_job.terminal' AND published_at_ms IS NOT NULL AND published_at_ms<=?1 AND EXISTS(SELECT 1 FROM runtime_jobs AS jobs WHERE jobs.job_id=outbox.job_id AND jobs.status IN('succeeded','failed','dead_lettered','cancelled'))",
            params![published_before_ms],
        ).map_err(|error| format!("requeue runtime job notifications failed: {error}")))
    }
}

pub(super) fn upsert_runtime_job_outbox_event(
    conn: &rusqlite::Connection,
    job_id: &str,
    event_type: &str,
) -> Result<(), String> {
    conn.execute(
        "INSERT INTO runtime_job_outbox(job_id,event_type,published_at_ms,generation) VALUES(?1,?2,NULL,0) ON CONFLICT(job_id,event_type) DO UPDATE SET published_at_ms=NULL,generation=runtime_job_outbox.generation+1",
        params![job_id, event_type],
    )
    .map(|_| ())
    .map_err(|error| format!("upsert runtime job outbox failed: {error}"))
}

pub(super) fn insert_runtime_job(
    conn: &rusqlite::Connection,
    job: &RuntimeJobRecord,
) -> Result<bool, String> {
    let backoff_policy_json = serde_json::to_string(&job.backoff_policy)
        .map_err(|err| format!("serialize runtime job backoff policy failed: {err}"))?;
    let output_refs_json = serde_json::to_string(&job.output_refs)
        .map_err(|err| format!("serialize runtime job output refs failed: {err}"))?;
    let inserted = conn
        .execute(
            "
            INSERT OR IGNORE INTO runtime_jobs(
                job_id, job_kind, status, run_at_ms, lease_owner, lease_expires_at_ms,
                retry_count, max_retries, backoff_policy_json, idempotency_key,
                session_id, branch_id, checkpoint_id, payload_ref, output_refs_json,
                last_error, created_at_ms, updated_at_ms, heartbeat_at_ms
            )
            VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)
            ",
            params![
                job.job_id.as_str(),
                job.job_kind.as_str(),
                runtime_job_status_to_db(&job.status),
                job.run_at_ms,
                job.lease_owner.as_deref(),
                job.lease_expires_at_ms,
                i64::from(job.retry_count),
                i64::from(job.max_retries),
                backoff_policy_json,
                job.idempotency_key.as_str(),
                job.session_id.as_deref(),
                job.branch_id.as_deref(),
                job.checkpoint_id.as_deref(),
                job.payload_ref.as_deref(),
                output_refs_json,
                job.last_error.as_deref(),
                job.created_at_ms,
                job.updated_at_ms,
                job.heartbeat_at_ms,
            ],
        )
        .map_err(|err| format!("insert runtime job failed: {err}"))?;
    Ok(inserted > 0)
}

pub(super) fn load_runtime_job_by_id(
    conn: &rusqlite::Connection,
    job_id: &str,
) -> Result<Option<RuntimeJobRecord>, String> {
    let mut stmt = conn
        .prepare(
            "
            SELECT job_id, job_kind, status, run_at_ms, lease_owner, lease_expires_at_ms,
                   retry_count, max_retries, backoff_policy_json, idempotency_key,
                   session_id, branch_id, checkpoint_id, payload_ref, output_refs_json,
                   last_error, created_at_ms, updated_at_ms, heartbeat_at_ms
            FROM runtime_jobs
            WHERE job_id = ?1
            LIMIT 1
            ",
        )
        .map_err(|err| format!("prepare load_runtime_job_by_id failed: {err}"))?;
    stmt.query_row(params![job_id], row_to_runtime_job)
        .optional()
        .map_err(|err| format!("query load_runtime_job_by_id failed: {err}"))
}

pub(super) fn load_runtime_job_by_kind_and_idempotency(
    conn: &rusqlite::Connection,
    job_kind: &str,
    idempotency_key: &str,
) -> Result<Option<RuntimeJobRecord>, String> {
    let mut stmt = conn
        .prepare(
            "
            SELECT job_id, job_kind, status, run_at_ms, lease_owner, lease_expires_at_ms,
                   retry_count, max_retries, backoff_policy_json, idempotency_key,
                   session_id, branch_id, checkpoint_id, payload_ref, output_refs_json,
                   last_error, created_at_ms, updated_at_ms, heartbeat_at_ms
            FROM runtime_jobs
            WHERE job_kind = ?1
              AND idempotency_key = ?2
            LIMIT 1
            ",
        )
        .map_err(|err| format!("prepare load_runtime_job_by_kind_and_idempotency failed: {err}"))?;
    stmt.query_row(params![job_kind, idempotency_key], row_to_runtime_job)
        .optional()
        .map_err(|err| format!("query load_runtime_job_by_kind_and_idempotency failed: {err}"))
}

fn load_runtime_jobs_by_ids(
    conn: &rusqlite::Connection,
    job_ids: &[String],
) -> Result<Vec<RuntimeJobRecord>, String> {
    if job_ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = vec!["?"; job_ids.len()].join(",");
    let sql = format!(
        "
        SELECT job_id, job_kind, status, run_at_ms, lease_owner, lease_expires_at_ms,
               retry_count, max_retries, backoff_policy_json, idempotency_key,
               session_id, branch_id, checkpoint_id, payload_ref, output_refs_json,
               last_error, created_at_ms, updated_at_ms, heartbeat_at_ms
        FROM runtime_jobs
        WHERE job_id IN ({placeholders})
        "
    );
    let mut stmt = conn
        .prepare(sql.as_str())
        .map_err(|err| format!("prepare load_runtime_jobs_by_ids failed: {err}"))?;
    let rows = stmt
        .query_map(params_from_iter(job_ids.iter()), row_to_runtime_job)
        .map_err(|err| format!("query load_runtime_jobs_by_ids failed: {err}"))?;
    let mut jobs = Vec::new();
    for row in rows {
        jobs.push(row.map_err(|err| format!("decode runtime job by ids failed: {err}"))?);
    }
    jobs.sort_by_key(|job| (job.run_at_ms, job.created_at_ms, job.job_id.clone()));
    Ok(jobs)
}

fn load_resource_claim(
    conn: &rusqlite::Connection,
    resource_kind: &str,
    resource_key: &str,
) -> Result<Option<ResourceClaimRecord>, String> {
    let mut stmt = conn
        .prepare(
            "
            SELECT resource_kind, resource_key, owner, owner_kind, session_id, branch_id,
                   expires_at_ms, metadata_json, created_at_ms, updated_at_ms
            FROM resource_claims
            WHERE resource_kind = ?1
              AND resource_key = ?2
            LIMIT 1
            ",
        )
        .map_err(|err| format!("prepare load_resource_claim failed: {err}"))?;
    stmt.query_row(params![resource_kind, resource_key], row_to_resource_claim)
        .optional()
        .map_err(|err| format!("query load_resource_claim failed: {err}"))
}

pub(super) fn insert_dead_letter_record(
    conn: &rusqlite::Connection,
    record: &DeadLetterRecord,
) -> Result<bool, String> {
    let replay_policy_json = serde_json::to_string(&record.replay_policy)
        .map_err(|err| format!("serialize dead letter replay policy failed: {err}"))?;
    let inserted = conn
        .execute(
            "
            INSERT OR IGNORE INTO dead_letters(
                dead_letter_id, original_job_id, job_kind, status, session_id, branch_id,
                checkpoint_id, payload_ref, idempotency_key, failure_reason, last_error,
                attempts, first_failed_at_ms, last_failed_at_ms, replay_policy_json,
                replayed_job_id, dismissed_by, dismissed_reason, updated_at_ms
            )
            VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)
            ",
            params![
                record.dead_letter_id.as_str(),
                record.original_job_id.as_str(),
                record.job_kind.as_str(),
                dead_letter_status_to_db(&record.status),
                record.session_id.as_deref(),
                record.branch_id.as_deref(),
                record.checkpoint_id.as_deref(),
                record.payload_ref.as_deref(),
                record.idempotency_key.as_str(),
                record.failure_reason.as_str(),
                record.last_error.as_str(),
                i64::from(record.attempts),
                record.first_failed_at_ms,
                record.last_failed_at_ms,
                replay_policy_json,
                record.replayed_job_id.as_deref(),
                record.dismissed_by.as_deref(),
                record.dismissed_reason.as_deref(),
                record.updated_at_ms,
            ],
        )
        .map_err(|err| format!("insert dead letter failed: {err}"))?;
    Ok(inserted > 0)
}

fn load_dead_letter_by_id(
    conn: &rusqlite::Connection,
    dead_letter_id: &str,
) -> Result<Option<DeadLetterRecord>, String> {
    let mut stmt = conn
        .prepare(
            "
            SELECT dead_letter_id, original_job_id, job_kind, status, session_id, branch_id,
                   checkpoint_id, payload_ref, idempotency_key, failure_reason, last_error,
                   attempts, first_failed_at_ms, last_failed_at_ms, replay_policy_json,
                   replayed_job_id, dismissed_by, dismissed_reason, updated_at_ms
            FROM dead_letters
            WHERE dead_letter_id = ?1
            LIMIT 1
            ",
        )
        .map_err(|err| format!("prepare load_dead_letter_by_id failed: {err}"))?;
    stmt.query_row(params![dead_letter_id], row_to_dead_letter)
        .optional()
        .map_err(|err| format!("query load_dead_letter_by_id failed: {err}"))
}

pub(super) fn load_dead_letter_by_original_job(
    conn: &rusqlite::Connection,
    original_job_id: &str,
) -> Result<Option<DeadLetterRecord>, String> {
    let mut stmt = conn
        .prepare(
            "
            SELECT dead_letter_id, original_job_id, job_kind, status, session_id, branch_id,
                   checkpoint_id, payload_ref, idempotency_key, failure_reason, last_error,
                   attempts, first_failed_at_ms, last_failed_at_ms, replay_policy_json,
                   replayed_job_id, dismissed_by, dismissed_reason, updated_at_ms
            FROM dead_letters
            WHERE original_job_id = ?1
            LIMIT 1
            ",
        )
        .map_err(|err| format!("prepare load_dead_letter_by_original_job failed: {err}"))?;
    stmt.query_row(params![original_job_id], row_to_dead_letter)
        .optional()
        .map_err(|err| format!("query load_dead_letter_by_original_job failed: {err}"))
}

fn list_runtime_jobs_with_filters(
    conn: &rusqlite::Connection,
    req: ListRuntimeJobsRequest,
) -> Result<Vec<RuntimeJobRecord>, String> {
    let mut conditions = Vec::new();
    let mut params_vec = Vec::<SqlValue>::new();

    if !req.statuses.is_empty() {
        let placeholders = vec!["?"; req.statuses.len()].join(",");
        conditions.push(format!("status IN ({placeholders})"));
        for status in req.statuses {
            params_vec.push(SqlValue::from(
                runtime_job_status_to_db(&status).to_string(),
            ));
        }
    }
    if let Some(job_kind) = req.job_kind {
        conditions.push("job_kind = ?".to_string());
        params_vec.push(SqlValue::from(job_kind));
    }
    if let Some(session_id) = req.session_id {
        conditions.push("session_id = ?".to_string());
        params_vec.push(SqlValue::from(session_id));
    }
    if let Some(branch_id) = req.branch_id {
        conditions.push("branch_id = ?".to_string());
        params_vec.push(SqlValue::from(branch_id));
    }

    let mut sql = "
        SELECT job_id, job_kind, status, run_at_ms, lease_owner, lease_expires_at_ms,
               retry_count, max_retries, backoff_policy_json, idempotency_key,
               session_id, branch_id, checkpoint_id, payload_ref, output_refs_json,
               last_error, created_at_ms, updated_at_ms, heartbeat_at_ms
        FROM runtime_jobs
    "
    .to_string();
    if !conditions.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(conditions.join(" AND ").as_str());
    }
    sql.push_str(" ORDER BY run_at_ms ASC, created_at_ms ASC, job_id ASC LIMIT ? OFFSET ?");
    params_vec.push(SqlValue::from(to_i64(req.limit)?));
    params_vec.push(SqlValue::from(to_i64(req.offset)?));

    let mut stmt = conn
        .prepare(sql.as_str())
        .map_err(|err| format!("prepare list_runtime_jobs_with_filters failed: {err}"))?;
    let rows = stmt
        .query_map(params_from_iter(params_vec.iter()), row_to_runtime_job)
        .map_err(|err| format!("query list_runtime_jobs_with_filters failed: {err}"))?;
    let mut jobs = Vec::new();
    for row in rows {
        jobs.push(row.map_err(|err| format!("decode list_runtime_jobs row failed: {err}"))?);
    }
    Ok(jobs)
}

fn list_dead_letters_with_filters(
    conn: &rusqlite::Connection,
    req: ListDeadLettersRequest,
) -> Result<Vec<DeadLetterRecord>, String> {
    let mut conditions = Vec::new();
    let mut params_vec = Vec::<SqlValue>::new();

    if !req.statuses.is_empty() {
        let placeholders = vec!["?"; req.statuses.len()].join(",");
        conditions.push(format!("status IN ({placeholders})"));
        for status in req.statuses {
            params_vec.push(SqlValue::from(
                dead_letter_status_to_db(&status).to_string(),
            ));
        }
    }
    if let Some(job_kind) = req.job_kind {
        conditions.push("job_kind = ?".to_string());
        params_vec.push(SqlValue::from(job_kind));
    }
    if let Some(session_id) = req.session_id {
        conditions.push("session_id = ?".to_string());
        params_vec.push(SqlValue::from(session_id));
    }
    if let Some(branch_id) = req.branch_id {
        conditions.push("branch_id = ?".to_string());
        params_vec.push(SqlValue::from(branch_id));
    }

    let mut sql = "
        SELECT dead_letter_id, original_job_id, job_kind, status, session_id, branch_id,
               checkpoint_id, payload_ref, idempotency_key, failure_reason, last_error,
               attempts, first_failed_at_ms, last_failed_at_ms, replay_policy_json,
               replayed_job_id, dismissed_by, dismissed_reason, updated_at_ms
        FROM dead_letters
    "
    .to_string();
    if !conditions.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(conditions.join(" AND ").as_str());
    }
    sql.push_str(" ORDER BY last_failed_at_ms DESC, dead_letter_id DESC LIMIT ? OFFSET ?");
    params_vec.push(SqlValue::from(to_i64(req.limit)?));
    params_vec.push(SqlValue::from(to_i64(req.offset)?));

    let mut stmt = conn
        .prepare(sql.as_str())
        .map_err(|err| format!("prepare list_dead_letters_with_filters failed: {err}"))?;
    let rows = stmt
        .query_map(params_from_iter(params_vec.iter()), row_to_dead_letter)
        .map_err(|err| format!("query list_dead_letters_with_filters failed: {err}"))?;
    let mut dead_letters = Vec::new();
    for row in rows {
        dead_letters
            .push(row.map_err(|err| format!("decode list_dead_letters row failed: {err}"))?);
    }
    Ok(dead_letters)
}

pub(super) fn runtime_job_status_to_db(status: &RuntimeJobStatus) -> &'static str {
    match status {
        RuntimeJobStatus::Queued => "queued",
        RuntimeJobStatus::Leased => "leased",
        RuntimeJobStatus::Running => "running",
        RuntimeJobStatus::Succeeded => "succeeded",
        RuntimeJobStatus::Failed => "failed",
        RuntimeJobStatus::DeadLettered => "dead_lettered",
        RuntimeJobStatus::Cancelled => "cancelled",
    }
}

fn runtime_job_status_from_db(raw: &str) -> Result<RuntimeJobStatus, String> {
    match raw {
        "queued" | "Queued" => Ok(RuntimeJobStatus::Queued),
        "leased" | "Leased" => Ok(RuntimeJobStatus::Leased),
        "running" | "Running" => Ok(RuntimeJobStatus::Running),
        "succeeded" | "Succeeded" => Ok(RuntimeJobStatus::Succeeded),
        "failed" | "Failed" => Ok(RuntimeJobStatus::Failed),
        "dead_lettered" | "DeadLettered" => Ok(RuntimeJobStatus::DeadLettered),
        "cancelled" | "Cancelled" => Ok(RuntimeJobStatus::Cancelled),
        other => Err(format!("unknown runtime job status: {other}")),
    }
}

fn dead_letter_status_to_db(status: &DeadLetterStatus) -> &'static str {
    match status {
        DeadLetterStatus::Open => "open",
        DeadLetterStatus::Replaying => "replaying",
        DeadLetterStatus::Replayed => "replayed",
        DeadLetterStatus::Dismissed => "dismissed",
    }
}

fn dead_letter_status_from_db(raw: &str) -> Result<DeadLetterStatus, String> {
    match raw {
        "open" | "Open" => Ok(DeadLetterStatus::Open),
        "replaying" | "Replaying" => Ok(DeadLetterStatus::Replaying),
        "replayed" | "Replayed" => Ok(DeadLetterStatus::Replayed),
        "dismissed" | "Dismissed" => Ok(DeadLetterStatus::Dismissed),
        other => Err(format!("unknown dead letter status: {other}")),
    }
}

pub(super) fn row_to_runtime_job(row: &rusqlite::Row<'_>) -> rusqlite::Result<RuntimeJobRecord> {
    let status_raw: String = row.get(2)?;
    let status = runtime_job_status_from_db(status_raw.as_str()).map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(
            2,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, err)),
        )
    })?;
    let retry_count_raw: i64 = row.get(6)?;
    let retry_count = u32::try_from(retry_count_raw).map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(6, rusqlite::types::Type::Integer, Box::new(err))
    })?;
    let max_retries_raw: i64 = row.get(7)?;
    let max_retries = u32::try_from(max_retries_raw).map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(7, rusqlite::types::Type::Integer, Box::new(err))
    })?;
    let backoff_policy_json: String = row.get(8)?;
    let backoff_policy: RuntimeBackoffPolicy = serde_json::from_str(backoff_policy_json.as_str())
        .map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(8, rusqlite::types::Type::Text, Box::new(err))
    })?;
    let output_refs_json: String = row.get(14)?;
    let output_refs: Vec<String> =
        serde_json::from_str(output_refs_json.as_str()).map_err(|err| {
            rusqlite::Error::FromSqlConversionFailure(
                14,
                rusqlite::types::Type::Text,
                Box::new(err),
            )
        })?;

    Ok(RuntimeJobRecord {
        job_id: row.get(0)?,
        job_kind: row.get(1)?,
        status,
        run_at_ms: row.get(3)?,
        lease_owner: row.get(4)?,
        lease_expires_at_ms: row.get(5)?,
        retry_count,
        max_retries,
        backoff_policy,
        idempotency_key: row.get(9)?,
        session_id: row.get(10)?,
        branch_id: row.get(11)?,
        checkpoint_id: row.get(12)?,
        payload_ref: row.get(13)?,
        output_refs,
        last_error: row.get(15)?,
        created_at_ms: row.get(16)?,
        updated_at_ms: row.get(17)?,
        heartbeat_at_ms: row.get(18)?,
    })
}

fn row_to_resource_claim(row: &rusqlite::Row<'_>) -> rusqlite::Result<ResourceClaimRecord> {
    Ok(ResourceClaimRecord {
        resource_kind: row.get(0)?,
        resource_key: row.get(1)?,
        owner: row.get(2)?,
        owner_kind: row.get(3)?,
        session_id: row.get(4)?,
        branch_id: row.get(5)?,
        expires_at_ms: row.get(6)?,
        metadata_json: row.get(7)?,
        created_at_ms: row.get(8)?,
        updated_at_ms: row.get(9)?,
    })
}

fn row_to_dead_letter(row: &rusqlite::Row<'_>) -> rusqlite::Result<DeadLetterRecord> {
    let status_raw: String = row.get(3)?;
    let status = dead_letter_status_from_db(status_raw.as_str()).map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(
            3,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, err)),
        )
    })?;
    let attempts_raw: i64 = row.get(11)?;
    let attempts = u32::try_from(attempts_raw).map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(11, rusqlite::types::Type::Integer, Box::new(err))
    })?;
    let replay_policy_json: String = row.get(14)?;
    let replay_policy: DeadLetterReplayPolicy = serde_json::from_str(replay_policy_json.as_str())
        .map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(14, rusqlite::types::Type::Text, Box::new(err))
    })?;

    Ok(DeadLetterRecord {
        dead_letter_id: row.get(0)?,
        original_job_id: row.get(1)?,
        job_kind: row.get(2)?,
        status,
        session_id: row.get(4)?,
        branch_id: row.get(5)?,
        checkpoint_id: row.get(6)?,
        payload_ref: row.get(7)?,
        idempotency_key: row.get(8)?,
        failure_reason: row.get(9)?,
        last_error: row.get(10)?,
        attempts,
        first_failed_at_ms: row.get(12)?,
        last_failed_at_ms: row.get(13)?,
        replay_policy,
        replayed_job_id: row.get(15)?,
        dismissed_by: row.get(16)?,
        dismissed_reason: row.get(17)?,
        updated_at_ms: row.get(18)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_db_path(suffix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "centaeris_sqlite_resource_claim_{suffix}_{}_{}.db",
            std::process::id(),
            nanos
        ))
    }

    fn acquire_req(owner: &str, now_ms: i64) -> AcquireResourceClaimRequest {
        AcquireResourceClaimRequest {
            resource_kind: "file".to_string(),
            resource_key: "D:/Projects/Centaeris/file.txt".to_string(),
            owner: owner.to_string(),
            owner_kind: "task".to_string(),
            session_id: Some("chat-resource-claim".to_string()),
            branch_id: Some("turn-resource-claim".to_string()),
            now_ms,
            ttl_ms: 30_000,
            metadata_json: "{}".to_string(),
        }
    }

    #[test]
    fn sqlite_resource_claim_conflicts_until_released() {
        let db_path = temp_db_path("conflicts_until_released");
        let store = SqliteRuntimeStore::new(&db_path).expect("create sqlite runtime store");

        let first = store
            .acquire_resource_claim(acquire_req("task-a", 1_000))
            .expect("first claim");
        assert_eq!(first.disposition, AcquireResourceClaimDisposition::Acquired);

        let conflict = store
            .acquire_resource_claim(acquire_req("task-b", 2_000))
            .expect("conflicting claim should return conflict disposition");
        assert_eq!(
            conflict.disposition,
            AcquireResourceClaimDisposition::Conflict
        );
        assert_eq!(conflict.claim.owner, "task-a");

        assert!(store
            .release_resource_claim(ReleaseResourceClaimRequest {
                resource_kind: "file".to_string(),
                resource_key: "D:/Projects/Centaeris/file.txt".to_string(),
                owner: "task-a".to_string(),
                released_at_ms: 3_000,
            })
            .expect("release first claim"));

        let second = store
            .acquire_resource_claim(acquire_req("task-b", 4_000))
            .expect("second claim after release");
        assert_eq!(
            second.disposition,
            AcquireResourceClaimDisposition::Acquired
        );
        assert_eq!(second.claim.owner, "task-b");

        let _ = std::fs::remove_file(db_path);
    }

    #[test]
    fn sqlite_resource_claim_reclaims_expired_claims() {
        let db_path = temp_db_path("reclaims_expired");
        let store = SqliteRuntimeStore::new(&db_path).expect("create sqlite runtime store");
        store
            .acquire_resource_claim(AcquireResourceClaimRequest {
                ttl_ms: 10,
                ..acquire_req("task-a", 1_000)
            })
            .expect("first claim");

        let reclaimed = store
            .reclaim_expired_resource_claims(1_011)
            .expect("reclaim expired claim");
        assert_eq!(reclaimed, 1);
        assert!(store
            .get_resource_claim("file", "D:/Projects/Centaeris/file.txt")
            .expect("get resource claim")
            .is_none());

        let _ = std::fs::remove_file(db_path);
    }

    #[test]
    fn sqlite_runtime_job_heartbeat_and_reclaim_reject_old_worker() {
        let db_path = temp_db_path("runtime_job_heartbeat");
        let store = SqliteRuntimeStore::new(&db_path).expect("create sqlite runtime store");
        store
            .schedule_runtime_job(ScheduleRuntimeJobRequest {
                job: RuntimeJobRecord {
                    job_id: "job_heartbeat".to_string(),
                    job_kind: "worker.noop".to_string(),
                    status: RuntimeJobStatus::Queued,
                    run_at_ms: 1,
                    lease_owner: None,
                    lease_expires_at_ms: None,
                    heartbeat_at_ms: None,
                    retry_count: 0,
                    max_retries: 1,
                    backoff_policy: RuntimeBackoffPolicy::default(),
                    idempotency_key: "heartbeat:1".to_string(),
                    session_id: None,
                    branch_id: None,
                    checkpoint_id: None,
                    payload_ref: None,
                    output_refs: vec![],
                    last_error: None,
                    created_at_ms: 1,
                    updated_at_ms: 1,
                },
            })
            .expect("schedule job");
        let claimed = store
            .claim_due_runtime_jobs(ClaimDueRuntimeJobsRequest {
                now_ms: 10,
                worker_id: "worker:old".to_string(),
                job_id: None,
                job_kind: None,
                session_id: None,
                limit: 1,
                lease_ms: 100,
            })
            .expect("claim job");
        assert_eq!(claimed[0].heartbeat_at_ms, Some(10));
        store
            .start_runtime_job(StartRuntimeJobRequest {
                job_id: "job_heartbeat".to_string(),
                lease_owner: "worker:old".to_string(),
                started_at_ms: 11,
            })
            .expect("start job");
        store
            .renew_runtime_job_lease(RenewRuntimeJobLeaseRequest {
                job_id: "job_heartbeat".to_string(),
                lease_owner: "worker:old".to_string(),
                heartbeat_at_ms: 20,
                lease_ms: 100,
            })
            .expect("renew job");
        assert_eq!(
            store
                .reclaim_expired_runtime_job_leases(119)
                .expect("not expired"),
            0
        );
        assert!(store
            .complete_runtime_job(CompleteRuntimeJobRequest {
                job_id: "job_heartbeat".to_string(),
                lease_owner: "worker:old".to_string(),
                output_refs: vec![],
                completed_at_ms: 120,
            })
            .is_err());
        assert_eq!(
            store
                .reclaim_expired_runtime_job_leases(120)
                .expect("expired"),
            1
        );
        store
            .claim_due_runtime_jobs(ClaimDueRuntimeJobsRequest {
                now_ms: 120,
                worker_id: "worker:new".to_string(),
                job_id: None,
                job_kind: None,
                session_id: None,
                limit: 1,
                lease_ms: 100,
            })
            .expect("reclaim job");
        assert!(store
            .complete_runtime_job(CompleteRuntimeJobRequest {
                job_id: "job_heartbeat".to_string(),
                lease_owner: "worker:old".to_string(),
                output_refs: vec![],
                completed_at_ms: 121,
            })
            .is_err());
        let _ = std::fs::remove_file(db_path);
    }
}
