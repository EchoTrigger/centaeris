use centaeris_core::session::transcript::{
    TranscriptBlockBodyV1, TranscriptBlockStatusV1, TranscriptPagePolicyV1,
    TranscriptPageReadRequestV1, TranscriptPageReadResultV1, TranscriptPatchReadRequestV1,
    TranscriptPatchReadResultV1, TranscriptProjectionCheckpointV1,
    TranscriptProjectionCommitDispositionV1, TranscriptProjectionCommitV1,
    TranscriptProjectionFrontierV1, TranscriptProjectionHeadV1, TranscriptProjectionStorePort,
    TranscriptProjectionUpdateV1, TranscriptProjectorV1, TranscriptResumeCursorV1,
};
use centaeris_core::session::{parse_event, SequencedSessionRecord};
use serde_json::{json, Value};
use std::sync::Mutex;

#[derive(Default)]
struct RejectOnceStore {
    commits: Mutex<Vec<TranscriptProjectionCommitV1>>,
}

impl TranscriptProjectionStorePort for RejectOnceStore {
    fn commit_transcript_projection(
        &self,
        commit: TranscriptProjectionCommitV1,
    ) -> Result<TranscriptProjectionCommitDispositionV1, String> {
        let mut commits = self.commits.lock().expect("commits lock");
        if commits.is_empty() {
            commits.push(commit);
            return Err("injected commit failure".to_string());
        }
        commits.push(commit);
        Ok(TranscriptProjectionCommitDispositionV1::Applied)
    }

    fn load_transcript_projection_head(
        &self,
        _session_id: &str,
        _projection_generation: &str,
    ) -> Result<Option<TranscriptProjectionHeadV1>, String> {
        unreachable!("not used by this test")
    }

    fn load_transcript_page(
        &self,
        _request: TranscriptPageReadRequestV1,
    ) -> Result<TranscriptPageReadResultV1, String> {
        unreachable!("not used by this test")
    }

    fn load_latest_transcript_checkpoint(
        &self,
        _session_id: &str,
        _projection_generation: &str,
        _at_or_before_source_high_water: u64,
    ) -> Result<Option<TranscriptProjectionCheckpointV1>, String> {
        unreachable!("not used by this test")
    }

    fn load_latest_transcript_recovery(
        &self,
        _session_id: &str,
        _projection_generation: &str,
        _at_or_before_source_high_water: u64,
    ) -> Result<Option<centaeris_core::session::transcript::TranscriptProjectionRecoveryV1>, String>
    {
        unreachable!("not used by this test")
    }

    fn load_transcript_patches(
        &self,
        _request: TranscriptPatchReadRequestV1,
    ) -> Result<TranscriptPatchReadResultV1, String> {
        unreachable!("not used by this test")
    }

    fn load_transcript_resume_cursors(
        &self,
        _session_id: &str,
        _projection_generation: &str,
        _at_or_before_source_high_water: u64,
    ) -> Result<Vec<TranscriptResumeCursorV1>, String> {
        unreachable!("not used by this test")
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
        .expect("valid session event"),
    }
}

fn tool_call(sequence: u64) -> SequencedSessionRecord {
    record(
        sequence,
        "tool_call",
        "event-tool-call",
        json!({
            "callId": "call-1",
            "toolName": "read",
            "toolContractDigest": format!("sha256:{}", "a".repeat(64)),
            "providerId": "builtin",
            "normalizedInput": {"path": "README.md"},
            "displayTarget": "README.md"
        }),
    )
}

fn tool_result(sequence: u64) -> SequencedSessionRecord {
    record(
        sequence,
        "tool_result",
        "event-tool-result",
        json!({
            "callId": "call-1",
            "toolName": "read",
            "resultState": "successWithOutput",
            "modelContent": "contents",
            "fullOutputPath": null,
            "outputStartByte": null,
            "outputByteLength": 8,
            "outputComplete": true,
            "summary": "Read README.md",
            "operations": [],
            "modelInputImages": [],
            "latencyMs": 10
        }),
    )
}

#[test]
fn committed_tool_result_emits_a_revision_patch_at_the_original_order_key() {
    let mut projector =
        TranscriptProjectorV1::new("session-1".to_string(), "generation-1".to_string())
            .expect("projector");

    let call = projector
        .apply(&tool_call(100), "run-1", "cursor-100")
        .expect("tool call patch");
    let TranscriptProjectionUpdateV1::Patch(call) = call else {
        panic!("tool call must emit a patch");
    };
    assert_eq!(call.upserts[0].block_revision, "1");

    let result = projector
        .apply(&tool_result(1_000), "run-1", "cursor-1000")
        .expect("tool result patch");
    let TranscriptProjectionUpdateV1::Patch(result) = result else {
        panic!("tool result must emit a patch");
    };
    assert_eq!(result.upserts[0].block_revision, "2");
    assert_eq!(result.upserts[0].order_key.source_sequence, "100");
    assert!(matches!(
        result.upserts[0].body,
        TranscriptBlockBodyV1::Tool {
            status: TranscriptBlockStatusV1::Completed,
            ..
        }
    ));

    let page = projector
        .page_at(1_000, None, TranscriptPagePolicyV1::default())
        .expect("page at tool completion");
    assert_eq!(page.blocks, result.upserts);
    assert_eq!(page.resume_cursors[0].cursor, "cursor-1000");
}

#[test]
fn tombstone_invalidates_the_view_instead_of_attempting_partial_undo() {
    let mut projector =
        TranscriptProjectorV1::new("session-1".to_string(), "generation-1".to_string())
            .expect("projector");
    projector
        .apply(&tool_call(100), "run-1", "cursor-100")
        .expect("tool call");
    let tombstone = record(
        101,
        "tombstone",
        "event-tombstone",
        json!({
            "tombstoneId": "tombstone-1",
            "targetEventIds": ["event-tool-call"],
            "reasonType": "rewrite"
        }),
    );

    let update = projector
        .apply(&tombstone, "run-1", "cursor-101")
        .expect("tombstone update");
    assert!(matches!(
        update,
        TranscriptProjectionUpdateV1::ViewInvalidated { .. }
    ));
    assert!(projector
        .page_at(101, None, TranscriptPagePolicyV1::default())
        .is_err());
}

#[test]
fn store_failure_does_not_advance_the_shared_projector_before_retry() {
    let mut projector =
        TranscriptProjectorV1::new("session-1".to_string(), "generation-1".to_string())
            .expect("projector");
    let store = RejectOnceStore::default();
    let call = tool_call(100);

    let error = projector
        .apply_and_commit(
            &call,
            "run-1",
            "cursor-100",
            "projection-commit-100",
            None,
            &store,
        )
        .expect_err("first store write fails");
    assert!(error.contains("injected commit failure"));
    assert!(projector
        .page_at(100, None, TranscriptPagePolicyV1::default())
        .is_err());

    let update = projector
        .apply_and_commit(
            &call,
            "run-1",
            "cursor-100",
            "projection-commit-100",
            None,
            &store,
        )
        .expect("retry commits once");
    assert!(matches!(update, TranscriptProjectionUpdateV1::Patch(_)));
    let commits = store.commits.lock().expect("commits lock");
    assert_eq!(commits.len(), 2);
    assert_eq!(commits[1].expected_source_high_water, "0");
    assert_eq!(commits[1].source_high_water, "100");
    assert_eq!(commits[1].upserts[0].applied_source_sequence, "100");
}

#[test]
fn projector_restores_unsettled_tool_state_from_a_bounded_frontier() {
    let mut projector =
        TranscriptProjectorV1::new("session-1".to_string(), "generation-1".to_string())
            .expect("projector");
    projector
        .apply(&tool_call(100), "run-1", "cursor-100")
        .expect("tool call");

    let recovery = projector
        .checkpoint_recovery("frontier-100", "block-index-1")
        .expect("checkpoint recovery");
    assert_eq!(recovery.frontier.open_tools.len(), 1);
    let encoded = serde_json::to_value(&recovery.frontier).expect("encode frontier");
    let decoded: TranscriptProjectionFrontierV1 =
        serde_json::from_value(encoded).expect("strict frontier roundtrip");

    let mut restored = TranscriptProjectorV1::from_checkpoint_recovery(recovery)
        .expect("restore projector frontier");
    let update = restored
        .apply(&tool_result(1_000), "run-1", "cursor-1000")
        .expect("tool result after restore");
    let TranscriptProjectionUpdateV1::Patch(patch) = update else {
        panic!("restored tool result must emit a patch");
    };
    assert_eq!(patch.upserts[0].block_revision, "2");
    assert_eq!(patch.upserts[0].order_key.source_sequence, "100");

    let duplicate = restored
        .apply(&tool_result(1_001), "run-1", "cursor-1001")
        .expect_err("settled tool is absent from the recovery frontier");
    assert!(duplicate.contains("no projected tool call"));
    assert_eq!(decoded.source_high_water, "100");
}
