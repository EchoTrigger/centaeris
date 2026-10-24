//! Real PostgreSQL owner facts for independent provider provisioning.
use super::*;
use crate::environment_authorization::provision::{ProvisionAuthorization, ProvisionBinding};
use centaeris_core::execution::EnvironmentTaskOwner;
use hmac::{Hmac, Mac};

pub(super) fn setup() -> (String, PostgresRuntimeStore, Client, ProvisionAuthorization) {
    let (url, store, mut db) = retained::setup();
    let vector: serde_json::Value = serde_json::from_str(include_str!("../../../../../../tests/workspace/fixtures/environment_provision_authorization/v1/valid.json")).unwrap();
    let mut payload: ProvisionBinding = serde_json::from_value(vector["payload"].clone()).unwrap();
    payload.workspace_id = "ws_1".into();
    payload.session_id = "sess_1".into();
    payload.environment_id = "environment".into();
    payload.user_id = "user_1".into();
    payload.agent_id = "centaeris".into();
    payload.expires_at_ms = 4_000_000_000_000;
    payload.environment_profile = serde_json::from_str(include_str!(
        "../../../../../../tests/workspace/fixtures/e2b_environment_profile/v1/valid.json"
    ))
    .unwrap();
    if let crate::agent_run_authorization::EnvironmentProfile::E2b(profile) =
        &mut payload.environment_profile
    {
        profile.lifetime = "retained".into();
    }
    payload.environment_profile_digest = payload.environment_profile.digest().unwrap();
    payload.controller_lease_digest = format!(
        "sha256:{:x}",
        Sha256::digest(b"workspace:environment-controller-lease:v1\0provision-lease")
    );
    payload.request_digest = format!(
        "sha256:{:x}",
        Sha256::digest(
            crate::agent_run_authorization::canonical_json(
                serde_json::to_value(payload.request()).unwrap()
            )
            .unwrap()
            .as_bytes()
        )
    );
    let mut auth = ProvisionAuthorization {
        payload,
        signature: String::new(),
    };
    let mut mac = Hmac::<Sha256>::new_from_slice(KEY).unwrap();
    mac.update(b"workspace:environment-provision-authorization:v1\0");
    mac.update(auth.digest().unwrap().as_bytes());
    auth.signature = format!("hmac-sha256:{:x}", mac.finalize().into_bytes());
    db.batch_execute("DROP TABLE IF EXISTS public.app_core_environmentmaintenancegrant,public.app_core_environmentmaintenanceoperation CASCADE; ALTER TABLE public.app_core_sessionenvironment ADD COLUMN IF NOT EXISTS controller_maintenance_operation_id text; CREATE TABLE public.app_core_environmentmaintenanceoperation(operation_id text PRIMARY KEY,workspace_id text,session_id text,environment_id text,operation_kind text,request jsonb,request_digest text,profile_digest text,current_grant_id text,state text,instance_id text,container_id text,host_id text,allocation_execution_id text); CREATE TABLE public.app_core_environmentmaintenancegrant(id text PRIMARY KEY,operation_id text,payload jsonb,signature text,digest text);").unwrap();
    let b = &auth.payload;
    db.execute("UPDATE public.app_core_sessionenvironment SET controller_maintenance_operation_id=$1,controller_epoch=$2,controller_lease_owner='provision-lease',controller_purpose='execute',controller_released=false,environment_profile=$3::text::jsonb,environment_profile_digest=$4",&[&b.operation_id,&b.controller_epoch,&serde_json::to_string(&b.environment_profile).unwrap(),&b.environment_profile_digest]).unwrap();
    db.execute("INSERT INTO public.app_core_environmentmaintenanceoperation VALUES($1,'ws_1','sess_1','environment','provision',$2::text::jsonb,$3,$4,'grant','active',NULL,NULL,NULL,NULL)",&[&b.operation_id,&serde_json::to_string(&b.request()).unwrap(),&b.request_digest,&b.environment_profile_digest]).unwrap();
    db.execute("INSERT INTO public.app_core_environmentmaintenancegrant VALUES('grant',$1,$2::text::jsonb,$3,$4)",&[&b.operation_id,&serde_json::to_string(b).unwrap(),&auth.signature,&auth.digest().unwrap()]).unwrap();
    (url, store, db, auth)
}

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_provider_provision_owner_schema_accepts_real_maintenance_without_run_or_job() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, _, mut db, auth) = setup();
    let b = &auth.payload;
    let owner = serde_json::to_string(&EnvironmentTaskOwner::MaintenanceOperation {
        operation_id: b.operation_id.clone(),
    })
    .unwrap();
    db.execute("INSERT INTO runtime.provider_creation_operations(attempt_id,execution_id,instance_id,environment_id,session_id,workspace_id,agent_run_id,authorization_digest,profile_digest,provider,provider_config_digest,request_digest,idempotency_key,processing_job_id,dispatch_state,recorded_at_ms,creation_owner_json,maintenance_operation_id) VALUES('provision-attempt','provision-execution','provision-instance','environment','sess_1','ws_1',NULL,$1,$2,'e2b',$3,$4,'provision-key',NULL,'prepared',0,$5,$6)",&[&auth.digest().unwrap(),&b.environment_profile_digest,&CONFIG,&REQUEST,&owner,&b.operation_id]).expect("a real maintenance origin must not invent Run or Job rows");
    let row=db.query_one("SELECT agent_run_id,processing_job_id,maintenance_operation_id,creation_owner_json FROM runtime.provider_creation_operations",&[]).unwrap();
    assert!(row.get::<_, Option<String>>(0).is_none());
    assert!(row.get::<_, Option<String>>(1).is_none());
    assert_eq!(row.get::<_, String>(2), b.operation_id);
    assert_eq!(
        serde_json::from_str::<EnvironmentTaskOwner>(&row.get::<_, String>(3)).unwrap(),
        b.controller_owner
    );
    assert_eq!(db.query_one("SELECT (SELECT count(*) FROM public.app_core_agentrun)+(SELECT count(*) FROM runtime.runtime_jobs)",&[]).unwrap().get::<_,i64>(0),0);
}

fn verified(
    store: &PostgresRuntimeStore,
    auth: &ProvisionAuthorization,
) -> (
    crate::postgres_store::provider_operations::ProviderCreationOperation,
    crate::postgres_store::provider_operations::cleanup::ProviderVerifiedTarget,
) {
    let operation = store
        .prepare_provider_provision_creation(
            auth,
            KEY,
            &request("provision-execution", "provision-attempt", "provision-key"),
        )
        .unwrap();
    let permit = store
        .dispatch_provider_provision_creation(auth, KEY, &operation.identity)
        .unwrap();
    assert_eq!(
        permit.operation().dispatch_state,
        ProviderCreationDispatchState::Dispatched
    );
    drop(permit);
    store
        .record_provider_creation_observation(&operation.identity, "sandbox-provision")
        .unwrap();
    let crate::agent_run_authorization::EnvironmentProfile::E2b(profile) =
        &auth.payload.environment_profile
    else {
        unreachable!()
    };
    let observed =
        crate::postgres_store::provider_operations::cleanup::ProviderSandboxObservation {
            sandbox_id: "sandbox-provision".into(),
            template_id: profile.template_ref.template_id.clone(),
            metadata: crate::postgres_store::provider_operations::cleanup::expected_metadata(
                &operation.identity,
            ),
        };
    let target = store
        .record_provider_provision_target(auth, KEY, &operation.identity, &observed)
        .unwrap();
    (operation, target)
}

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_provider_provision_owner_prepare_dispatch_and_restart_never_invent_a_run_or_redispatch_unknown(
) {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db, auth) = setup();
    let operation = store
        .prepare_provider_provision_creation(&auth, KEY, &request("execution", "attempt", "key"))
        .unwrap();
    assert_eq!(operation.identity.owner, auth.payload.controller_owner);
    assert!(operation.identity.agent_run_id().is_none());
    assert!(operation.identity.processing_job_id.is_none());
    assert_eq!(
        operation,
        store
            .prepare_provider_provision_creation(
                &auth,
                KEY,
                &request("execution", "attempt", "key")
            )
            .unwrap()
    );
    let permit = store
        .dispatch_provider_provision_creation(&auth, KEY, &operation.identity)
        .unwrap();
    drop(permit);
    drop(store);
    let reopened = PostgresRuntimeStore::new(&url).unwrap();
    let unknown = reopened
        .provider_creation_operation("attempt")
        .unwrap()
        .unwrap();
    assert_eq!(
        unknown.dispatch_state,
        ProviderCreationDispatchState::Dispatched
    );
    assert!(unknown.reported_sandbox_id.is_none());
    assert!(reopened
        .dispatch_provider_provision_creation(&auth, KEY, &unknown.identity)
        .is_err());
    assert!(reopened
        .close_provider_provision_creation_before_dispatch(&auth, KEY, &unknown.identity)
        .is_err());
    assert!(reopened
        .prepare_provider_provision_creation(
            &auth,
            KEY,
            &request("other-execution", "other-attempt", "other-key")
        )
        .is_err());
    assert!(reopened
        .provider_provision_completion(&auth.payload.operation_id)
        .unwrap()
        .is_none());
    let row=db.query_one("SELECT (SELECT count(*) FROM public.app_core_agentrun),(SELECT count(*) FROM runtime.runtime_jobs),settlement_id FROM runtime.provider_creation_operations",&[]).unwrap();
    assert_eq!(row.get::<_, i64>(0), 0);
    assert_eq!(row.get::<_, i64>(1), 0);
    assert!(row.get::<_, Option<String>>(2).is_none());
}

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_provider_provision_owner_concurrent_dispatch_has_one_fresh_permit() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, _, auth) = setup();
    let op = store
        .prepare_provider_provision_creation(&auth, KEY, &request("execution", "attempt", "key"))
        .unwrap();
    let barrier = std::sync::Barrier::new(2);
    let winners = std::thread::scope(|scope| {
        let one = scope.spawn(|| {
            barrier.wait();
            store
                .dispatch_provider_provision_creation(&auth, KEY, &op.identity)
                .is_ok()
        });
        let two = scope.spawn(|| {
            barrier.wait();
            store
                .dispatch_provider_provision_creation(&auth, KEY, &op.identity)
                .is_ok()
        });
        usize::from(one.join().unwrap()) + usize::from(two.join().unwrap())
    });
    assert_eq!(winners, 1);
}

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_provider_provision_owner_handoff_requires_preparation_release_and_exact_original_target(
) {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db, auth) = setup();
    let (_, target) = verified(&store, &auth);
    use crate::postgres_store::provider_operations::provision::{
        require_completed_handoff, ProviderProvisionPreparation,
    };
    assert!(require_completed_handoff(&mut db, &target).is_err());
    let receipt = store
        .complete_provider_provision(
            &auth,
            KEY,
            &target,
            &ProviderProvisionPreparation {
                instance_id: target.identity.instance_id.clone(),
                sandbox_id: target.sandbox_id.clone(),
                preparation_digest: REQUEST.into(),
            },
        )
        .unwrap();
    assert_eq!(receipt.creation_attempt_id, target.identity.attempt_id);
    assert!(require_completed_handoff(&mut db, &target).is_err());
    db.execute("UPDATE public.app_core_environmentmaintenanceoperation SET state='released' WHERE operation_id=$1",&[&auth.payload.operation_id]).unwrap();
    db.execute(
        "UPDATE public.app_core_sessionenvironment SET controller_released=true",
        &[],
    )
    .unwrap();
    require_completed_handoff(&mut db, &target).unwrap();
    let mut wrong = target.clone();
    wrong.verification_digest = format!("sha256:{}", "f".repeat(64));
    assert!(require_completed_handoff(&mut db, &wrong).is_err());
    let mut wrong = target.clone();
    wrong.identity.request_digest = format!("sha256:{}", "f".repeat(64));
    assert!(require_completed_handoff(&mut db, &wrong).is_err());
    assert_eq!(
        store
            .provider_provision_completion(&auth.payload.operation_id)
            .unwrap(),
        Some(receipt)
    );
}

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_provider_provision_owner_cannot_complete_before_verified_target_or_mutate_completion() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db, auth) = setup();
    let (_, target) = verified(&store, &auth);
    use crate::postgres_store::provider_operations::provision::ProviderProvisionPreparation;
    let proof = ProviderProvisionPreparation {
        instance_id: target.identity.instance_id.clone(),
        sandbox_id: target.sandbox_id.clone(),
        preparation_digest: REQUEST.into(),
    };
    let mut wrong = proof.clone();
    wrong.sandbox_id = "foreign-sandbox".into();
    assert!(store
        .complete_provider_provision(&auth, KEY, &target, &wrong)
        .is_err());
    let result = store
        .complete_provider_provision(&auth, KEY, &target, &proof)
        .unwrap();
    assert_eq!(
        store
            .complete_provider_provision(&auth, KEY, &target, &proof)
            .unwrap(),
        result
    );
    for sql in ["UPDATE runtime.provider_environment_provision_completions SET preparation_digest=preparation_digest","DELETE FROM runtime.provider_environment_provision_completions"] {assert!(db.execute(sql,&[]).is_err());}
    let verified_at: i64 = db
        .query_one(
            "SELECT verified_at_ms FROM runtime.provider_creation_operations",
            &[],
        )
        .unwrap()
        .get(0);
    assert!(result.recorded_at_ms >= verified_at);
}

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_provider_provision_owner_rejects_forged_operation_and_mixed_or_mutated_origin() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db, auth) = setup();
    let fake = serde_json::to_string(&EnvironmentTaskOwner::MaintenanceOperation {
        operation_id: "missing-operation".into(),
    })
    .unwrap();
    let error=db.execute("INSERT INTO runtime.provider_creation_operations(attempt_id,execution_id,instance_id,environment_id,session_id,workspace_id,authorization_digest,profile_digest,provider,provider_config_digest,request_digest,idempotency_key,dispatch_state,recorded_at_ms,creation_owner_json,maintenance_operation_id) VALUES('fake-attempt','fake-execution','fake-instance','environment','sess_1','ws_1',$1,$2,'e2b',$3,$4,'fake-key','prepared',0,$5,'missing-operation')",&[&auth.digest().unwrap(),&auth.payload.environment_profile_digest,&CONFIG,&REQUEST,&fake]).unwrap_err();
    assert_eq!(
        error.code(),
        Some(&postgres::error::SqlState::RAISE_EXCEPTION)
    );
    let op = store
        .prepare_provider_provision_creation(&auth, KEY, &request("execution", "attempt", "key"))
        .unwrap();
    assert!(db
        .execute(
            "UPDATE runtime.provider_creation_operations SET creation_owner_json='{}'",
            &[]
        )
        .is_err());
    assert!(db
        .execute(
            "UPDATE runtime.provider_creation_operations SET processing_job_id='invented-job'",
            &[]
        )
        .is_err());
    assert!(db
        .execute("DELETE FROM runtime.provider_creation_operations", &[])
        .is_err());
    let mut changed = auth.clone();
    changed.payload.operation_id = "missing-operation".into();
    changed.payload.controller_owner = EnvironmentTaskOwner::MaintenanceOperation {
        operation_id: "missing-operation".into(),
    };
    assert!(store
        .prepare_provider_provision_creation(
            &changed,
            KEY,
            &request("foreign", "foreign", "foreign")
        )
        .is_err());
    assert_eq!(
        store
            .provider_creation_operation("attempt")
            .unwrap()
            .unwrap(),
        op
    );
}

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_provider_provision_owner_driver_sends_once_and_preserves_transport_unknown_after_restart(
) {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db, auth) = setup();
    use crate::e2b_creation::{E2bCreationDriver, E2bProvisionCreationRequest};
    struct Unknown(std::sync::atomic::AtomicUsize);
    impl crate::e2b_creation::E2bCreationAdapter for Unknown {
        fn create(
            &self,
            _id: &str,
            payload: serde_json::Value,
            _timeout: std::time::Duration,
        ) -> Result<crate::e2b_adapter::AdapterResponse, crate::e2b_adapter::AdapterFailure>
        {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let metadata = payload["metadata"].as_object().unwrap();
            assert_eq!(metadata.len(), 10);
            assert_eq!(metadata["creationOwnerKind"], "maintenanceOperation");
            assert!(metadata["maintenanceOperationId"]
                .as_str()
                .is_some_and(|id| !id.is_empty()));
            assert!(!metadata.contains_key("agentRunId"));
            assert!(!metadata.contains_key("processingJobId"));
            Err(crate::e2b_adapter::AdapterFailure::Unknown("response_lost"))
        }
    }
    let adapter = Unknown(std::sync::atomic::AtomicUsize::new(0));
    let request = E2bProvisionCreationRequest {
        authorization: &auth,
        signing_key: KEY,
        execution_id: "execution",
        attempt_id: "attempt",
        idempotency_key: "key",
        configured_provider_config_digest: CONFIG,
        transport_timeout: std::time::Duration::from_secs(10),
    };
    let op = E2bCreationDriver::new(&store, &adapter)
        .create_provision_or_reopen(&request)
        .unwrap();
    assert_eq!(op.dispatch_state, ProviderCreationDispatchState::Dispatched);
    assert!(op.reported_sandbox_id.is_none());
    drop(store);
    let reopened = PostgresRuntimeStore::new(&url).unwrap();
    assert_eq!(
        E2bCreationDriver::new(&reopened, &adapter)
            .create_provision_or_reopen(&request)
            .unwrap(),
        op
    );
    assert_eq!(adapter.0.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(db.query_one("SELECT (SELECT count(*) FROM public.app_core_agentrun)+(SELECT count(*) FROM runtime.runtime_jobs)",&[]).unwrap().get::<_,i64>(0),0);
}

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_provider_provision_owner_dispatched_evidence_survives_source_removal() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db, auth) = setup();
    let op = store
        .prepare_provider_provision_creation(&auth, KEY, &request("execution", "attempt", "key"))
        .unwrap();
    drop(
        store
            .dispatch_provider_provision_creation(&auth, KEY, &op.identity)
            .unwrap(),
    );
    db.execute(
        "DELETE FROM public.app_core_environmentmaintenancegrant",
        &[],
    )
    .unwrap();
    db.execute(
        "DELETE FROM public.app_core_environmentmaintenanceoperation",
        &[],
    )
    .unwrap();
    let evidence = store
        .record_provider_creation_observation(&op.identity, "late-unknown-sandbox")
        .expect(
            "immutable responsibility must retain late evidence independently of source retention",
        );
    assert_eq!(
        evidence.reported_sandbox_id.as_deref(),
        Some("late-unknown-sandbox")
    );
    assert!(store
        .provider_provision_completion(&auth.payload.operation_id)
        .unwrap()
        .is_none());
    assert!(db
        .query_one(
            "SELECT settlement_id FROM runtime.provider_creation_operations",
            &[]
        )
        .unwrap()
        .get::<_, Option<String>>(0)
        .is_none());
}

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_provider_provision_owner_execution_pins_real_authority_and_rejects_stale_before_callback(
) {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db, auth) = setup();
    let (_, target) = verified(&store, &auth);
    use crate::postgres_store::provider_operations::cleanup::ProviderExecutionDispatchError;
    let calls = std::sync::atomic::AtomicUsize::new(0);
    let seen=store.with_provider_provision_execution_dispatch(&auth,KEY,&target.identity,CONFIG,|value|->Result<_,()>{
        calls.fetch_add(1,std::sync::atomic::Ordering::SeqCst);
        let mut writer=Client::connect(&url,NoTls).unwrap();writer.batch_execute("SET lock_timeout='30ms'").unwrap();
        let error=writer.execute("UPDATE public.app_core_sessionenvironment SET controller_epoch=controller_epoch+1",&[]).unwrap_err();
        assert_eq!(error.code(),Some(&postgres::error::SqlState::LOCK_NOT_AVAILABLE));
        Ok(value.clone())
    }).unwrap();
    assert_eq!(seen, target);
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    let result = store.with_provider_provision_execution_dispatch(
        &auth,
        KEY,
        &target.identity,
        CONFIG,
        |_| -> Result<(), &str> { Err("providerUnknown") },
    );
    assert!(matches!(
        result,
        Err(ProviderExecutionDispatchError::Callback("providerUnknown"))
    ));
    db.execute(
        "UPDATE public.app_core_sessionenvironment SET controller_released=true",
        &[],
    )
    .unwrap();
    let result = store.with_provider_provision_execution_dispatch(
        &auth,
        KEY,
        &target.identity,
        CONFIG,
        |_| -> Result<(), ()> {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        },
    );
    assert!(matches!(
        result,
        Err(ProviderExecutionDispatchError::Rejected(_))
    ));
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_provider_provision_owner_forward_migration_preserves_legacy_unknown_and_original_facts()
{
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db) = retained::setup();
    let mut profile: crate::agent_run_authorization::EnvironmentProfile =
        serde_json::from_str(include_str!(
            "../../../../../../tests/workspace/fixtures/e2b_environment_profile/v1/valid.json"
        ))
        .unwrap();
    let crate::agent_run_authorization::EnvironmentProfile::E2b(p) = &mut profile else {
        unreachable!()
    };
    p.lifetime = "retained".into();
    let mut run = run_with_profile(&store, &mut db, profile);
    run.grant.payload.execution_mode = "retained".into();
    run.grant = retained::sign(run.grant.payload.clone());
    let op = store
        .prepare_provider_creation(
            &run.authority(ControllerPurpose::Execute),
            &request("legacy-execution", "legacy-attempt", "legacy-key"),
        )
        .unwrap();
    let permit = store
        .dispatch_provider_creation(&run.authority(ControllerPurpose::Execute), &op.identity)
        .unwrap();
    let original = permit.operation().clone();
    drop(permit);
    let metadata = serde_json::to_string(
        &crate::postgres_store::provider_operations::cleanup::expected_metadata(&op.identity),
    )
    .unwrap();
    crate::postgres_store::provider_operations::origin::schema::restore_previous_fixture(&mut db);
    let before: String = db
        .query_one(
            "SELECT to_jsonb(p)::text FROM runtime.provider_creation_operations p",
            &[],
        )
        .unwrap()
        .get(0);
    let grants:String=db.query_one("SELECT jsonb_agg(to_jsonb(g) ORDER BY agent_run_id)::text FROM public.app_core_agentrunauthorization g",&[]).unwrap().get(0);
    let versions = db
        .query(
            "SELECT version,applied_at_ms FROM runtime.schema_migrations ORDER BY version",
            &[],
        )
        .unwrap()
        .into_iter()
        .map(|r| (r.get::<_, i64>(0), r.get::<_, i64>(1)))
        .collect::<Vec<_>>();
    assert_eq!(versions.len(), 21);
    let reopened = PostgresRuntimeStore::new(&url).unwrap();
    let after:String=db.query_one("SELECT (to_jsonb(p)-'creation_owner_json'-'maintenance_operation_id')::text FROM runtime.provider_creation_operations p",&[]).unwrap().get(0);
    assert_eq!(after, before);
    assert_eq!(db.query_one("SELECT jsonb_agg(to_jsonb(g) ORDER BY agent_run_id)::text FROM public.app_core_agentrunauthorization g",&[]).unwrap().get::<_,String>(0),grants);
    assert_eq!(
        reopened
            .provider_creation_operation("legacy-attempt")
            .unwrap(),
        Some(original.clone())
    );
    assert_eq!(
        serde_json::to_string(
            &crate::postgres_store::provider_operations::cleanup::expected_metadata(
                &original.identity
            )
        )
        .unwrap(),
        metadata
    );
    assert!(reopened
        .dispatch_provider_creation(
            &run.authority(ControllerPurpose::Execute),
            &original.identity
        )
        .is_err());
    assert!(reopened
        .retained_provider_run_use(&run.original.agent_run_id)
        .unwrap()
        .is_none());
    drop(reopened);
    let reopened = PostgresRuntimeStore::new(&url).unwrap();
    assert_eq!(
        reopened
            .provider_creation_operation("legacy-attempt")
            .unwrap(),
        Some(original)
    );
    assert_eq!(db.query("SELECT version,applied_at_ms FROM runtime.schema_migrations WHERE version<=21 ORDER BY version",&[]).unwrap().into_iter().map(|r|(r.get::<_,i64>(0),r.get::<_,i64>(1))).collect::<Vec<_>>(),versions);
    assert_eq!(
        db.query_one("SELECT max(version) FROM runtime.schema_migrations", &[])
            .unwrap()
            .get::<_, i64>(0),
        24
    );
    let row=db.query_one("SELECT reported_sandbox_id,verified_sandbox_id,settlement_id FROM runtime.provider_creation_operations",&[]).unwrap();
    for c in 0..3 {
        assert!(row.get::<_, Option<String>>(c).is_none());
    }
}

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_provider_provision_owner_forward_migration_rolls_back_all_changes_on_invalid_legacy_origin(
) {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db) = retained::setup();
    let run = run(&store, &mut db);
    let op = store
        .prepare_provider_creation(
            &run.authority(ControllerPurpose::Execute),
            &request("execution", "attempt", "key"),
        )
        .unwrap();
    drop(
        store
            .dispatch_provider_creation(&run.authority(ControllerPurpose::Execute), &op.identity)
            .unwrap(),
    );
    crate::postgres_store::provider_operations::origin::schema::restore_previous_fixture(&mut db);
    db.execute("INSERT INTO runtime.provider_creation_operations(attempt_id,execution_id,instance_id,environment_id,session_id,workspace_id,agent_run_id,authorization_digest,profile_digest,provider,provider_config_digest,request_digest,idempotency_key,processing_job_id,dispatch_state,recorded_at_ms) SELECT 'invalid-attempt','invalid-execution','invalid-instance','invalid-environment',session_id,workspace_id,'',authorization_digest,profile_digest,provider,provider_config_digest,request_digest,'invalid-key',processing_job_id,'prepared',recorded_at_ms FROM runtime.provider_creation_operations WHERE attempt_id='attempt'",&[]).unwrap();
    let before:String=db.query_one("SELECT jsonb_agg(to_jsonb(p) ORDER BY attempt_id)::text FROM runtime.provider_creation_operations p",&[]).unwrap().get(0);
    let versions = db
        .query(
            "SELECT version,applied_at_ms FROM runtime.schema_migrations ORDER BY version",
            &[],
        )
        .unwrap()
        .into_iter()
        .map(|r| (r.get::<_, i64>(0), r.get::<_, i64>(1)))
        .collect::<Vec<_>>();
    assert_eq!(versions.len(), 21);
    assert!(PostgresRuntimeStore::new(&url).is_err());
    assert_eq!(db.query_one("SELECT jsonb_agg(to_jsonb(p) ORDER BY attempt_id)::text FROM runtime.provider_creation_operations p",&[]).unwrap().get::<_,String>(0),before);
    assert_eq!(
        db.query(
            "SELECT version,applied_at_ms FROM runtime.schema_migrations ORDER BY version",
            &[]
        )
        .unwrap()
        .into_iter()
        .map(|r| (r.get::<_, i64>(0), r.get::<_, i64>(1)))
        .collect::<Vec<_>>(),
        versions
    );
    assert_eq!(db.query_one("SELECT count(*) FROM information_schema.columns WHERE table_schema='runtime' AND table_name='provider_creation_operations' AND column_name IN('creation_owner_json','maintenance_operation_id')",&[]).unwrap().get::<_,i64>(0),0);
    assert!(db
        .query_one(
            "SELECT to_regclass('runtime.provider_environment_provision_completions') IS NULL",
            &[]
        )
        .unwrap()
        .get::<_, bool>(0));
    crate::postgres_store::provider_operations::validate(&mut db).unwrap();
}

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_provider_provision_owner_history_reader_rejects_changed_signed_frozen_origin() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db, auth) = setup();
    let (_, target) = verified(&store, &auth);
    use crate::postgres_store::provider_operations::provision::ProviderProvisionPreparation;
    let receipt = store
        .complete_provider_provision(
            &auth,
            KEY,
            &target,
            &ProviderProvisionPreparation {
                instance_id: target.identity.instance_id.clone(),
                sandbox_id: target.sandbox_id.clone(),
                preparation_digest: REQUEST.into(),
            },
        )
        .unwrap();
    let original:String=db.query_one("SELECT payload::text FROM public.app_core_environmentmaintenancegrant WHERE operation_id=$1",&[&auth.payload.operation_id]).unwrap().get(0);
    let changed = format!("sha256:{}", "f".repeat(64));
    for path in [
        vec!["environmentProfile", "providerConfigDigest"],
        vec!["environmentProfileDigest"],
        vec!["requestDigest"],
    ] {
        db.execute("UPDATE public.app_core_environmentmaintenancegrant SET payload=jsonb_set(payload,$2,$3::text::jsonb) WHERE operation_id=$1",&[&auth.payload.operation_id,&path,&serde_json::to_string(&changed).unwrap()]).unwrap();
        assert!(
            store
                .provider_provision_completion(&auth.payload.operation_id)
                .is_err(),
            "historical receipt must bind the signed immutable frozen origin"
        );
        db.execute("UPDATE public.app_core_environmentmaintenancegrant SET payload=$2::text::jsonb WHERE operation_id=$1",&[&auth.payload.operation_id,&original]).unwrap();
        assert_eq!(
            store
                .provider_provision_completion(&auth.payload.operation_id)
                .unwrap(),
            Some(receipt.clone())
        );
    }
}

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_provider_provision_owner_reopen_rejects_weakened_same_named_owner_constraint() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, _, mut db, _) = setup();
    db.batch_execute("ALTER TABLE runtime.provider_creation_operations DROP CONSTRAINT provider_creation_owner_facts; ALTER TABLE runtime.provider_creation_operations ADD CONSTRAINT provider_creation_owner_facts CHECK(true)").unwrap();
    assert!(
        PostgresRuntimeStore::new(&url).is_err(),
        "a same-named relaxed origin check cannot authorize a supported Runtime schema"
    );
}

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_provider_provision_owner_cannot_complete_an_unverified_creation() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, _, auth) = setup();
    let op = store
        .prepare_provider_provision_creation(&auth, KEY, &request("execution", "attempt", "key"))
        .unwrap();
    drop(
        store
            .dispatch_provider_provision_creation(&auth, KEY, &op.identity)
            .unwrap(),
    );
    use crate::postgres_store::provider_operations::{
        cleanup::ProviderVerifiedTarget, provision::ProviderProvisionPreparation,
    };
    let target = ProviderVerifiedTarget {
        identity: op.identity,
        sandbox_id: "unverified-sandbox".into(),
        verification_digest: REQUEST.into(),
    };
    assert!(store
        .complete_provider_provision(
            &auth,
            KEY,
            &target,
            &ProviderProvisionPreparation {
                instance_id: target.identity.instance_id.clone(),
                sandbox_id: target.sandbox_id.clone(),
                preparation_digest: REQUEST.into()
            }
        )
        .is_err());
    assert!(store
        .provider_provision_completion(&auth.payload.operation_id)
        .unwrap()
        .is_none());
}

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_provider_provision_owner_driver_preflight_and_success_preserve_real_origin_without_runs(
) {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    use crate::e2b_creation::{E2bCreationDriver, E2bProvisionCreationRequest};
    struct Adapter(std::sync::atomic::AtomicUsize);
    impl crate::e2b_creation::E2bCreationAdapter for Adapter {
        fn create(
            &self,
            id: &str,
            payload: serde_json::Value,
            _timeout: std::time::Duration,
        ) -> Result<crate::e2b_adapter::AdapterResponse, crate::e2b_adapter::AdapterFailure>
        {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            assert_eq!(
                payload["metadata"]["creationOwnerKind"],
                "maintenanceOperation"
            );
            assert_eq!(payload["metadata"].as_object().unwrap().len(), 10);
            Ok(serde_json::from_value(serde_json::json!({"schema":"runtime.e2b.adapter.response.v1","requestId":id,"outcome":"succeeded","result":{"attemptId":id,"sandboxId":"provision-driver-sandbox","templateId":payload["templateRef"]["templateId"],"metadata":payload["metadata"],"state":"running"}})).unwrap())
        }
    }
    for valid_timeout in [false, true] {
        let (_, store, mut db, auth) = setup();
        let adapter = Adapter(std::sync::atomic::AtomicUsize::new(0));
        let req = E2bProvisionCreationRequest {
            authorization: &auth,
            signing_key: KEY,
            execution_id: "execution",
            attempt_id: "attempt",
            idempotency_key: "key",
            configured_provider_config_digest: CONFIG,
            transport_timeout: if valid_timeout {
                std::time::Duration::from_secs(10)
            } else {
                std::time::Duration::ZERO
            },
        };
        let result = E2bCreationDriver::new(&store, &adapter).create_provision_or_reopen(&req);
        let op = store
            .provider_creation_operation("attempt")
            .unwrap()
            .unwrap();
        if valid_timeout {
            assert!(result.is_ok());
            assert_eq!(adapter.0.load(std::sync::atomic::Ordering::SeqCst), 1);
            assert_eq!(op.dispatch_state, ProviderCreationDispatchState::Dispatched);
            assert_eq!(
                db.query_one(
                    "SELECT verified_sandbox_id FROM runtime.provider_creation_operations",
                    &[]
                )
                .unwrap()
                .get::<_, String>(0),
                "provision-driver-sandbox"
            );
        } else {
            assert!(matches!(
                result,
                Err(crate::e2b_creation::E2bCreationError::Rejected(
                    "e2b_creation_transport_timeout_invalid"
                ))
            ));
            assert_eq!(adapter.0.load(std::sync::atomic::Ordering::SeqCst), 0);
            assert_eq!(
                op.dispatch_state,
                ProviderCreationDispatchState::NotDispatched
            );
        }
        assert_eq!(db.query_one("SELECT (SELECT count(*) FROM public.app_core_agentrun)+(SELECT count(*) FROM runtime.runtime_jobs)",&[]).unwrap().get::<_,i64>(0),0);
        assert!(db
            .query_one(
                "SELECT settlement_id FROM runtime.provider_creation_operations",
                &[]
            )
            .unwrap()
            .get::<_, Option<String>>(0)
            .is_none());
        assert!(store
            .provider_provision_completion(&auth.payload.operation_id)
            .unwrap()
            .is_none());
    }
}
