//! Real PG plus controlled SDK processes for independent provisioning.
use super::*;
use crate::e2b_runtime::E2bProvisionOutcome;
use crate::environment_authorization::provision::ProvisionAuthorization;
use hmac::{Hmac, Mac};

fn signed(mut auth: ProvisionAuthorization, profile: EnvironmentProfile) -> ProvisionAuthorization {
    auth.payload.environment_profile = profile;
    auth.payload.environment_profile_digest = auth.payload.environment_profile.digest().unwrap();
    let canonical = crate::agent_run_authorization::canonical_json(
        serde_json::to_value(auth.payload.request()).unwrap(),
    )
    .unwrap();
    auth.payload.request_digest = format!("sha256:{:x}", Sha256::digest(canonical.as_bytes()));
    let mut mac = Hmac::<Sha256>::new_from_slice(KEY).unwrap();
    mac.update(b"workspace:environment-provision-authorization:v1\0");
    mac.update(auth.digest().unwrap().as_bytes());
    auth.signature = format!("hmac-sha256:{:x}", mac.finalize().into_bytes());
    auth
}

fn setup() -> (PostgresRuntimeStore, Client, ProvisionAuthorization) {
    let (_, store, mut db, auth) = super::super::provision_tests::setup();
    let mut profile = factory_profile();
    let EnvironmentProfile::E2b(ref mut p) = profile else {
        unreachable!()
    };
    p.lifetime = "retained".into();
    let auth = signed(auth, profile);
    let b = &auth.payload;
    db.execute("UPDATE public.app_core_sessionenvironment SET environment_profile=$1::text::jsonb,environment_profile_digest=$2 WHERE id=$3",&[&serde_json::to_string(&b.environment_profile).unwrap(),&b.environment_profile_digest,&b.environment_id]).unwrap();
    db.execute("UPDATE public.app_core_environmentmaintenanceoperation SET profile_digest=$1,request_digest=$2,request=$3::text::jsonb WHERE operation_id=$4",&[&b.environment_profile_digest,&b.request_digest,&serde_json::to_string(&b.request()).unwrap(),&b.operation_id]).unwrap();
    db.execute("UPDATE public.app_core_environmentmaintenancegrant SET payload=$1::text::jsonb,signature=$2,digest=$3 WHERE id='grant'",&[&serde_json::to_string(b).unwrap(),&auth.signature,&auth.digest().unwrap()]).unwrap();
    (store, db, auth)
}

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_e2b_provision_fresh_create_prepares_once_and_replays_completion_without_sdk() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (store, mut db, auth) = setup();
    let root = tempfile::tempdir().unwrap();
    let launcher = Arc::new(FixtureLauncher::new(
        "productReady",
        root.path().join("requests.jsonl"),
    ));
    let runtime = E2bRuntime::with_launcher(
        store.clone(),
        config(auth.payload.environment_profile.clone()),
        launcher.clone(),
    );
    let E2bProvisionOutcome::Completed(first) = runtime.provision(&auth, KEY).unwrap() else {
        panic!("fresh confirmed provision must complete")
    };
    assert_eq!(first.operation_id, auth.payload.operation_id);
    assert_eq!(first.sandbox_id, "factory-sandbox");
    assert_eq!(launcher.creates(), 1);
    let preparations = || {
        launcher
            .requests()
            .iter()
            .filter(|r| {
                r["operation"] == "invokeControlHelper"
                    && r["payload"]["helper"] == "prepare-workspace"
            })
            .count()
    };
    assert_eq!(preparations(), 1);
    assert_eq!(db.query_one("SELECT (SELECT count(*) FROM public.app_core_agentrun)+(SELECT count(*) FROM runtime.runtime_jobs)",&[]).unwrap().get::<_,i64>(0),0);
    let before = launcher.requests().len();
    let restarted = E2bRuntime::with_launcher(
        store.clone(),
        config(auth.payload.environment_profile.clone()),
        launcher.clone(),
    );
    let E2bProvisionOutcome::Completed(replay) = restarted.provision(&auth, KEY).unwrap() else {
        panic!("durable completion must replay")
    };
    assert_eq!(first, replay);
    assert_eq!(launcher.requests().len(), before);
    assert_eq!(launcher.launches.load(Ordering::SeqCst), 2);
    assert_eq!(preparations(), 1);
}

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_e2b_provision_unknown_create_retains_responsibility_across_restart_without_sdk() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (store, mut db, auth) = setup();
    let root = tempfile::tempdir().unwrap();
    let launcher = Arc::new(FixtureLauncher::new(
        "unknown",
        root.path().join("requests.jsonl"),
    ));
    let runtime = E2bRuntime::with_launcher(
        store.clone(),
        config(auth.payload.environment_profile.clone()),
        launcher.clone(),
    );
    let E2bProvisionOutcome::Pending(first) = runtime.provision(&auth, KEY).unwrap() else {
        panic!("lost reply is pending")
    };
    assert_eq!(launcher.creates(), 1);
    let before = launcher.requests().len();
    let restarted = E2bRuntime::with_launcher(
        store.clone(),
        config(auth.payload.environment_profile.clone()),
        launcher.clone(),
    );
    let E2bProvisionOutcome::Pending(replay) = restarted.provision(&auth, KEY).unwrap() else {
        panic!("restart cannot resend")
    };
    assert_eq!(first.identity, replay.identity);
    assert_eq!(launcher.requests().len(), before);
    assert_eq!(launcher.launches.load(Ordering::SeqCst), 2);
    assert!(store
        .provider_provision_completion(&auth.payload.operation_id)
        .unwrap()
        .is_none());
    assert_eq!(db.query_one("SELECT count(*) FROM runtime.provider_creation_operations WHERE dispatch_state='dispatched' AND settlement_id IS NULL",&[]).unwrap().get::<_,i64>(0),1);
}

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_e2b_provision_unconfirmed_workspace_preparation_is_not_a_completion_or_retry_permit() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (store, mut db, auth) = setup();
    let root = tempfile::tempdir().unwrap();
    let launcher = Arc::new(FixtureLauncher::new(
        "helperUnknown",
        root.path().join("requests.jsonl"),
    ));
    let runtime = E2bRuntime::with_launcher(
        store.clone(),
        config(auth.payload.environment_profile.clone()),
        launcher.clone(),
    );
    assert!(matches!(
        runtime.provision(&auth, KEY),
        Err(E2bRuntimeError::Unknown(
            "e2b_provision_preparation_unconfirmed"
        ))
    ));
    assert_eq!(launcher.creates(), 1);
    assert!(store
        .provider_provision_completion(&auth.payload.operation_id)
        .unwrap()
        .is_none());
    let before = launcher.requests().len();
    assert!(matches!(
        runtime.provision(&auth, KEY),
        Ok(E2bProvisionOutcome::Pending(_))
    ));
    assert_eq!(launcher.requests().len(), before);
    assert_eq!(db.query_one("SELECT count(*) FROM runtime.provider_creation_operations WHERE verified_sandbox_id IS NOT NULL AND settlement_id IS NULL",&[]).unwrap().get::<_,i64>(0),1);
}

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_e2b_provision_rejects_wrong_signature_or_profile_before_launch_or_create() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (store, mut db, auth) = setup();
    let root = tempfile::tempdir().unwrap();
    let launcher = Arc::new(FixtureLauncher::new(
        "productReady",
        root.path().join("requests.jsonl"),
    ));
    let runtime = E2bRuntime::with_launcher(
        store.clone(),
        config(auth.payload.environment_profile.clone()),
        launcher.clone(),
    );
    assert!(matches!(
        runtime.provision(&auth, b"wrong-key"),
        Err(E2bRuntimeError::Rejected(
            "e2b_provision_authorization_rejected"
        ))
    ));
    let per_run = signed(auth, factory_profile());
    assert!(matches!(
        runtime.provision(&per_run, KEY),
        Err(E2bRuntimeError::Rejected("e2b_provision_profile_rejected"))
    ));
    assert_eq!(launcher.launches.load(Ordering::SeqCst), 0);
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.provider_creation_operations",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        0
    );
}
