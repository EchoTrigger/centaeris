use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use centaeris_core::session::store::SessionDataStorePort;
use centaeris_core::session::transcript::{
    transcript_patch_read_result_serialized_bytes, TranscriptBlockBodyV1, TranscriptBlockStatusV1,
    TranscriptBlockV1, TranscriptCheckpointRefsV1, TranscriptCommittedBlockV1,
    TranscriptOrderKeyV1, TranscriptPagePolicyV1, TranscriptPageReadRequestV1,
    TranscriptPatchReadRequestV1, TranscriptProjectionCheckpointV1,
    TranscriptProjectionCommitDispositionV1, TranscriptProjectionCommitV1,
    TranscriptProjectionFrontierV1, TranscriptProjectionGenerationRotationDispositionV1,
    TranscriptProjectionGenerationRotationV1, TranscriptProjectionGenerationStorePortV1,
    TranscriptProjectionOpenToolV1, TranscriptProjectionRecoveryV1, TranscriptProjectionStorePort,
    TranscriptResumeCursorV1, TranscriptTextContentV1, TRANSCRIPT_CHECKPOINT_SCHEMA_V1,
    TRANSCRIPT_FRONTIER_SCHEMA_V1, TRANSCRIPT_PATCH_READ_SERIALIZED_MAX_BYTES,
    TRANSCRIPT_PROJECTION_VERSION_V1,
};
use centaeris_core::session::{parse_event, SequencedSessionRecord};
use centaeris_runtime_sqlite::SqliteRuntimeStore;
use rusqlite::Connection;
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
        presentation: None,
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
        presentation: None,
        order_key: TranscriptOrderKeyV1 {
            source_sequence: "2".to_string(),
            ordinal: 0,
        },
        body: TranscriptBlockBodyV1::Tool {
            call_id: "call-a".to_string(),
            tool_name: "read".to_string(),
            status,
            summary: Some(format!("tool revision {revision}")),
            summary_ref: None,
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

fn checkpoint_with_open_tool(
    source_high_water: u64,
    summary: String,
) -> TranscriptProjectionRecoveryV1 {
    let mut recovery = checkpoint(source_high_water);
    let mut block = tool_block(1, TranscriptBlockStatusV1::Running);
    let TranscriptBlockBodyV1::Tool {
        summary: block_summary,
        ..
    } = &mut block.body
    else {
        unreachable!("tool_block returns a tool body")
    };
    *block_summary = Some(summary);
    block.order_key.source_sequence = "1".to_string();
    recovery.frontier.open_tools = vec![TranscriptProjectionOpenToolV1 {
        call_id: "call-a".to_string(),
        block,
    }];
    recovery
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

#[test]
fn committed_patches_resume_exclusively_without_visiting_raw_events() {
    let db_path = temp_db_path();
    let store = SqliteRuntimeStore::new(&db_path).expect("create store");
    for (commit_id, expected, source, block) in [
        ("patch-1", 0, 1, Some(text_block("message-1", 1, "one"))),
        ("patch-2", 1, 2, None),
        ("patch-3", 2, 3, Some(text_block("message-3", 3, "three"))),
    ] {
        store
            .commit_transcript_projection(commit(
                commit_id,
                expected,
                source,
                block
                    .map(|block| TranscriptCommittedBlockV1 {
                        applied_source_sequence: source.to_string(),
                        block,
                    })
                    .into_iter()
                    .collect(),
                None,
            ))
            .expect("commit projection event");
    }

    let request = TranscriptPatchReadRequestV1 {
        session_id: "session-a".to_string(),
        projection_version: TRANSCRIPT_PROJECTION_VERSION_V1.to_string(),
        projection_generation: "generation-a".to_string(),
        after_source_high_water: "0".to_string(),
        through_source_high_water: "3".to_string(),
    };
    let first = store
        .load_transcript_patches(request.clone())
        .expect("read committed patches");
    assert_eq!(
        first
            .patches
            .iter()
            .map(|patch| patch.source_high_water.as_str())
            .collect::<Vec<_>>(),
        vec!["1", "2", "3"]
    );
    assert!(first.patches[1].upserts.is_empty());
    assert_eq!(first.next_source_high_water, "3");
    assert!(!first.has_more);
    assert_eq!(first.work.commit_rows_read, 3);
    assert_eq!(first.work.raw_event_visits, 0);
    assert_eq!(
        store
            .load_transcript_patches(request)
            .expect("safe retry returns the same patch sequence"),
        first
    );

    let caught_up = store
        .load_transcript_patches(TranscriptPatchReadRequestV1 {
            session_id: "session-a".to_string(),
            projection_version: TRANSCRIPT_PROJECTION_VERSION_V1.to_string(),
            projection_generation: "generation-a".to_string(),
            after_source_high_water: "3".to_string(),
            through_source_high_water: "3".to_string(),
        })
        .expect("already caught up");
    assert!(caught_up.patches.is_empty());
    assert_eq!(caught_up.next_source_high_water, "3");
    assert!(!caught_up.has_more);
    drop(store);
    let _ = std::fs::remove_file(db_path);
}

#[test]
fn current_recovery_is_single_slot_and_is_not_embedded_in_commit_history() {
    let db_path = temp_db_path();
    let store = SqliteRuntimeStore::new(&db_path).expect("create store");
    let large_open_tool_summary = "open".repeat(8 * 1024);
    for sequence in 1..=64_u64 {
        store
            .commit_transcript_projection(commit(
                format!("recovery-slot-{sequence}").as_str(),
                sequence - 1,
                sequence,
                vec![TranscriptCommittedBlockV1 {
                    applied_source_sequence: sequence.to_string(),
                    block: text_block(format!("settled-{sequence}").as_str(), sequence, "settled"),
                }],
                Some(checkpoint_with_open_tool(
                    sequence,
                    large_open_tool_summary.clone(),
                )),
            ))
            .expect("advance replaceable recovery slot");
    }

    let conn = Connection::open(&db_path).expect("inspect store");
    let current_recoveries: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM transcript_projection_current_recoveries",
            [],
            |row| row.get(0),
        )
        .expect("count current recoveries");
    let historical_checkpoints: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM transcript_projection_checkpoints",
            [],
            |row| row.get(0),
        )
        .expect("count historical checkpoints");
    let historical_frontiers: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM transcript_projection_frontiers",
            [],
            |row| row.get(0),
        )
        .expect("count historical frontiers");
    let commit_history_bytes: i64 = conn
        .query_row(
            "SELECT COALESCE(SUM(LENGTH(commit_json)), 0) FROM transcript_projection_commits",
            [],
            |row| row.get(0),
        )
        .expect("measure commit history");
    assert_eq!(current_recoveries, 1);
    assert_eq!(historical_checkpoints, 0);
    assert_eq!(historical_frontiers, 0);
    assert!(
        commit_history_bytes < 256 * 1024,
        "commit history must not multiply the 32KiB open-tool frontier: {commit_history_bytes}"
    );
    drop(conn);
    drop(store);
    let _ = std::fs::remove_file(db_path);
}

#[test]
fn patch_query_truncates_on_the_complete_result_envelope_budget() {
    let db_path = temp_db_path();
    let store = SqliteRuntimeStore::new(&db_path).expect("create store");
    let large_text = "x".repeat(40 * 1024);
    for sequence in 1..=20_u64 {
        store
            .commit_transcript_projection(commit(
                format!("large-patch-{sequence}").as_str(),
                sequence - 1,
                sequence,
                vec![TranscriptCommittedBlockV1 {
                    applied_source_sequence: sequence.to_string(),
                    block: text_block(
                        format!("large-message-{sequence}").as_str(),
                        sequence,
                        large_text.as_str(),
                    ),
                }],
                None,
            ))
            .expect("commit large patch");
    }

    let mut after = 0_u64;
    let mut observed = Vec::new();
    let mut batches = 0;
    while after < 20 {
        let request = TranscriptPatchReadRequestV1 {
            session_id: "session-a".to_string(),
            projection_version: TRANSCRIPT_PROJECTION_VERSION_V1.to_string(),
            projection_generation: "generation-a".to_string(),
            after_source_high_water: after.to_string(),
            through_source_high_water: "20".to_string(),
        };
        let result = store
            .load_transcript_patches(request)
            .expect("bounded patch batch");
        assert!(
            transcript_patch_read_result_serialized_bytes(&result)
                .expect("serialized patch result")
                <= TRANSCRIPT_PATCH_READ_SERIALIZED_MAX_BYTES
        );
        assert!(!result.patches.is_empty());
        if result.has_more {
            assert_eq!(result.work.commit_rows_read, result.patches.len() + 1);
        }
        observed.extend(
            result
                .patches
                .iter()
                .map(|patch| patch.source_high_water.parse::<u64>().expect("waterline")),
        );
        after = result
            .next_source_high_water
            .parse::<u64>()
            .expect("next waterline");
        batches += 1;
    }
    assert!(batches >= 2);
    assert_eq!(observed, (1..=20_u64).collect::<Vec<_>>());
    drop(store);
    let _ = std::fs::remove_file(db_path);
}

#[test]
fn current_generation_rotation_is_atomic_and_rejects_old_page_cursors() {
    let db_path = temp_db_path();
    let store = SqliteRuntimeStore::new(&db_path).expect("create store");
    store
        .commit_transcript_projection(commit("generation-a-commit", 0, 1, Vec::new(), None))
        .expect("commit initial generation");

    let initial_rotation = TranscriptProjectionGenerationRotationV1 {
        session_id: "session-a".to_string(),
        projection_version: TRANSCRIPT_PROJECTION_VERSION_V1.to_string(),
        expected_current_generation: None,
        next_generation: "generation-a".to_string(),
        target_source_high_water: "1".to_string(),
    };
    assert_eq!(
        store
            .rotate_current_transcript_projection_generation(initial_rotation.clone())
            .expect("publish initial generation"),
        TranscriptProjectionGenerationRotationDispositionV1::Applied
    );
    assert_eq!(
        store
            .rotate_current_transcript_projection_generation(initial_rotation)
            .expect("idempotent initial publication"),
        TranscriptProjectionGenerationRotationDispositionV1::AlreadyApplied
    );

    let mut next_commit = commit("generation-b-commit", 0, 1, Vec::new(), None);
    next_commit.projection_generation = "generation-b".to_string();
    store
        .commit_transcript_projection(next_commit)
        .expect("commit replacement generation");

    let stale_rotation = TranscriptProjectionGenerationRotationV1 {
        session_id: "session-a".to_string(),
        projection_version: TRANSCRIPT_PROJECTION_VERSION_V1.to_string(),
        expected_current_generation: Some("not-current".to_string()),
        next_generation: "generation-b".to_string(),
        target_source_high_water: "1".to_string(),
    };
    assert!(store
        .rotate_current_transcript_projection_generation(stale_rotation)
        .expect_err("stale compare-and-swap must fail")
        .contains("rotation conflict"));

    assert_eq!(
        store
            .rotate_current_transcript_projection_generation(
                TranscriptProjectionGenerationRotationV1 {
                    session_id: "session-a".to_string(),
                    projection_version: TRANSCRIPT_PROJECTION_VERSION_V1.to_string(),
                    expected_current_generation: Some("generation-a".to_string()),
                    next_generation: "generation-b".to_string(),
                    target_source_high_water: "1".to_string(),
                },
            )
            .expect("rotate current generation"),
        TranscriptProjectionGenerationRotationDispositionV1::Applied
    );
    let current = store
        .load_current_transcript_projection_generation("session-a")
        .expect("load current generation")
        .expect("published generation");
    assert_eq!(current.projection_generation, "generation-b");

    let error = store
        .load_current_transcript_page(TranscriptPageReadRequestV1 {
            session_id: "session-a".to_string(),
            projection_version: TRANSCRIPT_PROJECTION_VERSION_V1.to_string(),
            projection_generation: "generation-a".to_string(),
            source_high_water: "1".to_string(),
            older_cursor: None,
            policy: TranscriptPagePolicyV1::default(),
        })
        .expect_err("old generation must not remain readable through current-only API");
    assert_eq!(
        error,
        centaeris_core::session::transcript::TRANSCRIPT_PROJECTION_GENERATION_INVALIDATED
    );
    drop(store);
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
