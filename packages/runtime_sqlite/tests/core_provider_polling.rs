use centaeris_core::execution::sandbox::{SandboxErr, SandboxPolicy, SandboxTransformRequest};
use centaeris_core::execution::{
    ExecutionCancellationProbe, ExecutionFileSystemError, ExecutionFileSystemOutput,
    ExecutionFileSystemRequest, ExecutionHostBinding, ExecutionHostCommandOutput,
    ExecutionHostMode, ExecutionHostRunner, ExecutionHostStatus,
};
use centaeris_core::extension::skills::SkillCatalogLoadConfig;
use centaeris_core::model::provider_polling::{
    build_provider_poll_payload_ref, ProviderPollingRuntimePayload, ProviderPollingSchedulerConfig,
    ProviderPollingToolLayerResolution, StoreBackedProviderPollingScheduler,
    PROVIDER_POLL_RUNTIME_JOB_KIND,
};
use centaeris_core::session::external_context::{
    ExternalContextStorePort, ListExternalContextObjectsRequest,
};
use centaeris_core::session::reliability::{
    ClaimDueRuntimeJobsRequest, DeadLetterStatus, DeadLetterStorePort, ListDeadLettersRequest,
    RuntimeBackoffPolicy, RuntimeJobRecord, RuntimeJobStatus, RuntimeJobStorePort,
    ScheduleRuntimeJobRequest,
};
use centaeris_core::tool::layer::{
    DynamicToolProvider, DynamicToolProviderRequest, DynamicToolProviderResponse,
    ToolExecutionFact, ToolLayer,
};
use centaeris_core::tool::{
    DynamicToolContract, DynamicToolRegistry, ToolErrorInfo, ToolFailureKind,
};
use centaeris_runtime_sqlite::SqliteRuntimeStore;
use serde_json::{json, Value};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

struct NoopExecutionHostRunner;

impl ExecutionHostRunner for NoopExecutionHostRunner {
    fn status(&self, _policy: &SandboxPolicy) -> Result<ExecutionHostStatus, SandboxErr> {
        panic!("provider polling test must not query execution host status")
    }

    fn run_file_system_operation(
        &self,
        _request: ExecutionFileSystemRequest,
    ) -> Result<ExecutionFileSystemOutput, ExecutionFileSystemError> {
        panic!("provider polling test must not execute file operations")
    }

    fn run_host_command(
        &self,
        _operation_id: Option<&str>,
        _req: SandboxTransformRequest,
        _cancellation_probe: Option<&ExecutionCancellationProbe>,
    ) -> Result<ExecutionHostCommandOutput, SandboxErr> {
        panic!("provider polling test must not execute host commands")
    }
}

fn tool_layer() -> ToolLayer {
    let cwd = std::env::temp_dir();
    let binding = ExecutionHostBinding::new(
        ExecutionHostMode::Local,
        Arc::new(NoopExecutionHostRunner),
        cwd.clone(),
        SandboxPolicy::workspace_write_no_network(&cwd),
    )
    .expect("build provider polling execution host binding");
    ToolLayer::try_new_with_skill_catalog_config_dynamic_tool_registry_and_execution_host_binding(
        SkillCatalogLoadConfig::default(),
        dynamic_registry(),
        Arc::new(binding),
    )
    .expect("build provider polling tool layer")
}

#[derive(Debug)]
struct CompletedPollProvider;

impl DynamicToolProvider for CompletedPollProvider {
    fn provider_id(&self) -> &str {
        "ragflow.clinic"
    }

    fn execute<'a>(
        &'a self,
        req: DynamicToolProviderRequest,
    ) -> Pin<Box<dyn Future<Output = Result<DynamicToolProviderResponse, String>> + Send + 'a>>
    {
        Box::pin(async move {
            let args = serde_json::from_str::<Value>(req.args_json.as_str()).unwrap_or(Value::Null);
            Ok(DynamicToolProviderResponse {
                content: "provider poll completed".to_string(),
                details: json!({
                    "externalObject": {
                        "mode": "externalObject",
                        "pointer": {
                            "objectId": "external_context:provider_poll_done",
                            "objectKind": "externalKnowledge",
                            "source": "ragflow_clinic_search",
                            "recency": "warm",
                            "trust": "raw",
                            "reason": "provider poll completed",
                            "updatedAtMs": 5_200
                        },
                        "object": {
                            "schemaVersion": "external_context.v1",
                            "objectId": "external_context:provider_poll_done",
                            "objectKind": "externalKnowledge",
                            "sourceProviderId": "ragflow.clinic",
                            "sourceToolName": req.tool_name,
                            "title": "Provider poll completed",
                            "content": format!("poll result for {}", args.get("ticket").and_then(Value::as_str).unwrap_or("unknown")),
                            "metadata": {
                                "args": args
                            },
                            "updatedAtMs": 5_200
                        }
                    }
                }),
                is_error: false,
                facts: vec![ToolExecutionFact::CitationRecorded(json!({
                    "citationId": format!("citation:{}", "a".repeat(64)),
                    "inputRef": "input_provider_poll",
                    "ownerRef": "source_provider_poll",
                    "ownerKind": "sourceObject",
                    "displayName": "provider-poll.txt",
                    "evidenceKind": "workspaceSource",
                    "ownerSha256": format!("sha256:{}", "b".repeat(64)),
                    "sourceToolCallId": req.tool_call_id,
                    "sourceToolName": req.tool_name,
                    "locator": {"startLine": 1, "endLine": 1},
                }))],
                transition_reason: Some("provider_poll_completed".to_string()),
            })
        })
    }
}

#[derive(Debug)]
struct PendingPollProvider {
    next_poll_at_ms: i64,
}

impl DynamicToolProvider for PendingPollProvider {
    fn provider_id(&self) -> &str {
        "ragflow.clinic"
    }

    fn execute<'a>(
        &'a self,
        _req: DynamicToolProviderRequest,
    ) -> Pin<Box<dyn Future<Output = Result<DynamicToolProviderResponse, String>> + Send + 'a>>
    {
        let next_poll_at_ms = self.next_poll_at_ms;
        Box::pin(async move {
            Ok(DynamicToolProviderResponse {
                content: "provider result is pending".to_string(),
                details: json!({
                    "providerPolling": {
                        "status": "pending",
                        "pollKey": "ticket-continue",
                        "pollArgs": {
                            "ticket": "ticket-continue"
                        },
                        "nextPollAtMs": next_poll_at_ms,
                        "leaseMs": 30_000,
                        "maxPollAttempts": 4,
                        "idempotencyKey": "ragflow.clinic.poll:ticket-continue"
                    }
                }),
                is_error: false,
                facts: Vec::new(),
                transition_reason: Some("provider_poll_pending".to_string()),
            })
        })
    }
}

#[derive(Debug)]
struct PendingThenCompletedPollProvider {
    calls: Arc<AtomicUsize>,
}

impl PendingThenCompletedPollProvider {
    fn new() -> Self {
        Self {
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl DynamicToolProvider for PendingThenCompletedPollProvider {
    fn provider_id(&self) -> &str {
        "ragflow.clinic"
    }

    fn execute<'a>(
        &'a self,
        req: DynamicToolProviderRequest,
    ) -> Pin<Box<dyn Future<Output = Result<DynamicToolProviderResponse, String>> + Send + 'a>>
    {
        Box::pin(async move {
            let call_index = self.calls.fetch_add(1, Ordering::SeqCst);
            if call_index == 0 {
                return Ok(DynamicToolProviderResponse {
                    content: "provider result is pending".to_string(),
                    details: json!({
                        "providerPolling": {
                            "status": "pending",
                            "pollKey": "ticket-soak",
                            "pollArgs": {
                                "ticket": "ticket-soak"
                            },
                            "nextPollAtMs": 99_999_999_999_999_i64,
                            "leaseMs": 30_000,
                            "maxPollAttempts": 4,
                            "idempotencyKey": "ragflow.clinic.poll:ticket-soak"
                        }
                    }),
                    is_error: false,
                    facts: Vec::new(),
                    transition_reason: Some("provider_poll_pending".to_string()),
                });
            }

            Ok(DynamicToolProviderResponse {
                content: "provider poll completed after scheduler restart".to_string(),
                details: json!({
                    "externalObject": {
                        "mode": "externalObject",
                        "pointer": {
                            "objectId": "external_context:provider_poll_soak_done",
                            "objectKind": "externalKnowledge",
                            "source": "ragflow_clinic_search",
                            "recency": "warm",
                            "trust": "raw",
                            "reason": "provider poll soak completed",
                            "updatedAtMs": 6_200
                        },
                        "object": {
                            "schemaVersion": "external_context.v1",
                            "objectId": "external_context:provider_poll_soak_done",
                            "objectKind": "externalKnowledge",
                            "sourceProviderId": "ragflow.clinic",
                            "sourceToolName": req.tool_name,
                            "title": "Provider poll soak completed",
                            "content": "provider poll completed after scheduler restart",
                            "metadata": {
                                "calls": call_index + 1
                            },
                            "updatedAtMs": 6_200
                        }
                    }
                }),
                is_error: false,
                facts: Vec::new(),
                transition_reason: Some("provider_poll_completed_after_restart".to_string()),
            })
        })
    }
}

#[derive(Debug)]
struct ErrorPollProvider;

impl DynamicToolProvider for ErrorPollProvider {
    fn provider_id(&self) -> &str {
        "ragflow.clinic"
    }

    fn execute<'a>(
        &'a self,
        _req: DynamicToolProviderRequest,
    ) -> Pin<Box<dyn Future<Output = Result<DynamicToolProviderResponse, String>> + Send + 'a>>
    {
        Box::pin(async move { Err("provider poll failed".to_string()) })
    }

    fn execute_with_error_info<'a>(
        &'a self,
        _req: DynamicToolProviderRequest,
    ) -> Pin<Box<dyn Future<Output = Result<DynamicToolProviderResponse, ToolErrorInfo>> + Send + 'a>>
    {
        Box::pin(async move {
            Err(ToolErrorInfo::new(
                ToolFailureKind::HostUnavailable,
                "provider poll failed",
                "Provider unavailable",
            )
            .with_retryable(true))
        })
    }
}

fn temp_db_path(suffix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock moved backwards")
        .as_nanos();
    std::env::temp_dir().join(format!("centaeris_provider_poll_{suffix}_{nanos}.db"))
}

fn wait_for_job_status(db_path: &PathBuf, job_id: &str, status: &str, timeout_ms: u64) -> bool {
    let started = SystemTime::now();
    loop {
        let conn = rusqlite::Connection::open(db_path).expect("open provider poll db");
        let current = conn
            .query_row(
                "SELECT status FROM runtime_jobs WHERE job_id = ?1 LIMIT 1",
                rusqlite::params![job_id],
                |row| row.get::<_, String>(0),
            )
            .ok();
        if current.as_deref() == Some(status) {
            return true;
        }
        if SystemTime::now()
            .duration_since(started)
            .expect("elapsed")
            .as_millis() as u64
            >= timeout_ms
        {
            return false;
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn wait_for_job_retry_count(
    db_path: &PathBuf,
    job_id: &str,
    retry_count: u32,
    timeout_ms: u64,
) -> bool {
    let started = SystemTime::now();
    loop {
        let conn = rusqlite::Connection::open(db_path).expect("open provider poll db");
        let current = conn
            .query_row(
                "SELECT retry_count FROM runtime_jobs WHERE job_id = ?1 LIMIT 1",
                rusqlite::params![job_id],
                |row| row.get::<_, u32>(0),
            )
            .ok();
        if current == Some(retry_count) {
            return true;
        }
        if SystemTime::now()
            .duration_since(started)
            .expect("elapsed")
            .as_millis() as u64
            >= timeout_ms
        {
            return false;
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn dynamic_registry() -> Arc<DynamicToolRegistry> {
    Arc::new(
        DynamicToolRegistry::from_contracts(vec![DynamicToolContract {
            name: "ragflow_clinic_search".to_string(),
            category: "external.context".to_string(),
            summary: "Search clinic knowledge".to_string(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": true
            }),
            provider_id: "ragflow.clinic".to_string(),
            scopes: vec!["knowledge.read".to_string()],
            concurrency_safe: true,
            turn_behavior: centaeris_core::tool::ToolTurnBehavior::ContinueTurn,
        }])
        .expect("build provider poll dynamic registry"),
    )
}

fn sample_runtime_job(
    job_id: &str,
    run_at_ms: i64,
    payload_ref: String,
    max_retries: u32,
) -> RuntimeJobRecord {
    RuntimeJobRecord {
        job_id: job_id.to_string(),
        job_kind: PROVIDER_POLL_RUNTIME_JOB_KIND.to_string(),
        status: RuntimeJobStatus::Queued,
        run_at_ms,
        lease_owner: None,
        lease_expires_at_ms: None,
        heartbeat_at_ms: None,
        retry_count: 0,
        max_retries,
        backoff_policy: RuntimeBackoffPolicy::default(),
        idempotency_key: format!("idem:{job_id}"),
        session_id: Some("chat-provider-poll".to_string()),
        branch_id: None,
        checkpoint_id: None,
        payload_ref: Some(payload_ref),
        output_refs: vec![],
        last_error: None,
        created_at_ms: run_at_ms,
        updated_at_ms: run_at_ms,
    }
}

#[test]
fn provider_poll_scheduler_completes_job_and_persists_external_context() {
    let db_path = temp_db_path("complete");
    let store = SqliteRuntimeStore::new(&db_path).expect("create provider poll sqlite store");
    let payload_ref = build_provider_poll_payload_ref(&ProviderPollingRuntimePayload {
        provider_id: "ragflow.clinic".to_string(),
        tool_name: "ragflow_clinic_search".to_string(),
        poll_key: "ticket-done".to_string(),
        poll_args: json!({ "ticket": "ticket-done" }),
        source_agent_run_id: "agent-run-provider-poll".to_string(),
        source_turn_id: "turn-provider-poll".to_string(),
        source_tool_call_id: "tc-provider-poll".to_string(),
        lease_ms: 30_000,
    })
    .expect("build provider poll payload ref");
    store
        .schedule_runtime_job(ScheduleRuntimeJobRequest {
            job: sample_runtime_job("provider-poll-job-done", 0, payload_ref, 4),
        })
        .expect("schedule provider poll job");

    let mut tool_layer = tool_layer();
    tool_layer
        .register_dynamic_tool_provider(Arc::new(CompletedPollProvider))
        .expect("provider binding");
    let scheduler = StoreBackedProviderPollingScheduler::new(
        store.clone(),
        tool_layer,
        ProviderPollingSchedulerConfig {
            tick_ms: 25,
            lease_ms: 30_000,
            claim_limit: 2,
            max_jobs_per_tick: 1,
            ..ProviderPollingSchedulerConfig::default()
        },
    );
    scheduler.start().expect("start provider poll scheduler");
    assert!(wait_for_job_status(
        &db_path,
        "provider-poll-job-done",
        "succeeded",
        3_000
    ));
    scheduler.stop().expect("stop provider poll scheduler");

    let job = store
        .get_runtime_job("provider-poll-job-done")
        .expect("get provider poll job")
        .expect("provider poll job exists");
    assert_eq!(job.status, RuntimeJobStatus::Succeeded);
    assert_eq!(
        job.output_refs,
        vec!["external_context:provider_poll_done".to_string()]
    );
    let objects = store
        .list_external_context_objects(ListExternalContextObjectsRequest {
            session_id: Some("chat-provider-poll".to_string()),
            limit: 10,
            offset: 0,
        })
        .expect("list provider poll external objects");
    assert_eq!(objects.len(), 1);
    assert_eq!(objects[0].object_id, "external_context:provider_poll_done");
    let object = store
        .load_external_context_object("external_context:provider_poll_done")
        .expect("load provider poll object")
        .expect("provider poll object");
    assert_eq!(
        object.metadata["toolExecutionFacts"][0]["payload"]["sourceToolCallId"],
        "tc-provider-poll"
    );

    let _ = std::fs::remove_file(db_path);
}

#[test]
fn provider_poll_scheduler_dead_letters_old_payload_instead_of_retrying_forever() {
    let db_path = temp_db_path("old_payload");
    let store = SqliteRuntimeStore::new(&db_path).expect("create provider poll sqlite store");
    let old_payload = format!(
        "provider-polling-json:{}",
        json!({
            "providerId": "ragflow.clinic",
            "toolName": "ragflow_clinic_search",
            "pollKey": "ticket-old",
            "pollArgs": {"ticket": "ticket-old"},
            "sourceTurnId": "turn-old",
            "sourceToolCallId": "tc-old",
            "leaseMs": 30_000
        })
    );
    store
        .schedule_runtime_job(ScheduleRuntimeJobRequest {
            job: sample_runtime_job("provider-poll-job-old-payload", 0, old_payload, 1_200),
        })
        .expect("schedule old provider poll payload");

    let scheduler = StoreBackedProviderPollingScheduler::new(
        store.clone(),
        tool_layer(),
        ProviderPollingSchedulerConfig {
            tick_ms: 25,
            lease_ms: 30_000,
            claim_limit: 1,
            max_jobs_per_tick: 1,
            ..ProviderPollingSchedulerConfig::default()
        },
    );
    scheduler.start().expect("start provider poll scheduler");
    assert!(wait_for_job_status(
        &db_path,
        "provider-poll-job-old-payload",
        "dead_lettered",
        3_000
    ));
    scheduler.stop().expect("stop provider poll scheduler");

    let job = store
        .get_runtime_job("provider-poll-job-old-payload")
        .expect("get old payload job")
        .expect("old payload job exists");
    assert_eq!(job.status, RuntimeJobStatus::DeadLettered);
    assert_eq!(job.retry_count, 1);
    let letters = store
        .list_dead_letters(ListDeadLettersRequest {
            statuses: vec![DeadLetterStatus::Open],
            job_kind: Some(PROVIDER_POLL_RUNTIME_JOB_KIND.to_string()),
            session_id: Some("chat-provider-poll".to_string()),
            branch_id: None,
            limit: 10,
            offset: 0,
        })
        .expect("list old payload dead letter");
    assert_eq!(letters.len(), 1);
    assert_eq!(letters[0].failure_reason, "provider_poll_payload_invalid");
    assert!(letters[0].last_error.contains("sourceAgentRunId"));

    let _ = std::fs::remove_file(db_path);
}

#[test]
fn provider_poll_scheduler_reclaims_expired_leased_job_after_restart() {
    let db_path = temp_db_path("restart_reclaim");
    let store = SqliteRuntimeStore::new(&db_path).expect("create provider poll sqlite store");
    let payload_ref = build_provider_poll_payload_ref(&ProviderPollingRuntimePayload {
        provider_id: "ragflow.clinic".to_string(),
        tool_name: "ragflow_clinic_search".to_string(),
        poll_key: "ticket-restart-done".to_string(),
        poll_args: json!({ "ticket": "ticket-restart-done" }),
        source_agent_run_id: "agent-run-provider-poll".to_string(),
        source_turn_id: "turn-provider-poll".to_string(),
        source_tool_call_id: "tc-provider-poll".to_string(),
        lease_ms: 30_000,
    })
    .expect("build provider poll payload ref");
    store
        .schedule_runtime_job(ScheduleRuntimeJobRequest {
            job: sample_runtime_job("provider-poll-job-restart", 0, payload_ref, 4),
        })
        .expect("schedule provider poll restart job");
    let claimed_by_crashed_worker = store
        .claim_due_runtime_jobs(ClaimDueRuntimeJobsRequest {
            now_ms: 1,
            worker_id: "provider-poll-crashed-worker".to_string(),
            job_id: None,
            job_kind: Some(PROVIDER_POLL_RUNTIME_JOB_KIND.to_string()),
            session_id: None,
            limit: 1,
            lease_ms: 1,
        })
        .expect("claim provider poll job before simulated crash");
    assert_eq!(claimed_by_crashed_worker.len(), 1);
    assert_eq!(
        claimed_by_crashed_worker[0].lease_owner.as_deref(),
        Some("provider-poll-crashed-worker")
    );

    let mut tool_layer = tool_layer();
    tool_layer
        .register_dynamic_tool_provider(Arc::new(CompletedPollProvider))
        .expect("provider binding");
    let scheduler = StoreBackedProviderPollingScheduler::new(
        store.clone(),
        tool_layer,
        ProviderPollingSchedulerConfig {
            tick_ms: 25,
            lease_ms: 30_000,
            claim_limit: 2,
            max_jobs_per_tick: 1,
            ..ProviderPollingSchedulerConfig::default()
        },
    );
    scheduler
        .start()
        .expect("start restarted provider poll scheduler");
    assert!(wait_for_job_status(
        &db_path,
        "provider-poll-job-restart",
        "succeeded",
        3_000
    ));
    scheduler
        .stop()
        .expect("stop restarted provider poll scheduler");

    let status_json = scheduler
        .status_snapshot()
        .expect("provider poll scheduler status snapshot");
    let status: Value =
        serde_json::from_str(status_json.as_str()).expect("parse provider poll scheduler status");
    assert!(
        status
            .get("reclaimedLeases")
            .and_then(Value::as_u64)
            .unwrap_or_default()
            >= 1
    );

    let _ = std::fs::remove_file(db_path);
}

#[test]
fn provider_poll_scheduler_restart_soak_retries_pending_then_completes() {
    let db_path = temp_db_path("restart_soak");
    let store = SqliteRuntimeStore::new(&db_path).expect("create provider poll sqlite store");
    let payload_ref = build_provider_poll_payload_ref(&ProviderPollingRuntimePayload {
        provider_id: "ragflow.clinic".to_string(),
        tool_name: "ragflow_clinic_search".to_string(),
        poll_key: "ticket-soak".to_string(),
        poll_args: json!({ "ticket": "ticket-soak" }),
        source_agent_run_id: "agent-run-provider-poll".to_string(),
        source_turn_id: "turn-provider-poll-soak".to_string(),
        source_tool_call_id: "tc-provider-poll-soak".to_string(),
        lease_ms: 30_000,
    })
    .expect("build provider poll payload ref");
    store
        .schedule_runtime_job(ScheduleRuntimeJobRequest {
            job: sample_runtime_job("provider-poll-job-soak", 0, payload_ref, 4),
        })
        .expect("schedule provider poll soak job");

    let provider = Arc::new(PendingThenCompletedPollProvider::new());
    let mut first_tool_layer = tool_layer();
    first_tool_layer
        .register_dynamic_tool_provider(provider.clone())
        .expect("provider binding");
    let first_scheduler = StoreBackedProviderPollingScheduler::new(
        store.clone(),
        first_tool_layer,
        ProviderPollingSchedulerConfig {
            tick_ms: 25,
            lease_ms: 30_000,
            claim_limit: 2,
            max_jobs_per_tick: 1,
            ..ProviderPollingSchedulerConfig::default()
        },
    );
    first_scheduler
        .start()
        .expect("start first provider poll scheduler");
    assert!(wait_for_job_retry_count(
        &db_path,
        "provider-poll-job-soak",
        1,
        3_000
    ));
    first_scheduler
        .stop()
        .expect("stop first provider poll scheduler");

    {
        let conn = rusqlite::Connection::open(&db_path).expect("open provider poll db");
        conn.execute(
            "UPDATE runtime_jobs SET run_at_ms = 0 WHERE job_id = ?1",
            rusqlite::params!["provider-poll-job-soak"],
        )
        .expect("simulate provider poll time passing");
    }

    let mut second_tool_layer = tool_layer();
    second_tool_layer
        .register_dynamic_tool_provider(provider.clone())
        .expect("provider binding");
    let second_scheduler = StoreBackedProviderPollingScheduler::new(
        store.clone(),
        second_tool_layer,
        ProviderPollingSchedulerConfig {
            tick_ms: 25,
            lease_ms: 30_000,
            claim_limit: 2,
            max_jobs_per_tick: 1,
            ..ProviderPollingSchedulerConfig::default()
        },
    );
    second_scheduler
        .start()
        .expect("start second provider poll scheduler");
    assert!(wait_for_job_status(
        &db_path,
        "provider-poll-job-soak",
        "succeeded",
        3_000
    ));
    second_scheduler
        .stop()
        .expect("stop second provider poll scheduler");

    let job = store
        .get_runtime_job("provider-poll-job-soak")
        .expect("get provider poll soak job")
        .expect("provider poll soak job exists");
    assert_eq!(job.status, RuntimeJobStatus::Succeeded);
    assert_eq!(job.retry_count, 1);
    assert_eq!(
        job.output_refs,
        vec!["external_context:provider_poll_soak_done".to_string()]
    );

    let objects = store
        .list_external_context_objects(ListExternalContextObjectsRequest {
            session_id: Some("chat-provider-poll".to_string()),
            limit: 10,
            offset: 0,
        })
        .expect("list provider poll soak external objects");
    assert_eq!(objects.len(), 1);
    assert_eq!(
        objects[0].object_id,
        "external_context:provider_poll_soak_done"
    );

    let _ = std::fs::remove_file(db_path);
}

#[test]
fn provider_poll_scheduler_requeues_pending_job() {
    let db_path = temp_db_path("pending");
    let store = SqliteRuntimeStore::new(&db_path).expect("create provider poll sqlite store");
    let payload_ref = build_provider_poll_payload_ref(&ProviderPollingRuntimePayload {
        provider_id: "ragflow.clinic".to_string(),
        tool_name: "ragflow_clinic_search".to_string(),
        poll_key: "ticket-continue".to_string(),
        poll_args: json!({ "ticket": "ticket-continue" }),
        source_agent_run_id: "agent-run-provider-poll".to_string(),
        source_turn_id: "turn-provider-poll".to_string(),
        source_tool_call_id: "tc-provider-poll".to_string(),
        lease_ms: 30_000,
    })
    .expect("build provider poll payload ref");
    store
        .schedule_runtime_job(ScheduleRuntimeJobRequest {
            job: sample_runtime_job("provider-poll-job-pending", 0, payload_ref, 4),
        })
        .expect("schedule provider poll pending job");

    let next_poll_at_ms =
        centaeris_core::runtime::contracts::current_timestamp_ms().saturating_add(8_500);
    let mut tool_layer = tool_layer();
    tool_layer
        .register_dynamic_tool_provider(Arc::new(PendingPollProvider { next_poll_at_ms }))
        .expect("provider binding");
    let scheduler = StoreBackedProviderPollingScheduler::new(
        store.clone(),
        tool_layer,
        ProviderPollingSchedulerConfig {
            tick_ms: 25,
            lease_ms: 30_000,
            claim_limit: 2,
            max_jobs_per_tick: 1,
            ..ProviderPollingSchedulerConfig::default()
        },
    );
    scheduler.start().expect("start provider poll scheduler");
    assert!(wait_for_job_retry_count(
        &db_path,
        "provider-poll-job-pending",
        1,
        3_000
    ));
    scheduler.stop().expect("stop provider poll scheduler");

    let job = store
        .get_runtime_job("provider-poll-job-pending")
        .expect("get provider poll pending job")
        .expect("provider poll pending job exists");
    assert_eq!(job.status, RuntimeJobStatus::Queued);
    assert_eq!(job.retry_count, 1);
    assert_eq!(job.run_at_ms, next_poll_at_ms);
    let progress = job
        .last_error
        .as_deref()
        .and_then(|value| value.strip_prefix("provider-polling-progress-json:"))
        .and_then(|value| serde_json::from_str::<Value>(value).ok())
        .expect("provider poll pending progress");
    assert_eq!(progress["pendingAttempts"], 1);
    assert_eq!(progress["consecutiveErrorAttempts"], 0);

    let _ = std::fs::remove_file(db_path);
}

#[test]
fn provider_poll_scheduler_dead_letters_exhausted_job() {
    let db_path = temp_db_path("dead_letter");
    let store = SqliteRuntimeStore::new(&db_path).expect("create provider poll sqlite store");
    let payload_ref = build_provider_poll_payload_ref(&ProviderPollingRuntimePayload {
        provider_id: "ragflow.clinic".to_string(),
        tool_name: "ragflow_clinic_search".to_string(),
        poll_key: "ticket-error".to_string(),
        poll_args: json!({ "ticket": "ticket-error" }),
        source_agent_run_id: "agent-run-provider-poll".to_string(),
        source_turn_id: "turn-provider-poll".to_string(),
        source_tool_call_id: "tc-provider-poll".to_string(),
        lease_ms: 30_000,
    })
    .expect("build provider poll payload ref");
    let mut job = sample_runtime_job("provider-poll-job-error", 0, payload_ref, 1);
    job.backoff_policy = RuntimeBackoffPolicy {
        base_delay_ms: 25,
        max_delay_ms: 25,
        multiplier: 1.0,
        jitter_ms: 0,
    };
    store
        .schedule_runtime_job(ScheduleRuntimeJobRequest { job })
        .expect("schedule provider poll error job");

    let mut tool_layer = tool_layer();
    tool_layer
        .register_dynamic_tool_provider(Arc::new(ErrorPollProvider))
        .expect("provider binding");
    let scheduler = StoreBackedProviderPollingScheduler::new(
        store.clone(),
        tool_layer,
        ProviderPollingSchedulerConfig {
            tick_ms: 25,
            lease_ms: 30_000,
            claim_limit: 2,
            max_jobs_per_tick: 1,
            ..ProviderPollingSchedulerConfig::default()
        },
    );
    scheduler.start().expect("start provider poll scheduler");
    assert!(wait_for_job_status(
        &db_path,
        "provider-poll-job-error",
        "dead_lettered",
        3_000
    ));
    scheduler.stop().expect("stop provider poll scheduler");

    let letters = store
        .list_dead_letters(ListDeadLettersRequest {
            statuses: vec![DeadLetterStatus::Open],
            job_kind: Some(PROVIDER_POLL_RUNTIME_JOB_KIND.to_string()),
            session_id: Some("chat-provider-poll".to_string()),
            branch_id: None,
            limit: 10,
            offset: 0,
        })
        .expect("list provider poll dead letters");
    assert_eq!(letters.len(), 1);
    assert_eq!(letters[0].original_job_id, "provider-poll-job-error");
    assert_eq!(letters[0].failure_reason, "provider_poll_retry_exhausted");

    let _ = std::fs::remove_file(db_path);
}

#[test]
fn provider_poll_scheduler_resolver_failure_uses_retry_pipeline() {
    let db_path = temp_db_path("resolver_failure");
    let store = SqliteRuntimeStore::new(&db_path).expect("create provider poll sqlite store");
    let payload_ref = build_provider_poll_payload_ref(&ProviderPollingRuntimePayload {
        provider_id: "ragflow.clinic".to_string(),
        tool_name: "ragflow_clinic_search".to_string(),
        poll_key: "ticket-missing-config".to_string(),
        poll_args: json!({ "ticket": "ticket-missing-config" }),
        source_agent_run_id: "agent-run-provider-poll".to_string(),
        source_turn_id: "turn-provider-poll".to_string(),
        source_tool_call_id: "tc-provider-poll".to_string(),
        lease_ms: 30_000,
    })
    .expect("build provider poll payload ref");
    let mut job = sample_runtime_job("provider-poll-job-resolver-failure", 0, payload_ref, 1);
    job.backoff_policy = RuntimeBackoffPolicy {
        base_delay_ms: 25,
        max_delay_ms: 25,
        multiplier: 1.0,
        jitter_ms: 0,
    };
    store
        .schedule_runtime_job(ScheduleRuntimeJobRequest { job })
        .expect("schedule provider poll resolver failure job");

    let scheduler = StoreBackedProviderPollingScheduler::new_with_tool_layer_resolver(
        store.clone(),
        Arc::new(|_job| {
            ProviderPollingToolLayerResolution::Failed(
                ToolErrorInfo::new(
                    ToolFailureKind::HostUnavailable,
                    "session provider config missing",
                    "Provider unavailable",
                )
                .with_retryable(true),
            )
        }),
        ProviderPollingSchedulerConfig {
            tick_ms: 25,
            lease_ms: 30_000,
            claim_limit: 2,
            max_jobs_per_tick: 1,
            ..ProviderPollingSchedulerConfig::default()
        },
    );
    scheduler.start().expect("start provider poll scheduler");
    assert!(wait_for_job_status(
        &db_path,
        "provider-poll-job-resolver-failure",
        "dead_lettered",
        3_000
    ));
    scheduler.stop().expect("stop provider poll scheduler");

    let letters = store
        .list_dead_letters(ListDeadLettersRequest {
            statuses: vec![DeadLetterStatus::Open],
            job_kind: Some(PROVIDER_POLL_RUNTIME_JOB_KIND.to_string()),
            session_id: Some("chat-provider-poll".to_string()),
            branch_id: None,
            limit: 10,
            offset: 0,
        })
        .expect("list provider poll resolver failure dead letters");
    assert_eq!(letters.len(), 1);
    assert_eq!(
        letters[0].original_job_id,
        "provider-poll-job-resolver-failure"
    );
    assert_eq!(letters[0].failure_reason, "provider_poll_retry_exhausted");
    assert_eq!(letters[0].last_error, "session provider config missing");

    let _ = std::fs::remove_file(db_path);
}
