use super::cold_append_fault_tests::lease;
use super::cold_append_tests::{control_record, fixture, RUN};
use super::*;
use std::sync::mpsc;

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_cold_append_rechecks_owner_and_expiry_after_history_before_commit() {
    let _guard = TEST_LOCK.lock().unwrap();
    for change in ["owner", "expiry"] {
        let fixture = fixture(100);
        let fence = lease(&fixture);
        let cold = fixture.fresh_log();
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        cold.set_cold_append_barrier(move |_| {
            entered_tx.send(()).map_err(|error| error.to_string())?;
            release_rx
                .recv_timeout(Duration::from_secs(15))
                .map_err(|error| error.to_string())
        });
        let append_log = cold.clone();
        let append_fence = fence.clone();
        let event = control_record(fixture.next_sequence);
        let append_event = event.clone();
        let append = std::thread::spawn(move || {
            append_log.append_session_records_with_runtime_job_lease_blocking(
                RUN,
                &[append_event],
                &append_fence,
            )
        });
        entered_rx.recv_timeout(Duration::from_secs(15)).unwrap();
        let mut db = Client::connect(&test_url(), NoTls).unwrap();
        db.batch_execute("SET lock_timeout='1000ms'").unwrap();
        let changed = if change == "owner" {
            db.execute(
                "UPDATE runtime.runtime_jobs SET lease_owner='replacement-owner' WHERE job_id=$1",
                &[&fence.job_id],
            )
        } else {
            db.execute("UPDATE runtime.runtime_jobs SET lease_expires_at_ms=(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint-1 WHERE job_id=$1", &[&fence.job_id])
        };
        release_tx.send(()).unwrap();
        let result = append.join().unwrap();
        assert_eq!(
            changed.unwrap(),
            1,
            "history blocked authority change {change}"
        );
        assert_eq!(result.unwrap_err(), RUNTIME_JOB_LEASE_FENCE_REJECTED);
        assert!(!cold.has_append_cache(RUN));
        assert_eq!(db.query_one("SELECT COUNT(*) FROM public.app_core_sessionevent WHERE agent_run_id=$1 AND agent_run_sequence=$2", &[&RUN, &(fixture.next_sequence as i32)]).unwrap().get::<_,i64>(0), 0);
        // Only restore this fixture's exact authority to verify a failed cold
        // attempt did not advance or poison its append cache.
        db.execute("UPDATE runtime.runtime_jobs SET lease_owner=$2,lease_expires_at_ms=(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint+60000 WHERE job_id=$1", &[&fence.job_id, &fence.lease_owner]).unwrap();
        let receipt = cold
            .append_session_records_with_runtime_job_lease_blocking(
                RUN,
                std::slice::from_ref(&event),
                &fence,
            )
            .unwrap();
        assert_eq!(receipt.records[0].event, event.event);
        assert_eq!(cold.cold_append_observation().loads, 2);
    }
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_append_waiting_for_job_lock_rechecks_database_time_after_lock() {
    let _guard = TEST_LOCK.lock().unwrap();
    let fixture = fixture(1);
    let fence = lease(&fixture);
    let cold = fixture.fresh_log();
    let (entered_tx, entered_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    cold.set_cold_append_barrier(move |pid| {
        entered_tx.send(pid).map_err(|error| error.to_string())?;
        release_rx
            .recv_timeout(Duration::from_secs(15))
            .map_err(|error| error.to_string())
    });
    let append_log = cold.clone();
    let append_fence = fence.clone();
    let event = control_record(fixture.next_sequence);
    let append = std::thread::spawn(move || {
        append_log.append_session_records_with_runtime_job_lease_blocking(
            RUN,
            &[event],
            &append_fence,
        )
    });
    let append_pid = entered_rx.recv_timeout(Duration::from_secs(15)).unwrap();
    let mut db = Client::connect(&test_url(), NoTls).unwrap();
    db.batch_execute("SET lock_timeout='1000ms'").unwrap();
    // Expiry is observed in DB time; no host clock or fixed sleep defines it.
    let shortened = db.execute("UPDATE runtime.runtime_jobs SET lease_expires_at_ms=(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint+2000 WHERE job_id=$1", &[&fence.job_id]);
    if let Err(error) = shortened {
        release_tx.send(()).unwrap();
        let _ = append.join().unwrap();
        panic!("history unexpectedly locked the job before final fence: {error}");
    }
    let mut blocker = Client::connect(&test_url(), NoTls).unwrap();
    let blocker_pid: i32 = blocker
        .query_one("SELECT pg_backend_pid()", &[])
        .unwrap()
        .get(0);
    let mut hold = blocker.transaction().unwrap();
    hold.query_one(
        "SELECT job_id FROM runtime.runtime_jobs WHERE job_id=$1 FOR UPDATE",
        &[&fence.job_id],
    )
    .unwrap();
    release_tx.send(()).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut observed_wait = false;
    while std::time::Instant::now() < deadline {
        let blocked: bool = db
            .query_one(
                "SELECT $2=ANY(pg_blocking_pids($1))",
                &[&append_pid, &blocker_pid],
            )
            .unwrap()
            .get(0);
        if blocked {
            observed_wait = true;
            break;
        }
        std::thread::yield_now();
    }
    let mut expired = false;
    while std::time::Instant::now() < deadline {
        expired = db.query_one("SELECT lease_expires_at_ms<=(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint FROM runtime.runtime_jobs WHERE job_id=$1", &[&fence.job_id]).unwrap().get(0);
        if expired {
            break;
        }
        std::thread::yield_now();
    }
    // Do not update the locked tuple: a time predicate evaluated before waiting
    // must not remain permission after a pure row-lock wait.
    hold.commit().unwrap();
    let result = append.join().unwrap();
    assert!(
        observed_wait,
        "append never reached the final job lock before expiry"
    );
    assert!(expired, "DB expiry was not reached while holding the lock");
    assert_eq!(result.unwrap_err(), RUNTIME_JOB_LEASE_FENCE_REJECTED);
    assert!(!cold.has_append_cache(RUN));
    assert_eq!(db.query_one("SELECT COUNT(*) FROM public.app_core_sessionevent WHERE agent_run_id=$1 AND agent_run_sequence=$2", &[&RUN, &(fixture.next_sequence as i32)]).unwrap().get::<_,i64>(0), 0);
}
