//! Native asynchronous Job operations. The existing synchronous adapter remains
//! available for snapshots and non-Job capabilities; neither path calls an Actor.
use crate::sqlite_job_connection::{Driver, Options};
use crate::{sqlite_external_context, sqlite_reliability, sqlite_transactions};
use centaeris_core::session::external_context::ExternalContextObject;
use centaeris_core::session::reliability::*;
use centaeris_core::session::store::job::{JobStoreError, JobStoreOperation, JobWriteOutcome};
use centaeris_core::session::store::{
    AsyncJobStorePort, UpsertExternalContextAndScheduleJobRequest,
    UpsertExternalContextLinkAndCompleteJobRequest,
};
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tokio::sync::{watch, Mutex, Semaphore};

pub use crate::sqlite_job_connection::JobConnectionMetrics;

struct Owner {
    driver: Driver,
}

#[derive(Clone)]
pub struct SqliteJobStore {
    path: PathBuf,
    limit: usize,
    admission: Option<Arc<Semaphore>>,
    owner: Arc<Mutex<Owner>>,
    closing: Arc<AtomicBool>,
    closed: watch::Sender<Option<Result<(), String>>>,
}

impl SqliteJobStore {
    /// Trusted host service control. The marker and fenced cancellation commit
    /// together; an arbitrary user-supplied cancellation reason cannot create it.
    pub async fn cancel_for_service_shutdown(
        &self,
        req: CancelRuntimeJobRequest,
    ) -> Result<(), JobStoreError> {
        self.execute(JobStoreOperation::Cancel, move |conn, observe| {
            let tx = conn.transaction().map_err(|error| {
                sqlite_transactions::observe_sqlite_error(observe, &error);
                format!("begin service Job cancellation failed: {error}")
            })?;
            sqlite_transactions::commit_job_write(
                tx,
                "commit service Job cancellation failed",
                observe,
                |conn| {
                    sqlite_reliability::cancel_runtime_job_conn(conn, &req)?;
                    let job = sqlite_reliability::load_runtime_job_by_id(conn, &req.job_id)?
                        .ok_or("service Job cancellation lost Job")?;
                    let session = job
                        .session_id
                        .ok_or("service Job cancellation parent missing")?;
                    crate::sqlite_runtime::append_event_conn_idempotent(
                        conn,
                        &centaeris_core::runtime::contracts::RuntimeEvent {
                            event_id: format!("local_service_stop:{}", req.job_id),
                            session_id: session,
                            task_id: job.branch_id,
                            event_type: "local_runtime.service_stop.v1".into(),
                            at_ms: req.cancelled_at_ms,
                            visibility:
                                centaeris_core::runtime::contracts::EventVisibility::Internal,
                            payload_json:
                                serde_json::json!({"runtimeJobId": req.job_id, "source": "service"})
                                    .to_string(),
                        },
                    )?;
                    sqlite_reliability::upsert_runtime_job_outbox_event(
                        conn,
                        &req.job_id,
                        RUNTIME_JOB_TERMINAL_EVENT,
                    )
                },
            )
        })
        .await
    }

    pub async fn was_service_stopped(&self, id: &str) -> Result<bool, JobStoreError> {
        let event = format!("local_service_stop:{id}");
        self.execute(JobStoreOperation::Read, move |conn, _| {
            conn.query_row("SELECT EXISTS(SELECT 1 FROM runtime_events WHERE event_id=?1 AND event_type='local_runtime.service_stop.v1')",
                [event], |row| row.get::<_, bool>(0)).map_err(|error| format!("read service Job stop provenance failed: {error}"))
        }).await
    }
    pub async fn pending_terminal_deliveries(
        &self,
        limit: usize,
        job_kind: Option<String>,
    ) -> Result<Vec<RuntimeJobOutboxRecord>, JobStoreError> {
        self.execute(JobStoreOperation::Read, move |conn, _| {
            sqlite_reliability::list_pending_runtime_job_outbox_conn(
                conn,
                limit,
                job_kind.as_deref(),
                None,
            )
        })
        .await
    }

    /// Continue an ordered outbox scan after the last attempted delivery.
    /// Generation remains an acknowledgement fence, not a pagination key.
    pub async fn pending_terminal_deliveries_after(
        &self,
        limit: usize,
        job_kind: Option<String>,
        after: Option<(String, String)>,
    ) -> Result<Vec<RuntimeJobOutboxRecord>, JobStoreError> {
        self.execute(JobStoreOperation::Read, move |conn, _| {
            sqlite_reliability::list_pending_runtime_job_outbox_after_conn(
                conn,
                limit,
                job_kind.as_deref(),
                None,
                after
                    .as_ref()
                    .map(|(job, event)| (job.as_str(), event.as_str())),
            )
        })
        .await
    }

    pub async fn pending_terminal_delivery(
        &self,
        job_id: &str,
    ) -> Result<Option<RuntimeJobOutboxRecord>, JobStoreError> {
        let id = job_id.to_string();
        self.execute(JobStoreOperation::Read, move |conn, _| {
            sqlite_reliability::list_pending_runtime_job_outbox_conn(conn, 1, None, Some(&id))
                .map(|records| records.into_iter().next())
        })
        .await
    }

    pub async fn acknowledge_terminal_delivery(
        &self,
        record: RuntimeJobOutboxRecord,
        at_ms: i64,
    ) -> Result<RuntimeJobOutboxPublishDisposition, JobStoreError> {
        self.execute(JobStoreOperation::Acknowledge, move |conn, observe| {
            let tx = conn.transaction().map_err(|error| {
                sqlite_transactions::observe_sqlite_error(observe, &error);
                format!("begin terminal delivery acknowledgement failed: {error}")
            })?;
            sqlite_transactions::commit_job_write(
                tx,
                "commit terminal delivery acknowledgement failed",
                observe,
                |conn| {
                    sqlite_reliability::mark_runtime_job_outbox_published_conn(
                        conn,
                        &record.job_id,
                        &record.event_type,
                        record.generation,
                        at_ms,
                    )
                },
            )
        })
        .await
    }
    /// Synchronous assembly only. The library takes ownership of the configured
    /// connection; all Job operations subsequently use its asynchronous queue.
    pub fn new(path: PathBuf, limit: usize) -> Result<Self, JobStoreError> {
        Self::new_inner(path, limit, None)
    }

    /// The host supplies the same budget used by its other managed store paths.
    pub fn new_with_admission(
        path: PathBuf,
        limit: usize,
        admission: Arc<Semaphore>,
    ) -> Result<Self, JobStoreError> {
        Self::new_inner(path, limit, Some(admission))
    }

    fn new_inner(
        path: PathBuf,
        limit: usize,
        admission: Option<Arc<Semaphore>>,
    ) -> Result<Self, JobStoreError> {
        if limit == 0 {
            return Err(JobStoreError::Rejected(
                "SQLite Job capacity must be positive".into(),
            ));
        }
        let conn = rusqlite::Connection::open(&path).map_err(|error| {
            JobStoreError::not_executed(JobStoreOperation::Read, error.to_string())
        })?;
        crate::sqlite_schema::ensure_schema(&conn)
            .and_then(|()| crate::configure_conn(&conn))
            .map_err(|error| JobStoreError::not_executed(JobStoreOperation::Read, error))?;
        let driver = Driver::from_connection_with_admission(conn, limit, admission.clone());
        Ok(Self::from_driver(path, limit, driver, admission))
    }
    pub async fn open(path: PathBuf, limit: usize) -> Result<Self, JobStoreError> {
        Self::open_inner(path, limit, None).await
    }

    pub async fn open_with_admission(
        path: PathBuf,
        limit: usize,
        admission: Arc<Semaphore>,
    ) -> Result<Self, JobStoreError> {
        Self::open_inner(path, limit, Some(admission)).await
    }

    async fn open_inner(
        path: PathBuf,
        limit: usize,
        admission: Option<Arc<Semaphore>>,
    ) -> Result<Self, JobStoreError> {
        if limit == 0 {
            return Err(JobStoreError::Rejected(
                "SQLite Job capacity must be positive".into(),
            ));
        }
        let driver = Driver::open_with_admission(path.clone(), limit, admission.clone())
            .await
            .map_err(|error| JobStoreError::not_executed(JobStoreOperation::Read, error))?;
        Ok(Self::from_driver(path, limit, driver, admission))
    }

    fn from_driver(
        path: PathBuf,
        limit: usize,
        driver: Driver,
        admission: Option<Arc<Semaphore>>,
    ) -> Self {
        let (closed, _) = watch::channel(None);
        Self {
            path,
            limit,
            admission,
            owner: Arc::new(Mutex::new(Owner { driver })),
            closing: Arc::new(AtomicBool::new(false)),
            closed,
        }
    }

    fn require_admission(&self, operation: JobStoreOperation) -> Result<(), JobStoreError> {
        if self.closing.load(Ordering::Acquire) {
            Err(JobStoreError::not_executed(
                operation,
                "SQLite Job admission stopped".into(),
            ))
        } else {
            Ok(())
        }
    }

    /// The owner drains the retired epoch before opening its replacement.
    /// This never retries the previous operation. A new read cannot race the
    /// previous epoch's transaction and mistake temporary absence for rollback.
    async fn connection(&self, operation: JobStoreOperation) -> Result<Driver, JobStoreError> {
        self.require_admission(operation)?;
        let mut owner = self.owner.lock().await;
        self.require_admission(operation)?;
        if !owner.driver.is_healthy() {
            owner
                .driver
                .close()
                .await
                .map_err(|error| JobStoreError::not_executed(operation, error))?;
            self.require_admission(operation)?;
            owner.driver =
                Driver::open_with_admission(self.path.clone(), self.limit, self.admission.clone())
                    .await
                    .map_err(|error| JobStoreError::not_executed(operation, error))?;
        }
        self.require_admission(operation)?;
        Ok(owner.driver.clone())
    }

    pub async fn close(&self) -> Result<(), String> {
        if !self.closing.swap(true, Ordering::AcqRel) {
            let owner = self.owner.clone();
            let closed = self.closed.clone();
            // Even a waiter cancelled behind epoch replacement has requested
            // shutdown. This fixed lifecycle task owns the actual completion.
            tokio::spawn(async move {
                let driver = {
                    let owner = owner.lock().await;
                    owner.driver.begin_close();
                    owner.driver.clone()
                };
                closed.send_replace(Some(driver.close().await));
            });
        }
        let mut closed = self.closed.subscribe();
        loop {
            if let Some(result) = closed.borrow().clone() {
                return result;
            }
            closed.changed().await.map_err(|error| error.to_string())?;
        }
    }

    pub async fn capacity(&self) -> (usize, usize) {
        self.owner.lock().await.driver.capacity()
    }

    /// Snapshot of the current connection epoch; a replacement starts new counters.
    pub async fn resource_metrics(&self) -> JobConnectionMetrics {
        self.owner.lock().await.driver.metrics()
    }

    async fn execute<T: Send + 'static>(
        &self,
        operation: JobStoreOperation,
        write: impl FnOnce(
                &mut rusqlite::Connection,
                &mut dyn FnMut(sqlite_transactions::TransactionEvent),
            ) -> Result<T, String>
            + Send
            + 'static,
    ) -> Result<T, JobStoreError> {
        let driver = self.connection(operation).await?;
        let pending = driver
            .submit(
                Options {
                    read_only: operation == JobStoreOperation::Read,
                    full_sync: operation == JobStoreOperation::Reclaim,
                    ..Options::default()
                },
                write,
            )
            .await
            .map_err(|error| JobStoreError::Storage {
                operation,
                outcome: JobWriteOutcome::NotExecuted,
                message: error.to_string(),
            })?;
        let receipt = pending.await;
        // Connection health is owned independently of the write result. In
        // particular, successful commit + failed cleanup still returns success.
        // The next connection acquisition drains and replaces the retired epoch.
        debug_assert!(receipt.healthy || !driver.is_healthy());
        #[cfg(test)]
        let _ = receipt.sync_after;
        receipt.result.map_err(|error| JobStoreError::Storage {
            operation,
            outcome: receipt.outcome,
            message: match receipt.cleanup_error {
                Some(cleanup) => format!("{error}; connection cleanup failed: {cleanup}"),
                None => error.to_string(),
            },
        })
    }
}

#[async_trait::async_trait]
impl AsyncJobStorePort for SqliteJobStore {
    async fn settle_job_operations(&self) -> Result<(), JobStoreError> {
        self.execute(JobStoreOperation::Read, |_, _| Ok(())).await
    }
    async fn get_runtime_job(
        &self,
        job_id: &str,
    ) -> Result<Option<RuntimeJobRecord>, JobStoreError> {
        let id = job_id.to_owned();
        self.execute(JobStoreOperation::Read, move |conn, _| {
            sqlite_reliability::load_runtime_job_by_id(conn, &id)
        })
        .await
    }
    async fn list_runtime_jobs(
        &self,
        req: ListRuntimeJobsRequest,
    ) -> Result<Vec<RuntimeJobRecord>, JobStoreError> {
        self.execute(JobStoreOperation::Read, move |conn, _| {
            sqlite_reliability::list_runtime_jobs_with_filters(conn, req)
        })
        .await
    }
    async fn claim_due_runtime_jobs(
        &self,
        req: ClaimDueRuntimeJobsRequest,
    ) -> Result<Vec<RuntimeJobRecord>, JobStoreError> {
        self.execute(JobStoreOperation::Claim, move |conn, observe| {
            sqlite_reliability::claim_due_runtime_jobs_transaction_conn(conn, req, observe)
        })
        .await
    }
    async fn start_runtime_job(&self, req: StartRuntimeJobRequest) -> Result<(), JobStoreError> {
        self.execute(JobStoreOperation::Start, move |conn, observe| {
            let tx = conn.transaction().map_err(|error| {
                sqlite_transactions::observe_sqlite_error(observe, &error);
                format!("begin start_runtime_job transaction failed: {error}")
            })?;
            sqlite_transactions::commit_job_write(
                tx,
                "commit start_runtime_job transaction failed",
                observe,
                |conn| sqlite_reliability::start_runtime_job_conn(conn, &req),
            )
        })
        .await
    }
    async fn renew_runtime_job_lease(
        &self,
        req: RenewRuntimeJobLeaseRequest,
    ) -> Result<(), JobStoreError> {
        let lease_ms = i64::try_from(req.lease_ms)
            .map_err(|_| JobStoreError::Rejected(format!("lease_ms overflow: {}", req.lease_ms)))?;
        let expires = req.heartbeat_at_ms.saturating_add(lease_ms);
        self.execute(JobStoreOperation::Renew, move |conn, observe| {
            let tx = conn.transaction().map_err(|error| {
                sqlite_transactions::observe_sqlite_error(observe, &error);
                format!("begin renew_runtime_job_lease transaction failed: {error}")
            })?;
            sqlite_transactions::commit_job_write(
                tx,
                "commit renew_runtime_job_lease transaction failed",
                observe,
                |conn| sqlite_reliability::renew_runtime_job_lease_conn(conn, &req, expires),
            )
        })
        .await
    }
    async fn complete_runtime_job(
        &self,
        req: CompleteRuntimeJobRequest,
    ) -> Result<(), JobStoreError> {
        self.execute(JobStoreOperation::Complete, move |conn, observe| {
            sqlite_reliability::complete_runtime_job_transaction_conn(conn, req, observe)
        })
        .await
    }
    async fn fail_runtime_job(&self, req: FailRuntimeJobRequest) -> Result<(), JobStoreError> {
        self.execute(JobStoreOperation::Fail, move |conn, observe| {
            sqlite_reliability::fail_runtime_job_transaction_conn(conn, req, observe)
        })
        .await
    }
    async fn cancel_runtime_job(&self, req: CancelRuntimeJobRequest) -> Result<(), JobStoreError> {
        self.execute(JobStoreOperation::Cancel, move |conn, observe| {
            sqlite_reliability::cancel_runtime_job_transaction_conn(conn, req, observe)
        })
        .await
    }
    async fn reclaim_expired_runtime_job_leases(
        &self,
        now_ms: i64,
    ) -> Result<usize, JobStoreError> {
        self.execute(JobStoreOperation::Reclaim, move |conn, observe| {
            sqlite_reliability::reclaim_expired_runtime_job_leases_conn(conn, now_ms, observe)
        })
        .await
    }
    async fn load_external_context_object(
        &self,
        object_id: &str,
    ) -> Result<Option<ExternalContextObject>, JobStoreError> {
        let id = object_id.trim().to_owned();
        if id.is_empty() {
            return Err(JobStoreError::Rejected(
                "external context object_id is required".into(),
            ));
        }
        self.execute(JobStoreOperation::Read, move |conn, _| {
            Ok(sqlite_external_context::load_external_context_object_conn(
                conn, &id,
            ))
        })
        .await?
        .map_err(sqlite_external_context::ObjectLoadError::into_job_error)
    }
    async fn upsert_external_context_and_schedule_job(
        &self,
        req: UpsertExternalContextAndScheduleJobRequest,
    ) -> Result<ScheduleRuntimeJobResult, JobStoreError> {
        self.execute(JobStoreOperation::Create, move |conn, observe| {
            sqlite_transactions::upsert_external_context_and_schedule_job_conn(conn, req, observe)
        })
        .await
    }
    async fn upsert_external_context_link_and_complete_job(
        &self,
        req: UpsertExternalContextLinkAndCompleteJobRequest,
    ) -> Result<(), JobStoreError> {
        self.execute(JobStoreOperation::Complete, move |conn, observe| {
            sqlite_transactions::upsert_external_context_link_and_complete_job_conn(
                conn, req, observe,
            )
        })
        .await
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod shared_admission_tests;
