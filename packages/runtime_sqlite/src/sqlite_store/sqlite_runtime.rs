use super::*;
use centaeris_core::runtime::contracts::{
    CheckpointKindV1, CheckpointRecord, EventVisibility, RuntimeEvent,
};
use centaeris_core::session::store::{
    AgentRuntimeSnapshotStorePort, RuntimeStore, RuntimeStoreError, SessionDataStorePort,
};
use rusqlite::{params, OptionalExtension};

impl RuntimeStore for SqliteRuntimeStore {
    fn save_checkpoint(&self, checkpoint: CheckpointRecord) -> Result<(), RuntimeStoreError> {
        self.with_conn(|conn| save_checkpoint_conn(conn, &checkpoint))
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
                "checkpoints",
                "dead_letters",
                "resource_claims",
                "runtime_jobs",
                "external_context_links",
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
        #[cfg(test)]
        if self
            .fail_next_session_snapshot_save
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return Err("injected one-shot session runtime snapshot save failure".to_string());
        }
        self.with_conn(|conn| {
            conn.execute(
                "
                INSERT INTO session_runtime_snapshots(
                    session_id, snapshot_json, updated_at_ms
                )
                VALUES(?1, ?2, ?3)
                ON CONFLICT(session_id) DO UPDATE SET
                    snapshot_json = excluded.snapshot_json,
                    updated_at_ms = excluded.updated_at_ms
                ",
                params![session_id, snapshot_json, updated_at_ms],
            )
            .map_err(|err| format!("save session runtime snapshot failed: {err}"))?;
            Ok(())
        })
    }
}

pub(super) fn save_checkpoint_conn(
    conn: &rusqlite::Connection,
    checkpoint: &CheckpointRecord,
) -> Result<(), String> {
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
