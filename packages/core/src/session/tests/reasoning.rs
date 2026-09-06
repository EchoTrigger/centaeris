use super::*;

fn reasoning_event() -> SessionLogRecord {
    serde_json::from_value(base_event(
        "reasoning_block",
        "reasoning-event",
        serde_json::from_str(include_str!("../../../tests/fixtures/reasoning_block.json")).unwrap(),
    ))
    .expect("reasoning event is a v1 Session record")
}

fn reasoning_history() -> Vec<SessionLogRecord> {
    let mut events = started_agent_run_records("session-1", "turn-1", "agent-run-1", "question", 1)
        .unwrap()
        .to_vec();
    events.push(
        serde_json::from_value(base_event(
            "model_request_started",
            "request-event",
            model_request_payload(vec![]),
        ))
        .unwrap(),
    );
    events
}

#[test]
fn reasoning_record_replays_without_becoming_model_context() {
    let mut events = reasoning_history();
    let reasoning = reasoning_event();
    validate_event_shape(&reasoning).unwrap();
    assert!(session_record_projects_to_agent_run_stream(
        reasoning.event_type
    ));
    let SessionStreamProjection::SessionEvent { event, .. } =
        project_committed_session_record(&reasoning, 5).unwrap();
    assert_eq!(event.event_type, "Reasoning");
    assert_eq!(event.payload["text"], " inspect\n");
    events.push(reasoning);
    reduce_events("session-1", events.iter()).unwrap();
    let snapshot = restore_runtime_snapshot_from_session_records("session-1", &events).unwrap();
    assert!(!snapshot
        .messages
        .iter()
        .any(|message| message.content.contains("inspect")));
}

#[test]
fn reasoning_record_is_idempotent_at_commit_and_survives_state_restore() {
    let mut state = AgentRunSessionState::new("session-1", "agent-run-1").unwrap();
    let mut committed = Vec::new();
    for event in reasoning_history() {
        committed.push(state.record(event).unwrap());
    }
    let block = state
        .record_reasoning_block("turn-1", "model-request-1", " inspect\n", "done", 2)
        .unwrap()
        .unwrap();
    committed.push(block);
    assert!(state
        .record_reasoning_block("turn-1", "model-request-1", " inspect\n", "done", 3)
        .unwrap()
        .is_none());
    assert!(state
        .record_reasoning_block("turn-1", "model-request-1", "changed", "done", 3)
        .is_err());
    let mut restored = AgentRunSessionState::new("session-1", "agent-run-1").unwrap();
    for event in committed {
        restored.restore(event).unwrap();
    }
    assert!(restored
        .record_reasoning_block("turn-1", "model-request-1", " inspect\n", "done", 4)
        .unwrap()
        .is_none());
}

#[test]
fn reasoning_record_rejects_bad_shape_and_request_binding() {
    for (key, value) in [
        ("text", json!("")),
        ("text", json!(42)),
        ("status", json!("streaming")),
        ("blockId", json!("other")),
        ("durationMs", json!(10)),
    ] {
        let mut record = reasoning_event();
        record.payload[key] = value;
        assert!(validate_event_shape(&record).is_err(), "accepted {key}");
    }
    for mismatch in ["missing", "turn", "run", "duplicate", "compaction"] {
        let mut events = reasoning_history();
        let mut record = reasoning_event();
        match mismatch {
            "missing" => {
                events.pop();
            }
            "turn" => record.turn_id = Some("other".to_string()),
            "run" => record.agent_run_id = Some("other".to_string()),
            "duplicate" => {
                events.push(record.clone());
                record.event_id = "duplicate".to_string();
            }
            "compaction" => {
                events.last_mut().unwrap().payload["purpose"] = json!("compaction");
            }
            _ => unreachable!(),
        }
        events.push(record);
        assert!(
            reduce_events("session-1", events.iter()).is_err(),
            "accepted {mismatch}"
        );
    }
}

#[test]
fn reasoning_recovery_seals_only_bound_partial_and_never_overwrites_terminal() {
    let sample: Value =
        serde_json::from_str(include_str!("../../../tests/fixtures/live_reasoning.json")).unwrap();
    let mut state = AgentRunSessionState::new("session-1", "agent-run-1").unwrap();
    for event in reasoning_history() {
        state.record(event).unwrap();
    }
    assert!(state
        .recover_reasoning_snapshot("other-turn", &sample, 3)
        .is_err());
    let mut unknown = sample.clone();
    unknown["durationMs"] = json!(3);
    assert!(state
        .recover_reasoning_snapshot("turn-1", &unknown, 3)
        .is_err());
    let sealed = state
        .recover_reasoning_snapshot("turn-1", &sample, 3)
        .unwrap()
        .unwrap();
    assert_eq!(sealed.event.payload["status"], "interrupted");
    assert_eq!(sealed.event.payload["text"], sample["text"]);
    let mut stale = sample;
    stale["text"] = json!("older text");
    assert!(state
        .recover_reasoning_snapshot("turn-1", &stale, 4)
        .unwrap()
        .is_none());
    assert!(state
        .recover_reasoning_snapshot("other-turn", &stale, 4)
        .is_err());
}
