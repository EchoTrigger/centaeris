use super::*;
fn request(manager: &Manager, op: &str, script: &str) -> StartRequest {
    StartRequest {
        session_id: "session-test".into(),
        service_instance_id: manager.service_instance_id.clone(),
        operation_id: op.into(),
        program: "bash".into(),
        args: vec!["-c".into(), script.into()],
        timeout_ms: 3000,
    }
}
fn target(id: &str) -> Target {
    Target {
        session_id: "session-test".into(),
        process_session_id: id.into(),
    }
}

#[test]
fn agent_process_source_is_distinct_from_manual_host_commands() {
    // Test the common spawn boundary without requiring a user profile.
    let manager = Manager::default();
    let id = manager
        .start_for_agent_at(
            request(&manager, "agent-source", "printf done"),
            std::env::current_dir().unwrap(),
        )
        .unwrap();
    let snapshot = finished(&manager, &id);
    assert_eq!(snapshot.source, "agentTool");
}
fn read(manager: &Manager, id: &str, cursor: &str, wait_ms: u64) -> Output {
    manager
        .read(ReadRequest {
            session_id: "session-test".into(),
            process_session_id: id.into(),
            cursor: cursor.into(),
            wait_ms,
        })
        .unwrap()
}
fn finished(manager: &Manager, id: &str) -> Snapshot {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let s = manager.get(target(id)).unwrap();
        if s.output_complete {
            return s;
        }
        assert!(Instant::now() < deadline, "process failed to settle: {s:?}");
        thread::sleep(Duration::from_millis(10));
    }
}
#[test]
fn managed_process_start_has_identity_and_independent_readers() {
    let manager = Manager::default();
    let req = request(&manager, "op-1", "printf hello; printf error >&2");
    let id = manager
        .start_at(req.clone(), std::env::current_dir().unwrap())
        .unwrap();
    assert!(id.starts_with("process-"));
    assert_eq!(
        manager
            .start_at(req, std::env::current_dir().unwrap())
            .unwrap(),
        id
    );
    assert_eq!(finished(&manager, &id).exit_code, Some(0));
    let a = read(&manager, &id, "0", 0);
    let b = read(&manager, &id, "0", 0);
    assert_eq!(
        serde_json::to_value(&a.chunks).unwrap(),
        serde_json::to_value(&b.chunks).unwrap()
    );
    assert_eq!(
        a.chunks
            .iter()
            .filter(|c| c.stream == "stdout")
            .flat_map(|c| STANDARD.decode(&c.data_base64).unwrap())
            .collect::<Vec<_>>(),
        b"hello"
    );
    assert!(manager
        .entry(&Target {
            session_id: "other".into(),
            process_session_id: id.clone()
        })
        .is_err());
    assert!(manager
        .start_at(
            request(&manager, "op-1", "exit 2"),
            std::env::current_dir().unwrap()
        )
        .unwrap_err()
        .contains("conflict"));
    manager.shutdown().unwrap();
}
#[test]
fn wait_does_not_kill_stop_is_scoped_and_repeated_stop_is_safe() {
    let manager = Manager::default();
    let cwd = std::env::current_dir().unwrap();
    let a = manager
        .start_at(request(&manager, "a", "sleep 30"), cwd.clone())
        .unwrap();
    let b = manager
        .start_at(request(&manager, "b", "sleep 30"), cwd)
        .unwrap();
    assert_eq!(read(&manager, &a, "0", 30).process.state, "running");
    assert!(manager.has_active());
    assert!(manager.ensure_deletable(&["session-test".into()]).is_err());
    manager.stop(target(&a)).unwrap();
    assert_eq!(finished(&manager, &a).termination_reason, Some("stopped"));
    manager.stop(target(&a)).unwrap();
    assert_eq!(manager.get(target(&b)).unwrap().state, "running");
    manager.shutdown().unwrap();
    assert!(!manager.has_active());
    assert!(manager.ensure_deletable(&["session-test".into()]).is_ok());
}
#[test]
fn timeout_and_nonzero_exit_are_distinct() {
    let manager = Manager::default();
    let mut req = request(&manager, "timeout", "sleep 30");
    req.timeout_ms = 50;
    let id = manager
        .start_at(req, std::env::current_dir().unwrap())
        .unwrap();
    assert_eq!(finished(&manager, &id).termination_reason, Some("timedOut"));
    let id = manager
        .start_at(
            request(&manager, "failed", "exit 7"),
            std::env::current_dir().unwrap(),
        )
        .unwrap();
    let s = finished(&manager, &id);
    assert_eq!(s.exit_code, Some(7));
    assert_eq!(s.termination_reason, None);
    manager.shutdown().unwrap();
}
#[test]
fn output_capacity_and_cursor_contract_are_explicit() {
    let mut log = Log::new();
    log.append("stdout", &vec![b'x'; OUTPUT_CAP + CHUNK_BYTES]);
    assert_eq!(log.bytes, OUTPUT_CAP);
    assert_eq!(log.earliest(), 1);
    let manager = Manager::default();
    let id = manager
        .start_at(
            request(&manager, "bytes", "printf '\\377\\000x'"),
            std::env::current_dir().unwrap(),
        )
        .unwrap();
    finished(&manager, &id);
    let output = read(&manager, &id, "0", 0);
    assert_eq!(
        output
            .chunks
            .iter()
            .flat_map(|c| STANDARD.decode(&c.data_base64).unwrap())
            .collect::<Vec<_>>(),
        [255, 0, b'x']
    );
    assert!(manager
        .read(ReadRequest {
            session_id: "session-test".into(),
            process_session_id: id,
            cursor: "999".into(),
            wait_ms: 0
        })
        .is_err());
    manager.shutdown().unwrap();
}
#[test]
fn protocol_rejects_unknown_fields_and_old_identifiers() {
    let manager = Manager::default();
    let mut value = serde_json::to_value(request(&manager, "op", "echo ok")).unwrap();
    value["pid"] = serde_json::json!(42);
    assert!(serde_json::from_value::<StartRequest>(value).is_err());
    let manager = Manager::default();
    assert!(manager.get(target("process-old-service")).is_err());
}
#[test]
fn concurrent_start_reuses_one_process() {
    let manager = Arc::new(Manager::default());
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let m = manager.clone();
            thread::spawn(move || {
                m.start_at(
                    request(&m, "same", "sleep 1"),
                    std::env::current_dir().unwrap(),
                )
                .unwrap()
            })
        })
        .collect();
    let ids: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    assert!(ids.iter().all(|id| id == &ids[0]));
    assert_eq!(manager.list("session-test").len(), 1);
    manager.shutdown().unwrap();
}

#[test]
fn output_gap_and_completed_record_eviction_do_not_reexecute_receipts() {
    let manager = Manager::default();
    let req = request(&manager, "gap", "printf ok");
    let id = manager
        .start_at(req.clone(), std::env::current_dir().unwrap())
        .unwrap();
    finished(&manager, &id);
    {
        let entry = manager.entry(&target(&id)).unwrap();
        let mut s = entry.state.lock().unwrap();
        s.log
            .append("stderr", &vec![b'x'; OUTPUT_CAP + CHUNK_BYTES]);
    }
    let output = read(&manager, &id, "0", 0);
    assert!(output.gap);
    assert!(output.has_more);
    assert!(
        output
            .chunks
            .iter()
            .map(|c| STANDARD.decode(&c.data_base64).unwrap().len())
            .sum::<usize>()
            <= MAX_READ_BYTES
    );
    manager.forget_closed(&["session-test".into()]);
    assert!(manager.get(target(&id)).is_err());
    assert_eq!(
        manager
            .start_at(req, std::env::current_dir().unwrap())
            .unwrap(),
        id
    );
    assert!(manager.list("session-test").is_empty());
    manager.shutdown().unwrap();
}
#[test]
fn stopped_process_tree_cannot_write_a_late_marker() {
    let manager = Manager::default();
    let root = std::env::temp_dir().join(identity().unwrap());
    std::fs::create_dir(&root).unwrap();
    let id = manager
        .start_at(
            request(
                &manager,
                "tree",
                "(sleep 1; printf leaked > marker) & printf ready; wait",
            ),
            root.clone(),
        )
        .unwrap();
    assert!(!read(&manager, &id, "0", 1000).chunks.is_empty());
    manager.stop(target(&id)).unwrap();
    let s = finished(&manager, &id);
    assert!(s.cleanup_complete);
    thread::sleep(Duration::from_millis(1200));
    assert!(!root.join("marker").exists());
    manager.shutdown().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn service_restart_rejects_old_start_even_with_same_operation_id() {
    let old = Manager::default();
    let current = Manager::default();
    let req = request(&old, "op", "echo unwanted");
    assert!(current.start(req).unwrap_err().contains("instance_changed"));
}

#[test]
fn host_methods_use_the_existing_request_envelope() {
    use crate::commands::RuntimeHostCommand as C;
    let result = handle(
        C::ProcessSessionList,
        serde_json::json!({"request":{"sessionId":"envelope-test"}}),
    )
    .unwrap();
    assert!(result["serviceInstanceId"].as_str().is_some());
    assert_eq!(result["processes"], serde_json::json!([]));
    assert!(handle(
        C::ProcessSessionList,
        serde_json::json!({"request":{"sessionId":"envelope-test","extra":true}})
    )
    .is_err());
}

#[test]
fn shutdown_closes_start_admission() {
    let manager = Manager::default();
    manager.begin_shutdown();
    assert!(manager
        .start_at(
            request(&manager, "late", "echo unwanted"),
            std::env::current_dir().unwrap()
        )
        .unwrap_err()
        .contains("stopping"));
    assert!(manager.list("session-test").is_empty());
}

#[test]
fn tiny_output_writes_have_a_bounded_record_count() {
    let mut log = Log::new();
    for _ in 0..4097 {
        log.append("stdout", b"x");
    }
    assert_eq!(log.chunks.len(), 4096);
    assert_eq!(log.earliest(), 1);
}

#[test]
fn desktop_and_tui_samples_match_process_wire_serialization() {
    let samples: serde_json::Value =
        serde_json::from_str(include_str!("../../generated/process-session-samples.json")).unwrap();
    let start: StartRequest = serde_json::from_value(samples["startRequest"].clone()).unwrap();
    let target: Target = serde_json::from_value(samples["target"].clone()).unwrap();
    let read: ReadRequest = serde_json::from_value(samples["readRequest"].clone()).unwrap();
    assert_eq!(
        serde_json::to_value(&start).unwrap(),
        samples["startRequest"]
    );
    assert_eq!(read.session_id, target.session_id);
    assert_eq!(read.process_session_id, target.process_session_id);
    assert_eq!(read.cursor, "0");
    assert_eq!(read.wait_ms, 1000);
    let snapshot = Snapshot {
        process_session_id: target.process_session_id,
        session_id: start.session_id,
        program: start.program,
        args: start.args,
        cwd: "sample-root".into(),
        state: "exited",
        source: "hostCommand",
        cleanup_complete: true,
        exit_code: Some(0),
        termination_reason: None,
        output_complete: true,
        error: None,
    };
    assert_eq!(
        serde_json::to_value(&snapshot).unwrap(),
        samples["snapshot"]
    );
    let mut agent_snapshot = snapshot.clone();
    agent_snapshot.source = "agentTool";
    assert_eq!(
        serde_json::to_value(agent_snapshot).unwrap(),
        samples["agentSnapshot"]
    );
    assert_eq!(
        serde_json::json!({"serviceInstanceId":"service-sample","processes":[snapshot.clone()]}),
        samples["list"]
    );
    let output = Output {
        process: snapshot,
        chunks: vec![Chunk {
            cursor: "0".into(),
            stream: "stdout",
            data_base64: STANDARD.encode(b"hello\n"),
        }],
        next_cursor: "1".into(),
        earliest_cursor: "0".into(),
        gap: false,
        has_more: false,
    };
    assert_eq!(serde_json::to_value(output).unwrap(), samples["output"]);
}
