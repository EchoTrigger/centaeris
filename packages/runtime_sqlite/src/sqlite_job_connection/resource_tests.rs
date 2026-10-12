use super::*;
use centaeris_core::runtime::contracts::{EventVisibility, RuntimeEvent};
use centaeris_core::session::store::actor::RuntimeStoreActor;
use std::sync::mpsc;
use std::time::Duration;

async fn until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !condition() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resource_metrics_retain_actual_work_after_reply_drop_and_timeout() {
    let directory = tempfile::tempdir().unwrap();
    let driver = Driver::open(directory.path().join("runtime.db"), 129)
        .await
        .unwrap();
    let (started, start) = mpsc::channel();
    let (release, blocked) = mpsc::channel();
    let first = driver
        .submit(
            Options {
                read_only: true,
                ..Default::default()
            },
            move |_, _| {
                started.send(()).unwrap();
                blocked.recv().unwrap();
                Ok(())
            },
        )
        .await
        .unwrap();
    start.recv_timeout(Duration::from_secs(5)).unwrap();
    drop(first);
    let mut waiters = Vec::new();
    for _ in 0..128 {
        waiters.push(
            driver
                .submit(
                    Options {
                        read_only: true,
                        ..Default::default()
                    },
                    |_, _| Ok(()),
                )
                .await
                .unwrap(),
        );
    }
    assert_eq!(driver.metrics().in_flight, 129);
    assert_eq!(driver.metrics().executing, 1);
    let mut extra = tokio::spawn({
        let driver = driver.clone();
        async move {
            driver
                .submit(
                    Options {
                        read_only: true,
                        ..Default::default()
                    },
                    |_, _| Ok(()),
                )
                .await
        }
    });
    tokio::task::yield_now().await;
    assert_eq!(driver.metrics().in_flight, 129);
    assert!(!extra.is_finished());
    assert!(
        tokio::time::timeout(Duration::from_millis(20), waiters.pop().unwrap())
            .await
            .is_err()
    );
    assert_eq!(driver.metrics().in_flight, 129);
    assert!(tokio::time::timeout(Duration::from_millis(20), &mut extra)
        .await
        .is_err());
    release.send(()).unwrap();
    extra.await.unwrap().unwrap().await.result.unwrap();
    drop(waiters);
    until(|| driver.metrics().in_flight == 0).await;
    let metrics = driver.metrics();
    assert_eq!(metrics.executing, 0);
    assert_eq!(metrics.started, 130);
    assert!(metrics.admission_wait_ns >= 20_000_000);
    assert!(metrics.queue_wait_ns >= 20_000_000);
    println!("native: {metrics:?}");
    driver.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resource_metrics_show_independent_actor_and_native_order_on_one_database() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("runtime.db");
    let store = crate::SqliteRuntimeStore::new(&path).unwrap();
    let actor = RuntimeStoreActor::start(store).unwrap();
    let driver = Driver::open(path.clone(), 129).await.unwrap();
    let lock = rusqlite::Connection::open(path).unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    let (started, start) = mpsc::channel();
    let (release, blocked) = mpsc::channel();
    let pending = driver
        .submit(
            Options {
                read_only: true,
                ..Default::default()
            },
            move |_, _| {
                started.send(()).unwrap();
                blocked.recv().unwrap();
                Ok(())
            },
        )
        .await
        .unwrap();
    start.recv_timeout(Duration::from_secs(5)).unwrap();
    let writing = tokio::spawn({
        let actor = actor.clone();
        async move {
            actor
                .append_event(RuntimeEvent {
                    event_id: "event".into(),
                    session_id: "parent".into(),
                    task_id: None,
                    event_type: "test".into(),
                    at_ms: 10,
                    visibility: EventVisibility::Internal,
                    payload_json: "{}".into(),
                })
                .await
        }
    });
    until(|| actor.resource_metrics().executing == 1).await;
    let mut reading = Vec::new();
    for _ in 0..128 {
        reading.push(tokio::spawn({
            let actor = actor.clone();
            async move { actor.load_latest_checkpoint("parent").await }
        }));
    }
    until(|| actor.resource_metrics().queued == 128).await;
    let mut native_reads = Vec::new();
    for _ in 0..128 {
        native_reads.push(
            driver
                .submit(
                    Options {
                        read_only: true,
                        ..Default::default()
                    },
                    |conn, _| {
                        conn.query_row("SELECT COUNT(*) FROM runtime_events", [], |row| {
                            row.get::<_, i64>(0)
                        })
                        .map_err(|e| e.to_string())
                    },
                )
                .await
                .unwrap(),
        );
    }
    // Drop both reply waiters without releasing either path's real work.
    drop(pending);
    assert_eq!(
        actor.resource_metrics().queued + actor.resource_metrics().executing,
        129
    );
    assert_eq!(driver.metrics().in_flight, 129);
    assert_eq!(
        actor.resource_metrics().queued
            + actor.resource_metrics().executing
            + driver.metrics().in_flight,
        258
    );
    println!(
        "shared database peak: actor={:?}; native={:?}; combined=258",
        actor.resource_metrics(),
        driver.metrics()
    );
    release.send(()).unwrap();
    for queued in native_reads {
        assert_eq!(queued.await.result.unwrap(), 0);
    }
    assert_eq!(actor.resource_metrics().executing, 1);
    assert_eq!(actor.resource_metrics().queued, 128);
    lock.execute_batch("ROLLBACK").unwrap();
    writing.await.unwrap().unwrap();
    for read in reading {
        assert!(read.await.unwrap().unwrap().is_none());
    }
    let visible = driver
        .submit(
            Options {
                read_only: true,
                ..Default::default()
            },
            |conn, _| {
                conn.query_row("SELECT COUNT(*) FROM runtime_events", [], |row| {
                    row.get::<_, i64>(0)
                })
                .map_err(|e| e.to_string())
            },
        )
        .await
        .unwrap()
        .await
        .result
        .unwrap();
    assert_eq!(visible, 1);
    println!(
        "actor: {:?}; native: {:?}",
        actor.resource_metrics(),
        driver.metrics()
    );
    assert!(actor.resource_metrics().wait_ns > 0);
    driver.close().await.unwrap();
}

#[tokio::test]
async fn resource_metrics_count_typed_transaction_lock_conflicts_not_error_text() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("runtime.db");
    let driver = Driver::open(path.clone(), 129).await.unwrap();
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch("BEGIN IMMEDIATE").unwrap();
    // Set only this connection's busy timeout so the real conflict completes promptly.
    driver
        .submit(
            Options {
                read_only: true,
                ..Default::default()
            },
            |conn, _| conn.busy_timeout(Duration::ZERO).map_err(|e| e.to_string()),
        )
        .await
        .unwrap()
        .await
        .result
        .unwrap();
    let receipt = driver
        .submit(Options::default(), |conn, observe| {
            crate::sqlite_reliability::reclaim_expired_runtime_job_leases_conn(conn, 10, observe)
        })
        .await
        .unwrap()
        .await;
    assert!(receipt.result.is_err());
    assert_eq!(receipt.outcome, Outcome::NotExecuted);
    assert_eq!(driver.metrics().transaction_lock_conflicts, 1);
    conn.execute_batch("ROLLBACK").unwrap();
    driver
        .submit(
            Options {
                read_only: true,
                ..Default::default()
            },
            |_, _| Err::<(), _>("database is locked".into()),
        )
        .await
        .unwrap()
        .await
        .result
        .unwrap_err();
    assert_eq!(driver.metrics().transaction_lock_conflicts, 1);
    println!("typed conflicts: {:?}", driver.metrics());
    driver.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resource_metrics_actor_close_drains_real_work_after_close_waiter_drop() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("runtime.db");
    let actor = RuntimeStoreActor::start(crate::SqliteRuntimeStore::new(&path).unwrap()).unwrap();
    let cached = actor.clone();
    let lock = rusqlite::Connection::open(path).unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    let writing = tokio::spawn({
        let actor = actor.clone();
        async move {
            actor
                .append_event(RuntimeEvent {
                    event_id: "close-event".into(),
                    session_id: "parent".into(),
                    task_id: None,
                    event_type: "test".into(),
                    at_ms: 10,
                    visibility: EventVisibility::Internal,
                    payload_json: "{}".into(),
                })
                .await
        }
    });
    until(|| actor.resource_metrics().executing == 1).await;
    let reading = tokio::spawn({
        let actor = actor.clone();
        async move { actor.load_latest_checkpoint("parent").await }
    });
    until(|| actor.resource_metrics().queued == 1).await;
    let accepted_write = tokio::spawn({
        let actor = actor.clone();
        async move {
            actor
                .append_event(RuntimeEvent {
                    event_id: "queued-close-event".into(),
                    session_id: "parent".into(),
                    task_id: None,
                    event_type: "test".into(),
                    at_ms: 10,
                    visibility: EventVisibility::Internal,
                    payload_json: "{}".into(),
                })
                .await
        }
    });
    until(|| actor.resource_metrics().queued == 2).await;
    let mut closing = tokio::spawn({
        let actor = actor.clone();
        async move { actor.close().await }
    });
    until(|| !actor.resource_metrics().accepting).await;
    // Close must not wait for the blocked write before stopping cached senders.
    assert!(tokio::time::timeout(
        Duration::from_secs(1),
        cached.load_latest_checkpoint("rejected")
    )
    .await
    .unwrap()
    .is_err());
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut closing)
            .await
            .is_err()
    );
    closing.abort();
    assert_eq!(actor.resource_metrics().executing, 1);
    let mut second_close = tokio::spawn({
        let actor = actor.clone();
        async move { actor.close().await }
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut second_close)
            .await
            .is_err()
    );
    lock.execute_batch("ROLLBACK").unwrap();
    writing.await.unwrap().unwrap();
    assert!(reading.await.unwrap().unwrap().is_none());
    second_close.await.unwrap().unwrap();
    accepted_write.await.unwrap().unwrap();
    assert_eq!(actor.resource_metrics().executing, 0);
    assert_eq!(actor.resource_metrics().queued, 0);
    assert!(cached.load_latest_checkpoint("rejected").await.is_err());
    assert!(cached
        .append_event(RuntimeEvent {
            event_id: "rejected-close-event".into(),
            session_id: "parent".into(),
            task_id: None,
            event_type: "test".into(),
            at_ms: 10,
            visibility: EventVisibility::Internal,
            payload_json: "{}".into()
        })
        .await
        .is_err());
    assert_eq!(
        lock.query_row("SELECT COUNT(*) FROM runtime_events", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        2
    );
}

#[tokio::test]
async fn resource_metrics_lost_driver_reply_preserves_declared_read_only_outcome() {
    for read_only in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("runtime.db");
        let driver = Driver::open(path.clone(), 2).await.unwrap();
        let independent = rusqlite::Connection::open(path).unwrap();
        independent.execute_batch("CREATE TABLE reply_probe(value TEXT NOT NULL); INSERT INTO reply_probe VALUES('baseline')").unwrap();
        // The real library handle, rather than a fabricated reply, loses its executor.
        driver.connection.clone().close().await.unwrap();
        let receipt = driver
            .submit(
                Options {
                    read_only,
                    ..Default::default()
                },
                move |conn, observe| {
                    if read_only {
                        conn.query_row("SELECT COUNT(*) FROM reply_probe", [], |row| {
                            row.get::<_, i64>(0)
                        })
                        .map_err(|error| error.to_string())
                    } else {
                        let tx = conn.transaction().map_err(|error| error.to_string())?;
                        crate::sqlite_transactions::commit_job_write(
                            tx,
                            "commit reply probe failed",
                            observe,
                            |conn| {
                                conn.execute(
                                    "INSERT INTO reply_probe VALUES('unexpected write')",
                                    [],
                                )
                                .map_err(|error| error.to_string())?;
                                Ok(2)
                            },
                        )
                    }
                },
            )
            .await
            .unwrap()
            .await;
        assert!(matches!(
            receipt.result,
            Err(WorkError::ReplyUnavailable(_))
        ));
        assert!(!receipt.healthy);
        assert!(
            !driver.is_healthy(),
            "lost reply must retire the connection for reads and writes"
        );
        driver.assert_idle();
        assert_eq!(
            independent
                .query_row("SELECT COUNT(*) FROM reply_probe", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1,
            "failed transport must not silently execute a write"
        );
        assert_eq!(receipt.outcome, if read_only { Outcome::ReadOnly } else { Outcome::Unknown },
            "transport failure preserves declared read capability without guessing from transaction events");
    }
}
