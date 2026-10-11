use super::*;
use centaeris_core::session::reliability::{
    RuntimeJobFailureDisposition, ScheduleRuntimeJobDisposition, ScheduleRuntimeJobResult,
    RUNTIME_JOB_TERMINAL_EVENT,
};
use centaeris_core::session::store::{
    ConsumeWaitCheckpointRequest, CreateDeadLetterAndFailJobRequest, RuntimeStoreTransactionPort,
    SaveWaitCheckpointRequest, UpsertExternalContextAndScheduleJobRequest,
    UpsertExternalContextLinkAndCompleteJobRequest,
};
use rusqlite::{params, OptionalExtension};

impl RuntimeStoreTransactionPort for SqliteRuntimeStore {
    fn save_wait_checkpoint(&self, req: SaveWaitCheckpointRequest) -> Result<(), String> {
        req.validate()?;
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
        req.validate()?;
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
            upsert_external_context_link_and_complete_job_conn(conn, req, &mut |_| {})
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
            super::sqlite_reliability::fail_runtime_job_conn(&tx, &req.fail_job)?;
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

/// Internal observation seam for transaction outcome and deterministic fault tests.
/// The ordinary Store path passes a no-op observer.
#[derive(Debug, Clone)]
pub(super) enum TransactionEvent {
    Began,
    BeforeCommit,
    CommitAttempted,
    Committed,
    RolledBack,
    RollbackFailed,
}

pub(super) fn upsert_external_context_link_and_complete_job_conn(
    conn: &mut rusqlite::Connection,
    req: UpsertExternalContextLinkAndCompleteJobRequest,
    observe: &mut dyn FnMut(TransactionEvent),
) -> Result<(), String> {
    let tx = conn.transaction().map_err(|err| {
        format!("begin upsert_external_context_link_and_complete_job transaction failed: {err}")
    })?;
    observe(TransactionEvent::Began);
    let result = (|| {
        if let Some(object) = req.object.as_ref() {
            super::sqlite_external_context::upsert_external_context_object_conn(&tx, object)?;
        }
        if let Some(link) = req.link.as_ref() {
            super::sqlite_external_context::link_external_context_object_conn(&tx, link)?;
        }
        super::sqlite_reliability::complete_runtime_job_conn(&tx, &req.complete_job)?;
        super::sqlite_reliability::upsert_runtime_job_outbox_event(
            &tx,
            req.complete_job.job_id.as_str(),
            RUNTIME_JOB_TERMINAL_EVENT,
        )?;
        Ok(())
    })();
    if let Err(error) = result {
        match tx.rollback() {
            Ok(()) => observe(TransactionEvent::RolledBack),
            Err(_) => observe(TransactionEvent::RollbackFailed),
        }
        return Err(error);
    }
    observe(TransactionEvent::BeforeCommit);
    observe(TransactionEvent::CommitAttempted);
    tx.commit().map_err(|err| {
        format!("commit upsert_external_context_link_and_complete_job transaction failed: {err}")
    })?;
    observe(TransactionEvent::Committed);
    Ok(())
}
