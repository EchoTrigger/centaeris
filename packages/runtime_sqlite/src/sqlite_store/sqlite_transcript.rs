use super::*;
use centaeris_core::session::transcript::{
    assemble_transcript_page_from_newest, transcript_page_before_order, TranscriptBlockV1,
    TranscriptPageReadRequestV1, TranscriptPageReadResultV1, TranscriptPersistentPageQueryWorkV1,
    TranscriptProjectionCheckpointV1, TranscriptProjectionCommitDispositionV1,
    TranscriptProjectionCommitV1, TranscriptProjectionFrontierV1, TranscriptProjectionHeadV1,
    TranscriptProjectionRecoveryV1, TranscriptProjectionStorePort, TranscriptResumeCursorV1,
    TRANSCRIPT_PAGE_RESUME_CURSOR_MAX_COUNT, TRANSCRIPT_PROJECTION_VERSION_V1,
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
        let commit_json = serde_json::to_string(&commit)
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
                if existing == commit {
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
                transaction
                    .execute(
                        "INSERT INTO transcript_projection_frontiers(frontier_ref,session_id,projection_version,projection_generation,source_high_water,frontier_json) VALUES(?1,?2,?3,?4,?5,?6)",
                        params![
                            frontier.frontier_ref.as_str(),
                            commit.session_id.as_str(),
                            commit.projection_version.as_str(),
                            commit.projection_generation.as_str(),
                            source_high_water,
                            frontier_json
                        ],
                    )
                    .map_err(|error| format!("save transcript projection frontier failed: {error}"))?;
                let checkpoint_json = serde_json::to_string(checkpoint).map_err(|error| {
                    format!("serialize transcript projection checkpoint failed: {error}")
                })?;
                transaction
                    .execute(
                        "INSERT INTO transcript_projection_checkpoints(session_id,projection_version,projection_generation,source_high_water,frontier_ref,checkpoint_json) VALUES(?1,?2,?3,?4,?5,?6)",
                        params![
                            commit.session_id.as_str(),
                            commit.projection_version.as_str(),
                            commit.projection_generation.as_str(),
                            source_high_water,
                            frontier.frontier_ref.as_str(),
                            checkpoint_json
                        ],
                    )
                    .map_err(|error| format!("save transcript projection checkpoint failed: {error}"))?;
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
            let checkpoint_json = conn
                .query_row(
                    "SELECT checkpoint_json FROM transcript_projection_checkpoints WHERE session_id=?1 AND projection_version=?2 AND projection_generation=?3 AND source_high_water<=?4 ORDER BY source_high_water DESC LIMIT 1",
                    params![session_id, TRANSCRIPT_PROJECTION_VERSION_V1, projection_generation, high_water],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(|error| format!("load transcript projection checkpoint failed: {error}"))?;
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
            let stored = conn
                .query_row(
                    "SELECT checkpoint.checkpoint_json,frontier.frontier_json FROM transcript_projection_checkpoints AS checkpoint JOIN transcript_projection_frontiers AS frontier ON frontier.frontier_ref=checkpoint.frontier_ref WHERE checkpoint.session_id=?1 AND checkpoint.projection_version=?2 AND checkpoint.projection_generation=?3 AND checkpoint.source_high_water<=?4 ORDER BY checkpoint.source_high_water DESC LIMIT 1",
                    params![session_id, TRANSCRIPT_PROJECTION_VERSION_V1, projection_generation, high_water],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()
                .map_err(|error| format!("load transcript projection recovery failed: {error}"))?;
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
