use super::*;
use crate::postgres_store::integration_tests::{retained_instance_tests as fixture, TEST_LOCK};
use crate::resident_capacity::SharedHostInventory;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

fn residents(count: usize) -> (PostgresRuntimeStore, Client, Vec<ObservedResidentSandbox>) {
    let (_, store, mut db) = fixture::setup();
    let mut inventory = Vec::with_capacity(count);
    for index in 0..count {
        let id = format!("cost-run-{index}");
        let job_id = agent_run_lifecycle_job_id(&id).unwrap();
        let backoff = serde_json::to_string(
            &centaeris_core::session::reliability::RuntimeBackoffPolicy::default(),
        )
        .unwrap();
        db.execute("INSERT INTO runtime.runtime_jobs(job_id,job_kind,status,run_at_ms,backoff_policy_json,idempotency_key,created_at_ms,updated_at_ms) VALUES($1,'contract.test','queued',1,$2,$1,1,1)", &[&job_id,&backoff]).unwrap();
        db.execute(
            "INSERT INTO runtime.execution_job_tenants VALUES($1,'ws_1')",
            &[&job_id],
        )
        .unwrap();
        let resources = ResidentResourceUsage {
            sandbox_count: 1,
            memory_bytes: 1024 * 1024,
            cpu_milli: 1000,
            pids: 32,
            workspace_bytes: 1024 * 1024,
        };
        let execution_id = format!("cost-execution-{index}");
        let container_id = format!("cost-container-{index}");
        let allocation_owner =
            centaeris_core::execution::EnvironmentTaskOwner::AgentRun { run_id: id.clone() };
        db.execute("INSERT INTO runtime.resident_sandboxes(host_id,execution_id,agent_run_id,workspace_id,container_id,state,memory_bytes,cpu_milli,pids,workspace_bytes,created_at_ms,updated_at_ms,allocation_owner_json) VALUES('host',$1,$2,'ws_1',$3,'resident',$4,$5,$6,$7,1,1,$8)", &[&execution_id,&id,&container_id,&(resources.memory_bytes as i64),&(resources.cpu_milli as i64),&(resources.pids as i64),&(resources.workspace_bytes as i64),&serde_json::to_string(&allocation_owner).unwrap()]).unwrap();
        inventory.push(ObservedResidentSandbox {
            execution_id,
            allocation_owner,
            container_id,
            resources,
        });
    }
    (store, db, inventory)
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_reconciliation_preserves_matching_rows_and_rejects_mismatched_inventory() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (store, mut db, inventory) = residents(4);
    store
        .reconcile_resident_sandboxes("host", || Ok(inventory.clone()))
        .unwrap();
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.resident_sandboxes WHERE updated_at_ms=1",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        4
    );
    let mut changed = inventory.clone();
    changed[2].container_id = "replaced-container".into();
    assert!(store
        .reconcile_resident_sandboxes("host", || Ok(changed))
        .unwrap_err()
        .contains("disagrees with reservation"));
    db.execute(
        "DELETE FROM runtime.execution_job_tenants WHERE job_id='agent_run.lifecycle:cost-run-2'",
        &[],
    )
    .unwrap();
    assert!(store
        .reconcile_resident_sandboxes("host", || Ok(inventory))
        .unwrap_err()
        .contains("unknown tenant"));
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.resident_sandboxes WHERE updated_at_ms=1",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        4
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_reconciliation_reuses_inserted_declarations_within_the_inventory() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (store, mut db, mut inventory) = residents(4);
    db.execute(
        "DELETE FROM runtime.resident_sandboxes WHERE execution_id='cost-execution-0'",
        &[],
    )
    .unwrap();
    inventory.push(inventory[0].clone());
    store
        .reconcile_resident_sandboxes("host", || Ok(inventory))
        .unwrap();
    assert_eq!(
        db.query_one("SELECT count(*) FROM runtime.resident_sandboxes", &[])
            .unwrap()
            .get::<_, i64>(0),
        4
    );
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.resident_sandboxes WHERE updated_at_ms=1",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        3
    );
}

/// Run the same ignored probe before and after changes against an owned database.
/// Query counts include the complete reconciliation gate/transaction/wake path.
#[test]
#[ignore = "timing probe: requires owned Postgres with pg_stat_statements"]
fn resident_processing_timings() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    for count in [4, 8, 32, 64] {
        let (store, mut db, inventory) = residents(count);
        db.batch_execute("CREATE EXTENSION IF NOT EXISTS pg_stat_statements")
            .unwrap();
        let snapshot = ResidentInventorySnapshot::freeze(&mut db, "host", None).unwrap();
        let sample = SharedHostInventory {
            host_id: "host".into(),
            available_memory_bytes: u64::MAX / 2,
            memory_headroom_bytes: 1,
            valid_until: Instant::now() + Duration::from_secs(300),
            measured_by_container: BTreeMap::new(),
        };
        for _ in 0..2 {
            store
                .reconcile_resident_sandboxes("host", || Ok(inventory.clone()))
                .unwrap();
        }
        db.query_one("SELECT pg_stat_statements_reset()", &[])
            .unwrap();
        let mut timings = Vec::new();
        for _ in 0..15 {
            let started = Instant::now();
            store
                .reconcile_resident_sandboxes("host", || Ok(inventory.clone()))
                .unwrap();
            timings.push(started.elapsed().as_secs_f64() * 1_000_000.0);
        }
        let calls: i64 = db.query_one("SELECT COALESCE(sum(calls),0)::bigint FROM pg_stat_statements WHERE dbid=(SELECT oid FROM pg_database WHERE datname=current_database()) AND query NOT LIKE '%pg_stat_statements%'", &[]).unwrap().get(0);
        timings.sort_by(f64::total_cmp);
        println!(
            "resident_reconcile n={count} p50_us={:.3} p95_us={:.3} sql_calls_per_operation={:.1}",
            timings[7],
            timings[14],
            calls as f64 / 15.0
        );
        let mut usage_timings = Vec::new();
        for _ in 0..15 {
            let started = Instant::now();
            std::hint::black_box(used_shared(&mut db, "host", "ws_1", &sample, &snapshot).unwrap());
            usage_timings.push(started.elapsed().as_secs_f64() * 1_000_000.0);
        }
        usage_timings.sort_by(f64::total_cmp);
        println!(
            "resident_usage n={count} p50_us={:.3} p95_us={:.3}",
            usage_timings[7], usage_timings[14]
        );
    }
}
