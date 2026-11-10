use super::cold_append_fault_tests::lease;
use super::cold_append_tests::{control_record, fixture, RUN};
use super::*;
use std::sync::mpsc;

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_cold_append_history_barrier_does_not_block_lease_renewal() {
    let _guard = TEST_LOCK.lock().unwrap();
    let fixture = fixture(100);
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
    let renew_store = fixture.store.clone();
    let (renewed_tx, renewed_rx) = mpsc::sync_channel(1);
    let renew = std::thread::spawn(move || {
        let result = renew_store.renew_runtime_job_lease(RenewRuntimeJobLeaseRequest {
            job_id: fence.job_id,
            lease_owner: fence.lease_owner,
            heartbeat_at_ms: crate::now_ms().unwrap(),
            lease_ms: 60_000,
        });
        renewed_tx.send(result.clone()).unwrap();
        result
    });
    // The barrier establishes the history window. The deadline only bounds
    // observation; pg_blocking_pids distinguishes a job lock from scheduling.
    let renewed_before_release = renewed_rx.recv_timeout(Duration::from_secs(1));
    let mut observer = Client::connect(&test_url(), NoTls).unwrap();
    let blocked: i64 = observer
        .query_one(
            "SELECT COUNT(*) FROM pg_stat_activity WHERE $1=ANY(pg_blocking_pids(pid))",
            &[&append_pid],
        )
        .unwrap()
        .get(0);
    release_tx.send(()).unwrap();
    let appended = append.join().unwrap();
    let renewal = renew.join().unwrap();
    appended.unwrap();
    renewal.unwrap();
    println!(
        "coldHistoryBarrier blockedByAppend={blocked} renewalBeforeRelease={}",
        renewed_before_release.is_ok()
    );
    assert_eq!(
        blocked, 0,
        "cold history validation holds a row needed by heartbeat"
    );
    assert!(
        renewed_before_release.unwrap().is_ok(),
        "heartbeat did not finish while history was paused"
    );
}
