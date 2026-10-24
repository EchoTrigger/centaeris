use super::*;
use crate::postgres_store::provider_operations::cleanup::ProviderCleanupReceiptQuery;
use crate::postgres_store::provider_operations::retained::{
    self as history, RetainedProviderRunUse, TargetLookup,
};

fn closed_use(
    store: &PostgresRuntimeStore,
    db: &mut Client,
) -> (
    Run,
    RetainedProviderRunUse,
    history::RetainedProviderCleanupReceipt,
) {
    let (mut run, _, value) = uses::candidate(store, db);
    terminal(db, &mut run);
    let receipt = store
        .confirm_retained_provider_run_cleanup(
            &run.authority(ControllerPurpose::Cleanup),
            &value,
            CONFIG,
            |value| Ok(uses::proof(value, 2)),
        )
        .unwrap();
    (run, value, receipt)
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_retained_history_receipt_and_use_keep_original_target_when_current_selection_changes() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db) = retained::setup();
    let (mut run, _, original) = uses::candidate(&store, &mut db);
    let mut current = original.target.clone();
    current.identity.attempt_id = "current-b-attempt".into();
    current.identity.instance_id = "current-b-instance".into();
    current.identity.execution_id = "current-b-execution".into();
    current.sandbox_id = "current-b-sandbox".into();
    let calls = std::sync::atomic::AtomicUsize::new(0);
    let admission = store.with_retained_provider_execution_dispatch_with_target_lookup(
        &run.authority(ControllerPurpose::Execute),
        &original,
        CONFIG,
        |_, selection| match selection {
            TargetLookup::CurrentEnvironment { .. } => Ok(current.clone()),
            TargetLookup::CreationAttempt { .. } => Ok(original.target.clone()),
        },
        |_| -> Result<(), String> {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        },
    );
    assert!(
        admission.is_err(),
        "historical A identity grants no live dispatch after current selects B"
    );
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    terminal(&mut db, &mut run);
    let receipt = store
        .confirm_retained_provider_run_cleanup(
            &run.authority(ControllerPurpose::Cleanup),
            &original,
            CONFIG,
            |value| Ok(uses::proof(value, 2)),
        )
        .unwrap();
    let lookup = |_: &mut Client, selection: TargetLookup| match selection {
        TargetLookup::CurrentEnvironment { .. } => Ok(current.clone()),
        TargetLookup::CreationAttempt {
            environment_id,
            creation_attempt_id,
        } => {
            assert_eq!(environment_id, original.target.identity.environment_id);
            assert_eq!(creation_attempt_id, original.target.identity.attempt_id);
            Ok(original.target.clone())
        }
    };
    assert_eq!(
        history::load_use_with_target_lookup(&mut db, &run.original.agent_run_id, lookup).unwrap(),
        Some(original.clone())
    );
    assert_eq!(
        history::receipts::load_receipt_with_target_lookup(
            &mut db,
            &run.original.agent_run_id,
            lookup
        )
        .unwrap(),
        Some(receipt)
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_retained_history_insert_and_reopen_keep_physical_attempt_anchor() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db) = retained::setup();
    let (run, original, receipt) = closed_use(&store, &mut db);
    assert_eq!(db.query_one("SELECT creation_attempt_id FROM runtime.provider_environment_run_uses WHERE agent_run_id=$1", &[&run.original.agent_run_id]).expect("a Run-use must persist its exact creation attempt").get::<_, String>(0), original.target.identity.attempt_id);
    let reopened = PostgresRuntimeStore::new(&url).unwrap();
    assert_eq!(
        reopened
            .retained_provider_run_use(&run.original.agent_run_id)
            .unwrap(),
        Some(original)
    );
    let query = ProviderCleanupReceiptQuery {
        schema: "runtime.agent_run.cleanup_receipt.get.v2".into(),
        agent_run_id: run.original.agent_run_id.clone(),
        authorization_digest: run.original.digest().unwrap(),
        cleanup_receipt_id: receipt.receipt_id.clone(),
    };
    assert_eq!(
        serde_json::to_value(
            reopened
                .get_provider_cleanup_receipt(&query)
                .unwrap()
                .unwrap()
        )
        .unwrap(),
        serde_json::to_value(receipt).unwrap()
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_retained_history_schema_requires_immutable_attempt_reference() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, _, mut db) = retained::setup();
    assert_eq!(
        db.query_one("SELECT max(version) FROM runtime.schema_migrations", &[])
            .unwrap()
            .get::<_, i64>(0),
        24
    );
    let column = db.query_opt("SELECT is_nullable FROM information_schema.columns WHERE table_schema='runtime' AND table_name='provider_environment_run_uses' AND column_name='creation_attempt_id'", &[]).unwrap().expect("history anchor is required");
    assert_eq!(column.get::<_, String>(0), "NO");
}

fn durable_facts(db: &mut Client) -> String {
    db.query_one("SELECT jsonb_build_object('creation',(SELECT jsonb_agg(to_jsonb(p)-'creation_owner_json'-'maintenance_operation_id' ORDER BY attempt_id) FROM runtime.provider_creation_operations p),'binding',(SELECT jsonb_agg(to_jsonb(b) ORDER BY environment_id) FROM runtime.provider_environment_bindings b),'uses',(SELECT jsonb_agg(to_jsonb(u)-'creation_attempt_id' ORDER BY provider_use_id) FROM runtime.provider_environment_run_uses u),'authorization',(SELECT jsonb_agg(to_jsonb(g) ORDER BY agent_run_id) FROM public.app_core_agentrunauthorization g))::text", &[]).unwrap().get(0)
}

fn history_versions(db: &mut Client, through: i64) -> Vec<(i64, i64)> {
    db.query("SELECT version,applied_at_ms FROM runtime.schema_migrations WHERE version<=$1 ORDER BY version", &[&through]).unwrap().into_iter().map(|r| (r.get(0),r.get(1))).collect()
}

fn copy_creation(
    db: &mut Client,
    environment: &str,
    profile: &str,
    config: &str,
    verified: bool,
) -> String {
    let attempt = format!("{environment}-attempt");
    db.execute("INSERT INTO runtime.provider_creation_operations(attempt_id,execution_id,instance_id,environment_id,session_id,workspace_id,agent_run_id,authorization_digest,profile_digest,provider,provider_config_digest,request_digest,idempotency_key,processing_job_id,dispatch_state,recorded_at_ms,creation_owner_json,maintenance_operation_id) SELECT $1,$1||'-execution',$1||'-instance',$2,session_id,workspace_id,agent_run_id,authorization_digest,$3,provider,$4,request_digest,$1||'-key',processing_job_id,'prepared',recorded_at_ms,creation_owner_json,maintenance_operation_id FROM runtime.provider_creation_operations ORDER BY attempt_id LIMIT 1", &[&attempt,&environment,&profile,&config]).unwrap();
    db.execute("UPDATE runtime.provider_creation_operations SET dispatch_state='dispatched' WHERE attempt_id=$1", &[&attempt]).unwrap();
    if verified {
        db.execute("UPDATE runtime.provider_creation_operations SET reported_sandbox_id=$1||'-sandbox' WHERE attempt_id=$1", &[&attempt]).unwrap();
        db.execute("UPDATE runtime.provider_creation_operations SET verified_sandbox_id=reported_sandbox_id,verification_digest=$2,verified_at_ms=(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint WHERE attempt_id=$1", &[&attempt,&REQUEST]).unwrap();
    }
    db.execute(
        "INSERT INTO runtime.provider_environment_bindings VALUES($1,$2,0)",
        &[&environment, &attempt],
    )
    .unwrap();
    attempt
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_retained_history_forward_migration_backfills_and_preserves_receipt_and_signed_facts() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db) = retained::setup();
    let (run, original, receipt) = closed_use(&store, &mut db);
    history::schema::restore_previous_fixture(&mut db);
    let before = durable_facts(&mut db);
    let versions = history_versions(&mut db, 20);
    assert_eq!(versions.len(), 20);
    let reopened = PostgresRuntimeStore::new(&url).unwrap();
    assert_eq!(durable_facts(&mut db), before);
    assert_eq!(history_versions(&mut db, 20), versions);
    assert_eq!(
        db.query_one(
            "SELECT creation_attempt_id FROM runtime.provider_environment_run_uses",
            &[]
        )
        .expect("legacy use must be backfilled")
        .get::<_, String>(0),
        original.target.identity.attempt_id
    );
    assert_eq!(
        reopened
            .retained_provider_run_use(&run.original.agent_run_id)
            .unwrap(),
        Some(original)
    );
    let query = ProviderCleanupReceiptQuery {
        schema: "runtime.agent_run.cleanup_receipt.get.v2".into(),
        agent_run_id: run.original.agent_run_id.clone(),
        authorization_digest: run.original.digest().unwrap(),
        cleanup_receipt_id: receipt.receipt_id.clone(),
    };
    assert_eq!(
        serde_json::to_value(
            reopened
                .get_provider_cleanup_receipt(&query)
                .unwrap()
                .unwrap()
        )
        .unwrap(),
        serde_json::to_value(receipt).unwrap()
    );
    drop(reopened);
    PostgresRuntimeStore::new(&url).unwrap();
    assert_eq!(durable_facts(&mut db), before);
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_retained_history_migration_preserves_dispatched_unknown_without_a_use() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db) = retained::setup();
    let run = retained_run(&store, &mut db);
    let operation = store
        .prepare_provider_creation(
            &run.authority(ControllerPurpose::Execute),
            &request("unknown-execution", "unknown-attempt", "unknown-key"),
        )
        .unwrap();
    store
        .dispatch_provider_creation(
            &run.authority(ControllerPurpose::Execute),
            &operation.identity,
        )
        .unwrap();
    history::schema::restore_previous_fixture(&mut db);
    let before = durable_facts(&mut db);
    let reopened = PostgresRuntimeStore::new(&url).unwrap();
    assert_eq!(history_versions(&mut db, 21).len(), 21);
    assert_eq!(durable_facts(&mut db), before);
    assert!(reopened
        .retained_provider_run_use(&run.original.agent_run_id)
        .unwrap()
        .is_none());
    assert!(reopened
        .bind_retained_provider_run_use(
            &run.authority(ControllerPurpose::Execute),
            "unknown-execution",
            "unknown-attempt",
            CONFIG
        )
        .is_err());
    let row=db.query_one("SELECT dispatch_state,reported_sandbox_id,verified_sandbox_id,settlement_id FROM runtime.provider_creation_operations",&[]).unwrap();
    assert_eq!(row.get::<_, String>(0), "dispatched");
    for column in 1..4 {
        assert!(row.get::<_, Option<String>>(column).is_none());
    }
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_retained_history_unverifiable_legacy_use_rolls_back_the_entire_migration() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db) = retained::setup();
    let (_, original, _) = closed_use(&store, &mut db);
    copy_creation(
        &mut db,
        "unknown-environment",
        &original.target.identity.profile_digest,
        CONFIG,
        false,
    );
    history::schema::restore_previous_fixture(&mut db);
    // Released schema allowed this structurally valid claim without a verified target.
    db.execute("INSERT INTO runtime.provider_environment_run_uses(provider_use_id,agent_run_id,environment_id,authorization_digest,execution_id,provider_use_digest,recorded_at_ms) VALUES('unknown-use','unknown-run','unknown-environment',$1,'unknown-use-execution',$1,0)", &[&REQUEST]).unwrap();
    let before = durable_facts(&mut db);
    let versions = history_versions(&mut db, 20);
    assert!(
        PostgresRuntimeStore::new(&url).is_err(),
        "unverifiable legacy responsibility must not acquire an invented anchor"
    );
    assert_eq!(durable_facts(&mut db), before);
    assert_eq!(history_versions(&mut db, 21), versions);
    assert!(!db.query_one("SELECT EXISTS(SELECT 1 FROM information_schema.columns WHERE table_schema='runtime' AND table_name='provider_environment_run_uses' AND column_name='creation_attempt_id')",&[]).unwrap().get::<_,bool>(0));
    history::validate(&mut db).unwrap();
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_retained_history_anchor_rejects_null_empty_missing_and_foreign_scope() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db) = retained::setup();
    let (a, _, original) = uses::candidate(&store, &mut db);
    let b = uses::successor(&store, &mut db, &a);
    let foreign = copy_creation(
        &mut db,
        "foreign-environment",
        &original.target.identity.profile_digest,
        CONFIG,
        true,
    );
    let insert="INSERT INTO runtime.provider_environment_run_uses(provider_use_id,agent_run_id,environment_id,authorization_digest,execution_id,provider_use_digest,recorded_at_ms,creation_attempt_id) VALUES('guard-use',$1,$2,$3,'guard-execution',$4,0,$5)";
    let digest = b.original.digest().unwrap();
    for anchor in [None, Some(""), Some("missing"), Some(foreign.as_str())] {
        assert!(db
            .execute(
                insert,
                &[
                    &b.original.agent_run_id,
                    &"environment",
                    &digest,
                    &REQUEST,
                    &anchor
                ]
            )
            .is_err());
    }
    assert!(
        db.execute(
            insert,
            &[
                &b.original.agent_run_id,
                &"foreign-environment",
                &digest,
                &REQUEST,
                &Some(foreign.as_str())
            ]
        )
        .is_err(),
        "foreign Environment cannot borrow another Run's grant"
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
    // Positive control: ordinary SQL can insert an open scoped use; digest verification
    // remains a separate read boundary, as in the released receipt-fact tests.
    db.execute(
        insert,
        &[
            &b.original.agent_run_id,
            &"environment",
            &digest,
            &REQUEST,
            &Some(original.target.identity.attempt_id.as_str()),
        ],
    )
    .unwrap();
    assert!(store
        .retained_provider_run_use(&b.original.agent_run_id)
        .is_err());
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_retained_history_foreign_profile_and_changed_original_scope_are_rejected() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db) = retained::setup();
    let (a, _, original) = uses::candidate(&store, &mut db);
    let b = uses::successor(&store, &mut db, &a);
    let foreign = copy_creation(&mut db, "profile-environment", REQUEST, CONFIG, true);
    db.execute(
        "UPDATE public.app_core_agentrun SET environment_id='profile-environment' WHERE id=$1",
        &[&b.original.agent_run_id],
    )
    .unwrap();
    assert!(db.execute("INSERT INTO runtime.provider_environment_run_uses(provider_use_id,agent_run_id,environment_id,authorization_digest,execution_id,provider_use_digest,recorded_at_ms,creation_attempt_id) VALUES('foreign-profile-use',$1,'profile-environment',$2,'foreign-profile-execution',$3,0,$4)",&[&b.original.agent_run_id,&b.original.digest().unwrap(),&REQUEST,&foreign]).is_err(),"matching Environment alone does not grant a foreign frozen profile");
    let config_foreign = copy_creation(
        &mut db,
        "config-environment",
        &original.target.identity.profile_digest,
        REQUEST,
        true,
    );
    db.execute(
        "UPDATE public.app_core_agentrun SET environment_id='config-environment' WHERE id=$1",
        &[&b.original.agent_run_id],
    )
    .unwrap();
    assert!(db.execute("INSERT INTO runtime.provider_environment_run_uses(provider_use_id,agent_run_id,environment_id,authorization_digest,execution_id,provider_use_digest,recorded_at_ms,creation_attempt_id) VALUES('foreign-config-use',$1,'config-environment',$2,'foreign-config-execution',$3,0,$4)",&[&b.original.agent_run_id,&b.original.digest().unwrap(),&REQUEST,&config_foreign]).is_err(),"matching profile does not grant a foreign provider configuration");
    db.execute(
        "UPDATE public.app_core_agentrun SET environment_id='foreign-environment' WHERE id=$1",
        &[&a.original.agent_run_id],
    )
    .unwrap();
    assert!(
        store
            .retained_provider_run_use(&a.original.agent_run_id)
            .is_err(),
        "history read must validate its original Run scope"
    );
    assert!(db.execute("UPDATE runtime.provider_environment_run_uses SET cleanup_receipt_id='receipt',cleanup_controller_epoch=2,quiescence_digest=$2,cleanup_recorded_at_ms=(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint WHERE agent_run_id=$1",&[&a.original.agent_run_id,&REQUEST]).is_err(),"cleanup UPDATE must recheck the original scope");
    assert_eq!(original.target.identity.environment_id, "environment");
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_retained_history_attempt_fk_and_immutable_guards_survive_reopen() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db) = retained::setup();
    let (_, _, original) = uses::candidate(&store, &mut db);
    assert!(db.execute("UPDATE runtime.provider_environment_run_uses SET creation_attempt_id='missing' WHERE provider_use_id=$1",&[&original.provider_use_id]).is_err());
    assert!(db
        .execute(
            "DELETE FROM runtime.provider_environment_run_uses WHERE provider_use_id=$1",
            &[&original.provider_use_id]
        )
        .is_err());
    assert!(db
        .execute(
            "UPDATE runtime.provider_environment_bindings SET creation_attempt_id='missing'",
            &[]
        )
        .is_err());
    assert!(db
        .execute("DELETE FROM runtime.provider_environment_bindings", &[])
        .is_err());
    // Retain the constraint count while grafting the FK onto the wrong authority.
    // Reopening must inspect its target, not merely count constraints.
    db.batch_execute("ALTER TABLE runtime.provider_environment_run_uses DROP CONSTRAINT retained_provider_run_use_creation_attempt_fkey; ALTER TABLE runtime.provider_environment_run_uses ADD CONSTRAINT retained_provider_run_use_creation_attempt_fkey FOREIGN KEY(creation_attempt_id) REFERENCES runtime.provider_environment_bindings(creation_attempt_id)").expect("history anchor must have its own exact physical-attempt FK");
    assert!(PostgresRuntimeStore::new(&url).is_err());
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_retained_history_attempt_fk_is_effective_without_the_scope_trigger() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db) = retained::setup();
    let (a, _, _) = uses::candidate(&store, &mut db);
    let b = uses::successor(&store, &mut db, &a);
    db.batch_execute("ALTER TABLE runtime.provider_environment_run_uses DISABLE TRIGGER retained_provider_run_use_facts").unwrap();
    for anchor in [None, Some(""), Some("missing")] {
        let error=db.execute("INSERT INTO runtime.provider_environment_run_uses(provider_use_id,agent_run_id,environment_id,authorization_digest,execution_id,provider_use_digest,recorded_at_ms,creation_attempt_id) VALUES('fk-use',$1,'environment',$2,'fk-execution',$3,0,$4)",&[&b.original.agent_run_id,&b.original.digest().unwrap(),&REQUEST,&anchor]).unwrap_err();
        let expected = if anchor.is_none() {
            &postgres::error::SqlState::NOT_NULL_VIOLATION
        } else {
            &postgres::error::SqlState::FOREIGN_KEY_VIOLATION
        };
        assert_eq!(error.code(), Some(expected));
    }
    db.batch_execute("ALTER TABLE runtime.provider_environment_run_uses ENABLE TRIGGER retained_provider_run_use_facts").unwrap();
    PostgresRuntimeStore::new(&url).unwrap();
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
