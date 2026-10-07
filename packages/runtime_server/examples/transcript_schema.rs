// Use the actual HTTP wire types, not a parallel exporter-only model.
#[path = "../src/transcript_protocol/wire.rs"]
mod wire;

use centaeris_core::session::transcript::*;
use serde_json::{json, Value};
use wire::*;

fn schema<T: schemars::JsonSchema>(request: bool) -> Value {
    let settings = schemars::generate::SchemaSettings::draft2020_12();
    let settings = if request {
        settings.for_deserialize()
    } else {
        settings.for_serialize()
    };
    serde_json::to_value(settings.into_generator().into_root_schema_for::<T>()).unwrap()
}

fn main() {
    let page_request = PageRequest {
        schema: "runtime.transcript.page.read.v1".into(),
        session_id: "session-1".into(),
        projection_version: "transcript.projection.v1".into(),
        projection_generation: None,
        source_high_water: "0".into(),
        older_cursor: None,
    };
    let patch_request = PatchRequest {
        schema: "runtime.transcript.patch.read.v1".into(),
        session_id: "session-1".into(),
        projection_version: "transcript.projection.v1".into(),
        projection_generation: "generation-1".into(),
        after_source_high_water: "0".into(),
        through_source_high_water: "1".into(),
    };
    let content_request = TranscriptContentRangeReadRequestV1 {
        schema: TRANSCRIPT_CONTENT_RANGE_REQUEST_SCHEMA_V1.into(),
        session_id: "session-1".into(),
        projection_version: "transcript.projection.v1".into(),
        projection_generation: "generation-1".into(),
        ref_id: "session-event:event-1:modelMarkdown".into(),
        revision: "1".into(),
        byte_length: "3".into(),
        offset: "0".into(),
        max_bytes: 65536,
    };
    let block = TranscriptBlockV1 {
        block_id: "block-1".into(),
        block_revision: "1".into(),
        presentation: Some(TranscriptPresentationV1 {
            agent_run_id: None,
            source_type: "user_message".into(),
            observed_at_ms: 0,
            display_target: None,
            duration_ms: None,
            operation: None,
        }),
        order_key: TranscriptOrderKeyV1 {
            source_sequence: "1".into(),
            ordinal: 0,
        },
        body: TranscriptBlockBodyV1::UserText {
            content: TranscriptTextContentV1::inline("中".into()),
        },
    };
    let page = TranscriptPageV1 {
        schema: TRANSCRIPT_PAGE_SCHEMA_V1.into(),
        session_id: "session-1".into(),
        projection_version: "transcript.projection.v1".into(),
        projection_generation: "generation-1".into(),
        source_high_water: "1".into(),
        blocks: vec![block.clone()],
        older_cursor: None,
        has_older: false,
        resume_cursors: vec![TranscriptResumeCursorV1 {
            stream_id: "workspace-transcript.v1".into(),
            cursor: "1".into(),
        }],
    };
    let patch = TranscriptPatchV1 {
        schema: "transcript.patch.v1".into(),
        session_id: "session-1".into(),
        projection_version: "transcript.projection.v1".into(),
        projection_generation: "generation-1".into(),
        source_high_water: "1".into(),
        stream_id: "workspace-transcript.v1".into(),
        applied_cursor: "1".into(),
        upserts: vec![block],
        removals: vec![TranscriptBlockRemovalV1 {
            block_id: "removed".into(),
            block_revision: "1".into(),
            presentation: None,
        }],
    };
    let patches = PatchPage {
        schema: "transcript.patch.page.v1",
        session_id: "session-1".into(),
        projection_version: "transcript.projection.v1".into(),
        projection_generation: "generation-1".into(),
        through_source_high_water: "1".into(),
        patches: vec![patch],
        next_source_high_water: "1".into(),
        has_more: false,
    };
    let content = TranscriptContentRangeV1 {
        schema: TRANSCRIPT_CONTENT_RANGE_SCHEMA_V1.into(),
        session_id: "session-1".into(),
        projection_version: "transcript.projection.v1".into(),
        projection_generation: "generation-1".into(),
        ref_id: content_request.ref_id.clone(),
        revision: "1".into(),
        byte_length: "3".into(),
        start_offset: "0".into(),
        end_offset: "3".into(),
        content: "中".into(),
        has_more: false,
    };
    let errors: Vec<_> = [
        ErrorCode::ContentRequestInvalid,
        ErrorCode::ContentUnavailable,
        ErrorCode::PageRequestInvalid,
        ErrorCode::PatchRequestInvalid,
        ErrorCode::ViewInvalidated,
    ]
    .into_iter()
    .map(|error| ErrorResponse { error })
    .collect();
    println!(
        "{}",
        json!({
            "schemas": {
                "pageRequest": schema::<PageRequest>(true), "patchRequest": schema::<PatchRequest>(true),
                "contentRequest": schema::<TranscriptContentRangeReadRequestV1>(true),
                "page": schema::<TranscriptPageV1>(false), "patches": schema::<PatchPage>(false),
                "content": schema::<TranscriptContentRangeV1>(false),
                "error": schema::<ErrorResponse>(false), "projectionNotReady": schema::<ProjectionNotReady>(false)
            },
            "samples": {
                "pageRequest": [page_request], "patchRequest": [patch_request], "contentRequest": [content_request],
                "page": [page], "patches": [patches], "content": [content], "error": errors,
                "projectionNotReady": [ProjectionNotReady { error: "transcript_projection_not_ready",
                    projection_generation: "generation-1", source_high_water: "1", projected_high_water: "0".into() }]
            }
        })
    );
}
