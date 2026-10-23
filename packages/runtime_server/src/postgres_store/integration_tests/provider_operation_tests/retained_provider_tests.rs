use super::*;
mod migration;
mod power;
mod prior_receipts;
mod retirement;
pub(super) mod uses;
use crate::agent_run_authorization::EnvironmentProfile;

fn retained_run(store: &PostgresRuntimeStore, db: &mut Client) -> Run {
    let mut profile: EnvironmentProfile = serde_json::from_str(include_str!(
        "../../../../../../tests/workspace/fixtures/e2b_environment_profile/v1/valid.json"
    ))
    .unwrap();
    let EnvironmentProfile::E2b(value) = &mut profile else {
        unreachable!()
    };
    value.lifetime = "retained".into();
    let mut run = run_with_profile(store, db, profile);
    run.grant.payload.execution_mode = "retained".into();
    run.grant = retained::sign(run.grant.payload.clone());
    run
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_retained_provider_creation_atomically_anchors_the_environment_without_docker_rows() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (url, store, mut db) = retained::setup();
    let run = retained_run(&store, &mut db);
    let operation = store
        .prepare_provider_creation(
            &run.authority(ControllerPurpose::Execute),
            &request("retained-execution-a", "retained-attempt", "retained-key"),
        )
        .unwrap();
    let anchor = db
        .query_one(
            "SELECT environment_id,creation_attempt_id FROM runtime.provider_environment_bindings",
            &[],
        )
        .unwrap();
    assert_eq!(anchor.get::<_, String>(0), "environment");
    assert_eq!(anchor.get::<_, String>(1), operation.identity.attempt_id);
    let restarted = PostgresRuntimeStore::new(&url).unwrap();
    assert_eq!(
        restarted
            .prepare_provider_creation(
                &run.authority(ControllerPurpose::Execute),
                &request("retained-execution-a", "retained-attempt", "retained-key"),
            )
            .unwrap(),
        operation
    );
    assert!(store
        .prepare_provider_creation(
            &run.authority(ControllerPurpose::Execute),
            &request("replacement", "replacement-attempt", "replacement-key"),
        )
        .is_err());
    assert_eq!(db.query_one("SELECT (SELECT count(*) FROM runtime.execution_instances)+(SELECT count(*) FROM runtime.resident_sandboxes)",&[]).unwrap().get::<_,i64>(0),0);
}

pub(super) fn terminal(db: &mut Client, run: &mut Run) {
    let controller = run.grant.payload.controller.as_mut().unwrap();
    controller.controller_epoch += 1;
    controller.controller_purpose = "cleanup".into();
    db.execute("UPDATE public.app_core_sessionenvironment SET controller_epoch=$1,controller_purpose='cleanup'",&[&controller.controller_epoch]).unwrap();
    db.execute("INSERT INTO public.app_core_sessionevent VALUES('ws_1','sess_1',$1,false,'{\"type\":\"agent_run_interrupted\"}')",&[&run.original.agent_run_id]).unwrap();
    db.execute(
        "UPDATE runtime.runtime_jobs SET lease_owner=NULL WHERE job_id=$1",
        &[&run.fence.job_id],
    )
    .unwrap();
    db.execute("UPDATE runtime.runtime_jobs SET lease_owner=$2,lease_purpose='control',lease_control_stage='cleanup' WHERE job_id=$1",&[&run.fence.job_id,&run.fence.lease_owner]).unwrap();
    run.grant = retained::sign(run.grant.payload.clone());
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_per_run_creation_does_not_acquire_an_environment_lifetime_binding() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db) = retained::setup();
    let run = run(&store, &mut db);
    store
        .prepare_provider_creation(
            &run.authority(ControllerPurpose::Execute),
            &request("per-run", "per-run-attempt", "per-run-key"),
        )
        .unwrap();
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.provider_environment_bindings",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        0
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_retained_provider_unknown_creation_cannot_be_refunded_or_reported_no_effects() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db) = retained::setup();
    let mut run = retained_run(&store, &mut db);
    let operation = store
        .prepare_provider_creation(
            &run.authority(ControllerPurpose::Execute),
            &request(
                "retained-unknown",
                "retained-unknown-attempt",
                "retained-unknown-key",
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
    assert!(store
        .confirm_provider_run_no_effects(&run.authority(ControllerPurpose::Cleanup))
        .is_err());
    assert!(store
        .settle_provider_creation_not_dispatched(
            &run.authority(ControllerPurpose::Cleanup),
            &operation.identity
        )
        .is_err());
    assert!(store
        .confirm_provider_run_cleanup(&run.authority(ControllerPurpose::Cleanup))
        .is_err());
    assert!(db.query_one("SELECT settlement_id IS NULL FROM runtime.provider_creation_operations WHERE attempt_id=$1",&[&operation.identity.attempt_id]).unwrap().get::<_,bool>(0));
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_retained_provider_definitely_unsent_creation_has_replayable_settlement_without_a_use() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db) = retained::setup();
    let mut run = retained_run(&store, &mut db);
    let operation = store
        .prepare_provider_creation(
            &run.authority(ControllerPurpose::Execute),
            &request(
                "retained-unsent",
                "retained-unsent-attempt",
                "retained-unsent-key",
            ),
        )
        .unwrap();
    terminal(&mut db, &mut run);
    let settlement = store
        .settle_provider_creation_not_dispatched(
            &run.authority(ControllerPurpose::Cleanup),
            &operation.identity,
        )
        .unwrap();
    assert_eq!(
        settlement.kind,
        crate::postgres_store::provider_operations::cleanup::ProviderSettlementKind::NotDispatched
    );
    assert_eq!(
        store
            .settle_provider_creation_not_dispatched(
                &run.authority(ControllerPurpose::Cleanup),
                &operation.identity
            )
            .unwrap(),
        settlement
    );
    let receipt = store
        .confirm_provider_run_cleanup(&run.authority(ControllerPurpose::Cleanup))
        .unwrap();
    assert_eq!(receipt.operation_count, 1);
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.provider_environment_run_uses",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        0
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_retained_provider_precreate_no_effects_does_not_require_an_environment_allocation() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db) = retained::setup();
    let mut a = retained_run(&store, &mut db);
    terminal(&mut db, &mut a);
    let receipt = store
        .confirm_provider_run_no_effects(&a.authority(ControllerPurpose::Cleanup))
        .unwrap();
    assert_eq!(receipt.scope, "noProviderEffectsForRun");
    assert_eq!(db.query_one("SELECT (SELECT count(*) FROM runtime.provider_environment_bindings)+(SELECT count(*) FROM runtime.provider_environment_run_uses)+(SELECT count(*) FROM runtime.provider_creation_operations)",&[]).unwrap().get::<_,i64>(0),0);
    assert_eq!(
        PostgresRuntimeStore::new(&url)
            .unwrap()
            .confirm_provider_run_no_effects(&a.authority(ControllerPurpose::Cleanup))
            .unwrap(),
        receipt
    );
}
