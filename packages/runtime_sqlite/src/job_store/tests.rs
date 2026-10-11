use super::*;
use crate::sqlite_job_connection::{Outcome, WorkError};
use crate::sqlite_transactions::TransactionEvent;
use centaeris_core::runtime::subagent::{build_subagent_run_job, SubagentRunJobRequest};
use centaeris_core::session::external_context::EXTERNAL_CONTEXT_SCHEMA_VERSION;
use std::sync::mpsc;
use std::time::Duration;

fn creation(id: &str) -> UpsertExternalContextAndScheduleJobRequest {
    let job = build_subagent_run_job(SubagentRunJobRequest {
        session_id: "parent".into(),
        parent_turn_id: "turn".into(),
        tool_call_id: id.into(),
        subagent_id: id.into(),
        work_packet_ref: format!("packet:{id}"),
        checkpoint_id: None,
        run_at_ms: 10,
        created_at_ms: 9,
        max_retries: 1,
    });
    UpsertExternalContextAndScheduleJobRequest {
        object: ExternalContextObject {
            schema_version: EXTERNAL_CONTEXT_SCHEMA_VERSION.into(),
            object_id: format!("packet:{id}"),
            object_kind: "subagent_work_packet".into(),
            source_provider_id: "centaeris.core".into(),
            source_tool_name: "agent".into(),
            title: id.into(),
            content: "{}".into(),
            metadata: serde_json::json!({}),
            updated_at_ms: 9,
        },
        job,
    }
}

async fn claim(jobs: &SqliteJobStore, id: &str, at: i64) -> RuntimeJobRecord {
    jobs.claim_due_runtime_jobs(ClaimDueRuntimeJobsRequest {
        job_id: Some(id.into()),
        worker_id: "worker".into(),
        now_ms: at,
        lease_ms: 100,
        job_kind: None,
        session_id: None,
        limit: 1,
    })
    .await
    .unwrap()
    .pop()
    .unwrap()
}

#[tokio::test]
async fn native_operations_preserve_job_and_outbox_facts() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("runtime.db");
    let jobs = SqliteJobStore::open(path.clone(), 2).await.unwrap();
    let request = creation("job");
    let job_id = request.job.job_id.clone();
    assert_eq!(
        jobs.upsert_external_context_and_schedule_job(request.clone())
            .await
            .unwrap()
            .disposition,
        ScheduleRuntimeJobDisposition::Inserted
    );
    assert_eq!(
        jobs.upsert_external_context_and_schedule_job(request)
            .await
            .unwrap()
            .disposition,
        ScheduleRuntimeJobDisposition::Existing
    );
    let job = claim(&jobs, &job_id, 10).await;
    let lease = job.lease_owner.unwrap();
    let error = jobs
        .start_runtime_job(StartRuntimeJobRequest {
            job_id: job_id.clone(),
            lease_owner: "wrong".into(),
            started_at_ms: 11,
        })
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        JobStoreError::Storage {
            operation: JobStoreOperation::Start,
            outcome: JobWriteOutcome::RolledBack,
            ..
        }
    ));
    jobs.start_runtime_job(StartRuntimeJobRequest {
        job_id: job_id.clone(),
        lease_owner: lease.clone(),
        started_at_ms: 11,
    })
    .await
    .unwrap();
    jobs.renew_runtime_job_lease(RenewRuntimeJobLeaseRequest {
        job_id: job_id.clone(),
        lease_owner: lease.clone(),
        heartbeat_at_ms: 12,
        lease_ms: 100,
    })
    .await
    .unwrap();
    jobs.fail_runtime_job(FailRuntimeJobRequest {
        job_id: job_id.clone(),
        lease_owner: lease,
        failed_at_ms: 13,
        last_error: "retryable".into(),
        next_run_at_ms: Some(14),
        disposition: RuntimeJobFailureDisposition::RetryScheduled,
    })
    .await
    .unwrap();
    let job = jobs.get_runtime_job(&job_id).await.unwrap().unwrap();
    assert_eq!(job.status, RuntimeJobStatus::Queued);
    assert_eq!(job.retry_count, 1);
    let job = claim(&jobs, &job_id, 14).await;
    jobs.complete_runtime_job(CompleteRuntimeJobRequest {
        job_id: job_id.clone(),
        lease_owner: job.lease_owner.unwrap(),
        completed_at_ms: 15,
        output_refs: vec![],
    })
    .await
    .unwrap();
    assert_eq!(
        jobs.get_runtime_job(&job_id).await.unwrap().unwrap().status,
        RuntimeJobStatus::Succeeded
    );
    let cancelled = jobs
        .upsert_external_context_and_schedule_job(creation("cancelled"))
        .await
        .unwrap()
        .job;
    jobs.cancel_runtime_job(CancelRuntimeJobRequest {
        job_id: cancelled.job_id.clone(),
        reason: "user".into(),
        cancelled_at_ms: 20,
        expected_status: None,
        expected_lease_owner: None,
    })
    .await
    .unwrap();
    let expired = jobs
        .upsert_external_context_and_schedule_job(creation("expired"))
        .await
        .unwrap()
        .job;
    claim(&jobs, &expired.job_id, 20).await;
    assert_eq!(
        jobs.reclaim_expired_runtime_job_leases(120).await.unwrap(),
        1
    );
    assert_eq!(
        jobs.execute(JobStoreOperation::Read, |conn, _| conn
            .pragma_query_value(None, "synchronous", |row| row.get::<_, i64>(0))
            .map_err(|e| e.to_string()))
            .await
            .unwrap(),
        1
    );
    jobs.close().await.unwrap();
    let conn = rusqlite::Connection::open(path).unwrap();
    assert_eq!(
        conn.query_row("SELECT count(*) FROM runtime_job_outbox", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        conn.query_row(
            "SELECT status FROM runtime_jobs WHERE job_id=?1",
            [&cancelled.job_id],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "cancelled"
    );
}

#[tokio::test]
async fn retired_epoch_rejects_queued_work_and_recovery_never_replays_commit() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("runtime.db");
    let jobs = SqliteJobStore::open(path.clone(), 2).await.unwrap();
    let job = jobs
        .upsert_external_context_and_schedule_job(creation("job"))
        .await
        .unwrap()
        .job;
    let claimed = claim(&jobs, &job.job_id, 10).await;
    let completion = UpsertExternalContextLinkAndCompleteJobRequest {
        object: None,
        link: None,
        complete_job: CompleteRuntimeJobRequest {
            job_id: job.job_id.clone(),
            lease_owner: claimed.lease_owner.unwrap(),
            output_refs: vec![],
            completed_at_ms: 11,
        },
    };
    let driver = jobs.connection(JobStoreOperation::Complete).await.unwrap();
    let (started, ready) = tokio::sync::oneshot::channel();
    let (release, wait) = mpsc::channel();
    let first = driver
        .submit(Options::default(), move |conn, observe| {
            let mut started = Some(started);
            sqlite_transactions::upsert_external_context_link_and_complete_job_conn(
                conn,
                completion,
                &mut |event| {
                    observe(event.clone());
                    if matches!(event, TransactionEvent::Committed) {
                        started.take().unwrap().send(()).unwrap();
                        wait.recv_timeout(Duration::from_secs(10)).unwrap();
                        panic!("reply lost after commit");
                    }
                },
            )
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), ready)
        .await
        .unwrap()
        .unwrap();
    let queued = driver
        .submit(
            Options {
                read_only: true,
                ..Options::default()
            },
            |_, _| Ok(()),
        )
        .await
        .unwrap();
    release.send(()).unwrap();
    let receipt = first.await;
    assert_eq!(receipt.outcome, Outcome::Committed);
    assert_eq!(receipt.result, Err(WorkError::Panicked));
    assert!(!receipt.healthy);
    // Replacement awaits all actual work, including queued rejection.
    jobs.settle_job_operations().await.unwrap();
    let rejected = queued.await;
    assert_eq!(rejected.outcome, Outcome::NotExecuted);
    assert_eq!(rejected.result, Err(WorkError::Unusable));
    assert_eq!(
        jobs.get_runtime_job(&job.job_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        RuntimeJobStatus::Succeeded
    );
    assert_eq!(jobs.capacity().await, (2, 0));
    jobs.close().await.unwrap();
    let conn = rusqlite::Connection::open(path).unwrap();
    assert_eq!(
        conn.query_row("SELECT count(*) FROM runtime_job_outbox", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn cancelled_close_waiter_does_not_reopen_admission_or_cancel_drain() {
    use std::future::{poll_fn, Future};
    use std::task::Poll;
    let directory = tempfile::tempdir().unwrap();
    let jobs = SqliteJobStore::open(directory.path().join("runtime.db"), 1)
        .await
        .unwrap();
    let driver = jobs.connection(JobStoreOperation::Read).await.unwrap();
    let (started, ready) = tokio::sync::oneshot::channel();
    let (release, wait) = mpsc::channel();
    let work = driver
        .submit(
            Options {
                read_only: true,
                ..Options::default()
            },
            move |_, _| {
                started.send(()).unwrap();
                wait.recv_timeout(Duration::from_secs(10)).unwrap();
                Ok(())
            },
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), ready)
        .await
        .unwrap()
        .unwrap();
    let mut closing = Box::pin(jobs.close());
    poll_fn(|cx| {
        assert!(closing.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    assert!(jobs.owner.lock().await.stopped);
    drop(closing);
    assert!(matches!(
        jobs.get_runtime_job("any").await,
        Err(JobStoreError::Storage {
            outcome: JobWriteOutcome::NotExecuted,
            ..
        })
    ));
    assert_eq!(jobs.capacity().await, (1, 1));
    release.send(()).unwrap();
    work.await.result.unwrap();
    tokio::time::timeout(Duration::from_secs(10), jobs.close())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(jobs.capacity().await, (1, 0));
}

#[tokio::test]
async fn failed_initialization_closes_the_connection_without_mutating_unsupported_schema() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("runtime.db");
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute("CREATE TABLE unrelated(value)", [])
        .unwrap();
    let error = match SqliteJobStore::open(path.clone(), 1).await {
        Ok(_) => panic!("unsupported schema accepted"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        JobStoreError::Storage {
            outcome: JobWriteOutcome::NotExecuted,
            ..
        }
    ));
    let conn = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table'",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    drop(conn);
    // On Windows this also checks the failed owner left no file handle alive.
    std::fs::remove_file(path).unwrap();
}
