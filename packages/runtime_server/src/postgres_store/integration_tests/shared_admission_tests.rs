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
