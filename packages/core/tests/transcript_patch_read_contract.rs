use centaeris_core::session::transcript::{
    TranscriptPatchReadRequestV1, TranscriptPatchReadResultV1, TranscriptPatchV1,
    TranscriptPersistentPatchQueryWorkV1, TRANSCRIPT_PATCH_SCHEMA_V1,
    TRANSCRIPT_PROJECTION_VERSION_V1,
};

fn patch(source_high_water: u64) -> TranscriptPatchV1 {
    TranscriptPatchV1 {
        schema: TRANSCRIPT_PATCH_SCHEMA_V1.to_string(),
        session_id: "session-1".to_string(),
        projection_version: TRANSCRIPT_PROJECTION_VERSION_V1.to_string(),
        projection_generation: "generation-1".to_string(),
        source_high_water: source_high_water.to_string(),
        stream_id: "run-1".to_string(),
        applied_cursor: format!("cursor-{source_high_water}"),
        upserts: Vec::new(),
        removals: Vec::new(),
    }
}

#[test]
fn committed_patch_read_is_exclusive_after_h_frozen_through_h_and_strictly_ascending() {
    let request = TranscriptPatchReadRequestV1 {
        session_id: "session-1".to_string(),
        projection_version: TRANSCRIPT_PROJECTION_VERSION_V1.to_string(),
        projection_generation: "generation-1".to_string(),
        after_source_high_water: "40".to_string(),
        through_source_high_water: "43".to_string(),
    };
    request.validate().expect("request");

    let result = TranscriptPatchReadResultV1 {
        patches: vec![patch(41), patch(43)],
        next_source_high_water: "43".to_string(),
        has_more: false,
        work: TranscriptPersistentPatchQueryWorkV1 {
            commit_rows_read: 2,
            raw_event_visits: 0,
        },
    };
    result.validate(&request).expect("result");

    let out_of_order = TranscriptPatchReadResultV1 {
        patches: vec![patch(43), patch(41)],
        next_source_high_water: "41".to_string(),
        has_more: true,
        work: TranscriptPersistentPatchQueryWorkV1 {
            commit_rows_read: 2,
            raw_event_visits: 0,
        },
    };
    assert!(out_of_order.validate(&request).is_err());
}

#[test]
fn empty_patch_read_can_only_finish_at_the_already_reached_frozen_waterline() {
    let request = TranscriptPatchReadRequestV1 {
        session_id: "session-1".to_string(),
        projection_version: TRANSCRIPT_PROJECTION_VERSION_V1.to_string(),
        projection_generation: "generation-1".to_string(),
        after_source_high_water: "43".to_string(),
        through_source_high_water: "43".to_string(),
    };
    let result = TranscriptPatchReadResultV1 {
        patches: Vec::new(),
        next_source_high_water: "43".to_string(),
        has_more: false,
        work: Default::default(),
    };

    result.validate(&request).expect("caught up result");
}

#[test]
fn patch_read_rejects_unknown_wire_fields_and_false_zero_work_reports() {
    let unknown = serde_json::json!({
        "sessionId": "session-1",
        "projectionVersion": TRANSCRIPT_PROJECTION_VERSION_V1,
        "projectionGeneration": "generation-1",
        "afterSourceHighWater": "40",
        "throughSourceHighWater": "43",
        "unexpected": true
    });
    assert!(serde_json::from_value::<TranscriptPatchReadRequestV1>(unknown).is_err());

    let request = TranscriptPatchReadRequestV1 {
        session_id: "session-1".to_string(),
        projection_version: TRANSCRIPT_PROJECTION_VERSION_V1.to_string(),
        projection_generation: "generation-1".to_string(),
        after_source_high_water: "40".to_string(),
        through_source_high_water: "41".to_string(),
    };
    let false_work = TranscriptPatchReadResultV1 {
        patches: vec![patch(41)],
        next_source_high_water: "41".to_string(),
        has_more: false,
        work: Default::default(),
    };

    assert!(false_work.validate(&request).is_err());
}
