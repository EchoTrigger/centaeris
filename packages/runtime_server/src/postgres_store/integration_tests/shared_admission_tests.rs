//! PostgreSQL admission contracts use injected host observations, not physical evidence.
use super::retained_instance_tests as fixture;
use super::{PostgresRuntimeStore, TEST_LOCK};
use crate::agent_run_authorization::{EnvironmentProfile, WorkspaceAgentRunAuthorization};
use crate::environment_authorization::{ControllerBinding, EnvironmentBinding};
use crate::resident_capacity::{
    declared_usage, ObservedResidentSandbox, ResidentCapacity, ResidentInventory,
    SharedHostInventory,
};
use crate::shared_resident_admission::MeasuredUsage;
use centaeris_core::execution::ResidentResourceUsage;
use centaeris_core::session::reliability::{RuntimeJobStatus, ScheduleRuntimeJobRequest};
use centaeris_core::session::RuntimeJobLeaseFence;
use postgres::Client;
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex};

const GIB: u64 = 1024 * 1024 * 1024;

/// Each call creates its own Session, Environment, signed grant, and lifecycle fence.
/// In particular, acquiring another controller never updates a previous environment.
pub(super) fn run_scoped(
    store: &PostgresRuntimeStore,
    db: &mut Client,
    id: &str,
    session_id: &str,
    environment_id: &str,
    profile: EnvironmentProfile,
) -> fixture::Run {
    let mut original: WorkspaceAgentRunAuthorization = serde_json::from_str(include_str!(
        "../../../../../tests/workspace/fixtures/agent_run_authorization/v1/valid.json"
    ))
    .unwrap();
    original.agent_run_id = id.into();
    original.id = format!("authorization-{id}");
    original.session_id = session_id.into();
    original.environment_profile = profile;
    original.environment_profile_digest = original.environment_profile.digest().unwrap();
    let digest = original.digest().unwrap();
    let profile_json = serde_json::to_string(&original.environment_profile).unwrap();
    let owner = format!("owner-{id}");
    db.execute(
        "INSERT INTO public.app_core_session(id,workspace_id,owner_id,agent_id,current_environment_id,\"deletedAt\") VALUES($1,$2,$3,$4,$5,NULL)",
        &[&session_id,&original.workspace_id,&original.user_id,&original.agent_id,&environment_id],
    ).unwrap();
    db.execute(
        "INSERT INTO public.app_core_sessionenvironment(id,session_id,controller_run_id,controller_epoch,controller_lease_owner,controller_purpose,controller_released,environment_profile,environment_profile_digest) VALUES($1,$2,$3,1,$4,'execute',false,$5::text::jsonb,$6)",
        &[&environment_id,&session_id,&id,&owner,&profile_json,&original.environment_profile_digest],
    ).unwrap();
    db.execute(
        "INSERT INTO public.app_core_agentrun(id,session_id,environment_id,workspace_id,user_id,membership_ref,status,\"modelConfig_id\",\"thinkingMode\") VALUES($1,$2,$3,$4,$5,'member','running',$6,$7)",
        &[&id,&session_id,&environment_id,&original.workspace_id,&original.user_id,&original.model_config_ref,&original.thinking_mode],
    ).unwrap();
    db.execute(
        "INSERT INTO public.app_core_agentrunauthorization VALUES($1,$2,$3::text::jsonb,$4)",
        &[
            &id,
            &digest,
            &serde_json::to_string(&original).unwrap(),
            &original.signature(fixture::KEY).unwrap(),
        ],
    )
    .unwrap();
    let job_id = format!("agent_run.lifecycle:{id}");
    let mut job = super::job(&job_id, &format!("{job_id}:{digest}"));
    job.job_kind = "agent_run.lifecycle".into();
    job.status = RuntimeJobStatus::Running;
    job.lease_owner = Some(owner.clone());
    job.lease_expires_at_ms = Some(4_000_000_000_000);
    job.session_id = Some(session_id.into());
    job.payload_ref = Some(format!("record:agent_run:{id}"));
    store
        .schedule_worker_job(
            ScheduleRuntimeJobRequest { job },
            Some(&original.workspace_id),
        )
        .unwrap();
    let mut hash = Sha256::new();
    hash.update(b"workspace:environment-controller-lease:v1\0");
    hash.update(owner.as_bytes());
    let grant = fixture::sign(EnvironmentBinding {
        schema: "workspace.environment_authorization.v1".into(),
        environment_id: environment_id.into(),
        session_id: session_id.into(),
        agent_run_id: id.into(),
        agent_run_authorization_digest: digest,
        execution_mode: "retained".into(),
        controller: Some(ControllerBinding {
            controller_id: id.into(),
            controller_epoch: 1,
            controller_lease_digest: format!("sha256:{:x}", hash.finalize()),
            controller_purpose: "execute".into(),
        }),
    });
    fixture::Run {
        original,
        grant,
        fence: RuntimeJobLeaseFence {
            job_id,
            job_kind: "agent_run.lifecycle".into(),
            lease_owner: owner,
        },
    }
}

fn profile() -> EnvironmentProfile {
    let mut authorization: WorkspaceAgentRunAuthorization = serde_json::from_str(include_str!(
        "../../../../../tests/workspace/fixtures/agent_run_authorization/v1/valid.json"
    ))
    .unwrap();
    authorization.environment_profile.resources.memory_bytes = 4 * GIB;
    authorization.environment_profile
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_four_independent_live_environments_share_observed_resources() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_url, store, mut db) = fixture::setup();
    let runs: Vec<_> = (0..4)
        .map(|index| {
            run_scoped(
                &store,
                &mut db,
                &format!("shared-run-{index}"),
                &format!("shared-session-{index}"),
                &format!("shared-environment-{index}"),
                profile(),
            )
        })
        .collect();
    let global = ResidentResourceUsage {
        sandbox_count: 16,
        memory_bytes: 12 * GIB,
        cpu_milli: 8000,
        pids: 8192,
        workspace_bytes: 32 * GIB,
    };
    let limits = ResidentCapacity {
        global,
        host: global,
        tenant: ResidentResourceUsage {
            sandbox_count: 8,
            memory_bytes: 8 * GIB,
            cpu_milli: 4000,
            pids: 2048,
            workspace_bytes: 32 * GIB,
        },
    };
    let physical = Arc::new(Mutex::new(Vec::<ObservedResidentSandbox>::new()));
    for (index, run) in runs.iter().enumerate() {
        let profile_digest = run.original.environment_profile.digest().unwrap();
        let execution = format!("shared-execution-{index}");
        let result = store.prepare_retained_instance_with_inventory(
            &run.request(&execution, &profile_digest),
            limits,
            || {
                let sandboxes = physical.lock().unwrap().clone();
                let measured_by_container = sandboxes
                    .iter()
                    .map(|sandbox| {
                        (
                            sandbox.container_id.clone(),
                            MeasuredUsage {
                                memory_current_bytes: GIB / 4,
                                swap_current_bytes: 0,
                                pids_current: 8,
                                workspace_upper_bound_bytes: Some(GIB / 16),
                            },
                        )
                    })
                    .collect();
                Ok(ResidentInventory {
                    sandboxes,
                    shared: Some(SharedHostInventory {
                        host_id: "host".into(),
                        available_memory_bytes: 10 * GIB,
                        memory_headroom_bytes: 2 * GIB,
                        valid_until: std::time::Instant::now() + std::time::Duration::from_secs(5),
                        measured_by_container,
                    }),
                })
            },
            |_, _| Ok(()),
            |allocation| {
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
            },
        );
        assert!(
            result.is_ok(),
            "environment {index} should share measured resources: {result:?}"
        );
    }
    assert_eq!(physical.lock().unwrap().len(), 4);
    let row = db.query_one(
        "SELECT count(*), count(DISTINCT controller_run_id), count(DISTINCT session_id) FROM public.app_core_sessionenvironment WHERE id LIKE 'shared-environment-%' AND controller_purpose='execute' AND NOT controller_released", &[]
    ).unwrap();
    assert_eq!(
        (
            row.get::<_, i64>(0),
            row.get::<_, i64>(1),
            row.get::<_, i64>(2)
        ),
        (4, 4, 4)
    );
    assert_eq!(db.query_one("SELECT count(*) FROM runtime.resident_sandboxes WHERE state='resident' AND memory_bytes=$1 AND cpu_milli=2000 AND pids=512 AND workspace_bytes=$1", &[&((4 * GIB) as i64)]).unwrap().get::<_,i64>(0), 4, "admission must preserve each immutable ceiling");
}

fn shared_limits() -> ResidentCapacity {
    let global = ResidentResourceUsage {
        sandbox_count: 16,
        memory_bytes: 12 * GIB,
        cpu_milli: 8000,
        pids: 8192,
        workspace_bytes: 32 * GIB,
    };
    ResidentCapacity {
        global,
        host: global,
        tenant: ResidentResourceUsage {
            sandbox_count: 8,
            memory_bytes: 8 * GIB,
            cpu_milli: 4000,
            pids: 2048,
            workspace_bytes: 32 * GIB,
        },
    }
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_unmeasured_residents_keep_full_declarations_and_block_third_environment() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_url, store, mut db) = fixture::setup();
    let physical = Arc::new(Mutex::new(Vec::<ObservedResidentSandbox>::new()));
    let dispatched = std::sync::atomic::AtomicUsize::new(0);
    for index in 0..3 {
        let run = run_scoped(
            &store,
            &mut db,
            &format!("unmeasured-run-{index}"),
            &format!("unmeasured-session-{index}"),
            &format!("unmeasured-environment-{index}"),
            profile(),
        );
        let digest = run.original.environment_profile.digest().unwrap();
        let execution = format!("unmeasured-execution-{index}");
        let result = store.prepare_retained_instance_with_inventory(
            &run.request(&execution, &digest),
            shared_limits(),
            || {
                Ok(ResidentInventory {
                    sandboxes: physical.lock().unwrap().clone(),
                    shared: Some(SharedHostInventory {
                        host_id: "host".into(),
                        available_memory_bytes: 10 * GIB,
                        memory_headroom_bytes: 2 * GIB,
                        valid_until: std::time::Instant::now() + std::time::Duration::from_secs(5),
                        measured_by_container: Default::default(),
                    }),
                })
            },
            |_, _| Ok(()),
            |allocation| {
                dispatched.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
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
            },
        );
        if index < 2 {
            assert!(result.is_ok(), "first two declarations fit: {result:?}");
        } else {
            assert_eq!(
                result.unwrap_err(),
                crate::resident_capacity::RESIDENT_CAPACITY_WAIT
            );
        }
    }
    assert_eq!(dispatched.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert_eq!(physical.lock().unwrap().len(), 2);
    let row = db
        .query_one(
            "SELECT count(*),sum(memory_bytes) FROM runtime.resident_sandboxes",
            &[],
        )
        .unwrap();
    assert_eq!(row.get::<_, i64>(0), 2);
    // PostgreSQL sum(bigint) is numeric; cast the aggregate to its bounded test domain.
    assert_eq!(
        db.query_one(
            "SELECT sum(memory_bytes)::bigint FROM runtime.resident_sandboxes",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        (8 * GIB) as i64
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_foreign_host_sample_never_reserves_or_dispatches_creation() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_url, store, mut db) = fixture::setup();
    let run = run_scoped(
        &store,
        &mut db,
        "foreign-sample-run",
        "foreign-sample-session",
        "foreign-sample-environment",
        profile(),
    );
    let digest = run.original.environment_profile.digest().unwrap();
    let dispatched = std::sync::atomic::AtomicUsize::new(0);
    let result = store.prepare_retained_instance_with_inventory(
        &run.request("foreign-sample-execution", &digest),
        shared_limits(),
        || {
            Ok(ResidentInventory {
                sandboxes: Vec::new(),
                shared: Some(SharedHostInventory {
                    host_id: "foreign-host".into(),
                    available_memory_bytes: 10 * GIB,
                    memory_headroom_bytes: 2 * GIB,
                    valid_until: std::time::Instant::now() + std::time::Duration::from_secs(5),
                    measured_by_container: Default::default(),
                }),
            })
        },
        |_, _| {
            dispatched.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        },
        |_| {
            dispatched.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok("forbidden-container".into())
        },
    );
    assert!(result.is_err());
    assert_eq!(dispatched.load(std::sync::atomic::Ordering::SeqCst), 0);
    for table in [
        "execution_instances",
        "resident_sandboxes",
        "environment_creation_attempts",
    ] {
        assert_eq!(
            db.query_one(&format!("SELECT count(*) FROM runtime.{table}"), &[])
                .unwrap()
                .get::<_, i64>(0),
            0,
            "foreign sample must not create {table}"
        );
    }
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_expired_shared_sample_never_reserves_or_dispatches_creation() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_url, store, mut db) = fixture::setup();
    let run = run_scoped(
        &store,
        &mut db,
        "expired-run",
        "expired-session",
        "expired-environment",
        profile(),
    );
    let digest = run.original.environment_profile.digest().unwrap();
    let dispatched = std::sync::atomic::AtomicUsize::new(0);
    let result = store.prepare_retained_instance_with_inventory(
        &run.request("expired-execution", &digest),
        shared_limits(),
        || {
            Ok(ResidentInventory {
                sandboxes: Vec::new(),
                shared: Some(SharedHostInventory {
                    host_id: "host".into(),
                    available_memory_bytes: 10 * GIB,
                    memory_headroom_bytes: 2 * GIB,
                    valid_until: std::time::Instant::now() - std::time::Duration::from_millis(1),
                    measured_by_container: Default::default(),
                }),
            })
        },
        |_, _| {
            dispatched.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        },
        |_| {
            dispatched.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok("forbidden-container".into())
        },
    );
    assert_eq!(
        result.unwrap_err(),
        crate::resident_capacity::RESIDENT_CAPACITY_WAIT
    );
    assert_eq!(dispatched.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.resident_capacity_waiters WHERE job_id=$1",
            &[&run.fence.job_id]
        )
        .unwrap()
        .get::<_, i64>(0),
        1
    );
    for table in [
        "execution_instances",
        "resident_sandboxes",
        "environment_creation_attempts",
    ] {
        assert_eq!(
            db.query_one(&format!("SELECT count(*) FROM runtime.{table}"), &[])
                .unwrap()
                .get::<_, i64>(0),
            0,
            "expired sample must not create {table}"
        );
    }
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_low_host_headroom_blocks_new_run_reuse_without_changing_original_instance() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_url, store, mut db) = fixture::setup();
    let first = fixture::run(&store, &mut db, "reuse-origin-run", 1);
    let physical = Arc::new(Mutex::new(Vec::new()));
    let allocation = fixture::prepare(&store, &first, "reuse-origin-execution", &physical).unwrap();
    db.batch_execute("UPDATE public.app_core_agentrun SET status='completed'; UPDATE public.app_core_sessionenvironment SET controller_released=true").unwrap();
    let next = fixture::run(&store, &mut db, "reuse-demand-run", 2);
    let digest = next.original.environment_profile.digest().unwrap();
    let dispatched = std::sync::atomic::AtomicUsize::new(0);
    let result = store.prepare_retained_instance_with_inventory(
        &next.request("reuse-demand-execution", &digest),
        shared_limits(),
        || {
            Ok(ResidentInventory {
                sandboxes: physical.lock().unwrap().clone(),
                shared: Some(SharedHostInventory {
                    host_id: "host".into(),
                    available_memory_bytes: GIB,
                    memory_headroom_bytes: 2 * GIB,
                    valid_until: std::time::Instant::now() + std::time::Duration::from_secs(5),
                    measured_by_container: Default::default(),
                }),
            })
        },
        |_, _| {
            dispatched.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        },
        |_| {
            dispatched.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(allocation.container_id.clone().unwrap())
        },
    );
    assert_eq!(
        result.unwrap_err(),
        crate::resident_capacity::RESIDENT_CAPACITY_WAIT
    );
    assert_eq!(dispatched.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(physical.lock().unwrap().len(), 1);
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.resident_capacity_waiters WHERE job_id=$1",
            &[&next.fence.job_id]
        )
        .unwrap()
        .get::<_, i64>(0),
        1
    );
    let row = db
        .query_one(
            "SELECT container_id,state FROM runtime.resident_sandboxes",
            &[],
        )
        .unwrap();
    assert_eq!(row.get::<_, String>(0), allocation.container_id.unwrap());
    assert_eq!(row.get::<_, String>(1), "resident");
    assert_eq!(
        db.query_one("SELECT count(*) FROM runtime.execution_instances", &[])
            .unwrap()
            .get::<_, i64>(0),
        1,
        "rejected new Run must not acquire a new execution association"
    );
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.environment_creation_attempts",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        1
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_host_preparation_unlock_wakes_original_job_without_resource_release() {
    use centaeris_core::session::reliability::{
        ClaimDueRuntimeJobsRequest, RuntimeJobStorePort, YieldRuntimeJobRequest,
    };
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db) = fixture::setup();
    let run = run_scoped(
        &store,
        &mut db,
        "host-preparation-wait-run",
        "host-preparation-wait-session",
        "host-preparation-wait-environment",
        profile(),
    );
    let (entered, held) = std::sync::mpsc::channel();
    let (release, finish) = std::sync::mpsc::channel();
    let owner = store.clone();
    let holder = std::thread::spawn(move || {
        owner.reconcile_resident_sandboxes("host", move || {
            entered.send(()).unwrap();
            finish
                .recv_timeout(std::time::Duration::from_secs(10))
                .map_err(|_| "owned host preparation barrier timed out")?;
            Ok(Vec::new())
        })
    });
    held.recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    let digest = run.original.environment_profile.digest().unwrap();
    let rejected = store
        .prepare_retained_instance_with_inventory(
            &run.request("host-preparation-wait-execution", &digest),
            shared_limits(),
            || panic!("a competing preparation cannot inspect physical inventory"),
            |_, _| panic!("a competing preparation cannot publish execution"),
            |_| panic!("a competing preparation cannot dispatch creation"),
        )
        .unwrap_err();
    let now = |db: &mut Client| {
        db.query_one(
            "SELECT (EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint",
            &[],
        )
        .unwrap()
        .get::<_, i64>(0)
    };
    let yielded = now(&mut db);
    let future = yielded + 1_800_000;
    store
        .yield_runtime_job(YieldRuntimeJobRequest {
            job_id: run.fence.job_id.clone(),
            lease_owner: run.fence.lease_owner.clone(),
            yielded_at_ms: yielded,
            run_at_ms: future,
            transition_reason: rejected.clone(),
        })
        .unwrap();
    let request = || ClaimDueRuntimeJobsRequest {
        now_ms: now(&mut Client::connect(&url, super::NoTls).unwrap()),
        worker_id: "host-preparation-claim-observer".into(),
        job_id: Some(run.fence.job_id.clone()),
        job_kind: Some("agent_run.lifecycle".into()),
        session_id: None,
        limit: 1,
        lease_ms: 60_000,
    };
    assert!(store.claim_worker_jobs(request()).unwrap().is_empty());
    release.send(()).unwrap();
    holder.join().unwrap().unwrap();
    let reopened = PostgresRuntimeStore::new(&url).unwrap();
    let claimed = reopened.claim_worker_jobs(request()).unwrap();
    assert_eq!(claimed.len(),1,"host preparation unlock must make the original waiter eligible without a pause or resource release");
    assert_eq!(claimed[0].job_id, run.fence.job_id);
    assert_eq!(claimed[0].retry_count, 0);
    assert!(now(&mut db) < future);
    assert_eq!(rejected, "execution_host_preparation_wait");
    assert_eq!(
        db.query_one("SELECT count(*) FROM runtime.resident_sandboxes", &[])
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    assert_eq!(
        db.query_one("SELECT count(*) FROM runtime.execution_instances", &[])
            .unwrap()
            .get::<_, i64>(0),
        0
    );
}

fn hold_host_preparation(
    store: &PostgresRuntimeStore,
) -> (
    std::sync::mpsc::Sender<()>,
    std::thread::JoinHandle<Result<(), String>>,
) {
    hold_host_preparation_with_inventory(store, Vec::new())
}

fn hold_host_preparation_with_inventory(
    store: &PostgresRuntimeStore,
    inventory: Vec<ObservedResidentSandbox>,
) -> (
    std::sync::mpsc::Sender<()>,
    std::thread::JoinHandle<Result<(), String>>,
) {
    let (entered, held) = std::sync::mpsc::channel();
    let (release, finish) = std::sync::mpsc::channel();
    let owner = store.clone();
    let holder = std::thread::spawn(move || {
        owner.reconcile_resident_sandboxes("host", move || {
            entered.send(()).unwrap();
            finish
                .recv_timeout(std::time::Duration::from_secs(10))
                .map_err(|_| "owned preparation hold timed out")?;
            Ok(inventory)
        })
    });
    held.recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    (release, holder)
}
fn preparation_now(db: &mut Client) -> i64 {
    db.query_one(
        "SELECT (EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint",
        &[],
    )
    .unwrap()
    .get(0)
}
fn preparation_claim(
    store: &PostgresRuntimeStore,
    db: &mut Client,
    run: &fixture::Run,
) -> Vec<centaeris_core::session::reliability::RuntimeJobRecord> {
    store
        .claim_worker_jobs(
            centaeris_core::session::reliability::ClaimDueRuntimeJobsRequest {
                now_ms: preparation_now(db),
                worker_id: "preparation-wake-observer".into(),
                job_id: Some(run.fence.job_id.clone()),
                job_kind: Some("agent_run.lifecycle".into()),
                session_id: None,
                limit: 1,
                lease_ms: 60_000,
            },
        )
        .unwrap()
}
fn preparation_yield(
    store: &PostgresRuntimeStore,
    db: &mut Client,
    run: &fixture::Run,
    reason: &str,
) {
    use centaeris_core::session::reliability::RuntimeJobStorePort;
    let now = preparation_now(db);
    store
        .yield_runtime_job(
            centaeris_core::session::reliability::YieldRuntimeJobRequest {
                job_id: run.fence.job_id.clone(),
                lease_owner: run.fence.lease_owner.clone(),
                yielded_at_ms: now,
                run_at_ms: now + 1_800_000,
                transition_reason: reason.into(),
            },
        )
        .unwrap();
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_preparation_unlock_before_registration_rechecks_instead_of_losing_wake() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_url, store, mut db) = fixture::setup();
    let run = run_scoped(
        &store,
        &mut db,
        "registration-race-run",
        "registration-race-session",
        "registration-race-environment",
        profile(),
    );
    let generation_before: i64 = db
        .query_one(
            "SELECT generation FROM runtime.resident_capacity_clock",
            &[],
        )
        .unwrap()
        .get(0);
    let job_before = db
        .query_one(
            "SELECT status,lease_owner,retry_count FROM runtime.runtime_jobs WHERE job_id=$1",
            &[&run.fence.job_id],
        )
        .unwrap();
    let (release, holder) = hold_host_preparation(&store);
    crate::postgres_store::residency::before_preparation_wait_registration(move || {
        release.send(()).unwrap();
        holder.join().unwrap().unwrap();
    });
    let digest = run.original.environment_profile.digest().unwrap();
    let dispatched = std::sync::atomic::AtomicUsize::new(0);
    let result = store
        .prepare_retained_instance_with_inventory(
            &run.request("registration-race-execution", &digest),
            shared_limits(),
            || Ok(ResidentInventory::declarations(Vec::new())),
            |_, _| Ok(()),
            |_| {
                dispatched.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok("registration-race-container".into())
            },
        )
        .unwrap();
    assert_eq!(
        result.container_id.as_deref(),
        Some("registration-race-container")
    );
    assert_eq!(dispatched.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.resident_capacity_waiters",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        0
    );
    assert_eq!(
        db.query_one(
            "SELECT generation FROM runtime.resident_capacity_clock",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        generation_before + 1,
        "successful first confirmation advances eligibility exactly once"
    );
    assert!(
        preparation_claim(&store, &mut db, &run).is_empty(),
        "confirmation eligibility cannot reclaim the still-active original Job"
    );
    let job = db
        .query_one(
            "SELECT status,lease_owner,retry_count FROM runtime.runtime_jobs WHERE job_id=$1",
            &[&run.fence.job_id],
        )
        .unwrap();
    assert_eq!(job.get::<_, String>(0), job_before.get::<_, String>(0));
    assert_eq!(
        job.get::<_, Option<String>>(1),
        job_before.get::<_, Option<String>>(1)
    );
    assert_eq!(job.get::<_, i64>(2), job_before.get::<_, i64>(2));
    assert_eq!(db.query_one("SELECT count(*) FROM runtime.runtime_events WHERE task_id=$1 AND event_type='runtime.resident_capacity.claimed.v1'", &[&run.fence.job_id]).unwrap().get::<_,i64>(0), 0);
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_preparation_unlock_before_yield_is_durable_and_contention_fact_is_idempotent() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_url, store, mut db) = fixture::setup();
    let run = run_scoped(
        &store,
        &mut db,
        "yield-race-run",
        "yield-race-session",
        "yield-race-environment",
        profile(),
    );
    let (release, holder) = hold_host_preparation(&store);
    let digest = run.original.environment_profile.digest().unwrap();
    for _ in 0..2 {
        let reason = store
            .prepare_retained_instance_with_inventory(
                &run.request("yield-race-execution", &digest),
                shared_limits(),
                || panic!("busy cannot collect"),
                |_, _| panic!("busy cannot publish"),
                |_| panic!("busy cannot create"),
            )
            .unwrap_err();
        assert_eq!(reason, crate::resident_capacity::HOST_PREPARATION_WAIT);
    }
    assert_eq!(db.query_one("SELECT count(*) FROM runtime.runtime_events WHERE event_type='runtime.host_preparation.waiting.v1'",&[]).unwrap().get::<_,i64>(0),1);
    release.send(()).unwrap();
    holder.join().unwrap().unwrap();
    preparation_yield(
        &store,
        &mut db,
        &run,
        crate::resident_capacity::HOST_PREPARATION_WAIT,
    );
    let claim = preparation_claim(&store, &mut db, &run);
    assert_eq!(claim.len(), 1);
    assert_eq!(claim[0].retry_count, 0);
    let evidence:serde_json::Value=serde_json::from_str(&db.query_one("SELECT payload_json FROM runtime.runtime_events WHERE event_type='runtime.host_preparation.available.v1'",&[]).unwrap().get::<_,String>(0)).unwrap();
    assert_eq!(evidence["hostId"], "host");
    assert_eq!(
        evidence["generation"].as_i64(),
        Some(
            db.query_one(
                "SELECT generation FROM runtime.resident_capacity_clock",
                &[]
            )
            .unwrap()
            .get::<_, i64>(0)
        )
    );
    assert!(evidence["availableAtMs"].as_i64().is_some());
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_preparation_wake_excludes_cancelled_withdrawn_and_fenced_waiters() {
    use centaeris_core::session::reliability::{CancelRuntimeJobRequest, RuntimeJobStorePort};
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    for condition in ["cancelled", "withdrawn", "fenced"] {
        let (_url, store, mut db) = fixture::setup();
        let run = run_scoped(
            &store,
            &mut db,
            "stale-wait-run",
            "stale-wait-session",
            "stale-wait-environment",
            profile(),
        );
        let (release, holder) = hold_host_preparation(&store);
        let digest = run.original.environment_profile.digest().unwrap();
        let request = run.request("stale-wait-execution", &digest);
        let reason = store
            .prepare_retained_instance_with_inventory(
                &request,
                shared_limits(),
                || panic!("busy cannot collect"),
                |_, _| panic!("busy cannot publish"),
                |_| panic!("busy cannot create"),
            )
            .unwrap_err();
        assert_eq!(reason, crate::resident_capacity::HOST_PREPARATION_WAIT);
        match condition {
            "cancelled" => {
                let now = preparation_now(&mut db);
                store
                    .cancel_runtime_job(CancelRuntimeJobRequest {
                        expected_lease_owner: Some(run.fence.lease_owner.clone()),
                        job_id: run.fence.job_id.clone(),
                        reason: "cancelled by owned test".into(),
                        cancelled_at_ms: now,
                        expected_status: Some(RuntimeJobStatus::Running),
                    })
                    .unwrap();
            }
            "withdrawn" => {
                crate::postgres_store::capacity_wait::clear(&mut db, &run.fence).unwrap()
            }
            "fenced" => {
                db.execute("UPDATE runtime.runtime_jobs SET lease_owner='superseding-owner' WHERE job_id=$1",&[&run.fence.job_id]).unwrap();
            }
            _ => preparation_yield(&store, &mut db, &run, &reason),
        }
        release.send(()).unwrap();
        holder.join().unwrap().unwrap();
        assert_eq!(
            db.query_one(
                "SELECT generation FROM runtime.resident_capacity_clock",
                &[]
            )
            .unwrap()
            .get::<_, i64>(0),
            0,
            "{condition} cannot cause a preparation wake"
        );
        assert_eq!(db.query_one("SELECT count(*) FROM runtime.runtime_events WHERE event_type='runtime.host_preparation.available.v1'",&[]).unwrap().get::<_,i64>(0),0);
        assert_eq!(
            db.query_one("SELECT count(*) FROM runtime.resident_sandboxes", &[])
                .unwrap()
                .get::<_, i64>(0),
            0
        );
    }
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_real_resident_pressure_does_not_wake_itself_on_preparation_unlock() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_url, store, mut db) = fixture::setup();
    let origin = fixture::run(&store, &mut db, "pressure-origin", 1);
    let physical = Arc::new(Mutex::new(Vec::new()));
    fixture::prepare(&store, &origin, "pressure-origin-execution", &physical).unwrap();
    let run = run_scoped(
        &store,
        &mut db,
        "pressure-demand",
        "pressure-session",
        "pressure-environment",
        origin.original.environment_profile.clone(),
    );
    let digest = run.original.environment_profile.digest().unwrap();
    let before = db
        .query_one(
            "SELECT generation FROM runtime.resident_capacity_clock",
            &[],
        )
        .unwrap()
        .get::<_, i64>(0);
    let events_before: i64 = db.query_one("SELECT count(*) FROM runtime.runtime_events WHERE event_type IN('runtime.host_preparation.waiting.v1','runtime.host_preparation.available.v1')", &[]).unwrap().get(0);
    let reason = store
        .prepare_retained_instance_with_inventory(
            &run.request("pressure-demand-execution", &digest),
            fixture::limits(&origin),
            || {
                Ok(ResidentInventory::declarations(
                    physical.lock().unwrap().clone(),
                ))
            },
            |_, _| panic!("full cannot publish"),
            |_| panic!("full cannot create"),
        )
        .unwrap_err();
    assert_eq!(reason, crate::resident_capacity::RESIDENT_CAPACITY_WAIT);
    preparation_yield(&store, &mut db, &run, &reason);
    assert!(preparation_claim(&store, &mut db, &run).is_empty());
    assert_eq!(
        db.query_one(
            "SELECT generation FROM runtime.resident_capacity_clock",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        before
    );
    assert_eq!(db.query_one("SELECT count(*) FROM runtime.runtime_events WHERE event_type IN('runtime.host_preparation.waiting.v1','runtime.host_preparation.available.v1')",&[]).unwrap().get::<_,i64>(0),events_before);
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.resident_sandboxes WHERE state='resident'",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        1
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_global_preparation_unlock_wakes_legitimate_waiter_on_another_host() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_url, store, mut db) = fixture::setup();
    let run = run_scoped(
        &store,
        &mut db,
        "other-host-run",
        "other-host-session",
        "other-host-environment",
        profile(),
    );
    let (release, holder) = hold_host_preparation(&store);
    let digest = run.original.environment_profile.digest().unwrap();
    let mut request = run.request("other-host-execution", &digest);
    request.host_id = "other-host";
    let reason = store
        .prepare_retained_instance_with_inventory(
            &request,
            shared_limits(),
            || panic!("busy cannot collect"),
            |_, _| panic!("busy cannot publish"),
            |_| panic!("busy cannot create"),
        )
        .unwrap_err();
    preparation_yield(&store, &mut db, &run, &reason);
    release.send(()).unwrap();
    holder.join().unwrap().unwrap();
    assert_eq!(preparation_claim(&store, &mut db, &run).len(), 1);
    let evidence:serde_json::Value=serde_json::from_str(&db.query_one("SELECT payload_json FROM runtime.runtime_events WHERE event_type='runtime.host_preparation.available.v1'",&[]).unwrap().get::<_,String>(0)).unwrap();
    assert_eq!(evidence["hostId"], "host");
    assert_eq!(evidence["scope"], "globalPreparation");
    assert_eq!(
        db.query_one("SELECT count(*) FROM runtime.resident_sandboxes", &[])
            .unwrap()
            .get::<_, i64>(0),
        0
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_mixed_preparation_and_pressure_wake_rechecks_budget_without_self_loop() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_url, store, mut db) = fixture::setup();
    let origin = fixture::run(&store, &mut db, "mixed-origin", 1);
    let physical = Arc::new(Mutex::new(Vec::new()));
    fixture::prepare(&store, &origin, "mixed-origin-execution", &physical).unwrap();
    let mut pressure = run_scoped(
        &store,
        &mut db,
        "mixed-pressure",
        "mixed-pressure-session",
        "mixed-pressure-environment",
        origin.original.environment_profile.clone(),
    );
    let digest = pressure.original.environment_profile.digest().unwrap();
    let try_pressure = |run: &fixture::Run| {
        store.prepare_retained_instance_with_inventory(
            &run.request("mixed-pressure-execution", &digest),
            fixture::limits(&origin),
            || {
                Ok(ResidentInventory::declarations(
                    physical.lock().unwrap().clone(),
                ))
            },
            |_, _| panic!("pressure cannot publish"),
            |_| panic!("pressure cannot create"),
        )
    };
    let reason = try_pressure(&pressure).unwrap_err();
    assert_eq!(reason, crate::resident_capacity::RESIDENT_CAPACITY_WAIT);
    preparation_yield(&store, &mut db, &pressure, &reason);
    let waiting = run_scoped(
        &store,
        &mut db,
        "mixed-preparation",
        "mixed-preparation-session",
        "mixed-preparation-environment",
        profile(),
    );
    let (release, holder) =
        hold_host_preparation_with_inventory(&store, physical.lock().unwrap().clone());
    let waiting_digest = waiting.original.environment_profile.digest().unwrap();
    let reason = store
        .prepare_retained_instance_with_inventory(
            &waiting.request("mixed-preparation-execution", &waiting_digest),
            shared_limits(),
            || panic!("busy cannot collect"),
            |_, _| panic!("busy cannot publish"),
            |_| panic!("busy cannot create"),
        )
        .unwrap_err();
    preparation_yield(&store, &mut db, &waiting, &reason);
    release.send(()).unwrap();
    holder.join().unwrap().unwrap();
    assert_eq!(preparation_claim(&store, &mut db, &waiting).len(), 1);
    let claim = preparation_claim(&store, &mut db, &pressure);
    assert_eq!(
        claim.len(),
        1,
        "global readiness may ask a pressure waiter to recheck; it grants no resident budget"
    );
    pressure.fence.lease_owner = claim[0].lease_owner.clone().unwrap();
    let mut binding = pressure.grant.payload.clone();
    let controller = binding.controller.as_mut().unwrap();
    controller.controller_epoch += 1;
    let mut hash = Sha256::new();
    hash.update(b"workspace:environment-controller-lease:v1\0");
    hash.update(pressure.fence.lease_owner.as_bytes());
    controller.controller_lease_digest = format!("sha256:{:x}", hash.finalize());
    db.execute("UPDATE public.app_core_sessionenvironment SET controller_epoch=$2,controller_lease_owner=$3 WHERE id=$1",&[&binding.environment_id,&controller.controller_epoch,&pressure.fence.lease_owner]).unwrap();
    pressure.grant = fixture::sign(binding);
    let before = db
        .query_one(
            "SELECT generation FROM runtime.resident_capacity_clock",
            &[],
        )
        .unwrap()
        .get::<_, i64>(0);
    let reason = try_pressure(&pressure).unwrap_err();
    assert_eq!(reason, crate::resident_capacity::RESIDENT_CAPACITY_WAIT);
    preparation_yield(&store, &mut db, &pressure, &reason);
    assert!(preparation_claim(&store, &mut db, &pressure).is_empty());
    assert_eq!(
        db.query_one(
            "SELECT generation FROM runtime.resident_capacity_clock",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        before
    );
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.resident_sandboxes WHERE state='resident'",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        1
    );
    assert_eq!(
        db.query_one("SELECT count(*) FROM runtime.execution_instances", &[])
            .unwrap()
            .get::<_, i64>(0),
        1
    );
}
