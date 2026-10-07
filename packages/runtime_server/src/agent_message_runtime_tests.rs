use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::contract::AgentRunStart;
use crate::postgres_store::{run_postgres_blocking, PostgresRuntimeStore};
use centaeris_core::execution::*;
use centaeris_core::extension::skills::SkillCatalogLoadConfig;
use centaeris_core::model::prepared_prompt::ModelMessageRoleV1;
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
    ClaimDueRuntimeJobsRequest, CompleteRuntimeJobRequest, RuntimeBackoffPolicy, RuntimeJobRecord,
    RuntimeJobStatus, RuntimeJobStorePort, ScheduleRuntimeJobRequest, StartRuntimeJobRequest,
};
use centaeris_core::session::{
    AgentRunSessionState, RuntimeJobLeaseFence, RUNTIME_JOB_LEASE_FENCE_REJECTED,
};
use centaeris_core::tool::layer::ToolLayer;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Fixture {
    live_server_url: String,
    agent_run_start: AgentRunStart,
    internal_token: String,
    cookie_name: String,
    cookie_value: String,
    work_session_id: String,
    file_ref: String,
    library_object_id: String,
    membership_id: String,
    message_contract_digest: String,
    first_body: String,
    second_body: String,
    database: Value,
}

#[derive(Clone)]
struct History {
    client: reqwest::blocking::Client,
    url: String,
    cookie: String,
}

impl History {
    fn status(&self) -> u16 {
        run_postgres_blocking(|| {
            Ok(self
                .client
                .get(&self.url)
                .header("Cookie", &self.cookie)
                .send()
                .unwrap()
                .status()
                .as_u16())
        })
        .unwrap()
    }

    fn read(&self) -> Value {
        run_postgres_blocking(|| {
            let response = self
                .client
                .get(&self.url)
                .header("Cookie", &self.cookie)
                .send()
                .unwrap();
            assert_eq!(response.status().as_u16(), 200);
            Ok(response.json::<Value>().unwrap())
        })
        .unwrap()
    }
    fn bubbles(&self) -> Vec<Value> {
        self.read()["messages"].as_array().unwrap().clone()
    }
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
        panic!("message acceptance must never launch a process")
    }
}

struct ProbeProvider;
impl DynamicToolProvider for ProbeProvider {
    fn provider_id(&self) -> &str {
        "test.probe"
    }
    fn execute<'a>(
        &'a self,
        _: DynamicToolProviderRequest,
    ) -> Pin<Box<dyn Future<Output = Result<DynamicToolProviderResponse, String>> + Send + 'a>>
    {
        Box::pin(async {
            Ok(DynamicToolProviderResponse {
                content: "inspected".into(),
                details: json!({}),
                is_error: false,
                facts: vec![],
                transition_reason: None,
            })
        })
    }
}

struct SyntheticModel {
    requests: Mutex<Vec<ModelClientRequest>>,
    count: AtomicUsize,
    history: History,
    first: String,
    second: String,
    work_session: String,
    file_ref: String,
    revocation_window: bool,
}

impl SyntheticModel {
    fn call(&self, id: &str, body: &str) -> ToolCallEnvelope {
        ToolCallEnvelope {
            id: id.into(),
            name: "send_message".into(),
            args_json: json!({"body":body,
            "session_refs":[self.work_session],"file_refs":[self.file_ref]})
            .to_string(),
        }
    }
    fn assert_context(&self) {
        let requests = self.requests.lock().unwrap();
        assert_eq!(requests.len(), 4);
        for (index, request) in requests.iter().enumerate() {
            request.prepared_prompt.validate().unwrap();
            let calls: Vec<_> = request
                .prepared_prompt
                .messages
                .iter()
                .flat_map(|message| &message.tool_calls)
                .collect();
            let results: Vec<_> = request
                .prepared_prompt
                .messages
                .iter()
                .filter(|message| message.role == ModelMessageRoleV1::Tool)
                .collect();
            assert_eq!(calls.len(), index);
            assert_eq!(results.len(), index);
            for (call, result) in calls.iter().zip(results) {
                assert_eq!(result.tool_call_id.as_deref(), Some(call.id.as_str()));
                if call.name == "send_message" {
                    let actual: Value = serde_json::from_str(&call.args_json).unwrap();
                    let expected = self.call(
                        &call.id,
                        if call.id == "message-one" {
                            &self.first
                        } else {
                            &self.second
                        },
                    );
                    assert_eq!(
                        actual,
                        serde_json::from_str::<Value>(&expected.args_json).unwrap()
                    );
                }
            }
        }
    }
}

impl ModelClient for SyntheticModel {
    fn generate<'a>(
        &'a self,
        request: &'a ModelClientRequest,
    ) -> ModelClientFuture<'a, ModelClientResponse> {
        Box::pin(async move {
            self.requests.lock().unwrap().push(request.clone());
            let index = self.count.fetch_add(1, Ordering::SeqCst);
            let bubbles = if self.revocation_window && index > 0 {
                assert_eq!(self.history.status(), 404);
                vec![]
            } else {
                self.history.bubbles()
            };
            assert_eq!(
                bubbles.len(),
                if self.revocation_window {
                    0
                } else {
                    [0, 1, 1, 2][index]
                }
            );
            if !bubbles.is_empty() {
                assert_eq!(bubbles[0]["body"], self.first);
                assert_eq!(bubbles[0]["sessionRefs"], json!([self.work_session]));
                assert_eq!(bubbles[0]["fileRefs"], json!([self.file_ref]));
            }
            let calls = match index {
                0 => vec![self.call("message-one", &self.first)],
                1 => vec![ToolCallEnvelope {
                    id: "ordinary-probe".into(),
                    name: "ordinary_probe".into(),
                    args_json: "{}".into(),
                }],
                2 => vec![self.call("message-two", &self.second)],
                3 => vec![],
                _ => panic!("unexpected model request"),
            };
            Ok(ModelClientResponse {
                generate_result: GenerateResult {
                    content: if calls.is_empty() {
                        "Completed without repeating posted messages.".into()
                    } else {
                        String::new()
                    },
                    tool_calls: calls,
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
#[ignore = "executed by Django LiveServerTestCase against its migrated disposable database"]
fn django_message_runtime_contract() {
    run_message_runtime_contract(false);
}

#[test]
#[ignore = "executed by Django's validation/revocation barrier against its disposable database"]
fn django_message_revocation_commit_window() {
    run_message_runtime_contract(true);
}

fn run_message_runtime_contract(revocation_window: bool) {
    let fixture: Fixture =
        serde_json::from_str(&std::env::var("CENTAERIS_AGENT_MESSAGE_FIXTURE").unwrap()).unwrap();
    let start = &fixture.agent_run_start;
    assert_eq!(
        start.coordination_session_id.as_deref(),
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
            worker_id: "message-contract-worker".into(),
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
    let log = store
        .session_log(
            start.authorization.workspace_id.clone(),
            session.clone(),
            start.prompt.clone(),
        )
        .with_agent_run_start(start)
        .unwrap();
    let mut sequence = AgentRunSessionState::new(session, run).unwrap();
    let initial = crate::started_session_records(start, &mut sequence, now).unwrap();
    assert_ne!(run, &start.turn_id, "hosted Run and Turn identities differ");
    let mut changed_identity = initial.clone();
    changed_identity[1].event.payload["messageId"] = json!(format!("message:{run}:user"));
    assert_eq!(
        log.append_session_records_with_runtime_job_lease_blocking(run, &changed_identity, &fence)
            .unwrap_err(),
        "user message does not match accepted prompt"
    );
    let mut changed_turn = changed_identity.clone();
    for record in &mut changed_turn {
        record.event.turn_id = Some(run.clone());
    }
    assert_eq!(
        log.append_session_records_with_runtime_job_lease_blocking(run, &changed_turn, &fence)
            .unwrap_err(),
        "user message does not match accepted prompt"
    );
    log.append_session_records_with_runtime_job_lease_blocking(run, &initial, &fence)
        .unwrap();
    assert_eq!(
        initial[1].event.payload["messageId"],
        format!("message:{}:user", start.turn_id)
    );
    for record in &initial {
        assert_eq!(
            record.event.turn_id.as_deref(),
            Some(start.turn_id.as_str())
        );
        assert_eq!(record.event.agent_run_id.as_deref(), Some(run.as_str()));
    }
    // Lost acknowledgements and a reopened writer retain the accepted identity.
    store
        .session_log(
            start.authorization.workspace_id.clone(),
            session.clone(),
            start.prompt.clone(),
        )
        .append_session_records_with_runtime_job_lease_blocking(run, &initial, &fence)
        .unwrap();
    let committed = db
        .query(
            "SELECT payload::text FROM app_core_sessionevent WHERE agent_run_id=$1 ORDER BY agent_run_sequence",
            &[run],
        )
        .unwrap();
    assert_eq!(committed.len(), initial.len());
    let mut restored = AgentRunSessionState::new(session, run).unwrap();
    for row in committed {
        let wire: Value = serde_json::from_str(row.get::<_, String>(0).as_str()).unwrap();
        restored
            .restore(centaeris_core::session::parse_wire_record(&wire).unwrap())
            .unwrap();
    }
    assert_eq!(restored.next_sequence(), sequence.next_sequence());
    sequence = restored;
    let history = History {
        client: reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .unwrap(),
        url: format!(
            "{}/api/agents/{}/messages",
            fixture.live_server_url, start.authorization.agent_id
        ),
        cookie: format!("{}={}", fixture.cookie_name, fixture.cookie_value),
    };
    assert!(history.bubbles().is_empty());
    let provider = Arc::new(
        WorkspaceAgentMessageProvider::new(
            fixture.live_server_url.clone(),
            fixture.internal_token.clone(),
            &start.authorization,
            start.authorization_digest.clone(),
        )
        .unwrap(),
    );
    let mut probe = message_tool_contract();
    probe.name = "ordinary_probe".into();
    probe.provider_id = "test.probe".into();
    probe.summary = "Inspect synthetic evidence.".into();
    probe.input_schema = json!({"type":"object","properties":{},"additionalProperties":false});
    let registry = Arc::new(
        DynamicToolRegistry::from_contracts(vec![message_tool_contract(), probe]).unwrap(),
    );
    assert_eq!(
        registry
            .find_contract("send_message")
            .unwrap()
            .contract_digest()
            .unwrap(),
        fixture.message_contract_digest
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
    let mut tools=ToolLayer::try_new_with_skill_catalog_config_dynamic_tool_registry_and_execution_host_binding(
        SkillCatalogLoadConfig::default(),registry,binding).unwrap().with_session_id(session.clone());
    tools
        .register_dynamic_tool_provider(provider.clone())
        .unwrap();
    tools
        .register_dynamic_tool_provider(Arc::new(ProbeProvider))
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
        requests: Mutex::new(vec![]),
        count: AtomicUsize::new(0),
        history: history.clone(),
        first: fixture.first_body.clone(),
        second: fixture.second_body.clone(),
        work_session: fixture.work_session_id.clone(),
        file_ref: fixture.file_ref.clone(),
        revocation_window,
    };
    let mut batches = vec![];
    let executor = tokio::runtime::Runtime::new().unwrap();
    let result=executor.block_on(engine.process_turn_loop_online_with_model_client_stream_cancellable_and_tool_safe_point_async(
        AgentRunRequest {session_id:session.clone(),initial_turn_id:start.turn_id.clone(),initial_input:AgentRunInitialInput::UserMessage(start.prompt.clone()),
            agent_run_identity:Some(RuntimeAgentRunIdentityV1 {agent_run_id:run.clone(),execution_id:"synthetic-message-execution".into(),
                authorization_digest:start.authorization_digest.clone()}),
            runtime_scope:centaeris_core::model::prompt::PromptCompactionScopeV1::main(),resume_from_turn_id:None,auto_continue_after_resume_wait:None},
        &model,&EmptyModelSessionConfigStore::new(),&mut |_| {},&|| Ok(None),&mut |point| {
            let mut next=sequence.clone();
            let records=match point {
                ToolSafePoint::DurableToolCall {turn_id,call,provider_id,tool_contract_digest,recorded_at_ms,..} =>
                    crate::tool_call_session_records(start,None,&turn_id,&call,&mut next,crate::ToolCallRecordContext {
                        provider_id:&provider_id,tool_contract_digest:&tool_contract_digest,created_at_ms:recorded_at_ms})?,
                ToolSafePoint::DurableReceipt {turn_id,call,result,..} => {
                    assert_eq!(result.status,if revocation_window && call.id=="message-two" {"error"} else {"ok"});
                    let before=if revocation_window {assert_eq!(history.status(),404);vec![]} else {history.bubbles()};
                    assert!(!before.iter().any(|bubble| bubble["toolCallId"]==call.id),"provider receipt is not delivery");
                    let records=crate::tool_result_session_records(start,None,&turn_id,&call,&result,&mut next,crate::now_ms()?)?;
                    if call.id=="message-one" && !revocation_window {
                        run_postgres_blocking(|| {
                            db.batch_execute("CREATE FUNCTION reject_message_fixture() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.payload->>'type'='tool_result' THEN RAISE EXCEPTION 'synthetic commit failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_message_fixture AFTER INSERT ON app_core_sessionevent FOR EACH ROW EXECUTE FUNCTION reject_message_fixture();").unwrap();
                            Ok(())
                        })?;
                        assert!(log.append_session_records_with_runtime_job_lease_blocking(run,&records,&fence).is_err());
                        assert_eq!(history.bubbles(),before);
                        run_postgres_blocking(|| {db.batch_execute("DROP TRIGGER reject_message_fixture ON app_core_sessionevent; DROP FUNCTION reject_message_fixture();").unwrap();Ok(())})?;
                    }
                    if call.id=="message-two" && !revocation_window {
                        run_postgres_blocking(|| {db.execute("UPDATE runtime.runtime_jobs SET lease_owner='replacement-message-worker' WHERE job_id=$1",&[&job]).unwrap();Ok(())})?;
                        assert_eq!(log.append_session_records_with_runtime_job_lease_blocking(run,&records,&fence).unwrap_err(),RUNTIME_JOB_LEASE_FENCE_REJECTED);
                        assert_eq!(history.bubbles(),before);
                        run_postgres_blocking(|| {db.execute("UPDATE runtime.runtime_jobs SET lease_owner=$1 WHERE job_id=$2",&[&owner,&job]).unwrap();Ok(())})?;
                    }
                    records
                }
                _ => return Ok(()),
            };
            log.append_session_records_with_runtime_job_lease_blocking(run,&records,&fence)?;
            sequence=next;batches.push(records);Ok(())
        })).unwrap();
    assert_eq!(result.stop, AgentRunStop::Finalized);
    assert_eq!(result.turn_responses.len(), 4);
    model.assert_context();
    let before_final = if revocation_window {
        assert_eq!(history.status(), 404);
        vec![]
    } else {
        let bubbles = history.bubbles();
        assert_eq!(bubbles.len(), 2);
        assert_eq!(bubbles[1]["body"], fixture.second_body);
        bubbles
    };
    let last = result.turn_responses.last().unwrap();
    let markdown = last
        .session_snapshot
        .messages
        .iter()
        .rev()
        .find(|message| message.role == centaeris_core::session::state::MessageRole::Assistant)
        .unwrap()
        .content
        .clone();
    let mut text = crate::AssistantTextProjection::default();
    text.begin_model_request(run.clone(), markdown).unwrap();
    text.finish_model_request(run).unwrap();
    let final_records = crate::completed_session_records(
        start,
        None,
        &text,
        &result,
        &mut sequence,
        crate::now_ms().unwrap(),
    )
    .unwrap();
    log.append_session_records_with_runtime_job_lease_blocking(run, &final_records, &fence)
        .unwrap();
    for batch in &batches {
        log.append_session_records_with_runtime_job_lease_blocking(run, batch, &fence)
            .unwrap();
    }
    let reopened = PostgresRuntimeStore::new(url.as_str())
        .unwrap()
        .session_log(
            start.authorization.workspace_id.clone(),
            session.clone(),
            start.prompt.clone(),
        );
    reopened
        .append_session_records_with_runtime_job_lease_blocking(run, &final_records, &fence)
        .unwrap();
    if revocation_window {
        assert_eq!(
            history.status(),
            404,
            "replay/reopen does not bypass current read authority"
        );
        let committed = db.query_one(
            "SELECT COUNT(*) FROM app_core_sessionevent WHERE session_id=$1 AND payload->>'type'='tool_result' AND payload->'payload'->>'toolName'='send_message' AND payload->'payload'->>'resultState'='successWithOutput'",
            &[session],
        ).unwrap().get::<_, i64>(0);
        assert_eq!(
            committed, 1,
            "the previously validated call committed once; the subsequent call failed"
        );
        store
            .complete_runtime_job(CompleteRuntimeJobRequest {
                job_id: job,
                lease_owner: owner,
                output_refs: vec![],
                completed_at_ms: crate::now_ms().unwrap(),
            })
            .unwrap();
        println!("agent-message-revocation-window-ok: validated call committed after membership revocation; replay/reopen stable; history 404; subsequent send rejected; 1 Final");
        return;
    }
    assert_eq!(
        history.bubbles(),
        before_final,
        "Final/replay/reopen must not duplicate posted messages"
    );
    db.execute(
        "UPDATE app_core_userlibraryobject SET status='failed' WHERE id=$1",
        &[&fixture.library_object_id],
    )
    .unwrap();
    let revoked = DynamicToolProviderRequest {
        tool_call_id: "revoked-file".into(),
        tool_name: "send_message".into(),
        args_json: model.call("revoked-file", "No revoked file.").args_json,
        contract: provider.contract.clone(),
        cancellation_probe: None,
    };
    assert!(
        executor.block_on(provider.execute(revoked)).is_err(),
        "cached provider must recheck current file authority"
    );
    db.execute(
        "DELETE FROM app_core_workspacemembership WHERE id=$1",
        &[&fixture.membership_id],
    )
    .unwrap();
    let revoked = DynamicToolProviderRequest {
        tool_call_id: "revoked-membership".into(),
        tool_name: "send_message".into(),
        args_json: json!({"body":"No revoked membership.","session_refs":[],"file_refs":[]})
            .to_string(),
        contract: provider.contract.clone(),
        cancellation_probe: None,
    };
    assert!(executor.block_on(provider.execute(revoked)).is_err());
    assert_eq!(
        history
            .client
            .get(&history.url)
            .header("Cookie", &history.cookie)
            .send()
            .unwrap()
            .status()
            .as_u16(),
        404
    );
    store
        .complete_runtime_job(CompleteRuntimeJobRequest {
            job_id: job,
            lease_owner: owner,
            output_refs: vec![],
            completed_at_ms: crate::now_ms().unwrap(),
        })
        .unwrap();
    println!("agent-message-django-contract-ok: real provider and history; failed/stale commits invisible; complete context/refs; replay/reopen; ContinueTurn; unique Final; revoked calls denied");
}
