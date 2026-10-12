//! Native scheduler acceptance with real Core Workers and process-crash recovery.
use super::queue_tests::{job, object, packet, Profile};
use super::*;
use centaeris_core::session::store::{AgentRuntimeSnapshotStorePort, RuntimeStoreTransactionPort};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

const TEST_NAME: &str =
    "subagent_scheduler::job_chain_tests::native_worker_terminal_delivery_survives_process_exit";
const LATE_READ_TEST_NAME: &str =
    "subagent_scheduler::job_chain_tests::acknowledged_cancellation_rejects_late_read_before_durable_tool_admission";

fn child_name(row: &RuntimeJobRecord) -> &str {
    row.payload_ref
        .as_deref()
        .unwrap()
        .strip_prefix("packet:")
        .unwrap()
}

struct MockModel {
    address: std::net::SocketAddr,
    requests: Arc<AtomicUsize>,
    joined: Arc<AtomicUsize>,
    held: Arc<AtomicBool>,
    release: Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl MockModel {
    fn start() -> Self {
        Self::start_with_late_read(false)
    }

    fn start_with_late_read(late_read: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(AtomicUsize::new(0));
        let joined = Arc::new(AtomicUsize::new(0));
        let held = Arc::new(AtomicBool::new(false));
        let release = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let count = requests.clone();
        let joins = joined.clone();
        let holding = held.clone();
        let releasing = release.clone();
        let stopping = stop.clone();
        let thread = std::thread::spawn(move || {
            for stream in listener.incoming() {
                if stopping.load(Ordering::SeqCst) {
                    break;
                }
                let mut stream = stream.unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(15)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut chunk = [0; 4096];
                let body = loop {
                    let read = stream.read(&mut chunk).unwrap();
                    assert_ne!(read, 0, "model request ended before its body");
                    bytes.extend_from_slice(&chunk[..read]);
                    if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..end]);
                        let length: usize = headers
                            .lines()
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse().unwrap())
                            })
                            .expect("model HTTP request content length");
                        if bytes.len() >= end + 4 + length {
                            break String::from_utf8(bytes[end + 4..end + 4 + length].to_vec())
                                .unwrap();
                        }
                    }
                };
                let request: serde_json::Value = serde_json::from_str(&body).unwrap();
                assert_eq!(request["model"], "mock");
                count.fetch_add(1, Ordering::SeqCst);
                if body.contains("held-worker") {
                    holding.store(true, Ordering::SeqCst);
                    let (lock, ready) = &*releasing;
                    let released = lock.lock().unwrap();
                    drop(ready.wait_while(released, |released| !*released).unwrap());
                }
                let (status, content_type, response) = if body.contains("fail-worker") {
                    (
                        "400 Bad Request",
                        "application/json",
                        "{\"error\":{\"message\":\"intentional sibling model failure\"}}"
                            .to_string(),
                    )
                } else {
                    let join_request =
                        request["messages"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .find_map(|message| {
                                message["content"]
                                    .as_str()?
                                    .strip_prefix("JOIN_DURABLE_RESULT ")
                            });
                    let has_tool_result = request["messages"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|message| message["role"] == "tool");
                    let (delta, finish) = if late_read
                        && body.contains("held-worker")
                        && !has_tool_result
                    {
                        (
                            serde_json::json!({"tool_calls":[{"index":0,"id":"late-read-after-cancel","type":"function","function":{"name":"read","arguments":serde_json::json!({"path":"late-read-probe.txt"}).to_string()}}]}),
                            "tool_calls",
                        )
                    } else if let Some(args) = join_request {
                        if let Some(tool) = request["messages"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .find(|message| message["role"] == "tool")
                        {
                            assert!(
                                tool["content"]
                                    .as_str()
                                    .unwrap()
                                    .contains("Durable Worker evidence."),
                                "task_output must return the durable Worker result to the model: {}", tool["content"]
                            );
                            joins.fetch_add(1, Ordering::SeqCst);
                            (
                                serde_json::json!({"content":"Durable result joined."}),
                                "stop",
                            )
                        } else {
                            let args: serde_json::Value = serde_json::from_str(args).unwrap();
                            (
                                serde_json::json!({"tool_calls":[{"index":0,"id":"join-result","type":"function","function":{"name":"task_output","arguments":args.to_string()}}]}),
                                "tool_calls",
                            )
                        }
                    } else {
                        (
                            serde_json::json!({"content":"Durable Worker evidence."}),
                            "stop",
                        )
                    };
                    let response = format!(
                        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                        serde_json::json!({"id":"mock","choices":[{"index":0,"delta":delta,"finish_reason":null}]}),
                        serde_json::json!({"id":"mock","choices":[{"index":0,"delta":{},"finish_reason":finish}],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}})
                    );
                    ("200 OK", "text/event-stream", response)
                };
                write!(stream, "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).unwrap();
                stream.flush().unwrap();
            }
        });
        Self {
            address,
            requests,
            joined,
            held,
            release,
            stop,
            thread: Some(thread),
        }
    }

    fn configure(&self) {
        for value in [
            serde_json::json!({"customModelProviders":[{"providerId":"custom.job-chain","name":"Local Job acceptance","baseUrl":format!("http://{}", self.address),"api":"openai-completions","models":[{"model":"mock","displayName":"Mock","contextTokens":"32k","maxOutputTokens":"4k","supportsVision":false}]}]}),
            serde_json::json!({"modelProviderId":"custom.job-chain","modelApiKey":"local-mock-only"}),
            serde_json::json!({"modelProviderId":"custom.job-chain","model":"mock"}),
        ] {
            runtime_config::set(serde_json::from_value(value).unwrap()).unwrap();
        }
    }

    fn release_worker(&self) {
        let (lock, ready) = &*self.release;
        *lock.lock().unwrap() = true;
        ready.notify_all();
    }
}

impl Drop for MockModel {
    fn drop(&mut self) {
        self.release_worker();
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.address);
        let joined = self.thread.take().unwrap().join();
        if !std::thread::panicking() {
            joined.unwrap();
        }
    }
}

async fn queued_child(profile: &Profile, parent: &str, name: &str) -> RuntimeJobRecord {
    let cwd = profile.root.join("workspace").to_string_lossy().to_string();
    let row = job(parent, name, 1);
    let child = format!("session-{name}");
    let mut payload = packet(parent, &child, &row, &cwd);
    payload["workPacket"]["run_context"]["agentRef"]["agentId"] = name.into();
    payload["workPacket"]["task_brief"]["objective"] = name.into();
    let mut work_object = object(&format!("packet:{name}"), payload.to_string());
    work_object.metadata = serde_json::json!({
        "runtimeJobId": row.job_id,
        "childSessionId": child,
        "resultRef": centaeris_core::runtime::keys::external_context::subagent_result_ref(&row.job_id),
    });
    let snapshots = agent_runtime::agent_runtime_store_actor().unwrap();
    RuntimeStoreTransactionPort::upsert_external_context_and_schedule_job(
        &snapshots,
        centaeris_core::session::store::UpsertExternalContextAndScheduleJobRequest {
            object: work_object,
            job: row.clone(),
        },
    )
    .unwrap();
    sessions::ensure_agent_session(&child, parent, &row.job_id, name, &cwd, 1).unwrap();
    message_log::append_agent_turn_queued(&child, "child-turn", &row.job_id, name, 1).unwrap();
    row
}

fn subprocess(mode: &str, boundary: Option<&str>) -> std::process::Output {
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([TEST_NAME, "--exact", "--nocapture", "--test-threads=1"])
        .env("CENTAERIS_JOB_CHAIN_TEST_MODE", mode);
    if let Some(boundary) = boundary {
        command.env("CENTAERIS_JOB_DELIVERY_TEST_EXIT", boundary);
    } else {
        command.env_remove("CENTAERIS_JOB_DELIVERY_TEST_EXIT");
    }
    command.output().unwrap()
}

async fn child_run() {
    let writer = Arc::new(RuntimeServerClientHub::default()).broadcaster();
    crate::subagent_delivery::reconcile_terminal_jobs(&writer)
        .await
        .unwrap();
    while let Some(parent) = next_queued_parent_session().await.unwrap() {
        run_parent_batch(&parent, writer.clone(), &ShutdownJobs::default())
            .await
            .unwrap();
    }
    crate::subagent_delivery::reconcile_terminal_jobs(&writer)
        .await
        .unwrap();
    agent_runtime::native_job_store_handle()
        .unwrap()
        .close()
        .await
        .unwrap();
}

async fn join_result(parent: &str, row: &RuntimeJobRecord) {
    let config = runtime_config::get(runtime_config::AgentRuntimeConfigGetRequest {}).unwrap();
    let cwd = PathBuf::from(sessions::cwd_for_session_id(parent).unwrap());
    message_log::append_agent_run_started(
        parent,
        "parent-result-join",
        "parent-result-join",
        "Join the durable Worker result",
        current_timestamp_ms(),
    )
    .unwrap();
    let identity =
        agent_runtime::native_agent_run_identity("parent-result-join", parent, &cwd).unwrap();
    let runtime = agent_runtime::build_agent_runtime(agent_runtime::AgentRuntimeBuildRequest {
        store: agent_runtime::agent_runtime_store_actor().unwrap(),
        session_id: parent.into(),
        concurrency_scope_id: parent.into(),
        cwd,
        execution_owner: "parent-result-join".into(),
        bash_path: config.bash_path.as_deref().map(PathBuf::from),
        runtime_config: subagent_engine_config(&config, None).unwrap(),
        native_plugin_activation: None,
        execution_cancellation_probe: None,
        file_mutation_commit_port: None,
    })
    .unwrap();
    let jobs = agent_runtime::native_job_store_handle().unwrap();
    let current = jobs.get_runtime_job(&row.job_id).await.unwrap().unwrap();
    let args = serde_json::json!({"output_ref":{
        "schema":"task_output_ref_v1", "kind":"agent", "runtime_job_id":row.job_id,
        "child_session_id":format!("session-{}", child_name(row)),
        "result_ref":current.output_refs[0],
    }});
    let (model_config, registry) =
        agent_runtime::model_session_config_and_registry(&config).unwrap();
    let config_store = agent_runtime::SingleModelSessionConfigStore {
        session_id: parent.into(),
        config: model_config,
    };
    let transport = ReqwestJsonHttpTransport::for_session(
        "parent-result-join".into(),
        parent,
        &config_store.config.provider_id,
    )
    .unwrap();
    let client = OpenAiCompatibleModelClient::new(registry, transport);
    let result = runtime
        .process_turn_loop_online_with_model_client_stream_cancellable_and_tool_safe_point_async(
            centaeris_core::runtime::AgentRunRequest {
                session_id: parent.into(),
                agent_run_identity: Some(identity),
                initial_turn_id: "parent-result-join".into(),
                initial_input: centaeris_core::runtime::AgentRunInitialInput::UserMessage(format!(
                    "JOIN_DURABLE_RESULT {args}"
                )),
                runtime_scope: centaeris_core::model::prompt::PromptCompactionScopeV1::main(),
                resume_from_turn_id: None,
                auto_continue_after_resume_wait: None,
            },
            &client,
            &config_store,
            &mut |_| {},
            &|| Ok(None),
            &mut |_| Ok(()),
        )
        .await
        .unwrap();
    assert_eq!(
        result.stop,
        centaeris_core::runtime::AgentRunStop::Finalized
    );
}

fn assert_child_output(output: &std::process::Output, code: i32) {
    assert_eq!(
        output.status.code(),
        Some(code),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

async fn assert_delivered(parent: &str, rows: &[RuntimeJobRecord]) {
    let jobs = agent_runtime::native_job_store_handle().unwrap();
    assert!(jobs
        .pending_terminal_deliveries(64, Some(SUBAGENT_RUN_JOB_KIND.into()))
        .await
        .unwrap()
        .is_empty());
    let snapshots = agent_runtime::agent_runtime_store_actor().unwrap();
    let raw = snapshots
        .load_agent_runtime_snapshot(parent)
        .unwrap()
        .unwrap();
    let snapshot: centaeris_core::session::state::SessionStateSnapshot =
        serde_json::from_str(&raw).unwrap();
    let projection = snapshot
        .metadata
        .values()
        .find_map(|raw| {
            let value: serde_json::Value = serde_json::from_str(raw).ok()?;
            (value["schema"] == "subagent_result_projection_v1").then_some(value)
        })
        .expect("parent terminal result projection");
    let items = projection["items"].as_array().unwrap();
    assert_eq!(
        items.len(),
        rows.len(),
        "idempotent delivery must retain each sibling exactly once"
    );
    for row in rows {
        let current = jobs.get_runtime_job(&row.job_id).await.unwrap().unwrap();
        let failed = child_name(row) == "fail-worker";
        assert_eq!(
            current.status,
            if failed {
                RuntimeJobStatus::Failed
            } else {
                RuntimeJobStatus::Succeeded
            }
        );
        let run = message_log::project_agent_run(&row.job_id)
            .unwrap()
            .unwrap();
        assert_eq!(run.status, if failed { "failed" } else { "succeeded" });
        assert_eq!(run.turn_id, "child-turn");
        assert_eq!(run.session_id, format!("session-{}", child_name(row)));
        let matching = items
            .iter()
            .filter(|item| item["subagentRunRef"] == format!("runtime_job:{}", row.job_id))
            .collect::<Vec<_>>();
        assert_eq!(
            matching.len(),
            1,
            "parent must persist one bound terminal result"
        );
        let item = matching[0];
        assert_eq!(item["status"], if failed { "failed" } else { "succeeded" });
        assert_eq!(
            item["childSessionRef"],
            format!("session:{}", run.session_id)
        );
        assert_eq!(item["parentTurnId"], "parent-turn");
        if !failed {
            assert_eq!(current.output_refs.len(), 1);
            let result = jobs
                .load_external_context_object(&current.output_refs[0])
                .await
                .unwrap()
                .unwrap();
            assert!(result.content.contains("Durable Worker evidence."));
            assert_eq!(result.metadata["runtimeJobId"], row.job_id);
            assert_eq!(result.metadata["parentSessionId"], parent);
            assert_eq!(item["resultRef"], current.output_refs[0]);
            assert_eq!(item["boundedSummary"], "Subagent job completed.");
        }
    }
    jobs.close().await.unwrap();
}

#[test]
fn native_worker_terminal_delivery_survives_process_exit() {
    let _guard = message_log::test_env_mutex()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    if std::env::var_os("CENTAERIS_JOB_CHAIN_TEST_MODE").is_some() {
        rt.block_on(child_run());
        return;
    }
    let model = MockModel::start();
    for boundary in [
        "BeforeParent",
        "AfterParent",
        "BeforeChildTerminal",
        "AfterChild",
        "BeforeAcknowledgement",
    ] {
        let profile = Profile::new();
        model.configure();
        let (parent, row) = rt.block_on(async {
            let parent = sessions::create(sessions::SessionCreateRequest {
                title: None,
                cwd: profile.root.join("workspace").to_string_lossy().to_string(),
            })
            .unwrap();
            let row = queued_child(&profile, &parent.id, "success-worker").await;
            (parent, row)
        });
        let before = model.requests.load(Ordering::SeqCst);
        let crashed = subprocess("execute", Some(boundary));
        assert_child_output(&crashed, 81);
        assert!(String::from_utf8_lossy(&crashed.stdout)
            .contains(&format!("delivery boundary reached: {boundary}")));
        assert_eq!(
            model.requests.load(Ordering::SeqCst),
            before + 1,
            "real Worker must request model once before crash"
        );
        rt.block_on(async {
            let jobs = agent_runtime::native_job_store_handle().unwrap();
            let committed = jobs.get_runtime_job(&row.job_id).await.unwrap().unwrap();
            assert_eq!(committed.status, RuntimeJobStatus::Succeeded);
            let result = jobs
                .load_external_context_object(&committed.output_refs[0])
                .await
                .unwrap()
                .unwrap();
            assert!(result.content.contains("Durable Worker evidence."));
            let child_run = message_log::project_agent_run(&row.job_id)
                .unwrap()
                .unwrap();
            assert_eq!(
                child_run.status,
                if matches!(boundary, "AfterChild" | "BeforeAcknowledgement") {
                    "succeeded"
                } else {
                    "running"
                }
            );
            let parent_snapshot = agent_runtime::agent_runtime_store_actor()
                .unwrap()
                .load_agent_runtime_snapshot(&parent.id)
                .unwrap();
            let parent_has_result = parent_snapshot
                .as_deref()
                .is_some_and(|raw| raw.contains(&format!("runtime_job:{}", row.job_id)));
            assert_eq!(
                parent_has_result,
                boundary != "BeforeParent",
                "checkpoint must expose the expected durable parent boundary"
            );
            assert_eq!(
                jobs.pending_terminal_deliveries(64, Some(SUBAGENT_RUN_JOB_KIND.into()))
                    .await
                    .unwrap()
                    .len(),
                1
            );
        });
        for _ in 0..2 {
            assert_child_output(&subprocess("recover", None), 0);
        }
        assert_eq!(
            model.requests.load(Ordering::SeqCst),
            before + 1,
            "terminal Worker must not rerun during fresh-process recovery"
        );
        if boundary == "BeforeAcknowledgement" {
            rt.block_on(join_result(&parent.id, &row));
            assert_eq!(model.joined.load(Ordering::SeqCst), 1);
            assert_eq!(model.requests.load(Ordering::SeqCst), before + 3);
        }
        rt.block_on(assert_delivered(&parent.id, &[row]));
    }
    // Both real Workers run in one batch; one model failure must not lose its sibling.
    let profile = Profile::new();
    model.configure();
    let (parent, rows) = rt.block_on(async {
        let parent = sessions::create(sessions::SessionCreateRequest {
            title: None,
            cwd: profile.root.join("workspace").to_string_lossy().to_string(),
        })
        .unwrap();
        let failed = queued_child(&profile, &parent.id, "fail-worker").await;
        let succeeded = queued_child(&profile, &parent.id, "success-worker").await;
        (parent, [failed, succeeded])
    });
    let before = model.requests.load(Ordering::SeqCst);
    assert_child_output(&subprocess("execute", None), 0);
    assert_child_output(&subprocess("recover", None), 0);
    assert_eq!(model.requests.load(Ordering::SeqCst), before + 2);
    rt.block_on(assert_delivered(&parent.id, &rows));
}

#[test]
fn deleting_running_child_waits_for_real_worker_and_preserves_sibling() {
    let _guard = message_log::test_env_mutex()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let profile = Profile::new();
    let model = MockModel::start();
    model.configure();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let parent = sessions::create(sessions::SessionCreateRequest {
            title: None,
            cwd: profile.root.join("workspace").to_string_lossy().to_string(),
        })
        .unwrap();
        let held = queued_child(&profile, &parent.id, "held-worker").await;
        let sibling = queued_child(&profile, &parent.id, "success-worker").await;
        let hub = Arc::new(RuntimeServerClientHub::default());
        let writer = hub.broadcaster();
        let batch_writer = writer.clone();
        let parent_id = parent.id.clone();
        let batch = tokio::spawn(async move {
            run_parent_batch(&parent_id, batch_writer, &ShutdownJobs::default()).await
        });
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            while !model.held.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("real child Worker must reach held model request");
        let deleting = tokio::spawn(sessions::delete_async(
            writer.clone(),
            sessions::SessionDeleteRequest {
                session_id: "session-held-worker".into(),
            },
        ));
        let jobs = agent_runtime::native_job_store_handle().unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            loop {
                let row = jobs.get_runtime_job(&held.job_id).await.unwrap().unwrap();
                if row.status == RuntimeJobStatus::Cancelled {
                    break;
                }
                assert!(
                    !deleting.is_finished(),
                    "deletion returned before cancelling the bound Job"
                );
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("direct child deletion must cancel the parent-owned bound Job");
        assert!(
            !deleting.is_finished(),
            "Session deletion must await the actual held Worker sequence"
        );
        model.release_worker();
        let batch_result = tokio::time::timeout(std::time::Duration::from_secs(15), batch)
            .await
            .unwrap()
            .unwrap();
        // Admission is closed to ordinary delivery; the deletion owner below
        // delivers and acknowledges this terminal after draining the batch.
        if let Err(error) = batch_result {
            assert_eq!(error, "session is being deleted: session-held-worker");
        }
        tokio::time::timeout(std::time::Duration::from_secs(15), deleting)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(model.requests.load(Ordering::SeqCst), 2);
        assert_eq!(
            jobs.get_runtime_job(&held.job_id)
                .await
                .unwrap()
                .unwrap()
                .status,
            RuntimeJobStatus::Cancelled
        );
        assert_eq!(
            jobs.get_runtime_job(&sibling.job_id)
                .await
                .unwrap()
                .unwrap()
                .status,
            RuntimeJobStatus::Succeeded
        );
        assert_eq!(
            message_log::project_agent_run(&sibling.job_id)
                .unwrap()
                .unwrap()
                .status,
            "succeeded"
        );
        assert!(jobs
            .pending_terminal_deliveries(64, Some(SUBAGENT_RUN_JOB_KIND.into()))
            .await
            .unwrap()
            .is_empty());
        assert!(
            jobs.load_external_context_object(
                &centaeris_core::runtime::keys::external_context::subagent_result_ref(&held.job_id)
            )
            .await
            .unwrap()
            .is_none(),
            "a cancelled Worker must not publish its late successful result"
        );
        let raw = agent_runtime::agent_runtime_store_actor()
            .unwrap()
            .load_agent_runtime_snapshot(&parent.id)
            .unwrap()
            .unwrap();
        let snapshot: centaeris_core::session::state::SessionStateSnapshot =
            serde_json::from_str(&raw).unwrap();
        let projection = snapshot
            .metadata
            .values()
            .find_map(|raw| {
                let value: serde_json::Value = serde_json::from_str(raw).ok()?;
                (value["schema"] == "subagent_result_projection_v1").then_some(value)
            })
            .unwrap();
        let items = projection["items"].as_array().unwrap();
        assert_eq!(items.len(), 2);
        for (row, status) in [(&held, "cancelled"), (&sibling, "succeeded")] {
            let matching = items
                .iter()
                .filter(|item| item["subagentRunRef"] == format!("runtime_job:{}", row.job_id))
                .collect::<Vec<_>>();
            assert_eq!(matching.len(), 1);
            assert_eq!(matching[0]["status"], status);
            assert_eq!(
                matching[0]["childSessionRef"],
                format!("session:session-{}", child_name(row))
            );
        }
        assert!(!sessions::list(sessions::SessionListRequest {})
            .unwrap()
            .iter()
            .any(|session| session.id == "session-held-worker"));
        assert!(
            crate::user_data_layout::find_session_log_file_path("session-held-worker")
                .unwrap()
                .is_none()
        );
        // Read fresh persistence after both host sequences complete; neither the
        // late model response nor its Job completion may recreate deleted data.
        let db = rusqlite::Connection::open(profile.root.join("runtime.sqlite3")).unwrap();
        for table in [
            "runtime_events",
            "session_runtime_snapshots",
            "checkpoints",
            "transcript_projection_heads",
        ] {
            let count: i64 = db
                .query_row(
                    &format!("SELECT count(*) FROM {table} WHERE session_id=?1"),
                    ["session-held-worker"],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 0, "late Worker must not recreate {table}");
        }
        jobs.close().await.unwrap();
    });
}

#[test]
fn acknowledged_cancellation_rejects_late_read_before_durable_tool_admission() {
    if std::env::var_os("CENTAERIS_LATE_READ_TEST_CHILD").is_none() {
        // Bound only this owned integration-test subprocess independently of
        // the Worker and model transport timeout implementations.
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                LATE_READ_TEST_NAME,
                "--exact",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("CENTAERIS_LATE_READ_TEST_CHILD", "1")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            if std::time::Instant::now() >= deadline {
                child.kill().unwrap();
                let output = child.wait_with_output().unwrap();
                panic!(
                    "late-read subprocess exceeded wall-clock budget: {}\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            // This is the outer process watchdog, never a cancellation-cache
            // synchronization step in the acceptance fixture.
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_child_output(&child.wait_with_output().unwrap(), 0);
        return;
    }
    let _guard = message_log::test_env_mutex()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let profile = Profile::new();
    std::fs::write(
        profile.root.join("workspace/late-read-probe.txt"),
        "A real readable cancellation probe.",
    )
    .unwrap();
    let model = MockModel::start_with_late_read(true);
    model.configure();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let parent = sessions::create(sessions::SessionCreateRequest {
            title: None,
            cwd: profile.root.join("workspace").to_string_lossy().to_string(),
        })
        .unwrap();
        let held = queued_child(&profile, &parent.id, "held-worker").await;
        let writer = Arc::new(RuntimeServerClientHub::default()).broadcaster();
        let parent_id = parent.id.clone();
        let batch = tokio::spawn(async move {
            run_parent_batch(&parent_id, writer, &ShutdownJobs::default()).await
        });
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            while !model.held.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("real Worker must send its held model request");
        let jobs = agent_runtime::native_job_store_handle().unwrap();
        let running = jobs.get_runtime_job(&held.job_id).await.unwrap().unwrap();
        assert_eq!(running.status, RuntimeJobStatus::Running);
        jobs.cancel_runtime_job(
            centaeris_core::session::reliability::CancelRuntimeJobRequest {
                job_id: held.job_id.clone(),
                expected_lease_owner: running.lease_owner,
                expected_status: Some(RuntimeJobStatus::Running),
                reason: "user_cancelled".into(),
                cancelled_at_ms: current_timestamp_ms(),
            },
        )
        .await
        .unwrap();
        assert_eq!(
            jobs.get_runtime_job(&held.job_id)
                .await
                .unwrap()
                .unwrap()
                .status,
            RuntimeJobStatus::Cancelled
        );
        // The mock resumes only after the cancellation acknowledgement and
        // authoritative Job read. Do not wait for the polling cache to refresh.
        model.release_worker();
        tokio::time::timeout(std::time::Duration::from_secs(15), batch)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let path = crate::user_data_layout::find_session_log_file_path("session-held-worker")
            .unwrap()
            .unwrap();
        let document = message_log::read_session_document(&path).unwrap();
        let tools = document
            .records
            .iter()
            .filter(|record| {
                record.agent_run_id.as_deref() == Some(&held.job_id)
                    && matches!(
                        record.event_type,
                        centaeris_core::session::SessionRecordType::ToolCall
                            | centaeris_core::session::SessionRecordType::ToolResult
                    )
            })
            .collect::<Vec<_>>();
        assert!(
            tools.is_empty(),
            "acknowledged cancellation must reject late ToolCall/ToolResult, got {} records",
            tools.len()
        );
        assert_eq!(
            model.requests.load(Ordering::SeqCst),
            1,
            "cancelled Worker must not request another model round for a late read result"
        );
        assert_eq!(
            jobs.get_runtime_job(&held.job_id)
                .await
                .unwrap()
                .unwrap()
                .status,
            RuntimeJobStatus::Cancelled
        );
        assert!(jobs
            .pending_terminal_deliveries(64, Some(SUBAGENT_RUN_JOB_KIND.into()))
            .await
            .unwrap()
            .is_empty());
        jobs.close().await.unwrap();
    });
}
