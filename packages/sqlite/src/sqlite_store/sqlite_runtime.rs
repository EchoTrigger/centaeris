use super::*;
use centaeris_core::runtime::contracts::{
    CheckpointKindV1, CheckpointRecord, EventVisibility, RuntimeEvent,
};
use centaeris_core::session::history::{
    prepare_session_snapshot_write, SessionHistoryEntryV1, SessionHistoryRecordV1,
    SessionHistorySelectionV1, SessionSnapshotWriteV1,
};
use centaeris_core::session::store::{
    AgentRuntimeSnapshotStorePort, RuntimeStore, RuntimeStoreError, SessionDataStorePort,
};
use rusqlite::{params, OptionalExtension};

#[cfg(test)]
#[path = "sqlite_runtime/history_tests.rs"]
mod history_tests;

impl RuntimeStore for SqliteRuntimeStore {
    fn list_runtime_job_waiters(
        &self,
        source_job_id: Option<&str>,
        after: Option<&centaeris_core::session::store::RuntimeJobWaiterCursor>,
        limit: usize,
    ) -> Result<Vec<centaeris_core::session::store::RuntimeJobWaiter>, RuntimeStoreError> {
        use centaeris_core::session::store::{RuntimeJobWaiter, RuntimeJobWaiterCursor};
        self.with_conn(|conn| {
            let checkpoint = after.map_or("", |cursor| cursor.checkpoint_id.as_str());
            let call = after.map_or("", |cursor| cursor.tool_call_id.as_str());
            let base = "SELECT checkpoint_id,tool_call_id,source_job_id,source_job_kind,session_id,agent_run_id FROM runtime_job_waiters WHERE (checkpoint_id,tool_call_id)>(?1,?2)";
            let sql = match source_job_id {
                Some(_) => format!("{base} AND source_job_id=?4 ORDER BY checkpoint_id,tool_call_id LIMIT ?3"),
                None => format!("{base} ORDER BY checkpoint_id,tool_call_id LIMIT ?3"),
            };
            let mut statement = conn.prepare(&sql).map_err(|error| format!("prepare waiter lookup failed: {error}"))?;
            let decode = |row: &rusqlite::Row<'_>| Ok(RuntimeJobWaiter {
                cursor: RuntimeJobWaiterCursor { checkpoint_id: row.get(0)?, tool_call_id: row.get(1)? },
                source_job_id: row.get(2)?, source_job_kind: row.get(3)?, session_id: row.get(4)?, agent_run_id: row.get(5)?,
            });
            let rows = match source_job_id {
                Some(source) => statement.query_map(params![checkpoint, call, to_i64(limit)?, source], decode),
                None => statement.query_map(params![checkpoint, call, to_i64(limit)?], decode),
            }.map_err(|error| format!("query waiters failed: {error}"))?;
            rows.collect::<Result<Vec<_>, _>>().map_err(|error| format!("decode waiters failed: {error}"))
        }).map_err(RuntimeStoreError::backend)
    }

    fn save_checkpoint(&self, checkpoint: CheckpointRecord) -> Result<(), RuntimeStoreError> {
        self.with_conn(|conn| {
            let tx = conn
                .unchecked_transaction()
                .map_err(|error| format!("begin checkpoint save failed: {error}"))?;
            save_checkpoint_conn(&tx, &checkpoint)?;
            tx.commit()
                .map_err(|error| format!("commit checkpoint save failed: {error}"))
        })
        .map_err(RuntimeStoreError::backend)
    }

    fn load_latest_checkpoint(
        &self,
        session_id: &str,
    ) -> Result<Option<CheckpointRecord>, RuntimeStoreError> {
        self.with_conn(|conn| {
            let mut stmt = conn
                .prepare(
                    "
                    SELECT checkpoint_id, kind, session_id, turn_id, status, done_reason,
                           updated_at_ms, payload_json
                    FROM checkpoints
                    WHERE session_id = ?1 AND kind != 'recovery'
                    ORDER BY updated_at_ms DESC, checkpoint_id DESC
                    LIMIT 1
                    ",
                )
                .map_err(|err| format!("prepare load_latest_checkpoint failed: {err}"))?;

            let row = stmt
                .query_row(params![session_id], row_to_checkpoint)
                .optional()
                .map_err(|err| format!("query load_latest_checkpoint failed: {err}"))?;
            Ok(row)
        })
        .map_err(RuntimeStoreError::backend)
    }

    fn load_checkpoint_by_turn(
        &self,
        session_id: &str,
        turn_id: &str,
    ) -> Result<Option<CheckpointRecord>, RuntimeStoreError> {
        self.with_conn(|conn| {
            let mut stmt = conn
                .prepare(
                    "
                    SELECT checkpoint_id, kind, session_id, turn_id, status, done_reason,
                           updated_at_ms, payload_json
                    FROM checkpoints
                    WHERE session_id = ?1 AND turn_id = ?2 AND kind != 'recovery'
                    LIMIT 1
                    ",
                )
                .map_err(|err| format!("prepare load_checkpoint_by_turn failed: {err}"))?;

            let row = stmt
                .query_row(params![session_id, turn_id], row_to_checkpoint)
                .optional()
                .map_err(|err| format!("query load_checkpoint_by_turn failed: {err}"))?;
            Ok(row)
        })
        .map_err(RuntimeStoreError::backend)
    }

    fn list_checkpoints(
        &self,
        session_id: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<CheckpointRecord>, RuntimeStoreError> {
        self.with_conn(|conn| {
            let limit_i64 = to_i64(limit)?;
            let offset_i64 = to_i64(offset)?;
            let mut stmt = conn
                .prepare(
                    "
                    SELECT checkpoint_id, kind, session_id, turn_id, status, done_reason,
                           updated_at_ms, payload_json
                    FROM checkpoints
                    WHERE session_id = ?1
                    ORDER BY updated_at_ms DESC, checkpoint_id DESC
                    LIMIT ?2 OFFSET ?3
                    ",
                )
                .map_err(|err| format!("prepare list_checkpoints failed: {err}"))?;

            let rows = stmt
                .query_map(
                    params![session_id, limit_i64, offset_i64],
                    row_to_checkpoint,
                )
                .map_err(|err| format!("query list_checkpoints failed: {err}"))?;

            let mut items = Vec::new();
            for row in rows {
                items
                    .push(row.map_err(|err| format!("decode list_checkpoints row failed: {err}"))?);
            }
            Ok(items)
        })
        .map_err(RuntimeStoreError::backend)
    }

    fn list_waiting_runtime_job_checkpoints(
        &self,
        after: Option<&centaeris_core::session::store::RuntimeJobWaitCheckpointCursor>,
        limit: usize,
    ) -> Result<Vec<CheckpointRecord>, RuntimeStoreError> {
        self.with_conn(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT checkpoint_id,kind,session_id,turn_id,status,done_reason,updated_at_ms,payload_json FROM checkpoints WHERE kind='wait' AND status='waiting' AND done_reason='runtime_job' AND (?1 IS NULL OR session_id>?1 OR (session_id=?1 AND turn_id>?2)) ORDER BY session_id,turn_id LIMIT ?3",
                )
                .map_err(|error| {
                    format!("prepare list_waiting_runtime_job_checkpoints failed: {error}")
                })?;
            let rows = stmt
                .query_map(
                    params![
                        after.map(|cursor| cursor.session_id.as_str()),
                        after.map(|cursor| cursor.turn_id.as_str()),
                        to_i64(limit)?
                    ],
                    row_to_checkpoint,
                )
                .map_err(|error| {
                    format!("query list_waiting_runtime_job_checkpoints failed: {error}")
                })?;
            rows.collect::<Result<Vec<_>, _>>().map_err(|error| {
                format!("decode list_waiting_runtime_job_checkpoints failed: {error}")
            })
        })
        .map_err(RuntimeStoreError::backend)
    }

    fn append_event(&self, event: RuntimeEvent) -> Result<(), RuntimeStoreError> {
        self.with_conn(|conn| append_event_conn(conn, &event))
            .map_err(RuntimeStoreError::backend)
    }

    fn append_event_idempotent(&self, event: RuntimeEvent) -> Result<(), RuntimeStoreError> {
        self.with_conn(|conn| append_event_conn_idempotent(conn, &event).map(|_| ()))
            .map_err(RuntimeStoreError::backend)
    }

    fn list_events(
        &self,
        session_id: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<RuntimeEvent>, RuntimeStoreError> {
        self.with_conn(|conn| {
            let limit_i64 = to_i64(limit)?;
            let offset_i64 = to_i64(offset)?;

            let mut stmt = conn
                .prepare(
                    "
                    SELECT event_id, session_id, task_id,
                           event_type, at_ms, visibility, payload_json
                    FROM runtime_events
                    WHERE session_id = ?1
                    ORDER BY at_ms ASC, event_id ASC
                    LIMIT ?2 OFFSET ?3
                    ",
                )
                .map_err(|err| format!("prepare list_events failed: {err}"))?;

            let rows = stmt
                .query_map(params![session_id, limit_i64, offset_i64], row_to_event)
                .map_err(|err| format!("query list_events failed: {err}"))?;

            let mut items = Vec::new();
            for row in rows {
                items.push(row.map_err(|err| format!("decode list_events row failed: {err}"))?);
            }
            Ok(items)
        })
        .map_err(RuntimeStoreError::backend)
    }
}

impl SessionDataStorePort for SqliteRuntimeStore {
    fn delete_session_data(&self, session_id: &str) -> Result<(), String> {
        let session_id = session_id.trim();
        if session_id.is_empty() {
            return Err("sessionId is required".to_string());
        }
        self.with_conn(|conn| {
            let tx = conn
                .transaction()
                .map_err(|error| format!("begin delete session data transaction failed: {error}"))?;
            let object_ids = {
                let mut statement = tx
                    .prepare(
                        "SELECT DISTINCT object_id FROM external_context_links WHERE session_id = ?1",
                    )
                    .map_err(|error| format!("prepare session external object lookup failed: {error}"))?;
                let rows = statement
                    .query_map(params![session_id], |row| row.get::<_, String>(0))
                    .map_err(|error| format!("query session external object ids failed: {error}"))?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|error| format!("read session external object ids failed: {error}"))?;
                rows
            };
            for table in [
                "runtime_events",
                "session_runtime_snapshots",
                "session_message_history_heads",
                "session_message_history_entries",
                "session_model_input_images",
                "checkpoints",
                "dead_letters",
                "resource_claims",
                "runtime_jobs",
                "external_context_links",
                "transcript_projection_heads",
            ] {
                tx.execute(
                    format!("DELETE FROM {table} WHERE session_id = ?1").as_str(),
                    params![session_id],
                )
                .map_err(|error| format!("delete session data from {table} failed: {error}"))?;
            }
            for object_id in object_ids {
                tx.execute(
                    "DELETE FROM external_context_objects WHERE object_id = ?1 AND NOT EXISTS (SELECT 1 FROM external_context_links WHERE object_id = ?1)",
                    params![object_id],
                )
                .map_err(|error| format!("delete orphan external context object failed: {error}"))?;
            }
            tx.commit()
                .map_err(|error| format!("commit delete session data failed: {error}"))
        })
    }
}

fn visibility_to_db(visibility: &EventVisibility) -> &'static str {
    match visibility {
        EventVisibility::User => "user",
        EventVisibility::Internal => "internal",
    }
}

fn visibility_from_db(raw: &str) -> Result<EventVisibility, String> {
    match raw {
        "user" | "User" => Ok(EventVisibility::User),
        "internal" | "Internal" => Ok(EventVisibility::Internal),
        _ => Err(format!("invalid event visibility: {raw}")),
    }
}

impl AgentRuntimeSnapshotStorePort for SqliteRuntimeStore {
    fn load_session_history_page(
        &self,
        session_id: &str,
        before_position: Option<u64>,
        limit: usize,
        selection: SessionHistorySelectionV1,
    ) -> Result<Vec<SessionHistoryRecordV1>, String> {
        if !(1..=256).contains(&limit) {
            return Err("session_history_page_limit_invalid".into());
        }
        let before = before_position
            .map(i64::try_from)
            .transpose()
            .map_err(|_| "session_history_cursor_out_of_range".to_string())?
            .unwrap_or(i64::MAX);
        self.with_conn(|conn| {
            let mut statement = conn.prepare(
                "SELECT h.position,e.entry_json FROM session_message_history_heads h
                 JOIN session_message_history_entries e ON e.session_id=h.session_id AND e.record_id=h.current_record_id
                 WHERE h.session_id=?1 AND h.position<?2 AND h.retired=0
                 AND (?3=0 OR h.is_original_user_request=1)
                 ORDER BY h.position DESC LIMIT ?4"
            ).map_err(|error| error.to_string())?;
            let rows = statement.query_map(params![session_id, before,
                i64::from(selection == SessionHistorySelectionV1::OriginalUserRequests), limit as i64],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)))
                .map_err(|error| error.to_string())?;
            rows.map(|row| {
                let (position, raw) = row.map_err(|error| error.to_string())?;
                let entry: SessionHistoryEntryV1 = serde_json::from_str(&raw).map_err(|error| error.to_string())?;
                entry.validate()?;
                Ok(SessionHistoryRecordV1 { position: u64::try_from(position)
                    .map_err(|_| "session_history_position_invalid".to_string())?, entry })
            }).collect()
        })
    }

    fn save_model_input_image(
        &self,
        session_id: &str,
        digest: &str,
        bytes: &[u8],
    ) -> Result<(), String> {
        centaeris_core::model::prepared_prompt::validate_model_input_image_content(digest, bytes)?;
        self.with_conn(|conn| {
            let inserted = conn.execute(
                "INSERT INTO session_model_input_images(session_id,content_digest,content) VALUES(?1,?2,?3) ON CONFLICT(session_id,content_digest) DO NOTHING",
                params![session_id, digest, bytes],
            ).map_err(|e| e.to_string())?;
            if inserted == 0 {
                let existing: Vec<u8> = conn.query_row(
                    "SELECT content FROM session_model_input_images WHERE session_id=?1 AND content_digest=?2",
                    params![session_id, digest], |row| row.get(0),
                ).map_err(|e| e.to_string())?;
                if existing != bytes { return Err("model_input_image_content_conflict".into()); }
            }
            Ok(())
        })
    }

    fn load_model_input_image(
        &self,
        session_id: &str,
        digest: &str,
    ) -> Result<Option<Vec<u8>>, String> {
        self.with_conn(|conn| conn.query_row(
            "SELECT content FROM session_model_input_images WHERE session_id=?1 AND content_digest=?2",
            params![session_id, digest], |row| row.get(0),
        ).optional().map_err(|e| e.to_string()))
    }

    fn load_agent_runtime_snapshot(&self, session_id: &str) -> Result<Option<String>, String> {
        self.with_conn(|conn| {
            conn.query_row(
                "
                SELECT snapshot_json
                FROM session_runtime_snapshots
                WHERE session_id = ?1
                ",
                params![session_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|err| format!("load session runtime snapshot failed: {err}"))
        })
    }

    fn save_agent_runtime_snapshot(
        &self,
        session_id: &str,
        snapshot_json: &str,
        updated_at_ms: i64,
    ) -> Result<(), String> {
        let write = prepare_session_snapshot_write(snapshot_json)?;
        if write.session_id() != session_id {
            return Err("session_snapshot_identity_mismatch".into());
        }
        self.save_prepared_agent_runtime_snapshot(write, updated_at_ms)
            .map(|_| ())
    }

    fn save_prepared_agent_runtime_snapshot(
        &self,
        write: SessionSnapshotWriteV1,
        updated_at_ms: i64,
    ) -> Result<centaeris_core::session::state::SessionStateSnapshot, String> {
        #[cfg(test)]
        if self
            .fail_next_session_snapshot_save
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return Err("injected one-shot session runtime snapshot save failure".to_string());
        }
        self.with_conn(|conn| {
            let tx = conn.transaction().map_err(|error| error.to_string())?;
            persist_session_history_conn(&tx, write.session_id(), &write)?;
            tx.execute(
                "
                INSERT INTO session_runtime_snapshots(
                    session_id, snapshot_json, updated_at_ms
                )
                VALUES(?1, ?2, ?3)
                ON CONFLICT(session_id) DO UPDATE SET
                    snapshot_json = excluded.snapshot_json,
                    updated_at_ms = excluded.updated_at_ms
                ",
                params![write.session_id(), write.snapshot_json(), updated_at_ms],
            )
            .map_err(|err| format!("save session runtime snapshot failed: {err}"))?;
            tx.commit().map_err(|error| error.to_string())
        })?;
        Ok(write.into_snapshot())
    }

    fn compare_and_save_agent_runtime_snapshot(
        &self,
        session_id: &str,
        expected_snapshot: Option<&str>,
        snapshot_json: &str,
        updated_at_ms: i64,
    ) -> Result<bool, String> {
        #[cfg(test)]
        if self
            .fail_next_session_snapshot_save
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return Err("injected one-shot session runtime snapshot save failure".to_string());
        }
        let write = prepare_session_snapshot_write(snapshot_json)?;
        if write.session_id() != session_id {
            return Err("session_snapshot_identity_mismatch".into());
        }
        self.with_conn(|conn| {
            let tx = conn
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(|error| error.to_string())?;
            let actual = tx
                .query_row(
                    "SELECT snapshot_json FROM session_runtime_snapshots WHERE session_id=?1",
                    [session_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(|error| error.to_string())?;
            if actual.as_deref() != expected_snapshot {
                return Ok(false);
            }
            persist_session_history_conn(&tx, session_id, &write)?;
            let changed = match expected_snapshot {
                Some(expected) => tx.execute(
                    "UPDATE session_runtime_snapshots
                     SET snapshot_json = ?2, updated_at_ms = ?3
                     WHERE session_id = ?1 AND snapshot_json = ?4",
                    params![session_id, write.snapshot_json(), updated_at_ms, expected],
                ),
                None => tx.execute(
                    "INSERT INTO session_runtime_snapshots(session_id, snapshot_json, updated_at_ms)
                     VALUES(?1, ?2, ?3) ON CONFLICT(session_id) DO NOTHING",
                    params![session_id, write.snapshot_json(), updated_at_ms],
                ),
            }
            .map_err(|error| format!("compare and save session runtime snapshot failed: {error}"))?;
            tx.commit().map_err(|error| error.to_string())?;
            Ok(changed == 1)
        })
    }
}

pub(super) fn persist_session_history_conn(
    conn: &rusqlite::Connection,
    session_id: &str,
    write: &SessionSnapshotWriteV1,
) -> Result<(), String> {
    for entry in write.entries() {
        let raw = entry.canonical_json()?;
        let inserted = conn.execute(
            "INSERT INTO session_message_history_entries(session_id,record_id,message_id,entry_json)
             VALUES(?1,?2,?3,?4) ON CONFLICT(session_id,record_id) DO NOTHING",
            params![session_id, entry.record_id, entry.message.message_id, raw],
        ).map_err(|error| error.to_string())?;
        if inserted == 0 {
            let existing: String = conn.query_row(
                "SELECT entry_json FROM session_message_history_entries WHERE session_id=?1 AND record_id=?2",
                params![session_id, entry.record_id], |row| row.get(0),
            ).map_err(|error| error.to_string())?;
            if existing != raw {
                return Err("session_history_immutable_body_conflict".into());
            }
        }
        conn.execute(
            "INSERT INTO session_message_history_heads(session_id,message_id,position,current_record_id,is_original_user_request,retired)
             SELECT ?1,?2,COALESCE(MAX(position),0)+1,?3,?4,0 FROM session_message_history_heads WHERE session_id=?1
             ON CONFLICT(session_id,message_id) DO UPDATE SET current_record_id=excluded.current_record_id,
             is_original_user_request=excluded.is_original_user_request,retired=0
             WHERE current_record_id<>excluded.current_record_id OR is_original_user_request<>excluded.is_original_user_request OR retired<>0",
            params![session_id, entry.message.message_id, entry.record_id,
                i64::from(entry.is_original_user_request())],
        ).map_err(|error| error.to_string())?;
    }
    for id in write.retired_message_ids() {
        let changed = conn.execute(
            "UPDATE session_message_history_heads SET retired=1 WHERE session_id=?1 AND message_id=?2",
            params![session_id, id],
        ).map_err(|error| error.to_string())?;
        if changed != 1 {
            return Err("session_history_retired_source_missing".into());
        }
    }
    for id in write.recovery_retired_message_ids() {
        conn.execute(
            "UPDATE session_message_history_heads SET retired=1 WHERE session_id=?1 AND message_id=?2",
            params![session_id, id],
        ).map_err(|error| error.to_string())?;
    }
    for reference in write.retained_entries() {
        let present: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM session_message_history_heads WHERE session_id=?1 AND message_id=?2 AND current_record_id=?3 AND retired=0)",
            params![session_id, reference.message_id, reference.record_id], |row| row.get(0),
        ).map_err(|error| error.to_string())?;
        if !present {
            return Err("session_history_working_source_missing".into());
        }
    }
    Ok(())
}

pub(super) fn save_checkpoint_conn(
    conn: &rusqlite::Connection,
    checkpoint: &CheckpointRecord,
) -> Result<(), String> {
    let waiters = centaeris_core::session::store::runtime_job_waiters(checkpoint)?;
    if checkpoint.kind == CheckpointKindV1::Recovery {
        let inserted = conn
            .execute(
                "INSERT INTO checkpoints(checkpoint_id,kind,session_id,turn_id,status,done_reason,updated_at_ms,payload_json) VALUES(?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(checkpoint_id) DO NOTHING",
                params![
                    &checkpoint.checkpoint_id,
                    checkpoint.kind.as_str(),
                    &checkpoint.session_id,
                    &checkpoint.turn_id,
                    &checkpoint.status,
                    &checkpoint.done_reason,
                    checkpoint.updated_at_ms,
                    &checkpoint.payload_json,
                ],
            )
            .map_err(|err| format!("save recovery checkpoint failed: {err}"))?;
        if inserted == 0 {
            let existing = conn
                .query_row(
                    "SELECT checkpoint_id,kind,session_id,turn_id,status,done_reason,updated_at_ms,payload_json FROM checkpoints WHERE checkpoint_id=?1",
                    params![&checkpoint.checkpoint_id],
                    row_to_checkpoint,
                )
                .map_err(|err| format!("load recovery checkpoint failed: {err}"))?;
            if existing != *checkpoint {
                return Err(format!(
                    "recovery_checkpoint_idempotency_conflict: checkpointId={}",
                    checkpoint.checkpoint_id
                ));
            }
        }
        return Ok(());
    }
    conn.execute(
        "
        INSERT INTO checkpoints(
            checkpoint_id, kind, session_id, turn_id, status, done_reason,
            updated_at_ms, payload_json
        )
        VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
        ON CONFLICT(checkpoint_id) DO UPDATE SET
            kind = excluded.kind,
            session_id = excluded.session_id,
            turn_id = excluded.turn_id,
            status = excluded.status,
            done_reason = excluded.done_reason,
            updated_at_ms = excluded.updated_at_ms,
            payload_json = excluded.payload_json
        ",
        params![
            &checkpoint.checkpoint_id,
            checkpoint.kind.as_str(),
            &checkpoint.session_id,
            &checkpoint.turn_id,
            &checkpoint.status,
            &checkpoint.done_reason,
            checkpoint.updated_at_ms,
            &checkpoint.payload_json,
        ],
    )
    .map_err(|err| format!("save checkpoint failed: {err}"))?;
    conn.execute(
        "DELETE FROM runtime_job_waiters WHERE checkpoint_id=?1",
        params![&checkpoint.checkpoint_id],
    )
    .map_err(|error| format!("replace checkpoint waiters failed: {error}"))?;
    for waiter in waiters {
        conn.execute("INSERT INTO runtime_job_waiters(checkpoint_id,tool_call_id,source_job_id,source_job_kind,session_id,agent_run_id) VALUES(?1,?2,?3,?4,?5,?6)",
            params![&waiter.cursor.checkpoint_id, &waiter.cursor.tool_call_id, &waiter.source_job_id, &waiter.source_job_kind, &waiter.session_id, &waiter.agent_run_id])
            .map_err(|error| format!("save checkpoint waiter failed: {error}"))?;
    }
    Ok(())
}

pub(super) fn append_event_conn(
    conn: &rusqlite::Connection,
    event: &RuntimeEvent,
) -> Result<(), String> {
    conn.execute(
        "
        INSERT INTO runtime_events(
            event_id, session_id, task_id,
            event_type, at_ms, visibility, payload_json
        )
        VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)
        ",
        params![
            event.event_id.as_str(),
            event.session_id.as_str(),
            event.task_id.as_deref(),
            event.event_type.as_str(),
            event.at_ms,
            visibility_to_db(&event.visibility),
            event.payload_json.as_str(),
        ],
    )
    .map_err(|err| format!("append runtime event failed: {err}"))?;
    Ok(())
}

pub(super) fn append_event_conn_idempotent(
    conn: &rusqlite::Connection,
    event: &RuntimeEvent,
) -> Result<bool, String> {
    let inserted = conn
        .execute(
            "
            INSERT OR IGNORE INTO runtime_events(
                event_id, session_id, task_id,
                event_type, at_ms, visibility, payload_json
            )
            VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)
            ",
            params![
                event.event_id.as_str(),
                event.session_id.as_str(),
                event.task_id.as_deref(),
                event.event_type.as_str(),
                event.at_ms,
                visibility_to_db(&event.visibility),
                event.payload_json.as_str(),
            ],
        )
        .map_err(|error| format!("append idempotent runtime event failed: {error}"))?;
    if inserted == 1 {
        return Ok(true);
    }
    let existing = conn
        .query_row(
            "SELECT event_id, session_id, task_id, event_type, at_ms, visibility, payload_json FROM runtime_events WHERE event_id = ?1",
            params![event.event_id.as_str()],
            row_to_event,
        )
        .optional()
        .map_err(|error| format!("load idempotent runtime event failed: {error}"))?
        .ok_or_else(|| format!("idempotent runtime event disappeared: {}", event.event_id))?;
    if existing != *event {
        return Err(format!(
            "runtime_event_idempotency_conflict: eventId={}",
            event.event_id
        ));
    }
    Ok(false)
}

pub(super) fn row_to_checkpoint(row: &rusqlite::Row<'_>) -> rusqlite::Result<CheckpointRecord> {
    let kind = CheckpointKindV1::parse(row.get::<_, String>(1)?.as_str()).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            1,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, error)),
        )
    })?;
    Ok(CheckpointRecord {
        checkpoint_id: row.get(0)?,
        kind,
        session_id: row.get(2)?,
        turn_id: row.get(3)?,
        status: row.get(4)?,
        done_reason: row.get(5)?,
        updated_at_ms: row.get(6)?,
        payload_json: row.get(7)?,
    })
}

pub(super) fn row_to_event(row: &rusqlite::Row<'_>) -> rusqlite::Result<RuntimeEvent> {
    let visibility_raw: String = row.get(5)?;
    let visibility = visibility_from_db(&visibility_raw).map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(
            5,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, err)),
        )
    })?;

    Ok(RuntimeEvent {
        event_id: row.get(0)?,
        session_id: row.get(1)?,
        task_id: row.get(2)?,
        event_type: row.get(3)?,
        at_ms: row.get(4)?,
        visibility,
        payload_json: row.get(6)?,
    })
}
