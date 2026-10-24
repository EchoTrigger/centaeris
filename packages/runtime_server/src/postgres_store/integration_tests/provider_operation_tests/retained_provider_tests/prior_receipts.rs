use super::uses::{candidate, release, successor};
use super::*;
use crate::postgres_store::provider_operations::cleanup::{
    expected_metadata, ProviderSandboxObservation,
};
use crate::postgres_store::provider_operations::retained::RetainedProviderRunUse;

fn digest(domain: &str, value: &serde_json::Value) -> String {
    let mut hash = Sha256::new();
    hash.update(domain.as_bytes());
    hash.update(serde_json::to_vec(value).unwrap());
    format!("sha256:{:x}", hash.finalize())
}

fn invalid_prior(kind: usize) -> (PostgresRuntimeStore, Client, Run) {
    let (_, store, mut db) = retained::setup();
    let (mut a, use_a) = if kind == 2 {
        db.batch_execute("CREATE TABLE IF NOT EXISTS public.app_core_agentrunenvironmentrelease(agent_run_id text PRIMARY KEY,environment_id text NOT NULL,authorization_digest text NOT NULL,controller_epoch bigint NOT NULL); DELETE FROM public.app_core_agentrunenvironmentrelease;").unwrap();
        let a = retained_run(&store, &mut db);
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
        let target = store
            .record_retained_provider_execution_target(
                &a.authority(ControllerPurpose::Execute),
                &operation.identity,
                CONFIG,
                &ProviderSandboxObservation {
                    sandbox_id: "retained-sandbox".into(),
                    template_id: "centaeris-core".into(),
                    metadata: expected_metadata(&operation.identity),
                },
            )
            .unwrap();
        let value = RetainedProviderRunUse {
            provider_use_id: "provider_use_unbound".into(),
            provider_use_digest: REQUEST.into(),
            agent_run_id: a.original.agent_run_id.clone(),
            authorization_digest: a.original.digest().unwrap(),
            execution_id: "retained-a".into(),
            target,
        };
        // The existing INSERT guard preserves these fields but does not prove
        // their digest binding. This uses ordinary SQL with every guard enabled.
        db.execute("INSERT INTO runtime.provider_environment_run_uses(provider_use_id,agent_run_id,environment_id,authorization_digest,execution_id,provider_use_digest,recorded_at_ms,creation_attempt_id) VALUES($1,$2,'environment',$3,$4,$5,(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint,$6)", &[&value.provider_use_id,&value.agent_run_id,&value.authorization_digest,&value.execution_id,&value.provider_use_digest,&value.target.identity.attempt_id]).unwrap();
        (a, value)
    } else {
        let (a, _, value) = candidate(&store, &mut db);
        (a, value)
    };
    terminal(&mut db, &mut a);
    let mut quiescence = digest(
        "runtime:retained-provider-run-quiescence:v1\0",
        &serde_json::json!({"agentRunId":use_a.agent_run_id,"executionId":use_a.execution_id,"instanceId":use_a.target.identity.instance_id,"sandboxId":use_a.target.sandbox_id,"controllerEpoch":2,"quiescent":true,"providerUseDigest":use_a.provider_use_digest}),
    );
    let mut receipt = format!(
        "provider_retained_cleanup_{}",
        &digest(
            "runtime:retained-provider-cleanup:v1\0",
            &serde_json::json!([use_a.provider_use_id, use_a.provider_use_digest]),
        )[7..]
    );
    match kind {
        0 => receipt = "provider_retained_cleanup_unbound".into(),
        1 => quiescence = REQUEST.into(),
        _ => (),
    }
    // A first null-to-present transition is legal under the immutable guard;
    // later attempts to alter an issued receipt remain prohibited.
    db.execute("UPDATE runtime.provider_environment_run_uses SET cleanup_receipt_id=$2,cleanup_controller_epoch=2,quiescence_digest=$3,cleanup_recorded_at_ms=(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint WHERE provider_use_id=$1", &[&use_a.provider_use_id,&receipt,&quiescence]).unwrap();
    release(&mut db, &a);
    (store, db, a)
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_retained_provider_invalid_prior_receipt_use_or_quiescence_blocks_successor_bind() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    for kind in 0..3 {
        let (store, mut db, a) = invalid_prior(kind);
        let b = successor(&store, &mut db, &a);
        assert!(
            store
                .bind_retained_provider_run_use(
                    &b.authority(ControllerPurpose::Execute),
                    "retained-b-execution",
                    "retained-attempt",
                    CONFIG,
                )
                .is_err(),
            "a nonempty prior tuple is not a bound cleanup proof: case {kind}"
        );
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
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_retained_provider_invalid_prior_receipt_use_or_quiescence_blocks_no_effects_cleanup() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    for kind in 0..3 {
        let (store, mut db, a) = invalid_prior(kind);
        let mut b = successor(&store, &mut db, &a);
        terminal(&mut db, &mut b);
        assert!(
            store
                .confirm_provider_run_no_effects(&b.authority(ControllerPurpose::Cleanup))
                .is_err(),
            "a nonempty prior tuple cannot authorize the successor's no-effects gate: case {kind}"
        );
        assert_eq!(
            db.query_one(
                "SELECT count(*) FROM runtime.agent_run_cleanup_receipts WHERE agent_run_id=$1",
                &[&b.original.agent_run_id]
            )
            .unwrap()
            .get::<_, i64>(0),
            0
        );
    }
}

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_retained_provider_maintenance_handoff_requires_all_consumers_cleanup_and_release_without_exclusion(
) {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db) = retained::setup();
    let (mut a, _, use_a) = candidate(&store, &mut db);
    use crate::postgres_store::provider_operations::retained::require_maintenance_handoff;
    assert!(require_maintenance_handoff(&mut db, &use_a.target).is_err());
    terminal(&mut db, &mut a);
    store
        .confirm_retained_provider_run_cleanup(
            &a.authority(ControllerPurpose::Cleanup),
            &use_a,
            CONFIG,
            |v| Ok(uses::proof(v, 2)),
        )
        .unwrap();
    assert!(require_maintenance_handoff(&mut db, &use_a.target).is_err());
    release(&mut db, &a);
    require_maintenance_handoff(&mut db, &use_a.target).unwrap();
    let mut blocker = Client::connect(&url, NoTls).unwrap();
    let mut held = blocker.transaction().unwrap();
    held.query_one(
        "SELECT provider_use_id FROM runtime.provider_environment_run_uses FOR UPDATE",
        &[],
    )
    .unwrap();
    db.batch_execute("SET lock_timeout='50ms'").unwrap();
    assert!(require_maintenance_handoff(&mut db, &use_a.target).is_err());
    held.rollback().unwrap();
    require_maintenance_handoff(&mut db, &use_a.target).unwrap();
    let mut b = successor(&store, &mut db, &a);
    let use_b = store
        .bind_retained_provider_run_use(
            &b.authority(ControllerPurpose::Execute),
            "retained-b",
            "retained-attempt",
            CONFIG,
        )
        .unwrap();
    assert!(require_maintenance_handoff(&mut db, &use_b.target).is_err());
    terminal(&mut db, &mut b);
    store
        .confirm_retained_provider_run_cleanup(
            &b.authority(ControllerPurpose::Cleanup),
            &use_b,
            CONFIG,
            |v| {
                Ok(uses::proof(
                    v,
                    b.grant
                        .payload
                        .controller
                        .as_ref()
                        .unwrap()
                        .controller_epoch,
                ))
            },
        )
        .unwrap();
    assert!(require_maintenance_handoff(&mut db, &use_b.target).is_err());
    release(&mut db, &b);
    require_maintenance_handoff(&mut db, &use_b.target).unwrap();
}
