use super::*;
use crate::postgres_store::controller_authority::{
    verify_controller_authority, ControllerAuthorityRequest, ControllerPurpose,
};

fn cleanup_grant(db: &mut Client, r: &mut Run) {
    let mut binding = r.grant.payload.clone();
    binding.controller.as_mut().unwrap().controller_purpose = "cleanup".into();
    r.grant = sign(binding);
    db.execute("UPDATE public.app_core_sessionenvironment SET controller_purpose='cleanup' WHERE id='environment'", &[]).unwrap();
}

fn verify(db: &mut Client, r: &Run) -> Result<(), String> {
    let mut tx = db.transaction().map_err(|e| e.to_string())?;
    verify_controller_authority(
        &mut tx,
        &ControllerAuthorityRequest {
            environment_authorization: &r.grant,
            signing_key: KEY,
            workspace_id: &r.original.workspace_id,
            lease: &r.fence,
            purpose: ControllerPurpose::Cleanup,
        },
    )?;
    tx.commit().map_err(|e| e.to_string())
}

#[test]
#[ignore = "requires owned disposable PostgreSQL cleanup database"]
fn postgres_cleanup_authority_uses_core_terminal_and_allows_revoked_membership_without_execute() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_url, store, mut db) = setup();
    let mut r = run(&store, &mut db, "run-cleanup", 1);
    let physical = Arc::new(Mutex::new(Vec::new()));
    let allocated = prepare(&store, &r, "execution-cleanup", &physical).unwrap();
    cleanup_grant(&mut db, &mut r);
    db.batch_execute("DELETE FROM public.app_core_workspacemembership; UPDATE public.app_core_workspace SET status='inactive'; UPDATE public.app_core_session SET \"deletedAt\"=now(),current_environment_id=NULL;
        INSERT INTO public.app_core_sessionevent VALUES('ws_1','sess_1','run-cleanup',false,'{\"type\":\"agent_run_failed\"}')").unwrap();
    assert!(verify(&mut db, &r).is_ok());
    let profile = format!("sha256:{}", "b".repeat(64));
    assert!(store
        .with_retained_instance_operation::<()>(
            &r.request("execution-cleanup", &profile),
            &allocated,
            |_| panic!("cleanup cannot perform execute task operations")
        )
        .is_err());
}

#[test]
#[ignore = "requires owned disposable PostgreSQL cleanup database"]
fn postgres_cleanup_authority_accepts_pre_admission_cancellation_but_not_status_only_failure() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_url, store, mut db) = setup();
    let mut r = run(&store, &mut db, "run-cleanup", 1);
    cleanup_grant(&mut db, &mut r);
    db.batch_execute("UPDATE public.app_core_agentrun SET status='failed'")
        .unwrap();
    assert!(verify(&mut db, &r).is_err());
    db.batch_execute("UPDATE public.app_core_agentrun SET \"preAdmissionCancelledAt\"=now()")
        .unwrap();
    assert!(verify(&mut db, &r).is_ok());
    db.batch_execute("UPDATE public.app_core_sessionenvironment SET controller_epoch=2")
        .unwrap();
    assert!(verify(&mut db, &r).is_err());
}
