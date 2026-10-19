use std::sync::{Arc, Mutex};

use centaeris_core::execution::ResidentResourceUsage;
use centaeris_core::session::reliability::{
    RuntimeJobStatus, RuntimeJobStorePort, ScheduleRuntimeJobRequest, YieldRuntimeJobRequest,
};
use postgres::{Client, NoTls};

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_instance_allocation_precedes_create_and_unknown_creation_keeps_budget() {
    use super::retained_instance_tests as fixture;
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, initial, mut db) = fixture::setup();
    let a = fixture::run(&initial, &mut db, "instance-unknown-create", 1);
    drop(initial);
    let async_url = format!(
        "{url}{}options=-c%20synchronous_commit%3Doff",
        if url.contains('?') { "&" } else { "?" }
    );
    let store = PostgresRuntimeStore::new_with_pool_limits(
        &async_url,
        1,
        std::time::Duration::from_millis(100),
    )
    .unwrap();
    db.batch_execute("DROP TABLE IF EXISTS public.execution_instance_commit_policy; CREATE TABLE public.execution_instance_commit_policy(execution_id text NOT NULL,commit_policy text NOT NULL); CREATE OR REPLACE FUNCTION public.observe_execution_instance_commit_policy() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN INSERT INTO public.execution_instance_commit_policy VALUES(NEW.execution_id,current_setting('synchronous_commit')); RETURN NEW; END $$; CREATE TRIGGER observe_execution_instance_commit_policy AFTER INSERT ON runtime.execution_instances FOR EACH ROW EXECUTE FUNCTION public.observe_execution_instance_commit_policy();").unwrap();
    let profile = a.original.environment_profile.digest().unwrap();
    let request = a.request("unknown-physical", &profile);
    let error = store.prepare_retained_instance(&request, fixture::limits(&a), || Ok(vec![]), |client, allocation| {
        assert_eq!(db.query_one("SELECT count(*) FROM runtime.resident_sandboxes", &[]).unwrap().get::<_,i64>(0), 1);
        assert_eq!(db.query_one("SELECT commit_policy FROM public.execution_instance_commit_policy WHERE execution_id='unknown-physical'", &[]).unwrap().get::<_,String>(0), "on");
        assert_eq!(db.query_one("SELECT instance_id FROM runtime.execution_instances WHERE execution_id='unknown-physical'", &[]).unwrap().get::<_,String>(0), allocation.instance_id);
        assert_eq!(client.query_one("SHOW synchronous_commit", &[]).unwrap().get::<_,String>(0), "off");
        Ok(())
    }, |_| Err("unknown daemon outcome".into())).unwrap_err();
    assert_eq!(error, "unknown daemon outcome");
    assert_eq!(
        db.query_one("SELECT count(*) FROM runtime.resident_sandboxes", &[])
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    drop(store);
    db.batch_execute("DROP TRIGGER observe_execution_instance_commit_policy ON runtime.execution_instances; DROP FUNCTION public.observe_execution_instance_commit_policy(); DROP TABLE public.execution_instance_commit_policy;").unwrap();
    let reopened = PostgresRuntimeStore::new(&url).unwrap();
    assert!(reopened
        .prepare_retained_instance(
            &request,
            fixture::limits(&a),
            || Ok(vec![]),
            |_, _| panic!("uncertain allocation repeated acceptance"),
            |_| panic!("uncertain allocation repeated physical create")
        )
        .is_err());
    let physical = Arc::new(Mutex::new(vec![
        crate::resident_capacity::ObservedResidentSandbox {
            execution_id: "unknown-physical".into(),
            allocation_owner: centaeris_core::execution::EnvironmentTaskOwner::AgentRun {
                run_id: a.original.agent_run_id.clone(),
            },
            container_id: "late-physical".into(),
            resources: request.resources,
        },
    ]));
    let adopted = fixture::prepare(&reopened, &a, "unknown-physical", &physical).unwrap();
    assert!(adopted.reused);
    assert_eq!(adopted.container_id.as_deref(), Some("late-physical"));
    assert_eq!(
        db.query_one("SELECT count(*) FROM runtime.resident_sandboxes", &[])
            .unwrap()
            .get::<_, i64>(0),
        1
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_resident_recovery_attempt_is_durable_before_create_with_async_connection() {
    use centaeris_core::runtime::contracts::{CheckpointKindV1, CheckpointRecord};
    use centaeris_core::session::AgentRunSessionState;
    use std::time::Duration;

    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (url, initial) = setup();
    drop(initial);
    let async_url = format!(
        "{url}{}options=-c%20synchronous_commit%3Doff",
        if url.contains('?') { "&" } else { "?" }
    );
    let store =
        PostgresRuntimeStore::new_with_pool_limits(&async_url, 1, Duration::from_millis(100))
            .unwrap();
    let fixture = sandbox(
        &store,
        &url,
        "resident-durable-recovery",
        "durable-recovery-tenant",
    );
    let session = format!("session-{}", fixture.run);
    let mut observer = Client::connect(&url, NoTls).unwrap();
    observer
        .execute(
            "UPDATE runtime.runtime_jobs SET session_id=$2 WHERE job_id=$1",
            &[&fixture.job, &session],
        )
        .unwrap();
    observer.batch_execute("DROP TABLE IF EXISTS public.app_core_sessionevent; CREATE TABLE public.app_core_sessionevent(\"eventId\" text PRIMARY KEY,workspace_id text NOT NULL,session_id text NOT NULL,agent_run_id text NOT NULL,sequence integer NOT NULL,agent_run_sequence integer,session_level boolean NOT NULL DEFAULT false,projects_to_agent_run_stream boolean NOT NULL,payload jsonb NOT NULL,\"createdAtMs\" bigint NOT NULL,\"insertedAt\" timestamptz NOT NULL DEFAULT clock_timestamp(),UNIQUE(session_id,sequence),UNIQUE(agent_run_id,agent_run_sequence));").unwrap();
    observer.batch_execute("DROP TABLE IF EXISTS public.recovery_commit_policy; CREATE TABLE public.recovery_commit_policy(event_type text NOT NULL,commit_policy text NOT NULL); CREATE OR REPLACE FUNCTION public.observe_recovery_commit_policy() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN INSERT INTO public.recovery_commit_policy VALUES(NEW.payload->>'type',current_setting('synchronous_commit')); RETURN NEW; END $$; CREATE TRIGGER observe_recovery_commit_policy AFTER INSERT ON public.app_core_sessionevent FOR EACH ROW EXECUTE FUNCTION public.observe_recovery_commit_policy();").unwrap();
    let log = store.session_log(
        fixture.tenant.clone(),
        session.clone(),
        "recover durably".into(),
    );
    let mut state = AgentRunSessionState::new(&session, &fixture.run).unwrap();
    let mut records = state
        .start("durable-turn", "recover durably", Vec::new(), 1)
        .unwrap();
    records.push(
        state
            .start_execution(
                "durable-turn",
                "lost-execution",
                &format!("sha256:{}", "a".repeat(64)),
                None,
                2,
            )
            .unwrap(),
    );
    records.push(
        state
            .checkpoint_ref(&CheckpointRecord {
                checkpoint_id: "durable-checkpoint".into(),
                kind: CheckpointKindV1::Recovery,
                session_id: session.clone(),
                turn_id: "durable-turn".into(),
                status: "committed".into(),
                done_reason: None,
                updated_at_ms: 3,
                payload_json: "{}".into(),
            })
            .unwrap(),
    );
    records.push(
        state
            .end_execution(
                "durable-turn",
                "lost-execution",
                "lost",
                "execution_environment_lost",
                true,
                Some("durable-checkpoint"),
                Vec::new(),
                4,
            )
            .unwrap(),
    );
    log.append_session_records_with_runtime_job_lease_blocking(
        &fixture.run,
        &records,
        &fixture.fence(),
    )
    .unwrap();
    let attempt = state
        .reserve_execution_recovery("durable-turn", "durable-checkpoint", 5, 5)
        .unwrap();
    let mut receipt = None;
    let mut created = false;
    prepare_with_client(&store, "durable-recovery-host", &fixture, limits(1, 1),
        || Ok(Vec::new()),
        |client| {
            let before: String = client.query_one("SHOW synchronous_commit", &[]).map_err(|error| error.to_string())?.get(0);
            assert_eq!(before, "off", "reservation must preserve the ordinary connection policy");
            receipt = Some(log.append_session_records_with_runtime_job_lease_on_client(client, &fixture.run, std::slice::from_ref(&attempt), &fixture.fence())?);
            let committed_policy: String = observer.query_one("SELECT commit_policy FROM public.recovery_commit_policy WHERE event_type='agent_run_recovery_attempted'", &[]).map_err(|error| error.to_string())?.get(0);
            assert_eq!(committed_policy, "on", "the actual recovery source insert must use synchronous commit before physical creation");
            let after: String = client.query_one("SHOW synchronous_commit", &[]).map_err(|error| error.to_string())?.get(0);
            assert_eq!(after, "off", "recovery append must not change ordinary connection policy");
            created = true;
            Ok(())
        }, |_| Ok(fixture.observed().container_id)).unwrap();
    log.catch_up_transcript_projection_after_append(receipt.as_ref().unwrap());
    assert!(created);
    assert_eq!(count(&url), 1);
    store
        .with_client(|client| {
            assert_eq!(
                client
                    .query_one("SHOW synchronous_commit", &[])
                    .map_err(|error| error.to_string())?
                    .get::<_, String>(0),
                "off"
            );
            Ok(())
        })
        .unwrap();
    observer.batch_execute("DROP TRIGGER observe_recovery_commit_policy ON public.app_core_sessionevent; DROP FUNCTION public.observe_recovery_commit_policy(); DROP TABLE public.recovery_commit_policy;").unwrap();
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_resident_recovery_append_reuses_one_pool_connection_before_create() {
    use centaeris_core::session::SessionRecordType;
    use std::time::Duration;
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (url, initial) = setup();
    drop(initial);
    let store =
        PostgresRuntimeStore::new_with_pool_limits(&url, 1, Duration::from_millis(100)).unwrap();
    let fixture = sandbox(&store, &url, "resident-one-pool", "one-pool-tenant");
    let session = format!("session-{}", fixture.run);
    let mut observer = Client::connect(&url, NoTls).unwrap();
    observer
        .execute(
            "UPDATE runtime.runtime_jobs SET session_id=$2 WHERE job_id=$1",
            &[&fixture.job, &session],
        )
        .unwrap();
    observer.batch_execute("DROP TABLE IF EXISTS public.app_core_sessionevent; CREATE TABLE public.app_core_sessionevent(\"eventId\" text PRIMARY KEY,workspace_id text NOT NULL,session_id text NOT NULL,agent_run_id text NOT NULL,sequence integer NOT NULL,agent_run_sequence integer,session_level boolean NOT NULL DEFAULT false,projects_to_agent_run_stream boolean NOT NULL,payload jsonb NOT NULL,\"createdAtMs\" bigint NOT NULL,\"insertedAt\" timestamptz NOT NULL DEFAULT clock_timestamp(),UNIQUE(session_id,sequence),UNIQUE(agent_run_id,agent_run_sequence));").unwrap();
    let log = store.session_log(
        fixture.tenant.clone(),
        session.clone(),
        "recovery pool proof".into(),
    );
    let first = super::session_record(
        &fixture.run,
        &session,
        1,
        SessionRecordType::AgentRunStarted,
        serde_json::json!({"userObjective":"recovery pool proof"}),
        1,
    );
    log.append_session_records_with_runtime_job_lease_blocking(
        &fixture.run,
        &[first],
        &fixture.fence(),
    )
    .unwrap();
    let second = super::session_record(
        &fixture.run,
        &session,
        2,
        SessionRecordType::UserMessage,
        serde_json::json!({"messageId":"message:turn_pg_fenced_terminal:user","text":"recovery pool proof","attachments":[]}),
        2,
    );
    let mut receipt = None;
    let created = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let create_flag = created.clone();
    prepare_with_client(
        &store,
        "one-pool-host",
        &fixture,
        limits(1, 1),
        || Ok(Vec::new()),
        |client| {
            receipt = Some(log.append_session_records_with_runtime_job_lease_on_client(
                client,
                &fixture.run,
                &[second],
                &fixture.fence(),
            )?);
            let source_count: i64 = client
                .query_one(
                    "SELECT count(*) FROM app_core_sessionevent WHERE agent_run_id=$1",
                    &[&fixture.run],
                )
                .map_err(|error| error.to_string())?
                .get(0);
            assert_eq!(
                source_count, 2,
                "source commit must precede physical creation"
            );
            create_flag.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        },
        |_| Ok(fixture.observed().container_id),
    )
    .unwrap();
    log.catch_up_transcript_projection_after_append(receipt.as_ref().unwrap());
    assert!(created.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(count(&url), 1);
    store
        .with_client(|client| {
            client
                .simple_query("SELECT 1")
                .map(|_| ())
                .map_err(|error| error.to_string())
        })
        .unwrap();
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_resident_declared_profile_waits_for_capacity_without_creation() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (url, store) = setup();
    let fixture = sandbox(&store, &url, "resident-profile-wait", "profile-tenant");
    let host = Arc::new(Mutex::new(Vec::new()));
    let mut too_small = limits(1, 1);
    too_small.host.memory_bytes = resources().memory_bytes / 2;
    assert_eq!(
        prepare(&store, &fixture, &host, too_small).unwrap_err(),
        RESIDENT_CAPACITY_WAIT
    );
    assert!(host.lock().unwrap().is_empty());
    assert_eq!(count(&url), 0);
    prepare(&store, &fixture, &host, limits(1, 1)).unwrap();
    assert_eq!(host.lock().unwrap().len(), 1);
    assert_eq!(count(&url), 1);
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_resident_removal_serializes_lease_takeover_until_physical_absence() {
    use std::sync::mpsc;
    use std::time::Duration;
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (url, store) = setup();
    let fixture = sandbox(
        &store,
        &url,
        "resident-remove-linearization",
        "removal-tenant",
    );
    let host = Arc::new(Mutex::new(Vec::new()));
    seed_historical_resident(&url, &fixture, &host);
    let expiry: i64 = Client::connect(&url, NoTls)
        .unwrap()
        .query_one(
            "SELECT lease_expires_at_ms FROM runtime.runtime_jobs WHERE job_id=$1",
            &[&fixture.job],
        )
        .unwrap()
        .get(0);
    let (entered_tx, entered_rx) = mpsc::sync_channel(1);
    let (resume_tx, resume_rx) = mpsc::sync_channel(1);
    let remover = store.clone();
    let run = fixture.run.clone();
    let fence = fixture.fence();
    let remove_host = host.clone();
    let inventory_host = host.clone();
    let task = std::thread::spawn(move || {
        remover.remove_resident_sandboxes(
            "fake-docker-host",
            &run,
            &fence,
            move || {
                entered_tx.send(()).unwrap();
                resume_rx
                    .recv_timeout(Duration::from_secs(5))
                    .map_err(|error| error.to_string())?;
                remove_host.lock().unwrap().clear();
                Ok(())
            },
            || Ok(inventory_host.lock().unwrap().clone()),
        )
    });
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    // A controlled future clock makes the lease eligible while the callback is paused.
    // The held row fence must prevent another owner from being assigned mid-removal.
    let reclaimed = store
        .reclaim_expired_runtime_job_leases(expiry + 1)
        .unwrap();
    resume_tx.send(()).unwrap();
    let removed = task.join().unwrap();
    assert_eq!(
        reclaimed, 0,
        "lease takeover must wait until the physical removal transaction finishes"
    );
    removed.unwrap();
    assert!(host.lock().unwrap().is_empty());
    assert_eq!(count(&url), 0);
    assert_eq!(
        store
            .reclaim_expired_runtime_job_leases(expiry + 1)
            .unwrap(),
        1
    );
}

use super::retained_instance_tests::{sign, KEY};
use super::{job, TEST_LOCK};
use crate::agent_run_authorization::WorkspaceAgentRunAuthorization;
use crate::environment_authorization::{
    ControllerBinding, EnvironmentAuthorization, EnvironmentBinding,
};
use crate::postgres_store::execution_instances::{
    RetainedInstanceAllocation, RetainedInstanceRequest,
};
use crate::postgres_store::PostgresRuntimeStore;
use crate::resident_capacity::{ObservedResidentSandbox, ResidentCapacity, RESIDENT_CAPACITY_WAIT};
use sha2::{Digest, Sha256};

#[derive(Clone)]
struct SandboxFixture {
    run: String,
    job: String,
    execution: String,
    tenant: String,
    owner: String,
    original: WorkspaceAgentRunAuthorization,
    grant: EnvironmentAuthorization,
}

fn resources() -> ResidentResourceUsage {
    ResidentResourceUsage {
        sandbox_count: 1,
        memory_bytes: 1024,
        cpu_milli: 1000,
        pids: 16,
        workspace_bytes: 512,
    }
}

fn limits(global: u64, tenant: u64) -> ResidentCapacity {
    let multiply = |count| ResidentResourceUsage {
        sandbox_count: count,
        memory_bytes: 1024 * count,
        cpu_milli: 1000 * count,
        pids: 16 * count,
        workspace_bytes: 512 * count,
    };
    ResidentCapacity {
        global: multiply(global),
        tenant: multiply(tenant),
        host: multiply(global),
    }
}

impl SandboxFixture {
    fn fence(&self) -> centaeris_core::session::RuntimeJobLeaseFence {
        centaeris_core::session::RuntimeJobLeaseFence {
            job_id: self.job.clone(),
            job_kind: "agent_run.lifecycle".into(),
            lease_owner: self.owner.clone(),
        }
    }
    fn resources(&self) -> ResidentResourceUsage {
        crate::resident_capacity::declared_usage(
            self.original
                .environment_profile
                .resources
                .sandbox_resources(),
        )
    }
    fn request<'a>(
        &'a self,
        host_id: &'a str,
        fence: &'a centaeris_core::session::RuntimeJobLeaseFence,
    ) -> RetainedInstanceRequest<'a> {
        RetainedInstanceRequest {
            environment_authorization: &self.grant,
            signing_key: KEY,
            execution_id: &self.execution,
            workspace_id: &self.tenant,
            host_id,
            image_digest: &self.original.environment_profile.image_digest,
            profile_digest: &self.original.environment_profile_digest,
            resources: self.resources(),
            lease: fence,
        }
    }
    fn take_controller(&mut self, url: &str) {
        let mut db = Client::connect(url, NoTls).unwrap();
        let epoch: i64 = db.query_one("UPDATE public.app_core_sessionenvironment SET controller_epoch=controller_epoch+1,controller_lease_owner=$2 WHERE id=$1 RETURNING controller_epoch", &[&self.grant.payload.environment_id,&self.owner]).unwrap().get(0);
        self.grant.payload.controller = Some(ControllerBinding {
            controller_id: self.run.clone(),
            controller_epoch: epoch,
            controller_lease_digest: controller_lease_digest(&self.owner),
            controller_purpose: "execute".into(),
        });
        self.grant = sign(self.grant.payload.clone());
    }
    fn observed(&self) -> ObservedResidentSandbox {
        ObservedResidentSandbox {
            execution_id: self.execution.clone(),
            allocation_owner: centaeris_core::execution::EnvironmentTaskOwner::AgentRun {
                run_id: self.run.clone(),
            },
            container_id: format!(
                "{:x}",
                Sha256::digest(format!("residency-container:{}", self.execution).as_bytes())
            ),
            resources: self.resources(),
        }
    }
}

fn setup() -> (String, PostgresRuntimeStore) {
    let (url, store, _) = super::retained_instance_tests::setup();
    (url, store)
}

fn sandbox(store: &PostgresRuntimeStore, url: &str, id: &str, tenant: &str) -> SandboxFixture {
    sandbox_with_resources(store, url, id, tenant, resources())
}

fn controller_lease_digest(owner: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(b"workspace:environment-controller-lease:v1\0");
    hash.update(owner.as_bytes());
    format!("sha256:{:x}", hash.finalize())
}

fn sandbox_with_resources(
    store: &PostgresRuntimeStore,
    url: &str,
    id: &str,
    tenant: &str,
    usage: ResidentResourceUsage,
) -> SandboxFixture {
    let mut original: WorkspaceAgentRunAuthorization = serde_json::from_str(include_str!(
        "../../../../../tests/workspace/fixtures/agent_run_authorization/v1/valid.json"
    ))
    .unwrap();
    original.agent_run_id = id.into();
    original.id = format!("authorization-{id}");
    original.workspace_id = tenant.into();
    original.session_id = format!("session-{id}");
    let profile = &mut original.environment_profile.resources;
    profile.memory_bytes = usage.memory_bytes;
    profile.cpu_milli = usage.cpu_milli.try_into().unwrap();
    profile.pids_limit = usage.pids.try_into().unwrap();
    profile.data_tmpfs_bytes = usage.workspace_bytes;
    profile.storage_bytes = usage.workspace_bytes;
    original.environment_profile_digest = original.environment_profile.digest().unwrap();
    let digest = original.digest().unwrap();
    let owner = format!("lease-owner-{id}");
    let environment = format!("environment-{id}");
    let grant = sign(EnvironmentBinding {
        schema: "workspace.environment_authorization.v1".into(),
        environment_id: environment.clone(),
        session_id: original.session_id.clone(),
        agent_run_id: id.into(),
        agent_run_authorization_digest: digest.clone(),
        execution_mode: "perRun".into(),
        controller: Some(ControllerBinding {
            controller_id: id.into(),
            controller_epoch: 1,
            controller_lease_digest: controller_lease_digest(&owner),
            controller_purpose: "execute".into(),
        }),
    });
    let fixture = SandboxFixture {
        run: id.into(),
        job: format!("agent_run.lifecycle:{id}"),
        execution: format!("execution-{id}"),
        tenant: tenant.into(),
        owner,
        original,
        grant,
    };
    let mut db = Client::connect(url, NoTls).unwrap();
    let member = format!("member-{tenant}");
    db.execute(
        "INSERT INTO public.app_core_workspace VALUES($1,'active') ON CONFLICT DO NOTHING",
        &[&tenant],
    )
    .unwrap();
    db.execute("INSERT INTO public.app_core_workspacemembership VALUES($1,$2,'user_1','member') ON CONFLICT DO NOTHING", &[&member,&tenant]).unwrap();
    db.execute(
        "INSERT INTO public.app_core_session VALUES($1,$2,'user_1','centaeris',$3,NULL)",
        &[&fixture.original.session_id, &tenant, &environment],
    )
    .unwrap();
    db.execute("INSERT INTO public.app_core_sessionenvironment VALUES($1,$2,$3,1,$4,'execute',false,$5::text::jsonb,$6)", &[&environment,&fixture.original.session_id,&id,&fixture.owner,&serde_json::to_string(&fixture.original.environment_profile).unwrap(),&fixture.original.environment_profile_digest]).unwrap();
    db.execute("INSERT INTO public.app_core_agentrun(id,session_id,environment_id,workspace_id,user_id,membership_ref,status,\"modelConfig_id\",\"thinkingMode\") VALUES($1,$2,$3,$4,'user_1',$5,'running','model_1','high')", &[&id,&fixture.original.session_id,&environment,&tenant,&member]).unwrap();
    db.execute(
        "INSERT INTO public.app_core_agentrunauthorization VALUES($1,$2,$3::text::jsonb,$4)",
        &[
            &id,
            &digest,
            &serde_json::to_string(&fixture.original).unwrap(),
            &fixture.original.signature(KEY).unwrap(),
        ],
    )
    .unwrap();
    let now = db
        .query_one(
            "SELECT (EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint",
            &[],
        )
        .unwrap()
        .get::<_, i64>(0);
    let mut record = job(&fixture.job, &format!("{}:{digest}", fixture.job));
    record.job_kind = "agent_run.lifecycle".into();
    record.status = RuntimeJobStatus::Running;
    record.lease_owner = Some(fixture.owner.clone());
    record.lease_expires_at_ms = Some(now + 600_000);
    record.session_id = Some(fixture.original.session_id.clone());
    record.payload_ref = Some(format!("record:agent_run:{id}"));
    store
        .schedule_worker_job(ScheduleRuntimeJobRequest { job: record }, Some(tenant))
        .unwrap();
    fixture
}

fn prepare(
    store: &PostgresRuntimeStore,
    fixture: &SandboxFixture,
    host: &Arc<Mutex<Vec<ObservedResidentSandbox>>>,
    capacity: ResidentCapacity,
) -> Result<(), String> {
    prepare_allocation(store, fixture, host, capacity).map(|_| ())
}

fn prepare_allocation(
    store: &PostgresRuntimeStore,
    fixture: &SandboxFixture,
    host: &Arc<Mutex<Vec<ObservedResidentSandbox>>>,
    capacity: ResidentCapacity,
) -> Result<RetainedInstanceAllocation, String> {
    prepare_with_client(
        store,
        "fake-docker-host",
        fixture,
        capacity,
        || Ok(host.lock().unwrap().clone()),
        |_| Ok(()),
        |allocation| {
            if allocation.reused {
                return Ok(allocation.container_id.clone().unwrap());
            }
            let mut host = host.lock().unwrap();
            if !host
                .iter()
                .any(|resident| resident.execution_id == fixture.execution)
            {
                host.push(fixture.observed());
            }
            Ok(fixture.observed().container_id)
        },
    )
}

fn prepare_with_client(
    store: &PostgresRuntimeStore,
    host_id: &str,
    fixture: &SandboxFixture,
    capacity: ResidentCapacity,
    inventory: impl FnOnce() -> Result<Vec<ObservedResidentSandbox>, String> + Send,
    before_create: impl FnOnce(&mut Client) -> Result<(), String> + Send,
    create: impl FnOnce(&RetainedInstanceAllocation) -> Result<String, String> + Send,
) -> Result<RetainedInstanceAllocation, String> {
    let fence = fixture.fence();
    store.prepare_retained_instance(
        &fixture.request(host_id, &fence),
        capacity,
        inventory,
        |client, _| before_create(client),
        create,
    )
}

// These facts characterize the still-supported removal/restore fence for an
// already persisted historical resident. They do not exercise current creation
// or claim that cold recovery can allocate a new unified environment instance.
fn seed_historical_resident(
    url: &str,
    fixture: &SandboxFixture,
    host: &Arc<Mutex<Vec<ObservedResidentSandbox>>>,
) {
    let observed = fixture.observed();
    let usage = observed.resources;
    let mut db = Client::connect(url, NoTls).unwrap();
    super::owned_database::require_owned_database(&mut db);
    db.execute("INSERT INTO runtime.execution_instances(execution_id,instance_id,environment_id,session_id,agent_run_id,host_id,authorization_digest,binding_digest,image_digest,allocation_execution_id,allocation_mode) VALUES($1,$2,$3,$4,$5,'fake-docker-host',$6,$7,$8,$1,'perRun')", &[&fixture.execution,&crate::postgres_store::execution_instances::instance_id(&fixture.execution),&fixture.grant.payload.environment_id,&fixture.original.session_id,&fixture.run,&fixture.original.digest().unwrap(),&fixture.grant.payload.digest().unwrap(),&fixture.original.environment_profile.image_digest]).unwrap();
    db.execute("INSERT INTO runtime.resident_sandboxes(host_id,execution_id,agent_run_id,workspace_id,container_id,state,memory_bytes,cpu_milli,pids,workspace_bytes,created_at_ms,updated_at_ms) VALUES('fake-docker-host',$1,$2,$3,$4,'resident',$5,$6,$7,$8,1,1)", &[&fixture.execution,&fixture.run,&fixture.tenant,&observed.container_id,&(usage.memory_bytes as i64),&(usage.cpu_milli as i64),&(usage.pids as i64),&(usage.workspace_bytes as i64)]).unwrap();
    host.lock().unwrap().push(observed);
}

fn count(url: &str) -> i64 {
    Client::connect(url, NoTls)
        .unwrap()
        .query_one("SELECT COUNT(*) FROM runtime.resident_sandboxes", &[])
        .unwrap()
        .get(0)
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_resident_waits_consume_host_and_tenant_resources_until_confirmed_removal() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (url, store) = setup();
    let host = Arc::new(Mutex::new(Vec::new()));
    let mut first = sandbox(&store, &url, "resident-first", "tenant-one");
    let same_tenant = sandbox(&store, &url, "resident-same-tenant", "tenant-one");
    let other_tenant = sandbox(&store, &url, "resident-other-tenant", "tenant-two");
    seed_historical_resident(&url, &first, &host);
    let now = Client::connect(&url, NoTls)
        .unwrap()
        .query_one(
            "SELECT (EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint",
            &[],
        )
        .unwrap()
        .get::<_, i64>(0);
    store
        .yield_runtime_job(YieldRuntimeJobRequest {
            job_id: first.job.clone(),
            lease_owner: first.owner.clone(),
            yielded_at_ms: now,
            run_at_ms: now + 600_000,
            transition_reason: "question_wait".into(),
        })
        .unwrap();
    assert_eq!(
        prepare(&store, &same_tenant, &host, limits(2, 1)).unwrap_err(),
        RESIDENT_CAPACITY_WAIT
    );
    prepare(&store, &other_tenant, &host, limits(2, 1)).unwrap();
    assert_eq!(host.lock().unwrap().len(), 2);
    let restarted = PostgresRuntimeStore::new(&url).unwrap();
    assert_eq!(
        prepare(&restarted, &same_tenant, &host, limits(2, 1)).unwrap_err(),
        RESIDENT_CAPACITY_WAIT
    );
    restarted
        .wake_runtime_job(
            centaeris_core::session::reliability::WakeRuntimeJobRequest {
                job_id: first.job.clone(),
                source_job_id: "cancel-cleanup".into(),
                woken_at_ms: now,
                transition_reason: "agent_run_cancel_requested".into(),
            },
        )
        .unwrap();
    first.owner = restarted
        .claim_due_runtime_jobs(
            centaeris_core::session::reliability::ClaimDueRuntimeJobsRequest {
                now_ms: now,
                worker_id: "cleanup-worker".into(),
                job_id: Some(first.job.clone()),
                job_kind: None,
                session_id: None,
                limit: 1,
                lease_ms: 600000,
            },
        )
        .unwrap()
        .remove(0)
        .lease_owner
        .unwrap();
    assert!(restarted
        .remove_resident_sandboxes(
            "fake-docker-host",
            &first.run,
            &first.fence(),
            || Err("daemon removal unavailable".into()),
            || Ok(host.lock().unwrap().clone())
        )
        .is_err());
    assert_eq!(count(&url), 2);
    assert!(restarted
        .remove_resident_sandboxes(
            "fake-docker-host",
            &first.run,
            &first.fence(),
            || Ok(()),
            || Ok(host.lock().unwrap().clone())
        )
        .is_err());
    assert_eq!(count(&url), 2);
    restarted
        .remove_resident_sandboxes(
            "fake-docker-host",
            &first.run,
            &first.fence(),
            || {
                host.lock().unwrap().retain(|resident| {
                    resident.allocation_owner
                        != (centaeris_core::execution::EnvironmentTaskOwner::AgentRun {
                            run_id: first.run.clone(),
                        })
                });
                Ok(())
            },
            || Ok(host.lock().unwrap().clone()),
        )
        .unwrap();
    assert_eq!(count(&url), 1);
    prepare(&restarted, &same_tenant, &host, limits(2, 1)).unwrap();
    assert_eq!(host.lock().unwrap().len(), 2);
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_resident_concurrent_creation_cannot_exceed_last_slot() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (url, store) = setup();
    let host = Arc::new(Mutex::new(Vec::new()));
    let first = sandbox(&store, &url, "concurrent-resident-one", "tenant-one");
    let second = sandbox(&store, &url, "concurrent-resident-two", "tenant-two");
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let attempts = [first, second]
        .into_iter()
        .map(|fixture| {
            let store = store.clone();
            let host = host.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                prepare(&store, &fixture, &host, limits(1, 1))
            })
        })
        .collect::<Vec<_>>();
    let results = attempts
        .into_iter()
        .map(|attempt| attempt.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert!(results
        .iter()
        .filter_map(|result| result.as_ref().err())
        .all(|error| error == RESIDENT_CAPACITY_WAIT));
    assert_eq!(host.lock().unwrap().len(), 1);
    assert_eq!(count(&url), 1);
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_resident_unknown_creation_outcome_keeps_charge_and_adopts_after_restart() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (url, store) = setup();
    let host = Arc::new(Mutex::new(Vec::new()));
    let first = sandbox(&store, &url, "uncertain-resident", "tenant-one");
    let second = sandbox(&store, &url, "next-resident", "tenant-two");
    assert!(prepare_with_client(
        &store,
        "fake-docker-host",
        &first,
        limits(1, 1),
        || Ok(Vec::new()),
        |_| Ok(()),
        |_| Err("create response lost".into())
    )
    .is_err());
    assert_eq!(count(&url), 1);
    let restarted = PostgresRuntimeStore::new(&url).unwrap();
    assert_eq!(
        prepare(&restarted, &second, &host, limits(1, 1)).unwrap_err(),
        RESIDENT_CAPACITY_WAIT
    );
    host.lock().unwrap().push(first.observed());
    restarted
        .reconcile_resident_sandboxes("fake-docker-host", || Ok(host.lock().unwrap().clone()))
        .unwrap();
    prepare(&restarted, &first, &host, limits(1, 1)).unwrap();
    assert_eq!(host.lock().unwrap().len(), 1);
    assert_eq!(count(&url), 1);
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_preexisting_residents_are_charged_and_unknown_tenants_block_admission() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (url, store) = setup();
    let first = sandbox(&store, &url, "preexisting-resident", "tenant-one");
    let next = sandbox(&store, &url, "new-resident", "tenant-two");
    let host = Arc::new(Mutex::new(vec![first.observed()]));
    store
        .reconcile_resident_sandboxes("fake-docker-host", || Ok(host.lock().unwrap().clone()))
        .unwrap();
    assert_eq!(count(&url), 1);
    assert!(prepare(&store, &next, &host, limits(1, 1))
        .unwrap_err()
        .contains("unresolved reservation"));
    let mut unknown = first.observed();
    unknown.execution_id = "unknown-execution".into();
    unknown.allocation_owner = centaeris_core::execution::EnvironmentTaskOwner::AgentRun {
        run_id: "unknown-owner".into(),
    };
    unknown.container_id = "unknown-container".into();
    host.lock().unwrap().push(unknown);
    assert!(prepare(&store, &next, &host, limits(3, 3))
        .unwrap_err()
        .contains("unknown tenant"));
    assert_eq!(host.lock().unwrap().len(), 2);
    assert_eq!(
        count(&url),
        1,
        "failed reconciliation rolls back partial accounting and blocks new create"
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_resident_stale_owner_and_terminal_run_cannot_create() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (url, store) = setup();
    let first = sandbox(&store, &url, "resident-fenced", "tenant-one");
    let host = Arc::new(Mutex::new(Vec::new()));
    let mut stale = first.clone();
    stale.owner = "stale-worker-token".into();
    assert!(prepare(&store, &stale, &host, limits(1, 1)).is_err());
    Client::connect(&url, NoTls)
        .unwrap()
        .execute(
            "UPDATE public.app_core_agentrun SET status='completed' WHERE id=$1",
            &[&first.run],
        )
        .unwrap();
    assert!(prepare(&store, &first, &host, limits(1, 1)).is_err());
    assert!(host.lock().unwrap().is_empty());
    assert_eq!(count(&url), 0);
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_resident_late_owner_cannot_remove_a_reclaimed_sandbox() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (url, store) = setup();
    let first = sandbox(&store, &url, "removal-fenced", "tenant-one");
    let host = Arc::new(Mutex::new(Vec::new()));
    seed_historical_resident(&url, &first, &host);
    Client::connect(&url, NoTls)
        .unwrap()
        .execute(
            "UPDATE runtime.runtime_jobs SET lease_owner='new-worker-owner' WHERE job_id=$1",
            &[&first.job],
        )
        .unwrap();
    let mut removals = 0;
    let result = store.remove_resident_sandboxes(
        "fake-docker-host",
        &first.run,
        &first.fence(),
        || {
            removals += 1;
            host.lock().unwrap().clear();
            Ok(())
        },
        || Ok(host.lock().unwrap().clone()),
    );
    assert!(
        result.is_err(),
        "a late old Worker must not delete the new owner's adopted box"
    );
    assert_eq!(removals, 0);
    assert_eq!(host.lock().unwrap().len(), 1);
    assert_eq!(count(&url), 1);
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_shipped_resource_profile_allows_serial_child_progress_after_confirmed_parent_release() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (url, store) = setup();
    let example = include_str!("../../../../../.env.example");
    let configured = |name: &str| {
        example
            .lines()
            .find_map(|line| line.strip_prefix(&format!("{name}=")))
            .unwrap()
            .parse::<u64>()
            .unwrap()
    };
    let shipped = ResidentResourceUsage {
        sandbox_count: 1,
        memory_bytes: configured("SANDBOX_MEMORY_BYTES"),
        cpu_milli: configured("SANDBOX_CPU_MILLI"),
        pids: configured("SANDBOX_PIDS_LIMIT"),
        workspace_bytes: configured("SANDBOX_DATA_TMPFS_BYTES"),
    };
    let capacity = ResidentCapacity::default()
        .bound_to_host(64 * 1024 * 1024 * 1024, 16, 8 * 1024 * 1024 * 1024, 1600)
        .unwrap();
    let parent = sandbox_with_resources(&store, &url, "default-parent", "default-tenant", shipped);
    let child = sandbox_with_resources(&store, &url, "default-child", "default-tenant", shipped);
    let host = Arc::new(Mutex::new(Vec::new()));
    let admit = |fixture: &SandboxFixture| {
        prepare_with_client(
            &store,
            "fake-docker-host",
            fixture,
            capacity,
            || Ok(host.lock().unwrap().clone()),
            |_| Ok(()),
            |_| {
                let mut observed = fixture.observed();
                observed.resources = shipped;
                let id = observed.container_id.clone();
                host.lock().unwrap().push(observed);
                Ok(id)
            },
        )
    };
    seed_historical_resident(&url, &parent, &host);
    assert_eq!(admit(&child).unwrap_err(), RESIDENT_CAPACITY_WAIT);
    assert_eq!(host.lock().unwrap().len(), 1);
    Client::connect(&url, NoTls)
        .unwrap()
        .execute(
            "UPDATE public.app_core_agentrun SET status='completed' WHERE id=$1",
            &[&parent.run],
        )
        .unwrap();
    store
        .remove_resident_sandboxes(
            "fake-docker-host",
            &parent.run,
            &parent.fence(),
            || {
                host.lock().unwrap().clear();
                Ok(())
            },
            || Ok(host.lock().unwrap().clone()),
        )
        .unwrap();
    admit(&child).unwrap();
    assert_eq!(host.lock().unwrap().len(), 1);
    assert_eq!(
        host.lock().unwrap()[0].allocation_owner,
        centaeris_core::execution::EnvironmentTaskOwner::AgentRun {
            run_id: child.run.clone()
        }
    );
    assert_eq!(count(&url), 1);
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_resident_new_daemon_identity_does_not_reset_tenant_or_global_budget() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (url, store) = setup();
    let host = Arc::new(Mutex::new(Vec::new()));
    let first = sandbox(&store, &url, "host-one-resident", "tenant-one");
    let same_tenant = sandbox(&store, &url, "host-two-same-tenant", "tenant-one");
    let other_tenant = sandbox(&store, &url, "host-two-other-tenant", "tenant-two");
    prepare(&store, &first, &host, limits(2, 1)).unwrap();
    let mut creates = 0;
    let denied = prepare_with_client(
        &store,
        "other-fake-docker-host",
        &same_tenant,
        limits(2, 1),
        || Ok(Vec::new()),
        |_| Ok(()),
        |_| {
            creates += 1;
            Ok("unexpected-container".into())
        },
    );
    assert_eq!(denied.unwrap_err(), RESIDENT_CAPACITY_WAIT);
    assert_eq!(creates, 0);
    let denied = prepare_with_client(
        &store,
        "other-fake-docker-host",
        &other_tenant,
        limits(1, 1),
        || Ok(Vec::new()),
        |_| Ok(()),
        |_| {
            creates += 1;
            Ok("unexpected-container".into())
        },
    );
    assert_eq!(denied.unwrap_err(), RESIDENT_CAPACITY_WAIT);
    assert_eq!(creates, 0);
    prepare_with_client(
        &store,
        "other-fake-docker-host",
        &other_tenant,
        limits(2, 1),
        || Ok(Vec::new()),
        |_| Ok(()),
        |_| {
            creates += 1;
            Ok("host-two-container".into())
        },
    )
    .unwrap();
    assert_eq!(creates, 1);
    assert_eq!(count(&url), 2);
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_resident_forward_migration_preserves_reclaim_budget_and_rejects_bad_constraints() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (url, store) = setup();
    let first = sandbox(&store, &url, "resident-previous-schema", "tenant-one");
    let before = store.get_runtime_job(&first.job).unwrap();
    let mut db = Client::connect(&url, NoTls).unwrap();
    super::retained_instance_tests::downgrade_to_eight(&mut db);
    db.batch_execute("DROP TABLE runtime.execution_instances; DROP TABLE runtime.resident_sandboxes; DELETE FROM runtime.schema_migrations WHERE version>=7; UPDATE runtime.runtime_jobs SET lease_reclaim_count=2,lease_reclaim_not_before_ms=12345;").unwrap();
    let history = db
        .query(
            "SELECT version,applied_at_ms FROM runtime.schema_migrations ORDER BY version",
            &[],
        )
        .unwrap()
        .iter()
        .map(|row| (row.get::<_, i64>(0), row.get::<_, i64>(1)))
        .collect::<Vec<_>>();
    let upgraded = PostgresRuntimeStore::new(&url).unwrap();
    assert_eq!(upgraded.get_runtime_job(&first.job).unwrap(), before);
    assert_eq!(db.query_one("SELECT lease_reclaim_count,lease_reclaim_not_before_ms FROM runtime.runtime_jobs WHERE job_id=$1", &[&first.job]).unwrap().get::<_, i64>(0), 2);
    assert_eq!(
        db.query_one(
            "SELECT lease_reclaim_not_before_ms FROM runtime.runtime_jobs WHERE job_id=$1",
            &[&first.job]
        )
        .unwrap()
        .get::<_, i64>(0),
        12345
    );
    let retained = db.query("SELECT version,applied_at_ms FROM runtime.schema_migrations WHERE version<=6 ORDER BY version", &[]).unwrap().iter().map(|row| (row.get::<_, i64>(0), row.get::<_, i64>(1))).collect::<Vec<_>>();
    assert_eq!(retained, history);
    assert_eq!(count(&url), 0);
    db.batch_execute("ALTER TABLE runtime.resident_sandboxes DROP CONSTRAINT resident_sandboxes_memory_bytes_check;").unwrap();
    assert!(PostgresRuntimeStore::new(&url)
        .unwrap_err()
        .contains("memory_bytes constraint mismatch"));
    assert_eq!(count(&url), 0);
}
#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_resident_checkpoint_recovery_retires_old_execution_before_single_slot_restore() {
    use centaeris_core::runtime::contracts::{CheckpointKindV1, CheckpointRecord};
    use centaeris_core::session::AgentRunSessionState;
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (url, store) = setup();
    let old = sandbox(
        &store,
        &url,
        "checkpoint-recovery",
        "default-recovery-tenant",
    );
    let mut replacement = old.clone();
    replacement.execution = "replacement-execution".into();
    let session = format!("session-{}", old.run);
    let mut state = AgentRunSessionState::new(&session, &old.run).unwrap();
    let mut records = state
        .start("recovery-turn", "restore checkpoint", Vec::new(), 1)
        .unwrap();
    let digest = format!("sha256:{}", "a".repeat(64));
    records.push(
        state
            .start_execution("recovery-turn", &old.execution, &digest, None, 2)
            .unwrap(),
    );
    records.push(
        state
            .checkpoint_ref(&CheckpointRecord {
                checkpoint_id: "recovery-checkpoint".into(),
                kind: CheckpointKindV1::Recovery,
                session_id: session.clone(),
                turn_id: "recovery-turn".into(),
                status: "committed".into(),
                done_reason: None,
                updated_at_ms: 3,
                payload_json: "{}".into(),
            })
            .unwrap(),
    );
    records.push(
        state
            .end_execution(
                "recovery-turn",
                &old.execution,
                "lost",
                "execution_environment_lost",
                true,
                Some("recovery-checkpoint"),
                Vec::new(),
                4,
            )
            .unwrap(),
    );
    let host = Arc::new(Mutex::new(Vec::new()));
    let events = Mutex::new(Vec::new());
    let admit = |fixture: &SandboxFixture,
                 before: &mut (dyn FnMut(Option<&mut postgres::Client>) -> Result<(), String>
                           + Send)|
     -> Result<(), String> {
        // Historical callback-order characterization only. Current admission
        // must reject this old incarnation; it cannot authorize cold recovery.
        store.with_client(|client| before(Some(client)))?;
        seed_historical_resident(&url, fixture, &host);
        Ok(())
    };
    seed_historical_resident(&url, &old, &host);
    assert!(prepare(&store, &replacement, &host, limits(1, 1)).is_err());
    crate::prepare_execution_host_with_recovery(
        || {
            store.remove_resident_execution(
                "fake-docker-host",
                &old.run,
                &old.execution,
                &old.fence(),
                || {
                    assert_eq!(count(&url), 1, "charge remains during physical removal");
                    host.lock()
                        .unwrap()
                        .retain(|resident| resident.execution_id != old.execution);
                    events.lock().unwrap().push("remove");
                    Ok(())
                },
                || Ok(host.lock().unwrap().clone()),
            )
        },
        |_| {
            records.push(state.reserve_execution_recovery(
                "recovery-turn",
                "recovery-checkpoint",
                5,
                5,
            )?);
            events.lock().unwrap().push("persist-attempt");
            Ok(())
        },
        |before| {
            admit(&replacement, before)?;
            events.lock().unwrap().push("create");
            Ok(())
        },
    )
    .unwrap();
    let mut replay = AgentRunSessionState::new(&session, &old.run).unwrap();
    for record in records {
        replay.restore(record).unwrap();
    }
    replay
        .start_execution(
            "recovery-turn",
            &replacement.execution,
            &digest,
            Some("recovery-checkpoint"),
            6,
        )
        .unwrap();
    events.lock().unwrap().push("restore");
    assert_eq!(
        replay.active_execution_id(),
        Some(replacement.execution.as_str())
    );
    assert_eq!(replay.recovery_execution_count(), 1);
    assert_eq!(
        events.into_inner().unwrap(),
        ["remove", "persist-attempt", "create", "restore"]
    );
    assert_eq!(count(&url), 1);
    assert_eq!(host.lock().unwrap()[0].execution_id, replacement.execution);
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_resident_recovery_removal_failure_or_unknown_inventory_keeps_charge_and_blocks_create()
{
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (url, store) = setup();
    let old = sandbox(&store, &url, "recovery-remove-failure", "recovery-tenant");
    let host = Arc::new(Mutex::new(Vec::new()));
    seed_historical_resident(&url, &old, &host);
    for failure in [
        "remove unavailable",
        "inventory unavailable",
        "resident sandbox removal was not confirmed",
    ] {
        let created = std::sync::atomic::AtomicBool::new(false);
        let attempted = std::sync::atomic::AtomicBool::new(false);
        let result = crate::prepare_execution_host_with_recovery(
            || {
                store.remove_resident_execution(
                    "fake-docker-host",
                    &old.run,
                    &old.execution,
                    &old.fence(),
                    || {
                        if failure == "remove unavailable" {
                            Err(failure.into())
                        } else {
                            Ok(())
                        }
                    },
                    || {
                        if failure == "inventory unavailable" {
                            Err(failure.into())
                        } else {
                            Ok(host.lock().unwrap().clone())
                        }
                    },
                )
            },
            |_| {
                attempted.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            },
            |before| {
                before(None)?;
                created.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            },
        );
        assert_eq!(result.unwrap_err(), failure);
        assert!(!attempted.load(std::sync::atomic::Ordering::SeqCst));
        assert!(!created.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(count(&url), 1);
        assert_eq!(host.lock().unwrap().len(), 1);
    }
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_resident_exact_recovery_retirement_fences_old_worker_and_preserves_replacement() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (url, store) = setup();
    let old = sandbox(&store, &url, "exact-recovery", "exact-tenant");
    let mut replacement = old.clone();
    replacement.execution = "already-created-replacement".into();
    let host = Arc::new(Mutex::new(Vec::new()));
    seed_historical_resident(&url, &old, &host);
    seed_historical_resident(&url, &replacement, &host);
    let mut db = Client::connect(&url, NoTls).unwrap();
    db.execute(
        "UPDATE runtime.runtime_jobs SET lease_owner='replacement-worker' WHERE job_id=$1",
        &[&old.job],
    )
    .unwrap();
    let removed = std::sync::atomic::AtomicBool::new(false);
    let result = store.remove_resident_execution(
        "fake-docker-host",
        &old.run,
        &old.execution,
        &old.fence(),
        || {
            removed.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        },
        || Ok(host.lock().unwrap().clone()),
    );
    assert_eq!(result.unwrap_err(), "resident removal lease rejected");
    assert!(!removed.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(count(&url), 2);
    let mut current = old.clone();
    current.owner = "replacement-worker".into();
    let result = store.remove_resident_execution(
        "fake-docker-host",
        &old.run,
        &old.execution,
        &current.fence(),
        || {
            host.lock()
                .unwrap()
                .retain(|sandbox| sandbox.execution_id != old.execution);
            Ok(())
        },
        || Err("response lost after physical removal".into()),
    );
    assert_eq!(result.unwrap_err(), "response lost after physical removal");
    assert_eq!(count(&url), 2, "unknown physical outcome retains charge");
    assert_eq!(host.lock().unwrap()[0].execution_id, replacement.execution);
    store
        .remove_resident_execution(
            "fake-docker-host",
            &old.run,
            &old.execution,
            &current.fence(),
            || Ok(()),
            || Ok(host.lock().unwrap().clone()),
        )
        .unwrap();
    assert_eq!(count(&url), 1);
    assert_eq!(host.lock().unwrap()[0].execution_id, replacement.execution);
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_resident_legal_question_and_runtime_job_waits_reuse_their_live_execution() {
    use centaeris_core::session::AgentRunSessionState;
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (url, store) = setup();
    let host = Arc::new(Mutex::new(Vec::new()));
    for (index, reason) in ["question_wait", "runtime_job_wait"]
        .into_iter()
        .enumerate()
    {
        let fixture = sandbox(&store, &url, reason, reason);
        prepare(&store, &fixture, &host, limits(2, 1)).unwrap();
        let mut start = crate::tests::agent_run_start();
        start.agent_run_id = fixture.run.clone();
        start.authorization.workspace_id = fixture.tenant.clone();
        start.authorization.session_id = format!("session-{}", fixture.run);
        let mut state =
            AgentRunSessionState::new(&start.authorization.session_id, &fixture.run).unwrap();
        state
            .start(&start.turn_id, "legitimate wait", Vec::new(), 1)
            .unwrap();
        state
            .start_execution(
                &start.turn_id,
                &fixture.execution,
                &start.authorization_digest,
                None,
                2,
            )
            .unwrap();
        let now = Client::connect(&url, NoTls)
            .unwrap()
            .query_one(
                "SELECT (EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint",
                &[],
            )
            .unwrap()
            .get::<_, i64>(0);
        store
            .yield_runtime_job(YieldRuntimeJobRequest {
                job_id: fixture.job.clone(),
                lease_owner: fixture.owner.clone(),
                yielded_at_ms: now,
                run_at_ms: now + 600000,
                transition_reason: reason.into(),
            })
            .unwrap();
        assert!(
            crate::select_agent_run_recovery_checkpoint(&store, &start, &state)
                .unwrap()
                .is_none()
        );
        assert!(!state.has_ended_execution(&fixture.execution));
        assert_eq!(host.lock().unwrap()[index].execution_id, fixture.execution);
        store
            .wake_runtime_job(
                centaeris_core::session::reliability::WakeRuntimeJobRequest {
                    job_id: fixture.job.clone(),
                    source_job_id: "wait-resume".into(),
                    woken_at_ms: now,
                    transition_reason: "resume".into(),
                },
            )
            .unwrap();
        let claimed = store
            .claim_due_runtime_jobs(
                centaeris_core::session::reliability::ClaimDueRuntimeJobsRequest {
                    now_ms: now,
                    worker_id: "resume-worker".into(),
                    job_id: Some(fixture.job.clone()),
                    job_kind: None,
                    session_id: None,
                    limit: 1,
                    lease_ms: 600000,
                },
            )
            .unwrap()
            .remove(0);
        let mut resumed = fixture.clone();
        resumed.owner = claimed.lease_owner.unwrap();
        resumed.take_controller(&url);
        prepare_with_client(
            &store,
            "fake-docker-host",
            &resumed,
            limits(2, 1),
            || Ok(host.lock().unwrap().clone()),
            |_| Ok(()),
            |allocation| {
                assert!(
                    allocation.reused,
                    "a wait resumes the same physical instance"
                );
                Ok(allocation.container_id.clone().unwrap())
            },
        )
        .unwrap();
        assert_eq!(host.lock().unwrap().len(), index + 1);
    }
    assert_eq!(count(&url), 2);
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_resident_unavailable_or_corrupt_restore_source_preserves_old_execution() {
    use crate::docker_execution_host::{
        prepare_recovery_workspace_restore, RecoveryRestoreBinding, SessionWorkspaceLease,
    };
    use centaeris_core::runtime::contracts::RecoveryWorkspaceSnapshotV1;
    use sha2::{Digest, Sha256};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (url, store) = setup();
    let old = sandbox(&store, &url, "preflight-recovery", "preflight-tenant");
    let host = Arc::new(Mutex::new(Vec::new()));
    prepare(&store, &old, &host, limits(1, 1)).unwrap();
    let body = b"corrupt snapshot";
    let snapshot = RecoveryWorkspaceSnapshotV1 {
        object_ref: Some("immutable-checkpoint-object".into()),
        snapshot_sha256: format!("sha256:{:x}", Sha256::digest(body)),
        snapshot_size_bytes: body.len() as u64,
        expanded_size_bytes: 3,
        file_count: 1,
    };
    let lease = SessionWorkspaceLease {
        job_id: old.job.clone(),
        lease_owner: old.owner.clone(),
    };
    for status in [404, 200] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let api_url = format!("http://{}", listener.local_addr().unwrap());
        let hash = snapshot.snapshot_sha256.clone();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                socket.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
            }
            assert!(request.starts_with(
                b"POST /internal/agent-runs/execution-workspace/download HTTP/1.1\r\n"
            ));
            write!(socket, "HTTP/1.1 {status} fixture\r\nContent-Length: {}\r\nX-Content-Sha256: {hash}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
            socket.write_all(body).unwrap();
        });
        let result = prepare_recovery_workspace_restore(
            RecoveryRestoreBinding {
                api_url: &api_url,
                api_token: "local-test-token",
                agent_run_id: &old.run,
                authorization_digest: &format!("sha256:{}", "a".repeat(64)),
                lease: &lease,
                checkpoint_id: "preflight-checkpoint",
                data_tmpfs_bytes: 1024 * 1024 * 1024,
                input_upper_bound_bytes: 0,
            },
            &snapshot,
            None,
        );
        assert!(result.is_err());
        server.join().unwrap();
        assert_eq!(count(&url), 1);
        assert_eq!(host.lock().unwrap()[0].execution_id, old.execution);
        assert_eq!(
            crate::recovery_preparation_waiting("session_workspace_resolve_unavailable")
                .disposition,
            "waiting"
        );
    }
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_resident_restore_pins_owner_until_physical_completion_and_rejects_stale_dispatch() {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    };
    use std::time::Duration;
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (url, store) = setup();
    let fixture = sandbox(&store, &url, "restore-owner-fence", "restore-owner-tenant");
    let host = Arc::new(Mutex::new(Vec::new()));
    let allocation = prepare_allocation(&store, &fixture, &host, limits(1, 1)).unwrap();
    let expiry = Client::connect(&url, NoTls)
        .unwrap()
        .query_one(
            "SELECT lease_expires_at_ms FROM runtime.runtime_jobs WHERE job_id=$1",
            &[&fixture.job],
        )
        .unwrap()
        .get::<_, i64>(0);
    let (entered_tx, entered_rx) = mpsc::sync_channel(1);
    let (resume_tx, resume_rx) = mpsc::sync_channel(1);
    let restoring = store.clone();
    let owned = fixture.clone();
    let expected = allocation.clone();
    let task = std::thread::spawn(move || {
        let fence = owned.fence();
        restoring.with_retained_instance_operation(
            &owned.request("fake-docker-host", &fence),
            &expected,
            move |_| {
                entered_tx.send(()).unwrap();
                resume_rx
                    .recv_timeout(Duration::from_secs(5))
                    .map_err(|error| error.to_string())
            },
        )
    });
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let reclaimed = store
        .reclaim_expired_runtime_job_leases(expiry + 1)
        .unwrap();
    resume_tx.send(()).unwrap();
    task.join().unwrap().unwrap();
    assert_eq!(
        reclaimed, 0,
        "lease takeover must skip the owner pinned across physical restore"
    );
    assert_eq!(
        store
            .reclaim_expired_runtime_job_leases(expiry + 1)
            .unwrap(),
        1
    );
    let dispatched = AtomicBool::new(false);
    let fence = fixture.fence();
    assert!(store
        .with_retained_instance_operation(
            &fixture.request("fake-docker-host", &fence),
            &allocation,
            |_| {
                dispatched.store(true, Ordering::SeqCst);
                Ok(())
            }
        )
        .is_err());
    assert!(
        !dispatched.load(Ordering::SeqCst),
        "stale owner must not enter physical restore"
    );
    assert_eq!(count(&url), 1);
    assert_eq!(host.lock().unwrap()[0].execution_id, fixture.execution);
}
