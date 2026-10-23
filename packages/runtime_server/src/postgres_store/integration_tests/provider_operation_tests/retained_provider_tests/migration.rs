use super::*;

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_retained_provider_forward_migration_keeps_original_signatures_and_run_owned_unknowns() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db) = retained::setup();
    let original = run(&store, &mut db);
    let operation = store
        .prepare_provider_creation(
            &original.authority(ControllerPurpose::Execute),
            &request("legacy-execution", "legacy-attempt", "legacy-key"),
        )
        .unwrap();
    store
        .dispatch_provider_creation(
            &original.authority(ControllerPurpose::Execute),
            &operation.identity,
        )
        .unwrap();
    crate::postgres_store::provider_operations::retained::restore_previous_fixture(&mut db);
    let old: String = db
        .query_one(
            "SELECT row_to_json(p)::text FROM runtime.provider_creation_operations p",
            &[],
        )
        .unwrap()
        .get(0);
    let grant: String = db
        .query_one(
            "SELECT row_to_json(g)::text FROM public.app_core_agentrunauthorization g",
            &[],
        )
        .unwrap()
        .get(0);
    let history: Vec<(i64, i64)> = db
        .query(
            "SELECT version,applied_at_ms FROM runtime.schema_migrations ORDER BY version",
            &[],
        )
        .unwrap()
        .into_iter()
        .map(|r| (r.get(0), r.get(1)))
        .collect();
    assert_eq!(history.len(), 18);
    drop(PostgresRuntimeStore::new(&url).unwrap());
    assert_eq!(
        db.query_one(
            "SELECT row_to_json(p)::text FROM runtime.provider_creation_operations p",
            &[]
        )
        .unwrap()
        .get::<_, String>(0),
        old
    );
    assert_eq!(
        db.query_one(
            "SELECT row_to_json(g)::text FROM public.app_core_agentrunauthorization g",
            &[]
        )
        .unwrap()
        .get::<_, String>(0),
        grant
    );
    assert_eq!(db.query("SELECT version,applied_at_ms FROM runtime.schema_migrations WHERE version<=18 ORDER BY version",&[]).unwrap().into_iter().map(|r|(r.get(0),r.get(1))).collect::<Vec<(i64,i64)>>(),history);
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.provider_environment_bindings",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        0
    );
    assert_eq!(
        db.query_one("SELECT max(version) FROM runtime.schema_migrations", &[])
            .unwrap()
            .get::<_, i64>(0),
        20
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_retained_provider_schema_and_immutable_use_facts_are_enforced() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    for change in ["ALTER TABLE runtime.provider_environment_run_uses DROP CONSTRAINT provider_environment_run_uses_environment_id_fkey", "ALTER TABLE runtime.provider_environment_run_uses DISABLE TRIGGER retained_provider_run_use_facts", "CREATE OR REPLACE FUNCTION runtime.protect_provider_environment_binding() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END; $$"] {
        let (url,_,mut db)=retained::setup();db.batch_execute(change).unwrap();assert!(PostgresRuntimeStore::new(&url).is_err(),"schema tampering must fail before use");
    }
}
