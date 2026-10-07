use centaeris_core::session::reliability::{
    CreateDeadLetterDisposition, CreateDeadLetterResult, RuntimeJobFailureDisposition,
    ScheduleRuntimeJobDisposition, ScheduleRuntimeJobResult, RUNTIME_JOB_TERMINAL_EVENT,
};
use centaeris_core::session::store::{
    ConsumeWaitCheckpointRequest, CreateDeadLetterAndFailJobRequest, RuntimeStoreTransactionPort,
    SaveWaitCheckpointRequest, UpsertExternalContextAndScheduleJobRequest,
    UpsertExternalContextLinkAndCompleteJobRequest,
};

use super::external_context::{link_object, upsert_object};
use super::reliability::{
    insert_dead_letter, insert_job, load_dead_letter_by_original, load_job_by_key,
    upsert_job_outbox,
};
use super::runtime::{append_runtime_event_idempotent, load_checkpoint, save_checkpoint};
use super::PostgresRuntimeStore;

impl RuntimeStoreTransactionPort for PostgresRuntimeStore {
    fn save_wait_checkpoint(&self, req: SaveWaitCheckpointRequest) -> Result<(), String> {
        validate_checkpoint_event_scope(&req.checkpoint, &req.event)?;
        self.with_client(|client| {
            let mut tx = client
                .transaction()
                .map_err(|error| format!("begin Postgres save_wait_checkpoint failed: {error}"))?;
            match load_checkpoint(
                &mut tx,
                req.checkpoint.session_id.as_str(),
                req.checkpoint.turn_id.as_str(),
            )? {
                Some(existing) if existing != req.checkpoint => {
                    return Err("save_wait_checkpoint idempotency conflict".to_string())
                }
                Some(_) => {}
                None => save_checkpoint(&mut tx, &req.checkpoint)?,
            }
            append_runtime_event_idempotent(&mut tx, &req.event)?;
            tx.commit()
                .map_err(|error| format!("commit Postgres save_wait_checkpoint failed: {error}"))
        })
    }

    fn consume_wait_checkpoint(&self, req: ConsumeWaitCheckpointRequest) -> Result<(), String> {
        validate_consume_wait_checkpoint(&req)?;
        self.with_client(|client| {
            let mut tx = client.transaction().map_err(|error| {
                format!("begin Postgres consume_wait_checkpoint failed: {error}")
            })?;
            let current = load_checkpoint(
                &mut tx,
                req.checkpoint.session_id.as_str(),
                req.checkpoint.turn_id.as_str(),
            )?;
            if current
                .as_ref()
                .is_some_and(|current| current != &req.checkpoint)
            {
                return Err("consume_wait_checkpoint identity conflict".to_string());
            }
            let mut inserted_any = false;
            for event in &req.events {
                inserted_any |= append_runtime_event_idempotent(&mut tx, event)?;
            }
            let Some(_) = current else {
                if inserted_any {
                    return Err("consume_wait_checkpoint missing checkpoint".to_string());
                }
                return tx.commit().map_err(|error| {
                    format!("commit idempotent Postgres consume_wait_checkpoint failed: {error}")
                });
            };
            let deleted = tx
                .execute(
                    "DELETE FROM checkpoints WHERE session_id=$1 AND turn_id=$2 AND kind<>'recovery'",
                    &[&req.checkpoint.session_id, &req.checkpoint.turn_id],
                )
                .map_err(|error| {
                    format!("delete Postgres consumed wait checkpoint failed: {error}")
                })?;
            if deleted != 1 {
                return Err("consume_wait_checkpoint delete mismatch".to_string());
            }
            tx.commit()
                .map_err(|error| format!("commit Postgres consume_wait_checkpoint failed: {error}"))
        })
    }

    fn upsert_external_context_and_schedule_job(
        &self,
        req: UpsertExternalContextAndScheduleJobRequest,
    ) -> Result<ScheduleRuntimeJobResult, String> {
        self.with_client(|client| {
            let mut tx = client
                .transaction()
                .map_err(|e| format!("begin Postgres external schedule transaction failed: {e}"))?;
            upsert_object(&mut tx, &req.object)?;
            let inserted = insert_job(&mut tx, &req.job)?;
            let job = if inserted {
                req.job
            } else {
                load_job_by_key(&mut tx, &req.job.job_kind, &req.job.idempotency_key)?
                    .ok_or("existing scheduled job missing")?
            };
            tx.commit()
                .map_err(|e| format!("commit Postgres external schedule failed: {e}"))?;
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

    fn upsert_external_context_link_and_complete_job(
        &self,
        req: UpsertExternalContextLinkAndCompleteJobRequest,
    ) -> Result<(), String> {
        self.with_client(|client| {
            let mut tx = client
                .transaction()
                .map_err(|e| format!("begin Postgres external complete transaction failed: {e}"))?;
            if let Some(object) = req.object.as_ref() {
                upsert_object(&mut tx, object)?;
            }
            if let Some(link) = req.link.as_ref() {
                link_object(&mut tx, link)?;
            }
            let outputs =
                serde_json::to_string(&req.complete_job.output_refs).map_err(|e| e.to_string())?;
            let updated = tx.execute("UPDATE runtime_jobs SET status='succeeded',output_refs_json=$1,updated_at_ms=$2,lease_owner=NULL,lease_expires_at_ms=NULL,last_error=NULL WHERE job_id=$3 AND status IN('leased','running') AND lease_owner=$4 AND lease_expires_at_ms>$2", &[&outputs,&req.complete_job.completed_at_ms,&req.complete_job.job_id,&req.complete_job.lease_owner]).map_err(|e|format!("complete Postgres external job failed: {e}"))?;
            if updated == 0 {
                return Err("complete external job state mismatch".to_string());
            }
            upsert_job_outbox(
                &mut tx,
                req.complete_job.job_id.as_str(),
                RUNTIME_JOB_TERMINAL_EVENT,
            )?;
            tx.commit()
                .map_err(|e| format!("commit Postgres external complete failed: {e}"))
        })
    }

    fn create_dead_letter_and_fail_job(
        &self,
        req: CreateDeadLetterAndFailJobRequest,
    ) -> Result<CreateDeadLetterResult, String> {
        self.with_client(|client| {
            let mut tx = client
                .transaction()
                .map_err(|e| format!("begin Postgres DLQ fail transaction failed: {e}"))?;
            let inserted = insert_dead_letter(&mut tx, &req.dead_letter.dead_letter)?;
            let dead_letter = if inserted {
                req.dead_letter.dead_letter
            } else {
                load_dead_letter_by_original(
                    &mut tx,
                    &req.dead_letter.dead_letter.original_job_id,
                )?
                .ok_or("existing DLQ row missing")?
            };
            let (status, run) = match req.fail_job.disposition {
                RuntimeJobFailureDisposition::RetryScheduled => (
                    "queued",
                    req.fail_job
                        .next_run_at_ms
                        .ok_or("retry requires next run")?,
                ),
                RuntimeJobFailureDisposition::Failed => ("failed", req.fail_job.failed_at_ms),
                RuntimeJobFailureDisposition::DeadLettered => {
                    ("dead_lettered", req.fail_job.failed_at_ms)
                }
            };
            let updated = tx.execute("UPDATE runtime_jobs SET status=$1,run_at_ms=$2,retry_count=retry_count+1,updated_at_ms=$3,lease_owner=NULL,lease_expires_at_ms=NULL,last_error=$4 WHERE job_id=$5 AND status IN('leased','running') AND lease_owner=$6 AND lease_expires_at_ms>$3", &[&status,&run,&req.fail_job.failed_at_ms,&req.fail_job.last_error,&req.fail_job.job_id,&req.fail_job.lease_owner]).map_err(|e|format!("fail Postgres runtime job in DLQ transaction failed: {e}"))?;
            if updated == 0 {
                return Err("fail job in DLQ transaction state mismatch".to_string());
            }
            if !matches!(
                req.fail_job.disposition,
                RuntimeJobFailureDisposition::RetryScheduled
            ) {
                upsert_job_outbox(
                    &mut tx,
                    req.fail_job.job_id.as_str(),
                    RUNTIME_JOB_TERMINAL_EVENT,
                )?;
            }
            tx.commit()
                .map_err(|e| format!("commit Postgres DLQ fail failed: {e}"))?;
            Ok(CreateDeadLetterResult {
                disposition: if inserted {
                    CreateDeadLetterDisposition::Inserted
                } else {
                    CreateDeadLetterDisposition::Existing
                },
                dead_letter,
            })
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
