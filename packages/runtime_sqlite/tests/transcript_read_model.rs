use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use centaeris_core::session::store::SessionDataStorePort;
use centaeris_core::session::transcript::{
    TranscriptBlockBodyV1, TranscriptBlockStatusV1, TranscriptBlockV1, TranscriptCheckpointRefsV1,
    TranscriptCommittedBlockV1, TranscriptOrderKeyV1, TranscriptPagePolicyV1,
    TranscriptPageReadRequestV1, TranscriptProjectionCheckpointV1,
    TranscriptProjectionCommitDispositionV1, TranscriptProjectionCommitV1,
    TranscriptProjectionFrontierV1, TranscriptProjectionOpenToolV1, TranscriptProjectionRecoveryV1,
    TranscriptProjectionStorePort, TranscriptResumeCursorV1, TranscriptTextContentV1,
    TRANSCRIPT_CHECKPOINT_SCHEMA_V1, TRANSCRIPT_FRONTIER_SCHEMA_V1,
    TRANSCRIPT_PROJECTION_VERSION_V1,
};
use centaeris_core::session::{parse_event, SequencedSessionRecord};
use centaeris_runtime_sqlite::SqliteRuntimeStore;
use serde_json::{json, Value};

fn temp_db_path() -> PathBuf {
    static NEXT_DATABASE: AtomicU64 = AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "centaeris-transcript-read-model-{}-{}-{}.db",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos(),
        NEXT_DATABASE.fetch_add(1, Ordering::Relaxed)
    ))
}

#[test]
fn persistent_tail_query_decodes_only_the_page_and_one_lookahead_row() {
    let db_path = temp_db_path();
    let store = SqliteRuntimeStore::new(&db_path).expect("create store");
    let mut expected_source_high_water = 0;
    for batch_start in (1..=1_024_u64).step_by(128) {
        let batch_end = batch_start + 127;
        let upserts = (batch_start..=batch_end)
            .map(|sequence| TranscriptCommittedBlockV1 {
                applied_source_sequence: sequence.to_string(),
                block: text_block(format!("message-{sequence}").as_str(), sequence, "bounded"),
            })
            .collect();
        store
            .commit_transcript_projection(commit(
                format!("scale-commit-{batch_end}").as_str(),
                expected_source_high_water,
                batch_end,
                upserts,
                None,
            ))
            .expect("projection batch");
        expected_source_high_water = batch_end;
    }

    let tail = store
        .load_transcript_page(TranscriptPageReadRequestV1 {
            session_id: "session-a".to_string(),
            projection_version: TRANSCRIPT_PROJECTION_VERSION_V1.to_string(),
            projection_generation: "generation-a".to_string(),
            source_high_water: "1024".to_string(),
            older_cursor: None,
            policy: TranscriptPagePolicyV1 {
                max_blocks: 2,
                ..TranscriptPagePolicyV1::default()
            },
        })
        .expect("tail page");
    assert_eq!(
        tail.page
            .blocks
            .iter()
            .map(|block| block.block_id.as_str())
            .collect::<Vec<_>>(),
        vec!["message-1023", "message-1024"]
    );
    assert_eq!(tail.work.candidate_rows_read, 3);
    assert_eq!(tail.work.raw_event_visits, 0);
    drop(store);
    let _ = std::fs::remove_file(db_path);
}

fn text_block(block_id: &str, source_sequence: u64, text: &str) -> TranscriptBlockV1 {
    TranscriptBlockV1 {
        block_id: block_id.to_string(),
        block_revision: "1".to_string(),
        order_key: TranscriptOrderKeyV1 {
            source_sequence: source_sequence.to_string(),
            ordinal: 0,
        },
        body: TranscriptBlockBodyV1::UserText {
            content: TranscriptTextContentV1::inline(text.to_string()),
        },
    }
}

fn tool_block(revision: u64, status: TranscriptBlockStatusV1) -> TranscriptBlockV1 {
    TranscriptBlockV1 {
        block_id: "tool:call-a".to_string(),
        block_revision: revision.to_string(),
        order_key: TranscriptOrderKeyV1 {
            source_sequence: "2".to_string(),
            ordinal: 0,
        },
        body: TranscriptBlockBodyV1::Tool {
            call_id: "call-a".to_string(),
            tool_name: "read".to_string(),
            status,
            summary: Some(format!("tool revision {revision}")),
            output_ref: None,
        },
    }
}

fn checkpoint(source_high_water: u64) -> TranscriptProjectionRecoveryV1 {
    let frontier_ref = format!("frontier-{source_high_water}");
    TranscriptProjectionRecoveryV1 {
        checkpoint: TranscriptProjectionCheckpointV1 {
            schema: TRANSCRIPT_CHECKPOINT_SCHEMA_V1.to_string(),
            session_id: "session-a".to_string(),
            projection_version: TRANSCRIPT_PROJECTION_VERSION_V1.to_string(),
            projection_generation: "generation-a".to_string(),
            source_high_water: source_high_water.to_string(),
            frontier_ref: frontier_ref.clone(),
            block_index_ref: "block-index-a".to_string(),
        },
        frontier: TranscriptProjectionFrontierV1 {
            schema: TRANSCRIPT_FRONTIER_SCHEMA_V1.to_string(),
            frontier_ref,
            session_id: "session-a".to_string(),
            projection_version: TRANSCRIPT_PROJECTION_VERSION_V1.to_string(),
            projection_generation: "generation-a".to_string(),
            source_high_water: source_high_water.to_string(),
            open_tools: (source_high_water == 2)
                .then(|| TranscriptProjectionOpenToolV1 {
                    call_id: "call-a".to_string(),
                    block: tool_block(1, TranscriptBlockStatusV1::Running),
                })
                .into_iter()
                .collect(),
        },
    }
}

fn commit(
    commit_id: &str,
    expected_source_high_water: u64,
    source_high_water: u64,
    upserts: Vec<TranscriptCommittedBlockV1>,
    recovery: Option<TranscriptProjectionRecoveryV1>,
) -> TranscriptProjectionCommitV1 {
    TranscriptProjectionCommitV1 {
        commit_id: commit_id.to_string(),
        session_id: "session-a".to_string(),
        projection_version: TRANSCRIPT_PROJECTION_VERSION_V1.to_string(),
        projection_generation: "generation-a".to_string(),
        expected_source_high_water: expected_source_high_water.to_string(),
        source_high_water: source_high_water.to_string(),
        upserts,
        resume_cursors: vec![TranscriptResumeCursorV1 {
            stream_id: "run-a".to_string(),
            cursor: format!("{source_high_water}-0"),
        }],
        checkpoint: recovery.as_ref().map(|value| value.checkpoint.clone()),
        frontier: recovery.map(|value| value.frontier),
        invalidation_reason: None,
    }
}

#[test]
fn sqlite_transcript_pages_survive_restart_and_preserve_waterline_versions() {
    let db_path = temp_db_path();
    let store = SqliteRuntimeStore::new(&db_path).expect("create store");

    let first = commit(
        "commit-1",
        0,
        2,
        vec![
            TranscriptCommittedBlockV1 {
                applied_source_sequence: "1".to_string(),
                block: text_block("message-user", 1, "hello"),
            },
            TranscriptCommittedBlockV1 {
                applied_source_sequence: "2".to_string(),
                block: tool_block(1, TranscriptBlockStatusV1::Running),
            },
        ],
        Some(checkpoint(2)),
    );
    assert_eq!(
        store
            .commit_transcript_projection(first)
            .expect("first commit"),
        TranscriptProjectionCommitDispositionV1::Applied
    );
    store
        .commit_transcript_projection(commit(
            "commit-2",
            2,
            5,
            vec![TranscriptCommittedBlockV1 {
                applied_source_sequence: "5".to_string(),
                block: tool_block(2, TranscriptBlockStatusV1::Completed),
            }],
            Some(checkpoint(5)),
        ))
        .expect("tool result commit");
    let final_commit = commit(
        "commit-3",
        5,
        6,
        vec![TranscriptCommittedBlockV1 {
            applied_source_sequence: "6".to_string(),
            block: text_block("message-assistant", 6, "done"),
        }],
        None,
    );
    store
        .commit_transcript_projection(final_commit.clone())
        .expect("assistant commit");

    let at_two = store
        .load_transcript_page(TranscriptPageReadRequestV1 {
            session_id: "session-a".to_string(),
            projection_version: TRANSCRIPT_PROJECTION_VERSION_V1.to_string(),
            projection_generation: "generation-a".to_string(),
            source_high_water: "2".to_string(),
            older_cursor: None,
            policy: TranscriptPagePolicyV1::default(),
        })
        .expect("page at old waterline");
    assert_eq!(at_two.page.blocks.len(), 2);
    assert_eq!(at_two.page.blocks[1].block_revision, "1");
    assert!(matches!(
        at_two.page.blocks[1].body,
        TranscriptBlockBodyV1::Tool {
            status: TranscriptBlockStatusV1::Running,
            ..
        }
    ));

    drop(store);
    let reopened = SqliteRuntimeStore::new(&db_path).expect("reopen store");
    assert_eq!(
        reopened
            .commit_transcript_projection(final_commit)
            .expect("idempotent replay"),
        TranscriptProjectionCommitDispositionV1::AlreadyApplied
    );
    let tail = reopened
        .load_transcript_page(TranscriptPageReadRequestV1 {
            session_id: "session-a".to_string(),
            projection_version: TRANSCRIPT_PROJECTION_VERSION_V1.to_string(),
            projection_generation: "generation-a".to_string(),
            source_high_water: "6".to_string(),
            older_cursor: None,
            policy: TranscriptPagePolicyV1 {
                max_blocks: 2,
                ..TranscriptPagePolicyV1::default()
            },
        })
        .expect("tail page");
    assert_eq!(
        tail.page
            .blocks
            .iter()
            .map(|block| block.block_id.as_str())
            .collect::<Vec<_>>(),
        vec!["tool:call-a", "message-assistant"]
    );
    assert_eq!(tail.page.blocks[0].block_revision, "2");
    assert!(tail.page.has_older);
    assert!(tail.work.candidate_rows_read <= tail.page.blocks.len() + 1);
    let older = reopened
        .load_transcript_page(TranscriptPageReadRequestV1 {
            session_id: "session-a".to_string(),
            projection_version: TRANSCRIPT_PROJECTION_VERSION_V1.to_string(),
            projection_generation: "generation-a".to_string(),
            source_high_water: "6".to_string(),
            older_cursor: tail.page.older_cursor,
            policy: TranscriptPagePolicyV1 {
                max_blocks: 2,
                ..TranscriptPagePolicyV1::default()
            },
        })
        .expect("older page");
    assert_eq!(older.page.blocks[0].block_id, "message-user");
    assert!(!older.page.has_older);
    assert_eq!(
        reopened
            .load_latest_transcript_checkpoint("session-a", "generation-a", 6)
            .expect("checkpoint")
            .expect("stored checkpoint")
            .source_high_water,
        "5"
    );
    let recovery = reopened
        .load_latest_transcript_recovery("session-a", "generation-a", 6)
        .expect("load recovery")
        .expect("stored recovery");
    assert_eq!(recovery.checkpoint.source_high_water, "5");
    assert_eq!(recovery.frontier.source_high_water, "5");
    assert!(recovery.frontier.open_tools.is_empty());

    let conflicting = commit(
        "commit-conflicting-order",
        6,
        7,
        vec![
            TranscriptCommittedBlockV1 {
                applied_source_sequence: "7".to_string(),
                block: text_block("message-first-at-seven", 7, "first"),
            },
            TranscriptCommittedBlockV1 {
                applied_source_sequence: "7".to_string(),
                block: text_block("message-second-at-seven", 7, "second"),
            },
        ],
        None,
    );
    let error = reopened
        .commit_transcript_projection(conflicting)
        .expect_err("duplicate order key must fail atomically");
    assert!(error.contains("save transcript block identity failed"));
    assert_eq!(
        reopened
            .load_transcript_projection_head("session-a", "generation-a")
            .expect("head after rolled back commit")
            .expect("projection head")
            .source_high_water,
        "6"
    );
    reopened
        .commit_transcript_projection(commit(
            "commit-4",
            6,
            7,
            vec![TranscriptCommittedBlockV1 {
                applied_source_sequence: "7".to_string(),
                block: text_block("message-first-at-seven", 7, "first"),
            }],
            None,
        ))
        .expect("rolled back identity can be committed");

    let stale = commit("commit-stale", 6, 8, Vec::new(), None);
    let error = reopened
        .commit_transcript_projection(stale)
        .expect_err("stale writer must fail");
    assert!(error.contains("sourceHighWater conflict"));
    assert_eq!(
        reopened
            .load_transcript_projection_head("session-a", "generation-a")
            .expect("head")
            .expect("projection head")
            .source_high_water,
        "7"
    );

    let mut invalidation = commit("commit-invalidate", 7, 8, Vec::new(), None);
    invalidation.invalidation_reason = Some("tombstone".to_string());
    reopened
        .commit_transcript_projection(invalidation)
        .expect("invalidate projection generation");
    let error = reopened
        .load_transcript_page(TranscriptPageReadRequestV1 {
            session_id: "session-a".to_string(),
            projection_version: TRANSCRIPT_PROJECTION_VERSION_V1.to_string(),
            projection_generation: "generation-a".to_string(),
            source_high_water: "8".to_string(),
            older_cursor: None,
            policy: TranscriptPagePolicyV1::default(),
        })
        .expect_err("invalidated generation must reject reads");
    assert!(error.contains("projection is invalidated: tombstone"));

    reopened
        .delete_session_data("session-a")
        .expect("delete session");
    assert!(reopened
        .load_transcript_projection_head("session-a", "generation-a")
        .expect("deleted head")
        .is_none());
    drop(reopened);
    let _ = std::fs::remove_file(db_path);
}

fn source_record(
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
            "sessionId": "session-recovery",
            "turnId": "turn-1",
            "agentRunId": "run-1",
            "createdAtMs": sequence,
            "payload": payload,
        }))
        .expect("valid source event"),
    }
}

#[test]
fn restored_frontier_commits_a_late_tool_result_without_replaying_earlier_events() {
    let db_path = temp_db_path();
    let store = SqliteRuntimeStore::new(&db_path).expect("create store");
    let mut projector = centaeris_core::session::transcript::TranscriptProjectorV1::new(
        "session-recovery".to_string(),
        "generation-recovery".to_string(),
    )
    .expect("projector");
    let call = source_record(
        100,
        "tool_call",
        "event-call-recovery",
        json!({
            "callId": "call-recovery",
            "toolName": "read",
            "toolContractDigest": format!("sha256:{}", "a".repeat(64)),
            "providerId": "builtin",
            "normalizedInput": {"path": "README.md"},
            "displayTarget": "README.md"
        }),
    );
    projector
        .apply_and_commit(
            &call,
            "run-1",
            "cursor-100",
            "commit-recovery-100",
            Some(TranscriptCheckpointRefsV1 {
                frontier_ref: "frontier-recovery-100".to_string(),
                block_index_ref: "block-index-recovery".to_string(),
            }),
            &store,
        )
        .expect("commit checkpointed tool call");
    drop(store);

    let reopened = SqliteRuntimeStore::new(&db_path).expect("reopen store");
    let recovery = reopened
        .load_latest_transcript_recovery("session-recovery", "generation-recovery", 100)
        .expect("load recovery")
        .expect("stored recovery");
    let mut restored =
        centaeris_core::session::transcript::TranscriptProjectorV1::from_checkpoint_recovery(
            recovery,
        )
        .expect("restore projector");
    let result = source_record(
        1_000,
        "tool_result",
        "event-result-recovery",
        json!({
            "callId": "call-recovery",
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
    );
    restored
        .apply_and_commit(
            &result,
            "run-1",
            "cursor-1000",
            "commit-recovery-1000",
            None,
            &reopened,
        )
        .expect("commit late tool result");
    let page = reopened
        .load_transcript_page(TranscriptPageReadRequestV1 {
            session_id: "session-recovery".to_string(),
            projection_version: TRANSCRIPT_PROJECTION_VERSION_V1.to_string(),
            projection_generation: "generation-recovery".to_string(),
            source_high_water: "1000".to_string(),
            older_cursor: None,
            policy: TranscriptPagePolicyV1::default(),
        })
        .expect("load completed tool page");
    assert_eq!(page.work.raw_event_visits, 0);
    assert_eq!(page.page.blocks[0].order_key.source_sequence, "100");
    assert_eq!(page.page.blocks[0].block_revision, "2");
    drop(reopened);
    let _ = std::fs::remove_file(db_path);
}
