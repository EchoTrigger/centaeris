//! Test-only admission and lifetime policy around the library's single queue.
//! No actor, database thread, SQL, command registry or blocking bridge lives here.
use crate::sqlite_transactions::TransactionEvent;
use std::future::{poll_fn, Future};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::Instant;
use tokio::sync::{watch, Semaphore};
use tokio_util::task::TaskTracker;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Outcome {
    ReadOnly,
    NotExecuted,
    RolledBack,
    Committed,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(super) enum WorkError {
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

#[derive(Debug)]
pub(super) struct Receipt<T> {
    pub outcome: Outcome,
    pub result: Result<T, WorkError>,
    pub healthy: bool,
    pub cleanup_error: Option<String>,
    pub sync_after: Option<i64>,
}

pub(super) type Pending<T> = Pin<Box<dyn Future<Output = Receipt<T>> + Send>>;

pub(super) struct Options {
    pub full_sync: bool,
    pub deadline: Option<Instant>,
    pub now: Arc<dyn Fn() -> Instant + Send + Sync>,
    pub before_restore: Option<Box<dyn FnOnce() + Send>>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            full_sync: false,
            deadline: None,
            now: Arc::new(Instant::now),
            before_restore: None,
        }
    }
}

struct Admission {
    accepting: bool,
    healthy: bool,
    closing: bool,
}

#[derive(Clone)]
pub(super) struct Driver {
    connection: tokio_rusqlite::Connection,
    state: Arc<Mutex<Admission>>,
    capacity: Arc<Semaphore>,
    tracker: TaskTracker,
    closed: watch::Sender<Option<Result<(), String>>>,
    limit: usize,
}

impl Driver {
    pub async fn open(path: std::path::PathBuf, limit: usize) -> Self {
        let connection = tokio_rusqlite::Connection::open(path).await.unwrap();
        connection
            .call(|conn| {
                crate::sqlite_schema::ensure_schema(conn)
                    .map_err(|e| tokio_rusqlite::Error::Other(Box::new(WorkError::Business(e))))?;
                crate::configure_conn(conn)
                    .map_err(|e| tokio_rusqlite::Error::Other(Box::new(WorkError::Business(e))))?;
                Ok(())
            })
            .await
            .unwrap();
        let (closed, _) = watch::channel(None);
        Self {
            connection,
            state: Arc::new(Mutex::new(Admission {
                accepting: true,
                healthy: true,
                closing: false,
            })),
            capacity: Arc::new(Semaphore::new(limit)),
            tracker: TaskTracker::new(),
            closed,
            limit,
        }
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
        let permit = self
            .capacity
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| WorkError::AdmissionStopped)?;
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
        let state = self.state.clone();
        let error_state = state.clone();
        let connection = self.connection.clone();
        let mut pending: Pending<T> = Box::pin(async move {
            let reply = connection
                .call(move |conn| {
                    let _permit = permit;
                    let _token = token;
                    if !state.lock().unwrap().healthy {
                        return Ok(Receipt {
                            outcome: Outcome::NotExecuted,
                            result: Err(WorkError::Unusable),
                            healthy: false,
                            cleanup_error: None,
                            sync_after: None,
                        });
                    }
                    if options
                        .deadline
                        .is_some_and(|deadline| (options.now)() > deadline)
                    {
                        return Ok(Receipt {
                            outcome: Outcome::NotExecuted,
                            result: Err(WorkError::Expired),
                            healthy: true,
                            cleanup_error: None,
                            sync_after: None,
                        });
                    }
                    let original_sync =
                        conn.pragma_query_value(None, "synchronous", |r| r.get::<_, i64>(0));
                    let mut outcome = Outcome::NotExecuted;
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
                        })
                    }));
                    let result = match result {
                        Ok(Ok(value)) => {
                            if !begun {
                                outcome = Outcome::ReadOnly;
                            }
                            Ok(value)
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
                    let sync_after = conn
                        .pragma_query_value(None, "synchronous", |r| r.get::<_, i64>(0))
                        .ok();
                    Ok(Receipt {
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
                        outcome: Outcome::Unknown,
                        result: Err(WorkError::ReplyUnavailable(error.to_string())),
                        healthy: false,
                        cleanup_error: None,
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

    pub fn assert_idle(&self) {
        assert!(self.tracker.is_empty());
        assert_eq!(self.capacity.available_permits(), self.limit);
    }
}
