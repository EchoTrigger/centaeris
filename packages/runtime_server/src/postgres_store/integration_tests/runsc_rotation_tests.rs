use super::retained_instance_tests as fixture;
use super::*;
use crate::postgres_store::runsc_rotation::{self, RunscBinding, RUNSC_CAPACITY_WAIT};
use hosted_execution::host_supervisor::IdlePauseOutcome;
use std::sync::{Arc, Mutex};

fn retained() -> (
    String,
    PostgresRuntimeStore,
    Client,
    fixture::Run,
    runsc_rotation::RunscBinding,
) {
    let (url, store, mut db) = fixture::setup();
    let run = fixture::run(&store, &mut db, "rotation-run", 1);
    let physical = Arc::new(Mutex::new(Vec::new()));
    let allocation = fixture::prepare(&store, &run, "rotation-execution", &physical).unwrap();
    let binding = RunscBinding::from(&allocation);
    (url, store, db, run, binding)
}

#[test]
#[ignore = "requires explicitly owned disposable PostgreSQL database"]
fn postgres_runsc_completed_pause_releases_only_running_capacity() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_url, store, mut db, run, binding) = retained();
    let resident = db
        .query_one(
            "SELECT row_to_json(s)::text FROM runtime.resident_sandboxes s",
            &[],
        )
        .unwrap()
        .get::<_, String>(0);
    db.execute(
        "UPDATE public.app_core_agentrun SET status='completed' WHERE id=$1",
        &[&run.original.agent_run_id],
    )
    .unwrap();
    db.batch_execute("UPDATE public.app_core_sessionenvironment SET controller_released=true")
        .unwrap();
    assert!(store
        .runsc_idle_candidates("host", None, 1)
        .unwrap()
        .candidates
        .is_empty());
    db.batch_execute("UPDATE runtime.runsc_environment_power SET idle_since_ms=0")
        .unwrap();
    let candidate = store
        .runsc_idle_candidates("host", None, 1)
        .unwrap()
        .candidates
        .remove(0);
    store
        .with_runsc_idle_pause(&candidate, |allocation, profile| {
            assert_eq!(allocation.container_id, binding.container_id);
            assert!(profile.validate().is_ok());
            Ok(IdlePauseOutcome::Paused)
        })
        .unwrap();
    assert_eq!(
        db.query_one("SELECT state FROM runtime.runsc_environment_power", &[])
            .unwrap()
            .get::<_, String>(0),
        "paused"
    );
    assert_eq!(
        db.query_one(
            "SELECT row_to_json(s)::text FROM runtime.resident_sandboxes s",
            &[]
        )
        .unwrap()
        .get::<_, String>(0),
        resident
    );
    let generation = db
        .query_one(
            "SELECT generation FROM runtime.resident_capacity_clock",
            &[],
        )
        .unwrap()
        .get::<_, i64>(0);
    assert_eq!(generation, 1);
    let mut tx = db.transaction().unwrap();
    runsc_rotation::reserve_running(&mut tx, &binding, 1).unwrap();
    assert_eq!(
        tx.query_one("SELECT state FROM runtime.runsc_environment_power", &[])
            .unwrap()
            .get::<_, String>(0),
        "resumePrepared"
    );
    tx.rollback().unwrap();
}

#[test]
#[ignore = "requires explicitly owned disposable PostgreSQL database"]
fn postgres_runsc_unknown_pause_keeps_running_charge_and_is_reconcilable() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_url, store, mut db, run, binding) = retained();
    db.execute(
        "UPDATE public.app_core_agentrun SET status='completed' WHERE id=$1",
        &[&run.original.agent_run_id],
    )
    .unwrap();
    db.batch_execute("UPDATE public.app_core_sessionenvironment SET controller_released=true")
        .unwrap();
    store.runsc_idle_candidates("host", None, 1).unwrap();
    db.batch_execute("UPDATE runtime.runsc_environment_power SET idle_since_ms=0")
        .unwrap();
    let candidate = store
        .runsc_idle_candidates("host", None, 1)
        .unwrap()
        .candidates
        .remove(0);
    assert!(store
        .with_runsc_idle_pause(&candidate, |_, _| Err("lost pause acknowledgement".into()))
        .is_err());
    assert_eq!(
        db.query_one("SELECT state FROM runtime.runsc_environment_power", &[])
            .unwrap()
            .get::<_, String>(0),
        "pauseDispatched"
    );
    assert_eq!(runsc_rotation::running_count(&mut db, "host").unwrap(), 1);
    let reopened = PostgresRuntimeStore::new(&_url).unwrap();
    let candidate = reopened
        .runsc_idle_candidates("host", None, 1)
        .unwrap()
        .candidates
        .remove(0);
    reopened
        .with_runsc_idle_pause(&candidate, |_, _| Ok(IdlePauseOutcome::Paused))
        .unwrap();
    assert_eq!(runsc_rotation::running_count(&mut db, "host").unwrap(), 0);
    let mut tx = db.transaction().unwrap();
    runsc_rotation::reserve_running(&mut tx, &binding, 1).unwrap();
    runsc_rotation::observe_running(&mut tx, &binding, binding.container_id.as_deref().unwrap())
        .unwrap();
    tx.commit().unwrap();
    assert_eq!(runsc_rotation::running_count(&mut db, "host").unwrap(), 1);
}

#[test]
#[ignore = "requires explicitly owned disposable PostgreSQL database"]
fn postgres_runsc_pause_rechecks_controller_and_demand_before_dispatch() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_url, store, mut db, run, _binding) = retained();
    assert!(store
        .runsc_idle_candidates("host", None, 1)
        .unwrap()
        .candidates
        .is_empty());
    db.execute(
        "UPDATE public.app_core_agentrun SET status='completed' WHERE id=$1",
        &[&run.original.agent_run_id],
    )
    .unwrap();
    db.batch_execute("UPDATE public.app_core_sessionenvironment SET controller_released=true")
        .unwrap();
    store.runsc_idle_candidates("host", None, 1).unwrap();
    db.batch_execute("UPDATE runtime.runsc_environment_power SET idle_since_ms=0")
        .unwrap();
    let candidate = store
        .runsc_idle_candidates("host", None, 1)
        .unwrap()
        .candidates
        .remove(0);
    db.batch_execute("UPDATE public.app_core_sessionenvironment SET controller_released=false,controller_epoch=2").unwrap();
    assert!(store
        .with_runsc_idle_pause(&candidate, |_, _| panic!("stale pause dispatched"))
        .is_err());
    db.batch_execute("UPDATE public.app_core_sessionenvironment SET controller_released=true,controller_epoch=1; UPDATE public.app_core_agentrun SET status='queued'").unwrap();
    assert!(store
        .with_runsc_idle_pause(&candidate, |_, _| panic!("queued demand was paused"))
        .is_err());
}

#[test]
#[ignore = "requires explicitly owned disposable PostgreSQL database"]
fn postgres_runsc_unregistered_residency_and_uncertain_resume_are_charged() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_url, _store, mut db, _run, binding) = retained();
    assert_eq!(runsc_rotation::running_count(&mut db, "host").unwrap(), 1);
    let other = RunscBinding {
        environment_id: "other".into(),
        allocation_execution_id: "other-allocation".into(),
        instance_id: "other-instance".into(),
        container_id: None,
        host_id: "host".into(),
    };
    let mut tx = db.transaction().unwrap();
    assert_eq!(
        runsc_rotation::reserve_running(&mut tx, &other, 1).unwrap_err(),
        RUNSC_CAPACITY_WAIT
    );
    tx.rollback().unwrap();
    let mut tx = db.transaction().unwrap();
    runsc_rotation::reserve_running(&mut tx, &binding, 1).unwrap();
    tx.commit().unwrap();
    assert_eq!(runsc_rotation::running_count(&mut db, "host").unwrap(), 1);
}

#[test]
#[ignore = "requires explicitly owned disposable PostgreSQL database"]
fn postgres_runsc_forward_migration_preserves_populated_residents() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db, _run, _binding) = retained();
    let resident = db
        .query_one(
            "SELECT row_to_json(s)::text FROM runtime.resident_sandboxes s",
            &[],
        )
        .unwrap()
        .get::<_, String>(0);
    let allocation = db
        .query_one(
            "SELECT row_to_json(i)::text FROM runtime.execution_instances i",
            &[],
        )
        .unwrap()
        .get::<_, String>(0);
    drop(store);
    db.batch_execute("DROP TABLE runtime.runsc_environment_power; DELETE FROM runtime.schema_migrations WHERE version=17").unwrap();
    assert_eq!(
        db.query_one("SELECT max(version) FROM runtime.schema_migrations", &[])
            .unwrap()
            .get::<_, i64>(0),
        16
    );
    let reopened = PostgresRuntimeStore::new(&url).unwrap();
    assert_eq!(
        db.query_one("SELECT max(version) FROM runtime.schema_migrations", &[])
            .unwrap()
            .get::<_, i64>(0),
        17
    );
    assert_eq!(
        db.query_one("SELECT count(*) FROM runtime.runsc_environment_power", &[])
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    assert_eq!(
        db.query_one(
            "SELECT row_to_json(s)::text FROM runtime.resident_sandboxes s",
            &[]
        )
        .unwrap()
        .get::<_, String>(0),
        resident
    );
    assert_eq!(
        db.query_one(
            "SELECT row_to_json(i)::text FROM runtime.execution_instances i",
            &[]
        )
        .unwrap()
        .get::<_, String>(0),
        allocation
    );
    assert_eq!(runsc_rotation::running_count(&mut db, "host").unwrap(), 1);
    drop(reopened);
    db.batch_execute("ALTER TABLE runtime.runsc_environment_power DROP CONSTRAINT runsc_power_state; ALTER TABLE runtime.runsc_environment_power ADD CONSTRAINT runsc_power_state CHECK(state<>'invalid')").unwrap();
    assert!(PostgresRuntimeStore::new(&url)
        .unwrap_err()
        .contains("runsc power constraint mismatch"));
}

#[test]
#[ignore = "requires explicitly owned disposable PostgreSQL database"]
fn postgres_runsc_paused_reconciliation_charges_unknown_and_preserves_memory() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_url, store, mut db, run, _binding) = retained();
    db.execute(
        "UPDATE public.app_core_agentrun SET status='completed' WHERE id=$1",
        &[&run.original.agent_run_id],
    )
    .unwrap();
    db.batch_execute("UPDATE public.app_core_sessionenvironment SET controller_released=true")
        .unwrap();
    store.runsc_idle_candidates("host", None, 1).unwrap();
    db.batch_execute("UPDATE runtime.runsc_environment_power SET idle_since_ms=0")
        .unwrap();
    let candidate = store
        .runsc_idle_candidates("host", None, 1)
        .unwrap()
        .candidates
        .remove(0);
    store
        .with_runsc_idle_pause(&candidate, |_, _| Ok(IdlePauseOutcome::Paused))
        .unwrap();
    let resident = db
        .query_one(
            "SELECT row_to_json(s)::text FROM runtime.resident_sandboxes s",
            &[],
        )
        .unwrap()
        .get::<_, String>(0);
    let page = store
        .reconcile_runsc_paused("host", None, |_, _| Err("unobservable Docker".into()))
        .unwrap();
    assert_eq!(page.unconfirmed, 1);
    assert_eq!(runsc_rotation::running_count(&mut db, "host").unwrap(), 1);
    store
        .reconcile_runsc_paused("host", None, |_, _| Ok(true))
        .unwrap();
    assert_eq!(runsc_rotation::running_count(&mut db, "host").unwrap(), 0);
    store
        .reconcile_runsc_paused("host", None, |_, _| Ok(false))
        .unwrap();
    assert_eq!(runsc_rotation::running_count(&mut db, "host").unwrap(), 1);
    assert_eq!(
        db.query_one("SELECT state FROM runtime.runsc_environment_power", &[])
            .unwrap()
            .get::<_, String>(0),
        "resumePrepared"
    );
    assert_eq!(
        db.query_one(
            "SELECT row_to_json(s)::text FROM runtime.resident_sandboxes s",
            &[]
        )
        .unwrap()
        .get::<_, String>(0),
        resident
    );
}

#[test]
#[ignore = "requires explicitly owned disposable PostgreSQL database"]
fn postgres_runsc_uncertain_pauses_cannot_bypass_single_running_slot_after_restart() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_url, _store, mut db, _run, binding) = retained();
    db.batch_execute("INSERT INTO runtime.execution_instances(execution_id,instance_id,environment_id,session_id,agent_run_id,host_id,authorization_digest,binding_digest,image_digest,allocation_execution_id,allocation_mode,profile_digest,allocation_owner_json) SELECT 'other-allocation','other-instance','other-environment',session_id,agent_run_id,host_id,authorization_digest,binding_digest,image_digest,'other-allocation','retained',profile_digest,allocation_owner_json FROM runtime.execution_instances LIMIT 1; INSERT INTO runtime.resident_sandboxes(host_id,execution_id,agent_run_id,workspace_id,container_id,state,memory_bytes,cpu_milli,pids,workspace_bytes,created_at_ms,updated_at_ms,instance_id,environment_id,allocation_mode,allocation_owner_json) SELECT host_id,'other-allocation',agent_run_id,workspace_id,'other-container',state,memory_bytes,cpu_milli,pids,workspace_bytes,created_at_ms,updated_at_ms,'other-instance','other-environment','retained',allocation_owner_json FROM runtime.resident_sandboxes LIMIT 1; DELETE FROM runtime.runsc_environment_power; INSERT INTO runtime.runsc_environment_power SELECT environment_id,host_id,execution_id,instance_id,container_id,'pauseDispatched',0,NULL,0 FROM runtime.resident_sandboxes;").unwrap();
    assert_eq!(runsc_rotation::running_count(&mut db, "host").unwrap(), 2);
    let mut tx = db.transaction().unwrap();
    assert_eq!(
        runsc_rotation::reserve_running(&mut tx, &binding, 1).unwrap_err(),
        RUNSC_CAPACITY_WAIT
    );
    tx.rollback().unwrap();
    db.batch_execute("UPDATE runtime.runsc_environment_power SET state='paused'")
        .unwrap();
    let other = RunscBinding {
        environment_id: "other-environment".into(),
        allocation_execution_id: "other-allocation".into(),
        instance_id: "other-instance".into(),
        container_id: Some("other-container".into()),
        host_id: "host".into(),
    };
    let mut tx = db.transaction().unwrap();
    runsc_rotation::reserve_running(&mut tx, &binding, 1).unwrap();
    tx.commit().unwrap();
    let mut tx = db.transaction().unwrap();
    assert_eq!(
        runsc_rotation::reserve_running(&mut tx, &other, 1).unwrap_err(),
        RUNSC_CAPACITY_WAIT
    );
    tx.rollback().unwrap();
    assert_eq!(runsc_rotation::running_count(&mut db, "host").unwrap(), 1);
}

#[test]
#[ignore = "requires explicitly owned disposable PostgreSQL database"]
fn postgres_runsc_queued_demand_does_not_block_confirming_unknown_paused_slots() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_url, store, mut db, _run, binding) = retained();
    db.batch_execute("INSERT INTO runtime.execution_instances(execution_id,instance_id,environment_id,session_id,agent_run_id,host_id,authorization_digest,binding_digest,image_digest,allocation_execution_id,allocation_mode,profile_digest,allocation_owner_json) SELECT 'demand-allocation','demand-instance','demand-environment',session_id,agent_run_id,host_id,authorization_digest,binding_digest,image_digest,'demand-allocation','retained',profile_digest,allocation_owner_json FROM runtime.execution_instances LIMIT 1; INSERT INTO runtime.resident_sandboxes(host_id,execution_id,agent_run_id,workspace_id,container_id,state,memory_bytes,cpu_milli,pids,workspace_bytes,created_at_ms,updated_at_ms,instance_id,environment_id,allocation_mode,allocation_owner_json) SELECT host_id,'demand-allocation',agent_run_id,workspace_id,'demand-container',state,memory_bytes,cpu_milli,pids,workspace_bytes,created_at_ms,updated_at_ms,'demand-instance','demand-environment','retained',allocation_owner_json FROM runtime.resident_sandboxes LIMIT 1; INSERT INTO public.app_core_agentrun(id,session_id,environment_id,workspace_id,user_id,membership_ref,status,\"modelConfig_id\",\"thinkingMode\") SELECT 'demand-queued-run',session_id,'demand-environment',workspace_id,user_id,membership_ref,'queued',\"modelConfig_id\",\"thinkingMode\" FROM public.app_core_agentrun LIMIT 1; INSERT INTO public.app_core_sessionenvironment(id,session_id,controller_run_id,controller_epoch,controller_lease_owner,controller_purpose,controller_released,environment_profile,environment_profile_digest) SELECT 'demand-environment',session_id,'demand-queued-run',1,'queued-owner','execute',false,environment_profile,environment_profile_digest FROM public.app_core_sessionenvironment LIMIT 1; DELETE FROM runtime.runsc_environment_power; INSERT INTO runtime.runsc_environment_power SELECT environment_id,host_id,execution_id,instance_id,container_id,'pauseDispatched',1,NULL,0 FROM runtime.resident_sandboxes;").unwrap();
    assert_eq!(runsc_rotation::running_count(&mut db, "host").unwrap(), 2);
    let page = store
        .reconcile_runsc_paused("host", None, |_, _| Ok(true))
        .unwrap();
    assert_eq!(page.unconfirmed, 0);
    assert_eq!(
        runsc_rotation::running_count(&mut db, "host").unwrap(),
        0,
        "physical pause confirmation must not depend on absence of queued demand"
    );
    let mut tx = db.transaction().unwrap();
    runsc_rotation::reserve_running(&mut tx, &binding, 1).unwrap();
    tx.commit().unwrap();
    let other = RunscBinding {
        environment_id: "demand-environment".into(),
        host_id: "host".into(),
        allocation_execution_id: "demand-allocation".into(),
        instance_id: "demand-instance".into(),
        container_id: Some("demand-container".into()),
    };
    let mut tx = db.transaction().unwrap();
    assert_eq!(
        runsc_rotation::reserve_running(&mut tx, &other, 1).unwrap_err(),
        RUNSC_CAPACITY_WAIT
    );
    tx.rollback().unwrap();
}

#[test]
#[ignore = "requires explicitly owned disposable PostgreSQL database"]
fn postgres_runsc_stop_after_one_observation_preserves_unobserved_pause_debits() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_url, store, mut db, _run, _binding) = retained();
    db.batch_execute("INSERT INTO runtime.execution_instances(execution_id,instance_id,environment_id,session_id,agent_run_id,host_id,authorization_digest,binding_digest,image_digest,allocation_execution_id,allocation_mode,profile_digest,allocation_owner_json) SELECT 'demand-allocation','demand-instance','demand-environment',session_id,agent_run_id,host_id,authorization_digest,binding_digest,image_digest,'demand-allocation','retained',profile_digest,allocation_owner_json FROM runtime.execution_instances LIMIT 1; INSERT INTO runtime.resident_sandboxes(host_id,execution_id,agent_run_id,workspace_id,container_id,state,memory_bytes,cpu_milli,pids,workspace_bytes,created_at_ms,updated_at_ms,instance_id,environment_id,allocation_mode,allocation_owner_json) SELECT host_id,'demand-allocation',agent_run_id,workspace_id,'demand-container',state,memory_bytes,cpu_milli,pids,workspace_bytes,created_at_ms,updated_at_ms,'demand-instance','demand-environment','retained',allocation_owner_json FROM runtime.resident_sandboxes LIMIT 1; INSERT INTO public.app_core_agentrun(id,session_id,environment_id,workspace_id,user_id,membership_ref,status,\"modelConfig_id\",\"thinkingMode\") SELECT 'demand-queued-run',session_id,'demand-environment',workspace_id,user_id,membership_ref,'queued',\"modelConfig_id\",\"thinkingMode\" FROM public.app_core_agentrun LIMIT 1; INSERT INTO public.app_core_sessionenvironment(id,session_id,controller_run_id,controller_epoch,controller_lease_owner,controller_purpose,controller_released,environment_profile,environment_profile_digest) SELECT 'demand-environment',session_id,'demand-queued-run',1,'queued-owner','execute',false,environment_profile,environment_profile_digest FROM public.app_core_sessionenvironment LIMIT 1; DELETE FROM runtime.runsc_environment_power; INSERT INTO runtime.runsc_environment_power SELECT environment_id,host_id,execution_id,instance_id,container_id,'pauseDispatched',1,NULL,0 FROM runtime.resident_sandboxes;").unwrap();
    let stopping = std::sync::atomic::AtomicBool::new(false);
    let mut observed = Vec::new();
    store
        .reconcile_runsc_paused_until(
            "host",
            None,
            || stopping.load(std::sync::atomic::Ordering::SeqCst),
            |allocation, _| {
                observed.push(allocation.environment_id.clone());
                stopping.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(true)
            },
        )
        .unwrap();
    assert_eq!(
        observed.len(),
        1,
        "shutdown must not start another physical inspection"
    );
    let untouched: i64 = db
        .query_one(
            "SELECT count(*) FROM runtime.runsc_environment_power WHERE state='pauseDispatched'",
            &[],
        )
        .unwrap()
        .get(0);
    assert_eq!(untouched, 1, "unobserved responsibility stays charged");
    assert_eq!(runsc_rotation::running_count(&mut db, "host").unwrap(), 1);
}

#[test]
#[ignore = "requires explicitly owned disposable PostgreSQL database"]
fn postgres_runsc_yield_preserves_waiter_until_confirmed_pause_wakes_claim() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db, run, _binding) = retained();
    db.execute(
        "UPDATE public.app_core_agentrun SET status='completed' WHERE id=$1",
        &[&run.original.agent_run_id],
    )
    .unwrap();
    db.batch_execute("UPDATE public.app_core_sessionenvironment SET controller_released=true")
        .unwrap();
    store.runsc_idle_candidates("host", None, 1).unwrap();
    db.batch_execute("UPDATE runtime.runsc_environment_power SET idle_since_ms=0")
        .unwrap();
    let candidate = store
        .runsc_idle_candidates("host", None, 1)
        .unwrap()
        .candidates
        .remove(0);
    let resident_before: String = db
        .query_one(
            "SELECT row_to_json(s)::text FROM runtime.resident_sandboxes s",
            &[],
        )
        .unwrap()
        .get(0);
    let allocation_before: String = db
        .query_one(
            "SELECT row_to_json(i)::text FROM runtime.execution_instances i",
            &[],
        )
        .unwrap()
        .get(0);

    let job_id = "agent_run.lifecycle:running-slot-waiter";
    let owner = "running-slot-wait-owner";
    let mut waiting = job(job_id, job_id);
    waiting.job_kind = "agent_run.lifecycle".into();
    waiting.status = RuntimeJobStatus::Running;
    waiting.lease_owner = Some(owner.into());
    waiting.lease_expires_at_ms = Some(4_000_000_000_000);
    waiting.session_id = Some("waiting-session".into());
    waiting.retry_count = 2;
    waiting.max_retries = 10;
    store
        .schedule_worker_job(ScheduleRuntimeJobRequest { job: waiting }, Some("ws_1"))
        .unwrap();
    let fence = RuntimeJobLeaseFence {
        job_id: job_id.into(),
        job_kind: "agent_run.lifecycle".into(),
        lease_owner: owner.into(),
    };
    // The retained admission path durably registers this before it attempts
    // running-slot reservation; a full slot must not discard the registration.
    let mut tx = db.transaction().unwrap();
    crate::postgres_store::capacity_wait::register(&mut tx, &fence).unwrap();
    tx.commit().unwrap();
    let mut tx = db.transaction().unwrap();
    assert_eq!(
        runsc_rotation::reserve_running(
            &mut tx,
            &RunscBinding {
                environment_id: "waiting-environment".into(),
                host_id: "host".into(),
                allocation_execution_id: "waiting-allocation".into(),
                instance_id: "waiting-instance".into(),
                container_id: None,
            },
            1
        )
        .unwrap_err(),
        RUNSC_CAPACITY_WAIT
    );
    tx.rollback().unwrap();
    let now = |db: &mut Client| -> i64 {
        db.query_one(
            "SELECT (EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint",
            &[],
        )
        .unwrap()
        .get(0)
    };
    let yielded_at = now(&mut db);
    let future_retry = yielded_at + 1_800_000;
    store
        .yield_runtime_job(YieldRuntimeJobRequest {
            job_id: job_id.into(),
            lease_owner: owner.into(),
            yielded_at_ms: yielded_at,
            run_at_ms: future_retry,
            transition_reason: RUNSC_CAPACITY_WAIT.into(),
        })
        .unwrap();
    let claim = |store: &PostgresRuntimeStore, at| {
        store
            .claim_worker_jobs(ClaimDueRuntimeJobsRequest {
                now_ms: at,
                worker_id: "running-slot-wake-observer".into(),
                job_id: Some(job_id.into()),
                job_kind: Some("agent_run.lifecycle".into()),
                session_id: None,
                limit: 1,
                lease_ms: 60_000,
            })
            .unwrap()
    };
    assert!(
        claim(&store, now(&mut db)).is_empty(),
        "yield alone cannot grant running capacity"
    );
    store
        .with_runsc_idle_pause(&candidate, |_, _| Ok(IdlePauseOutcome::Paused))
        .unwrap();
    assert_eq!(runsc_rotation::running_count(&mut db, "host").unwrap(), 0);
    drop(store);
    let reopened = PostgresRuntimeStore::new(&url).unwrap();
    let claimed_at = now(&mut db);
    assert!(claimed_at < future_retry);
    let claimed = claim(&reopened, claimed_at);
    assert_eq!(
        claimed.len(),
        1,
        "confirmed pause must wake a yielded runsc capacity waiter before its future retry"
    );
    assert_eq!(claimed[0].retry_count, 2);
    assert_eq!(claimed[0].max_retries, 10);
    assert_ne!(claimed[0].lease_owner.as_deref(), Some(owner));
    assert!(
        claim(&reopened, now(&mut db)).is_empty(),
        "a wake grants only one current execution owner"
    );
    assert_eq!(
        db.query_one(
            "SELECT row_to_json(s)::text FROM runtime.resident_sandboxes s",
            &[]
        )
        .unwrap()
        .get::<_, String>(0),
        resident_before
    );
    assert_eq!(
        db.query_one(
            "SELECT row_to_json(i)::text FROM runtime.execution_instances i",
            &[]
        )
        .unwrap()
        .get::<_, String>(0),
        allocation_before
    );
    let evidence: serde_json::Value = serde_json::from_str(&db.query_one(
        "SELECT payload_json FROM runtime.runtime_events WHERE task_id=$1 AND event_type='runtime.resident_capacity.claimed.v1'", &[&job_id]
    ).unwrap().get::<_, String>(0)).unwrap();
    assert_eq!(evidence["generation"], 1);
    assert_eq!(
        evidence["leaseOwner"].as_str(),
        claimed[0].lease_owner.as_deref()
    );
}

#[test]
#[ignore = "requires explicitly owned disposable PostgreSQL database"]
fn postgres_runsc_known_busy_pause_restores_running_and_restarts_idle_interval() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_url, store, mut db, run, _binding) = retained();
    db.execute(
        "UPDATE public.app_core_agentrun SET status='completed' WHERE id=$1",
        &[&run.original.agent_run_id],
    )
    .unwrap();
    db.batch_execute("UPDATE public.app_core_sessionenvironment SET controller_released=true")
        .unwrap();
    store.runsc_idle_candidates("host", None, 1).unwrap();
    db.batch_execute("UPDATE runtime.runsc_environment_power SET idle_since_ms=0")
        .unwrap();
    let candidate = store
        .runsc_idle_candidates("host", None, 1)
        .unwrap()
        .candidates
        .remove(0);
    let generation: i64 = db
        .query_one(
            "SELECT generation FROM runtime.resident_capacity_clock",
            &[],
        )
        .unwrap()
        .get(0);
    let result = store.with_runsc_idle_pause(&candidate, |_, _| Ok(IdlePauseOutcome::Busy));
    let row = db
        .query_one(
            "SELECT state,idle_since_ms FROM runtime.runsc_environment_power",
            &[],
        )
        .unwrap();
    assert_eq!(
        row.get::<_, String>(0),
        "running",
        "a known refusal to pause must not retain an ambiguous dispatched pause"
    );
    assert_eq!(
        row.get::<_, Option<i64>>(1),
        None,
        "background activity restarts the continuous idle interval"
    );
    assert!(result.is_ok());
    assert_eq!(runsc_rotation::running_count(&mut db, "host").unwrap(), 1);
    assert_eq!(
        db.query_one(
            "SELECT generation FROM runtime.resident_capacity_clock",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        generation,
        "busy must not announce freed running capacity"
    );
    assert!(store
        .runsc_idle_candidates("host", None, 1000)
        .unwrap()
        .candidates
        .is_empty());
}

#[test]
#[ignore = "requires explicitly owned disposable PostgreSQL database"]
fn postgres_runsc_typed_unknown_pause_keeps_dispatched_responsibility() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_url, store, mut db, run, _binding) = retained();
    db.execute(
        "UPDATE public.app_core_agentrun SET status='completed' WHERE id=$1",
        &[&run.original.agent_run_id],
    )
    .unwrap();
    db.batch_execute("UPDATE public.app_core_sessionenvironment SET controller_released=true")
        .unwrap();
    store.runsc_idle_candidates("host", None, 1).unwrap();
    db.batch_execute("UPDATE runtime.runsc_environment_power SET idle_since_ms=0")
        .unwrap();
    let candidate = store
        .runsc_idle_candidates("host", None, 1)
        .unwrap()
        .candidates
        .remove(0);
    assert!(store
        .with_runsc_idle_pause(&candidate, |_, _| Ok(IdlePauseOutcome::Unknown {
            reason: "task_state_unknown".into()
        }))
        .is_err());
    let row = db
        .query_one(
            "SELECT state,idle_since_ms FROM runtime.runsc_environment_power",
            &[],
        )
        .unwrap();
    assert_eq!(row.get::<_, String>(0), "pauseDispatched");
    assert_eq!(row.get::<_, Option<i64>>(1), Some(0));
    assert_eq!(runsc_rotation::running_count(&mut db, "host").unwrap(), 1);
    assert_eq!(
        db.query_one(
            "SELECT generation FROM runtime.resident_capacity_clock",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        0
    );
}
