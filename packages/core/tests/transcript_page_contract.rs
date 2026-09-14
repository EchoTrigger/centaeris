use centaeris_core::session::transcript::{
    TranscriptBlockBodyV1, TranscriptBlockIndexV1, TranscriptBlockStatusV1, TranscriptBlockV1,
    TranscriptCheckpointDecisionV1, TranscriptCheckpointWorkV1, TranscriptContentRefV1,
    TranscriptOrderKeyV1, TranscriptPagePolicyV1, TranscriptPageV1, TranscriptPatchV1,
    TranscriptTextContentV1, TRANSCRIPT_CHECKPOINT_REQUIRED_EVENTS,
    TRANSCRIPT_CHECKPOINT_REQUIRED_EVENT_BYTES, TRANSCRIPT_PAGE_SCHEMA_V1,
    TRANSCRIPT_PATCH_SCHEMA_V1, TRANSCRIPT_PROJECTION_VERSION_V1,
};

fn tool_block(revision: u64, status: TranscriptBlockStatusV1) -> TranscriptBlockV1 {
    TranscriptBlockV1 {
        block_id: "tool:call-a".to_string(),
        block_revision: revision.to_string(),
        order_key: TranscriptOrderKeyV1 {
            source_sequence: "100".to_string(),
            ordinal: 0,
        },
        body: TranscriptBlockBodyV1::Tool {
            call_id: "call-a".to_string(),
            tool_name: "read".to_string(),
            status,
            summary: Some("Read the source".to_string()),
            summary_ref: None,
            output_ref: None,
        },
    }
}

fn assistant_block(sequence: u64, text: &str) -> TranscriptBlockV1 {
    TranscriptBlockV1 {
        block_id: format!("assistant:{sequence}"),
        block_revision: "1".to_string(),
        order_key: TranscriptOrderKeyV1 {
            source_sequence: sequence.to_string(),
            ordinal: 0,
        },
        body: TranscriptBlockBodyV1::AssistantText {
            content: TranscriptTextContentV1::inline(text.to_string()),
            status: TranscriptBlockStatusV1::Completed,
        },
    }
}

#[test]
fn page_at_high_water_uses_the_latest_block_revision_without_moving_it() {
    let mut index =
        TranscriptBlockIndexV1::new("session-1".to_string(), "generation-1".to_string())
            .expect("index");
    index
        .apply_committed(100, tool_block(1, TranscriptBlockStatusV1::Running))
        .expect("tool call");
    index
        .apply_committed(1_000, tool_block(2, TranscriptBlockStatusV1::Completed))
        .expect("tool result");
    index
        .advance_source_high_water(1_200)
        .expect("project unrelated facts through the requested waterline");

    let early = index
        .page_at(150, None, TranscriptPagePolicyV1::default())
        .expect("early page");
    assert_eq!(
        early.blocks,
        vec![tool_block(1, TranscriptBlockStatusV1::Running)]
    );

    let current = index
        .page_at(1_200, None, TranscriptPagePolicyV1::default())
        .expect("current page");
    assert_eq!(
        current.blocks,
        vec![tool_block(2, TranscriptBlockStatusV1::Completed)]
    );
    assert_eq!(current.blocks[0].order_key().source_sequence, "100");
}

#[test]
fn page_contract_is_exact_and_binds_its_projection_identity() {
    let mut index =
        TranscriptBlockIndexV1::new("session-1".to_string(), "generation-1".to_string())
            .expect("index");
    index
        .apply_committed(100, tool_block(1, TranscriptBlockStatusV1::Running))
        .expect("tool call");
    let page = index
        .page_at(100, None, TranscriptPagePolicyV1::default())
        .expect("page");
    let mut value = serde_json::to_value(&page).expect("serialize page");

    assert_eq!(value["schema"], TRANSCRIPT_PAGE_SCHEMA_V1);
    assert_eq!(value["sessionId"], "session-1");
    assert_eq!(value["projectionGeneration"], "generation-1");
    assert_eq!(value["sourceHighWater"], "100");
    assert!(value.get("events").is_none());

    value
        .as_object_mut()
        .expect("page object")
        .insert("unknown".to_string(), serde_json::json!(true));
    assert!(serde_json::from_value::<TranscriptPageV1>(value).is_err());
}

#[test]
fn page_query_is_bounded_by_inline_bytes_and_always_advances() {
    let mut index =
        TranscriptBlockIndexV1::new("session-1".to_string(), "generation-1".to_string())
            .expect("index");
    for sequence in 1..=3_u64 {
        index
            .apply_committed(sequence, assistant_block(sequence, "12345678"))
            .expect("assistant block");
    }
    let policy = TranscriptPagePolicyV1 {
        max_blocks: 128,
        max_inline_content_bytes: 8,
        max_serialized_bytes: 256 * 1024,
    };

    let newest = index.page_at(3, None, policy).expect("newest page");
    assert_eq!(newest.blocks.len(), 1);
    assert!(newest.has_older);
    let cursor = newest.older_cursor.as_deref().expect("older cursor");
    let older = index.page_at(3, Some(cursor), policy).expect("older page");
    assert_eq!(older.blocks.len(), 1);
    assert_ne!(older.blocks[0].block_id, newest.blocks[0].block_id);
}

#[test]
fn all_pages_prepend_to_the_same_order_as_a_full_projection() {
    let mut index =
        TranscriptBlockIndexV1::new("session-1".to_string(), "generation-1".to_string())
            .expect("index");
    for sequence in 1..=5_u64 {
        index
            .apply_committed(sequence, assistant_block(sequence, "x"))
            .expect("assistant block");
    }
    let policy = TranscriptPagePolicyV1 {
        max_blocks: 2,
        ..TranscriptPagePolicyV1::default()
    };

    let mut cursor = None;
    let mut blocks = Vec::new();
    loop {
        let page = index
            .page_at(5, cursor.as_deref(), policy)
            .expect("history page");
        let mut combined = page.blocks;
        combined.extend(blocks);
        blocks = combined;
        if !page.has_older {
            break;
        }
        cursor = page.older_cursor;
    }

    assert_eq!(
        blocks
            .iter()
            .map(|block| block.block_id.as_str())
            .collect::<Vec<_>>(),
        vec![
            "assistant:1",
            "assistant:2",
            "assistant:3",
            "assistant:4",
            "assistant:5"
        ]
    );
}

#[test]
fn older_cursor_is_bound_to_waterline_and_projection_generation() {
    let mut index =
        TranscriptBlockIndexV1::new("session-1".to_string(), "generation-1".to_string())
            .expect("index");
    for sequence in 1..=2_u64 {
        index
            .apply_committed(sequence, assistant_block(sequence, "x"))
            .expect("assistant block");
    }
    let policy = TranscriptPagePolicyV1 {
        max_blocks: 1,
        ..TranscriptPagePolicyV1::default()
    };
    let page = index.page_at(2, None, policy).expect("newest page");
    let cursor = page.older_cursor.as_deref().expect("older cursor");

    assert!(index.page_at(1, Some(cursor), policy).is_err());
    let other_generation =
        TranscriptBlockIndexV1::new("session-1".to_string(), "generation-2".to_string())
            .expect("other generation");
    assert!(other_generation.page_at(2, Some(cursor), policy).is_err());
}

#[test]
fn page_requires_a_fully_projected_waterline() {
    let index = TranscriptBlockIndexV1::new("session-1".to_string(), "generation-1".to_string())
        .expect("index");

    assert!(index
        .page_at(1, None, TranscriptPagePolicyV1::default())
        .is_err());
}

#[test]
fn replaying_an_already_projected_block_revision_is_idempotent() {
    let mut index =
        TranscriptBlockIndexV1::new("session-1".to_string(), "generation-1".to_string())
            .expect("index");
    let block = assistant_block(1, "once");
    index
        .apply_committed(1, block.clone())
        .expect("initial projection");
    index
        .advance_source_high_water(10)
        .expect("project later facts");

    index
        .apply_committed(1, block)
        .expect("idempotent replay after later facts");
    assert_eq!(
        index
            .page_at(10, None, TranscriptPagePolicyV1::default())
            .expect("page")
            .blocks
            .len(),
        1
    );
}

#[test]
fn page_validation_enforces_cursor_shape_and_all_response_budgets() {
    let mut index =
        TranscriptBlockIndexV1::new("session-1".to_string(), "generation-1".to_string())
            .expect("index");
    index
        .apply_committed(1, assistant_block(1, "bounded"))
        .expect("assistant block");
    let mut page = index
        .page_at(1, None, TranscriptPagePolicyV1::default())
        .expect("page");
    page.has_older = true;

    assert!(page.validate(TranscriptPagePolicyV1::default()).is_err());
}

#[test]
fn patch_validation_rejects_duplicate_block_changes() {
    let block = assistant_block(1, "bounded");
    let patch = TranscriptPatchV1 {
        schema: TRANSCRIPT_PATCH_SCHEMA_V1.to_string(),
        session_id: "session-1".to_string(),
        projection_version: TRANSCRIPT_PROJECTION_VERSION_V1.to_string(),
        projection_generation: "generation-1".to_string(),
        source_high_water: "1".to_string(),
        stream_id: "run-1".to_string(),
        applied_cursor: "cursor-1".to_string(),
        upserts: vec![block.clone(), block],
        removals: Vec::new(),
    };

    assert!(patch.validate().is_err());
}

#[test]
fn referenced_oversized_text_does_not_stall_pagination() {
    let mut index =
        TranscriptBlockIndexV1::new("session-1".to_string(), "generation-1".to_string())
            .expect("index");
    let mut block = assistant_block(1, "placeholder");
    block.body = TranscriptBlockBodyV1::AssistantText {
        content: TranscriptTextContentV1::referenced(TranscriptContentRefV1 {
            ref_id: "session-event:event-1:modelMarkdown".to_string(),
            revision: "1".to_string(),
            byte_length: (1024_u64 * 1024).to_string(),
        }),
        status: TranscriptBlockStatusV1::Completed,
    };
    index.apply_committed(1, block).expect("referenced block");

    let page = index
        .page_at(1, None, TranscriptPagePolicyV1::default())
        .expect("referenced page");
    assert_eq!(page.blocks.len(), 1);
    assert!(!page.has_older);
}

#[test]
fn page_resume_cursor_is_selected_at_the_same_waterline() {
    let mut index =
        TranscriptBlockIndexV1::new("session-1".to_string(), "generation-1".to_string())
            .expect("index");
    index
        .apply_committed(100, assistant_block(100, "first"))
        .expect("first block");
    index
        .record_resume_cursor(100, "run-1".to_string(), "cursor-100".to_string())
        .expect("first cursor");
    index
        .advance_source_high_water(1_000)
        .expect("advance facts");
    index
        .record_resume_cursor(1_000, "run-1".to_string(), "cursor-1000".to_string())
        .expect("second cursor");

    assert_eq!(
        index
            .page_at(150, None, TranscriptPagePolicyV1::default())
            .expect("early page")
            .resume_cursors[0]
            .cursor,
        "cursor-100"
    );
    assert_eq!(
        index
            .page_at(1_000, None, TranscriptPagePolicyV1::default())
            .expect("current page")
            .resume_cursors[0]
            .cursor,
        "cursor-1000"
    );
}

#[test]
fn tail_page_query_work_is_bounded_by_page_size_not_history_size() {
    let mut index =
        TranscriptBlockIndexV1::new("session-1".to_string(), "generation-1".to_string())
            .expect("index");
    for sequence in 1..=10_000_u64 {
        index
            .apply_committed(sequence, assistant_block(sequence, "x"))
            .expect("assistant block");
    }
    let policy = TranscriptPagePolicyV1 {
        max_blocks: 2,
        ..TranscriptPagePolicyV1::default()
    };

    let (page, work) = index
        .page_at_with_work(10_000, None, policy)
        .expect("measured tail page");
    assert_eq!(page.blocks.len(), 2);
    assert_eq!(work.returned_blocks, 2);
    assert!(work.order_keys_visited <= 3, "work={work:?}");
    assert_eq!(work.raw_event_visits, 0);
}

#[test]
fn rejected_first_revision_does_not_reserve_its_order_key() {
    let mut index =
        TranscriptBlockIndexV1::new("session-1".to_string(), "generation-1".to_string())
            .expect("index");
    let mut rejected = assistant_block(1, "bad arrival");
    rejected.block_id = "assistant:rejected".to_string();
    assert!(index.apply_committed(2, rejected).is_err());

    index
        .apply_committed(1, assistant_block(1, "valid arrival"))
        .expect("rejected write must not mutate the index");
}

#[test]
fn checkpoint_work_waits_for_a_safe_point_and_resets_only_after_commit() {
    let mut work = TranscriptCheckpointWorkV1::default();
    for _ in 0..TRANSCRIPT_CHECKPOINT_REQUIRED_EVENTS - 1 {
        work.observe_source_event(1).expect("bounded event");
    }
    assert_eq!(work.decision(true), TranscriptCheckpointDecisionV1::NotDue);
    work.observe_source_event(1).expect("threshold event");
    assert_eq!(
        work.decision(false),
        TranscriptCheckpointDecisionV1::AwaitingSafePoint
    );
    assert_eq!(
        work.decision(true),
        TranscriptCheckpointDecisionV1::WriteCheckpoint
    );
    assert_eq!(
        work.events_since_checkpoint(),
        TRANSCRIPT_CHECKPOINT_REQUIRED_EVENTS
    );
    work.checkpoint_committed();
    assert_eq!(work.decision(true), TranscriptCheckpointDecisionV1::NotDue);

    work.observe_source_event(TRANSCRIPT_CHECKPOINT_REQUIRED_EVENT_BYTES)
        .expect("byte threshold");
    assert_eq!(
        work.decision(true),
        TranscriptCheckpointDecisionV1::WriteCheckpoint
    );
    work.checkpoint_committed();
    work.observe_page_sealed();
    assert_eq!(
        work.decision(true),
        TranscriptCheckpointDecisionV1::WriteCheckpoint
    );
}
