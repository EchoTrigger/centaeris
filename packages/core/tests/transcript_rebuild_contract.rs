use centaeris_core::session::transcript::{
    parse_transcript_projection_source_envelope, parse_transcript_rebuild_ledger_fact,
    rebuild_transcript_generation_v1, TranscriptBlockBodyV1, TranscriptGenerationRebuildRequestV1,
    TranscriptGenerationRebuildSourcePortV1, TranscriptPagePolicyV1, TranscriptPageReadRequestV1,
    TranscriptPageReadResultV1, TranscriptPatchReadRequestV1, TranscriptPatchReadResultV1,
    TranscriptProjectionCheckpointV1, TranscriptProjectionCommitDispositionV1,
    TranscriptProjectionCommitV1, TranscriptProjectionCurrentGenerationV1,
    TranscriptProjectionGenerationRotationDispositionV1, TranscriptProjectionGenerationRotationV1,
    TranscriptProjectionGenerationStorePortV1, TranscriptProjectionHeadV1,
    TranscriptProjectionPayloadRequirementV1, TranscriptProjectionRecoveryV1,
    TranscriptProjectionSourceEnvelopeV1, TranscriptProjectionStorePort,
    TranscriptProjectionUpdateV1, TranscriptProjectorV1, TranscriptRebuildLedgerFactV1,
    TranscriptRebuildProjectionFactV1, TranscriptResumeCursorV1,
    TRANSCRIPT_PROJECTION_GENERATION_INVALIDATED, TRANSCRIPT_PROJECTION_VERSION_V1,
};
use centaeris_core::session::{
    parse_event, parse_wire_record, wire_record_value, SequencedSessionRecord, SessionRecordType,
};
use serde_json::{json, Value};
use std::sync::Mutex;

struct RecordingStore {
    commits: Mutex<Vec<TranscriptProjectionCommitV1>>,
    current: Mutex<Option<TranscriptProjectionCurrentGenerationV1>>,
}

impl RecordingStore {
    fn with_current(generation: &str, source_high_water: &str) -> Self {
        Self::with_current_for("session-1", generation, source_high_water)
    }

    fn with_current_for(session_id: &str, generation: &str, source_high_water: &str) -> Self {
        Self {
            commits: Mutex::new(Vec::new()),
            current: Mutex::new(Some(TranscriptProjectionCurrentGenerationV1 {
                session_id: session_id.to_string(),
                projection_version: TRANSCRIPT_PROJECTION_VERSION_V1.to_string(),
                projection_generation: generation.to_string(),
                source_high_water: source_high_water.to_string(),
            })),
        }
    }
}

#[test]
fn rebuild_identities_do_not_collide_between_sessions_at_the_same_generation_and_h() {
    let first_wire = wire_record_value(&record(
        1,
        "user_message",
        "event-user",
        json!({"messageId": "message-user", "text": "text", "attachments": []}),
    ))
    .expect("wire");
    let mut second_wire = first_wire.clone();
    second_wire["sessionId"] = json!("session-2");
    second_wire["eventId"] = json!("event-user-2");

    let run = |session_id: &str, wire: &Value| {
        let source = MemorySource {
            ledger: vec![parse_transcript_rebuild_ledger_fact(wire).expect("ledger")],
            projection: vec![full_fact(wire)],
        };
        let request = TranscriptGenerationRebuildRequestV1 {
            session_id: session_id.to_string(),
            projection_generation: "generation-2".to_string(),
            target_source_high_water: "1".to_string(),
            expected_current_generation: Some("generation-1".to_string()),
        };
        let store = RecordingStore::with_current_for(session_id, "generation-1", "1");
        rebuild_transcript_generation_v1(&request, &source, &store).expect("rebuild");
        let commits = store.commits.into_inner().expect("commits");
        let commit = commits.into_iter().next().expect("commit");
        (
            commit.commit_id,
            commit.checkpoint.expect("checkpoint").frontier_ref,
            commit.frontier.expect("frontier").frontier_ref,
        )
    };

    let first = run("session-1", &first_wire);
    let second = run("session-2", &second_wire);
    assert_ne!(first.0, second.0);
    assert_ne!(first.1, second.1);
    assert_ne!(first.2, second.2);
}

impl TranscriptProjectionStorePort for RecordingStore {
    fn commit_transcript_projection(
        &self,
        commit: TranscriptProjectionCommitV1,
    ) -> Result<TranscriptProjectionCommitDispositionV1, String> {
        commit.validate()?;
        self.commits.lock().expect("commits lock").push(commit);
        Ok(TranscriptProjectionCommitDispositionV1::Applied)
    }

    fn load_transcript_projection_head(
        &self,
        _session_id: &str,
        _projection_generation: &str,
    ) -> Result<Option<TranscriptProjectionHeadV1>, String> {
        unreachable!("not used")
    }

    fn load_transcript_page(
        &self,
        _request: TranscriptPageReadRequestV1,
    ) -> Result<TranscriptPageReadResultV1, String> {
        unreachable!("current-only rejection occurs before storage read")
    }

    fn load_latest_transcript_checkpoint(
        &self,
        _session_id: &str,
        _projection_generation: &str,
        _at_or_before_source_high_water: u64,
    ) -> Result<Option<TranscriptProjectionCheckpointV1>, String> {
        unreachable!("not used")
    }

    fn load_latest_transcript_recovery(
        &self,
        _session_id: &str,
        _projection_generation: &str,
        _at_or_before_source_high_water: u64,
    ) -> Result<Option<TranscriptProjectionRecoveryV1>, String> {
        unreachable!("not used")
    }

    fn load_transcript_patches(
        &self,
        _request: TranscriptPatchReadRequestV1,
    ) -> Result<TranscriptPatchReadResultV1, String> {
        unreachable!("not used")
    }

    fn load_transcript_resume_cursors(
        &self,
        _session_id: &str,
        _projection_generation: &str,
        _at_or_before_source_high_water: u64,
    ) -> Result<Vec<TranscriptResumeCursorV1>, String> {
        unreachable!("not used")
    }
}

impl TranscriptProjectionGenerationStorePortV1 for RecordingStore {
    fn load_current_transcript_projection_generation(
        &self,
        _session_id: &str,
    ) -> Result<Option<TranscriptProjectionCurrentGenerationV1>, String> {
        Ok(self.current.lock().expect("current lock").clone())
    }

    fn rotate_current_transcript_projection_generation(
        &self,
        rotation: TranscriptProjectionGenerationRotationV1,
    ) -> Result<TranscriptProjectionGenerationRotationDispositionV1, String> {
        rotation.validate()?;
        let complete = self
            .commits
            .lock()
            .expect("commits lock")
            .iter()
            .any(|commit| {
                commit.projection_generation == rotation.next_generation
                    && commit.source_high_water == rotation.target_source_high_water
                    && commit.invalidation_reason.is_none()
            });
        if !complete {
            return Err("next transcript generation is incomplete".to_string());
        }
        let mut current = self.current.lock().expect("current lock");
        if current
            .as_ref()
            .map(|value| value.projection_generation.as_str())
            != rotation.expected_current_generation.as_deref()
        {
            return Err("transcript generation rotation compare-and-swap failed".to_string());
        }
        *current = Some(TranscriptProjectionCurrentGenerationV1 {
            session_id: rotation.session_id,
            projection_version: rotation.projection_version,
            projection_generation: rotation.next_generation,
            source_high_water: rotation.target_source_high_water,
        });
        Ok(TranscriptProjectionGenerationRotationDispositionV1::Applied)
    }
}

#[derive(Clone)]
struct MemorySource {
    ledger: Vec<TranscriptRebuildLedgerFactV1>,
    projection: Vec<TranscriptRebuildProjectionFactV1>,
}

impl TranscriptGenerationRebuildSourcePortV1 for MemorySource {
    fn scan_transcript_rebuild_ledger(
        &self,
        _request: &TranscriptGenerationRebuildRequestV1,
        visitor: &mut dyn FnMut(TranscriptRebuildLedgerFactV1) -> Result<(), String>,
    ) -> Result<(), String> {
        self.ledger.iter().cloned().try_for_each(visitor)
    }

    fn scan_transcript_rebuild_projection(
        &self,
        _request: &TranscriptGenerationRebuildRequestV1,
        visitor: &mut dyn FnMut(TranscriptRebuildProjectionFactV1) -> Result<(), String>,
    ) -> Result<(), String> {
        self.projection.iter().cloned().try_for_each(visitor)
    }
}

fn record(
    sequence: u64,
    event_type: &str,
    event_id: &str,
    payload: Value,
) -> SequencedSessionRecord {
    SequencedSessionRecord {
        sequence,
        event: parse_event(&json!({
            "schemaVersion": "session.event.v1",
            "eventVersion": 1,
            "type": event_type,
            "eventId": event_id,
            "sessionId": "session-1",
            "turnId": "turn-1",
            "agentRunId": "run-1",
            "createdAtMs": sequence,
            "payload": payload,
        }))
        .expect("valid event"),
    }
}

fn full_fact(raw: &Value) -> TranscriptRebuildProjectionFactV1 {
    let record = parse_wire_record(raw).expect("full source record");
    let sequence = record.sequence;
    TranscriptRebuildProjectionFactV1::FullRecord {
        record,
        stream_id: "run-1".to_string(),
        applied_cursor: format!("cursor-{sequence}"),
    }
}

fn envelope_fact(raw: &Value) -> TranscriptRebuildProjectionFactV1 {
    let envelope = parse_transcript_projection_source_envelope(raw).expect("source envelope");
    let sequence = envelope.sequence;
    TranscriptRebuildProjectionFactV1::Envelope {
        envelope,
        stream_id: "run-1".to_string(),
        applied_cursor: format!("cursor-{sequence}"),
    }
}

#[test]
fn strict_flat_wire_parser_classifies_payload_without_decoding_it() {
    let mut raw = wire_record_value(&record(
        1,
        "provider_usage",
        "event-1",
        json!({
            "inputTokens": 1, "outputTokens": 1, "totalTokens": 2,
            "promptCacheHitTokens": 0, "promptCacheMissTokens": 1
        }),
    ))
    .expect("wire");
    let envelope = parse_transcript_projection_source_envelope(&raw).expect("envelope");
    assert_eq!(
        envelope
            .transcript_payload_requirement()
            .expect("classification"),
        TranscriptProjectionPayloadRequirementV1::EnvelopeOnly
    );
    raw.as_object_mut()
        .expect("wire object")
        .insert("futureField".to_string(), Value::Bool(true));
    assert!(parse_transcript_projection_source_envelope(&raw)
        .expect_err("unknown outer field")
        .contains("unknown field"));
    raw.as_object_mut()
        .expect("wire object")
        .remove("futureField");
    raw["type"] = json!("future_event");
    assert!(parse_transcript_projection_source_envelope(&raw)
        .expect_err("unknown type")
        .contains("unsupported"));

    let mut invalid_time = wire_record_value(&record(
        2,
        "provider_usage",
        "event-2",
        json!({
            "inputTokens": 1, "outputTokens": 1, "totalTokens": 2,
            "promptCacheHitTokens": 0, "promptCacheMissTokens": 1
        }),
    ))
    .expect("wire");
    invalid_time["createdAtMs"] = json!(-1);
    assert!(parse_transcript_projection_source_envelope(&invalid_time)
        .expect_err("negative time")
        .contains("createdAtMs"));
    invalid_time["createdAtMs"] = json!(2);
    invalid_time["turnId"] = json!("");
    assert!(parse_transcript_projection_source_envelope(&invalid_time)
        .expect_err("empty turn")
        .contains("turnId"));
}

#[test]
fn payload_free_envelope_commits_a_legal_empty_change() {
    let mut projector =
        TranscriptProjectorV1::new("session-1".to_string(), "generation-1".to_string())
            .expect("projector");
    let store = RecordingStore::with_current("generation-1", "0");
    let envelope = TranscriptProjectionSourceEnvelopeV1 {
        sequence: 1,
        schema_version: "session.event.v1".to_string(),
        event_version: 1,
        event_type: SessionRecordType::ProviderUsage,
        event_id: "event-1".to_string(),
        session_id: "session-1".to_string(),
    };
    let update = projector
        .apply_envelope_and_commit(&envelope, "run-1", "cursor-1", "commit-1", None, &store)
        .expect("known neutral envelope");
    assert!(matches!(
        update,
        TranscriptProjectionUpdateV1::NoDisplayChange { .. }
    ));
    let commits = store.commits.lock().expect("commits lock");
    assert!(commits[0].upserts.is_empty());
    assert_eq!(commits[0].source_high_water, "1");
    assert_eq!(commits[0].resume_cursors[0].cursor, "cursor-1");
}

#[test]
fn rebuild_skips_heavy_neutral_payload_and_publishes_only_after_reaching_h() {
    let user = wire_record_value(&record(
        1,
        "user_message",
        "event-user",
        json!({"messageId": "message-user", "text": "keep me", "attachments": []}),
    ))
    .expect("user wire");
    let assistant = wire_record_value(&record(
        2,
        "assistant_message",
        "event-assistant",
        json!({
            "messageId": "message:turn-1:assistant", "modelMarkdown": "remove me",
            "artifactRefs": [], "status": "done"
        }),
    ))
    .expect("assistant wire");
    // This payload intentionally does not satisfy model_request_started's full schema. The
    // transcript rebuild must never hydrate it because Core classifies the envelope as neutral.
    let heavy_neutral = json!({
        "schemaVersion": "session.event.v1", "eventVersion": 1, "sequence": 3,
        "type": "model_request_started", "eventId": "event-heavy", "sessionId": "session-1",
        "turnId": "turn-1", "agentRunId": "run-1", "createdAtMs": 3,
        "payload": {"observations": "opaque-heavy-reference"}
    });
    let tombstone = wire_record_value(&record(
        4,
        "tombstone",
        "event-tombstone",
        json!({
            "tombstoneId": "tombstone-1",
            "targetEventIds": ["event-assistant"],
            "reasonType": "rewrite"
        }),
    ))
    .expect("tombstone wire");
    let raws = [&user, &assistant, &heavy_neutral, &tombstone];
    let source = MemorySource {
        ledger: raws
            .iter()
            .map(|raw| parse_transcript_rebuild_ledger_fact(raw).expect("ledger fact"))
            .collect(),
        projection: vec![
            full_fact(&user),
            full_fact(&assistant),
            envelope_fact(&heavy_neutral),
            envelope_fact(&tombstone),
        ],
    };
    let request = TranscriptGenerationRebuildRequestV1 {
        session_id: "session-1".to_string(),
        projection_generation: "generation-2".to_string(),
        target_source_high_water: "4".to_string(),
        expected_current_generation: Some("generation-1".to_string()),
    };
    let store = RecordingStore::with_current("generation-1", "4");
    let projector =
        rebuild_transcript_generation_v1(&request, &source, &store).expect("Core-owned rebuild");
    let page = projector
        .page_at(4, None, TranscriptPagePolicyV1::default())
        .expect("page");
    assert_eq!(page.source_high_water, "4");
    assert_eq!(page.blocks.len(), 1);
    assert_eq!(page.blocks[0].order_key.source_sequence, "1");
    assert!(matches!(
        page.blocks[0].body,
        TranscriptBlockBodyV1::UserText { .. }
    ));
    assert_eq!(page.resume_cursors[0].cursor, "cursor-4");
    let commits = store.commits.lock().expect("commits lock");
    assert_eq!(commits.len(), 4);
    assert!(commits[1].upserts.is_empty());
    assert!(commits[2].upserts.is_empty());
    assert!(commits[3].checkpoint.is_some());
    drop(commits);
    assert_eq!(
        store
            .current
            .lock()
            .expect("current lock")
            .as_ref()
            .expect("current")
            .projection_generation,
        "generation-2"
    );
}

#[test]
fn incomplete_rebuild_keeps_old_generation_and_old_cursor_is_explicitly_invalid_after_rotation() {
    let user = wire_record_value(&record(
        1,
        "user_message",
        "event-user",
        json!({"messageId": "message-user", "text": "text", "attachments": []}),
    ))
    .expect("wire");
    let source = MemorySource {
        ledger: vec![parse_transcript_rebuild_ledger_fact(&user).expect("ledger")],
        projection: vec![full_fact(&user)],
    };
    let request = TranscriptGenerationRebuildRequestV1 {
        session_id: "session-1".to_string(),
        projection_generation: "generation-2".to_string(),
        target_source_high_water: "2".to_string(),
        expected_current_generation: Some("generation-1".to_string()),
    };
    let store = RecordingStore::with_current("generation-1", "2");
    assert!(rebuild_transcript_generation_v1(&request, &source, &store)
        .expect_err("incomplete")
        .contains("targetSourceHighWater"));
    assert_eq!(
        store
            .current
            .lock()
            .expect("current lock")
            .as_ref()
            .expect("current")
            .projection_generation,
        "generation-1"
    );

    *store.current.lock().expect("current lock") = Some(TranscriptProjectionCurrentGenerationV1 {
        session_id: "session-1".to_string(),
        projection_version: TRANSCRIPT_PROJECTION_VERSION_V1.to_string(),
        projection_generation: "generation-2".to_string(),
        source_high_water: "2".to_string(),
    });
    let error = store
        .load_current_transcript_page(TranscriptPageReadRequestV1 {
            session_id: "session-1".to_string(),
            projection_version: TRANSCRIPT_PROJECTION_VERSION_V1.to_string(),
            projection_generation: "generation-1".to_string(),
            source_high_water: "2".to_string(),
            older_cursor: None,
            policy: TranscriptPagePolicyV1::default(),
        })
        .expect_err("old cursor generation");
    assert_eq!(error, TRANSCRIPT_PROJECTION_GENERATION_INVALIDATED);
}
