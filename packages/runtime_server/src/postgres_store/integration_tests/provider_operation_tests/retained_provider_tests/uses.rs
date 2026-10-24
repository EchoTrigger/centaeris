use super::*;
use crate::postgres_store::provider_operations::cleanup::{
    ProviderCleanupReceiptQuery, ProviderRemovalEvidence,
};
use crate::postgres_store::provider_operations::retained::{
    RetainedProviderQuiescence, RetainedProviderRunUse,
};

pub(super) fn candidate(
    store: &PostgresRuntimeStore,
    db: &mut Client,
) -> (
    Run,
    crate::postgres_store::provider_operations::ProviderCreationOperation,
    RetainedProviderRunUse,
) {
    db.batch_execute("CREATE TABLE IF NOT EXISTS public.app_core_agentrunenvironmentrelease(agent_run_id text PRIMARY KEY,environment_id text NOT NULL,authorization_digest text NOT NULL,controller_epoch bigint NOT NULL); DELETE FROM public.app_core_agentrunenvironmentrelease;").unwrap();
    let run = retained_run(store, db);
    let operation = store
        .prepare_provider_creation(
            &run.authority(ControllerPurpose::Execute),
            &request("retained-a", "retained-attempt", "retained-key"),
        )
        .unwrap();
    store
        .dispatch_provider_creation(
            &run.authority(ControllerPurpose::Execute),
            &operation.identity,
        )
        .unwrap();
    let operation = store
        .record_provider_creation_observation(&operation.identity, "retained-sandbox")
        .unwrap();
    let observation =
        crate::postgres_store::provider_operations::cleanup::ProviderSandboxObservation {
            sandbox_id: "retained-sandbox".into(),
            template_id: "centaeris-core".into(),
            metadata: crate::postgres_store::provider_operations::cleanup::expected_metadata(
                &operation.identity,
            ),
        };
    store
        .record_retained_provider_execution_target(
            &run.authority(ControllerPurpose::Execute),
            &operation.identity,
            CONFIG,
            &observation,
        )
        .unwrap();
    let use_ = store
        .bind_retained_provider_run_use(
            &run.authority(ControllerPurpose::Execute),
            "retained-a",
            &operation.identity.attempt_id,
            CONFIG,
        )
        .unwrap();
    (run, operation, use_)
}

pub(super) fn proof(value: &RetainedProviderRunUse, epoch: i64) -> RetainedProviderQuiescence {
    RetainedProviderQuiescence {
        agent_run_id: value.agent_run_id.clone(),
        execution_id: value.execution_id.clone(),
        instance_id: value.target.identity.instance_id.clone(),
        sandbox_id: value.target.sandbox_id.clone(),
        controller_epoch: epoch,
        quiescent: true,
    }
}

pub(in crate::postgres_store::integration_tests::provider_operation_tests) fn successor(
    store: &PostgresRuntimeStore,
    db: &mut Client,
    prior: &Run,
) -> Run {
    let mut original = prior.original.clone();
    original.agent_run_id = "retained-b".into();
    original.id = "retained-b-authorization".into();
    let digest = original.digest().unwrap();
    let owner = "worker:retained-b";
    db.execute("INSERT INTO public.app_core_agentrun(id,session_id,environment_id,workspace_id,user_id,membership_ref,status,\"modelConfig_id\",\"thinkingMode\") VALUES($1,'sess_1','environment','ws_1','user_1','member','running','model_1','high')",&[&original.agent_run_id]).unwrap();
    db.execute(
        "INSERT INTO public.app_core_agentrunauthorization VALUES($1,$2,$3::text::jsonb,$4)",
        &[
            &original.agent_run_id,
            &digest,
            &serde_json::to_string(&original).unwrap(),
            &original.signature(KEY).unwrap(),
        ],
    )
    .unwrap();
    let job_id = format!("agent_run.lifecycle:{}", original.agent_run_id);
    let mut record = job(&job_id, &format!("{job_id}:{digest}"));
    record.job_kind = "agent_run.lifecycle".into();
    record.status = RuntimeJobStatus::Running;
    record.lease_owner = Some(owner.into());
    record.lease_expires_at_ms = Some(4_000_000_000_000);
    record.session_id = Some(original.session_id.clone());
    record.payload_ref = Some(format!("record:agent_run:{}", original.agent_run_id));
    store
        .schedule_worker_job(ScheduleRuntimeJobRequest { job: record }, Some("ws_1"))
        .unwrap();
    let epoch = prior
        .grant
        .payload
        .controller
        .as_ref()
        .unwrap()
        .controller_epoch
        + 1;
    db.execute("UPDATE public.app_core_sessionenvironment SET controller_run_id=$1,controller_epoch=$2,controller_lease_owner=$3,controller_purpose='execute',controller_released=false",&[&original.agent_run_id,&epoch,&owner]).unwrap();
    let mut payload = prior.grant.payload.clone();
    payload.agent_run_id = original.agent_run_id.clone();
    payload.agent_run_authorization_digest = digest;
    let controller = payload.controller.as_mut().unwrap();
    controller.controller_id = original.agent_run_id.clone();
    controller.controller_epoch = epoch;
    controller.controller_purpose = "execute".into();
    let mut lease = Sha256::new();
    lease.update(b"workspace:environment-controller-lease:v1\0");
    lease.update(owner.as_bytes());
    controller.controller_lease_digest = format!("sha256:{:x}", lease.finalize());
    Run {
        original,
        grant: retained::sign(payload),
        fence: RuntimeJobLeaseFence {
            job_id,
            job_kind: "agent_run.lifecycle".into(),
            lease_owner: owner.into(),
        },
    }
}

pub(in crate::postgres_store::integration_tests::provider_operation_tests) fn release(
    db: &mut Client,
    run: &Run,
) {
    db.execute(
        "INSERT INTO public.app_core_agentrunenvironmentrelease VALUES($1,'environment',$2,$3)",
        &[
            &run.original.agent_run_id,
            &run.original.digest().unwrap(),
            &run.grant
                .payload
                .controller
                .as_ref()
                .unwrap()
                .controller_epoch,
        ],
    )
    .unwrap();
    db.batch_execute("UPDATE public.app_core_sessionenvironment SET controller_released=true")
        .unwrap();
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_retained_provider_successor_uses_current_authority_after_creator_job_completed() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db) = retained::setup();
    let (mut a, creation, use_a) = candidate(&store, &mut db);
    let before: String = db
        .query_one(
            "SELECT row_to_json(p)::text FROM runtime.provider_creation_operations p",
            &[],
        )
        .unwrap()
        .get(0);
    terminal(&mut db, &mut a);
    assert!(store
        .remove_provider_creation(
            &a.authority(ControllerPurpose::Cleanup),
            &creation.identity,
            CONFIG,
            |_| -> Result<ProviderRemovalEvidence, String> {
                panic!("Run cleanup cannot retire the Environment")
            }
        )
        .is_err());
    let receipt = store
        .confirm_retained_provider_run_cleanup(
            &a.authority(ControllerPurpose::Cleanup),
            &use_a,
            CONFIG,
            |value| Ok(proof(value, 2)),
        )
        .unwrap();
    assert_eq!(receipt.scope, "runEffectsOnly");
    release(&mut db, &a);
    store
        .complete_runtime_job(CompleteRuntimeJobRequest {
            job_id: a.fence.job_id.clone(),
            lease_owner: a.fence.lease_owner.clone(),
            output_refs: vec![],
            completed_at_ms: crate::now_ms().unwrap(),
        })
        .unwrap();
    let b = successor(&store, &mut db, &a);
    let reopened = PostgresRuntimeStore::new(&url).unwrap();
    let use_b = reopened
        .bind_retained_provider_run_use(
            &b.authority(ControllerPurpose::Execute),
            "retained-b-execution",
            &creation.identity.attempt_id,
            CONFIG,
        )
        .unwrap();
    assert_ne!(use_a.provider_use_id, use_b.provider_use_id);
    assert_eq!(use_b.target, use_a.target);
    assert_eq!(
        use_b.target.identity.agent_run_id(),
        Some(a.original.agent_run_id.as_str())
    );
    reopened
        .with_retained_provider_execution_dispatch(
            &b.authority(ControllerPurpose::Execute),
            &use_b,
            CONFIG,
            |target| -> Result<(), String> {
                assert_eq!(target.sandbox_id, "retained-sandbox");
                Ok(())
            },
        )
        .unwrap();
    assert!(reopened
        .with_retained_provider_execution_dispatch(
            &a.authority(ControllerPurpose::Execute),
            &use_a,
            CONFIG,
            |_| -> Result<(), String> { panic!("stale creator cannot dispatch") }
        )
        .is_err());
    let query = ProviderCleanupReceiptQuery {
        schema: "runtime.agent_run.cleanup_receipt.get.v2".into(),
        agent_run_id: a.original.agent_run_id.clone(),
        authorization_digest: a.original.digest().unwrap(),
        cleanup_receipt_id: receipt.receipt_id.clone(),
    };
    assert_eq!(
        serde_json::to_value(
            reopened
                .get_provider_cleanup_receipt(&query)
                .unwrap()
                .unwrap()
        )
        .unwrap(),
        serde_json::to_value(receipt).unwrap()
    );
    assert_eq!(
        db.query_one(
            "SELECT row_to_json(p)::text FROM runtime.provider_creation_operations p",
            &[]
        )
        .unwrap()
        .get::<_, String>(0),
        before
    );
    assert!(db
        .query_one(
            "SELECT settlement_id IS NULL FROM runtime.provider_creation_operations",
            &[]
        )
        .unwrap()
        .get::<_, bool>(0));
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_retained_provider_use_requires_closed_prior_cleanup_and_release() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db) = retained::setup();
    let (a, creation, _) = candidate(&store, &mut db);
    let b = successor(&store, &mut db, &a);
    assert!(store
        .bind_retained_provider_run_use(
            &b.authority(ControllerPurpose::Execute),
            "retained-b-execution",
            &creation.identity.attempt_id,
            CONFIG
        )
        .is_err());
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.provider_environment_run_uses",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        1
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_retained_provider_cleanup_rejects_wrong_or_unconfirmed_quiescence_and_replays_once() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db) = retained::setup();
    let (mut a, _, use_a) = candidate(&store, &mut db);
    terminal(&mut db, &mut a);
    for field in 0..6 {
        let mut evidence = proof(&use_a, 2);
        match field {
            0 => evidence.agent_run_id = "wrong".into(),
            1 => evidence.execution_id = "wrong".into(),
            2 => evidence.instance_id = "wrong".into(),
            3 => evidence.sandbox_id = "wrong".into(),
            4 => evidence.controller_epoch = 1,
            _ => evidence.quiescent = false,
        };
        assert!(store
            .confirm_retained_provider_run_cleanup(
                &a.authority(ControllerPurpose::Cleanup),
                &use_a,
                CONFIG,
                |_| Ok(evidence)
            )
            .is_err());
    }
    assert!(store
        .confirm_retained_provider_run_cleanup(
            &a.authority(ControllerPurpose::Cleanup),
            &use_a,
            CONFIG,
            |_| Err("unknown helper result".into())
        )
        .is_err());
    let receipt = store
        .confirm_retained_provider_run_cleanup(
            &a.authority(ControllerPurpose::Cleanup),
            &use_a,
            CONFIG,
            |value| Ok(proof(value, 2)),
        )
        .unwrap();
    let replay = store
        .confirm_retained_provider_run_cleanup(
            &a.authority(ControllerPurpose::Cleanup),
            &use_a,
            CONFIG,
            |_| panic!("replay must not call the helper"),
        )
        .unwrap();
    assert_eq!(receipt, replay);
    assert!(store.verify_lifecycle_completion(&a.fence.job_id).is_err());
    assert!(db
        .query_one(
            "SELECT settlement_id IS NULL FROM runtime.provider_creation_operations",
            &[]
        )
        .unwrap()
        .get::<_, bool>(0));
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_retained_provider_retirement_barrier_rejects_successor_use() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db) = retained::setup();
    let (mut a, creation, use_a) = candidate(&store, &mut db);
    terminal(&mut db, &mut a);
    store
        .confirm_retained_provider_run_cleanup(
            &a.authority(ControllerPurpose::Cleanup),
            &use_a,
            CONFIG,
            |value| Ok(proof(value, 2)),
        )
        .unwrap();
    release(&mut db, &a);
    db.execute("INSERT INTO runtime.provider_environment_retirements VALUES('retirement','environment',$1,'{}',2,1)",&[&REQUEST]).unwrap();
    let b = successor(&store, &mut db, &a);
    assert!(store
        .bind_retained_provider_run_use(
            &b.authority(ControllerPurpose::Execute),
            "retained-b-execution",
            &creation.identity.attempt_id,
            CONFIG
        )
        .is_err());
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_retained_provider_cancelled_successor_without_a_use_preserves_the_environment() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db) = retained::setup();
    let (mut a, _, use_a) = candidate(&store, &mut db);
    terminal(&mut db, &mut a);
    store
        .confirm_retained_provider_run_cleanup(
            &a.authority(ControllerPurpose::Cleanup),
            &use_a,
            CONFIG,
            |value| Ok(proof(value, 2)),
        )
        .unwrap();
    release(&mut db, &a);
    let mut b = successor(&store, &mut db, &a);
    terminal(&mut db, &mut b);
    let receipt = store
        .confirm_provider_run_no_effects(&b.authority(ControllerPurpose::Cleanup))
        .unwrap();
    assert_eq!(receipt.scope, "noProviderEffectsForRun");
    assert!(store.verify_lifecycle_completion(&b.fence.job_id).is_err());
    release(&mut db, &b);
    assert!(store.verify_lifecycle_completion(&b.fence.job_id).is_ok());
    let reopened = PostgresRuntimeStore::new(&url).unwrap();
    let query = ProviderCleanupReceiptQuery {
        schema: "runtime.agent_run.cleanup_receipt.get.v2".into(),
        agent_run_id: b.original.agent_run_id.clone(),
        authorization_digest: b.original.digest().unwrap(),
        cleanup_receipt_id: receipt.receipt_id.clone(),
    };
    assert!(reopened
        .get_provider_cleanup_receipt(&query)
        .unwrap()
        .is_some());
    assert!(db
        .query_one(
            "SELECT settlement_id IS NULL FROM runtime.provider_creation_operations",
            &[]
        )
        .unwrap()
        .get::<_, bool>(0));
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.provider_environment_run_uses",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        1
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_retained_provider_use_and_target_cannot_be_forged_or_rebound() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db) = retained::setup();
    let (a, creation, use_a) = candidate(&store, &mut db);
    assert!(store
        .bind_retained_provider_run_use(
            &a.authority(ControllerPurpose::Execute),
            "other-execution",
            &creation.identity.attempt_id,
            CONFIG
        )
        .is_err());
    for field in 0..5 {
        let mut wrong = use_a.clone();
        match field {
            0 => wrong.provider_use_id = "wrong".into(),
            1 => wrong.authorization_digest = REQUEST.into(),
            2 => wrong.execution_id = "wrong".into(),
            3 => wrong.target.sandbox_id = "wrong".into(),
            _ => wrong.target.identity.profile_digest = REQUEST.into(),
        };
        assert!(store
            .with_retained_provider_execution_dispatch(
                &a.authority(ControllerPurpose::Execute),
                &wrong,
                CONFIG,
                |_| -> Result<(), String> { panic!("forged consumer cannot dispatch") }
            )
            .is_err());
    }
    assert!(store
        .confirm_provider_run_no_effects(&a.authority(ControllerPurpose::Execute))
        .is_err());
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.provider_environment_run_uses",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        1
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_retained_provider_creator_cleanup_recovers_a_late_verified_target_without_execution_authority(
) {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db) = retained::setup();
    let mut a = retained_run(&store, &mut db);
    let operation = store
        .prepare_provider_creation(
            &a.authority(ControllerPurpose::Execute),
            &request("retained-a", "retained-attempt", "retained-key"),
        )
        .unwrap();
    store
        .dispatch_provider_creation(
            &a.authority(ControllerPurpose::Execute),
            &operation.identity,
        )
        .unwrap();
    let operation = store
        .record_provider_creation_observation(&operation.identity, "retained-sandbox")
        .unwrap();
    terminal(&mut db, &mut a);
    let reopened = PostgresRuntimeStore::new(&url).unwrap();
    reopened.verify_retained_provider_cleanup_target(&a.authority(ControllerPurpose::Cleanup),&operation.identity,CONFIG,|identity,sandbox|Ok(crate::postgres_store::provider_operations::cleanup::ProviderSandboxObservation{sandbox_id:sandbox.into(),template_id:"centaeris-core".into(),metadata:crate::postgres_store::provider_operations::cleanup::expected_metadata(identity)})).unwrap();
    assert!(reopened
        .recover_retained_provider_run_use_for_cleanup(
            &a.authority(ControllerPurpose::Cleanup),
            "invented-execution",
            &operation.identity.attempt_id,
            CONFIG
        )
        .is_err());
    let use_a = reopened
        .recover_retained_provider_run_use_for_cleanup(
            &a.authority(ControllerPurpose::Cleanup),
            "retained-a",
            &operation.identity.attempt_id,
            CONFIG,
        )
        .unwrap();
    assert!(reopened
        .with_retained_provider_execution_dispatch(
            &a.authority(ControllerPurpose::Cleanup),
            &use_a,
            CONFIG,
            |_| -> Result<(), String> { panic!("cleanup recovery cannot execute") }
        )
        .is_err());
    let receipt = reopened
        .confirm_retained_provider_run_cleanup(
            &a.authority(ControllerPurpose::Cleanup),
            &use_a,
            CONFIG,
            |value| Ok(proof(value, 2)),
        )
        .unwrap();
    assert_eq!(receipt.scope, "runEffectsOnly");
    assert!(db
        .query_one(
            "SELECT settlement_id IS NULL FROM runtime.provider_creation_operations",
            &[]
        )
        .unwrap()
        .get::<_, bool>(0));
}
