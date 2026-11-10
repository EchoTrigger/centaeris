//! Job row leases preserve ownership without imposing Run concurrency quotas.
use super::retained_instance_tests as fixture;
use super::*;

const LIFECYCLE: &str = "agent_run.lifecycle";

fn schedule(
    store: &PostgresRuntimeStore,
    id: &str,
    workspace: &str,
    status: RuntimeJobStatus,
    lease: Option<(&str, i64)>,
) {
    let mut record = job(id, id);
    record.job_kind = LIFECYCLE.into();
    record.status = status;
    if let Some((owner, deadline)) = lease {
        record.lease_owner = Some(owner.into());
        record.lease_expires_at_ms = Some(deadline);
    }
    store
        .schedule_worker_job(ScheduleRuntimeJobRequest { job: record }, Some(workspace))
        .unwrap();
}

fn claim(store: &PostgresRuntimeStore, worker: &str) -> Vec<RuntimeJobRecord> {
    store
        .claim_worker_jobs(ClaimDueRuntimeJobsRequest {
            now_ms: 1,
            worker_id: worker.into(),
            job_id: None,
            job_kind: Some(LIFECYCLE.into()),
            session_id: None,
            limit: 1,
            lease_ms: 60_000,
        })
        .unwrap()
}

fn queued_run(
    store: &PostgresRuntimeStore,
    db: &mut Client,
    run: &str,
    environment: &str,
) -> String {
    db.execute(
        "INSERT INTO public.app_core_agentrun(id,session_id,environment_id) VALUES($1,'sess_1',$2)",
        &[&run, &environment],
    )
    .unwrap();
    let id = format!("agent_run.lifecycle:{run}");
    let mut record = job(&id, &format!("{id}:fixture"));
    record.job_kind = LIFECYCLE.into();
    record.session_id = Some("sess_1".into());
    record.payload_ref = Some(format!("record:agent_run:{run}"));
    store
        .schedule_worker_job(ScheduleRuntimeJobRequest { job: record }, Some("ws_1"))
        .unwrap();
    id
}

#[test]
#[ignore = "requires owned disposable PostgreSQL"]
fn postgres_dispatch_without_a_count_quota_keeps_host_leases_beyond_eight_starts() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_url, store, mut db) = fixture::setup();
    let host = store
        .clone()
        .with_dispatch_host("resource-host".into(), None)
        .unwrap();
    for index in 0..12 {
        queued_run(
            &store,
            &mut db,
            &format!("resource-run-{index}"),
            &format!("resource-environment-{index}"),
        );
    }
    for index in 0..12 {
        assert!(
            host.wait_for_runtime_jobs(&[LIFECYCLE.into()], Duration::ZERO)
                .unwrap()
                .ready
        );
        let jobs = claim(&host, &format!("worker-{index}"));
        assert_eq!(jobs.len(), 1);
        host.require_execute_lease(&jobs[0].job_id, jobs[0].lease_owner.as_deref().unwrap())
            .unwrap();
    }
    assert!(
        !host
            .wait_for_runtime_jobs(&[LIFECYCLE.into()], Duration::ZERO)
            .unwrap()
            .ready
    );
}

#[test]
#[ignore = "requires owned disposable PostgreSQL"]
fn postgres_dispatch_reserves_one_start_slot_and_another_host_claims_remaining_work() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_url, store, mut db) = fixture::setup();
    let host = store
        .clone()
        .with_dispatch_host("first-host".into(), Some(1))
        .unwrap();
    let other = store
        .clone()
        .with_dispatch_host("second-host".into(), Some(1))
        .unwrap();
    let first = queued_run(&store, &mut db, "dispatch-a", "environment-a");
    let second = queued_run(&store, &mut db, "dispatch-b", "environment-b");
    let third = queued_run(&store, &mut db, "dispatch-c", "environment-c");
    let ready = |s: &PostgresRuntimeStore| {
        s.wait_for_runtime_jobs(&[LIFECYCLE.into()], Duration::ZERO)
            .unwrap()
            .ready
    };
    assert!(ready(&host));
    let (one, two) = std::thread::scope(|scope| {
        let one = scope.spawn(|| claim(&host, "first-worker"));
        let two = claim(&host, "second-worker-on-same-host");
        (one.join().unwrap(), two)
    });
    let mut claims = one.into_iter().chain(two).collect::<Vec<_>>();
    assert_eq!(claims.len(), 1);
    let claimed = claims.pop().unwrap();
    assert_eq!(claimed.job_id, first);
    assert!(!ready(&host));
    assert!(ready(&other));
    assert_eq!(claim(&other, "other-host-worker")[0].job_id, second);
    assert!(!ready(&other));
    let owner = claimed.lease_owner.unwrap();
    host.require_execute_lease(&first, &owner).unwrap();
    assert!(other.require_execute_lease(&first, &owner).is_err());
    let now = db
        .query_one(
            "SELECT (EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint",
            &[],
        )
        .unwrap()
        .get::<_, i64>(0);
    host.yield_runtime_job(YieldRuntimeJobRequest {
        job_id: first,
        lease_owner: owner,
        yielded_at_ms: now,
        run_at_ms: now + 60_000,
        transition_reason: "question_wait".into(),
    })
    .unwrap();
    assert!(ready(&host));
    assert_eq!(claim(&host, "first-host-worker")[0].job_id, third);
}

#[test]
#[ignore = "requires owned disposable PostgreSQL"]
fn postgres_dispatch_pins_existing_residency_and_control_to_its_owning_host() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_url, store, mut db) = fixture::setup();
    let prior = fixture::run(&store, &mut db, "resident-origin", 1);
    fixture::prepare(
        &store,
        &prior,
        "resident-execution",
        &std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
    )
    .unwrap();
    let host = store
        .clone()
        .with_dispatch_host("host".into(), Some(1))
        .unwrap();
    let other = store
        .clone()
        .with_dispatch_host("other-host".into(), Some(1))
        .unwrap();
    let job_id = queued_run(&store, &mut db, "resident-next", "environment");
    assert!(claim(&other, "other-host-worker").is_empty());
    let claimed = claim(&host, "owner-host-worker").pop().unwrap();
    assert_eq!(claimed.job_id, job_id);
    let now = db
        .query_one(
            "SELECT (EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint",
            &[],
        )
        .unwrap()
        .get::<_, i64>(0);
    store
        .yield_runtime_job(YieldRuntimeJobRequest {
            job_id: job_id.clone(),
            lease_owner: claimed.lease_owner.unwrap(),
            yielded_at_ms: now,
            run_at_ms: now,
            transition_reason: "question_wait".into(),
        })
        .unwrap();
    super::super::lifecycle_control::mark_terminal_pending(&mut db, "resident-next").unwrap();
    let control = |store: &PostgresRuntimeStore| {
        store
            .claim_control_jobs(
                ClaimDueRuntimeJobsRequest {
                    now_ms: now,
                    worker_id: "control-worker".into(),
                    job_id: Some(job_id.clone()),
                    job_kind: Some(LIFECYCLE.into()),
                    session_id: None,
                    limit: 1,
                    lease_ms: 60_000,
                },
                "advance",
            )
            .unwrap()
    };
    assert!(!other.control_ready("advance").unwrap());
    assert!(control(&other).is_empty());
    assert!(host.control_ready("advance").unwrap());
    let control_job = control(&host).pop().unwrap();
    let owner = control_job.lease_owner.unwrap();
    assert!(other.start_control_job(&job_id, &owner).is_err());
    host.start_control_job(&job_id, &owner).unwrap();
}

#[test]
#[ignore = "requires owned disposable PostgreSQL"]
fn postgres_execution_claims_are_unique_across_replicas_with_many_active_jobs() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let url = test_url();
    reset_store(&url);
    let replicas = [
        PostgresRuntimeStore::new(&url).unwrap(),
        PostgresRuntimeStore::new(&url).unwrap(),
    ];
    for index in 0..16 {
        schedule(
            &replicas[0],
            &format!("agent_run.lifecycle:independent-{index}"),
            "one-workspace",
            RuntimeJobStatus::Queued,
            None,
        );
    }
    let claimed = std::thread::scope(|scope| {
        let handles = (0..16)
            .map(|index| {
                let store = replicas[index % 2].clone();
                scope.spawn(move || claim(&store, &format!("worker-{index}")))
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .flat_map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(claimed.len(), 16);
    let ids = claimed
        .iter()
        .map(|j| j.job_id.clone())
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(ids.len(), 16);
    schedule(
        &replicas[0],
        "agent_run.lifecycle:next",
        "one-workspace",
        RuntimeJobStatus::Queued,
        None,
    );
    assert!(
        replicas[1]
            .wait_for_runtime_jobs(&[LIFECYCLE.into()], Duration::from_millis(10))
            .unwrap()
            .ready
    );
    assert_eq!(
        claim(&replicas[1], "next-owner")[0].job_id,
        "agent_run.lifecycle:next"
    );
    let mut conflicting = job(
        "agent_run.lifecycle:independent-0",
        "agent_run.lifecycle:independent-0",
    );
    conflicting.job_kind = LIFECYCLE.into();
    assert!(replicas[1]
        .schedule_worker_job(
            ScheduleRuntimeJobRequest { job: conflicting },
            Some("foreign-workspace")
        )
        .is_err());
}
