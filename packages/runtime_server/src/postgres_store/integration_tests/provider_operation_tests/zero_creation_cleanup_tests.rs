//! A terminal controller can predate any physical creation attempt.
use super::*;
use crate::postgres_store::provider_operations::cleanup::ProviderCleanupReceiptQuery;

// This boundary proves absence without manufacturing a creation attempt.
fn owned_no_provider_effects_receipt(
    store: &PostgresRuntimeStore,
    authority: &ControllerAuthorityRequest<'_>,
) -> Result<serde_json::Value, String> {
    let current = store.confirm_provider_run_no_effects(authority)?;
    serde_json::to_value(current).map_err(|error| error.to_string())
}

fn terminal_before_creation(db: &mut Client, run: &mut Run) {
    db.batch_execute("UPDATE public.app_core_sessionenvironment SET controller_epoch=2,controller_purpose='cleanup'; INSERT INTO public.app_core_sessionevent VALUES('ws_1','sess_1','provider-run',false,'{\"type\":\"agent_run_interrupted\"}'); UPDATE runtime.runtime_jobs SET lease_owner=NULL;").unwrap();
    db.execute("UPDATE runtime.runtime_jobs SET lease_owner=$1,lease_purpose='control',lease_control_stage='cleanup'", &[&run.fence.lease_owner]).unwrap();
    let controller = run.grant.payload.controller.as_mut().unwrap();
    controller.controller_epoch = 2;
    controller.controller_purpose = "cleanup".into();
    run.grant = retained::sign(run.grant.payload.clone());
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_provider_terminal_before_create_requires_owned_absence_receipt_real_release_and_complete(
) {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (url, store, mut db) = retained::setup();
    let mut run = run(&store, &mut db);
    assert!(
        owned_no_provider_effects_receipt(&store, &run.authority(ControllerPurpose::Execute))
            .is_err()
    );
    terminal_before_creation(&mut db, &mut run);
    db.batch_execute("CREATE TABLE IF NOT EXISTS public.app_core_agentrunenvironmentrelease(agent_run_id text PRIMARY KEY,environment_id text NOT NULL,authorization_digest text NOT NULL,controller_epoch bigint NOT NULL); DELETE FROM public.app_core_agentrunenvironmentrelease;").unwrap();
    assert_eq!(db.query_one("SELECT (SELECT count(*) FROM runtime.provider_creation_operations)+(SELECT count(*) FROM runtime.execution_instances)+(SELECT count(*) FROM runtime.resident_sandboxes)", &[]).unwrap().get::<_,i64>(0), 0);
    let control = crate::contract::AgentRunControl {
        agent_run_id: run.original.agent_run_id.clone(),
        authorization_digest: run.original.digest().unwrap(),
        authorization_signature: run.original.signature(KEY).unwrap(),
        authorization: run.original.clone(),
    };
    assert!(
        store
            .confirm_no_effects(&control, &run.fence.job_id, &run.fence.lease_owner)
            .unwrap()
            .is_none(),
        "an owned controller still requires real release; old noOperation proof is inapplicable"
    );
    let complete = || CompleteRuntimeJobRequest {
        job_id: run.fence.job_id.clone(),
        lease_owner: run.fence.lease_owner.clone(),
        output_refs: vec![],
        completed_at_ms: crate::now_ms().unwrap(),
    };
    assert!(store.complete_runtime_job(complete()).is_err());
    let receipt = owned_no_provider_effects_receipt(
        &store,
        &run.authority(ControllerPurpose::Cleanup),
    )
    .expect(
        "terminal Cleanup owner with no physical responsibility needs an explicit absence receipt",
    );
    assert_eq!(
        receipt,
        serde_json::json!({
            "schema":"runtime.agent_run.cleanup_receipt.no_provider_effects.v1",
            "receiptId":receipt["receiptId"],"agentRunId":run.original.agent_run_id,"environmentId":"environment",
            "authorizationDigest":run.original.digest().unwrap(),"controllerEpoch":2,"cleanupComplete":true,
            "provider":"e2b","profileDigest":run.original.environment_profile_digest,"providerConfigDigest":CONFIG,
            "scope":"noProviderEffectsForRun"
        })
    );
    let reopened = PostgresRuntimeStore::new(&url).unwrap();
    assert_eq!(
        owned_no_provider_effects_receipt(&reopened, &run.authority(ControllerPurpose::Cleanup))
            .unwrap(),
        receipt
    );
    let query = ProviderCleanupReceiptQuery {
        schema: "runtime.agent_run.cleanup_receipt.get.v2".into(),
        agent_run_id: run.original.agent_run_id.clone(),
        authorization_digest: run.original.digest().unwrap(),
        cleanup_receipt_id: receipt["receiptId"].as_str().unwrap().into(),
    };
    assert_eq!(
        serde_json::to_value(
            reopened
                .get_provider_cleanup_receipt(&query)
                .unwrap()
                .unwrap()
        )
        .unwrap(),
        receipt
    );
    assert!(
        reopened.complete_runtime_job(complete()).is_err(),
        "absence receipt cannot bypass real business controller release"
    );
    db.execute(
        "INSERT INTO public.app_core_agentrunenvironmentrelease VALUES($1,'environment',$2,2)",
        &[&run.original.agent_run_id, &run.original.digest().unwrap()],
    )
    .unwrap();
    db.batch_execute("UPDATE public.app_core_sessionenvironment SET controller_released=true")
        .unwrap();
    // Real release opens admission before the previous lifecycle Job completes.
    let successor = admit_successor(&store, &mut db, &run);
    let next_operation = store
        .prepare_provider_creation(
            &successor.authority(ControllerPurpose::Execute),
            &request("execution-successor", "attempt-successor", "key-successor"),
        )
        .unwrap();
    store
        .dispatch_provider_creation(
            &successor.authority(ControllerPurpose::Execute),
            &next_operation.identity,
        )
        .unwrap();
    assert_eq!(
        serde_json::to_value(
            reopened
                .get_provider_cleanup_receipt(&query)
                .unwrap()
                .unwrap()
        )
        .unwrap(),
        receipt,
        "successor live responsibility cannot invalidate an immutable prior Run absence proof"
    );
    reopened.complete_runtime_job(complete()).unwrap();
    assert_eq!(
        db.query_one(
            "SELECT status FROM runtime.runtime_jobs WHERE job_id=$1",
            &[&run.fence.job_id]
        )
        .unwrap()
        .get::<_, String>(0),
        "succeeded"
    );
    assert_eq!(db.query_one("SELECT (SELECT count(*) FROM runtime.provider_creation_operations WHERE agent_run_id=$1)+(SELECT count(*) FROM runtime.execution_instances)+(SELECT count(*) FROM runtime.resident_sandboxes)+(SELECT count(*) FROM runtime.provider_run_cleanup_receipts)",&[&run.original.agent_run_id]).unwrap().get::<_,i64>(0),0,"proof must not invent a physical allocation or positive-count settlement");
    assert_eq!(db.query_one("SELECT controller_run_id FROM public.app_core_sessionenvironment WHERE id='environment' AND NOT controller_released",&[]).unwrap().get::<_,String>(0), successor.original.agent_run_id);
    assert_eq!(db.query_one("SELECT count(*) FROM runtime.provider_creation_operations WHERE agent_run_id=$1 AND dispatch_state='dispatched' AND settlement_id IS NULL",&[&successor.original.agent_run_id]).unwrap().get::<_,i64>(0),1);
    assert_eq!(db.query_one("SELECT count(*) FROM runtime.agent_run_cleanup_receipts WHERE agent_run_id=$1 AND allocation_kind='neverAllocated' AND instance_id IS NULL",&[&run.original.agent_run_id]).unwrap().get::<_,i64>(0),1);
    assert_eq!(db.query_one("SELECT count(*) FROM runtime.runtime_events WHERE event_id='agent_run_no_effects:'||$1",&[&run.original.agent_run_id]).unwrap().get::<_,i64>(0),0,"real release is distinct from noOperation release");
}

fn admit_successor(store: &PostgresRuntimeStore, db: &mut Client, prior: &Run) -> Run {
    let mut original = prior.original.clone();
    original.agent_run_id = "provider-successor".into();
    original.id = "provider-successor-authorization".into();
    let digest = original.digest().unwrap();
    let owner = "worker:provider-successor";
    db.execute("INSERT INTO public.app_core_agentrun(id,session_id,environment_id,workspace_id,user_id,membership_ref,status,\"modelConfig_id\",\"thinkingMode\") VALUES($1,'sess_1','environment','ws_1','user_1','member','running','model_1','high')", &[&original.agent_run_id]).unwrap();
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
    db.execute("UPDATE public.app_core_sessionenvironment SET controller_run_id=$1,controller_epoch=3,controller_lease_owner=$2,controller_purpose='execute',controller_released=false",&[&original.agent_run_id,&owner]).unwrap();
    let mut payload = prior.grant.payload.clone();
    payload.agent_run_id = original.agent_run_id.clone();
    payload.agent_run_authorization_digest = digest;
    let controller = payload.controller.as_mut().unwrap();
    controller.controller_id = original.agent_run_id.clone();
    controller.controller_epoch = 3;
    controller.controller_purpose = "execute".into();
    let mut lease_digest = Sha256::new();
    lease_digest.update(b"workspace:environment-controller-lease:v1\0");
    lease_digest.update(owner.as_bytes());
    controller.controller_lease_digest = format!("sha256:{:x}", lease_digest.finalize());
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

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_provider_owned_absence_cannot_erase_a_real_dispatched_unknown_attempt() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (_, store, mut db) = retained::setup();
    let mut run = run(&store, &mut db);
    let prepared = store
        .prepare_provider_creation(
            &run.authority(ControllerPurpose::Execute),
            &request("execution-unknown", "attempt-unknown", "key-unknown"),
        )
        .unwrap();
    store
        .dispatch_provider_creation(
            &run.authority(ControllerPurpose::Execute),
            &prepared.identity,
        )
        .unwrap();
    terminal_before_creation(&mut db, &mut run);
    let operations = store
        .provider_creation_operations_for_cleanup(
            &run.authority(ControllerPurpose::Cleanup),
            CONFIG,
        )
        .unwrap();
    assert_eq!(operations.len(), 1);
    assert_eq!(operations[0].identity, prepared.identity);
    assert_eq!(
        operations[0].dispatch_state,
        ProviderCreationDispatchState::Dispatched
    );
    assert!(operations[0].reported_sandbox_id.is_none());
    assert!(store
        .confirm_provider_run_no_effects(&run.authority(ControllerPurpose::Cleanup))
        .is_err());
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.agent_run_cleanup_receipts WHERE agent_run_id=$1",
            &[&run.original.agent_run_id]
        )
        .unwrap()
        .get::<_, i64>(0),
        0
    );
    assert_eq!(db.query_one("SELECT count(*) FROM runtime.provider_creation_operations WHERE agent_run_id=$1 AND dispatch_state='dispatched' AND reported_sandbox_id IS NULL AND settlement_id IS NULL",&[&run.original.agent_run_id]).unwrap().get::<_,i64>(0),1);
    assert!(store
        .verify_lifecycle_completion(&run.fence.job_id)
        .is_err());
}
