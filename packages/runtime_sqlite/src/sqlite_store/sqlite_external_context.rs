use super::*;
use centaeris_core::session::external_context::{
    ExternalContextObject, ExternalContextObjectIndexEntry, ExternalContextObjectLink,
    ExternalContextStorePort, ListExternalContextObjectsRequest,
};
use rusqlite::{params, OptionalExtension};
use sha2::{Digest, Sha256};
use std::io::{Cursor, Read};

const EXTERNAL_CONTEXT_ZSTD_LEVEL: i32 = 3;
const EXTERNAL_CONTEXT_ZSTD_MIN_BYTES: usize = 16 * 1024;
const CONTENT_CODEC_IDENTITY_V1: &str = "identity_v1";
const CONTENT_CODEC_ZSTD_V1: &str = "zstd_v1";

struct StoredExternalContextObject {
    schema_version: String,
    object_id: String,
    object_kind: String,
    source_provider_id: String,
    source_tool_name: String,
    title: String,
    content: Vec<u8>,
    content_codec: String,
    content_uncompressed_bytes: i64,
    content_sha256: String,
    metadata_json: String,
    updated_at_ms: i64,
}

impl ExternalContextStorePort for SqliteRuntimeStore {
    fn upsert_external_context_object(&self, object: ExternalContextObject) -> Result<(), String> {
        self.with_conn(|conn| upsert_external_context_object_conn(conn, &object))
    }

    fn load_external_context_object(
        &self,
        object_id: &str,
    ) -> Result<Option<ExternalContextObject>, String> {
        let object_id = object_id.trim();
        if object_id.is_empty() {
            return Err("external context object_id is required".to_string());
        }
        self.with_conn(|conn| {
            let stored = conn
                .query_row(
                    "
                SELECT schema_version, object_id, object_kind, source_provider_id, source_tool_name,
                       title, content, content_codec, content_uncompressed_bytes,
                       content_sha256, metadata_json, updated_at_ms
                FROM external_context_objects
                WHERE object_id = ?1
                ",
                    params![object_id],
                    row_to_stored_external_context_object,
                )
                .optional()
                .map_err(|err| format!("load external context object failed: {err}"))?;
            stored
                .map(decode_stored_external_context_object)
                .transpose()
                .map_err(|err| format!("load external context object failed: {err}"))
        })
    }

    fn link_external_context_object(&self, link: ExternalContextObjectLink) -> Result<(), String> {
        let session_id = link.session_id.trim();
        if session_id.is_empty() {
            return Err("external context link session_id is required".to_string());
        }
        let object_id = link.object_id.trim();
        if object_id.is_empty() {
            return Err("external context link object_id is required".to_string());
        }
        self.with_conn(|conn| link_external_context_object_conn(conn, &link))
    }

    fn load_external_context_object_link(
        &self,
        session_id: &str,
        object_id: &str,
        turn_id: &str,
        tool_call_id: &str,
    ) -> Result<Option<ExternalContextObjectLink>, String> {
        if [session_id, object_id, turn_id, tool_call_id]
            .iter()
            .any(|value| value.trim().is_empty())
        {
            return Err("external context link identity is required".to_string());
        }
        self.with_conn(|conn| {
            conn.query_row(
                "SELECT session_id,turn_id,tool_call_id,object_id,source_provider_id,source_tool_name,linked_at_ms FROM external_context_links WHERE session_id=?1 AND object_id=?2 AND turn_id=?3 AND tool_call_id=?4",
                params![session_id, object_id, turn_id, tool_call_id],
                |row| {
                    Ok(ExternalContextObjectLink {
                        session_id: row.get(0)?,
                        turn_id: Some(row.get(1)?),
                        tool_call_id: Some(row.get(2)?),
                        object_id: row.get(3)?,
                        source_provider_id: row.get(4)?,
                        source_tool_name: row.get(5)?,
                        linked_at_ms: row.get(6)?,
                    })
                },
            )
            .optional()
            .map_err(|error| format!("load external context object link failed: {error}"))
        })
    }

    fn list_external_context_objects(
        &self,
        req: ListExternalContextObjectsRequest,
    ) -> Result<Vec<ExternalContextObjectIndexEntry>, String> {
        let limit = req.limit.clamp(1, 128);
        let offset = req.offset;
        self.with_conn(|conn| {
            let limit_i64 = to_i64(limit)?;
            let offset_i64 = to_i64(offset)?;
            if let Some(session_id) = req.session_id.as_ref() {
                let mut stmt = conn
                    .prepare(
                        "
                        SELECT obj.object_id, obj.object_kind, obj.source_provider_id,
                               obj.source_tool_name, obj.title, obj.updated_at_ms,
                               COUNT(link.object_id) AS link_count,
                               MAX(link.linked_at_ms) AS last_linked_at_ms
                        FROM external_context_links link
                        JOIN external_context_objects obj ON obj.object_id = link.object_id
                        WHERE link.session_id = ?1
                        GROUP BY obj.object_id, obj.object_kind, obj.source_provider_id,
                                 obj.source_tool_name, obj.title, obj.updated_at_ms
                        ORDER BY last_linked_at_ms DESC, obj.object_id ASC
                        LIMIT ?2 OFFSET ?3
                        ",
                    )
                    .map_err(|err| format!("prepare list linked external context objects failed: {err}"))?;
                let rows = stmt
                    .query_map(params![session_id.as_str(), limit_i64, offset_i64], |row| {
                        row_to_external_context_index_entry(row)
                    })
                    .map_err(|err| format!("query linked external context objects failed: {err}"))?;
                let mut entries = vec![];
                for row in rows {
                    entries.push(row.map_err(|err| {
                        format!("decode linked external context object failed: {err}")
                    })?);
                }
                return Ok(entries);
            }

            let mut stmt = conn
                .prepare(
                    "
                    SELECT obj.object_id, obj.object_kind, obj.source_provider_id,
                           obj.source_tool_name, obj.title, obj.updated_at_ms,
                           COALESCE(link_stats.link_count, 0) AS link_count,
                           link_stats.last_linked_at_ms
                    FROM external_context_objects obj
                    LEFT JOIN (
                        SELECT object_id, COUNT(*) AS link_count, MAX(linked_at_ms) AS last_linked_at_ms
                        FROM external_context_links
                        GROUP BY object_id
                    ) link_stats ON link_stats.object_id = obj.object_id
                    ORDER BY obj.updated_at_ms DESC, obj.object_id ASC
                    LIMIT ?1 OFFSET ?2
                    ",
                )
                .map_err(|err| format!("prepare list external context objects failed: {err}"))?;
            let rows = stmt
                .query_map(params![limit_i64, offset_i64], |row| {
                    row_to_external_context_index_entry(row)
                })
                .map_err(|err| format!("query external context objects failed: {err}"))?;
            let mut entries = vec![];
            for row in rows {
                entries.push(
                    row.map_err(|err| format!("decode external context object failed: {err}"))?,
                );
            }
            Ok(entries)
        })
    }
}

pub(super) fn upsert_external_context_object_conn(
    conn: &rusqlite::Connection,
    object: &ExternalContextObject,
) -> Result<(), String> {
    let metadata_json = serde_json::to_string(&object.metadata)
        .map_err(|err| format!("serialize external context metadata failed: {err}"))?;
    let (content, content_codec, content_uncompressed_bytes, content_sha256) =
        encode_external_context_content(object.content.as_str())?;
    conn.execute(
        "
        INSERT INTO external_context_objects (
            object_id, schema_version, object_kind, source_provider_id, source_tool_name,
            title, content, content_codec, content_uncompressed_bytes, content_sha256,
            metadata_json, updated_at_ms, inserted_at_ms
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?12)
        ON CONFLICT(object_id) DO UPDATE SET
            schema_version = excluded.schema_version,
            object_kind = excluded.object_kind,
            source_provider_id = excluded.source_provider_id,
            source_tool_name = excluded.source_tool_name,
            title = excluded.title,
            content = excluded.content,
            content_codec = excluded.content_codec,
            content_uncompressed_bytes = excluded.content_uncompressed_bytes,
            content_sha256 = excluded.content_sha256,
            metadata_json = excluded.metadata_json,
            updated_at_ms = excluded.updated_at_ms
        ",
        params![
            object.object_id.as_str(),
            object.schema_version.as_str(),
            object.object_kind.as_str(),
            object.source_provider_id.as_str(),
            object.source_tool_name.as_str(),
            object.title.as_str(),
            content,
            content_codec,
            content_uncompressed_bytes,
            content_sha256,
            metadata_json.as_str(),
            object.updated_at_ms,
        ],
    )
    .map_err(|err| format!("upsert external context object failed: {err}"))?;
    Ok(())
}

pub(super) fn link_external_context_object_conn(
    conn: &rusqlite::Connection,
    link: &ExternalContextObjectLink,
) -> Result<(), String> {
    let session_id = link.session_id.trim();
    if session_id.is_empty() {
        return Err("external context link session_id is required".to_string());
    }
    let object_id = link.object_id.trim();
    if object_id.is_empty() {
        return Err("external context link object_id is required".to_string());
    }
    let exists = conn
        .query_row(
            "SELECT 1 FROM external_context_objects WHERE object_id = ?1",
            params![object_id],
            |_| Ok(()),
        )
        .optional()
        .map_err(|err| format!("check external context object link target failed: {err}"))?;
    if exists.is_none() {
        return Err(format!(
            "external context object not found for link: {object_id}"
        ));
    }
    conn.execute(
        "
        INSERT INTO external_context_links (
            session_id, object_id, turn_id, tool_call_id,
            source_provider_id, source_tool_name, linked_at_ms
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
        ON CONFLICT(session_id, object_id, turn_id, tool_call_id) DO UPDATE SET
            source_provider_id = excluded.source_provider_id,
            source_tool_name = excluded.source_tool_name,
            linked_at_ms = excluded.linked_at_ms
        ",
        params![
            session_id,
            object_id,
            link.turn_id.clone().unwrap_or_default(),
            link.tool_call_id.clone().unwrap_or_default(),
            link.source_provider_id.trim(),
            link.source_tool_name.trim(),
            link.linked_at_ms,
        ],
    )
    .map_err(|err| format!("link external context object failed: {err}"))?;
    Ok(())
}

fn row_to_stored_external_context_object(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<StoredExternalContextObject> {
    Ok(StoredExternalContextObject {
        schema_version: row.get(0)?,
        object_id: row.get(1)?,
        object_kind: row.get(2)?,
        source_provider_id: row.get(3)?,
        source_tool_name: row.get(4)?,
        title: row.get(5)?,
        content: row.get(6)?,
        content_codec: row.get(7)?,
        content_uncompressed_bytes: row.get(8)?,
        content_sha256: row.get(9)?,
        metadata_json: row.get(10)?,
        updated_at_ms: row.get(11)?,
    })
}

fn decode_stored_external_context_object(
    stored: StoredExternalContextObject,
) -> Result<ExternalContextObject, String> {
    let content = decode_external_context_content(
        stored.content,
        stored.content_codec.as_str(),
        stored.content_uncompressed_bytes,
        stored.content_sha256.as_str(),
    )?;
    let metadata = serde_json::from_str(stored.metadata_json.as_str())
        .map_err(|err| format!("decode external context metadata failed: {err}"))?;
    Ok(ExternalContextObject {
        schema_version: stored.schema_version,
        object_id: stored.object_id,
        object_kind: stored.object_kind,
        source_provider_id: stored.source_provider_id,
        source_tool_name: stored.source_tool_name,
        title: stored.title,
        content,
        metadata,
        updated_at_ms: stored.updated_at_ms,
    })
}

pub(super) fn encode_external_context_content(
    content: &str,
) -> Result<(Vec<u8>, &'static str, i64, String), String> {
    let bytes = content.as_bytes();
    let uncompressed_bytes = i64::try_from(bytes.len())
        .map_err(|_| "external context content exceeds sqlite size range".to_string())?;
    let sha256 = content_sha256(bytes);
    if bytes.len() < EXTERNAL_CONTEXT_ZSTD_MIN_BYTES {
        return Ok((
            bytes.to_vec(),
            CONTENT_CODEC_IDENTITY_V1,
            uncompressed_bytes,
            sha256,
        ));
    }
    let compressed = zstd::stream::encode_all(Cursor::new(bytes), EXTERNAL_CONTEXT_ZSTD_LEVEL)
        .map_err(|err| format!("compress external context content failed: {err}"))?;
    if compressed.len() >= bytes.len() {
        return Ok((
            bytes.to_vec(),
            CONTENT_CODEC_IDENTITY_V1,
            uncompressed_bytes,
            sha256,
        ));
    }
    Ok((
        compressed,
        CONTENT_CODEC_ZSTD_V1,
        uncompressed_bytes,
        sha256,
    ))
}

fn decode_external_context_content(
    content: Vec<u8>,
    content_codec: &str,
    expected_uncompressed_bytes: i64,
    expected_sha256: &str,
) -> Result<String, String> {
    let expected_uncompressed_bytes = u64::try_from(expected_uncompressed_bytes)
        .map_err(|_| "external context content uncompressed size is invalid".to_string())?;
    let bytes = match content_codec {
        CONTENT_CODEC_IDENTITY_V1 => content,
        CONTENT_CODEC_ZSTD_V1 => {
            let decoder = zstd::stream::read::Decoder::new(Cursor::new(content))
                .map_err(|err| format!("decompress external context content failed: {err}"))?;
            let mut bytes = Vec::new();
            decoder
                .take(expected_uncompressed_bytes.saturating_add(1))
                .read_to_end(&mut bytes)
                .map_err(|err| format!("decompress external context content failed: {err}"))?;
            bytes
        }
        codec => {
            return Err(format!(
                "unsupported external context content codec: {codec}"
            ))
        }
    };
    if bytes.len() as u64 != expected_uncompressed_bytes {
        return Err(format!(
            "external context content size mismatch: expected {expected_uncompressed_bytes}, got {}",
            bytes.len()
        ));
    }
    let actual_sha256 = content_sha256(bytes.as_slice());
    if actual_sha256 != expected_sha256 {
        return Err(format!(
            "external context content sha256 mismatch: expected {expected_sha256}, got {actual_sha256}"
        ));
    }
    String::from_utf8(bytes).map_err(|err| format!("external context content is not UTF-8: {err}"))
}

fn content_sha256(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn row_to_external_context_index_entry(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<ExternalContextObjectIndexEntry> {
    let link_count_i64: i64 = row.get(6)?;
    Ok(ExternalContextObjectIndexEntry {
        object_id: row.get(0)?,
        object_kind: row.get(1)?,
        source_provider_id: row.get(2)?,
        source_tool_name: row.get(3)?,
        title: row.get(4)?,
        updated_at_ms: row.get(5)?,
        link_count: usize::try_from(link_count_i64).unwrap_or(usize::MAX),
        last_linked_at_ms: row.get(7)?,
    })
}
