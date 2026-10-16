use super::*;
use crate::postgres_store::execution_instances::bind;
use std::sync::mpsc;
use std::time::{Duration, Instant};

fn debit_totals(db: &mut Client) -> Vec<i64> {
    let row = db.query_one("SELECT count(*),COALESCE(sum(memory_bytes),0)::bigint,COALESCE(sum(cpu_milli),0)::bigint,COALESCE(sum(pids),0)::bigint,COALESCE(sum(workspace_bytes),0)::bigint FROM runtime.resident_sandboxes", &[]).unwrap();
    (0..5).map(|index| row.get(index)).collect()
}

fn legacy(run: &Run) -> EnvironmentBinding {
    let mut binding = run.grant.payload.clone();
    binding.schema = "workspace.environment_authorization.v1".into();
    binding.execution_mode = "perRun".into();
    binding.controller = None;
    binding
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_legacy_binding_cannot_add_writer_to_retained_environment() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (_, store, mut db) = setup();
    let a = run(&store, &mut db, "run-a", 1);
    let physical = Arc::new(Mutex::new(vec![]));
    let allocation = prepare(&store, &a, "retained-execution", &physical).unwrap();
    let b = run(&store, &mut db, "run-b", 2);
    let result = bind(
        &mut db,
        &legacy(&b),
        "legacy-execution",
        "host",
        &b.original.image_digest,
    );
    assert!(
        result.is_err(),
        "legacy binding added a second allocation: {result:?}"
    );
    assert_eq!(
        db.query_one(
            "SELECT count(*),min(instance_id) FROM runtime.execution_instances",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        1
    );
    assert_eq!(
        db.query_one("SELECT instance_id FROM runtime.resident_sandboxes", &[])
            .unwrap()
            .get::<_, String>(0),
        allocation.instance_id
    );
    // Loss of the live holding does not authorize legacy cold recreation either.
    db.batch_execute("DELETE FROM runtime.resident_sandboxes")
        .unwrap();
    assert!(bind(
        &mut db,
        &legacy(&b),
        "legacy-after-loss",
        "host",
        &b.original.image_digest
    )
    .is_err());
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_legacy_binding_preserves_per_run_replay_and_new_execution_behavior() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (_, store, mut db) = setup();
    let a = run(&store, &mut db, "run-a", 1);
    let first = bind(
        &mut db,
        &legacy(&a),
        "execution-a",
        "host",
        &a.original.image_digest,
    )
    .unwrap();
    assert_eq!(
        bind(
            &mut db,
            &legacy(&a),
            "execution-a",
            "host",
            &a.original.image_digest
        )
        .unwrap(),
        first
    );
    let recovery = bind(
        &mut db,
        &legacy(&a),
        "execution-recovery",
        "host",
        &a.original.image_digest,
    )
    .unwrap();
    assert_ne!(recovery, first);
    let b = run(&store, &mut db, "run-b", 2);
    let second = bind(
        &mut db,
        &legacy(&b),
        "execution-b",
        "host",
        &b.original.image_digest,
    )
    .unwrap();
    assert_ne!(second, first);
    assert!(bind(
        &mut db,
        &legacy(&b),
        "execution-a",
        "host",
        &b.original.image_digest
    )
    .is_err());
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.execution_instances WHERE allocation_mode='perRun'",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        3
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_legacy_binding_waits_for_in_flight_retained_admission_before_checking_mode() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (url, store, mut db) = setup();
    let a = run(&store, &mut db, "run-a", 1);
    let old_binding = legacy(&a);
    let image = a.original.image_digest.clone();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let admission = std::thread::spawn(move || {
        let profile = format!("sha256:{}", "b".repeat(64));
        store.prepare_retained_instance(
            &a.request("retained-execution", &profile),
            limits(&a),
            move || {
                entered_tx.send(()).unwrap();
                release_rx
                    .recv_timeout(Duration::from_secs(15))
                    .map_err(|error| error.to_string())?;
                Ok(vec![])
            },
            |_, _| Ok(()),
            |_| Ok("retained-container".into()),
        )
    });
    entered_rx.recv_timeout(Duration::from_secs(15)).unwrap();
    let (pid_tx, pid_rx) = mpsc::channel();
    let (result_tx, result_rx) = mpsc::channel();
    let binding = std::thread::spawn(move || {
        let mut client = Client::connect(&url, NoTls).unwrap();
        pid_tx
            .send(
                client
                    .query_one("SELECT pg_backend_pid()", &[])
                    .unwrap()
                    .get::<_, i32>(0),
            )
            .unwrap();
        let result = bind(
            &mut client,
            &old_binding,
            "legacy-execution",
            "host",
            &image,
        );
        result_tx.send(result.clone()).unwrap();
        result
    });
    let pid = pid_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let blocked = loop {
        if result_rx.try_recv().is_ok() || Instant::now() >= deadline {
            break false;
        }
        let waiting = db.query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid=$1 AND wait_event_type='Lock')", &[&pid]).unwrap().get::<_,bool>(0);
        if waiting {
            break true;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    // Always unblock and join workers before asserting, including the vulnerable baseline.
    release_tx.send(()).unwrap();
    let admission = admission.join().unwrap();
    let binding = binding.join().unwrap();
    assert!(admission.is_ok(), "{admission:?}");
    assert!(
        blocked,
        "legacy binding escaped the ongoing retained admission"
    );
    assert!(binding.is_err(), "{binding:?}");
    assert_eq!(
        db.query_one("SELECT count(*) FROM runtime.execution_instances", &[])
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    assert_eq!(
        db.query_one("SELECT count(*) FROM runtime.resident_sandboxes", &[])
            .unwrap()
            .get::<_, i64>(0),
        1
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_legacy_binding_reenters_normal_admission_without_self_deadlock() {
    use crate::resident_capacity::ResidentSandboxRequest;
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (_, store, mut db) = setup();
    let a = run(&store, &mut db, "run-a", 1);
    store
        .with_client(|client| {
            client
                .batch_execute("SET statement_timeout='5s'")
                .map_err(|error| error.to_string())
        })
        .unwrap();
    let binding = legacy(&a);
    store
        .prepare_resident_sandbox_with_client(
            "host",
            &ResidentSandboxRequest {
                execution_id: "legacy-managed",
                agent_run_id: "run-a",
                workspace_id: "ws_1",
                lifecycle_job_id: &a.fence.job_id,
                lifecycle_lease_owner: &a.fence.lease_owner,
                must_exist: false,
                resources: declared_usage(a.original.resources),
            },
            limits(&a),
            || Ok(vec![]),
            |client| {
                bind(
                    client,
                    &binding,
                    "legacy-managed",
                    "host",
                    &a.original.image_digest,
                )?;
                Ok("legacy-container".into())
            },
        )
        .unwrap();
    assert_eq!(db.query_one("SELECT allocation_mode FROM runtime.execution_instances WHERE execution_id='legacy-managed'", &[]).unwrap().get::<_,String>(0), "perRun");
    assert_eq!(
        db.query_one(
            "SELECT state FROM runtime.resident_sandboxes WHERE execution_id='legacy-managed'",
            &[]
        )
        .unwrap()
        .get::<_, String>(0),
        "resident"
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_legacy_admission_rejection_preserves_single_retained_resource_debit() {
    use crate::resident_capacity::ResidentSandboxRequest;
    use std::sync::atomic::{AtomicBool, Ordering};
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (_, store, mut db) = setup();
    let a = run(&store, &mut db, "run-a", 1);
    let physical = Arc::new(Mutex::new(vec![]));
    prepare(&store, &a, "retained-execution", &physical).unwrap();
    let before = debit_totals(&mut db);
    let b = run(&store, &mut db, "run-b", 2);
    let binding = legacy(&b);
    let two = declared_usage(b.original.resources)
        .checked_add(declared_usage(b.original.resources))
        .unwrap();
    let callback_entered = AtomicBool::new(false);
    let result = store.prepare_resident_sandbox_with_client(
        "host",
        &ResidentSandboxRequest {
            execution_id: "rejected-legacy",
            agent_run_id: "run-b",
            workspace_id: "ws_1",
            lifecycle_job_id: &b.fence.job_id,
            lifecycle_lease_owner: &b.fence.lease_owner,
            must_exist: false,
            resources: declared_usage(b.original.resources),
        },
        ResidentCapacity {
            global: two,
            tenant: two,
            host: two,
        },
        || Ok(physical.lock().unwrap().clone()),
        |client| {
            callback_entered.store(true, Ordering::SeqCst);
            bind(
                client,
                &binding,
                "rejected-legacy",
                "host",
                &b.original.image_digest,
            )?;
            panic!("rejected legacy request must never reach Docker creation")
        },
    );
    assert_eq!(
        result.unwrap_err(),
        "retained environment requires controller admission"
    );
    assert_eq!(
        debit_totals(&mut db),
        before,
        "known mode rejection must not leave an additional reserved debit"
    );
    assert!(!callback_entered.load(Ordering::SeqCst));
    assert_eq!(
        db.query_one("SELECT count(*) FROM runtime.execution_instances", &[])
            .unwrap()
            .get::<_, i64>(0),
        1
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_legacy_admission_unknown_docker_result_keeps_its_resource_debit() {
    use crate::resident_capacity::ResidentSandboxRequest;
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (url, store, mut db) = setup();
    let a = run(&store, &mut db, "run-a", 1);
    let binding = legacy(&a);
    let result = store.prepare_resident_sandbox_with_client(
        "host",
        &ResidentSandboxRequest {
            execution_id: "uncertain-legacy",
            agent_run_id: "run-a",
            workspace_id: "ws_1",
            lifecycle_job_id: &a.fence.job_id,
            lifecycle_lease_owner: &a.fence.lease_owner,
            must_exist: false,
            resources: declared_usage(a.original.resources),
        },
        limits(&a),
        || Ok(vec![]),
        |client| {
            bind(
                client,
                &binding,
                "uncertain-legacy",
                "host",
                &a.original.image_digest,
            )?;
            Err("docker outcome unknown".into())
        },
    );
    assert_eq!(result.unwrap_err(), "docker outcome unknown");
    let before = debit_totals(&mut db);
    assert_eq!(before[0], 1);
    assert_eq!(before[1], a.original.resources.memory_bytes as i64);
    let reservation = db
        .query_one(
            "SELECT state,container_id FROM runtime.resident_sandboxes",
            &[],
        )
        .unwrap();
    assert_eq!(reservation.get::<_, String>(0), "reserved");
    assert_eq!(reservation.get::<_, Option<String>>(1), None);
    drop(store);
    drop(PostgresRuntimeStore::new(&url).unwrap());
    assert_eq!(debit_totals(&mut db), before);
}
