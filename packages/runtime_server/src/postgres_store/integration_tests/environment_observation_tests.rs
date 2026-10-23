//! Database observations preserve authority facts; the host supplies physical proof.
use super::{retained_instance_tests, PostgresRuntimeStore, TEST_LOCK};
use centaeris_core::execution::EnvironmentTaskOwner;
use postgres::Client;
use std::sync::atomic::{AtomicBool, Ordering};

fn setup() -> (
    PostgresRuntimeStore,
    Client,
    crate::agent_run_authorization::EnvironmentProfile,
) {
    let (_, store, mut db) = retained_instance_tests::setup();
    let auth: crate::agent_run_authorization::WorkspaceAgentRunAuthorization =
        serde_json::from_str(include_str!(
            "../../../../../tests/workspace/fixtures/agent_run_authorization/v1/valid.json"
        ))
        .unwrap();
    db.batch_execute("ALTER TABLE public.app_core_sessionenvironment ADD COLUMN IF NOT EXISTS controller_maintenance_operation_id text").unwrap();
    db.execute("UPDATE public.app_core_sessionenvironment SET environment_profile=$1::text::jsonb,environment_profile_digest=$2,controller_epoch=3,controller_run_id=NULL,controller_maintenance_operation_id='observe-operation',controller_released=false", &[&serde_json::to_string(&auth.environment_profile).unwrap(),&auth.environment_profile_digest]).unwrap();
    (store, db, auth.environment_profile)
}
fn seed_allocation(
    db: &mut Client,
    profile: &crate::agent_run_authorization::EnvironmentProfile,
    digest: &str,
) {
    db.execute("INSERT INTO runtime.execution_instances(execution_id,instance_id,environment_id,session_id,agent_run_id,host_id,authorization_digest,binding_digest,image_digest,allocation_execution_id,allocation_mode,profile_digest,allocation_owner_json) VALUES('provision:observe','instance-observe','environment','sess_1',NULL,'host','origin-grant','origin-binding',$1,'provision:observe','retained',$2,'{\"kind\":\"maintenanceOperation\",\"operationId\":\"observe-operation\"}')", &[&profile.legacy().unwrap().image_digest,&digest]).unwrap();
    db.batch_execute("INSERT INTO runtime.resident_sandboxes(host_id,execution_id,agent_run_id,workspace_id,container_id,state,memory_bytes,cpu_milli,pids,workspace_bytes,created_at_ms,updated_at_ms,instance_id,environment_id,allocation_mode,allocation_owner_json) VALUES('host','provision:observe',NULL,'ws_1',NULL,'reserved',1,1,1,1,0,0,'instance-observe','environment','retained','{\"kind\":\"maintenanceOperation\",\"operationId\":\"observe-operation\"}')").unwrap();
}

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_environment_observation_without_run_preserves_frozen_profile_and_typed_controller() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (store, mut db, profile) = setup();
    let value = store
        .with_environment_observation("sess_1", "environment", |target| {
            assert_eq!(target.session_id, "sess_1");
            assert_eq!(target.environment_id, "environment");
            assert_eq!(target.profile, profile);
            assert_eq!(target.profile_digest, profile.digest().unwrap());
            assert_eq!(target.controller_epoch, 3);
            assert_eq!(
                target.controller_owner,
                Some(EnvironmentTaskOwner::MaintenanceOperation {
                    operation_id: "observe-operation".into()
                })
            );
            assert!(target.allocation.is_none());
            assert!(target.holding_state.is_none());
            assert!(!target.resource_holding);
            assert!(!target.destroyed);
            Ok("unallocated")
        })
        .unwrap();
    assert_eq!(value, "unallocated");
    assert_eq!(
        db.query_one("SELECT count(*) FROM public.app_core_agentrun", &[])
            .unwrap()
            .get::<_, i64>(0),
        0
    );
}

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_environment_observation_keeps_receipts_separate_from_physical_and_resource_proof() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (store, mut db, profile) = setup();
    let digest = profile.digest().unwrap();
    seed_allocation(&mut db, &profile, &digest);
    db.execute("INSERT INTO runtime.environment_provision_completions(operation_id,environment_id,session_id,workspace_id,host_id,instance_id,container_id,profile_digest,controller_epoch,controller_lease_digest,request_digest,completion_json,recorded_at_ms) VALUES('observe-operation','environment','sess_1','ws_1','host','instance-observe','receipt-container',$1,3,'lease','request','{\"kind\":\"provisioned\",\"instanceId\":\"instance-observe\",\"containerId\":\"receipt-container\"}',1)", &[&digest]).unwrap();
    store
        .with_environment_observation("sess_1", "environment", |target| {
            assert_eq!(target.holding_state.as_deref(), Some("reserved"));
            assert!(target.resource_holding);
            let allocation = target.allocation.as_ref().unwrap();
            assert_eq!(allocation.allocation_agent_run_id, None);
            assert_eq!(
                allocation.allocation_owner,
                EnvironmentTaskOwner::MaintenanceOperation {
                    operation_id: "observe-operation".into()
                }
            );
            assert_eq!(
                allocation.container_id, None,
                "a receipt cannot fill an unconfirmed physical target"
            );
            Ok(())
        })
        .unwrap();
    db.batch_execute(
        "UPDATE runtime.resident_sandboxes SET state='resident',container_id='receipt-container'",
    )
    .unwrap();
    let physical_result = store
        .with_environment_observation("sess_1", "environment", |target| {
            assert_eq!(target.holding_state.as_deref(), Some("resident"));
            assert!(target.resource_holding);
            assert!(!target.destroyed);
            Ok("unknown")
        })
        .unwrap();
    assert_eq!(
        physical_result, "unknown",
        "recorded residency cannot replace the host observation"
    );
    db.batch_execute("DELETE FROM runtime.resident_sandboxes")
        .unwrap();
    store
        .with_environment_observation("sess_1", "environment", |target| {
            assert!(!target.resource_holding);
            assert!(
                !target.destroyed,
                "absence of the debit or an old provision receipt is not retirement proof"
            );
            Ok(())
        })
        .unwrap();
}

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_environment_observation_rejects_foreign_session_and_changed_profile_before_host() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (store, mut db, profile) = setup();
    let called = AtomicBool::new(false);
    db.batch_execute("INSERT INTO public.app_core_session VALUES('foreign','ws_1','user_1','centaeris','environment',NULL)").unwrap();
    assert!(store
        .with_environment_observation("foreign", "environment", |_| {
            called.store(true, Ordering::SeqCst);
            Ok(())
        })
        .is_err());
    assert!(!called.load(Ordering::SeqCst));
    db.execute(
        "UPDATE public.app_core_sessionenvironment SET environment_profile_digest=$1",
        &[&format!("sha256:{}", "b".repeat(64))],
    )
    .unwrap();
    assert!(store
        .with_environment_observation("sess_1", "environment", |_| {
            called.store(true, Ordering::SeqCst);
            Ok(())
        })
        .is_err());
    assert!(!called.load(Ordering::SeqCst));
    db.execute(
        "UPDATE public.app_core_sessionenvironment SET environment_profile_digest=$1",
        &[&profile.digest().unwrap()],
    )
    .unwrap();
    seed_allocation(&mut db, &profile, &format!("sha256:{}", "b".repeat(64)));
    assert!(store
        .with_environment_observation("sess_1", "environment", |_| {
            called.store(true, Ordering::SeqCst);
            Ok(())
        })
        .is_err());
    assert!(!called.load(Ordering::SeqCst));
}
