use super::*;
use crate::postgres_store::controller_authority::{
    verify_controller_authority, ControllerAuthorityRequest, ControllerPurpose,
    VerifiedOriginalGrant,
};

fn cleanup_grant(db: &mut Client, r: &mut Run) {
    let mut binding = r.grant.payload.clone();
    binding.controller.as_mut().unwrap().controller_purpose = "cleanup".into();
    r.grant = sign(binding);
    db.execute("UPDATE public.app_core_sessionenvironment SET controller_purpose='cleanup' WHERE id='environment'", &[]).unwrap();
}

fn verify(db: &mut Client, r: &Run) -> Result<(), String> {
    verify_purpose(db, r, ControllerPurpose::Cleanup).map(|_| ())
}

fn verify_purpose(
    db: &mut Client,
    r: &Run,
    purpose: ControllerPurpose,
) -> Result<VerifiedOriginalGrant, String> {
    let mut tx = db.transaction().map_err(|e| e.to_string())?;
    let original_timeouts = tx
        .query_one(
            "SELECT current_setting('lock_timeout'),current_setting('statement_timeout')",
            &[],
        )
        .map_err(|e| e.to_string())?;
    let grant = verify_controller_authority(
        &mut tx,
        &ControllerAuthorityRequest {
            environment_authorization: &r.grant,
            signing_key: KEY,
            workspace_id: &r.original.workspace_id,
            lease: &r.fence,
            purpose,
        },
    )?;
    let timeouts = tx
        .query_one(
            "SELECT current_setting('lock_timeout'),current_setting('statement_timeout')",
            &[],
        )
        .map_err(|e| e.to_string())?;
    assert_eq!(
        (timeouts.get::<_, String>(0), timeouts.get::<_, String>(1)),
        (
            original_timeouts.get::<_, String>(0),
            original_timeouts.get::<_, String>(1)
        ),
        "authority lock waits restore the transaction's policy before physical work"
    );
    tx.commit().map_err(|e| e.to_string())?;
    Ok(grant)
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
    let profile = r.original.environment_profile.digest().unwrap();
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
    let original_timeouts = db
        .query_one(
            "SELECT current_setting('lock_timeout'),current_setting('statement_timeout')",
            &[],
        )
        .unwrap();
    db.batch_execute("SET lock_timeout='7s'; SET statement_timeout='9s'")
        .unwrap();
    let grant = verify_purpose(&mut db, &r, ControllerPurpose::Execute)
        .expect("current signed authority succeeds with non-default session timeouts");
    assert_eq!(grant.authorization, r.original);
    assert_eq!(grant.authorization_digest, r.original.digest().unwrap());
    assert_eq!(grant.controller_epoch, 1);
    let assert_timeouts_restored = |db: &mut Client| {
        let timeouts = db
            .query_one(
                "SELECT current_setting('lock_timeout'),current_setting('statement_timeout')",
                &[],
            )
            .unwrap();
        assert_eq!(timeouts.get::<_, String>(0), "7s");
        assert_eq!(timeouts.get::<_, String>(1), "9s");
    };
    assert_timeouts_restored(&mut db);
    db.execute(
        "UPDATE runtime.runtime_jobs SET lease_expires_at_ms=1 WHERE job_id=$1",
        &[&r.fence.job_id],
    )
    .unwrap();
    assert_eq!(
        verify_purpose(&mut db, &r, ControllerPurpose::Execute)
            .err()
            .expect("the same connection must reject authority expired in a later transaction"),
        "retained job fence rejected"
    );
    assert_timeouts_restored(&mut db);
    db.execute(
        "UPDATE runtime.runtime_jobs SET lease_expires_at_ms=4000000000000 WHERE job_id=$1",
        &[&r.fence.job_id],
    )
    .unwrap();
    verify_purpose(&mut db, &r, ControllerPurpose::Execute)
        .expect("the same connection observes restored current lease authority");
    assert_timeouts_restored(&mut db);
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
    db.query_one(
        "SELECT set_config('lock_timeout',$1,false),set_config('statement_timeout',$2,false)",
        &[
            &original_timeouts.get::<_, String>(0),
            &original_timeouts.get::<_, String>(1),
        ],
    )
    .unwrap();
}

#[test]
#[ignore = "requires owned disposable PostgreSQL cleanup database"]
fn postgres_cleanup_authority_accepts_bound_pre_admission_failure_and_rejects_reexecution() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_url, store, mut db) = setup();
    let mut r = run(&store, &mut db, "run-before-admission", 1);
    cleanup_grant(&mut db, &mut r);
    db.batch_execute("UPDATE public.app_core_agentrun SET status='failed'")
        .unwrap();
    assert!(
        verify(&mut db, &r).is_err(),
        "status alone grants no cleanup authority"
    );
    let payload = serde_json::json!({
        "schema": "runtime.agent_run.pre_admission_failure.v1",
        "workspaceId": r.original.workspace_id,
        "sessionId": r.original.session_id,
        "agentRunId": r.original.agent_run_id,
        "authorizationDigest": r.original.digest().unwrap(),
        "jobId": r.fence.job_id,
        "leaseOwner": r.fence.lease_owner,
        "failedAtMs": 1234,
    });
    db.execute("INSERT INTO runtime.runtime_events(event_id,session_id,task_id,event_type,at_ms,visibility,payload_json) VALUES($1,'sess_1',$2,'runtime.agent_run.pre_admission_failure.v1',1234,'internal',$3)",
        &[&format!("agent_run_pre_admission_failure:{}",r.original.agent_run_id), &r.original.agent_run_id, &payload.to_string()]).unwrap();
    verify(&mut db, &r)
        .expect("fenced pre-admission failure permits cleanup without model history");
    let mut execute = r.grant.payload.clone();
    execute.controller.as_mut().unwrap().controller_purpose = "execute".into();
    r.grant = sign(execute);
    db.batch_execute("UPDATE public.app_core_sessionenvironment SET controller_purpose='execute'; UPDATE public.app_core_agentrun SET status='running'").unwrap();
    let mut tx = db.transaction().unwrap();
    assert!(
        verify_controller_authority(
            &mut tx,
            &ControllerAuthorityRequest {
                environment_authorization: &r.grant,
                signing_key: KEY,
                workspace_id: &r.original.workspace_id,
                lease: &r.fence,
                purpose: ControllerPurpose::Execute,
            }
        )
        .is_err(),
        "terminal failure cannot execute again"
    );
}
