use std::fs;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

#[cfg(test)]
use rusqlite::params;
use rusqlite::Connection;

#[cfg(test)]
mod agent_runtime_fault_tests;

#[path = "sqlite_store/sqlite_external_context.rs"]
mod sqlite_external_context;
#[path = "sqlite_store/sqlite_reliability.rs"]
mod sqlite_reliability;
#[path = "sqlite_store/sqlite_runtime.rs"]
mod sqlite_runtime;
#[path = "sqlite_store/sqlite_schema.rs"]
mod sqlite_schema;
#[path = "sqlite_store/sqlite_transactions.rs"]
mod sqlite_transactions;
#[path = "sqlite_store/sqlite_turn_supplement.rs"]
mod sqlite_turn_supplement;

pub const STORE_SCHEMA_VERSION: i64 = 1;

#[derive(Debug, Clone)]
pub struct SqliteRuntimeStore {
    db_path: PathBuf,
    #[cfg(test)]
    fail_next_session_snapshot_save: Arc<AtomicBool>,
}

impl SqliteRuntimeStore {
    pub fn new<P: AsRef<Path>>(db_path: P) -> Result<Self, String> {
        let path = db_path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|err| format!("create runtime store directory failed: {err}"))?;
        }

        let store = Self {
            db_path: path,
            #[cfg(test)]
            fail_next_session_snapshot_save: Arc::new(AtomicBool::new(false)),
        };
        let conn = Connection::open(&store.db_path)
            .map_err(|err| format!("open runtime sqlite failed: {err}"))?;
        sqlite_schema::ensure_schema(&conn)?;
        configure_conn(&conn)?;
        Ok(store)
    }

    #[cfg(test)]
    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    #[cfg(test)]
    pub(crate) fn fail_next_agent_runtime_snapshot_save(&self) {
        self.fail_next_session_snapshot_save
            .store(true, Ordering::SeqCst);
    }

    pub(crate) fn with_conn<T, F>(&self, f: F) -> Result<T, String>
    where
        F: FnOnce(&mut Connection) -> Result<T, String>,
    {
        let mut conn = Connection::open(&self.db_path)
            .map_err(|err| format!("open runtime sqlite failed: {err}"))?;
        configure_conn(&conn)?;
        f(&mut conn)
    }

    pub(crate) fn with_conn_error<T, F, E>(&self, f: F) -> Result<T, E>
    where
        F: FnOnce(&mut Connection) -> Result<T, E>,
        E: From<String>,
    {
        let mut conn = Connection::open(&self.db_path)
            .map_err(|err| E::from(format!("open runtime sqlite failed: {err}")))?;
        configure_conn(&conn).map_err(E::from)?;
        f(&mut conn)
    }
}

fn configure_conn(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "
        PRAGMA journal_mode = WAL;
        PRAGMA synchronous = NORMAL;
        PRAGMA foreign_keys = ON;
        ",
    )
    .map_err(|err| format!("configure sqlite pragmas failed: {err}"))
}

fn to_i64(value: usize) -> Result<i64, String> {
    i64::try_from(value).map_err(|_| format!("usize to i64 overflow: {value}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use centaeris_core::runtime::contracts::{
        CheckpointKindV1, CheckpointRecord, EventVisibility, RuntimeEvent,
    };
    use centaeris_core::session::external_context::{
        ExternalContextObject, ExternalContextObjectLink, ExternalContextStorePort,
        EXTERNAL_CONTEXT_SCHEMA_VERSION,
    };
    use centaeris_core::session::reliability::{
        CancelRuntimeJobRequest, CompleteRuntimeJobRequest, CreateDeadLetterDisposition,
        CreateDeadLetterRequest, DeadLetterRecord, DeadLetterReplayPolicy, DeadLetterStatus,
        DeadLetterStorePort, FailRuntimeJobRequest, RuntimeBackoffPolicy,
        RuntimeJobFailureDisposition, RuntimeJobOutboxPort, RuntimeJobRecord, RuntimeJobStatus,
        RuntimeJobStorePort, ScheduleRuntimeJobDisposition, WakeRuntimeJobDisposition,
        WakeRuntimeJobRequest, YieldRuntimeJobRequest,
    };
    use centaeris_core::session::store::{
        AgentRuntimeSnapshotStorePort, ConsumeWaitCheckpointRequest,
        CreateDeadLetterAndFailJobRequest, RuntimeStore, RuntimeStoreTransactionPort,
        SaveWaitCheckpointRequest, SessionDataStorePort,
        UpsertExternalContextAndScheduleJobRequest, UpsertExternalContextLinkAndCompleteJobRequest,
    };
    use centaeris_core::session::supplement::{
        AcknowledgeTurnSupplementsRequest, ClaimTurnSupplementsRequest,
        CloseTurnSupplementQueueRequest, EnqueueTurnSupplementDisposition,
        EnqueueTurnSupplementRequest, TurnSupplementStoreError, TurnSupplementStorePort,
        MAX_PENDING_TURN_SUPPLEMENTS,
    };
    fn temp_db_path(suffix: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "centaeris_sqlite_store_{suffix}_{}_{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time")
                .as_nanos()
        ))
    }

    fn table_exists(db_path: &Path, table_name: &str) -> bool {
        let conn = Connection::open(db_path).expect("open sqlite db");
        conn.query_row(
            "
            SELECT EXISTS(
                SELECT 1
                FROM sqlite_master
                WHERE type = 'table' AND name = ?1
            )
            ",
            params![table_name],
            |row| row.get::<_, i64>(0),
        )
        .expect("query sqlite schema")
            != 0
    }

    fn external_context_object(object_id: &str) -> ExternalContextObject {
        ExternalContextObject {
            schema_version: EXTERNAL_CONTEXT_SCHEMA_VERSION.to_string(),
            object_id: object_id.to_string(),
            object_kind: "testObject".to_string(),
            source_provider_id: "test-provider".to_string(),
            source_tool_name: "test-tool".to_string(),
            title: format!("Object {object_id}"),
            content: "{\"ok\":true}".to_string(),
            metadata: serde_json::json!({"source": "sqlite-test"}),
            updated_at_ms: 100,
        }
    }

    fn runtime_job(
        job_id: &str,
        status: RuntimeJobStatus,
        lease_owner: Option<&str>,
    ) -> RuntimeJobRecord {
        RuntimeJobRecord {
            job_id: job_id.to_string(),
            job_kind: "test.job".to_string(),
            status,
            run_at_ms: 100,
            lease_owner: lease_owner.map(ToString::to_string),
            lease_expires_at_ms: lease_owner.map(|_| 1_000),
            heartbeat_at_ms: lease_owner.map(|_| 1),
            retry_count: 0,
            max_retries: 1,
            backoff_policy: RuntimeBackoffPolicy::default(),
            idempotency_key: format!("test.job:{job_id}"),
            session_id: Some("chat-sqlite-tx".to_string()),
            branch_id: Some("turn-sqlite-tx".to_string()),
            checkpoint_id: None,
            payload_ref: None,
            output_refs: vec![],
            last_error: None,
            created_at_ms: 100,
            updated_at_ms: 100,
        }
    }

    fn agent_run_lifecycle_job(
        agent_run_id: &str,
        session_id: &str,
        digest: &str,
    ) -> RuntimeJobRecord {
        let mut job = runtime_job(
            format!("agent_run.lifecycle:{agent_run_id}").as_str(),
            RuntimeJobStatus::Running,
            Some("worker-supplement"),
        );
        job.job_kind =
            centaeris_core::session::reliability::AGENT_RUN_LIFECYCLE_JOB_KIND.to_string();
        job.idempotency_key = format!("agent_run.lifecycle:{agent_run_id}:{digest}");
        job.session_id = Some(session_id.to_string());
        job.branch_id = None;
        job.payload_ref = Some(format!("record:agent_run:{agent_run_id}"));
        job
    }

    fn dead_letter_record(dead_letter_id: &str, original_job_id: &str) -> DeadLetterRecord {
        DeadLetterRecord {
            dead_letter_id: dead_letter_id.to_string(),
            original_job_id: original_job_id.to_string(),
            job_kind: "test.job".to_string(),
            status: DeadLetterStatus::Open,
            session_id: Some("chat-sqlite-tx".to_string()),
            branch_id: Some("turn-sqlite-tx".to_string()),
            checkpoint_id: None,
            payload_ref: None,
            idempotency_key: format!("test.job:{original_job_id}"),
            failure_reason: "test_failure".to_string(),
            last_error: "test failure".to_string(),
            attempts: 1,
            first_failed_at_ms: 200,
            last_failed_at_ms: 200,
            replay_policy: DeadLetterReplayPolicy::default(),
            replayed_job_id: None,
            dismissed_by: None,
            dismissed_reason: None,
            updated_at_ms: 200,
        }
    }

    #[test]
    fn sqlite_schema_creates_runtime_jobs_table() {
        let db_path = temp_db_path("runtime_jobs_schema");
        let store = SqliteRuntimeStore::new(&db_path).expect("create sqlite runtime store");

        assert!(table_exists(store.db_path(), "runtime_jobs"));

        let _ = std::fs::remove_file(db_path);
    }

    #[test]
    fn sqlite_turn_supplements_are_durable_bounded_and_lease_fenced() {
        let db_path = temp_db_path("turn_supplement_fifo");
        let store = SqliteRuntimeStore::new(&db_path).expect("create sqlite runtime store");
        let agent_run_id = "agent-run-supplement";
        let session_id = "chat-supplement";
        let digest = "digest-supplement";
        let job_id = format!("agent_run.lifecycle:{agent_run_id}");
        store
            .schedule_runtime_job(
                centaeris_core::session::reliability::ScheduleRuntimeJobRequest {
                    job: agent_run_lifecycle_job(agent_run_id, session_id, digest),
                },
            )
            .expect("schedule run lifecycle job");
        assert_eq!(
            store
                .enqueue_turn_supplement(EnqueueTurnSupplementRequest {
                    agent_run_id: agent_run_id.to_string(),
                    lifecycle_job_id: job_id.clone(),
                    session_id: session_id.to_string(),
                    authorization_digest: "banana".to_string(),
                    supplement_id: "wrong-identity".to_string(),
                    message: "banana".to_string(),
                    created_at_ms: 99,
                })
                .expect_err("authorization mismatch must fail"),
            TurnSupplementStoreError::IdentityMismatch
        );

        for index in 0..MAX_PENDING_TURN_SUPPLEMENTS {
            let result = store
                .enqueue_turn_supplement(EnqueueTurnSupplementRequest {
                    agent_run_id: agent_run_id.to_string(),
                    lifecycle_job_id: job_id.clone(),
                    session_id: session_id.to_string(),
                    authorization_digest: digest.to_string(),
                    supplement_id: format!("supplement-{index}"),
                    message: format!("message {index}"),
                    created_at_ms: 100 + index as i64,
                })
                .expect("enqueue supplement");
            assert_eq!(
                result.disposition,
                EnqueueTurnSupplementDisposition::Accepted
            );
            assert_eq!(result.queued_count, index + 1);
        }
        assert_eq!(
            store
                .enqueue_turn_supplement(EnqueueTurnSupplementRequest {
                    agent_run_id: agent_run_id.to_string(),
                    lifecycle_job_id: job_id.clone(),
                    session_id: session_id.to_string(),
                    authorization_digest: digest.to_string(),
                    supplement_id: "supplement-0".to_string(),
                    message: "message 0".to_string(),
                    created_at_ms: 200,
                })
                .expect("duplicate supplement")
                .disposition,
            EnqueueTurnSupplementDisposition::Duplicate
        );
        assert_eq!(
            store
                .enqueue_turn_supplement(EnqueueTurnSupplementRequest {
                    agent_run_id: agent_run_id.to_string(),
                    lifecycle_job_id: job_id.clone(),
                    session_id: session_id.to_string(),
                    authorization_digest: digest.to_string(),
                    supplement_id: "supplement-overflow".to_string(),
                    message: "banana".to_string(),
                    created_at_ms: 201,
                })
                .expect_err("ninth pending supplement must fail"),
            TurnSupplementStoreError::QueueFull
        );

        let claim = |claim_token: &str, lease_owner: &str| ClaimTurnSupplementsRequest {
            agent_run_id: agent_run_id.to_string(),
            lifecycle_job_id: job_id.clone(),
            session_id: session_id.to_string(),
            authorization_digest: digest.to_string(),
            lease_owner: lease_owner.to_string(),
            claim_token: claim_token.to_string(),
            now_ms: 300,
            close_if_empty: false,
            limit: MAX_PENDING_TURN_SUPPLEMENTS,
        };
        assert_eq!(
            store
                .claim_turn_supplements(ClaimTurnSupplementsRequest {
                    lease_owner: "banana".to_string(),
                    ..claim("wrong-lease", "worker-supplement")
                })
                .expect_err("stale lease must fail"),
            TurnSupplementStoreError::LeaseFenceRejected
        );
        let first_claim = store
            .claim_turn_supplements(claim("claim-before-crash", "worker-supplement"))
            .expect("claim supplements");
        assert_eq!(
            first_claim
                .iter()
                .map(|item| item.supplement_id.as_str())
                .collect::<Vec<_>>(),
            (0..MAX_PENDING_TURN_SUPPLEMENTS)
                .map(|index| format!("supplement-{index}"))
                .collect::<Vec<_>>()
        );
        assert!(store
            .claim_turn_supplements(claim("claim-before-crash", "worker-supplement"))
            .expect("same process does not rematerialize")
            .is_empty());
        assert_eq!(
            store
                .claim_turn_supplements(claim("claim-same-lease", "worker-supplement"))
                .expect_err("same lease cannot steal an in-flight claim"),
            TurnSupplementStoreError::ClaimInProgress
        );
        store
            .with_conn(|connection| {
                connection
                    .execute(
                        "UPDATE runtime_jobs SET lease_owner='worker-after-crash',lease_expires_at_ms=1000 WHERE job_id=?1",
                        params![job_id],
                    )
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            })
            .expect("transfer lifecycle lease after crash");
        let recovered = store
            .claim_turn_supplements(claim("claim-after-crash", "worker-after-crash"))
            .expect("new lease reclaims durable supplements");
        store
            .acknowledge_turn_supplements(AcknowledgeTurnSupplementsRequest {
                agent_run_id: agent_run_id.to_string(),
                lifecycle_job_id: job_id.clone(),
                session_id: session_id.to_string(),
                authorization_digest: digest.to_string(),
                lease_owner: "worker-after-crash".to_string(),
                claim_token: "claim-after-crash".to_string(),
                supplement_ids: recovered
                    .iter()
                    .map(|item| item.supplement_id.clone())
                    .collect(),
                acknowledged_at_ms: 301,
            })
            .expect("acknowledge supplements");
        let consumed_duplicate = store
            .enqueue_turn_supplement(EnqueueTurnSupplementRequest {
                agent_run_id: agent_run_id.to_string(),
                lifecycle_job_id: job_id.clone(),
                session_id: session_id.to_string(),
                authorization_digest: digest.to_string(),
                supplement_id: "supplement-0".to_string(),
                message: "message 0".to_string(),
                created_at_ms: 302,
            })
            .expect("consumed idempotent retry");
        assert_eq!(
            consumed_duplicate.disposition,
            EnqueueTurnSupplementDisposition::Duplicate
        );
        assert_eq!(consumed_duplicate.queued_count, 0);
        assert_eq!(
            store
                .enqueue_turn_supplement(EnqueueTurnSupplementRequest {
                    agent_run_id: agent_run_id.to_string(),
                    lifecycle_job_id: job_id.clone(),
                    session_id: session_id.to_string(),
                    authorization_digest: digest.to_string(),
                    supplement_id: "supplement-0".to_string(),
                    message: "banana".to_string(),
                    created_at_ms: 302,
                })
                .expect_err("consumed id cannot change payload"),
            TurnSupplementStoreError::IdempotencyConflict
        );
        store
            .enqueue_turn_supplement(EnqueueTurnSupplementRequest {
                agent_run_id: agent_run_id.to_string(),
                lifecycle_job_id: job_id.clone(),
                session_id: session_id.to_string(),
                authorization_digest: digest.to_string(),
                supplement_id: "supplement-before-cancel".to_string(),
                message: "clear this pending item".to_string(),
                created_at_ms: 303,
            })
            .expect("enqueue before cancellation");
        store
            .with_conn(|connection| {
                connection
                    .execute(
                        "INSERT INTO runtime_events(event_id,session_id,task_id,event_type,at_ms,visibility,payload_json) VALUES(?1,?2,?3,'runtime.agent_run.cancel_requested.v1',303,'internal','{}')",
                        params![format!("agent_run_cancel_requested:{agent_run_id}"), session_id, agent_run_id],
                    )
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            })
            .expect("persist cancellation fact");
        store
            .close_turn_supplement_queue(CloseTurnSupplementQueueRequest {
                agent_run_id: agent_run_id.to_string(),
                lifecycle_job_id: job_id.clone(),
                session_id: session_id.to_string(),
                authorization_digest: digest.to_string(),
                lease_owner: Some("worker-after-crash".to_string()),
                reason: "agent_run_cancel_requested".to_string(),
                closed_at_ms: 304,
            })
            .expect("cancel close clears pending queue");
        assert_eq!(
            store
                .enqueue_turn_supplement(EnqueueTurnSupplementRequest {
                    agent_run_id: agent_run_id.to_string(),
                    lifecycle_job_id: job_id.clone(),
                    session_id: session_id.to_string(),
                    authorization_digest: digest.to_string(),
                    supplement_id: "supplement-after-terminal".to_string(),
                    message: "banana".to_string(),
                    created_at_ms: 305,
                })
                .expect_err("closed queue must reject admission"),
            TurnSupplementStoreError::AdmissionClosed
        );
        store
            .with_conn(|connection| {
                connection
                    .execute("DELETE FROM runtime_jobs WHERE job_id=?1", params![job_id])
                    .map_err(|error| error.to_string())?;
                let queues = connection
                    .query_row(
                        "SELECT COUNT(*) FROM runtime_turn_supplement_queues WHERE agent_run_id=?1",
                        params![agent_run_id],
                        |row| row.get::<_, i64>(0),
                    )
                    .map_err(|error| error.to_string())?;
                assert_eq!(queues, 0);
                Ok(())
            })
            .expect("lifecycle deletion cascades to supplement queue");

        let _ = std::fs::remove_file(db_path);
    }

    #[test]
    fn sqlite_turn_supplement_does_not_wake_any_runtime_wait_checkpoint() {
        let db_path = temp_db_path("turn_supplement_waits");
        let store = SqliteRuntimeStore::new(&db_path).expect("create sqlite runtime store");

        for (index, reason) in ["question", "runtime_job"].into_iter().enumerate() {
            let agent_run_id = format!("agent-run-supplement-wait-{index}");
            let session_id = format!("chat-supplement-wait-{index}");
            let digest = format!("digest-supplement-wait-{index}");
            let job_id = format!("agent_run.lifecycle:{agent_run_id}");
            let mut job = agent_run_lifecycle_job(&agent_run_id, &session_id, &digest);
            job.status = RuntimeJobStatus::Queued;
            job.run_at_ms = 10_000;
            job.lease_owner = None;
            job.lease_expires_at_ms = None;
            job.heartbeat_at_ms = None;
            store
                .schedule_runtime_job(
                    centaeris_core::session::reliability::ScheduleRuntimeJobRequest { job },
                )
                .expect("schedule waiting lifecycle job");
            store
                .save_checkpoint(CheckpointRecord {
                    checkpoint_id: format!("checkpoint:wait-{index}"),
                    kind: centaeris_core::runtime::contracts::CheckpointKindV1::Wait,
                    session_id: session_id.clone(),
                    turn_id: format!("turn-wait-{index}"),
                    status: format!("paused_{reason}"),
                    done_reason: Some(reason.to_string()),
                    updated_at_ms: 90,
                    payload_json: "{}".to_string(),
                })
                .expect("save wait checkpoint");

            store
                .enqueue_turn_supplement(EnqueueTurnSupplementRequest {
                    agent_run_id: agent_run_id.clone(),
                    lifecycle_job_id: job_id.clone(),
                    session_id: session_id.clone(),
                    authorization_digest: digest.clone(),
                    supplement_id: format!("supplement-wait-{index}"),
                    message: "keep this pending".to_string(),
                    created_at_ms: 100,
                })
                .expect("enqueue during wait");

            assert_eq!(
                store
                    .get_runtime_job(job_id.as_str())
                    .expect("load waiting job")
                    .expect("waiting job exists")
                    .run_at_ms,
                10_000,
                "{reason} wait must not be resumed by supplement admission"
            );
        }

        let _ = std::fs::remove_file(db_path);
    }

    #[test]
    fn recovery_checkpoint_is_immutable_and_idempotent() {
        let db_path = temp_db_path("recovery_checkpoint_immutable");
        let store = SqliteRuntimeStore::new(&db_path).expect("create sqlite runtime store");
        let checkpoint = CheckpointRecord {
            checkpoint_id: "checkpoint:recovery-1".to_string(),
            kind: CheckpointKindV1::Recovery,
            session_id: "chat-recovery".to_string(),
            turn_id: "turn-recovery".to_string(),
            status: "committed".to_string(),
            done_reason: None,
            updated_at_ms: 10,
            payload_json: "{\"schema\":\"runtime.recovery_checkpoint.v1\"}".to_string(),
        };
        store
            .save_checkpoint(checkpoint.clone())
            .expect("save recovery checkpoint");
        store
            .save_checkpoint(checkpoint.clone())
            .expect("idempotent recovery checkpoint");
        let mut conflicting = checkpoint;
        conflicting.payload_json = "{\"schema\":\"banana\"}".to_string();
        assert!(store
            .save_checkpoint(conflicting)
            .expect_err("mutating a recovery checkpoint must fail")
            .to_string()
            .contains("recovery_checkpoint_idempotency_conflict"));
        let _ = std::fs::remove_file(db_path);
    }

    #[test]
    fn sqlite_job_cancel_honors_expected_status_atomically() {
        let db_path = temp_db_path("runtime_job_expected_cancel");
        let store = SqliteRuntimeStore::new(&db_path).expect("create sqlite runtime store");
        store
            .schedule_runtime_job(
                centaeris_core::session::reliability::ScheduleRuntimeJobRequest {
                    job: runtime_job("job-expected-cancel", RuntimeJobStatus::Queued, None),
                },
            )
            .expect("schedule runtime job");

        store
            .cancel_runtime_job(CancelRuntimeJobRequest {
                job_id: "job-expected-cancel".to_string(),
                reason: "user_cancelled".to_string(),
                cancelled_at_ms: 200,
                expected_status: Some(RuntimeJobStatus::Running),
            })
            .expect_err("stale expected status must reject cancellation");
        store
            .cancel_runtime_job(CancelRuntimeJobRequest {
                job_id: "job-expected-cancel".to_string(),
                reason: "user_cancelled".to_string(),
                cancelled_at_ms: 201,
                expected_status: Some(RuntimeJobStatus::Queued),
            })
            .expect("matching expected status must cancel");
        assert_eq!(
            store
                .get_runtime_job("job-expected-cancel")
                .expect("load runtime job")
                .expect("runtime job exists")
                .status,
            RuntimeJobStatus::Cancelled
        );

        let _ = std::fs::remove_file(db_path);
    }

    #[test]
    fn sqlite_terminal_runtime_job_notification_is_recoverable() {
        let db_path = temp_db_path("runtime_job_terminal_outbox");
        let store = SqliteRuntimeStore::new(&db_path).expect("create sqlite runtime store");
        store
            .schedule_runtime_job(
                centaeris_core::session::reliability::ScheduleRuntimeJobRequest {
                    job: runtime_job(
                        "job-terminal",
                        RuntimeJobStatus::Running,
                        Some("worker-terminal"),
                    ),
                },
            )
            .expect("schedule running runtime job");
        store
            .complete_runtime_job(CompleteRuntimeJobRequest {
                job_id: "job-terminal".to_string(),
                lease_owner: "worker-terminal".to_string(),
                output_refs: vec!["result.json".to_string()],
                completed_at_ms: 200,
            })
            .expect("complete runtime job");
        let pending = store
            .list_pending_runtime_job_outbox(10)
            .expect("load terminal notification");
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].event_type, "runtime_job.terminal");
        assert_eq!(pending[0].generation, 0);

        store
            .mark_runtime_job_outbox_published("job-terminal", "runtime_job.terminal", 0, 250)
            .expect("publish terminal notification");
        assert_eq!(
            store
                .requeue_runtime_job_notifications(250)
                .expect("requeue dropped terminal notification"),
            1
        );
        let requeued = store
            .list_pending_runtime_job_outbox(10)
            .expect("load requeued terminal notification");
        assert_eq!(requeued.len(), 1);
        assert_eq!(requeued[0].event_type, "runtime_job.terminal");
        assert_eq!(requeued[0].generation, 1);

        let _ = std::fs::remove_file(db_path);
    }

    #[test]
    fn sqlite_runtime_job_yield_requeues_same_job_without_queued_outbox() {
        for status in [RuntimeJobStatus::Leased, RuntimeJobStatus::Running] {
            let suffix = format!("runtime_job_yield_{status:?}");
            let db_path = temp_db_path(suffix.as_str());
            let store = SqliteRuntimeStore::new(&db_path).expect("create sqlite runtime store");
            let mut job = runtime_job("job-yield", status, Some("worker-yield-owner"));
            job.retry_count = 1;
            store
                .schedule_runtime_job(
                    centaeris_core::session::reliability::ScheduleRuntimeJobRequest { job },
                )
                .expect("schedule leased runtime job");
            let request = YieldRuntimeJobRequest {
                job_id: "job-yield".to_string(),
                lease_owner: "worker-yield-owner".to_string(),
                yielded_at_ms: 200,
                run_at_ms: 250,
                transition_reason: "waiting_for_durable_input".to_string(),
            };
            store
                .yield_runtime_job(request.clone())
                .expect("yield runtime job");
            let cross_lease_error = store
                .yield_runtime_job(YieldRuntimeJobRequest {
                    lease_owner: "worker-other-owner".to_string(),
                    ..request.clone()
                })
                .expect_err("same millisecond from another lease is not idempotent");
            assert!(cross_lease_error.contains("idempotency conflict"));
            store
                .yield_runtime_job(request)
                .expect("duplicate yield is idempotent");
            let old_owner_error = store
                .yield_runtime_job(YieldRuntimeJobRequest {
                    job_id: "job-yield".to_string(),
                    lease_owner: "worker-yield-owner".to_string(),
                    yielded_at_ms: 203,
                    run_at_ms: 253,
                    transition_reason: "waiting_for_durable_input".to_string(),
                })
                .expect_err("old owner must be fenced");
            assert!(old_owner_error.contains("lease mismatch or expired"));

            let yielded = store
                .get_runtime_job("job-yield")
                .expect("load yielded job")
                .expect("yielded job exists");
            assert_eq!(yielded.status, RuntimeJobStatus::Queued);
            assert_eq!(yielded.run_at_ms, 250);
            assert_eq!(yielded.retry_count, 1);
            assert!(yielded.lease_owner.is_none());
            assert!(yielded.lease_expires_at_ms.is_none());
            assert!(yielded.heartbeat_at_ms.is_none());
            let pending = store
                .list_pending_runtime_job_outbox(10)
                .expect("load terminal-only outbox");
            assert!(pending.is_empty());
            let events = store
                .list_events("chat-sqlite-tx", 10, 0)
                .expect("load yield event");
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].event_type, "runtime_job_yielded");
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&events[0].payload_json)
                    .expect("decode yield event")["transitionReason"],
                "waiting_for_durable_input"
            );

            let _ = std::fs::remove_file(db_path);
        }
    }

    #[test]
    fn sqlite_runtime_job_wake_advances_yielded_job_without_queued_outbox() {
        let db_path = temp_db_path("runtime_job_wake");
        let store = SqliteRuntimeStore::new(&db_path).expect("create sqlite runtime store");
        let mut job = runtime_job(
            "agent_run.lifecycle:agent-run-1",
            RuntimeJobStatus::Queued,
            None,
        );
        job.run_at_ms = 10_000;
        store
            .schedule_runtime_job(
                centaeris_core::session::reliability::ScheduleRuntimeJobRequest { job },
            )
            .expect("schedule yielded lifecycle job");
        let request = WakeRuntimeJobRequest {
            job_id: "agent_run.lifecycle:agent-run-1".to_string(),
            source_job_id: "provider_poll_job:child".to_string(),
            woken_at_ms: 200,
            transition_reason: "runtime_job_terminal".to_string(),
        };
        assert_eq!(
            store
                .wake_runtime_job(request.clone())
                .expect("wake lifecycle job"),
            WakeRuntimeJobDisposition::Woken
        );
        assert_eq!(
            store
                .get_runtime_job("agent_run.lifecycle:agent-run-1")
                .expect("load lifecycle job")
                .expect("lifecycle job exists")
                .run_at_ms,
            200
        );
        assert_eq!(
            store
                .wake_runtime_job(request)
                .expect("duplicate wake is idempotent"),
            WakeRuntimeJobDisposition::AlreadyRunnable
        );
        let pending = store
            .list_pending_runtime_job_outbox(10)
            .expect("load terminal-only outbox");
        assert!(pending.is_empty());
        let events = store
            .list_events("chat-sqlite-tx", 10, 0)
            .expect("load wake event");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "runtime_job_wake_requested");

        let _ = std::fs::remove_file(db_path);
    }

    #[test]
    fn sqlite_runtime_job_wake_during_active_step_is_consumed_by_yield() {
        let db_path = temp_db_path("runtime_job_active_wake");
        let store = SqliteRuntimeStore::new(&db_path).expect("create sqlite runtime store");
        store
            .schedule_runtime_job(
                centaeris_core::session::reliability::ScheduleRuntimeJobRequest {
                    job: runtime_job(
                        "agent_run.lifecycle:agent-run-active",
                        RuntimeJobStatus::Running,
                        Some("worker-active-owner"),
                    ),
                },
            )
            .expect("schedule active lifecycle job");
        assert_eq!(
            store
                .wake_runtime_job(WakeRuntimeJobRequest {
                    job_id: "agent_run.lifecycle:agent-run-active".to_string(),
                    source_job_id: "provider_poll_job:done".to_string(),
                    woken_at_ms: 200,
                    transition_reason: "runtime_job_terminal".to_string(),
                })
                .expect("record active wake"),
            WakeRuntimeJobDisposition::Active
        );
        let yield_request = YieldRuntimeJobRequest {
            job_id: "agent_run.lifecycle:agent-run-active".to_string(),
            lease_owner: "worker-active-owner".to_string(),
            yielded_at_ms: 250,
            run_at_ms: 10_000,
            transition_reason: "runtime_job_wait".to_string(),
        };
        store
            .yield_runtime_job(yield_request.clone())
            .expect("yield observes active wake");
        store
            .yield_runtime_job(yield_request)
            .expect("duplicate yielded response is idempotent");
        let job = store
            .get_runtime_job("agent_run.lifecycle:agent-run-active")
            .expect("load lifecycle job")
            .expect("lifecycle job exists");
        assert_eq!(job.status, RuntimeJobStatus::Queued);
        assert_eq!(job.run_at_ms, 250);
        let events = store
            .list_events("chat-sqlite-tx", 10, 0)
            .expect("load wake lifecycle events");
        assert_eq!(
            events
                .iter()
                .map(|event| event.event_type.as_str())
                .collect::<Vec<_>>(),
            [
                "runtime_job_wake_requested",
                "runtime_job_wake_consumed",
                "runtime_job_yielded",
            ]
        );

        let _ = std::fs::remove_file(db_path);
    }

    #[test]
    fn sqlite_runtime_job_yield_rejects_old_or_expired_owner() {
        let db_path = temp_db_path("runtime_job_yield_fencing");
        let store = SqliteRuntimeStore::new(&db_path).expect("create sqlite runtime store");
        store
            .schedule_runtime_job(
                centaeris_core::session::reliability::ScheduleRuntimeJobRequest {
                    job: runtime_job(
                        "job-yield-fenced",
                        RuntimeJobStatus::Running,
                        Some("worker-current-owner"),
                    ),
                },
            )
            .expect("schedule running runtime job");

        let old_owner_error = store
            .yield_runtime_job(YieldRuntimeJobRequest {
                job_id: "job-yield-fenced".to_string(),
                lease_owner: "worker-old-owner".to_string(),
                yielded_at_ms: 200,
                run_at_ms: 250,
                transition_reason: "waiting_for_durable_input".to_string(),
            })
            .expect_err("old owner must be fenced");
        assert!(old_owner_error.contains("lease mismatch or expired"));

        let expired_error = store
            .yield_runtime_job(YieldRuntimeJobRequest {
                job_id: "job-yield-fenced".to_string(),
                lease_owner: "worker-current-owner".to_string(),
                yielded_at_ms: 1_000,
                run_at_ms: 1_050,
                transition_reason: "waiting_for_durable_input".to_string(),
            })
            .expect_err("expired owner must be fenced");
        assert!(expired_error.contains("lease mismatch or expired"));
        assert_eq!(
            store
                .get_runtime_job("job-yield-fenced")
                .expect("load fenced job")
                .expect("fenced job exists")
                .status,
            RuntimeJobStatus::Running
        );

        let _ = std::fs::remove_file(db_path);
    }

    #[test]
    fn sqlite_external_context_content_roundtrips_zstd_and_corruption_loud_fails() {
        let db_path = temp_db_path("external_context_zstd");
        let store = SqliteRuntimeStore::new(&db_path).expect("create sqlite runtime store");
        let mut object = external_context_object("object-zstd");
        object.content = "compressible tool output\n".repeat(1_000);
        store
            .upsert_external_context_object(object.clone())
            .expect("store compressed external context object");

        let conn = Connection::open(&db_path).expect("inspect compressed external context object");
        let (storage_type, content_codec, compressed_bytes, uncompressed_bytes): (
            String,
            String,
            i64,
            i64,
        ) = conn
            .query_row(
                "
                SELECT typeof(content), content_codec, length(content), content_uncompressed_bytes
                FROM external_context_objects
                WHERE object_id='object-zstd'
                ",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .expect("query compressed external context object");
        assert_eq!(storage_type, "blob");
        assert_eq!(content_codec, "zstd_v1");
        assert!(compressed_bytes < uncompressed_bytes);
        assert_eq!(uncompressed_bytes, object.content.len() as i64);
        assert_eq!(
            store
                .load_external_context_object("object-zstd")
                .expect("load compressed external context object")
                .expect("compressed external context object exists")
                .content,
            object.content
        );

        store
            .upsert_external_context_object(external_context_object("object-identity"))
            .expect("store small external context object");
        let identity_codec: String = conn
            .query_row(
                "SELECT content_codec FROM external_context_objects WHERE object_id='object-identity'",
                [],
                |row| row.get(0),
            )
            .expect("query small external context codec");
        assert_eq!(identity_codec, "identity_v1");

        conn.execute_batch("PRAGMA ignore_check_constraints = ON")
            .expect("allow corrupt codec fixture");
        conn.execute(
            "UPDATE external_context_objects SET content_codec='banana' WHERE object_id='object-zstd'",
            [],
        )
        .expect("corrupt external context content codec");
        let error = store
            .load_external_context_object("object-zstd")
            .expect_err("unknown external context content codec must fail");
        assert!(error.contains("unsupported external context content codec: banana"));
        conn.execute(
            "UPDATE external_context_objects SET content_codec='zstd_v1' WHERE object_id='object-zstd'",
            [],
        )
        .expect("restore external context content codec");
        conn.execute_batch("PRAGMA ignore_check_constraints = OFF")
            .expect("restore codec constraint enforcement");

        conn.execute(
            "UPDATE external_context_objects SET content=?1 WHERE object_id='object-zstd'",
            params![vec![0_u8, 1, 2, 3]],
        )
        .expect("corrupt compressed external context content");
        let error = store
            .load_external_context_object("object-zstd")
            .expect_err("corrupt compressed external context content must fail");
        assert!(error.contains("decompress external context content failed"));
        drop(conn);
        drop(store);
        let _ = std::fs::remove_file(db_path);
    }

    #[test]
    fn sqlite_transaction_upserts_external_context_and_schedules_job() {
        let db_path = temp_db_path("external_context_schedule_tx");
        let store = SqliteRuntimeStore::new(&db_path).expect("create sqlite runtime store");

        let result = store
            .upsert_external_context_and_schedule_job(UpsertExternalContextAndScheduleJobRequest {
                object: external_context_object("object-schedule-1"),
                job: runtime_job("job-schedule-1", RuntimeJobStatus::Queued, None),
            })
            .expect("upsert object and schedule job");

        assert_eq!(result.disposition, ScheduleRuntimeJobDisposition::Inserted);
        assert!(store
            .load_external_context_object("object-schedule-1")
            .expect("load external object")
            .is_some());
        assert_eq!(
            store
                .get_runtime_job("job-schedule-1")
                .expect("load runtime job")
                .expect("runtime job exists")
                .status,
            RuntimeJobStatus::Queued
        );

        let _ = std::fs::remove_file(db_path);
    }

    #[test]
    fn sqlite_transaction_rolls_back_external_context_when_job_completion_fails() {
        let db_path = temp_db_path("external_context_complete_rollback");
        let store = SqliteRuntimeStore::new(&db_path).expect("create sqlite runtime store");

        let error = store
            .upsert_external_context_link_and_complete_job(
                UpsertExternalContextLinkAndCompleteJobRequest {
                    object: Some(external_context_object("object-rollback-1")),
                    link: None,
                    complete_job: CompleteRuntimeJobRequest {
                        job_id: "missing-job".to_string(),
                        lease_owner: "worker-1".to_string(),
                        output_refs: vec![],
                        completed_at_ms: 300,
                    },
                },
            )
            .expect_err("completion mismatch must rollback object upsert");

        assert!(error.contains("complete_runtime_job lease mismatch"));
        assert!(store
            .load_external_context_object("object-rollback-1")
            .expect("load rolled back object")
            .is_none());

        let _ = std::fs::remove_file(db_path);
    }

    #[test]
    fn sqlite_transaction_completes_external_context_link_and_job_atomically() {
        let db_path = temp_db_path("external_context_complete_tx");
        let store = SqliteRuntimeStore::new(&db_path).expect("create sqlite runtime store");
        store
            .schedule_runtime_job(
                centaeris_core::session::reliability::ScheduleRuntimeJobRequest {
                    job: runtime_job(
                        "job-complete-1",
                        RuntimeJobStatus::Running,
                        Some("worker-1"),
                    ),
                },
            )
            .expect("insert running job");

        store
            .upsert_external_context_link_and_complete_job(
                UpsertExternalContextLinkAndCompleteJobRequest {
                    object: Some(external_context_object("object-complete-1")),
                    link: Some(ExternalContextObjectLink {
                        session_id: "chat-sqlite-tx".to_string(),
                        turn_id: Some("turn-sqlite-tx".to_string()),
                        tool_call_id: Some("tool-sqlite-tx".to_string()),
                        object_id: "object-complete-1".to_string(),
                        source_provider_id: "test-provider".to_string(),
                        source_tool_name: "test-tool".to_string(),
                        linked_at_ms: 301,
                    }),
                    complete_job: CompleteRuntimeJobRequest {
                        job_id: "job-complete-1".to_string(),
                        lease_owner: "worker-1".to_string(),
                        output_refs: vec!["ref:object-complete-1".to_string()],
                        completed_at_ms: 302,
                    },
                },
            )
            .expect("complete job with external context link");

        assert_eq!(
            store
                .get_runtime_job("job-complete-1")
                .expect("load completed job")
                .expect("completed job exists")
                .status,
            RuntimeJobStatus::Succeeded
        );
        assert_eq!(
            store
                .list_external_context_objects(
                    centaeris_core::session::external_context::ListExternalContextObjectsRequest {
                        session_id: Some("chat-sqlite-tx".to_string()),
                        limit: 10,
                        offset: 0,
                    }
                )
                .expect("list linked objects")
                .len(),
            1
        );

        let _ = std::fs::remove_file(db_path);
    }

    #[test]
    fn sqlite_transaction_dead_letters_and_fails_job_atomically() {
        let db_path = temp_db_path("dead_letter_fail_tx");
        let store = SqliteRuntimeStore::new(&db_path).expect("create sqlite runtime store");
        store
            .schedule_runtime_job(
                centaeris_core::session::reliability::ScheduleRuntimeJobRequest {
                    job: runtime_job(
                        "job-dead-letter-1",
                        RuntimeJobStatus::Running,
                        Some("worker-1"),
                    ),
                },
            )
            .expect("insert running job");

        let result = store
            .create_dead_letter_and_fail_job(CreateDeadLetterAndFailJobRequest {
                dead_letter: CreateDeadLetterRequest {
                    dead_letter: dead_letter_record("dead-letter-1", "job-dead-letter-1"),
                },
                fail_job: FailRuntimeJobRequest {
                    job_id: "job-dead-letter-1".to_string(),
                    lease_owner: "worker-1".to_string(),
                    failed_at_ms: 400,
                    last_error: "provider failed".to_string(),
                    next_run_at_ms: None,
                    disposition: RuntimeJobFailureDisposition::DeadLettered,
                },
            })
            .expect("dead-letter and fail job");

        assert_eq!(result.disposition, CreateDeadLetterDisposition::Inserted);
        assert_eq!(
            store
                .get_runtime_job("job-dead-letter-1")
                .expect("load failed job")
                .expect("failed job exists")
                .status,
            RuntimeJobStatus::DeadLettered
        );
        assert_eq!(result.dead_letter.dead_letter_id, "dead-letter-1");

        let _ = std::fs::remove_file(db_path);
    }

    #[test]
    fn sqlite_transaction_rolls_back_dead_letter_when_job_fail_fails() {
        let db_path = temp_db_path("dead_letter_fail_rollback");
        let store = SqliteRuntimeStore::new(&db_path).expect("create sqlite runtime store");

        let error = store
            .create_dead_letter_and_fail_job(CreateDeadLetterAndFailJobRequest {
                dead_letter: CreateDeadLetterRequest {
                    dead_letter: dead_letter_record("dead-letter-rollback", "missing-job"),
                },
                fail_job: FailRuntimeJobRequest {
                    job_id: "missing-job".to_string(),
                    lease_owner: "worker-1".to_string(),
                    failed_at_ms: 401,
                    last_error: "provider failed".to_string(),
                    next_run_at_ms: None,
                    disposition: RuntimeJobFailureDisposition::DeadLettered,
                },
            })
            .expect_err("job fail mismatch must rollback dead letter");

        assert!(error.contains("fail_runtime_job lease mismatch"));
        assert!(store
            .get_dead_letter("dead-letter-rollback")
            .expect("load rolled back dead letter")
            .is_none());

        let _ = std::fs::remove_file(db_path);
    }

    #[test]
    fn sqlite_transaction_rolls_back_job_when_dead_letter_primary_key_conflicts_other_job() {
        let db_path = temp_db_path("dead_letter_primary_key_conflict");
        let store = SqliteRuntimeStore::new(&db_path).expect("create sqlite runtime store");
        store
            .schedule_runtime_job(
                centaeris_core::session::reliability::ScheduleRuntimeJobRequest {
                    job: runtime_job(
                        "job-dead-letter-conflict",
                        RuntimeJobStatus::Running,
                        Some("worker-1"),
                    ),
                },
            )
            .expect("insert running job");
        store
            .create_dead_letter(CreateDeadLetterRequest {
                dead_letter: dead_letter_record("dead-letter-conflict", "other-job"),
            })
            .expect("insert conflicting dead letter");

        let error = store
            .create_dead_letter_and_fail_job(CreateDeadLetterAndFailJobRequest {
                dead_letter: CreateDeadLetterRequest {
                    dead_letter: dead_letter_record(
                        "dead-letter-conflict",
                        "job-dead-letter-conflict",
                    ),
                },
                fail_job: FailRuntimeJobRequest {
                    job_id: "job-dead-letter-conflict".to_string(),
                    lease_owner: "worker-1".to_string(),
                    failed_at_ms: 402,
                    last_error: "provider failed".to_string(),
                    next_run_at_ms: None,
                    disposition: RuntimeJobFailureDisposition::DeadLettered,
                },
            })
            .expect_err("dead letter primary key conflict must rollback job fail");

        assert!(error.contains("existing row missing original_job_id=job-dead-letter-conflict"));
        assert_eq!(
            store
                .get_runtime_job("job-dead-letter-conflict")
                .expect("load job after rollback")
                .expect("job exists")
                .status,
            RuntimeJobStatus::Running
        );

        let _ = std::fs::remove_file(db_path);
    }

    #[test]
    fn sqlite_wait_checkpoint_is_idempotent_and_consumed_atomically() {
        let db_path = temp_db_path("wait_checkpoint_transaction");
        let store = SqliteRuntimeStore::new(&db_path).expect("create sqlite runtime store");
        let checkpoint = CheckpointRecord {
            checkpoint_id: "checkpoint:wait".to_string(),
            kind: centaeris_core::runtime::contracts::CheckpointKindV1::Wait,
            session_id: "chat-wait".to_string(),
            turn_id: "turn-wait".to_string(),
            status: "paused_question".to_string(),
            done_reason: Some("question".to_string()),
            updated_at_ms: 500,
            payload_json: "{\"schema\":\"runtime.await_question.v1\"}".to_string(),
        };
        let waiting = RuntimeEvent {
            event_id: "runtime-wait-question-waiting".to_string(),
            session_id: "chat-wait".to_string(),
            task_id: Some("turn-wait".to_string()),
            event_type: "runtime_wait_changed.v1".to_string(),
            at_ms: 500,
            visibility: EventVisibility::Internal,
            payload_json: "{\"status\":\"waiting\"}".to_string(),
        };
        let save = SaveWaitCheckpointRequest {
            checkpoint: checkpoint.clone(),
            event: waiting,
        };
        store
            .save_wait_checkpoint(save.clone())
            .expect("save wait checkpoint");
        store
            .save_wait_checkpoint(save)
            .expect("replay exact wait checkpoint");

        let resumed = RuntimeEvent {
            event_id: "runtime-wait-question-resumed".to_string(),
            session_id: "chat-wait".to_string(),
            task_id: Some("turn-wait".to_string()),
            event_type: "runtime_wait_changed.v1".to_string(),
            at_ms: 501,
            visibility: EventVisibility::Internal,
            payload_json: "{\"status\":\"resumed\"}".to_string(),
        };
        let consume = ConsumeWaitCheckpointRequest {
            checkpoint: checkpoint.clone(),
            events: vec![resumed],
        };
        store
            .consume_wait_checkpoint(consume.clone())
            .expect("consume wait checkpoint");
        store
            .consume_wait_checkpoint(consume)
            .expect("replay exact wait consumption");

        assert!(store
            .load_checkpoint_by_turn("chat-wait", "turn-wait")
            .expect("load consumed checkpoint")
            .is_none());
        assert_eq!(
            store
                .list_events("chat-wait", 10, 0)
                .expect("list wait events")
                .len(),
            2
        );
        let _ = std::fs::remove_file(db_path);
    }

    #[test]
    fn sqlite_agent_runtime_snapshot_round_trips() {
        let db_path = temp_db_path("session_snapshot_ownership");
        let store = SqliteRuntimeStore::new(&db_path).expect("create sqlite runtime store");

        store
            .save_agent_runtime_snapshot("chat-session-1", "{\"messages\":[]}", 700)
            .expect("save session runtime snapshot");

        assert_eq!(
            store
                .load_agent_runtime_snapshot("chat-session-1")
                .expect("load session runtime snapshot")
                .as_deref(),
            Some("{\"messages\":[]}")
        );
        let _ = std::fs::remove_file(db_path);
    }

    #[test]
    fn sqlite_schema_rejects_non_empty_db_without_schema_migrations() {
        let db_path = temp_db_path("missing_schema_migrations");
        {
            let conn = Connection::open(&db_path).expect("open sqlite db");
            conn.execute("CREATE TABLE stray_runtime_state(id TEXT PRIMARY KEY)", [])
                .expect("create stray table");
        }

        let error = SqliteRuntimeStore::new(&db_path)
            .expect_err("non-empty db without schema_migrations must fail");

        assert!(error.contains("schema_migrations table is missing"));
        let _ = std::fs::remove_file(db_path);
    }

    #[test]
    fn sqlite_schema_rejects_unknown_schema_version() {
        for journal_mode in ["DELETE", "WAL"] {
            let db_path = temp_db_path("unknown_schema_version");
            {
                let conn = Connection::open(&db_path).expect("open sqlite db");
                conn.pragma_update(None, "journal_mode", journal_mode)
                    .expect("set fixture journal mode");
                conn.execute_batch(
                    "CREATE TABLE schema_migrations (
                        version INTEGER PRIMARY KEY,
                        applied_at_ms INTEGER NOT NULL
                    );
                    INSERT INTO schema_migrations(version, applied_at_ms) VALUES(14, 1);",
                )
                .expect("create unsupported schema fixture");
            }
            let before = std::fs::read(&db_path).expect("original database bytes");
            let error =
                SqliteRuntimeStore::new(&db_path).expect_err("unknown schema version must fail");

            assert!(error.contains("refuses schema downgrade"));
            assert_eq!(
                std::fs::read(&db_path).expect("database bytes after rejection"),
                before,
                "rejected {journal_mode} database must remain unchanged",
            );
            std::fs::remove_file(db_path).expect("remove fixture database");
        }
    }

    #[test]
    fn sqlite_schema_v1_fixture_reopens_with_ordered_history() {
        let db_path = temp_db_path("v1_ordered_history");
        drop(SqliteRuntimeStore::new(&db_path).expect("create v1 store"));
        let conn = Connection::open(&db_path).expect("open v1 fixture");
        let versions = conn
            .prepare("SELECT version FROM schema_migrations ORDER BY version ASC")
            .expect("prepare history")
            .query_map([], |row| row.get::<_, i64>(0))
            .expect("query history")
            .collect::<Result<Vec<_>, _>>()
            .expect("decode history");
        assert_eq!(versions, vec![STORE_SCHEMA_VERSION]);
        drop(conn);
        drop(SqliteRuntimeStore::new(&db_path).expect("reopen v1 fixture"));
        let _ = std::fs::remove_file(db_path);
    }

    #[test]
    fn sqlite_schema_rejects_unknown_table_in_current_schema() {
        let db_path = temp_db_path("unknown_table");
        let store = SqliteRuntimeStore::new(&db_path).expect("create sqlite runtime store");
        drop(store);
        {
            let conn = Connection::open(&db_path).expect("open sqlite db");
            conn.execute("CREATE TABLE rogue_state(id TEXT PRIMARY KEY)", [])
                .expect("create rogue table");
        }

        let error = SqliteRuntimeStore::new(&db_path).expect_err("unknown table must fail");

        assert!(error.contains("runtime sqlite unknown table: rogue_state"));
        let _ = std::fs::remove_file(db_path);
    }

    #[test]
    fn sqlite_schema_rejects_missing_required_index_in_current_schema() {
        let db_path = temp_db_path("missing_required_index");
        let store = SqliteRuntimeStore::new(&db_path).expect("create sqlite runtime store");
        drop(store);
        {
            let conn = Connection::open(&db_path).expect("open sqlite db");
            conn.execute("DROP INDEX idx_runtime_jobs_status_run_at", [])
                .expect("drop required index");
        }

        let error = SqliteRuntimeStore::new(&db_path).expect_err("missing index must fail");

        assert!(error.contains("required index missing: idx_runtime_jobs_status_run_at"));
        let _ = std::fs::remove_file(db_path);
    }

    #[test]
    fn sqlite_schema_rejects_wrong_required_index_definition() {
        let db_path = temp_db_path("wrong_required_index_definition");
        let store = SqliteRuntimeStore::new(&db_path).expect("create sqlite runtime store");
        drop(store);
        {
            let conn = Connection::open(&db_path).expect("open sqlite db");
            conn.execute("DROP INDEX idx_runtime_jobs_status_run_at", [])
                .expect("drop required index");
            conn.execute(
                "
                CREATE INDEX idx_runtime_jobs_status_run_at
                    ON runtime_jobs(status, job_id ASC)
                ",
                [],
            )
            .expect("create wrong index definition");
        }

        let error = SqliteRuntimeStore::new(&db_path).expect_err("wrong index must fail");

        assert!(error
            .contains("runtime sqlite index definition mismatch: idx_runtime_jobs_status_run_at"));
        let _ = std::fs::remove_file(db_path);
    }

    #[test]
    fn sqlite_schema_rejects_wrong_required_table_definition() {
        let db_path = temp_db_path("wrong_required_table_definition");
        let store = SqliteRuntimeStore::new(&db_path).expect("create sqlite runtime store");
        drop(store);
        {
            let conn = Connection::open(&db_path).expect("open sqlite db");
            conn.execute("DROP TABLE checkpoints", [])
                .expect("drop required table");
            conn.execute(
                "
                CREATE TABLE checkpoints (
                    session_id TEXT NOT NULL,
                    turn_id TEXT NOT NULL,
                    status TEXT,
                    done_reason TEXT,
                    updated_at_ms INTEGER NOT NULL,
                    payload_json TEXT NOT NULL,
                    PRIMARY KEY (session_id, turn_id)
                )
                ",
                [],
            )
            .expect("create wrong table definition");
            conn.execute(
                "
                CREATE INDEX idx_checkpoints_session_updated
                    ON checkpoints(session_id, updated_at_ms DESC, turn_id DESC)
                ",
                [],
            )
            .expect("recreate checkpoints index");
        }

        let error = SqliteRuntimeStore::new(&db_path).expect_err("wrong table must fail");

        assert!(error.contains("runtime sqlite table definition mismatch: checkpoints"));
        let _ = std::fs::remove_file(db_path);
    }

    #[test]
    fn delete_session_data_removes_owned_runtime_rows_and_only_orphan_objects() {
        let db_path = temp_db_path("delete_session_data");
        let store = SqliteRuntimeStore::new(&db_path).expect("create sqlite runtime store");
        store
            .save_checkpoint(CheckpointRecord {
                checkpoint_id: "checkpoint:delete".to_string(),
                kind: centaeris_core::runtime::contracts::CheckpointKindV1::Wait,
                session_id: "chat-delete".to_string(),
                turn_id: "turn-delete".to_string(),
                status: "completed".to_string(),
                done_reason: Some("final".to_string()),
                updated_at_ms: 1,
                payload_json: "{}".to_string(),
            })
            .expect("checkpoint");
        store
            .save_agent_runtime_snapshot("chat-delete", "{}", 1)
            .expect("snapshot");
        store
            .append_event(RuntimeEvent {
                event_id: "evt-delete".to_string(),
                session_id: "chat-delete".to_string(),
                task_id: Some("task-delete".to_string()),
                event_type: "turn.completed".to_string(),
                at_ms: 1,
                visibility: EventVisibility::Internal,
                payload_json: "{}".to_string(),
            })
            .expect("event");
        store
            .with_conn(|conn| {
                for object_id in ["object-orphan", "object-shared"] {
                    conn.execute(
                        "INSERT INTO external_context_objects(object_id,schema_version,object_kind,source_provider_id,source_tool_name,title,content,content_codec,content_uncompressed_bytes,content_sha256,metadata_json,updated_at_ms,inserted_at_ms) VALUES(?1,'external_context.v1','text','provider','tool','title','content','identity_v1',7,'0000000000000000000000000000000000000000000000000000000000000000','{}',1,1)",
                        params![object_id],
                    )
                    .map_err(|error| error.to_string())?;
                    conn.execute(
                        "INSERT INTO external_context_links(session_id,object_id,turn_id,tool_call_id,source_provider_id,source_tool_name,linked_at_ms) VALUES('chat-delete',?1,'turn','call','provider','tool',1)",
                        params![object_id],
                    )
                    .map_err(|error| error.to_string())?;
                }
                conn.execute(
                    "INSERT INTO external_context_links(session_id,object_id,turn_id,tool_call_id,source_provider_id,source_tool_name,linked_at_ms) VALUES('chat-keep','object-shared','turn','call','provider','tool',1)",
                    [],
                )
                .map_err(|error| error.to_string())?;
                Ok::<(), String>(())
            })
            .expect("external context fixtures");

        store
            .delete_session_data("chat-delete")
            .expect("delete session data");

        assert!(store
            .load_checkpoint_by_turn("chat-delete", "turn-delete")
            .expect("checkpoint lookup")
            .is_none());
        assert!(store
            .load_agent_runtime_snapshot("chat-delete")
            .expect("snapshot lookup")
            .is_none());
        assert!(store
            .list_events("chat-delete", 10, 0)
            .expect("event lookup")
            .is_empty());
        store
            .with_conn(|conn| {
                let orphan_count: i64 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM external_context_objects WHERE object_id='object-orphan'",
                        [],
                        |row| row.get(0),
                    )
                    .map_err(|error| error.to_string())?;
                let shared_count: i64 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM external_context_objects WHERE object_id='object-shared'",
                        [],
                        |row| row.get(0),
                    )
                    .map_err(|error| error.to_string())?;
                assert_eq!(orphan_count, 0);
                assert_eq!(shared_count, 1);
                Ok(())
            })
            .expect("object cleanup assertions");
        let _ = std::fs::remove_file(db_path);
    }
}
