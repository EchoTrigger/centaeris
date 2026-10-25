use super::*;
use crate::postgres_store::provider_maintenance::direct::{
    DirectCompletion, DirectDispatchState, DirectOutput,
};
use base64::Engine;

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_provider_direct_file_failure_is_durable_and_replayed_without_new_dispatch() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db, mut auth, target) = setup();
    let fixture: serde_json::Value = serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/workspace/fixtures/provider_environment_maintenance_completion/v1/file-not-found.json"))).unwrap();
    let request = fixture["request"].clone();
    auth.payload.request_digest = format!(
        "sha256:{:x}",
        Sha256::digest(
            crate::agent_run_authorization::canonical_json(request.clone())
                .unwrap()
                .as_bytes()
        )
    );
    db.execute("UPDATE public.app_core_environmentmaintenanceoperation SET request=$1::text::jsonb,request_digest=$2 WHERE operation_id='maint-task'", &[&request.to_string(), &auth.payload.request_digest]).unwrap();
    rewrite(&mut db, &mut auth);
    let configured = &target.identity.provider_config_digest;
    assert_eq!(
        store
            .claim_direct_provider_maintenance(&auth, KEY, configured)
            .unwrap(),
        DirectDispatchState::Fresh
    );
    assert_eq!(
        store
            .claim_direct_provider_maintenance(&auth, KEY, configured)
            .unwrap(),
        DirectDispatchState::Unknown
    );
    let completion = DirectCompletion::new(
        &auth,
        serde_json::from_value(fixture["output"].clone()).unwrap(),
    )
    .unwrap();
    store
        .record_direct_provider_maintenance_completion(&auth, KEY, configured, &completion)
        .unwrap();
    let reopened = PostgresRuntimeStore::new(&url).unwrap();
    assert_eq!(
        reopened
            .claim_direct_provider_maintenance(&auth, KEY, configured)
            .unwrap(),
        DirectDispatchState::Completed(Box::new(completion.clone()))
    );
    reopened
        .record_direct_provider_maintenance_completion(&auth, KEY, configured, &completion)
        .unwrap();
    assert_eq!(db.query_one("SELECT count(*) FROM runtime.runtime_events WHERE event_type='runtime.provider_environment.maintenance.dispatched.v1'", &[]).unwrap().get::<_, i64>(0), 1);
    assert_eq!(db.query_one("SELECT count(*) FROM runtime.runtime_events WHERE event_type='runtime.provider_environment.maintenance.completed.v1'", &[]).unwrap().get::<_, i64>(0), 1);
}

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_provider_direct_file_failure_cannot_complete_a_command() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db, auth, target) = setup();
    let configured = &target.identity.provider_config_digest;
    let output = serde_json::from_value(
        serde_json::json!({"kind":"filesystemFailed","code":"fileNotFound"}),
    )
    .unwrap();
    let completion = DirectCompletion::new(&auth, output).unwrap();
    assert_eq!(
        store
            .claim_direct_provider_maintenance(&auth, KEY, configured)
            .unwrap(),
        DirectDispatchState::Fresh
    );
    assert!(store
        .record_direct_provider_maintenance_completion(&auth, KEY, configured, &completion)
        .is_err());
    assert_eq!(
        store
            .claim_direct_provider_maintenance(&auth, KEY, configured)
            .unwrap(),
        DirectDispatchState::Unknown
    );
    assert_eq!(db.query_one("SELECT count(*) FROM runtime.runtime_events WHERE event_type='runtime.provider_environment.maintenance.completed.v1'", &[]).unwrap().get::<_, i64>(0), 0);
}

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_provider_direct_dispatch_survives_http_replay_and_process_restart_without_resending() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db, auth, target) = setup();
    let configured = &target.identity.provider_config_digest;
    assert_eq!(
        store
            .claim_direct_provider_maintenance(&auth, KEY, configured)
            .unwrap(),
        DirectDispatchState::Fresh
    );
    assert_eq!(
        store
            .claim_direct_provider_maintenance(&auth, KEY, configured)
            .unwrap(),
        DirectDispatchState::Unknown
    );
    let reopened = PostgresRuntimeStore::new(&url).unwrap();
    assert_eq!(
        reopened
            .claim_direct_provider_maintenance(&auth, KEY, configured)
            .unwrap(),
        DirectDispatchState::Unknown
    );
    assert_eq!(db.query_one("SELECT count(*) FROM runtime.runtime_events WHERE event_type='runtime.provider_environment.maintenance.dispatched.v1'",&[]).unwrap().get::<_,i64>(0),1);
    let raw = b"\0\xffcomplete result";
    let completion = DirectCompletion::new(
        &auth,
        DirectOutput::Command {
            exit_code: 7,
            stdout_bytes_base64: base64::engine::general_purpose::STANDARD.encode(raw),
            stdout_byte_length: raw.len() as u64,
            stderr_bytes_base64: String::new(),
            stderr_byte_length: 0,
        },
    )
    .unwrap();
    reopened
        .record_direct_provider_maintenance_completion(&auth, KEY, configured, &completion)
        .unwrap();
    let restarted = PostgresRuntimeStore::new(&url).unwrap();
    assert_eq!(
        restarted
            .claim_direct_provider_maintenance(&auth, KEY, configured)
            .unwrap(),
        DirectDispatchState::Completed(Box::new(completion.clone()))
    );
    let row=db.query_one("SELECT payload_json FROM runtime.runtime_events WHERE event_id='provider_maintenance_completed:maint-task'",&[]).unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&row.get::<_, String>(0)).unwrap(),
        serde_json::to_value(completion).unwrap()
    );
}

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_provider_direct_completion_rejects_missing_dispatch_and_wrong_binding() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db, auth, target) = setup();
    let configured = &target.identity.provider_config_digest;
    let completion = DirectCompletion::new(
        &auth,
        DirectOutput::Command {
            exit_code: 0,
            stdout_bytes_base64: String::new(),
            stdout_byte_length: 0,
            stderr_bytes_base64: String::new(),
            stderr_byte_length: 0,
        },
    )
    .unwrap();
    assert!(store
        .record_direct_provider_maintenance_completion(&auth, KEY, configured, &completion)
        .is_err());
    assert_eq!(
        store
            .claim_direct_provider_maintenance(&auth, KEY, configured)
            .unwrap(),
        DirectDispatchState::Fresh
    );
    let mut wrong = completion.dispatch_record().unwrap();
    wrong["providerTarget"]["sandboxId"] = serde_json::json!("other-sandbox");
    db.execute("UPDATE runtime.runtime_events SET payload_json=$1 WHERE event_id='provider_maintenance_dispatched:maint-task'",&[&wrong.to_string()]).unwrap();
    assert!(store
        .claim_direct_provider_maintenance(&auth, KEY, configured)
        .is_err());
    assert!(store
        .record_direct_provider_maintenance_completion(&auth, KEY, configured, &completion)
        .is_err());
    assert_eq!(db.query_one("SELECT count(*) FROM runtime.runtime_events WHERE event_type='runtime.provider_environment.maintenance.completed.v1'",&[]).unwrap().get::<_,i64>(0),0);
}
