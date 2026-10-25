//! Execution leases are global AgentRun-step permits, independent of Workspace size.
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
#[ignore = "requires owned disposable PostgreSQL and default execution limits"]
fn postgres_execution_capacity_one_workspace_uses_global_permits_with_ready_claim_agreement() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    assert_eq!(
        crate::execution_capacity::ExecutionCapacity::from_env()
            .unwrap()
            .global,
        8
    );
    let url = test_url();
    reset_store(&url);
    let one = PostgresRuntimeStore::new(&url).unwrap();
    let two = PostgresRuntimeStore::new(&url).unwrap();
    for index in 0..9 {
        schedule(
            &one,
            &format!("agent_run.lifecycle:same-workspace-{index}"),
            "one-workspace",
            RuntimeJobStatus::Queued,
            None,
        );
    }
    for index in 0..4 {
        assert_eq!(claim(&one, &format!("first-{index}")).len(), 1);
    }
    let ready = two
        .wait_for_runtime_jobs(&[LIFECYCLE.into()], Duration::from_millis(10))
        .unwrap();
    let fifth = claim(&two, "fifth");
    assert!(
        ready.ready && fifth.len() == 1,
        "Workspace must not cap AgentRun steps at four while global permits remain: ready={}, fifthClaimCount={}",
        ready.ready,
        fifth.len()
    );
    for index in 5..8 {
        assert_eq!(claim(&two, &format!("next-{index}")).len(), 1);
    }
    assert!(claim(&one, "ninth").is_empty());
    assert!(
        !two.wait_for_runtime_jobs(&[LIFECYCLE.into()], Duration::from_millis(10))
            .unwrap()
            .ready
    );
    let mut db = Client::connect(&url, NoTls).unwrap();
    assert_eq!(db.query_one("SELECT count(*) FROM runtime.runtime_jobs WHERE job_kind='agent_run.lifecycle' AND status IN('leased','running') AND lease_purpose='execute'", &[]).unwrap().get::<_,i64>(0),8);
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.execution_job_tenants WHERE workspace_id='one-workspace'",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        9
    );
    let mut conflicting = job(
        "agent_run.lifecycle:same-workspace-0",
        "agent_run.lifecycle:same-workspace-0",
    );
    conflicting.job_kind = LIFECYCLE.into();
    assert!(
        one.schedule_worker_job(
            ScheduleRuntimeJobRequest { job: conflicting },
            Some("foreign-workspace")
        )
        .is_err()
    );
}

#[test]
#[ignore = "requires owned disposable PostgreSQL and default execution limits"]
fn postgres_execution_capacity_terminal_history_does_not_count_but_expired_owners_do_until_yield() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let url = test_url();
    reset_store(&url);
    let one = PostgresRuntimeStore::new(&url).unwrap();
    let two = PostgresRuntimeStore::new(&url).unwrap();
    let mut db = Client::connect(&url, NoTls).unwrap();
    let now: i64 = db
        .query_one(
            "SELECT (EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint",
            &[],
        )
        .unwrap()
        .get(0);
    for index in 0..40 {
        let state = match index % 4 {
            0 => RuntimeJobStatus::Succeeded,
            1 => RuntimeJobStatus::Failed,
            2 => RuntimeJobStatus::Cancelled,
            _ => RuntimeJobStatus::DeadLettered,
        };
        schedule(
            &one,
            &format!("agent_run.lifecycle:historical-{index}"),
            "history-workspace",
            state,
            None,
        );
    }
    for index in 0..7 {
        schedule(
            &one,
            &format!("agent_run.lifecycle:expired-{index}"),
            &format!("expired-workspace-{index}"),
            if index % 2 == 0 {
                RuntimeJobStatus::Running
            } else {
                RuntimeJobStatus::Leased
            },
            Some(("expired-owner", now - 1000)),
        );
    }
    let fresh = "agent_run.lifecycle:current-owner";
    schedule(
        &one,
        fresh,
        "current-workspace",
        RuntimeJobStatus::Running,
        Some(("current-owner", now + 60_000)),
    );
    schedule(
        &one,
        "agent_run.lifecycle:waiting-next",
        "next-workspace",
        RuntimeJobStatus::Queued,
        None,
    );
    assert!(
        !two.wait_for_runtime_jobs(&[LIFECYCLE.into()], Duration::from_millis(10))
            .unwrap()
            .ready
    );
    assert!(claim(&two, "before-expired-reconciliation").is_empty());
    assert_eq!(db.query_one("SELECT count(*) FROM runtime.runtime_jobs WHERE job_kind='agent_run.lifecycle' AND status IN('leased','running') AND lease_purpose='execute' AND lease_expires_at_ms<$1", &[&now]).unwrap().get::<_,i64>(0),7);
    one.yield_runtime_job(YieldRuntimeJobRequest {
        job_id: fresh.into(),
        lease_owner: "current-owner".into(),
        yielded_at_ms: now,
        run_at_ms: now + 60_000,
        transition_reason: "question_wait".into(),
    })
    .unwrap();
    assert!(
        two.wait_for_runtime_jobs(&[LIFECYCLE.into()], Duration::from_millis(10))
            .unwrap()
            .ready
    );
    let next = claim(&two, "after-explicit-yield");
    assert_eq!(next.len(), 1);
    assert_eq!(next[0].job_id, "agent_run.lifecycle:waiting-next");
    assert!(claim(&one, "still-full").is_empty());
    assert_eq!(db.query_one("SELECT count(*) FROM runtime.runtime_jobs WHERE job_kind='agent_run.lifecycle' AND status IN('leased','running') AND lease_purpose='execute'", &[]).unwrap().get::<_,i64>(0),8);
    assert_eq!(db.query_one("SELECT count(*) FROM runtime.runtime_jobs WHERE status IN('succeeded','failed','cancelled','dead_lettered')", &[]).unwrap().get::<_,i64>(0),40);
}
