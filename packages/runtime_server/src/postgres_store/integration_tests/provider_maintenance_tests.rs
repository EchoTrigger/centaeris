//! Real PostgreSQL facts for independent provider maintenance dispatch and completion.
use super::retained_instance_tests::{self as retained, KEY};
use super::*;
use crate::agent_run_authorization::{EnvironmentProfile, WorkspaceAgentRunAuthorization};
use crate::environment_authorization::provider_maintenance::{
    ProviderMaintenanceAuthorization, ProviderMaintenanceBinding, ProviderMaintenanceTarget,
};
use crate::environment_authorization::provision::{ProvisionAuthorization, ProvisionBinding};
use crate::postgres_store::provider_operations::cleanup::{
    ProviderExecutionDispatchError, ProviderSandboxObservation, ProviderVerifiedTarget,
};
use crate::postgres_store::provider_operations::provision::ProviderProvisionPreparation;
use crate::postgres_store::provider_operations::ProviderCreationRequest;
use centaeris_core::execution::{
    EnvironmentTaskControllerPurpose, EnvironmentTaskOwner, EnvironmentTaskResult,
};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicUsize, Ordering};

const REQUEST: &str = "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";
const LEASE: &str = "provider-maintenance-lease";

fn signature(domain: &[u8], digest: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(KEY).unwrap();
    mac.update(domain);
    mac.update(digest.as_bytes());
    format!("hmac-sha256:{:x}", mac.finalize().into_bytes())
}

fn lease_digest(owner: &str) -> String {
    format!(
        "sha256:{:x}",
        Sha256::digest(
            [
                b"workspace:environment-controller-lease:v1\0".as_slice(),
                owner.as_bytes()
            ]
            .concat()
        )
    )
}

fn profile() -> EnvironmentProfile {
    let mut selected: EnvironmentProfile = serde_json::from_str(include_str!(
        "../../../../../tests/workspace/fixtures/e2b_environment_profile/v1/valid.json"
    ))
    .unwrap();
    let EnvironmentProfile::E2b(value) = &mut selected else {
        unreachable!()
    };
    value.lifetime = "retained".into();
    selected
}

fn install_api_facts(db: &mut Client) {
    db.batch_execute("DROP TABLE IF EXISTS public.app_core_environmentmaintenancegrant,public.app_core_environmentmaintenanceoperation CASCADE;
        ALTER TABLE public.app_core_sessionenvironment ADD COLUMN IF NOT EXISTS controller_maintenance_operation_id text;
        CREATE TABLE public.app_core_environmentmaintenanceoperation(operation_id text PRIMARY KEY,workspace_id text,session_id text,environment_id text,operation_kind text,request jsonb,request_digest text,profile_digest text,current_grant_id text,state text,instance_id text,container_id text,host_id text,allocation_execution_id text,provider_target jsonb);
        CREATE TABLE public.app_core_environmentmaintenancegrant(id text PRIMARY KEY,operation_id text,payload jsonb,signature text,digest text,previous_controller_epoch bigint,controller_epoch bigint,controller_lease_digest text,controller_purpose text,expires_at_ms bigint);").unwrap();
    if db
        .query_one(
            "SELECT to_regclass('runtime.provider_environment_maintenance_completions') IS NULL",
            &[],
        )
        .unwrap()
        .get::<_, bool>(0)
    {
        db.batch_execute(crate::postgres_store::provider_maintenance_schema::DDL)
            .unwrap();
    }
}

fn insert_grant(
    db: &mut Client,
    id: &str,
    operation: &str,
    payload: &serde_json::Value,
    signed: &str,
    digest: &str,
) {
    db.execute("INSERT INTO public.app_core_environmentmaintenancegrant VALUES($1,$2,$3::text::jsonb,$4,$5,$6,$7,$8,$9,$10)",
        &[&id,&operation,&payload.to_string(),&signed,&digest,&(payload["controllerEpoch"].as_i64().unwrap()-1),
          &payload["controllerEpoch"].as_i64().unwrap(),&payload["controllerLeaseDigest"].as_str().unwrap(),
          &payload["controllerPurpose"].as_str().unwrap(),&payload["expiresAtMs"].as_i64().unwrap()]).unwrap();
}

fn install_task(
    db: &mut Client,
    target: &ProviderVerifiedTarget,
    profile: &EnvironmentProfile,
) -> ProviderMaintenanceAuthorization {
    install_named_task(db, target, profile, "maint-task")
}

fn install_named_task(
    db: &mut Client,
    target: &ProviderVerifiedTarget,
    profile: &EnvironmentProfile,
    operation: &str,
) -> ProviderMaintenanceAuthorization {
    let mut payload: ProviderMaintenanceBinding = serde_json::from_value(serde_json::from_str::<serde_json::Value>(include_str!(
        "../../../../../tests/workspace/fixtures/provider_environment_maintenance_authorization/v1/valid.json")).unwrap()["payload"].clone()).unwrap();
    payload.workspace_id = "ws_1".into();
    payload.session_id = "sess_1".into();
    payload.environment_id = "environment".into();
    payload.controller_owner = EnvironmentTaskOwner::MaintenanceOperation {
        operation_id: operation.into(),
    };
    payload.controller_epoch=db.query_one("SELECT controller_epoch+1 FROM public.app_core_sessionenvironment WHERE id='environment'",&[]).unwrap().get(0);
    payload.controller_lease_digest = lease_digest(LEASE);
    payload.expires_at_ms = 4_000_000_000_000;
    payload.profile_digest = profile.digest().unwrap();
    payload.provider_target = ProviderMaintenanceTarget {
        provider: "e2b".into(),
        creation_attempt_id: target.identity.attempt_id.clone(),
        instance_id: target.identity.instance_id.clone(),
        sandbox_id: target.sandbox_id.clone(),
        verification_digest: target.verification_digest.clone(),
        provider_config_digest: target.identity.provider_config_digest.clone(),
    };
    let request =
        serde_json::json!({"program":"/bin/true","args":[],"cwd":"/app","timeoutMs":1000});
    payload.request_digest = format!(
        "sha256:{:x}",
        Sha256::digest(
            crate::agent_run_authorization::canonical_json(request.clone())
                .unwrap()
                .as_bytes()
        )
    );
    let auth = ProviderMaintenanceAuthorization {
        signature: signature(
            b"workspace:provider-environment-maintenance-authorization:v1\0",
            &payload.digest().unwrap(),
        ),
        payload,
    };
    let b = &auth.payload;
    let grant_id = if operation == "maint-task" {
        "task-grant".to_string()
    } else {
        format!("{operation}-grant")
    };
    db.execute("INSERT INTO public.app_core_environmentmaintenanceoperation VALUES($1,'ws_1','sess_1','environment','task',$2::text::jsonb,$3,$4,$5,'active',NULL,NULL,NULL,NULL,$6::text::jsonb)",
        &[&operation,&request.to_string(),&b.request_digest,&b.profile_digest,&grant_id,&serde_json::to_string(&b.provider_target).unwrap()]).unwrap();
    insert_grant(
        db,
        &grant_id,
        operation,
        &serde_json::to_value(b).unwrap(),
        &auth.signature,
        &b.digest().unwrap(),
    );
    db.execute("UPDATE public.app_core_sessionenvironment SET controller_run_id=NULL,controller_maintenance_operation_id=$5,controller_epoch=$1,controller_lease_owner=$2,controller_purpose='execute',controller_released=false,environment_profile=$3::text::jsonb,environment_profile_digest=$4",
        &[&b.controller_epoch,&LEASE,&serde_json::to_string(profile).unwrap(),&b.profile_digest,&operation]).unwrap();
    auth
}

fn provision_origin() -> (
    String,
    PostgresRuntimeStore,
    Client,
    EnvironmentProfile,
    ProviderVerifiedTarget,
) {
    provision_origin_with_profile(profile())
}

fn provision_origin_with_profile(
    selected: EnvironmentProfile,
) -> (
    String,
    PostgresRuntimeStore,
    Client,
    EnvironmentProfile,
    ProviderVerifiedTarget,
) {
    let (url, store, mut db) = retained::setup();
    install_api_facts(&mut db);
    let mut b: ProvisionBinding=serde_json::from_value(serde_json::from_str::<serde_json::Value>(include_str!(
        "../../../../../tests/workspace/fixtures/environment_provision_authorization/v1/valid.json")).unwrap()["payload"].clone()).unwrap();
    b.workspace_id = "ws_1".into();
    b.session_id = "sess_1".into();
    b.environment_id = "environment".into();
    b.user_id = "user_1".into();
    b.agent_id = "centaeris".into();
    b.environment_profile = selected.clone();
    b.environment_profile_digest = selected.digest().unwrap();
    b.expires_at_ms = 4_000_000_000_000;
    b.controller_lease_digest = lease_digest("origin-lease");
    b.request_digest = format!(
        "sha256:{:x}",
        Sha256::digest(
            crate::agent_run_authorization::canonical_json(
                serde_json::to_value(b.request()).unwrap()
            )
            .unwrap()
            .as_bytes()
        )
    );
    let mut origin = ProvisionAuthorization {
        payload: b,
        signature: String::new(),
    };
    origin.signature = signature(
        b"workspace:environment-provision-authorization:v1\0",
        &origin.digest().unwrap(),
    );
    let b = &origin.payload;
    db.execute("INSERT INTO public.app_core_environmentmaintenanceoperation VALUES($1,'ws_1','sess_1','environment','provision',$2::text::jsonb,$3,$4,'origin-grant','active',NULL,NULL,NULL,NULL,NULL)",
        &[&b.operation_id,&serde_json::to_string(&b.request()).unwrap(),&b.request_digest,&b.environment_profile_digest]).unwrap();
    insert_grant(
        &mut db,
        "origin-grant",
        &b.operation_id,
        &serde_json::to_value(b).unwrap(),
        &origin.signature,
        &origin.digest().unwrap(),
    );
    db.execute("UPDATE public.app_core_sessionenvironment SET controller_maintenance_operation_id=$1,controller_epoch=$2,controller_lease_owner='origin-lease',controller_purpose='execute',controller_released=false,environment_profile=$3::text::jsonb,environment_profile_digest=$4",
        &[&b.operation_id,&b.controller_epoch,&serde_json::to_string(&selected).unwrap(),&b.environment_profile_digest]).unwrap();
    let EnvironmentProfile::E2b(p) = &selected else {
        unreachable!()
    };
    let op = store
        .prepare_provider_provision_creation(
            &origin,
            KEY,
            &ProviderCreationRequest {
                execution_id: "maint-origin-execution",
                attempt_id: "maint-origin-attempt",
                idempotency_key: "maint-origin-key",
                request_digest: REQUEST,
                configured_provider_config_digest: &p.provider_config_digest,
            },
        )
        .unwrap();
    drop(
        store
            .dispatch_provider_provision_creation(&origin, KEY, &op.identity)
            .unwrap(),
    );
    store
        .record_provider_creation_observation(&op.identity, "sandbox-maintenance")
        .unwrap();
    let target = store
        .record_provider_provision_target(
            &origin,
            KEY,
            &op.identity,
            &ProviderSandboxObservation {
                sandbox_id: "sandbox-maintenance".into(),
                template_id: p.template_ref.template_id.clone(),
                metadata: crate::postgres_store::provider_operations::cleanup::expected_metadata(
                    &op.identity,
                ),
            },
        )
        .unwrap();
    store
        .complete_provider_provision(
            &origin,
            KEY,
            &target,
            &ProviderProvisionPreparation {
                instance_id: target.identity.instance_id.clone(),
                sandbox_id: target.sandbox_id.clone(),
                preparation_digest: REQUEST.into(),
            },
        )
        .unwrap();
    db.execute("UPDATE public.app_core_environmentmaintenanceoperation SET state='released' WHERE operation_id=$1",&[&origin.payload.operation_id]).unwrap();
    db.execute(
        "UPDATE public.app_core_sessionenvironment SET controller_released=true",
        &[],
    )
    .unwrap();
    (url, store, db, selected, target)
}

fn setup() -> (
    String,
    PostgresRuntimeStore,
    Client,
    ProviderMaintenanceAuthorization,
    ProviderVerifiedTarget,
) {
    let (url, store, mut db, selected, target) = provision_origin();
    let auth = install_task(&mut db, &target, &selected);
    (url, store, db, auth, target)
}

fn complete() -> EnvironmentTaskResult {
    EnvironmentTaskResult::OperationEnded {
        task_ids: vec!["task-one".into()],
        cleanup_complete: true,
    }
}

fn rewrite(db: &mut Client, auth: &mut ProviderMaintenanceAuthorization) {
    auth.signature = signature(
        b"workspace:provider-environment-maintenance-authorization:v1\0",
        &auth.payload.digest().unwrap(),
    );
    db.execute("UPDATE public.app_core_environmentmaintenancegrant SET payload=$1::text::jsonb,signature=$2,digest=$3,controller_epoch=$4,controller_lease_digest=$5,controller_purpose=$6,expires_at_ms=$7 WHERE id='task-grant'",
        &[&serde_json::to_string(&auth.payload).unwrap(),&auth.signature,&auth.payload.digest().unwrap(),&auth.payload.controller_epoch,
          &auth.payload.controller_lease_digest,&serde_json::to_value(&auth.payload.controller_purpose).unwrap().as_str().unwrap(),&auth.payload.expires_at_ms]).unwrap();
}

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_provider_maintenance_dispatch_resolves_real_provision_target_without_native_aliases() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db, auth, target) = setup();
    let calls = AtomicUsize::new(0);
    let result = store
        .with_current_provider_maintenance(
            &auth,
            KEY,
            &target.identity.provider_config_digest,
            |current| -> Result<_, ()> {
                calls.fetch_add(1, Ordering::SeqCst);
                assert_eq!(current.target, target);
                assert_eq!(
                    current.profile.digest().unwrap(),
                    auth.payload.profile_digest
                );
                Ok(current.target.sandbox_id.clone())
            },
        )
        .unwrap();
    assert_eq!(result, target.sandbox_id);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(db.query_one("SELECT (SELECT count(*) FROM runtime.execution_instances)+(SELECT count(*) FROM runtime.resident_sandboxes)+(SELECT count(*) FROM public.app_core_agentrun)",&[]).unwrap().get::<_,i64>(0),0);
}

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_provider_maintenance_rejects_current_target_or_origin_mismatch_before_callback() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db, mut auth, target) = setup();
    let calls = AtomicUsize::new(0);
    let reject = |current: &ProviderMaintenanceAuthorization| {
        let result = store.with_current_provider_maintenance(
            current,
            KEY,
            &target.identity.provider_config_digest,
            |_| -> Result<(), ()> {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
        );
        assert!(
            matches!(result, Err(ProviderExecutionDispatchError::Rejected(_))),
            "{result:?}"
        );
    };
    db.execute(
        "UPDATE public.app_core_sessionenvironment SET controller_released=true",
        &[],
    )
    .unwrap();
    reject(&auth);
    db.execute(
        "UPDATE public.app_core_sessionenvironment SET controller_released=false",
        &[],
    )
    .unwrap();
    let original = auth.clone();
    auth.payload.provider_target.sandbox_id = "foreign-sandbox".into();
    rewrite(&mut db, &mut auth);
    db.execute("UPDATE public.app_core_environmentmaintenanceoperation SET provider_target=$1::text::jsonb WHERE operation_id='maint-task'",&[&serde_json::to_string(&auth.payload.provider_target).unwrap()]).unwrap();
    reject(&auth);
    auth = original;
    rewrite(&mut db, &mut auth);
    db.execute("UPDATE public.app_core_environmentmaintenanceoperation SET provider_target=$1::text::jsonb WHERE operation_id='maint-task'",&[&serde_json::to_string(&auth.payload.provider_target).unwrap()]).unwrap();
    db.execute("UPDATE public.app_core_environmentmaintenanceoperation SET state='active' WHERE operation_id<>'maint-task'",&[]).unwrap();
    reject(&auth);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_provider_maintenance_keeps_callback_unknown_and_post_callback_failure_distinct() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db, mut auth, target) = setup();
    let callback = store.with_current_provider_maintenance(
        &auth,
        KEY,
        &target.identity.provider_config_digest,
        |_| -> Result<(), &str> { Err("transportUnknown") },
    );
    assert!(matches!(
        callback,
        Err(ProviderExecutionDispatchError::Callback("transportUnknown"))
    ));
    let now: i64 = db
        .query_one(
            "SELECT (EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint",
            &[],
        )
        .unwrap()
        .get(0);
    auth.payload.expires_at_ms = now + 1500;
    rewrite(&mut db, &mut auth);
    let result = store.with_current_provider_maintenance(
        &auth,
        KEY,
        &target.identity.provider_config_digest,
        |_| -> Result<(), ()> {
            std::thread::sleep(Duration::from_millis(1600));
            Ok(())
        },
    );
    assert!(
        matches!(
            result,
            Err(ProviderExecutionDispatchError::AfterDispatch(_))
        ),
        "{result:?}"
    );
}

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_provider_maintenance_completion_is_exact_replayable_insert_only_and_durable() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db, auth, target) = setup();
    let database: String = db
        .query_one("SELECT quote_ident(current_database())", &[])
        .unwrap()
        .get(0);
    db.batch_execute(&format!(
        "ALTER DATABASE {database} SET synchronous_commit=off"
    ))
    .unwrap();
    drop(store);
    let store = PostgresRuntimeStore::new(&url).unwrap();
    assert_eq!(
        store
            .with_client(|client| Ok(client
                .query_one("SHOW synchronous_commit", &[])
                .map_err(|e| e.to_string())?
                .get::<_, String>(0)))
            .unwrap(),
        "off"
    );
    db.batch_execute("CREATE TABLE public.provider_maintenance_commit_audit(policy text);
        CREATE FUNCTION public.observe_provider_maintenance_commit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN INSERT INTO public.provider_maintenance_commit_audit VALUES(current_setting('synchronous_commit')); RETURN NEW; END $$;
        CREATE TRIGGER observe_provider_maintenance_commit AFTER INSERT ON runtime.provider_environment_maintenance_completions FOR EACH ROW EXECUTE FUNCTION public.observe_provider_maintenance_commit();").unwrap();
    store
        .record_provider_maintenance_completion(
            &auth,
            KEY,
            &target.identity.provider_config_digest,
            &complete(),
        )
        .unwrap();
    store
        .record_provider_maintenance_completion(
            &auth,
            KEY,
            &target.identity.provider_config_digest,
            &complete(),
        )
        .unwrap();
    assert_eq!(
        db.query_one(
            "SELECT policy FROM public.provider_maintenance_commit_audit",
            &[]
        )
        .unwrap()
        .get::<_, String>(0),
        "on"
    );
    let row=db.query_one("SELECT creation_attempt_id,instance_id,sandbox_id,verification_digest,completion_json FROM runtime.provider_environment_maintenance_completions",&[]).unwrap();
    assert_eq!(row.get::<_, String>(0), target.identity.attempt_id);
    assert_eq!(row.get::<_, String>(1), target.identity.instance_id);
    assert_eq!(row.get::<_, String>(2), target.sandbox_id);
    assert_eq!(row.get::<_, String>(3), target.verification_digest);
    assert_eq!(
        serde_json::from_str::<EnvironmentTaskResult>(&row.get::<_, String>(4)).unwrap(),
        complete()
    );
    for sql in ["UPDATE runtime.provider_environment_maintenance_completions SET completion_json=completion_json","DELETE FROM runtime.provider_environment_maintenance_completions"] {assert!(db.execute(sql,&[]).is_err());}
    let wrong = EnvironmentTaskResult::OperationEnded {
        task_ids: vec!["different-task".into()],
        cleanup_complete: true,
    };
    assert!(store
        .record_provider_maintenance_completion(
            &auth,
            KEY,
            &target.identity.provider_config_digest,
            &wrong
        )
        .is_err());
    assert_eq!(
        store
            .with_client(|client| Ok(client
                .query_one("SHOW synchronous_commit", &[])
                .map_err(|e| e.to_string())?
                .get::<_, String>(0)))
            .unwrap(),
        "off"
    );
    db.batch_execute("DROP TRIGGER observe_provider_maintenance_commit ON runtime.provider_environment_maintenance_completions; DROP FUNCTION public.observe_provider_maintenance_commit(); DROP TABLE public.provider_maintenance_commit_audit;").unwrap();
    db.batch_execute(&format!(
        "ALTER DATABASE {database} RESET synchronous_commit"
    ))
    .unwrap();
}

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_provider_maintenance_only_trusted_finished_operation_creates_completion() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db, auth, target) = setup();
    for invalid in [
        EnvironmentTaskResult::RunEnded {
            task_ids: vec![],
            cleanup_complete: true,
        },
        EnvironmentTaskResult::OperationEnded {
            task_ids: vec![],
            cleanup_complete: false,
        },
        EnvironmentTaskResult::OperationEnded {
            task_ids: vec!["same".into(), "same".into()],
            cleanup_complete: true,
        },
    ] {
        assert!(store
            .record_provider_maintenance_completion(
                &auth,
                KEY,
                &target.identity.provider_config_digest,
                &invalid
            )
            .is_err());
    }
    let output = store
        .with_provider_maintenance_completion(
            &auth,
            KEY,
            &target.identity.provider_config_digest,
            |current| -> Result<_, ()> {
                assert_eq!(current.target, target);
                Ok((42, complete()))
            },
        )
        .unwrap();
    assert_eq!(output, 42);
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.provider_environment_maintenance_completions",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        1
    );
}

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_provider_maintenance_pins_authority_and_rejects_contended_provider_rows_before_callback(
) {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db, auth, target) = setup();
    store.with_current_provider_maintenance(&auth,KEY,&target.identity.provider_config_digest,|_|->Result<(),()>{
        let mut writer=Client::connect(&url,NoTls).unwrap();
        writer.batch_execute("SET lock_timeout='25ms'").unwrap();
        for sql in [
            "UPDATE public.app_core_workspace SET status=status",
            "UPDATE public.app_core_session SET workspace_id=workspace_id",
            "UPDATE public.app_core_sessionenvironment SET controller_epoch=controller_epoch",
            "UPDATE public.app_core_environmentmaintenanceoperation SET state=state WHERE operation_id='maint-task'",
            "UPDATE public.app_core_environmentmaintenancegrant SET signature=signature WHERE id='task-grant'",
            "UPDATE runtime.provider_creation_operations SET dispatch_state=dispatch_state",
            "UPDATE runtime.provider_environment_bindings SET creation_attempt_id=creation_attempt_id",
        ] {
            let error=writer.execute(sql,&[]).unwrap_err();
            assert_eq!(error.code(),Some(&postgres::error::SqlState::LOCK_NOT_AVAILABLE),"{sql}");
        }
        Ok(())
    }).unwrap();
    let calls = AtomicUsize::new(0);
    let mut lock = db.transaction().unwrap();
    lock.query_one(
        "SELECT attempt_id FROM runtime.provider_creation_operations FOR UPDATE",
        &[],
    )
    .unwrap();
    let result = store.with_current_provider_maintenance(
        &auth,
        KEY,
        &target.identity.provider_config_digest,
        |_| -> Result<(), ()> {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        },
    );
    assert!(
        matches!(result, Err(ProviderExecutionDispatchError::Rejected(_))),
        "{result:?}"
    );
    lock.rollback().unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_provider_maintenance_waiting_authority_uses_the_clock_after_locks() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db, mut auth, target) = setup();
    let now: i64 = db
        .query_one(
            "SELECT (EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint",
            &[],
        )
        .unwrap()
        .get(0);
    auth.payload.expires_at_ms = now + 700;
    rewrite(&mut db, &mut auth);
    let mut blocker = Client::connect(&url, NoTls).unwrap();
    let blocker_pid: i32 = blocker
        .query_one("SELECT pg_backend_pid()", &[])
        .unwrap()
        .get(0);
    let mut lock = blocker.transaction().unwrap();
    lock.query_one(
        "SELECT id FROM public.app_core_sessionenvironment WHERE id='environment' FOR UPDATE",
        &[],
    )
    .unwrap();
    let calls = AtomicUsize::new(0);
    let result = std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            store.with_current_provider_maintenance(
                &auth,
                KEY,
                &target.identity.provider_config_digest,
                |_| -> Result<(), ()> {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                },
            )
        });
        let started = std::time::Instant::now();
        loop {
            let waiting:bool=db.query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE $1=ANY(pg_blocking_pids(pid)))",&[&blocker_pid]).unwrap().get(0);
            if waiting {
                break;
            }
            assert!(
                started.elapsed() < Duration::from_secs(2),
                "dispatch never reached the locked Environment"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        // The transaction's lock_timeout is one second. Expire authority while
        // actually blocked, then release before that timeout can decide for us.
        std::thread::sleep(Duration::from_millis(750));
        lock.rollback().unwrap();
        worker.join().unwrap()
    });
    assert!(
        matches!(result, Err(ProviderExecutionDispatchError::Rejected(_))),
        "{result:?}"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_provider_maintenance_post_callback_commit_failure_is_after_dispatch_and_keeps_no_receipt(
) {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db, auth, target) = setup();
    db.batch_execute("CREATE FUNCTION public.reject_provider_maintenance_commit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'controlled deferred commit failure'; END $$;
        CREATE CONSTRAINT TRIGGER reject_provider_maintenance_commit AFTER INSERT ON runtime.provider_environment_maintenance_completions DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION public.reject_provider_maintenance_commit();").unwrap();
    let calls = AtomicUsize::new(0);
    let result = store.with_provider_maintenance_completion(
        &auth,
        KEY,
        &target.identity.provider_config_digest,
        |_| -> Result<_, ()> {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok((17, complete()))
        },
    );
    assert!(
        matches!(
            result,
            Err(ProviderExecutionDispatchError::AfterDispatch(_))
        ),
        "{result:?}"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.provider_environment_maintenance_completions",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        0
    );
    db.batch_execute("DROP TRIGGER reject_provider_maintenance_commit ON runtime.provider_environment_maintenance_completions; DROP FUNCTION public.reject_provider_maintenance_commit();").unwrap();
}

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_provider_maintenance_rejects_wrong_configuration_before_physical_callback() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, _, auth, _) = setup();
    let calls = AtomicUsize::new(0);
    let result =
        store.with_current_provider_maintenance(&auth, KEY, REQUEST, |_| -> Result<(), ()> {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });
    assert!(
        matches!(result, Err(ProviderExecutionDispatchError::Rejected(_))),
        "{result:?}"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

struct PriorRun {
    grant: crate::environment_authorization::EnvironmentAuthorization,
    original: WorkspaceAgentRunAuthorization,
    fence: RuntimeJobLeaseFence,
}

impl PriorRun {
    fn authority(
        &self,
        purpose: crate::postgres_store::controller_authority::ControllerPurpose,
    ) -> crate::postgres_store::controller_authority::ControllerAuthorityRequest<'_> {
        crate::postgres_store::controller_authority::ControllerAuthorityRequest {
            environment_authorization: &self.grant,
            signing_key: KEY,
            workspace_id: &self.original.workspace_id,
            lease: &self.fence,
            purpose,
        }
    }
}

fn start_prior_run(
    store: &PostgresRuntimeStore,
    db: &mut Client,
    selected: &EnvironmentProfile,
    epoch: i64,
) -> PriorRun {
    let mut original: WorkspaceAgentRunAuthorization = serde_json::from_str(include_str!(
        "../../../../../tests/workspace/fixtures/agent_run_authorization/v1/valid.json"
    ))
    .unwrap();
    original.agent_run_id = "provider-maintenance-prior-run".into();
    original.id = "provider-maintenance-prior-auth".into();
    original.environment_profile = selected.clone();
    original.environment_profile_digest = selected.digest().unwrap();
    let digest = original.digest().unwrap();
    let owner = "worker:provider-maintenance-prior";
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
    db.execute("UPDATE public.app_core_sessionenvironment SET controller_run_id=$1,controller_maintenance_operation_id=NULL,controller_epoch=$2,controller_lease_owner=$3,controller_purpose='execute',controller_released=false,environment_profile=$4::text::jsonb,environment_profile_digest=$5",
        &[&original.agent_run_id,&epoch,&owner,&serde_json::to_string(selected).unwrap(),&original.environment_profile_digest]).unwrap();
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
    let grant = retained::sign(crate::environment_authorization::EnvironmentBinding {
        schema: "workspace.environment_authorization.v1".into(),
        environment_id: "environment".into(),
        session_id: original.session_id.clone(),
        agent_run_id: original.agent_run_id.clone(),
        agent_run_authorization_digest: digest,
        execution_mode: "retained".into(),
        controller: Some(crate::environment_authorization::ControllerBinding {
            controller_id: original.agent_run_id.clone(),
            controller_epoch: epoch,
            controller_lease_digest: lease_digest(owner),
            controller_purpose: "execute".into(),
        }),
    });
    PriorRun {
        grant,
        original,
        fence: RuntimeJobLeaseFence {
            job_id,
            job_kind: "agent_run.lifecycle".into(),
            lease_owner: owner.into(),
        },
    }
}

fn close_prior_run(
    store: &PostgresRuntimeStore,
    db: &mut Client,
    run: &mut PriorRun,
    value: &crate::postgres_store::provider_operations::retained::RetainedProviderRunUse,
) {
    use crate::postgres_store::controller_authority::ControllerPurpose;
    let c = run.grant.payload.controller.as_mut().unwrap();
    c.controller_epoch += 1;
    c.controller_purpose = "cleanup".into();
    db.execute("UPDATE public.app_core_sessionenvironment SET controller_epoch=$1,controller_purpose='cleanup'",&[&c.controller_epoch]).unwrap();
    db.execute("INSERT INTO public.app_core_sessionevent VALUES('ws_1','sess_1',$1,false,'{\"type\":\"agent_run_interrupted\"}')",&[&run.original.agent_run_id]).unwrap();
    db.execute(
        "UPDATE runtime.runtime_jobs SET lease_owner=NULL WHERE job_id=$1",
        &[&run.fence.job_id],
    )
    .unwrap();
    db.execute("UPDATE runtime.runtime_jobs SET lease_owner=$2,lease_purpose='control',lease_control_stage='cleanup' WHERE job_id=$1",&[&run.fence.job_id,&run.fence.lease_owner]).unwrap();
    run.grant = retained::sign(run.grant.payload.clone());
    let epoch = run
        .grant
        .payload
        .controller
        .as_ref()
        .unwrap()
        .controller_epoch;
    let receipt=store.confirm_retained_provider_run_cleanup(&run.authority(ControllerPurpose::Cleanup),value,&value.target.identity.provider_config_digest,|v|Ok(
        crate::postgres_store::provider_operations::retained::RetainedProviderQuiescence {agent_run_id:v.agent_run_id.clone(),execution_id:v.execution_id.clone(),
            instance_id:v.target.identity.instance_id.clone(),sandbox_id:v.target.sandbox_id.clone(),controller_epoch:epoch,quiescent:true})).unwrap();
    db.execute(
        "INSERT INTO public.app_core_agentrunenvironmentrelease VALUES($1,'environment',$2,$3)",
        &[
            &run.original.agent_run_id,
            &receipt.authorization_digest,
            &receipt.controller_epoch,
        ],
    )
    .unwrap();
    db.execute(
        "UPDATE public.app_core_sessionenvironment SET controller_released=true",
        &[],
    )
    .unwrap();
}

fn release_table(db: &mut Client) {
    db.batch_execute("CREATE TABLE IF NOT EXISTS public.app_core_agentrunenvironmentrelease(agent_run_id text PRIMARY KEY,environment_id text NOT NULL,authorization_digest text NOT NULL,controller_epoch bigint NOT NULL);
        DELETE FROM public.app_core_agentrunenvironmentrelease;").unwrap();
}

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_provider_maintenance_accepts_real_run_origin_only_after_cleanup_and_business_release() {
    use crate::postgres_store::controller_authority::ControllerPurpose;
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db) = retained::setup();
    install_api_facts(&mut db);
    release_table(&mut db);
    let selected = profile();
    let mut prior = start_prior_run(&store, &mut db, &selected, 1);
    let EnvironmentProfile::E2b(p) = &selected else {
        unreachable!()
    };
    let op = store
        .prepare_provider_creation(
            &prior.authority(ControllerPurpose::Execute),
            &ProviderCreationRequest {
                execution_id: "maintenance-run-origin",
                attempt_id: "maintenance-run-attempt",
                idempotency_key: "maintenance-run-key",
                request_digest: REQUEST,
                configured_provider_config_digest: &p.provider_config_digest,
            },
        )
        .unwrap();
    drop(
        store
            .dispatch_provider_creation(&prior.authority(ControllerPurpose::Execute), &op.identity)
            .unwrap(),
    );
    store
        .record_provider_creation_observation(&op.identity, "sandbox-maintenance-run")
        .unwrap();
    let target = store
        .record_retained_provider_execution_target(
            &prior.authority(ControllerPurpose::Execute),
            &op.identity,
            &p.provider_config_digest,
            &ProviderSandboxObservation {
                sandbox_id: "sandbox-maintenance-run".into(),
                template_id: p.template_ref.template_id.clone(),
                metadata: crate::postgres_store::provider_operations::cleanup::expected_metadata(
                    &op.identity,
                ),
            },
        )
        .unwrap();
    let value = store
        .bind_retained_provider_run_use(
            &prior.authority(ControllerPurpose::Execute),
            "maintenance-run-origin",
            &op.identity.attempt_id,
            &p.provider_config_digest,
        )
        .unwrap();
    close_prior_run(&store, &mut db, &mut prior, &value);
    let auth = install_task(&mut db, &target, &selected);
    store
        .with_current_provider_maintenance(
            &auth,
            KEY,
            &p.provider_config_digest,
            |current| -> Result<(), ()> {
                assert_eq!(current.target, target);
                Ok(())
            },
        )
        .unwrap();
    db.execute(
        "DELETE FROM public.app_core_agentrunenvironmentrelease",
        &[],
    )
    .unwrap();
    let calls = AtomicUsize::new(0);
    let result = store.with_current_provider_maintenance(
        &auth,
        KEY,
        &p.provider_config_digest,
        |_| -> Result<(), ()> {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        },
    );
    assert!(
        matches!(result, Err(ProviderExecutionDispatchError::Rejected(_))),
        "{result:?}"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_provider_maintenance_provision_receipt_cannot_bypass_a_later_unfinished_run_consumer() {
    use crate::postgres_store::controller_authority::ControllerPurpose;
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db, selected, target) = provision_origin();
    release_table(&mut db);
    let mut prior = start_prior_run(&store, &mut db, &selected, 2);
    let value = store
        .bind_retained_provider_run_use(
            &prior.authority(ControllerPurpose::Execute),
            "maintenance-later-run",
            &target.identity.attempt_id,
            &target.identity.provider_config_digest,
        )
        .unwrap();
    // Adversarial persisted current-grant fixture: a valid maintenance signature
    // must not turn the old provision receipt into evidence that this Run ended.
    let pending_task = install_task(&mut db, &target, &selected);
    let calls = AtomicUsize::new(0);
    let result = store.with_current_provider_maintenance(
        &pending_task,
        KEY,
        &target.identity.provider_config_digest,
        |_| -> Result<(), ()> {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        },
    );
    assert!(
        matches!(result, Err(ProviderExecutionDispatchError::Rejected(_))),
        "{result:?}"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(db
        .query_one(
            "SELECT cleanup_receipt_id IS NULL FROM runtime.provider_environment_run_uses",
            &[]
        )
        .unwrap()
        .get::<_, bool>(0));
    db.execute("UPDATE public.app_core_sessionenvironment SET controller_run_id=$1,controller_maintenance_operation_id=NULL,controller_epoch=$2,controller_lease_owner=$3,controller_purpose='execute',controller_released=false",
        &[&prior.original.agent_run_id, &prior.grant.payload.controller.as_ref().unwrap().controller_epoch, &prior.fence.lease_owner]).unwrap();
    close_prior_run(&store, &mut db, &mut prior, &value);
    let resumed = install_named_task(&mut db, &target, &selected, "maint-next");
    store
        .with_current_provider_maintenance(
            &resumed,
            KEY,
            &target.identity.provider_config_digest,
            |_| -> Result<(), ()> { Ok(()) },
        )
        .unwrap();
    assert_eq!(db.query_one("SELECT count(*) FROM public.app_core_environmentmaintenanceoperation WHERE operation_kind='task'", &[]).unwrap().get::<_,i64>(0), 2,
        "the rejected current-grant fixture and its grant remain as history");
}

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_provider_maintenance_power_responsibility_blocks_until_confirmed_resume() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db, auth, target) = setup();
    db.execute("INSERT INTO runtime.provider_environment_power_operations(operation_id,environment_id,operation_sequence,predecessor_operation_id,request_digest,request_json,action,dispatch_state,recorded_at_ms) VALUES('pause-maint','environment',1,NULL,$1,'{}','pause','prepared',0)",&[&REQUEST]).unwrap();
    let calls = AtomicUsize::new(0);
    let reject = || {
        let result = store.with_current_provider_maintenance(
            &auth,
            KEY,
            &target.identity.provider_config_digest,
            |_| -> Result<(), ()> {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
        );
        assert!(
            matches!(result, Err(ProviderExecutionDispatchError::Rejected(_))),
            "{result:?}"
        );
    };
    reject();
    db.execute("UPDATE runtime.provider_environment_power_operations SET dispatch_state='dispatched',dispatched_at_ms=1 WHERE operation_id='pause-maint'",&[]).unwrap();
    db.execute("UPDATE runtime.provider_environment_power_operations SET acknowledged_at_ms=2,ack_json='{}' WHERE operation_id='pause-maint'",&[]).unwrap();
    reject();
    db.execute("INSERT INTO runtime.provider_environment_power_operations(operation_id,environment_id,operation_sequence,predecessor_operation_id,request_digest,request_json,action,dispatch_state,recorded_at_ms) VALUES('resume-maint','environment',2,'pause-maint',$1,'{}','resume','prepared',3)",&[&REQUEST]).unwrap();
    reject();
    db.execute("UPDATE runtime.provider_environment_power_operations SET dispatch_state='dispatched',dispatched_at_ms=4 WHERE operation_id='resume-maint'",&[]).unwrap();
    reject();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    db.execute("UPDATE runtime.provider_environment_power_operations SET acknowledged_at_ms=5,ack_json='{}' WHERE operation_id='resume-maint'",&[]).unwrap();
    store
        .with_current_provider_maintenance(
            &auth,
            KEY,
            &target.identity.provider_config_digest,
            |_| -> Result<(), ()> { Ok(()) },
        )
        .unwrap();
}

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_provider_maintenance_retirement_responsibility_blocks_dispatch_without_clearing_it() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db, auth, target) = setup();
    db.execute("INSERT INTO runtime.provider_environment_retirements VALUES('retire-maint','environment',$1,'{}',$2,0)",&[&REQUEST,&auth.payload.controller_epoch]).unwrap();
    let calls = AtomicUsize::new(0);
    let result = store.with_current_provider_maintenance(
        &auth,
        KEY,
        &target.identity.provider_config_digest,
        |_| -> Result<(), ()> {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        },
    );
    assert!(
        matches!(result, Err(ProviderExecutionDispatchError::Rejected(_))),
        "{result:?}"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.provider_environment_retirements",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        1
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
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_provider_maintenance_cleanup_uses_current_fence_after_business_session_deletion() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db, mut auth, target) = setup();
    db.execute(
        "UPDATE public.app_core_workspace SET status='archived'",
        &[],
    )
    .unwrap();
    db.execute("UPDATE public.app_core_session SET \"deletedAt\"=clock_timestamp(),current_environment_id=NULL",&[]).unwrap();
    let rejected = store.with_current_provider_maintenance(
        &auth,
        KEY,
        &target.identity.provider_config_digest,
        |_| -> Result<(), ()> { Ok(()) },
    );
    assert!(
        matches!(rejected, Err(ProviderExecutionDispatchError::Rejected(_))),
        "{rejected:?}"
    );
    auth.payload.controller_purpose = EnvironmentTaskControllerPurpose::Cleanup;
    rewrite(&mut db, &mut auth);
    db.execute(
        "UPDATE public.app_core_sessionenvironment SET controller_purpose='cleanup'",
        &[],
    )
    .unwrap();
    store
        .with_provider_maintenance_completion(
            &auth,
            KEY,
            &target.identity.provider_config_digest,
            |_| -> Result<_, ()> { Ok(((), complete())) },
        )
        .unwrap();
}

mod direct_tests;
