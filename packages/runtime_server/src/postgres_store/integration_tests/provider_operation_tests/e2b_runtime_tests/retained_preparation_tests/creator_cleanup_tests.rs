use super::*;
use crate::postgres_store::integration_tests::provider_operation_tests::retained_provider_tests::{
    terminal, uses::release,
};
use crate::postgres_store::provider_operations::cleanup::ProviderRunCleanupReceipt;
use crate::postgres_store::provider_operations::retained::retirement::ProviderEnvironmentRetirementRequest;

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_e2b_retained_creator_cleanup_recovers_exact_late_target_then_retires_independently() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (url, store, mut db) = retained::setup();
    let mut run = retained_run(&store, &mut db);
    let operation = store
        .prepare_provider_creation(
            &run.authority(ControllerPurpose::Execute),
            &request(
                "failed-retained-creator",
                "late-retained-attempt",
                "late-retained-key",
            ),
        )
        .unwrap();
    store
        .dispatch_provider_creation(
            &run.authority(ControllerPurpose::Execute),
            &operation.identity,
        )
        .unwrap();
    terminal(&mut db, &mut run);
    let root = tempfile::tempdir().unwrap();
    let observation = json!({"sandboxId":"factory-sandbox", "templateId":"centaeris-core",
        "metadata":crate::postgres_store::provider_operations::cleanup::expected_metadata(&operation.identity),
        "state":"running"});
    let launcher = Arc::new(
        FixtureLauncher::new("retainedReady", root.path().join("requests.jsonl"))
            .with_observation(observation.clone()),
    );
    let runtime = E2bRuntime::with_launcher(
        PostgresRuntimeStore::new(&url).unwrap(),
        config(run.original.environment_profile.clone()),
        launcher.clone(),
    );

    assert!(runtime
        .cleanup_retained(&run.authority(ControllerPurpose::Cleanup), None)
        .is_err());
    assert_eq!(
        launcher.launches.load(Ordering::SeqCst),
        0,
        "a dispatched create without a locator keeps its responsibility without provider IO"
    );
    assert!(store
        .retained_provider_run_use(&run.original.agent_run_id)
        .unwrap()
        .is_none());
    let pending = store
        .provider_creation_operation(&operation.identity.attempt_id)
        .unwrap()
        .unwrap();
    assert!(pending.reported_sandbox_id.is_none());
    assert_eq!(
        pending.dispatch_state,
        ProviderCreationDispatchState::Dispatched
    );

    store
        .record_provider_creation_observation(&operation.identity, "factory-sandbox")
        .unwrap();
    let mut foreign = observation;
    foreign["metadata"]["agentRunId"] = json!("foreign-run");
    let rejected_launcher = Arc::new(
        FixtureLauncher::new("retainedReady", root.path().join("rejected.jsonl"))
            .with_observation(foreign),
    );
    let rejected_runtime = E2bRuntime::with_launcher(
        store.clone(),
        config(run.original.environment_profile.clone()),
        rejected_launcher.clone(),
    );
    assert!(rejected_runtime
        .cleanup_retained(&run.authority(ControllerPurpose::Cleanup), None)
        .is_err());
    assert!(
        store
            .retained_provider_run_use(&run.original.agent_run_id)
            .unwrap()
            .is_none(),
        "a candidate locator cannot become a consumer before exact provider identity verification"
    );
    assert!(
        rejected_launcher
            .requests()
            .iter()
            .all(|request| request["operation"] == "capabilities"
                || request["operation"] == "inspect")
    );

    let receipt = runtime
        .cleanup_retained(&run.authority(ControllerPurpose::Cleanup), None)
        .expect(
            "the exact late provider target must close the creator Run under cleanup authority",
        );
    let ProviderRunCleanupReceipt::RetainedProvider(receipt) = &receipt else {
        panic!("recovery must preserve retained environment responsibility");
    };
    assert_eq!(receipt.scope, "runEffectsOnly");
    let recovered = store
        .retained_provider_run_use(&run.original.agent_run_id)
        .unwrap()
        .unwrap();
    assert_eq!(recovered.execution_id, operation.identity.execution_id);
    assert_eq!(recovered.target.identity, operation.identity);
    assert_eq!(recovered.target.sandbox_id, "factory-sandbox");
    assert!(store
        .with_retained_provider_execution_dispatch(
            &run.authority(ControllerPurpose::Cleanup),
            &recovered,
            CONFIG,
            |_| -> Result<(), String> {
                panic!("cleanup never acquires ordinary execution authority")
            }
        )
        .is_err());
    assert!(db.query_one("SELECT settlement_id IS NULL FROM runtime.provider_creation_operations WHERE attempt_id=$1",
        &[&operation.identity.attempt_id]).unwrap().get::<_,bool>(0));
    let before_replay = launcher.requests().len();
    assert_eq!(
        runtime
            .cleanup_retained(&run.authority(ControllerPurpose::Cleanup), None)
            .unwrap()
            .receipt_id(),
        receipt.receipt_id
    );
    assert_eq!(
        launcher.requests().len(),
        before_replay,
        "receipt replay needs no provider action"
    );
    let operations = launcher.requests();
    assert!(
        operations
            .iter()
            .all(|request| match request["operation"].as_str() {
                Some("capabilities" | "inspect") => true,
                Some("invokeControlHelper") => matches!(
                    request["payload"]["helper"].as_str(),
                    Some("quiesce-agent-processes" | "agent-process-state")
                ),
                _ => false,
            }),
        "creator cleanup cannot create, attach, connect, run, prepare workspace, pause or kill"
    );
    assert_eq!(
        operations
            .iter()
            .filter(|request| request["operation"] == "invokeControlHelper")
            .count(),
        2
    );

    release(&mut db, &run);
    let retirement = ProviderEnvironmentRetirementRequest {
        schema: "runtime.provider_environment.retire.v1".into(),
        operation_id: "retire-failed-creator".into(),
        environment_id: operation.identity.environment_id.clone(),
        creation_attempt_id: operation.identity.attempt_id.clone(),
        sandbox_id: "factory-sandbox".into(),
        provider_config_digest: CONFIG.into(),
        expected_controller_epoch: 2,
        acknowledge_data_loss: true,
    };
    runtime.retire(&retirement).unwrap();
    assert!(db.query_one("SELECT settlement_id IS NOT NULL FROM runtime.provider_creation_operations WHERE attempt_id=$1",
        &[&operation.identity.attempt_id]).unwrap().get::<_,bool>(0));
    assert_eq!(
        launcher
            .requests()
            .iter()
            .filter(|request| request["operation"] == "kill")
            .count(),
        1
    );
    runtime.retire(&retirement).unwrap();
    assert_eq!(
        launcher
            .requests()
            .iter()
            .filter(|request| request["operation"] == "kill")
            .count(),
        1
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_e2b_retained_cleanup_without_original_context_preserves_open_consumer() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (_, store, mut db) = retained::setup();
    let mut run = retained_run(&store, &mut db);
    let root = tempfile::tempdir().unwrap();
    let launcher = Arc::new(FixtureLauncher::new(
        "retainedReady",
        root.path().join("requests.jsonl"),
    ));
    let runtime = E2bRuntime::with_launcher(
        store.clone(),
        config(run.original.environment_profile.clone()),
        launcher.clone(),
    );
    let host = match runtime
        .prepare(
            &run.authority(ControllerPurpose::Execute),
            &run.original,
            "original-open-consumer",
        )
        .unwrap()
    {
        E2bPrepareOutcome::Prepared(host) => host,
        E2bPrepareOutcome::Pending { .. } => {
            panic!("fixture must create the original live consumer")
        }
    };
    let original = host.retained_use().unwrap().clone();
    terminal(&mut db, &mut run);
    let before = launcher.requests().len();
    assert!(runtime
        .cleanup_retained(&run.authority(ControllerPurpose::Cleanup), None)
        .is_err());
    assert_eq!(
        launcher.requests().len(),
        before,
        "lost command evidence cannot be replaced by a fresh runner"
    );
    assert_eq!(
        store
            .retained_provider_run_use(&run.original.agent_run_id)
            .unwrap()
            .unwrap(),
        original
    );
    assert!(db.query_one("SELECT cleanup_receipt_id IS NULL FROM runtime.provider_environment_run_uses WHERE provider_use_id=$1",
        &[&original.provider_use_id]).unwrap().get::<_,bool>(0));
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_e2b_retained_recovered_use_keeps_unknown_cleanup_pending_without_redispatch() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (_, store, mut db) = retained::setup();
    let mut run = retained_run(&store, &mut db);
    let operation = store
        .prepare_provider_creation(
            &run.authority(ControllerPurpose::Execute),
            &request(
                "recovered-unknown-cleanup",
                "recovered-cleanup-attempt",
                "recovered-cleanup-key",
            ),
        )
        .unwrap();
    store
        .dispatch_provider_creation(
            &run.authority(ControllerPurpose::Execute),
            &operation.identity,
        )
        .unwrap();
    store
        .record_provider_creation_observation(&operation.identity, "factory-sandbox")
        .unwrap();
    terminal(&mut db, &mut run);
    let root = tempfile::tempdir().unwrap();
    // This fixture returns Unknown for the first fixed quiescence helper. The
    // locator inspection succeeds first, so the recovered use is already durable.
    let launcher = Arc::new(FixtureLauncher::new("cleanupHelperUnknown", root.path().join("requests.jsonl"))
        .with_observation(json!({"sandboxId":"factory-sandbox", "templateId":"centaeris-core",
            "metadata":crate::postgres_store::provider_operations::cleanup::expected_metadata(&operation.identity),
            "state":"running"})));
    let runtime = E2bRuntime::with_launcher(
        store.clone(),
        config(run.original.environment_profile.clone()),
        launcher.clone(),
    );
    assert!(runtime
        .cleanup_retained(&run.authority(ControllerPurpose::Cleanup), None)
        .is_err());
    let recovered = store
        .retained_provider_run_use(&run.original.agent_run_id)
        .unwrap()
        .unwrap();
    assert_eq!(recovered.target.identity, operation.identity);
    assert!(db.query_one("SELECT cleanup_receipt_id IS NULL FROM runtime.provider_environment_run_uses WHERE provider_use_id=$1",
        &[&recovered.provider_use_id]).unwrap().get::<_,bool>(0));
    assert!(db.query_one("SELECT settlement_id IS NULL FROM runtime.provider_creation_operations WHERE attempt_id=$1",
        &[&operation.identity.attempt_id]).unwrap().get::<_,bool>(0));
    assert_eq!(
        launcher
            .requests()
            .iter()
            .filter(|request| request["operation"] == "invokeControlHelper")
            .count(),
        1
    );
    let before = launcher.requests().len();
    assert!(
        runtime
            .cleanup_retained(&run.authority(ControllerPurpose::Cleanup), None)
            .is_err(),
        "an already recovered open use cannot recreate lost command or helper evidence"
    );
    assert_eq!(
        launcher.requests().len(),
        before,
        "Unknown cleanup does not redispatch on host-less replay"
    );
    assert_eq!(
        store
            .retained_provider_run_use(&run.original.agent_run_id)
            .unwrap()
            .unwrap(),
        recovered
    );
    assert!(db.query_one("SELECT cleanup_receipt_id IS NULL FROM runtime.provider_environment_run_uses WHERE provider_use_id=$1",
        &[&recovered.provider_use_id]).unwrap().get::<_,bool>(0));
    assert!(db.query_one("SELECT settlement_id IS NULL FROM runtime.provider_creation_operations WHERE attempt_id=$1",
        &[&operation.identity.attempt_id]).unwrap().get::<_,bool>(0));
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_e2b_retained_successor_before_use_and_stale_creator_cleanup_preserve_environment() {
    use crate::postgres_store::integration_tests::provider_operation_tests::retained_provider_tests::uses::successor;
    use crate::postgres_store::provider_operations::retained::RetainedProviderQuiescence;
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    for successor_has_use in [false, true] {
        let (_, store, mut db) = retained::setup();
        let mut creator = retained_run(&store, &mut db);
        let root = tempfile::tempdir().unwrap();
        let launcher = Arc::new(FixtureLauncher::new(
            "retainedReady",
            root.path().join("requests.jsonl"),
        ));
        let runtime = E2bRuntime::with_launcher(
            store.clone(),
            config(creator.original.environment_profile.clone()),
            launcher.clone(),
        );
        let host = match runtime
            .prepare(
                &creator.authority(ControllerPurpose::Execute),
                &creator.original,
                "retained-creator",
            )
            .unwrap()
        {
            E2bPrepareOutcome::Prepared(host) => host,
            E2bPrepareOutcome::Pending { .. } => panic!("fixture must create retained environment"),
        };
        let creator_use = host.retained_use().unwrap().clone();
        terminal(&mut db, &mut creator);
        store
            .confirm_retained_provider_run_cleanup(
                &creator.authority(ControllerPurpose::Cleanup),
                &creator_use,
                CONFIG,
                |value| {
                    Ok(RetainedProviderQuiescence {
                        agent_run_id: value.agent_run_id.clone(),
                        execution_id: value.execution_id.clone(),
                        instance_id: value.target.identity.instance_id.clone(),
                        sandbox_id: value.target.sandbox_id.clone(),
                        controller_epoch: 2,
                        quiescent: true,
                    })
                },
            )
            .unwrap();
        release(&mut db, &creator);
        let mut next = successor(&store, &mut db, &creator);
        let before = launcher.requests().len();
        if !successor_has_use {
            terminal(&mut db, &mut next);
            assert!(runtime.cleanup_retained(&next.authority(ControllerPurpose::Cleanup), None).is_err(),
        "a successor without a use stays pending rather than borrowing creator cleanup or deleting the VM");
            assert_eq!(launcher.requests().len(), before);
            assert!(store
                .retained_provider_run_use(&next.original.agent_run_id)
                .unwrap()
                .is_none());
            assert!(db.query_one("SELECT settlement_id IS NULL FROM runtime.provider_creation_operations WHERE attempt_id=$1",
        &[&creator_use.target.identity.attempt_id]).unwrap().get::<_,bool>(0));
        } else {
            let next_use = store
                .bind_retained_provider_run_use(
                    &next.authority(ControllerPurpose::Execute),
                    "retained-successor",
                    &creator_use.target.identity.attempt_id,
                    CONFIG,
                )
                .unwrap();
            store
                .record_provider_creation_observation(
                    &creator_use.target.identity,
                    &creator_use.target.sandbox_id,
                )
                .unwrap();
            assert!(
                runtime
                    .cleanup_retained(&creator.authority(ControllerPurpose::Cleanup), None)
                    .is_err(),
                "late creator evidence does not restore a superseded controller"
            );
            assert_eq!(launcher.requests().len(), before);
            assert_eq!(
                store
                    .retained_provider_run_use(&next.original.agent_run_id)
                    .unwrap()
                    .unwrap(),
                next_use
            );
            assert!(db.query_one("SELECT settlement_id IS NULL FROM runtime.provider_creation_operations WHERE attempt_id=$1",
        &[&creator_use.target.identity.attempt_id]).unwrap().get::<_,bool>(0));
        }
    }
}
