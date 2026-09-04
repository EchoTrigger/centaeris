use super::*;
use centaeris_core::session::reliability::{
    RuntimeJobFailureDisposition, RuntimeJobStatus, ScheduleRuntimeJobDisposition,
    ScheduleRuntimeJobResult, RUNTIME_JOB_TERMINAL_EVENT,
};
use centaeris_core::session::store::{
    ConsumeWaitCheckpointRequest, CreateDeadLetterAndFailJobRequest, RuntimeStoreTransactionPort,
    SaveWaitCheckpointRequest, UpsertExternalContextAndScheduleJobRequest,
    UpsertExternalContextLinkAndCompleteJobRequest,
};
use rusqlite::{params, OptionalExtension};

impl RuntimeStoreTransactionPort for SqliteRuntimeStore {
    fn save_wait_checkpoint(&self, req: SaveWaitCheckpointRequest) -> Result<(), String> {
        validate_checkpoint_event_scope(&req.checkpoint, &req.event)?;
        self.with_conn(|conn| {
            let tx = conn
                .transaction()
                .map_err(|error| format!("begin save_wait_checkpoint failed: {error}"))?;
            let existing_checkpoint = tx
                .query_row(
                    "SELECT checkpoint_id,kind,session_id,turn_id,status,done_reason,updated_at_ms,payload_json FROM checkpoints WHERE session_id=?1 AND turn_id=?2 AND kind<>'recovery'",
                    params![req.checkpoint.session_id, req.checkpoint.turn_id],
                    super::sqlite_runtime::row_to_checkpoint,
                )
                .optional()
                .map_err(|error| format!("load existing wait checkpoint failed: {error}"))?;
            match existing_checkpoint {
                Some(existing) if existing != req.checkpoint => {
                    return Err("save_wait_checkpoint idempotency conflict".to_string())
                }
                Some(_) => {}
                None => super::sqlite_runtime::save_checkpoint_conn(&tx, &req.checkpoint)?,
            }
            super::sqlite_runtime::append_event_conn_idempotent(&tx, &req.event)?;
            tx.commit()
                .map_err(|error| format!("commit save_wait_checkpoint failed: {error}"))
        })
    }

    fn consume_wait_checkpoint(&self, req: ConsumeWaitCheckpointRequest) -> Result<(), String> {
        validate_consume_wait_checkpoint(&req)?;
        self.with_conn(|conn| {
            let tx = conn
                .transaction()
                .map_err(|error| format!("begin consume_wait_checkpoint failed: {error}"))?;
            let current = tx
                .query_row(
                    "SELECT checkpoint_id,kind,session_id,turn_id,status,done_reason,updated_at_ms,payload_json FROM checkpoints WHERE session_id=?1 AND turn_id=?2 AND kind<>'recovery'",
                    params![req.checkpoint.session_id, req.checkpoint.turn_id],
                    super::sqlite_runtime::row_to_checkpoint,
                )
                .optional()
                .map_err(|error| format!("load consumed wait checkpoint failed: {error}"))?;
            if current.as_ref().is_some_and(|current| current != &req.checkpoint) {
                return Err("consume_wait_checkpoint identity conflict".to_string());
            }
            let mut inserted_any = false;
            for event in &req.events {
                inserted_any |=
                    super::sqlite_runtime::append_event_conn_idempotent(&tx, event)?;
            }
            let Some(_) = current else {
                if inserted_any {
                    return Err("consume_wait_checkpoint missing checkpoint".to_string());
                }
                return tx.commit().map_err(|error| {
                    format!("commit idempotent consume_wait_checkpoint failed: {error}")
                });
            };
            let deleted = tx
                .execute(
                    "DELETE FROM checkpoints WHERE session_id=?1 AND turn_id=?2 AND kind<>'recovery'",
                    params![req.checkpoint.session_id, req.checkpoint.turn_id],
                )
                .map_err(|error| format!("delete consumed wait checkpoint failed: {error}"))?;
            if deleted != 1 {
                return Err("consume_wait_checkpoint delete mismatch".to_string());
            }
            tx.commit()
                .map_err(|error| format!("commit consume_wait_checkpoint failed: {error}"))
        })
    }

    fn upsert_external_context_and_schedule_job(
        &self,
        req: UpsertExternalContextAndScheduleJobRequest,
    ) -> Result<ScheduleRuntimeJobResult, String> {
        self.with_conn(|conn| {
            let tx = conn.transaction().map_err(|err| {
                format!("begin upsert_external_context_and_schedule_job transaction failed: {err}")
            })?;
            super::sqlite_external_context::upsert_external_context_object_conn(&tx, &req.object)?;
            let inserted = super::sqlite_reliability::insert_runtime_job(&tx, &req.job)?;
            let result = if inserted {
                ScheduleRuntimeJobResult {
                    disposition: ScheduleRuntimeJobDisposition::Inserted,
                    job: req.job,
                }
            } else {
                let job = super::sqlite_reliability::load_runtime_job_by_kind_and_idempotency(
                    &tx,
                    req.job.job_kind.as_str(),
                    req.job.idempotency_key.as_str(),
                )?
                .ok_or_else(|| {
                    format!(
                        "upsert_external_context_and_schedule_job existing row missing job_kind={} idempotency_key={}",
                        req.job.job_kind, req.job.idempotency_key
                    )
                })?;
                ScheduleRuntimeJobResult {
                    disposition: ScheduleRuntimeJobDisposition::Existing,
                    job,
                }
            };
            tx.commit().map_err(|err| {
                format!("commit upsert_external_context_and_schedule_job transaction failed: {err}")
            })?;
            Ok(result)
        })
    }

    fn upsert_external_context_link_and_complete_job(
        &self,
        req: UpsertExternalContextLinkAndCompleteJobRequest,
    ) -> Result<(), String> {
        self.with_conn(|conn| {
            let tx = conn.transaction().map_err(|err| {
                format!(
                    "begin upsert_external_context_link_and_complete_job transaction failed: {err}"
                )
            })?;
            if let Some(object) = req.object.as_ref() {
                super::sqlite_external_context::upsert_external_context_object_conn(&tx, object)?;
            }
            if let Some(link) = req.link.as_ref() {
                super::sqlite_external_context::link_external_context_object_conn(&tx, link)?;
            }
            complete_runtime_job_conn(&tx, &req.complete_job)?;
            super::sqlite_reliability::upsert_runtime_job_outbox_event(
                &tx,
                req.complete_job.job_id.as_str(),
                RUNTIME_JOB_TERMINAL_EVENT,
            )?;
            tx.commit().map_err(|err| {
                format!(
                    "commit upsert_external_context_link_and_complete_job transaction failed: {err}"
                )
            })?;
            Ok(())
        })
    }

    fn create_dead_letter_and_fail_job(
        &self,
        req: CreateDeadLetterAndFailJobRequest,
    ) -> Result<centaeris_core::session::reliability::CreateDeadLetterResult, String> {
        self.with_conn(|conn| {
            let tx = conn.transaction().map_err(|err| {
                format!("begin create_dead_letter_and_fail_job transaction failed: {err}")
            })?;
            let inserted = super::sqlite_reliability::insert_dead_letter_record(
                &tx,
                &req.dead_letter.dead_letter,
            )?;
            fail_runtime_job_conn(&tx, &req.fail_job)?;
            if !matches!(
                req.fail_job.disposition,
                RuntimeJobFailureDisposition::RetryScheduled
            ) {
                super::sqlite_reliability::upsert_runtime_job_outbox_event(
                    &tx,
                    req.fail_job.job_id.as_str(),
                    RUNTIME_JOB_TERMINAL_EVENT,
                )?;
            }
            let dead_letter = if inserted {
                req.dead_letter.dead_letter
            } else {
                super::sqlite_reliability::load_dead_letter_by_original_job(
                    &tx,
                    req.dead_letter.dead_letter.original_job_id.as_str(),
                )?
                .ok_or_else(|| {
                    format!(
                        "create_dead_letter_and_fail_job existing row missing original_job_id={}",
                        req.dead_letter.dead_letter.original_job_id
                    )
                })?
            };
            tx.commit().map_err(|err| {
                format!("commit create_dead_letter_and_fail_job transaction failed: {err}")
            })?;
            Ok(
                centaeris_core::session::reliability::CreateDeadLetterResult {
                    disposition: if inserted {
                        centaeris_core::session::reliability::CreateDeadLetterDisposition::Inserted
                    } else {
                        centaeris_core::session::reliability::CreateDeadLetterDisposition::Existing
                    },
                    dead_letter,
                },
            )
        })
    }
}

fn validate_checkpoint_event_scope(
    checkpoint: &centaeris_core::runtime::contracts::CheckpointRecord,
    event: &centaeris_core::runtime::contracts::RuntimeEvent,
) -> Result<(), String> {
    if checkpoint.session_id != event.session_id
        || event.task_id.as_deref() != Some(checkpoint.turn_id.as_str())
    {
        return Err(format!(
            "save_wait_checkpoint scope mismatch: checkpointChat={} eventChat={} checkpointTurn={} eventTask={:?}",
            checkpoint.session_id, event.session_id, checkpoint.turn_id, event.task_id
        ));
    }
    Ok(())
}

fn validate_consume_wait_checkpoint(req: &ConsumeWaitCheckpointRequest) -> Result<(), String> {
    if req.events.is_empty() {
        return Err("consume_wait_checkpoint requires events".to_string());
    }
    let mut event_ids = std::collections::HashSet::new();
    for event in &req.events {
        if event.session_id != req.checkpoint.session_id
            || event.task_id.as_deref() != Some(req.checkpoint.turn_id.as_str())
        {
            return Err("consume_wait_checkpoint event scope mismatch".to_string());
        }
        if !event_ids.insert(event.event_id.as_str()) {
            return Err("consume_wait_checkpoint duplicate event id".to_string());
        }
    }
    Ok(())
}

fn complete_runtime_job_conn(
    conn: &rusqlite::Connection,
    req: &centaeris_core::session::reliability::CompleteRuntimeJobRequest,
) -> Result<(), String> {
    let output_refs_json = serde_json::to_string(&req.output_refs)
        .map_err(|err| format!("serialize complete_runtime_job output_refs failed: {err}"))?;
    let updated = conn
        .execute(
            "
            UPDATE runtime_jobs
            SET status = ?1,
                output_refs_json = ?2,
                updated_at_ms = ?3,
                lease_owner = NULL,
                lease_expires_at_ms = NULL,
                last_error = NULL
            WHERE job_id = ?4
              AND lease_owner = ?5
              AND status IN ('leased', 'running')
              AND lease_expires_at_ms > ?3
            ",
            params![
                super::sqlite_reliability::runtime_job_status_to_db(&RuntimeJobStatus::Succeeded),
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
    Ok(())
}

fn fail_runtime_job_conn(
    conn: &rusqlite::Connection,
    req: &centaeris_core::session::reliability::FailRuntimeJobRequest,
) -> Result<(), String> {
    let (next_status, next_run_at_ms) = match req.disposition {
        RuntimeJobFailureDisposition::RetryScheduled => {
            let next = req.next_run_at_ms.ok_or_else(|| {
                format!(
                    "fail_runtime_job retry_scheduled requires next_run_at_ms job_id={}",
                    req.job_id
                )
            })?;
            (RuntimeJobStatus::Queued, next)
        }
        RuntimeJobFailureDisposition::Failed => (RuntimeJobStatus::Failed, req.failed_at_ms),
        RuntimeJobFailureDisposition::DeadLettered => {
            (RuntimeJobStatus::DeadLettered, req.failed_at_ms)
        }
    };
    let updated = conn
        .execute(
            "
            UPDATE runtime_jobs
            SET status = ?1,
                run_at_ms = ?2,
                retry_count = retry_count + 1,
                lease_owner = NULL,
                lease_expires_at_ms = NULL,
                last_error = ?3,
                updated_at_ms = ?4
            WHERE job_id = ?5
              AND lease_owner = ?6
              AND status IN ('leased', 'running')
              AND lease_expires_at_ms > ?4
            ",
            params![
                super::sqlite_reliability::runtime_job_status_to_db(&next_status),
                next_run_at_ms,
                req.last_error.as_str(),
                req.failed_at_ms,
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
    Ok(())
}
