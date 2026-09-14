use super::*;
use centaeris_core::session::transcript::{
    assemble_transcript_page_from_newest, transcript_page_before_order,
    transcript_patch_read_result_serialized_bytes, validate_transcript_resume_cursor_read,
    TranscriptBlockV1, TranscriptPageReadRequestV1, TranscriptPageReadResultV1,
    TranscriptPatchReadRequestV1, TranscriptPatchReadResultV1, TranscriptPatchV1,
    TranscriptPersistentPageQueryWorkV1, TranscriptPersistentPatchQueryWorkV1,
    TranscriptProjectionCheckpointV1, TranscriptProjectionCommitDispositionV1,
    TranscriptProjectionCommitV1, TranscriptProjectionCurrentGenerationV1,
    TranscriptProjectionFrontierV1, TranscriptProjectionGenerationRotationDispositionV1,
    TranscriptProjectionGenerationRotationV1, TranscriptProjectionGenerationStorePortV1,
    TranscriptProjectionHeadV1, TranscriptProjectionRecoveryV1, TranscriptProjectionStorePort,
    TranscriptResumeCursorV1, TRANSCRIPT_PAGE_RESUME_CURSOR_MAX_COUNT,
    TRANSCRIPT_PATCH_READ_MAX_PATCHES, TRANSCRIPT_PATCH_READ_SERIALIZED_MAX_BYTES,
    TRANSCRIPT_PATCH_SCHEMA_V1, TRANSCRIPT_PROJECTION_VERSION_V1,
};
use rusqlite::{params, OptionalExtension, TransactionBehavior};

impl TranscriptProjectionStorePort for SqliteRuntimeStore {
    fn commit_transcript_projection(
        &self,
        commit: TranscriptProjectionCommitV1,
    ) -> Result<TranscriptProjectionCommitDispositionV1, String> {
        commit.validate()?;
        let expected_source_high_water = decimal_to_i64(
            commit.expected_source_high_water.as_str(),
            "expectedSourceHighWater",
        )?;
        let source_high_water =
            decimal_to_i64(commit.source_high_water.as_str(), "sourceHighWater")?;
        // Recovery is a replaceable control-state slot at the projection head, not part of
        // immutable patch history. Keeping it out of commit_json prevents repeated open-tool
        // frontiers from multiplying with the number of source events.
        let mut durable_commit = commit.clone();
        durable_commit.checkpoint = None;
        durable_commit.frontier = None;
        let commit_json = serde_json::to_string(&durable_commit)
            .map_err(|error| format!("serialize transcript projection commit failed: {error}"))?;
        self.with_conn(|conn| {
            let transaction = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|error| format!("begin transcript projection commit failed: {error}"))?;
            if let Some(existing_json) = transaction
                .query_row(
                    "SELECT commit_json FROM transcript_projection_commits WHERE commit_id=?1",
                    params![commit.commit_id.as_str()],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(|error| format!("load transcript projection commit failed: {error}"))?
            {
                let existing: TranscriptProjectionCommitV1 = serde_json::from_str(&existing_json)
                    .map_err(|error| {
                        format!("decode stored transcript projection commit failed: {error}")
                    })?;
                if existing == durable_commit {
                    return Ok(TranscriptProjectionCommitDispositionV1::AlreadyApplied);
                }
                return Err(format!(
                    "transcript projection commitId conflict: {}",
                    commit.commit_id
                ));
            }

            let current = transaction
                .query_row(
                    "SELECT source_high_water,invalidation_reason FROM transcript_projection_heads WHERE session_id=?1 AND projection_version=?2 AND projection_generation=?3",
                    params![
                        commit.session_id.as_str(),
                        commit.projection_version.as_str(),
                        commit.projection_generation.as_str()
                    ],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?)),
                )
                .optional()
                .map_err(|error| format!("load transcript projection head failed: {error}"))?;
            let current_source_high_water = match current {
                Some((_, Some(reason))) => {
                    return Err(format!("transcript projection is invalidated: {reason}"))
                }
                Some((value, None)) => value,
                None if expected_source_high_water == 0 => {
                    transaction
                        .execute(
                            "INSERT INTO transcript_projection_heads(session_id,projection_version,projection_generation,source_high_water,invalidation_reason) VALUES(?1,?2,?3,0,NULL)",
                            params![
                                commit.session_id.as_str(),
                                commit.projection_version.as_str(),
                                commit.projection_generation.as_str()
                            ],
                        )
                        .map_err(|error| format!("create transcript projection head failed: {error}"))?;
                    0
                }
                None => {
                    return Err(
                        "transcript projection sourceHighWater conflict: projection head is missing"
                            .to_string(),
                    )
                }
            };
            if current_source_high_water != expected_source_high_water {
                return Err(format!(
                    "transcript projection sourceHighWater conflict: expected {expected_source_high_water}, actual {current_source_high_water}"
                ));
            }

            for item in &commit.upserts {
                save_block_version(&transaction, &commit, item)?;
            }
            for cursor in &commit.resume_cursors {
                transaction
                    .execute(
                        "INSERT INTO transcript_resume_cursors(session_id,projection_version,projection_generation,stream_id,source_high_water,cursor) VALUES(?1,?2,?3,?4,?5,?6)",
                        params![
                            commit.session_id.as_str(),
                            commit.projection_version.as_str(),
                            commit.projection_generation.as_str(),
                            cursor.stream_id.as_str(),
                            source_high_water,
                            cursor.cursor.as_str()
                        ],
                    )
                    .map_err(|error| format!("save transcript resume cursor failed: {error}"))?;
            }
            if let Some(checkpoint) = &commit.checkpoint {
                let frontier = commit
                    .frontier
                    .as_ref()
                    .expect("validated checkpoint frontier pair");
                let frontier_json = serde_json::to_string(frontier).map_err(|error| {
                    format!("serialize transcript projection frontier failed: {error}")
                })?;
                let checkpoint_json = serde_json::to_string(checkpoint).map_err(|error| {
                    format!("serialize transcript projection checkpoint failed: {error}")
                })?;
                transaction
                    .execute(
                        "INSERT INTO transcript_projection_current_recoveries(session_id,projection_version,projection_generation,source_high_water,checkpoint_json,frontier_json) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(session_id,projection_version,projection_generation) DO UPDATE SET source_high_water=excluded.source_high_water,checkpoint_json=excluded.checkpoint_json,frontier_json=excluded.frontier_json",
                        params![
                            checkpoint.session_id.as_str(),
                            checkpoint.projection_version.as_str(),
                            checkpoint.projection_generation.as_str(),
                            source_high_water,
                            checkpoint_json,
                            frontier_json
                        ],
                    )
                    .map_err(|error| format!("save transcript current recovery failed: {error}"))?;
            }
            let changed = transaction
                .execute(
                    "UPDATE transcript_projection_heads SET source_high_water=?1,invalidation_reason=?2 WHERE session_id=?3 AND projection_version=?4 AND projection_generation=?5 AND source_high_water=?6 AND invalidation_reason IS NULL",
                    params![
                        source_high_water,
                        commit.invalidation_reason.as_deref(),
                        commit.session_id.as_str(),
                        commit.projection_version.as_str(),
                        commit.projection_generation.as_str(),
                        expected_source_high_water
                    ],
                )
                .map_err(|error| format!("advance transcript projection head failed: {error}"))?;
            if changed != 1 {
                return Err("transcript projection sourceHighWater conflict while committing".to_string());
            }
            transaction
                .execute(
                    "INSERT INTO transcript_projection_commits(commit_id,session_id,projection_version,projection_generation,expected_source_high_water,source_high_water,commit_json) VALUES(?1,?2,?3,?4,?5,?6,?7)",
                    params![
                        commit.commit_id.as_str(),
                        commit.session_id.as_str(),
                        commit.projection_version.as_str(),
                        commit.projection_generation.as_str(),
                        expected_source_high_water,
                        source_high_water,
                        commit_json
                    ],
                )
                .map_err(|error| format!("record transcript projection commit failed: {error}"))?;
            transaction
                .commit()
                .map_err(|error| format!("commit transcript projection failed: {error}"))?;
            Ok(TranscriptProjectionCommitDispositionV1::Applied)
        })
    }

    fn load_transcript_projection_head(
        &self,
        session_id: &str,
        projection_generation: &str,
    ) -> Result<Option<TranscriptProjectionHeadV1>, String> {
        require_nonempty(session_id, "sessionId")?;
        require_nonempty(projection_generation, "projectionGeneration")?;
        self.with_conn(|conn| {
            conn.query_row(
                "SELECT source_high_water,invalidation_reason FROM transcript_projection_heads WHERE session_id=?1 AND projection_version=?2 AND projection_generation=?3",
                params![session_id, TRANSCRIPT_PROJECTION_VERSION_V1, projection_generation],
                |row| {
                    Ok(TranscriptProjectionHeadV1 {
                        session_id: session_id.to_string(),
                        projection_version: TRANSCRIPT_PROJECTION_VERSION_V1.to_string(),
                        projection_generation: projection_generation.to_string(),
                        source_high_water: row.get::<_, i64>(0)?.to_string(),
                        invalidation_reason: row.get(1)?,
                    })
                },
            )
            .optional()
            .map_err(|error| format!("load transcript projection head failed: {error}"))
            .and_then(|head| {
                if let Some(head) = &head {
                    head.validate()?;
                }
                Ok(head)
            })
        })
    }

    fn load_transcript_page(
        &self,
        request: TranscriptPageReadRequestV1,
    ) -> Result<TranscriptPageReadResultV1, String> {
        request.validate()?;
        let source_high_water =
            decimal_to_i64(request.source_high_water.as_str(), "sourceHighWater")?;
        let before = transcript_page_before_order(&request)?;
        self.with_conn(|conn| {
            let head = conn
                .query_row(
                    "SELECT source_high_water,invalidation_reason FROM transcript_projection_heads WHERE session_id=?1 AND projection_version=?2 AND projection_generation=?3",
                    params![
                        request.session_id.as_str(),
                        request.projection_version.as_str(),
                        request.projection_generation.as_str()
                    ],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?)),
                )
                .optional()
                .map_err(|error| format!("load transcript projection head failed: {error}"))?
                .ok_or_else(|| "transcript projection head is missing".to_string())?;
            if let Some(reason) = head.1 {
                return Err(format!("transcript projection is invalidated: {reason}"));
            }
            if source_high_water > head.0 {
                return Err("transcript page sourceHighWater is not fully projected".to_string());
            }
            let resume_cursors = load_resume_cursors(
                conn,
                request.session_id.as_str(),
                request.projection_version.as_str(),
                request.projection_generation.as_str(),
                source_high_water,
            )?;
            let resume_cursor_rows_read = resume_cursors.len();
            let (page, candidate_rows_read) = match before {
                Some((before_sequence, before_ordinal)) => {
                    let before_sequence = u64_to_i64(before_sequence, "olderCursor sequence")?;
                    let before_ordinal = i64::from(before_ordinal);
                    let mut statement = conn
                        .prepare(TRANSCRIPT_PAGE_BEFORE_SQL)
                        .map_err(|error| format!("prepare older transcript page failed: {error}"))?;
                    let rows = statement
                        .query_map(
                            params![
                                request.session_id.as_str(),
                                request.projection_version.as_str(),
                                request.projection_generation.as_str(),
                                source_high_water,
                                before_sequence,
                                before_ordinal
                            ],
                            |row| row.get::<_, String>(0),
                        )
                        .map_err(|error| format!("query older transcript page failed: {error}"))?;
                    assemble_transcript_page_from_newest(
                        &request,
                        decode_block_rows(rows),
                        resume_cursors,
                    )?
                }
                None => {
                    let mut statement = conn
                        .prepare(TRANSCRIPT_TAIL_PAGE_SQL)
                        .map_err(|error| format!("prepare transcript tail page failed: {error}"))?;
                    let rows = statement
                        .query_map(
                            params![
                                request.session_id.as_str(),
                                request.projection_version.as_str(),
                                request.projection_generation.as_str(),
                                source_high_water
                            ],
                            |row| row.get::<_, String>(0),
                        )
                        .map_err(|error| format!("query transcript tail page failed: {error}"))?;
                    assemble_transcript_page_from_newest(
                        &request,
                        decode_block_rows(rows),
                        resume_cursors,
                    )?
                }
            };
            Ok(TranscriptPageReadResultV1 {
                page,
                work: TranscriptPersistentPageQueryWorkV1 {
                    candidate_rows_read,
                    resume_cursor_rows_read,
                    raw_event_visits: 0,
                },
            })
        })
    }

    fn load_latest_transcript_checkpoint(
        &self,
        session_id: &str,
        projection_generation: &str,
        at_or_before_source_high_water: u64,
    ) -> Result<Option<TranscriptProjectionCheckpointV1>, String> {
        require_nonempty(session_id, "sessionId")?;
        require_nonempty(projection_generation, "projectionGeneration")?;
        let high_water = u64_to_i64(at_or_before_source_high_water, "checkpoint sourceHighWater")?;
        self.with_conn(|conn| {
            let current = conn
                .query_row(
                    "SELECT checkpoint_json FROM transcript_projection_current_recoveries WHERE session_id=?1 AND projection_version=?2 AND projection_generation=?3 AND source_high_water<=?4",
                    params![session_id, TRANSCRIPT_PROJECTION_VERSION_V1, projection_generation, high_water],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(|error| format!("load transcript current checkpoint failed: {error}"))?;
            let checkpoint_json = if current.is_some() {
                current
            } else {
                conn
                .query_row(
                    "SELECT checkpoint_json FROM transcript_projection_checkpoints WHERE session_id=?1 AND projection_version=?2 AND projection_generation=?3 AND source_high_water<=?4 ORDER BY source_high_water DESC LIMIT 1",
                    params![session_id, TRANSCRIPT_PROJECTION_VERSION_V1, projection_generation, high_water],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(|error| format!("load transcript projection checkpoint failed: {error}"))?
            };
            checkpoint_json
                .map(|json| {
                    let checkpoint: TranscriptProjectionCheckpointV1 = serde_json::from_str(&json)
                        .map_err(|error| format!("decode transcript projection checkpoint failed: {error}"))?;
                    checkpoint.validate()?;
                    Ok(checkpoint)
                })
                .transpose()
        })
    }

    fn load_latest_transcript_recovery(
        &self,
        session_id: &str,
        projection_generation: &str,
        at_or_before_source_high_water: u64,
    ) -> Result<Option<TranscriptProjectionRecoveryV1>, String> {
        require_nonempty(session_id, "sessionId")?;
        require_nonempty(projection_generation, "projectionGeneration")?;
        let high_water = u64_to_i64(at_or_before_source_high_water, "checkpoint sourceHighWater")?;
        self.with_conn(|conn| {
            let current = conn
                .query_row(
                    "SELECT checkpoint_json,frontier_json FROM transcript_projection_current_recoveries WHERE session_id=?1 AND projection_version=?2 AND projection_generation=?3 AND source_high_water<=?4",
                    params![session_id, TRANSCRIPT_PROJECTION_VERSION_V1, projection_generation, high_water],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()
                .map_err(|error| format!("load transcript current recovery failed: {error}"))?;
            let stored = if current.is_some() {
                current
            } else {
                conn
                .query_row(
                    "SELECT checkpoint.checkpoint_json,frontier.frontier_json FROM transcript_projection_checkpoints AS checkpoint JOIN transcript_projection_frontiers AS frontier ON frontier.frontier_ref=checkpoint.frontier_ref WHERE checkpoint.session_id=?1 AND checkpoint.projection_version=?2 AND checkpoint.projection_generation=?3 AND checkpoint.source_high_water<=?4 ORDER BY checkpoint.source_high_water DESC LIMIT 1",
                    params![session_id, TRANSCRIPT_PROJECTION_VERSION_V1, projection_generation, high_water],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()
                .map_err(|error| format!("load transcript projection recovery failed: {error}"))?
            };
            stored
                .map(|(checkpoint_json, frontier_json)| {
                    let recovery = TranscriptProjectionRecoveryV1 {
                        checkpoint: serde_json::from_str::<TranscriptProjectionCheckpointV1>(
                            &checkpoint_json,
                        )
                        .map_err(|error| {
                            format!("decode transcript projection checkpoint failed: {error}")
                        })?,
                        frontier: serde_json::from_str::<TranscriptProjectionFrontierV1>(
                            &frontier_json,
                        )
                        .map_err(|error| {
                            format!("decode transcript projection frontier failed: {error}")
                        })?,
                    };
                    recovery.validate()?;
                    Ok(recovery)
                })
                .transpose()
        })
    }

    fn load_transcript_patches(
        &self,
        request: TranscriptPatchReadRequestV1,
    ) -> Result<TranscriptPatchReadResultV1, String> {
        request.validate()?;
        let after = decimal_to_i64(
            request.after_source_high_water.as_str(),
            "patch afterSourceHighWater",
        )?;
        let through = decimal_to_i64(
            request.through_source_high_water.as_str(),
            "patch throughSourceHighWater",
        )?;
        self.with_conn(|conn| {
            let (head, invalidation_reason) = conn
                .query_row(
                    "SELECT source_high_water,invalidation_reason FROM transcript_projection_heads WHERE session_id=?1 AND projection_version=?2 AND projection_generation=?3",
                    params![
                        request.session_id.as_str(),
                        request.projection_version.as_str(),
                        request.projection_generation.as_str()
                    ],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?)),
                )
                .optional()
                .map_err(|error| format!("load transcript projection head failed: {error}"))?
                .ok_or_else(|| "transcript projection head is missing".to_string())?;
            if let Some(reason) = invalidation_reason {
                return Err(format!("transcript projection is invalidated: {reason}"));
            }
            if through > head {
                return Err("transcript patch throughSourceHighWater is not fully projected".to_string());
            }

            let mut statement = conn
                .prepare(
                    "SELECT commit_json FROM transcript_projection_commits INDEXED BY idx_transcript_projection_commits_session WHERE session_id=?1 AND projection_version=?2 AND projection_generation=?3 AND source_high_water>?4 AND source_high_water<=?5 ORDER BY source_high_water ASC,commit_id ASC",
                )
                .map_err(|error| format!("prepare transcript patch query failed: {error}"))?;
            let rows = statement
                .query_map(
                    params![
                        request.session_id.as_str(),
                        request.projection_version.as_str(),
                        request.projection_generation.as_str(),
                        after,
                        through
                    ],
                    |row| row.get::<_, String>(0),
                )
                .map_err(|error| format!("query transcript projection commits failed: {error}"))?;

            let mut patches = Vec::new();
            let mut commit_rows_read = 0usize;
            let mut has_more = false;
            for row in rows {
                commit_rows_read = commit_rows_read.saturating_add(1);
                let commit_json = row.map_err(|error| {
                    format!("decode transcript projection commit row failed: {error}")
                })?;
                let commit: TranscriptProjectionCommitV1 = serde_json::from_str(&commit_json)
                    .map_err(|error| {
                        format!("decode stored transcript projection commit failed: {error}")
                    })?;
                commit.validate()?;
                let cursor = commit.resume_cursors.as_slice();
                let [cursor] = cursor else {
                    return Err(
                        "transcript projection commit must contain exactly one resume cursor"
                            .to_string(),
                    );
                };
                let patch = TranscriptPatchV1 {
                    schema: TRANSCRIPT_PATCH_SCHEMA_V1.to_string(),
                    session_id: commit.session_id,
                    projection_version: commit.projection_version,
                    projection_generation: commit.projection_generation,
                    source_high_water: commit.source_high_water,
                    stream_id: cursor.stream_id.clone(),
                    applied_cursor: cursor.cursor.clone(),
                    upserts: commit.upserts.into_iter().map(|item| item.block).collect(),
                    removals: Vec::new(),
                };
                patch.validate()?;
                let mut candidate_patches = patches.clone();
                candidate_patches.push(patch.clone());
                let candidate = TranscriptPatchReadResultV1 {
                    next_source_high_water: patch.source_high_water.clone(),
                    patches: candidate_patches,
                    has_more: false,
                    work: TranscriptPersistentPatchQueryWorkV1 {
                        commit_rows_read,
                        raw_event_visits: 0,
                    },
                };
                let candidate_bytes = transcript_patch_read_result_serialized_bytes(&candidate)?;
                if candidate_bytes > TRANSCRIPT_PATCH_READ_SERIALIZED_MAX_BYTES
                    && patches.is_empty()
                {
                    return Err("transcript patch cannot fit the patch read budget".to_string());
                }
                if patches.len() == TRANSCRIPT_PATCH_READ_MAX_PATCHES
                    || candidate_bytes > TRANSCRIPT_PATCH_READ_SERIALIZED_MAX_BYTES
                {
                    has_more = true;
                    break;
                }
                patches.push(patch);
            }
            let next_source_high_water = patches
                .last()
                .map(|patch| patch.source_high_water.clone())
                .unwrap_or_else(|| request.after_source_high_water.clone());
            let result = TranscriptPatchReadResultV1 {
                patches,
                next_source_high_water,
                has_more,
                work: TranscriptPersistentPatchQueryWorkV1 {
                    commit_rows_read,
                    raw_event_visits: 0,
                },
            };
            result.validate(&request)?;
            Ok(result)
        })
    }

    fn load_transcript_resume_cursors(
        &self,
        session_id: &str,
        projection_generation: &str,
        at_or_before_source_high_water: u64,
    ) -> Result<Vec<TranscriptResumeCursorV1>, String> {
        require_nonempty(session_id, "sessionId")?;
        require_nonempty(projection_generation, "projectionGeneration")?;
        let high_water = u64_to_i64(
            at_or_before_source_high_water,
            "resume cursor sourceHighWater",
        )?;
        self.with_conn(|conn| {
            let (head, invalidation_reason) = conn
                .query_row(
                    "SELECT source_high_water,invalidation_reason FROM transcript_projection_heads WHERE session_id=?1 AND projection_version=?2 AND projection_generation=?3",
                    params![
                        session_id,
                        TRANSCRIPT_PROJECTION_VERSION_V1,
                        projection_generation
                    ],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?)),
                )
                .optional()
                .map_err(|error| format!("load transcript projection head failed: {error}"))?
                .ok_or_else(|| "transcript projection head is missing".to_string())?;
            if let Some(reason) = invalidation_reason {
                return Err(format!("transcript projection is invalidated: {reason}"));
            }
            if high_water > head {
                return Err(
                    "transcript resume cursor sourceHighWater is not fully projected".to_string(),
                );
            }
            let cursors = load_resume_cursors(
                conn,
                session_id,
                TRANSCRIPT_PROJECTION_VERSION_V1,
                projection_generation,
                high_water,
            )?;
            validate_transcript_resume_cursor_read(cursors.as_slice())?;
            Ok(cursors)
        })
    }
}

impl TranscriptProjectionGenerationStorePortV1 for SqliteRuntimeStore {
    fn load_current_transcript_projection_generation(
        &self,
        session_id: &str,
    ) -> Result<Option<TranscriptProjectionCurrentGenerationV1>, String> {
        require_nonempty(session_id, "sessionId")?;
        self.with_conn(|conn| {
            let current = conn
                .query_row(
                    "SELECT projection_generation,source_high_water FROM transcript_projection_current_generations WHERE session_id=?1 AND projection_version=?2",
                    params![session_id, TRANSCRIPT_PROJECTION_VERSION_V1],
                    |row| {
                        Ok(TranscriptProjectionCurrentGenerationV1 {
                            session_id: session_id.to_string(),
                            projection_version: TRANSCRIPT_PROJECTION_VERSION_V1.to_string(),
                            projection_generation: row.get(0)?,
                            source_high_water: row.get::<_, i64>(1)?.to_string(),
                        })
                    },
                )
                .optional()
                .map_err(|error| {
                    format!("load current transcript projection generation failed: {error}")
                })?;
            if let Some(current) = &current {
                current.validate()?;
            }
            Ok(current)
        })
    }

    fn rotate_current_transcript_projection_generation(
        &self,
        rotation: TranscriptProjectionGenerationRotationV1,
    ) -> Result<TranscriptProjectionGenerationRotationDispositionV1, String> {
        rotation.validate()?;
        let target = decimal_to_i64(
            rotation.target_source_high_water.as_str(),
            "generation targetSourceHighWater",
        )?;
        self.with_conn(|conn| {
            let transaction = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|error| format!("begin transcript generation rotation failed: {error}"))?;
            let next_head = transaction
                .query_row(
                    "SELECT source_high_water,invalidation_reason FROM transcript_projection_heads WHERE session_id=?1 AND projection_version=?2 AND projection_generation=?3",
                    params![
                        rotation.session_id.as_str(),
                        rotation.projection_version.as_str(),
                        rotation.next_generation.as_str()
                    ],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?)),
                )
                .optional()
                .map_err(|error| format!("load next transcript generation head failed: {error}"))?
                .ok_or_else(|| "next transcript projection generation head is missing".to_string())?;
            if next_head.0 != target || next_head.1.is_some() {
                return Err(
                    "next transcript projection generation is not complete and readable"
                        .to_string(),
                );
            }
            let current = transaction
                .query_row(
                    "SELECT projection_generation,source_high_water FROM transcript_projection_current_generations WHERE session_id=?1 AND projection_version=?2",
                    params![rotation.session_id.as_str(), rotation.projection_version.as_str()],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
                )
                .optional()
                .map_err(|error| format!("load current transcript generation failed: {error}"))?;
            if current.as_ref()
                == Some(&(rotation.next_generation.clone(), target))
            {
                return Ok(TranscriptProjectionGenerationRotationDispositionV1::AlreadyApplied);
            }
            match (&rotation.expected_current_generation, current) {
                (None, None) => {
                    transaction
                        .execute(
                            "INSERT INTO transcript_projection_current_generations(session_id,projection_version,projection_generation,source_high_water) VALUES(?1,?2,?3,?4)",
                            params![
                                rotation.session_id.as_str(),
                                rotation.projection_version.as_str(),
                                rotation.next_generation.as_str(),
                                target
                            ],
                        )
                        .map_err(|error| format!("publish initial transcript generation failed: {error}"))?;
                }
                (Some(expected), Some((current_generation, _)))
                    if expected == &current_generation =>
                {
                    let changed = transaction
                        .execute(
                            "UPDATE transcript_projection_current_generations SET projection_generation=?1,source_high_water=?2 WHERE session_id=?3 AND projection_version=?4 AND projection_generation=?5",
                            params![
                                rotation.next_generation.as_str(),
                                target,
                                rotation.session_id.as_str(),
                                rotation.projection_version.as_str(),
                                expected.as_str()
                            ],
                        )
                        .map_err(|error| format!("rotate transcript generation failed: {error}"))?;
                    if changed != 1 {
                        return Err("transcript projection generation rotation conflict".to_string());
                    }
                }
                _ => {
                    return Err("transcript projection generation rotation conflict".to_string())
                }
            }
            transaction
                .commit()
                .map_err(|error| format!("commit transcript generation rotation failed: {error}"))?;
            Ok(TranscriptProjectionGenerationRotationDispositionV1::Applied)
        })
    }
}

fn save_block_version(
    transaction: &rusqlite::Transaction<'_>,
    commit: &TranscriptProjectionCommitV1,
    item: &centaeris_core::session::transcript::TranscriptCommittedBlockV1,
) -> Result<(), String> {
    let applied_source_sequence = decimal_to_i64(
        item.applied_source_sequence.as_str(),
        "appliedSourceSequence",
    )?;
    let order_source_sequence = decimal_to_i64(
        item.block.order_key.source_sequence.as_str(),
        "order sourceSequence",
    )?;
    let order_ordinal = i64::from(item.block.order_key.ordinal);
    let block_revision = decimal_to_i64(item.block.block_revision.as_str(), "blockRevision")?;
    let identity = transaction
        .query_row(
            "SELECT order_source_sequence,order_ordinal FROM transcript_block_identities WHERE session_id=?1 AND projection_version=?2 AND projection_generation=?3 AND block_id=?4",
            params![
                commit.session_id.as_str(),
                commit.projection_version.as_str(),
                commit.projection_generation.as_str(),
                item.block.block_id.as_str()
            ],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()
        .map_err(|error| format!("load transcript block identity failed: {error}"))?;
    if let Some(existing) = identity {
        if existing != (order_source_sequence, order_ordinal) {
            return Err("transcript block orderKey changed across revisions".to_string());
        }
    } else {
        if applied_source_sequence != order_source_sequence {
            return Err(
                "transcript block first revision must be applied at its order sourceSequence"
                    .to_string(),
            );
        }
        transaction
            .execute(
                "INSERT INTO transcript_block_identities(session_id,projection_version,projection_generation,block_id,order_source_sequence,order_ordinal) VALUES(?1,?2,?3,?4,?5,?6)",
                params![
                    commit.session_id.as_str(),
                    commit.projection_version.as_str(),
                    commit.projection_generation.as_str(),
                    item.block.block_id.as_str(),
                    order_source_sequence,
                    order_ordinal
                ],
            )
            .map_err(|error| format!("save transcript block identity failed: {error}"))?;
    }
    if let Some((previous_applied, previous_revision)) = transaction
        .query_row(
            "SELECT applied_source_sequence,block_revision FROM transcript_block_versions WHERE session_id=?1 AND projection_version=?2 AND projection_generation=?3 AND block_id=?4 ORDER BY applied_source_sequence DESC LIMIT 1",
            params![
                commit.session_id.as_str(),
                commit.projection_version.as_str(),
                commit.projection_generation.as_str(),
                item.block.block_id.as_str()
            ],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()
        .map_err(|error| format!("load transcript block version failed: {error}"))?
    {
        if applied_source_sequence <= previous_applied || block_revision <= previous_revision {
            return Err("transcript block version is not increasing".to_string());
        }
    }
    let block_json = serde_json::to_string(&item.block)
        .map_err(|error| format!("serialize transcript block failed: {error}"))?;
    transaction
        .execute(
            "INSERT INTO transcript_block_versions(session_id,projection_version,projection_generation,block_id,applied_source_sequence,block_revision,block_json) VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![
                commit.session_id.as_str(),
                commit.projection_version.as_str(),
                commit.projection_generation.as_str(),
                item.block.block_id.as_str(),
                applied_source_sequence,
                block_revision,
                block_json
            ],
        )
        .map_err(|error| format!("save transcript block version failed: {error}"))?;
    Ok(())
}

fn load_resume_cursors(
    conn: &rusqlite::Connection,
    session_id: &str,
    projection_version: &str,
    projection_generation: &str,
    source_high_water: i64,
) -> Result<Vec<TranscriptResumeCursorV1>, String> {
    let mut statement = conn
        .prepare(
            "SELECT current.stream_id,current.cursor FROM transcript_resume_cursors current WHERE current.session_id=?1 AND current.projection_version=?2 AND current.projection_generation=?3 AND current.source_high_water=(SELECT MAX(candidate.source_high_water) FROM transcript_resume_cursors candidate WHERE candidate.session_id=current.session_id AND candidate.projection_version=current.projection_version AND candidate.projection_generation=current.projection_generation AND candidate.stream_id=current.stream_id AND candidate.source_high_water<=?4) ORDER BY current.stream_id ASC",
        )
        .map_err(|error| format!("prepare transcript resume cursor query failed: {error}"))?;
    let rows = statement
        .query_map(
            params![
                session_id,
                projection_version,
                projection_generation,
                source_high_water
            ],
            |row| {
                Ok(TranscriptResumeCursorV1 {
                    stream_id: row.get(0)?,
                    cursor: row.get(1)?,
                })
            },
        )
        .map_err(|error| format!("query transcript resume cursors failed: {error}"))?;
    let cursors = rows
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("decode transcript resume cursor failed: {error}"))?;
    if cursors.len() > TRANSCRIPT_PAGE_RESUME_CURSOR_MAX_COUNT {
        return Err("transcript projection exceeds maximum resume stream count".to_string());
    }
    for cursor in &cursors {
        cursor.validate()?;
    }
    Ok(cursors)
}

fn decode_block_rows<'statement>(
    rows: rusqlite::MappedRows<
        'statement,
        impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<String> + 'statement,
    >,
) -> impl Iterator<Item = Result<TranscriptBlockV1, String>> + 'statement {
    rows.map(|row| {
        let json = row.map_err(|error| format!("decode transcript block row failed: {error}"))?;
        let block: TranscriptBlockV1 = serde_json::from_str(&json)
            .map_err(|error| format!("decode stored transcript block failed: {error}"))?;
        block.validate()?;
        Ok(block)
    })
}

fn decimal_to_i64(value: &str, name: &str) -> Result<i64, String> {
    let value = value
        .parse::<u64>()
        .map_err(|_| format!("transcript {name} is not a valid u64"))?;
    u64_to_i64(value, name)
}

fn u64_to_i64(value: u64, name: &str) -> Result<i64, String> {
    i64::try_from(value).map_err(|_| format!("transcript {name} exceeds SQLite INTEGER range"))
}

fn require_nonempty(value: &str, name: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err(format!("transcript {name} must not be empty"));
    }
    Ok(())
}

const TRANSCRIPT_TAIL_PAGE_SQL: &str = "
SELECT version.block_json
FROM transcript_block_identities AS identity INDEXED BY idx_transcript_block_identities_page
JOIN transcript_block_versions version
  ON version.session_id=identity.session_id
 AND version.projection_version=identity.projection_version
 AND version.projection_generation=identity.projection_generation
 AND version.block_id=identity.block_id
WHERE identity.session_id=?1
  AND identity.projection_version=?2
  AND identity.projection_generation=?3
  AND identity.order_source_sequence<=?4
  AND version.applied_source_sequence=(
      SELECT MAX(candidate.applied_source_sequence)
      FROM transcript_block_versions candidate
      WHERE candidate.session_id=identity.session_id
        AND candidate.projection_version=identity.projection_version
        AND candidate.projection_generation=identity.projection_generation
        AND candidate.block_id=identity.block_id
        AND candidate.applied_source_sequence<=?4
  )
ORDER BY identity.order_source_sequence DESC, identity.order_ordinal DESC
";

const TRANSCRIPT_PAGE_BEFORE_SQL: &str = "
SELECT version.block_json
FROM transcript_block_identities AS identity INDEXED BY idx_transcript_block_identities_page
JOIN transcript_block_versions version
  ON version.session_id=identity.session_id
 AND version.projection_version=identity.projection_version
 AND version.projection_generation=identity.projection_generation
 AND version.block_id=identity.block_id
WHERE identity.session_id=?1
  AND identity.projection_version=?2
  AND identity.projection_generation=?3
  AND identity.order_source_sequence<=?4
  AND (identity.order_source_sequence<?5 OR (identity.order_source_sequence=?5 AND identity.order_ordinal<?6))
  AND version.applied_source_sequence=(
      SELECT MAX(candidate.applied_source_sequence)
      FROM transcript_block_versions candidate
      WHERE candidate.session_id=identity.session_id
        AND candidate.projection_version=identity.projection_version
        AND candidate.projection_generation=identity.projection_generation
        AND candidate.block_id=identity.block_id
        AND candidate.applied_source_sequence<=?4
  )
ORDER BY identity.order_source_sequence DESC, identity.order_ordinal DESC
";
