//! Bounded Host catalog queries. Cursors bind scope and index generation.
use super::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Request {
    pub mode: String,
    pub cwd: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<usize>,
    pub session_id: Option<String>,
}

pub(crate) struct Page {
    pub items: Vec<SessionFileItem>,
    pub deleted_ids: Vec<String>,
    pub revision: String,
    pub next_cursor: Option<String>,
    pub reset: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Cursor {
    generation: String,
    revision: i64,
    mode: String,
    cwd: Option<String>,
    key: Option<(i64, String)>,
}

pub(super) fn install(conn: &mut Connection) -> Result<(), String> {
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    tx.execute_batch("CREATE TABLE catalog_clock(id INTEGER PRIMARY KEY CHECK(id=1), revision INTEGER NOT NULL, generation TEXT NOT NULL);
        INSERT INTO catalog_clock VALUES(1,0,lower(hex(randomblob(16))));
        CREATE TABLE catalog_entries(id TEXT PRIMARY KEY, revision INTEGER NOT NULL, summary TEXT, cwd TEXT, pinned INTEGER, updated INTEGER, ordering INTEGER, kind TEXT);
        CREATE INDEX catalog_revision ON catalog_entries(revision);
        CREATE INDEX catalog_recent ON catalog_entries(kind,pinned,ordering,id);
        CREATE INDEX catalog_workspace ON catalog_entries(cwd,kind,pinned,ordering,id);
        CREATE INDEX catalog_workspace_pinned ON catalog_entries(cwd,kind,pinned,ordering,id);
        CREATE INDEX catalog_pinned ON catalog_entries(kind,pinned,ordering,id);
        PRAGMA user_version=2;").map_err(|e| e.to_string())?;
    let summaries: Vec<(String, Option<String>)> = {
        let mut stmt = tx
            .prepare("SELECT session_id,CASE WHEN count(*)=1 THEN summary ELSE NULL END FROM sources GROUP BY session_id")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(|e| e.to_string())?
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?;
        rows
    };
    for (id, summary) in summaries {
        publish(&tx, &id, summary.as_deref())?;
    }
    tx.commit().map_err(|e| e.to_string())
}

fn cwd_key(value: &str) -> String {
    let path = value.trim_start_matches("\\\\?\\").replace('\\', "/");
    let path = path.trim_end_matches('/');
    if cfg!(windows) {
        path.to_lowercase()
    } else {
        path.to_string()
    }
}

pub(super) fn publish(conn: &Connection, id: &str, summary: Option<&str>) -> Result<(), String> {
    let old: Option<Option<String>> = conn
        .query_row(
            "SELECT summary FROM catalog_entries WHERE id=?1",
            [id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    if old.as_ref().map(|s| s.as_deref()) == Some(summary) {
        return Ok(());
    }
    let item = summary
        .map(serde_json::from_str::<SessionFileItem>)
        .transpose()
        .map_err(|e| e.to_string())?;
    conn.execute(
        "UPDATE catalog_clock SET revision=revision+1 WHERE id=1",
        [],
    )
    .map_err(|e| e.to_string())?;
    conn.execute("INSERT INTO catalog_entries VALUES(?1,(SELECT revision FROM catalog_clock),?2,?3,?4,?5,?6,?7)
        ON CONFLICT(id) DO UPDATE SET revision=excluded.revision,summary=excluded.summary,cwd=excluded.cwd,pinned=excluded.pinned,updated=excluded.updated,ordering=excluded.ordering,kind=excluded.kind",
        params![id,summary,item.as_ref().map(|i|cwd_key(&i.cwd)),item.as_ref().map(|i|i.is_pinned),item.as_ref().map(|i|i.updated_at),item.as_ref().map(|i|if i.is_pinned { i.sort_order.unwrap_or(-i.updated_at) } else { -i.updated_at }),item.as_ref().map(|i|&i.session_kind)]).map_err(|e|e.to_string())?;
    conn.execute("DELETE FROM catalog_entries WHERE summary IS NULL AND revision < (SELECT revision-4096 FROM catalog_clock)", []).map_err(|e|e.to_string())?;
    Ok(())
}

pub(crate) fn query(root: &Path, request: Request) -> Result<Page, String> {
    let limit = request.limit.unwrap_or(50);
    if !(1..=100).contains(&limit) {
        return Err("catalog limit must be between 1 and 100".into());
    }
    if !matches!(
        request.mode.as_str(),
        "recent" | "pinned" | "changes" | "lookup"
    ) {
        return Err("invalid catalog mode".into());
    }
    if request.mode == "lookup" && request.session_id.as_deref().is_none_or(str::is_empty) {
        return Err("lookup requires sessionId".into());
    }
    if request.mode != "lookup" && request.session_id.is_some() {
        return Err("sessionId only valid for lookup".into());
    }
    if matches!(request.mode.as_str(), "changes" | "lookup") && request.cwd.is_some() {
        return Err("cwd only valid for recent or pinned".into());
    }
    let cursor: Option<Cursor> = request
        .cursor
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(|e| format!("invalid catalog cursor: {e}"))?;
    let cwd = request.cwd.as_deref().map(cwd_key);
    let _source = message_log::lock_session_logs_for_read()?;
    with_index(root, |conn| {
        refresh(conn, root, request.session_id.as_deref())?;
        let (revision, generation): (i64, String) = conn
            .query_row("SELECT revision,generation FROM catalog_clock", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .map_err(|e| e.to_string())?;
        let encode = |mode: &str, revision, key| {
            serde_json::to_string(&Cursor {
                generation: generation.clone(),
                revision,
                mode: mode.into(),
                cwd: if mode == "changes" { None } else { cwd.clone() },
                key,
            })
            .map_err(|e| e.to_string())
        };
        let mut page = Page {
            items: vec![],
            deleted_ids: vec![],
            revision: encode("changes", revision, None)?,
            next_cursor: None,
            reset: false,
        };
        if let Some(c) = &cursor {
            if c.mode != request.mode || c.cwd != cwd {
                return Err("catalog cursor scope mismatch".into());
            }
            if c.generation != generation
                || c.revision > revision
                || (request.mode == "changes" && c.revision < revision - 4096)
                || (request.mode != "changes" && c.revision != revision)
            {
                page.reset = true;
                return Ok(page);
            }
        }
        if request.mode == "changes" && cursor.is_none() {
            return Ok(page);
        }
        let mut sql =
            String::from("SELECT id,summary,revision,ordering FROM catalog_entries WHERE ");
        let mut args: Vec<rusqlite::types::Value> = vec![];
        match request.mode.as_str() {
            "changes" => {
                sql.push_str("revision > ? ORDER BY revision");
                args.push(cursor.as_ref().unwrap().revision.into());
            }
            "lookup" => {
                sql.push_str("id=? AND summary IS NOT NULL");
                args.push(request.session_id.clone().unwrap().into());
            }
            mode => {
                sql.push_str("summary IS NOT NULL AND kind='main' AND pinned=?");
                args.push((if mode == "pinned" { 1 } else { 0 }).into());
                if let Some(cwd) = &cwd {
                    sql.push_str(" AND cwd=?");
                    args.push(cwd.clone().into());
                }
                if let Some((key, id)) = cursor.as_ref().and_then(|c| c.key.as_ref()) {
                    sql.push_str(" AND (ordering,id) > (?,?)");
                    args.extend([(*key).into(), id.clone().into()]);
                }
                sql.push_str(" ORDER BY ordering,id");
            }
        }
        sql.push_str(" LIMIT ?");
        args.push(((limit + 1) as i64).into());
        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        let mut rows = stmt
            .query_map(rusqlite::params_from_iter(args), |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, Option<i64>>(3)?,
                ))
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        let more = rows.len() > limit;
        rows.truncate(limit);
        if more {
            let (id, _, rev, key) = rows.last().unwrap();
            page.next_cursor = Some(encode(
                &request.mode,
                if request.mode == "changes" {
                    *rev
                } else {
                    revision
                },
                if request.mode == "changes" {
                    None
                } else {
                    Some((key.unwrap(), id.clone()))
                },
            )?);
        }
        if request.mode == "changes" && more {
            page.revision = page.next_cursor.clone().unwrap();
        }
        for (id, json, _, _) in rows {
            if let Some(json) = json {
                page.items
                    .push(serde_json::from_str(&json).map_err(|e| e.to_string())?);
            } else {
                page.deleted_ids.push(id);
            }
        }
        Ok(page)
    })
}
