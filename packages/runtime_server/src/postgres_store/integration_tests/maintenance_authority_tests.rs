use super::{retained_instance_tests, PostgresRuntimeStore, TEST_LOCK};
use crate::environment_authorization::maintenance::{MaintenanceAuthorization, MaintenanceBinding};
use centaeris_core::execution::{
    EnvironmentTaskControllerPurpose, EnvironmentTaskOwner, EnvironmentTaskResult,
};
use hmac::{Hmac, Mac};
use postgres::{error::SqlState, Client, NoTls};
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicUsize, Ordering};

const KEY: &[u8] = b"maintenance-integration-test-key";
const LEASE: &str = "maintenance-integration-test-lease";

fn sign(payload: MaintenanceBinding) -> MaintenanceAuthorization {
    let canonical =
        crate::agent_run_authorization::canonical_json(serde_json::to_value(&payload).unwrap())
            .unwrap();
    let digest = format!("sha256:{:x}", Sha256::digest(canonical.as_bytes()));
    let mut mac = Hmac::<Sha256>::new_from_slice(KEY).unwrap();
    mac.update(b"workspace:environment-maintenance-authorization:v1\0");
    mac.update(digest.as_bytes());
    MaintenanceAuthorization {
        payload,
        signature: format!("hmac-sha256:{:x}", mac.finalize().into_bytes()),
    }
}

fn save_grant(db: &mut Client, auth: &MaintenanceAuthorization) {
    db.execute(
        "UPDATE public.app_core_environmentmaintenancegrant SET payload=$1::text::jsonb,signature=$2,digest=$3 WHERE id='maintenance-grant'",
        &[&serde_json::to_string(&auth.payload).unwrap(), &auth.signature, &auth.payload.digest().unwrap()],
    )
    .unwrap();
}

fn setup() -> (
    String,
    PostgresRuntimeStore,
    Client,
    MaintenanceAuthorization,
) {
    let (url, store, mut db) = retained_instance_tests::setup();
    // Match the API's varchar/JSONB types, including the nullable current grant.
    // The historical AgentRun below is allocation provenance, never maintenance authority.
    let original: crate::agent_run_authorization::WorkspaceAgentRunAuthorization =
        serde_json::from_str(include_str!(
            "../../../../../tests/workspace/fixtures/agent_run_authorization/v1/valid.json"
        ))
        .unwrap();
    let profile = original.environment_profile.digest().unwrap();
    db.batch_execute(
        &"DROP TABLE IF EXISTS public.app_core_environmentmaintenancegrant,public.app_core_environmentmaintenanceoperation CASCADE;
        ALTER TABLE public.app_core_sessionenvironment ADD COLUMN controller_maintenance_operation_id varchar(160);
        CREATE TABLE public.app_core_environmentmaintenanceoperation(
            operation_id varchar(160) PRIMARY KEY,workspace_id varchar(64),session_id varchar(64),
            environment_id varchar(64),instance_id varchar(73),container_id varchar(64),host_id varchar(160),
            allocation_execution_id text,profile_digest varchar(71),request_digest varchar(71),
            current_grant_id varchar(64),state varchar(8),operation_kind text);
        CREATE TABLE public.app_core_environmentmaintenancegrant(
            id varchar(64) PRIMARY KEY,operation_id varchar(160),payload jsonb,signature varchar(76),digest text);
        INSERT INTO public.app_core_environmentmaintenanceoperation VALUES(
            'maintenance-operation','ws_1','sess_1','environment','instance_'||repeat('a',64),repeat('c',64),
            'host','origin-execution','sha256:'||repeat('b',64),'sha256:'||repeat('d',64),'maintenance-grant','active','task');
        INSERT INTO public.app_core_environmentmaintenancegrant VALUES('maintenance-grant','maintenance-operation','{}','','');
        UPDATE public.app_core_sessionenvironment SET controller_maintenance_operation_id='maintenance-operation',
            controller_epoch=2,controller_lease_owner='maintenance-integration-test-lease',controller_purpose='execute',controller_released=false;
        INSERT INTO runtime.execution_instances(execution_id,instance_id,environment_id,session_id,agent_run_id,host_id,
            authorization_digest,binding_digest,image_digest,allocation_execution_id,allocation_mode,profile_digest)
        VALUES('origin-execution','instance_'||repeat('a',64),'environment','sess_1','historical-origin-run','host',
            'origin-authorization','origin-binding','origin-image','origin-execution','retained','PROFILE_DIGEST_PLACEHOLDER');
        INSERT INTO runtime.resident_sandboxes(host_id,execution_id,agent_run_id,workspace_id,container_id,state,
            memory_bytes,cpu_milli,pids,workspace_bytes,created_at_ms,updated_at_ms,instance_id,environment_id,allocation_mode)
        VALUES('host','origin-execution','historical-origin-run','ws_1',repeat('c',64),'resident',1,1,1,1,0,0,
            'instance_'||repeat('a',64),'environment','retained');".replace("PROFILE_DIGEST_PLACEHOLDER", &profile),
    )
    .unwrap();
    db.execute("UPDATE public.app_core_sessionenvironment SET environment_profile=$1::text::jsonb,environment_profile_digest=$2", &[&serde_json::to_string(&original.environment_profile).unwrap(), &profile]).unwrap();
    db.execute(
        "UPDATE public.app_core_environmentmaintenanceoperation SET profile_digest=$1",
        &[&profile],
    )
    .unwrap();
    let mut hash = Sha256::new();
    hash.update(b"workspace:environment-controller-lease:v1\0");
    hash.update(LEASE.as_bytes());
    let now: i64 = db
        .query_one(
            "SELECT (EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint",
            &[],
        )
        .unwrap()
        .get(0);
    let auth = sign(MaintenanceBinding {
        schema: "workspace.environment_maintenance_authorization.v1".into(),
        operation_kind: "task".into(),
        workspace_id: "ws_1".into(),
        session_id: "sess_1".into(),
        environment_id: "environment".into(),
        instance_id: format!("instance_{}", "a".repeat(64)),
        container_id: "c".repeat(64),
        profile_digest: profile,
        controller_owner: EnvironmentTaskOwner::MaintenanceOperation {
            operation_id: "maintenance-operation".into(),
        },
        controller_epoch: 2,
        controller_lease_digest: format!("sha256:{:x}", hash.finalize()),
        controller_purpose: EnvironmentTaskControllerPurpose::Execute,
        request_digest: format!("sha256:{}", "d".repeat(64)),
        expires_at_ms: now + 60_000,
    });
    save_grant(&mut db, &auth);
    (url, store, db, auth)
}

fn assert_denied(store: &PostgresRuntimeStore, auth: &MaintenanceAuthorization, expected: &str) {
    let calls = AtomicUsize::new(0);
    let result = store.with_current_maintenance(auth, KEY, |_| {
        calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    });
    assert_eq!(result, Err(expected.to_string()));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_maintenance_current_authority_pins_exact_target_and_rows_through_dispatch() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (url, store, mut db, mut auth) = setup();
    for purpose in [
        EnvironmentTaskControllerPurpose::Execute,
        EnvironmentTaskControllerPurpose::Cleanup,
    ] {
        auth.payload.controller_purpose = purpose.clone();
        auth = sign(auth.payload);
        save_grant(&mut db, &auth);
        db.execute(
            "UPDATE public.app_core_sessionenvironment SET controller_purpose=$1",
            &[&if purpose == EnvironmentTaskControllerPurpose::Execute {
                "execute"
            } else {
                "cleanup"
            }],
        )
        .unwrap();
        let calls = AtomicUsize::new(0);
        let value = store.with_current_maintenance(&auth, KEY, |target| {
            calls.fetch_add(1, Ordering::SeqCst);
            assert_eq!(target.fence.controller_owner, auth.payload.controller_owner);
            assert_eq!(target.fence.controller_purpose, purpose);
            assert_eq!(target.fence.controller_epoch, 2);
            assert_eq!(target.fence.environment_instance_id, auth.payload.instance_id);
            assert_eq!(target.container_id, auth.payload.container_id);
            assert_eq!(target.host_id, "host");
            assert_eq!(target.allocation_execution_id, "origin-execution");
            assert_eq!(target.allocation_agent_run_id.as_deref(), Some("historical-origin-run"));
            assert_eq!(target.binding_digest, "origin-binding");
            assert_eq!(target.image_digest, "origin-image");
            assert_eq!(target.profile_digest, auth.payload.profile_digest);
            let mut contender = Client::connect(&url, NoTls).unwrap();
            contender.batch_execute("SET lock_timeout='50ms'").unwrap();
            for query in [
                "UPDATE public.app_core_workspace SET status=status",
                "UPDATE public.app_core_session SET workspace_id=workspace_id",
                "UPDATE public.app_core_sessionenvironment SET controller_epoch=controller_epoch",
                "UPDATE public.app_core_environmentmaintenanceoperation SET state=state",
                "UPDATE public.app_core_environmentmaintenancegrant SET signature=signature",
                "SELECT execution_id FROM runtime.execution_instances FOR UPDATE",
                "SELECT execution_id FROM runtime.resident_sandboxes FOR UPDATE",
            ] {
                assert_eq!(contender.batch_execute(query).unwrap_err().code(), Some(&SqlState::LOCK_NOT_AVAILABLE), "{query}");
            }
            Ok("dispatched")
        }).unwrap();
        assert_eq!(value, "dispatched");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
    assert_eq!(
        db.query_one("SELECT count(*) FROM public.app_core_agentrun", &[])
            .unwrap()
            .get::<_, i64>(0),
        0
    );
}

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_maintenance_stale_or_released_controller_never_dispatches() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    for mutation in [
        "controller_epoch=3",
        "controller_lease_owner='replacement-lease'",
        "controller_released=true",
        "controller_run_id='unrelated-run'",
        "controller_maintenance_operation_id='other-operation'",
        "controller_purpose='cleanup'",
    ] {
        let (_url, store, mut db, auth) = setup();
        db.batch_execute(&format!(
            "UPDATE public.app_core_sessionenvironment SET {mutation}"
        ))
        .unwrap();
        assert_denied(&store, &auth, "maintenance_controller_rejected");
    }
}

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_maintenance_changed_operation_grant_or_allocation_never_dispatches() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    for (mutation, expected) in [
        ("UPDATE public.app_core_environmentmaintenanceoperation SET request_digest='changed'", "maintenance_operation_rejected"),
        ("UPDATE public.app_core_environmentmaintenanceoperation SET container_id=repeat('e',64)", "maintenance_operation_rejected"),
        ("UPDATE public.app_core_environmentmaintenanceoperation SET profile_digest='changed'", "maintenance_operation_rejected"),
        ("UPDATE public.app_core_environmentmaintenanceoperation SET state='released'", "maintenance_operation_rejected"),
        ("UPDATE public.app_core_environmentmaintenanceoperation SET current_grant_id=NULL", "maintenance_grant_missing"),
        ("UPDATE public.app_core_environmentmaintenancegrant SET operation_id='other-operation'", "maintenance_grant_rejected"),
        ("UPDATE public.app_core_environmentmaintenancegrant SET signature='changed'", "maintenance_grant_rejected"),
        ("UPDATE public.app_core_environmentmaintenancegrant SET payload=jsonb_set(payload,'{requestDigest}',to_jsonb('changed'::text))", "maintenance_authorization_binding_rejected"),
        ("UPDATE public.app_core_environmentmaintenanceoperation SET host_id='other-host'", "maintenance_allocation_rejected"),
        ("UPDATE public.app_core_environmentmaintenanceoperation SET allocation_execution_id='missing-origin'", "maintenance_allocation_missing"),
        ("UPDATE runtime.resident_sandboxes SET state='releasing'", "maintenance_allocation_rejected"),
        ("INSERT INTO runtime.environment_retirements VALUES('retirement','environment','instance_'||repeat('a',64),'host','origin-execution',repeat('c',64),'{}','pending',0)", "maintenance_environment_retiring"),
        ("UPDATE public.app_core_session SET current_environment_id='other-environment'", "maintenance_session_rejected"),
    ] {
        let (_url, store, mut db, auth) = setup();
        db.batch_execute(mutation).unwrap();
        assert_denied(&store, &auth, expected);
    }
}

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_maintenance_signature_and_post_lock_deadline_are_authoritative() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (url, store, mut db, mut auth) = setup();
    auth.signature = format!("hmac-sha256:{}", "0".repeat(64));
    save_grant(&mut db, &auth);
    assert_denied(&store, &auth, "maintenance_authorization_signature_invalid");
    let now: i64 = db
        .query_one(
            "SELECT (EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint",
            &[],
        )
        .unwrap()
        .get(0);
    auth.payload.expires_at_ms = now + 200;
    auth = sign(auth.payload);
    save_grant(&mut db, &auth);
    let mut blocker = Client::connect(&url, NoTls).unwrap();
    let mut tx = blocker.transaction().unwrap();
    tx.batch_execute("SELECT id FROM public.app_core_workspace FOR UPDATE")
        .unwrap();
    let (started, waiting) = std::sync::mpsc::channel();
    let dispatch = std::thread::spawn(move || {
        started.send(()).unwrap();
        assert_denied(&store, &auth, "maintenance_authorization_binding_rejected");
    });
    waiting.recv().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(350));
    tx.commit().unwrap();
    dispatch.join().unwrap();
}

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_maintenance_completion_requires_confirmed_operation_and_is_idempotent_immutable() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (url, store, mut db, auth) = setup();
    for result in [
        EnvironmentTaskResult::RunEnded {
            task_ids: vec![],
            cleanup_complete: true,
        },
        EnvironmentTaskResult::OperationEnded {
            task_ids: vec![],
            cleanup_complete: false,
        },
        EnvironmentTaskResult::OperationEnded {
            task_ids: vec!["duplicate".into(), "duplicate".into()],
            cleanup_complete: true,
        },
    ] {
        assert!(store
            .record_maintenance_completion(&auth, KEY, &result)
            .is_err());
        assert_eq!(
            db.query_one(
                "SELECT count(*) FROM runtime.environment_maintenance_completions",
                &[]
            )
            .unwrap()
            .get::<_, i64>(0),
            0
        );
    }
    let confirmed = EnvironmentTaskResult::OperationEnded {
        task_ids: vec!["operation-task".into()],
        cleanup_complete: true,
    };
    store
        .record_maintenance_completion(&auth, KEY, &confirmed)
        .unwrap();
    let fact = |db: &mut Client| -> String {
        db.query_one(
            "SELECT row_to_json(c)::text FROM runtime.environment_maintenance_completions c",
            &[],
        )
        .unwrap()
        .get(0)
    };
    let before = fact(&mut db);
    let reopened = PostgresRuntimeStore::new(&url).unwrap();
    reopened
        .record_maintenance_completion(&auth, KEY, &confirmed)
        .unwrap();
    assert_eq!(fact(&mut db), before);
    let different = EnvironmentTaskResult::OperationEnded {
        task_ids: vec!["different-task".into()],
        cleanup_complete: true,
    };
    assert_eq!(
        store.record_maintenance_completion(&auth, KEY, &different),
        Err("maintenance_completion_conflict".into())
    );
    for sql in [
        "UPDATE runtime.environment_maintenance_completions SET request_digest='changed'",
        "DELETE FROM runtime.environment_maintenance_completions",
    ] {
        assert_eq!(
            db.batch_execute(sql).unwrap_err().code(),
            Some(&SqlState::RAISE_EXCEPTION)
        );
    }
    assert_eq!(fact(&mut db), before);
}
