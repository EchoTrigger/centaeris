use super::*;
use centaeris_core::session::store::job::JobStoreError;

struct HeldExclusiveRunner {
    entered: tokio::sync::mpsc::UnboundedSender<String>,
    release: tokio::sync::Semaphore,
    calls: Mutex<Vec<String>>,
    active: AtomicUsize,
    max_active: AtomicUsize,
}

impl AsyncSubagentWorkerRunner for HeldExclusiveRunner {
    fn run_async<'a>(&'a self, req: SubagentWorkerRunRequest) -> SubagentWorkerRunFuture<'a> {
        Box::pin(async move {
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active.fetch_max(active, Ordering::SeqCst);
            self.calls.lock().unwrap().push(req.job.job_id.clone());
            self.entered.send(req.job.job_id).unwrap();
            self.release.acquire().await.unwrap().forget();
            self.active.fetch_sub(1, Ordering::SeqCst);
            SubagentWorkerRunOutcome::Succeeded {
                summary: "Exclusive work completed.".into(),
                output_refs: vec![],
            }
        })
    }
}

async fn pending_leases_survive_exclusive_work(max_parallelism: usize, cancel_pending: bool) {
    let directory = tempfile::tempdir().unwrap();
    let store = SqliteRuntimeStore::new(directory.path().join("runtime.db")).unwrap();
    let mut scheduled_ids = vec![];
    for index in 0..3 {
        let work_packet_ref = runtime_external_context_keys::subagent_work_packet_ref(&format!(
            "pending-lease-{index}"
        ));
        store_valid_subagent_work_packet(
            &store,
            &work_packet_ref,
            "workspace:pending-leases",
            "Move",
            vec!["read".into()],
            vec![],
        );
        let scheduled = enqueue_subagent_run_job(
            &store,
            SubagentRunJobRequest {
                session_id: "chat-resource-claim".into(),
                parent_turn_id: "turn-resource-claim".into(),
                tool_call_id: format!("pending-lease-{index}"),
                subagent_id: format!("subagent:pending-lease-{index}"),
                work_packet_ref,
                checkpoint_id: None,
                run_at_ms: 1000,
                created_at_ms: 900,
                max_retries: 0,
            },
        )
        .unwrap();
        scheduled_ids.push(scheduled.job.job_id);
    }
    let actor = RuntimeStoreActor::start(store.clone()).unwrap();
    let (entered, mut entries) = tokio::sync::mpsc::unbounded_channel();
    let runner = HeldExclusiveRunner {
        entered,
        release: tokio::sync::Semaphore::new(0),
        calls: Mutex::new(vec![]),
        active: AtomicUsize::new(0),
        max_active: AtomicUsize::new(0),
    };
    let claimed_snapshot = Mutex::new(None);
    let execute = run_due_subagent_jobs_with_worker_pool_async(
        &actor,
        &runner,
        &NoopSubagentLifecycleObserver,
        RunDueSubagentJobsRequest {
            now_ms: 1001,
            worker_id: "pending-lease-worker".into(),
            session_id: Some("chat-resource-claim".into()),
            limit: 3,
            lease_ms: 120,
            started_at_ms: 1002,
            finished_at_ms: 1002,
        },
        SubagentWorkerPoolPolicy { max_parallelism },
    );
    let inspect = async {
        let first = entries.recv().await.unwrap();
        let pending = actor
            .list_runtime_jobs(ListRuntimeJobsRequest {
                statuses: vec![RuntimeJobStatus::Leased],
                job_kind: Some(SUBAGENT_RUN_JOB_KIND.into()),
                session_id: Some("chat-resource-claim".into()),
                branch_id: None,
                limit: 3,
                offset: 0,
            })
            .await
            .unwrap();
        assert_eq!(pending.len(), 2);
        let original_expiry = pending[0].lease_expires_at_ms.unwrap();
        let cancelled = cancel_pending.then(|| pending[0].job_id.clone());
        let first_job = actor.get_runtime_job(&first).await.unwrap().unwrap();
        *claimed_snapshot.lock().unwrap() = Some((
            first_job,
            pending.clone(),
            original_expiry,
            cancelled.clone(),
        ));
        if let Some(job_id) = &cancelled {
            actor
                .cancel_runtime_job(CancelRuntimeJobRequest {
                    job_id: job_id.clone(),
                    reason: "pending_work_cancelled".into(),
                    cancelled_at_ms: 1003,
                    expected_status: Some(RuntimeJobStatus::Leased),
                    expected_lease_owner: pending[0].lease_owner.clone(),
                })
                .await
                .unwrap();
        }
        loop {
            let mut renewed_past_original_expiry = true;
            for claimed in &pending {
                let current = actor
                    .get_runtime_job(&claimed.job_id)
                    .await
                    .unwrap()
                    .unwrap();
                if cancelled.as_ref() == Some(&claimed.job_id) {
                    assert_eq!(current.status, RuntimeJobStatus::Cancelled);
                } else {
                    assert_eq!(current.status, RuntimeJobStatus::Leased);
                    assert_eq!(current.lease_owner, claimed.lease_owner);
                    renewed_past_original_expiry &= current
                        .heartbeat_at_ms
                        .is_some_and(|at| at > original_expiry);
                }
            }
            if renewed_past_original_expiry {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        // Drive the existing reclaimer at the original lease boundary. Pending
        // work must retain its original owner without bypassing exclusivity.
        assert_eq!(
            actor
                .reclaim_expired_runtime_job_leases(original_expiry + 1)
                .await
                .unwrap(),
            0
        );
        assert_eq!(*runner.calls.lock().unwrap(), vec![first]);
        runner.release.add_permits(3);
        Ok::<_, JobStoreError>(cancelled)
    };
    let outcome = tokio::time::timeout(Duration::from_secs(3), async {
        tokio::try_join!(execute, inspect)
    })
    .await;
    let (result, cancelled) = match outcome {
        Ok(Ok(result)) => result,
        error => {
            let mut current = vec![];
            for job_id in &scheduled_ids {
                current.push(actor.get_runtime_job(job_id).await);
            }
            panic!(
                "pending lease execution failed: {error:?}; scheduledIds={scheduled_ids:?}; \
                 claimedSnapshot={:?}; currentJobs={current:?}",
                claimed_snapshot.lock().unwrap()
            );
        }
    };
    assert_eq!(result.claimed, 3);
    assert_eq!(result.succeeded, if cancel_pending { 2 } else { 3 });
    assert_eq!(result.cancelled, usize::from(cancel_pending));
    assert_eq!(runner.max_active.load(Ordering::SeqCst), 1);
    if let Some(job_id) = cancelled {
        assert!(!runner.calls.lock().unwrap().contains(&job_id));
    }
    actor.close().await.unwrap();
    drop(actor);
    drop(store);
    directory.close().unwrap();
}

#[tokio::test(start_paused = true)]
async fn worker_pool_renews_leased_jobs_waiting_for_exclusive_resources() {
    pending_leases_survive_exclusive_work(3, false).await;
}

#[tokio::test(start_paused = true)]
async fn serial_subagent_worker_renews_pending_leases_and_skips_cancelled_work() {
    pending_leases_survive_exclusive_work(1, true).await;
}
