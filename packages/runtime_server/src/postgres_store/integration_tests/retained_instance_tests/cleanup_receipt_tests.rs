mod cleanup_preservation_tests;
use super::*;
use crate::contract::AgentRunCleanup;
use crate::postgres_store::cleanup_receipts::{
    CleanupAllocation, CleanupAuthorityRequest, CleanupTarget,
};
use std::sync::atomic::{AtomicBool, Ordering};

fn terminal(db: &mut Client, r: &mut Run, pre_cancel: bool) -> AgentRunCleanup {
    let mut b = r.grant.payload.clone();
    b.controller.as_mut().unwrap().controller_purpose = "cleanup".into();
    r.grant = sign(b);
    db.execute("UPDATE public.app_core_sessionenvironment SET controller_purpose='cleanup' WHERE id='environment'",&[]).unwrap();
    if pre_cancel {
        db.execute(
            "UPDATE public.app_core_agentrun SET \"preAdmissionCancelledAt\"=now() WHERE id=$1",
            &[&r.original.agent_run_id],
        )
        .unwrap();
    } else {
        db.execute("INSERT INTO public.app_core_sessionevent VALUES('ws_1','sess_1',$1,false,'{\"type\":\"agent_run_failed\"}')",&[&r.original.agent_run_id]).unwrap();
    }
    AgentRunCleanup {
        schema: "workspace.agent_run.cleanup.v1".into(),
        agent_run_id: r.original.agent_run_id.clone(),
        authorization_digest: r.original.digest().unwrap(),
        authorization_signature: r.original.signature(KEY).unwrap(),
        authorization: r.original.clone(),
        environment_authorization: r.grant.clone(),
    }
}

#[test]
#[ignore = "requires owned disposable PostgreSQL cleanup database"]
fn postgres_cleanup_preflight_pins_authority_before_other_terminal_side_effects() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_url, store, mut db) = setup();
    let mut r = run(&store, &mut db, "run-cleanup", 1);
    let cleanup = terminal(&mut db, &mut r, true);
    let verified = store
        .check_cleanup_authority(&cleanup, KEY, &r.fence)
        .unwrap();
    assert_eq!(verified.authorization_digest, cleanup.authorization_digest);
    assert_eq!(verified.controller_epoch, 1);
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.agent_run_cleanup_receipts",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        0
    );
    // The short check releases its pin. Final cleanup must independently
    // recheck authority after another controller changes the epoch.
    db.execute(
        "UPDATE public.app_core_sessionenvironment SET controller_epoch=2",
        &[],
    )
    .unwrap();
    assert!(store
        .check_cleanup_authority(&cleanup, KEY, &r.fence)
        .is_err());
}

#[test]
#[ignore = "requires owned disposable PostgreSQL cleanup database"]
fn postgres_cleanup_target_capture_pins_exact_authority_without_acknowledging_cleanup() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db) = setup();
    let mut r = run(&store, &mut db, "run-capture-cleanup", 1);
    let physical = Arc::new(Mutex::new(Vec::new()));
    let allocation = prepare(&store, &r, "execution-capture", &physical).unwrap();
    let cleanup = terminal(&mut db, &mut r, false);
    let resident_before: String = db
        .query_one(
            "SELECT (to_jsonb(s)-'updated_at_ms')::text FROM runtime.resident_sandboxes s",
            &[],
        )
        .unwrap()
        .get(0);
    let controller_before: String = db
        .query_one(
            "SELECT row_to_json(e)::text FROM public.app_core_sessionenvironment e",
            &[],
        )
        .unwrap()
        .get(0);
    let captured = AtomicBool::new(false);
    store.with_cleanup_target(&CleanupAuthorityRequest {
        cleanup:&cleanup, signing_key:KEY, lease:&r.fence, host_id:"host",
    }, || Ok(physical.lock().unwrap().clone()), |target, verified| {
        let CleanupTarget::Instance { allocation:target, .. } = target else { panic!("capture needs exact resident target") };
        assert_eq!(target.instance_id, allocation.instance_id);
        assert_eq!(target.container_id, allocation.container_id);
        assert_eq!(verified.authorization_digest, cleanup.authorization_digest);
        let mut observer = Client::connect(&url, NoTls).unwrap();
        assert!(observer.query_one("SELECT pg_try_advisory_lock(731946,3)",&[]).unwrap().get::<_,bool>(0), "archive capture must release the global host preparation lock");
        assert!(observer.query_one("SELECT pg_advisory_unlock(731946,3)",&[]).unwrap().get::<_,bool>(0));
        assert!(observer.batch_execute("SELECT id FROM public.app_core_session WHERE id='sess_1' FOR UPDATE NOWAIT").is_err(), "capture retains Session authority pins");
        assert!(observer.batch_execute("SELECT execution_id FROM runtime.resident_sandboxes FOR UPDATE NOWAIT").is_err(), "capture retains exact resource responsibility pins");
        captured.store(true, Ordering::SeqCst);
        Ok(())
    }).unwrap();
    assert!(captured.load(Ordering::SeqCst));
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.agent_run_cleanup_receipts",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        0,
        "archive capture alone cannot acknowledge cleanup"
    );
    assert_eq!(
        db.query_one(
            "SELECT (to_jsonb(s)-'updated_at_ms')::text FROM runtime.resident_sandboxes s",
            &[]
        )
        .unwrap()
        .get::<_, String>(0),
        resident_before
    );
    assert_eq!(
        db.query_one(
            "SELECT row_to_json(e)::text FROM public.app_core_sessionenvironment e",
            &[]
        )
        .unwrap()
        .get::<_, String>(0),
        controller_before
    );
    db.batch_execute("SELECT id FROM public.app_core_session WHERE id='sess_1' FOR UPDATE NOWAIT")
        .unwrap();
    let mut stale = r.fence.clone();
    stale.lease_owner = "stale-capture-owner".into();
    assert!(store
        .with_cleanup_target(
            &CleanupAuthorityRequest {
                cleanup: &cleanup,
                signing_key: KEY,
                lease: &stale,
                host_id: "host",
            },
            || Ok(physical.lock().unwrap().clone()),
            |_, _| -> Result<(), String> {
                panic!("stale capture lease cannot read physical target")
            }
        )
        .is_err());
}

#[test]
#[ignore = "requires owned disposable PostgreSQL cleanup database"]
fn postgres_cleanup_recovery_capture_observes_prior_resident_despite_unstarted_paused_shortcut() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_url, store, mut db) = setup();
    let origin = run(&store, &mut db, "prior-paused-origin", 1);
    let physical = Arc::new(Mutex::new(Vec::new()));
    let allocation = prepare(&store, &origin, "prior-paused-allocation", &physical).unwrap();
    db.execute("INSERT INTO runtime.runsc_environment_power(environment_id,host_id,allocation_execution_id,instance_id,container_id,state,controller_epoch,idle_since_ms,updated_at_ms) VALUES('environment','host',$1,$2,$3,'paused',1,0,0)", &[&allocation.allocation_execution_id,&allocation.instance_id,&allocation.container_id]).unwrap();
    let mut current = run(&store, &mut db, "unstarted-recovery-controller", 2);
    let cleanup = terminal(&mut db, &mut current, true);
    let request = CleanupAuthorityRequest {
        cleanup: &cleanup,
        signing_key: KEY,
        lease: &current.fence,
        host_id: "host",
    };
    store.with_cleanup_target(&request, || Ok(physical.lock().unwrap().clone()), |target,_| {
        let CleanupTarget::Instance { allocation:observed,.. } = target else {
            return Err("recovery capture must inspect original resident rather than trust persisted paused state".into());
        };
        assert_eq!(observed.instance_id,allocation.instance_id);
        assert_eq!(observed.container_id,allocation.container_id);
        Ok(())
    }).unwrap();
    let receipt = store
        .with_cleanup_authority(
            &request,
            || Ok(physical.lock().unwrap().clone()),
            |target, _| {
                assert!(
                    matches!(target, CleanupTarget::NeverAllocated),
                    "ordinary unstarted Run-only cleanup must retain the paused origin"
                );
                Ok(())
            },
        )
        .unwrap();
    assert_eq!(receipt.allocation, CleanupAllocation::NeverAllocated {});
    assert_eq!(physical.lock().unwrap().len(), 1);
    assert_eq!(
        db.query_one("SELECT count(*) FROM runtime.resident_sandboxes", &[])
            .unwrap()
            .get::<_, i64>(0),
        1
    );
}

#[test]
#[ignore = "requires owned disposable PostgreSQL cleanup database"]
fn postgres_cleanup_zero_history_failure_requires_fenced_receipt_before_never_allocated_cleanup() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_url, store, mut db) = setup();
    let mut r = run(&store, &mut db, "run-unallocated-failure", 1);
    let run_id = r.original.agent_run_id.clone();
    let digest = r.original.digest().unwrap();
    db.execute(
        "UPDATE public.app_core_agentrun SET status='failed' WHERE id=$1",
        &[&run_id],
    )
    .unwrap();
    let mut binding = r.grant.payload.clone();
    binding.controller.as_mut().unwrap().controller_purpose = "cleanup".into();
    r.grant = sign(binding);
    db.execute(
        "UPDATE public.app_core_sessionenvironment SET controller_purpose='cleanup' WHERE controller_run_id=$1",
        &[&run_id],
    )
    .unwrap();
    let cleanup = AgentRunCleanup {
        schema: "workspace.agent_run.cleanup.v1".into(),
        agent_run_id: run_id.clone(),
        authorization_digest: digest.clone(),
        authorization_signature: r.original.signature(KEY).unwrap(),
        authorization: r.original.clone(),
        environment_authorization: r.grant.clone(),
    };
    let request = CleanupAuthorityRequest {
        cleanup: &cleanup,
        signing_key: KEY,
        lease: &r.fence,
        host_id: "host",
    };
    assert!(store
        .with_cleanup_authority(
            &request,
            || panic!("status-only failure must not inspect physical resources"),
            |_, _| -> Result<(), String> {
                panic!("status-only failure must not authorize cleanup")
            },
        )
        .is_err());

    // Exercise the real lease-fenced producer, without fabricating Session
    // history or a cancellation marker to make cleanup authority pass.
    let log = store.session_log(
        r.original.workspace_id.clone(),
        r.original.session_id.clone(),
        "unadmitted input".into(),
    );
    log.commit_pre_admission_failure(&run_id, &digest, &r.fence)
        .unwrap();
    let receipt = store
        .with_cleanup_authority(
            &request,
            || Ok(vec![]),
            |target, verified| {
                assert!(matches!(target, CleanupTarget::NeverAllocated));
                assert_eq!(verified.authorization_digest, digest);
                // No container/task stop or removal is needed for this target.
                Ok(())
            },
        )
        .unwrap();
    assert_eq!(receipt.allocation, CleanupAllocation::NeverAllocated {});
    assert!(receipt.cleanup_complete);
    let replay = store
        .with_cleanup_authority(
            &request,
            || panic!("receipt replay must not reinspect resources"),
            |_, _| -> Result<(), String> { panic!("receipt replay must not repeat cleanup") },
        )
        .unwrap();
    assert_eq!(replay, receipt);
    let read = store
        .get_cleanup_receipt(
            &crate::postgres_store::cleanup_receipts::CleanupReceiptQuery {
                schema: "runtime.agent_run.cleanup_receipt.get.v1".into(),
                agent_run_id: run_id.clone(),
                authorization_digest: digest,
                cleanup_receipt_id: receipt.receipt_id,
            },
        )
        .unwrap()
        .unwrap();
    assert_eq!(read, replay);
    let facts = db
        .query_one(
            "SELECT (SELECT count(*) FROM public.app_core_sessionevent WHERE agent_run_id=$1), (SELECT count(*) FROM runtime.execution_instances WHERE agent_run_id=$1), (SELECT count(*) FROM runtime.resident_sandboxes), (SELECT count(*) FROM runtime.agent_run_cleanup_receipts WHERE agent_run_id=$1), (SELECT \"preAdmissionCancelledAt\" IS NULL FROM public.app_core_agentrun WHERE id=$1), (SELECT controller_released FROM public.app_core_sessionenvironment WHERE controller_run_id=$1)",
            &[&run_id],
        )
        .unwrap();
    assert_eq!(facts.get::<_, i64>(0), 0);
    assert_eq!(facts.get::<_, i64>(1), 0);
    assert_eq!(facts.get::<_, i64>(2), 0);
    assert_eq!(facts.get::<_, i64>(3), 1);
    assert!(facts.get::<_, bool>(4));
    assert!(
        !facts.get::<_, bool>(5),
        "cleanup receipt cannot silently release the controller"
    );
}

#[test]
#[ignore = "requires owned disposable PostgreSQL cleanup database"]
fn postgres_cleanup_receipt_proves_no_allocation_for_cancel_and_pre_admission_core_failure() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    for pre_cancel in [true, false] {
        let (url, store, mut db) = setup();
        let mut r = run(&store, &mut db, "run-cleanup", 1);
        let cleanup = terminal(&mut db, &mut r, pre_cancel);
        let receipt = store
            .with_cleanup_authority(
                &CleanupAuthorityRequest {
                    cleanup: &cleanup,
                    signing_key: KEY,
                    lease: &r.fence,
                    host_id: "host",
                },
                || Ok(vec![]),
                |target, _| {
                    assert!(matches!(target, CleanupTarget::NeverAllocated));
                    Ok(())
                },
            )
            .unwrap();
        assert_eq!(receipt.allocation, CleanupAllocation::NeverAllocated {});
        assert!(receipt.cleanup_complete);
        let mut observer = Client::connect(&url, NoTls).unwrap();
        assert_eq!(observer.query_one("SELECT receipt_id FROM runtime.agent_run_cleanup_receipts WHERE agent_run_id='run-cleanup'",&[]).unwrap().get::<_,String>(0),receipt.receipt_id);
    }
}

#[test]
#[ignore = "requires owned disposable PostgreSQL cleanup database"]
fn postgres_cleanup_cancelled_unallocated_run_checks_existing_environment_anchor_without_reassignment(
) {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_url, store, mut db) = setup();
    let physical = Arc::new(Mutex::new(vec![]));
    let origin = run(&store, &mut db, "run-origin", 1);
    let allocation = prepare(&store, &origin, "execution-origin", &physical).unwrap();
    let mut r = run(&store, &mut db, "run-cleanup", 2);
    let cleanup = terminal(&mut db, &mut r, true);
    let receipt = store
        .with_cleanup_authority(
            &CleanupAuthorityRequest {
                cleanup: &cleanup,
                signing_key: KEY,
                lease: &r.fence,
                host_id: "host",
            },
            || Ok(physical.lock().unwrap().clone()),
            |target, _| {
                let CleanupTarget::Instance {
                    allocation: target,
                    origin_authorization,
                    ..
                } = target
                else {
                    panic!("existing environment was not confirmed")
                };
                assert_eq!(
                    target.allocation_agent_run_id.as_deref(),
                    Some("run-origin")
                );
                assert_eq!(target.instance_id, allocation.instance_id);
                assert_eq!(origin_authorization.session_id, origin.original.session_id);
                assert_eq!(
                    origin_authorization.as_ref(),
                    &crate::postgres_store::allocation_origin::AllocationOrigin::from(
                        origin.original.clone()
                    )
                );
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
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.execution_instances WHERE agent_run_id='run-cleanup'",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        0
    );
    assert_eq!(physical.lock().unwrap().len(), 1);
}

#[test]
#[ignore = "requires owned disposable PostgreSQL cleanup database"]
fn postgres_cleanup_rejects_corrupt_anchor_original_grant_before_task_rpc() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_url, store, mut db) = setup();
    let physical = Arc::new(Mutex::new(vec![]));
    let origin = run(&store, &mut db, "run-origin", 1);
    prepare(&store, &origin, "execution-origin", &physical).unwrap();
    let mut r = run(&store, &mut db, "run-cleanup", 2);
    let cleanup = terminal(&mut db, &mut r, true);
    db.execute("UPDATE public.app_core_agentrunauthorization SET signature=$1 WHERE agent_run_id='run-origin'", &[&format!("hmac-sha256:{}", "0".repeat(64))]).unwrap();
    let called = AtomicBool::new(false);
    let result = store.with_cleanup_authority(
        &CleanupAuthorityRequest {
            cleanup: &cleanup,
            signing_key: KEY,
            lease: &r.fence,
            host_id: "host",
        },
        || Ok(physical.lock().unwrap().clone()),
        |_, _| {
            called.store(true, Ordering::SeqCst);
            Ok(())
        },
    );
    assert!(
        result.is_err(),
        "corrupt physical-origin grant must not be accepted"
    );
    assert!(
        !called.load(Ordering::SeqCst),
        "task RPC ran before origin verification"
    );
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.agent_run_cleanup_receipts",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        0
    );
}

#[test]
#[ignore = "requires owned disposable PostgreSQL cleanup database"]
fn postgres_cleanup_receipt_replays_after_target_disappears_without_rewriting_fact_epoch() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_url, store, mut db) = setup();
    let physical = Arc::new(Mutex::new(vec![]));
    let mut r = run(&store, &mut db, "run-cleanup", 1);
    prepare(&store, &r, "execution-cleanup", &physical).unwrap();
    let cleanup = terminal(&mut db, &mut r, false);
    let request = CleanupAuthorityRequest {
        cleanup: &cleanup,
        signing_key: KEY,
        lease: &r.fence,
        host_id: "host",
    };
    let original = store
        .with_cleanup_authority(
            &request,
            || Ok(physical.lock().unwrap().clone()),
            |_, _| Ok(()),
        )
        .unwrap();
    physical.lock().unwrap().clear();
    let replay = store
        .with_cleanup_authority(
            &request,
            || Err("inventory currently unavailable".into()),
            |_, _| Err::<(), _>("must not repeat physical cleanup".into()),
        )
        .unwrap();
    assert_eq!(replay, original);
    assert_eq!(db.execute("UPDATE runtime.agent_run_cleanup_receipts SET controller_epoch=2 WHERE agent_run_id='run-cleanup'",&[]).unwrap_err().code(),Some(&postgres::error::SqlState::RAISE_EXCEPTION));
    assert_eq!(
        store
            .with_cleanup_authority(&request, || Ok(vec![]), |_, _| Ok(()))
            .unwrap(),
        original
    );
}

#[test]
#[ignore = "requires owned disposable PostgreSQL cleanup database"]
fn postgres_cleanup_preservation_preflight_replays_removed_per_run_instance() {
    use crate::postgres_store::cleanup_receipts::CleanupPhysicalResult;
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (_, store, mut db) = setup();
    let mut r = run(&store, &mut db, "run-preservation-replay", 1);
    r.grant.payload.execution_mode = "perRun".into();
    r.grant = sign(r.grant.payload);
    let physical = Arc::new(Mutex::new(Vec::new()));
    let allocation = prepare(&store, &r, "preservation-replay-allocation", &physical).unwrap();
    let container = "a".repeat(64);
    db.execute(
        "UPDATE runtime.resident_sandboxes SET container_id=$1",
        &[&container],
    )
    .unwrap();
    physical.lock().unwrap()[0].container_id = container;
    let cleanup = terminal(&mut db, &mut r, false);
    let request = CleanupAuthorityRequest {
        cleanup: &cleanup,
        signing_key: KEY,
        lease: &r.fence,
        host_id: "host",
    };
    let original = store
        .with_cleanup_authority(
            &request,
            || Ok(physical.lock().unwrap().clone()),
            |target, _| {
                let CleanupTarget::Instance { allocation, .. } = target else {
                    panic!("cleanup must remove the exact allocated instance");
                };
                physical.lock().unwrap().clear();
                Ok(CleanupPhysicalResult::Removed {
                    instance_id: allocation.instance_id.clone(),
                    container_id: allocation.container_id.clone().unwrap(),
                })
            },
        )
        .unwrap();
    assert_eq!(
        original.allocation,
        CleanupAllocation::Instance {
            instance_id: allocation.instance_id,
        }
    );
    assert!(db
        .query_one(
            "SELECT physical_removed FROM runtime.agent_run_cleanup_receipts WHERE receipt_id=$1",
            &[&original.receipt_id]
        )
        .unwrap()
        .get::<_, bool>(0));

    // HTTP preservation preflight precedes the ordinary cleanup replay.
    store
        .with_cleanup_target(
            &request,
            || Ok(vec![]),
            |_, _| -> Result<(), String> {
                panic!("a completed cleanup must not recapture the removed instance");
            },
        )
        .expect("preservation preflight must reach receipt replay after physical removal");
    let replay = store
        .with_cleanup_authority(
            &request,
            || Err("physical inventory must not be needed for committed cleanup".into()),
            |_, _| -> Result<(), String> {
                panic!("a completed cleanup must not run physical cleanup again");
            },
        )
        .unwrap();
    assert_eq!(replay, original);
    assert_eq!(
        db.query_one("SELECT count(*) FROM runtime.resident_sandboxes", &[])
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    assert!(!db.query_one("SELECT controller_released FROM public.app_core_sessionenvironment WHERE id='environment'", &[]).unwrap().get::<_, bool>(0), "cleanup replay must leave controller release to its existing contract");

    let mut stale = r.fence.clone();
    stale.lease_owner = "stale-preservation-replay-owner".into();
    assert!(
        store
            .with_cleanup_target(
                &CleanupAuthorityRequest {
                    lease: &stale,
                    ..request
                },
                || -> Result<Vec<ObservedResidentSandbox>, String> {
                    panic!("a stale lease must not observe physical inventory");
                },
                |_, _| -> Result<(), String> {
                    panic!("a stale lease must not recapture the target");
                },
            )
            .is_err(),
        "receipt replay still requires the current cleanup lease"
    );
}

#[test]
#[ignore = "requires owned disposable PostgreSQL cleanup database"]
fn postgres_cleanup_receipt_rejects_historical_execution_unknown_inventory_and_unconfirmed_tasks() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_url, store, mut db) = setup();
    let mut r = run(&store, &mut db, "run-cleanup", 1);
    let cleanup = terminal(&mut db, &mut r, false);
    let request = CleanupAuthorityRequest {
        cleanup: &cleanup,
        signing_key: KEY,
        lease: &r.fence,
        host_id: "host",
    };
    let called = AtomicBool::new(false);
    db.execute("INSERT INTO public.app_core_sessionevent VALUES('ws_1','sess_1','run-cleanup',false,'{\"type\":\"agent_run_execution_started\"}')",&[]).unwrap();
    assert!(store
        .with_cleanup_authority(
            &request,
            || Ok(vec![]),
            |_, _| {
                called.store(true, Ordering::SeqCst);
                Ok(())
            }
        )
        .is_err());
    assert!(!called.load(Ordering::SeqCst));
    db.execute("DELETE FROM public.app_core_sessionevent WHERE payload->>'type'='agent_run_execution_started'",&[]).unwrap();
    assert!(store
        .with_cleanup_authority(
            &request,
            || Err("inventory unavailable".into()),
            |_, _| {
                called.store(true, Ordering::SeqCst);
                Ok(())
            }
        )
        .is_err());
    assert!(!called.load(Ordering::SeqCst));
    assert!(store
        .with_cleanup_authority(
            &request,
            || Ok(vec![]),
            |_, _| Err::<(), _>("tasks not positively empty".into())
        )
        .is_err());
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.agent_run_cleanup_receipts",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        0
    );
}

#[test]
#[ignore = "requires owned disposable PostgreSQL cleanup database"]
fn postgres_per_run_cleanup_requires_physical_removal_and_allows_fresh_instance_without_retirement()
{
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_url, store, mut db) = setup();
    let mut r = run(&store, &mut db, "per-run-physical-cleanup", 1);
    r.grant.payload.execution_mode = "perRun".into();
    r.grant = sign(r.grant.payload);
    let physical = Arc::new(Mutex::new(Vec::new()));
    let mut first = prepare(&store, &r, "per-run-allocation", &physical).unwrap();
    // Docker returns its immutable canonical ID, not a name or an alias.
    let container = "a".repeat(64);
    db.execute("UPDATE runtime.resident_sandboxes SET container_id=$1 WHERE execution_id='per-run-allocation'", &[&container]).unwrap();
    physical.lock().unwrap()[0].container_id = container.clone();
    first.container_id = Some(container);
    let cleanup = terminal(&mut db, &mut r, false);
    let request = CleanupAuthorityRequest {
        cleanup: &cleanup,
        signing_key: KEY,
        lease: &r.fence,
        host_id: "host",
    };
    let result = store.with_cleanup_authority(
        &request,
        || Ok(physical.lock().unwrap().clone()),
        |target, _| {
            assert!(matches!(target, CleanupTarget::Instance { .. }));
            Ok(())
        },
    );
    assert!(
        result.is_err(),
        "EndRun alone cannot acknowledge perRun physical deletion: {result:?}"
    );
    assert_eq!(
        db.query_one("SELECT count(*) FROM runtime.resident_sandboxes", &[])
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.agent_run_cleanup_receipts",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        0
    );
    assert_eq!(
        db.query_one("SELECT count(*) FROM runtime.environment_retirements", &[])
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    let receipt = store
        .with_cleanup_authority(
            &request,
            || Ok(physical.lock().unwrap().clone()),
            |target, _| {
                let CleanupTarget::Instance { allocation, .. } = target else {
                    panic!("expected exact physical instance");
                };
                physical.lock().unwrap().clear();
                Ok(
                    crate::postgres_store::cleanup_receipts::CleanupPhysicalResult::Removed {
                        instance_id: allocation.instance_id.clone(),
                        container_id: allocation.container_id.clone().unwrap(),
                    },
                )
            },
        )
        .unwrap();
    assert_eq!(
        receipt.allocation,
        CleanupAllocation::Instance {
            instance_id: first.instance_id.clone()
        }
    );
    assert!(receipt.cleanup_complete);
    assert_eq!(
        db.query_one("SELECT count(*) FROM runtime.resident_sandboxes", &[])
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    let historical: String = db.query_one("SELECT row_to_json(i)::text FROM runtime.execution_instances i WHERE execution_id='per-run-allocation'", &[]).unwrap().get(0);
    db.batch_execute("UPDATE public.app_core_agentrun SET status='failed'; UPDATE public.app_core_sessionenvironment SET controller_released=true").unwrap();
    let mut next = run(&store, &mut db, "next-per-run", 2);
    next.grant.payload.execution_mode = "perRun".into();
    next.grant = sign(next.grant.payload);
    let fresh = prepare(&store, &next, "fresh-per-run-allocation", &physical).unwrap();
    assert!(!fresh.reused);
    assert_ne!(fresh.instance_id, first.instance_id);
    assert_eq!(
        db.query_one("SELECT count(*) FROM runtime.resident_sandboxes", &[])
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    assert_eq!(db.query_one("SELECT row_to_json(i)::text FROM runtime.execution_instances i WHERE execution_id='per-run-allocation'", &[]).unwrap().get::<_,String>(0), historical);
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.agent_run_cleanup_receipts WHERE receipt_id=$1",
            &[&receipt.receipt_id]
        )
        .unwrap()
        .get::<_, i64>(0),
        1
    );
    assert_eq!(
        db.query_one("SELECT count(*) FROM runtime.environment_retirements", &[])
            .unwrap()
            .get::<_, i64>(0),
        0
    );
}
