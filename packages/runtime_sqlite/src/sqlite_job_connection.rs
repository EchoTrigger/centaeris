//! Admission and lifetime policy around the library's single SQLite queue.
//! No actor, database thread, SQL, command registry or blocking bridge lives here.
use crate::sqlite_transactions::TransactionEvent;
use std::future::{poll_fn, Future};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::Instant;
use tokio::sync::{watch, Semaphore};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

pub(crate) use centaeris_core::session::store::job::JobWriteOutcome as Outcome;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum WorkError {
    #[error("admission stopped")]
    AdmissionStopped,
    #[error("connection unusable")]
    Unusable,
    #[error("execution deadline expired")]
    Expired,
    #[error("{0}")]
    Business(String),
    #[error("database work panicked")]
    Panicked,
    #[error("driver reply unavailable: {0}")]
    ReplyUnavailable(String),
}

#[cfg(test)]
mod resource_tests;

#[derive(Debug)]
pub(crate) struct Receipt<T> {
    pub outcome: Outcome,
    pub result: Result<T, WorkError>,
    pub healthy: bool,
    pub cleanup_error: Option<String>,
    #[cfg(test)]
    pub sync_after: Option<i64>,
}

pub(crate) type Pending<T> = Pin<Box<dyn Future<Output = Receipt<T>> + Send>>;

#[cfg_attr(not(test), derive(Default))]
pub(crate) struct Options {
    pub full_sync: bool,
    pub read_only: bool,
    pub deadline: Option<Instant>,
    #[cfg(test)]
    pub now: Arc<dyn Fn() -> Instant + Send + Sync>,
    #[cfg(test)]
    pub before_restore: Option<Box<dyn FnOnce() + Send>>,
}

#[cfg(test)]
impl Default for Options {
    fn default() -> Self {
        Self {
            full_sync: false,
            read_only: false,
            deadline: None,
            #[cfg(test)]
            now: Arc::new(Instant::now),
            #[cfg(test)]
            before_restore: None,
        }
    }
}

impl Options {
    fn now(&self) -> Instant {
        #[cfg(test)]
        {
            (self.now)()
        }
        #[cfg(not(test))]
        {
            Instant::now()
        }
    }
}

struct Admission {
    accepting: bool,
    healthy: bool,
    closing: bool,
}

/// A connection epoch's actual work, rather than the number of reply waiters.
#[derive(Debug, Clone, Copy)]
pub struct JobConnectionMetrics {
    pub capacity: usize,
    pub in_flight: usize,
    pub executing: usize,
    pub started: u64,
    pub admission_wait_ns: u64,
    pub queue_wait_ns: u64,
    /// Typed BUSY/LOCKED errors observed at transaction begin/commit boundaries.
    /// String-returning business helpers do not expose their inner SQL errors.
    pub transaction_lock_conflicts: u64,
}

#[derive(Default)]
struct Metrics {
    executing: AtomicUsize,
    started: AtomicU64,
    admission_wait_ns: AtomicU64,
    queue_wait_ns: AtomicU64,
    transaction_lock_conflicts: AtomicU64,
}

struct Executing(Arc<Metrics>);
impl Drop for Executing {
    fn drop(&mut self) {
        self.0.executing.fetch_sub(1, Ordering::Relaxed);
    }
}

fn elapsed_ns(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

#[derive(Clone)]
pub(crate) struct Driver {
    connection: tokio_rusqlite::Connection,
    state: Arc<Mutex<Admission>>,
    capacity: Arc<Semaphore>,
    admission: Option<Arc<Semaphore>>,
    stop: CancellationToken,
    tracker: TaskTracker,
    closed: watch::Sender<Option<Result<(), String>>>,
    limit: usize,
    metrics: Arc<Metrics>,
}

impl Driver {
    pub fn from_connection_with_admission(
        connection: rusqlite::Connection,
        limit: usize,
        admission: Option<Arc<Semaphore>>,
    ) -> Self {
        Self::from_handle(
            tokio_rusqlite::Connection::from(connection),
            limit,
            admission,
        )
    }

    fn from_handle(
        connection: tokio_rusqlite::Connection,
        limit: usize,
        admission: Option<Arc<Semaphore>>,
    ) -> Self {
        let (closed, _) = watch::channel(None);
        Self {
            connection,
            state: Arc::new(Mutex::new(Admission {
                accepting: true,
                healthy: true,
                closing: false,
            })),
            capacity: Arc::new(Semaphore::new(limit)),
            admission,
            stop: CancellationToken::new(),
            tracker: TaskTracker::new(),
            closed,
            limit,
            metrics: Arc::new(Metrics::default()),
        }
    }
    pub fn is_healthy(&self) -> bool {
        self.state.lock().unwrap().healthy
    }

    pub fn capacity(&self) -> (usize, usize) {
        (self.limit, self.limit - self.capacity.available_permits())
    }
    pub fn metrics(&self) -> JobConnectionMetrics {
        JobConnectionMetrics {
            capacity: self.limit,
            in_flight: self.tracker.len(),
            executing: self.metrics.executing.load(Ordering::Relaxed),
            started: self.metrics.started.load(Ordering::Relaxed),
            admission_wait_ns: self.metrics.admission_wait_ns.load(Ordering::Relaxed),
            queue_wait_ns: self.metrics.queue_wait_ns.load(Ordering::Relaxed),
            transaction_lock_conflicts: self
                .metrics
                .transaction_lock_conflicts
                .load(Ordering::Relaxed),
        }
    }
    #[cfg(test)]
    pub async fn open(path: std::path::PathBuf, limit: usize) -> Result<Self, String> {
        Self::open_with_admission(path, limit, None).await
    }

    pub async fn open_with_admission(
        path: std::path::PathBuf,
        limit: usize,
        admission: Option<Arc<Semaphore>>,
    ) -> Result<Self, String> {
        let connection = tokio_rusqlite::Connection::open(path)
            .await
            .map_err(|error| error.to_string())?;
        let configured = connection
            .call(|conn| {
                crate::sqlite_schema::ensure_schema(conn)
                    .map_err(|e| tokio_rusqlite::Error::Other(Box::new(WorkError::Business(e))))?;
                crate::configure_conn(conn)
                    .map_err(|e| tokio_rusqlite::Error::Other(Box::new(WorkError::Business(e))))?;
                Ok(())
            })
            .await;
        if let Err(error) = configured {
            let closed = connection.close().await;
            return Err(match closed {
                Ok(()) => error.to_string(),
                Err(close_error) => {
                    format!("{error}; failed to close startup connection: {close_error}")
                }
            });
        }
        Ok(Self::from_handle(connection, limit, admission))
    }

    /// Poll once before returning: the library has accepted the closure, or the
    /// returned receipt already describes rejection. Dropping Pending only drops
    /// the reply waiter; the permit and tracking token stay with the closure.
    pub async fn submit<T, F>(
        &self,
        options: Options,
        operation: F,
    ) -> Result<Pending<T>, WorkError>
    where
        T: Send + 'static,
        F: FnOnce(&mut rusqlite::Connection, &mut dyn FnMut(TransactionEvent)) -> Result<T, String>
            + Send
            + 'static,
    {
        let admission_started = Instant::now();
        let permit = self
            .capacity
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| WorkError::AdmissionStopped)?;
        let admission = if let Some(admission) = &self.admission {
            Some(tokio::select! {
                biased;
                _ = self.stop.cancelled() => return Err(WorkError::AdmissionStopped),
                permit = admission.clone().acquire_owned() => permit.map_err(|_| WorkError::AdmissionStopped)?,
            })
        } else {
            None
        };
        let token = {
            let state = self.state.lock().unwrap();
            if !state.accepting {
                return Err(WorkError::AdmissionStopped);
            }
            if !state.healthy {
                return Err(WorkError::Unusable);
            }
            // Close acquires the same lock before stopping admission and waiting.
            self.tracker.token()
        };
        self.metrics
            .admission_wait_ns
            .fetch_add(elapsed_ns(admission_started), Ordering::Relaxed);
        let enqueued_at = Instant::now();
        let metrics = self.metrics.clone();
        let state = self.state.clone();
        let error_state = state.clone();
        let connection = self.connection.clone();
        let read_only = options.read_only;
        let mut pending: Pending<T> = Box::pin(async move {
            let reply = connection
                .call(move |conn| {
                    let _permit = permit;
                    let _admission = admission;
                    let _token = token;
                    metrics
                        .queue_wait_ns
                        .fetch_add(elapsed_ns(enqueued_at), Ordering::Relaxed);
                    metrics.started.fetch_add(1, Ordering::Relaxed);
                    metrics.executing.fetch_add(1, Ordering::Relaxed);
                    let _executing = Executing(metrics.clone());
                    if !state.lock().unwrap().healthy {
                        return Ok(Receipt {
                            outcome: Outcome::NotExecuted,
                            result: Err(WorkError::Unusable),
                            healthy: false,
                            cleanup_error: None,
                            #[cfg(test)]
                            sync_after: None,
                        });
                    }
                    if options
                        .deadline
                        .is_some_and(|deadline| options.now() > deadline)
                    {
                        return Ok(Receipt {
                            outcome: Outcome::NotExecuted,
                            result: Err(WorkError::Expired),
                            healthy: true,
                            cleanup_error: None,
                            #[cfg(test)]
                            sync_after: None,
                        });
                    }
                    let original_sync =
                        conn.pragma_query_value(None, "synchronous", |r| r.get::<_, i64>(0));
                    let mut outcome = if options.read_only {
                        Outcome::ReadOnly
                    } else {
                        Outcome::NotExecuted
                    };
                    let mut begun = false;
                    let mut commit_attempted = false;
                    let mut cleanup_error = None;
                    let mut healthy = true;
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        if options.full_sync {
                            original_sync.as_ref().map_err(|e| e.to_string())?;
                            conn.pragma_update(None, "synchronous", "FULL")
                                .map_err(|e| e.to_string())?;
                        }
                        operation(conn, &mut |event| match event {
                            TransactionEvent::Began => {
                                begun = true;
                                outcome = Outcome::Unknown;
                            }
                            TransactionEvent::BeforeCommit => {}
                            TransactionEvent::CommitAttempted => {
                                commit_attempted = true;
                            }
                            TransactionEvent::Committed => {
                                outcome = Outcome::Committed;
                            }
                            TransactionEvent::RolledBack => {
                                outcome = Outcome::RolledBack;
                            }
                            TransactionEvent::RollbackFailed => {
                                healthy = false;
                                cleanup_error = Some("transaction rollback failed".into());
                            }
                            TransactionEvent::SqliteError(code) => {
                                if matches!(
                                    code,
                                    rusqlite::ErrorCode::DatabaseBusy
                                        | rusqlite::ErrorCode::DatabaseLocked
                                ) {
                                    metrics
                                        .transaction_lock_conflicts
                                        .fetch_add(1, Ordering::Relaxed);
                                }
                            }
                        })
                    }));
                    let result = match result {
                        Ok(Ok(value)) => {
                            if !options.read_only && !begun {
                                outcome = Outcome::Unknown;
                                Err(WorkError::ReplyUnavailable(
                                    "write transaction confirmation missing".into(),
                                ))
                            } else {
                                Ok(value)
                            }
                        }
                        Ok(Err(error)) => Err(WorkError::Business(error)),
                        Err(_) => {
                            healthy = false;
                            if begun && !commit_attempted && conn.is_autocommit() {
                                outcome = Outcome::RolledBack;
                            }
                            Err(WorkError::Panicked)
                        }
                    };
                    if !conn.is_autocommit() {
                        healthy = false;
                        if let Err(error) = conn.execute_batch("ROLLBACK") {
                            cleanup_error = Some(error.to_string());
                        }
                    }
                    if options.full_sync {
                        let cleanup =
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                #[cfg(test)]
                                if let Some(before_restore) = options.before_restore {
                                    before_restore();
                                }
                                match original_sync {
                                    Ok(original) => {
                                        if let Err(error) =
                                            conn.pragma_update(None, "synchronous", original)
                                        {
                                            healthy = false;
                                            cleanup_error = Some(error.to_string());
                                        }
                                    }
                                    Err(error) => {
                                        healthy = false;
                                        cleanup_error = Some(error.to_string());
                                    }
                                }
                            }));
                        if cleanup.is_err() {
                            healthy = false;
                            cleanup_error = Some("connection cleanup panicked".into());
                        }
                    }
                    if outcome == Outcome::Unknown {
                        healthy = false;
                    }
                    if !healthy {
                        state.lock().unwrap().healthy = false;
                    }
                    #[cfg(test)]
                    let sync_after = conn
                        .pragma_query_value(None, "synchronous", |r| r.get::<_, i64>(0))
                        .ok();
                    Ok(Receipt {
                        #[cfg(test)]
                        sync_after,
                        outcome,
                        result,
                        healthy,
                        cleanup_error,
                    })
                })
                .await;
            match reply {
                Ok(receipt) => receipt,
                Err(error) => {
                    error_state.lock().unwrap().healthy = false;
                    Receipt {
                        outcome: if read_only {
                            Outcome::ReadOnly
                        } else {
                            Outcome::Unknown
                        },
                        result: Err(WorkError::ReplyUnavailable(error.to_string())),
                        healthy: false,
                        cleanup_error: None,
                        #[cfg(test)]
                        sync_after: None,
                    }
                }
            }
        });
        let immediate = poll_fn(|cx| {
            Poll::Ready(match pending.as_mut().poll(cx) {
                Poll::Ready(receipt) => Some(receipt),
                Poll::Pending => None,
            })
        })
        .await;
        Ok(match immediate {
            Some(receipt) => Box::pin(std::future::ready(receipt)),
            None => pending,
        })
    }

    pub fn begin_close(&self) {
        let mut state = self.state.lock().unwrap();
        if state.closing {
            return;
        }
        state.closing = true;
        state.accepting = false;
        self.stop.cancel();
        self.capacity.close();
        self.tracker.close();
        let tracker = self.tracker.clone();
        let connection = self.connection.clone();
        let closed = self.closed.clone();
        // This fixed closing task outlives any particular close waiter.
        tokio::spawn(async move {
            tracker.wait().await;
            let result = connection.close().await.map_err(|e| e.to_string());
            closed.send_replace(Some(result));
        });
    }

    pub async fn close(&self) -> Result<(), String> {
        self.begin_close();
        let mut closed = self.closed.subscribe();
        loop {
            if let Some(result) = closed.borrow().clone() {
                return result;
            }
            closed.changed().await.map_err(|e| e.to_string())?;
        }
    }

    #[cfg(test)]
    pub fn assert_idle(&self) {
        assert!(self.tracker.is_empty());
        assert_eq!(self.capacity.available_permits(), self.limit);
    }
}
