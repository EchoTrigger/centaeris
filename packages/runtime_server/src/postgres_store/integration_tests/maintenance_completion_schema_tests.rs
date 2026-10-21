use super::{reset_store, test_url, PostgresRuntimeStore, TEST_LOCK};
use postgres::{error::SqlState, Client, NoTls};

fn setup() -> (String, Client) {
    let url = test_url();
    let mut db = Client::connect(&url, NoTls).unwrap();
    super::owned_database::require_owned_database(&mut db);
    reset_store(&url);
    drop(PostgresRuntimeStore::new(&url).unwrap());
    (url, db)
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

fn downgrade_to_eleven(db: &mut Client) {
    super::super::environment_owner_schema::restore_previous_fixture(db);
    db.batch_execute(
        "DROP TABLE IF EXISTS runtime.environment_maintenance_completions;
        DROP FUNCTION IF EXISTS runtime.protect_environment_maintenance_completion();
        DELETE FROM runtime.schema_migrations WHERE version>=12;",
    )
    .unwrap();
    assert_eq!(
        history(db).iter().map(|row| row.0).collect::<Vec<_>>(),
        (1..=11).collect::<Vec<_>>()
    );
}

fn insert_completion(db: &mut Client) -> Result<u64, postgres::Error> {
    db.execute("INSERT INTO runtime.environment_maintenance_completions(operation_id,environment_id,
        instance_id,container_id,profile_digest,controller_epoch,controller_lease_digest,
        request_digest,completion_json,recorded_at_ms)
        VALUES('maintenance-1','environment','instance','container','profile',3,'lease','request','{\"completed\":true}',123)", &[])
}

fn completion(db: &mut Client) -> String {
    db.query_one(
        "SELECT row_to_json(c)::text FROM runtime.environment_maintenance_completions c",
        &[],
    )
    .unwrap()
    .get(0)
}

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_maintenance_completion_fresh_schema_persists_immutable_facts() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (url, mut db) = setup();
    assert!(db
        .query_one(
            "SELECT to_regclass('runtime.environment_maintenance_completions') IS NOT NULL",
            &[]
        )
        .unwrap()
        .get::<_, bool>(0));
    insert_completion(&mut db).unwrap();
    let before = completion(&mut db);
    assert_eq!(
        insert_completion(&mut db).unwrap_err().code(),
        Some(&SqlState::UNIQUE_VIOLATION)
    );
    for mutation in [
        "UPDATE runtime.environment_maintenance_completions SET completion_json='private replacement'",
        "UPDATE runtime.environment_maintenance_completions SET controller_epoch=controller_epoch",
        "DELETE FROM runtime.environment_maintenance_completions",
    ] {
        assert_eq!(db.execute(mutation, &[]).unwrap_err().code(), Some(&SqlState::RAISE_EXCEPTION));
        assert_eq!(completion(&mut db), before);
    }
    drop(PostgresRuntimeStore::new(&url).unwrap());
    assert_eq!(completion(&mut db), before);
}

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_maintenance_completion_v11_migration_preserves_existing_facts_and_history() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (url, mut db) = setup();
    downgrade_to_eleven(&mut db);
    db.batch_execute("INSERT INTO runtime.session_runtime_snapshots VALUES('kept-session','{\"fact\":\"retained\"}',123);").unwrap();
    let before = db
        .query_one(
            "SELECT row_to_json(s)::text FROM runtime.session_runtime_snapshots s",
            &[],
        )
        .unwrap()
        .get::<_, String>(0);
    let previous = history(&mut db);
    drop(PostgresRuntimeStore::new(&url).unwrap());
    assert_eq!(
        history(&mut db).iter().map(|row| row.0).collect::<Vec<_>>(),
        (1..=16).collect::<Vec<_>>()
    );
    assert_eq!(&history(&mut db)[..11], previous.as_slice());
    assert_eq!(
        db.query_one(
            "SELECT row_to_json(s)::text FROM runtime.session_runtime_snapshots s",
            &[]
        )
        .unwrap()
        .get::<_, String>(0),
        before
    );
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.environment_maintenance_completions",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        0
    );
    insert_completion(&mut db).unwrap();
    let after = completion(&mut db);
    drop(PostgresRuntimeStore::new(&url).unwrap());
    assert_eq!(completion(&mut db), after);
}

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_maintenance_completion_reopen_rejects_shape_key_and_trigger_drift() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    for (change, expected) in [
        ("ALTER TABLE runtime.environment_maintenance_completions ALTER COLUMN profile_digest DROP NOT NULL", "table definition mismatch"),
        ("ALTER TABLE runtime.environment_maintenance_completions ADD COLUMN unexpected text", "table definition mismatch"),
        ("ALTER TABLE runtime.environment_maintenance_completions DROP CONSTRAINT environment_maintenance_completions_pkey", "maintenance completion identity constraints mismatch"),
        ("DROP TRIGGER environment_maintenance_completion_facts ON runtime.environment_maintenance_completions", "immutable maintenance completion trigger"),
        ("ALTER TABLE runtime.environment_maintenance_completions DISABLE TRIGGER environment_maintenance_completion_facts", "immutable maintenance completion trigger"),
        ("DROP TRIGGER environment_maintenance_completion_facts ON runtime.environment_maintenance_completions; CREATE TRIGGER environment_maintenance_completion_facts BEFORE UPDATE OR DELETE ON runtime.environment_maintenance_completions FOR EACH ROW WHEN(false) EXECUTE FUNCTION runtime.protect_environment_maintenance_completion()", "immutable maintenance completion trigger"),
        ("CREATE OR REPLACE FUNCTION runtime.protect_environment_maintenance_completion() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END; $$", "immutable maintenance completion trigger"),
    ] {
        let (url, mut db) = setup();
        db.batch_execute(change).unwrap();
        let before = history(&mut db);
        assert!(PostgresRuntimeStore::new(&url).unwrap_err().contains(expected), "{change}");
        assert_eq!(history(&mut db), before);
    }
}

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_maintenance_completion_v11_rejects_unexpected_table_without_schema_writes() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (url, mut db) = setup();
    downgrade_to_eleven(&mut db);
    db.batch_execute("CREATE TABLE runtime.environment_maintenance_completions(unexpected text); INSERT INTO runtime.environment_maintenance_completions VALUES('preserved');").unwrap();
    let before = history(&mut db);
    assert!(PostgresRuntimeStore::new(&url).is_err());
    assert_eq!(history(&mut db), before);
    assert_eq!(
        db.query_one(
            "SELECT unexpected FROM runtime.environment_maintenance_completions",
            &[]
        )
        .unwrap()
        .get::<_, String>(0),
        "preserved"
    );
}
