// Diagnostic characterization only: all entry points are inside cfg(test).
struct LongCommandOutputHost(TextHost);

impl centaeris_core::execution::ExecutionHostRunner for LongCommandOutputHost {
    fn status(
        &self,
        policy: &centaeris_core::execution::ExecutionPolicy,
    ) -> Result<
        centaeris_core::execution::ExecutionHostStatus,
        centaeris_core::execution::ExecutionError,
    > {
        centaeris_core::execution::ExecutionHostRunner::status(&self.0, policy)
    }
    fn run_file_system_operation(
        &self,
        request: centaeris_core::execution::ExecutionFileSystemRequest,
    ) -> Result<
        centaeris_core::execution::ExecutionFileSystemOutput,
        centaeris_core::execution::ExecutionFileSystemError,
    > {
        centaeris_core::execution::ExecutionHostRunner::run_file_system_operation(&self.0, request)
    }
    fn run_host_command(
        &self,
        _: Option<&str>,
        request: centaeris_core::execution::ExecutionCommandRequest,
        _: Option<&centaeris_core::execution::ExecutionCancellationProbe>,
    ) -> Result<
        centaeris_core::execution::ExecutionHostCommandOutput,
        centaeris_core::execution::ExecutionError,
    > {
        use centaeris_core::execution::*;
        let stdout = decode_process_output(
            ("BOUNDARY-STDOUT-".to_string() + &"0123456789abcdef".repeat(8192)).as_bytes(),
        );
        let stderr = decode_process_output(b"");
        Ok(ExecutionHostCommandOutput {
            process: ExecutionProcessOutput {
                exit_code: Some(0),
                stdout: stdout.text,
                stderr: stderr.text,
                stdout_decode: stdout.summary,
                stderr_decode: stderr.summary,
                timed_out: false,
                attempt: ExecutionAttempt {
                    transition_reason: "synthetic-command-output".into(),
                    policy: ExecutionPolicySummary {
                        enforced: true,
                        network: NetworkPolicy::Disabled,
                        workspace_root: request.cwd.display().to_string(),
                        read_only_root_count: 0,
                        writable_root_count: 1,
                        denied_read_path_count: 0,
                        denied_write_path_count: 0,
                    },
                },
                runtime_diagnostics: vec![],
            },
            failure_kind: ExecutionHostFailureKind::None,
            input_state_changes: vec![],
        })
    }
}

#[test]
#[ignore = "explicit PostgreSQL publication contract against a fresh disposable database"]
fn transcript_capture_second_run_requires_committed_session_sequence() {
    use centaeris_core::execution::*;
    use centaeris_core::extension::skills::SkillCatalogLoadConfig;
    use centaeris_core::model::ToolCallEnvelope;
    use centaeris_core::session::{AgentRunSessionState, SessionLogPort};
    use centaeris_core::tool::layer::{ToolInvocationRequest, ToolLayer};

    let url =
        std::env::var("CENTAERIS_CAPTURE_BOUNDARY_DATABASE").expect("dedicated database required");
    let mut db = postgres::Client::connect(&url, postgres::NoTls).unwrap();
    let name: String = db
        .query_one("SELECT current_database()", &[])
        .unwrap()
        .get(0);
    assert!(
        name.starts_with("test_managed_assistant_capture_") || name.starts_with("test_outbox_")
    );
    db.batch_execute(r#"
        CREATE TABLE app_core_session(id text PRIMARY KEY,workspace_id text NOT NULL,
            status text NOT NULL DEFAULT 'active',"purgedAt" timestamptz);
        CREATE TABLE app_core_sessionevent(
            "eventId" text PRIMARY KEY,workspace_id text NOT NULL,session_id text NOT NULL,
            agent_run_id text NOT NULL,sequence integer NOT NULL,agent_run_sequence integer,
            session_level boolean NOT NULL DEFAULT false,projects_to_agent_run_stream boolean NOT NULL,
            payload jsonb NOT NULL,"createdAtMs" bigint NOT NULL,"insertedAt" timestamptz NOT NULL,
            UNIQUE(session_id,sequence),UNIQUE(agent_run_id,agent_run_sequence));
        CREATE TABLE app_core_transcriptoutputcapture(event_id text PRIMARY KEY,
            "executionId" text NOT NULL,sha256 text NOT NULL,"byteLength" bigint NOT NULL,
            "chunkSize" integer NOT NULL,"createdAt" timestamptz NOT NULL,"purgedAt" timestamptz);
        CREATE TABLE app_core_transcriptoutputchunk(capture_id text NOT NULL,index integer NOT NULL,
            data bytea NOT NULL,sha256 text NOT NULL,PRIMARY KEY(capture_id,index));
        INSERT INTO app_core_session(id,workspace_id) VALUES('session-two-runs','workspace-fixture');
    "#).unwrap();
    let store = PostgresRuntimeStore::new(&url).unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let session = "session-two-runs";
    let first_id = "run-first";
    let mut first = AgentRunSessionState::new(session, first_id).unwrap();
    let mut first_events = first.start("run-first", "first", vec![], 1).unwrap();
    first_events.push(first.event_for_turn("run-first", SessionRecordType::AssistantMessage,
        json!({"messageId":"message:run-first:assistant","modelMarkdown":"done","artifactRefs":[],"status":"done"}), 2).unwrap());
    first_events.push(
        first
            .record(
                centaeris_core::session::completed_agent_run_record(
                    session,
                    "run-first",
                    first_id,
                    "finalized",
                    3,
                )
                .unwrap(),
            )
            .unwrap(),
    );
    let first_log = store.session_log("workspace-fixture".into(), session.into(), "first".into());
    let first_receipt = runtime
        .block_on(first_log.append_session_records(first_id, &first_events))
        .unwrap();
    assert_eq!(first_receipt.last_sequence().unwrap(), 4);

    let host = Arc::new(LongCommandOutputHost(TextHost {
        capture: Default::default(),
        budget: 1_000_000,
    }));
    let workspace = std::env::temp_dir();
    let binding = Arc::new(
        ExecutionHostBinding::new(
            ExecutionHostMode::Remote,
            host.clone(),
            workspace.clone(),
            ExecutionPolicy::workspace_write_no_network(workspace),
        )
        .unwrap(),
    );
    let layer = ToolLayer::try_new_with_skill_catalog_config_and_execution_host_binding(
        SkillCatalogLoadConfig::default(),
        binding,
    )
    .unwrap()
    .with_session_id(session);
    let call = ToolCallEnvelope {
        id: "call-second".into(),
        name: "bash".into(),
        args_json: json!({"command":"printf synthetic"}).to_string(),
    };
    let result = runtime.block_on(layer.execute_async(ToolInvocationRequest {
        tool_call_id: call.id.clone(),
        tool_name: call.name.clone(),
        args_json: call.args_json.clone(),
    }));
    assert_eq!(result.status, "ok");
    let second_id = "run-second";
    let mut second = AgentRunSessionState::new(session, second_id).unwrap();
    let mut second_events = second.start("run-second", "second", vec![], 4).unwrap();
    second_events.push(
        second
            .start_execution(
                "run-second",
                "execution-second",
                &format!("sha256:{}", "a".repeat(64)),
                None,
                5,
            )
            .unwrap(),
    );
    second_events.extend(
        second
            .record_tool_call(
                "run-second",
                &call,
                "centaeris.builtin",
                &format!("sha256:{}", "b".repeat(64)),
                "printf synthetic",
                6,
            )
            .unwrap(),
    );
    second_events.extend(
        second
            .record_tool_result("run-second", &call, &result, 7)
            .unwrap(),
    );
    let second_log = store.session_log("workspace-fixture".into(), session.into(), "second".into());
    let receipt = runtime
        .block_on(second_log.append_session_records(second_id, &second_events))
        .unwrap();
    let result_record = second_events
        .iter()
        .find(|r| r.event.event_type == SessionRecordType::ToolResult)
        .unwrap();
    let local = result_record.sequence;
    let committed = receipt
        .records
        .iter()
        .find(|r| r.event.event_id == result_record.event.event_id)
        .unwrap();
    assert_eq!(committed.sequence, local + first_events.len() as u64);
    let payload = &result_record.event.payload;
    let path = payload["fullOutputPath"].as_str().unwrap();
    let start = payload["outputStartByte"].as_u64().unwrap() as usize;
    let length = payload["outputByteLength"].as_u64().unwrap() as usize;
    let spill = host.0.capture.lock().unwrap().writes[path].clone();
    let expected = spill[start..start + length].to_vec();
    assert!(expected.len() > 65_536);
    let publisher = Arc::new(CapturePublisher::default());
    let wait = || {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while publisher.0.lock().unwrap().running {
            assert!(
                std::time::Instant::now() < deadline,
                "publication did not drain"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    };
    // This is the production publication entry used after the fenced commit.
    publish_committed(
        store.clone(),
        "execution-second".into(),
        publisher.clone(),
        &mut host.0.capture.lock().unwrap(),
        &receipt,
        1_000_000,
    );
    wait();
    assert_eq!(
        db.query_one("SELECT count(*) FROM app_core_transcriptoutputcapture", &[])
            .unwrap()
            .get::<_, i64>(0),
        1,
        "second Run must publish using the committed Session sequence"
    );
    let mut repeat = CaptureBuffer::default();
    repeat.observe(path.into(), &spill, 1_000_000);
    publish_committed(
        store.clone(),
        "execution-second".into(),
        publisher.clone(),
        &mut repeat,
        &receipt,
        1_000_000,
    );
    wait();
    let rows = db
        .query(
            "SELECT data,sha256 FROM app_core_transcriptoutputchunk ORDER BY index",
            &[],
        )
        .unwrap();
    let mut archived = Vec::new();
    for row in &rows {
        let bytes: Vec<u8> = row.get(0);
        assert_eq!(row.get::<_, String>(1), digest(&bytes));
        archived.extend(bytes);
    }
    assert_eq!(
        archived, expected,
        "every UTF-8 byte survives chunking and idempotent publication"
    );
    let metadata = db
        .query_one(
            "SELECT sha256,\"byteLength\" FROM app_core_transcriptoutputcapture",
            &[],
        )
        .unwrap();
    assert_eq!(metadata.get::<_, String>(0), digest(&archived));
    assert_eq!(metadata.get::<_, i64>(1), archived.len() as i64);
    println!(
        "{}",
        json!({"diagnostic":"second-run-capture","runLocalSequence":local,
        "sessionGlobalSequence":committed.sequence,"bytes":archived.len(),"chunks":rows.len(),
        "sha256":digest(&archived),"productionPublicationPassed":true})
    );
}
