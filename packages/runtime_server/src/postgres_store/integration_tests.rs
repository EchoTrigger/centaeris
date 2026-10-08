use centaeris_core::runtime::contracts::{CheckpointRecord, EventVisibility, RuntimeEvent};
use centaeris_core::session::external_context::{
    ExternalContextObject, ExternalContextObjectLink, ExternalContextStorePort,
};
use centaeris_core::session::reliability::{
    CancelRuntimeJobRequest, ClaimDueRuntimeJobsRequest, CompleteRuntimeJobRequest,
    FailRuntimeJobRequest, RenewRuntimeJobLeaseRequest, RuntimeBackoffPolicy,
    RuntimeJobFailureDisposition, RuntimeJobOutboxPort, RuntimeJobRecord, RuntimeJobStatus,
    RuntimeJobStorePort, ScheduleRuntimeJobRequest, StartRuntimeJobRequest, YieldRuntimeJobRequest,
};
use centaeris_core::session::store::{
    AgentRuntimeSnapshotStorePort, ConsumeWaitCheckpointRequest, RuntimeStore,
    RuntimeStoreTransactionPort, SaveWaitCheckpointRequest, SessionDataStorePort,
    UpsertExternalContextLinkAndCompleteJobRequest,
};
use centaeris_core::session::supplement::{
    AcknowledgeTurnSupplementsRequest, ClaimTurnSupplementsRequest,
    CloseTurnSupplementQueueRequest, EnqueueTurnSupplementDisposition,
    EnqueueTurnSupplementRequest, TurnSupplementStoreError, TurnSupplementStorePort,
};
use centaeris_core::session::transcript::{
    TranscriptPagePolicyV1, TranscriptPageReadRequestV1, TranscriptPatchReadRequestV1,
    TranscriptProjectionStorePort, TRANSCRIPT_PROJECTION_VERSION_V1,
};
use centaeris_core::session::{
    RuntimeJobLeaseFence, SequencedSessionRecord, SessionLogPort, SessionLogRecord,
    SessionRecordType, RUNTIME_JOB_LEASE_FENCE_REJECTED, SESSION_EVENT_SCHEMA_VERSION,
};
use postgres::fallible_iterator::FallibleIterator;
use postgres::{Client, NoTls};
use std::time::Duration;

use super::PostgresRuntimeStore;
use centaeris_runtime_sqlite::SqliteRuntimeStore;

static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

mod snapshot_cas_tests;
mod tool_result_storage_tests;
mod wait_handoff_tests;

fn outbox_protocol(
    store: &PostgresRuntimeStore,
    path: &str,
    body: serde_json::Value,
) -> serde_json::Value {
    let headers = std::collections::HashMap::from([(
        "x-internal-token".to_string(),
        std::env::var("INTERNAL_API_TOKEN").expect("test token"),
    )]);
    let (status, response) = crate::job_protocol::handle(
        "POST",
        path,
        &headers,
        &serde_json::to_vec(&body).unwrap(),
        store,
    )
    .expect("job protocol response");
    let result: serde_json::Value = serde_json::from_slice(&response).unwrap();
    assert_eq!(status, 200, "{result}");
    result
}

fn terminal_source(store: &PostgresRuntimeStore, id: &str) {
    let mut source = job(id, id);
    source.session_id = None;
    source.status = RuntimeJobStatus::Running;
    source.lease_owner = Some("worker_source".to_string());
    source.lease_expires_at_ms = Some(10_000);
    store
        .schedule_runtime_job(ScheduleRuntimeJobRequest { job: source })
        .unwrap();
    store
        .complete_runtime_job(CompleteRuntimeJobRequest {
            job_id: id.to_string(),
            lease_owner: "worker_source".to_string(),
            output_refs: vec![],
            completed_at_ms: 20,
        })
        .unwrap();
}

fn late_waiter(store: &PostgresRuntimeStore, index: usize) -> String {
    use centaeris_core::runtime::contracts::{
        RuntimeAgentRunIdentityV1, RuntimeAwaitJobCheckpointV1, RuntimeJobWaitV1,
    };
    let agent = format!("agent_run_late_{index:04}");
    let session = format!("session_late_{index:04}");
    let turn = format!("turn_late_{index:04}");
    let id = centaeris_core::session::reliability::agent_run_lifecycle_job_id(&agent).unwrap();
    let mut lifecycle = job(&id, &id);
    lifecycle.job_kind = "agent_run.lifecycle".to_string();
    lifecycle.session_id = Some(session.clone());
    lifecycle.run_at_ms = 100_000;
    store
        .schedule_runtime_job(ScheduleRuntimeJobRequest { job: lifecycle })
        .unwrap();
    let digest = format!("sha256:{}", "a".repeat(64));
    let wait = RuntimeAwaitJobCheckpointV1::new(
        &RuntimeAgentRunIdentityV1 {
            agent_run_id: agent,
            execution_id: format!("execution_{index}"),
            authorization_digest: digest.clone(),
        },
        &turn,
        vec![RuntimeJobWaitV1 {
            tool_call_id: format!("call_{index}"),
            source_tool_name: "read_result".to_string(),
            tool_definition_digest: digest,
            job_id: "source:terminal".to_string(),
            job_kind: "contract.test".to_string(),
        }],
    )
    .unwrap();
    store
        .save_checkpoint(CheckpointRecord {
            checkpoint_id: format!("checkpoint:{index}"),
            kind: centaeris_core::runtime::contracts::CheckpointKindV1::Wait,
            session_id: session,
            turn_id: turn,
            status: "waiting".to_string(),
            done_reason: Some("runtime_job".to_string()),
            updated_at_ms: 50,
            payload_json: serde_json::to_string(&wait).unwrap(),
        })
        .unwrap();
    id
}

fn reconcile_waiter_pass(store: &PostgresRuntimeStore) -> serde_json::Value {
    let mut after = serde_json::Value::Null;
    let mut checked = 0;
    let mut pages = 0;
    let mut page_bytes = 0;
    let mut waiters = Vec::new();
    loop {
        let response = outbox_protocol(
            store,
            "/internal/job-outbox/reconcile-waiters",
            serde_json::json!({"schema":"runtime.job.waiters.reconcile.v1", "after":after}),
        );
        let count = response["checked"].as_u64().unwrap();
        assert!(count <= 256);
        checked += count;
        pages += 1;
        page_bytes += serde_json::to_vec(&response).unwrap().len();
        waiters.extend(response["waiters"].as_array().unwrap().iter().cloned());
        let next = response["next"].clone();
        if next.is_null() {
            break;
        }
        assert_ne!(next, after);
        assert!(count > 0);
        after = next;
    }
    serde_json::json!({"checked":checked, "waiters":waiters, "pages":pages, "pageBytes":page_bytes})
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_outbox_reconcile_does_not_replay_acknowledged_history() {
    let _guard = TEST_LOCK.lock().unwrap();
    let url = test_url();
    reset_store(&url);
    let store = PostgresRuntimeStore::new(&url).unwrap();
    terminal_source(&store, "history:acknowledged");
    store
        .mark_runtime_job_outbox_published("history:acknowledged", "runtime_job.terminal", 0, 30)
        .unwrap();
    terminal_source(&store, "source:unacknowledged");
    for now in [60_000, 120_000, 3_600_000] {
        outbox_protocol(
            &store,
            "/internal/jobs/reconcile",
            serde_json::json!({"schema":"runtime.job.reconcile.v1","nowMs":now}),
        );
        let pending = store.list_pending_runtime_job_outbox(256).unwrap();
        assert_eq!(
            pending.len(),
            1,
            "acknowledged history must not become pending again"
        );
        assert_eq!(pending[0].job_id, "source:unacknowledged");
        assert_eq!(pending[0].generation, 0);
    }
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_outbox_late_waiters_recover_after_ack_and_restart_across_pages() {
    let _guard = TEST_LOCK.lock().unwrap();
    let url = test_url();
    reset_store(&url);
    let store = PostgresRuntimeStore::new(&url).unwrap();
    terminal_source(&store, "source:terminal");
    let wake = outbox_protocol(
        &store,
        "/internal/job-outbox/wake-waiter",
        serde_json::json!({"schema":"runtime.job.waiter_wake.v1","jobId":"source:terminal","generation":0}),
    );
    assert_eq!(wake["disposition"], "no_waiter");
    store
        .mark_runtime_job_outbox_published("source:terminal", "runtime_job.terminal", 0, 30)
        .unwrap();
    let ids: Vec<_> = (0..257).map(|i| late_waiter(&store, i)).collect();
    drop(store);
    let handles: Vec<_> = (0..2)
        .map(|_| {
            let url = url.clone();
            std::thread::spawn(move || {
                let reopened = PostgresRuntimeStore::new(&url).unwrap();
                reconcile_waiter_pass(&reopened)
            })
        })
        .collect();
    for handle in handles {
        assert_eq!(handle.join().unwrap()["checked"], 257);
    }
    let reopened = PostgresRuntimeStore::new(&url).unwrap();
    for id in ids {
        assert_eq!(
            reopened.get_runtime_job(&id).unwrap().unwrap().run_at_ms,
            20
        );
    }
    assert!(reopened
        .list_pending_runtime_job_outbox(256)
        .unwrap()
        .is_empty());
    let mut client = Client::connect(&url, NoTls).unwrap();
    let count: i64 = client.query_one("SELECT count(*) FROM runtime.runtime_events WHERE event_type='runtime_job_wake_requested'", &[]).unwrap().get(0);
    assert_eq!(
        count, 257,
        "concurrent compensation must not multiply wake facts"
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_outbox_unacknowledged_delivery_survives_restart_and_duplicate_ack() {
    use centaeris_core::session::reliability::RuntimeJobOutboxPublishDisposition;
    let _guard = TEST_LOCK.lock().unwrap();
    let url = test_url();
    reset_store(&url);
    let store = PostgresRuntimeStore::new(&url).unwrap();
    terminal_source(&store, "source:terminal");
    late_waiter(&store, 0);
    outbox_protocol(
        &store,
        "/internal/job-outbox/wake-waiter",
        serde_json::json!({"schema":"runtime.job.waiter_wake.v1","jobId":"source:terminal","generation":0}),
    );
    drop(store); // Crash between durable wake and delivery acknowledgement.
    let store = PostgresRuntimeStore::new(&url).unwrap();
    assert_eq!(store.list_pending_runtime_job_outbox(10).unwrap().len(), 1);
    outbox_protocol(
        &store,
        "/internal/job-outbox/wake-waiter",
        serde_json::json!({"schema":"runtime.job.waiter_wake.v1","jobId":"source:terminal","generation":0}),
    );
    assert_eq!(
        store
            .mark_runtime_job_outbox_published("source:terminal", "runtime_job.terminal", 1, 40)
            .unwrap(),
        RuntimeJobOutboxPublishDisposition::Stale
    );
    assert_eq!(store.list_pending_runtime_job_outbox(10).unwrap().len(), 1);
    assert_eq!(
        store
            .mark_runtime_job_outbox_published("source:terminal", "runtime_job.terminal", 0, 40)
            .unwrap(),
        RuntimeJobOutboxPublishDisposition::Published
    );
    assert_eq!(
        store
            .mark_runtime_job_outbox_published("source:terminal", "runtime_job.terminal", 0, 41)
            .unwrap(),
        RuntimeJobOutboxPublishDisposition::AlreadyPublished
    );
    assert!(store
        .list_pending_runtime_job_outbox(10)
        .unwrap()
        .is_empty());
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_outbox_wake_during_active_waiter_survives_yield() {
    let _guard = TEST_LOCK.lock().unwrap();
    let url = test_url();
    reset_store(&url);
    let store = PostgresRuntimeStore::new(&url).unwrap();
    terminal_source(&store, "source:terminal");
    let id = late_waiter(&store, 0);
    let waiter_owner = store
        .claim_due_runtime_jobs(ClaimDueRuntimeJobsRequest {
            now_ms: 100_000,
            worker_id: "worker_waiter".to_string(),
            job_id: Some(id.clone()),
            job_kind: None,
            session_id: None,
            limit: 1,
            lease_ms: 10_000,
        })
        .unwrap()
        .remove(0)
        .lease_owner
        .unwrap();
    outbox_protocol(
        &store,
        "/internal/job-outbox/wake-waiter",
        serde_json::json!({"schema":"runtime.job.waiter_wake.v1","jobId":"source:terminal","generation":0}),
    );
    store
        .mark_runtime_job_outbox_published("source:terminal", "runtime_job.terminal", 0, 100_001)
        .unwrap();
    store
        .yield_runtime_job(YieldRuntimeJobRequest {
            job_id: id.clone(),
            lease_owner: waiter_owner,
            yielded_at_ms: 100_002,
            run_at_ms: 400_000,
            transition_reason: "runtime_job_wait".to_string(),
        })
        .unwrap();
    assert_eq!(
        store.get_runtime_job(&id).unwrap().unwrap().run_at_ms,
        100_002
    );
    assert!(store
        .list_pending_runtime_job_outbox(10)
        .unwrap()
        .is_empty());
}

fn test_url() -> String {
    assert_eq!(
        std::env::var("CENTAERIS_ALLOW_POSTGRES_TEST_RESET").as_deref(),
        Ok("1")
    );
    std::env::var("CENTAERIS_TEST_POSTGRES_URL").expect("CENTAERIS_TEST_POSTGRES_URL is required")
}

fn reset_store(url: &str) {
    let mut client = Client::connect(url, NoTls).expect("connect test Postgres");
    client.batch_execute("CREATE TABLE IF NOT EXISTS public.app_core_agentrun(id text PRIMARY KEY, session_id text NOT NULL, \"preAdmissionCancelledAt\" timestamptz); DELETE FROM public.app_core_agentrun;").expect("reset hosted run receipts");
    client
        .batch_execute("DROP SCHEMA IF EXISTS runtime CASCADE")
        .expect("reset runtime schema");
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_pre_admission_cancellation_is_durable_fenced_and_history_free() {
    let _guard = TEST_LOCK.lock().unwrap();
    let url = test_url();
    reset_store(&url);
    let store = PostgresRuntimeStore::new(&url).unwrap();
    let mut db = Client::connect(&url, NoTls).unwrap();
    db.batch_execute(
        "DROP TABLE IF EXISTS public.app_core_sessionevent CASCADE;
        DROP TABLE IF EXISTS public.app_core_session CASCADE;
        CREATE TABLE public.app_core_session(id text PRIMARY KEY,workspace_id text NOT NULL);
        CREATE TABLE public.app_core_sessionevent(agent_run_id text,session_level boolean NOT NULL);
        INSERT INTO public.app_core_session VALUES('cancel-session','cancel-workspace');
        INSERT INTO public.app_core_agentrun(id,session_id) VALUES('cancel-run','cancel-session');",
    )
    .unwrap();
    let mut record = job("agent_run.lifecycle:cancel-run", "unused");
    record.job_kind = "agent_run.lifecycle".into();
    record.idempotency_key = "agent_run.lifecycle:cancel-run:digest".into();
    record.session_id = Some("cancel-session".into());
    record.payload_ref = Some("record:agent_run:cancel-run".into());
    store
        .schedule_runtime_job(ScheduleRuntimeJobRequest { job: record })
        .unwrap();
    let now: i64 = db
        .query_one(
            "SELECT (EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint",
            &[],
        )
        .unwrap()
        .get(0);
    let owner = store
        .claim_due_runtime_jobs(ClaimDueRuntimeJobsRequest {
            now_ms: now,
            worker_id: "owner".into(),
            job_id: Some("agent_run.lifecycle:cancel-run".into()),
            job_kind: None,
            session_id: None,
            limit: 1,
            lease_ms: 60_000,
        })
        .unwrap()
        .remove(0)
        .lease_owner
        .unwrap();
    store
        .start_runtime_job(StartRuntimeJobRequest {
            job_id: "agent_run.lifecycle:cancel-run".into(),
            lease_owner: owner.clone(),
            started_at_ms: now,
        })
        .unwrap();
    let fence = RuntimeJobLeaseFence {
        job_id: "agent_run.lifecycle:cancel-run".into(),
        job_kind: "agent_run.lifecycle".into(),
        lease_owner: owner,
    };
    let log = store.session_log(
        "cancel-workspace".into(),
        "cancel-session".into(),
        "never admitted".into(),
    );
    assert!(log
        .commit_pre_admission_cancellation("cancel-run", "digest", &fence)
        .is_err());
    store
        .request_agent_run_cancellation("cancel-run", "cancel-session", "digest", now)
        .unwrap();
    let stale = RuntimeJobLeaseFence {
        lease_owner: "stale".into(),
        ..fence.clone()
    };
    assert!(log
        .commit_pre_admission_cancellation("cancel-run", "digest", &stale)
        .is_err());
    assert!(log
        .commit_pre_admission_cancellation("cancel-run", "wrong", &fence)
        .is_err());
    for _ in 0..2 {
        log.commit_pre_admission_cancellation("cancel-run", "digest", &fence)
            .unwrap();
    }
    let receipt: String = db.query_one("SELECT \"preAdmissionCancelledAt\"::text FROM public.app_core_agentrun WHERE id='cancel-run'", &[]).unwrap().get(0);
    log.commit_pre_admission_cancellation("cancel-run", "digest", &fence)
        .unwrap();
    assert_eq!(receipt, db.query_one("SELECT \"preAdmissionCancelledAt\"::text FROM public.app_core_agentrun WHERE id='cancel-run'", &[]).unwrap().get::<_, String>(0));
    assert_eq!(
        db.query_one("SELECT COUNT(*) FROM public.app_core_sessionevent", &[])
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    db.execute(
        "INSERT INTO public.app_core_sessionevent VALUES('cancel-run',false)",
        &[],
    )
    .unwrap();
    assert!(log
        .commit_pre_admission_cancellation("cancel-run", "digest", &fence)
        .is_err());
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_transcript_concurrent_duplicate_commit_is_idempotent() {
    use centaeris_core::session::transcript::{
        TranscriptProjectionCommitDispositionV1, TranscriptProjectionCommitV1,
    };
    let _guard = TEST_LOCK.lock().unwrap();
    let url = test_url();
    reset_store(&url);
    let store = PostgresRuntimeStore::new(&url).unwrap();
    let mut commit = TranscriptProjectionCommitV1 {
        commit_id: "first".into(),
        session_id: "race-session".into(),
        projection_version: TRANSCRIPT_PROJECTION_VERSION_V1.into(),
        projection_generation: "generation-1".into(),
        expected_source_high_water: "0".into(),
        source_high_water: "1".into(),
        upserts: vec![],
        resume_cursors: vec![],
        checkpoint: None,
        frontier: None,
        invalidation_reason: None,
    };
    store.commit_transcript_projection(commit.clone()).unwrap();
    commit.commit_id = "second".into();
    commit.expected_source_high_water = "1".into();
    commit.source_high_water = "2".into();
    let mut blocker = Client::connect(&url, NoTls).unwrap();
    let mut tx = blocker.transaction().unwrap();
    tx.query("SELECT source_high_water FROM runtime.transcript_projection_heads WHERE session_id='race-session' FOR UPDATE", &[]).unwrap();
    let workers = (0..2)
        .map(|_| {
            let store = store.clone();
            let commit = commit.clone();
            std::thread::spawn(move || store.commit_transcript_projection(commit))
        })
        .collect::<Vec<_>>();
    let mut observer = Client::connect(&url, NoTls).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let both_waiting = loop {
        let waiting: i64 = observer.query_one("SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query LIKE '%runtime.transcript_projection_heads%'", &[]).unwrap().get(0);
        if waiting == 2 {
            break true;
        }
        if std::time::Instant::now() >= deadline {
            break false;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    tx.commit().unwrap();
    let outcomes = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect::<Vec<_>>();
    assert!(both_waiting, "both writers must contend on the same head");
    assert_eq!(
        outcomes
            .iter()
            .filter(|r| matches!(r, Ok(TranscriptProjectionCommitDispositionV1::Applied)))
            .count(),
        1,
        "{outcomes:?}"
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|r| matches!(
                r,
                Ok(TranscriptProjectionCommitDispositionV1::AlreadyApplied)
            ))
            .count(),
        1,
        "{outcomes:?}"
    );
    assert_eq!(
        store
            .load_transcript_projection_head("race-session", "generation-1")
            .unwrap()
            .unwrap()
            .source_high_water,
        "2"
    );
    let progress = store
        .catch_up_transcript_projection("race-session", "generation-1", 1)
        .expect("a concurrent writer already satisfied the requested waterline");
    assert!(progress.caught_up);
    commit.source_high_water = "3".into();
    assert!(store
        .commit_transcript_projection(commit.clone())
        .unwrap_err()
        .contains("commitId conflict"));
    commit.commit_id = "different-commit".into();
    assert!(store
        .commit_transcript_projection(commit)
        .unwrap_err()
        .contains("sourceHighWater conflict"));
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_transcript_producer_serves_versioned_page_patch_and_deletes_derived_state() {
    let _guard = TEST_LOCK.lock().expect("Postgres test lock");
    let url = test_url();
    reset_store(&url);
    let mut setup = Client::connect(&url, NoTls).expect("connect transcript setup");
    setup
        .batch_execute(
            r#"
            DROP TABLE IF EXISTS public.app_core_sessionevent;
            DROP TABLE IF EXISTS public.app_core_session CASCADE;
            CREATE TABLE public.app_core_session(id varchar(64) PRIMARY KEY,workspace_id varchar(64) NOT NULL);
            CREATE TABLE public.app_core_sessionevent(
                "eventId" varchar(160) PRIMARY KEY,workspace_id varchar(64) NOT NULL,
                session_id varchar(64) NOT NULL,agent_run_id varchar(64) NOT NULL,
                sequence integer NOT NULL,agent_run_sequence integer,
                session_level boolean NOT NULL DEFAULT false,
                projects_to_agent_run_stream boolean NOT NULL,payload jsonb NOT NULL,
                "createdAtMs" bigint NOT NULL,"insertedAt" timestamptz NOT NULL DEFAULT clock_timestamp(),
                UNIQUE(session_id,sequence),UNIQUE(agent_run_id,agent_run_sequence)
            );
            INSERT INTO public.app_core_session(id,workspace_id) VALUES('session_transcript','workspace_transcript');
            "#,
        )
        .expect("create transcript source tables");
    drop(setup);
    let store = PostgresRuntimeStore::new(&url).expect("open Postgres transcript store");
    let log = super::PostgresSessionLog::new(
        store.ordinary_connections.clone(),
        "workspace_transcript".to_string(),
        "session_transcript".to_string(),
        "hello".to_string(),
    );
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let run = "agent_run_transcript";
    let records = vec![
        session_record(
            run,
            "session_transcript",
            1,
            SessionRecordType::AgentRunStarted,
            serde_json::json!({"userObjective":"hello"}),
            1,
        ),
        session_record(
            run,
            "session_transcript",
            2,
            SessionRecordType::UserMessage,
            serde_json::json!({"messageId":"message:turn_pg_fenced_terminal:user","text":"hello","attachments":[]}),
            2,
        ),
        session_record(
            run,
            "session_transcript",
            3,
            SessionRecordType::AssistantMessage,
            serde_json::json!({"messageId":"message:turn_pg_fenced_terminal:assistant","modelMarkdown":"done","artifactRefs":[],"status":"done"}),
            3,
        ),
        session_record(
            run,
            "session_transcript",
            4,
            SessionRecordType::AgentRunCompleted,
            serde_json::json!({"doneReason":"finalized"}),
            4,
        ),
    ];
    runtime
        .block_on(log.append_session_records(run, &records))
        .expect("append source facts independently of transcript projection");
    let first = store
        .catch_up_transcript_projection("session_transcript", "generation-1", 1)
        .expect("commit first projection event");
    assert_eq!(first.projected_high_water, 1);
    drop(log);
    drop(store);
    let store = PostgresRuntimeStore::new(&url).expect("reopen Postgres transcript store");
    let request = serde_json::to_vec(&serde_json::json!({
        "schema":"runtime.transcript.page.read.v1",
        "sessionId":"session_transcript",
        "projectionVersion":"transcript.projection.v1",
        "projectionGeneration":null,
        "sourceHighWater":"4",
        "olderCursor":null
    }))
    .unwrap();
    let mut served = false;
    for _ in 0..=records.len() {
        let (status, body) = crate::transcript_protocol::handle(
            "/internal/transcript/page",
            request.as_slice(),
            &store,
        )
        .expect("transcript protocol route")
        .expect("transcript protocol response");
        if status == 200 {
            let page: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(page["sourceHighWater"], "4");
            assert_eq!(page["projectionGeneration"], "generation-1");
            assert_eq!(
                page["resumeCursors"],
                serde_json::json!([{
                    "streamId": "workspace-transcript.v1",
                    "cursor": "4"
                }])
            );
            served = true;
            break;
        }
        assert_eq!(status, 409, "{}", String::from_utf8_lossy(&body));
        let progress: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(progress["error"], "transcript_projection_not_ready");
        assert_eq!(progress["sourceHighWater"], "4");
    }
    assert!(
        served,
        "bounded page retries must finish an existing backlog"
    );
    let page = store
        .load_transcript_page(TranscriptPageReadRequestV1 {
            session_id: "session_transcript".to_string(),
            projection_version: TRANSCRIPT_PROJECTION_VERSION_V1.to_string(),
            projection_generation: "generation-1".to_string(),
            source_high_water: "4".to_string(),
            older_cursor: None,
            policy: TranscriptPagePolicyV1::default(),
        })
        .expect("load projected transcript page");
    assert_eq!(page.work.raw_event_visits, 0);
    // AgentRun start and completion are first-class transcript boundary blocks
    // in the Core projection, alongside the user and assistant messages.
    assert_eq!(page.page.blocks.len(), 4);
    let content_request = serde_json::json!({
        "schema":"transcript.content.range.read.v1", "sessionId":"session_transcript",
        "projectionVersion":"transcript.projection.v1", "projectionGeneration":"generation-1",
        "refId":format!("session-event:{}:modelMarkdown", records[2].event.event_id),
        "revision":"1", "byteLength":"4", "offset":"0", "maxBytes":2
    });
    let (status, body) = crate::transcript_protocol::handle(
        "/internal/transcript/content",
        &serde_json::to_vec(&content_request).unwrap(),
        &store,
    )
    .unwrap()
    .unwrap();
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    let content: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(content["content"], "do");
    assert_eq!(content["hasMore"], true);
    for (field, value) in [
        ("sessionId", "foreign"),
        ("projectionGeneration", "stale"),
        ("byteLength", "5"),
    ] {
        let mut invalid = content_request.clone();
        invalid[field] = value.into();
        let (status, _) = crate::transcript_protocol::handle(
            "/internal/transcript/content",
            &serde_json::to_vec(&invalid).unwrap(),
            &store,
        )
        .unwrap()
        .unwrap();
        assert_eq!(status, 409);
    }

    let patches = store
        .load_transcript_patches(TranscriptPatchReadRequestV1 {
            session_id: "session_transcript".to_string(),
            projection_version: TRANSCRIPT_PROJECTION_VERSION_V1.to_string(),
            projection_generation: "generation-1".to_string(),
            after_source_high_water: "2".to_string(),
            through_source_high_water: "4".to_string(),
        })
        .expect("load committed transcript patches");
    assert_eq!(patches.work.raw_event_visits, 0);
    assert_eq!(patches.next_source_high_water, "4");

    let mut storage = Client::connect(&url, NoTls).expect("inspect transcript storage");
    let recovery_count = storage
        .query_one(
            "SELECT count(*) FROM runtime.transcript_projection_current_recoveries WHERE session_id='session_transcript' AND projection_generation='generation-1'",
            &[],
        )
        .expect("count current transcript recoveries")
        .get::<_, i64>(0);
    assert_eq!(
        recovery_count, 1,
        "current recovery is a single overwritten slot"
    );
    let commits_with_recovery = storage
        .query_one(
            "SELECT count(*) FROM runtime.transcript_projection_commits WHERE session_id='session_transcript' AND (commit_json::jsonb->'checkpoint' <> 'null'::jsonb OR commit_json::jsonb->'frontier' <> 'null'::jsonb)",
            &[],
        )
        .expect("inspect transcript patch history")
        .get::<_, i64>(0);
    assert_eq!(
        commits_with_recovery, 0,
        "patch history must not copy current recovery payloads"
    );

    let live_log = store.session_log(
        "workspace_transcript".to_string(),
        "session_transcript".to_string(),
        "next".to_string(),
    );
    let next_run = "agent_run_transcript_next";
    runtime
        .block_on(live_log.append_session_records(
            next_run,
            &[
                session_record(
                    next_run,
                    "session_transcript",
                    1,
                    SessionRecordType::AgentRunStarted,
                    serde_json::json!({"userObjective":"next"}),
                    5,
                ),
                session_record(
                    next_run,
                    "session_transcript",
                    2,
                    SessionRecordType::UserMessage,
                    serde_json::json!({"messageId":"message:turn_transcript_next:user","text":"next","attachments":[]}),
                    6,
                ),
            ].map(|mut record| {
                record.event.turn_id = Some("turn_transcript_next".into());
                record
            }),
        ))
        .expect("append new facts while transcript producer is active");
    for _ in 0..=2 {
        if store
            .catch_up_transcript_projection("session_transcript", "generation-1", 6)
            .expect("catch up the appended facts in bounded slices")
            .caught_up
        {
            break;
        }
    }
    let new_patches = store
        .load_transcript_patches(TranscriptPatchReadRequestV1 {
            session_id: "session_transcript".to_string(),
            projection_version: TRANSCRIPT_PROJECTION_VERSION_V1.to_string(),
            projection_generation: "generation-1".to_string(),
            after_source_high_water: "4".to_string(),
            through_source_high_water: "6".to_string(),
        })
        .expect("new source facts are projected without a full history replay");
    assert_eq!(new_patches.next_source_high_water, "6");
    assert_eq!(new_patches.patches.len(), 2);

    store
        .delete_session_data("session_transcript")
        .expect("delete session-derived transcript state");
    assert!(store
        .load_transcript_projection_head("session_transcript", "generation-1")
        .expect("load deleted transcript head")
        .is_none());

    let mut break_projection = Client::connect(&url, NoTls).expect("connect projection failure");
    break_projection
        .batch_execute("DROP TABLE runtime.transcript_projection_heads CASCADE")
        .expect("break only the derived transcript read model");
    drop(break_projection);
    let failure_log = store.session_log(
        "workspace_transcript".to_string(),
        "session_transcript".to_string(),
        "facts survive".to_string(),
    );
    let failure_run = "agent_run_projection_failure";
    let receipt = runtime
        .block_on(failure_log.append_session_records(
            failure_run,
            &[
                session_record(
                    failure_run,
                    "session_transcript",
                    1,
                    SessionRecordType::AgentRunStarted,
                    serde_json::json!({"userObjective":"facts survive"}),
                    7,
                ),
                session_record(
                    failure_run,
                    "session_transcript",
                    2,
                    SessionRecordType::UserMessage,
                    serde_json::json!({"messageId":"message:turn_transcript_failure:user","text":"facts survive","attachments":[]}),
                    8,
                ),
            ].map(|mut record| {
                record.event.turn_id = Some("turn_transcript_failure".into());
                record
            }),
        ))
        .expect("derived projection failure must not turn a committed source append into failure");
    assert_eq!(receipt.records.len(), 2);
    let mut verify = Client::connect(&url, NoTls).expect("verify committed source facts");
    let count = verify
        .query_one(
            "SELECT count(*) FROM public.app_core_sessionevent WHERE agent_run_id=$1",
            &[&failure_run],
        )
        .unwrap()
        .get::<_, i64>(0);
    assert_eq!(count, 2);
}

fn job(id: &str, key: &str) -> RuntimeJobRecord {
    RuntimeJobRecord {
        job_id: id.to_string(),
        job_kind: "contract.test".to_string(),
        status: RuntimeJobStatus::Queued,
        run_at_ms: 1,
        lease_owner: None,
        lease_expires_at_ms: None,
        heartbeat_at_ms: None,
        retry_count: 0,
        max_retries: 2,
        backoff_policy: RuntimeBackoffPolicy::default(),
        idempotency_key: key.to_string(),
        session_id: Some("session_pg".to_string()),
        branch_id: None,
        checkpoint_id: None,
        payload_ref: None,
        output_refs: vec![],
        last_error: None,
        created_at_ms: 1,
        updated_at_ms: 1,
    }
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_same_worker_reclaim_fences_old_writes_and_result_transaction() {
    let _guard = TEST_LOCK.lock().unwrap();
    let url = test_url();
    reset_store(&url);
    let store = PostgresRuntimeStore::new(&url).unwrap();
    store
        .schedule_runtime_job(ScheduleRuntimeJobRequest {
            job: job("job_pg_reclaim", "key_pg_reclaim"),
        })
        .unwrap();
    let claim = |store: &PostgresRuntimeStore, now_ms| {
        store
            .claim_due_runtime_jobs(ClaimDueRuntimeJobsRequest {
                now_ms,
                worker_id: "worker:stable-worker".into(),
                job_id: Some("job_pg_reclaim".into()),
                job_kind: None,
                session_id: None,
                limit: 1,
                lease_ms: 100,
            })
            .unwrap()
            .remove(0)
            .lease_owner
            .unwrap()
    };
    let old = claim(&store, 10);
    store
        .start_runtime_job(StartRuntimeJobRequest {
            job_id: "job_pg_reclaim".into(),
            lease_owner: old.clone(),
            started_at_ms: 11,
        })
        .unwrap();
    drop(store);
    let store = PostgresRuntimeStore::new(&url).unwrap();
    assert_eq!(
        store
            .get_runtime_job("job_pg_reclaim")
            .unwrap()
            .unwrap()
            .lease_owner
            .as_deref(),
        Some(old.as_str())
    );
    assert_eq!(store.reclaim_expired_runtime_job_leases(110).unwrap(), 1);
    let current = claim(&store, 110);
    assert_ne!(
        old, current,
        "each claim needs a new identity even for the same worker"
    );
    assert!(store
        .start_runtime_job(StartRuntimeJobRequest {
            job_id: "job_pg_reclaim".into(),
            lease_owner: old.clone(),
            started_at_ms: 111
        })
        .is_err());
    assert!(store
        .renew_runtime_job_lease(RenewRuntimeJobLeaseRequest {
            job_id: "job_pg_reclaim".into(),
            lease_owner: old.clone(),
            heartbeat_at_ms: 111,
            lease_ms: 100
        })
        .is_err());
    assert!(store
        .yield_runtime_job(YieldRuntimeJobRequest {
            job_id: "job_pg_reclaim".into(),
            lease_owner: old.clone(),
            yielded_at_ms: 111,
            run_at_ms: 120,
            transition_reason: "stale".into()
        })
        .is_err());
    assert!(store
        .fail_runtime_job(FailRuntimeJobRequest {
            job_id: "job_pg_reclaim".into(),
            lease_owner: old.clone(),
            failed_at_ms: 111,
            last_error: "stale".into(),
            next_run_at_ms: None,
            disposition: RuntimeJobFailureDisposition::Failed
        })
        .is_err());
    assert!(store
        .complete_runtime_job(CompleteRuntimeJobRequest {
            job_id: "job_pg_reclaim".into(),
            lease_owner: old.clone(),
            output_refs: vec![],
            completed_at_ms: 111
        })
        .is_err());
    assert!(store
        .cancel_runtime_job(CancelRuntimeJobRequest {
            job_id: "job_pg_reclaim".into(),
            reason: "stale".into(),
            cancelled_at_ms: 111,
            expected_status: None,
            expected_lease_owner: Some(old.clone())
        })
        .is_err());
    let object = ExternalContextObject {
        schema_version: "external_context.object.v1".into(),
        object_id: "old_result".into(),
        object_kind: "subagent_result".into(),
        source_provider_id: "test".into(),
        source_tool_name: "test".into(),
        title: "old".into(),
        content: "old result".into(),
        metadata: serde_json::json!({}),
        updated_at_ms: 111,
    };
    let link = ExternalContextObjectLink {
        session_id: "session_pg".into(),
        turn_id: Some("turn_pg".into()),
        tool_call_id: Some("call_pg".into()),
        object_id: object.object_id.clone(),
        source_provider_id: "test".into(),
        source_tool_name: "test".into(),
        linked_at_ms: 111,
    };
    assert!(store
        .upsert_external_context_link_and_complete_job(
            UpsertExternalContextLinkAndCompleteJobRequest {
                object: Some(object),
                link: Some(link),
                complete_job: CompleteRuntimeJobRequest {
                    job_id: "job_pg_reclaim".into(),
                    lease_owner: old,
                    output_refs: vec!["old_result".into()],
                    completed_at_ms: 111
                },
            }
        )
        .is_err());
    assert!(store
        .load_external_context_object("old_result")
        .unwrap()
        .is_none());
    assert!(store
        .load_external_context_object_link("session_pg", "old_result", "turn_pg", "call_pg")
        .unwrap()
        .is_none());
    assert!(store
        .list_pending_runtime_job_outbox(10)
        .unwrap()
        .is_empty());
    assert_eq!(
        store
            .get_runtime_job("job_pg_reclaim")
            .unwrap()
            .unwrap()
            .status,
        RuntimeJobStatus::Leased
    );
    store
        .start_runtime_job(StartRuntimeJobRequest {
            job_id: "job_pg_reclaim".into(),
            lease_owner: current.clone(),
            started_at_ms: 111,
        })
        .unwrap();
    store
        .complete_runtime_job(CompleteRuntimeJobRequest {
            job_id: "job_pg_reclaim".into(),
            lease_owner: current,
            output_refs: vec![],
            completed_at_ms: 112,
        })
        .unwrap();
    assert_eq!(
        store
            .get_runtime_job("job_pg_reclaim")
            .unwrap()
            .unwrap()
            .status,
        RuntimeJobStatus::Succeeded
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_worker_cancel_checks_claim_but_user_cancel_targets_job() {
    let _guard = TEST_LOCK.lock().unwrap();
    let url = test_url();
    reset_store(&url);
    let store = PostgresRuntimeStore::new(&url).unwrap();
    let mut legacy = job("job_pg_cancel", "key_pg_cancel");
    legacy.status = RuntimeJobStatus::Leased;
    legacy.lease_owner = Some("legacy_claim_identity".into());
    legacy.lease_expires_at_ms = Some(200);
    store
        .schedule_runtime_job(ScheduleRuntimeJobRequest { job: legacy })
        .unwrap();
    drop(store);
    let store = PostgresRuntimeStore::new(&url).unwrap();
    assert_eq!(
        store
            .get_runtime_job("job_pg_cancel")
            .unwrap()
            .unwrap()
            .lease_owner
            .as_deref(),
        Some("legacy_claim_identity")
    );
    store
        .renew_runtime_job_lease(RenewRuntimeJobLeaseRequest {
            job_id: "job_pg_cancel".into(),
            lease_owner: "legacy_claim_identity".into(),
            heartbeat_at_ms: 100,
            lease_ms: 100,
        })
        .unwrap();
    assert_eq!(
        store
            .get_runtime_job("job_pg_cancel")
            .unwrap()
            .unwrap()
            .lease_owner
            .as_deref(),
        Some("legacy_claim_identity"),
        "renewal preserves a released lease identity"
    );
    assert!(store
        .cancel_runtime_job(CancelRuntimeJobRequest {
            job_id: "job_pg_cancel".into(),
            reason: "stale worker".into(),
            cancelled_at_ms: 101,
            expected_status: Some(RuntimeJobStatus::Leased),
            expected_lease_owner: Some("wrong_claim_identity".into())
        })
        .is_err());
    assert_eq!(
        store
            .get_runtime_job("job_pg_cancel")
            .unwrap()
            .unwrap()
            .status,
        RuntimeJobStatus::Leased
    );
    store
        .cancel_runtime_job(CancelRuntimeJobRequest {
            job_id: "job_pg_cancel".into(),
            reason: "user requested".into(),
            cancelled_at_ms: 102,
            expected_status: None,
            expected_lease_owner: None,
        })
        .unwrap();
    assert_eq!(
        store
            .get_runtime_job("job_pg_cancel")
            .unwrap()
            .unwrap()
            .status,
        RuntimeJobStatus::Cancelled
    );
    let mut published = job("job_pg_published_transition", "key_pg_published_transition");
    published.status = RuntimeJobStatus::Leased;
    published.lease_owner = Some("released_worker_identity".into());
    published.lease_expires_at_ms = Some(200);
    store
        .schedule_runtime_job(ScheduleRuntimeJobRequest { job: published })
        .unwrap();
    drop(store);
    let store = PostgresRuntimeStore::new(&url).unwrap();
    assert_eq!(
        store
            .get_runtime_job("job_pg_published_transition")
            .unwrap()
            .unwrap()
            .lease_owner
            .as_deref(),
        Some("released_worker_identity")
    );
    assert_eq!(store.reclaim_expired_runtime_job_leases(200).unwrap(), 1);
    let claim = store
        .claim_due_runtime_jobs(ClaimDueRuntimeJobsRequest {
            now_ms: 200,
            worker_id: "released_worker_identity".into(),
            job_id: Some("job_pg_published_transition".into()),
            job_kind: None,
            session_id: None,
            limit: 1,
            lease_ms: 100,
        })
        .unwrap()
        .remove(0);
    assert_ne!(
        claim.lease_owner.as_deref(),
        Some("released_worker_identity")
    );
    store
        .complete_runtime_job(CompleteRuntimeJobRequest {
            job_id: claim.job_id,
            lease_owner: claim.lease_owner.unwrap(),
            output_refs: vec![],
            completed_at_ms: 201,
        })
        .unwrap();
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_hosted_worker_reclaim_changes_claim_identity() {
    let _guard = TEST_LOCK.lock().unwrap();
    let url = test_url();
    reset_store(&url);
    let store = PostgresRuntimeStore::new(&url).unwrap();
    let id = "agent_run.lifecycle:agent_run_claim_generation";
    let mut lifecycle = job(id, id);
    lifecycle.job_kind = "agent_run.lifecycle".into();
    store
        .schedule_worker_job(
            ScheduleRuntimeJobRequest { job: lifecycle },
            Some("workspace_claim_generation"),
        )
        .unwrap();
    let claim = |store: &PostgresRuntimeStore| {
        store
            .claim_worker_jobs(ClaimDueRuntimeJobsRequest {
                now_ms: 1,
                worker_id: "worker:stable-hosted-worker".into(),
                job_id: Some(id.into()),
                job_kind: Some("agent_run.lifecycle".into()),
                session_id: None,
                limit: 1,
                lease_ms: 60_000,
            })
            .unwrap()
            .remove(0)
            .lease_owner
            .unwrap()
    };
    let old = claim(&store);
    let mut db = Client::connect(&url, NoTls).unwrap();
    db.execute(
        "UPDATE runtime.runtime_jobs SET lease_expires_at_ms=0 WHERE job_id=$1",
        &[&id],
    )
    .unwrap();
    let now: i64 = db
        .query_one(
            "SELECT (EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint",
            &[],
        )
        .unwrap()
        .get(0);
    assert_eq!(store.reclaim_expired_runtime_job_leases(now).unwrap(), 1);
    let current = claim(&store);
    assert_ne!(old, current);
    assert!(store
        .complete_runtime_job(CompleteRuntimeJobRequest {
            job_id: id.into(),
            lease_owner: old,
            output_refs: vec![],
            completed_at_ms: now + 1
        })
        .is_err());
    store
        .complete_runtime_job(CompleteRuntimeJobRequest {
            job_id: id.into(),
            lease_owner: current,
            output_refs: vec![],
            completed_at_ms: now + 1,
        })
        .unwrap();
    assert_eq!(
        store.get_runtime_job(id).unwrap().unwrap().status,
        RuntimeJobStatus::Succeeded
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_subagent_projection_uses_core_durable_job_binding() {
    use centaeris_core::runtime::persist_subagent_result_projection_from_scheduler_events;
    use centaeris_core::runtime::subagent::{
        build_subagent_run_job, SubagentLifecycleStatus, SubagentRunJobRequest,
        SubagentSchedulerEvent, SubagentSchedulerEventKind,
    };
    use centaeris_core::session::manager::SessionManager;

    let _guard = TEST_LOCK.lock().unwrap();
    let url = test_url();
    reset_store(&url);
    let store = PostgresRuntimeStore::new(&url).unwrap();
    let mut job = build_subagent_run_job(SubagentRunJobRequest {
        session_id: "session_projection_parent".into(),
        parent_turn_id: "turn_projection_parent".into(),
        tool_call_id: "call_projection".into(),
        subagent_id: "agent_projection".into(),
        work_packet_ref: "external_context:subagent_work_packet:projection".into(),
        checkpoint_id: None,
        run_at_ms: 1,
        created_at_ms: 1,
        max_retries: 0,
    });
    job.status = RuntimeJobStatus::Succeeded;
    job.output_refs = vec!["external_context:subagent_result:projection".into()];
    job.updated_at_ms = 20;
    let event = SubagentSchedulerEvent {
        kind: SubagentSchedulerEventKind::Succeeded,
        subagent_id: "agent_projection".into(),
        child_session_id: "session-agent_projection".into(),
        parent_turn_id: "turn_projection_parent".into(),
        job_id: job.job_id.clone(),
        work_packet_ref: job.payload_ref.clone(),
        result_ref: job.output_refs.first().cloned(),
        worker_id: Some("worker:projection".into()),
        status: SubagentLifecycleStatus::Succeeded,
        summary: "completed".into(),
        description: None,
        started_at_ms: None,
        completed_at_ms: Some(20),
        at_ms: 20,
    };
    store
        .schedule_runtime_job(ScheduleRuntimeJobRequest { job })
        .unwrap();
    let project = |session_id: &str, event: SubagentSchedulerEvent| {
        persist_subagent_result_projection_from_scheduler_events(&store, session_id, &[event])
    };
    assert!(project("session_projection_other", event.clone()).is_err());
    let mut wrong = event.clone();
    wrong.subagent_id = "agent_other".into();
    assert!(project("session_projection_parent", wrong).is_err());
    let mut wrong = event.clone();
    wrong.child_session_id = "session-agent_other".into();
    assert!(project("session_projection_parent", wrong).is_err());
    let mut wrong = event.clone();
    wrong.parent_turn_id = "turn_other".into();
    assert!(project("session_projection_parent", wrong).is_err());
    let mut wrong = event.clone();
    wrong.result_ref = Some("external_context:subagent_result:old".into());
    assert!(project("session_projection_parent", wrong).is_err());
    let mut wrong = event.clone();
    wrong.status = SubagentLifecycleStatus::Failed;
    assert!(project("session_projection_parent", wrong).is_err());
    assert!(SessionManager::new(store.clone())
        .load_session("session_projection_parent")
        .unwrap()
        .is_none());
    assert_eq!(
        project("session_projection_parent", event.clone()).unwrap(),
        1
    );
    assert_eq!(project("session_projection_parent", event).unwrap(), 0);
    assert!(SessionManager::new(store)
        .load_session("session_projection_parent")
        .unwrap()
        .is_some());
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_checkpoint_profile_reconcile_waiters() {
    let _guard = TEST_LOCK.lock().unwrap();
    let url = test_url();
    reset_store(&url);
    let store = PostgresRuntimeStore::new(&url).unwrap();
    let waiting: usize = std::env::var("CENTAERIS_CHECKPOINT_PROFILE_WAITING")
        .expect("CENTAERIS_CHECKPOINT_PROFILE_WAITING is required")
        .parse()
        .expect("profile waiting count must be an integer");
    assert!(
        waiting <= 1024,
        "profile is intentionally bounded at 1024 waiters"
    );
    let terminal = match std::env::var("CENTAERIS_CHECKPOINT_PROFILE_SOURCE")
        .expect("CENTAERIS_CHECKPOINT_PROFILE_SOURCE is required")
        .as_str()
    {
        "terminal" => true,
        "nonterminal" => false,
        value => panic!("unsupported checkpoint profile source: {value}"),
    };
    if terminal {
        terminal_source(&store, "source:terminal");
    } else {
        let mut source = job("source:terminal", "source:terminal");
        source.session_id = None;
        store
            .schedule_runtime_job(ScheduleRuntimeJobRequest { job: source })
            .unwrap();
    }
    for index in 0..waiting {
        late_waiter(&store, index);
    }

    let mut measurements = Vec::new();
    let passes = if terminal { 2 } else { 1 };
    for pass in 1..=passes {
        let mut audit = Client::connect(&url, NoTls).unwrap();
        let sessions_before: i64 = audit
            .query_one(
                "SELECT sessions FROM pg_stat_database WHERE datname=current_database()",
                &[],
            )
            .unwrap()
            .get(0);
        let statements_before = checkpoint_profile_statements(&mut audit);
        let started = std::time::Instant::now();
        let response = reconcile_waiter_pass(&store);
        let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
        let sessions_after: i64 = audit
            .query_one(
                "SELECT sessions FROM pg_stat_database WHERE datname=current_database()",
                &[],
            )
            .unwrap()
            .get(0);
        let statements_after = checkpoint_profile_statements(&mut audit);
        let mut dispositions = std::collections::BTreeMap::<String, usize>::new();
        if let Some(waiters) = response["waiters"].as_array() {
            for waiter in waiters {
                let disposition = waiter["disposition"].as_str().unwrap().to_string();
                *dispositions.entry(disposition).or_default() += 1;
            }
        }
        let row = audit.query_one(
            "SELECT \
             (SELECT count(*) FROM runtime.checkpoints WHERE kind='wait' AND status='waiting' AND done_reason='runtime_job'),\
             (SELECT count(*) FROM runtime.runtime_job_outbox),\
             (SELECT count(*) FROM runtime.runtime_events WHERE event_type='runtime_job_wake_requested')",
            &[],
        ).unwrap();
        assert_eq!(response["checked"].as_u64(), Some(waiting as u64));
        assert_eq!(
            response["waiters"].as_array().map_or(0, Vec::len),
            if terminal { waiting } else { 0 }
        );
        assert_eq!(
            row.get::<_, i64>(2),
            if terminal { waiting as i64 } else { 0 },
            "a repeated pass must not append duplicate wake events"
        );
        measurements.push(serde_json::json!({
            "pass": pass,
            "elapsedMs": elapsed_ms,
            "responseBytes": response["pageBytes"],
            "pageQueries": response["pages"],
            "databaseSessionsBefore": sessions_before,
            "databaseSessionsAfter": sessions_after,
            "databaseSessions": sessions_after - sessions_before,
            "pgStatStatementsBefore": checkpoint_profile_statement_snapshot(&statements_before),
            "pgStatStatementsAfter": checkpoint_profile_statement_snapshot(&statements_after),
            "pgStatStatements": checkpoint_profile_statement_delta(&statements_before, &statements_after),
            "checked": response["checked"],
            "returnedWaiters": response["waiters"].as_array().map_or(0, Vec::len),
            "dispositions": dispositions,
            "waitingRows": row.get::<_, i64>(0),
            "outboxRows": row.get::<_, i64>(1),
            "wakeEventRows": row.get::<_, i64>(2),
        }));
    }
    println!(
        "CHECKPOINT_PROFILE_JSON={}",
        serde_json::json!({
            "waiting": waiting,
            "source": if terminal { "terminal" } else { "nonterminal" },
            "pageSize": 256,
            "minimumPageQueries": waiting / 256 + 1,
            "measurementBoundary": "in_process_protocol_call_includes_handler_json_encode_and_test_helper_json_decode_excludes_http_transport",
            "measurements": measurements,
        })
    );
}

fn checkpoint_profile_statements(
    client: &mut Client,
) -> std::collections::BTreeMap<String, (String, i64, i64, f64, i64, i64)> {
    client
        .query(
            "SELECT queryid::text,query,calls::bigint,rows::bigint,total_exec_time,shared_blks_hit,shared_blks_read \
             FROM pg_stat_statements WHERE dbid=(SELECT oid FROM pg_database WHERE datname=current_database()) \
             AND query NOT ILIKE '%pg_stat_statements%' \
             AND (query ILIKE '%checkpoints%' OR query ILIKE '%runtime_jobs%' OR query ILIKE '%runtime_events%')",
            &[],
        )
        .expect("pg_stat_statements must be installed by the isolated profile harness")
        .into_iter()
        .map(|row| {
            (
                row.get::<_, String>(0),
                (
                    row.get(1),
                    row.get(2),
                    row.get(3),
                    row.get(4),
                    row.get(5),
                    row.get(6),
                ),
            )
        })
        .collect()
}

fn checkpoint_profile_statement_delta(
    before: &std::collections::BTreeMap<String, (String, i64, i64, f64, i64, i64)>,
    after: &std::collections::BTreeMap<String, (String, i64, i64, f64, i64, i64)>,
) -> Vec<serde_json::Value> {
    after
        .iter()
        .filter_map(|(query_id, (query, calls, rows, total_ms, hits, reads))| {
            let baseline = before.get(query_id);
            let calls_delta = calls - baseline.map_or(0, |value| value.1);
            (calls_delta > 0).then(|| {
                serde_json::json!({
                    "queryId": query_id,
                    "query": query,
                    "calls": calls_delta,
                    "rows": rows - baseline.map_or(0, |value| value.2),
                    "totalExecMs": total_ms - baseline.map_or(0.0, |value| value.3),
                    "sharedBlocksHit": hits - baseline.map_or(0, |value| value.4),
                    "sharedBlocksRead": reads - baseline.map_or(0, |value| value.5),
                })
            })
        })
        .collect()
}

fn checkpoint_profile_statement_snapshot(
    snapshot: &std::collections::BTreeMap<String, (String, i64, i64, f64, i64, i64)>,
) -> Vec<serde_json::Value> {
    snapshot
        .iter()
        .map(|(query_id, (query, calls, rows, total_ms, hits, reads))| {
            serde_json::json!({
                "queryId": query_id,
                "query": query,
                "calls": calls,
                "rows": rows,
                "totalExecMs": total_ms,
                "sharedBlocksHit": hits,
                "sharedBlocksRead": reads,
            })
        })
        .collect()
}

fn session_record(
    agent_run_id: &str,
    session_id: &str,
    sequence: u64,
    event_type: SessionRecordType,
    payload: serde_json::Value,
    at_ms: i64,
) -> SequencedSessionRecord {
    SequencedSessionRecord {
        sequence,
        event: SessionLogRecord {
            schema_version: SESSION_EVENT_SCHEMA_VERSION.to_string(),
            event_version: centaeris_core::session::SESSION_EVENT_VERSION,
            event_type,
            event_id: format!("event:{agent_run_id}:{sequence}"),
            session_id: session_id.to_string(),
            turn_id: Some("turn_pg_fenced_terminal".to_string()),
            agent_run_id: Some(agent_run_id.to_string()),
            created_at_ms: at_ms,
            payload,
        },
    }
}

fn shared_runtime_store_contract<
    S: RuntimeStore + AgentRuntimeSnapshotStorePort + RuntimeStoreTransactionPort,
>(
    store: &S,
) {
    shared_waiter_index_contract(store);
    store
        .save_checkpoint(CheckpointRecord {
            checkpoint_id: "checkpoint:shared".to_string(),
            kind: centaeris_core::runtime::contracts::CheckpointKindV1::Wait,
            session_id: "shared_session".to_string(),
            turn_id: "shared_turn".to_string(),
            status: "running".to_string(),
            done_reason: None,
            updated_at_ms: 3,
            payload_json: "{\"shared\":true}".to_string(),
        })
        .expect("shared checkpoint");
    assert_eq!(
        store
            .load_latest_checkpoint("shared_session")
            .expect("shared load checkpoint")
            .expect("shared row")
            .turn_id,
        "shared_turn"
    );
    store
        .save_agent_runtime_snapshot("shared_session", "{\"sharedSnapshot\":true}", 4)
        .expect("shared snapshot");
    assert_eq!(
        store
            .load_agent_runtime_snapshot("shared_session")
            .expect("shared load snapshot")
            .as_deref(),
        Some("{\"sharedSnapshot\":true}")
    );
}

fn shared_waiter_index_contract<S: RuntimeStore + RuntimeStoreTransactionPort>(store: &S) {
    use centaeris_core::runtime::contracts::{
        CheckpointKindV1, RuntimeAgentRunIdentityV1, RuntimeAwaitJobCheckpointV1, RuntimeJobWaitV1,
    };
    let digest = format!("sha256:{}", "a".repeat(64));
    let wait = RuntimeAwaitJobCheckpointV1::new(
        &RuntimeAgentRunIdentityV1 {
            agent_run_id: "agent_waiter_contract".into(),
            execution_id: "execution_waiter_contract".into(),
            authorization_digest: digest.clone(),
        },
        "turn_waiter_contract",
        (0..257)
            .map(|index| RuntimeJobWaitV1 {
                tool_call_id: format!("call_{index:04}"),
                source_tool_name: "read_result".into(),
                tool_definition_digest: digest.clone(),
                job_id: if index == 256 {
                    "source:other"
                } else {
                    "source:target"
                }
                .into(),
                job_kind: "contract.test".into(),
            })
            .collect(),
    )
    .unwrap();
    let checkpoint = CheckpointRecord {
        checkpoint_id: "checkpoint:waiter_contract".into(),
        kind: CheckpointKindV1::Wait,
        session_id: "session_waiter_contract".into(),
        turn_id: wait.turn_id.clone(),
        status: "waiting".into(),
        done_reason: Some("runtime_job".into()),
        updated_at_ms: 1,
        payload_json: serde_json::to_string(&wait).unwrap(),
    };
    let event = RuntimeEvent {
        event_id: "event_waiter_contract".into(),
        session_id: checkpoint.session_id.clone(),
        task_id: Some(checkpoint.turn_id.clone()),
        event_type: "runtime_wait_changed.v1".into(),
        at_ms: 1,
        visibility: EventVisibility::Internal,
        payload_json: "{\"status\":\"waiting\"}".into(),
    };
    store.append_event(event.clone()).unwrap();
    let mut conflicting = event.clone();
    conflicting.payload_json = "{}".into();
    assert!(store
        .save_wait_checkpoint(SaveWaitCheckpointRequest {
            checkpoint: checkpoint.clone(),
            event: conflicting.clone()
        })
        .is_err());
    assert!(store
        .load_checkpoint_by_turn(&checkpoint.session_id, &checkpoint.turn_id)
        .unwrap()
        .is_none());
    assert!(store
        .list_runtime_job_waiters(Some("source:target"), None, 256)
        .unwrap()
        .is_empty());
    for _ in 0..2 {
        store
            .save_wait_checkpoint(SaveWaitCheckpointRequest {
                checkpoint: checkpoint.clone(),
                event: event.clone(),
            })
            .unwrap();
    }
    let first = store.list_runtime_job_waiters(None, None, 256).unwrap();
    assert_eq!(first.len(), 256);
    let second = store
        .list_runtime_job_waiters(None, Some(&first.last().unwrap().cursor), 256)
        .unwrap();
    assert_eq!(second.len(), 1);
    assert_eq!(
        second[0].cursor.checkpoint_id,
        first[0].cursor.checkpoint_id
    );
    assert_eq!(second[0].source_job_id, "source:other");
    assert_eq!(
        store
            .list_runtime_job_waiters(Some("source:other"), None, 256)
            .unwrap()
            .len(),
        1
    );
    assert!(store
        .list_runtime_job_waiters(Some("source:absent"), None, 256)
        .unwrap()
        .is_empty());
    assert!(store
        .consume_wait_checkpoint(ConsumeWaitCheckpointRequest {
            checkpoint: checkpoint.clone(),
            events: vec![conflicting]
        })
        .is_err());
    assert_eq!(
        store
            .list_runtime_job_waiters(Some("source:target"), None, 256)
            .unwrap()
            .len(),
        256
    );
    let mut consumed = event;
    consumed.event_id = "event_waiter_contract_consumed".into();
    consumed.payload_json = "{\"status\":\"resumed\"}".into();
    store
        .consume_wait_checkpoint(ConsumeWaitCheckpointRequest {
            checkpoint,
            events: vec![consumed],
        })
        .unwrap();
    assert!(store
        .list_runtime_job_waiters(None, None, 256)
        .unwrap()
        .is_empty());
}

#[test]
fn sqlite_runtime_store_passes_shared_contract() {
    let path = std::env::temp_dir().join(format!(
        "centaeris-shared-store-contract-{}.sqlite",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let store = SqliteRuntimeStore::new(&path).expect("open SQLite contract store");
    shared_runtime_store_contract(&store);
    drop(store);
    let _ = std::fs::remove_file(path);
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_runtime_job_wait_is_notified_and_closes_lost_wakeups() {
    let _guard = TEST_LOCK.lock().expect("Postgres test lock");
    let url = test_url();
    reset_store(&url);
    let store = PostgresRuntimeStore::new(&url).expect("open Postgres store");
    let mut listener = Client::connect(&url, NoTls).expect("connect notification listener");
    listener
        .batch_execute("SET search_path TO runtime,public; LISTEN runtime_job_ready_v1")
        .expect("listen for runtime jobs");
    let now_ms = listener
        .query_one(
            "SELECT (EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint",
            &[],
        )
        .expect("query database clock")
        .get::<_, i64>(0);

    let mut queued = job("worker.noop:wait", "worker.noop:wait");
    queued.job_kind = "worker.noop".to_string();
    queued.run_at_ms = now_ms;
    queued.created_at_ms = now_ms;
    queued.updated_at_ms = now_ms;
    store
        .schedule_runtime_job(ScheduleRuntimeJobRequest { job: queued })
        .expect("schedule queued worker job");
    let inserted = listener
        .notifications()
        .timeout_iter(Duration::from_secs(1))
        .next()
        .expect("read insert notification")
        .expect("insert must notify");
    assert_eq!(inserted.payload(), "");

    let kinds = vec!["worker.noop".to_string()];
    let after_consumed_notification = store
        .wait_for_runtime_jobs(kinds.as_slice(), Duration::from_millis(50))
        .expect("wait finds already-due job after notification was consumed");
    assert!(after_consumed_notification.ready);
    let claimed = store
        .claim_due_runtime_jobs(ClaimDueRuntimeJobsRequest {
            now_ms: now_ms + 1_000,
            worker_id: "worker:postgres-wait-test".to_string(),
            job_id: Some("worker.noop:wait".to_string()),
            job_kind: Some("worker.noop".to_string()),
            session_id: None,
            limit: 1,
            lease_ms: 60_000,
        })
        .expect("claim due job before transition test");
    assert_eq!(claimed.len(), 1);
    listener
        .execute(
            "UPDATE runtime_jobs SET status='queued',run_at_ms=$1,lease_owner=NULL,lease_expires_at_ms=NULL WHERE job_id=$2",
            &[&(now_ms + 60_000), &"worker.noop:wait"],
        )
        .expect("transition job back to queued");
    let transitioned = listener
        .notifications()
        .timeout_iter(Duration::from_secs(1))
        .next()
        .expect("read transition notification")
        .expect("transition to queued must notify");
    assert_eq!(transitioned.payload(), "");

    let future_now_ms = listener
        .query_one(
            "SELECT (EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint",
            &[],
        )
        .expect("query future-job clock")
        .get::<_, i64>(0);
    let mut future = job("worker.noop:future", "worker.noop:future");
    future.job_kind = "worker.noop".to_string();
    future.run_at_ms = future_now_ms + 100;
    future.created_at_ms = future_now_ms;
    future.updated_at_ms = future_now_ms;
    store
        .schedule_runtime_job(ScheduleRuntimeJobRequest { job: future })
        .expect("schedule future worker job");
    let future_kinds = vec!["worker.noop".to_string()];
    assert!(
        store
            .wait_for_runtime_jobs(future_kinds.as_slice(), Duration::from_secs(1))
            .expect("future job timer")
            .ready
    );

    let sender_url = url.clone();
    let sender = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(10));
        Client::connect(&sender_url, NoTls)
            .expect("connect irrelevant notifier")
            .batch_execute("NOTIFY runtime_job_ready_v1, 'provider.poll'")
            .expect("send irrelevant notification");
    });
    let idle_kinds = vec!["agent_run.lifecycle".to_string()];
    let idle = store
        .wait_for_runtime_jobs(idle_kinds.as_slice(), Duration::from_millis(50))
        .expect("irrelevant notification remains a hint");
    sender.join().expect("join irrelevant notifier");
    assert!(!idle.ready);
    assert_eq!(idle.next_run_at_ms, None);
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_runtime_store_clones_reuse_one_short_lived_connection() {
    let _guard = TEST_LOCK.lock().expect("Postgres test lock");
    let url = test_url();
    reset_store(&url);
    let store = PostgresRuntimeStore::new(&url).expect("open Postgres store");
    let first_backend: i32 = store
        .with_client(|client| {
            client
                .query_one("SELECT pg_backend_pid()", &[])
                .map(|row| row.get(0))
                .map_err(|error| error.to_string())
        })
        .expect("first pooled operation");
    let second_backend: i32 = store
        .clone()
        .with_client(|client| {
            client
                .query_one("SELECT pg_backend_pid()", &[])
                .map(|row| row.get(0))
                .map_err(|error| error.to_string())
        })
        .expect("second pooled operation through clone");
    assert_eq!(
        first_backend, second_backend,
        "sequential short operations through Store clones must reuse the shared pool"
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_runtime_store_pool_can_drop_inside_tokio_runtime() {
    let _guard = TEST_LOCK.lock().expect("Postgres test lock");
    let url = test_url();
    reset_store(&url);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build Tokio runtime");
    let dropped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        runtime.block_on(async {
            let store = PostgresRuntimeStore::new(&url).expect("open pooled Postgres store");
            store
                .with_client(|client| {
                    client
                        .simple_query("SELECT 1")
                        .map_err(|error| error.to_string())?;
                    Ok(())
                })
                .expect("populate idle pool");
            drop(store);
        });
    }));
    assert!(
        dropped.is_ok(),
        "pooled clients must be safe to drop inside Tokio"
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn recovery_orchestration_releases_capacity_before_followup_store_work() {
    let _guard = TEST_LOCK.lock().expect("Postgres test lock");
    let url = test_url();
    reset_store(&url);
    let store = PostgresRuntimeStore::new_with_pool_limits(&url, 1, Duration::from_millis(50))
        .expect("open capacity-one Postgres store");
    let mut setup = Client::connect(&url, NoTls).expect("connect recovery setup");
    setup
        .batch_execute(
            "CREATE TABLE runtime.app_core_sessionevent(\
             sequence integer NOT NULL, session_id varchar(64) NOT NULL, payload jsonb NOT NULL)",
        )
        .expect("create recovery event table");
    let event = session_record(
        "agent_run_recovery_pool",
        "session_recovery_pool",
        1,
        SessionRecordType::AgentRunStarted,
        serde_json::json!({"userObjective":"recover"}),
        1,
    );
    let wire = centaeris_core::session::wire_record_value(&event).expect("encode recovery wire");
    setup
        .execute(
            "INSERT INTO runtime.app_core_sessionevent(sequence,session_id,payload) VALUES(1,$1,$2::text::jsonb)",
            &[&"session_recovery_pool", &wire.to_string()],
        )
        .expect("insert recovery event");
    drop(setup);
    crate::with_recovery_session_events(&store, "session_recovery_pool", 1, |events| {
        assert_eq!(events.len(), 1);
        store.with_client(|client| {
            client
                .simple_query("SELECT 1")
                .map_err(|error| error.to_string())?;
            Ok(())
        })
    })
    .expect("real recovery follow-up gets the released capacity-one lease");
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_runtime_store_pool_checkout_is_bounded() {
    use std::sync::{Arc, Barrier};

    let _guard = TEST_LOCK.lock().expect("Postgres test lock");
    let url = test_url();
    reset_store(&url);
    let store = PostgresRuntimeStore::new_with_pool_limits(&url, 1, Duration::from_millis(50))
        .expect("open bounded Postgres store");
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let holder_store = store.clone();
    let holder_entered = entered.clone();
    let holder_release = release.clone();
    let holder = std::thread::spawn(move || {
        holder_store.with_client(|client| {
            client
                .simple_query("SELECT 1")
                .map_err(|error| error.to_string())?;
            holder_entered.wait();
            holder_release.wait();
            Ok(())
        })
    });
    entered.wait();
    let exhausted = store.with_client(|_| Ok(()));
    release.wait();
    holder
        .join()
        .expect("join pool holder")
        .expect("pool holder");
    assert_eq!(
        exhausted.unwrap_err(),
        "Postgres connection pool checkout timed out"
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_runtime_store_discards_failed_and_panicked_leases_without_replay() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    let _guard = TEST_LOCK.lock().expect("Postgres test lock");
    let url = test_url();
    reset_store(&url);
    let store = PostgresRuntimeStore::new_with_pool_limits(&url, 1, Duration::from_millis(50))
        .expect("open bounded Postgres store");
    let calls = Arc::new(AtomicUsize::new(0));
    let observed_calls = calls.clone();
    assert_eq!(
        store
            .with_client(move |client| {
                observed_calls.fetch_add(1, Ordering::SeqCst);
                client
                    .simple_query("SELECT 1")
                    .map_err(|error| error.to_string())?;
                Err::<(), _>("operation result is unknown".to_string())
            })
            .unwrap_err(),
        "operation result is unknown"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "failed SQL operation must not be replayed"
    );

    let panic_store = store.clone();
    let panicked = std::thread::spawn(move || {
        let _ = panic_store.with_client::<()>(|_| panic!("test operation panic"));
    })
    .join();
    assert!(panicked.is_err());
    store
        .with_client(|client| {
            client
                .simple_query("SELECT 1")
                .map_err(|error| error.to_string())?;
            Ok(())
        })
        .expect("panic must release the only connection slot");
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_runtime_store_replaces_closed_idle_connection_before_operation() {
    let _guard = TEST_LOCK.lock().expect("Postgres test lock");
    let url = test_url();
    reset_store(&url);
    let store = PostgresRuntimeStore::new_with_pool_limits(&url, 1, Duration::from_millis(50))
        .expect("open bounded Postgres store");
    let first_backend: i32 = store
        .with_client(|client| {
            client
                .query_one("SELECT pg_backend_pid()", &[])
                .map(|row| row.get(0))
                .map_err(|error| error.to_string())
        })
        .expect("load pooled backend identity");
    let mut administrator = Client::connect(&url, NoTls).expect("connect test administrator");
    assert!(administrator
        .query_one("SELECT pg_terminate_backend($1)", &[&first_backend])
        .expect("terminate pooled backend")
        .get::<_, bool>(0));
    std::thread::sleep(Duration::from_millis(20));
    let replacement_backend: i32 = store
        .with_client(|client| {
            client
                .query_one("SELECT pg_backend_pid()", &[])
                .map(|row| row.get(0))
                .map_err(|error| error.to_string())
        })
        .expect("closed idle connection is replaced before operation");
    assert_ne!(first_backend, replacement_backend);
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_control_and_listener_connections_do_not_consume_the_ordinary_pool() {
    use std::sync::{Arc, Barrier};

    let _guard = TEST_LOCK.lock().expect("Postgres test lock");
    let url = test_url();
    reset_store(&url);
    let store = PostgresRuntimeStore::new_with_pool_limits(&url, 1, Duration::from_millis(50))
        .expect("open bounded Postgres store");
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let holder_store = store.clone();
    let holder_entered = entered.clone();
    let holder_release = release.clone();
    let holder = std::thread::spawn(move || {
        holder_store.with_client(|_| {
            holder_entered.wait();
            holder_release.wait();
            Ok(())
        })
    });
    entered.wait();
    let control = store.with_execution_control_client(|client| {
        client
            .simple_query("SELECT 1")
            .map_err(|error| error.to_string())?;
        Ok(())
    });
    let listener =
        store.wait_for_runtime_jobs(&["worker.noop".to_string()], Duration::from_millis(10));
    let listener_is_clean = store.with_listener_client(|client| {
        client
            .query_one("SELECT count(*) FROM pg_listening_channels()", &[])
            .map(|row| row.get::<_, i64>(0) == 0)
            .map_err(|error| error.to_string())
    });
    release.wait();
    holder
        .join()
        .expect("join pool holder")
        .expect("pool holder");
    control.expect("control capacity remains available");
    assert!(!listener.expect("listener capacity remains available").ready);
    assert!(listener_is_clean.expect("inspect returned listener connection"));
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_runtime_store_persists_core_state_and_claims_jobs_once() {
    let _guard = TEST_LOCK.lock().expect("Postgres test lock");
    let url = test_url();
    reset_store(&url);
    let store = PostgresRuntimeStore::new(&url).expect("open Postgres store");
    shared_runtime_store_contract(&store);
    store
        .save_checkpoint(CheckpointRecord {
            checkpoint_id: "checkpoint:pg".to_string(),
            kind: centaeris_core::runtime::contracts::CheckpointKindV1::Wait,
            session_id: "session_pg".to_string(),
            turn_id: "turn_pg".to_string(),
            status: "running".to_string(),
            done_reason: None,
            updated_at_ms: 3,
            payload_json: "{\"ok\":true}".to_string(),
        })
        .expect("save checkpoint");
    assert_eq!(
        store
            .load_latest_checkpoint("session_pg")
            .expect("checkpoint")
            .expect("row")
            .turn_id,
        "turn_pg"
    );
    store
        .save_agent_runtime_snapshot("session_pg", "{\"snapshot\":true}", 4)
        .expect("snapshot");
    assert_eq!(
        store
            .load_agent_runtime_snapshot("session_pg")
            .expect("load snapshot")
            .as_deref(),
        Some("{\"snapshot\":true}")
    );
    let wait_checkpoint = CheckpointRecord {
        checkpoint_id: "checkpoint:wait-pg".to_string(),
        kind: centaeris_core::runtime::contracts::CheckpointKindV1::Wait,
        session_id: "session_pg".to_string(),
        turn_id: "turn_wait_pg".to_string(),
        status: "waiting".to_string(),
        done_reason: Some("question".to_string()),
        updated_at_ms: 5,
        payload_json: "{\"schema\":\"runtime.await_question.v1\"}".to_string(),
    };
    store
        .save_wait_checkpoint(SaveWaitCheckpointRequest {
            checkpoint: wait_checkpoint.clone(),
            event: RuntimeEvent {
                event_id: "runtime_wait_pg_waiting".to_string(),
                session_id: "session_pg".to_string(),
                task_id: Some("turn_wait_pg".to_string()),
                event_type: "runtime_wait_changed.v1".to_string(),
                at_ms: 5,
                visibility: EventVisibility::Internal,
                payload_json: "{\"status\":\"waiting\"}".to_string(),
            },
        })
        .expect("atomic wait transition");
    store
        .consume_wait_checkpoint(ConsumeWaitCheckpointRequest {
            checkpoint: wait_checkpoint,
            events: vec![RuntimeEvent {
                event_id: "runtime_wait_pg_resumed".to_string(),
                session_id: "session_pg".to_string(),
                task_id: Some("turn_wait_pg".to_string()),
                event_type: "runtime_wait_changed.v1".to_string(),
                at_ms: 6,
                visibility: EventVisibility::Internal,
                payload_json: "{\"status\":\"resumed\"}".to_string(),
            }],
        })
        .expect("atomic wait consumption");
    assert!(store
        .load_checkpoint_by_turn("session_pg", "turn_wait_pg")
        .expect("wait checkpoint removed")
        .is_none());
    assert_eq!(
        store
            .list_events("session_pg", 10, 0)
            .expect("runtime events")
            .len(),
        2
    );
    store
        .schedule_runtime_job(ScheduleRuntimeJobRequest {
            job: job("job_pg", "key_pg"),
        })
        .expect("schedule");
    store
        .schedule_runtime_job(ScheduleRuntimeJobRequest {
            job: job("job_pg_duplicate", "key_pg"),
        })
        .expect("idempotent schedule");
    let pending = store
        .list_pending_runtime_job_outbox(10)
        .expect("pending outbox");
    assert!(pending.is_empty());
    let request = ClaimDueRuntimeJobsRequest {
        now_ms: 10,
        worker_id: "worker_a".to_string(),
        job_id: Some("job_pg".to_string()),
        job_kind: None,
        session_id: None,
        limit: 1,
        lease_ms: 1000,
    };
    let old_owner = store
        .claim_due_runtime_jobs(request.clone())
        .expect("first claim")
        .remove(0)
        .lease_owner
        .unwrap();
    assert!(store
        .claim_due_runtime_jobs(ClaimDueRuntimeJobsRequest {
            worker_id: "worker_b".to_string(),
            ..request
        })
        .expect("second claim")
        .is_empty());
    store
        .start_runtime_job(StartRuntimeJobRequest {
            job_id: "job_pg".to_string(),
            lease_owner: old_owner.clone(),
            started_at_ms: 11,
        })
        .expect("start job");
    store
        .renew_runtime_job_lease(RenewRuntimeJobLeaseRequest {
            job_id: "job_pg".to_string(),
            lease_owner: old_owner.clone(),
            heartbeat_at_ms: 20,
            lease_ms: 100,
        })
        .expect("heartbeat job");
    assert_eq!(
        store
            .reclaim_expired_runtime_job_leases(119)
            .expect("live lease"),
        0
    );
    assert!(store
        .complete_runtime_job(CompleteRuntimeJobRequest {
            job_id: "job_pg".to_string(),
            lease_owner: old_owner.clone(),
            output_refs: vec![],
            completed_at_ms: 120,
        })
        .is_err());
    assert_eq!(
        store
            .reclaim_expired_runtime_job_leases(120)
            .expect("expired lease"),
        1
    );
    let current_owner = store
        .claim_due_runtime_jobs(ClaimDueRuntimeJobsRequest {
            now_ms: 120,
            worker_id: "worker_b".to_string(),
            job_id: Some("job_pg".to_string()),
            job_kind: None,
            session_id: None,
            limit: 1,
            lease_ms: 100,
        })
        .expect("reclaim job")
        .remove(0)
        .lease_owner
        .unwrap();
    assert_ne!(old_owner, current_owner);
    assert!(store
        .complete_runtime_job(CompleteRuntimeJobRequest {
            job_id: "job_pg".to_string(),
            lease_owner: old_owner,
            output_refs: vec![],
            completed_at_ms: 121,
        })
        .is_err());
    store
        .complete_runtime_job(CompleteRuntimeJobRequest {
            job_id: "job_pg".to_string(),
            lease_owner: current_owner,
            output_refs: vec![],
            completed_at_ms: 121,
        })
        .expect("current owner completes job");
    let mut audit = Client::connect(&url, NoTls).expect("connect");
    let outbox = audit
        .query_one(
            "SELECT event_type,generation FROM runtime.runtime_job_outbox WHERE job_id='job_pg'",
            &[],
        )
        .expect("terminal outbox");
    let outbox_event_type: String = outbox.get(0);
    let outbox_generation: i64 = outbox.get(1);
    assert_eq!(outbox_event_type, "runtime_job.terminal");
    assert_eq!(outbox_generation, 0);
    let tables = audit
        .query(
            "SELECT table_name FROM information_schema.tables WHERE table_schema='runtime'",
            &[],
        )
        .expect("tables");
    let tables = tables
        .into_iter()
        .map(|row| row.get::<_, String>(0))
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        tables,
        super::schema::RUNTIME_TABLES
            .iter()
            .copied()
            .map(str::to_string)
            .collect()
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_runtime_job_yield_requeues_same_job_without_queued_outbox_and_fences_old_owner() {
    let _guard = TEST_LOCK.lock().expect("Postgres test lock");
    let url = test_url();
    reset_store(&url);
    let store = PostgresRuntimeStore::new(&url).expect("open Postgres store");
    store
        .schedule_runtime_job(ScheduleRuntimeJobRequest {
            job: job("job_pg_yield", "key_pg_yield"),
        })
        .expect("schedule job");
    let yield_owner = store
        .claim_due_runtime_jobs(ClaimDueRuntimeJobsRequest {
            now_ms: 10,
            worker_id: "worker_pg_yield_owner".to_string(),
            job_id: Some("job_pg_yield".to_string()),
            job_kind: None,
            session_id: None,
            limit: 1,
            lease_ms: 1_000,
        })
        .expect("claim job")
        .remove(0)
        .lease_owner
        .unwrap();
    store
        .start_runtime_job(StartRuntimeJobRequest {
            job_id: "job_pg_yield".to_string(),
            lease_owner: yield_owner.clone(),
            started_at_ms: 11,
        })
        .expect("start job");
    let request = YieldRuntimeJobRequest {
        job_id: "job_pg_yield".to_string(),
        lease_owner: yield_owner,
        yielded_at_ms: 20,
        run_at_ms: 40,
        transition_reason: "waiting_for_durable_input".to_string(),
    };
    store.yield_runtime_job(request.clone()).expect("yield job");
    let cross_lease_error = store
        .yield_runtime_job(YieldRuntimeJobRequest {
            lease_owner: "worker_pg_other_owner".to_string(),
            ..request.clone()
        })
        .expect_err("same millisecond from another lease is not idempotent");
    assert!(cross_lease_error.contains("idempotency conflict"));
    store
        .yield_runtime_job(request)
        .expect("duplicate yield is idempotent");

    let yielded = store
        .get_runtime_job("job_pg_yield")
        .expect("load yielded job")
        .expect("yielded job exists");
    assert_eq!(yielded.status, RuntimeJobStatus::Queued);
    assert_eq!(yielded.run_at_ms, 40);
    assert_eq!(yielded.retry_count, 0);
    let pending = store
        .list_pending_runtime_job_outbox(10)
        .expect("load terminal-only outbox");
    assert!(pending.is_empty());

    let old_owner_error = store
        .yield_runtime_job(YieldRuntimeJobRequest {
            job_id: "job_pg_yield".to_string(),
            lease_owner: "worker_pg_old_owner".to_string(),
            yielded_at_ms: 21,
            run_at_ms: 41,
            transition_reason: "waiting_for_durable_input".to_string(),
        })
        .expect_err("old owner must be fenced");
    assert!(old_owner_error.contains("lease mismatch or expired"));
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_turn_supplement_queue_is_lease_fenced_and_cancel_closes_admission() {
    let _guard = TEST_LOCK.lock().expect("Postgres test lock");
    let url = test_url();
    reset_store(&url);
    let store = PostgresRuntimeStore::new(&url).expect("open Postgres store");
    let agent_run_id = "agent_run_pg_supplement";
    let session_id = "session_pg_supplement";
    let digest = "digest_pg_supplement";
    let job_id = format!("agent_run.lifecycle:{agent_run_id}");
    let worker_id = "worker_pg_supplement";
    let mut lifecycle_job = job(job_id.as_str(), "unused");
    lifecycle_job.job_kind =
        centaeris_core::session::reliability::AGENT_RUN_LIFECYCLE_JOB_KIND.to_string();
    lifecycle_job.idempotency_key = format!("agent_run.lifecycle:{agent_run_id}:{digest}");
    lifecycle_job.session_id = Some(session_id.to_string());
    lifecycle_job.payload_ref = Some(format!("record:agent_run:{agent_run_id}"));
    store
        .schedule_runtime_job(ScheduleRuntimeJobRequest { job: lifecycle_job })
        .expect("schedule lifecycle job");
    let owner = store
        .claim_due_runtime_jobs(ClaimDueRuntimeJobsRequest {
            now_ms: 10,
            worker_id: worker_id.to_string(),
            job_id: Some(job_id.clone()),
            job_kind: None,
            session_id: None,
            limit: 1,
            lease_ms: 1_000,
        })
        .expect("claim lifecycle job")
        .remove(0)
        .lease_owner
        .unwrap();
    store
        .start_runtime_job(StartRuntimeJobRequest {
            job_id: job_id.clone(),
            lease_owner: owner.to_string(),
            started_at_ms: 11,
        })
        .expect("start lifecycle job");
    let enqueue = |supplement_id: &str, message: &str, at_ms: i64| {
        store.enqueue_turn_supplement(EnqueueTurnSupplementRequest {
            agent_run_id: agent_run_id.to_string(),
            lifecycle_job_id: job_id.clone(),
            session_id: session_id.to_string(),
            authorization_digest: digest.to_string(),
            supplement_id: supplement_id.to_string(),
            message: message.to_string(),
            created_at_ms: at_ms,
        })
    };
    assert_eq!(
        enqueue("supplement-pg-1", "first", 12)
            .expect("enqueue first supplement")
            .disposition,
        EnqueueTurnSupplementDisposition::Accepted
    );
    let claimed = store
        .claim_turn_supplements(ClaimTurnSupplementsRequest {
            agent_run_id: agent_run_id.to_string(),
            lifecycle_job_id: job_id.clone(),
            session_id: session_id.to_string(),
            authorization_digest: digest.to_string(),
            lease_owner: owner.to_string(),
            claim_token: "claim-pg-1".to_string(),
            now_ms: 13,
            close_if_empty: false,
            limit: 8,
        })
        .expect("claim first supplement");
    assert_eq!(claimed.len(), 1);
    store
        .acknowledge_turn_supplements(AcknowledgeTurnSupplementsRequest {
            agent_run_id: agent_run_id.to_string(),
            lifecycle_job_id: job_id.clone(),
            session_id: session_id.to_string(),
            authorization_digest: digest.to_string(),
            lease_owner: owner.to_string(),
            claim_token: "claim-pg-1".to_string(),
            supplement_ids: vec!["supplement-pg-1".to_string()],
            acknowledged_at_ms: 14,
        })
        .expect("ack first supplement");
    enqueue("supplement-pg-2", "second", 15).expect("enqueue before cancel");
    store
        .request_agent_run_cancellation(agent_run_id, session_id, digest, 16)
        .expect("cancel run");
    assert_eq!(
        enqueue("supplement-pg-3", "banana", 17).expect_err("cancel closes supplement admission"),
        TurnSupplementStoreError::AdmissionClosed
    );
    store
        .close_turn_supplement_queue(CloseTurnSupplementQueueRequest {
            agent_run_id: agent_run_id.to_string(),
            lifecycle_job_id: job_id,
            session_id: session_id.to_string(),
            authorization_digest: digest.to_string(),
            lease_owner: Some(owner.to_string()),
            reason: "agent_run_terminal".to_string(),
            closed_at_ms: 18,
        })
        .expect("terminal close remains idempotent after cancel");
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_session_terminal_append_fences_reclaimed_lease_owner() {
    let _guard = TEST_LOCK.lock().expect("Postgres test lock");
    for terminal_type in [
        SessionRecordType::AgentRunCompleted,
        SessionRecordType::AgentRunFailed,
        SessionRecordType::AgentRunInterrupted,
    ] {
        terminal_append_consumes_waiter_contract(terminal_type);
    }
}

fn terminal_append_consumes_waiter_contract(terminal_type: SessionRecordType) {
    let url = test_url();
    reset_store(&url);
    let mut setup = Client::connect(&url, NoTls).expect("connect test Postgres");
    setup
        .batch_execute(
            r#"
            DROP TABLE IF EXISTS public.app_core_sessionevent;
            DROP TABLE IF EXISTS public.app_core_session CASCADE;
            CREATE TABLE public.app_core_session(
                id varchar(64) PRIMARY KEY,
                workspace_id varchar(64) NOT NULL
            );
            CREATE TABLE public.app_core_sessionevent(
                "eventId" varchar(160) PRIMARY KEY,
                workspace_id varchar(64) NOT NULL,
                session_id varchar(64) NOT NULL,
                agent_run_id varchar(64) NOT NULL,
                sequence integer NOT NULL,
                agent_run_sequence integer,
                session_level boolean NOT NULL DEFAULT false,
                projects_to_agent_run_stream boolean NOT NULL,
                payload jsonb NOT NULL,
                "createdAtMs" bigint NOT NULL,
                "insertedAt" timestamptz NOT NULL DEFAULT clock_timestamp(),
                UNIQUE(session_id, sequence),
                UNIQUE(agent_run_id, agent_run_sequence)
            );
            INSERT INTO public.app_core_session(id, workspace_id)
            VALUES('session_fenced_terminal', 'workspace_fenced_terminal');
            "#,
        )
        .expect("create session event table");
    drop(setup);
    let store = PostgresRuntimeStore::new(&url).expect("open Postgres store");
    let now_ms = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time")
            .as_millis(),
    )
    .expect("timestamp");
    let agent_run_id = "agent_run_fenced_terminal";
    let session_id = "session_fenced_terminal";
    let mut authorization: crate::agent_run_authorization::WorkspaceAgentRunAuthorization =
        serde_json::from_str(include_str!(
            "../../../../tests/workspace/fixtures/agent_run_authorization/v1/valid.json"
        ))
        .unwrap();
    authorization.agent_run_id = agent_run_id.into();
    authorization.session_id = session_id.into();
    authorization.workspace_id = "workspace_fenced_terminal".into();
    let start = crate::contract::AgentRunStart {
        schema: crate::contract::AGENT_RUN_START_SCHEMA.into(),
        agent_run_id: agent_run_id.into(),
        turn_id: "turn_pg_fenced_terminal".into(),
        prompt: "fence terminal".into(),
        initial_input: crate::contract::AgentRunStartInitialInput::UserMessage {},
        agent_instructions: "Read committed work history.".into(),
        model_context_tokens: 200_000,
        model_max_output_tokens: 32_768,
        authorization_digest: authorization.digest().unwrap(),
        authorization_signature: authorization
            .signature(b"test-run-authorization-signing-key")
            .unwrap(),
        authorization,
        tail_action: crate::contract::AgentRunTailAction::Append,
        coordination_session_id: None,
        native_coordination_session_id: None,
    };
    let mut lifecycle_job = job(
        "agent_run.lifecycle:agent_run_fenced_terminal",
        "fenced-terminal",
    );
    lifecycle_job.job_kind = "agent_run.lifecycle".to_string();
    lifecycle_job.session_id = Some(session_id.to_string());
    lifecycle_job.run_at_ms = now_ms;
    lifecycle_job.created_at_ms = now_ms;
    lifecycle_job.updated_at_ms = now_ms;
    store
        .schedule_runtime_job(ScheduleRuntimeJobRequest { job: lifecycle_job })
        .expect("schedule lifecycle job");
    let old_owner = store
        .claim_due_runtime_jobs(ClaimDueRuntimeJobsRequest {
            now_ms,
            worker_id: "old_owner".to_string(),
            job_id: Some("agent_run.lifecycle:agent_run_fenced_terminal".to_string()),
            job_kind: None,
            session_id: None,
            limit: 1,
            lease_ms: 10,
        })
        .expect("claim old lease")
        .remove(0)
        .lease_owner
        .unwrap();
    store
        .start_runtime_job(StartRuntimeJobRequest {
            job_id: "agent_run.lifecycle:agent_run_fenced_terminal".to_string(),
            lease_owner: old_owner.clone(),
            started_at_ms: now_ms,
        })
        .expect("start old lease");
    let pooled_sessions_before_log = postgres_peer_session_count(&url);
    let session_log = store.session_log(
        "workspace_fenced_terminal".to_string(),
        session_id.to_string(),
        "fence terminal".to_string(),
    );
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    runtime
        .block_on(session_log.append_session_records(
            agent_run_id,
            &[
                session_record(
                    agent_run_id,
                    session_id,
                    1,
                    SessionRecordType::AgentRunStarted,
                    serde_json::json!({"userObjective": "fence terminal"}),
                    now_ms,
                ),
                session_record(
                    agent_run_id,
                    session_id,
                    2,
                    SessionRecordType::UserMessage,
                    serde_json::json!({
                        "messageId": "message:turn_pg_fenced_terminal:user",
                        "text": "fence terminal",
                        "attachments": []
                    }),
                    now_ms,
                ),
            ],
        ))
        .expect("append started records");
    assert_eq!(
        postgres_peer_session_count(&url),
        pooled_sessions_before_log,
        "SessionLog append must borrow briefly from the Store pool instead of retaining a per-run connection"
    );
    store
        .reclaim_expired_runtime_job_leases(now_ms + 11)
        .expect("reclaim old lease");
    let new_owner = store
        .claim_due_runtime_jobs(ClaimDueRuntimeJobsRequest {
            now_ms: now_ms + 11,
            worker_id: "new_owner".to_string(),
            job_id: Some("agent_run.lifecycle:agent_run_fenced_terminal".to_string()),
            job_kind: None,
            session_id: None,
            limit: 1,
            lease_ms: 60_000,
        })
        .expect("claim replacement lease")
        .remove(0)
        .lease_owner
        .unwrap();
    store
        .start_runtime_job(StartRuntimeJobRequest {
            job_id: "agent_run.lifecycle:agent_run_fenced_terminal".to_string(),
            lease_owner: new_owner.clone(),
            started_at_ms: now_ms + 12,
        })
        .expect("start replacement lease");
    let final_assistant = session_record(
        agent_run_id,
        session_id,
        3,
        SessionRecordType::AssistantMessage,
        serde_json::json!({
            "messageId": "message:turn_pg_fenced_terminal:assistant",
            "modelMarkdown": "done",
            "artifactRefs": [],
            "status": if terminal_type == SessionRecordType::AgentRunCompleted { "done" } else { "error" }
        }),
        now_ms + 13,
    );
    let terminal = [
        final_assistant,
        session_record(
            agent_run_id,
            session_id,
            4,
            terminal_type,
            match terminal_type {
                SessionRecordType::AgentRunCompleted => {
                    serde_json::json!({"doneReason": "finalized"})
                }
                SessionRecordType::AgentRunFailed => {
                    serde_json::json!({"reasonType": "runtime_internal_error", "message": "sandbox unavailable"})
                }
                SessionRecordType::AgentRunInterrupted => {
                    serde_json::json!({"reasonType": "cancelled", "message": "cancelled", "retryable": false})
                }
                _ => unreachable!(),
            },
            now_ms + 13,
        ),
    ];
    use centaeris_core::runtime::contracts::{
        CheckpointKindV1, RuntimeAgentRunIdentityV1, RuntimeAwaitJobCheckpointV1, RuntimeJobWaitV1,
    };
    let digest = format!("sha256:{}", "a".repeat(64));
    let wait = RuntimeAwaitJobCheckpointV1::new(
        &RuntimeAgentRunIdentityV1 {
            agent_run_id: agent_run_id.into(),
            execution_id: "execution_terminal_wait".into(),
            authorization_digest: digest.clone(),
        },
        "turn_terminal_wait",
        vec![RuntimeJobWaitV1 {
            tool_call_id: "call_terminal_wait".into(),
            source_tool_name: "read_result".into(),
            tool_definition_digest: digest,
            job_id: "source:terminal_wait".into(),
            job_kind: "subagent.run".into(),
        }],
    )
    .unwrap();
    let checkpoint = CheckpointRecord {
        checkpoint_id: "checkpoint:terminal_wait".into(),
        kind: CheckpointKindV1::Wait,
        session_id: session_id.into(),
        turn_id: wait.turn_id.clone(),
        status: "waiting".into(),
        done_reason: Some("runtime_job".into()),
        updated_at_ms: now_ms,
        payload_json: serde_json::to_string(&wait).unwrap(),
    };
    store
        .save_wait_checkpoint(SaveWaitCheckpointRequest {
            checkpoint: checkpoint.clone(),
            event: RuntimeEvent {
                event_id: "terminal_wait:waiting".into(),
                session_id: session_id.into(),
                task_id: Some(wait.turn_id.clone()),
                event_type: "runtime_wait_changed.v1".into(),
                at_ms: now_ms,
                visibility: EventVisibility::Internal,
                payload_json: "{}".into(),
            },
        })
        .unwrap();
    assert!(
        store
            .repair_terminal_runtime_job_waits(session_id, agent_run_id)
            .is_err(),
        "a live owner cannot be repaired as terminal"
    );
    let old_error = runtime
        .block_on(session_log.append_session_records_with_runtime_job_lease(
            agent_run_id,
            &terminal,
            &RuntimeJobLeaseFence {
                job_id: "agent_run.lifecycle:agent_run_fenced_terminal".to_string(),
                job_kind: "agent_run.lifecycle".to_string(),
                lease_owner: old_owner.clone(),
            },
        ))
        .expect_err("reclaimed owner must not commit terminal records");
    assert_eq!(old_error, RUNTIME_JOB_LEASE_FENCE_REJECTED);

    assert_eq!(
        store
            .list_runtime_job_waiters(Some("source:terminal_wait"), None, 10)
            .unwrap()
            .len(),
        1,
        "a fenced owner must not consume the wait"
    );
    // A conflicting consumption event must roll back the terminal append too.
    let abandoned_id = format!("runtime_wait:{}:abandoned", wait.continuation_id);
    store
        .append_event(RuntimeEvent {
            event_id: abandoned_id.clone(),
            session_id: session_id.into(),
            task_id: Some(wait.turn_id.clone()),
            event_type: "runtime_wait_changed.v1".into(),
            at_ms: now_ms,
            visibility: EventVisibility::Internal,
            payload_json: "{}".into(),
        })
        .unwrap();
    runtime
        .block_on(session_log.append_session_records_with_runtime_job_lease(
            agent_run_id,
            &terminal,
            &RuntimeJobLeaseFence {
                job_id: "agent_run.lifecycle:agent_run_fenced_terminal".into(),
                job_kind: "agent_run.lifecycle".into(),
                lease_owner: new_owner.clone(),
            },
        ))
        .expect_err("consumption conflict must reject the whole terminal transaction");
    let mut observer = Client::connect(&url, NoTls).unwrap();
    assert_eq!(
        observer
            .query_one(
                "SELECT count(*) FROM public.app_core_sessionevent WHERE agent_run_id=$1",
                &[&agent_run_id]
            )
            .unwrap()
            .get::<_, i64>(0),
        2
    );
    assert_eq!(
        store
            .list_runtime_job_waiters(Some("source:terminal_wait"), None, 10)
            .unwrap()
            .len(),
        1
    );
    observer
        .execute(
            "DELETE FROM runtime.runtime_events WHERE event_id=$1",
            &[&abandoned_id],
        )
        .unwrap();
    drop(observer);

    runtime
        .block_on(session_log.append_session_records_with_runtime_job_lease(
            agent_run_id,
            &terminal,
            &RuntimeJobLeaseFence {
                job_id: "agent_run.lifecycle:agent_run_fenced_terminal".to_string(),
                job_kind: "agent_run.lifecycle".to_string(),
                lease_owner: new_owner.clone(),
            },
        ))
        .expect("current owner commits terminal records");
    let mut audit = Client::connect(&url, NoTls).expect("connect audit");
    let count: i64 = audit
        .query_one(
            "SELECT COUNT(*) FROM public.app_core_sessionevent WHERE agent_run_id=$1",
            &[&agent_run_id],
        )
        .expect("count session records")
        .get(0);
    assert_eq!(count, 4);
    let expected_state = match terminal_type {
        SessionRecordType::AgentRunCompleted => "completed",
        SessionRecordType::AgentRunFailed => "failed",
        SessionRecordType::AgentRunInterrupted => "cancelled",
        _ => unreachable!(),
    };
    assert_eq!(
        store
            .get_runtime_job("agent_run.lifecycle:agent_run_fenced_terminal")
            .unwrap()
            .unwrap()
            .status,
        RuntimeJobStatus::Running,
        "Session terminal commits before lifecycle job completion"
    );
    assert!(store
        .list_pending_runtime_job_outbox(10)
        .unwrap()
        .is_empty());
    assert_eq!(
        crate::load_existing_terminal_state(&store, &start).unwrap(),
        Some(expected_state),
        "committed terminal history is authoritative before any terminal outbox"
    );
    store
        .complete_runtime_job(CompleteRuntimeJobRequest {
            job_id: "agent_run.lifecycle:agent_run_fenced_terminal".into(),
            lease_owner: new_owner,
            output_refs: vec![],
            completed_at_ms: now_ms + 14,
        })
        .unwrap();
    let pending = store.list_pending_runtime_job_outbox(10).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(
        pending[0].job_id,
        "agent_run.lifecycle:agent_run_fenced_terminal"
    );
    assert_eq!(pending[0].event_type, "runtime_job.terminal");
    store
        .mark_runtime_job_outbox_published(
            &pending[0].job_id,
            &pending[0].event_type,
            pending[0].generation,
            now_ms + 15,
        )
        .unwrap();
    assert!(store
        .list_pending_runtime_job_outbox(10)
        .unwrap()
        .is_empty());
    let reopened = PostgresRuntimeStore::new(&url).unwrap();
    assert_eq!(
        crate::load_existing_terminal_state(&reopened, &start).unwrap(),
        Some(expected_state),
        "ACK and reopen remove the delivery hint, never the committed terminal"
    );
    assert!(store
        .list_runtime_job_waiters(Some("source:terminal_wait"), None, 10)
        .unwrap()
        .is_empty());
    assert!(store
        .load_checkpoint_by_turn(session_id, &wait.turn_id)
        .unwrap()
        .is_none());
    for _ in 0..2 {
        store
            .repair_terminal_runtime_job_waits(session_id, agent_run_id)
            .unwrap();
    }
    let abandoned_id = format!("runtime_wait:{}:abandoned", wait.continuation_id);
    let rows = audit
        .query(
            "SELECT payload_json FROM runtime.runtime_events WHERE event_id=$1",
            &[&abandoned_id],
        )
        .unwrap();
    assert_eq!(rows.len(), 1);
    let abandoned: serde_json::Value =
        serde_json::from_str(rows[0].get::<_, String>(0).as_str()).unwrap();
    assert_eq!(abandoned["status"], "abandoned");
    assert_eq!(abandoned["transitionReason"], "agent_run_terminal");
    // Reproduce a leftover checkpoint belonging to an already committed terminal.
    store.save_checkpoint(checkpoint).unwrap();
    store
        .repair_terminal_runtime_job_waits(session_id, agent_run_id)
        .unwrap();
    assert!(store
        .list_runtime_job_waiters(Some("source:terminal_wait"), None, 10)
        .unwrap()
        .is_empty());
    assert_eq!(
        audit
            .query_one(
                "SELECT count(*) FROM runtime.runtime_events WHERE event_id=$1",
                &[&abandoned_id]
            )
            .unwrap()
            .get::<_, i64>(0),
        1
    );
}

fn postgres_peer_session_count(url: &str) -> i64 {
    let mut observer = Client::connect(url, NoTls).expect("connect session observer");
    observer
        .query_one(
            "SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() AND pid<>pg_backend_pid()",
            &[],
        )
        .expect("count peer Postgres sessions")
        .get(0)
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_model_request_batch_deduplicates_and_hydrates_observations() {
    let _guard = TEST_LOCK.lock().expect("Postgres test lock");
    let url = test_url();
    reset_store(&url);
    let mut setup = Client::connect(&url, NoTls).expect("connect test Postgres");
    setup
        .batch_execute(
            r#"
            DROP TABLE IF EXISTS public.app_core_sessionevent;
            DROP TABLE IF EXISTS public.app_core_session CASCADE;
            CREATE TABLE public.app_core_session(
                id varchar(64) PRIMARY KEY,
                workspace_id varchar(64) NOT NULL
            );
            CREATE TABLE public.app_core_sessionevent(
                "eventId" varchar(160) PRIMARY KEY,
                workspace_id varchar(64) NOT NULL,
                session_id varchar(64) NOT NULL,
                agent_run_id varchar(64) NOT NULL,
                sequence integer NOT NULL,
                agent_run_sequence integer,
                session_level boolean NOT NULL DEFAULT false,
                projects_to_agent_run_stream boolean NOT NULL,
                payload jsonb NOT NULL,
                "createdAtMs" bigint NOT NULL,
                "insertedAt" timestamptz NOT NULL DEFAULT clock_timestamp(),
                UNIQUE(session_id, sequence),
                UNIQUE(agent_run_id, agent_run_sequence)
            );
            INSERT INTO public.app_core_session(id, workspace_id)
            VALUES('session_model_request', 'workspace_model_request');
            "#,
        )
        .expect("create session event table");
    drop(setup);
    let store = PostgresRuntimeStore::new(&url).expect("open Postgres store");
    let session_id = "session_model_request";
    let agent_run_id = "agent_run_model_request";
    let session_log = store.session_log(
        "workspace_model_request".to_string(),
        session_id.to_string(),
        "dedup".to_string(),
    );
    let digest = format!("sha256:{}", "a".repeat(64));
    let composition = centaeris_core::extension::composition::resolve_agent_composition(
        centaeris_core::extension::composition::AgentCompositionInputsV1 {
            prompt_digest: digest.clone(),
            model_binding: centaeris_core::extension::composition::ResolvedModelBindingV1 {
                provider_id: "test-provider".to_string(),
                model_name: "test-model".to_string(),
                wire_protocol: "test-wire".to_string(),
                config_digest: digest.clone(),
            },
            skill_catalog_digest: digest.clone(),
            plugin_activation_digest: digest.clone(),
            hook_composition_digest: digest.clone(),
            execution_profile_digest: digest,
            policy_version: "test-v1".to_string(),
        },
        std::iter::empty(),
    )
    .expect("composition");
    let request = |sequence: u64, request_id: &str, message_id: &str| {
        session_record(
            agent_run_id,
            session_id,
            sequence,
            SessionRecordType::ModelRequestStarted,
            serde_json::json!({
                "requestId": request_id,
                "purpose": "main",
                "loopIndex": sequence,
                "toolChoice": {"type": "none"},
                "maxOutputTokens": 1024,
                "promptCacheKey": null,
                "promptCacheRetention": null,
                "preparedPromptSchema": "prepared_prompt.v1",
                "contextTokenEstimate": 0,
                "contextTokenBreakdown": {
                    "systemPromptTokens": 0,
                    "systemToolTokens": 0,
                    "mcpToolTokens": 0,
                    "skillsTokens": 0,
                    "messageTokens": 0,
                    "mcpTools": [],
                },
                "agentComposition": composition.clone(),
                "observations": [
                    {"kind": "system_prompt", "content": "stable secret prompt"},
                    {"kind": "message", "message": {
                        "messageId": message_id,
                        "role": "user",
                        "content": format!("context for {message_id}")
                    }}
                ]
            }),
            i64::try_from(sequence).expect("timestamp"),
        )
    };
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    runtime
        .block_on(session_log.append_session_records(
            agent_run_id,
            &[
                session_record(
                    agent_run_id,
                    session_id,
                    1,
                    SessionRecordType::AgentRunStarted,
                    serde_json::json!({"userObjective": "dedup"}),
                    1,
                ),
                session_record(
                    agent_run_id,
                    session_id,
                    2,
                    SessionRecordType::UserMessage,
                    serde_json::json!({
                        "messageId": "message:turn_pg_fenced_terminal:user",
                        "text": "dedup",
                        "attachments": []
                    }),
                    1,
                ),
            ],
        ))
        .expect("append start");
    let first = request(3, "request_1", "context_1");
    let second = request(4, "request_2", "context_2");
    runtime
        .block_on(session_log.append_session_records(agent_run_id, std::slice::from_ref(&first)))
        .expect("append first request");
    runtime
        .block_on(session_log.append_session_records(agent_run_id, std::slice::from_ref(&second)))
        .expect("append second request");
    let retry = runtime
        .block_on(session_log.append_session_records(agent_run_id, std::slice::from_ref(&second)))
        .expect("idempotent hydrated retry");
    assert_eq!(retry.records[0].event, second.event);

    let mut audit = Client::connect(&url, NoTls).expect("connect audit");
    let raw_requests = audit
        .query(
            "SELECT payload::text FROM public.app_core_sessionevent WHERE payload->>'type'='model_request_started' ORDER BY sequence",
            &[],
        )
        .expect("load stored requests");
    assert_eq!(raw_requests.len(), 2);
    assert!(raw_requests.iter().all(|row| {
        let payload = row.get::<_, String>(0);
        !payload.contains("stable secret prompt")
            && payload.contains("manifestDigest")
            && !payload.contains("contentDigest")
    }));
    let content_count = audit
        .query_one(
            "SELECT COUNT(*) FROM runtime.model_observation_contents WHERE session_id=$1",
            &[&session_id],
        )
        .expect("count unique observation contents")
        .get::<_, i64>(0);
    assert_eq!(content_count, 3);
    let session_rows = audit
        .query_one(
            "SELECT COUNT(*) FROM public.app_core_sessionevent WHERE session_id=$1",
            &[&session_id],
        )
        .expect("count session rows")
        .get::<_, i64>(0);
    let commit_rows = audit
        .query_one(
            "SELECT COUNT(*) FROM runtime.runtime_events WHERE session_id=$1 AND event_type='session_record_committed'",
            &[&session_id],
        )
        .expect("count commit rows")
        .get::<_, i64>(0);
    assert_eq!((session_rows, commit_rows), (4, 4));
    assert_eq!(
        audit
            .query_one(
                "SELECT COUNT(*) FROM runtime.model_observation_manifests WHERE session_id=$1",
                &[&session_id],
            )
            .expect("count manifest nodes")
            .get::<_, i64>(0),
        2
    );

    // Real append/UNNEST/CAS/hydration growth, excluding the two fixtures above.
    let storage_totals = |audit: &mut Client| {
        let row = audit.query_one(
            "SELECT (SELECT COUNT(*) FROM runtime.model_observation_manifests WHERE session_id=$1),(SELECT COALESCE(SUM(jsonb_array_length(manifest_json::jsonb->'changes')),0)::bigint FROM runtime.model_observation_manifests WHERE session_id=$1),(SELECT COALESCE(SUM(manifest_bytes),0)::bigint FROM runtime.model_observation_manifests WHERE session_id=$1),(SELECT COUNT(*) FROM runtime.model_observation_contents WHERE session_id=$1),(SELECT COALESCE(SUM(content_bytes),0)::bigint FROM runtime.model_observation_contents WHERE session_id=$1),(SELECT COALESCE(SUM(octet_length(payload::text)),0)::bigint FROM public.app_core_sessionevent WHERE session_id=$1 AND payload->>'type'='model_request_started'),(SELECT COALESCE(SUM(octet_length(payload_json)),0)::bigint FROM runtime.runtime_events WHERE session_id=$1 AND event_type='session_record_committed' AND payload_json::jsonb->>'sessionRecordType'='model_request_started'),(SELECT COUNT(*) FROM runtime.runtime_events WHERE session_id=$1 AND event_type='session_record_committed' AND payload_json::jsonb->>'sessionRecordType'='model_request_started')",
            &[&session_id],
        ).expect("measure stored observation payloads");
        [
            row.get::<_, i64>(0),
            row.get(1),
            row.get(2),
            row.get(3),
            row.get(4),
            row.get(5),
            row.get(6),
            row.get(7),
        ]
    };
    let baseline = storage_totals(&mut audit);
    let mut observations = vec![
        serde_json::json!({"kind": "system_prompt", "content": "growth stable system"}),
        serde_json::json!({"kind": "message", "message": {"messageId": "runtime-context", "role": "user", "content": "context 0"}}),
        serde_json::json!({"kind": "message", "message": {"messageId": "stable-context", "role": "user", "content": "stable prior context"}}),
    ];
    let mut curve = Vec::new();
    let mut latest = second.clone();
    for round in 1..=2046usize {
        observations[1]["message"]["content"] =
            serde_json::json!(format!("runtime context {round}"));
        for role in ["user", "assistant"] {
            observations.push(serde_json::json!({"kind": "message", "message": {
                "messageId": format!("growth-{round}-{role}"), "role": role,
                "content": format!("{round}:{role}:{}", "m".repeat(64)),
            }}));
        }
        latest = request(
            round as u64 + 4,
            &format!("growth-request-{round}"),
            "unused",
        );
        latest.event.payload["observations"] = serde_json::json!(observations);
        runtime
            .block_on(
                session_log.append_session_records(agent_run_id, std::slice::from_ref(&latest)),
            )
            .expect("append growth request");
        if [20, 81, 512, 2046].contains(&round) {
            let measured = storage_totals(&mut audit);
            let values = std::array::from_fn::<_, 8, _>(|index| measured[index] - baseline[index]);
            assert_eq!(values[0], round as i64);
            assert_eq!(values[1], (3 * round + 2) as i64);
            assert_eq!(values[3], (3 * round + 2) as i64);
            assert_eq!(values[7], round as i64);
            curve.push(
                serde_json::json!({"rounds": round, "observationCount": observations.len(),
                "manifestNodes": values[0], "manifestRefs": values[1], "manifestBytes": values[2],
                "uniqueContents": values[3], "contentBytes": values[4], "eventRootBytes": values[5],
                "commitPayloadBytes": values[6], "commitRows": values[7],
                "physicalRows": values[0] + values[3] + 2 * values[7],
                "physicalPayloadBytes": values[2] + values[4] + values[5] + values[6]}),
            );
        }
    }
    let before_retry = storage_totals(&mut audit);
    let retry = runtime
        .block_on(session_log.append_session_records(agent_run_id, std::slice::from_ref(&latest)))
        .expect("retry long-chain request");
    assert_eq!(retry.records[0].event, latest.event);
    assert_eq!(storage_totals(&mut audit), before_retry);
    println!(
        "RUNTIME_01_ARTIFACT {}",
        serde_json::json!({
            "gate": "postgres_manifest_database_growth", "measurement": "actual_postgres_rows_and_payload_bytes_excludes_indexes_mvcc_relation_overhead",
            "workload": "early_runtime_context_replaced_and_two_tail_observations_appended_per_round", "curve": curve,
        })
    );
    let stored_root = audit.query_one(
        "SELECT payload::text FROM public.app_core_sessionevent WHERE session_id=$1 AND payload->>'type'='model_request_started' ORDER BY sequence DESC LIMIT 1", &[&session_id],
    ).expect("read last compact root").get::<_, String>(0);
    let mut other_wire: serde_json::Value = serde_json::from_str(&stored_root).unwrap();
    other_wire["sessionId"] = serde_json::json!("session_other");
    assert!(super::runtime::hydrate_session_wire_values(
        &mut audit,
        std::slice::from_mut(&mut other_wire)
    )
    .expect_err("cross-session manifest must fail")
    .contains("missing"));
    let root: serde_json::Value = serde_json::from_str(&stored_root).unwrap();
    let digest = root["payload"]["observations"]["manifestDigest"]
        .as_str()
        .unwrap();
    let original = audit.query_one("SELECT manifest_json,manifest_bytes FROM runtime.model_observation_manifests WHERE session_id=$1 AND manifest_digest=$2", &[&session_id, &digest]).unwrap();
    audit.execute("UPDATE runtime.model_observation_manifests SET manifest_json='{}',manifest_bytes=2 WHERE session_id=$1 AND manifest_digest=$2", &[&session_id, &digest]).unwrap();
    assert!(runtime
        .block_on(session_log.append_session_records(agent_run_id, std::slice::from_ref(&latest)))
        .expect_err("tampered manifest must fail")
        .contains("digest mismatch"));
    audit.execute("UPDATE runtime.model_observation_manifests SET manifest_json=$3,manifest_bytes=$4 WHERE session_id=$1 AND manifest_digest=$2", &[&session_id, &digest, &original.get::<_, String>(0), &original.get::<_, i64>(1)]).unwrap();
    let content_digest = audit.query_one("SELECT content_digest FROM runtime.model_observation_contents WHERE session_id=$1 AND kind='system_prompt' ORDER BY first_seen_at_ms DESC LIMIT 1", &[&session_id]).unwrap().get::<_, String>(0);
    audit.execute("DELETE FROM runtime.model_observation_contents WHERE session_id=$1 AND content_digest=$2", &[&session_id, &content_digest]).unwrap();
    let next = request(2051, "after-missing-content", "unused");
    assert!(runtime
        .block_on(session_log.append_session_records(agent_run_id, &[next]))
        .expect_err("new append must not silently heal missing content")
        .contains("incomplete"));

    store
        .delete_session_data(session_id)
        .expect("delete runtime session data");
    assert_eq!(
        audit
            .query_one(
                "SELECT COUNT(*) FROM runtime.model_observation_contents WHERE session_id=$1",
                &[&session_id],
            )
            .expect("count deleted contents")
            .get::<_, i64>(0),
        0
    );
    assert_eq!(
        audit
            .query_one(
                "SELECT COUNT(*) FROM runtime.model_observation_manifests WHERE session_id=$1",
                &[&session_id],
            )
            .expect("count deleted manifests")
            .get::<_, i64>(0),
        0
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_runtime_store_validates_waiter_owner_index_on_reopen() {
    let _guard = TEST_LOCK.lock().expect("Postgres test lock");
    let url = test_url();
    reset_store(&url);
    drop(PostgresRuntimeStore::new(&url).expect("fresh schema must validate"));
    drop(PostgresRuntimeStore::new(&url).expect("existing schema must validate"));
    let mut client = Client::connect(&url, NoTls).expect("connect");
    client.batch_execute(
        "DROP INDEX runtime.idx_runtime_job_waiters_owner; CREATE INDEX idx_runtime_job_waiters_owner ON runtime.runtime_job_waiters(session_id,agent_run_id,checkpoint_id)"
    ).expect("replace with wrong column order");
    let error = PostgresRuntimeStore::new(&url).expect_err("owner index order drift must fail");
    assert!(
        error.contains("index definition mismatch: idx_runtime_job_waiters_owner"),
        "{error}"
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_runtime_store_rejects_schema_drift() {
    let _guard = TEST_LOCK.lock().expect("Postgres test lock");
    let url = test_url();
    reset_store(&url);
    PostgresRuntimeStore::new(&url).expect("create schema");
    let mut client = Client::connect(&url, NoTls).expect("connect");
    client
        .batch_execute("ALTER TABLE runtime.runtime_jobs ALTER COLUMN last_error TYPE varchar(40)")
        .expect("corrupt schema");
    let error = PostgresRuntimeStore::new(&url).expect_err("schema drift must fail");
    assert!(error.contains("table definition mismatch"));
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database and default execution limits"]
fn hosted_execution_capacity_is_shared_across_replicas_and_released_on_yield() {
    let _guard = TEST_LOCK.lock().unwrap();
    let limits = crate::execution_capacity::ExecutionCapacity::from_env().unwrap();
    assert_eq!((limits.global, limits.tenant), (8, 4));
    let url = test_url();
    reset_store(&url);
    let replicas = [
        PostgresRuntimeStore::new(&url).unwrap(),
        PostgresRuntimeStore::new(&url).unwrap(),
    ];
    for tenant in ["a", "b"] {
        for index in 0..6 {
            let id = format!("agent_run.lifecycle:agent_run_{tenant}_{index}");
            let mut record = job(&id, &id);
            record.job_kind = "agent_run.lifecycle".to_string();
            replicas[0]
                .schedule_worker_job(
                    ScheduleRuntimeJobRequest {
                        job: record.clone(),
                    },
                    Some(tenant),
                )
                .unwrap();
            assert!(replicas[1]
                .schedule_worker_job(ScheduleRuntimeJobRequest { job: record }, Some("other"))
                .is_err());
        }
    }
    fn claim(store: &PostgresRuntimeStore, index: usize) -> Result<Vec<RuntimeJobRecord>, String> {
        store.claim_worker_jobs(ClaimDueRuntimeJobsRequest {
            now_ms: 1,
            worker_id: format!("worker:capacity:{index}"),
            job_id: None,
            job_kind: Some("agent_run.lifecycle".to_string()),
            session_id: None,
            limit: 1,
            lease_ms: 60_000,
        })
    }
    let mut claimed = std::thread::scope(|scope| {
        let handles = (0..16)
            .map(|index| {
                let store = replicas[index % 2].clone();
                scope.spawn(move || claim(&store, index))
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .flat_map(|handle| match handle.join().unwrap() {
                Ok(jobs) => jobs,
                Err(error) if error == "execution claim busy" => Vec::new(),
                Err(error) => panic!("unexpected claim failure: {error}"),
            })
            .collect::<Vec<_>>()
    });
    loop {
        let jobs = claim(&replicas[1], 100).unwrap();
        if jobs.is_empty() {
            break;
        }
        claimed.extend(jobs);
        assert!(claimed.len() <= 8);
    }
    assert_eq!(claimed.len(), 8);
    assert_eq!(
        claimed
            .iter()
            .filter(|job| job.job_id.contains("_a_"))
            .count(),
        4
    );
    assert_eq!(
        claimed
            .iter()
            .filter(|job| job.job_id.contains("_b_"))
            .count(),
        4
    );
    let waiting = replicas[1]
        .wait_for_runtime_jobs(
            &["agent_run.lifecycle".to_string()],
            Duration::from_millis(10),
        )
        .unwrap();
    assert!(!waiting.ready);
    let released = &claimed[0];
    let now = released.heartbeat_at_ms.unwrap() + 1;
    replicas[0]
        .yield_runtime_job(YieldRuntimeJobRequest {
            job_id: released.job_id.clone(),
            lease_owner: released.lease_owner.clone().unwrap(),
            yielded_at_ms: now,
            run_at_ms: now + 60_000,
            transition_reason: "question_wait".to_string(),
        })
        .unwrap();
    assert!(
        replicas[1]
            .wait_for_runtime_jobs(
                &["agent_run.lifecycle".to_string()],
                Duration::from_millis(10)
            )
            .unwrap()
            .ready
    );
    assert_eq!(claim(&replicas[1], 101).unwrap().len(), 1);
    assert!(claim(&replicas[0], 102).unwrap().is_empty());
}

struct RecordingModelClient {
    calls: std::sync::atomic::AtomicUsize,
}

impl centaeris_core::model::ModelClient for RecordingModelClient {
    fn generate<'a>(
        &'a self,
        _request: &'a centaeris_core::model::ModelClientRequest,
    ) -> centaeris_core::model::ModelClientFuture<'a, centaeris_core::model::ModelClientResponse>
    {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Box::pin(async {
            Err(centaeris_core::model::ModelClientError::new(
                centaeris_core::model::ModelClientErrorKind::Provider,
                "model must not be invoked while admitting a new user turn".to_string(),
                false,
            ))
        })
    }
}

struct UnavailableExecutionHost;

impl centaeris_core::execution::ExecutionHostRunner for UnavailableExecutionHost {
    fn status(
        &self,
        _policy: &centaeris_core::execution::ExecutionPolicy,
    ) -> Result<
        centaeris_core::execution::ExecutionHostStatus,
        centaeris_core::execution::ExecutionError,
    > {
        Err(centaeris_core::execution::ExecutionError::HostUnavailable {
            reason: "test execution host is unavailable".to_string(),
        })
    }

    fn run_file_system_operation(
        &self,
        _request: centaeris_core::execution::ExecutionFileSystemRequest,
    ) -> Result<
        centaeris_core::execution::ExecutionFileSystemOutput,
        centaeris_core::execution::ExecutionFileSystemError,
    > {
        panic!("test does not execute filesystem operations")
    }

    fn run_host_command(
        &self,
        _operation_id: Option<&str>,
        _request: centaeris_core::execution::ExecutionCommandRequest,
        _cancellation_probe: Option<&centaeris_core::execution::ExecutionCancellationProbe>,
    ) -> Result<
        centaeris_core::execution::ExecutionHostCommandOutput,
        centaeris_core::execution::ExecutionError,
    > {
        Err(centaeris_core::execution::ExecutionError::HostUnavailable {
            reason: "test execution host is unavailable".to_string(),
        })
    }
}

fn unavailable_tool_layer() -> centaeris_core::tool::layer::ToolLayer {
    let workspace_root = std::env::temp_dir();
    let binding = std::sync::Arc::new(
        centaeris_core::execution::ExecutionHostBinding::new(
            centaeris_core::execution::ExecutionHostMode::Remote,
            std::sync::Arc::new(UnavailableExecutionHost),
            workspace_root.clone(),
            centaeris_core::execution::ExecutionPolicy::workspace_write_no_network(workspace_root),
        )
        .expect("test execution host binding"),
    );
    centaeris_core::tool::layer::ToolLayer::try_new_with_skill_catalog_config_and_execution_host_binding(
        centaeris_core::extension::skills::SkillCatalogLoadConfig::default(),
        binding,
    )
    .expect("test tool layer")
}

// A run that terminates with an assistant tool call but no tool result leaves an
// unpaired tail. Hosted persists the next user turn before Core closes that tail,
// so the durable history becomes illegal and Core must reject the new turn.
#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_new_user_turn_after_unpaired_tool_call_fails_admission() {
    use centaeris_core::model::ToolCallEnvelope;
    use centaeris_core::runtime::{
        AgentRunInitialInput, AgentRunRequest, AgentRuntime, AgentRuntimeConfig,
        ToolConcurrencyCoordinator, TurnUpdate,
    };
    use std::sync::atomic::Ordering;

    let _guard = TEST_LOCK.lock().expect("Postgres test lock");
    let url = test_url();
    reset_store(&url);

    let session_id = "session_unpaired";
    let workspace_id = "workspace_unpaired";
    let mut setup = Client::connect(&url, NoTls).expect("connect setup");
    setup
        .batch_execute(
            r#"
            DROP TABLE IF EXISTS public.app_core_sessionevent;
            DROP TABLE IF EXISTS public.app_core_session CASCADE;
            CREATE TABLE public.app_core_session(id varchar(64) PRIMARY KEY,workspace_id varchar(64) NOT NULL);
            CREATE TABLE public.app_core_sessionevent(
                "eventId" varchar(160) PRIMARY KEY,workspace_id varchar(64) NOT NULL,
                session_id varchar(64) NOT NULL,agent_run_id varchar(64) NOT NULL,
                sequence integer NOT NULL,agent_run_sequence integer,
                session_level boolean NOT NULL DEFAULT false,
                projects_to_agent_run_stream boolean NOT NULL,payload jsonb NOT NULL,
                "createdAtMs" bigint NOT NULL,"insertedAt" timestamptz NOT NULL DEFAULT clock_timestamp(),
                UNIQUE(session_id,sequence),UNIQUE(agent_run_id,agent_run_sequence)
            );
            INSERT INTO public.app_core_session(id,workspace_id) VALUES('session_unpaired','workspace_unpaired');
            "#,
        )
        .expect("create session source tables");
    drop(setup);

    let store = PostgresRuntimeStore::new(&url).expect("open Postgres store");
    let old_log = store.session_log(
        workspace_id.to_string(),
        session_id.to_string(),
        "旧请求".to_string(),
    );
    let new_log = store.session_log(
        workspace_id.to_string(),
        session_id.to_string(),
        "继续".to_string(),
    );
    let runtime = tokio::runtime::Runtime::new().expect("runtime");

    let old_run = "agent_run_unpaired_old";
    let mut old = centaeris_core::session::AgentRunSessionState::new(session_id, old_run)
        .expect("old run state");
    let mut old_records = old
        .start(old_run, "旧请求", Vec::new(), 1)
        .expect("old started records");
    let call = ToolCallEnvelope {
        id: "call_open".to_string(),
        name: "bash".to_string(),
        args_json: "{\"command\":\"ls\"}".to_string(),
    };
    old_records.extend(
        old.record_tool_call(
            "turn_old",
            &call,
            "centaeris.builtin",
            format!("sha256:{}", "c".repeat(64)).as_str(),
            "bash",
            2,
        )
        .expect("record tool call"),
    );
    old_records.push(
        old.record(
            centaeris_core::session::failed_agent_run_record(
                session_id,
                "turn_old",
                old_run,
                "execution_failed",
                "boom",
                3,
            )
            .expect("failed record"),
        )
        .expect("record terminal"),
    );
    runtime
        .block_on(old_log.append_session_records(old_run, &old_records))
        .expect("append old run");

    // Hosted's first commit for the new run writes the user turn before Core runs.
    let new_run = "agent_run_unpaired_new";
    let mut next = centaeris_core::session::AgentRunSessionState::new(session_id, new_run)
        .expect("new run state");
    let new_records = next
        .start(new_run, "继续", Vec::new(), 4)
        .expect("new started records");
    runtime
        .block_on(new_log.append_session_records(new_run, &new_records))
        .expect("append new run");

    let mut audit = Client::connect(&url, NoTls).expect("connect audit");
    let rows = audit
        .query(
            "SELECT payload::text FROM public.app_core_sessionevent WHERE session_id=$1 ORDER BY sequence",
            &[&session_id],
        )
        .expect("read session log");
    let mut wires = rows
        .iter()
        .map(|row| {
            serde_json::from_str::<serde_json::Value>(row.get::<_, String>(0).as_str())
                .expect("wire json")
        })
        .collect::<Vec<_>>();
    super::runtime::hydrate_session_wire_values(&mut audit, &mut wires).expect("hydrate wires");
    let events = wires
        .iter()
        .map(|wire| {
            centaeris_core::session::parse_wire_record(wire)
                .expect("parse wire")
                .event
        })
        .collect::<Vec<_>>();
    let snapshot =
        centaeris_core::session::restore_runtime_snapshot_from_session_records(session_id, &events)
            .expect("rebuild Core history from durable records");
    centaeris_core::session::manager::SessionManager::new(store.clone())
        .save_session(&snapshot)
        .expect("materialize Core session snapshot");

    let tool_concurrency =
        ToolConcurrencyCoordinator::global_for_scope(format!("test:{session_id}"), 4)
            .expect("tool concurrency");
    let agent_runtime = AgentRuntime::new(
        store.clone(),
        unavailable_tool_layer(),
        AgentRuntimeConfig::default(),
        tool_concurrency,
    );
    let model_client = RecordingModelClient {
        calls: std::sync::atomic::AtomicUsize::new(0),
    };
    let config_store = centaeris_core::model::EmptyModelSessionConfigStore::new();
    let mut stream = |_update: TurnUpdate| {};
    let cancellation = || Ok(None);
    let mut safe_point = |_point: centaeris_core::runtime::ToolSafePoint| Ok(());
    let result = runtime.block_on(
        agent_runtime
            .process_turn_loop_online_with_model_client_stream_cancellable_and_tool_safe_point_async(
                AgentRunRequest {
                    session_id: session_id.to_string(),
                    initial_turn_id: "turn_new".to_string(),
                    initial_input: AgentRunInitialInput::UserMessage("继续".to_string()),
                    agent_run_identity: Some(
                        centaeris_core::runtime::contracts::RuntimeAgentRunIdentityV1 {
                            agent_run_id: new_run.to_string(),
                            execution_id: "execution_unpaired".to_string(),
                            authorization_digest: format!("sha256:{}", "a".repeat(64)),
                        },
                    ),
                    runtime_scope:
                        centaeris_core::model::prompt::PromptCompactionScopeV1::main(),
                    resume_from_turn_id: None,
                    auto_continue_after_resume_wait: None,
                },
                &model_client,
                &config_store,
                &mut stream,
                &cancellation,
                &mut safe_point,
            ),
    );

    let error = result.expect_err("an unpaired tool call must block the new user turn");
    assert!(
        error.contains("context_window_materialization_invalid_tool_pairing"),
        "unexpected admission error: {error}"
    );
    assert_eq!(
        model_client.calls.load(Ordering::SeqCst),
        0,
        "the model must not be called while admitting the new user turn"
    );
}

// Confirms the transcript generation rotation contract the block-schema guard
// will rely on: rotation is a blue/green pointer switch over a pre-built,
// complete next generation. It does not build the next generation and it does
// not delete the previous generation's derived rows.
#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_transcript_generation_rotation_switches_pointer_without_deleting_old_rows() {
    use centaeris_core::session::transcript::{
        TranscriptProjectionGenerationRotationDispositionV1,
        TranscriptProjectionGenerationRotationV1, TranscriptProjectionGenerationStorePortV1,
    };

    let _guard = TEST_LOCK.lock().expect("Postgres test lock");
    let url = test_url();
    reset_store(&url);
    let store = PostgresRuntimeStore::new(&url).expect("open Postgres store");
    let mut setup = Client::connect(&url, NoTls).expect("connect setup");
    setup
        .batch_execute(
            r#"
            DROP TABLE IF EXISTS public.app_core_sessionevent;
            DROP TABLE IF EXISTS public.app_core_session CASCADE;
            CREATE TABLE public.app_core_session(id varchar(64) PRIMARY KEY,workspace_id varchar(64) NOT NULL);
            CREATE TABLE public.app_core_sessionevent(
                "eventId" varchar(160) PRIMARY KEY,workspace_id varchar(64) NOT NULL,
                session_id varchar(64) NOT NULL,agent_run_id varchar(64) NOT NULL,
                sequence integer NOT NULL,agent_run_sequence integer,
                session_level boolean NOT NULL DEFAULT false,
                projects_to_agent_run_stream boolean NOT NULL,payload jsonb NOT NULL,
                "createdAtMs" bigint NOT NULL,"insertedAt" timestamptz NOT NULL DEFAULT clock_timestamp(),
                UNIQUE(session_id,sequence),UNIQUE(agent_run_id,agent_run_sequence)
            );
            INSERT INTO public.app_core_session(id,workspace_id) VALUES('session_rotate','workspace_rotate');
            "#,
        )
        .expect("create rotation source tables");
    setup
        .execute(
            "INSERT INTO runtime.transcript_projection_heads(session_id,projection_version,projection_generation,source_high_water,invalidation_reason) VALUES('session_rotate',$1,'generation-1',5,NULL),('session_rotate',$1,'generation-2',9,NULL)",
            &[&TRANSCRIPT_PROJECTION_VERSION_V1],
        )
        .expect("seed generation heads");
    setup
        .execute(
            "INSERT INTO runtime.transcript_projection_current_generations(session_id,projection_version,projection_generation,source_high_water) VALUES('session_rotate',$1,'generation-1',5)",
            &[&TRANSCRIPT_PROJECTION_VERSION_V1],
        )
        .expect("seed current generation");

    // A rotation whose next generation is missing or incomplete must be rejected.
    let rejected = store.rotate_current_transcript_projection_generation(
        TranscriptProjectionGenerationRotationV1 {
            session_id: "session_rotate".to_string(),
            projection_version: TRANSCRIPT_PROJECTION_VERSION_V1.to_string(),
            expected_current_generation: Some("generation-1".to_string()),
            next_generation: "generation-3".to_string(),
            target_source_high_water: "9".to_string(),
        },
    );
    assert!(
        rejected.is_err(),
        "rotation must require a built next generation"
    );

    let disposition = store
        .rotate_current_transcript_projection_generation(TranscriptProjectionGenerationRotationV1 {
            session_id: "session_rotate".to_string(),
            projection_version: TRANSCRIPT_PROJECTION_VERSION_V1.to_string(),
            expected_current_generation: Some("generation-1".to_string()),
            next_generation: "generation-2".to_string(),
            target_source_high_water: "9".to_string(),
        })
        .expect("rotate generation");
    assert!(matches!(
        disposition,
        TranscriptProjectionGenerationRotationDispositionV1::Applied
    ));
    let current = store
        .load_current_transcript_projection_generation("session_rotate")
        .expect("load current generation")
        .expect("current generation exists");
    assert_eq!(current.projection_generation, "generation-2");
    assert_eq!(current.source_high_water, "9");
    let old_heads = setup
        .query_one(
            "SELECT count(*) FROM runtime.transcript_projection_heads WHERE session_id='session_rotate' AND projection_generation='generation-1'",
            &[],
        )
        .expect("count old generation heads")
        .get::<_, i64>(0);
    assert_eq!(
        old_heads, 1,
        "rotation must not delete the previous generation's derived rows"
    );
}

// Positive admission: a session-level closure and the new run's first batch are
// committed in one transaction, re-delivery is idempotent, and a stale plan is
// rejected.
#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_new_user_turn_admission_commits_closure_and_run_atomically() {
    use centaeris_core::model::ToolCallEnvelope;
    use centaeris_core::runtime::contracts::{NewUserTurnClosurePlanV1, UnpairedToolCallClosureV1};
    use centaeris_core::session::AgentRunSessionState;
    use centaeris_core::tool::layer::ToolExecutionResult;

    let _guard = TEST_LOCK.lock().expect("Postgres test lock");
    let url = test_url();
    reset_store(&url);

    let store = PostgresRuntimeStore::new(&url).expect("open Postgres store");
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let session_id = "session_admission";
    let workspace_id = "workspace_admission";
    let mut setup = Client::connect(&url, NoTls).expect("connect setup");
    setup
        .batch_execute(
            r#"
            DROP TABLE IF EXISTS public.app_core_sessionevent;
            DROP TABLE IF EXISTS public.app_core_session CASCADE;
            CREATE TABLE public.app_core_session(id varchar(64) PRIMARY KEY,workspace_id varchar(64) NOT NULL);
            CREATE TABLE public.app_core_sessionevent(
                "eventId" varchar(160) PRIMARY KEY,workspace_id varchar(64) NOT NULL,
                session_id varchar(64) NOT NULL,agent_run_id varchar(64) NOT NULL,
                sequence integer NOT NULL,agent_run_sequence integer,
                session_level boolean NOT NULL DEFAULT false,
                projects_to_agent_run_stream boolean NOT NULL,payload jsonb NOT NULL,
                "createdAtMs" bigint NOT NULL,"insertedAt" timestamptz NOT NULL DEFAULT clock_timestamp(),
                UNIQUE(session_id,sequence),UNIQUE(agent_run_id,agent_run_sequence)
            );
            INSERT INTO public.app_core_session(id,workspace_id) VALUES('session_admission','workspace_admission');
            INSERT INTO runtime.runtime_jobs(job_id,job_kind,status,run_at_ms,lease_owner,lease_expires_at_ms,backoff_policy_json,idempotency_key,session_id,created_at_ms,updated_at_ms)
            VALUES('admission_job','agent_run.lifecycle','running',0,'admission_owner',(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint + 60000,'{}','admission_key','session_admission',1,1);
            "#,
        )
        .expect("create admission source tables");
    drop(setup);

    let old_run = "agent_run_admission_old";
    let mut old = AgentRunSessionState::new(session_id, old_run).expect("old run state");
    let mut old_records = old
        .start(old_run, "旧请求", Vec::new(), 1)
        .expect("old start");
    let call = ToolCallEnvelope {
        id: "call_admission".to_string(),
        name: "bash".to_string(),
        args_json: "{\"command\":\"ls\"}".to_string(),
    };
    old_records.extend(
        old.record_tool_call(
            "turn_old",
            &call,
            "centaeris.builtin",
            format!("sha256:{}", "c".repeat(64)).as_str(),
            "bash",
            2,
        )
        .expect("record tool call"),
    );
    old_records.push(
        old.record(
            centaeris_core::session::failed_agent_run_record(
                session_id,
                "turn_old",
                old_run,
                "execution_failed",
                "boom",
                3,
            )
            .expect("failed record"),
        )
        .expect("record terminal"),
    );
    runtime
        .block_on(
            store
                .session_log(
                    workspace_id.to_string(),
                    session_id.to_string(),
                    "旧请求".to_string(),
                )
                .append_session_records(old_run, &old_records),
        )
        .expect("append old run");

    let result = ToolExecutionResult {
        tool_call_id: call.id.clone(),
        tool_name: call.name.clone(),
        status: "blocked".to_string(),
        content:
            "The previous unpaired tool call was closed before execution; it was not replayed.\0Literal \\u0000"
                .to_string(),
        details: serde_json::json!({"schema": "tool_result_tombstone_v1", "status": "blocked"}),
        facts: Vec::new(),
        error: None,
        started_at_ms: 4,
        completed_at_ms: 4,
        latency_ms: 0,
        parallel_group: None,
        transition_reason: Some("unpaired_tool_call_closed_by_new_user_turn".to_string()),
    };
    let new_run = "agent_run_admission_new";
    let closure = UnpairedToolCallClosureV1 {
        recovery: "not_executed".to_string(),
        session_id: session_id.to_string(),
        turn_id: "turn_old".to_string(),
        agent_run_id: old_run.to_string(),
        call: call.clone(),
        result: result.clone(),
        call_event_id: None,
        trigger_agent_run_id: new_run.to_string(),
        trigger_turn_id: "turn_new".to_string(),
    };
    let closure_event = centaeris_core::runtime::canonical_tool_call_closure_record(
        session_id,
        "turn_old",
        old_run,
        &call,
        &result,
        "not_executed",
        None,
        new_run,
        "turn_new",
        4,
    )
    .expect("closure record");

    let mut next = AgentRunSessionState::new(session_id, new_run).expect("new run state");
    let new_records = next
        .start(new_run, "继续", Vec::new(), 5)
        .expect("new start");
    let plan = NewUserTurnClosurePlanV1 {
        session_id: session_id.to_string(),
        expected_session_sequence: 4,
        trigger_agent_run_id: new_run.to_string(),
        trigger_turn_id: "turn_new".to_string(),
        evidence_preconditions: Vec::new(),
        closures: vec![closure],
    };
    let fence = RuntimeJobLeaseFence {
        job_id: "admission_job".to_string(),
        job_kind: centaeris_core::session::reliability::AGENT_RUN_LIFECYCLE_JOB_KIND.to_string(),
        lease_owner: "admission_owner".to_string(),
    };
    let log = store.session_log(
        workspace_id.to_string(),
        session_id.to_string(),
        "继续".to_string(),
    );

    let receipt = log
        .append_new_user_turn_admission_blocking(
            new_run,
            &new_records,
            std::slice::from_ref(&closure_event),
            &plan,
            &fence,
        )
        .expect("admission commits");
    assert_eq!(receipt.records.len(), 3);

    let mut audit = Client::connect(&url, NoTls).expect("connect audit");
    let closure_row = audit
        .query_one(
            "SELECT session_level,agent_run_sequence,projects_to_agent_run_stream,agent_run_id FROM app_core_sessionevent WHERE \"eventId\"=$1",
            &[&closure_event.event_id],
        )
        .expect("closure row");
    assert!(closure_row.get::<_, bool>(0));
    assert!(closure_row.get::<_, Option<i32>>(1).is_none());
    assert!(!closure_row.get::<_, bool>(2));
    assert_eq!(closure_row.get::<_, String>(3), old_run);
    // SSE isolation: the closure stays out of the per-run stream while
    // remaining part of the session the transcript reads.
    let closure_in_stream: i64 = audit
        .query_one(
            "SELECT count(*) FROM app_core_sessionevent WHERE agent_run_id=$1 AND \"eventId\"=$2 AND projects_to_agent_run_stream=true",
            &[&old_run, &closure_event.event_id],
        )
        .expect("count closure in per-run stream")
        .get(0);
    assert_eq!(closure_in_stream, 0);

    // Re-delivery of the same batch returns the stored receipt without new rows.
    let again = log
        .append_new_user_turn_admission_blocking(
            new_run,
            &new_records,
            std::slice::from_ref(&closure_event),
            &plan,
            &fence,
        )
        .expect("idempotent re-delivery");
    assert_eq!(again.records.len(), 3);
    let total: i64 = audit
        .query_one(
            "SELECT count(*) FROM app_core_sessionevent WHERE session_id=$1",
            &[&session_id],
        )
        .expect("count session records")
        .get(0);
    assert_eq!(total, 7);

    // A stale plan over an uncommitted batch is rejected: its expected head no
    // longer matches the confirmed session head.
    let stale_run = "agent_run_admission_stale";
    let mut stale_state =
        AgentRunSessionState::new(session_id, stale_run).expect("stale run state");
    let stale_records = stale_state
        .start(stale_run, "继续", Vec::new(), 8)
        .expect("stale start");
    let stale = NewUserTurnClosurePlanV1 {
        session_id: session_id.to_string(),
        expected_session_sequence: 999,
        trigger_agent_run_id: stale_run.to_string(),
        trigger_turn_id: "turn_stale".to_string(),
        evidence_preconditions: Vec::new(),
        closures: Vec::new(),
    };
    assert!(log
        .append_new_user_turn_admission_blocking(stale_run, &stale_records, &[], &stale, &fence,)
        .is_err());

    // The closure updates the original tool block in the session transcript,
    // keeping the block id and order key and advancing the revision.
    for _ in 0..=7 {
        if store
            .catch_up_transcript_projection(session_id, "generation-1", 7)
            .expect("catch up transcript")
            .caught_up
        {
            break;
        }
    }
    let (status, body) = crate::transcript_protocol::handle(
        "/internal/transcript/page",
        serde_json::to_vec(&serde_json::json!({
            "schema": "runtime.transcript.page.read.v1",
            "sessionId": session_id,
            "projectionVersion": TRANSCRIPT_PROJECTION_VERSION_V1,
            "projectionGeneration": null,
            "sourceHighWater": "7",
            "olderCursor": null,
        }))
        .expect("page request")
        .as_slice(),
        &store,
    )
    .expect("transcript protocol route")
    .expect("transcript protocol response");
    assert_eq!(status, 200);
    let page: serde_json::Value = serde_json::from_slice(&body).expect("page json");
    let tool = page["blocks"]
        .as_array()
        .expect("blocks")
        .iter()
        .find(|block| block["body"]["callId"] == "call_admission")
        .expect("projected tool block");
    assert_eq!(tool["blockId"], "tool:call_admission");
    assert_eq!(tool["orderKey"]["sourceSequence"], "3");
    assert_ne!(tool["blockRevision"], "1");
    let output_ref = tool["body"]["outputRef"]["refId"]
        .as_str()
        .expect("closure output reference");
    assert!(output_ref.starts_with("session-event:"));
    assert!(output_ref.ends_with(":modelContent"));
}

// A projection written by an older block encoding is invalidated and rebuilt
// under the current generation instead of being decoded under the new fields.
#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_transcript_old_block_encoding_generation_is_invalidated_and_reinitialized() {
    let _guard = TEST_LOCK.lock().expect("Postgres test lock");
    let url = test_url();
    reset_store(&url);
    let store = PostgresRuntimeStore::new(&url).expect("open Postgres store");
    let mut setup = Client::connect(&url, NoTls).expect("connect setup");
    setup
        .execute(
            "INSERT INTO runtime.transcript_projection_heads(session_id,projection_version,projection_generation,source_high_water,invalidation_reason) VALUES('session_old',$1,'generation-old',5,NULL)",
            &[&TRANSCRIPT_PROJECTION_VERSION_V1],
        )
        .expect("seed old head");
    setup
        .execute(
            "INSERT INTO runtime.transcript_projection_current_generations(session_id,projection_version,projection_generation,source_high_water) VALUES('session_old',$1,'generation-old',5)",
            &[&TRANSCRIPT_PROJECTION_VERSION_V1],
        )
        .expect("seed old current generation");

    let current = store
        .load_or_initialize_current_transcript_generation("session_old")
        .expect("load current generation");
    assert_eq!(current.projection_generation, "generation-1");

    let old_heads: i64 = setup
        .query_one(
            "SELECT count(*) FROM runtime.transcript_projection_heads WHERE session_id='session_old' AND projection_generation='generation-old'",
            &[],
        )
        .expect("count old heads")
        .get(0);
    assert_eq!(old_heads, 0);
    let new_heads: i64 = setup
        .query_one(
            "SELECT count(*) FROM runtime.transcript_projection_heads WHERE session_id='session_old' AND projection_generation='generation-1'",
            &[],
        )
        .expect("count new heads")
        .get(0);
    assert_eq!(new_heads, 1);
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_fenced_rewrite_tombstones_completed_dispatch_request() {
    use centaeris_core::session::{rewrite_last_user_tail_tombstone, RewriteLastUserTailRequest};
    use centaeris_core::tool::{DynamicToolContract, DynamicToolRegistry};

    let _guard = TEST_LOCK.lock().unwrap();
    let url = test_url();
    reset_store(&url);
    let mut audit = Client::connect(&url, NoTls).unwrap();
    audit.batch_execute(
        "DROP TABLE IF EXISTS public.app_core_sessionevent CASCADE;
         DROP TABLE IF EXISTS public.app_core_session CASCADE;
         CREATE TABLE public.app_core_session(id text PRIMARY KEY,workspace_id text NOT NULL);
         CREATE TABLE public.app_core_sessionevent(
             \"eventId\" text PRIMARY KEY,workspace_id text NOT NULL,session_id text NOT NULL,
             agent_run_id text NOT NULL,sequence integer NOT NULL,agent_run_sequence integer,
             session_level boolean NOT NULL DEFAULT false,projects_to_agent_run_stream boolean NOT NULL,
             payload jsonb NOT NULL,\"createdAtMs\" bigint NOT NULL,
             \"insertedAt\" timestamptz NOT NULL DEFAULT clock_timestamp(),
             UNIQUE(session_id,sequence),UNIQUE(agent_run_id,agent_run_sequence));
         INSERT INTO public.app_core_session VALUES('session_work_rewrite','workspace_work_rewrite');",
    ).unwrap();
    let store = PostgresRuntimeStore::new(&url).unwrap();
    let log = super::PostgresSessionLog::new(
        store.ordinary_connections.clone(),
        "workspace_work_rewrite".into(),
        "session_work_rewrite".into(),
        "Dispatch work".into(),
    );
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let contract: DynamicToolContract = serde_json::from_str(include_str!(
        "../../../api/app_core/contracts/dispatch_work.json"
    ))
    .unwrap();
    let registry = DynamicToolRegistry::from_contracts(vec![contract]).unwrap();
    let digest = registry
        .find_contract("dispatch_work")
        .unwrap()
        .contract_digest()
        .unwrap();
    let source_run = "agent_run_work_source";
    let session = "session_work_rewrite";
    let user_message = "message:turn_pg_fenced_terminal:user".to_string();
    let tail_message = "message:turn_pg_fenced_terminal:assistant";
    let record = |sequence, kind, payload| {
        session_record(
            source_run,
            session,
            sequence,
            kind,
            payload,
            sequence as i64,
        )
    };
    let mut source = vec![
        record(
            1,
            SessionRecordType::AgentRunStarted,
            serde_json::json!({"userObjective":"Dispatch work"}),
        ),
        record(
            2,
            SessionRecordType::UserMessage,
            serde_json::json!({"messageId":user_message,"text":"Dispatch work","attachments":[]}),
        ),
        record(
            3,
            SessionRecordType::ToolCall,
            serde_json::json!({
            "callId":"dispatch-one","toolName":"dispatch_work","providerId":"workspace.agent_work",
            "toolContractDigest":digest,"displayTarget":"Private Agent work",
            "normalizedInput":{"objective":"Complete objective","session_refs":[],"file_refs":[]}}),
        ),
        record(
            4,
            SessionRecordType::ToolResult,
            serde_json::json!({
            "callId":"dispatch-one","toolName":"dispatch_work","resultState":"successWithOutput",
            "modelContent":"Request accepted for Session commit","fullOutputPath":null,"outputStartByte":null,
            "outputByteLength":35,"outputComplete":true,"summary":"Request recorded",
            "operations":[],"modelInputImages":[],"latencyMs":0}),
        ),
    ];
    // Core's real planner must reject the incomplete tail, then accept it once
    // its Final and terminal source facts are committed with no file mutations.
    runtime
        .block_on(log.append_session_records(source_run, &source))
        .unwrap();
    let existing = source
        .iter()
        .map(|item| item.event.clone())
        .collect::<Vec<_>>();
    assert!(rewrite_last_user_tail_tombstone(
        &existing,
        session,
        &user_message,
        &user_message,
        "turn_work_replacement",
        "agent_run_work_replacement",
        10,
    )
    .unwrap_err()
    .contains("non-terminal AgentRun"));
    source.extend([
        record(5, SessionRecordType::AssistantMessage, serde_json::json!({
            "messageId":tail_message,"modelMarkdown":"Dispatch request recorded.","artifactRefs":[],"status":"done"})),
        record(6, SessionRecordType::AgentRunCompleted, serde_json::json!({"doneReason":"finalized"})),
    ]);
    runtime
        .block_on(log.append_session_records(source_run, &source[4..]))
        .unwrap();
    let existing = source
        .iter()
        .map(|item| item.event.clone())
        .collect::<Vec<_>>();
    let planned = rewrite_last_user_tail_tombstone(
        &existing,
        session,
        &user_message,
        tail_message,
        "turn_work_replacement",
        "agent_run_work_replacement",
        10,
    )
    .unwrap();
    assert_eq!(
        planned.payload["targetEventIds"].as_array().unwrap().len(),
        6
    );
    for event in &source[2..4] {
        assert!(planned.payload["targetEventIds"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!(event.event.event_id)));
    }
    let replacement_run = "agent_run_work_replacement";
    let replacement_log = super::PostgresSessionLog::new(
        store.ordinary_connections.clone(),
        "workspace_work_rewrite".into(),
        session.into(),
        "Replace input".into(),
    );
    let mut started = session_record(
        replacement_run,
        session,
        2,
        SessionRecordType::AgentRunStarted,
        serde_json::json!({"userObjective":"Replace input"}),
        10,
    );
    started.event.turn_id = Some("turn_work_replacement".into());
    let now = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    let job_id = format!("agent_run.lifecycle:{replacement_run}");
    let mut lifecycle = job(&job_id, "work-rewrite");
    lifecycle.job_kind = "agent_run.lifecycle".into();
    lifecycle.session_id = Some(session.into());
    lifecycle.status = RuntimeJobStatus::Running;
    lifecycle.lease_owner = Some("work-rewrite-owner".into());
    lifecycle.lease_expires_at_ms = Some(now + 60_000);
    store
        .schedule_runtime_job(ScheduleRuntimeJobRequest { job: lifecycle })
        .unwrap();
    let fence = RuntimeJobLeaseFence {
        job_id,
        job_kind: "agent_run.lifecycle".into(),
        lease_owner: "work-rewrite-owner".into(),
    };
    let mut replacement = session_record(
        replacement_run,
        session,
        3,
        SessionRecordType::UserMessage,
        serde_json::json!({"messageId":"message:turn_work_replacement:user","text":"Replace input","attachments":[]}),
        10,
    );
    replacement.event.turn_id = Some("turn_work_replacement".into());
    let rewrite = RewriteLastUserTailRequest {
        target_message_id: user_message,
        expected_tail_message_id: tail_message.into(),
        new_turn_id: "turn_work_replacement".into(),
        new_agent_run_id: replacement_run.into(),
        created_at_ms: 10,
    };
    let mut held = audit.transaction().unwrap();
    held.query_one(
        "SELECT id FROM public.app_core_session WHERE id=$1 FOR UPDATE",
        &[&session],
    )
    .unwrap();
    let rewriting = std::thread::spawn(move || {
        replacement_log.append_rewritten_session_records_with_runtime_job_lease_blocking(
            replacement_run,
            &[started, replacement],
            &rewrite,
            &fence,
        )
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let blocked = loop {
        let blocked: bool = held.query_one(
            "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND pg_backend_pid()=ANY(pg_blocking_pids(pid)))",
            &[],
        ).unwrap().get(0);
        if blocked || std::time::Instant::now() >= deadline {
            break blocked;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    held.commit().unwrap();
    let receipt = rewriting.join().unwrap().unwrap();
    assert!(
        blocked,
        "production rewrite must wait for the source Session lock"
    );
    assert_eq!(
        receipt.records[0].event.event_type,
        SessionRecordType::Tombstone
    );
    assert_eq!(
        receipt.records[0].event.payload["targetEventIds"],
        planned.payload["targetEventIds"]
    );
    let inactive: i64 = audit.query_one(
        "SELECT count(*) FROM public.app_core_sessionevent WHERE agent_run_id=$1 AND NOT projects_to_agent_run_stream", &[&source_run],
    ).unwrap().get(0);
    assert_eq!(inactive, 6);
    println!("completed-dispatch-rewrite-ok: real Core planner and fenced PostgreSQL rewrite invalidate committed call/result after a terminal Final; Session lock serializes replacement");
}
