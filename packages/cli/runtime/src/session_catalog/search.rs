//! Rebuildable full-text index of visible conversation text.
use super::{message_log, paging, source_paths, with_index, SessionFileItem};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::path::{Path, PathBuf};

const MATCH_START: &str = "\u{1}";
const MATCH_END: &str = "\u{2}";
// Default FTS5 tokenization keeps consecutive Han characters together. This
// invisible separator makes each Han character a token, including inside a
// longer sentence, without changing the visible snippet boundaries.
const HAN_SEPARATOR: char = '\u{2063}';

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Request {
    pub(crate) query: String,
    pub(crate) cwd: Option<String>,
    pub(crate) limit: Option<usize>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Response {
    items: Vec<Hit>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Hit {
    id: String,
    title: String,
    cwd: String,
    updated_at: i64,
    is_pinned: bool,
    snippet: Option<String>,
}

pub(super) fn install(conn: &mut Connection) -> Result<(), String> {
    let tx = conn.transaction().map_err(|error| error.to_string())?;
    tx.execute_batch(
        "CREATE TABLE session_search_dirty (session_id TEXT PRIMARY KEY);
         INSERT INTO session_search_dirty SELECT DISTINCT session_id FROM sources;
         CREATE TABLE session_search_documents (
             session_id TEXT NOT NULL UNIQUE,
             title TEXT NOT NULL,
             body TEXT NOT NULL,
             cwd TEXT NOT NULL,
             cwd_key TEXT NOT NULL,
             updated_at INTEGER NOT NULL,
             is_pinned INTEGER NOT NULL
         );
         CREATE VIRTUAL TABLE session_search_fts USING fts5(
             title, body, content='session_search_documents'
         );
         CREATE TRIGGER session_search_documents_insert AFTER INSERT ON session_search_documents BEGIN
             INSERT INTO session_search_fts(rowid, title, body)
             VALUES (new.rowid, new.title, new.body);
         END;
         CREATE TRIGGER session_search_documents_delete AFTER DELETE ON session_search_documents BEGIN
             INSERT INTO session_search_fts(session_search_fts, rowid, title, body)
             VALUES ('delete', old.rowid, old.title, old.body);
         END;
         CREATE TRIGGER session_search_documents_update
             AFTER UPDATE OF title, body ON session_search_documents
             WHEN old.title IS NOT new.title OR old.body IS NOT new.body BEGIN
             INSERT INTO session_search_fts(session_search_fts, rowid, title, body)
             VALUES ('delete', old.rowid, old.title, old.body);
             INSERT INTO session_search_fts(rowid, title, body)
             VALUES (new.rowid, new.title, new.body);
         END;
         PRAGMA user_version=3;",
    )
    .map_err(|error| error.to_string())?;
    tx.commit().map_err(|error| error.to_string())
}

pub(crate) fn query(root: &Path, request: Request) -> Result<Response, String> {
    let limit = request.limit.unwrap_or(20);
    if !(1..=50).contains(&limit) {
        return Err("session search limit must be between 1 and 50".into());
    }
    let terms: Vec<_> = request
        .query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|term| !term.is_empty())
        .map(|term| format!("\"{}\"*", index_text(term)))
        .collect();
    if terms.is_empty() {
        return Ok(Response { items: Vec::new() });
    }
    let cwd = request.cwd.as_deref().map(paging::cwd_key);
    refresh_documents(root)?;
    with_index(root, |conn| {
        let mut items = matches(conn, &terms.join(" AND "), cwd.as_deref(), limit)?;
        if items.is_empty() && terms.len() > 1 {
            items = matches(conn, &terms.join(" OR "), cwd.as_deref(), limit)?;
        }
        Ok(Response { items })
    })
}

fn refresh_documents(root: &Path) -> Result<(), String> {
    let dirty_ids: Vec<String> = with_index(root, |conn| {
        let mut stmt = conn
            .prepare("SELECT session_id FROM session_search_dirty ORDER BY session_id")
            .map_err(|error| error.to_string())?;
        let rows = stmt
            .query_map([], |row| row.get(0))
            .map_err(|error| error.to_string())?;
        rows.collect::<Result<_, _>>()
            .map_err(|error| error.to_string())
    })?;
    for id in dirty_ids {
        let _source = message_log::lock_session_logs_for_read()?;
        // Release the catalog locks while hydrating source text. The source
        // guard stabilizes the candidate until the replacement is committed.
        let pending = with_index(root, |conn| {
            let dirty: bool = conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM session_search_dirty WHERE session_id=?1)",
                    [&id],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())?;
            if dirty {
                source_paths(conn, &id).map(Some)
            } else {
                Ok(None)
            }
        })?;
        let Some(paths) = pending else { continue };
        let projected = project_document(root, &id, paths)?;
        with_index(root, |conn| publish(conn, &id, projected.as_ref()))?;
    }
    Ok(())
}

fn project_document(
    root: &Path,
    id: &str,
    paths: Vec<String>,
) -> Result<Option<(SessionFileItem, String)>, String> {
    let mut existing: Option<PathBuf> = None;
    for relative in paths {
        let path = root.join(relative);
        if path.try_exists().map_err(|error| error.to_string())? && existing.replace(path).is_some()
        {
            // Catalog lookups also hide ambiguous source identities.
            return Ok(None);
        }
    }
    let Some(path) = existing else {
        return Ok(None);
    };
    match message_log::read_state::catalog_search_projection(&path, root) {
        Ok(document) => Ok(document.filter(|(item, _)| item.id == id)),
        Err(error) => {
            // An invalid history cannot supply a search result for this ID.
            eprintln!(
                "session_search_source_invalid: path={} error={error}",
                path.display()
            );
            Ok(None)
        }
    }
}

fn publish(
    conn: &mut Connection,
    id: &str,
    projected: Option<&(SessionFileItem, String)>,
) -> Result<(), String> {
    let tx = conn.transaction().map_err(|error| error.to_string())?;
    if let Some((item, body)) = projected {
        tx.execute(
            "INSERT INTO session_search_documents(session_id, title, body, cwd, cwd_key, updated_at, is_pinned)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(session_id) DO UPDATE SET
                 title=excluded.title, body=excluded.body, cwd=excluded.cwd,
                 cwd_key=excluded.cwd_key, updated_at=excluded.updated_at,
                 is_pinned=excluded.is_pinned",
            params![
                id, index_text(&item.title).as_ref(), index_text(body).as_ref(), item.cwd,
                paging::cwd_key(&item.cwd), item.updated_at, item.is_pinned,
            ],
        )
        .map_err(|error| error.to_string())?;
    } else {
        tx.execute(
            "DELETE FROM session_search_documents WHERE session_id=?1",
            [id],
        )
        .map_err(|error| error.to_string())?;
    }
    tx.execute("DELETE FROM session_search_dirty WHERE session_id=?1", [id])
        .map_err(|error| error.to_string())?;
    tx.commit().map_err(|error| error.to_string())
}

fn matches(
    conn: &Connection,
    expression: &str,
    cwd: Option<&str>,
    limit: usize,
) -> Result<Vec<Hit>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT d.session_id, d.title, d.cwd, d.updated_at, d.is_pinned,
                    snippet(session_search_fts, 1, ?4, ?5, '…', 18)
             FROM session_search_fts JOIN session_search_documents d ON d.rowid=session_search_fts.rowid
             WHERE session_search_fts MATCH ?1
               AND (?2 IS NULL OR d.cwd_key=?2)
               AND NOT EXISTS (SELECT 1 FROM session_search_dirty b WHERE b.session_id=d.session_id)
             ORDER BY bm25(session_search_fts, 10.0, 1.0), d.updated_at DESC, d.session_id
             LIMIT ?3",
        )
        .map_err(|error| error.to_string())?;
    let rows = stmt
        .query_map(
            params![expression, cwd, limit, MATCH_START, MATCH_END],
            |row| {
                let title: String = row.get(1)?;
                let snippet: Option<String> = row.get(5)?;
                Ok(Hit {
                    id: row.get(0)?,
                    title: title.replace(HAN_SEPARATOR, ""),
                    cwd: row.get(2)?,
                    updated_at: row.get(3)?,
                    is_pinned: row.get(4)?,
                    snippet: snippet.as_deref().and_then(visible_snippet),
                })
            },
        )
        .map_err(|error| error.to_string())?;
    rows.collect::<Result<_, _>>()
        .map_err(|error| error.to_string())
}

fn visible_snippet(marked: &str) -> Option<String> {
    let (before, rest) = marked.split_once(MATCH_START)?;
    let (matched, after) = rest.split_once(MATCH_END)?;
    let plain = |text: &str| {
        text.chars()
            .filter(|ch| !matches!(*ch, HAN_SEPARATOR | '\u{1}' | '\u{2}'))
            .map(|ch| if ch.is_control() { ' ' } else { ch })
            .collect::<String>()
    };
    let before = plain(before);
    let matched = plain(matched);
    let after = plain(after);
    let mut snippet = String::new();
    if let Some((index, _)) = before
        .char_indices()
        .rev()
        .nth(7)
        .filter(|(index, _)| *index > 0)
    {
        snippet.push('…');
        snippet.push_str(&before[index..]);
    } else {
        snippet.push_str(&before);
    }
    snippet.push_str(&matched);
    if let Some((index, _)) = after.char_indices().nth(48) {
        snippet.push_str(&after[..index]);
        snippet.push('…');
    } else {
        snippet.push_str(&after);
    }
    Some(snippet.split_whitespace().collect::<Vec<_>>().join(" "))
}

#[cfg(test)]
#[test]
fn body_snippet_keeps_match_after_an_unrelated_line() {
    let snippet = visible_snippet(
        "Prepare a small local fixture.\nThe \u{1}body\u{2}-only-needle was found.",
    )
    .unwrap();
    assert!(snippet.contains("body-only-needle"));
    assert!(!snippet.contains('\n'));
    assert!(!snippet.contains("Prepare"));
}

fn is_han(ch: char) -> bool {
    matches!(
        ch as u32,
        0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x20000..=0x2FA1F | 0x30000..=0x323AF
    )
}

fn index_text(text: &str) -> Cow<'_, str> {
    if !text
        .chars()
        .any(|ch| is_han(ch) || matches!(ch, '\u{1}' | '\u{2}'))
    {
        return Cow::Borrowed(text);
    }
    let mut indexed = String::with_capacity(text.len());
    for ch in text.chars() {
        if is_han(ch) {
            indexed.extend([HAN_SEPARATOR, ch, HAN_SEPARATOR]);
        } else if !matches!(ch, '\u{1}' | '\u{2}') {
            indexed.push(ch);
        }
    }
    Cow::Owned(indexed)
}
