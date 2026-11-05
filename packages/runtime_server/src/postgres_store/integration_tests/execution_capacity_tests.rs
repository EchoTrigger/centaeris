//! Job row leases preserve ownership without imposing Run concurrency quotas.
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
    assert!(
        replicas[1]
            .schedule_worker_job(
                ScheduleRuntimeJobRequest { job: conflicting },
                Some("foreign-workspace")
            )
            .is_err()
    );
}
