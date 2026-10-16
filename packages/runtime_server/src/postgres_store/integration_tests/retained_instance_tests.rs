use super::{reset_store, test_url, PostgresRuntimeStore, TEST_LOCK};
use crate::agent_run_authorization::WorkspaceAgentRunAuthorization;
use crate::environment_authorization::{
    ControllerBinding, EnvironmentAuthorization, EnvironmentBinding,
};
use crate::postgres_store::execution_instances::{
    RetainedInstanceAllocation, RetainedInstanceRequest,
};
use crate::resident_capacity::{declared_usage, ObservedResidentSandbox, ResidentCapacity};
use centaeris_core::session::reliability::{RuntimeJobStatus, ScheduleRuntimeJobRequest};
use centaeris_core::session::RuntimeJobLeaseFence;
use hmac::{Hmac, Mac};
use postgres::{Client, NoTls};
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex};

const KEY: &[u8] = b"retained-test-key";
mod allocation_mode_tests;
mod cleanup_authority_tests;
mod cleanup_receipt_tests;
mod lease_contention_tests;
mod pre_admission_failure_tests;
mod retirement_tests;
fn controller_lease_digest(owner: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(b"workspace:environment-controller-lease:v1\0");
    hash.update(owner.as_bytes());
    format!("sha256:{:x}", hash.finalize())
}
fn sign(payload: EnvironmentBinding) -> EnvironmentAuthorization {
    let mut mac = Hmac::<Sha256>::new_from_slice(KEY).unwrap();
    mac.update(b"workspace:environment-authorization:v2\0");
    mac.update(payload.digest().unwrap().as_bytes());
    let signature = format!("hmac-sha256:{:x}", mac.finalize().into_bytes());
    EnvironmentAuthorization { payload, signature }
}
struct Run {
    original: WorkspaceAgentRunAuthorization,
    grant: EnvironmentAuthorization,
    fence: RuntimeJobLeaseFence,
}
impl Run {
    fn request<'a>(
        &'a self,
        execution_id: &'a str,
        profile_digest: &'a str,
    ) -> RetainedInstanceRequest<'a> {
        RetainedInstanceRequest {
            environment_authorization: &self.grant,
            signing_key: KEY,
            execution_id,
            workspace_id: &self.original.workspace_id,
            host_id: "host",
            image_digest: &self.original.image_digest,
            profile_digest,
            resources: declared_usage(self.original.resources),
            lease: &self.fence,
        }
    }
}
fn require_owned_database(db: &mut Client) {
    super::owned_database::require_owned_database(db);
}
fn setup() -> (String, PostgresRuntimeStore, Client) {
    let url = test_url();
    let mut db = Client::connect(&url, NoTls).unwrap();
    require_owned_database(&mut db);
    reset_store(&url);
    db.batch_execute("DROP TABLE IF EXISTS public.app_core_sessionevent,public.app_core_agentrunauthorization,public.app_core_sessionenvironment,public.app_core_session,public.app_core_workspacemembership,public.app_core_workspace CASCADE;
        ALTER TABLE public.app_core_agentrun ADD COLUMN IF NOT EXISTS environment_id text, ADD COLUMN IF NOT EXISTS workspace_id text, ADD COLUMN IF NOT EXISTS user_id text, ADD COLUMN IF NOT EXISTS membership_ref text, ADD COLUMN IF NOT EXISTS status text, ADD COLUMN IF NOT EXISTS \"modelConfig_id\" text, ADD COLUMN IF NOT EXISTS \"thinkingMode\" text;
        CREATE TABLE public.app_core_workspace(id text PRIMARY KEY,status text); INSERT INTO public.app_core_workspace VALUES('ws_1','active');
        CREATE TABLE public.app_core_workspacemembership(id text PRIMARY KEY,workspace_id text,user_id text,role text); INSERT INTO public.app_core_workspacemembership VALUES('member','ws_1','user_1','member');
        CREATE TABLE public.app_core_session(id text PRIMARY KEY,workspace_id text,owner_id text,agent_id text,current_environment_id text,\"deletedAt\" timestamptz); INSERT INTO public.app_core_session VALUES('sess_1','ws_1','user_1','centaeris','environment',NULL);
        CREATE TABLE public.app_core_sessionenvironment(id text PRIMARY KEY,session_id text,controller_run_id text,controller_epoch bigint,controller_lease_owner text,controller_purpose text,controller_released boolean); INSERT INTO public.app_core_sessionenvironment VALUES('environment','sess_1',NULL,0,'','',true);
        CREATE TABLE public.app_core_agentrunauthorization(agent_run_id text PRIMARY KEY,digest text,payload jsonb,signature text);
        CREATE TABLE public.app_core_sessionevent(workspace_id text,session_id text,agent_run_id text,session_level boolean,payload jsonb);").unwrap();
    let store = PostgresRuntimeStore::new(&url).unwrap();
    (url, store, db)
}
fn run(store: &PostgresRuntimeStore, db: &mut Client, id: &str, epoch: i64) -> Run {
    let mut original: WorkspaceAgentRunAuthorization = serde_json::from_str(include_str!(
        "../../../../../tests/workspace/fixtures/agent_run_authorization/v1/valid.json"
    ))
    .unwrap();
    original.agent_run_id = id.into();
    original.id = format!("authorization-{id}");
    let digest = original.digest().unwrap();
    let owner = format!("owner-{id}");
    db.execute("INSERT INTO public.app_core_agentrun(id,session_id,environment_id,workspace_id,user_id,membership_ref,status,\"modelConfig_id\",\"thinkingMode\") VALUES($1,'sess_1','environment','ws_1','user_1','member','running','model_1','high')", &[&id]).unwrap();
    db.execute(
        "INSERT INTO public.app_core_agentrunauthorization VALUES($1,$2,$3::text::jsonb,$4)",
        &[
            &id,
            &digest,
            &serde_json::to_string(&original).unwrap(),
            &original.signature(KEY).unwrap(),
        ],
    )
    .unwrap();
    db.execute("UPDATE public.app_core_sessionenvironment SET controller_run_id=$1,controller_epoch=$2,controller_lease_owner=$3,controller_purpose='execute',controller_released=false", &[&id,&epoch,&owner]).unwrap();
    let job_id = format!("agent_run.lifecycle:{id}");
    let mut job = super::job(&job_id, &format!("{job_id}:{digest}"));
    job.job_kind = "agent_run.lifecycle".into();
    job.status = RuntimeJobStatus::Running;
    job.lease_owner = Some(owner.clone());
    job.lease_expires_at_ms = Some(4_000_000_000_000);
    job.session_id = Some("sess_1".into());
    job.payload_ref = Some(format!("record:agent_run:{id}"));
    store
        .schedule_worker_job(ScheduleRuntimeJobRequest { job }, Some("ws_1"))
        .unwrap();
    let fence = RuntimeJobLeaseFence {
        job_id,
        job_kind: "agent_run.lifecycle".into(),
        lease_owner: owner.clone(),
    };
    let grant = sign(EnvironmentBinding {
        schema: "workspace.environment_authorization.v2".into(),
        environment_id: "environment".into(),
        session_id: "sess_1".into(),
        agent_run_id: id.into(),
        agent_run_authorization_digest: digest,
        execution_mode: "retained".into(),
        controller: Some(ControllerBinding {
            controller_id: id.into(),
            controller_epoch: epoch,
            controller_lease_digest: controller_lease_digest(&owner),
            controller_purpose: "execute".into(),
        }),
    });
    Run {
        original,
        grant,
        fence,
    }
}
fn limits(run: &Run) -> ResidentCapacity {
    let one = declared_usage(run.original.resources);
    ResidentCapacity {
        global: one,
        tenant: one,
        host: one,
    }
}
fn prepare(
    store: &PostgresRuntimeStore,
    run: &Run,
    execution: &str,
    inventory: &Arc<Mutex<Vec<ObservedResidentSandbox>>>,
) -> Result<RetainedInstanceAllocation, String> {
    let profile = format!("sha256:{}", "b".repeat(64));
    store.prepare_retained_instance(
        &run.request(execution, &profile),
        limits(run),
        || Ok(inventory.lock().unwrap().clone()),
        |_, _| Ok(()),
        |allocation| {
            let mut physical = inventory.lock().unwrap();
            if allocation.reused {
                assert_eq!(physical[0].execution_id, allocation.allocation_execution_id);
                return Ok(physical[0].container_id.clone());
            }
            let container = format!("container-{}", allocation.instance_id);
            physical.push(ObservedResidentSandbox {
                execution_id: allocation.allocation_execution_id.clone(),
                agent_run_id: allocation.allocation_agent_run_id.clone(),
                container_id: container.clone(),
                resources: declared_usage(run.original.resources),
            });
            Ok(container)
        },
    )
}
#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_retained_new_run_reuses_origin_and_single_resource_holding() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db) = setup();
    let a = run(&store, &mut db, "run-a", 1);
    let physical = Arc::new(Mutex::new(Vec::new()));
    let first = prepare(&store, &a, "execution-a", &physical).unwrap();
    assert!(!first.reused);
    db.batch_execute("UPDATE public.app_core_agentrun SET status='completed' WHERE id='run-a'; UPDATE public.app_core_sessionenvironment SET controller_released=true;").unwrap();
    // Foreground completion leaves the physical resident and its resource debit.
    assert_eq!(
        db.query_one("SELECT count(*) FROM runtime.resident_sandboxes", &[])
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    let b = run(&store, &mut db, "run-b", 2);
    let second = prepare(&store, &b, "execution-b", &physical).unwrap();
    assert!(second.reused);
    assert_eq!(second.instance_id, first.instance_id);
    assert_eq!(second.allocation_execution_id, "execution-a");
    assert_eq!(second.allocation_agent_run_id, "run-a");
    assert_eq!(second.authorization_digest, first.authorization_digest);
    assert_eq!(second.binding_digest, first.binding_digest);
    let mut observer = Client::connect(&url, NoTls).unwrap();
    assert_eq!(
        observer
            .query_one(
                "SELECT count(*),sum(memory_bytes) FROM runtime.resident_sandboxes",
                &[]
            )
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    assert_eq!(
        observer
            .query_one(
                "SELECT count(*) FROM runtime.execution_instances WHERE instance_id=$1",
                &[&first.instance_id]
            )
            .unwrap()
            .get::<_, i64>(0),
        2
    );
    assert!(prepare(&store, &a, "stale-a", &physical).is_err());
    assert!(store
        .remove_resident_sandboxes(
            "host",
            "run-a",
            &a.fence,
            || panic!("legacy removal cannot touch retained"),
            || Ok(physical.lock().unwrap().clone())
        )
        .is_err());
    assert!(store
        .with_resident_execution_restore("host", "run-b", "execution-b", &b.fence, || panic!(
            "legacy restore cannot touch retained"
        ))
        .is_err());
    assert_eq!(physical.lock().unwrap().len(), 1);
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_retained_core_terminal_rejects_execution_before_status_projection() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_url, store, mut db) = setup();
    let a = run(&store, &mut db, "run-a", 1);
    let profile = format!("sha256:{}", "b".repeat(64));
    for terminal in [
        "agent_run_completed",
        "agent_run_failed",
        "agent_run_interrupted",
    ] {
        db.execute("INSERT INTO public.app_core_sessionevent VALUES('ws_1','sess_1','run-a',false,jsonb_build_object('type',$1::text))",&[&terminal]).unwrap();
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let result = store.prepare_retained_instance(
            &a.request("execution-a", &profile),
            limits(&a),
            || Ok(Vec::new()),
            |_, _| Ok(()),
            |_| {
                calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok("prohibited".into())
            },
        );
        assert!(
            result.is_err(),
            "Core {terminal} is authoritative while status is running"
        );
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(
            db.query_one("SELECT count(*) FROM runtime.resident_sandboxes", &[])
                .unwrap()
                .get::<_, i64>(0),
            0
        );
        db.batch_execute("DELETE FROM public.app_core_sessionevent")
            .unwrap();
    }
    db.batch_execute("UPDATE public.app_core_agentrun SET \"preAdmissionCancelledAt\"=clock_timestamp() WHERE id='run-a'").unwrap();
    assert!(store
        .prepare_retained_instance(
            &a.request("execution-a", &profile),
            limits(&a),
            || Ok(Vec::new()),
            |_, _| panic!("pre-admission cancellation"),
            |_| panic!("pre-admission cancellation cannot create")
        )
        .is_err());
    db.batch_execute("UPDATE public.app_core_agentrun SET \"preAdmissionCancelledAt\"=NULL; INSERT INTO public.app_core_sessionevent VALUES('ws_1','sess_1','run-a',true,'{\"type\":\"agent_run_completed\"}'),('ws_1','sess_1','other-run',false,'{\"type\":\"agent_run_completed\"}')").unwrap();
    let physical = Arc::new(Mutex::new(Vec::new()));
    assert!(
        prepare(&store, &a, "execution-a", &physical).is_ok(),
        "Session-level/foreign-Run records do not terminate this Run"
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_retained_unknown_and_legacy_per_run_holding_block_creation() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    for known in [false, true] {
        let (_url, store, mut db) = setup();
        let a = run(&store, &mut db, "run-a", 1);
        if known {
            db.batch_execute("INSERT INTO runtime.execution_instances(execution_id,instance_id,environment_id,session_id,agent_run_id,host_id,authorization_digest,binding_digest,image_digest,allocation_execution_id) VALUES('legacy','legacy-instance','environment','sess_1','run-a','host','grant','binding','image','legacy')").unwrap();
        }
        db.batch_execute("INSERT INTO runtime.resident_sandboxes(host_id,execution_id,agent_run_id,workspace_id,state,memory_bytes,cpu_milli,pids,workspace_bytes,created_at_ms,updated_at_ms) VALUES('host','legacy','run-a','ws_1','reserved',1,1,1,1,1,1)").unwrap();
        let profile = format!("sha256:{}", "b".repeat(64));
        let error = store
            .prepare_retained_instance(
                &a.request("new-execution", &profile),
                ResidentCapacity::default(),
                || Ok(Vec::new()),
                |_, _| panic!("existing unknown writer blocks"),
                |_| panic!("existing writer blocks physical create"),
            )
            .unwrap_err();
        assert!(error.contains("unresolved reservation"), "{error}");
        assert_eq!(
            db.query_one("SELECT count(*) FROM runtime.resident_sandboxes", &[])
                .unwrap()
                .get::<_, i64>(0),
            1
        );
    }
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_retained_confirmed_lost_environment_never_cold_creates_for_new_run() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_url, store, mut db) = setup();
    let a = run(&store, &mut db, "run-a", 1);
    let physical = Arc::new(Mutex::new(Vec::new()));
    prepare(&store, &a, "execution-a", &physical).unwrap();
    db.batch_execute("UPDATE public.app_core_agentrun SET status='completed' WHERE id='run-a'; UPDATE public.app_core_sessionenvironment SET controller_released=true").unwrap();
    let b = run(&store, &mut db, "run-b", 2);
    physical.lock().unwrap().clear();
    let profile = format!("sha256:{}", "b".repeat(64));
    let calls = std::sync::atomic::AtomicUsize::new(0);
    let result = store.prepare_retained_instance(
        &b.request("execution-b", &profile),
        limits(&b),
        || Ok(Vec::new()),
        |_, _| Ok(()),
        |_| {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok("prohibited-cold-instance".into())
        },
    );
    assert!(
        result.is_err(),
        "a lost retained environment cannot silently resume from an old snapshot"
    );
    assert!(result
        .unwrap_err()
        .starts_with("execution_environment_lost:"));
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(
        db.query_one("SELECT count(*) FROM runtime.execution_instances", &[])
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    assert_eq!(db.query_one("SELECT count(*) FROM runtime.resident_sandboxes",&[]).unwrap().get::<_,i64>(0),0,"confirmed physical absence releases the old budget; rejected cold creation reserves nothing");
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_retained_unknown_create_survives_restart_without_second_writer() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db) = setup();
    let a = run(&store, &mut db, "run-a", 1);
    let profile = format!("sha256:{}", "b".repeat(64));
    let r = a.request("execution-a", &profile);
    let error = store
        .prepare_retained_instance(
            &r,
            limits(&a),
            || Ok(Vec::new()),
            |_, allocation| {
                let mut observer = Client::connect(&url, NoTls).unwrap();
                assert_eq!(
                    observer
                        .query_one("SELECT count(*) FROM runtime.execution_instances", &[])
                        .unwrap()
                        .get::<_, i64>(0),
                    1
                );
                assert_eq!(
                    observer
                        .query_one(
                            "SELECT state FROM runtime.resident_sandboxes WHERE execution_id=$1",
                            &[&allocation.allocation_execution_id]
                        )
                        .unwrap()
                        .get::<_, String>(0),
                    "reserved"
                );
                Ok(())
            },
            |_| Err("unknown Docker outcome".into()),
        )
        .unwrap_err();
    assert_eq!(error, "unknown Docker outcome");
    drop(store);
    let reopened = PostgresRuntimeStore::new(&url).unwrap();
    assert!(reopened
        .prepare_retained_instance(
            &r,
            limits(&a),
            || Ok(Vec::new()),
            |_, _| panic!("unknown reservation blocks fact replay"),
            |_| panic!("unknown reservation blocks second writer")
        )
        .is_err());
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.resident_sandboxes WHERE state='reserved'",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        1
    );
    // A later complete inventory positively identifies the first creation.
    let physical = Arc::new(Mutex::new(vec![ObservedResidentSandbox {
        execution_id: "execution-a".into(),
        agent_run_id: "run-a".into(),
        container_id: "late-ack-container".into(),
        resources: r.resources,
    }]));
    let adopted = prepare(&reopened, &a, "execution-a", &physical).unwrap();
    assert!(adopted.reused);
    assert_eq!(adopted.container_id.as_deref(), Some("late-ack-container"));
    assert_eq!(
        db.query_one("SELECT count(*) FROM runtime.resident_sandboxes", &[])
            .unwrap()
            .get::<_, i64>(0),
        1
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_retained_fact_window_rechecks_epoch_and_job_before_physical_work() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    for mutation in [
        "UPDATE public.app_core_sessionenvironment SET controller_epoch=controller_epoch+1",
        "UPDATE runtime.runtime_jobs SET lease_owner='new-worker'",
        "INSERT INTO public.app_core_sessionevent VALUES('ws_1','sess_1','run-a',false,'{\"type\":\"agent_run_completed\"}')",
    ] {
        let (_url, store, mut db) = setup();
        let a = run(&store, &mut db, "run-a", 1);
        let profile = format!("sha256:{}", "b".repeat(64));
        let error = store
            .prepare_retained_instance(
                &a.request("execution-a", &profile),
                limits(&a),
                || Ok(Vec::new()),
                |client, _| client.batch_execute(mutation).map_err(|e| e.to_string()),
                |_| panic!("authority changed after Core fact; no physical work"),
            )
            .unwrap_err();
        assert!(error.contains("fence") || error.contains("Core terminal"), "{error}");
        assert_eq!(
            db.query_one("SELECT count(*) FROM runtime.execution_instances", &[])
                .unwrap()
                .get::<_, i64>(0),
            1
        );
        assert_eq!(db.query_one("SELECT count(*) FROM runtime.resident_sandboxes WHERE state='reserved' AND container_id IS NULL",&[]).unwrap().get::<_,i64>(0),1);
    }
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_retained_admission_rejects_stale_grant_tenant_membership_and_configuration() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_url, store, mut db) = setup();
    let mut a = run(&store, &mut db, "run-a", 1);
    let physical = Arc::new(Mutex::new(Vec::new()));
    for (mutation,restore) in [
        ("UPDATE public.app_core_workspacemembership SET role='revoked'","UPDATE public.app_core_workspacemembership SET role='member'"),
        ("UPDATE runtime.execution_job_tenants SET workspace_id='foreign'","UPDATE runtime.execution_job_tenants SET workspace_id='ws_1'"),
        ("UPDATE runtime.runtime_jobs SET lease_owner='foreign'","UPDATE runtime.runtime_jobs SET lease_owner='owner-run-a'"),
        ("UPDATE runtime.runtime_jobs SET idempotency_key='foreign'","UPDATE runtime.runtime_jobs SET idempotency_key='agent_run.lifecycle:run-a:'||(SELECT digest FROM public.app_core_agentrunauthorization WHERE agent_run_id='run-a')"),
        ("UPDATE public.app_core_agentrunauthorization SET signature='hmac-sha256:'||repeat('0',64)",""),
    ] {
        db.batch_execute(mutation).unwrap(); assert!(prepare(&store,&a,"execution-a",&physical).is_err(),"{mutation}");
        if restore.is_empty() { db.execute("UPDATE public.app_core_agentrunauthorization SET signature=$1",&[&a.original.signature(KEY).unwrap()]).unwrap(); } else { db.batch_execute(restore).unwrap(); }
        assert_eq!(db.query_one("SELECT count(*) FROM runtime.execution_instances",&[]).unwrap().get::<_,i64>(0),0);
    }
    let saved = a.grant.clone();
    let mut changed = a.grant.payload.clone();
    changed.controller.as_mut().unwrap().controller_epoch = 2;
    a.grant = sign(changed);
    assert!(prepare(&store, &a, "execution-a", &physical).is_err());
    a.grant = saved;
    let first = prepare(&store, &a, "execution-a", &physical).unwrap();
    let other_profile = format!("sha256:{}", "c".repeat(64));
    assert!(store
        .prepare_retained_instance(
            &a.request("another-execution", &other_profile),
            limits(&a),
            || Ok(physical.lock().unwrap().clone()),
            |_, _| panic!("config mismatch"),
            |_| panic!("config mismatch must not create")
        )
        .is_err());
    assert!(db.execute("UPDATE runtime.execution_instances SET agent_run_id='foreign' WHERE execution_id='execution-a'",&[]).is_err());
    assert!(db.execute("INSERT INTO runtime.execution_instances SELECT 'bad-child',instance_id,'foreign',session_id,agent_run_id,host_id,authorization_digest,binding_digest,image_digest,execution_id,allocation_mode,profile_digest FROM runtime.execution_instances WHERE execution_id='execution-a'",&[]).is_err());
    assert_eq!(physical.lock().unwrap().len(), 1);
    assert_eq!(first.allocation_execution_id, "execution-a");
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_retained_physical_guard_pins_authority_and_global_host_admission() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db) = setup();
    let a = run(&store, &mut db, "run-a", 1);
    let profile = format!("sha256:{}", "b".repeat(64));
    let r = a.request("execution-a", &profile);
    store
        .prepare_retained_instance(
            &r,
            limits(&a),
            || Ok(Vec::new()),
            |_, _| Ok(()),
            |_| {
                // Independent connections cannot revoke the pinned authority or reclaim
                // the job while the bounded physical mutation is in progress.
                let mut rival = Client::connect(&url, NoTls).unwrap();
                rival.batch_execute("SET lock_timeout='150ms'").unwrap();
                assert!(rival
                    .batch_execute(
                        "UPDATE public.app_core_sessionenvironment SET controller_epoch=2"
                    )
                    .is_err());
                assert!(rival
                    .batch_execute("UPDATE runtime.runtime_jobs SET lease_owner='rival'")
                    .is_err());
                assert!(store
                    .prepare_retained_instance(
                        &r,
                        limits(&a),
                        || panic!("host lock rejects before inventory"),
                        |_, _| panic!("second fact"),
                        |_| panic!("second writer")
                    )
                    .unwrap_err()
                    .contains("capacity_wait"));
                Ok("one-container".into())
            },
        )
        .unwrap();
    assert_eq!(
        db.query_one(
            "SELECT controller_epoch FROM public.app_core_sessionenvironment",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        1
    );
    assert_eq!(
        db.query_one("SELECT count(*) FROM runtime.resident_sandboxes", &[])
            .unwrap()
            .get::<_, i64>(0),
        1
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_retained_operation_requires_current_association_controller_and_job() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db) = setup();
    let a = run(&store, &mut db, "run-a", 1);
    let physical = Arc::new(Mutex::new(Vec::new()));
    let allocated = prepare(&store, &a, "execution-a", &physical).unwrap();
    let profile = format!("sha256:{}", "b".repeat(64));
    let completed = store
        .with_retained_instance_operation(
            &a.request("execution-a", &profile),
            &allocated,
            |allocation| {
                assert_eq!(allocation.instance_id, allocated.instance_id);
                let mut rival = Client::connect(&url, NoTls).unwrap();
                rival.batch_execute("SET lock_timeout='150ms'").unwrap();
                for mutation in [
                    "UPDATE public.app_core_sessionenvironment SET controller_epoch=2",
                    "UPDATE runtime.runtime_jobs SET lease_owner='rival'",
                    "UPDATE runtime.resident_sandboxes SET state='releasing'",
                ] {
                    assert!(rival.batch_execute(mutation).is_err());
                }
                Ok("completed")
            },
        )
        .unwrap();
    assert_eq!(completed, "completed");
    assert!(store
        .with_retained_instance_operation::<()>(
            &a.request("foreign-execution", &profile),
            &allocated,
            |_| panic!("unassociated operation")
        )
        .is_err());
    let wrong_profile = format!("sha256:{}", "c".repeat(64));
    assert!(store
        .with_retained_instance_operation::<()>(
            &a.request("execution-a", &wrong_profile),
            &allocated,
            |_| panic!("different profile operation")
        )
        .is_err());
    let mut wrong_resources = a.request("execution-a", &profile);
    wrong_resources.resources.memory_bytes += 1;
    assert!(store
        .with_retained_instance_operation::<()>(&wrong_resources, &allocated, |_| panic!(
            "different resource binding operation"
        ))
        .is_err());
    let mut wrong_incarnation = allocated.clone();
    wrong_incarnation.container_id = Some("different-container".into());
    assert!(store
        .with_retained_instance_operation::<()>(
            &a.request("execution-a", &profile),
            &wrong_incarnation,
            |_| panic!("different physical incarnation operation")
        )
        .is_err());
    db.batch_execute("UPDATE public.app_core_sessionenvironment SET controller_epoch=2")
        .unwrap();
    assert!(store
        .with_retained_instance_operation::<()>(
            &a.request("execution-a", &profile),
            &allocated,
            |_| panic!("stale epoch operation")
        )
        .is_err());
    db.batch_execute("UPDATE public.app_core_sessionenvironment SET controller_epoch=1; UPDATE runtime.runtime_jobs SET lease_owner='reclaimed'").unwrap();
    assert!(store
        .with_retained_instance_operation::<()>(
            &a.request("execution-a", &profile),
            &allocated,
            |_| panic!("old job operation")
        )
        .is_err());
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_retained_migration_rejects_provenance_conflict_atomically() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db) = setup();
    drop(store);
    downgrade_to_eight(&mut db);
    db.batch_execute("INSERT INTO runtime.execution_instances VALUES('origin','instance','env','session','run','host','grant','binding','image'); INSERT INTO runtime.resident_sandboxes VALUES('foreign-host','origin','run','tenant',NULL,'reserved',1,2,3,4,5,6);").unwrap();
    assert!(PostgresRuntimeStore::new(&url)
        .unwrap_err()
        .contains("provenance mismatch"));
    assert_eq!(
        db.query_one("SELECT max(version) FROM runtime.schema_migrations", &[])
            .unwrap()
            .get::<_, i64>(0),
        8
    );
    assert_eq!(
        db.query_one(
            "SELECT state,updated_at_ms FROM runtime.resident_sandboxes",
            &[]
        )
        .unwrap()
        .get::<_, String>(0),
        "reserved"
    );
}

pub(super) fn downgrade_to_eight(db: &mut Client) {
    let version = db
        .query_one("SELECT max(version) FROM runtime.schema_migrations", &[])
        .unwrap()
        .get::<_, i64>(0);
    if version >= 11 {
        db.batch_execute("DROP TABLE runtime.environment_retirements; DROP FUNCTION runtime.protect_environment_retirement(); DELETE FROM runtime.schema_migrations WHERE version=11;").unwrap();
    }
    if version >= 10 {
        db.batch_execute(
            "DROP TABLE runtime.agent_run_cleanup_receipts;
            DELETE FROM runtime.schema_migrations WHERE version=10;",
        )
        .unwrap();
    }
    if version == 8 {
        return;
    }
    db.batch_execute("DROP TRIGGER resident_allocation_facts ON runtime.resident_sandboxes;
        DROP FUNCTION runtime.verify_resident_allocation();
        ALTER TABLE runtime.resident_sandboxes DROP COLUMN instance_id CASCADE, DROP COLUMN environment_id CASCADE, DROP COLUMN allocation_mode CASCADE;
        DROP TRIGGER execution_allocation_facts ON runtime.execution_instances;
        DROP FUNCTION runtime.verify_execution_allocation();
        ALTER TABLE runtime.execution_instances DROP COLUMN allocation_execution_id CASCADE, DROP COLUMN allocation_mode CASCADE, DROP COLUMN profile_digest CASCADE;
        ALTER TABLE runtime.execution_instances DROP CONSTRAINT execution_instances_execution_instance_key;
        ALTER TABLE runtime.execution_instances ADD CONSTRAINT execution_instances_instance_id_key UNIQUE(instance_id);
        DELETE FROM runtime.schema_migrations WHERE version=9;").unwrap();
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_retained_migration_preserves_allocation_and_unknown_reservation() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let url = test_url();
    let mut db = Client::connect(&url, NoTls).unwrap();
    require_owned_database(&mut db);
    reset_store(&url);
    drop(PostgresRuntimeStore::new(&url).unwrap());
    downgrade_to_eight(&mut db);
    // The fixture's allocation is a released v8 per-Run fact, not permission to
    // reconstruct a retained physical process or invent an unknown identity.
    db.batch_execute("INSERT INTO runtime.execution_instances(execution_id,instance_id,environment_id,session_id,agent_run_id,host_id,authorization_digest,binding_digest,image_digest) VALUES('origin','instance-origin','env','session','run-a','host','grant','binding','image');
        INSERT INTO runtime.resident_sandboxes(host_id,execution_id,agent_run_id,workspace_id,container_id,state,memory_bytes,cpu_milli,pids,workspace_bytes,created_at_ms,updated_at_ms) VALUES('host','origin','run-a','tenant','container','resident',101,102,103,104,105,106),('host','unknown','unknown-run','tenant',NULL,'reserved',201,202,203,204,205,206);").unwrap();
    let before = db
        .query(
            "SELECT version,applied_at_ms FROM runtime.schema_migrations ORDER BY version",
            &[],
        )
        .unwrap()
        .into_iter()
        .map(|r| (r.get::<_, i64>(0), r.get::<_, i64>(1)))
        .collect::<Vec<_>>();
    drop(PostgresRuntimeStore::new(&url).unwrap());
    let anchor = db.query_one("SELECT allocation_execution_id,allocation_mode,profile_digest FROM runtime.execution_instances WHERE execution_id='origin'", &[]).unwrap();
    assert_eq!(anchor.get::<_, String>(0), "origin");
    assert_eq!(anchor.get::<_, String>(1), "perRun");
    assert_eq!(anchor.get::<_, Option<String>>(2), None);
    let known = db.query_one("SELECT instance_id,environment_id,allocation_mode FROM runtime.resident_sandboxes WHERE execution_id='origin'", &[]).unwrap();
    assert_eq!(
        known.get::<_, Option<String>>(0).as_deref(),
        Some("instance-origin")
    );
    assert_eq!(known.get::<_, Option<String>>(1).as_deref(), Some("env"));
    assert_eq!(known.get::<_, String>(2), "perRun");
    let unknown = db.query_one("SELECT instance_id,environment_id,allocation_mode,state,memory_bytes,cpu_milli,pids,workspace_bytes,created_at_ms,updated_at_ms FROM runtime.resident_sandboxes WHERE execution_id='unknown'", &[]).unwrap();
    assert_eq!(unknown.get::<_, Option<String>>(0), None);
    assert_eq!(unknown.get::<_, Option<String>>(1), None);
    assert_eq!(unknown.get::<_, String>(2), "unbound");
    assert_eq!(unknown.get::<_, String>(3), "reserved");
    for i in 4..10 {
        assert_eq!(unknown.get::<_, i64>(i), 197 + i as i64);
    }
    let after = db.query("SELECT version,applied_at_ms FROM runtime.schema_migrations WHERE version<=8 ORDER BY version", &[]).unwrap().into_iter().map(|r|(r.get::<_,i64>(0),r.get::<_,i64>(1))).collect::<Vec<_>>();
    assert_eq!(before, after);
}
