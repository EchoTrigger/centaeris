//! File-backed driver characterization; production keeps its existing entry path.
use super::job_updates::{combined_completion, completion, Database};
use super::*;
use crate::sqlite_transactions::{
    upsert_external_context_link_and_complete_job_conn, TransactionEvent,
};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::oneshot;
mod driver;
use driver::{Driver, Options, Outcome, Pending, WorkError};
use std::future::Future;

fn database() -> Database {
    let db = Database::new();
    for id in ["job", "other"] {
        db.store
            .schedule_runtime_job(
                centaeris_core::session::reliability::ScheduleRuntimeJobRequest {
                    job: runtime_job(id, RuntimeJobStatus::Running, Some("owner")),
                },
            )
            .unwrap();
    }
    db
}

async fn finish<T>(pending: Pending<T>) -> driver::Receipt<T> {
    tokio::time::timeout(Duration::from_secs(10), pending)
        .await
        .expect("database work did not finish")
}

async fn close(driver: &Driver) {
    tokio::time::timeout(Duration::from_secs(10), driver.close())
        .await
        .expect("close did not finish")
        .unwrap();
    driver.assert_idle();
}

fn assert_facts(db: &Database, committed: bool) {
    // A new Store/connection reads durable facts only after the worker is done.
    let reopened = SqliteRuntimeStore::new(db.store.db_path()).unwrap();
    assert_eq!(
        reopened.get_runtime_job("job").unwrap().unwrap().status,
        if committed {
            RuntimeJobStatus::Succeeded
        } else {
            RuntimeJobStatus::Running
        }
    );
    assert_eq!(
        reopened
            .load_external_context_object("object")
            .unwrap()
            .is_some(),
        committed
    );
    let conn = Connection::open(db.store.db_path()).unwrap();
    let links: i64 = conn
        .query_row(
            "SELECT count(*) FROM external_context_links WHERE object_id='object'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(links, i64::from(committed));
    assert_eq!(db.terminal_outbox(), i64::from(committed));
}

async fn queued_completion(driver: &Driver, options: Options) -> Pending<()> {
    driver
        .submit(options, |conn, observe| {
            upsert_external_context_link_and_complete_job_conn(
                conn,
                combined_completion(completion("owner", 999)),
                observe,
            )
        })
        .await
        .unwrap()
}

async fn blocked_read(driver: &Driver) -> (Pending<Option<RuntimeJobRecord>>, mpsc::Sender<()>) {
    let (started, ready) = oneshot::channel();
    let (release, wait) = mpsc::channel();
    let pending = driver
        .submit(Options::default(), move |conn, _| {
            started.send(()).unwrap();
            wait.recv_timeout(Duration::from_secs(10)).unwrap();
            crate::sqlite_reliability::load_runtime_job_by_id(conn, "job")
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), ready)
        .await
        .unwrap()
        .unwrap();
    (pending, release)
}

#[tokio::test]
async fn file_driver_reuses_reads_compound_transactions_and_durability_policy() {
    let db = database();
    let driver = Driver::open(db.store.db_path().to_owned(), 2).await;
    let read = driver
        .submit(Options::default(), |conn, _| {
            crate::sqlite_reliability::load_runtime_job_by_id(conn, "job")
        })
        .await
        .unwrap();
    assert_eq!(
        finish(read).await.result.unwrap(),
        db.store.get_runtime_job("job").unwrap()
    );
    assert_eq!(
        finish(queued_completion(&driver, Options::default()).await)
            .await
            .outcome,
        Outcome::Committed
    );
    // The recovery transaction sees FULL; the next call sees the prior NORMAL.
    let reclaim = driver
        .submit(
            Options {
                full_sync: true,
                ..Options::default()
            },
            |conn, observe| {
                assert_eq!(
                    conn.pragma_query_value(None, "synchronous", |r| r.get::<_, i64>(0))
                        .unwrap(),
                    2
                );
                crate::sqlite_reliability::reclaim_expired_runtime_job_leases_conn(
                    conn, 1000, observe,
                )
            },
        )
        .await
        .unwrap();
    let receipt = finish(reclaim).await;
    assert_eq!(receipt.result.unwrap(), 1);
    assert_eq!(receipt.outcome, Outcome::Committed);
    assert!(receipt.healthy);
    let sync = driver
        .submit(Options::default(), |conn, _| {
            conn.pragma_query_value(None, "synchronous", |r| r.get::<_, i64>(0))
                .map_err(|e| e.to_string())
        })
        .await
        .unwrap();
    assert_eq!(finish(sync).await.result.unwrap(), 1);
    close(&driver).await;
    assert_facts(&db, true);
    assert_eq!(
        db.store.get_runtime_job("other").unwrap().unwrap().status,
        RuntimeJobStatus::Queued
    );
}

#[tokio::test]
async fn cancellation_before_admission_leaves_no_work_or_capacity_leak() {
    let db = database();
    let driver = Driver::open(db.store.db_path().to_owned(), 1).await;
    let (first, release) = blocked_read(&driver).await;
    let mut waiting = Box::pin(driver.submit(Options::default(), |conn, observe| {
        upsert_external_context_link_and_complete_job_conn(
            conn,
            combined_completion(completion("owner", 999)),
            observe,
        )
    }));
    std::future::poll_fn(|cx| {
        assert!(waiting.as_mut().poll(cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    drop(waiting);
    release.send(()).unwrap();
    finish(first).await.result.unwrap();
    close(&driver).await;
    assert_facts(&db, false);
}

#[tokio::test]
async fn dropping_a_queued_waiter_does_not_cancel_accepted_write() {
    let db = database();
    let driver = Driver::open(db.store.db_path().to_owned(), 2).await;
    let (first, release) = blocked_read(&driver).await;
    drop(queued_completion(&driver, Options::default()).await);
    release.send(()).unwrap();
    finish(first).await.result.unwrap();
    close(&driver).await;
    assert_facts(&db, true);
}

#[tokio::test]
async fn execution_side_deadline_rejects_queued_write_without_mutation() {
    let db = database();
    let driver = Driver::open(db.store.db_path().to_owned(), 2).await;
    let (first, release) = blocked_read(&driver).await;
    let deadline = Instant::now();
    let clock = Arc::new(Mutex::new(deadline));
    let source = clock.clone();
    let next = queued_completion(
        &driver,
        Options {
            deadline: Some(deadline),
            now: Arc::new(move || *source.lock().unwrap()),
            ..Options::default()
        },
    )
    .await;
    *clock.lock().unwrap() = deadline + Duration::from_nanos(1);
    release.send(()).unwrap();
    finish(first).await.result.unwrap();
    let receipt = finish(next).await;
    assert_eq!(receipt.outcome, Outcome::NotExecuted);
    assert_eq!(receipt.result, Err(WorkError::Expired));
    close(&driver).await;
    assert_facts(&db, false);
}

#[tokio::test]
async fn waiting_timeout_during_execution_keeps_work_owned_until_commit() {
    let db = database();
    let driver = Driver::open(db.store.db_path().to_owned(), 1).await;
    let (started, ready) = oneshot::channel();
    let (release, wait) = mpsc::channel();
    let mut started = Some(started);
    let pending = driver
        .submit(Options::default(), move |conn, observe| {
            upsert_external_context_link_and_complete_job_conn(
                conn,
                combined_completion(completion("owner", 999)),
                &mut |event| {
                    let before = matches!(event, TransactionEvent::BeforeCommit);
                    observe(event);
                    if before {
                        started.take().unwrap().send(()).unwrap();
                        wait.recv_timeout(Duration::from_secs(10)).unwrap();
                    }
                },
            )
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), ready)
        .await
        .unwrap()
        .unwrap();
    assert!(tokio::time::timeout(Duration::ZERO, pending).await.is_err());
    driver.begin_close();
    release.send(()).unwrap();
    close(&driver).await;
    assert_facts(&db, true);
}

#[tokio::test]
async fn committed_write_survives_reply_loss_without_replay() {
    let db = database();
    let driver = Driver::open(db.store.db_path().to_owned(), 1).await;
    let (started, ready) = oneshot::channel();
    let (release, wait) = mpsc::channel();
    let mut started = Some(started);
    let pending = driver
        .submit(Options::default(), move |conn, observe| {
            upsert_external_context_link_and_complete_job_conn(
                conn,
                combined_completion(completion("owner", 999)),
                &mut |event| {
                    let committed = matches!(event, TransactionEvent::Committed);
                    observe(event);
                    if committed {
                        started.take().unwrap().send(()).unwrap();
                        wait.recv_timeout(Duration::from_secs(10)).unwrap();
                    }
                },
            )
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), ready)
        .await
        .unwrap()
        .unwrap();
    drop(pending);
    release.send(()).unwrap();
    close(&driver).await;
    assert_facts(&db, true);
    let generation: i64 = Connection::open(db.store.db_path())
        .unwrap()
        .query_row(
            "SELECT generation FROM runtime_job_outbox WHERE job_id='job'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(generation, 0);
}

#[tokio::test]
#[cfg(panic = "unwind")]
async fn unwinding_panics_preserve_facts_and_retire_queued_work() {
    for after_commit in [false, true] {
        let db = database();
        let driver = Driver::open(db.store.db_path().to_owned(), 2).await;
        let (started, ready) = oneshot::channel();
        let (release, wait) = mpsc::channel();
        let mut started = Some(started);
        let first = driver
            .submit(
                Options {
                    full_sync: true,
                    ..Options::default()
                },
                move |conn, observe| {
                    upsert_external_context_link_and_complete_job_conn(
                        conn,
                        combined_completion(completion("owner", 999)),
                        &mut |event| {
                            let stop = if after_commit {
                                matches!(event, TransactionEvent::Committed)
                            } else {
                                matches!(event, TransactionEvent::BeforeCommit)
                            };
                            observe(event);
                            if stop {
                                started.take().unwrap().send(()).unwrap();
                                wait.recv_timeout(Duration::from_secs(10)).unwrap();
                                panic!("injected database panic");
                            }
                        },
                    )
                },
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), ready)
            .await
            .unwrap()
            .unwrap();
        let queued = driver
            .submit(Options::default(), |conn, _| {
                crate::sqlite_reliability::load_runtime_job_by_id(conn, "other")
            })
            .await
            .unwrap();
        release.send(()).unwrap();
        let receipt = finish(first).await;
        assert_eq!(receipt.result, Err(WorkError::Panicked));
        assert!(!receipt.healthy);
        assert_eq!(receipt.sync_after, Some(1));
        assert_eq!(
            receipt.outcome,
            if after_commit {
                Outcome::Committed
            } else {
                Outcome::RolledBack
            }
        );
        let rejected = finish(queued).await;
        assert_eq!(rejected.outcome, Outcome::NotExecuted);
        assert_eq!(rejected.result, Err(WorkError::Unusable));
        close(&driver).await;
        assert_facts(&db, after_commit);
    }
}

#[tokio::test]
async fn restoration_failure_preserves_commit_or_rollback_and_rejects_queued_work() {
    use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
    for (commit, cleanup_panic) in [(false, false), (true, false), (false, true), (true, true)] {
        let db = database();
        let driver = Driver::open(db.store.db_path().to_owned(), 2).await;
        let (started, ready) = oneshot::channel();
        let (release, wait) = mpsc::channel();
        let mut started = Some(started);
        let first = driver
            .submit(
                Options {
                    full_sync: true,
                    before_restore: if cleanup_panic {
                        Some(Box::new(|| panic!("injected restoration panic")))
                    } else {
                        None
                    },
                    ..Options::default()
                },
                move |conn, observe| {
                    // Deny actual restoration while allowing promotion to FULL and reads.
                    conn.authorizer(Some(|ctx: AuthContext<'_>| match ctx.action {
                        AuthAction::Pragma {
                            pragma_name: "synchronous",
                            pragma_value: Some("1"),
                        } => Authorization::Deny,
                        _ => Authorization::Allow,
                    }));
                    let owner = if commit { "owner" } else { "other" };
                    upsert_external_context_link_and_complete_job_conn(
                        conn,
                        combined_completion(completion(owner, 999)),
                        &mut |event| {
                            let settled = matches!(
                                event,
                                TransactionEvent::Committed | TransactionEvent::RolledBack
                            );
                            observe(event);
                            if settled {
                                started.take().unwrap().send(()).unwrap();
                                wait.recv_timeout(Duration::from_secs(10)).unwrap();
                            }
                        },
                    )
                },
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), ready)
            .await
            .unwrap()
            .unwrap();
        let queued = driver
            .submit(Options::default(), |conn, _| {
                crate::sqlite_reliability::load_runtime_job_by_id(conn, "other")
            })
            .await
            .unwrap();
        release.send(()).unwrap();
        let receipt = finish(first).await;
        assert_eq!(
            receipt.outcome,
            if commit {
                Outcome::Committed
            } else {
                Outcome::RolledBack
            }
        );
        assert_eq!(receipt.result.is_ok(), commit);
        assert!(!receipt.healthy);
        assert!(receipt.cleanup_error.is_some());
        if cleanup_panic {
            assert_eq!(
                receipt.cleanup_error.as_deref(),
                Some("connection cleanup panicked")
            );
        }
        assert_eq!(finish(queued).await.outcome, Outcome::NotExecuted);
        close(&driver).await;
        assert_facts(&db, commit);
    }
}

#[tokio::test]
async fn transaction_start_commit_and_rollback_failures_have_distinct_receipts() {
    use rusqlite::hooks::{AuthAction, AuthContext, Authorization, TransactionOperation};
    for fault in ["begin", "commit", "rollback"] {
        let db = database();
        let driver = Driver::open(db.store.db_path().to_owned(), 1).await;
        let receipt = finish(
            driver
                .submit(
                    Options {
                        full_sync: true,
                        ..Options::default()
                    },
                    move |conn, observe| {
                        match fault {
                            "begin" => conn.execute_batch("BEGIN").unwrap(),
                            "commit" => conn.commit_hook(Some(|| true)),
                            "rollback" => {
                                conn.authorizer(Some(|ctx: AuthContext<'_>| match ctx.action {
                                    AuthAction::Transaction {
                                        operation: TransactionOperation::Rollback,
                                    } => Authorization::Deny,
                                    _ => Authorization::Allow,
                                }))
                            }
                            _ => unreachable!(),
                        }
                        let owner = if fault == "rollback" {
                            "other"
                        } else {
                            "owner"
                        };
                        upsert_external_context_link_and_complete_job_conn(
                            conn,
                            combined_completion(completion(owner, 999)),
                            observe,
                        )
                    },
                )
                .await
                .unwrap(),
        )
        .await;
        assert!(receipt.result.is_err());
        assert!(!receipt.healthy);
        if fault != "rollback" {
            assert_eq!(receipt.sync_after, Some(1));
        }
        assert_eq!(
            receipt.outcome,
            if fault == "begin" {
                Outcome::NotExecuted
            } else {
                Outcome::Unknown
            }
        );
        close(&driver).await;
        assert_facts(&db, false);
    }
}

#[tokio::test]
async fn recovery_restores_prior_policy_after_success_and_business_error() {
    for fail in [false, true] {
        let db = database();
        let driver = Driver::open(db.store.db_path().to_owned(), 1).await;
        if fail {
            Connection::open(db.store.db_path()).unwrap().execute_batch("CREATE TRIGGER reject_reclaim_test BEFORE UPDATE ON runtime_jobs BEGIN SELECT RAISE(FAIL,'injected reclaim failure'); END;").unwrap();
        }
        let receipt = finish(
            driver
                .submit(
                    Options {
                        full_sync: true,
                        ..Options::default()
                    },
                    move |conn, observe| {
                        crate::sqlite_reliability::reclaim_expired_runtime_job_leases_conn(
                            conn, 1000, observe,
                        )
                    },
                )
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(receipt.result.is_err(), fail);
        assert!(receipt.healthy);
        assert_eq!(receipt.sync_after, Some(1));
        assert_eq!(
            receipt.outcome,
            if fail {
                Outcome::RolledBack
            } else {
                Outcome::Committed
            }
        );
        let sync = driver
            .submit(Options::default(), |conn, _| {
                conn.pragma_query_value(None, "synchronous", |r| r.get::<_, i64>(0))
                    .map_err(|e| e.to_string())
            })
            .await
            .unwrap();
        assert_eq!(finish(sync).await.result.unwrap(), 1);
        close(&driver).await;
        let expected = if fail {
            RuntimeJobStatus::Running
        } else {
            RuntimeJobStatus::Queued
        };
        assert_eq!(
            db.store.get_runtime_job("job").unwrap().unwrap().status,
            expected
        );
    }
}

#[tokio::test]
async fn recovery_commit_failure_and_panic_restore_policy_and_retire_connection() {
    for panic in [false, true] {
        let db = database();
        let driver = Driver::open(db.store.db_path().to_owned(), 1).await;
        let receipt = finish(
            driver
                .submit(
                    Options {
                        full_sync: true,
                        ..Options::default()
                    },
                    move |conn, observe| {
                        if !panic {
                            conn.commit_hook(Some(|| true));
                        }
                        crate::sqlite_reliability::reclaim_expired_runtime_job_leases_conn(
                            conn,
                            1000,
                            &mut |event| {
                                let before_commit = matches!(event, TransactionEvent::BeforeCommit);
                                observe(event);
                                if panic && before_commit {
                                    panic!("injected recovery panic");
                                }
                            },
                        )
                    },
                )
                .await
                .unwrap(),
        )
        .await;
        assert!(receipt.result.is_err());
        assert_eq!(
            receipt.outcome,
            if panic {
                Outcome::RolledBack
            } else {
                Outcome::Unknown
            }
        );
        assert_eq!(receipt.sync_after, Some(1));
        assert!(!receipt.healthy);
        close(&driver).await;
        for id in ["job", "other"] {
            assert_eq!(
                db.store.get_runtime_job(id).unwrap().unwrap().status,
                RuntimeJobStatus::Running
            );
        }
        assert_eq!(db.terminal_outbox(), 0);
        let events: i64 = Connection::open(db.store.db_path())
            .unwrap()
            .query_row("SELECT count(*) FROM runtime_events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(events, 0);
    }
}

#[tokio::test]
async fn closing_stops_admission_drains_accepted_work_and_outlives_waiters() {
    let db = database();
    let driver = Driver::open(db.store.db_path().to_owned(), 2).await;
    let (first, release) = blocked_read(&driver).await;
    let next = queued_completion(&driver, Options::default()).await;
    driver.begin_close();
    let rejected = driver
        .submit(Options::default(), |conn, _| {
            crate::sqlite_reliability::load_runtime_job_by_id(conn, "other")
        })
        .await;
    assert!(matches!(rejected, Err(WorkError::AdmissionStopped)));
    assert!(tokio::time::timeout(Duration::ZERO, driver.close())
        .await
        .is_err());
    let waiting_driver = driver.clone();
    let waiter = tokio::spawn(async move { waiting_driver.close().await });
    tokio::task::yield_now().await;
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    release.send(()).unwrap();
    finish(first).await.result.unwrap();
    finish(next).await.result.unwrap();
    close(&driver).await;
    assert_facts(&db, true);
}

#[test]
fn abrupt_process_exit_reopens_without_partial_transaction() {
    if let Ok(path) = std::env::var("CENTAERIS_SQLITE_ABORT_TEST_FILE") {
        // Explicit process-abort mode: no claim that unwind cleanup ran.
        std::panic::set_hook(Box::new(|_| std::process::abort()));
        let mut conn = Connection::open(path).unwrap();
        crate::configure_conn(&conn).unwrap();
        let after_commit = std::env::var("CENTAERIS_SQLITE_ABORT_AFTER_COMMIT").unwrap() == "1";
        upsert_external_context_link_and_complete_job_conn(
            &mut conn,
            combined_completion(completion("owner", 999)),
            &mut |event| {
                if if after_commit {
                    matches!(event, TransactionEvent::Committed)
                } else {
                    matches!(event, TransactionEvent::BeforeCommit)
                } {
                    use std::io::Write;
                    println!("database abort boundary reached");
                    std::io::stdout().flush().unwrap();
                    panic!("aborting database child");
                }
            },
        )
        .unwrap();
        panic!("abort point was not reached");
    }
    for after_commit in [false, true] {
        let db = database();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--nocapture",
                "--exact",
                "tests::async_connection::abrupt_process_exit_reopens_without_partial_transaction",
            ])
            .env("CENTAERIS_SQLITE_ABORT_TEST_FILE", db.store.db_path())
            .env(
                "CENTAERIS_SQLITE_ABORT_AFTER_COMMIT",
                if after_commit { "1" } else { "0" },
            )
            .output()
            .unwrap();
        assert!(!status.status.success());
        assert!(String::from_utf8_lossy(&status.stdout).contains("database abort boundary reached"));
        assert_facts(&db, after_commit);
    }
}
