use super::*;
use crate::session::recovery::SessionRecoveryAccumulator;

fn model_record(index: usize) -> SessionLogRecord {
    let mut record = canonical_session_record(
        format!("request-event-{index}"),
        SessionRecordType::ModelRequestStarted,
        "session-1",
        Some(format!("turn-{index}")),
        Some("agent-run-1".into()),
        index as i64,
        model_request_payload(vec![json!({"kind":"message", "message":{
            "messageId":format!("message-{index}"), "role":"user", "content":"context".repeat(1000),
            "toolCalls":[], "toolCallId":null, "reasoningContent":null
        }})]),
    )
    .expect("valid model record");
    record.payload["requestId"] = json!(format!("request-{index}"));
    record
}

#[test]
fn query_loop_streamed_recovery_matches_canonical_snapshot() {
    let records = (0..5).map(model_record).collect::<Vec<_>>();
    let expected = legacy_snapshot("session-1", &records);
    let mut accumulator = SessionRecoveryAccumulator::new("session-1");
    for record in records {
        accumulator.push(record).unwrap();
    }
    let actual = accumulator.finish().unwrap();
    assert_eq!(
        serde_json::to_value(actual.snapshot).unwrap(),
        serde_json::to_value(expected).unwrap()
    );
    assert!(actual
        .source_records
        .iter()
        .all(|record| record.event_type != SessionRecordType::ModelRequestStarted));
}

#[test]
fn query_loop_streamed_recovery_releases_superseded_model_bodies() {
    let mut accumulator = SessionRecoveryAccumulator::new("session-1");
    let one_request_bytes = model_record(0).payload["observations"].to_string().len();
    for index in 0..10_000 {
        accumulator.push(model_record(index)).unwrap();
        assert!(
            accumulator.retained_model_observation_bytes() <= one_request_bytes + 100,
            "historical contexts remain owned after request {index}: {} bytes",
            accumulator.retained_model_observation_bytes()
        );
    }
}

// Frozen pre-streaming selection path: retain every active body and reduce the
// complete history. The common message reducer remains covered by existing
// explicit expected snapshot tests; this oracle protects streaming selection.
fn legacy_snapshot(session_id: &str, records: &[SessionLogRecord]) -> SessionStateSnapshot {
    let active = active_session_records(session_id, records).unwrap();
    let projection = reduce_events(session_id, active.iter()).unwrap();
    let controls = crate::session::recovery::control_facts(&active).unwrap();
    let sources = crate::session::recovery::compaction_sources(&active).unwrap();
    restore_active_runtime_snapshot(
        session_id,
        &active,
        &projection,
        &controls,
        &sources,
        active.last().map_or(0, |record| record.created_at_ms),
    )
    .unwrap()
}

fn streamed_snapshot(session_id: &str, records: &[SessionLogRecord]) -> SessionStateSnapshot {
    use crate::session::recovery::SessionRecoveryFilter;
    let mut filter = SessionRecoveryFilter::new(session_id);
    for (index, record) in records.iter().enumerate() {
        filter
            .push_wire_envelope(
                &wire_record_value(&SequencedSessionRecord {
                    sequence: index as u64 + 1,
                    event: record.clone(),
                })
                .unwrap(),
            )
            .unwrap();
    }
    let mut accumulator = SessionRecoveryAccumulator::new(session_id);
    for record in records.iter().filter(|record| {
        record.event_type != SessionRecordType::Tombstone && !filter.is_inactive(&record.event_id)
    }) {
        accumulator.push(record.clone()).unwrap();
    }
    accumulator.finish().unwrap().snapshot
}

#[test]
fn query_loop_streamed_recovery_preserves_real_tool_history_and_tombstone_fallback() {
    let mut records = valid_log()
        .iter()
        .map(|wire| parse_event(wire).unwrap())
        .collect::<Vec<_>>();
    let session_id = records[0].session_id.clone();
    let mut main = model_record(0);
    main.session_id = session_id.clone();
    main.agent_run_id = records
        .iter()
        .find_map(|record| record.agent_run_id.clone());
    main.turn_id = Some("turn-1".into());
    main.created_at_ms = 99;
    records.push(main.clone());
    let expected = legacy_snapshot(&session_id, &records);
    assert_eq!(
        serde_json::to_value(streamed_snapshot(&session_id, &records)).unwrap(),
        serde_json::to_value(expected).unwrap()
    );
    records.push(canonical_session_record("hide-main",SessionRecordType::Tombstone,&session_id,None,None,100,
        json!({"targetEventIds":[main.event_id], "tombstoneId":"hide-main", "reasonType":"rewrite_tail"})).unwrap());
    assert_eq!(
        serde_json::to_value(streamed_snapshot(&session_id, &records)).unwrap(),
        serde_json::to_value(legacy_snapshot(&session_id, &records)).unwrap()
    );
}

#[test]
fn query_loop_streamed_recovery_keeps_committed_input_history_and_latest_delivery() {
    let mut records = Vec::new();
    for index in 0..3 {
        let mut record = model_record(index);
        let turn = if index == 0 {
            "turn-old"
        } else {
            "turn-current"
        };
        record.turn_id = Some(turn.into());
        let observations = record.payload["observations"].as_array_mut().unwrap();
        observations.push(json!({"kind":"required_completion_delivery", "state":{
            "schema":"runtime.completion_delivery.v1", "agentRunId":"agent-run-1",
            "authorizationDigest":format!("sha256:{}","a".repeat(64)),"toolName":"deliver",
            "inputKey":format!("sha256:{}","b".repeat(64)),"turnIds":[turn],
            "deliveredCallId":null,"repairAttempted":true,"draft":format!("draft-{index}")
        }}));
        observations.push(
            json!({"kind":"input_uptake","inputIds":[format!("input-{index}"),"input-common"]}),
        );
        records.push(record);
    }
    let snapshot = streamed_snapshot("session-1", &records);
    assert_eq!(
        serde_json::to_value(&snapshot).unwrap(),
        serde_json::to_value(legacy_snapshot("session-1", &records)).unwrap()
    );
    let uptake: Value = serde_json::from_str(&snapshot.metadata["turnInputRecovery"]).unwrap();
    assert_eq!(
        uptake,
        json!({
            "committedByRun":{"agent-run-1":["input-0","input-1","input-2","input-common"]},
            "preparedByTurn":{"turn-current":{"agentRunId":"agent-run-1","inputIds":["input-1","input-common","input-2"]}}
        })
    );
    let delivery: Value =
        serde_json::from_str(&snapshot.metadata["required_completion_delivery_v1_json"]).unwrap();
    assert_eq!(
        delivery,
        json!({
            "schema":"runtime.completion_delivery.v1", "agentRunId":"agent-run-1",
            "authorizationDigest":format!("sha256:{}","a".repeat(64)),"toolName":"deliver",
            "inputKey":format!("sha256:{}","b".repeat(64)),"turnIds":["turn-current"],
            "deliveredCallId":null,"repairAttempted":true,"draft":"draft-2"
        })
    );
}

#[test]
fn query_loop_streamed_recovery_preserves_compaction_source_digest_and_snapshot_proof() {
    let mut source = model_record(0);
    source.payload["purpose"] = json!("compaction");
    source.payload["observations"][0]["kind"] = json!("compaction_prompt");
    let compaction = canonical_session_record(
        "compaction-event",
        SessionRecordType::Compaction,
        "session-1",
        source.turn_id.clone(),
        source.agent_run_id.clone(),
        1,
        json!({"compactionId":"compaction-1","summaryMessageId":"summary-1",
            "summaryMarkdown":"committed summary","firstKeptMessageId":null,
            "createdReason":"context_pressure_threshold_reached"}),
    )
    .unwrap();
    let mut main = model_record(2);
    main.payload["observations"][0]["message"]["role"] = json!("system");
    main.payload["observations"][0]["message"]["messageId"] = json!("summary-1");
    main.payload["observations"][0]["message"]["content"] = json!("committed summary");
    let records = vec![source, compaction, main];
    let snapshot = streamed_snapshot("session-1", &records);
    assert_eq!(
        serde_json::to_value(&snapshot).unwrap(),
        serde_json::to_value(legacy_snapshot("session-1", &records)).unwrap()
    );
    let proof: Value =
        serde_json::from_str(&snapshot.metadata["compaction_reconstructed_main_proof_v1_json"])
            .unwrap();
    assert_eq!(proof["compactionEventId"], "compaction-event");
    assert_eq!(proof["source"]["requestId"], "request-0");
    assert_eq!(
        proof["source"]["promptSha256"],
        crate::runtime::canonical_json::sha256(
            "centaeris.compaction_prompt.v1",
            &"context".repeat(1000)
        )
        .unwrap()
    );
}

#[test]
fn query_loop_streamed_recovery_rejects_invalid_history_before_retirement() {
    let mut recovery = SessionRecoveryAccumulator::new("session-1");
    let mut invalid = model_record(0);
    invalid.payload["observations"][0]["unknown"] = json!(true);
    assert!(recovery.push(invalid).is_err());
    let mut recovery = SessionRecoveryAccumulator::new("session-1");
    recovery.push(model_record(0)).unwrap();
    assert!(recovery
        .push(model_record(0))
        .unwrap_err()
        .contains("duplicate eventId"));
    let mut foreign = model_record(1);
    foreign.session_id = "foreign".into();
    assert!(recovery
        .push(foreign)
        .unwrap_err()
        .contains("cross-session"));
}

#[test]
fn query_loop_streamed_recovery_filter_keeps_tombstone_authority() {
    use crate::session::recovery::SessionRecoveryFilter;
    let original = model_record(0);
    let wire = wire_record_value(&SequencedSessionRecord {
        sequence: 1,
        event: original,
    })
    .unwrap();
    let mut filter = SessionRecoveryFilter::new("session-1");
    filter.push_wire_envelope(&wire).unwrap();
    let tombstone = canonical_session_record("hide",SessionRecordType::Tombstone,"session-1",None,None,2,
        json!({"tombstoneId":"hide","targetEventIds":["request-event-0"],"reasonType":"rewrite_tail"})).unwrap();
    let tombstone = wire_record_value(&SequencedSessionRecord {
        sequence: 2,
        event: tombstone,
    })
    .unwrap();
    filter.push_wire_envelope(&tombstone).unwrap();
    assert!(filter.is_inactive("request-event-0"));
    assert!(filter.push_wire_envelope(&tombstone).is_err());
    assert!(SessionRecoveryFilter::new("session-1")
        .push_wire_envelope(&tombstone)
        .unwrap_err()
        .contains("prior event"));
    let mut invalid = wire.clone();
    invalid["schemaVersion"] = json!("unknown");
    assert!(SessionRecoveryFilter::new("session-1")
        .push_wire_envelope(&invalid)
        .is_err());
    invalid = wire.clone();
    invalid["type"] = json!("future_type");
    assert!(SessionRecoveryFilter::new("session-1")
        .push_wire_envelope(&invalid)
        .is_err());
    assert!(SessionRecoveryFilter::new("other")
        .push_wire_envelope(&wire)
        .is_err());
}
