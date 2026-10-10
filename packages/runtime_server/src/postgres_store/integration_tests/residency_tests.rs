use std::sync::{Arc, Mutex};

use centaeris_core::execution::ResidentResourceUsage;
use centaeris_core::session::reliability::{
    RuntimeJobStatus, RuntimeJobStorePort, ScheduleRuntimeJobRequest, YieldRuntimeJobRequest,
};
use postgres::{Client, NoTls};

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
    observer.batch_execute("DROP TABLE IF EXISTS public.app_core_sessionevent; DROP TABLE IF EXISTS public.app_core_session CASCADE; CREATE TABLE public.app_core_session(id text PRIMARY KEY,workspace_id text NOT NULL); CREATE TABLE public.app_core_sessionevent(\"eventId\" text PRIMARY KEY,workspace_id text NOT NULL,session_id text NOT NULL,agent_run_id text NOT NULL,sequence integer NOT NULL,agent_run_sequence integer,session_level boolean NOT NULL DEFAULT false,projects_to_agent_run_stream boolean NOT NULL,payload jsonb NOT NULL,\"createdAtMs\" bigint NOT NULL,\"insertedAt\" timestamptz NOT NULL DEFAULT clock_timestamp(),UNIQUE(session_id,sequence),UNIQUE(agent_run_id,agent_run_sequence));").unwrap();
    observer
        .execute(
            "INSERT INTO public.app_core_session VALUES($1,$2)",
            &[&session, &fixture.tenant],
        )
        .unwrap();
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
    store.prepare_resident_sandbox_with_client("durable-recovery-host", &fixture.request(), limits(1, 1),
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
            Ok(fixture.observed().container_id)
        }).unwrap();
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
    observer.batch_execute("DROP TABLE IF EXISTS public.app_core_sessionevent; DROP TABLE IF EXISTS public.app_core_session CASCADE; CREATE TABLE public.app_core_session(id text PRIMARY KEY,workspace_id text NOT NULL); CREATE TABLE public.app_core_sessionevent(\"eventId\" text PRIMARY KEY,workspace_id text NOT NULL,session_id text NOT NULL,agent_run_id text NOT NULL,sequence integer NOT NULL,agent_run_sequence integer,session_level boolean NOT NULL DEFAULT false,projects_to_agent_run_stream boolean NOT NULL,payload jsonb NOT NULL,\"createdAtMs\" bigint NOT NULL,\"insertedAt\" timestamptz NOT NULL DEFAULT clock_timestamp(),UNIQUE(session_id,sequence),UNIQUE(agent_run_id,agent_run_sequence));").unwrap();
    observer
        .execute(
            "INSERT INTO public.app_core_session VALUES($1,$2)",
            &[&session, &fixture.tenant],
        )
        .unwrap();
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
    store
        .prepare_resident_sandbox_with_client(
            "one-pool-host",
            &fixture.request(),
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
                Ok(fixture.observed().container_id)
            },
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
    prepare(&store, &fixture, &host, limits(1, 1)).unwrap();
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

use super::{job, reset_store, test_url, TEST_LOCK};
use crate::postgres_store::PostgresRuntimeStore;
use crate::resident_capacity::{
    ObservedResidentSandbox, ResidentCapacity, ResidentSandboxRequest, RESIDENT_CAPACITY_WAIT,
};

#[derive(Clone)]
struct SandboxFixture {
    run: String,
    job: String,
    execution: String,
    tenant: String,
    owner: String,
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
    fn request(&self) -> ResidentSandboxRequest<'_> {
        ResidentSandboxRequest {
            execution_id: &self.execution,
            agent_run_id: &self.run,
            workspace_id: &self.tenant,
            lifecycle_job_id: &self.job,
            lifecycle_lease_owner: &self.owner,
            must_exist: false,
            resources: resources(),
        }
    }
    fn observed(&self) -> ObservedResidentSandbox {
        ObservedResidentSandbox {
            execution_id: self.execution.clone(),
            agent_run_id: self.run.clone(),
            container_id: format!("container-{}", self.run),
            resources: resources(),
        }
    }
}

fn setup() -> (String, PostgresRuntimeStore) {
    let url = test_url();
    reset_store(&url);
    let mut db = Client::connect(&url, NoTls).unwrap();
    db.batch_execute("ALTER TABLE public.app_core_agentrun ADD COLUMN IF NOT EXISTS workspace_id text NOT NULL DEFAULT 'resident-test'; ALTER TABLE public.app_core_agentrun ADD COLUMN IF NOT EXISTS status text NOT NULL DEFAULT 'running';").unwrap();
    let store = PostgresRuntimeStore::new(&url).unwrap();
    (url, store)
}

fn sandbox(store: &PostgresRuntimeStore, url: &str, id: &str, tenant: &str) -> SandboxFixture {
    let fixture = SandboxFixture {
        run: id.into(),
        job: format!("agent_run.lifecycle:{id}"),
        execution: format!("execution-{id}"),
        tenant: tenant.into(),
        owner: format!("lease-owner-{id}"),
    };
    let mut db = Client::connect(url, NoTls).unwrap();
    db.execute("INSERT INTO public.app_core_agentrun(id,session_id,workspace_id,status) VALUES($1,$2,$3,'running')", &[&fixture.run, &format!("session-{id}"), &tenant]).unwrap();
    let now = db
        .query_one(
            "SELECT (EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint",
            &[],
        )
        .unwrap()
        .get::<_, i64>(0);
    let mut record = job(&fixture.job, &fixture.job);
    record.job_kind = "agent_run.lifecycle".into();
    record.status = RuntimeJobStatus::Running;
    record.lease_owner = Some(fixture.owner.clone());
    record.lease_expires_at_ms = Some(now + 600_000);
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
    store.prepare_resident_sandbox(
        "fake-docker-host",
        &fixture.request(),
        capacity,
        || Ok(host.lock().unwrap().clone()),
        || {
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
    prepare(&store, &first, &host, limits(2, 1)).unwrap();
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
                host.lock()
                    .unwrap()
                    .retain(|resident| resident.agent_run_id != first.run);
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
    assert!(store
        .prepare_resident_sandbox(
            "fake-docker-host",
            &first.request(),
            limits(1, 1),
            || Ok(Vec::new()),
            || Err("create response lost".into())
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
    assert_eq!(
        prepare(&store, &next, &host, limits(1, 1)).unwrap_err(),
        RESIDENT_CAPACITY_WAIT
    );
    let mut unknown = first.observed();
    unknown.execution_id = "unknown-execution".into();
    unknown.agent_run_id = "unknown-owner".into();
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
    prepare(&store, &first, &host, limits(1, 1)).unwrap();
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
    let parent = sandbox(&store, &url, "default-parent", "default-tenant");
    let child = sandbox(&store, &url, "default-child", "default-tenant");
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
    let host = Arc::new(Mutex::new(Vec::new()));
    let admit = |fixture: &SandboxFixture| {
        let mut request = fixture.request();
        request.resources = shipped;
        store.prepare_resident_sandbox(
            "fake-docker-host",
            &request,
            capacity,
            || Ok(host.lock().unwrap().clone()),
            || {
                let mut observed = fixture.observed();
                observed.resources = shipped;
                let id = observed.container_id.clone();
                host.lock().unwrap().push(observed);
                Ok(id)
            },
        )
    };
    admit(&parent).unwrap();
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
    assert_eq!(host.lock().unwrap()[0].agent_run_id, child.run);
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
    let denied = store.prepare_resident_sandbox(
        "other-fake-docker-host",
        &same_tenant.request(),
        limits(2, 1),
        || Ok(Vec::new()),
        || {
            creates += 1;
            Ok("unexpected-container".into())
        },
    );
    assert_eq!(denied.unwrap_err(), RESIDENT_CAPACITY_WAIT);
    assert_eq!(creates, 0);
    let denied = store.prepare_resident_sandbox(
        "other-fake-docker-host",
        &other_tenant.request(),
        limits(1, 1),
        || Ok(Vec::new()),
        || {
            creates += 1;
            Ok("unexpected-container".into())
        },
    );
    assert_eq!(denied.unwrap_err(), RESIDENT_CAPACITY_WAIT);
    assert_eq!(creates, 0);
    store
        .prepare_resident_sandbox(
            "other-fake-docker-host",
            &other_tenant.request(),
            limits(2, 1),
            || Ok(Vec::new()),
            || {
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
    db.batch_execute("DROP TABLE runtime.resident_sandboxes; DELETE FROM runtime.schema_migrations WHERE version=7; UPDATE runtime.runtime_jobs SET lease_reclaim_count=2,lease_reclaim_not_before_ms=12345;").unwrap();
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
