use super::{reset_store, test_url, PostgresRuntimeStore, TEST_LOCK};
use postgres::{error::SqlState, Client, NoTls};

fn setup() -> (String, Client) {
    // Check the live database as well as the reset opt-in before any destructive SQL.
    let url = test_url();
    let mut db = Client::connect(&url, NoTls).unwrap();
    super::owned_database::require_owned_database(&mut db);
    reset_store(&url);
    drop(PostgresRuntimeStore::new(&url).unwrap());
    (url, db)
}

fn receipt_table_exists(db: &mut Client) -> bool {
    db.query_one(
        "SELECT to_regclass('runtime.agent_run_cleanup_receipts') IS NOT NULL",
        &[],
    )
    .unwrap()
    .get(0)
}

fn history(db: &mut Client) -> Vec<(i64, i64)> {
    db.query(
        "SELECT version,applied_at_ms FROM runtime.schema_migrations ORDER BY version",
        &[],
    )
    .unwrap()
    .iter()
    .map(|row| (row.get(0), row.get(1)))
    .collect()
}

fn downgrade_to_nine(db: &mut Client) {
    super::super::environment_owner_schema::restore_previous_fixture(db);
    db.batch_execute(
        "DROP TABLE IF EXISTS runtime.environment_maintenance_completions;
         DROP FUNCTION IF EXISTS runtime.protect_environment_maintenance_completion();
         DROP TABLE IF EXISTS runtime.environment_retirements;
         DROP FUNCTION IF EXISTS runtime.protect_environment_retirement();
         DROP TABLE IF EXISTS runtime.agent_run_cleanup_receipts;
         DELETE FROM runtime.schema_migrations WHERE version>=10;",
    )
    .unwrap();
    assert_eq!(
        history(db).iter().map(|row| row.0).collect::<Vec<_>>(),
        (1..=9).collect::<Vec<_>>()
    );
}

fn seed_live_allocation(db: &mut Client) {
    db.batch_execute(
        "INSERT INTO runtime.execution_instances(
            execution_id,instance_id,environment_id,session_id,agent_run_id,host_id,
            authorization_digest,binding_digest,image_digest,allocation_execution_id,
            allocation_mode,profile_digest)
         VALUES('allocation','instance','environment','session','run-a','host',
            'authorization','binding','image','allocation','retained','sha256:'||repeat('a',64));
         INSERT INTO runtime.execution_instances(
            execution_id,instance_id,environment_id,session_id,agent_run_id,host_id,
            authorization_digest,binding_digest,image_digest,allocation_execution_id,
            allocation_mode,profile_digest)
         VALUES('attachment','instance','environment','session','run-b','host',
            'authorization-b','binding-b','image','allocation','retained','sha256:'||repeat('a',64));
         INSERT INTO runtime.resident_sandboxes(
            host_id,execution_id,agent_run_id,workspace_id,container_id,state,
            memory_bytes,cpu_milli,pids,workspace_bytes,created_at_ms,updated_at_ms,
            instance_id,environment_id,allocation_mode)
         VALUES('host','allocation','run-a','workspace','container','resident',
            268435456,1000,128,536870912,123,456,'instance','environment','retained');",
    )
    .unwrap();
}

fn allocation_facts(db: &mut Client) -> Vec<String> {
    db.query(
        "SELECT (to_jsonb(i)-'allocation_owner_json')::text FROM runtime.execution_instances i ORDER BY execution_id",
        &[],
    )
    .unwrap()
    .iter()
    .map(|row| row.get(0))
    .chain(
        db.query(
            "SELECT (to_jsonb(s)-'allocation_owner_json')::text FROM runtime.resident_sandboxes s ORDER BY execution_id",
            &[],
        )
        .unwrap()
        .iter()
        .map(|row| row.get(0)),
    )
    .collect()
}

fn insert_receipt(db: &mut Client, patch: serde_json::Value) -> Result<u64, postgres::Error> {
    let mut receipt = serde_json::json!({
        "receipt_id":"receipt-a", "agent_run_id":"run-a", "environment_id":"environment",
        "authorization_digest":"authorization", "controller_epoch":1,
        "allocation_kind":"instance", "instance_id":"instance", "cleanup_complete":true,
        "file_protection":"deferred", "recorded_at_ms":789,
        "physical_removed":false, "removed_container_id":null
    });
    receipt
        .as_object_mut()
        .unwrap()
        .extend(patch.as_object().unwrap().clone());
    db.execute(
        "INSERT INTO runtime.agent_run_cleanup_receipts SELECT * FROM json_populate_record(
            NULL::runtime.agent_run_cleanup_receipts,$1::text::json)",
        &[&receipt.to_string()],
    )
}

#[test]
#[ignore = "requires destructive gate-owned disposable Postgres database"]
fn postgres_cleanup_migration_fresh_store_creates_receipt_table() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (_, mut db) = setup();
    assert!(
        receipt_table_exists(&mut db),
        "fresh store must persist cleanup receipts"
    );
    assert_eq!(
        history(&mut db).iter().map(|row| row.0).collect::<Vec<_>>(),
        (1..=21).collect::<Vec<_>>()
    );
}

#[test]
#[ignore = "requires destructive gate-owned disposable Postgres database"]
fn postgres_cleanup_migration_preserves_allocations_resources_and_receipts_on_reopen() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (url, mut db) = setup();
    downgrade_to_nine(&mut db);
    seed_live_allocation(&mut db);
    let before_history = history(&mut db);
    let before_facts = allocation_facts(&mut db);

    drop(PostgresRuntimeStore::new(&url).unwrap());
    assert!(
        receipt_table_exists(&mut db),
        "v9 must migrate to cleanup receipt storage"
    );
    assert_eq!(allocation_facts(&mut db), before_facts);
    let migrated_history = history(&mut db);
    assert_eq!(&migrated_history[..9], before_history.as_slice());
    assert_eq!(
        migrated_history.iter().map(|row| row.0).collect::<Vec<_>>(),
        (1..=21).collect::<Vec<_>>()
    );
    assert_eq!(migrated_history[9].0, 10);

    insert_receipt(&mut db, serde_json::json!({})).unwrap();
    insert_receipt(
        &mut db,
        serde_json::json!({
            "receipt_id":"receipt-absent", "agent_run_id":"run-absent",
            "allocation_kind":"neverAllocated", "instance_id":null
        }),
    )
    .unwrap();
    let receipts = db.query(
        "SELECT row_to_json(r)::text FROM runtime.agent_run_cleanup_receipts r ORDER BY receipt_id",
        &[],
    ).unwrap().iter().map(|row| row.get::<_, String>(0)).collect::<Vec<_>>();
    drop(PostgresRuntimeStore::new(&url).unwrap());
    assert_eq!(history(&mut db), migrated_history);
    assert_eq!(allocation_facts(&mut db), before_facts);
    assert_eq!(db.query(
        "SELECT row_to_json(r)::text FROM runtime.agent_run_cleanup_receipts r ORDER BY receipt_id",
        &[],
    ).unwrap().iter().map(|row| row.get::<_, String>(0)).collect::<Vec<_>>(), receipts);
}

#[test]
#[ignore = "requires destructive gate-owned disposable Postgres database"]
fn postgres_cleanup_migration_enforces_complete_exclusive_and_unique_receipts() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (_, mut db) = setup();
    for patch in [
        serde_json::json!({"controller_epoch":0}),
        serde_json::json!({"cleanup_complete":false}),
        serde_json::json!({"file_protection":"complete"}),
        serde_json::json!({"allocation_kind":"unknown"}),
        serde_json::json!({"instance_id":null}),
        serde_json::json!({"instance_id":""}),
        serde_json::json!({"allocation_kind":"neverAllocated"}),
    ] {
        let error = insert_receipt(&mut db, patch.clone()).unwrap_err();
        assert_eq!(
            error.code(),
            Some(&SqlState::CHECK_VIOLATION),
            "{patch}: {error:?}"
        );
    }
    insert_receipt(&mut db, serde_json::json!({})).unwrap();
    for patch in [
        serde_json::json!({"receipt_id":"another-receipt"}),
        serde_json::json!({"agent_run_id":"another-run"}),
    ] {
        let error = insert_receipt(&mut db, patch.clone()).unwrap_err();
        assert_eq!(
            error.code(),
            Some(&SqlState::UNIQUE_VIOLATION),
            "{patch}: {error:?}"
        );
    }
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.agent_run_cleanup_receipts",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        1
    );
}

#[test]
#[ignore = "requires destructive gate-owned disposable Postgres database"]
fn postgres_cleanup_migration_rejects_invalid_legacy_schema_without_partial_upgrade() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    for conflicting_receipt_table in [false, true] {
        let (url, mut db) = setup();
        downgrade_to_nine(&mut db);
        seed_live_allocation(&mut db);
        let before_history = history(&mut db);
        let before_facts = allocation_facts(&mut db);
        if conflicting_receipt_table {
            db.batch_execute("CREATE TABLE runtime.agent_run_cleanup_receipts(receipt_id text);
                INSERT INTO runtime.agent_run_cleanup_receipts VALUES('unrecognized-existing-fact');").unwrap();
        } else {
            db.batch_execute("ALTER TABLE runtime.resident_sandboxes DROP CONSTRAINT resident_sandboxes_pids_check").unwrap();
        }
        assert!(PostgresRuntimeStore::new(&url).is_err());
        assert_eq!(
            history(&mut db),
            before_history,
            "failed migration cannot advance version"
        );
        assert_eq!(allocation_facts(&mut db), before_facts);
        assert_eq!(receipt_table_exists(&mut db), conflicting_receipt_table);
        if conflicting_receipt_table {
            assert_eq!(
                db.query_one(
                    "SELECT receipt_id FROM runtime.agent_run_cleanup_receipts",
                    &[]
                )
                .unwrap()
                .get::<_, String>(0),
                "unrecognized-existing-fact"
            );
        }
    }
}
