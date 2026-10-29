//! Diagnose resource re-evaluation after pending creation settles, without a release signal.
use super::*;
use centaeris_core::session::reliability::{
    ClaimDueRuntimeJobsRequest, RuntimeJobStorePort, YieldRuntimeJobRequest,
};

fn database_now(db: &mut Client) -> i64 {
    db.query_one(
        "SELECT (EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint",
        &[],
    )
    .unwrap()
    .get(0)
}

fn capacity_generation(db: &mut Client) -> i64 {
    db.query_one(
        "SELECT generation FROM runtime.resident_capacity_clock",
        &[],
    )
    .unwrap()
    .get(0)
}

fn claim_original_waiter(
    store: &PostgresRuntimeStore,
    run: &Run,
    at: i64,
) -> Vec<centaeris_core::session::reliability::RuntimeJobRecord> {
    store
        .claim_worker_jobs(ClaimDueRuntimeJobsRequest {
            now_ms: at,
            worker_id: "pending-capacity-observer".into(),
            job_id: Some(run.fence.job_id.clone()),
            job_kind: Some("agent_run.lifecycle".into()),
            session_id: None,
            limit: 1,
            lease_ms: 60_000,
        })
        .unwrap()
}

fn confirmed_pending_resident_wake(before_yield: bool, via_reconciliation: bool) {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db) = setup();
    let (first_run, waiting, mut capacity) = independent_runs(&store, &mut db);
    let witness = per_run(super::super::super::shared_admission_tests::run_scoped(
        &store,
        &mut db,
        "pending-witness-run",
        "pending-witness-session",
        "pending-witness-environment",
        first_run.original.environment_profile.clone(),
    ));
    let one = declared_usage(
        first_run
            .original
            .environment_profile
            .resources
            .sandbox_resources(),
    );
    let memory_limit = one.memory_bytes + one.memory_bytes / 2;
    capacity.global.memory_bytes = memory_limit;
    capacity.tenant.memory_bytes = memory_limit;
    capacity.host.memory_bytes = memory_limit;
    let first_store = PostgresRuntimeStore::new(&url).unwrap();
    let waiting_store = PostgresRuntimeStore::new(&url).unwrap();
    let witness_store = PostgresRuntimeStore::new(&url).unwrap();
    let claiming_store = PostgresRuntimeStore::new(&url).unwrap();
    let physical = Arc::new(Mutex::new(Vec::new()));
    let creators = Arc::new(AtomicUsize::new(0));
    let first = if via_reconciliation {
        blocked_settlement(
            first_store,
            first_run.clone(),
            capacity,
            physical.clone(),
            creators.clone(),
            true,
        )
    } else {
        BlockedPreparation::start(
            first_store,
            first_run.clone(),
            capacity,
            physical.clone(),
            creators.clone(),
        )
    };
    let pending = first.allocation();
    let waiting_profile = waiting.original.environment_profile.digest().unwrap();
    let waiting_creators = AtomicUsize::new(0);
    let rejected = waiting_store.prepare_retained_instance_with_inventory(
        &waiting.request("pending-waiter-execution", &waiting_profile),
        capacity,
        || Ok(measured_inventory(&first_run, &pending, false)),
        |_, _| Ok(()),
        |_| {
            waiting_creators.fetch_add(1, Ordering::SeqCst);
            Err("waiter unexpectedly dispatched".into())
        },
    );
    assert_eq!(
        rejected.unwrap_err(),
        crate::resident_capacity::RESIDENT_CAPACITY_WAIT
    );
    assert_eq!(waiting_creators.load(Ordering::SeqCst), 0);
    assert_eq!(db.query_one("SELECT count(*) FROM runtime.runtime_events WHERE task_id=$1 AND event_type='runtime.host_preparation.waiting.v1'", &[&waiting.fence.job_id]).unwrap().get::<_,i64>(0), 0,
        "this must be a pure resource waiter, not a global/environment preparation waiter");
    let mut first = Some(first);
    let early_confirmation = if before_yield {
        let confirmed = settle_pending(
            first.take().unwrap(),
            &first_run,
            &pending,
            &physical,
            &store,
            &mut db,
            via_reconciliation,
        );
        assert!(
            claim_original_waiter(&claiming_store, &waiting, database_now(&mut db)).is_empty(),
            "confirmation must not steal a still-active Job before its owner yields"
        );
        Some(confirmed)
    } else {
        None
    };
    let yielded_at = database_now(&mut db);
    let fallback = yielded_at + 1_800_000;
    waiting_store
        .yield_runtime_job(YieldRuntimeJobRequest {
            job_id: waiting.fence.job_id.clone(),
            lease_owner: waiting.fence.lease_owner.clone(),
            yielded_at_ms: yielded_at,
            run_at_ms: fallback,
            transition_reason: crate::resident_capacity::RESIDENT_CAPACITY_WAIT.into(),
        })
        .unwrap();
    if !before_yield {
        assert!(
            claim_original_waiter(&claiming_store, &waiting, database_now(&mut db)).is_empty(),
            "the resource refusal must not immediately wake its own still-blocked job"
        );
    }
    let confirmed = match early_confirmation {
        Some(confirmed) => confirmed,
        None => settle_pending(
            first.take().unwrap(),
            &first_run,
            &pending,
            &physical,
            &store,
            &mut db,
            via_reconciliation,
        ),
    };
    assert_eq!(creators.load(Ordering::SeqCst), 1);
    assert_eq!(physical.lock().unwrap().len(), 1);
    assert_eq!(
        responsibility(&mut db).unwrap().reservation["state"],
        "resident"
    );
    let after_confirmation = capacity_generation(&mut db);
    let first_profile = first_run.original.environment_profile.digest().unwrap();
    let reused = store
        .prepare_retained_instance(
            &first_run.request(FIRST_EXECUTION, &first_profile),
            capacity,
            || Ok(physical.lock().unwrap().clone()),
            |_, _| Ok(()),
            |allocation| {
                assert!(allocation.reused);
                Ok(allocation.container_id.clone().unwrap())
            },
        )
        .unwrap();
    assert_eq!(reused.instance_id, confirmed.instance_id);
    store
        .reconcile_resident_sandboxes("host", || Ok(physical.lock().unwrap().clone()))
        .unwrap();
    assert_eq!(
        capacity_generation(&mut db),
        after_confirmation,
        "same-instance reuse and unchanged inventory must not repeat the first confirmation signal"
    );
    let original = responsibility(&mut db).unwrap();

    // An equivalent, independent request proves the fresh measured sample admits.
    // Fail only after physical dispatch: no refund, DELETE, or notDispatched branch
    // may provide an alternative wake source for the original waiter.
    let witness_profile = witness.original.environment_profile.digest().unwrap();
    let mut witness_allocation = None;
    let witness_result = witness_store.prepare_retained_instance_with_inventory(
        &witness.request("pending-witness-execution", &witness_profile),
        capacity,
        || Ok(measured_inventory(&first_run, &confirmed, true)),
        |_, _| Ok(()),
        |allocation| {
            witness_allocation = Some(allocation.clone());
            Err("diagnostic create acknowledgement unknown".into())
        },
    );
    assert_eq!(
        witness_result.unwrap_err(),
        "diagnostic create acknowledgement unknown"
    );
    let witness_allocation = witness_allocation
        .expect("fresh trusted measured inventory must enter real admission and physical dispatch");
    assert_ne!(witness_allocation.instance_id, confirmed.instance_id);
    let witness_state = db.query_one("SELECT s.state,s.container_id,a.state FROM runtime.resident_sandboxes s JOIN runtime.environment_creation_attempts a ON a.allocation_execution_id=s.execution_id AND a.instance_id=s.instance_id WHERE s.execution_id='pending-witness-execution'", &[]).unwrap();
    assert_eq!(witness_state.get::<_, String>(0), "reserved");
    assert_eq!(witness_state.get::<_, Option<String>>(1), None);
    assert_eq!(witness_state.get::<_, String>(2), "dispatched");
    assert_eq!(
        capacity_generation(&mut db),
        after_confirmation,
        "the fresh-admission witness must not supply a release or preparation notification"
    );
    assert_eq!(
        responsibility(&mut db).unwrap(),
        original,
        "eligibility diagnostics must not refund or rewrite the confirmed resource holding"
    );
    let at = database_now(&mut db);
    assert!(
        at < fallback,
        "claim proof must precede the original fallback deadline"
    );
    let claimed = claim_original_waiter(&claiming_store, &waiting, at);
    assert_eq!(claimed.len(), 1,
        "a fresh-admissible pure resource waiter must receive a re-evaluation opportunity after pending confirmation, before fallback");
    assert_eq!(claimed[0].job_id, waiting.fence.job_id);
    assert_eq!(claimed[0].retry_count, 0);
    assert_ne!(
        claimed[0].lease_owner.as_deref(),
        Some(waiting.fence.lease_owner.as_str())
    );
    assert_eq!(waiting_creators.load(Ordering::SeqCst), 0);
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.execution_instances WHERE environment_id=$1",
            &[&waiting.grant.payload.environment_id]
        )
        .unwrap()
        .get::<_, i64>(0),
        0,
        "wake and claim grant an opportunity to recheck, not a physical execution allocation"
    );
    assert_eq!(physical.lock().unwrap().len(), 1);
}

fn blocked_settlement(
    store: PostgresRuntimeStore,
    run: Arc<Run>,
    capacity: ResidentCapacity,
    physical: Arc<Mutex<Vec<ObservedResidentSandbox>>>,
    creators: Arc<AtomicUsize>,
    unknown: bool,
) -> BlockedPreparation {
    let (entered_tx, entered) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        let profile = run.original.environment_profile.digest().unwrap();
        let physical_run = run.clone();
        store.prepare_retained_instance(
            &run.request(FIRST_EXECUTION, &profile),
            capacity,
            || Ok(Vec::new()),
            |_, _| Ok(()),
            move |allocation| {
                creators.fetch_add(1, Ordering::SeqCst);
                entered_tx
                    .send(allocation.clone())
                    .map_err(|e| e.to_string())?;
                release_rx
                    .recv_timeout(Duration::from_secs(15))
                    .map_err(|e| e.to_string())?;
                if unknown {
                    Err("diagnostic create acknowledgement unknown".into())
                } else {
                    confirm(&physical_run, allocation, &physical)
                }
            },
        )
    });
    BlockedPreparation {
        entered,
        release: Some(release),
        worker: Some(worker),
    }
}

fn settle_pending(
    first: BlockedPreparation,
    run: &Run,
    pending: &RetainedInstanceAllocation,
    physical: &Arc<Mutex<Vec<ObservedResidentSandbox>>>,
    store: &PostgresRuntimeStore,
    db: &mut Client,
    via_reconciliation: bool,
) -> RetainedInstanceAllocation {
    if !via_reconciliation {
        return first.finish().unwrap();
    }
    let before = capacity_generation(db);
    assert_eq!(
        first.finish().unwrap_err(),
        "diagnostic create acknowledgement unknown"
    );
    assert_eq!(
        capacity_generation(db),
        before,
        "unknown creation alone supplies no confirmation"
    );
    let mut confirmed = pending.clone();
    confirmed.container_id = Some(confirm(run, pending, physical).unwrap());
    store
        .reconcile_resident_sandboxes("host", || Ok(physical.lock().unwrap().clone()))
        .unwrap();
    assert_eq!(responsibility(db).unwrap().reservation["state"], "resident");
    confirmed
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_confirmed_pending_resident_wakes_future_waiter_before_fallback() {
    confirmed_pending_resident_wake(false, false);
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_pending_confirmation_before_yield_preserves_future_waiter_opportunity() {
    confirmed_pending_resident_wake(true, false);
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_reconciled_unknown_creation_wakes_future_waiter_without_refund() {
    confirmed_pending_resident_wake(false, true);
}

fn yield_waiter(store: &PostgresRuntimeStore, db: &mut Client, run: &Run) -> i64 {
    let at = database_now(db);
    let fallback = at + 1_800_000;
    store
        .yield_runtime_job(YieldRuntimeJobRequest {
            job_id: run.fence.job_id.clone(),
            lease_owner: run.fence.lease_owner.clone(),
            yielded_at_ms: at,
            run_at_ms: fallback,
            transition_reason: crate::resident_capacity::RESIDENT_CAPACITY_WAIT.into(),
        })
        .unwrap();
    fallback
}

fn tight_capacity(run: &Run, mut capacity: ResidentCapacity) -> ResidentCapacity {
    let memory = run.original.environment_profile.resources.memory_bytes;
    capacity.global.memory_bytes = memory + memory / 2;
    capacity.tenant.memory_bytes = memory + memory / 2;
    capacity.host.memory_bytes = memory + memory / 2;
    capacity
}

fn reject_pending_waiter(
    store: &PostgresRuntimeStore,
    run: &Run,
    origin: &Run,
    pending: &RetainedInstanceAllocation,
    capacity: ResidentCapacity,
) {
    let profile = run.original.environment_profile.digest().unwrap();
    let reason = store
        .prepare_retained_instance_with_inventory(
            &run.request("pending-waiter-execution", &profile),
            capacity,
            || Ok(measured_inventory(origin, pending, false)),
            |_, _| panic!("pending resource pressure cannot publish a new allocation"),
            |_| panic!("pending resource pressure cannot dispatch physical work"),
        )
        .unwrap_err();
    assert_eq!(reason, crate::resident_capacity::RESIDENT_CAPACITY_WAIT);
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_confirmation_opportunity_does_not_loop_under_fresh_real_pressure() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db) = setup();
    let (origin, mut waiting, capacity) = independent_runs(&store, &mut db);
    let capacity = tight_capacity(&origin, capacity);
    let physical = Arc::new(Mutex::new(Vec::new()));
    let first = BlockedPreparation::start(
        PostgresRuntimeStore::new(&url).unwrap(),
        origin.clone(),
        capacity,
        physical.clone(),
        Arc::new(AtomicUsize::new(0)),
    );
    let pending = first.allocation();
    reject_pending_waiter(&store, &waiting, &origin, &pending, capacity);
    yield_waiter(&store, &mut db, &waiting);
    let confirmed = first.finish().unwrap();
    let claimed = claim_original_waiter(&store, &waiting, database_now(&mut db));
    assert_eq!(
        claimed.len(),
        1,
        "confirmation must offer one fresh evaluation"
    );
    assert_eq!(claimed[0].retry_count, 0);
    let owner = claimed[0].lease_owner.clone().unwrap();
    db.execute(
        "UPDATE public.app_core_sessionenvironment SET controller_lease_owner=$2 WHERE id=$1",
        &[&waiting.grant.payload.environment_id, &owner],
    )
    .unwrap();
    waiting.fence.lease_owner = owner.clone();
    waiting
        .grant
        .payload
        .controller
        .as_mut()
        .unwrap()
        .controller_lease_digest = super::super::controller_lease_digest(&owner);
    waiting.grant = sign(waiting.grant.payload);
    let baseline = capacity_generation(&mut db);
    let holding = responsibility(&mut db).unwrap();
    let profile = waiting.original.environment_profile.digest().unwrap();
    let reason = store
        .prepare_retained_instance_with_inventory(
            &waiting.request("pending-waiter-execution", &profile),
            capacity,
            || {
                let mut sample = measured_inventory(&origin, &confirmed, true);
                sample
                    .shared
                    .as_mut()
                    .unwrap()
                    .measured_by_container
                    .get_mut(confirmed.container_id.as_deref().unwrap())
                    .unwrap()
                    .memory_current_bytes =
                    origin.original.environment_profile.resources.memory_bytes;
                Ok(sample)
            },
            |_, _| panic!("fresh high usage still cannot allocate"),
            |_| panic!("a wake is not permission to exceed the budget"),
        )
        .unwrap_err();
    assert_eq!(reason, crate::resident_capacity::RESIDENT_CAPACITY_WAIT);
    let fallback = yield_waiter(&store, &mut db, &waiting);
    let now = database_now(&mut db);
    assert!(now < fallback);
    assert!(
        claim_original_waiter(&store, &waiting, now).is_empty(),
        "the failed re-evaluation must not notify itself again"
    );
    assert_eq!(capacity_generation(&mut db), baseline);
    assert_eq!(responsibility(&mut db).unwrap(), holding);
    assert_eq!(physical.lock().unwrap().len(), 1);
    assert_eq!(
        db.query_one(
            "SELECT retry_count FROM runtime.runtime_jobs WHERE job_id=$1",
            &[&waiting.fence.job_id]
        )
        .unwrap()
        .get::<_, i64>(0),
        0
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_unknown_or_rolled_back_confirmation_does_not_advance_waiter() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    for rollback in [false, true] {
        let (url, store, mut db) = setup();
        let origin_store = PostgresRuntimeStore::new(&url).unwrap();
        if rollback {
            db.batch_execute("CREATE FUNCTION runtime.reject_confirmation_commit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF OLD.state='reserved' AND NEW.state='resident' THEN RAISE EXCEPTION 'diagnostic confirmation commit rejected'; END IF; RETURN NEW; END $$; CREATE CONSTRAINT TRIGGER reject_confirmation_commit AFTER UPDATE ON runtime.resident_sandboxes DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION runtime.reject_confirmation_commit();").unwrap();
        }
        let (origin, waiting, capacity) = independent_runs(&store, &mut db);
        let capacity = tight_capacity(&origin, capacity);
        let physical = Arc::new(Mutex::new(Vec::new()));
        let first = blocked_settlement(
            origin_store,
            origin.clone(),
            capacity,
            physical,
            Arc::new(AtomicUsize::new(0)),
            !rollback,
        );
        let pending = first.allocation();
        reject_pending_waiter(&store, &waiting, &origin, &pending, capacity);
        let fallback = yield_waiter(&store, &mut db, &waiting);
        let before = capacity_generation(&mut db);
        let responsibility_before = responsibility(&mut db).unwrap();
        let result = first.finish();
        assert!(
            result.is_err(),
            "unknown ACK or deferred commit failure cannot confirm a resident"
        );
        assert_eq!(
            capacity_generation(&mut db),
            before,
            "a rolled-back or unknown confirmation cannot supply eligibility"
        );
        assert_eq!(
            responsibility(&mut db).unwrap(),
            responsibility_before,
            "the reservation and dispatched journal must survive without refund"
        );
        let now = database_now(&mut db);
        assert!(now < fallback);
        assert!(claim_original_waiter(&store, &waiting, now).is_empty());
    }
}
