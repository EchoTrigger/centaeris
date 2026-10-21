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
         CREATE TABLE public.app_core_sessionevent(agent_run_id text,session_id text,session_level boolean NOT NULL,payload jsonb);",
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
    assert!(
        !ready.ready,
        "ordinary readiness remains capacity constrained"
    );
    assert!(claim(&store, &target, now).is_empty());
    let (status, body) = crate::runtime_http_tests::stored_request(
        store.clone(),
        "POST",
        "/internal/jobs/control/ready",
        br#"{"schema":"runtime.job.control.ready.v1","stage":"advance"}"#,
    );
    assert_eq!(status, 200);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap()["ready"],
        true
    );
    let request = serde_json::to_vec(&serde_json::json!({
        "schema": "runtime.job.control.claim.v1", "workerId": "worker:control-regression",
        "jobId": target, "jobKind": LIFECYCLE, "nowMs": now,
        "leaseMs": 120000, "limit": 1, "stage": "advance"
    }))
    .unwrap();
    let (status, body) = crate::runtime_http_tests::stored_request(
        store.clone(),
        "POST",
        "/internal/jobs/control/claim",
        &request,
    );
    assert_eq!(
        status, 200,
        "control claim must have its own authenticated route"
    );
    let response: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let control_candidates = response["jobs"].as_array().unwrap();
    assert_eq!(control_candidates.len(), 1);
    let owner = control_candidates[0]["leaseOwner"].as_str().unwrap();
    assert_eq!(control_candidates[0]["leasePurpose"], "control");
    assert_eq!(
        db.query_one(
            "SELECT lease_purpose FROM runtime.runtime_jobs WHERE job_id=$1",
            &[&target]
        )
        .unwrap()
        .get::<_, String>(0),
        "control"
    );
    assert!(
        store
            .start_runtime_job(StartRuntimeJobRequest {
                job_id: target.clone(),
                lease_owner: owner.into(),
                started_at_ms: now
            })
            .is_err(),
        "ordinary start must reject a control lease"
    );
    let (status, body) = crate::runtime_http_tests::stored_request(
        store.clone(),
        "POST",
        "/internal/jobs/control/claim",
        &request,
    );
    assert_eq!(status, 200);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap()["jobs"],
        serde_json::json!([])
    );
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
        !ready.ready && control_candidates.len() == 1,
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
        15
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

#[test]
#[ignore = "requires owned disposable PostgreSQL"]
fn postgres_control_cancellation_completes_without_taking_foreign_environment() {
    use super::retained_instance_tests as fixture;
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_url, store, mut db) = fixture::setup();
    let target = fixture::run(&store, &mut db, "control-cancel-target", 1);
    let other = fixture::run(&store, &mut db, "foreign-environment-owner", 2);
    db.batch_execute("ALTER TABLE public.app_core_sessionevent ADD COLUMN sequence integer, ADD COLUMN agent_run_sequence integer;").unwrap();
    db.execute("UPDATE runtime.runtime_jobs SET status='queued',lease_owner=NULL,lease_expires_at_ms=NULL WHERE job_id=$1", &[&target.fence.job_id]).unwrap();
    let grant_digest = target.original.digest().unwrap();
    let now: i64 = db
        .query_one(
            "SELECT (EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint",
            &[],
        )
        .unwrap()
        .get(0);
    store
        .request_agent_run_cancellation(
            &target.original.agent_run_id,
            &target.original.session_id,
            &grant_digest,
            now,
        )
        .unwrap();
    std::env::set_var(
        "AGENT_RUN_AUTHORIZATION_SIGNING_KEY",
        std::str::from_utf8(fixture::KEY).unwrap(),
    );
    let invoke = |path: &str, body: serde_json::Value| {
        let (status, wire) = crate::runtime_http_tests::stored_request(
            store.clone(),
            "POST",
            path,
            &serde_json::to_vec(&body).unwrap(),
        );
        (
            status,
            serde_json::from_slice::<serde_json::Value>(&wire).unwrap(),
        )
    };
    let (status, claimed) = invoke(
        "/internal/jobs/control/claim",
        serde_json::json!({"schema":"runtime.job.control.claim.v1","workerId":"worker:control-terminal-test","jobId":target.fence.job_id,"jobKind":LIFECYCLE,"nowMs":now,"leaseMs":120000,"limit":1,"stage":"advance"}),
    );
    assert_eq!(status, 200, "{claimed}");
    let owner = claimed["jobs"][0]["leaseOwner"].as_str().unwrap();
    let complete = serde_json::json!({"schema":"runtime.job.complete.v1","jobId":target.fence.job_id,"leaseOwner":owner,"completedAtMs":now,"outputRefs":[]});
    assert_eq!(
        invoke("/internal/jobs/complete", complete.clone()).0,
        409,
        "a lease alone cannot complete a lifecycle Job"
    );
    assert_eq!(invoke("/internal/jobs/control/start",serde_json::json!({"schema":"runtime.job.control.start.v1","jobId":target.fence.job_id,"leaseOwner":owner,"atMs":now})).0,200);
    let control = serde_json::json!({"agentRunId":target.original.agent_run_id,"authorizationDigest":grant_digest,"authorizationSignature":target.original.signature(fixture::KEY).unwrap(),"authorization":target.original});
    let execute_start = serde_json::json!({"schema":"workspace.agent_run.start.v1","agentRunId":target.original.agent_run_id,"turnId":"turn-control-probe","prompt":"probe","initialInput":{"type":"userMessage"},"agentInstructions":"","modelContextTokens":131072,"modelMaxOutputTokens":4096,"authorizationDigest":grant_digest,"authorizationSignature":target.original.signature(fixture::KEY).unwrap(),"authorization":target.original,"environmentAuthorization":target.grant,"tailAction":{"type":"append"}});
    let (status, rejected) = invoke(
        "/agent-runs/step",
        serde_json::json!({"schema":"runtime.agent_run.step.v1","jobId":target.fence.job_id,"leaseOwner":owner,"agentRunStart":execute_start}),
    );
    assert_eq!(status, 409, "{rejected}");
    assert_eq!(rejected["error"], "agent_run_execute_lease_required");
    let request = serde_json::json!({"schema":"runtime.agent_run.control.v1","jobId":target.fence.job_id,"leaseOwner":owner,"agentRunControl":control});
    let (status, result) = invoke("/agent-runs/control", request.clone());
    assert_eq!(status, 200, "{result}");
    assert_eq!(result["terminalState"], "cancelled");
    assert_eq!(result["disposition"], "released");
    assert_eq!(
        invoke("/agent-runs/control", request).1,
        result,
        "lost replies replay the same receipts"
    );
    let saved_receipt: String = db
        .query_one(
            "SELECT payload_json FROM runtime.runtime_events WHERE event_id=$1",
            &[&format!(
                "agent_run_no_effects:{}",
                target.original.agent_run_id
            )],
        )
        .unwrap()
        .get(0);
    for invalid in [serde_json::json!({}), {
        let mut receipt: serde_json::Value = serde_json::from_str(&saved_receipt).unwrap();
        receipt["authorizationDigest"] = serde_json::json!(format!("sha256:{}", "f".repeat(64)));
        receipt
    }] {
        db.execute(
            "UPDATE runtime.runtime_events SET payload_json=$2 WHERE event_id=$1",
            &[
                &format!("agent_run_no_effects:{}", target.original.agent_run_id),
                &invalid.to_string(),
            ],
        )
        .unwrap();
        assert_eq!(
            invoke("/internal/jobs/complete", complete.clone()).0,
            409,
            "missing or foreign no-effects facts cannot authorize completion"
        );
    }
    db.execute(
        "UPDATE runtime.runtime_events SET payload_json=$2 WHERE event_id=$1",
        &[
            &format!("agent_run_no_effects:{}", target.original.agent_run_id),
            &saved_receipt,
        ],
    )
    .unwrap();
    assert_eq!(invoke("/internal/jobs/complete", complete).0, 200);
    let environment=db.query_one("SELECT controller_run_id,controller_epoch,controller_lease_owner,controller_released FROM public.app_core_sessionenvironment WHERE id='environment'",&[]).unwrap();
    assert_eq!(environment.get::<_, String>(0), other.original.agent_run_id);
    assert_eq!(environment.get::<_, i64>(1), 2);
    assert_eq!(environment.get::<_, String>(2), other.fence.lease_owner);
    assert!(!environment.get::<_, bool>(3));
    assert_eq!(
        db.query_one("SELECT count(*) FROM public.app_core_sessionevent", &[])
            .unwrap()
            .get::<_, i64>(0),
        0,
        "never admitted cancellation must not invent Core history"
    );
    assert_eq!(
        db.query_one("SELECT count(*) FROM runtime.execution_instances", &[])
            .unwrap()
            .get::<_, i64>(0),
        0
    );
}

#[test]
#[ignore = "requires owned disposable PostgreSQL"]
fn postgres_control_leases_require_intent_bound_capacity_and_survive_reclaim_exhaustion() {
    use super::retained_instance_tests as fixture;
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_url, store, mut db) = fixture::setup();
    let now: i64 = db
        .query_one(
            "SELECT (EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint",
            &[],
        )
        .unwrap()
        .get(0);
    let mut runs = vec![];
    for index in 0..6 {
        let run = fixture::run(
            &store,
            &mut db,
            &format!("control-budget-{index}"),
            index + 1,
        );
        db.execute("UPDATE runtime.runtime_jobs SET status='queued',lease_owner=NULL,lease_expires_at_ms=NULL WHERE job_id=$1", &[&run.fence.job_id]).unwrap();
        runs.push(run);
    }
    let claim = |stage: &str| {
        store
            .claim_control_jobs(
                ClaimDueRuntimeJobsRequest {
                    now_ms: now,
                    worker_id: "worker:control-budget-test".into(),
                    job_id: None,
                    job_kind: Some(LIFECYCLE.into()),
                    session_id: None,
                    limit: 2,
                    lease_ms: 120000,
                },
                stage,
            )
            .unwrap()
    };
    assert!(!store.control_ready("advance").unwrap());
    assert!(
        claim("advance").is_empty(),
        "a requested purpose alone is not authorization"
    );
    for (index, run) in runs.iter().enumerate() {
        store
            .request_agent_run_cancellation(
                &run.original.agent_run_id,
                &run.original.session_id,
                &run.original.digest().unwrap(),
                now,
            )
            .unwrap();
        if index >= 3 {
            db.execute("UPDATE public.app_core_agentrun SET \"preAdmissionCancelledAt\"=clock_timestamp() WHERE id=$1",&[&run.original.agent_run_id]).unwrap();
        }
    }
    assert!(store.control_ready("advance").unwrap());
    let short = claim("advance");
    assert_eq!(short.len(), 2);
    assert!(!store.control_ready("advance").unwrap());
    assert!(claim("advance").is_empty());
    assert!(store.control_ready("cleanup").unwrap());
    assert_eq!(claim("cleanup").len(), 2);
    assert!(db.execute("UPDATE runtime.runtime_jobs SET lease_purpose='execute',lease_control_stage=NULL WHERE job_id=$1",&[&short[0].job_id]).is_err(),"an active lease cannot change its purpose");
    let owner = short[0].lease_owner.as_ref().unwrap();
    assert!(store
        .require_execute_lease(&short[0].job_id, owner)
        .is_err());
    assert!(
        store
            .claim_worker_jobs(ClaimDueRuntimeJobsRequest {
                now_ms: now,
                worker_id: "ordinary".into(),
                job_id: None,
                job_kind: Some(LIFECYCLE.into()),
                session_id: None,
                limit: 8,
                lease_ms: 120000
            })
            .unwrap()
            .is_empty(),
        "ordinary claim cannot execute pending control work"
    );
    db.execute("UPDATE runtime.runtime_jobs SET lease_expires_at_ms=$2,lease_reclaim_count=100 WHERE job_id=$1",&[&short[0].job_id,&(now-1)]).unwrap();
    assert_eq!(store.reclaim_expired_runtime_job_leases(now).unwrap(), 1);
    assert_eq!(
        store
            .get_runtime_job(&short[0].job_id)
            .unwrap()
            .unwrap()
            .status,
        RuntimeJobStatus::Queued,
        "control responsibility survives ordinary reclaim exhaustion"
    );
}

#[test]
#[ignore = "requires owned disposable PostgreSQL"]
fn postgres_control_lease_forward_migration_preserves_active_execution_authority() {
    use super::retained_instance_tests as fixture;
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db) = fixture::setup();
    let run = fixture::run(&store, &mut db, "historical-execute-owner", 1);
    let before =
        serde_json::to_value(store.get_runtime_job(&run.fence.job_id).unwrap().unwrap()).unwrap();
    super::super::lifecycle_control::restore_previous_fixture(&mut db);
    let historical: Vec<(i64, i64)> = db
        .query(
            "SELECT version,applied_at_ms FROM runtime.schema_migrations ORDER BY version",
            &[],
        )
        .unwrap()
        .iter()
        .map(|row| (row.get(0), row.get(1)))
        .collect();
    let reopened = PostgresRuntimeStore::new(&url).unwrap();
    assert_eq!(
        serde_json::to_value(
            reopened
                .get_runtime_job(&run.fence.job_id)
                .unwrap()
                .unwrap()
        )
        .unwrap(),
        before
    );
    assert_eq!(
        db.query_one(
            "SELECT lease_purpose,lease_control_stage FROM runtime.runtime_jobs WHERE job_id=$1",
            &[&run.fence.job_id]
        )
        .unwrap()
        .get::<_, String>(0),
        "execute"
    );
    let after:Vec<(i64,i64)>=db.query("SELECT version,applied_at_ms FROM runtime.schema_migrations WHERE version<15 ORDER BY version",&[]).unwrap().iter().map(|row|(row.get(0),row.get(1))).collect();
    assert_eq!(after, historical);
    assert!(reopened
        .require_execute_lease(&run.fence.job_id, &run.fence.lease_owner)
        .is_ok());
    db.batch_execute(
        "ALTER TABLE runtime.runtime_jobs DROP CONSTRAINT runtime_jobs_lease_purpose_check",
    )
    .unwrap();
    assert!(
        PostgresRuntimeStore::new(&url).is_err(),
        "missing purpose enforcement is unsupported schema drift"
    );
}

#[test]
#[ignore = "requires owned disposable PostgreSQL"]
fn postgres_control_cancellation_after_yield_preserves_history_and_requires_cleanup() {
    use super::retained_instance_tests as fixture;
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db) = fixture::setup();
    let run = fixture::run(&store, &mut db, "control-after-yield", 1);
    db.batch_execute(r#"DROP TABLE public.app_core_sessionevent;
    CREATE TABLE public.app_core_sessionevent("eventId" text PRIMARY KEY,workspace_id text NOT NULL,session_id text NOT NULL,agent_run_id text NOT NULL,sequence integer NOT NULL,agent_run_sequence integer,session_level boolean NOT NULL DEFAULT false,projects_to_agent_run_stream boolean NOT NULL,payload jsonb NOT NULL,"createdAtMs" bigint NOT NULL,"insertedAt" timestamptz NOT NULL DEFAULT clock_timestamp(),UNIQUE(session_id,sequence),UNIQUE(agent_run_id,agent_run_sequence));"#).unwrap();
    let now = crate::now_ms().unwrap();
    let digest = run.original.digest().unwrap();
    let mut state =
        centaeris_core::session::AgentRunSessionState::new("sess_1", &run.original.agent_run_id)
            .unwrap();
    let mut records = state
        .start("turn-before-yield", "already admitted", vec![], now)
        .unwrap();
    records.push(
        state
            .start_execution(
                "turn-before-yield",
                "execution-before-yield",
                &digest,
                None,
                now,
            )
            .unwrap(),
    );
    store
        .session_log("ws_1".into(), "sess_1".into(), "already admitted".into())
        .append_session_records_with_runtime_job_lease_blocking(
            &run.original.agent_run_id,
            &records,
            &run.fence,
        )
        .unwrap();
    let before: Vec<String> = db
        .query(
            "SELECT payload::text FROM public.app_core_sessionevent ORDER BY sequence",
            &[],
        )
        .unwrap()
        .iter()
        .map(|row| row.get(0))
        .collect();
    store
        .yield_runtime_job(
            centaeris_core::session::reliability::YieldRuntimeJobRequest {
                job_id: run.fence.job_id.clone(),
                lease_owner: run.fence.lease_owner.clone(),
                yielded_at_ms: now,
                run_at_ms: now,
                transition_reason: "fixture_wait".into(),
            },
        )
        .unwrap();
    store
        .request_agent_run_cancellation(&run.original.agent_run_id, "sess_1", &digest, now)
        .unwrap();
    let store = PostgresRuntimeStore::new(&url).unwrap();
    let lease = store
        .claim_control_jobs(
            ClaimDueRuntimeJobsRequest {
                now_ms: now,
                worker_id: "worker:post-yield-control".into(),
                job_id: Some(run.fence.job_id.clone()),
                job_kind: Some(LIFECYCLE.into()),
                session_id: None,
                limit: 1,
                lease_ms: 120000,
            },
            "advance",
        )
        .unwrap()
        .remove(0);
    let owner = lease.lease_owner.unwrap();
    store.start_control_job(&run.fence.job_id, &owner).unwrap();
    std::env::set_var(
        "AGENT_RUN_AUTHORIZATION_SIGNING_KEY",
        std::str::from_utf8(fixture::KEY).unwrap(),
    );
    let body = serde_json::json!({"schema":"runtime.agent_run.control.v1","jobId":run.fence.job_id,"leaseOwner":owner,"agentRunControl":{"agentRunId":run.original.agent_run_id,"authorizationDigest":digest,"authorizationSignature":run.original.signature(fixture::KEY).unwrap(),"authorization":run.original}});
    let (status, wire) = crate::runtime_http_tests::stored_request(
        store.clone(),
        "POST",
        "/agent-runs/control",
        &serde_json::to_vec(&body).unwrap(),
    );
    let result: serde_json::Value = serde_json::from_slice(&wire).unwrap();
    assert_eq!(status, 200, "{result}");
    assert_eq!(result["terminalState"], "cancelled");
    assert_eq!(result["disposition"], "terminal");
    let after: Vec<String> = db
        .query(
            "SELECT payload::text FROM public.app_core_sessionevent ORDER BY sequence LIMIT 3",
            &[],
        )
        .unwrap()
        .iter()
        .map(|row| row.get(0))
        .collect();
    assert_eq!(before, after);
    assert_eq!(db.query_one("SELECT count(*) FROM public.app_core_sessionevent WHERE payload->>'type'='agent_run_interrupted'",&[]).unwrap().get::<_,i64>(0),1);
    assert_eq!(db.query_one("SELECT count(*) FROM runtime.runtime_events WHERE event_id LIKE 'agent_run_no_effects:%'",&[]).unwrap().get::<_,i64>(0),0,"a yielded Run with history cannot use NoEffectsForRun");
    assert!(
        store
            .complete_runtime_job(
                centaeris_core::session::reliability::CompleteRuntimeJobRequest {
                    job_id: run.fence.job_id,
                    lease_owner: owner,
                    output_refs: vec![],
                    completed_at_ms: crate::now_ms().unwrap()
                }
            )
            .is_err(),
        "terminal alone cannot conceal pending cleanup"
    );
}

#[test]
#[ignore = "requires owned disposable PostgreSQL"]
fn postgres_control_lease_rejects_host_creation_with_prior_execute_grant() {
    use super::retained_instance_tests as fixture;
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_url, store, mut db) = fixture::setup();
    let run = fixture::run(&store, &mut db, "control-host-authority", 1);
    // Keep a previously valid signed execute grant and matching controller fence.
    // Only the current Job lease purpose changes across a new lease boundary.
    db.execute(
        "UPDATE runtime.runtime_jobs SET lease_owner=NULL WHERE job_id=$1",
        &[&run.fence.job_id],
    )
    .unwrap();
    db.execute("UPDATE runtime.runtime_jobs SET lease_owner=$2,lease_purpose='control',lease_control_stage='advance' WHERE job_id=$1",&[&run.fence.job_id,&run.fence.lease_owner]).unwrap();
    let physical_calls = std::sync::atomic::AtomicUsize::new(0);
    let profile = run.original.environment_profile.digest().unwrap();
    let result = store.prepare_retained_instance(
        &run.request("forbidden-control-execution", &profile),
        fixture::limits(&run),
        || Ok(vec![]),
        |_, _| Ok(()),
        |_| {
            physical_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok("a".repeat(64))
        },
    );
    assert!(
        result.is_err(),
        "control lease plus a prior execute grant must not create an instance"
    );
    assert_eq!(physical_calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(
        db.query_one("SELECT count(*) FROM runtime.execution_instances", &[])
            .unwrap()
            .get::<_, i64>(0),
        0
    );
}
