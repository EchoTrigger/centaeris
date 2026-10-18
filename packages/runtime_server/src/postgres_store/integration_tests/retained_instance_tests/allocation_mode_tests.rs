use super::*;
use std::sync::mpsc;
use std::time::Duration;

fn per_run(mut value: Run) -> Run {
    value.grant.payload.execution_mode = "perRun".into();
    value.grant = sign(value.grant.payload);
    value
}
fn debit_totals(db: &mut Client) -> Vec<i64> {
    let row = db.query_one("SELECT count(*),COALESCE(sum(memory_bytes),0)::bigint,COALESCE(sum(cpu_milli),0)::bigint,COALESCE(sum(pids),0)::bigint,COALESCE(sum(workspace_bytes),0)::bigint FROM runtime.resident_sandboxes", &[]).unwrap();
    (0..5).map(|index| row.get(index)).collect()
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_per_run_controller_attaches_existing_instance_without_second_writer() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db) = setup();
    let a = run(&store, &mut db, "run-origin", 1);
    let physical = Arc::new(Mutex::new(vec![]));
    let first = prepare(&store, &a, "origin-execution", &physical).unwrap();
    let before = debit_totals(&mut db);
    db.batch_execute("UPDATE public.app_core_agentrun SET status='completed'; UPDATE public.app_core_sessionenvironment SET controller_released=true").unwrap();
    let b = per_run(run(&store, &mut db, "run-per-run", 2));
    let second = prepare(&store, &b, "per-run-execution", &physical).unwrap();
    assert!(second.reused);
    assert_eq!(second.instance_id, first.instance_id);
    assert_eq!(
        second.allocation_execution_id,
        first.allocation_execution_id
    );
    assert_eq!(debit_totals(&mut db), before);
    assert_eq!(physical.lock().unwrap().len(), 1);
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_per_run_controller_replays_and_recovers_one_durable_allocation() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db) = setup();
    let a = per_run(run(&store, &mut db, "run-current", 1));
    let physical = Arc::new(Mutex::new(vec![]));
    let first = prepare(&store, &a, "first-execution", &physical).unwrap();
    let before = debit_totals(&mut db);
    let replay = prepare(&store, &a, "first-execution", &physical).unwrap();
    assert!(replay.reused);
    assert_eq!(replay.instance_id, first.instance_id);
    drop(store);
    let reopened = PostgresRuntimeStore::new(&url).unwrap();
    let recovery = prepare(&reopened, &a, "recovery-execution", &physical).unwrap();
    assert!(recovery.reused);
    assert_eq!(recovery.instance_id, first.instance_id);
    assert_eq!(recovery.allocation_execution_id, "first-execution");
    assert_eq!(debit_totals(&mut db), before);
    assert_eq!(
        db.query_one("SELECT count(*) FROM runtime.execution_instances", &[])
            .unwrap()
            .get::<_, i64>(0),
        2
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_per_run_in_flight_admission_returns_wait_and_reuses_after_release() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db) = setup();
    let a = per_run(run(&store, &mut db, "run-current", 1));
    let b = Run {
        original: a.original.clone(),
        grant: a.grant.clone(),
        fence: a.fence.clone(),
    };
    let other_store = PostgresRuntimeStore::new(&url).unwrap();
    let retry_store = other_store.clone();
    let replay = Run {
        original: a.original.clone(),
        grant: a.grant.clone(),
        fence: a.fence.clone(),
    };
    let physical = Arc::new(Mutex::new(vec![]));
    let first_physical = physical.clone();
    let second_physical = physical.clone();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let first = std::thread::spawn(move || {
        let profile = a.original.environment_profile.digest().unwrap();
        store.prepare_retained_instance(
            &a.request("first-execution", &profile),
            limits(&a),
            || {
                entered_tx.send(()).unwrap();
                release_rx
                    .lock()
                    .map_err(|_| "release channel poisoned".to_owned())?
                    .recv_timeout(Duration::from_secs(15))
                    .map_err(|e| e.to_string())?;
                Ok(first_physical.lock().unwrap().clone())
            },
            |_, _| Ok(()),
            |allocation| {
                first_physical
                    .lock()
                    .unwrap()
                    .push(ObservedResidentSandbox {
                        execution_id: allocation.allocation_execution_id.clone(),
                        allocation_owner: allocation.allocation_owner.clone(),
                        container_id: "physical-container".into(),
                        resources: declared_usage(
                            a.original.environment_profile.resources.sandbox_resources(),
                        ),
                    });
                Ok("physical-container".into())
            },
        )
    });
    entered_rx.recv_timeout(Duration::from_secs(15)).unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    let second = std::thread::spawn(move || {
        let result = prepare(&other_store, &b, "recovery-execution", &second_physical);
        done_tx.send(result.clone()).unwrap();
        result
    });
    let while_in_flight = done_rx.recv_timeout(Duration::from_secs(5));
    release_tx.send(()).unwrap();
    let first = first.join().unwrap().unwrap();
    let rejected = second.join().unwrap();
    assert_eq!(
        while_in_flight.unwrap().unwrap_err(),
        crate::resident_capacity::RESIDENT_CAPACITY_WAIT
    );
    assert_eq!(
        rejected.unwrap_err(),
        crate::resident_capacity::RESIDENT_CAPACITY_WAIT
    );
    let second = prepare(&retry_store, &replay, "recovery-execution", &physical).unwrap();
    assert_eq!(first.instance_id, second.instance_id);
    assert!(second.reused);
    assert_eq!(debit_totals(&mut db)[0], 1);
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_per_run_controller_rejects_stale_authority_before_physical_work() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db) = setup();
    let a = per_run(run(&store, &mut db, "run-current", 1));
    db.batch_execute("UPDATE public.app_core_sessionenvironment SET controller_epoch=2")
        .unwrap();
    let profile = a.original.environment_profile.digest().unwrap();
    assert!(store
        .prepare_retained_instance(
            &a.request("stale", &profile),
            limits(&a),
            || Ok(vec![]),
            |_, _| panic!("stale controller reached Core acceptance"),
            |_| panic!("stale controller reached physical create")
        )
        .is_err());
    assert_eq!(debit_totals(&mut db), vec![0, 0, 0, 0, 0]);
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_per_run_capacity_rejection_reserves_nothing_and_allows_same_request_later() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db) = setup();
    let a = per_run(run(&store, &mut db, "run-current", 1));
    let mut capacity = limits(&a);
    capacity.host.memory_bytes -= 1;
    let profile = a.original.environment_profile.digest().unwrap();
    let result = store.prepare_retained_instance(
        &a.request("bounded", &profile),
        capacity,
        || Ok(vec![]),
        |_, _| panic!("capacity rejection reached acceptance"),
        |_| panic!("capacity rejection created physical instance"),
    );
    assert!(result.is_err());
    assert_eq!(debit_totals(&mut db), vec![0, 0, 0, 0, 0]);
    assert_eq!(
        db.query_one("SELECT count(*) FROM runtime.execution_instances", &[])
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    let physical = Arc::new(Mutex::new(vec![]));
    prepare(&store, &a, "bounded", &physical).unwrap();
    assert_eq!(debit_totals(&mut db)[0], 1);
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_per_run_unknown_create_keeps_charge_and_adopts_exact_late_receipt() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db) = setup();
    let a = per_run(run(&store, &mut db, "run-current", 1));
    let profile = a.original.environment_profile.digest().unwrap();
    let request = a.request("uncertain", &profile);
    let result = store.prepare_retained_instance(
        &request,
        limits(&a),
        || Ok(vec![]),
        |_, _| Ok(()),
        |_| Err("docker outcome unknown".into()),
    );
    assert_eq!(result.unwrap_err(), "docker outcome unknown");
    let before = debit_totals(&mut db);
    assert_eq!(before[0], 1);
    drop(store);
    let reopened = PostgresRuntimeStore::new(&url).unwrap();
    assert!(reopened
        .prepare_retained_instance(
            &request,
            limits(&a),
            || Ok(vec![]),
            |_, _| panic!("unknown reservation repeated acceptance"),
            |_| panic!("unknown reservation created second writer")
        )
        .is_err());
    assert_eq!(debit_totals(&mut db), before);
    let physical = Arc::new(Mutex::new(vec![ObservedResidentSandbox {
        execution_id: "uncertain".into(),
        allocation_owner: centaeris_core::execution::EnvironmentTaskOwner::AgentRun {
            run_id: a.original.agent_run_id.clone(),
        },
        container_id: "late-container".into(),
        resources: request.resources,
    }]));
    let adopted = prepare(&reopened, &a, "uncertain", &physical).unwrap();
    assert!(adopted.reused);
    assert_eq!(adopted.container_id.as_deref(), Some("late-container"));
    assert_eq!(debit_totals(&mut db), before);
}
