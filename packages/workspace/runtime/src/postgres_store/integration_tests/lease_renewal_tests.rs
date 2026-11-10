use super::cold_append_fault_tests::lease;
use super::cold_append_tests::fixture;
use super::*;
use crate::postgres_store::lease_renewal::WorkerLeaseRenewalError;

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_worker_renewal_cannot_revive_expired_lease_with_old_client_clock() {
    let _guard = TEST_LOCK.lock().unwrap();
    let fixture = fixture(1);
    let fence = lease(&fixture);
    let mut client = Client::connect(&test_url(), NoTls).unwrap();
    let now: i64 = client
        .query_one(
            "SELECT (EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint",
            &[],
        )
        .unwrap()
        .get(0);
    client
        .execute(
            "UPDATE runtime.runtime_jobs SET lease_expires_at_ms=$2 WHERE job_id=$1",
            &[&fence.job_id, &(now - 100)],
        )
        .unwrap();
    let result = fixture
        .store
        .renew_worker_job_lease(RenewRuntimeJobLeaseRequest {
            job_id: fence.job_id.clone(),
            lease_owner: fence.lease_owner.clone(),
            heartbeat_at_ms: now - 1000,
            lease_ms: 60000,
        });
    assert_eq!(
        result,
        Err(WorkerLeaseRenewalError::FenceRejected),
        "a delayed client timestamp revived expired authority"
    );
    let until: i64 = client
        .query_one(
            "SELECT lease_expires_at_ms FROM runtime.runtime_jobs WHERE job_id=$1",
            &[&fence.job_id],
        )
        .unwrap()
        .get(0);
    assert_eq!(
        until,
        now - 100,
        "rejected renewal must not alter the original expiry"
    );
}

fn db_now(client: &mut Client) -> i64 {
    client
        .query_one(
            "SELECT (EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint",
            &[],
        )
        .unwrap()
        .get(0)
}

fn lease_tuple(client: &mut Client, job: &str) -> (String, i64, i64, String) {
    let row = client.query_one("SELECT lease_owner,lease_expires_at_ms,heartbeat_at_ms,status FROM runtime.runtime_jobs WHERE job_id=$1", &[&job]).unwrap();
    (row.get(0), row.get(1), row.get(2), row.get(3))
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_worker_renewal_uses_database_time_despite_future_client_clock() {
    let _guard = TEST_LOCK.lock().unwrap();
    let fixture = fixture(1);
    let fence = lease(&fixture);
    let mut observer = Client::connect(&test_url(), NoTls).unwrap();
    let before = db_now(&mut observer);
    fixture
        .store
        .renew_worker_job_lease(RenewRuntimeJobLeaseRequest {
            job_id: fence.job_id.clone(),
            lease_owner: fence.lease_owner.clone(),
            heartbeat_at_ms: before + 86_400_000,
            lease_ms: 60_000,
        })
        .unwrap();
    let after = db_now(&mut observer);
    let (owner, expires, heartbeat, status) = lease_tuple(&mut observer, &fence.job_id);
    assert_eq!(owner, fence.lease_owner);
    assert_eq!(status, "running");
    assert!(
        (before..=after).contains(&heartbeat),
        "client clock became durable authority"
    );
    assert_eq!(
        expires,
        heartbeat + 60_000,
        "expiry must follow the accepted database time"
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_worker_renewal_rechecks_expiry_after_waiting_for_job_lock() {
    let _guard = TEST_LOCK.lock().unwrap();
    let fixture = fixture(1);
    let fence = lease(&fixture);
    let mut observer = Client::connect(&test_url(), NoTls).unwrap();
    // Pin the actual reserved-channel backend before occupying the row. No
    // second control operation can replace this idle connection in this fixture.
    let worker_pid: i32 = fixture
        .store
        .with_execution_control_client(|client| {
            client
                .query_one("SELECT pg_backend_pid()", &[])
                .map(|row| row.get(0))
                .map_err(|error| error.to_string())
        })
        .unwrap();
    let before = db_now(&mut observer);
    observer
        .execute(
            "UPDATE runtime.runtime_jobs SET lease_expires_at_ms=$2 WHERE job_id=$1",
            &[&fence.job_id, &(before + 2000)],
        )
        .unwrap();
    let original = lease_tuple(&mut observer, &fence.job_id);
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
    let renewal_store = fixture.store.clone();
    let job = fence.job_id.clone();
    let owner = fence.lease_owner.clone();
    let renewal = std::thread::spawn(move || {
        renewal_store.renew_worker_job_lease(RenewRuntimeJobLeaseRequest {
            job_id: job,
            lease_owner: owner,
            heartbeat_at_ms: before,
            lease_ms: 60_000,
        })
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut waiting = false;
    while std::time::Instant::now() < deadline {
        waiting = observer
            .query_one(
                "SELECT $2=ANY(pg_blocking_pids($1))",
                &[&worker_pid, &blocker_pid],
            )
            .unwrap()
            .get(0);
        if waiting {
            break;
        }
        std::thread::yield_now();
    }
    let mut expired = false;
    while std::time::Instant::now() < deadline {
        expired = db_now(&mut observer) >= original.1;
        if expired {
            break;
        }
        std::thread::yield_now();
    }
    // A pure lock wait: do not modify the tuple to force predicate re-evaluation.
    hold.commit().unwrap();
    let result = renewal.join().unwrap();
    assert!(waiting, "renewal did not enter the controlled job-row wait");
    assert!(expired, "database time did not cross the original expiry");
    assert_eq!(result, Err(WorkerLeaseRenewalError::FenceRejected));
    assert_eq!(lease_tuple(&mut observer, &fence.job_id), original);
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_worker_renewal_lock_timeout_is_unavailable_without_altering_authority() {
    let _guard = TEST_LOCK.lock().unwrap();
    let fixture = fixture(1);
    let fence = lease(&fixture);
    let url = test_url();
    let timeout_url = format!(
        "{url}{}options=-c%20lock_timeout%3D200ms",
        if url.contains('?') { "&" } else { "?" }
    );
    let renewal_store = PostgresRuntimeStore::new(&timeout_url).unwrap();
    let mut observer = Client::connect(&url, NoTls).unwrap();
    let original = lease_tuple(&mut observer, &fence.job_id);
    let mut blocker = Client::connect(&url, NoTls).unwrap();
    let mut hold = blocker.transaction().unwrap();
    hold.query_one(
        "SELECT job_id FROM runtime.runtime_jobs WHERE job_id=$1 FOR UPDATE",
        &[&fence.job_id],
    )
    .unwrap();
    let result = renewal_store.renew_worker_job_lease(RenewRuntimeJobLeaseRequest {
        job_id: fence.job_id.clone(),
        lease_owner: fence.lease_owner.clone(),
        heartbeat_at_ms: db_now(&mut observer),
        lease_ms: 60_000,
    });
    hold.commit().unwrap();
    assert_eq!(
        result,
        Err(WorkerLeaseRenewalError::Unavailable {
            stage: "lock",
            sqlstate: Some("55P03".into())
        })
    );
    assert_eq!(lease_tuple(&mut observer, &fence.job_id), original);
    // Temporary storage contention does not consume or replace this owner.
    fixture
        .store
        .renew_worker_job_lease(RenewRuntimeJobLeaseRequest {
            job_id: fence.job_id,
            lease_owner: fence.lease_owner,
            heartbeat_at_ms: db_now(&mut observer),
            lease_ms: 60_000,
        })
        .unwrap();
}
