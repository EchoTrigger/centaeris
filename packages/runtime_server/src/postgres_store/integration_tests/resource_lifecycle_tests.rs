use super::*;

const LIFECYCLE: &str = "agent_run.lifecycle";

fn claim(store: &PostgresRuntimeStore, id: &str, now: i64) -> Vec<RuntimeJobRecord> {
    store
        .claim_worker_jobs(ClaimDueRuntimeJobsRequest {
            now_ms: now,
            worker_id: "capacity-cancellation-observer".into(),
            job_id: Some(id.into()),
            job_kind: Some(LIFECYCLE.into()),
            session_id: None,
            limit: 1,
            lease_ms: 600_000,
        })
        .unwrap()
}

fn schedule_run(
    store: &PostgresRuntimeStore,
    db: &mut Client,
    run: &str,
    workspace: &str,
) -> String {
    let session = format!("session-{run}");
    db.execute(
        "INSERT INTO public.app_core_session VALUES($1,$2)",
        &[&session, &workspace],
    )
    .unwrap();
    db.execute(
        "INSERT INTO public.app_core_agentrun(id,session_id) VALUES($1,$2)",
        &[&run, &session],
    )
    .unwrap();
    let id = format!("agent_run.lifecycle:{run}");
    let mut record = job(&id, &format!("{id}:digest"));
    record.job_kind = LIFECYCLE.into();
    record.session_id = Some(session);
    record.payload_ref = Some(format!("record:agent_run:{run}"));
    store
        .schedule_worker_job(ScheduleRuntimeJobRequest { job: record }, Some(workspace))
        .unwrap();
    id
}

fn cancellation_at_capacity(global_full: bool) {
    let limits = crate::execution_capacity::ExecutionCapacity::from_env().unwrap();
    assert_eq!((limits.global, limits.tenant), (8, 4));
    let url = test_url();
    reset_store(&url);
    let store = PostgresRuntimeStore::new(&url).unwrap();
    let mut db = Client::connect(&url, NoTls).unwrap();
    db.batch_execute(
        "DROP TABLE IF EXISTS public.app_core_sessionevent,public.app_core_session CASCADE;
         CREATE TABLE public.app_core_session(id text PRIMARY KEY,workspace_id text NOT NULL);
         CREATE TABLE public.app_core_sessionevent(agent_run_id text,session_level boolean NOT NULL);",
    )
    .unwrap();
    let now: i64 = db
        .query_one(
            "SELECT (EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint",
            &[],
        )
        .unwrap()
        .get(0);
    let count = if global_full {
        limits.global
    } else {
        limits.tenant
    };
    let mut blockers = Vec::new();
    for index in 0..count {
        let workspace = if index < limits.tenant {
            "workspace-a"
        } else {
            "workspace-b"
        };
        let id = schedule_run(&store, &mut db, &format!("blocker-{index}"), workspace);
        let lease = claim(&store, &id, now).remove(0);
        store
            .start_runtime_job(StartRuntimeJobRequest {
                job_id: id.clone(),
                lease_owner: lease.lease_owner.clone().unwrap(),
                started_at_ms: now,
            })
            .unwrap();
        blockers.push((id, lease.lease_owner));
    }
    // An empty third Workspace isolates the global cap from the tenant cap.
    let workspace = if global_full {
        "workspace-c"
    } else {
        "workspace-a"
    };
    let target = schedule_run(&store, &mut db, "cancel-target", workspace);
    assert!(claim(&store, &target, now).is_empty());
    assert!(store
        .request_agent_run_cancellation("cancel-target", "session-cancel-target", "digest", now)
        .unwrap());
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.runtime_events WHERE event_id='agent_run_cancel_requested:cancel-target'",
            &[],
        )
        .unwrap()
        .get::<_, i64>(0),
        1
    );
    let ready = store
        .wait_for_runtime_jobs(&[LIFECYCLE.into()], Duration::from_millis(10))
        .unwrap();
    let control_candidates = claim(&store, &target, now);
    for (id, owner) in blockers {
        let retained = store.get_runtime_job(&id).unwrap().unwrap();
        assert_eq!(retained.status, RuntimeJobStatus::Running);
        assert_eq!(retained.lease_owner, owner);
    }
    assert_eq!(
        db.query_one("SELECT count(*) FROM runtime.execution_instances", &[])
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    assert!(
        ready.ready && control_candidates.len() == 1,
        "persisted cancellation must remain ready and claimable while all {count} execution owners stay running: ready={}, claimed={}",
        ready.ready,
        control_candidates.len()
    );
}

#[test]
#[ignore = "requires owned disposable PostgreSQL and default execution limits"]
fn postgres_queued_cancellation_is_claimable_when_workspace_execution_capacity_is_full() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    cancellation_at_capacity(false);
}

#[test]
#[ignore = "requires owned disposable PostgreSQL and default execution limits"]
fn postgres_queued_cancellation_is_claimable_when_global_execution_capacity_is_full() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    cancellation_at_capacity(true);
}

#[test]
#[ignore = "requires owned disposable PostgreSQL"]
fn postgres_failed_before_create_releases_the_committed_reservation() {
    use super::retained_instance_tests as fixture;
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (_url, store, mut db) = fixture::setup();
    let run = fixture::run(&store, &mut db, "before-create-failure", 1);
    let profile = run.original.environment_profile.digest().unwrap();
    let failure = store
        .prepare_retained_instance(
            &run.request("never-dispatched", &profile),
            fixture::limits(&run),
            || Ok(vec![]),
            |_, _| {
                assert_eq!(
                    db.query_one("SELECT count(*) FROM runtime.resident_sandboxes", &[])
                        .unwrap()
                        .get::<_, i64>(0),
                    1,
                    "reservation must already be committed before the injected failure"
                );
                Err("injected pre-dispatch failure".into())
            },
            |_| panic!("physical creation must not run after the pre-dispatch failure"),
        )
        .unwrap_err();
    assert_eq!(failure, "injected pre-dispatch failure");
    assert_eq!(
        db.query_one("SELECT count(*) FROM runtime.execution_instances", &[])
            .unwrap()
            .get::<_, i64>(0),
        1,
        "settlement must preserve the allocation history"
    );
    assert_eq!(
        db.query_one("SELECT count(*) FROM runtime.resident_sandboxes", &[])
            .unwrap()
            .get::<_, i64>(0),
        0,
        "a committed reservation with proof that create was never called must settle"
    );
}

#[test]
#[ignore = "requires owned disposable PostgreSQL"]
fn postgres_prepare_cleanup_without_transmitted_evidence_keeps_the_reservation() {
    use super::retained_instance_tests as fixture;
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (_url, store, mut db) = fixture::setup();
    let run = fixture::run(&store, &mut db, "removed-prepare-failure", 1);
    let profile = run.original.environment_profile.digest().unwrap();
    let mut physical = Vec::<String>::new();
    let mut created = 0;
    let mut removed = 0;
    let failure = store
        .prepare_retained_instance(
            &run.request("removed-before-bind", &profile),
            fixture::limits(&run),
            || Ok(vec![]),
            |_, _| Ok(()),
            |allocation| {
                assert!(!allocation.reused);
                assert!(allocation.container_id.is_none());
                // Model a synchronous host attempt whose start fails after create.
                // The exact object is removed before this attempt returns; unlike
                // the unknown-result regressions, no request remains in flight.
                let id = format!("removed-{}", allocation.instance_id);
                created += 1;
                physical.push(id.clone());
                assert_eq!(physical.pop(), Some(id));
                removed += 1;
                Err("injected preparation failure".into())
            },
        )
        .unwrap_err();
    assert_eq!(failure, "injected preparation failure");
    assert_eq!((created, removed), (1, 1));
    assert!(physical.is_empty());
    assert_eq!(
        db.query_one("SELECT count(*) FROM runtime.execution_instances", &[])
            .unwrap()
            .get::<_, i64>(0),
        1,
        "the removed attempt still needs its immutable allocation identity"
    );
    assert_eq!(
        db.query_one("SELECT count(*) FROM runtime.resident_sandboxes", &[])
            .unwrap()
            .get::<_, i64>(0),
        1,
        "a string failure cannot prove removal; keep the charge until structured evidence reaches the ledger"
    );
    assert_eq!(
        db.query_one(
            "SELECT state FROM runtime.environment_creation_attempts",
            &[]
        )
        .unwrap()
        .get::<_, String>(0),
        "dispatched"
    );
    assert!(
        db.execute(
            "UPDATE runtime.environment_creation_attempts SET state='notDispatched'",
            &[]
        )
        .is_err(),
        "a possible dispatch must never be rewritten as non-dispatch"
    );
}

fn failed_creation_cleanup(
    db: &mut Client,
    run: &mut super::retained_instance_tests::Run,
) -> crate::contract::AgentRunCleanup {
    use super::retained_instance_tests as fixture;
    let mut binding = run.grant.payload.clone();
    binding.controller.as_mut().unwrap().controller_purpose = "cleanup".into();
    run.grant = fixture::sign(binding);
    db.execute("UPDATE public.app_core_sessionenvironment SET controller_purpose='cleanup' WHERE id='environment'", &[]).unwrap();
    db.execute("INSERT INTO public.app_core_sessionevent VALUES('ws_1','sess_1',$1,false,'{\"type\":\"agent_run_failed\"}')", &[&run.original.agent_run_id]).unwrap();
    crate::contract::AgentRunCleanup {
        schema: "workspace.agent_run.cleanup.v1".into(),
        agent_run_id: run.original.agent_run_id.clone(),
        authorization_digest: run.original.digest().unwrap(),
        authorization_signature: run.original.signature(fixture::KEY).unwrap(),
        authorization: run.original.clone(),
        environment_authorization: run.grant.clone(),
    }
}

#[test]
#[ignore = "requires owned disposable PostgreSQL"]
fn postgres_reused_resident_preparation_failure_keeps_original_allocation() {
    use super::retained_instance_tests as fixture;
    use crate::resident_capacity::ObservedResidentSandbox;
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_url, store, mut db) = fixture::setup();
    let a = fixture::run(&store, &mut db, "resident-origin", 1);
    let profile = a.original.environment_profile.digest().unwrap();
    let original = store
        .prepare_retained_instance(
            &a.request("resident-origin-execution", &profile),
            fixture::limits(&a),
            || Ok(vec![]),
            |_, _| Ok(()),
            |_| Ok("a".repeat(64)),
        )
        .unwrap();
    let b = fixture::run(&store, &mut db, "resident-reuser", 2);
    let error = store
        .prepare_retained_instance(
            &b.request("resident-reuser-execution", &profile),
            fixture::limits(&b),
            || {
                Ok(vec![ObservedResidentSandbox {
                    execution_id: original.allocation_execution_id.clone(),
                    allocation_owner: original.allocation_owner.clone(),
                    container_id: original.container_id.clone().unwrap(),
                    resources: b.request("resident-reuser-execution", &profile).resources,
                }])
            },
            |_, allocation| {
                assert!(allocation.reused);
                assert_eq!(allocation.instance_id, original.instance_id);
                Err("injected pre-dispatch failure".into())
            },
            |_| panic!("failed callback must not touch the retained instance"),
        )
        .unwrap_err();
    assert_eq!(error, "injected pre-dispatch failure");
    let holding = db
        .query_one(
            "SELECT execution_id,container_id,state FROM runtime.resident_sandboxes",
            &[],
        )
        .unwrap();
    assert_eq!(
        holding.get::<_, String>(0),
        original.allocation_execution_id
    );
    assert_eq!(holding.get::<_, String>(1), original.container_id.unwrap());
    assert_eq!(holding.get::<_, String>(2), "resident");
    assert_eq!(
        db.query_one("SELECT count(*) FROM runtime.execution_instances", &[])
            .unwrap()
            .get::<_, i64>(0),
        2
    );
}

#[test]
#[ignore = "requires owned disposable PostgreSQL"]
fn postgres_unmaterialized_allocation_cleanup_replays_and_next_run_can_create() {
    use super::retained_instance_tests as fixture;
    use crate::postgres_store::cleanup_receipts::{
        CleanupAllocation, CleanupAuthorityRequest, CleanupTarget,
    };
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    for mode in ["retained", "perRun"] {
        let (url, store, mut db) = fixture::setup();
        let mut a = fixture::run(&store, &mut db, "unmaterialized-origin", 1);
        let mut binding = a.grant.payload.clone();
        binding.execution_mode = mode.into();
        a.grant = fixture::sign(binding);
        let profile = a.original.environment_profile.digest().unwrap();
        let error = store
            .prepare_retained_instance(
                &a.request("unmaterialized-execution", &profile),
                fixture::limits(&a),
                || Ok(vec![]),
                |_, _| Err("injected pre-dispatch failure".into()),
                |_| panic!("unmaterialized creation must not dispatch"),
            )
            .unwrap_err();
        assert_eq!(error, "injected pre-dispatch failure");
        drop(store);
        let store = PostgresRuntimeStore::new(&url).unwrap();
        db.batch_execute("ALTER TABLE public.app_core_sessionenvironment ADD COLUMN controller_maintenance_operation_id text").unwrap();
        store
            .with_environment_observation("sess_1", "environment", |target| {
                assert!(!target.resource_holding);
                assert!(!target.destroyed);
                assert!(
                    target.allocation.is_some(),
                    "allocation history must remain visible"
                );
                assert_eq!(
                    crate::docker_execution_host::observe_environment(target),
                    "notMaterialized"
                );
                Ok(())
            })
            .unwrap();
        let instance: String = db
            .query_one("SELECT instance_id FROM runtime.execution_instances", &[])
            .unwrap()
            .get(0);
        let cleanup = failed_creation_cleanup(&mut db, &mut a);
        let request = CleanupAuthorityRequest {
            cleanup: &cleanup,
            signing_key: fixture::KEY,
            lease: &a.fence,
            host_id: "host",
        };
        let receipt = store
            .with_cleanup_authority(
                &request,
                || Ok(vec![]),
                |target, _| {
                    assert!(!matches!(
                        target,
                        CleanupTarget::NeverAllocated | CleanupTarget::Instance { .. }
                    ));
                    Ok(())
                },
            )
            .unwrap();
        assert_eq!(
            receipt.allocation,
            CleanupAllocation::Instance {
                instance_id: instance
            }
        );
        assert!(receipt.cleanup_complete);
        drop(store);
        let store = PostgresRuntimeStore::new(&url).unwrap();
        let replay = store
            .with_cleanup_authority(
                &request,
                || {
                    panic!(
                        "completed unmaterialized cleanup must replay without physical observation"
                    )
                },
                |_, _| -> Result<(), String> { panic!("completed cleanup must not run again") },
            )
            .unwrap();
        assert_eq!(receipt, replay);
        let b = fixture::run(&store, &mut db, "later-run", 2);
        let next = store
            .prepare_retained_instance(
                &b.request("later-execution", &profile),
                fixture::limits(&b),
                || Ok(vec![]),
                |_, _| Ok(()),
                |_| Ok("b".repeat(64)),
            )
            .unwrap();
        assert!(!next.reused);
        assert_ne!(
            next.instance_id,
            match receipt.allocation {
                CleanupAllocation::Instance { instance_id } => instance_id,
                _ => unreachable!(),
            }
        );
        assert_eq!(
            db.query_one("SELECT count(*) FROM runtime.resident_sandboxes", &[])
                .unwrap()
                .get::<_, i64>(0),
            1
        );
    }
}

#[test]
#[ignore = "requires owned disposable PostgreSQL"]
fn postgres_creation_attempt_forward_migration_keeps_historical_unknown_charged() {
    use super::retained_instance_tests as fixture;
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db) = fixture::setup();
    let a = fixture::run(&store, &mut db, "historical-unknown", 1);
    let profile = a.original.environment_profile.digest().unwrap();
    store
        .prepare_retained_instance(
            &a.request("historical-execution", &profile),
            fixture::limits(&a),
            || Ok(vec![]),
            |_, _| Ok(()),
            |_| Err("unknown create acknowledgement".into()),
        )
        .unwrap_err();
    drop(store);
    // Reconstruct the released schema with an evidence-free historical reservation.
    super::super::creation_attempts::restore_previous_fixture(&mut db);
    let store = PostgresRuntimeStore::new(&url).unwrap();
    assert_eq!(
        db.query_one("SELECT max(version) FROM runtime.schema_migrations", &[])
            .unwrap()
            .get::<_, i64>(0),
        14
    );
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.environment_creation_attempts",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        0,
        "migration cannot invent dispatch or settlement evidence"
    );
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.resident_sandboxes WHERE state='reserved'",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        1
    );
    assert!(store
        .prepare_retained_instance(
            &a.request("historical-execution", &profile),
            fixture::limits(&a),
            || Ok(vec![]),
            |_, _| panic!("historical unknown cannot restart preparation"),
            |_| panic!("historical unknown cannot dispatch again"),
        )
        .is_err());
}

#[test]
#[ignore = "requires owned disposable PostgreSQL"]
fn postgres_pre_dispatch_settlement_and_resource_release_are_atomic() {
    use super::retained_instance_tests as fixture;
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_url, store, mut db) = fixture::setup();
    let a = fixture::run(&store, &mut db, "settlement-rollback", 1);
    let profile = a.original.environment_profile.digest().unwrap();
    db.batch_execute("CREATE FUNCTION public.reject_test_release() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected resource release failure'; END $$;
        CREATE TRIGGER reject_test_release BEFORE DELETE ON runtime.resident_sandboxes FOR EACH ROW EXECUTE FUNCTION public.reject_test_release();").unwrap();
    assert!(store
        .prepare_retained_instance(
            &a.request("rollback-execution", &profile),
            fixture::limits(&a),
            || Ok(vec![]),
            |_, _| Err("injected before-create failure".into()),
            |_| panic!("failed preparation must not create"),
        )
        .is_err());
    assert_eq!(
        db.query_one(
            "SELECT state FROM runtime.environment_creation_attempts",
            &[]
        )
        .unwrap()
        .get::<_, String>(0),
        "prepared",
        "a failed resource release must roll back the settlement evidence too"
    );
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.resident_sandboxes WHERE state='reserved'",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        1
    );
    db.batch_execute("DROP TRIGGER reject_test_release ON runtime.resident_sandboxes; DROP FUNCTION public.reject_test_release();").unwrap();
}

#[test]
#[ignore = "requires owned disposable PostgreSQL"]
fn postgres_pre_dispatch_failure_cannot_settle_after_controller_takeover() {
    use super::retained_instance_tests as fixture;
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db) = fixture::setup();
    let a = fixture::run(&store, &mut db, "stale-before-create", 1);
    let profile = a.original.environment_profile.digest().unwrap();
    assert!(store
        .prepare_retained_instance(
            &a.request("stale-execution", &profile),
            fixture::limits(&a),
            || Ok(vec![]),
            |_, _| {
                db.execute(
                    "UPDATE public.app_core_sessionenvironment SET controller_epoch=2",
                    &[],
                )
                .unwrap();
                Err("injected pre-dispatch failure".into())
            },
            |_| panic!("a stale preparation cannot dispatch"),
        )
        .is_err());
    drop(store);
    let _reopened = PostgresRuntimeStore::new(&url).unwrap();
    assert_eq!(
        db.query_one(
            "SELECT state FROM runtime.environment_creation_attempts",
            &[]
        )
        .unwrap()
        .get::<_, String>(0),
        "prepared"
    );
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.resident_sandboxes WHERE state='reserved'",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        1,
        "the expired controller cannot settle the current owner's resources"
    );
}
