use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

// Actual retained authority + association pinning, with an isolated PG row
// transaction simulating the short exclusive section of same-owner renewal.
// No Docker work is performed by this database regression.
fn query_during_renewal(change: &str) {
    let (url, store, mut db) = setup();
    let current = run(&store, &mut db, "run-renewal-query", 1);
    let physical = Arc::new(Mutex::new(Vec::new()));
    let allocation = prepare(&store, &current, "execution-renewal-query", &physical).unwrap();
    let job = current.fence.job_id.clone();
    let expected_instance = allocation.instance_id.clone();
    let expected_container = allocation.container_id.clone();
    let calls = Arc::new(AtomicUsize::new(0));
    let query_pid: i32 = store
        .with_client(|client| {
            client
                .query_one("SELECT pg_backend_pid()", &[])
                .map(|row| row.get(0))
                .map_err(|error| error.to_string())
        })
        .unwrap();
    let blocker_pid: i32 = db.query_one("SELECT pg_backend_pid()", &[]).unwrap().get(0);
    let mut observer = Client::connect(&url, NoTls).unwrap();
    if change == "expiry" {
        db.execute("UPDATE runtime.runtime_jobs SET lease_expires_at_ms=(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint+2000 WHERE job_id=$1", &[&job]).unwrap();
    }
    let mut hold = db.transaction().unwrap();
    hold.query_one(
        "SELECT job_id FROM runtime.runtime_jobs WHERE job_id=$1 FOR UPDATE",
        &[&job],
    )
    .unwrap();
    let query_store = store.clone();
    let host_calls = calls.clone();
    let (result_tx, result_rx) = mpsc::sync_channel(1);
    let query = std::thread::spawn(move || {
        let profile = format!("sha256:{}", "b".repeat(64));
        let result = query_store.with_retained_instance_operation(
            &current.request("execution-renewal-query", &profile),
            &allocation,
            |actual| {
                host_calls.fetch_add(1, Ordering::SeqCst);
                assert_eq!(actual.instance_id, expected_instance);
                assert_eq!(actual.container_id, expected_container);
                Ok(actual.instance_id.clone())
            },
        );
        result_tx.send(result.clone()).unwrap();
        result
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut waited = false;
    let mut early = false;
    while Instant::now() < deadline {
        if result_rx.try_recv().is_ok() {
            early = true;
            break;
        }
        waited = observer
            .query_one(
                "SELECT $2=ANY(pg_blocking_pids($1))",
                &[&query_pid, &blocker_pid],
            )
            .unwrap()
            .get(0);
        if waited {
            break;
        }
        std::thread::yield_now();
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "host closure ran while authority row was exclusively locked"
    );
    let mut expired = false;
    if waited {
        match change {
            "renewed" => {
                hold.execute("UPDATE runtime.runtime_jobs SET heartbeat_at_ms=(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint,lease_expires_at_ms=(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint+60000 WHERE job_id=$1", &[&job]).unwrap();
            }
            "reclaimed" => {
                hold.execute("UPDATE runtime.runtime_jobs SET lease_owner='replacement-owner' WHERE job_id=$1", &[&job]).unwrap();
            }
            "expiry" => {
                // Do not update the tuple while holding it: permission must be
                // checked against database time after a pure lock wait.
                while Instant::now() < deadline {
                    expired=observer.query_one("SELECT lease_expires_at_ms<=(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint FROM runtime.runtime_jobs WHERE job_id=$1", &[&job]).unwrap().get(0);
                    if expired {
                        break;
                    }
                    std::thread::yield_now();
                }
            }
            _ => unreachable!(),
        }
    }
    hold.commit().unwrap();
    let result = query.join().unwrap();
    println!("retainedQueryAuthority case={change} waitedForRenewal={waited} returnedBeforeRenewalCommit={early} hostCalls={}",calls.load(Ordering::SeqCst));
    assert!(
        waited,
        "retained Query rejected before a legitimate renewal lock could finish: {result:?}"
    );
    if change == "renewed" {
        result.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    } else {
        if change == "expiry" {
            assert!(
                expired,
                "database expiry was not reached under the controlled row lock"
            );
        }
        assert_eq!(result.unwrap_err(), "retained job fence rejected");
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_retained_query_waits_for_same_owner_renewal_before_host_operation() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    query_during_renewal("renewed");
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_retained_query_rejects_expiry_after_renewal_row_lock_wait() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    query_during_renewal("expiry");
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_retained_query_rejects_reclaimed_owner_after_renewal_row_lock_wait() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    query_during_renewal("reclaimed");
}
#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_retained_query_exhausted_lease_lock_budget_releases_upstream_pins() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (url, store, mut db) = setup();
    let current = run(&store, &mut db, "run-renewal-budget", 1);
    let physical = Arc::new(Mutex::new(Vec::new()));
    let allocation = prepare(&store, &current, "execution-renewal-budget", &physical).unwrap();
    let job = current.fence.job_id.clone();
    let query_pid: i32 = store
        .with_client(|client| {
            client
                .query_one("SELECT pg_backend_pid()", &[])
                .map(|row| row.get(0))
                .map_err(|error| error.to_string())
        })
        .unwrap();
    let blocker_pid: i32 = db.query_one("SELECT pg_backend_pid()", &[]).unwrap().get(0);
    let mut observer = Client::connect(&url, NoTls).unwrap();
    let mut hold = db.transaction().unwrap();
    hold.query_one(
        "SELECT job_id FROM runtime.runtime_jobs WHERE job_id=$1 FOR UPDATE",
        &[&job],
    )
    .unwrap();
    let host_calls = Arc::new(AtomicUsize::new(0));
    let calls = host_calls.clone();
    let query_store = store.clone();
    let (result_tx, result_rx) = mpsc::sync_channel(1);
    let query = std::thread::spawn(move || {
        // A smaller fixture request budget, never an increased production limit.
        let _deadline = crate::postgres_store::RequestDeadline::enter(Some(
            Instant::now() + Duration::from_millis(800),
        ));
        let profile = format!("sha256:{}", "b".repeat(64));
        let result = query_store.with_retained_instance_operation(
            &current.request("execution-renewal-budget", &profile),
            &allocation,
            |_| {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
        );
        result_tx.send(result.clone()).unwrap();
        result
    });
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut waited = false;
    let mut early = None;
    while Instant::now() < deadline {
        if let Ok(result) = result_rx.try_recv() {
            early = Some(result);
            break;
        }
        waited = observer
            .query_one(
                "SELECT $2=ANY(pg_blocking_pids($1))",
                &[&query_pid, &blocker_pid],
            )
            .unwrap()
            .get(0);
        if waited {
            break;
        }
        std::thread::yield_now();
    }
    let completed = early.or_else(|| result_rx.recv_timeout(Duration::from_secs(2)).ok());
    let mut upstream_released = false;
    if matches!(&completed, Some(Err(_))) {
        // Renewal still owns the job row. An exhausted Query must already have
        // rolled back all upstream pins, so an inverse writer can make progress.
        let proof=observer.batch_execute("BEGIN; SELECT id FROM public.app_core_workspace WHERE id='ws_1' FOR UPDATE NOWAIT; SELECT id FROM public.app_core_workspacemembership WHERE id='member' FOR UPDATE NOWAIT; SELECT id FROM public.app_core_session WHERE id='sess_1' FOR UPDATE NOWAIT; SELECT id FROM public.app_core_sessionenvironment WHERE id='environment' FOR UPDATE NOWAIT; SELECT id FROM public.app_core_agentrun WHERE id='run-renewal-budget' FOR UPDATE NOWAIT; SELECT agent_run_id FROM public.app_core_agentrunauthorization WHERE agent_run_id='run-renewal-budget' FOR UPDATE NOWAIT; COMMIT;");
        upstream_released = proof.is_ok();
        if proof.is_err() {
            observer.batch_execute("ROLLBACK").unwrap();
        }
    }
    hold.commit().unwrap();
    let result = query.join().unwrap();
    assert!(waited, "Query did not enter the bounded row-lock wait");
    assert!(
        matches!(completed, Some(Err(_))),
        "Query did not return an error within its request budget"
    );
    assert!(result.is_err());
    assert_eq!(host_calls.load(Ordering::SeqCst), 0);
    assert!(
        upstream_released,
        "budget failure retained upstream authority pins after rollback"
    );
}
