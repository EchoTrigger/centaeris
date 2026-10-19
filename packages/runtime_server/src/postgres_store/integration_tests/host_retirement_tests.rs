use super::{retained_instance_tests as fixture, PostgresRuntimeStore, TEST_LOCK};
use crate::contract::AgentRunCleanup;
use crate::postgres_store::cleanup_receipts::{CleanupAuthorityRequest, CleanupPhysicalResult};
use crate::postgres_store::residency::retirement::RetirementRequest;
use hosted_execution::host_supervisor::HostLaunchTarget;
use postgres::Client;
use std::sync::{Arc, Mutex};

type PhysicalInventory = Arc<Mutex<Vec<crate::resident_capacity::ObservedResidentSandbox>>>;

fn queued_targets(db: &mut Client) -> Vec<HostLaunchTarget> {
    db.query("SELECT payload_ref FROM runtime.runtime_jobs WHERE job_kind LIKE 'environment.host_retire:%' AND status='queued' ORDER BY job_id", &[])
        .unwrap().iter().map(|row| serde_json::from_str(&row.get::<_,String>(0)).unwrap()).collect()
}

fn canonical_allocation(
    store: &PostgresRuntimeStore,
    db: &mut Client,
    run: &fixture::Run,
) -> (
    crate::postgres_store::execution_instances::RetainedInstanceAllocation,
    PhysicalInventory,
    HostLaunchTarget,
) {
    let physical = Arc::new(Mutex::new(Vec::new()));
    let mut allocation =
        fixture::prepare(store, run, "host-retirement-allocation", &physical).unwrap();
    let id = "a".repeat(64);
    db.execute(
        "UPDATE runtime.resident_sandboxes SET container_id=$1 WHERE execution_id=$2",
        &[&id, &allocation.allocation_execution_id],
    )
    .unwrap();
    physical.lock().unwrap()[0].container_id = id.clone();
    allocation.container_id = Some(id.clone());
    let target = HostLaunchTarget {
        environment_instance_id: allocation.instance_id.clone(),
        container_id: id,
        environment_id: allocation.environment_id.clone(),
        profile_digest: allocation.profile_digest.clone(),
        image_digest: allocation.image_digest.clone(),
    };
    (allocation, physical, target)
}

fn completed_retirement() -> (String, PostgresRuntimeStore, Client, HostLaunchTarget) {
    let (url, store, mut db) = fixture::setup();
    let run = fixture::run(&store, &mut db, "host-retirement-run", 1);
    let (allocation, physical, target) = canonical_allocation(&store, &mut db, &run);
    let request = RetirementRequest {
        schema: "runtime.environment.retire.v1".into(),
        operation_id: "host-retirement-operation".into(),
        environment_id: allocation.environment_id.clone(),
        instance_id: allocation.instance_id.clone(),
        host_id: "host".into(),
        expected_controller_epoch: 1,
        acknowledge_tmpfs_loss: true,
    };
    assert_eq!(
        store
            .retire_retained_instance(&request, |_| {
                physical.lock().unwrap().clear();
                Ok(true)
            })
            .unwrap()
            .status,
        "destroyed"
    );
    (url, store, db, target)
}

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_host_retirement_retries_lost_ack_and_recovers_expired_or_terminal_jobs() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db, expected) = completed_retirement();
    let mut native_state_present = true;
    assert_eq!(
        store
            .process_host_retirements("other-host", |_| panic!("foreign Host cannot claim"))
            .unwrap(),
        0
    );
    assert_eq!(
        store
            .process_host_retirements("host", |target| {
                assert_eq!(target, expected);
                native_state_present = false;
                Err("lost ACK with secret response content".into())
            })
            .unwrap(),
        1
    );
    let row = db.query_one("SELECT status,last_error FROM runtime.runtime_jobs WHERE job_kind LIKE 'environment.host_retire:%'", &[]).unwrap();
    assert_eq!(row.get::<_, String>(0), "queued");
    assert_eq!(
        row.get::<_, String>(1),
        "environment_host_retirement_pending"
    );
    // A crash after physical success but before recording ACK leaves an expired
    // lease; even generic dead-lettering cannot discard this cleanup obligation.
    drop(store);
    let reopened = PostgresRuntimeStore::new(&url).unwrap();
    for state in ["running", "failed", "cancelled", "dead_lettered"] {
        db.execute("UPDATE runtime.runtime_jobs SET status=$1,lease_owner='crashed',lease_expires_at_ms=0 WHERE job_kind LIKE 'environment.host_retire:%'", &[&state]).unwrap();
        assert_eq!(
            reopened
                .process_host_retirements("host", |target| {
                    assert_eq!(target, expected);
                    assert!(
                        !native_state_present,
                        "retry observes already absent native state"
                    );
                    Err("host_retirement_busy".into())
                })
                .unwrap(),
            1
        );
    }
    db.batch_execute("UPDATE runtime.runtime_jobs SET run_at_ms=0 WHERE job_kind LIKE 'environment.host_retire:%'").unwrap();
    assert_eq!(
        reopened
            .process_host_retirements("host", |target| {
                assert_eq!(target, expected);
                Ok(())
            })
            .unwrap(),
        1
    );
    assert_eq!(
        reopened
            .process_host_retirements("host", |_| panic!("acknowledged work must not repeat"))
            .unwrap(),
        0
    );
    assert_eq!(db.query_one("SELECT status FROM runtime.runtime_jobs WHERE job_kind LIKE 'environment.host_retire:%'", &[]).unwrap().get::<_,String>(0), "succeeded");
}

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_host_retirement_backfill_deduplicates_and_rechecks_exact_authoritative_target() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db, expected) = completed_retirement();
    // Reconstruct an older completed fact without a job; no schema upgrade or
    // historical mutation is needed, and subsequent startups remain idempotent.
    db.batch_execute(
        "DELETE FROM runtime.runtime_jobs WHERE job_kind LIKE 'environment.host_retire:%'",
    )
    .unwrap();
    drop(store);
    let reopened = PostgresRuntimeStore::new(&url).unwrap();
    assert!(reopened
        .backfill_host_retirements("foreign-host", "")
        .unwrap()
        .is_none());
    assert!(queued_targets(&mut db).is_empty());
    for _ in 0..2 {
        assert!(reopened
            .backfill_host_retirements("host", "")
            .unwrap()
            .is_none());
    }
    assert_eq!(queued_targets(&mut db), vec![expected.clone()]);
    let mut forged = expected.clone();
    forged.container_id = "f".repeat(64);
    db.execute("UPDATE runtime.runtime_jobs SET payload_ref=$1 WHERE job_kind LIKE 'environment.host_retire:%'", &[&serde_json::to_string(&forged).unwrap()]).unwrap();
    assert!(reopened
        .backfill_host_retirements("host", "")
        .unwrap()
        .is_none());
    assert_eq!(
        queued_targets(&mut db),
        vec![forged],
        "backfill cannot replace an already fixed target"
    );
    assert_eq!(
        reopened
            .process_host_retirements("host", |_| panic!(
                "job payload is not physical-removal authority"
            ))
            .unwrap(),
        1
    );
    // An unresolved historical obligation is fixed once, under the current job
    // lease, only after the committed physical facts prove the exact target.
    db.batch_execute("UPDATE runtime.runtime_jobs SET payload_ref=NULL,run_at_ms=0 WHERE job_kind LIKE 'environment.host_retire:%'").unwrap();
    assert_eq!(
        reopened
            .process_host_retirements("host", |target| {
                assert_eq!(target, expected);
                Ok(())
            })
            .unwrap(),
        1
    );
    assert_eq!(db.query_one("SELECT payload_ref FROM runtime.runtime_jobs WHERE job_kind LIKE 'environment.host_retire:%'", &[]).unwrap().get::<_,String>(0), serde_json::to_string(&expected).unwrap());
}

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_host_retirement_startup_scan_is_bounded_and_never_retires_unremoved_instances() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_, store, mut db) = fixture::setup();
    for n in 0..65 {
        let execution = format!("anchor-{n}");
        let instance = format!("instance_{n:064x}");
        db.execute("INSERT INTO runtime.execution_instances(execution_id,instance_id,environment_id,session_id,agent_run_id,host_id,authorization_digest,binding_digest,image_digest,allocation_execution_id,allocation_mode,profile_digest,allocation_owner_json) VALUES($1,$2,'environment','sess_1',NULL,'host','grant','binding','sha256:'||repeat('a',64),$1,'retained','sha256:'||repeat('b',64),'{\"kind\":\"maintenanceOperation\",\"operationId\":\"provision\"}')", &[&execution,&instance]).unwrap();
    }
    let cursor = store
        .backfill_host_retirements("host", "")
        .unwrap()
        .expect("first page must stop at 64 anchors");
    assert_eq!(cursor, format!("instance_{:064x}", 63));
    assert!(store
        .backfill_host_retirements("host", &cursor)
        .unwrap()
        .is_none());
    assert!(
        queued_targets(&mut db).is_empty(),
        "an anchor is not proof of physical removal"
    );
    assert_eq!(
        store
            .process_host_retirements("host", |_| panic!("no removal authority"))
            .unwrap(),
        0
    );
    for n in 0..6 {
        let instance = format!("instance_{n:064x}");
        let id = format!("historical-{n}");
        db.execute("INSERT INTO runtime.agent_run_cleanup_receipts(receipt_id,agent_run_id,environment_id,authorization_digest,controller_epoch,allocation_kind,instance_id,cleanup_complete,file_protection,recorded_at_ms,physical_removed,removed_container_id) VALUES($1,$1,'environment','grant',1,'instance',$2,true,'deferred',0,true,repeat('c',64))", &[&id,&instance]).unwrap();
    }
    assert!(store
        .backfill_host_retirements("host", "")
        .unwrap()
        .is_some());
    assert_eq!(
        store.process_host_retirements("host", |_| Ok(())).unwrap(),
        4,
        "each pass is bounded independently of the backlog"
    );
    assert_eq!(queued_targets(&mut db).len(), 2);
    assert_eq!(
        store.process_host_retirements("host", |_| Ok(())).unwrap(),
        2
    );
}

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_host_retirement_bad_historical_fact_cannot_starve_other_completed_instances() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db, expected) = completed_retirement();
    db.batch_execute("DELETE FROM runtime.runtime_jobs WHERE job_kind LIKE 'environment.host_retire:%';
        INSERT INTO runtime.execution_instances(execution_id,instance_id,environment_id,session_id,agent_run_id,host_id,authorization_digest,binding_digest,image_digest,allocation_execution_id,allocation_mode,profile_digest,allocation_owner_json) VALUES('bad-anchor','instance_'||repeat('0',64),'bad-environment','sess_1',NULL,'host','grant','binding','sha256:'||repeat('a',64),'bad-anchor','retained','sha256:'||repeat('b',64),'{\"kind\":\"maintenanceOperation\",\"operationId\":\"provision\"}');
        INSERT INTO runtime.agent_run_cleanup_receipts(receipt_id,agent_run_id,environment_id,authorization_digest,controller_epoch,allocation_kind,instance_id,cleanup_complete,file_protection,recorded_at_ms,physical_removed,removed_container_id) SELECT 'bad-receipt-'||n,'bad-run-'||n,'bad-environment','grant',1,'instance','instance_'||repeat('0',64),true,'deferred',0,true,repeat(n::text,64) FROM generate_series(1,2) n;").unwrap();
    drop(store);
    let reopened = PostgresRuntimeStore::new(&url).unwrap();
    assert!(
        reopened
            .backfill_host_retirements("host", "")
            .unwrap()
            .is_none(),
        "one conflicting anchor must not roll back the good completion behind it"
    );
    let mut dispatched = vec![];
    assert_eq!(
        reopened
            .process_host_retirements("host", |target| {
                dispatched.push(target);
                Ok(())
            })
            .unwrap(),
        2
    );
    assert_eq!(dispatched, vec![expected]);
    let pending = db.query_one("SELECT status,last_error FROM runtime.runtime_jobs WHERE idempotency_key='instance_'||repeat('0',64)", &[]).unwrap();
    assert_eq!(
        pending.get::<_, String>(0),
        "queued",
        "conflicting history remains a visible retry obligation"
    );
    assert_eq!(
        pending.get::<_, String>(1),
        "environment_host_retirement_pending"
    );
    assert!(reopened
        .backfill_host_retirements("host", "")
        .unwrap()
        .is_none());
}

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_per_run_physical_cleanup_durably_schedules_exact_host_retirement() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db) = fixture::setup();
    let mut run = fixture::run(&store, &mut db, "host-retirement-run", 1);
    run.grant.payload.execution_mode = "perRun".into();
    run.grant = fixture::sign(run.grant.payload);
    let (allocation, physical, target) = canonical_allocation(&store, &mut db, &run);
    run.grant
        .payload
        .controller
        .as_mut()
        .unwrap()
        .controller_purpose = "cleanup".into();
    run.grant = fixture::sign(run.grant.payload);
    db.batch_execute("UPDATE public.app_core_sessionenvironment SET controller_purpose='cleanup'; INSERT INTO public.app_core_sessionevent VALUES('ws_1','sess_1','host-retirement-run',false,'{\"type\":\"agent_run_failed\"}')").unwrap();
    let cleanup = AgentRunCleanup {
        schema: "workspace.agent_run.cleanup.v1".into(),
        agent_run_id: run.original.agent_run_id.clone(),
        authorization_digest: run.original.digest().unwrap(),
        authorization_signature: run.original.signature(fixture::KEY).unwrap(),
        authorization: run.original.clone(),
        environment_authorization: run.grant.clone(),
    };
    let authority = CleanupAuthorityRequest {
        cleanup: &cleanup,
        signing_key: fixture::KEY,
        lease: &run.fence,
        host_id: "host",
    };
    assert!(queued_targets(&mut db).is_empty());
    store
        .with_cleanup_authority(
            &authority,
            || Ok(physical.lock().unwrap().clone()),
            |_, _| {
                physical.lock().unwrap().clear();
                Ok(CleanupPhysicalResult::Removed {
                    instance_id: allocation.instance_id.clone(),
                    container_id: allocation.container_id.clone().unwrap(),
                })
            },
        )
        .unwrap();
    assert_eq!(
        queued_targets(&mut db),
        vec![target.clone()],
        "a committed physical cleanup must retain its native Host cleanup obligation"
    );
    drop(store);
    let reopened = PostgresRuntimeStore::new(&url).unwrap();
    reopened
        .with_cleanup_authority(
            &authority,
            || Err("inventory unavailable after commit".into()),
            |_, _| Err::<(), _>("must not remove twice".into()),
        )
        .unwrap();
    assert_eq!(
        queued_targets(&mut db),
        vec![target],
        "receipt replay and restart must not forget or duplicate the obligation"
    );
}

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_permanent_retirement_durably_schedules_the_same_host_cleanup_path() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db) = fixture::setup();
    let run = fixture::run(&store, &mut db, "host-retirement-run", 1);
    let (allocation, physical, target) = canonical_allocation(&store, &mut db, &run);
    let request = RetirementRequest {
        schema: "runtime.environment.retire.v1".into(),
        operation_id: "host-retirement-operation".into(),
        environment_id: allocation.environment_id.clone(),
        instance_id: allocation.instance_id.clone(),
        host_id: "host".into(),
        expected_controller_epoch: 1,
        acknowledge_tmpfs_loss: true,
    };
    assert_eq!(
        store
            .retire_retained_instance(&request, |_| Err("unknown Docker outcome".into()))
            .unwrap()
            .status,
        "pending"
    );
    assert!(
        queued_targets(&mut db).is_empty(),
        "an unconfirmed physical deletion cannot authorize Host GC"
    );
    assert_eq!(
        store
            .retire_retained_instance(&request, |_| {
                physical.lock().unwrap().clear();
                Ok(true)
            })
            .unwrap()
            .status,
        "destroyed"
    );
    assert_eq!(
        queued_targets(&mut db),
        vec![target.clone()],
        "permanent retirement uses the same durable Host obligation as perRun cleanup"
    );
    drop(store);
    let reopened = PostgresRuntimeStore::new(&url).unwrap();
    reopened
        .retire_retained_instance(&request, |_| {
            panic!("completed retirement must not repeat Docker deletion")
        })
        .unwrap();
    assert_eq!(queued_targets(&mut db), vec![target]);
}
