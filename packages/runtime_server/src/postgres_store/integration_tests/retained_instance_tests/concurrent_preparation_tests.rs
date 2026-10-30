//! Cold creation overlap and durable responsibility use real independent PostgreSQL clients.
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

mod pending_capacity_wake_tests;

const FIRST_EXECUTION: &str = "cold-preparation-a";

type Prepared = Result<RetainedInstanceAllocation, String>;

/// Always release and join the creator, including when a test assertion unwinds.
struct BlockedPreparation {
    entered: mpsc::Receiver<RetainedInstanceAllocation>,
    release: Option<mpsc::Sender<()>>,
    worker: Option<JoinHandle<Prepared>>,
}

impl BlockedPreparation {
    fn start(
        store: PostgresRuntimeStore,
        run: Arc<Run>,
        capacity: ResidentCapacity,
        physical: Arc<Mutex<Vec<ObservedResidentSandbox>>>,
        creators: Arc<AtomicUsize>,
    ) -> Self {
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
                    confirm(&physical_run, allocation, &physical)
                },
            )
        });
        Self {
            entered,
            release: Some(release),
            worker: Some(worker),
        }
    }

    fn allocation(&self) -> RetainedInstanceAllocation {
        self.entered
            .recv_timeout(Duration::from_secs(5))
            .expect("first creator must reach its physical callback")
    }

    fn finish(mut self) -> Prepared {
        self.release.take().unwrap().send(()).unwrap();
        self.worker.take().unwrap().join().unwrap()
    }
}

impl Drop for BlockedPreparation {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            let _ = release.send(());
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn per_run(mut run: Run) -> Run {
    run.grant.payload.execution_mode = "perRun".into();
    run.grant = sign(run.grant.payload);
    run
}

fn independent_runs(
    store: &PostgresRuntimeStore,
    db: &mut Client,
) -> (Arc<Run>, Run, ResidentCapacity) {
    let first = per_run(run(store, db, "cold-run-a", 1));
    let second = per_run(super::super::shared_admission_tests::run_scoped(
        store,
        db,
        "cold-run-b",
        "cold-session-b",
        "cold-environment-b",
        first.original.environment_profile.clone(),
    ));
    assert_eq!(first.original.workspace_id, second.original.workspace_id);
    assert_ne!(
        first.grant.payload.session_id,
        second.grant.payload.session_id
    );
    assert_ne!(
        first.grant.payload.environment_id,
        second.grant.payload.environment_id
    );
    let one = declared_usage(
        first
            .original
            .environment_profile
            .resources
            .sandbox_resources(),
    );
    let two = one.checked_add(one).unwrap();
    (
        Arc::new(first),
        second,
        ResidentCapacity {
            global: two,
            tenant: two,
            host: two,
        },
    )
}

fn confirm(
    run: &Run,
    allocation: &RetainedInstanceAllocation,
    physical: &Arc<Mutex<Vec<ObservedResidentSandbox>>>,
) -> Result<String, String> {
    let container = format!("container-{}", allocation.instance_id);
    physical.lock().unwrap().push(ObservedResidentSandbox {
        execution_id: allocation.allocation_execution_id.clone(),
        allocation_owner: allocation.allocation_owner.clone(),
        container_id: container.clone(),
        resources: declared_usage(
            run.original
                .environment_profile
                .resources
                .sandbox_resources(),
        ),
    });
    Ok(container)
}

#[derive(Debug, PartialEq)]
struct Responsibility {
    reservation: serde_json::Value,
    creation_attempt: serde_json::Value,
    instance: serde_json::Value,
}

fn responsibility(db: &mut Client) -> Result<Responsibility, String> {
    let row = db.query_one(
        "SELECT row_to_json(s)::text,row_to_json(a)::text,row_to_json(i)::text FROM runtime.resident_sandboxes s JOIN runtime.environment_creation_attempts a ON a.allocation_execution_id=s.execution_id AND a.instance_id=s.instance_id JOIN runtime.execution_instances i ON i.execution_id=s.execution_id AND i.instance_id=s.instance_id WHERE s.execution_id=$1",
        &[&FIRST_EXECUTION],
    ).map_err(|e| e.to_string())?;
    let field =
        |index| serde_json::from_str(&row.get::<_, String>(index)).map_err(|e| e.to_string());
    Ok(Responsibility {
        reservation: field(0)?,
        creation_attempt: field(1)?,
        instance: field(2)?,
    })
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_distinct_environment_cold_preparations_overlap_with_shared_authority() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db) = setup();
    let (first_run, second_run, capacity) = independent_runs(&store, &mut db);
    let first_store = PostgresRuntimeStore::new(&url).unwrap();
    let second_store = PostgresRuntimeStore::new(&url).unwrap();
    let physical = Arc::new(Mutex::new(Vec::new()));
    let creators = Arc::new(AtomicUsize::new(0));
    let first = BlockedPreparation::start(
        first_store,
        first_run,
        capacity,
        physical.clone(),
        creators.clone(),
    );
    let first_allocation = first.allocation();
    let (entered_tx, entered_rx) = mpsc::channel();
    let profile = second_run.original.environment_profile.digest().unwrap();
    let second = {
        let _deadline = crate::postgres_store::RequestDeadline::enter(Some(
            Instant::now() + Duration::from_secs(3),
        ));
        second_store.prepare_retained_instance(
            &second_run.request("cold-preparation-b", &profile),
            capacity,
            || Ok(Vec::new()),
            |_, _| Ok(()),
            |allocation| {
                entered_tx.send(()).map_err(|e| e.to_string())?;
                confirm(&second_run, allocation, &physical)
            },
        )
    };
    let overlapped = entered_rx.try_recv().is_ok();
    // Releasing A only after B returns makes overlap independent of thread scheduling.
    let first = first.finish().unwrap();
    assert!(overlapped, "another environment could not enter physical preparation while A was in flight: {second:?}");
    let second = second.expect("independent cold preparation must complete while A is blocked");
    assert_eq!(creators.load(Ordering::SeqCst), 1);
    assert_eq!(first.instance_id, first_allocation.instance_id);
    assert_ne!(first.instance_id, second.instance_id);
    assert_eq!(physical.lock().unwrap().len(), 2);
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.resident_sandboxes WHERE state='resident'",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        2
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_missing_inflight_inventory_preserves_dispatched_creation_responsibility() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db) = setup();
    let (first_run, second_run, capacity) = independent_runs(&store, &mut db);
    let first_store = PostgresRuntimeStore::new(&url).unwrap();
    let second_store = PostgresRuntimeStore::new(&url).unwrap();
    let duplicate_store = PostgresRuntimeStore::new(&url).unwrap();
    let mut observer = Client::connect(&url, NoTls).unwrap();
    let physical = Arc::new(Mutex::new(Vec::new()));
    let creators = Arc::new(AtomicUsize::new(0));
    let first = BlockedPreparation::start(
        first_store,
        first_run.clone(),
        capacity,
        physical.clone(),
        creators.clone(),
    );
    let allocation = first.allocation();
    let before = responsibility(&mut observer).unwrap();
    assert_eq!(before.reservation["state"], "reserved");
    assert_eq!(before.reservation["container_id"], serde_json::Value::Null);
    assert_eq!(before.creation_attempt["state"], "dispatched");
    assert_eq!(before.instance["instance_id"], allocation.instance_id);
    let mut after_reconcile = None;
    let mut inventories = 0;
    let profile = second_run.original.environment_profile.digest().unwrap();
    let second = {
        let _deadline = crate::postgres_store::RequestDeadline::enter(Some(
            Instant::now() + Duration::from_secs(3),
        ));
        second_store.prepare_retained_instance(
            &second_run.request("cold-preparation-b", &profile),
            capacity,
            || {
                inventories += 1;
                Ok(Vec::new())
            },
            |_, _| {
                after_reconcile = Some(responsibility(&mut observer)?);
                Ok(())
            },
            |allocation| confirm(&second_run, allocation, &physical),
        )
    };
    let current_profile = first_run.original.environment_profile.digest().unwrap();
    let duplicate = {
        let _deadline = crate::postgres_store::RequestDeadline::enter(Some(
            Instant::now() + Duration::from_secs(1),
        ));
        duplicate_store.prepare_retained_instance(
            &first_run.request("duplicate-cold-preparation-a", &current_profile),
            capacity,
            || Ok(physical.lock().unwrap().clone()),
            |_, _| Ok(()),
            |_| {
                creators.fetch_add(1, Ordering::SeqCst);
                Err("duplicate creator must remain fenced".into())
            },
        )
    };
    let after_duplicate = responsibility(&mut observer).unwrap();
    let association_count = observer
        .query_one(
            "SELECT count(*) FROM runtime.execution_instances WHERE environment_id=$1",
            &[&first_run.grant.payload.environment_id],
        )
        .unwrap()
        .get::<_, i64>(0);
    let first_result = first.finish();
    assert_eq!(
        inventories, 1,
        "B must reconcile its inventory while A is dispatched: {second:?}"
    );
    assert_eq!(
        after_reconcile.as_ref(),
        Some(&before),
        "missing inventory must retain the entire original responsibility"
    );
    assert_eq!(
        after_duplicate, before,
        "a refused duplicate must preserve the original responsibility"
    );
    assert_eq!(after_duplicate.reservation["state"], "reserved");
    assert_eq!(after_duplicate.creation_attempt["state"], "dispatched");
    assert_eq!(
        after_duplicate.instance["instance_id"],
        allocation.instance_id
    );
    assert!(
        duplicate.is_err(),
        "same environment must reject a second creator"
    );
    assert_eq!(creators.load(Ordering::SeqCst), 1);
    assert_eq!(
        association_count, 1,
        "duplicate must not invent a replacement instance association"
    );
    second.expect("unrelated cold preparation must succeed without observing A's container");
    let first = first_result.unwrap();
    assert_eq!(first.instance_id, allocation.instance_id);
    assert_eq!(physical.lock().unwrap().len(), 2);
}

fn measured_inventory(
    run: &Run,
    allocation: &RetainedInstanceAllocation,
    include_first: bool,
) -> crate::resident_capacity::ResidentInventory {
    let resources = declared_usage(
        run.original
            .environment_profile
            .resources
            .sandbox_resources(),
    );
    let container = allocation
        .container_id
        .clone()
        .unwrap_or_else(|| format!("container-{}", allocation.instance_id));
    let mut measured_by_container = std::collections::BTreeMap::new();
    let sandboxes = if include_first {
        measured_by_container.insert(
            container.clone(),
            crate::shared_resident_admission::MeasuredUsage {
                memory_current_bytes: resources.memory_bytes / 16,
                swap_current_bytes: 0,
                pids_current: 1,
                workspace_upper_bound_bytes: Some(resources.workspace_bytes / 16),
            },
        );
        vec![ObservedResidentSandbox {
            execution_id: allocation.allocation_execution_id.clone(),
            allocation_owner: allocation.allocation_owner.clone(),
            container_id: container,
            resources,
        }]
    } else {
        Vec::new()
    };
    crate::resident_capacity::ResidentInventory {
        sandboxes,
        shared: Some(crate::resident_capacity::SharedHostInventory {
            host_id: "host".into(),
            available_memory_bytes: resources.memory_bytes.saturating_mul(4),
            memory_headroom_bytes: 1,
            valid_until: Instant::now() + Duration::from_secs(10),
            measured_by_container,
        }),
    }
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_pending_sample_cannot_refund_or_discount_newly_confirmed_resident() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    for include_first in [false, true] {
        let (url, store, mut db) = setup();
        let (first_run, second_run, mut capacity) = independent_runs(&store, &mut db);
        let one = declared_usage(
            first_run
                .original
                .environment_profile
                .resources
                .sandbox_resources(),
        );
        // Full A plus B cannot fit; a discounted or refunded A would admit B.
        let memory_limit = one.memory_bytes + one.memory_bytes / 2;
        capacity.global.memory_bytes = memory_limit;
        capacity.tenant.memory_bytes = memory_limit;
        capacity.host.memory_bytes = memory_limit;
        let first_store = PostgresRuntimeStore::new(&url).unwrap();
        let second_store = PostgresRuntimeStore::new(&url).unwrap();
        let fresh_store = PostgresRuntimeStore::new(&url).unwrap();
        let mut observer = Client::connect(&url, NoTls).unwrap();
        let physical = Arc::new(Mutex::new(Vec::new()));
        let creators = Arc::new(AtomicUsize::new(0));
        let first = BlockedPreparation::start(
            first_store,
            first_run.clone(),
            capacity,
            physical.clone(),
            creators.clone(),
        );
        let allocation = first.allocation();
        let pending = responsibility(&mut observer).unwrap();
        assert_eq!(pending.reservation["state"], "reserved");
        assert_eq!(pending.creation_attempt["state"], "dispatched");
        let mut first = Some(first);
        let mut first_result = None;
        let mut sampled_pending = false;
        let before_create = AtomicUsize::new(0);
        let second_creators = AtomicUsize::new(0);
        let profile = second_run.original.environment_profile.digest().unwrap();
        let second = {
            let _deadline = crate::postgres_store::RequestDeadline::enter(Some(
                Instant::now() + Duration::from_secs(5),
            ));
            second_store.prepare_retained_instance_with_inventory(
                &second_run.request("cold-preparation-b", &profile),
                capacity,
                || {
                    let at_sample = responsibility(&mut observer)?;
                    sampled_pending = at_sample.reservation["state"] == "reserved";
                    // Capture the old observation before allowing A's acknowledgement to commit.
                    let sample = measured_inventory(&first_run, &allocation, include_first);
                    first_result = Some(first.take().unwrap().finish());
                    let confirmed = responsibility(&mut observer)?;
                    if confirmed.reservation["state"] != "resident" {
                        return Err(
                            "first creation must commit resident before the old sample returns"
                                .into(),
                        );
                    }
                    Ok(sample)
                },
                |_, _| {
                    before_create.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                },
                |allocation| {
                    second_creators.fetch_add(1, Ordering::SeqCst);
                    confirm(&second_run, allocation, &physical)
                },
            )
        };
        // Even a failing baseline must release and join A before asserting.
        let completed = match first.take() {
            Some(first) => first.finish(),
            None => first_result.take().expect("sample must retain A's result"),
        }
        .unwrap();
        assert!(
            sampled_pending,
            "B must sample while A is pending: {second:?}"
        );
        assert_eq!(
            second.unwrap_err(),
            crate::resident_capacity::RESIDENT_CAPACITY_WAIT,
            "a pending-at-sample resident must keep its full charge: includeFirst={include_first}"
        );
        assert_eq!(before_create.load(Ordering::SeqCst), 0);
        assert_eq!(second_creators.load(Ordering::SeqCst), 0);
        assert_eq!(creators.load(Ordering::SeqCst), 1);
        let confirmed = responsibility(&mut observer).unwrap();
        assert_eq!(confirmed.reservation["state"], "resident");
        assert_eq!(
            confirmed.reservation["container_id"],
            completed.container_id.as_deref().unwrap()
        );
        for field in ["memory_bytes", "cpu_milli", "pids", "workspace_bytes"] {
            assert_eq!(
                confirmed.reservation[field], pending.reservation[field],
                "original declaration changed: {field}"
            );
        }
        assert_eq!(confirmed.creation_attempt, pending.creation_attempt);
        assert_eq!(confirmed.instance, pending.instance);
        assert_eq!(
            observer
                .query_one(
                    "SELECT count(*) FROM runtime.execution_instances WHERE environment_id=$1",
                    &[&second_run.grant.payload.environment_id]
                )
                .unwrap()
                .get::<_, i64>(0),
            0
        );
        assert_eq!(physical.lock().unwrap().len(), 1);
        // A later complete observation may use the real measured charge normally.
        let fresh = fresh_store
            .prepare_retained_instance_with_inventory(
                &second_run.request("cold-preparation-b", &profile),
                capacity,
                || Ok(measured_inventory(&first_run, &completed, true)),
                |_, _| Ok(()),
                |allocation| {
                    second_creators.fetch_add(1, Ordering::SeqCst);
                    confirm(&second_run, allocation, &physical)
                },
            )
            .expect("fresh resident observation must allow legitimate measured admission");
        assert_ne!(fresh.instance_id, completed.instance_id);
        assert_eq!(second_creators.load(Ordering::SeqCst), 1);
        assert_eq!(physical.lock().unwrap().len(), 2);
        let after_fresh = responsibility(&mut observer).unwrap();
        assert_eq!(
            after_fresh, confirmed,
            "measured admission must not mutate A's durable declaration or journal"
        );
    }
}
