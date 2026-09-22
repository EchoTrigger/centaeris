use centaeris_core::session::transcript::{
    TranscriptBlockBodyV1, TranscriptBlockIndexV1, TranscriptBlockStatusV1, TranscriptBlockV1,
    TranscriptOrderKeyV1, TranscriptPagePolicyV1, TranscriptPatchV1, TranscriptTextContentV1,
    TranscriptViewStateV1, TRANSCRIPT_PATCH_SCHEMA_V1, TRANSCRIPT_PROJECTION_VERSION_V1,
};
use std::collections::HashSet;

fn assistant_block(sequence: u64) -> TranscriptBlockV1 {
    TranscriptBlockV1 {
        block_id: format!("assistant:{sequence}"),
        block_revision: "1".to_string(),
        presentation: None,
        order_key: TranscriptOrderKeyV1 {
            source_sequence: sequence.to_string(),
            ordinal: 0,
        },
        body: TranscriptBlockBodyV1::AssistantText {
            content: TranscriptTextContentV1::inline(format!("answer {sequence}")),
            status: TranscriptBlockStatusV1::Completed,
        },
    }
}

fn tool_block(revision: u64, status: TranscriptBlockStatusV1) -> TranscriptBlockV1 {
    TranscriptBlockV1 {
        block_id: "tool:call-1".to_string(),
        block_revision: revision.to_string(),
        presentation: None,
        order_key: TranscriptOrderKeyV1 {
            source_sequence: "100".to_string(),
            ordinal: 0,
        },
        body: TranscriptBlockBodyV1::Tool {
            call_id: "call-1".to_string(),
            tool_name: "read".to_string(),
            status,
            summary: Some("README".to_string()),
            summary_ref: None,
            output_ref: None,
        },
    }
}

fn patch(
    source_high_water: u64,
    cursor: &str,
    upserts: Vec<TranscriptBlockV1>,
) -> TranscriptPatchV1 {
    TranscriptPatchV1 {
        schema: TRANSCRIPT_PATCH_SCHEMA_V1.to_string(),
        session_id: "session-1".to_string(),
        projection_version: TRANSCRIPT_PROJECTION_VERSION_V1.to_string(),
        projection_generation: "generation-1".to_string(),
        source_high_water: source_high_water.to_string(),
        stream_id: "run-1".to_string(),
        applied_cursor: cursor.to_string(),
        upserts,
        removals: Vec::new(),
    }
}

#[test]
fn late_history_page_cannot_overwrite_a_newer_hidden_block_override() {
    let mut index =
        TranscriptBlockIndexV1::new("session-1".to_string(), "generation-1".to_string())
            .expect("index");
    index
        .apply_committed(100, tool_block(1, TranscriptBlockStatusV1::Running))
        .expect("tool call");
    index
        .apply_committed(200, assistant_block(200))
        .expect("tail block");
    let one_block = TranscriptPagePolicyV1 {
        max_blocks: 1,
        ..TranscriptPagePolicyV1::default()
    };
    let tail = index.page_at(200, None, one_block).expect("tail page");
    let older_cursor = tail.older_cursor.clone().expect("older cursor");
    let mut view = TranscriptViewStateV1::open("view-1".to_string(), tail).expect("view");

    view.apply_patch(patch(
        1_000,
        "cursor-1000",
        vec![tool_block(2, TranscriptBlockStatusV1::Completed)],
    ))
    .expect("completed tool patch");
    assert_eq!(view.visible_blocks().len(), 1);
    assert_eq!(
        view.pending_override("tool:call-1")
            .expect("hidden override")
            .block_revision,
        "2"
    );

    let older = index
        .page_at(200, Some(older_cursor.as_str()), one_block)
        .expect("older page");
    assert_eq!(older.blocks[0].block_revision, "1");
    view.apply_page(older).expect("late older page");

    let visible = view.visible_blocks();
    assert_eq!(visible.len(), 2);
    assert_eq!(visible[0].block_id, "tool:call-1");
    assert_eq!(visible[0].block_revision, "2");
    assert!(matches!(
        visible[0].body,
        TranscriptBlockBodyV1::Tool {
            status: TranscriptBlockStatusV1::Completed,
            ..
        }
    ));
    assert_eq!(visible[1].block_id, "assistant:200");
    assert_eq!(view.applied_cursor("run-1"), Some("cursor-1000"));
}

#[test]
fn releasing_loaded_history_keeps_tail_committed_additions_and_old_block_overrides() {
    let mut index =
        TranscriptBlockIndexV1::new("session-1".to_string(), "generation-1".to_string())
            .expect("index");
    index
        .apply_committed(50, assistant_block(50))
        .expect("old assistant");
    index
        .apply_committed(100, tool_block(1, TranscriptBlockStatusV1::Running))
        .expect("tool call");
    index
        .apply_committed(200, assistant_block(200))
        .expect("tail assistant");
    let tail_policy = TranscriptPagePolicyV1 {
        max_blocks: 1,
        ..TranscriptPagePolicyV1::default()
    };
    let older_policy = TranscriptPagePolicyV1 {
        max_blocks: 2,
        ..TranscriptPagePolicyV1::default()
    };
    let tail = index.page_at(200, None, tail_policy).expect("tail page");
    let older_cursor = tail.older_cursor.clone().expect("older cursor");
    let older = index
        .page_at(200, Some(older_cursor.as_str()), older_policy)
        .expect("older page");
    let mut view = TranscriptViewStateV1::open("view-1".to_string(), tail).expect("view");
    view.apply_page(older.clone()).expect("load older page");
    view.apply_patch(patch(
        300,
        "cursor-300",
        vec![
            tool_block(2, TranscriptBlockStatusV1::Completed),
            assistant_block(300),
        ],
    ))
    .expect("post-base patch");

    let retained = HashSet::from(["assistant:200".to_string()]);
    assert_eq!(view.release_loaded_history(&retained), 1);
    assert_eq!(
        view.visible_blocks()
            .iter()
            .map(|block| block.block_id.as_str())
            .collect::<Vec<_>>(),
        vec!["tool:call-1", "assistant:200", "assistant:300"]
    );

    view.apply_page(older).expect("reload older page");
    assert_eq!(
        view.visible_blocks()
            .iter()
            .find(|block| block.block_id == "tool:call-1")
            .expect("tool override")
            .block_revision,
        "2"
    );
}

#[test]
fn patch_is_atomic_and_does_not_advance_cursor_after_a_merge_error() {
    let mut index =
        TranscriptBlockIndexV1::new("session-1".to_string(), "generation-1".to_string())
            .expect("index");
    index
        .apply_committed(200, assistant_block(200))
        .expect("tail block");
    let tail = index
        .page_at(200, None, TranscriptPagePolicyV1::default())
        .expect("tail page");
    let mut view = TranscriptViewStateV1::open("view-1".to_string(), tail).expect("view");
    view.apply_patch(patch(201, "cursor-201", vec![assistant_block(201)]))
        .expect("first patch");

    let mut conflicting = assistant_block(300);
    conflicting.block_id = "assistant:other".to_string();
    let failed = view.apply_patch(patch(
        300,
        "cursor-300",
        vec![assistant_block(300), conflicting],
    ));

    assert!(failed.is_err());
    assert_eq!(view.applied_cursor("run-1"), Some("cursor-201"));
    assert_eq!(
        view.visible_blocks()
            .iter()
            .map(|block| block.block_id.as_str())
            .collect::<Vec<_>>(),
        vec!["assistant:200", "assistant:201"]
    );
}
