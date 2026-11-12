use super::*;
use centaeris_core::execution::ExecutionCancellationProbe;
use centaeris_core::runtime::subagent::SUBAGENT_RUN_JOB_KIND;
use centaeris_core::session::reliability::{
    RuntimeJobRecord, RuntimeJobStorePort, WakeRuntimeJobDisposition, WakeRuntimeJobRequest,
};
use postgres::fallible_iterator::FallibleIterator;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::Duration;

fn running_child(store: &PostgresRuntimeStore, id: &str) -> RuntimeJobRecord {
    let mut job = super::super::job(id, id);
    job.job_kind = SUBAGENT_RUN_JOB_KIND.to_string();
    job.status = RuntimeJobStatus::Running;
    job.lease_owner = Some(format!("owner-{id}"));
    job.lease_expires_at_ms = Some(4_000_000_000_000);
    job.session_id = Some("sess_1".to_string());
    store
        .schedule_runtime_job(ScheduleRuntimeJobRequest { job: job.clone() })
        .unwrap();
    job
}

fn dispatch(
    store: &PostgresRuntimeStore,
    parent: &Run,
    allocation: &RetainedInstanceAllocation,
    probe: &ExecutionCancellationProbe,
    operation: impl FnOnce(&RetainedInstanceAllocation) -> Result<(), String> + Send,
) -> Result<(), String> {
    if let Some(reason) = probe()? {
        return Err(reason);
    }
    let profile = parent.original.environment_profile.digest()?;
    store.with_retained_instance_operation(
        &parent.request("subagent-shared-execution", &profile),
        allocation,
        operation,
    )
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_subagent_shared_parent_authority_preserves_parallel_multistep_execution() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (_url, store, mut db) = setup();
    let parent = run(&store, &mut db, "subagent-shared-parent", 1);
    let physical = Arc::new(Mutex::new(Vec::new()));
    let allocation = prepare(&store, &parent, "subagent-shared-execution", &physical).unwrap();
    let probes = (0..3)
        .map(|index| {
            let child = running_child(&store, &format!("subagent-parallel-{index}"));
            crate::hosted_subagent_cancellation_probe(
                Arc::new(store.clone()),
                &child,
                Arc::new(|| Ok(None)),
                true,
            )
            .unwrap()
        })
        .collect::<Vec<_>>();
    let calls = AtomicUsize::new(0);
    let (entered_tx, entered_rx) = mpsc::channel();
    std::thread::scope(|scope| {
        let mut releases = Vec::new();
        let mut workers = Vec::new();
        for probe in &probes {
            let (release_tx, release_rx) = mpsc::channel();
            releases.push(release_tx);
            let entered_tx = entered_tx.clone();
            let store = &store;
            let parent = &parent;
            let allocation = &allocation;
            let calls = &calls;
            workers.push(scope.spawn(move || {
                dispatch(store, parent, allocation, probe.as_ref(), move |actual| {
                    assert_eq!(actual.instance_id, allocation.instance_id);
                    assert_eq!(actual.container_id, allocation.container_id);
                    calls.fetch_add(1, Ordering::SeqCst);
                    entered_tx.send(()).map_err(|error| error.to_string())?;
                    release_rx
                        .recv_timeout(Duration::from_secs(10))
                        .map_err(|error| error.to_string())
                })?;
                dispatch(store, parent, allocation, probe.as_ref(), |actual| {
                    assert_eq!(actual.instance_id, allocation.instance_id);
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
            }));
        }
        // All first operations must overlap; serializing children cannot satisfy this.
        let overlap = (0..probes.len())
            .map(|_| entered_rx.recv_timeout(Duration::from_secs(5)))
            .collect::<Result<Vec<_>, _>>();
        for release in releases {
            let _ = release.send(());
        }
        let results = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>();
        assert!(
            overlap.is_ok(),
            "child operations failed to overlap: {results:?}"
        );
        for result in results {
            result.unwrap();
        }
    });
    assert_eq!(calls.load(Ordering::SeqCst), 6);
    assert_eq!(physical.lock().unwrap().len(), 1);
    for probe in probes {
        assert_eq!(probe().unwrap(), None);
    }
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_subagent_stable_parent_authority_does_not_bypass_child_fences() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (_url, store, mut db) = setup();
    let parent = run(&store, &mut db, "subagent-fenced-parent", 1);
    let physical = Arc::new(Mutex::new(Vec::new()));
    let allocation = prepare(&store, &parent, "subagent-shared-execution", &physical).unwrap();
    let sibling = running_child(&store, "subagent-current-sibling");
    let parent_cancelled = Arc::new(AtomicBool::new(false));
    let parent_lease_current = Arc::new(AtomicBool::new(true));
    let observed_parent_cancelled = parent_cancelled.clone();
    let observed_parent_lease_current = parent_lease_current.clone();
    let parent_probe: Arc<ExecutionCancellationProbe> = Arc::new(move || {
        Ok(crate::agent_run_execution_control_reason(
            crate::postgres_store::AgentRunExecutionControlState {
                cancellation_requested: observed_parent_cancelled.load(Ordering::SeqCst),
                lifecycle_lease_current: observed_parent_lease_current.load(Ordering::SeqCst),
            },
        ))
    });
    let sibling_probe = crate::hosted_subagent_cancellation_probe(
        Arc::new(store.clone()),
        &sibling,
        parent_probe.clone(),
        true,
    )
    .unwrap();
    let independent_probe = crate::hosted_subagent_cancellation_probe(
        Arc::new(store.clone()),
        &sibling,
        parent_probe.clone(),
        false,
    )
    .unwrap();
    let calls = AtomicUsize::new(0);
    for (id, update, expected) in [
        (
            "subagent-cancelled",
            "UPDATE runtime.runtime_jobs SET status='cancelled',last_error='user_cancelled' WHERE job_id=$1",
            "user_cancelled",
        ),
        (
            "subagent-reclaimed",
            "UPDATE runtime.runtime_jobs SET lease_owner='replacement-owner' WHERE job_id=$1",
            "subagent_lease_lost",
        ),
        (
            "subagent-expired",
            "UPDATE runtime.runtime_jobs SET lease_expires_at_ms=1 WHERE job_id=$1",
            "subagent_lease_lost",
        ),
    ] {
        let child = running_child(&store, id);
        let probe = crate::hosted_subagent_cancellation_probe(
            Arc::new(store.clone()),
            &child,
            parent_probe.clone(),
            true,
        )
        .unwrap();
        dispatch(&store, &parent, &allocation, probe.as_ref(), |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .unwrap();
        db.execute(update, &[&id]).unwrap();
        let before = calls.load(Ordering::SeqCst);
        let rejected = dispatch(&store, &parent, &allocation, probe.as_ref(), |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });
        assert_eq!(rejected.unwrap_err(), expected);
        assert_eq!(calls.load(Ordering::SeqCst), before);
        dispatch(&store, &parent, &allocation, sibling_probe.as_ref(), |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .unwrap();
    }
    assert_eq!(calls.load(Ordering::SeqCst), 6);
    assert_eq!(physical.lock().unwrap().len(), 1);
    parent_cancelled.store(true, Ordering::SeqCst);
    assert_eq!(
        sibling_probe().unwrap().as_deref(),
        Some("agent_run_cancel_requested")
    );
    assert_eq!(independent_probe().unwrap(), None);
    parent_lease_current.store(false, Ordering::SeqCst);
    for cancellation_requested in [false, true] {
        parent_cancelled.store(cancellation_requested, Ordering::SeqCst);
        assert_eq!(
            independent_probe().unwrap().as_deref(),
            Some("agent_run_lifecycle_lease_lost")
        );
        assert_eq!(
            sibling_probe().unwrap().as_deref(),
            Some("agent_run_lifecycle_lease_lost")
        );
    }
    assert_eq!(
        store
            .get_runtime_job(&sibling.job_id)
            .unwrap()
            .unwrap()
            .status,
        RuntimeJobStatus::Running,
        "the running child retains its lease until execution has settled"
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_subagent_owned_wait_consumes_wakes_without_handing_off_parent_authority() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (url, store, mut db) = setup();
    let parent = run(&store, &mut db, "subagent-wait-parent", 1);
    let physical = Arc::new(Mutex::new(Vec::new()));
    let allocation = prepare(&store, &parent, "subagent-shared-execution", &physical).unwrap();
    let mut listener = Client::connect(&url, NoTls).unwrap();
    listener
        .batch_execute("LISTEN runtime_job_wake_v1")
        .unwrap();
    for source in ["first-child", "second-child"] {
        let request = WakeRuntimeJobRequest {
            job_id: parent.fence.job_id.clone(),
            source_job_id: source.to_string(),
            woken_at_ms: 100,
            transition_reason: "runtime_job_terminal".to_string(),
        };
        assert_eq!(
            store.wake_runtime_job(request.clone()).unwrap(),
            WakeRuntimeJobDisposition::Active
        );
        let notification = listener
            .notifications()
            .timeout_iter(Duration::from_secs(2))
            .next()
            .unwrap()
            .expect("an active owner must receive the committed wake notification");
        assert_eq!(notification.payload(), parent.fence.job_id);
        // The notification was sent before this new waiter registered. The
        // persisted receipt must still wake it, without requiring a timeout.
        assert!(store
            .wait_for_owned_runtime_job_wake(&parent.fence, Duration::ZERO)
            .unwrap());
        assert_eq!(
            store.wake_runtime_job(request).unwrap(),
            WakeRuntimeJobDisposition::Active
        );
        assert!(!store
            .wait_for_owned_runtime_job_wake(&parent.fence, Duration::ZERO)
            .unwrap());
        dispatch(&store, &parent, &allocation, &|| Ok(None), |_| Ok(())).unwrap();
        let job = store
            .get_runtime_job(&parent.fence.job_id)
            .unwrap()
            .unwrap();
        assert_eq!(job.status, RuntimeJobStatus::Running);
        assert_eq!(
            job.lease_owner.as_deref(),
            Some(parent.fence.lease_owner.as_str())
        );
    }
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.runtime_events WHERE task_id=$1 AND event_type='runtime_job_wake_consumed'",
            &[&parent.fence.job_id],
        ).unwrap().get::<_, i64>(0),
        2
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_subagent_owned_wait_rejects_replaced_or_expired_parent_before_consuming_wake() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (_url, store, mut db) = setup();
    let parent = run(&store, &mut db, "subagent-invalid-wait-parent", 1);
    store
        .wake_runtime_job(WakeRuntimeJobRequest {
            job_id: parent.fence.job_id.clone(),
            source_job_id: "finished-child".to_string(),
            woken_at_ms: 100,
            transition_reason: "runtime_job_terminal".to_string(),
        })
        .unwrap();
    for update in [
        "UPDATE runtime.runtime_jobs SET lease_owner='replacement' WHERE job_id=$1",
        "UPDATE runtime.runtime_jobs SET lease_owner=$2,lease_expires_at_ms=1 WHERE job_id=$1",
    ] {
        if update.contains("$2") {
            db.execute(update, &[&parent.fence.job_id, &parent.fence.lease_owner])
                .unwrap();
        } else {
            db.execute(update, &[&parent.fence.job_id]).unwrap();
        }
        assert_eq!(
            store
                .wait_for_owned_runtime_job_wake(&parent.fence, Duration::ZERO)
                .unwrap_err(),
            "owned runtime job wake lease rejected"
        );
        assert_eq!(
            db.query_one(
                "SELECT count(*) FROM runtime.runtime_events WHERE task_id=$1 AND event_type='runtime_job_wake_consumed'",
                &[&parent.fence.job_id],
            ).unwrap().get::<_, i64>(0),
            0
        );
    }
}
