use super::*;
use crate::contract::AgentRunStart;
use crate::postgres_store::PostgresRuntimeStore;
use centaeris_core::execution::*;
use centaeris_core::extension::skills::SkillCatalogLoadConfig;
use centaeris_core::model::{
    EmptyModelSessionConfigStore, GenerateResult, ModelClient, ModelClientFuture,
    ModelClientRequest, ModelClientResponse, ToolCallEnvelope,
};
use centaeris_core::runtime::contracts::RuntimeAgentRunIdentityV1;
use centaeris_core::runtime::{
    AgentRunInitialInput, AgentRunRequest, AgentRunStop, AgentRuntime, AgentRuntimeConfig,
    ToolConcurrencyCoordinator, ToolSafePoint,
};
use centaeris_core::session::reliability::{
    ClaimDueRuntimeJobsRequest, RuntimeBackoffPolicy, RuntimeJobRecord, RuntimeJobStatus,
    RuntimeJobStorePort, ScheduleRuntimeJobRequest, StartRuntimeJobRequest,
};
use centaeris_core::session::{AgentRunSessionState, RuntimeJobLeaseFence, SessionCommitReceipt};
use centaeris_core::tool::layer::ToolLayer;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

struct TriggerProbe {
    case: String,
    calls: AtomicUsize,
    locks_checked: AtomicUsize,
    released: AtomicBool,
    release: tokio::sync::Notify,
    forwarded: Mutex<Vec<(u16, Value)>>,
    api_url: String,
    token: String,
    database_url: String,
    log: crate::postgres_store::PostgresSessionLog,
    initial: Vec<centaeris_core::session::SequencedSessionRecord>,
    fence: RuntimeJobLeaseFence,
    start: AgentRunStart,
}

async fn trigger_proxy(
    axum::extract::State(probe): axum::extract::State<Arc<TriggerProbe>>,
    body: axum::body::Bytes,
) -> (axum::http::StatusCode, String) {
    probe.calls.fetch_add(1, Ordering::SeqCst);
    let check = probe.clone();
    tokio::task::spawn_blocking(move || {
        // A real replay takes the same append mutex and fenced transaction.
        check
            .log
            .append_session_records_with_runtime_job_lease_blocking(
                &check.start.agent_run_id,
                &check.initial,
                &check.fence,
            )
            .unwrap();
        let mut db = postgres::Client::connect(&check.database_url, postgres::NoTls).unwrap();
        let mut tx = db.transaction().unwrap();
        tx.query_one(
            "SELECT id FROM app_core_session WHERE id=$1 FOR UPDATE NOWAIT",
            &[&check.start.authorization.session_id],
        )
        .unwrap();
        tx.query_one(
            "SELECT id FROM app_core_agentrun WHERE id=$1 FOR UPDATE NOWAIT",
            &[&check.start.agent_run_id],
        )
        .unwrap();
        tx.commit().unwrap();
        check.locks_checked.fetch_add(1, Ordering::SeqCst);
    })
    .await
    .unwrap();
    loop {
        let released = probe.release.notified();
        if probe.released.load(Ordering::SeqCst) {
            break;
        }
        released.await;
    }
    if probe.case == "api_error" {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "synthetic failure".into(),
        );
    }
    if probe.case == "timeout" {
        return std::future::pending().await;
    }
    let response = crate::agent_work::work_api_client()
        .unwrap()
        .post(format!("{}/internal/agent-work/materialize", probe.api_url))
        .header("X-Internal-Token", &probe.token)
        .json(&serde_json::from_slice::<Value>(&body).unwrap())
        .send()
        .await
        .unwrap();
    let status = response.status();
    let value: Value = response.json().await.unwrap();
    probe
        .forwarded
        .lock()
        .unwrap()
        .push((status.as_u16(), value.clone()));
    if probe.case == "response_lost" {
        // Admission is durable but the caller receives no response before its timeout.
        return std::future::pending().await;
    }
    (status, value.to_string())
}

async fn drain_trigger(slots: &tokio::sync::Semaphore) {
    let permit = tokio::time::timeout(Duration::from_secs(10), slots.acquire())
        .await
        .unwrap()
        .unwrap();
    drop(permit);
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Fixture {
    live_server_url: String,
    agent_run_start: AgentRunStart,
    internal_token: String,
    database: Value,
    fail_commit: bool,
    #[serde(default)]
    fast_trigger_case: Option<String>,
}
struct SyntheticHost;
impl ExecutionHostRunner for SyntheticHost {
    fn status(&self, _: &ExecutionPolicy) -> Result<ExecutionHostStatus, ExecutionError> {
        Err(ExecutionError::HostUnavailable {
            reason: "synthetic dynamic-only host".into(),
        })
    }
    fn run_file_system_operation(
        &self,
        request: ExecutionFileSystemRequest,
    ) -> Result<ExecutionFileSystemOutput, ExecutionFileSystemError> {
        assert!(request.model_path.ends_with("AGENTS.md"));
        Err(ExecutionFileSystemError::new(
            ExecutionFileSystemErrorKind::NotFound,
            "no synthetic instructions",
        ))
    }
    fn run_host_command(
        &self,
        _: Option<&str>,
        _: ExecutionCommandRequest,
        _: Option<&ExecutionCancellationProbe>,
    ) -> Result<ExecutionHostCommandOutput, ExecutionError> {
        panic!("dispatch validation must never launch a process")
    }
}

struct SyntheticModel {
    count: AtomicUsize,
    source_run_id: String,
}

struct ConfirmationModel {
    calls: AtomicUsize,
    args: Value,
    tool_name: String,
}

impl ModelClient for ConfirmationModel {
    fn generate<'a>(
        &'a self,
        request: &'a ModelClientRequest,
    ) -> ModelClientFuture<'a, ModelClientResponse> {
        Box::pin(async move {
            assert!(request
                .prepared_prompt
                .tool_definitions
                .iter()
                .any(|tool| tool.name == "confirm_work_return"));
            let index = self.calls.fetch_add(1, Ordering::SeqCst);
            assert!(index < 2);
            Ok(ModelClientResponse {
                generate_result: GenerateResult {
                    content: if index == 1 {
                        "Confirmation committed; one Final.".into()
                    } else {
                        String::new()
                    },
                    tool_calls: if index == 0 {
                        vec![ToolCallEnvelope {
                            id: "confirm-one".into(),
                            name: self.tool_name.clone(),
                            args_json: self.args.to_string(),
                        }]
                    } else {
                        vec![]
                    },
                    continuation_reasoning_content: None,
                    reasoning_content: None,
                    input_tokens: None,
                    total_tokens: None,
                    prompt_cache_hit_tokens: None,
                    prompt_cache_miss_tokens: None,
                },
                provider_request_id: None,
                provider_latency_ms: None,
                provider_attempts: 1,
            })
        })
    }
}

#[test]
#[ignore = "Django runs the real Core/provider/fenced PostgreSQL confirmation boundary"]
fn confirm_native_return_fenced_commit() {
    let fixture: Value =
        serde_json::from_str(&std::env::var("CENTAERIS_WORK_CONFIRM_FIXTURE").unwrap()).unwrap();
    let start = serde_json::from_value::<AgentRunStart>(fixture["agentRunStart"].clone())
        .unwrap()
        .validate(fixture["signingKey"].as_str().unwrap().as_bytes())
        .unwrap();
    let url = fixture["databaseUrl"].as_str().unwrap();
    assert!(reqwest::Url::parse(url)
        .unwrap()
        .path()
        .starts_with("/test_"));
    let store = PostgresRuntimeStore::new(url).unwrap();
    let mut db = postgres::Client::connect(url, postgres::NoTls).unwrap();
    let now = crate::now_ms().unwrap();
    let session = &start.authorization.session_id;
    let run = &start.agent_run_id;
    let job = crate::agent_run_lifecycle_job_id(run).unwrap();
    store
        .schedule_runtime_job(ScheduleRuntimeJobRequest {
            job: RuntimeJobRecord {
                job_id: job.clone(),
                job_kind: "agent_run.lifecycle".into(),
                status: RuntimeJobStatus::Queued,
                run_at_ms: now,
                lease_owner: None,
                lease_expires_at_ms: None,
                heartbeat_at_ms: None,
                retry_count: 0,
                max_retries: 0,
                backoff_policy: RuntimeBackoffPolicy::default(),
                idempotency_key: format!("{job}:{}", start.authorization_digest),
                session_id: Some(session.clone()),
                branch_id: None,
                checkpoint_id: None,
                payload_ref: Some(format!("record:agent_run:{run}")),
                output_refs: vec![],
                last_error: None,
                created_at_ms: now,
                updated_at_ms: now,
            },
        })
        .unwrap();
    let owner = store
        .claim_due_runtime_jobs(ClaimDueRuntimeJobsRequest {
            now_ms: now,
            worker_id: "confirmation-fixture".into(),
            job_id: Some(job.clone()),
            job_kind: None,
            session_id: None,
            limit: 1,
            lease_ms: 120_000,
        })
        .unwrap()
        .remove(0)
        .lease_owner
        .unwrap();
    store
        .start_runtime_job(StartRuntimeJobRequest {
            job_id: job.clone(),
            lease_owner: owner.clone(),
            started_at_ms: now,
        })
        .unwrap();
    let fence = RuntimeJobLeaseFence {
        job_id: job,
        job_kind: "agent_run.lifecycle".into(),
        lease_owner: owner,
    };
    let log = store
        .session_log(
            start.authorization.workspace_id.clone(),
            session.clone(),
            start.prompt.clone(),
        )
        .with_agent_run_start(&start)
        .unwrap();
    let mut sequence = AgentRunSessionState::new(session, run).unwrap();
    let initial = crate::started_session_records(&start, &mut sequence, now).unwrap();
    log.append_session_records_with_runtime_job_lease_blocking(run, &initial, &fence)
        .unwrap();
    let registry = Arc::new(
        DynamicToolRegistry::from_contracts(crate::workspace_agent_tool_contracts(&start)).unwrap(),
    );
    assert!(
        registry.find_contract("confirm_work_return").is_some(),
        "confirmation must be registered on the actual native Core path"
    );
    let root = std::env::temp_dir();
    let binding = Arc::new(
        ExecutionHostBinding::new(
            ExecutionHostMode::Remote,
            Arc::new(SyntheticHost),
            root.clone(),
            ExecutionPolicy::workspace_write_no_network(root),
        )
        .unwrap(),
    );
    let mut tools = ToolLayer::try_new_with_skill_catalog_config_dynamic_tool_registry_and_execution_host_binding(SkillCatalogLoadConfig::default(), registry, binding).unwrap().with_session_id(session.clone());
    crate::register_workspace_agent_tool_providers(
        &mut tools,
        &start,
        fixture["liveServerUrl"].as_str().unwrap(),
        fixture["internalToken"].as_str().unwrap(),
    )
    .unwrap();
    let engine = AgentRuntime::new(
        store,
        tools,
        AgentRuntimeConfig {
            enable_prompt_compaction: false,
            ..Default::default()
        },
        ToolConcurrencyCoordinator::new(1),
    );
    let case = fixture["case"].as_str().unwrap();
    let args = if case == "other_tool" {
        json!({"body":"Complete ordinary message", "session_refs":[], "file_refs":[]})
    } else {
        json!({"notice_id": fixture["notice"]["noticeId"], "attempt_id": fixture["attemptId"]})
    };
    let model = ConfirmationModel {
        calls: AtomicUsize::new(0),
        args,
        tool_name: if case == "other_tool" {
            "send_message"
        } else {
            "confirm_work_return"
        }
        .into(),
    };
    let executor = tokio::runtime::Runtime::new().unwrap();
    let http = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .unwrap();
    let query = || {
        crate::postgres_store::run_postgres_blocking(|| {
        let response = http.post(format!("{}/internal/agent-work/query", fixture["liveServerUrl"].as_str().unwrap()))
            .header("X-Internal-Token", fixture["internalToken"].as_str().unwrap())
            .json(&json!({"schema":"workspace.agent_work.query.v1", "agentRunId":run,
                "authorizationDigest":start.authorization_digest, "coordinationSessionId":session,
                "toolCallId":"query-confirm", "sourceAgentRunId":fixture["notice"]["identity"]["sourceAgentRunId"],
                "sourceToolCallId":fixture["notice"]["identity"]["sourceToolCallId"]})).send().unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        let value: Value = response.json().unwrap();
        Ok(value["work"]["returns"].as_array().unwrap().iter().find(|item| item["notice"]["noticeId"] == fixture["notice"]["noticeId"]).unwrap().clone())
    }).unwrap()
    };
    assert_eq!(query()["handled"], false);
    let mut receipts = 0;
    let mut committed_result = None;
    let initial_input = match &start.initial_input {
        crate::contract::AgentRunStartInitialInput::HostEvent { input, .. } => {
            AgentRunInitialInput::HostEvent(input.clone())
        }
        _ => AgentRunInitialInput::UserMessage(start.prompt.clone()),
    };
    let outcome = executor.block_on(engine.process_turn_loop_online_with_model_client_stream_cancellable_and_tool_safe_point_async(
        AgentRunRequest { session_id: session.clone(), initial_turn_id: start.turn_id.clone(), initial_input,
            agent_run_identity: Some(RuntimeAgentRunIdentityV1 { agent_run_id: run.clone(), execution_id: "confirmation-execution".into(), authorization_digest: start.authorization_digest.clone() }),
            runtime_scope: centaeris_core::model::prompt::PromptCompactionScopeV1::main(), resume_from_turn_id: None, auto_continue_after_resume_wait: None },
        &model, &EmptyModelSessionConfigStore::new(), &mut |_| {}, &|| Ok(None), &mut |point| {
            let mut next = sequence.clone();
            let mut records = match point {
                ToolSafePoint::DurableToolCall { turn_id, call, provider_id, tool_contract_digest, recorded_at_ms, .. } =>
                    crate::tool_call_session_records(&start, None, &turn_id, &call, &mut next, crate::ToolCallRecordContext { provider_id: &provider_id, tool_contract_digest: &tool_contract_digest, created_at_ms: recorded_at_ms })?,
                ToolSafePoint::DurableReceipt { turn_id, call, result, .. } => {
                    receipts += 1;
                    assert_eq!(result.status, "ok");
                    if case != "membership_rejoined" {
                        assert_eq!(query()["handled"], false, "provider success and committed call alone do not handle");
                    }
                    crate::postgres_store::run_postgres_blocking(|| { match case {
                        "commit_failure" => { db.batch_execute("CREATE FUNCTION reject_confirm_fixture() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.payload->>'type'='tool_result' THEN RAISE EXCEPTION 'synthetic confirmation commit failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_confirm_fixture AFTER INSERT ON app_core_sessionevent FOR EACH ROW EXECUTE FUNCTION reject_confirm_fixture();").unwrap(); }
                        "role_revoked" => { db.execute("UPDATE app_core_workspacemembership SET role='viewer' WHERE id=(SELECT membership_ref FROM app_core_agentrun WHERE id=$1)", &[run]).unwrap(); }
                        "child_archived" => { db.execute("UPDATE app_core_session SET status='archived' WHERE id=$1", &[&fixture["notice"]["identity"]["workSessionId"].as_str().unwrap()]).unwrap(); }
                        "user_inactive" => { db.execute("UPDATE auth_user SET is_active=false WHERE id=(SELECT user_id FROM app_core_agentrun WHERE id=$1)", &[run]).unwrap(); }
                        _ => {}
                    }; Ok(()) })?;
                    crate::tool_result_session_records(&start, None, &turn_id, &call, &result, &mut next, crate::now_ms()?)?
                }
                _ => return Ok(()),
            };
            let mut actual_fence = fence.clone();
            if receipts > 0 && case == "lease_failure" { actual_fence.lease_owner = "stale-confirm-owner".into(); }
            if receipts > 0 && case == "forged_result" {
                let record = records.iter_mut().find(|item| item.event.event_type == centaeris_core::session::SessionRecordType::ToolResult).unwrap();
                record.event.payload["modelContent"] = json!("forged confirmation success");
            }
            log.append_session_records_with_runtime_job_lease_blocking(run, &records, &actual_fence)?;
            if records.iter().any(|item| item.event.event_type == centaeris_core::session::SessionRecordType::ToolResult) {
                committed_result = Some(records.clone());
            }
            sequence = next;
            Ok(())
        }));
    assert_eq!(receipts, 1);
    let count: i64 = db.query_one("SELECT count(*) FROM app_core_sessionevent WHERE agent_run_id=$1 AND payload->>'type'='tool_result' AND payload->'payload'->>'toolName'='confirm_work_return'", &[run]).unwrap().get(0);
    let success = matches!(case, "success" | "active_success");
    assert_eq!(count, i64::from(success));
    if success || case == "other_tool" {
        assert_eq!(outcome.unwrap().stop, AgentRunStop::Finalized);
        assert_eq!(
            model.calls.load(Ordering::SeqCst),
            2,
            "ContinueTurn reaches Final"
        );
        let view = query();
        assert_eq!(view["handled"], success);
        log.append_session_records_with_runtime_job_lease_blocking(
            run,
            committed_result.as_ref().unwrap(),
            &fence,
        )
        .unwrap();
        let replay_count: i64 = db.query_one("SELECT count(*) FROM app_core_sessionevent WHERE agent_run_id=$1 AND payload->>'type'='tool_result'", &[run]).unwrap().get(0);
        assert_eq!(replay_count, 1, "replay adds no result");
        assert_eq!(query(), view, "query replay retains the committed source");
    } else {
        assert!(
            outcome.is_err(),
            "provider success must not hide failed commit: {case}"
        );
    }
    if case == "commit_failure" {
        db.batch_execute("DROP TRIGGER reject_confirm_fixture ON app_core_sessionevent; DROP FUNCTION reject_confirm_fixture();").unwrap();
    }
    println!("confirmation-core-pg-ok:{case}");
}
impl ModelClient for SyntheticModel {
    fn generate<'a>(
        &'a self,
        request: &'a ModelClientRequest,
    ) -> ModelClientFuture<'a, ModelClientResponse> {
        Box::pin(async move {
            assert!(request
                .prepared_prompt
                .tool_definitions
                .iter()
                .any(|tool| tool.name == "get_work_request"));
            let index = self.count.fetch_add(1, Ordering::SeqCst);
            assert!(index < 3);
            Ok(ModelClientResponse {
                generate_result: GenerateResult {
                    content: if index == 2 {
                        "One Final after dispatch validation.".into()
                    } else {
                        String::new()
                    },
                    tool_calls: if index == 0 {
                        vec![ToolCallEnvelope { id: "dispatch-one".into(), name: "dispatch_work".into(), args_json: json!({"objective":"Complete native work objective", "session_refs":[], "file_refs":[]}).to_string() }]
                    } else if index == 1 {
                        vec![ToolCallEnvelope { id: "query-one".into(), name: "get_work_request".into(), args_json: json!({"source_agent_run_id": self.source_run_id, "source_tool_call_id":"dispatch-one"}).to_string() }]
                    } else {
                        vec![]
                    },
                    continuation_reasoning_content: None,
                    reasoning_content: None,
                    input_tokens: None,
                    total_tokens: None,
                    prompt_cache_hit_tokens: None,
                    prompt_cache_miss_tokens: None,
                },
                provider_request_id: None,
                provider_latency_ms: None,
                provider_attempts: 1,
            })
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RegistrationFixture {
    live_server_url: String,
    internal_token: String,
    signing_key: String,
    runtime_store_root: std::path::PathBuf,
    query_contract_digest: String,
    cases: Vec<RegistrationCase>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RegistrationCase {
    kind: String,
    agent_run_start: AgentRunStart,
}

struct RegistrationModel {
    captured: std::sync::Mutex<Vec<Vec<String>>>,
}

impl ModelClient for RegistrationModel {
    fn generate<'a>(
        &'a self,
        request: &'a ModelClientRequest,
    ) -> ModelClientFuture<'a, ModelClientResponse> {
        Box::pin(async move {
            self.captured.lock().unwrap().push(
                request
                    .prepared_prompt
                    .tool_definitions
                    .iter()
                    .map(|tool| tool.name.clone())
                    .collect(),
            );
            Ok(ModelClientResponse {
                generate_result: GenerateResult {
                    content: "Registered tools observed; Final.".into(),
                    tool_calls: vec![],
                    continuation_reasoning_content: None,
                    reasoning_content: None,
                    input_tokens: None,
                    total_tokens: None,
                    prompt_cache_hit_tokens: None,
                    prompt_cache_miss_tokens: None,
                },
                provider_request_id: None,
                provider_latency_ms: None,
                provider_attempts: 1,
            })
        })
    }
}

#[test]
#[ignore = "executed by Django with real hosted Run start fixtures"]
fn django_production_registration_model_tools() {
    let fixture: RegistrationFixture =
        serde_json::from_str(&std::env::var("CENTAERIS_AGENT_REGISTRATION_FIXTURE").unwrap())
            .unwrap();
    let executor = tokio::runtime::Runtime::new().unwrap();
    let query_contract = DynamicToolRegistry::from_contracts(vec![work_query_tool_contract()])
        .unwrap()
        .find_contract("get_work_request")
        .unwrap();
    assert_eq!(
        query_contract.contract_digest().unwrap(),
        fixture.query_contract_digest
    );
    assert_eq!(
        fixture
            .cases
            .iter()
            .map(|case| case.kind.as_str())
            .collect::<Vec<_>>(),
        ["ordinary", "managed", "delegated", "native"]
    );
    for case in fixture.cases {
        let start = case
            .agent_run_start
            .validate(fixture.signing_key.as_bytes())
            .unwrap();
        let root = fixture.runtime_store_root.join(&case.kind);
        std::fs::create_dir(&root).unwrap();
        let binding = Arc::new(
            ExecutionHostBinding::new(
                ExecutionHostMode::Remote,
                Arc::new(SyntheticHost),
                root.clone(),
                ExecutionPolicy::workspace_write_no_network(&root),
            )
            .unwrap(),
        );
        let registry = Arc::new(
            DynamicToolRegistry::from_contracts(crate::workspace_agent_tool_contracts(&start))
                .unwrap(),
        );
        let mut tools = ToolLayer::try_new_with_skill_catalog_config_dynamic_tool_registry_and_execution_host_binding(SkillCatalogLoadConfig::default(), registry, binding).unwrap().with_session_id(start.authorization.session_id.clone());
        crate::register_workspace_agent_tool_providers(
            &mut tools,
            &start,
            &fixture.live_server_url,
            &fixture.internal_token,
        )
        .unwrap();
        let store =
            centaeris_runtime_sqlite::SqliteRuntimeStore::new(root.join("runtime.sqlite")).unwrap();
        let engine = AgentRuntime::new(
            store,
            tools,
            AgentRuntimeConfig {
                enable_prompt_compaction: false,
                ..Default::default()
            },
            ToolConcurrencyCoordinator::new(1),
        );
        let model = RegistrationModel {
            captured: std::sync::Mutex::new(vec![]),
        };
        let outcome = executor.block_on(engine.process_turn_loop_online_with_model_client_stream_cancellable_and_tool_safe_point_async(
            AgentRunRequest { session_id: start.authorization.session_id.clone(), initial_turn_id: start.turn_id.clone(), initial_input: AgentRunInitialInput::UserMessage(start.prompt.clone()), agent_run_identity: None, runtime_scope: centaeris_core::model::prompt::PromptCompactionScopeV1::main(), resume_from_turn_id: None, auto_continue_after_resume_wait: None },
            &model, &EmptyModelSessionConfigStore::new(), &mut |_| {}, &|| Ok(None), &mut |_| Ok(()))).unwrap();
        assert_eq!(outcome.stop, AgentRunStop::Finalized);
        let captured = model.captured.lock().unwrap();
        assert_eq!(captured.len(), 1);
        let dispatch_count = captured[0]
            .iter()
            .filter(|name| name.as_str() == "dispatch_work")
            .count();
        assert_eq!(
            dispatch_count,
            usize::from(case.kind == "native"),
            "{}: {:?}",
            case.kind,
            captured[0]
        );
        assert_eq!(
            captured[0]
                .iter()
                .filter(|name| name.as_str() == "get_work_request")
                .count(),
            usize::from(case.kind == "native")
        );
        assert_eq!(
            captured[0]
                .iter()
                .filter(|name| name.as_str() == "confirm_work_return")
                .count(),
            usize::from(case.kind == "native")
        );
        assert!(
            captured[0].iter().any(|name| name == "read"),
            "observe the actual Core tool list"
        );
        println!(
            "registered model tools: {}",
            json!({"kind": case.kind, "tools": captured[0]})
        );
    }
    println!("production-registration-model-tools-ok");
}

#[test]
#[ignore = "executed by Django LiveServerTestCase against its disposable database"]
fn django_work_commit_boundary() {
    let fixture: Fixture =
        serde_json::from_str(&std::env::var("CENTAERIS_AGENT_WORK_FIXTURE").unwrap()).unwrap();
    let start = &fixture.agent_run_start;
    if let Some(case) = &fixture.fast_trigger_case {
        println!("fast-trigger-case: {case}");
    }
    assert_eq!(
        start.native_coordination_session_id.as_deref(),
        Some(start.authorization.session_id.as_str())
    );
    let mut url = reqwest::Url::parse("postgresql://localhost/").unwrap();
    url.set_host(Some(fixture.database["HOST"].as_str().unwrap()))
        .unwrap();
    url.set_port(Some(
        fixture.database["PORT"].as_str().unwrap().parse().unwrap(),
    ))
    .unwrap();
    url.set_username(fixture.database["USER"].as_str().unwrap())
        .unwrap();
    url.set_password(Some(fixture.database["PASSWORD"].as_str().unwrap()))
        .unwrap();
    let database = fixture.database["NAME"].as_str().unwrap();
    assert!(database.starts_with("test_"));
    url.set_path(database);
    let mut db = postgres::Client::connect(url.as_str(), postgres::NoTls).unwrap();
    let store = PostgresRuntimeStore::new(url.as_str()).unwrap();
    let now = crate::now_ms().unwrap();
    let session = &start.authorization.session_id;
    let run = &start.agent_run_id;
    let job = format!("agent_run.lifecycle:{run}");
    store
        .schedule_runtime_job(ScheduleRuntimeJobRequest {
            job: RuntimeJobRecord {
                job_id: job.clone(),
                job_kind: "agent_run.lifecycle".into(),
                status: RuntimeJobStatus::Queued,
                run_at_ms: now,
                lease_owner: None,
                lease_expires_at_ms: None,
                heartbeat_at_ms: None,
                retry_count: 0,
                max_retries: 0,
                backoff_policy: RuntimeBackoffPolicy::default(),
                idempotency_key: format!("{job}:{}", start.authorization_digest),
                session_id: Some(session.clone()),
                branch_id: None,
                checkpoint_id: None,
                payload_ref: Some(format!("record:agent_run:{run}")),
                output_refs: vec![],
                last_error: None,
                created_at_ms: now,
                updated_at_ms: now,
            },
        })
        .unwrap();
    let owner = store
        .claim_due_runtime_jobs(ClaimDueRuntimeJobsRequest {
            now_ms: now,
            worker_id: "dispatch-validation-worker".into(),
            job_id: Some(job.clone()),
            job_kind: None,
            session_id: None,
            limit: 1,
            lease_ms: 120_000,
        })
        .unwrap()
        .remove(0)
        .lease_owner
        .unwrap();
    store
        .start_runtime_job(StartRuntimeJobRequest {
            job_id: job.clone(),
            lease_owner: owner.clone(),
            started_at_ms: now,
        })
        .unwrap();
    let fence = RuntimeJobLeaseFence {
        job_id: job.clone(),
        job_kind: "agent_run.lifecycle".into(),
        lease_owner: owner.clone(),
    };
    let log = store.session_log(
        start.authorization.workspace_id.clone(),
        session.clone(),
        start.prompt.clone(),
    );
    let mut sequence = AgentRunSessionState::new(session, run).unwrap();
    let initial = crate::started_session_records(start, &mut sequence, now).unwrap();
    log.append_session_records_with_runtime_job_lease_blocking(run, &initial, &fence)
        .unwrap();

    let registry = Arc::new(
        DynamicToolRegistry::from_contracts(crate::workspace_agent_tool_contracts(start)).unwrap(),
    );
    let registry_for_receipts = registry.clone();
    assert_eq!(
        registry
            .find_contract("dispatch_work")
            .unwrap()
            .turn_behavior,
        ToolTurnBehavior::ContinueTurn
    );
    let root = std::env::temp_dir();
    let binding = Arc::new(
        ExecutionHostBinding::new(
            ExecutionHostMode::Remote,
            Arc::new(SyntheticHost),
            root.clone(),
            ExecutionPolicy::workspace_write_no_network(root),
        )
        .unwrap(),
    );
    let mut tools = ToolLayer::try_new_with_skill_catalog_config_dynamic_tool_registry_and_execution_host_binding(SkillCatalogLoadConfig::default(), registry, binding).unwrap().with_session_id(session.clone());
    crate::register_workspace_agent_tool_providers(
        &mut tools,
        start,
        &fixture.live_server_url,
        &fixture.internal_token,
    )
    .unwrap();
    let engine = AgentRuntime::new(
        store.clone(),
        tools,
        AgentRuntimeConfig {
            enable_prompt_compaction: false,
            ..Default::default()
        },
        ToolConcurrencyCoordinator::new(1),
    );
    let model = SyntheticModel {
        count: AtomicUsize::new(0),
        source_run_id: run.clone(),
    };
    let executor = tokio::runtime::Runtime::new().unwrap();
    let slots = Arc::new(tokio::sync::Semaphore::new(1));
    let probe = fixture.fast_trigger_case.as_ref().map(|case| {
        Arc::new(TriggerProbe {
            case: case.clone(),
            calls: AtomicUsize::new(0),
            locks_checked: AtomicUsize::new(0),
            released: AtomicBool::new(false),
            release: tokio::sync::Notify::new(),
            forwarded: Mutex::new(vec![]),
            api_url: fixture.live_server_url.clone(),
            token: fixture.internal_token.clone(),
            database_url: url.to_string(),
            log: log.clone(),
            initial: initial.clone(),
            fence: fence.clone(),
            start: start.clone(),
        })
    });
    let proxy = probe.as_ref().map(|probe| {
        executor.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let app = axum::Router::new()
                .route(
                    "/internal/agent-work/materialize",
                    axum::routing::post(trigger_proxy),
                )
                .with_state(probe.clone());
            let task = tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            (url, task)
        })
    });
    let trigger = proxy.as_ref().map(|(url, _)| {
        let timeout = match fixture.fast_trigger_case.as_deref() {
            Some("timeout") => Duration::from_millis(200),
            Some("response_lost") => Duration::from_secs(2),
            _ => Duration::from_secs(30),
        };
        crate::agent_work_trigger::WorkCommitTrigger::new(
            executor.handle().clone(),
            url,
            &fixture.internal_token,
        )
        .unwrap()
        .with_test_limits(
            reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(timeout)
                .build()
                .unwrap(),
            slots.clone(),
        )
    });
    let full_capacity = (fixture.fast_trigger_case.as_deref() == Some("capacity"))
        .then(|| slots.clone().try_acquire_owned().unwrap());
    let fence_failure = fixture.fast_trigger_case.as_deref() == Some("fence_failure");
    let source_failure = fixture.fail_commit || fence_failure;
    let mut dispatch_commit: Option<SessionCommitReceipt> = None;
    let mut receipts = 0;
    if fixture.fail_commit {
        db.batch_execute("CREATE FUNCTION reject_dispatch_fixture() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.payload->>'type'='tool_result' THEN RAISE EXCEPTION 'synthetic dispatch commit failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_dispatch_fixture AFTER INSERT ON app_core_sessionevent FOR EACH ROW EXECUTE FUNCTION reject_dispatch_fixture();").unwrap();
    }
    let outcome = executor.block_on(engine.process_turn_loop_online_with_model_client_stream_cancellable_and_tool_safe_point_async(
        AgentRunRequest { session_id: session.clone(), initial_turn_id: start.turn_id.clone(), initial_input: AgentRunInitialInput::UserMessage(start.prompt.clone()), agent_run_identity: Some(RuntimeAgentRunIdentityV1 { agent_run_id: run.clone(), execution_id: "dispatch-validation-execution".into(), authorization_digest: start.authorization_digest.clone() }), runtime_scope: centaeris_core::model::prompt::PromptCompactionScopeV1::main(), resume_from_turn_id: None, auto_continue_after_resume_wait: None },
        &model, &EmptyModelSessionConfigStore::new(), &mut |_| {}, &|| Ok(None), &mut |point| {
            let mut next = sequence.clone();
            let records = match point {
                ToolSafePoint::DurableToolCall { turn_id, call, provider_id, tool_contract_digest, recorded_at_ms, .. } => {
                    assert_eq!(provider_id, PROVIDER_ID);
                    assert_eq!(tool_contract_digest, registry_for_receipts.find_contract(&call.name).unwrap().contract_digest().unwrap());
                    crate::tool_call_session_records(start, None, &turn_id, &call, &mut next, crate::ToolCallRecordContext { provider_id: &provider_id, tool_contract_digest: &tool_contract_digest, created_at_ms: recorded_at_ms })?
                }
                ToolSafePoint::DurableReceipt { turn_id, call, result, .. } => {
                    receipts += 1;
                    assert_eq!(result.status, "ok");
                    if call.id == "dispatch-one" {
                        assert!(result.content.contains("awaiting Session commit, not admitted"));
                    } else {
                        assert_eq!(call.id, "query-one");
                        let queried: Value = serde_json::from_str(&result.content).unwrap();
                        assert_eq!(queried["status"], "pending");
                        assert_eq!(queried["source"]["sourceAgentRunId"], *run);
                        assert_eq!(queried["source"]["sourceToolCallId"], "dispatch-one");
                        assert!(queried["operation"].is_null());
                    }
                    crate::tool_result_session_records(start, None, &turn_id, &call, &result, &mut next, crate::now_ms()?)?
                }
                _ => return Ok(()),
            };
            let mut commit_fence = fence.clone();
            if fence_failure && receipts == 1 && records.iter().any(|record| record.event.event_type == centaeris_core::session::SessionRecordType::ToolResult) {
                commit_fence.lease_owner = "wrong-fixture-owner".into();
            }
            let committed = crate::commit_agent_tool_records(&log, start, &records, None, &commit_fence, trigger.as_ref())?;
            if committed.records.iter().any(|record| record.event.payload["toolName"] == "dispatch_work"
                && record.event.event_type == centaeris_core::session::SessionRecordType::ToolResult) {
                dispatch_commit = Some(committed);
            }
            sequence = next;
            Ok(())
        }));
    assert_eq!(receipts, if source_failure { 1 } else { 2 });
    if source_failure {
        assert!(
            outcome.is_err(),
            "provider validation must not hide Session commit failure"
        );
        assert_eq!(model.count.load(Ordering::SeqCst), 1);
        if fixture.fail_commit {
            db.batch_execute("DROP TRIGGER reject_dispatch_fixture ON app_core_sessionevent; DROP FUNCTION reject_dispatch_fixture();").unwrap();
        }
    } else {
        assert_eq!(outcome.unwrap().stop, AgentRunStop::Finalized);
        assert_eq!(
            model.count.load(Ordering::SeqCst),
            3,
            "ContinueTurn must reach the next model request"
        );
    }
    let results: i64 = db.query_one("SELECT count(*) FROM app_core_sessionevent WHERE session_id=$1 AND payload->>'type'='tool_result'", &[session]).unwrap().get(0);
    assert_eq!(results, if source_failure { 0 } else { 2 });
    if let Some(probe) = &probe {
        assert!(
            probe.forwarded.lock().unwrap().is_empty(),
            "Core reaches Final while admission HTTP is held"
        );
        probe.released.store(true, Ordering::SeqCst);
        probe.release.notify_waiters();
        drop(full_capacity);
        executor.block_on(drain_trigger(&slots));
        let duplicate = matches!(probe.case.as_str(), "duplicate" | "response_lost");
        if duplicate {
            trigger
                .as_ref()
                .unwrap()
                .after_commit(start, dispatch_commit.as_ref().unwrap());
            executor.block_on(drain_trigger(&slots));
        }
        let expected = if source_failure || probe.case == "capacity" {
            0
        } else if duplicate {
            2
        } else {
            1
        };
        assert_eq!(probe.calls.load(Ordering::SeqCst), expected);
        assert_eq!(probe.locks_checked.load(Ordering::SeqCst), expected);
        let forwarded = probe.forwarded.lock().unwrap();
        if matches!(
            probe.case.as_str(),
            "success" | "duplicate" | "response_lost"
        ) {
            assert_eq!(forwarded.len(), if duplicate { 2 } else { 1 });
            assert_eq!(forwarded[0].0, 201);
            if duplicate {
                assert_eq!(forwarded[1].0, 200);
                assert_eq!(forwarded[0].1, forwarded[1].1);
            }
        } else {
            assert!(forwarded.is_empty());
        }
        println!(
            "work-fast-trigger-core-ok: {} httpCalls={expected} lockChecks={expected}",
            probe.case
        );
    }
    if let Some((_, task)) = proxy {
        task.abort();
    }
    println!(
        "work-provider-core-commit-boundary-ok: failCommit={}",
        fixture.fail_commit
    );
}
