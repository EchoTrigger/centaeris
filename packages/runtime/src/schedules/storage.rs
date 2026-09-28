use super::*;
use crate::local_work_store as db;
use rusqlite::{params, Connection};

fn open(path: &Path) -> Result<Connection, String> {
    let mut conn = db::open(&path.with_extension("sqlite3"))?;
    if db::get::<bool>(&conn, "meta", "imported")?.is_none() {
        let store = match fs::read(path) {
            Ok(bytes) => {
                let store: Store = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
                if store.schema_version != 1 {
                    return Err("unsupported schedule schema".into());
                }
                let backup = path.with_extension("json.pre-sqlite.backup");
                if !backup.exists() {
                    crate::atomic_file::write_file_atomically(
                        &backup,
                        &bytes,
                        "schedule migration backup",
                    )?;
                }
                store
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Store::default(),
            Err(e) => return Err(e.to_string()),
        };
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        write(&tx, &store)?;
        db::put(&tx, "meta", "imported", "", "", None, &true)?;
        tx.commit().map_err(|e| e.to_string())?;
    }
    Ok(conn)
}
fn base(conn: &Connection) -> Result<Store, String> {
    Ok(Store {
        service_enabled: db::get(conn, "meta", "enabled")?.unwrap_or(false),
        ..Store::default()
    })
}
fn write(conn: &Connection, store: &Store) -> Result<(), String> {
    db::put(
        conn,
        "meta",
        "enabled",
        "",
        "",
        None,
        &store.service_enabled,
    )?;
    for p in &store.plans {
        db::put(
            conn,
            "plans",
            &p.id,
            "",
            if p.deleted {
                "deleted"
            } else if p.enabled {
                "ready"
            } else {
                "paused"
            },
            p.next_at,
            p,
        )?;
    }
    for r in &store.runs {
        db::put(
            conn,
            "runs",
            &r.id,
            &r.schedule_id,
            if r.active() { "active" } else { "finished" },
            Some(r.scheduled_at),
            r,
        )?;
    }
    Ok(())
}
pub(super) fn save(path: &Path, store: &Store) -> Result<(), String> {
    let mut conn = open(path)?;
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    write(&tx, store)?;
    tx.commit().map_err(|e| e.to_string())
}
pub(super) fn load_work(path: &Path, now: i64) -> Result<Store, String> {
    let conn = open(path)?;
    let mut store = base(&conn)?;
    if store.service_enabled {
        store.plans=db::query(&conn,"SELECT body FROM records WHERE namespace='plans' AND state='ready' AND due<=?1 ORDER BY due,id",[now])?;
    }
    store.runs = db::query(
        &conn,
        "SELECT body FROM records WHERE namespace='runs' AND state='active' ORDER BY due,id",
        [],
    )?;
    Ok(store)
}
pub(super) fn keep_alive(path: &Path) -> Result<bool, String> {
    let conn = open(path)?;
    let enabled = base(&conn)?.service_enabled;
    conn.query_row("SELECT EXISTS(SELECT 1 FROM records WHERE namespace='runs' AND state='active') OR (?1 AND EXISTS(SELECT 1 FROM records WHERE namespace='plans' AND state='ready'))",[enabled],|r|r.get(0)).map_err(|e|e.to_string())
}
pub(super) fn load_request(path: &Path, request: &Request) -> Result<Store, String> {
    let conn = open(path)?;
    let mut store = base(&conn)?;
    let id = match request {
        Request::Create { operation_id, .. } => Some(operation_receipts::deterministic_identity(
            "schedule-",
            "schedule/create",
            operation_id,
        )),
        Request::Update { schedule_id, .. }
        | Request::Pause { schedule_id }
        | Request::Resume { schedule_id }
        | Request::Delete { schedule_id }
        | Request::Run { schedule_id, .. } => Some(schedule_id.clone()),
        _ => None,
    };
    if let Some(id) = id {
        store.plans = db::get::<Plan>(&conn, "plans", &id)?.into_iter().collect();
    }
    if let Request::Run {
        schedule_id,
        operation_id,
    } = request
    {
        store.runs = db::query(
            &conn,
            "SELECT body FROM records WHERE namespace='runs' AND state='active' AND owner=?1",
            [schedule_id],
        )?;
        let id =
            operation_receipts::deterministic_identity("occurrence-", schedule_id, operation_id);
        if !store.runs.iter().any(|r| r.id == id) {
            if let Some(run) = db::get(&conn, "runs", &id)? {
                store.runs.push(run);
            }
        }
    }
    Ok(store)
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Cursor {
    scope: Option<String>,
    id: String,
    due: Option<i64>,
}
pub(super) fn page(
    path: &Path,
    owner: Option<&str>,
    limit: Option<usize>,
    cursor: Option<String>,
) -> Result<Value, String> {
    let limit = limit.unwrap_or(50);
    if !(1..=100).contains(&limit) {
        return Err("schedule page limit must be 1..100".into());
    }
    let cursor: Option<Cursor> = cursor
        .map(|s| serde_json::from_str(&s).map_err(|e| e.to_string()))
        .transpose()?;
    if cursor.as_ref().is_some_and(|c| c.scope.as_deref() != owner) {
        return Err("schedule cursor scope mismatch".into());
    }
    let after = cursor.as_ref().map_or("", |c| c.id.as_str());
    let conn = open(path)?;
    let mut rows: Vec<Value> = if let Some(owner) = owner {
        db::query(&conn,"SELECT body FROM records WHERE namespace='runs' AND owner=?1 AND (due,id)<(?2,?3) ORDER BY due DESC,id DESC LIMIT ?4",params![owner,cursor.as_ref().and_then(|c|c.due).unwrap_or(i64::MAX),cursor.as_ref().map_or("~",|c|c.id.as_str()),limit+1])?
    } else {
        db::query(&conn,"SELECT body FROM records WHERE namespace='plans' AND state!='deleted' AND id>?1 ORDER BY id LIMIT ?2",params![after,limit+1])?
    };
    let more = rows.len() > limit;
    rows.truncate(limit);
    let next = if more {
        Some(
            serde_json::to_string(&Cursor {
                scope: owner.map(str::to_owned),
                due: rows.last().and_then(|r| r["scheduledAt"].as_i64()),
                id: rows.last().unwrap()["id"]
                    .as_str()
                    .ok_or("missing record identity")?
                    .into(),
            })
            .map_err(|e| e.to_string())?,
        )
    } else {
        None
    };
    if owner.is_some() {
        Ok(json!({"runs":rows,"nextCursor":next}))
    } else {
        let total: i64 = conn
            .query_row(
                "SELECT count(*) FROM records WHERE namespace='plans' AND state!='deleted'",
                [],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        Ok(
            json!({"schedules":rows,"nextCursor":next,"total":total,"serviceEnabled":base(&conn)?.service_enabled,"machineAwakeRequired":true}),
        )
    }
}
