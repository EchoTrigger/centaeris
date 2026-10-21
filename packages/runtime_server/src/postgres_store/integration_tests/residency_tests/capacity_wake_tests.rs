use super::*;
use centaeris_core::session::reliability::ClaimDueRuntimeJobsRequest;
use std::time::Duration;

fn now(db: &mut Client) -> i64 {
    db.query_one(
        "SELECT (EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint",
        &[],
    )
    .unwrap()
    .get(0)
}

fn yield_until(store: &PostgresRuntimeStore, fixture: &SandboxFixture, at: i64, reason: &str) {
    store
        .yield_runtime_job(YieldRuntimeJobRequest {
            job_id: fixture.job.clone(),
            lease_owner: fixture.owner.clone(),
            yielded_at_ms: at,
            run_at_ms: at + 30_000,
            transition_reason: reason.into(),
        })
        .unwrap();
}

fn claim_waiter(
    store: &PostgresRuntimeStore,
    fixture: &SandboxFixture,
    at: i64,
) -> Vec<centaeris_core::session::reliability::RuntimeJobRecord> {
    store
        .claim_worker_jobs(ClaimDueRuntimeJobsRequest {
            now_ms: at,
            worker_id: "capacity-wake-observer".into(),
            job_id: Some(fixture.job.clone()),
            job_kind: Some("agent_run.lifecycle".into()),
            session_id: None,
            limit: 1,
            lease_ms: 60_000,
        })
        .unwrap()
}

fn release_race(release_before_yield: bool) {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store) = setup();
    let blocker = sandbox(&store, &url, "release-blocker", "workspace-a");
    let waiting = sandbox(&store, &url, "release-waiter", "workspace-b");
    let unrelated = sandbox(&store, &url, "unrelated-question", "workspace-c");
    let host = Arc::new(Mutex::new(Vec::new()));
    seed_historical_resident(&url, &blocker, &host);
    assert_eq!(
        prepare(&store, &waiting, &host, limits(1, 1)).unwrap_err(),
        RESIDENT_CAPACITY_WAIT
    );
    assert_eq!(
        prepare(&store, &unrelated, &host, limits(1, 1)).unwrap_err(),
        RESIDENT_CAPACITY_WAIT
    );
    let mut db = Client::connect(&url, NoTls).unwrap();
    let at = now(&mut db);
    yield_until(&store, &unrelated, at, "question_wait");
    if !release_before_yield {
        yield_until(&store, &waiting, at, RESIDENT_CAPACITY_WAIT);
    }
    assert!(
        !store
            .wait_for_runtime_jobs(&["agent_run.lifecycle".into()], Duration::from_millis(5))
            .unwrap()
            .ready
    );
    store
        .remove_resident_sandboxes(
            "fake-docker-host",
            &blocker.run,
            &blocker.fence(),
            || {
                host.lock().unwrap().clear();
                Ok(())
            },
            || Ok(host.lock().unwrap().clone()),
        )
        .unwrap();
    assert_eq!(count(&url), 0);
    if release_before_yield {
        assert!(
            claim_waiter(&store, &waiting, now(&mut db)).is_empty(),
            "a release cannot steal an active owner"
        );
        yield_until(&store, &waiting, now(&mut db), RESIDENT_CAPACITY_WAIT);
    }
    drop(store);
    let reopened = PostgresRuntimeStore::new(&url).unwrap();
    assert!(reopened.wait_for_runtime_jobs(&["agent_run.lifecycle".into()], Duration::from_millis(5)).unwrap().ready,
        "confirmed release must survive reopen and make the capacity waiter ready before its 30-second fallback");
    assert!(
        claim_waiter(&reopened, &unrelated, now(&mut db)).is_empty(),
        "release cannot advance a question wait"
    );
    let claimed = claim_waiter(&reopened, &waiting, now(&mut db));
    assert_eq!(claimed.len(), 1, "ready and ordinary claim must agree");
    let evidence: serde_json::Value = serde_json::from_str(&db.query_one("SELECT payload_json FROM runtime.runtime_events WHERE task_id=$1 AND event_type='runtime.resident_capacity.claimed.v1'", &[&waiting.job]).unwrap().get::<_,String>(0)).unwrap();
    assert_eq!(
        evidence["leaseOwner"].as_str(),
        claimed[0].lease_owner.as_deref()
    );
    assert_eq!(evidence["generation"], 1);
    assert!(
        evidence["claimedAtMs"].as_i64().unwrap()
            >= evidence["releaseRecordedAtMs"].as_i64().unwrap()
    );
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.resident_capacity_waiters WHERE job_id=$1",
            &[&waiting.job]
        )
        .unwrap()
        .get::<_, i64>(0),
        0
    );
    assert_ne!(
        claimed[0].lease_owner.as_deref(),
        Some(waiting.owner.as_str())
    );
    assert!(
        claim_waiter(&reopened, &waiting, now(&mut db)).is_empty(),
        "at most one current owner"
    );
    assert!(
        host.lock().unwrap().is_empty(),
        "wake and claim do not create physical resources"
    );
}

#[test]
#[ignore = "requires owned disposable PostgreSQL"]
fn postgres_confirmed_release_wakes_future_capacity_wait_after_reopen() {
    release_race(false);
}

#[test]
#[ignore = "requires owned disposable PostgreSQL"]
fn postgres_release_before_capacity_yield_is_not_lost() {
    release_race(true);
}

#[test]
#[ignore = "requires owned disposable PostgreSQL"]
fn postgres_unconfirmed_removal_does_not_wake_capacity_wait() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store) = setup();
    let blocker = sandbox(&store, &url, "unknown-blocker", "workspace-a");
    let waiting = sandbox(&store, &url, "unknown-waiter", "workspace-b");
    let host = Arc::new(Mutex::new(Vec::new()));
    seed_historical_resident(&url, &blocker, &host);
    assert_eq!(
        prepare(&store, &waiting, &host, limits(1, 1)).unwrap_err(),
        RESIDENT_CAPACITY_WAIT
    );
    let mut db = Client::connect(&url, NoTls).unwrap();
    yield_until(&store, &waiting, now(&mut db), RESIDENT_CAPACITY_WAIT);
    assert!(store
        .remove_resident_sandboxes(
            "fake-docker-host",
            &blocker.run,
            &blocker.fence(),
            || Err("unknown removal".into()),
            || Ok(host.lock().unwrap().clone())
        )
        .is_err());
    assert_eq!(count(&url), 1);
    assert!(
        !store
            .wait_for_runtime_jobs(&["agent_run.lifecycle".into()], Duration::from_millis(5))
            .unwrap()
            .ready
    );
    assert!(claim_waiter(&store, &waiting, now(&mut db)).is_empty());
}

#[test]
#[ignore = "requires owned disposable PostgreSQL"]
fn postgres_capacity_signal_preserves_execution_limits_and_reclaim_embargo() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store) = setup();
    let blocker = sandbox(&store, &url, "full-blocker", "full-a");
    let waiting = sandbox(&store, &url, "full-waiter", "full-a");
    let host = Arc::new(Mutex::new(Vec::new()));
    seed_historical_resident(&url, &blocker, &host);
    assert_eq!(
        prepare(&store, &waiting, &host, limits(1, 1)).unwrap_err(),
        RESIDENT_CAPACITY_WAIT
    );
    let mut db = Client::connect(&url, NoTls).unwrap();
    yield_until(&store, &waiting, now(&mut db), RESIDENT_CAPACITY_WAIT);
    let mut fillers = Vec::new();
    for i in 0..7 {
        fillers.push(sandbox(
            &store,
            &url,
            &format!("full-{i}"),
            if i < 3 { "full-a" } else { "full-b" },
        ));
    }
    store
        .remove_resident_sandboxes(
            "fake-docker-host",
            &blocker.run,
            &blocker.fence(),
            || {
                host.lock().unwrap().clear();
                Ok(())
            },
            || Ok(host.lock().unwrap().clone()),
        )
        .unwrap();
    let ready = || {
        store
            .wait_for_runtime_jobs(&["agent_run.lifecycle".into()], Duration::from_millis(5))
            .unwrap()
            .ready
    };
    assert!(!ready(), "release cannot grant a ninth execution");
    assert!(claim_waiter(&store, &waiting, now(&mut db)).is_empty());
    yield_until(&store, &fillers[3], now(&mut db), "question_wait");
    assert!(!ready(), "release cannot grant a fifth workspace execution");
    assert!(claim_waiter(&store, &waiting, now(&mut db)).is_empty());
    yield_until(&store, &fillers[0], now(&mut db), "question_wait");
    db.execute(
        "UPDATE runtime.runtime_jobs SET lease_reclaim_not_before_ms=$2 WHERE job_id=$1",
        &[
            &waiting.job,
            &(now(&mut Client::connect(&url, NoTls).unwrap()) + 30_000),
        ],
    )
    .unwrap();
    assert!(!ready(), "release cannot bypass lease-reclaim backoff");
    assert!(claim_waiter(&store, &waiting, now(&mut db)).is_empty());
    db.execute(
        "UPDATE runtime.runtime_jobs SET lease_reclaim_not_before_ms=NULL WHERE job_id=$1",
        &[&waiting.job],
    )
    .unwrap();
    assert!(ready());
    assert_eq!(claim_waiter(&store, &waiting, now(&mut db)).len(), 1);
}

#[test]
#[ignore = "requires owned disposable PostgreSQL"]
fn postgres_rolled_back_release_does_not_advance_capacity_wait() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store) = setup();
    let blocker = sandbox(&store, &url, "rollback-blocker", "rollback-a");
    let waiting = sandbox(&store, &url, "rollback-waiter", "rollback-b");
    let host = Arc::new(Mutex::new(Vec::new()));
    seed_historical_resident(&url, &blocker, &host);
    assert_eq!(
        prepare(&store, &waiting, &host, limits(1, 1)).unwrap_err(),
        RESIDENT_CAPACITY_WAIT
    );
    let mut db = Client::connect(&url, NoTls).unwrap();
    yield_until(&store, &waiting, now(&mut db), RESIDENT_CAPACITY_WAIT);
    let mut tx = db.transaction().unwrap();
    assert_eq!(
        tx.execute("DELETE FROM runtime.resident_sandboxes", &[])
            .unwrap(),
        1
    );
    tx.rollback().unwrap();
    assert_eq!(count(&url), 1);
    drop(store);
    let reopened = PostgresRuntimeStore::new(&url).unwrap();
    assert!(
        !reopened
            .wait_for_runtime_jobs(&["agent_run.lifecycle".into()], Duration::from_millis(5))
            .unwrap()
            .ready
    );
    assert!(claim_waiter(&reopened, &waiting, now(&mut db)).is_empty());
}

fn preserved_facts(db: &mut Client) -> Vec<Vec<String>> {
    [
        "runtime.runtime_jobs",
        "runtime.execution_instances",
        "runtime.resident_sandboxes",
        "public.app_core_agentrunauthorization",
    ]
    .into_iter()
    .map(|table| {
        db.query(
            &format!("SELECT row_to_json(t)::text FROM {table} t ORDER BY row_to_json(t)::text"),
            &[],
        )
        .unwrap()
        .iter()
        .map(|row| row.get(0))
        .collect()
    })
    .collect()
}

#[test]
#[ignore = "requires owned disposable PostgreSQL"]
fn postgres_capacity_wait_migration_preserves_existing_resource_and_authorization_facts() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store) = setup();
    let blocker = sandbox(&store, &url, "migration-blocker", "migration-a");
    let host = Arc::new(Mutex::new(Vec::new()));
    seed_historical_resident(&url, &blocker, &host);
    drop(store);
    let mut db = Client::connect(&url, NoTls).unwrap();
    super::super::super::capacity_wait::restore_previous_fixture(&mut db);
    let facts = preserved_facts(&mut db);
    let history: Vec<(i64, i64)> = db
        .query(
            "SELECT version,applied_at_ms FROM runtime.schema_migrations ORDER BY version",
            &[],
        )
        .unwrap()
        .iter()
        .map(|row| (row.get(0), row.get(1)))
        .collect();
    assert_eq!(
        history.iter().map(|r| r.0).collect::<Vec<_>>(),
        (1..=15).collect::<Vec<_>>()
    );
    drop(PostgresRuntimeStore::new(&url).unwrap());
    assert_eq!(preserved_facts(&mut db), facts);
    let after: Vec<(i64, i64)> = db
        .query(
            "SELECT version,applied_at_ms FROM runtime.schema_migrations ORDER BY version",
            &[],
        )
        .unwrap()
        .iter()
        .map(|row| (row.get(0), row.get(1)))
        .collect();
    assert_eq!(&after[..15], history.as_slice());
    assert_eq!(after[15].0, 16);
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.resident_capacity_waiters",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        0,
        "migration must not invent historical attempt evidence"
    );
}

#[test]
#[ignore = "requires owned disposable PostgreSQL"]
fn postgres_capacity_wait_reopen_rejects_signal_and_binding_drift() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    for mutation in [
        "DELETE FROM runtime.resident_capacity_clock",
        "ALTER TABLE runtime.resident_sandboxes DISABLE TRIGGER resident_capacity_release",
        "ALTER TABLE runtime.runtime_jobs DISABLE TRIGGER resident_capacity_job_change",
        "CREATE OR REPLACE FUNCTION runtime.resident_capacity_release() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NULL; END; $$",
        "ALTER TABLE runtime.resident_capacity_waiters DROP CONSTRAINT resident_capacity_waiters_job_id_fkey",
        "ALTER TABLE runtime.resident_capacity_waiters DROP CONSTRAINT resident_capacity_waiter_bounds",
        "ALTER TABLE runtime.resident_capacity_clock DROP CONSTRAINT resident_capacity_clock_bounds",
    ] {
        let (url,store)=setup(); drop(store);
        let mut db=Client::connect(&url,NoTls).unwrap();
        db.batch_execute(mutation).unwrap();
        assert!(PostgresRuntimeStore::new(&url).is_err(),"schema drift accepted: {mutation}");
    }
}
