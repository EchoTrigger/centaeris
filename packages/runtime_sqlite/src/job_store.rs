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
use std::sync::Arc;
use tokio::sync::Mutex;

struct Owner {
    driver: Driver,
    stopped: bool,
}

#[derive(Clone)]
pub struct SqliteJobStore {
    path: PathBuf,
    limit: usize,
    owner: Arc<Mutex<Owner>>,
}

impl SqliteJobStore {
    pub async fn open(path: PathBuf, limit: usize) -> Result<Self, JobStoreError> {
        if limit == 0 {
            return Err(JobStoreError::Rejected(
                "SQLite Job capacity must be positive".into(),
            ));
        }
        let driver = Driver::open(path.clone(), limit)
            .await
            .map_err(|error| JobStoreError::not_executed(JobStoreOperation::Read, error))?;
        Ok(Self {
            path,
            limit,
            owner: Arc::new(Mutex::new(Owner {
                driver,
                stopped: false,
            })),
        })
    }

    /// The owner drains the retired epoch before opening its replacement.
    /// This never retries the previous operation. A new read cannot race the
    /// previous epoch's transaction and mistake temporary absence for rollback.
    async fn connection(&self, operation: JobStoreOperation) -> Result<Driver, JobStoreError> {
        let mut owner = self.owner.lock().await;
        if owner.stopped {
            return Err(JobStoreError::Storage {
                operation,
                outcome: JobWriteOutcome::NotExecuted,
                message: "SQLite Job admission stopped".into(),
            });
        }
        if !owner.driver.is_healthy() {
            owner
                .driver
                .close()
                .await
                .map_err(|error| JobStoreError::not_executed(operation, error))?;
            owner.driver = Driver::open(self.path.clone(), self.limit)
                .await
                .map_err(|error| JobStoreError::not_executed(operation, error))?;
        }
        Ok(owner.driver.clone())
    }

    pub async fn close(&self) -> Result<(), String> {
        let driver = {
            let mut owner = self.owner.lock().await;
            owner.stopped = true;
            owner.driver.begin_close();
            owner.driver.clone()
        };
        driver.close().await
    }

    pub async fn capacity(&self) -> (usize, usize) {
        self.owner.lock().await.driver.capacity()
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
            let tx = conn
                .transaction()
                .map_err(|error| format!("begin start_runtime_job transaction failed: {error}"))?;
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
