use super::*;
use crate::e2b_runtime::{E2bCleanupOutcome, E2bCleanupPendingStage};
use crate::postgres_store::provider_operations::cleanup::{
    expected_metadata, ProviderSandboxObservation,
};
use crate::postgres_store::provider_operations::ProviderCreationOperation;
use serde_json::json;

fn to_cleanup(db: &mut Client, run: &mut Run) {
    db.batch_execute("UPDATE public.app_core_sessionenvironment SET controller_epoch=2,controller_purpose='cleanup'; INSERT INTO public.app_core_sessionevent VALUES('ws_1','sess_1','provider-run',false,'{\"type\":\"agent_run_interrupted\"}'); UPDATE runtime.runtime_jobs SET lease_owner=NULL").unwrap();
    db.execute("UPDATE runtime.runtime_jobs SET lease_owner=$1,lease_purpose='control',lease_control_stage='cleanup'",&[&run.fence.lease_owner]).unwrap();
    let controller = run.grant.payload.controller.as_mut().unwrap();
    controller.controller_epoch = 2;
    controller.controller_purpose = "cleanup".into();
    run.grant = retained::sign(run.grant.payload.clone());
}

fn candidate(store: &PostgresRuntimeStore, db: &mut Client) -> (Run, ProviderCreationOperation) {
    let mut run = run_with_profile(store, db, factory_profile());
    let operation = store
        .prepare_provider_creation(
            &run.authority(ControllerPurpose::Execute),
            &request("execution-cleanup", "attempt-cleanup", "key-cleanup"),
        )
        .unwrap();
    store
        .dispatch_provider_creation(
            &run.authority(ControllerPurpose::Execute),
            &operation.identity,
        )
        .unwrap();
    let operation = store
        .record_provider_creation_observation(&operation.identity, "cleanup-sandbox")
        .unwrap();
    to_cleanup(db, &mut run);
    (run, operation)
}

fn info(identity: &crate::postgres_store::provider_operations::ProviderCreationIdentity) -> Value {
    json!({"sandboxId":"cleanup-sandbox","templateId":"centaeris-core","metadata":expected_metadata(identity),"state":"running"})
}

fn runtime(
    store: &PostgresRuntimeStore,
    run: &Run,
    mode: &'static str,
    log: PathBuf,
    observation: Value,
) -> (E2bRuntime, Arc<FixtureLauncher>) {
    let launcher = Arc::new(FixtureLauncher::new(mode, log).with_observation(observation));
    let runtime = E2bRuntime::with_launcher(
        store.clone(),
        config(run.original.environment_profile.clone()),
        launcher.clone(),
    );
    (runtime, launcher)
}

fn require_complete(outcome: E2bCleanupOutcome, count: u32) {
    assert!(
        matches!(outcome,E2bCleanupOutcome::Complete {operation_count} if operation_count==count)
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_e2b_runtime_cleanup_zero_and_unsent_operations_need_no_sdk_process() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    for mode in ["zero", "prepared", "notDispatched"] {
        let (_, store, mut db) = retained::setup();
        let mut run = run_with_profile(&store, &mut db, factory_profile());
        if mode != "zero" {
            let operation = store
                .prepare_provider_creation(
                    &run.authority(ControllerPurpose::Execute),
                    &request("execution-cleanup", "attempt-cleanup", "key-cleanup"),
                )
                .unwrap();
            if mode == "notDispatched" {
                store
                    .close_provider_creation_before_dispatch(
                        &run.authority(ControllerPurpose::Execute),
                        &operation.identity,
                    )
                    .unwrap();
            }
        }
        to_cleanup(&mut db, &mut run);
        let root = tempfile::tempdir().unwrap();
        let (runtime, launcher) = runtime(
            &store,
            &run,
            "cleanupSuccess",
            root.path().join("cleanup.jsonl"),
            Value::Null,
        );
        require_complete(
            runtime
                .cleanup(&run.authority(ControllerPurpose::Cleanup))
                .unwrap(),
            u32::from(mode != "zero"),
        );
        assert_eq!(launcher.launches.load(Ordering::SeqCst), 0, "{mode}");
        if mode != "zero" {
            let row=db.query_one("SELECT dispatch_state,settlement_kind FROM runtime.provider_creation_operations",&[]).unwrap();
            assert_eq!(row.get::<_, String>(0), "notDispatched");
            assert_eq!(row.get::<_, String>(1), "notDispatched");
        }
    }
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_e2b_runtime_cleanup_verifies_candidate_then_deletes_once_and_replays_without_get() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (_, store, mut db) = retained::setup();
    let (run, operation) = candidate(&store, &mut db);
    let root = tempfile::tempdir().unwrap();
    let (runtime, launcher) = runtime(
        &store,
        &run,
        "cleanupSuccess",
        root.path().join("cleanup.jsonl"),
        info(&operation.identity),
    );
    require_complete(
        runtime
            .cleanup(&run.authority(ControllerPurpose::Cleanup))
            .unwrap(),
        1,
    );
    let requests = launcher.requests();
    assert_eq!(
        requests
            .iter()
            .map(|request| request["operation"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["capabilities", "inspect", "kill"]
    );
    assert!(requests
        .iter()
        .skip(1)
        .all(|request| request["payload"]["sandboxId"] == "cleanup-sandbox"));
    let row=db.query_one("SELECT verified_sandbox_id,settlement_kind,settlement_sandbox_id,verified_at_ms<=settled_at_ms FROM runtime.provider_creation_operations",&[]).unwrap();
    assert_eq!(row.get::<_, String>(0), "cleanup-sandbox");
    assert_eq!(row.get::<_, String>(1), "removed");
    assert_eq!(row.get::<_, String>(2), "cleanup-sandbox");
    assert!(row.get::<_, bool>(3));
    require_complete(
        runtime
            .cleanup(&run.authority(ControllerPurpose::Cleanup))
            .unwrap(),
        1,
    );
    assert_eq!(launcher.launches.load(Ordering::SeqCst), 1);
    assert_eq!(
        launcher.requests().len(),
        3,
        "lost complete replies must replay stored settlements without another GET/delete"
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_e2b_runtime_cleanup_unknown_candidate_and_stale_authority_keep_duty() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (_, store, mut db) = retained::setup();
    let (run, operation) = candidate(&store, &mut db);
    let root = tempfile::tempdir().unwrap();
    let (runtime, launcher) = runtime(
        &store,
        &run,
        "inspectUnknown",
        root.path().join("cleanup.jsonl"),
        info(&operation.identity),
    );
    assert!(
        matches!(runtime.cleanup(&run.authority(ControllerPurpose::Cleanup)).unwrap(),E2bCleanupOutcome::Pending { identity,stage:E2bCleanupPendingStage::TargetUnverified } if *identity==operation.identity)
    );
    assert!(launcher
        .requests()
        .iter()
        .all(|request| request["operation"] != "kill"));
    assert!(db.query_one("SELECT verified_sandbox_id IS NULL AND settlement_id IS NULL FROM runtime.provider_creation_operations",&[]).unwrap().get::<_,bool>(0));
    db.batch_execute("UPDATE public.app_core_sessionenvironment SET controller_epoch=3")
        .unwrap();
    assert!(runtime
        .cleanup(&run.authority(ControllerPurpose::Cleanup))
        .is_err());
    assert_eq!(
        launcher.launches.load(Ordering::SeqCst),
        1,
        "stale cleanup cannot open an SDK lane"
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_e2b_runtime_cleanup_known_target_preserves_unknown_and_lost_settlement_until_not_found()
{
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (_, store, mut db) = retained::setup();
    let (run, operation) = candidate(&store, &mut db);
    store
        .verify_provider_cleanup_target(
            &run.authority(ControllerPurpose::Cleanup),
            &operation.identity,
            CONFIG,
            |identity, _| {
                Ok(ProviderSandboxObservation {
                    sandbox_id: "cleanup-sandbox".into(),
                    template_id: "centaeris-core".into(),
                    metadata: expected_metadata(identity),
                })
            },
        )
        .unwrap();
    let root = tempfile::tempdir().unwrap();
    let (unknown, unknown_launcher) = runtime(
        &store,
        &run,
        "deleteUnknown",
        root.path().join("unknown.jsonl"),
        info(&operation.identity),
    );
    assert!(matches!(
        unknown
            .cleanup(&run.authority(ControllerPurpose::Cleanup))
            .unwrap(),
        E2bCleanupOutcome::Pending {
            stage: E2bCleanupPendingStage::RemovalUnconfirmed,
            ..
        }
    ));
    assert!(unknown_launcher
        .requests()
        .iter()
        .all(|request| request["operation"] != "inspect"));
    for mode in ["ackFalse", "missingProviderResult"] {
        let (malformed, launcher) = runtime(
            &store,
            &run,
            mode,
            root.path().join(format!("{mode}.jsonl")),
            info(&operation.identity),
        );
        assert!(
            matches!(
                malformed
                    .cleanup(&run.authority(ControllerPurpose::Cleanup))
                    .unwrap(),
                E2bCleanupOutcome::Pending {
                    stage: E2bCleanupPendingStage::RemovalUnconfirmed,
                    ..
                }
            ),
            "{mode}"
        );
        assert!(
            db.query_one(
                "SELECT settlement_id IS NULL FROM runtime.provider_creation_operations",
                &[]
            )
            .unwrap()
            .get::<_, bool>(0),
            "{mode} must not authorize settlement"
        );
        assert_eq!(
            launcher
                .requests()
                .iter()
                .filter(|request| request["operation"] == "kill")
                .count(),
            1
        );
    }
    db.batch_execute("CREATE FUNCTION runtime.test_reject_provider_settlement() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.settlement_id IS NOT NULL THEN RAISE EXCEPTION 'fixture commit unavailable'; END IF; RETURN NEW; END $$; CREATE TRIGGER test_reject_provider_settlement BEFORE UPDATE ON runtime.provider_creation_operations FOR EACH ROW EXECUTE FUNCTION runtime.test_reject_provider_settlement()").unwrap();
    let (lost, lost_launcher) = runtime(
        &store,
        &run,
        "cleanupSuccess",
        root.path().join("lost.jsonl"),
        info(&operation.identity),
    );
    assert!(matches!(
        lost.cleanup(&run.authority(ControllerPurpose::Cleanup))
            .unwrap(),
        E2bCleanupOutcome::Pending {
            stage: E2bCleanupPendingStage::RemovalUnconfirmed,
            ..
        }
    ));
    assert!(db
        .query_one(
            "SELECT settlement_id IS NULL FROM runtime.provider_creation_operations",
            &[]
        )
        .unwrap()
        .get::<_, bool>(0));
    db.batch_execute("DROP TRIGGER test_reject_provider_settlement ON runtime.provider_creation_operations; DROP FUNCTION runtime.test_reject_provider_settlement()").unwrap();
    let (missing, missing_launcher) = runtime(
        &store,
        &run,
        "notFound",
        root.path().join("missing.jsonl"),
        Value::Null,
    );
    require_complete(
        missing
            .cleanup(&run.authority(ControllerPurpose::Cleanup))
            .unwrap(),
        1,
    );
    for launcher in [&unknown_launcher, &lost_launcher, &missing_launcher] {
        let requests = launcher.requests();
        assert_eq!(
            requests
                .iter()
                .filter(|request| request["operation"] == "kill")
                .count(),
            1
        );
        assert!(
            requests
                .iter()
                .all(|request| request["operation"] != "inspect"),
            "durable ownership must survive VM expiry without another GET"
        );
    }
    assert_eq!(
        db.query_one(
            "SELECT settlement_kind FROM runtime.provider_creation_operations",
            &[]
        )
        .unwrap()
        .get::<_, String>(0),
        "removed"
    );
}
