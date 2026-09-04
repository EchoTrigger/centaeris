use centaeris_core::runtime::contracts::TimestampMs;
use centaeris_core::runtime::keys::external_context as runtime_external_context_keys;
use centaeris_core::runtime::subagent::*;
use centaeris_core::session::external_context::{
    ExternalContextObject, ExternalContextStorePort, EXTERNAL_CONTEXT_SCHEMA_VERSION,
};
use centaeris_core::session::reliability::{
    CancelRuntimeJobRequest, ListRuntimeJobsRequest, RuntimeJobStatus, RuntimeJobStorePort,
    ScheduleRuntimeJobDisposition, StartRuntimeJobRequest,
};
use centaeris_core::session::store::RuntimeStoreActor;
use centaeris_runtime_sqlite::SqliteRuntimeStore;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Debug)]
struct RecordingRunner {
    outcome: SubagentWorkerRunOutcome,
}

#[derive(Debug)]
struct DelayedRunner {
    delay: Duration,
    outcome: SubagentWorkerRunOutcome,
}

#[derive(Debug, Clone)]
struct RecordingObserver {
    events: Arc<Mutex<Vec<SubagentLifecycleHookEvent>>>,
    fail_on_start: bool,
    fail_on_stop: bool,
}

impl AsyncSubagentLifecycleObserver for RecordingObserver {
    fn on_subagent_start<'a>(
        &'a self,
        event: SubagentLifecycleHookEvent,
    ) -> SubagentLifecycleObserverFuture<'a> {
        Box::pin(async move {
            if self.fail_on_start {
                return Err("forced subagent start observer failure".to_string());
            }
            self.events.lock().expect("observer lock").push(event);
            Ok(())
        })
    }

    fn on_subagent_stop<'a>(
        &'a self,
        event: SubagentLifecycleHookEvent,
    ) -> SubagentLifecycleObserverFuture<'a> {
        Box::pin(async move {
            if self.fail_on_stop {
                return Err("forced subagent stop observer failure".to_string());
            }
            self.events.lock().expect("observer lock").push(event);
            Ok(())
        })
    }
}

impl AsyncSubagentWorkerRunner for RecordingRunner {
    fn run_async<'a>(&'a self, req: SubagentWorkerRunRequest) -> SubagentWorkerRunFuture<'a> {
        Box::pin(async move {
            assert!(req.work_packet.content_json.get("workPacket").is_some());
            self.outcome.clone()
        })
    }
}

impl AsyncSubagentWorkerRunner for DelayedRunner {
    fn run_async<'a>(&'a self, req: SubagentWorkerRunRequest) -> SubagentWorkerRunFuture<'a> {
        Box::pin(async move {
            assert!(req.work_packet.content_json.get("workPacket").is_some());
            tokio::time::sleep(self.delay).await;
            self.outcome.clone()
        })
    }
}

#[derive(Debug)]
struct FlaggingRunner {
    called: Arc<AtomicBool>,
}

impl AsyncSubagentWorkerRunner for FlaggingRunner {
    fn run_async<'a>(&'a self, req: SubagentWorkerRunRequest) -> SubagentWorkerRunFuture<'a> {
        Box::pin(async move {
            assert!(req.work_packet.content_json.get("workPacket").is_some());
            self.called.store(true, Ordering::SeqCst);
            SubagentWorkerRunOutcome::Succeeded {
                summary: "done".to_string(),
                output_refs: vec![],
            }
        })
    }
}

struct CancellingRunner {
    store: RuntimeStoreActor,
    saw_running: Arc<AtomicBool>,
    reason: String,
    cancelled_at_ms: TimestampMs,
}

impl AsyncSubagentWorkerRunner for CancellingRunner {
    fn run_async<'a>(&'a self, req: SubagentWorkerRunRequest) -> SubagentWorkerRunFuture<'a> {
        Box::pin(async move {
            assert!(req.work_packet.content_json.get("workPacket").is_some());
            self.saw_running.store(
                req.job.status == RuntimeJobStatus::Running,
                Ordering::SeqCst,
            );
            self.store
                .cancel_runtime_job(CancelRuntimeJobRequest {
                    job_id: req.job.job_id,
                    reason: self.reason.clone(),
                    cancelled_at_ms: self.cancelled_at_ms,
                    expected_status: None,
                })
                .await
                .expect("cancel running subagent job");
            tokio::time::sleep(Duration::from_millis(2_000)).await;
            SubagentWorkerRunOutcome::Succeeded {
                summary: "runner finished after cancellation".to_string(),
                output_refs: vec!["checkpoint:should_not_complete".to_string()],
            }
        })
    }
}

#[derive(Debug)]
struct ConcurrentRecordingRunner {
    active: Arc<AtomicUsize>,
    max_active: Arc<AtomicUsize>,
    sleep_ms: u64,
    start_barrier: Option<Arc<tokio::sync::Barrier>>,
}

impl AsyncSubagentWorkerRunner for ConcurrentRecordingRunner {
    fn run_async<'a>(&'a self, req: SubagentWorkerRunRequest) -> SubagentWorkerRunFuture<'a> {
        Box::pin(async move {
            assert!(req.work_packet.content_json.get("workPacket").is_some());
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            loop {
                let previous = self.max_active.load(Ordering::SeqCst);
                if active <= previous {
                    break;
                }
                if self
                    .max_active
                    .compare_exchange(previous, active, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
                {
                    break;
                }
            }
            if let Some(barrier) = &self.start_barrier {
                barrier.wait().await;
            }
            tokio::time::sleep(Duration::from_millis(self.sleep_ms)).await;
            self.active.fetch_sub(1, Ordering::SeqCst);
            SubagentWorkerRunOutcome::Succeeded {
                summary: "done".to_string(),
                output_refs: vec![],
            }
        })
    }
}

#[derive(Debug)]
struct SelectivePanicRunner {
    panic_subagent_id_fragment: String,
}

impl AsyncSubagentWorkerRunner for SelectivePanicRunner {
    fn run_async<'a>(&'a self, req: SubagentWorkerRunRequest) -> SubagentWorkerRunFuture<'a> {
        Box::pin(async move {
            assert_eq!(req.job.status, RuntimeJobStatus::Running);
            if req
                .lifecycle
                .subagent_id
                .contains(self.panic_subagent_id_fragment.as_str())
            {
                panic!("intentional subagent worker panic");
            }
            SubagentWorkerRunOutcome::Succeeded {
                summary: "worker completed".to_string(),
                output_refs: vec!["checkpoint:worker-ok".to_string()],
            }
        })
    }
}

fn enqueue_and_claim(
    store: &SqliteRuntimeStore,
    session_id: &str,
    parent_turn_id: &str,
    tool_call_id: &str,
    worker_id: &str,
) -> ClaimedSubagentRunJob {
    enqueue_subagent_run_job(
        store,
        SubagentRunJobRequest {
            session_id: session_id.to_string(),
            parent_turn_id: parent_turn_id.to_string(),
            tool_call_id: tool_call_id.to_string(),
            subagent_id: format!("subagent:{parent_turn_id}:{tool_call_id}"),
            work_packet_ref: format!("external_context:{tool_call_id}:work_packet"),
            checkpoint_id: Some(format!("checkpoint:{tool_call_id}")),
            run_at_ms: 1_000,
            created_at_ms: 900,
            max_retries: 3,
        },
    )
    .expect("enqueue subagent job");
    claim_subagent_run_jobs(
        store,
        ClaimSubagentRunJobsRequest {
            now_ms: 1_001,
            worker_id: worker_id.to_string(),
            session_id: None,
            limit: 1,
            lease_ms: 5_000,
        },
    )
    .expect("claim subagent jobs")
    .into_iter()
    .next()
    .expect("claimed subagent job")
}

fn temp_db_path(name: &str) -> PathBuf {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time after unix epoch")
        .as_millis();
    std::env::temp_dir().join(format!("centaeris_{name}_{millis}.db"))
}

fn invalid_resource_claim_object(object_id: &str) -> ExternalContextObject {
    ExternalContextObject {
        schema_version: EXTERNAL_CONTEXT_SCHEMA_VERSION.to_string(),
        object_id: object_id.to_string(),
        object_kind: "subagentWorkPacket".to_string(),
        source_provider_id: "runtime".to_string(),
        source_tool_name: "SubagentScheduler".to_string(),
        title: "invalid worker packet".to_string(),
        content: serde_json::json!({
        "workPacket": {
        "taskBrief": {}
        }
        })
        .to_string(),
        metadata: serde_json::json!({}),
        updated_at_ms: 900,
    }
}

fn store_valid_subagent_work_packet(
    store: &SqliteRuntimeStore,
    object_id: &str,
    resource_key: &str,
    context_mode: &str,
    tool_names: Vec<String>,
    writable_path_prefixes: Vec<String>,
) {
    let delegated_tool_contracts = centaeris_core::tool::list_tool_contracts()
        .into_iter()
        .filter(|contract| tool_names.contains(&contract.name))
        .map(|contract| {
            serde_json::json!({
                "name": contract.name,
                "contractDigest": contract.contract_digest().expect("delegated tool digest"),
                "providerId": contract.provider_id.expect("delegated tool provider"),
                "concurrencySafe": contract.concurrency_safe,
            })
        })
        .collect::<Vec<_>>();
    let cwd = std::env::temp_dir().to_string_lossy().into_owned();
    let packet = serde_json::json!({
        "run_context": {
            "schema": "agent_run_context_v1",
            "sessionId": "chat-resource-claim",
            "branchId": "turn-resource-claim",
            "turnId": "turn-resource-claim:subagent-test",
            "agentRunId": "agent-run-subagent-resource-claim",
            "agentRef": {
                "agentId": "subagent-resource-claim",
                "agentRunId": "agent-run-subagent-resource-claim"
            },
            "parentAgentRef": {
                "agentId": "main-agent",
                "agentRunId": "agent-run-main-turn-resource-claim"
            },
            "parentTurnId": "turn-resource-claim",
            "depth": 1,
            "cwd": cwd,
            "cancellationScopeId": "cancel:agent-run-subagent-resource-claim",
            "parentCancellationScopeId": "cancel:agent-run-main-turn-resource-claim",
            "cancelOnParentCancel": true,
            "createdAtMs": 900
        },
        "task_brief": {
            "task_id": null,
            "objective": "inspect resource boundary",
            "success_criteria": [],
            "constraints": [],
            "output_hint": null
        },
        "hot_view": {
            "summary": "",
            "recent_message_ids": [],
            "state_kv": { "subagentResourceKey": resource_key }
        },
        "object_refs": [],
        "allowedTools": tool_names,
        "delegatedToolContracts": delegated_tool_contracts,
        "writablePathPrefixes": writable_path_prefixes,
        "output_contract": {
            "response_mode": "subagent_bounded_result",
            "expected_sections": ["summary"],
            "require_artifact_refs": false,
            "max_summary_chars": 2400
        },
        "parent_checkpoint_id": "checkpoint-resource-claim",
        "context_mode": context_mode
    });
    store
        .upsert_external_context_object(ExternalContextObject {
            schema_version: EXTERNAL_CONTEXT_SCHEMA_VERSION.to_string(),
            object_id: object_id.to_string(),
            object_kind: "subagentWorkPacket".to_string(),
            source_provider_id: "runtime".to_string(),
            source_tool_name: "SubagentScheduler".to_string(),
            title: "worker packet resource claim".to_string(),
            content: serde_json::json!({ "workPacket": packet }).to_string(),
            metadata: serde_json::json!({}),
            updated_at_ms: 900,
        })
        .expect("store work packet object");
}

#[test]
fn enqueue_subagent_run_job_is_idempotent_in_runtime_jobs() {
    let db_path = temp_db_path("enqueue_subagent_run_job");
    let store = SqliteRuntimeStore::new(&db_path).expect("create sqlite runtime store");
    let req = SubagentRunJobRequest {
        session_id: "chat-queue".to_string(),
        parent_turn_id: "turn-queue".to_string(),
        tool_call_id: "tool-queue".to_string(),
        subagent_id: "subagent:turn-queue:tool-queue".to_string(),
        work_packet_ref: "external_context:work_packet_queue".to_string(),
        checkpoint_id: Some("checkpoint-queue".to_string()),
        run_at_ms: 1_000,
        created_at_ms: 900,
        max_retries: 3,
    };

    let first = enqueue_subagent_run_job(&store, req.clone()).expect("enqueue first job");
    let second = enqueue_subagent_run_job(&store, req).expect("enqueue duplicate job");

    assert_eq!(first.disposition, ScheduleRuntimeJobDisposition::Inserted);
    assert_eq!(second.disposition, ScheduleRuntimeJobDisposition::Existing);
    assert_eq!(first.job.job_id, second.job.job_id);

    let listed = store
        .list_runtime_jobs(ListRuntimeJobsRequest {
            statuses: vec![RuntimeJobStatus::Queued],
            job_kind: Some(SUBAGENT_RUN_JOB_KIND.to_string()),
            session_id: Some("chat-queue".to_string()),
            branch_id: Some("turn-queue".to_string()),
            limit: 10,
            offset: 0,
        })
        .expect("list subagent runtime jobs");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].job_kind, SUBAGENT_RUN_JOB_KIND);
    assert_eq!(
        listed[0].payload_ref.as_deref(),
        Some("external_context:work_packet_queue")
    );

    drop(store);
    let _ = std::fs::remove_file(db_path);
}

#[test]
fn claim_subagent_run_jobs_leases_runtime_jobs_and_projects_lifecycle() {
    let db_path = temp_db_path("claim_subagent_run_jobs");
    let store = SqliteRuntimeStore::new(&db_path).expect("create sqlite runtime store");
    enqueue_subagent_run_job(
        &store,
        SubagentRunJobRequest {
            session_id: "chat-claim".to_string(),
            parent_turn_id: "turn-claim".to_string(),
            tool_call_id: "tool-claim".to_string(),
            subagent_id: "subagent:turn-claim:tool-claim".to_string(),
            work_packet_ref: "external_context:work_packet_claim".to_string(),
            checkpoint_id: Some("checkpoint-claim".to_string()),
            run_at_ms: 1_000,
            created_at_ms: 900,
            max_retries: 3,
        },
    )
    .expect("enqueue subagent job");

    let claimed = claim_subagent_run_jobs(
        &store,
        ClaimSubagentRunJobsRequest {
            now_ms: 1_001,
            worker_id: "worker-1".to_string(),
            session_id: None,
            limit: 4,
            lease_ms: 90,
        },
    )
    .expect("claim subagent jobs");

    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].job.status, RuntimeJobStatus::Leased);
    assert_eq!(claimed[0].job.lease_owner.as_deref(), Some("worker-1"));
    assert_eq!(claimed[0].lifecycle.status, SubagentLifecycleStatus::Leased);
    assert_eq!(
        claimed[0].lifecycle.subagent_id,
        "subagent:turn-claim:tool-claim"
    );
    assert_eq!(claimed[0].event.kind, SubagentSchedulerEventKind::Claimed);
    assert_eq!(
        claimed[0].event.child_session_id,
        "session-subagent:turn-claim:tool-claim"
    );
    assert_eq!(claimed[0].event.worker_id.as_deref(), Some("worker-1"));

    let (running, running_event) =
        build_running_lifecycle_record(&claimed[0].job, "worker-1", 1_002)
            .expect("build running lifecycle");
    assert_eq!(running.status, SubagentLifecycleStatus::Running);
    assert_eq!(running_event.kind, SubagentSchedulerEventKind::Running);

    drop(store);
    let _ = std::fs::remove_file(db_path);
}

#[test]
fn complete_fail_and_cancel_subagent_run_jobs_project_scheduler_events() {
    let complete_db_path = temp_db_path("complete_subagent_run_job");
    let complete_store =
        SqliteRuntimeStore::new(&complete_db_path).expect("create sqlite runtime store");
    let complete_claimed = enqueue_and_claim(
        &complete_store,
        "chat-complete",
        "turn-complete",
        "tool-complete",
        "worker-complete",
    );
    let completed = complete_subagent_run_job(
        &complete_store,
        CompleteSubagentRunJobRequest {
            job_id: complete_claimed.job.job_id.clone(),
            lease_owner: "worker-complete".to_string(),
            output_refs: vec!["external_context:subagent_result".to_string()],
            completed_at_ms: 2_000,
        },
    )
    .expect("complete subagent job");
    assert_eq!(completed.kind, SubagentSchedulerEventKind::Succeeded);
    assert_eq!(completed.status, SubagentLifecycleStatus::Succeeded);
    let completed_job = complete_store
        .get_runtime_job(complete_claimed.job.job_id.as_str())
        .expect("get completed job")
        .expect("completed job exists");
    assert_eq!(completed_job.status, RuntimeJobStatus::Succeeded);
    assert_eq!(
        completed_job.output_refs,
        vec!["external_context:subagent_result".to_string()]
    );
    drop(complete_store);
    let _ = std::fs::remove_file(complete_db_path);

    let fail_db_path = temp_db_path("fail_subagent_run_job");
    let fail_store = SqliteRuntimeStore::new(&fail_db_path).expect("create sqlite runtime store");
    let fail_claimed = enqueue_and_claim(
        &fail_store,
        "chat-fail",
        "turn-fail",
        "tool-fail",
        "worker-fail",
    );
    let failed = fail_subagent_run_job(
        &fail_store,
        FailSubagentRunJobRequest {
            job_id: fail_claimed.job.job_id.clone(),
            lease_owner: "worker-fail".to_string(),
            failed_at_ms: 3_000,
            last_error: "worker failed".to_string(),
            retry: None,
        },
    )
    .expect("fail subagent job");
    assert_eq!(failed.kind, SubagentSchedulerEventKind::Failed);
    assert_eq!(failed.status, SubagentLifecycleStatus::Failed);
    drop(fail_store);
    let _ = std::fs::remove_file(fail_db_path);

    let cancel_db_path = temp_db_path("cancel_subagent_run_job");
    let cancel_store =
        SqliteRuntimeStore::new(&cancel_db_path).expect("create sqlite runtime store");
    let cancel_claimed = enqueue_and_claim(
        &cancel_store,
        "chat-cancel",
        "turn-cancel",
        "tool-cancel",
        "worker-cancel",
    );
    let cancelled = cancel_subagent_run_job(
        &cancel_store,
        CancelSubagentRunJobRequest {
            job_id: cancel_claimed.job.job_id.clone(),
            reason: "user_cancelled".to_string(),
            cancelled_at_ms: 4_000,
        },
    )
    .expect("cancel subagent job");
    assert_eq!(cancelled.kind, SubagentSchedulerEventKind::Cancelled);
    assert_eq!(cancelled.status, SubagentLifecycleStatus::Cancelled);
    drop(cancel_store);
    let _ = std::fs::remove_file(cancel_db_path);
}

#[test]
fn retrying_failed_subagent_run_job_requeues_runtime_job() {
    let db_path = temp_db_path("retry_subagent_run_job");
    let store = SqliteRuntimeStore::new(&db_path).expect("create sqlite runtime store");
    let claimed = enqueue_and_claim(
        &store,
        "chat-retry",
        "turn-retry",
        "tool-retry",
        "worker-retry",
    );

    let event = fail_subagent_run_job(
        &store,
        FailSubagentRunJobRequest {
            job_id: claimed.job.job_id.clone(),
            lease_owner: "worker-retry".to_string(),
            failed_at_ms: 5_000,
            last_error: "temporary failure".to_string(),
            retry: Some(SubagentRunRetry {
                next_run_at_ms: 6_000,
            }),
        },
    )
    .expect("requeue subagent job");

    assert_eq!(event.kind, SubagentSchedulerEventKind::Requeued);
    assert_eq!(event.status, SubagentLifecycleStatus::Queued);
    let job = store
        .get_runtime_job(claimed.job.job_id.as_str())
        .expect("get retried job")
        .expect("retried job exists");
    assert_eq!(job.status, RuntimeJobStatus::Queued);
    assert_eq!(job.run_at_ms, 6_000);
    assert_eq!(job.retry_count, 1);

    drop(store);
    let _ = std::fs::remove_file(db_path);
}

#[tokio::test]
async fn run_claimed_subagent_job_async_loads_work_packet_and_completes() {
    let db_path = temp_db_path("run_claimed_subagent_job_async");
    let store = SqliteRuntimeStore::new(&db_path).expect("create sqlite runtime store");
    let work_packet_ref = runtime_external_context_keys::subagent_work_packet_ref("run");
    store_valid_subagent_work_packet(
        &store,
        work_packet_ref.as_str(),
        "workspace:test",
        "Borrow",
        vec!["read".to_string()],
        vec![],
    );
    enqueue_subagent_run_job(
        &store,
        SubagentRunJobRequest {
            session_id: "chat-run".to_string(),
            parent_turn_id: "turn-run:2".to_string(),
            tool_call_id: "call-run:tool-run".to_string(),
            subagent_id: "subagent:turn-run:tool-run".to_string(),
            work_packet_ref,
            checkpoint_id: Some("checkpoint-run".to_string()),
            run_at_ms: 1_000,
            created_at_ms: 900,
            max_retries: 3,
        },
    )
    .expect("enqueue subagent job");
    let claimed = claim_subagent_run_jobs(
        &store,
        ClaimSubagentRunJobsRequest {
            now_ms: 1_001,
            worker_id: "worker-run".to_string(),
            session_id: None,
            limit: 1,
            lease_ms: 5_000,
        },
    )
    .expect("claim subagent")
    .into_iter()
    .next()
    .expect("claimed subagent job");

    let actor = RuntimeStoreActor::start(store.clone()).expect("start runtime store actor");
    let observer_events = Arc::new(Mutex::new(Vec::new()));
    let observer = RecordingObserver {
        events: observer_events.clone(),
        fail_on_start: false,
        fail_on_stop: false,
    };
    let result = run_claimed_subagent_job_async(
        &actor,
        claimed,
        &DelayedRunner {
            delay: Duration::from_millis(50),
            outcome: SubagentWorkerRunOutcome::Succeeded {
                summary: "done".to_string(),
                output_refs: vec!["external_context:subagent_output".to_string()],
            },
        },
        &observer,
        RunClaimedSubagentJobRequest {
            worker_id: "worker-run".to_string(),
            started_at_ms: 1_002,
            finished_at_ms: 1_100,
        },
    )
    .await
    .expect("run claimed subagent");

    assert_eq!(result.events.len(), 2);
    assert_eq!(result.events[0].kind, SubagentSchedulerEventKind::Running);
    assert_eq!(
        result.events[0].description.as_deref(),
        Some("inspect resource boundary")
    );
    assert_eq!(result.events[0].started_at_ms, Some(1_002));
    assert_eq!(result.events[0].completed_at_ms, None);
    assert_eq!(result.events[1].kind, SubagentSchedulerEventKind::Succeeded);
    assert_eq!(result.events[1].description, None);
    assert_eq!(result.events[1].started_at_ms, None);
    assert_eq!(result.events[1].completed_at_ms, Some(1_100));
    assert_eq!(
        result.final_lifecycle.status,
        SubagentLifecycleStatus::Succeeded
    );
    let expected_result_ref =
        runtime_external_context_keys::subagent_result_ref(result.final_lifecycle.job_id.as_str());
    assert_eq!(
        result.final_lifecycle.result_ref.as_deref(),
        Some(expected_result_ref.as_str())
    );
    let observed = observer_events.lock().expect("observer lock").clone();
    assert_eq!(observed.len(), 2);
    assert_eq!(observed[0].phase, SubagentLifecycleHookPhase::Start);
    assert_eq!(observed[0].subagent_id, "subagent:turn-run:tool-run");
    assert_eq!(
        observed[0].description.as_deref(),
        Some("inspect resource boundary")
    );
    assert_eq!(observed[0].allowed_tools, vec!["read".to_string()]);
    let start_payload = serde_json::to_value(&observed[0]).expect("serialize start hook");
    assert!(start_payload.get("workPacket").is_none());
    assert!(start_payload.get("contentJson").is_none());
    assert_eq!(observed[1].phase, SubagentLifecycleHookPhase::Stop);
    assert_eq!(
        observed[1].status.as_ref(),
        Some(&SubagentLifecycleStatus::Succeeded)
    );
    assert_eq!(observed[1].output_refs, vec![expected_result_ref.clone()]);
    let result_object = store
        .load_external_context_object(expected_result_ref.as_str())
        .expect("load Agent result")
        .expect("Agent result object");
    assert_eq!(result_object.content, "done");
    assert_eq!(result_object.object_kind, "subagent_result");
    assert_eq!(
        result_object.metadata["subagentId"],
        "subagent:turn-run:tool-run"
    );
    assert!(store
        .get_runtime_job(result.final_lifecycle.job_id.as_str())
        .expect("load completed runtime job")
        .expect("completed runtime job")
        .heartbeat_at_ms
        .is_some_and(|heartbeat_at_ms| heartbeat_at_ms > 1_001));

    drop(store);
    let _ = std::fs::remove_file(db_path);
}

#[tokio::test]
async fn run_claimed_subagent_job_async_requires_claim_lease_owner() {
    let db_path = temp_db_path("run_claimed_subagent_job_async_missing_lease");
    let store = SqliteRuntimeStore::new(&db_path).expect("create sqlite runtime store");
    let mut claimed = enqueue_and_claim(
        &store,
        "chat-missing-lease",
        "turn-missing-lease",
        "tool-missing-lease",
        "worker-missing-lease",
    );
    claimed.job.lease_owner = None;

    let actor = RuntimeStoreActor::start(store.clone()).expect("start runtime store actor");
    let err = run_claimed_subagent_job_async(
        &actor,
        claimed,
        &RecordingRunner {
            outcome: SubagentWorkerRunOutcome::Succeeded {
                summary: "should not run".to_string(),
                output_refs: vec![],
            },
        },
        &NoopSubagentLifecycleObserver,
        RunClaimedSubagentJobRequest {
            worker_id: "worker-fallback-must-not-apply".to_string(),
            started_at_ms: 1_002,
            finished_at_ms: 1_100,
        },
    )
    .await
    .expect_err("missing subagent lease owner must fail");

    assert!(err.contains("lease_owner"));
    assert!(err.contains("subagent runtime job"));

    drop(store);
    let _ = std::fs::remove_file(db_path);
}

#[tokio::test]
async fn run_claimed_subagent_job_async_loud_fails_when_start_observer_fails() {
    let db_path = temp_db_path("run_claimed_subagent_job_async_start_observer_fail");
    let store = SqliteRuntimeStore::new(&db_path).expect("create sqlite runtime store");
    let work_packet_ref =
        runtime_external_context_keys::subagent_work_packet_ref("start_observer_fail");
    store
        .upsert_external_context_object(ExternalContextObject {
            schema_version: EXTERNAL_CONTEXT_SCHEMA_VERSION.to_string(),
            object_id: work_packet_ref.clone(),
            object_kind: "subagentWorkPacket".to_string(),
            source_provider_id: "runtime".to_string(),
            source_tool_name: "SubagentScheduler".to_string(),
            title: "worker packet".to_string(),
            content: serde_json::json!({
            "workPacket": {
            "taskBrief": {
            "objective": "inspect docs"
            },
            "allowedTools": ["read"]
            }
            })
            .to_string(),
            metadata: serde_json::json!({}),
            updated_at_ms: 900,
        })
        .expect("store work packet object");
    enqueue_subagent_run_job(
        &store,
        SubagentRunJobRequest {
            session_id: "chat-start-observer-fail".to_string(),
            parent_turn_id: "turn-start-observer-fail".to_string(),
            tool_call_id: "tool-start-observer-fail".to_string(),
            subagent_id: "subagent:start-observer-fail".to_string(),
            work_packet_ref,
            checkpoint_id: None,
            run_at_ms: 1_000,
            created_at_ms: 900,
            max_retries: 3,
        },
    )
    .expect("enqueue subagent job");
    let claimed = claim_subagent_run_jobs(
        &store,
        ClaimSubagentRunJobsRequest {
            now_ms: 1_001,
            worker_id: "worker-start-observer-fail".to_string(),
            session_id: Some("chat-start-observer-fail".to_string()),
            limit: 1,
            lease_ms: 5_000,
        },
    )
    .expect("claim subagent")
    .pop()
    .expect("claimed subagent job");
    let actor = RuntimeStoreActor::start(store.clone()).expect("start runtime store actor");
    let runner_called = Arc::new(AtomicBool::new(false));
    let observer = RecordingObserver {
        events: Arc::new(Mutex::new(Vec::new())),
        fail_on_start: true,
        fail_on_stop: false,
    };

    let error = run_claimed_subagent_job_async(
        &actor,
        claimed.clone(),
        &FlaggingRunner {
            called: runner_called.clone(),
        },
        &observer,
        RunClaimedSubagentJobRequest {
            worker_id: "worker-start-observer-fail".to_string(),
            started_at_ms: 1_010,
            finished_at_ms: 1_100,
        },
    )
    .await
    .expect_err("observer failure must loud-fail current scheduler tick");

    assert!(error.contains("forced subagent start observer failure"));
    assert!(!runner_called.load(Ordering::SeqCst));
    let job = store
        .get_runtime_job(claimed.job.job_id.as_str())
        .expect("get job")
        .expect("job exists");
    assert_eq!(job.status, RuntimeJobStatus::Running);

    drop(store);
    let _ = std::fs::remove_file(db_path);
}

#[tokio::test]
async fn run_claimed_subagent_job_async_honors_running_cancellation_token() {
    let db_path = temp_db_path("run_claimed_subagent_job_async_running_cancel");
    let store = SqliteRuntimeStore::new(&db_path).expect("create sqlite runtime store");
    let work_packet_ref = runtime_external_context_keys::subagent_work_packet_ref("running_cancel");
    store
        .upsert_external_context_object(ExternalContextObject {
            schema_version: EXTERNAL_CONTEXT_SCHEMA_VERSION.to_string(),
            object_id: work_packet_ref.clone(),
            object_kind: "subagentWorkPacket".to_string(),
            source_provider_id: "runtime".to_string(),
            source_tool_name: "SubagentScheduler".to_string(),
            title: "worker packet".to_string(),
            content: serde_json::json!({
            "workPacket": {
            "taskBrief": {
            "objective": "inspect docs"
            }
            }
            })
            .to_string(),
            metadata: serde_json::json!({}),
            updated_at_ms: 900,
        })
        .expect("store work packet object");
    enqueue_subagent_run_job(
        &store,
        SubagentRunJobRequest {
            session_id: "chat-running-cancel".to_string(),
            parent_turn_id: "turn-running-cancel".to_string(),
            tool_call_id: "tool-running-cancel".to_string(),
            subagent_id: "subagent:running-cancel".to_string(),
            work_packet_ref,
            checkpoint_id: None,
            run_at_ms: 1_000,
            created_at_ms: 900,
            max_retries: 3,
        },
    )
    .expect("enqueue subagent job");
    let claimed = claim_subagent_run_jobs(
        &store,
        ClaimSubagentRunJobsRequest {
            now_ms: 1_001,
            worker_id: "worker-running-cancel".to_string(),
            session_id: Some("chat-running-cancel".to_string()),
            limit: 1,
            lease_ms: 5_000,
        },
    )
    .expect("claim subagent jobs")
    .pop()
    .expect("claimed subagent job");
    let saw_running = Arc::new(AtomicBool::new(false));
    let actor = RuntimeStoreActor::start(store.clone()).expect("start runtime store actor");
    let runner = CancellingRunner {
        store: actor.clone(),
        saw_running: saw_running.clone(),
        reason: "user_cancelled_running".to_string(),
        cancelled_at_ms: 1_050,
    };

    let result = run_claimed_subagent_job_async(
        &actor,
        claimed,
        &runner,
        &NoopSubagentLifecycleObserver,
        RunClaimedSubagentJobRequest {
            worker_id: "worker-running-cancel".to_string(),
            started_at_ms: 1_010,
            finished_at_ms: 1_100,
        },
    )
    .await
    .expect("run claimed subagent job");

    assert!(saw_running.load(Ordering::SeqCst));
    assert_eq!(
        result.final_lifecycle.status,
        SubagentLifecycleStatus::Cancelled
    );
    assert!(result
        .events
        .iter()
        .any(|event| event.kind == SubagentSchedulerEventKind::Running));
    assert!(result
        .events
        .iter()
        .any(|event| event.kind == SubagentSchedulerEventKind::Cancelled));

    drop(store);
    let _ = std::fs::remove_file(db_path);
}

#[tokio::test]
async fn run_claimed_subagent_job_async_fails_when_work_packet_is_missing() {
    let db_path = temp_db_path("run_claimed_subagent_job_async_missing_packet");
    let store = SqliteRuntimeStore::new(&db_path).expect("create sqlite runtime store");
    enqueue_subagent_run_job(
        &store,
        SubagentRunJobRequest {
            session_id: "chat-missing".to_string(),
            parent_turn_id: "turn-missing".to_string(),
            tool_call_id: "tool-missing".to_string(),
            subagent_id: "subagent:turn-missing:tool-missing".to_string(),
            work_packet_ref: "external_context:missing_packet".to_string(),
            checkpoint_id: Some("checkpoint-missing".to_string()),
            run_at_ms: 1_000,
            created_at_ms: 900,
            max_retries: 3,
        },
    )
    .expect("enqueue subagent job");
    let claimed = claim_subagent_run_jobs(
        &store,
        ClaimSubagentRunJobsRequest {
            now_ms: 1_001,
            worker_id: "worker-missing".to_string(),
            session_id: None,
            limit: 1,
            lease_ms: 5_000,
        },
    )
    .expect("claim subagent")
    .into_iter()
    .next()
    .expect("claimed subagent job");

    let actor = RuntimeStoreActor::start(store.clone()).expect("start runtime store actor");
    let result = run_claimed_subagent_job_async(
        &actor,
        claimed,
        &RecordingRunner {
            outcome: SubagentWorkerRunOutcome::Succeeded {
                summary: "should not run".to_string(),
                output_refs: vec![],
            },
        },
        &NoopSubagentLifecycleObserver,
        RunClaimedSubagentJobRequest {
            worker_id: "worker-missing".to_string(),
            started_at_ms: 1_002,
            finished_at_ms: 1_100,
        },
    )
    .await
    .expect("fail missing work packet");

    assert_eq!(result.events.len(), 2);
    assert_eq!(result.events[0].kind, SubagentSchedulerEventKind::Running);
    assert_eq!(result.events[1].kind, SubagentSchedulerEventKind::Failed);
    assert_eq!(
        result.final_lifecycle.status,
        SubagentLifecycleStatus::Failed
    );
    assert!(result
        .final_lifecycle
        .last_error
        .as_deref()
        .unwrap_or_default()
        .contains("subagent work packet object not found"));

    drop(store);
    let _ = std::fs::remove_file(db_path);
}

#[tokio::test]
async fn run_due_subagent_jobs_async_claims_and_executes_batch() {
    let db_path = temp_db_path("run_due_subagent_jobs_async");
    let store = SqliteRuntimeStore::new(&db_path).expect("create sqlite runtime store");
    for index in 0..2 {
        let work_packet_ref = runtime_external_context_keys::subagent_work_packet_ref(
            format!("batch_{index}").as_str(),
        );
        store_valid_subagent_work_packet(
            &store,
            work_packet_ref.as_str(),
            format!("workspace:batch:{index}").as_str(),
            "Borrow",
            vec!["read".to_string()],
            vec![],
        );
        enqueue_subagent_run_job(
            &store,
            SubagentRunJobRequest {
                session_id: "chat-batch".to_string(),
                parent_turn_id: "turn-batch".to_string(),
                tool_call_id: format!("tool-batch-{index}"),
                subagent_id: format!("subagent:turn-batch:tool-batch-{index}"),
                work_packet_ref,
                checkpoint_id: Some("checkpoint-batch".to_string()),
                run_at_ms: 1_000,
                created_at_ms: 900,
                max_retries: 3,
            },
        )
        .expect("enqueue subagent job");
    }

    let actor = RuntimeStoreActor::start(store.clone()).expect("start runtime store actor");
    let result = run_due_subagent_jobs_async(
        &actor,
        &RecordingRunner {
            outcome: SubagentWorkerRunOutcome::Succeeded {
                summary: "done".to_string(),
                output_refs: vec!["external_context:subagent_output".to_string()],
            },
        },
        &NoopSubagentLifecycleObserver,
        RunDueSubagentJobsRequest {
            now_ms: 1_001,
            worker_id: "worker-batch".to_string(),
            session_id: None,
            limit: 8,
            lease_ms: 5_000,
            started_at_ms: 1_002,
            finished_at_ms: 1_100,
        },
    )
    .await
    .expect("run due subagent jobs");

    assert_eq!(result.claimed, 2);
    assert_eq!(result.succeeded, 2);
    assert_eq!(result.failed, 0);
    assert_eq!(result.events.len(), 6);
    assert_eq!(result.events[0].kind, SubagentSchedulerEventKind::Claimed);
    assert_eq!(
        result
            .events
            .iter()
            .filter(|event| event.kind == SubagentSchedulerEventKind::Running)
            .count(),
        2
    );
    assert_eq!(
        result
            .events
            .iter()
            .filter(|event| event.kind == SubagentSchedulerEventKind::Succeeded)
            .count(),
        2
    );
    let listed = store
        .list_runtime_jobs(ListRuntimeJobsRequest {
            statuses: vec![RuntimeJobStatus::Succeeded],
            job_kind: Some(SUBAGENT_RUN_JOB_KIND.to_string()),
            session_id: Some("chat-batch".to_string()),
            branch_id: Some("turn-batch".to_string()),
            limit: 10,
            offset: 0,
        })
        .expect("list succeeded subagent jobs");
    assert_eq!(listed.len(), 2);

    drop(store);
    let _ = std::fs::remove_file(db_path);
}

#[tokio::test]
async fn run_due_subagent_jobs_async_filters_by_session_id() {
    let db_path = temp_db_path("run_due_subagent_jobs_async_session_filter");
    let store = SqliteRuntimeStore::new(&db_path).expect("create sqlite runtime store");
    for session_id in ["chat-a", "chat-b"] {
        let work_packet_ref = format!("external_context:{session_id}:work_packet");
        store_valid_subagent_work_packet(
            &store,
            work_packet_ref.as_str(),
            format!("workspace:{session_id}").as_str(),
            "Borrow",
            vec!["read".to_string()],
            vec![],
        );
        enqueue_subagent_run_job(
            &store,
            SubagentRunJobRequest {
                session_id: session_id.to_string(),
                parent_turn_id: format!("turn-{session_id}"),
                tool_call_id: format!("tool-{session_id}"),
                subagent_id: format!("subagent:turn-{session_id}:tool-{session_id}"),
                work_packet_ref,
                checkpoint_id: Some(format!("checkpoint-{session_id}")),
                run_at_ms: 1_000,
                created_at_ms: 900,
                max_retries: 3,
            },
        )
        .expect("enqueue subagent job");
    }

    let actor = RuntimeStoreActor::start(store.clone()).expect("start runtime store actor");
    let result = run_due_subagent_jobs_async(
        &actor,
        &RecordingRunner {
            outcome: SubagentWorkerRunOutcome::Succeeded {
                summary: "done".to_string(),
                output_refs: vec![],
            },
        },
        &NoopSubagentLifecycleObserver,
        RunDueSubagentJobsRequest {
            now_ms: 1_001,
            worker_id: "worker-chat-a".to_string(),
            session_id: Some("chat-a".to_string()),
            limit: 8,
            lease_ms: 5_000,
            started_at_ms: 1_002,
            finished_at_ms: 1_100,
        },
    )
    .await
    .expect("run due subagent jobs");

    assert_eq!(result.claimed, 1);
    let chat_a = store
        .list_runtime_jobs(ListRuntimeJobsRequest {
            statuses: vec![RuntimeJobStatus::Succeeded],
            job_kind: Some(SUBAGENT_RUN_JOB_KIND.to_string()),
            session_id: Some("chat-a".to_string()),
            branch_id: None,
            limit: 10,
            offset: 0,
        })
        .expect("list chat-a jobs");
    assert_eq!(chat_a.len(), 1);
    let chat_b = store
        .list_runtime_jobs(ListRuntimeJobsRequest {
            statuses: vec![RuntimeJobStatus::Queued],
            job_kind: Some(SUBAGENT_RUN_JOB_KIND.to_string()),
            session_id: Some("chat-b".to_string()),
            branch_id: None,
            limit: 10,
            offset: 0,
        })
        .expect("list chat-b jobs");
    assert_eq!(chat_b.len(), 1);

    drop(store);
    let _ = std::fs::remove_file(db_path);
}

#[tokio::test]
async fn run_due_subagent_jobs_worker_pool_async_runs_jobs_concurrently() {
    let db_path = temp_db_path("run_due_subagent_jobs_worker_pool_async");
    let store = SqliteRuntimeStore::new(&db_path).expect("create sqlite runtime store");
    for index in 0..3 {
        let work_packet_ref = runtime_external_context_keys::subagent_work_packet_ref(
            format!("pool_{index}").as_str(),
        );
        store_valid_subagent_work_packet(
            &store,
            work_packet_ref.as_str(),
            format!("workspace:Centaeris:pool:{index}").as_str(),
            "Borrow",
            vec!["read".to_string()],
            vec![],
        );
        enqueue_subagent_run_job(
            &store,
            SubagentRunJobRequest {
                session_id: "chat-pool".to_string(),
                parent_turn_id: "turn-pool".to_string(),
                tool_call_id: format!("tool-pool-{index}"),
                subagent_id: format!("subagent:turn-pool:tool-pool-{index}"),
                work_packet_ref,
                checkpoint_id: Some("checkpoint-pool".to_string()),
                run_at_ms: 1_000,
                created_at_ms: 900,
                max_retries: 3,
            },
        )
        .expect("enqueue subagent job");
    }

    let active = Arc::new(AtomicUsize::new(0));
    let max_active = Arc::new(AtomicUsize::new(0));
    let actor = RuntimeStoreActor::start(store.clone()).expect("start runtime store actor");
    let result = run_due_subagent_jobs_with_worker_pool_async(
        &actor,
        &ConcurrentRecordingRunner {
            active: active.clone(),
            max_active: max_active.clone(),
            sleep_ms: 160,
            start_barrier: None,
        },
        &NoopSubagentLifecycleObserver,
        RunDueSubagentJobsRequest {
            now_ms: 1_001,
            worker_id: "worker-pool".to_string(),
            session_id: Some("chat-pool".to_string()),
            limit: 3,
            lease_ms: 5_000,
            started_at_ms: 1_002,
            finished_at_ms: 1_100,
        },
        SubagentWorkerPoolPolicy { max_parallelism: 2 },
    )
    .await
    .expect("run due subagent jobs with worker pool");

    assert_eq!(result.claimed, 3);
    assert_eq!(result.succeeded, 3);
    assert!(max_active.load(Ordering::SeqCst) >= 2);
    assert_eq!(
        result
            .events
            .iter()
            .filter(|event| event.kind == SubagentSchedulerEventKind::Claimed)
            .count(),
        3
    );

    drop(store);
    let _ = std::fs::remove_file(db_path);
}

#[tokio::test]
async fn run_due_subagent_jobs_worker_pool_async_serializes_exclusive_resource_claims() {
    let db_path = temp_db_path("run_due_subagent_jobs_worker_pool_async_resource_claim");
    let store = SqliteRuntimeStore::new(&db_path).expect("create sqlite runtime store");
    for (index, context_mode) in ["Move", "Borrow"].into_iter().enumerate() {
        let work_packet_ref = runtime_external_context_keys::subagent_work_packet_ref(
            format!("resource_claim_{index}").as_str(),
        );
        store_valid_subagent_work_packet(
            &store,
            work_packet_ref.as_str(),
            "workspace:Centaeris",
            context_mode,
            vec!["read".to_string()],
            vec![],
        );
        enqueue_subagent_run_job(
            &store,
            SubagentRunJobRequest {
                session_id: "chat-resource-claim".to_string(),
                parent_turn_id: "turn-resource-claim".to_string(),
                tool_call_id: format!("tool-resource-claim-{index}"),
                subagent_id: format!("subagent:turn-resource-claim:tool-resource-claim-{index}"),
                work_packet_ref,
                checkpoint_id: Some("checkpoint-resource-claim".to_string()),
                run_at_ms: 1_000,
                created_at_ms: 900,
                max_retries: 3,
            },
        )
        .expect("enqueue subagent job");
    }

    let active = Arc::new(AtomicUsize::new(0));
    let max_active = Arc::new(AtomicUsize::new(0));
    let actor = RuntimeStoreActor::start(store.clone()).expect("start runtime store actor");
    let result = run_due_subagent_jobs_with_worker_pool_async(
        &actor,
        &ConcurrentRecordingRunner {
            active: active.clone(),
            max_active: max_active.clone(),
            sleep_ms: 160,
            start_barrier: None,
        },
        &NoopSubagentLifecycleObserver,
        RunDueSubagentJobsRequest {
            now_ms: 1_001,
            worker_id: "worker-resource-claim".to_string(),
            session_id: Some("chat-resource-claim".to_string()),
            limit: 2,
            lease_ms: 5_000,
            started_at_ms: 1_002,
            finished_at_ms: 1_100,
        },
        SubagentWorkerPoolPolicy { max_parallelism: 2 },
    )
    .await
    .expect("run due subagent jobs with worker pool");

    assert_eq!(result.claimed, 2);
    assert_eq!(result.succeeded, 2);
    assert_eq!(max_active.load(Ordering::SeqCst), 1);

    drop(store);
    let _ = std::fs::remove_file(db_path);
}

#[tokio::test]
async fn run_due_subagent_jobs_worker_pool_async_keeps_shared_resource_claims_parallel() {
    let db_path = temp_db_path("run_due_subagent_jobs_worker_pool_async_shared_resource_claim");
    let store = SqliteRuntimeStore::new(&db_path).expect("create sqlite runtime store");
    for index in 0..2 {
        let work_packet_ref = runtime_external_context_keys::subagent_work_packet_ref(
            format!("shared_resource_claim_{index}").as_str(),
        );
        store_valid_subagent_work_packet(
            &store,
            work_packet_ref.as_str(),
            "workspace:Centaeris",
            "Borrow",
            vec!["read".to_string()],
            vec![],
        );
        enqueue_subagent_run_job(
            &store,
            SubagentRunJobRequest {
                session_id: "chat-shared-resource-claim".to_string(),
                parent_turn_id: "turn-shared-resource-claim".to_string(),
                tool_call_id: format!("tool-shared-resource-claim-{index}"),
                subagent_id: format!(
                    "subagent:turn-shared-resource-claim:tool-shared-resource-claim-{index}"
                ),
                work_packet_ref,
                checkpoint_id: Some("checkpoint-shared-resource-claim".to_string()),
                run_at_ms: 1_000,
                created_at_ms: 900,
                max_retries: 3,
            },
        )
        .expect("enqueue subagent job");
    }

    let active = Arc::new(AtomicUsize::new(0));
    let max_active = Arc::new(AtomicUsize::new(0));
    let start_barrier = Arc::new(tokio::sync::Barrier::new(2));
    let actor = RuntimeStoreActor::start(store.clone()).expect("start runtime store actor");
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        run_due_subagent_jobs_with_worker_pool_async(
            &actor,
            &ConcurrentRecordingRunner {
                active: active.clone(),
                max_active: max_active.clone(),
                sleep_ms: 160,
                start_barrier: Some(start_barrier),
            },
            &NoopSubagentLifecycleObserver,
            RunDueSubagentJobsRequest {
                now_ms: 1_001,
                worker_id: "worker-shared-resource-claim".to_string(),
                session_id: Some("chat-shared-resource-claim".to_string()),
                limit: 2,
                lease_ms: 5_000,
                started_at_ms: 1_002,
                finished_at_ms: 1_100,
            },
            SubagentWorkerPoolPolicy { max_parallelism: 2 },
        ),
    )
    .await
    .expect("shared resource claims must enter the runner concurrently")
    .expect("run due subagent jobs with worker pool");

    assert_eq!(result.claimed, 2);
    assert_eq!(result.succeeded, 2);
    assert!(max_active.load(Ordering::SeqCst) >= 2);

    drop(store);
    let _ = std::fs::remove_file(db_path);
}

#[tokio::test]
async fn run_due_subagent_jobs_worker_pool_async_recovers_panicked_worker() {
    let db_path = temp_db_path("run_due_subagent_jobs_worker_pool_async_panic");
    let store = SqliteRuntimeStore::new(&db_path).expect("create sqlite runtime store");
    for (index, tool_call_id) in ["tool-panic", "tool-ok"].into_iter().enumerate() {
        let work_packet_ref = runtime_external_context_keys::subagent_work_packet_ref(
            format!("panic_{index}").as_str(),
        );
        store_valid_subagent_work_packet(
            &store,
            work_packet_ref.as_str(),
            format!("workspace:Centaeris:panic:{index}").as_str(),
            "Borrow",
            vec!["read".to_string()],
            vec![],
        );
        enqueue_subagent_run_job(
            &store,
            SubagentRunJobRequest {
                session_id: "chat-panic-pool".to_string(),
                parent_turn_id: "turn-panic-pool".to_string(),
                tool_call_id: tool_call_id.to_string(),
                subagent_id: format!("subagent:turn-panic-pool:{tool_call_id}"),
                work_packet_ref,
                checkpoint_id: Some("checkpoint-panic-pool".to_string()),
                run_at_ms: 1_000,
                created_at_ms: 900,
                max_retries: 1,
            },
        )
        .expect("enqueue subagent job");
    }

    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let actor = RuntimeStoreActor::start(store.clone()).expect("start runtime store actor");
    let result = run_due_subagent_jobs_with_worker_pool_async(
        &actor,
        &SelectivePanicRunner {
            panic_subagent_id_fragment: "tool-panic".to_string(),
        },
        &NoopSubagentLifecycleObserver,
        RunDueSubagentJobsRequest {
            now_ms: 1_001,
            worker_id: "worker-panic-pool".to_string(),
            session_id: Some("chat-panic-pool".to_string()),
            limit: 2,
            lease_ms: 5_000,
            started_at_ms: 1_002,
            finished_at_ms: 1_100,
        },
        SubagentWorkerPoolPolicy { max_parallelism: 2 },
    )
    .await;
    std::panic::set_hook(previous_hook);
    let result = result.expect("run due subagent jobs with worker pool");

    assert_eq!(result.claimed, 2);
    assert_eq!(result.succeeded, 1);
    assert_eq!(result.requeued, 1);
    assert_eq!(result.failed, 0);
    assert_eq!(
        result
            .events
            .iter()
            .filter(|event| event.kind == SubagentSchedulerEventKind::Requeued)
            .count(),
        1
    );
    let requeued = store
        .list_runtime_jobs(ListRuntimeJobsRequest {
            statuses: vec![RuntimeJobStatus::Queued],
            job_kind: Some(SUBAGENT_RUN_JOB_KIND.to_string()),
            session_id: Some("chat-panic-pool".to_string()),
            branch_id: Some("turn-panic-pool".to_string()),
            limit: 10,
            offset: 0,
        })
        .expect("list requeued subagent jobs");
    assert_eq!(requeued.len(), 1);
    assert_eq!(
        requeued[0].last_error.as_deref(),
        Some("subagent worker panicked")
    );
    let succeeded = store
        .list_runtime_jobs(ListRuntimeJobsRequest {
            statuses: vec![RuntimeJobStatus::Succeeded],
            job_kind: Some(SUBAGENT_RUN_JOB_KIND.to_string()),
            session_id: Some("chat-panic-pool".to_string()),
            branch_id: Some("turn-panic-pool".to_string()),
            limit: 10,
            offset: 0,
        })
        .expect("list succeeded subagent jobs");
    assert_eq!(succeeded.len(), 1);

    drop(store);
    let _ = std::fs::remove_file(db_path);
}

#[test]
fn reclaim_expired_runtime_job_leases_recovers_running_subagent_job() {
    let db_path = temp_db_path("reclaim_running_subagent_job");
    let store = SqliteRuntimeStore::new(&db_path).expect("create sqlite runtime store");
    enqueue_subagent_run_job(
        &store,
        SubagentRunJobRequest {
            session_id: "chat-reclaim-running".to_string(),
            parent_turn_id: "turn-reclaim-running".to_string(),
            tool_call_id: "tool-reclaim-running".to_string(),
            subagent_id: "subagent:turn-reclaim-running:tool-reclaim-running".to_string(),
            work_packet_ref: serde_json::json!({
            "workPacket": {
            "taskBrief": {
            "objective": "inspect docs"
            }
            }
            })
            .to_string(),
            checkpoint_id: None,
            run_at_ms: 1_000,
            created_at_ms: 900,
            max_retries: 3,
        },
    )
    .expect("enqueue subagent job");
    let claimed = claim_subagent_run_jobs(
        &store,
        ClaimSubagentRunJobsRequest {
            now_ms: 1_001,
            worker_id: "worker-reclaim-running".to_string(),
            session_id: Some("chat-reclaim-running".to_string()),
            limit: 1,
            lease_ms: 100,
        },
    )
    .expect("claim subagent job");
    let job_id = claimed[0].job.job_id.clone();
    store
        .start_runtime_job(StartRuntimeJobRequest {
            job_id: job_id.clone(),
            lease_owner: "worker-reclaim-running".to_string(),
            started_at_ms: 1_010,
        })
        .expect("start runtime job");

    let reclaimed = store
        .reclaim_expired_runtime_job_leases(1_200)
        .expect("reclaim expired running job");

    assert_eq!(reclaimed, 1);
    let job = store
        .get_runtime_job(job_id.as_str())
        .expect("get reclaimed job")
        .expect("reclaimed job exists");
    assert_eq!(job.status, RuntimeJobStatus::Queued);
    assert_eq!(job.run_at_ms, 1_200);
    assert_eq!(job.last_error.as_deref(), Some("worker_crashed_reclaimed"));

    drop(store);
    let _ = std::fs::remove_file(db_path);
}

#[test]
fn cancel_subagent_run_jobs_cancels_queued_and_leased_jobs_by_scope() {
    let db_path = temp_db_path("cancel_subagent_run_jobs_by_scope");
    let store = SqliteRuntimeStore::new(&db_path).expect("create sqlite runtime store");
    for index in 0..3 {
        enqueue_subagent_run_job(
            &store,
            SubagentRunJobRequest {
                session_id: "chat-cancel-scope".to_string(),
                parent_turn_id: if index < 2 {
                    "turn-cancel-scope".to_string()
                } else {
                    "turn-other".to_string()
                },
                tool_call_id: format!("tool-cancel-scope-{index}"),
                subagent_id: format!("subagent:turn-cancel-scope:tool-cancel-scope-{index}"),
                work_packet_ref: format!("external_context:cancel_scope_{index}"),
                checkpoint_id: Some("checkpoint-cancel-scope".to_string()),
                run_at_ms: 1_000,
                created_at_ms: 900,
                max_retries: 3,
            },
        )
        .expect("enqueue subagent job");
    }
    let _claimed = claim_subagent_run_jobs(
        &store,
        ClaimSubagentRunJobsRequest {
            now_ms: 1_001,
            worker_id: "worker-cancel-scope".to_string(),
            session_id: Some("chat-cancel-scope".to_string()),
            limit: 1,
            lease_ms: 5_000,
        },
    )
    .expect("claim one subagent");

    let result = cancel_subagent_run_jobs(
        &store,
        CancelSubagentRunJobsRequest {
            session_id: Some("chat-cancel-scope".to_string()),
            parent_turn_id: Some("turn-cancel-scope".to_string()),
            subagent_id: None,
            reason: "parent_cancelled".to_string(),
            cancelled_at_ms: 2_000,
            limit: 10,
            include_running: false,
        },
    )
    .expect("cancel scoped subagent jobs");

    assert_eq!(result.cancelled, 2);
    assert_eq!(
        result
            .events
            .iter()
            .filter(|event| event.kind == SubagentSchedulerEventKind::Cancelled)
            .count(),
        2
    );
    let cancelled = store
        .list_runtime_jobs(ListRuntimeJobsRequest {
            statuses: vec![RuntimeJobStatus::Cancelled],
            job_kind: Some(SUBAGENT_RUN_JOB_KIND.to_string()),
            session_id: Some("chat-cancel-scope".to_string()),
            branch_id: Some("turn-cancel-scope".to_string()),
            limit: 10,
            offset: 0,
        })
        .expect("list cancelled subagent jobs");
    assert_eq!(cancelled.len(), 2);
    let other = store
        .list_runtime_jobs(ListRuntimeJobsRequest {
            statuses: vec![RuntimeJobStatus::Queued, RuntimeJobStatus::Leased],
            job_kind: Some(SUBAGENT_RUN_JOB_KIND.to_string()),
            session_id: Some("chat-cancel-scope".to_string()),
            branch_id: Some("turn-other".to_string()),
            limit: 10,
            offset: 0,
        })
        .expect("list other subagent jobs");
    assert_eq!(other.len(), 1);

    drop(store);
    let _ = std::fs::remove_file(db_path);
}

#[tokio::test]
async fn run_due_subagent_jobs_worker_pool_async_fails_when_resource_claim_packet_is_invalid() {
    let db_path = temp_db_path("run_due_subagent_jobs_worker_pool_async_bad_resource_claim");
    let store = SqliteRuntimeStore::new(&db_path).expect("create sqlite runtime store");
    let bad_work_packet_ref =
        runtime_external_context_keys::subagent_work_packet_ref("bad_resource_claim");
    store
        .upsert_external_context_object(invalid_resource_claim_object(bad_work_packet_ref.as_str()))
        .expect("store invalid work packet object");
    enqueue_subagent_run_job(
        &store,
        SubagentRunJobRequest {
            session_id: "chat-bad-resource-claim".to_string(),
            parent_turn_id: "turn-bad-resource-claim".to_string(),
            tool_call_id: "tool-bad-resource-claim".to_string(),
            subagent_id: "subagent:turn-bad-resource-claim:tool-bad-resource-claim".to_string(),
            work_packet_ref: bad_work_packet_ref,
            checkpoint_id: Some("checkpoint-bad-resource-claim".to_string()),
            run_at_ms: 1_000,
            created_at_ms: 900,
            max_retries: 3,
        },
    )
    .expect("enqueue invalid subagent job");

    let valid_work_packet_ref =
        runtime_external_context_keys::subagent_work_packet_ref("good_resource_claim");
    store_valid_subagent_work_packet(
        &store,
        valid_work_packet_ref.as_str(),
        "workspace:Centaeris:good-resource",
        "Borrow",
        vec!["read".to_string()],
        vec![],
    );
    enqueue_subagent_run_job(
        &store,
        SubagentRunJobRequest {
            session_id: "chat-bad-resource-claim".to_string(),
            parent_turn_id: "turn-bad-resource-claim".to_string(),
            tool_call_id: "tool-good-resource-claim".to_string(),
            subagent_id: "subagent:turn-bad-resource-claim:tool-good-resource-claim".to_string(),
            work_packet_ref: valid_work_packet_ref,
            checkpoint_id: Some("checkpoint-bad-resource-claim".to_string()),
            run_at_ms: 1_000,
            created_at_ms: 900,
            max_retries: 3,
        },
    )
    .expect("enqueue valid subagent job");

    let actor = RuntimeStoreActor::start(store.clone()).expect("start runtime store actor");
    let error = run_due_subagent_jobs_with_worker_pool_async(
        &actor,
        &ConcurrentRecordingRunner {
            active: Arc::new(AtomicUsize::new(0)),
            max_active: Arc::new(AtomicUsize::new(0)),
            sleep_ms: 1,
            start_barrier: None,
        },
        &NoopSubagentLifecycleObserver,
        RunDueSubagentJobsRequest {
            now_ms: 1_001,
            worker_id: "worker-bad-resource-claim".to_string(),
            session_id: Some("chat-bad-resource-claim".to_string()),
            limit: 2,
            lease_ms: 5_000,
            started_at_ms: 1_002,
            finished_at_ms: 1_100,
        },
        SubagentWorkerPoolPolicy { max_parallelism: 2 },
    )
    .await
    .expect_err("invalid resource claim packet must fail worker-pool planning");

    assert!(error.contains("decode subagent resource claim failed"));
    assert!(error.contains("job_id="));
    assert!(error.contains("invalid subagent work packet resource claim"));

    drop(store);
    let _ = std::fs::remove_file(db_path);
}
