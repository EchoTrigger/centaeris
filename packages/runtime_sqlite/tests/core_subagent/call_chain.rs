use super::*;
use centaeris_core::session::reliability::{
    ClaimDueRuntimeJobsRequest, RuntimeJobRecord, ScheduleRuntimeJobRequest,
};
use centaeris_core::session::store::{
    job::{JobStoreError, JobStoreOperation, JobWriteOutcome},
    AsyncJobStorePort, RuntimeStoreTransactionPort, UpsertExternalContextAndScheduleJobRequest,
};

/// Faults affect confirmation, never the underlying SQL. Independent reads
/// establish the facts rather than assuming a reply error cancelled a write.
struct LostConfirmation {
    actor: RuntimeStoreActor,
    missing_started_job: bool,
    started: std::sync::atomic::AtomicBool,
    read_unavailable: bool,
}

#[async_trait::async_trait]
impl AsyncJobStorePort for LostConfirmation {
    async fn settle_job_operations(&self) -> Result<(), JobStoreError> {
        AsyncJobStorePort::settle_job_operations(&self.actor).await
    }
    async fn get_runtime_job(&self, id: &str) -> Result<Option<RuntimeJobRecord>, JobStoreError> {
        if self.missing_started_job && self.started.load(Ordering::SeqCst) {
            Ok(None)
        } else {
            AsyncJobStorePort::get_runtime_job(&self.actor, id).await
        }
    }
    async fn start_runtime_job(&self, req: StartRuntimeJobRequest) -> Result<(), JobStoreError> {
        AsyncJobStorePort::start_runtime_job(&self.actor, req).await?;
        self.started.store(true, Ordering::SeqCst);
        Ok(())
    }
    async fn upsert_external_context_link_and_complete_job(
        &self,
        req: centaeris_core::session::store::UpsertExternalContextLinkAndCompleteJobRequest,
    ) -> Result<(), JobStoreError> {
        AsyncJobStorePort::upsert_external_context_link_and_complete_job(&self.actor, req).await?;
        Err(JobStoreError::unconfirmed(
            JobStoreOperation::Complete,
            "reply lost".into(),
        ))
    }
    async fn list_runtime_jobs(
        &self,
        req: ListRuntimeJobsRequest,
    ) -> Result<Vec<RuntimeJobRecord>, JobStoreError> {
        AsyncJobStorePort::list_runtime_jobs(&self.actor, req).await
    }
    async fn claim_due_runtime_jobs(
        &self,
        req: ClaimDueRuntimeJobsRequest,
    ) -> Result<Vec<RuntimeJobRecord>, JobStoreError> {
        AsyncJobStorePort::claim_due_runtime_jobs(&self.actor, req).await
    }
    async fn renew_runtime_job_lease(
        &self,
        req: centaeris_core::session::reliability::RenewRuntimeJobLeaseRequest,
    ) -> Result<(), JobStoreError> {
        AsyncJobStorePort::renew_runtime_job_lease(&self.actor, req).await
    }
    async fn complete_runtime_job(
        &self,
        req: centaeris_core::session::reliability::CompleteRuntimeJobRequest,
    ) -> Result<(), JobStoreError> {
        AsyncJobStorePort::complete_runtime_job(&self.actor, req).await
    }
    async fn fail_runtime_job(
        &self,
        req: centaeris_core::session::reliability::FailRuntimeJobRequest,
    ) -> Result<(), JobStoreError> {
        AsyncJobStorePort::fail_runtime_job(&self.actor, req).await
    }
    async fn cancel_runtime_job(&self, req: CancelRuntimeJobRequest) -> Result<(), JobStoreError> {
        AsyncJobStorePort::cancel_runtime_job(&self.actor, req).await
    }
    async fn reclaim_expired_runtime_job_leases(&self, req: i64) -> Result<usize, JobStoreError> {
        AsyncJobStorePort::reclaim_expired_runtime_job_leases(&self.actor, req).await
    }
    async fn load_external_context_object(
        &self,
        req: &str,
    ) -> Result<
        Option<centaeris_core::session::external_context::ExternalContextObject>,
        JobStoreError,
    > {
        if self.read_unavailable {
            return Err(JobStoreError::unconfirmed(
                JobStoreOperation::Read,
                "read unavailable".into(),
            ));
        }
        AsyncJobStorePort::load_external_context_object(&self.actor, req).await
    }
    async fn upsert_external_context_and_schedule_job(
        &self,
        req: UpsertExternalContextAndScheduleJobRequest,
    ) -> Result<centaeris_core::session::reliability::ScheduleRuntimeJobResult, JobStoreError> {
        AsyncJobStorePort::upsert_external_context_and_schedule_job(&self.actor, req).await
    }
}

#[tokio::test]
async fn missing_authority_never_reuses_the_claimed_job() {
    confirmation_fault(true).await;
}

#[tokio::test]
async fn committed_completion_without_reply_retains_unknown_result() {
    confirmation_fault(false).await;
}

#[tokio::test]
async fn resource_claim_read_failure_stays_a_storage_failure() {
    let directory = tempfile::tempdir().unwrap();
    let store = SqliteRuntimeStore::new(directory.path().join("runtime.db")).unwrap();
    let job = build_subagent_run_job(SubagentRunJobRequest {
        session_id: "parent".into(),
        parent_turn_id: "turn".into(),
        tool_call_id: "call".into(),
        subagent_id: "child".into(),
        work_packet_ref: "packet".into(),
        checkpoint_id: None,
        run_at_ms: 10,
        created_at_ms: 9,
        max_retries: 0,
    });
    let fault = LostConfirmation {
        actor: RuntimeStoreActor::start(store).unwrap(),
        missing_started_job: false,
        started: std::sync::atomic::AtomicBool::new(false),
        read_unavailable: true,
    };
    let error =
        centaeris_core::runtime::subagent::subagent_resource_claim_for_job_async(&fault, &job)
            .await
            .unwrap_err();
    assert!(matches!(
        error,
        JobStoreError::Storage {
            operation: JobStoreOperation::Read,
            outcome: JobWriteOutcome::ReadOnly,
            ..
        }
    ));
}

async fn confirmation_fault(missing_started_job: bool) {
    let directory = tempfile::tempdir().unwrap();
    let store = SqliteRuntimeStore::new(directory.path().join("runtime.db")).unwrap();
    store_valid_subagent_work_packet(
        &store,
        "packet",
        "resource",
        "Borrow",
        vec!["read".into()],
        vec![],
    );
    let job = build_subagent_run_job(SubagentRunJobRequest {
        session_id: "session-chain".into(),
        parent_turn_id: "turn-chain".into(),
        tool_call_id: "call-chain".into(),
        subagent_id: "subagent-chain".into(),
        work_packet_ref: "packet".into(),
        checkpoint_id: None,
        run_at_ms: 1000,
        created_at_ms: 900,
        max_retries: 1,
    });
    store
        .schedule_runtime_job(ScheduleRuntimeJobRequest { job: job.clone() })
        .unwrap();
    let trace = Arc::new(Mutex::new(vec![]));
    let fault = LostConfirmation {
        actor: RuntimeStoreActor::start(store.clone()).unwrap(),
        missing_started_job,
        started: std::sync::atomic::AtomicBool::new(false),
        read_unavailable: false,
    };
    let error = run_due_subagent_jobs_with_worker_pool_async(
        &fault,
        &OrderedRunner {
            trace: trace.clone(),
            outcome: SubagentWorkerRunOutcome::Succeeded {
                summary: "complete chain result".into(),
                output_refs: vec![],
            },
        },
        &DurableObserver {
            store: store.clone(),
            db_path: directory.path().join("runtime.db"),
            trace: trace.clone(),
        },
        RunDueSubagentJobsRequest {
            now_ms: 1001,
            worker_id: "worker-chain".into(),
            session_id: Some("session-chain".into()),
            limit: 1,
            lease_ms: 5000,
            started_at_ms: 1002,
            finished_at_ms: 1010,
        },
        SubagentWorkerPoolPolicy { max_parallelism: 1 },
    )
    .await
    .unwrap_err();
    let actual = store.get_runtime_job(&job.job_id).unwrap().unwrap();
    if missing_started_job {
        assert!(
            matches!(
                error,
                JobStoreError::Storage {
                    operation: JobStoreOperation::Start,
                    outcome: JobWriteOutcome::Committed,
                    ..
                }
            ),
            "acknowledged start must remain committed: {error:?}"
        );
        assert_eq!(actual.status, RuntimeJobStatus::Running);
        assert!(trace.lock().unwrap().is_empty());
    } else {
        assert!(matches!(
            error,
            JobStoreError::Storage {
                operation: JobStoreOperation::Complete,
                outcome: JobWriteOutcome::Unknown,
                ..
            }
        ));
        assert_eq!(actual.status, RuntimeJobStatus::Succeeded);
        assert_eq!(*trace.lock().unwrap(), vec!["start", "run"]);
        assert!(store
            .load_external_context_object(&actual.output_refs[0])
            .unwrap()
            .is_some());
        let conn = rusqlite::Connection::open(directory.path().join("runtime.db")).unwrap();
        assert_eq!(
            conn.query_row(
                "SELECT count(*) FROM runtime_job_outbox WHERE job_id=?1",
                [&job.job_id],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
    }
}

struct DurableObserver {
    store: SqliteRuntimeStore,
    db_path: PathBuf,
    trace: Arc<Mutex<Vec<&'static str>>>,
}

impl AsyncSubagentLifecycleObserver for DurableObserver {
    fn on_subagent_start<'a>(
        &'a self,
        event: SubagentLifecycleHookEvent,
    ) -> SubagentLifecycleObserverFuture<'a> {
        Box::pin(async move {
            let job = self.store.get_runtime_job(&event.job_id)?.unwrap();
            assert_eq!(job.status, RuntimeJobStatus::Running);
            assert!(self
                .store
                .load_external_context_object(job.payload_ref.as_deref().unwrap())?
                .is_some());
            self.trace.lock().unwrap().push("start");
            Ok(())
        })
    }
    fn on_subagent_stop<'a>(
        &'a self,
        event: SubagentLifecycleHookEvent,
    ) -> SubagentLifecycleObserverFuture<'a> {
        Box::pin(async move {
            let job = self.store.get_runtime_job(&event.job_id)?.unwrap();
            assert_eq!(
                event.status,
                Some(SubagentLifecycleStatus::from(job.status.clone()))
            );
            if job.status == RuntimeJobStatus::Succeeded {
                let result = self
                    .store
                    .load_external_context_object(&job.output_refs[0])?
                    .unwrap();
                assert_eq!(result.content, "complete chain result");
            }
            let conn = rusqlite::Connection::open(&self.db_path).unwrap();
            let pending: i64 = conn
                .query_row(
                    "SELECT count(*) FROM runtime_job_outbox WHERE job_id=?1",
                    [&job.job_id],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(pending, i64::from(job.status.is_terminal()));
            self.trace.lock().unwrap().push("stop");
            Ok(())
        })
    }
}

struct OrderedRunner {
    trace: Arc<Mutex<Vec<&'static str>>>,
    outcome: SubagentWorkerRunOutcome,
}

impl AsyncSubagentWorkerRunner for OrderedRunner {
    fn run_async<'a>(&'a self, req: SubagentWorkerRunRequest) -> SubagentWorkerRunFuture<'a> {
        Box::pin(async move {
            assert_eq!(req.job.status, RuntimeJobStatus::Running);
            assert_eq!(*self.trace.lock().unwrap(), vec!["start"]);
            self.trace.lock().unwrap().push("run");
            self.outcome.clone()
        })
    }
}

#[tokio::test]
async fn complete_job_chain_commits_before_lifecycle_notifications() {
    verify_chain(false).await;
}

#[tokio::test]
async fn native_sqlite_job_chain_preserves_commit_and_notification_order() {
    verify_chain(true).await;
}

async fn verify_chain(native: bool) {
    for outcome in [
        SubagentWorkerRunOutcome::Succeeded {
            summary: "complete chain result".into(),
            output_refs: vec![],
        },
        SubagentWorkerRunOutcome::Failed {
            error: "retryable".into(),
            retry: Some(SubagentRunRetry {
                next_run_at_ms: 2000,
            }),
        },
        SubagentWorkerRunOutcome::Failed {
            error: "fatal".into(),
            retry: None,
        },
        SubagentWorkerRunOutcome::Cancelled {
            reason: "cancelled".into(),
        },
    ] {
        let directory = tempfile::tempdir().unwrap();
        let template = SqliteRuntimeStore::new(directory.path().join("template.db")).unwrap();
        let store = SqliteRuntimeStore::new(directory.path().join("runtime.db")).unwrap();
        store_valid_subagent_work_packet(
            &template,
            "packet",
            "resource",
            "Borrow",
            vec!["read".into()],
            vec![],
        );
        let packet = template
            .load_external_context_object("packet")
            .unwrap()
            .unwrap();
        let job = build_subagent_run_job(SubagentRunJobRequest {
            session_id: "session-chain".into(),
            parent_turn_id: "turn-chain".into(),
            tool_call_id: "call-chain".into(),
            subagent_id: "subagent-chain".into(),
            work_packet_ref: "packet".into(),
            checkpoint_id: None,
            run_at_ms: 1000,
            created_at_ms: 900,
            max_retries: 1,
        });
        let creation = UpsertExternalContextAndScheduleJobRequest {
            object: packet,
            job: job.clone(),
        };
        let trace = Arc::new(Mutex::new(vec![]));
        let runner = OrderedRunner {
            trace: trace.clone(),
            outcome,
        };
        let observer = DurableObserver {
            store: store.clone(),
            db_path: directory.path().join("runtime.db"),
            trace: trace.clone(),
        };
        let request = RunDueSubagentJobsRequest {
            now_ms: 1001,
            worker_id: "worker-chain".into(),
            session_id: Some("session-chain".into()),
            limit: 1,
            lease_ms: 5000,
            started_at_ms: 1002,
            finished_at_ms: 1010,
        };
        let result = if native {
            let jobs = centaeris_runtime_sqlite::job_store::SqliteJobStore::open(
                directory.path().join("runtime.db"),
                2,
            )
            .await
            .unwrap();
            jobs.upsert_external_context_and_schedule_job(creation)
                .await
                .unwrap();
            let result = run_due_subagent_jobs_with_worker_pool_async(
                &jobs,
                &runner,
                &observer,
                request,
                SubagentWorkerPoolPolicy { max_parallelism: 1 },
            )
            .await
            .unwrap();
            assert_eq!(jobs.capacity().await, (2, 0));
            jobs.close().await.unwrap();
            result
        } else {
            store
                .upsert_external_context_and_schedule_job(creation)
                .unwrap();
            let actor = RuntimeStoreActor::start(store.clone()).unwrap();
            run_due_subagent_jobs_with_worker_pool_async(
                &actor,
                &runner,
                &observer,
                request,
                SubagentWorkerPoolPolicy { max_parallelism: 1 },
            )
            .await
            .unwrap()
        };
        assert_eq!(*trace.lock().unwrap(), vec!["start", "run", "stop"]);
        assert_eq!(result.results.len(), 1);
        assert!(result.results[0].final_lifecycle.status != SubagentLifecycleStatus::Running);
        assert!(SqliteRuntimeStore::new(directory.path().join("runtime.db"))
            .unwrap()
            .get_runtime_job(&job.job_id)
            .unwrap()
            .is_some());
    }
}
