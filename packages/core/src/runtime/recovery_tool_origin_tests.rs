use super::*;
use crate::session::recovery::SessionRecoveryAccumulator;
use crate::session::{canonical_session_record, SessionLogRecord, SessionRecordType};

fn original_call(event_id: &str, run: &str, turn: &str) -> SessionLogRecord {
    let call = ToolCallEnvelope {
        id: "reused-call".into(),
        name: "stream_boundary_test_tool".into(),
        args_json: r#"{"value":"unexecuted"}"#.into(),
    };
    let mut record = canonical_tool_call_record(
        "origin-recovery",
        turn,
        run,
        &call,
        "test-provider",
        &format!("sha256:{}", "a".repeat(64)),
        "fixture",
        1,
    )
    .unwrap();
    record.event_id = event_id.into();
    record
}

#[test]
fn query_loop_streamed_admission_keeps_inactive_tool_origin_ambiguity() {
    let store = AgentRuntimeTestStore::new();
    let executions = Arc::new(AtomicUsize::new(0));
    let engine = AgentRuntime::new_for_test_with_tools(
        store.clone(),
        stream_execution_boundary_tool_layer(executions.clone()),
        AgentRuntimeConfig::default(),
    );
    let old = original_call("old-call", "old-run", "old-turn");
    let active = original_call("active-call", "new-run", "new-turn");
    let tombstone = canonical_session_record(
        "hide-old",
        SessionRecordType::Tombstone,
        "origin-recovery",
        None,
        None,
        3,
        json!({"tombstoneId":"hide-old","targetEventIds":["old-call"],"reasonType":"rewrite_tail"}),
    )
    .unwrap();
    let records = vec![old.clone(), active.clone(), tombstone.clone()];
    let expected = engine
        .plan_new_user_turn_closures(
            "origin-recovery",
            &records,
            "trigger-run",
            "trigger-turn",
            3,
            4,
        )
        .unwrap_err();
    assert!(expected.contains("identity is ambiguous"));
    let mut recovery = SessionRecoveryAccumulator::new("origin-recovery");
    recovery.push_inactive_record(old).unwrap();
    recovery.push(active).unwrap();
    recovery.push_inactive_record(tombstone).unwrap();
    let recovery = recovery.finish().unwrap();
    let actual = engine.plan_new_user_turn_closures_from_recovery(
        "origin-recovery",
        &recovery,
        "trigger-run",
        "trigger-turn",
        3,
        4,
    );
    assert_eq!(actual.unwrap_err(), expected);
    assert_eq!(executions.load(Ordering::SeqCst), 0);
    assert!(store
        .list_events("origin-recovery", 10, 0)
        .unwrap()
        .is_empty());
}

#[test]
fn query_loop_streamed_admission_keeps_last_inactive_same_scope_call_identity() {
    let store = AgentRuntimeTestStore::new();
    let executions = Arc::new(AtomicUsize::new(0));
    let engine = AgentRuntime::new_for_test_with_tools(
        store.clone(),
        stream_execution_boundary_tool_layer(executions.clone()),
        AgentRuntimeConfig::default(),
    );
    let active = original_call("first-active", "run", "turn");
    let hidden = original_call("last-hidden", "run", "turn");
    let tombstone = canonical_session_record("hide-last",SessionRecordType::Tombstone,"origin-recovery",None,None,3,
        json!({"tombstoneId":"hide-last","targetEventIds":["last-hidden"],"reasonType":"rewrite_tail"})).unwrap();
    let records = vec![active.clone(), hidden.clone(), tombstone.clone()];
    let expected = engine
        .plan_new_user_turn_closures(
            "origin-recovery",
            &records,
            "trigger-run",
            "trigger-turn",
            3,
            4,
        )
        .unwrap();
    assert_eq!(
        expected.closures[0].call_event_id.as_deref(),
        Some("last-hidden")
    );
    let mut recovery = SessionRecoveryAccumulator::new("origin-recovery");
    recovery.push(active).unwrap();
    recovery.push_inactive_record(hidden).unwrap();
    recovery.push_inactive_record(tombstone).unwrap();
    let recovery = recovery.finish().unwrap();
    let actual = engine
        .plan_new_user_turn_closures_from_recovery(
            "origin-recovery",
            &recovery,
            "trigger-run",
            "trigger-turn",
            3,
            4,
        )
        .unwrap();
    assert_eq!(actual.closures.len(), expected.closures.len());
    assert_eq!(
        actual.closures[0].call_event_id,
        expected.closures[0].call_event_id
    );
    assert_eq!(
        actual.closures[0].agent_run_id,
        expected.closures[0].agent_run_id
    );
    assert_eq!(actual.closures[0].turn_id, expected.closures[0].turn_id);
    assert_eq!(
        serde_json::to_value(&actual.closures[0].result).unwrap(),
        serde_json::to_value(&expected.closures[0].result).unwrap()
    );
    assert_eq!(executions.load(Ordering::SeqCst), 0);
}
