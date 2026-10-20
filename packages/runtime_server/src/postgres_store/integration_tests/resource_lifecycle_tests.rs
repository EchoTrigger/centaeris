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
}
