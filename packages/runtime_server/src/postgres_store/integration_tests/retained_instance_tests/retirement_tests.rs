use super::*;
use crate::postgres_store::residency::retirement::{RetirementError, RetirementRequest};

fn request(a: &RetainedInstanceAllocation, epoch: i64) -> RetirementRequest {
    RetirementRequest {
        schema: "runtime.environment.retire.v1".into(),
        operation_id: "maintenance-1".into(),
        environment_id: a.environment_id.clone(),
        instance_id: a.instance_id.clone(),
        host_id: a.host_id.clone(),
        expected_controller_epoch: epoch,
        acknowledge_tmpfs_loss: true,
    }
}

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_retirement_pending_fences_current_and_new_runs_until_exact_absence() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (url, store, mut db) = setup();
    let a = run(&store, &mut db, "run-a", 1);
    let physical = Arc::new(Mutex::new(vec![]));
    let allocation = prepare(&store, &a, "execution-a", &physical).unwrap();
    let r = request(&allocation, 1);
    let first = store
        .retire_retained_instance(&r, |_| Err("Docker unobservable".into()))
        .unwrap();
    assert_eq!(first.status, "pending");
    assert_eq!(
        db.query_one("SELECT state FROM runtime.resident_sandboxes", &[])
            .unwrap()
            .get::<_, String>(0),
        "releasing"
    );
    let profile = a.original.environment_profile.digest().unwrap();
    assert!(store
        .with_retained_instance_operation(&a.request("execution-a", &profile), &allocation, |_| Ok(
            ()
        ))
        .unwrap_err()
        .contains("environment retiring"));
    // Independent maintenance survives revocation of the creating user's ACL.
    db.execute("DELETE FROM public.app_core_workspacemembership", &[])
        .unwrap();
    let second = store
        .retire_retained_instance(&r, |target| {
            assert_eq!(target.instance_id, allocation.instance_id);
            assert_eq!(
                target.container_id,
                allocation.container_id.clone().unwrap()
            );
            physical.lock().unwrap().clear();
            Ok(true)
        })
        .unwrap();
    assert_eq!(second.status, "destroyed");
    assert_eq!(
        db.query_one("SELECT count(*) FROM runtime.resident_sandboxes", &[])
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    assert_eq!(
        db.query_one("SELECT count(*) FROM runtime.execution_instances", &[])
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    let receipt_sql = "SELECT row_to_json(receipt)::text FROM runtime.environment_retirements receipt WHERE environment_id=$1 AND operation_id=$2";
    let receipt_before = db
        .query_one(receipt_sql, &[&r.environment_id, &r.operation_id])
        .unwrap()
        .get::<_, String>(0);
    drop(store);
    let store = PostgresRuntimeStore::new(&url).unwrap();
    assert_eq!(
        db.query_one(receipt_sql, &[&r.environment_id, &r.operation_id])
            .unwrap()
            .get::<_, String>(0),
        receipt_before
    );
    assert_eq!(
        store
            .retire_retained_instance(&r, |_| panic!("replay after reopen must not delete again"))
            .unwrap()
            .status,
        "destroyed"
    );
    assert_eq!(
        db.query_one(receipt_sql, &[&r.environment_id, &r.operation_id])
            .unwrap()
            .get::<_, String>(0),
        receipt_before
    );
    let b = run(&store, &mut db, "run-b", 2);
    db.execute(
        "INSERT INTO public.app_core_workspacemembership VALUES('member','ws_1','user_1','member')",
        &[],
    )
    .unwrap();
    assert!(prepare(&store, &b, "execution-b", &physical)
        .unwrap_err()
        .contains("environment retiring"));
    assert_eq!(
        db.query_one("SELECT count(*) FROM runtime.execution_instances", &[])
            .unwrap()
            .get::<_, i64>(0),
        1
    );
}

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_retirement_rejects_stale_epoch_conflicting_identity_and_preserves_history() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (_, store, mut db) = setup();
    let a = run(&store, &mut db, "run-a", 3);
    let physical = Arc::new(Mutex::new(vec![]));
    let allocation = prepare(&store, &a, "execution-a", &physical).unwrap();
    let mut r = request(&allocation, 2);
    assert!(store
        .retire_retained_instance(&r, |_| panic!("stale authority"))
        .is_err());
    r.expected_controller_epoch = 3;
    r.acknowledge_tmpfs_loss = false;
    assert!(store
        .retire_retained_instance(&r, |_| panic!("missing loss acknowledgement"))
        .is_err());
    r.acknowledge_tmpfs_loss = true;
    assert_eq!(
        store
            .retire_retained_instance(&r, |_| Ok(false))
            .unwrap()
            .status,
        "pending"
    );
    let mut conflict = r.clone();
    conflict.operation_id = "different-operation".into();
    assert!(store
        .retire_retained_instance(&conflict, |_| panic!("operation reassignment"))
        .is_err());
    assert!(db
        .execute(
            "UPDATE runtime.environment_retirements SET container_id='foreign'",
            &[]
        )
        .is_err());
    assert!(db
        .execute("DELETE FROM runtime.environment_retirements", &[])
        .is_err());
    physical.lock().unwrap().clear();
    assert_eq!(
        store
            .retire_retained_instance(&r, |_| Ok(true))
            .unwrap()
            .status,
        "destroyed"
    );
    assert!(db
        .execute(
            "UPDATE runtime.environment_retirements SET state='pending'",
            &[]
        )
        .is_err());
}

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_retirement_epoch_conflict_is_unaccepted_and_accepted_replay_is_immutable() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (_, store, mut db) = setup();
    let a = run(&store, &mut db, "run-a", 3);
    let physical = Arc::new(Mutex::new(vec![]));
    let allocation = prepare(&store, &a, "execution-a", &physical).unwrap();
    let debit_sql = "SELECT row_to_json(s)::text FROM runtime.resident_sandboxes s";
    let debit_before = db.query_one(debit_sql, &[]).unwrap().get::<_, String>(0);
    let mut r = request(&allocation, 2);
    let error = store
        .retire_retained_instance(&r, |_| panic!("unaccepted request must not mutate Docker"))
        .unwrap_err();
    assert!(matches!(&error, RetirementError::ControllerEpochConflict));
    assert_eq!(error.to_string(), "environment_retirement_epoch_conflict");
    assert_eq!(
        db.query_one("SELECT count(*) FROM runtime.environment_retirements", &[])
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    assert_eq!(
        db.query_one(debit_sql, &[]).unwrap().get::<_, String>(0),
        debit_before
    );

    // Refreshing an explicitly unaccepted request may keep its operation identity.
    r.expected_controller_epoch = 3;
    assert_eq!(
        store
            .retire_retained_instance(&r, |_| Ok(false))
            .unwrap()
            .status,
        "pending"
    );
    let body_sql = "SELECT request_json FROM runtime.environment_retirements WHERE operation_id=$1";
    let accepted_body = db
        .query_one(body_sql, &[&r.operation_id])
        .unwrap()
        .get::<_, String>(0);
    db.execute(
        "UPDATE public.app_core_sessionenvironment SET controller_epoch=4",
        &[],
    )
    .unwrap();
    let mut changed = r.clone();
    changed.expected_controller_epoch = 4;
    let error = store
        .retire_retained_instance(&changed, |_| panic!("accepted body cannot change"))
        .unwrap_err();
    assert!(matches!(error, RetirementError::Rejected(_)));
    changed.operation_id = "maintenance-2".into();
    let error = store
        .retire_retained_instance(&changed, |_| panic!("accepted identity cannot change"))
        .unwrap_err();
    assert!(matches!(error, RetirementError::Rejected(_)));
    assert_eq!(
        store
            .retire_retained_instance(&r, |_| {
                physical.lock().unwrap().clear();
                Ok(true)
            })
            .unwrap()
            .status,
        "destroyed"
    );
    assert_eq!(
        store
            .retire_retained_instance(&r, |_| panic!("destroyed replay must not mutate Docker"))
            .unwrap()
            .status,
        "destroyed"
    );
    assert_eq!(
        db.query_one(body_sql, &[&r.operation_id])
            .unwrap()
            .get::<_, String>(0),
        accepted_body
    );
    assert_eq!(
        db.query_one("SELECT count(*) FROM runtime.resident_sandboxes", &[])
            .unwrap()
            .get::<_, i64>(0),
        0
    );
}

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_retirement_committed_barrier_is_visible_before_physical_mutation() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (url, store, mut db) = setup();
    let a = run(&store, &mut db, "run-a", 1);
    let physical = Arc::new(Mutex::new(vec![]));
    let allocation = prepare(&store, &a, "execution-a", &physical).unwrap();
    let r = request(&allocation, 1);
    store
        .retire_retained_instance(&r, |_| {
            let reopened = PostgresRuntimeStore::new(&url)?;
            let b = run(&reopened, &mut db, "run-b", 2);
            assert!(prepare(&reopened, &b, "execution-b", &physical).is_err());
            let profile = a.original.environment_profile.digest().unwrap();
            assert!(reopened
                .with_retained_instance_operation(
                    &a.request("execution-a", &profile),
                    &allocation,
                    |_| Ok(())
                )
                .is_err());
            assert_eq!(
                db.query_one("SELECT state FROM runtime.environment_retirements", &[])
                    .unwrap()
                    .get::<_, String>(0),
                "pending"
            );
            Ok(false)
        })
        .unwrap();
    let b = run(&store, &mut db, "run-c", 3);
    assert!(prepare(&store, &b, "execution-c", &physical)
        .unwrap_err()
        .contains("environment retiring"));
}

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_retirement_forward_migration_preserves_existing_allocation_and_release_facts() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (url, store, mut db) = setup();
    let a = run(&store, &mut db, "run-a", 1);
    let physical = Arc::new(Mutex::new(vec![]));
    prepare(&store, &a, "execution-a", &physical).unwrap();
    let before = db
        .query_one(
            "SELECT row_to_json(i)::text FROM runtime.execution_instances i",
            &[],
        )
        .unwrap()
        .get::<_, String>(0);
    let history=db.query("SELECT version,applied_at_ms FROM runtime.schema_migrations WHERE version<=10 ORDER BY version",&[]).unwrap().iter().map(|r|(r.get::<_,i64>(0),r.get::<_,i64>(1))).collect::<Vec<_>>();
    crate::postgres_store::environment_owner_schema::restore_previous_fixture(&mut db);
    db.batch_execute("DROP TABLE runtime.environment_maintenance_completions; DROP FUNCTION runtime.protect_environment_maintenance_completion(); DROP TABLE runtime.environment_retirements; DROP FUNCTION runtime.protect_environment_retirement(); DELETE FROM runtime.schema_migrations WHERE version>=11").unwrap();
    drop(PostgresRuntimeStore::new(&url).unwrap());
    assert_eq!(
        db.query_one(
            "SELECT row_to_json(i)::text FROM runtime.execution_instances i",
            &[]
        )
        .unwrap()
        .get::<_, String>(0),
        before
    );
    assert_eq!(db.query("SELECT version,applied_at_ms FROM runtime.schema_migrations WHERE version<=10 ORDER BY version",&[]).unwrap().iter().map(|r|(r.get::<_,i64>(0),r.get::<_,i64>(1))).collect::<Vec<_>>(),history);
    assert_eq!(
        db.query_one("SELECT count(*) FROM runtime.resident_sandboxes", &[])
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    db.batch_execute(
        "DROP TRIGGER environment_retirement_facts ON runtime.environment_retirements",
    )
    .unwrap();
    assert!(PostgresRuntimeStore::new(&url)
        .unwrap_err()
        .contains("immutable retirement trigger missing"));
}

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_retirement_confirmed_destroyed_instance_allows_terminal_run_release_receipt() {
    use crate::postgres_store::cleanup_receipts::{
        CleanupAllocation, CleanupAuthorityRequest, CleanupTarget,
    };
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (_, store, mut db) = setup();
    let mut a = run(&store, &mut db, "run-a", 1);
    let physical = Arc::new(Mutex::new(vec![]));
    let allocation = prepare(&store, &a, "execution-a", &physical).unwrap();
    let r = request(&allocation, 1);
    store
        .retire_retained_instance(&r, |_| {
            physical.lock().unwrap().clear();
            Ok(true)
        })
        .unwrap();
    let mut binding = a.grant.payload.clone();
    binding.controller.as_mut().unwrap().controller_purpose = "cleanup".into();
    a.grant = sign(binding);
    db.batch_execute("UPDATE public.app_core_sessionenvironment SET controller_purpose='cleanup'; INSERT INTO public.app_core_sessionevent VALUES('ws_1','sess_1','run-a',false,'{\"type\":\"agent_run_failed\"}')").unwrap();
    let cleanup = crate::contract::AgentRunCleanup {
        schema: "workspace.agent_run.cleanup.v1".into(),
        agent_run_id: "run-a".into(),
        authorization_digest: a.original.digest().unwrap(),
        authorization_signature: a.original.signature(KEY).unwrap(),
        authorization: a.original.clone(),
        environment_authorization: a.grant.clone(),
    };
    let receipt = store
        .with_cleanup_authority(
            &CleanupAuthorityRequest {
                cleanup: &cleanup,
                signing_key: KEY,
                lease: &a.fence,
                host_id: "host",
            },
            || Ok(vec![]),
            |target, _| {
                assert!(!matches!(target, CleanupTarget::Instance { .. }));
                Ok(())
            },
        )
        .unwrap();
    assert_eq!(
        receipt.allocation,
        CleanupAllocation::Instance {
            instance_id: allocation.instance_id
        }
    );
    assert!(receipt.cleanup_complete);
}

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_retirement_uses_reserved_control_pool_when_ordinary_capacity_is_full() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (url, store, mut db) = setup();
    let a = run(&store, &mut db, "run-a", 1);
    let physical = Arc::new(Mutex::new(vec![]));
    let allocation = prepare(&store, &a, "execution-a", &physical).unwrap();
    let r = request(&allocation, 1);
    let limited = PostgresRuntimeStore::new_with_limits(
        &url,
        crate::postgres_store::PostgresConnectionLimits {
            ordinary: 1,
            execution_control: 1,
            listeners: 1,
            checkout_timeout: std::time::Duration::from_millis(100),
            connect_timeout: std::time::Duration::from_secs(5),
        },
    )
    .unwrap();
    limited
        .with_client(|_| {
            assert_eq!(
                limited.retire_retained_instance(&r, |_| Ok(false))?.status,
                "pending"
            );
            Ok(())
        })
        .unwrap();
}

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_retirement_rejects_disabled_predicate_history_trigger_on_reopen() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (url, _, mut db) = setup();
    db.batch_execute("DROP TRIGGER environment_retirement_facts ON runtime.environment_retirements; CREATE TRIGGER environment_retirement_facts BEFORE UPDATE OR DELETE ON runtime.environment_retirements FOR EACH ROW WHEN(false) EXECUTE FUNCTION runtime.protect_environment_retirement()").unwrap();
    assert!(
        PostgresRuntimeStore::new(&url).is_err(),
        "conditional false trigger must not certify immutable history"
    );
}

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_retirement_pending_other_environment_does_not_block_healthy_run_cleanup() {
    use crate::postgres_store::cleanup_receipts::{CleanupAuthorityRequest, CleanupTarget};
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (_, store, mut db) = setup();
    let a = run(&store, &mut db, "run-a", 1);
    let physical = Arc::new(Mutex::new(vec![]));
    let allocation = prepare(&store, &a, "execution-a", &physical).unwrap();
    store
        .retire_retained_instance(&request(&allocation, 1), |_| Ok(false))
        .unwrap();
    let mut b = run(&store, &mut db, "run-b", 2);
    b.original.session_id = "session-b".into();
    let digest = b.original.digest().unwrap();
    db.batch_execute("INSERT INTO public.app_core_session VALUES('session-b','ws_1','user_1','centaeris','environment-b',NULL); INSERT INTO public.app_core_sessionenvironment VALUES('environment-b','session-b','run-b',2,'owner-run-b','execute',false); UPDATE public.app_core_agentrun SET session_id='session-b',environment_id='environment-b' WHERE id='run-b'").unwrap();
    db.execute("UPDATE public.app_core_agentrunauthorization SET digest=$1,payload=$2::text::jsonb,signature=$3 WHERE agent_run_id='run-b'", &[&digest,&serde_json::to_string(&b.original).unwrap(),&b.original.signature(KEY).unwrap()]).unwrap();
    db.execute("UPDATE runtime.runtime_jobs SET session_id='session-b',idempotency_key=$1 WHERE job_id='agent_run.lifecycle:run-b'", &[&format!("agent_run.lifecycle:run-b:{digest}")]).unwrap();
    let mut binding = b.grant.payload.clone();
    binding.session_id = "session-b".into();
    binding.environment_id = "environment-b".into();
    binding.agent_run_authorization_digest = digest;
    b.grant = sign(binding);
    let two = declared_usage(b.original.environment_profile.resources.sandbox_resources())
        .checked_add(declared_usage(
            b.original.environment_profile.resources.sandbox_resources(),
        ))
        .unwrap();
    let profile = b.original.environment_profile.digest().unwrap();
    db.execute("UPDATE public.app_core_sessionenvironment SET environment_profile=$1::text::jsonb,environment_profile_digest=$2 WHERE id='environment-b'", &[&serde_json::to_string(&b.original.environment_profile).unwrap(),&profile]).unwrap();
    let second = store
        .prepare_retained_instance(
            &b.request("execution-b", &profile),
            ResidentCapacity {
                global: two,
                tenant: two,
                host: two,
            },
            || Ok(physical.lock().unwrap().clone()),
            |_, _| Ok(()),
            |allocation| {
                physical.lock().unwrap().push(ObservedResidentSandbox {
                    execution_id: allocation.allocation_execution_id.clone(),
                    allocation_owner: centaeris_core::execution::EnvironmentTaskOwner::AgentRun {
                        run_id: "run-b".into(),
                    },
                    container_id: "container-b".into(),
                    resources: declared_usage(
                        b.original.environment_profile.resources.sandbox_resources(),
                    ),
                });
                Ok("container-b".into())
            },
        )
        .unwrap();
    let mut binding = b.grant.payload.clone();
    binding.controller.as_mut().unwrap().controller_purpose = "cleanup".into();
    b.grant = sign(binding);
    db.batch_execute("UPDATE public.app_core_sessionenvironment SET controller_purpose='cleanup' WHERE id='environment-b'; INSERT INTO public.app_core_sessionevent VALUES('ws_1','session-b','run-b',false,'{\"type\":\"agent_run_failed\"}')").unwrap();
    let cleanup = crate::contract::AgentRunCleanup {
        schema: "workspace.agent_run.cleanup.v1".into(),
        agent_run_id: "run-b".into(),
        authorization_digest: b.original.digest().unwrap(),
        authorization_signature: b.original.signature(KEY).unwrap(),
        authorization: b.original.clone(),
        environment_authorization: b.grant.clone(),
    };
    let receipt=store.with_cleanup_authority(&CleanupAuthorityRequest {cleanup:&cleanup,signing_key:KEY,lease:&b.fence,host_id:"host"},|| Ok(physical.lock().unwrap().clone()),|target,_|{assert!(matches!(target,CleanupTarget::Instance {allocation,..} if allocation.instance_id==second.instance_id));Ok(())}).unwrap();
    assert!(receipt.cleanup_complete);
    assert_eq!(
        db.query_one(
            "SELECT state FROM runtime.resident_sandboxes WHERE environment_id='environment'",
            &[]
        )
        .unwrap()
        .get::<_, String>(0),
        "releasing"
    );
}
