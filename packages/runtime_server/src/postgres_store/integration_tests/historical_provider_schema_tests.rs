//! Released provider facts remain archival; native startup gains no settlement authority.
use super::retained_instance_tests as native;
use super::*;
use std::sync::{Arc, Mutex};

const TABLES: &[&str] = &[
    "provider_creation_operations",
    "provider_run_cleanup_receipts",
    "provider_environment_bindings",
    "provider_environment_run_uses",
    "provider_environment_retirements",
    "provider_environment_power_operations",
    "provider_environment_provision_completions",
    "provider_environment_maintenance_completions",
];
fn snapshot(db: &mut Client) -> Vec<String> {
    let mut result = Vec::new();
    for table in
        TABLES
            .iter()
            .copied()
            .chain(["schema_migrations", "resident_sandboxes", "runtime_jobs"])
    {
        let sql = format!(
            "SELECT COALESCE(jsonb_agg(j ORDER BY j::text),'[]'::jsonb)::text FROM (SELECT to_jsonb(t) AS j FROM runtime.{table} t) record_rows"
        );
        result.push(db.query_one(&sql, &[]).unwrap().get(0));
    }
    result
}
fn seed_creation(store: &PostgresRuntimeStore, db: &mut Client) {
    seed_creation_with_job_status(store, db, RuntimeJobStatus::Succeeded);
}
fn seed_creation_with_job_status(
    store: &PostgresRuntimeStore,
    db: &mut Client,
    status: RuntimeJobStatus,
) {
    let mut record = job("archive-job", "archive-session");
    record.status = status;
    store
        .schedule_runtime_job(ScheduleRuntimeJobRequest { job: record })
        .unwrap();
    let digest = format!("sha256:{}", "a".repeat(64));
    let owner = r#"{"kind":"agentRun","runId":"archive-run"}"#;
    db.execute("INSERT INTO runtime.provider_creation_operations(attempt_id,execution_id,instance_id,environment_id,session_id,workspace_id,agent_run_id,authorization_digest,profile_digest,provider,provider_config_digest,request_digest,idempotency_key,processing_job_id,dispatch_state,recorded_at_ms,creation_owner_json) VALUES('archive-attempt','archive-execution','archive-instance','archive-environment','archive-session','archive-workspace','archive-run',$1,$1,'e2b',$1,$1,'archive-key','archive-job','prepared',0,$2)", &[&digest,&owner]).unwrap();
}
fn verify_target(db: &mut Client) {
    let digest = format!("sha256:{}", "a".repeat(64));
    db.execute("UPDATE runtime.provider_creation_operations SET dispatch_state='dispatched' WHERE attempt_id='archive-attempt'", &[]).unwrap();
    db.execute("UPDATE runtime.provider_creation_operations SET reported_sandbox_id='archive-sandbox' WHERE attempt_id='archive-attempt'", &[]).unwrap();
    db.execute("UPDATE runtime.provider_creation_operations SET verified_sandbox_id='archive-sandbox',verification_digest=$1,verified_at_ms=1 WHERE attempt_id='archive-attempt'", &[&digest]).unwrap();
    db.execute("INSERT INTO runtime.provider_environment_bindings(environment_id,creation_attempt_id,recorded_at_ms) VALUES('archive-environment','archive-attempt',1)", &[]).unwrap();
}
fn remove_target(db: &mut Client) {
    db.execute("UPDATE runtime.provider_creation_operations SET settlement_id='archive-removal',settlement_kind='removed',settlement_sandbox_id='archive-sandbox',settlement_controller_epoch=2,settled_at_ms=2 WHERE attempt_id='archive-attempt'", &[]).unwrap();
}
fn open_use(db: &mut Client) {
    let digest = format!("sha256:{}", "a".repeat(64));
    let payload=serde_json::json!({"agentRunId":"archive-run","sessionId":"archive-session","workspaceId":"archive-workspace","environmentProfileDigest":digest,"environmentProfile":{"schema":"runtime.e2b_environment_profile.v1","lifetime":"retained","providerConfigDigest":digest}}).to_string();
    db.execute("INSERT INTO public.app_core_agentrun(id,session_id,environment_id,workspace_id,user_id,membership_ref,status) VALUES('archive-run','archive-session','archive-environment','archive-workspace','archive-user','archive-member','completed')", &[]).unwrap();
    db.execute("INSERT INTO public.app_core_agentrunauthorization(agent_run_id,digest,payload,signature) VALUES('archive-run',$1,$2::text::jsonb,'archival-fixture')", &[&digest,&payload]).unwrap();
    db.execute("INSERT INTO runtime.provider_environment_run_uses(provider_use_id,agent_run_id,environment_id,authorization_digest,execution_id,provider_use_digest,recorded_at_ms,creation_attempt_id) VALUES('archive-use','archive-run','archive-environment',$1,'archive-use-execution',$1,1,'archive-attempt')", &[&digest]).unwrap();
}
#[test]
#[ignore = "requires owned disposable PostgreSQL"]
fn postgres_native_archive_fresh_sixteen_upgrade_preserves_native_resident_and_all_version_timestamps()
 {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db) = native::setup();
    let run = native::run(&store, &mut db, "native-origin", 1);
    let inventory = Arc::new(Mutex::new(Vec::new()));
    let allocation = native::prepare(&store, &run, "native-origin-execution", &inventory).unwrap();
    crate::postgres_store::historical_provider_schema::restore_empty_fixture(&mut db);
    let before: Vec<(i64, i64)> = db
        .query(
            "SELECT version,applied_at_ms FROM runtime.schema_migrations ORDER BY version",
            &[],
        )
        .unwrap()
        .iter()
        .map(|r| (r.get(0), r.get(1)))
        .collect();
    assert_eq!(before.len(), 16);
    let resident: String = db
        .query_one(
            "SELECT row_to_json(s)::text FROM runtime.resident_sandboxes s WHERE execution_id=$1",
            &[&allocation.allocation_execution_id],
        )
        .unwrap()
        .get(0);
    drop(PostgresRuntimeStore::new(&url).unwrap());
    let after: Vec<(i64, i64)> = db
        .query(
            "SELECT version,applied_at_ms FROM runtime.schema_migrations ORDER BY version",
            &[],
        )
        .unwrap()
        .iter()
        .map(|r| (r.get(0), r.get(1)))
        .collect();
    assert_eq!(&after[..16], before.as_slice());
    assert_eq!(after.len(), 25);
    assert_eq!(
        db.query_one(
            "SELECT row_to_json(s)::text FROM runtime.resident_sandboxes s WHERE execution_id=$1",
            &[&allocation.allocation_execution_id]
        )
        .unwrap()
        .get::<_, String>(0),
        resident
    );
}
#[test]
#[ignore = "requires owned disposable PostgreSQL"]
fn postgres_native_archive_reopen_preserves_removed_bound_history_without_reinterpreting_it_as_live()
 {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db) = native::setup();
    seed_creation(&store, &mut db);
    verify_target(&mut db);
    open_use(&mut db);
    let digest = format!("sha256:{}", "b".repeat(64));
    db.execute("UPDATE runtime.provider_environment_run_uses SET cleanup_receipt_id='archive-receipt',cleanup_controller_epoch=2,quiescence_digest=$1,cleanup_recorded_at_ms=2 WHERE provider_use_id='archive-use'",&[&digest]).unwrap();
    remove_target(&mut db);
    let before = snapshot(&mut db);
    drop(PostgresRuntimeStore::new(&url).unwrap());
    assert_eq!(snapshot(&mut db), before);
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.provider_environment_bindings",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        1
    );
}
#[test]
#[ignore = "requires owned disposable PostgreSQL"]
fn postgres_native_archive_unknown_creation_blocks_activation_without_refund_or_rewrite() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db) = native::setup();
    seed_creation(&store, &mut db);
    db.execute("UPDATE runtime.provider_creation_operations SET dispatch_state='dispatched' WHERE attempt_id='archive-attempt'",&[]).unwrap();
    let before = snapshot(&mut db);
    assert!(
        PostgresRuntimeStore::new(&url).err().as_deref()
            == Some("native_activation_provider_creation_unsettled"),
        "unsettled provider responsibility must prevent native activation"
    );
    assert_eq!(snapshot(&mut db), before);
}
#[test]
#[ignore = "requires owned disposable PostgreSQL"]
fn postgres_native_archive_pending_power_blocks_activation_even_with_retired_binding() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db) = native::setup();
    seed_creation(&store, &mut db);
    verify_target(&mut db);
    let digest = format!("sha256:{}", "a".repeat(64));
    db.execute("INSERT INTO runtime.provider_environment_power_operations(operation_id,environment_id,operation_sequence,request_digest,request_json,action,dispatch_state,recorded_at_ms) VALUES('archive-pause','archive-environment',1,$1,'{}','pause','prepared',1)",&[&digest]).unwrap();
    remove_target(&mut db);
    let before = snapshot(&mut db);
    assert!(
        PostgresRuntimeStore::new(&url).err().as_deref()
            == Some("native_activation_provider_power_unsettled"),
        "pending power must remain supervised"
    );
    assert_eq!(snapshot(&mut db), before);
}
#[test]
#[ignore = "requires owned disposable PostgreSQL"]
fn postgres_native_archive_pending_run_cleanup_blocks_activation_after_exact_physical_removal() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db) = native::setup();
    seed_creation(&store, &mut db);
    verify_target(&mut db);
    open_use(&mut db);
    remove_target(&mut db);
    let before = snapshot(&mut db);
    assert!(
        PostgresRuntimeStore::new(&url).err().as_deref()
            == Some("native_activation_provider_run_cleanup_unsettled"),
        "physical removal does not erase run cleanup responsibility"
    );
    assert_eq!(snapshot(&mut db), before);
}
#[test]
#[ignore = "requires owned disposable PostgreSQL"]
fn postgres_native_archive_wrong_removed_sandbox_cannot_clear_an_uncertain_responsibility() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db) = native::setup();
    seed_creation(&store, &mut db);
    verify_target(&mut db);
    assert!(db.execute("UPDATE runtime.provider_creation_operations SET settlement_id='wrong-removal',settlement_kind='removed',settlement_sandbox_id='foreign-sandbox',settlement_controller_epoch=2,settled_at_ms=2 WHERE attempt_id='archive-attempt'",&[]).is_err());
    let before = snapshot(&mut db);
    assert!(
        PostgresRuntimeStore::new(&url).err().as_deref()
            == Some("native_activation_provider_creation_unsettled")
    );
    assert_eq!(snapshot(&mut db), before);
}

#[test]
#[ignore = "requires owned disposable PostgreSQL"]
fn postgres_native_archive_queued_creator_blocks_activation_after_exact_physical_removal() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db) = native::setup();
    seed_creation_with_job_status(&store, &mut db, RuntimeJobStatus::Queued);
    verify_target(&mut db);
    remove_target(&mut db);
    let before = snapshot(&mut db);
    assert!(
        PostgresRuntimeStore::new(&url).err().as_deref()
            == Some("native_activation_provider_creator_job_unsettled"),
        "physical removal does not complete the original creator Job"
    );
    assert_eq!(snapshot(&mut db), before);
}
