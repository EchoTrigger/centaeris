use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::json;

use centaeris_core::runtime::contracts::{CheckpointRecord, EventVisibility, RuntimeEvent};
use centaeris_core::session::external_context::{
    ExternalContextObject, ExternalContextObjectLink, ListExternalContextObjectsRequest,
};
use centaeris_core::session::reliability::{
    ClaimDueRuntimeJobsRequest, CompleteRuntimeJobRequest, CreateDeadLetterDisposition,
    CreateDeadLetterRequest, DeadLetterRecord, DeadLetterReplayPolicy, DeadLetterStatus,
    DismissDeadLetterRequest, ListDeadLettersRequest, ListRuntimeJobsRequest, RuntimeBackoffPolicy,
    RuntimeJobRecord, RuntimeJobStatus, ScheduleRuntimeJobDisposition, ScheduleRuntimeJobRequest,
    StartRuntimeJobRequest,
};
use centaeris_core::session::store::{RuntimeStore, RuntimeStoreActor, RuntimeStoreError};
use centaeris_runtime_sqlite::SqliteRuntimeStore;

#[tokio::test]
async fn runtime_store_actor_serializes_core_runtime_store_operations() {
    let store =
        SqliteRuntimeStore::new(temp_db_path("runtime_store_actor")).expect("create runtime store");
    let actor = RuntimeStoreActor::start(store).expect("start runtime store actor");
    let checkpoint = CheckpointRecord {
        checkpoint_id: "checkpoint:actor".to_string(),
        kind: centaeris_core::runtime::contracts::CheckpointKindV1::Wait,
        session_id: "chat-actor".to_string(),
        turn_id: "turn-actor-1".to_string(),
        status: "running".to_string(),
        done_reason: None,
        updated_at_ms: 2,
        payload_json: json!({"ok": true}).to_string(),
    };
    actor
        .save_checkpoint(checkpoint.clone())
        .await
        .expect("save checkpoint");
    let loaded_checkpoint = actor
        .load_checkpoint_by_turn("chat-actor", "turn-actor-1")
        .await
        .expect("load checkpoint")
        .expect("checkpoint exists");
    assert_eq!(loaded_checkpoint.payload_json, checkpoint.payload_json);

    actor
        .append_event(RuntimeEvent {
            event_id: "event-actor-1".to_string(),
            session_id: "chat-actor".to_string(),
            task_id: Some("task-actor-1".to_string()),
            event_type: "session_event".to_string(),
            at_ms: 3,
            visibility: EventVisibility::User,
            payload_json: json!({"type": "status"}).to_string(),
        })
        .await
        .expect("append event");
    let events = actor
        .list_events("chat-actor", 10, 0)
        .await
        .expect("list events");
    assert_eq!(events.len(), 1);
}

#[test]
fn runtime_store_actor_requires_tokio_runtime() {
    let store = SqliteRuntimeStore::new(temp_db_path("runtime_store_actor_no_runtime"))
        .expect("create runtime store");
    let error = match RuntimeStoreActor::start(store) {
        Ok(_) => panic!("start outside tokio must fail"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        RuntimeStoreError::ActorRuntimeUnavailable { .. }
    ));
}

#[test]
fn runtime_store_actor_sync_trait_proxy_round_trips_checkpoint() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime");
    runtime.block_on(async {
        let store = SqliteRuntimeStore::new(temp_db_path("runtime_store_actor_sync_proxy"))
            .expect("create runtime store");
        let actor = RuntimeStoreActor::start(store).expect("start runtime store actor");

        let actor_for_blocking = actor.clone();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let result = RuntimeStore::save_checkpoint(
                &actor_for_blocking,
                CheckpointRecord {
                    checkpoint_id: "checkpoint:sync-proxy".to_string(),
                    kind: centaeris_core::runtime::contracts::CheckpointKindV1::Wait,
                    session_id: "chat-sync-proxy".to_string(),
                    turn_id: "turn-sync-proxy-1".to_string(),
                    status: "running".to_string(),
                    done_reason: None,
                    updated_at_ms: 1,
                    payload_json: "{}".to_string(),
                },
            );
            done_tx.send(result).expect("send sync proxy result");
        });

        loop {
            match done_rx.try_recv() {
                Ok(result) => {
                    result.expect("sync proxy write");
                    break;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    tokio::task::yield_now().await;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    panic!("sync proxy thread disconnected");
                }
            }
        }

        let loaded = actor
            .load_checkpoint_by_turn("chat-sync-proxy", "turn-sync-proxy-1")
            .await
            .expect("load checkpoint")
            .expect("checkpoint exists");
        assert_eq!(loaded.session_id, "chat-sync-proxy");
    });
}

#[test]
fn runtime_store_actor_sync_trait_proxy_handles_parallel_threads() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime");
    runtime.block_on(async {
        let store = SqliteRuntimeStore::new(temp_db_path("runtime_store_actor_sync_parallel"))
            .expect("create runtime store");
        let actor = RuntimeStoreActor::start(store).expect("start runtime store actor");
        let (done_tx, done_rx) = std::sync::mpsc::channel();

        for idx in 0..16 {
            let actor_for_blocking = actor.clone();
            let done_tx = done_tx.clone();
            std::thread::spawn(move || {
                let result = RuntimeStore::save_checkpoint(
                    &actor_for_blocking,
                    CheckpointRecord {
                        checkpoint_id: format!("checkpoint:sync-parallel-{idx:02}"),
                        kind: centaeris_core::runtime::contracts::CheckpointKindV1::Wait,
                        session_id: "chat-sync-parallel".to_string(),
                        turn_id: format!("turn-sync-parallel-{idx:02}"),
                        status: "running".to_string(),
                        done_reason: None,
                        updated_at_ms: i64::from(idx),
                        payload_json: "{}".to_string(),
                    },
                );
                done_tx.send(result).expect("send sync proxy result");
            });
        }
        drop(done_tx);

        let mut completed = 0;
        while completed < 16 {
            match done_rx.try_recv() {
                Ok(result) => {
                    result.expect("sync proxy parallel write");
                    completed += 1;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    tokio::task::yield_now().await;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    panic!("sync proxy parallel threads disconnected");
                }
            }
        }

        assert_eq!(
            actor
                .list_checkpoints("chat-sync-parallel", 32, 0)
                .await
                .expect("list sync proxy checkpoints")
                .len(),
            16
        );
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runtime_store_actor_sync_trait_proxy_does_not_starve_actor_from_async_context() {
    let store = SqliteRuntimeStore::new(temp_db_path("runtime_store_actor_sync_async_context"))
        .expect("create runtime store");
    let actor = RuntimeStoreActor::start(store).expect("start runtime store actor");
    let actor_for_task = actor.clone();
    let join = tokio::spawn(async move {
        RuntimeStore::save_checkpoint(
            &actor_for_task,
            CheckpointRecord {
                checkpoint_id: "checkpoint:sync-async-context".to_string(),
                kind: centaeris_core::runtime::contracts::CheckpointKindV1::Wait,
                session_id: "chat-sync-async-context".to_string(),
                turn_id: "turn-sync-async-context-1".to_string(),
                status: "running".to_string(),
                done_reason: None,
                updated_at_ms: 1,
                payload_json: "{}".to_string(),
            },
        )
    });

    tokio::time::timeout(Duration::from_secs(5), join)
        .await
        .expect("sync proxy write should not starve actor")
        .expect("join sync proxy write")
        .expect("sync proxy write");

    let loaded = actor
        .load_checkpoint_by_turn("chat-sync-async-context", "turn-sync-async-context-1")
        .await
        .expect("load checkpoint")
        .expect("checkpoint exists");
    assert_eq!(loaded.session_id, "chat-sync-async-context");
}

#[tokio::test]
async fn runtime_store_actor_sync_trait_proxy_rejects_current_thread_runtime() {
    let store = SqliteRuntimeStore::new(temp_db_path("runtime_store_actor_current_thread"))
        .expect("create runtime store");
    let actor = RuntimeStoreActor::start(store).expect("start runtime store actor");

    let error = RuntimeStore::load_latest_checkpoint(&actor, "chat-current-thread")
        .expect_err("sync API must reject a current-thread runtime");
    assert_eq!(error, RuntimeStoreError::InvalidRuntimeContext);

    assert!(actor
        .load_latest_checkpoint("chat-current-thread")
        .await
        .expect("async API remains available")
        .is_none());
}

#[tokio::test]
async fn runtime_store_actor_owns_runtime_jobs_and_external_context_ports() {
    let store = SqliteRuntimeStore::new(temp_db_path("runtime_store_actor_ports"))
        .expect("create runtime store");
    let actor = RuntimeStoreActor::start(store).expect("start runtime store actor");

    let job = RuntimeJobRecord {
        job_id: "job-actor-1".to_string(),
        job_kind: "test.job".to_string(),
        status: RuntimeJobStatus::Queued,
        run_at_ms: 10,
        lease_owner: None,
        lease_expires_at_ms: None,
        heartbeat_at_ms: None,
        retry_count: 0,
        max_retries: 1,
        backoff_policy: RuntimeBackoffPolicy::default(),
        idempotency_key: "test.job:actor-1".to_string(),
        session_id: Some("chat-actor-port".to_string()),
        branch_id: Some("turn-actor-port".to_string()),
        checkpoint_id: None,
        payload_ref: None,
        output_refs: vec![],
        last_error: None,
        created_at_ms: 1,
        updated_at_ms: 1,
    };
    let scheduled = actor
        .schedule_runtime_job(ScheduleRuntimeJobRequest { job })
        .await
        .expect("schedule runtime job");
    assert_eq!(
        scheduled.disposition,
        ScheduleRuntimeJobDisposition::Inserted
    );

    let listed = actor
        .list_runtime_jobs(ListRuntimeJobsRequest {
            statuses: vec![RuntimeJobStatus::Queued],
            job_kind: Some("test.job".to_string()),
            session_id: Some("chat-actor-port".to_string()),
            branch_id: None,
            limit: 10,
            offset: 0,
        })
        .await
        .expect("list runtime jobs");
    assert_eq!(listed.len(), 1);

    let claimed = actor
        .claim_due_runtime_jobs(ClaimDueRuntimeJobsRequest {
            now_ms: 11,
            worker_id: "actor-worker".to_string(),
            job_id: None,
            job_kind: Some("test.job".to_string()),
            session_id: Some("chat-actor-port".to_string()),
            limit: 1,
            lease_ms: 100,
        })
        .await
        .expect("claim runtime job");
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].lease_owner.as_deref(), Some("actor-worker"));

    actor
        .start_runtime_job(StartRuntimeJobRequest {
            job_id: "job-actor-1".to_string(),
            lease_owner: "actor-worker".to_string(),
            started_at_ms: 12,
        })
        .await
        .expect("start runtime job");
    actor
        .complete_runtime_job(CompleteRuntimeJobRequest {
            job_id: "job-actor-1".to_string(),
            lease_owner: "actor-worker".to_string(),
            output_refs: vec!["ref:done".to_string()],
            completed_at_ms: 13,
        })
        .await
        .expect("complete runtime job");
    let completed = actor
        .get_runtime_job("job-actor-1")
        .await
        .expect("get runtime job")
        .expect("job exists");
    assert_eq!(completed.status, RuntimeJobStatus::Succeeded);
    assert_eq!(completed.output_refs, vec!["ref:done"]);

    let dead_letter = DeadLetterRecord {
        dead_letter_id: "dead-letter-actor-1".to_string(),
        original_job_id: "job-actor-1".to_string(),
        job_kind: "test.job".to_string(),
        status: DeadLetterStatus::Open,
        session_id: Some("chat-actor-port".to_string()),
        branch_id: Some("turn-actor-port".to_string()),
        checkpoint_id: None,
        payload_ref: None,
        idempotency_key: "test.job:actor-1".to_string(),
        failure_reason: "test_failure".to_string(),
        last_error: "failed by test".to_string(),
        attempts: 1,
        first_failed_at_ms: 14,
        last_failed_at_ms: 14,
        replay_policy: DeadLetterReplayPolicy::default(),
        replayed_job_id: None,
        dismissed_by: None,
        dismissed_reason: None,
        updated_at_ms: 14,
    };
    let created = actor
        .create_dead_letter(CreateDeadLetterRequest {
            dead_letter: dead_letter.clone(),
        })
        .await
        .expect("create dead letter");
    assert_eq!(created.disposition, CreateDeadLetterDisposition::Inserted);
    let listed_dead_letters = actor
        .list_dead_letters(ListDeadLettersRequest {
            statuses: vec![DeadLetterStatus::Open],
            job_kind: Some("test.job".to_string()),
            session_id: Some("chat-actor-port".to_string()),
            branch_id: None,
            limit: 10,
            offset: 0,
        })
        .await
        .expect("list dead letters");
    assert_eq!(listed_dead_letters.len(), 1);
    let loaded_dead_letter = actor
        .get_dead_letter("dead-letter-actor-1")
        .await
        .expect("get dead letter")
        .expect("dead letter exists");
    assert_eq!(loaded_dead_letter.status, DeadLetterStatus::Open);
    actor
        .dismiss_dead_letter(DismissDeadLetterRequest {
            dead_letter_id: "dead-letter-actor-1".to_string(),
            dismissed_by: "runtime-store-actor-test".to_string(),
            dismissed_reason: "port coverage".to_string(),
            updated_at_ms: 15,
        })
        .await
        .expect("dismiss dead letter");
    let dismissed_dead_letter = actor
        .get_dead_letter("dead-letter-actor-1")
        .await
        .expect("get dismissed dead letter")
        .expect("dead letter exists");
    assert_eq!(dismissed_dead_letter.status, DeadLetterStatus::Dismissed);

    actor
        .upsert_external_context_object(ExternalContextObject {
            schema_version: "external_context.v1".to_string(),
            object_id: "object-actor-1".to_string(),
            object_kind: "subagentWorkPacket".to_string(),
            source_provider_id: "test-provider".to_string(),
            source_tool_name: "test-tool".to_string(),
            title: "Actor object".to_string(),
            content: "{\"ok\":true}".to_string(),
            metadata: json!({"owner": "actor"}),
            updated_at_ms: 20,
        })
        .await
        .expect("upsert external context object");
    actor
        .link_external_context_object(ExternalContextObjectLink {
            session_id: "chat-actor-port".to_string(),
            turn_id: Some("turn-actor-port".to_string()),
            tool_call_id: Some("tool-actor-port".to_string()),
            object_id: "object-actor-1".to_string(),
            source_provider_id: "test-provider".to_string(),
            source_tool_name: "test-tool".to_string(),
            linked_at_ms: 21,
        })
        .await
        .expect("link external context object");
    let object = actor
        .load_external_context_object("object-actor-1")
        .await
        .expect("load external context object")
        .expect("object exists");
    assert_eq!(object.title, "Actor object");
    let linked = actor
        .list_external_context_objects(ListExternalContextObjectsRequest {
            session_id: Some("chat-actor-port".to_string()),
            limit: 10,
            offset: 0,
        })
        .await
        .expect("list linked external context objects");
    assert_eq!(linked.len(), 1);
    assert_eq!(linked[0].object_id, "object-actor-1");
}

#[tokio::test]
async fn runtime_store_actor_handles_parallel_writes_without_sqlite_lock_errors() {
    let store = SqliteRuntimeStore::new(temp_db_path("runtime_store_actor_parallel"))
        .expect("create runtime store");
    let actor = RuntimeStoreActor::start(store).expect("start runtime store actor");
    let mut joins = tokio::task::JoinSet::new();

    for idx in 0..32 {
        let actor = actor.clone();
        joins.spawn(async move {
            let task_id = format!("task-parallel-{idx:02}");
            let turn_id = format!("turn-parallel-{idx:02}");
            actor
                .save_checkpoint(CheckpointRecord {
                    checkpoint_id: format!("checkpoint:parallel-{idx}"),
                    kind: centaeris_core::runtime::contracts::CheckpointKindV1::Wait,
                    session_id: "chat-parallel".to_string(),
                    turn_id: turn_id.clone(),
                    status: "running".to_string(),
                    done_reason: None,
                    updated_at_ms: i64::from(idx),
                    payload_json: json!({ "idx": idx }).to_string(),
                })
                .await
                .map_err(|error| error.to_string())?;
            actor
                .append_event(RuntimeEvent {
                    event_id: format!("event-parallel-{idx:02}"),
                    session_id: "chat-parallel".to_string(),
                    task_id: Some(task_id),
                    event_type: "session_event".to_string(),
                    at_ms: i64::from(idx),
                    visibility: EventVisibility::Internal,
                    payload_json: json!({ "turnId": turn_id }).to_string(),
                })
                .await
                .map_err(|error| error.to_string())?;
            Ok::<(), String>(())
        });
    }

    while let Some(result) = joins.join_next().await {
        result
            .expect("join parallel writer")
            .expect("parallel writer");
    }

    assert_eq!(
        actor
            .list_checkpoints("chat-parallel", 64, 0)
            .await
            .expect("list parallel checkpoints")
            .len(),
        32
    );
    assert_eq!(
        actor
            .list_events("chat-parallel", 64, 0)
            .await
            .expect("list parallel events")
            .len(),
        32
    );
}

fn temp_db_path(suffix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock moved backwards")
        .as_nanos();
    std::env::temp_dir().join(format!("centaeris_{suffix}_{nanos}.sqlite3"))
}
