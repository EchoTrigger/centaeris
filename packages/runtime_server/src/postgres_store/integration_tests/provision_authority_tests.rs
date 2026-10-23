use super::{retained_instance_tests, PostgresRuntimeStore, TEST_LOCK};
use crate::environment_authorization::provision::{ProvisionAuthorization, ProvisionBinding};
use crate::resident_capacity::{declared_usage, ObservedResidentSandbox, ResidentCapacity};
use centaeris_core::execution::EnvironmentTaskOwner;
use hmac::{Hmac, Mac};
use postgres::{Client, NoTls};
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex};
const KEY: &[u8] = b"provision-test-key";
fn sign(payload: ProvisionBinding) -> ProvisionAuthorization {
    let mut auth = ProvisionAuthorization {
        payload,
        signature: String::new(),
    };
    let mut mac = Hmac::<Sha256>::new_from_slice(KEY).unwrap();
    mac.update(b"workspace:environment-provision-authorization:v1\0");
    mac.update(auth.digest().unwrap().as_bytes());
    auth.signature = format!("hmac-sha256:{:x}", mac.finalize().into_bytes());
    auth
}
fn setup() -> (String, PostgresRuntimeStore, Client, ProvisionAuthorization) {
    let (url, store, mut db) = retained_instance_tests::setup();
    let vector: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../tests/workspace/fixtures/environment_provision_authorization/v1/valid.json"
    ))
    .unwrap();
    let mut payload: ProvisionBinding = serde_json::from_value(vector["payload"].clone()).unwrap();
    payload.workspace_id = "ws_1".into();
    payload.session_id = "sess_1".into();
    payload.environment_id = "environment".into();
    payload.expires_at_ms = 4_000_000_000_000;
    payload.controller_lease_digest = format!(
        "sha256:{:x}",
        Sha256::digest(b"workspace:environment-controller-lease:v1\0provision-lease")
    );
    let auth = sign(payload);
    db.batch_execute("DROP TABLE IF EXISTS public.app_core_environmentmaintenancegrant,public.app_core_environmentmaintenanceoperation CASCADE;
        ALTER TABLE public.app_core_sessionenvironment ADD COLUMN IF NOT EXISTS controller_maintenance_operation_id text,ADD COLUMN IF NOT EXISTS environment_profile jsonb,ADD COLUMN IF NOT EXISTS environment_profile_digest text;
        CREATE TABLE public.app_core_environmentmaintenanceoperation(operation_id text PRIMARY KEY,workspace_id text,session_id text,environment_id text,operation_kind text,request jsonb,request_digest text,profile_digest text,current_grant_id text,state text,instance_id text,container_id text,host_id text,allocation_execution_id text);
        CREATE TABLE public.app_core_environmentmaintenancegrant(id text PRIMARY KEY,operation_id text,payload jsonb,signature text,digest text);").unwrap();
    let b = &auth.payload;
    db.execute("UPDATE public.app_core_sessionenvironment SET controller_maintenance_operation_id=$1,controller_epoch=$2,controller_lease_owner='provision-lease',controller_purpose='execute',controller_released=false,environment_profile=$3::text::jsonb,environment_profile_digest=$4",&[&b.operation_id,&b.controller_epoch,&serde_json::to_string(&b.environment_profile).unwrap(),&b.environment_profile_digest]).unwrap();
    db.execute("INSERT INTO public.app_core_environmentmaintenanceoperation VALUES($1,'ws_1','sess_1','environment','provision',$2::text::jsonb,$3,$4,'grant','active',NULL,NULL,NULL,NULL)",&[&b.operation_id,&serde_json::to_string(&b.request()).unwrap(),&b.request_digest,&b.environment_profile_digest]).unwrap();
    db.execute("INSERT INTO public.app_core_environmentmaintenancegrant VALUES('grant',$1,$2::text::jsonb,$3,$4)",&[&b.operation_id,&serde_json::to_string(b).unwrap(),&auth.signature,&auth.digest().unwrap()]).unwrap();
    (url, store, db, auth)
}
fn limits(auth: &ProvisionAuthorization) -> ResidentCapacity {
    let one = declared_usage(
        auth.payload
            .environment_profile
            .legacy()
            .unwrap()
            .resources
            .sandbox_resources(),
    );
    ResidentCapacity {
        global: one,
        tenant: one,
        host: one,
    }
}
#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_provision_pins_real_authority_and_commits_holding_before_host_without_a_run() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db, auth) = setup();
    let physical = Arc::new(Mutex::new(Vec::new()));
    let mut creates = 0;
    for _ in 0..2 {
        let result=store.provision_environment(&auth,KEY,"host",limits(&auth),||Ok(physical.lock().unwrap().clone()),|a|{
            let mut read=Client::connect(&url,NoTls).unwrap();
            assert!(read.query_one("SELECT agent_run_id IS NULL AND state IN('reserved','resident') FROM runtime.resident_sandboxes",&[]).unwrap().get::<_,bool>(0));
            assert_eq!(a.allocation_agent_run_id,None);assert_eq!(a.allocation_owner,EnvironmentTaskOwner::MaintenanceOperation{operation_id:auth.payload.operation_id.clone()});
            if a.container_id.is_none(){creates+=1;physical.lock().unwrap().push(ObservedResidentSandbox{execution_id:a.allocation_execution_id.clone(),allocation_owner:a.allocation_owner.clone(),container_id:"c".repeat(64),resources:declared_usage(auth.payload.environment_profile.legacy().unwrap().resources.sandbox_resources())});}
            Ok("c".repeat(64))
        }).unwrap();
        assert_eq!(result.container_id, Some("c".repeat(64)));
    }
    assert_eq!(creates, 1);
    assert_eq!(
        db.query_one("SELECT count(*) FROM public.app_core_agentrun", &[])
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.environment_provision_completions",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        1
    );
}
#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_provision_lost_ack_retains_reservation_and_recovers_exact_instance() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db, auth) = setup();
    let physical = Arc::new(Mutex::new(Vec::new()));
    let first = store.provision_environment(
        &auth,
        KEY,
        "host",
        limits(&auth),
        || Ok(vec![]),
        |a| {
            physical.lock().unwrap().push(ObservedResidentSandbox {
                execution_id: a.allocation_execution_id.clone(),
                allocation_owner: a.allocation_owner.clone(),
                container_id: "c".repeat(64),
                resources: declared_usage(
                    auth.payload
                        .environment_profile
                        .legacy()
                        .unwrap()
                        .resources
                        .sandbox_resources(),
                ),
            });
            Err("lost_ack".into())
        },
    );
    assert_eq!(first.unwrap_err(), "lost_ack");
    assert_eq!(
        db.query_one("SELECT count(*) FROM runtime.resident_sandboxes", &[])
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.environment_provision_completions",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        0
    );
    store
        .provision_environment(
            &auth,
            KEY,
            "host",
            limits(&auth),
            || Ok(physical.lock().unwrap().clone()),
            |a| {
                assert!(a.reused);
                assert_eq!(a.container_id, Some("c".repeat(64)));
                Ok("c".repeat(64))
            },
        )
        .unwrap();
}
#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_provision_stale_or_changed_authority_never_enters_host() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db, auth) = setup();
    for sql in ["UPDATE public.app_core_sessionenvironment SET controller_epoch=3","UPDATE public.app_core_sessionenvironment SET controller_epoch=2,controller_released=true","UPDATE public.app_core_sessionenvironment SET controller_released=false,controller_lease_owner='replacement'"]{
        db.batch_execute(sql).unwrap();assert!(store.provision_environment(&auth,KEY,"host",limits(&auth),||Ok(vec![]),|_|panic!("stale authority dispatched")).is_err());
    }
    assert_eq!(
        db.query_one("SELECT count(*) FROM runtime.resident_sandboxes", &[])
            .unwrap()
            .get::<_, i64>(0),
        0
    );
}
