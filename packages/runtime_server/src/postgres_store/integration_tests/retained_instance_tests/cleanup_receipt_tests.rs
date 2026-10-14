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
                assert_eq!(target.allocation_agent_run_id, "run-origin");
                assert_eq!(target.instance_id, allocation.instance_id);
                assert_eq!(
                    origin_authorization.agent_run_id,
                    origin.original.agent_run_id
                );
                assert_eq!(origin_authorization.as_ref(), &origin.original);
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
            |_, _| Err("must not repeat physical cleanup".into()),
        )
        .unwrap();
    assert_eq!(replay, original);
    db.execute("UPDATE runtime.agent_run_cleanup_receipts SET controller_epoch=2 WHERE agent_run_id='run-cleanup'",&[]).unwrap();
    assert!(store
        .with_cleanup_authority(&request, || Ok(vec![]), |_, _| Ok(()))
        .is_err());
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
            |_, _| Err("tasks not positively empty".into())
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
