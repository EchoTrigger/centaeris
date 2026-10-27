use super::*;
use std::sync::mpsc;
use std::time::Duration;

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_stable_resident_reconciliation_progresses_during_authorized_operation() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (url, store, mut db) = setup();
    let current = Arc::new(run(&store, &mut db, "run-stable-reconcile", 1));
    let physical = Arc::new(Mutex::new(Vec::new()));
    let allocated = prepare(&store, &current, "execution-stable-reconcile", &physical).unwrap();
    let before = db.query_one("SELECT container_id,state,memory_bytes,cpu_milli,pids,workspace_bytes,updated_at_ms FROM runtime.resident_sandboxes WHERE execution_id='execution-stable-reconcile'", &[]).unwrap();
    let (entered_tx, entered_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    let operation_store = store.clone();
    let operation_run = current.clone();
    let operation = std::thread::spawn(move || {
        let profile = operation_run.original.environment_profile.digest().unwrap();
        operation_store.with_retained_instance_operation(
            &operation_run.request("execution-stable-reconcile", &profile),
            &allocated,
            move |_| {
                entered_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(10)).unwrap();
                Ok(())
            },
        )
    });
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let reconcile_store = PostgresRuntimeStore::new(&url).unwrap();
    let observed = physical.lock().unwrap().clone();
    let (completed_tx, completed_rx) = mpsc::sync_channel(1);
    let reconcile = std::thread::spawn(move || {
        let result = reconcile_store.reconcile_resident_sandboxes("host", || Ok(observed));
        completed_tx.send(result.clone()).unwrap();
        result
    });
    let completed_while_pinned = completed_rx.recv_timeout(Duration::from_secs(2)).ok();
    release_tx.send(()).unwrap();
    operation.join().unwrap().unwrap();
    reconcile.join().unwrap().unwrap();
    assert!(matches!(completed_while_pinned, Some(Ok(()))),
        "a matching resident inventory waited for an unrelated authorized operation: {completed_while_pinned:?}");
    let after = db.query_one("SELECT container_id,state,memory_bytes,cpu_milli,pids,workspace_bytes,updated_at_ms FROM runtime.resident_sandboxes WHERE execution_id='execution-stable-reconcile'", &[]).unwrap();
    assert_eq!(
        before.get::<_, Option<String>>(0),
        after.get::<_, Option<String>>(0)
    );
    assert_eq!(before.get::<_, String>(1), after.get::<_, String>(1));
    for column in 2..7 {
        assert_eq!(before.get::<_, i64>(column), after.get::<_, i64>(column));
    }
}

fn confirm(
    run: &Run,
    allocation: &RetainedInstanceAllocation,
    physical: &Arc<Mutex<Vec<ObservedResidentSandbox>>>,
) -> Result<String, String> {
    let container = format!("container-{}", allocation.instance_id);
    physical.lock().unwrap().push(ObservedResidentSandbox {
        execution_id: allocation.allocation_execution_id.clone(),
        allocation_owner: allocation.allocation_owner.clone(),
        container_id: container.clone(),
        resources: declared_usage(
            run.original
                .environment_profile
                .resources
                .sandbox_resources(),
        ),
    });
    Ok(container)
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_confirmed_resident_initialization_does_not_hold_global_preparation() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (_url, store, mut db) = setup();
    let current = Arc::new(run(&store, &mut db, "run-initialize-a", 1));
    let other = super::super::shared_admission_tests::run_scoped(
        &store,
        &mut db,
        "run-initialize-b",
        "session-initialize-b",
        "environment-initialize-b",
        current.original.environment_profile.clone(),
    );
    let one = declared_usage(
        current
            .original
            .environment_profile
            .resources
            .sandbox_resources(),
    );
    let two = one.checked_add(one).unwrap();
    let capacity = ResidentCapacity {
        global: two,
        tenant: two,
        host: two,
    };
    let physical = Arc::new(Mutex::new(Vec::new()));
    let (entered_tx, entered_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    let first_store = store.clone();
    let first_run = current.clone();
    let first_physical = physical.clone();
    let first = std::thread::spawn(move || {
        let profile = first_run.original.environment_profile.digest().unwrap();
        first_store.prepare_retained_instance_then_initialize(
            &first_run.request("execution-initialize-a", &profile),
            capacity,
            || {
                Ok(crate::resident_capacity::ResidentInventory::declarations(
                    Vec::new(),
                ))
            },
            |_, _| Ok(()),
            |allocation| confirm(&first_run, allocation, &first_physical),
            move |_| {
                entered_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(10)).unwrap();
                Ok(())
            },
        )
    });
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let resident_while_initializing: String = db.query_one("SELECT state FROM runtime.resident_sandboxes WHERE execution_id='execution-initialize-a'", &[]).unwrap().get(0);
    let current_profile = current.original.environment_profile.digest().unwrap();
    let retry_before = db
        .query_one(
            "SELECT retry_count FROM runtime.runtime_jobs WHERE job_id=$1",
            &[&current.fence.job_id],
        )
        .unwrap()
        .get::<_, i64>(0);
    let duplicate = {
        let _deadline = crate::postgres_store::RequestDeadline::enter(Some(
            std::time::Instant::now() + Duration::from_secs(1),
        ));
        store.prepare_retained_instance(
            &current.request("execution-initialize-a", &current_profile),
            capacity,
            || panic!("a pinned same-lease duplicate cannot enter inventory"),
            |_, _| panic!("a pinned duplicate cannot append a preparation fact"),
            |_| panic!("a pinned duplicate cannot repeat physical work"),
        )
    };
    let retire = store.retire_retained_instance(
        &crate::postgres_store::residency::retirement::RetirementRequest {
            schema: "runtime.environment.retire.v1".into(),
            operation_id: "retire-during-initialization".into(),
            environment_id: current.grant.payload.environment_id.clone(),
            instance_id: crate::postgres_store::execution_instances::instance_id(
                "execution-initialize-a",
            ),
            host_id: "host".into(),
            expected_controller_epoch: 1,
            acknowledge_tmpfs_loss: true,
        },
        |_| panic!("retirement cannot mutate an initializing environment"),
    );
    let retry_after = db
        .query_one(
            "SELECT retry_count FROM runtime.runtime_jobs WHERE job_id=$1",
            &[&current.fence.job_id],
        )
        .unwrap()
        .get::<_, i64>(0);
    let observed_count = std::sync::atomic::AtomicUsize::new(0);
    let profile = other.original.environment_profile.digest().unwrap();
    let second = store.prepare_retained_instance_then_initialize(
        &other.request("execution-initialize-b", &profile),
        capacity,
        || {
            let sandboxes = physical.lock().unwrap().clone();
            observed_count.store(sandboxes.len(), std::sync::atomic::Ordering::SeqCst);
            Ok(crate::resident_capacity::ResidentInventory {
                sandboxes,
                shared: Some(crate::resident_capacity::SharedHostInventory {
                    host_id: "host".into(),
                    available_memory_bytes: two.memory_bytes.saturating_mul(2),
                    memory_headroom_bytes: 1,
                    valid_until: std::time::Instant::now() + Duration::from_secs(5),
                    measured_by_container: std::collections::BTreeMap::new(),
                }),
            })
        },
        |_, _| Ok(()),
        |allocation| confirm(&other, allocation, &physical),
        |_| Ok(()),
    );
    release_tx.send(()).unwrap();
    first.join().unwrap().unwrap();
    assert_eq!(
        resident_while_initializing, "resident",
        "physical confirmation must commit before controller initialization"
    );
    assert_eq!(
        duplicate.unwrap_err(),
        crate::resident_capacity::HOST_PREPARATION_WAIT
    );
    assert_eq!(
        retire.unwrap_err().to_string(),
        crate::resident_capacity::RESIDENT_CAPACITY_WAIT
    );
    assert_eq!(retry_before, retry_after);
    second.expect(
        "another environment must finish preparation before the first initialization is released",
    );
    assert_eq!(observed_count.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.resident_sandboxes WHERE state='resident'",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        2
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_initialization_failure_preserves_confirmed_resident_and_dispatch_responsibility() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (_url, store, mut db) = setup();
    let current = run(&store, &mut db, "run-initialize-failure", 1);
    let physical = Arc::new(Mutex::new(Vec::new()));
    let profile = current.original.environment_profile.digest().unwrap();
    let error = store
        .prepare_retained_instance_then_initialize(
            &current.request("execution-initialize-failure", &profile),
            limits(&current),
            || {
                Ok(crate::resident_capacity::ResidentInventory::declarations(
                    Vec::new(),
                ))
            },
            |_, _| Ok(()),
            |allocation| confirm(&current, allocation, &physical),
            |_| Err("controlled workspace initialization failure".into()),
        )
        .unwrap_err();
    assert_eq!(error, "controlled workspace initialization failure");
    assert_eq!(db.query_one("SELECT state FROM runtime.resident_sandboxes WHERE execution_id='execution-initialize-failure'", &[]).unwrap().get::<_, String>(0), "resident");
    assert_eq!(db.query_one("SELECT state FROM runtime.environment_creation_attempts WHERE allocation_execution_id='execution-initialize-failure'", &[]).unwrap().get::<_, String>(0), "dispatched");
    assert_eq!(physical.lock().unwrap().len(), 1);
    let container = physical.lock().unwrap()[0].container_id.clone();
    let retried = store
        .prepare_retained_instance_then_initialize(
            &current.request("execution-initialize-failure", &profile),
            limits(&current),
            || {
                Ok(crate::resident_capacity::ResidentInventory::declarations(
                    physical.lock().unwrap().clone(),
                ))
            },
            |_, allocation| {
                assert!(allocation.reused);
                Ok(())
            },
            |allocation| {
                assert!(allocation.reused);
                assert_eq!(allocation.container_id.as_ref(), Some(&container));
                Ok(container.clone())
            },
            |_| Ok(()),
        )
        .unwrap();
    assert_eq!(retried.container_id.as_ref(), Some(&container));
    assert!(retried.reused);
    assert_eq!(physical.lock().unwrap().len(), 1);
    assert_eq!(db.query_one("SELECT count(*) FROM runtime.environment_creation_attempts WHERE allocation_execution_id='execution-initialize-failure'", &[]).unwrap().get::<_, i64>(0), 1);
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_missing_pinned_resident_defers_reconciliation_without_refunding_capacity() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (url, store, mut db) = setup();
    let current = Arc::new(run(&store, &mut db, "run-missing-pinned", 1));
    let other = super::super::shared_admission_tests::run_scoped(
        &store,
        &mut db,
        "run-missing-pressure",
        "session-missing-pressure",
        "environment-missing-pressure",
        current.original.environment_profile.clone(),
    );
    let physical = Arc::new(Mutex::new(Vec::new()));
    let allocated = prepare(&store, &current, "execution-missing-pinned", &physical).unwrap();
    let (entered_tx, entered_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    let operation_store = store.clone();
    let operation_run = current.clone();
    let operation = std::thread::spawn(move || {
        let profile = operation_run.original.environment_profile.digest().unwrap();
        operation_store.with_retained_instance_operation(
            &operation_run.request("execution-missing-pinned", &profile),
            &allocated,
            move |_| {
                entered_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(10)).unwrap();
                Ok(())
            },
        )
    });
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let reconcile_store = PostgresRuntimeStore::new(&url).unwrap();
    let (completed_tx, completed_rx) = mpsc::sync_channel(1);
    let reconcile = std::thread::spawn(move || {
        let result = reconcile_store.reconcile_resident_sandboxes("host", || Ok(Vec::new()));
        completed_tx.send(result.clone()).unwrap();
        result
    });
    let completed_while_pinned = completed_rx.recv_timeout(Duration::from_secs(2)).ok();
    let count_while_pinned: i64 = db
        .query_one("SELECT count(*) FROM runtime.resident_sandboxes", &[])
        .unwrap()
        .get(0);
    let blocked_admission = if matches!(completed_while_pinned, Some(Ok(()))) {
        let profile = other.original.environment_profile.digest().unwrap();
        Some(store.prepare_retained_instance(
            &other.request("execution-missing-pressure", &profile),
            limits(&current),
            || Ok(Vec::new()),
            |_, _| panic!("a skipped debit cannot admit another environment"),
            |_| panic!("a skipped debit cannot create another environment"),
        ))
    } else {
        None
    };
    release_tx.send(()).unwrap();
    operation.join().unwrap().unwrap();
    reconcile.join().unwrap().unwrap();
    assert!(
        matches!(completed_while_pinned, Some(Ok(()))),
        "missing pinned inventory held the global reconciliation lock"
    );
    assert_eq!(
        count_while_pinned, 1,
        "busy absent candidates retain their charge"
    );
    assert_eq!(
        blocked_admission.unwrap().unwrap_err(),
        crate::resident_capacity::RESIDENT_CAPACITY_WAIT
    );
    store
        .reconcile_resident_sandboxes("host", || Ok(Vec::new()))
        .unwrap();
    assert_eq!(
        db.query_one("SELECT count(*) FROM runtime.resident_sandboxes", &[])
            .unwrap()
            .get::<_, i64>(0),
        0
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_failed_global_preparation_does_not_broadcast_environment_release() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (_url, store, mut db) = setup();
    let current = run(&store, &mut db, "run-global-contended", 1);
    let generation: i64 = db
        .query_one(
            "SELECT generation FROM runtime.resident_capacity_clock",
            &[],
        )
        .unwrap()
        .get(0);
    db.query_one("SELECT pg_advisory_lock(731946,3)", &[])
        .unwrap();
    let profile = current.original.environment_profile.digest().unwrap();
    let error = store
        .prepare_retained_instance(
            &current.request("execution-global-contended", &profile),
            limits(&current),
            || panic!("global contention must precede inventory"),
            |_, _| panic!("global contention cannot create a fact"),
            |_| panic!("global contention cannot create a container"),
        )
        .unwrap_err();
    let after: i64 = db
        .query_one(
            "SELECT generation FROM runtime.resident_capacity_clock",
            &[],
        )
        .unwrap()
        .get(0);
    db.query_one("SELECT pg_advisory_unlock(731946,3)", &[])
        .unwrap();
    assert_eq!(error, crate::resident_capacity::HOST_PREPARATION_WAIT);
    assert_eq!(generation, after, "a temporary environment guard cannot wake the shared preparation pool after global refusal");
    assert_eq!(
        db.query_one("SELECT count(*) FROM runtime.resident_sandboxes", &[])
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    let physical = Arc::new(Mutex::new(Vec::new()));
    prepare(&store, &current, "execution-global-contended", &physical).unwrap();
    assert_eq!(physical.lock().unwrap().len(), 1);
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_environment_unlock_closes_registration_after_global_unlock_wake() {
    use crate::postgres_store::residency::after_global_preparation_unlock;
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (url, store, mut db) = setup();
    let current = Arc::new(run(&store, &mut db, "run-unlock-owner", 1));
    let waiting = Arc::new(super::super::shared_admission_tests::run_scoped(
        &store,
        &mut db,
        "run-unlock-waiter",
        "session-unlock-waiter",
        "environment-unlock-waiter",
        current.original.environment_profile.clone(),
    ));
    let one = limits(&current).global;
    let two = one.checked_add(one).unwrap();
    let capacity = ResidentCapacity {
        global: two,
        tenant: two,
        host: two,
    };
    let physical = Arc::new(Mutex::new(Vec::new()));
    let generation_before: i64 = db
        .query_one(
            "SELECT generation FROM runtime.resident_capacity_clock",
            &[],
        )
        .unwrap()
        .get(0);
    let after_global = Arc::new(Mutex::new(None));
    let hook_generation = after_global.clone();
    let hook_store = store.clone();
    let hook_run = current.clone();
    after_global_preparation_unlock(move || {
        let mut observer = Client::connect(&url, NoTls).unwrap();
        *hook_generation.lock().unwrap() = Some(
            observer
                .query_one(
                    "SELECT generation FROM runtime.resident_capacity_clock",
                    &[],
                )
                .unwrap()
                .get::<_, i64>(0),
        );
        let profile = hook_run.original.environment_profile.digest().unwrap();
        let error = hook_store
            .prepare_retained_instance(
                &hook_run.request("execution-unlock-owner", &profile),
                capacity,
                || panic!("the environment gate remains owned after global unlock"),
                |_, _| panic!("no duplicate fact"),
                |_| panic!("no duplicate physical work"),
            )
            .unwrap_err();
        assert_eq!(error, crate::resident_capacity::HOST_PREPARATION_WAIT);
        assert_eq!(
            observer
                .query_one(
                    "SELECT last_error FROM runtime.runtime_jobs WHERE job_id=$1",
                    &[&hook_run.fence.job_id]
                )
                .unwrap()
                .get::<_, Option<String>>(0)
                .as_deref(),
            Some(crate::resident_capacity::HOST_PREPARATION_WAIT)
        );
    });
    let profile = current.original.environment_profile.digest().unwrap();
    store
        .prepare_retained_instance_then_initialize(
            &current.request("execution-unlock-owner", &profile),
            capacity,
            || {
                Ok(crate::resident_capacity::ResidentInventory::declarations(
                    Vec::new(),
                ))
            },
            |_, _| Ok(()),
            |allocation| {
                let waiting_profile = waiting.original.environment_profile.digest().unwrap();
                assert_eq!(
                    store
                        .prepare_retained_instance(
                            &waiting.request("execution-unlock-waiter", &waiting_profile),
                            capacity,
                            || panic!("the first owner still holds global admission"),
                            |_, _| panic!("no waiter fact"),
                            |_| panic!("no waiter container")
                        )
                        .unwrap_err(),
                    crate::resident_capacity::HOST_PREPARATION_WAIT
                );
                confirm(&current, allocation, &physical)
            },
            |_| Ok(()),
        )
        .unwrap();
    let after_global = after_global.lock().unwrap().unwrap();
    let final_generation: i64 = db
        .query_one(
            "SELECT generation FROM runtime.resident_capacity_clock",
            &[],
        )
        .unwrap()
        .get(0);
    assert!(
        after_global > generation_before,
        "a global waiter must observe the first unlock hint"
    );
    assert!(final_generation > after_global, "a registration that rechecked while the environment was held must observe its final unlock");
    assert_eq!(physical.lock().unwrap().len(), 1);
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_initialization_rechecks_controller_and_job_after_resident_commit() {
    use crate::postgres_store::residency::after_global_preparation_unlock;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    for change in ["controller", "job"] {
        let (url, store, mut db) = setup();
        let current = run(&store, &mut db, "run-repin-initialize", 1);
        let physical = Arc::new(Mutex::new(Vec::new()));
        after_global_preparation_unlock(move || {
            let mut authority = Client::connect(&url, NoTls).unwrap();
            if change == "controller" {
                authority.execute("UPDATE public.app_core_sessionenvironment SET controller_epoch=controller_epoch+1 WHERE id='environment'", &[]).unwrap();
            } else {
                authority.execute("UPDATE runtime.runtime_jobs SET lease_owner='reclaimed-owner' WHERE job_id='agent_run.lifecycle:run-repin-initialize'", &[]).unwrap();
            }
        });
        let calls = AtomicUsize::new(0);
        let profile = current.original.environment_profile.digest().unwrap();
        assert!(store
            .prepare_retained_instance_then_initialize(
                &current.request("execution-repin-initialize", &profile),
                limits(&current),
                || Ok(crate::resident_capacity::ResidentInventory::declarations(
                    Vec::new()
                )),
                |_, _| Ok(()),
                |allocation| confirm(&current, allocation, &physical),
                |_| {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            )
            .is_err());
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "changed authority cannot dispatch initialization"
        );
        assert_eq!(physical.lock().unwrap().len(), 1);
        assert_eq!(db.query_one("SELECT state FROM runtime.resident_sandboxes WHERE execution_id='execution-repin-initialize'", &[]).unwrap().get::<_, String>(0), "resident");
        assert_eq!(db.query_one("SELECT state FROM runtime.environment_creation_attempts WHERE allocation_execution_id='execution-repin-initialize'", &[]).unwrap().get::<_, String>(0), "dispatched");
    }
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_cleanup_during_initialization_declines_before_global_inventory_and_retries() {
    use crate::contract::AgentRunCleanup;
    use crate::postgres_store::cleanup_receipts::CleanupAuthorityRequest;
    use crate::postgres_store::residency::after_global_preparation_unlock;
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (url, store, mut db) = setup();
    let current = Arc::new(run(&store, &mut db, "run-cleanup-initializing", 1));
    let physical = Arc::new(Mutex::new(Vec::new()));
    let cleanup_record = Arc::new(Mutex::new(None));
    let hook_cleanup = cleanup_record.clone();
    let hook_run = current.clone();
    let hook_store = store.clone();
    after_global_preparation_unlock(move || {
        let mut authority = Client::connect(&url, NoTls).unwrap();
        authority.execute("UPDATE public.app_core_sessionenvironment SET controller_purpose='cleanup' WHERE id='environment'", &[]).unwrap();
        authority.execute("INSERT INTO public.app_core_sessionevent VALUES('ws_1','sess_1','run-cleanup-initializing',false,'{\"type\":\"agent_run_failed\"}')", &[]).unwrap();
        let mut binding = hook_run.grant.payload.clone();
        binding.controller.as_mut().unwrap().controller_purpose = "cleanup".into();
        let cleanup = AgentRunCleanup {
            schema: "workspace.agent_run.cleanup.v1".into(),
            agent_run_id: hook_run.original.agent_run_id.clone(),
            authorization_digest: hook_run.original.digest().unwrap(),
            authorization_signature: hook_run.original.signature(KEY).unwrap(),
            authorization: hook_run.original.clone(),
            environment_authorization: sign(binding),
        };
        let request = CleanupAuthorityRequest {
            cleanup: &cleanup,
            signing_key: KEY,
            lease: &hook_run.fence,
            host_id: "host",
        };
        let error = hook_store
            .with_cleanup_authority(
                &request,
                || panic!("cleanup must decline at the environment gate before global inventory"),
                |_, _| -> Result<(), String> {
                    panic!("cleanup cannot physically enter an initializing environment")
                },
            )
            .unwrap_err();
        assert_eq!(error, crate::resident_capacity::RESIDENT_CAPACITY_WAIT);
        assert!(authority
            .query_one("SELECT pg_try_advisory_lock(731946,3)", &[])
            .unwrap()
            .get::<_, bool>(0));
        authority
            .query_one("SELECT pg_advisory_unlock(731946,3)", &[])
            .unwrap();
        *hook_cleanup.lock().unwrap() = Some(cleanup);
    });
    let profile = current.original.environment_profile.digest().unwrap();
    assert!(store
        .prepare_retained_instance_then_initialize(
            &current.request("execution-cleanup-initializing", &profile),
            limits(&current),
            || Ok(crate::resident_capacity::ResidentInventory::declarations(
                Vec::new()
            )),
            |_, _| Ok(()),
            |allocation| confirm(&current, allocation, &physical),
            |_| panic!("changed cleanup authority must prevent execute initialization")
        )
        .is_err());
    let cleanup = cleanup_record.lock().unwrap().take().unwrap();
    let request = CleanupAuthorityRequest {
        cleanup: &cleanup,
        signing_key: KEY,
        lease: &current.fence,
        host_id: "host",
    };
    store
        .with_cleanup_authority_and_power_observation(
            &request,
            || Ok(physical.lock().unwrap().clone()),
            |_, _| Ok(false),
            |_, _| Ok(()),
        )
        .unwrap();
    assert_eq!(db.query_one("SELECT count(*) FROM runtime.agent_run_cleanup_receipts WHERE agent_run_id='run-cleanup-initializing'", &[]).unwrap().get::<_, i64>(0), 1);
    assert_eq!(physical.lock().unwrap().len(), 1);
}
