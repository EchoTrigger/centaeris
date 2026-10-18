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

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_environment_owner_accepts_maintenance_origin_without_a_fake_run() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, mut db) = setup();
    db.batch_execute("INSERT INTO runtime.execution_instances(execution_id,instance_id,environment_id,session_id,agent_run_id,host_id,authorization_digest,binding_digest,image_digest,allocation_execution_id,allocation_mode,profile_digest,allocation_owner_json) VALUES('provision','instance','environment','session',NULL,'host','grant','binding','image','provision','retained','sha256:'||repeat('a',64),'{\"kind\":\"maintenanceOperation\",\"operationId\":\"provision\"}');
        INSERT INTO runtime.resident_sandboxes(host_id,execution_id,agent_run_id,workspace_id,container_id,state,memory_bytes,cpu_milli,pids,workspace_bytes,created_at_ms,updated_at_ms,instance_id,environment_id,allocation_mode,allocation_owner_json) VALUES('host','provision',NULL,'workspace','physical','resident',1,1,1,1,0,0,'instance','environment','retained','{\"kind\":\"maintenanceOperation\",\"operationId\":\"provision\"}');").unwrap();
    assert!(db.query_one("SELECT agent_run_id IS NULL FROM runtime.execution_instances WHERE execution_id='provision'",&[]).unwrap().get::<_,bool>(0));
    let facts: String = db
        .query_one(
            "SELECT row_to_json(i)::text FROM runtime.execution_instances i",
            &[],
        )
        .unwrap()
        .get(0);
    assert!(db.batch_execute("UPDATE runtime.execution_instances SET allocation_owner_json='{\"kind\":\"agentRun\",\"runId\":\"forged\"}'").is_err());
    drop(PostgresRuntimeStore::new(&url).unwrap());
    assert_eq!(
        db.query_one(
            "SELECT row_to_json(i)::text FROM runtime.execution_instances i",
            &[]
        )
        .unwrap()
        .get::<_, String>(0),
        facts
    );
}

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_environment_provision_receipt_is_immutable_and_reopen_checks_authority_facts() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, mut db) = setup();
    db.batch_execute("INSERT INTO runtime.environment_provision_completions(operation_id,environment_id,session_id,workspace_id,host_id,instance_id,container_id,profile_digest,controller_epoch,controller_lease_digest,request_digest,completion_json,recorded_at_ms) VALUES('provision','environment','session','workspace','host','instance','physical','profile',1,'lease','request','{\"kind\":\"provisioned\"}',123)").unwrap();
    let before: String = db
        .query_one(
            "SELECT row_to_json(c)::text FROM runtime.environment_provision_completions c",
            &[],
        )
        .unwrap()
        .get(0);
    for sql in [
        "UPDATE runtime.environment_provision_completions SET container_id='replacement'",
        "UPDATE runtime.environment_provision_completions SET operation_id=operation_id",
        "DELETE FROM runtime.environment_provision_completions",
    ] {
        assert_eq!(
            db.batch_execute(sql).unwrap_err().code(),
            Some(&SqlState::RAISE_EXCEPTION)
        );
    }
    drop(PostgresRuntimeStore::new(&url).unwrap());
    assert_eq!(
        db.query_one(
            "SELECT row_to_json(c)::text FROM runtime.environment_provision_completions c",
            &[]
        )
        .unwrap()
        .get::<_, String>(0),
        before
    );
    db.batch_execute("ALTER TABLE runtime.environment_provision_completions DISABLE TRIGGER environment_provision_completion_facts").unwrap();
    assert!(PostgresRuntimeStore::new(&url).is_err());
}

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_environment_owner_v12_upgrade_preserves_run_facts_and_migration_history() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, mut db) = setup();
    super::super::environment_owner_schema::restore_previous_fixture(&mut db);
    db.batch_execute("INSERT INTO runtime.agent_run_cleanup_receipts(receipt_id,agent_run_id,environment_id,authorization_digest,controller_epoch,allocation_kind,instance_id,cleanup_complete,file_protection,recorded_at_ms) VALUES('historical-cleanup','real-run','environment','grant',1,'instance','instance',true,'deferred',123)").unwrap();
    let cleanup_before: serde_json::Value = db
        .query_one(
            "SELECT to_jsonb(r)::text FROM runtime.agent_run_cleanup_receipts r",
            &[],
        )
        .unwrap()
        .get::<_, String>(0)
        .parse()
        .unwrap();
    db.batch_execute("INSERT INTO runtime.execution_instances(execution_id,instance_id,environment_id,session_id,agent_run_id,host_id,authorization_digest,binding_digest,image_digest,allocation_execution_id,allocation_mode,profile_digest) VALUES('original','instance','environment','session','real-run','host','grant','binding','image','original','retained','sha256:'||repeat('a',64));
        INSERT INTO runtime.resident_sandboxes(host_id,execution_id,agent_run_id,workspace_id,container_id,state,memory_bytes,cpu_milli,pids,workspace_bytes,created_at_ms,updated_at_ms,instance_id,environment_id,allocation_mode) VALUES('host','original','real-run','workspace','physical','resident',1,1,1,1,123,456,'instance','environment','retained');").unwrap();
    let before: serde_json::Value = db
        .query_one(
            "SELECT to_jsonb(i)::text FROM runtime.execution_instances i",
            &[],
        )
        .unwrap()
        .get::<_, String>(0)
        .parse()
        .unwrap();
    let history: Vec<(i64, i64)> = db
        .query(
            "SELECT version,applied_at_ms FROM runtime.schema_migrations ORDER BY version",
            &[],
        )
        .unwrap()
        .iter()
        .map(|r| (r.get(0), r.get(1)))
        .collect();
    drop(PostgresRuntimeStore::new(&url).unwrap());
    let after: serde_json::Value = db
        .query_one(
            "SELECT (to_jsonb(i)-'allocation_owner_json')::text FROM runtime.execution_instances i",
            &[],
        )
        .unwrap()
        .get::<_, String>(0)
        .parse()
        .unwrap();
    assert_eq!(after, before);
    let owner: String = db
        .query_one(
            "SELECT allocation_owner_json FROM runtime.execution_instances",
            &[],
        )
        .unwrap()
        .get(0);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&owner).unwrap(),
        serde_json::json!({"kind":"agentRun","runId":"real-run"})
    );
    assert_eq!(
        db.query_one("SELECT agent_run_id FROM runtime.resident_sandboxes", &[])
            .unwrap()
            .get::<_, String>(0),
        "real-run"
    );
    let new_history: Vec<(i64, i64)> = db
        .query(
            "SELECT version,applied_at_ms FROM runtime.schema_migrations ORDER BY version",
            &[],
        )
        .unwrap()
        .iter()
        .map(|r| (r.get(0), r.get(1)))
        .collect();
    assert_eq!(&new_history[..12], history.as_slice());
    assert_eq!(new_history[12].0, 13);
    let cleanup_after:serde_json::Value=db.query_one("SELECT (to_jsonb(r)-'physical_removed'-'removed_container_id')::text FROM runtime.agent_run_cleanup_receipts r",&[]).unwrap().get::<_,String>(0).parse().unwrap();
    assert_eq!(cleanup_after, cleanup_before);
    assert!(db.query_one("SELECT NOT physical_removed AND removed_container_id IS NULL FROM runtime.agent_run_cleanup_receipts",&[]).unwrap().get::<_,bool>(0));
    for statement in ["UPDATE runtime.agent_run_cleanup_receipts SET physical_removed=true,removed_container_id=repeat('a',64)","DELETE FROM runtime.agent_run_cleanup_receipts"]{assert_eq!(db.batch_execute(statement).unwrap_err().code(),Some(&SqlState::RAISE_EXCEPTION));}
    for (removed, container) in [
        (true, None),
        (true, Some("named-container")),
        (false, Some("a")),
    ] {
        assert_eq!(db.execute("INSERT INTO runtime.agent_run_cleanup_receipts(receipt_id,agent_run_id,environment_id,authorization_digest,controller_epoch,allocation_kind,instance_id,cleanup_complete,file_protection,recorded_at_ms,physical_removed,removed_container_id) VALUES('invalid','invalid','environment','grant',1,'instance','instance',true,'deferred',123,$1,$2)",&[&removed,&container]).unwrap_err().code(),Some(&SqlState::CHECK_VIOLATION));
    }
    db.batch_execute("ALTER TABLE runtime.agent_run_cleanup_receipts DROP CONSTRAINT cleanup_physical_removal_facts").unwrap();
    assert!(PostgresRuntimeStore::new(&url).is_err());
}
