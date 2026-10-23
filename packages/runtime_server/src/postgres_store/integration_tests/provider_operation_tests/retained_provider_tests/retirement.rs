use super::uses::{candidate, proof, release, successor};
use super::*;
use crate::postgres_store::provider_operations::cleanup::ProviderRemovalEvidence;
use crate::postgres_store::provider_operations::retained::retirement::ProviderEnvironmentRetirementRequest;

fn retirement_request(epoch: i64) -> ProviderEnvironmentRetirementRequest {
    ProviderEnvironmentRetirementRequest {
        schema: "runtime.provider_environment.retire.v1".into(),
        operation_id: "retained-retirement".into(),
        environment_id: "environment".into(),
        creation_attempt_id: "retained-attempt".into(),
        sandbox_id: "retained-sandbox".into(),
        provider_config_digest: CONFIG.into(),
        expected_controller_epoch: epoch,
        acknowledge_data_loss: true,
    }
}

fn close_run(
    store: &PostgresRuntimeStore,
    db: &mut Client,
    run: &mut Run,
    use_: &crate::postgres_store::provider_operations::retained::RetainedProviderRunUse,
) {
    terminal(db, run);
    let epoch = run
        .grant
        .payload
        .controller
        .as_ref()
        .unwrap()
        .controller_epoch;
    store
        .confirm_retained_provider_run_cleanup(
            &run.authority(ControllerPurpose::Cleanup),
            use_,
            CONFIG,
            |value| Ok(proof(value, epoch)),
        )
        .unwrap();
    release(db, run);
    store
        .complete_runtime_job(CompleteRuntimeJobRequest {
            job_id: run.fence.job_id.clone(),
            lease_owner: run.fence.lease_owner.clone(),
            output_refs: vec![],
            completed_at_ms: crate::now_ms().unwrap(),
        })
        .unwrap();
}

fn facts(db: &mut Client) -> (i64, bool) {
    let row = db.query_one("SELECT (SELECT count(*) FROM runtime.provider_environment_retirements),settlement_id IS NULL FROM runtime.provider_creation_operations", &[]).unwrap();
    (row.get(0), row.get(1))
}

#[test]
fn retained_provider_retirement_request_requires_exact_schema_fields_and_data_loss_acknowledgement()
{
    let value = retirement_request(0);
    assert!(value.validate().is_ok());
    let json = serde_json::to_value(&value).unwrap();
    assert_eq!(json["creationAttemptId"], "retained-attempt");
    assert_eq!(json["acknowledgeDataLoss"], true);
    for (field, bad) in [
        ("schema", serde_json::json!("runtime.environment.retire.v1")),
        ("expectedControllerEpoch", serde_json::json!(-1)),
        ("acknowledgeDataLoss", serde_json::json!(false)),
        ("providerConfigDigest", serde_json::json!("invalid")),
        ("sandboxId", serde_json::json!("bad/path")),
    ] {
        let mut invalid = json.clone();
        invalid[field] = bad;
        assert!(
            serde_json::from_value::<ProviderEnvironmentRetirementRequest>(invalid)
                .unwrap()
                .validate()
                .is_err()
        );
    }
    let mut unknown = json.clone();
    unknown["agentRunId"] = serde_json::json!("creator");
    assert!(serde_json::from_value::<ProviderEnvironmentRetirementRequest>(unknown).is_err());
    let mut missing = json;
    missing
        .as_object_mut()
        .unwrap()
        .remove("acknowledgeDataLoss");
    assert!(serde_json::from_value::<ProviderEnvironmentRetirementRequest>(missing).is_err());
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_retained_provider_retirement_commits_intent_before_exact_removal_and_replays_after_restart(
) {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db) = retained::setup();
    let (mut a, creation, use_a) = candidate(&store, &mut db);
    close_run(&store, &mut db, &mut a, &use_a);
    let request = retirement_request(2);
    let mut calls = 0;
    let result = store
        .retire_provider_environment(&request, CONFIG, |target| {
            calls += 1;
            assert_eq!(target, &use_a.target);
            assert_eq!(
                facts(&mut db),
                (1, true),
                "intent must be visible to an independent connection before deletion"
            );
            Ok(ProviderRemovalEvidence::Deleted {
                sandbox_id: target.sandbox_id.clone(),
            })
        })
        .unwrap();
    assert_eq!(result.status, "removed");
    assert_eq!(calls, 1);
    assert_eq!(facts(&mut db), (1, false));
    assert_eq!(
        result.schema,
        "runtime.provider_environment.retire.result.v1"
    );
    let json = serde_json::to_value(&result).unwrap();
    assert_eq!(json.as_object().unwrap().len(), 6);
    assert_eq!(json["creationAttemptId"], creation.identity.attempt_id);
    let reopened = PostgresRuntimeStore::new(&url).unwrap();
    assert_eq!(
        reopened
            .retire_provider_environment(&request, CONFIG, |_| panic!(
                "successful replay must not remove again"
            ))
            .unwrap(),
        result
    );
    let settlement=db.query_one("SELECT settlement_kind,settlement_sandbox_id,settlement_controller_epoch FROM runtime.provider_creation_operations",&[]).unwrap();
    assert_eq!(settlement.get::<_, String>(0), "removed");
    assert_eq!(settlement.get::<_, String>(1), "retained-sandbox");
    assert_eq!(settlement.get::<_, i64>(2), 2);
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_retained_provider_retirement_unknown_preserves_intent_and_creation_until_exact_retry() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db) = retained::setup();
    let (mut a, _, use_a) = candidate(&store, &mut db);
    close_run(&store, &mut db, &mut a, &use_a);
    let request = retirement_request(2);
    let result = store
        .retire_provider_environment(&request, CONFIG, |_| Err("private provider failure".into()))
        .unwrap();
    assert_eq!(result.status, "pending");
    assert_eq!(facts(&mut db), (1, true));
    assert!(!serde_json::to_string(&result).unwrap().contains("private"));
    let reopened = PostgresRuntimeStore::new(&url).unwrap();
    assert_eq!(
        reopened
            .retire_provider_environment(&request, CONFIG, |target| Ok(
                ProviderRemovalEvidence::NotFound {
                    sandbox_id: target.sandbox_id.clone()
                }
            ))
            .unwrap()
            .status,
        "removed"
    );
    assert_eq!(facts(&mut db), (1, false));
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_retained_provider_retirement_cannot_guess_an_unknown_creation_target() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db) = retained::setup();
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
    terminal(&mut db, &mut a);
    db.batch_execute("UPDATE public.app_core_sessionenvironment SET controller_released=true")
        .unwrap();
    assert!(store
        .retire_provider_environment(&retirement_request(2), CONFIG, |_| panic!(
            "Unknown creation has no verified exact target"
        ))
        .is_err());
    assert_eq!(facts(&mut db), (0, true));
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_retained_provider_retirement_rejects_unbound_persisted_target_digest() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db) = retained::setup();
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
    store
        .record_provider_creation_observation(&operation.identity, "retained-sandbox")
        .unwrap();
    // A shape-valid digest is not evidence that this target matched the frozen template.
    db.execute("UPDATE runtime.provider_creation_operations SET verified_sandbox_id='retained-sandbox',verification_digest=$1,verified_at_ms=(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint WHERE attempt_id=$2", &[&REQUEST, &operation.identity.attempt_id]).unwrap();
    terminal(&mut db, &mut a);
    db.batch_execute("UPDATE public.app_core_sessionenvironment SET controller_released=true")
        .unwrap();
    assert!(store
        .retire_provider_environment(&retirement_request(2), CONFIG, |_| panic!(
            "unbound digest cannot authorize removal"
        ))
        .is_err());
    assert_eq!(facts(&mut db), (0, true));
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_retained_provider_retirement_rejects_active_controller_and_unclosed_consumers() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db) = retained::setup();
    let (mut a, _, use_a) = candidate(&store, &mut db);
    assert!(store
        .retire_provider_environment(&retirement_request(1), CONFIG, |_| panic!(
            "active controller cannot retire"
        ))
        .is_err());
    terminal(&mut db, &mut a);
    db.batch_execute("UPDATE public.app_core_sessionenvironment SET controller_released=true")
        .unwrap();
    assert!(store
        .retire_provider_environment(&retirement_request(2), CONFIG, |_| panic!(
            "unclosed Run effects cannot retire"
        ))
        .is_err());
    db.batch_execute("UPDATE public.app_core_sessionenvironment SET controller_released=false")
        .unwrap();
    store
        .confirm_retained_provider_run_cleanup(
            &a.authority(ControllerPurpose::Cleanup),
            &use_a,
            CONFIG,
            |v| Ok(proof(v, 2)),
        )
        .unwrap();
    db.batch_execute("UPDATE public.app_core_sessionenvironment SET controller_released=true")
        .unwrap();
    assert!(store
        .retire_provider_environment(&retirement_request(2), CONFIG, |_| panic!(
            "missing business release cannot retire"
        ))
        .is_err());
    assert_eq!(facts(&mut db), (0, true));
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_retained_provider_retirement_rejects_wrong_epoch_target_profile_or_configuration_before_intent(
) {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db) = retained::setup();
    let (mut a, _, use_a) = candidate(&store, &mut db);
    close_run(&store, &mut db, &mut a, &use_a);
    for field in 0..5 {
        let mut request = retirement_request(2);
        match field {
            0 => request.expected_controller_epoch = 1,
            1 => request.creation_attempt_id = "foreign-attempt".into(),
            2 => request.sandbox_id = "foreign-sandbox".into(),
            3 => request.provider_config_digest = REQUEST.into(),
            _ => request.environment_id = "foreign-environment".into(),
        }
        assert!(store
            .retire_provider_environment(&request, CONFIG, |_| panic!("mismatch cannot dispatch"))
            .is_err());
    }
    assert!(store
        .retire_provider_environment(&retirement_request(2), REQUEST, |_| panic!(
            "configured mismatch cannot dispatch"
        ))
        .is_err());
    db.execute(
        "UPDATE public.app_core_sessionenvironment SET environment_profile_digest=$1",
        &[&REQUEST],
    )
    .unwrap();
    assert!(store
        .retire_provider_environment(&retirement_request(2), CONFIG, |_| panic!(
            "profile mismatch cannot dispatch"
        ))
        .is_err());
    assert_eq!(facts(&mut db), (0, true));
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_retained_provider_retirement_conflicting_intent_is_rejected_without_another_callback() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db) = retained::setup();
    let (mut a, _, use_a) = candidate(&store, &mut db);
    close_run(&store, &mut db, &mut a, &use_a);
    let request = retirement_request(2);
    assert_eq!(
        store
            .retire_provider_environment(&request, CONFIG, |_| Err("unknown".into()))
            .unwrap()
            .status,
        "pending"
    );
    for field in 0..4 {
        let mut wrong = request.clone();
        match field {
            0 => wrong.operation_id = "other-retirement".into(),
            1 => wrong.sandbox_id = "other-sandbox".into(),
            2 => wrong.creation_attempt_id = "other-attempt".into(),
            _ => wrong.expected_controller_epoch = 3,
        }
        assert!(store
            .retire_provider_environment(&wrong, CONFIG, |_| panic!(
                "conflicting immutable request cannot dispatch"
            ))
            .is_err());
    }
    assert_eq!(facts(&mut db), (1, true));
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_retained_provider_retirement_foreign_removal_evidence_cannot_settle_original_creation()
{
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db) = retained::setup();
    let (mut a, _, use_a) = candidate(&store, &mut db);
    close_run(&store, &mut db, &mut a, &use_a);
    let request = retirement_request(2);
    assert!(store
        .retire_provider_environment(&request, CONFIG, |_| Ok(ProviderRemovalEvidence::Deleted {
            sandbox_id: "foreign-sandbox".into()
        }))
        .is_err());
    assert_eq!(facts(&mut db), (1, true));
    assert_eq!(
        store
            .retire_provider_environment(&request, CONFIG, |v| Ok(
                ProviderRemovalEvidence::NotFound {
                    sandbox_id: v.sandbox_id.clone()
                }
            ))
            .unwrap()
            .status,
        "removed"
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_retained_provider_retirement_rechecks_current_environment_epoch_after_unknown() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db) = retained::setup();
    let (mut a, _, use_a) = candidate(&store, &mut db);
    close_run(&store, &mut db, &mut a, &use_a);
    let request = retirement_request(2);
    assert_eq!(
        store
            .retire_provider_environment(&request, CONFIG, |_| Err("unknown".into()))
            .unwrap()
            .status,
        "pending"
    );
    db.batch_execute("UPDATE public.app_core_sessionenvironment SET controller_epoch=3")
        .unwrap();
    assert!(store
        .retire_provider_environment(&request, CONFIG, |_| panic!(
            "old epoch cannot act after restart"
        ))
        .is_err());
    assert_eq!(facts(&mut db), (1, true));
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_retained_provider_retirement_closes_all_successor_uses_without_creator_job_authority() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db) = retained::setup();
    let (mut a, creation, use_a) = candidate(&store, &mut db);
    close_run(&store, &mut db, &mut a, &use_a);
    let mut b = successor(&store, &mut db, &a);
    let use_b = store
        .bind_retained_provider_run_use(
            &b.authority(ControllerPurpose::Execute),
            "retained-b-execution",
            &creation.identity.attempt_id,
            CONFIG,
        )
        .unwrap();
    close_run(&store, &mut db, &mut b, &use_b);
    assert_eq!(
        store
            .retire_provider_environment(&retirement_request(4), CONFIG, |target| {
                assert_eq!(target.identity.agent_run_id, a.original.agent_run_id);
                Ok(ProviderRemovalEvidence::Deleted {
                    sandbox_id: target.sandbox_id.clone(),
                })
            })
            .unwrap()
            .status,
        "removed"
    );
    assert_eq!(db.query_one("SELECT count(*) FROM runtime.provider_environment_run_uses WHERE cleanup_receipt_id IS NOT NULL",&[]).unwrap().get::<_,i64>(0),2);
}
