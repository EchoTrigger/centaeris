//! Host-private, rebuildable read index. Source logs remain authoritative.
//! Lock order: Session log lock, then catalog lock. Never acquire a log lock here
//! after entering a catalog transaction. Dirty intents commit before source writes.
use crate::{
    message_log,
    session_files::{self, SessionFileItem},
};
use rusqlite::{params, Connection, OptionalExtension};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::Duration,
};

pub(crate) mod paging;

static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
const SCHEMA: &str = "
CREATE TABLE state (id INTEGER PRIMARY KEY CHECK(id=1), initialized INTEGER NOT NULL, runs_imported INTEGER NOT NULL);
INSERT INTO state VALUES(1,0,0);
CREATE TABLE sources (path TEXT PRIMARY KEY, session_id TEXT NOT NULL, summary TEXT, error TEXT, source_high_water INTEGER);
CREATE INDEX sources_session ON sources(session_id);
CREATE TABLE dirty (path TEXT PRIMARY KEY);
CREATE TABLE runs (run_id TEXT NOT NULL, path TEXT NOT NULL, session_id TEXT NOT NULL, status TEXT NOT NULL, started INTEGER NOT NULL, payload TEXT NOT NULL, PRIMARY KEY(run_id,path));
CREATE INDEX runs_session ON runs(session_id,started);
CREATE INDEX runs_status ON runs(started,run_id) WHERE status IN ('running','stalled');
CREATE TABLE run_locations (run_id TEXT NOT NULL, path TEXT NOT NULL, PRIMARY KEY(run_id,path));
PRAGMA user_version=1;
";

fn open(root: &Path) -> Result<Connection, String> {
    let dir = root
        .parent()
        .ok_or("session directory has no parent")?
        .join("runtime");
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let mut conn =
        Connection::open(dir.join("session-catalog.sqlite3")).map_err(|e| e.to_string())?;
    conn.busy_timeout(Duration::from_secs(5))
        .map_err(|e| e.to_string())?;
    conn.execute_batch("PRAGMA synchronous=FULL;")
        .map_err(|e| e.to_string())?;
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    if version == 0 {
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        tx.execute_batch(SCHEMA).map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
    } else if version != 1 && version != 2 {
        return Err(format!("unsupported session catalog version: {version}"));
    }
    if version < 2 {
        paging::install(&mut conn)?;
    }
    Ok(conn)
}

fn enqueue(conn: &Connection, root: &Path, path: &Path) -> Result<(), String> {
    let relative = path
        .strip_prefix(root)
        .map_err(|e| e.to_string())?
        .to_string_lossy()
        .replace('\\', "/");
    let id = path
        .file_stem()
        .and_then(|v| v.to_str())
        .ok_or("invalid Session path")?;
    conn.execute(
        "INSERT INTO sources(path,session_id) VALUES(?1,?2) ON CONFLICT(path) DO NOTHING",
        params![relative, id],
    )
    .map_err(|e| e.to_string())?;
    conn.execute("INSERT OR IGNORE INTO dirty VALUES(?1)", [&relative])
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn initialize(conn: &mut Connection, root: &Path) -> Result<(), String> {
    let ready: bool = conn
        .query_row("SELECT initialized FROM state WHERE id=1", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    if ready {
        return Ok(());
    }
    // Publish the complete work list first. A crash resumes the remaining dirty
    // entries rather than interpreting a partially built index as complete.
    let paths = session_files::session_log_file_paths(root)?;
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    for path in paths {
        enqueue(&tx, root, &path)?;
    }
    tx.execute("UPDATE state SET initialized=1 WHERE id=1", [])
        .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())
}

/// Called under the source write lock, before any source or CAS mutation.
pub(crate) fn mark_dirty(path: &Path, run_ids: &[&str]) -> Result<(), String> {
    // Standalone message-log fixtures are not SessionFiles catalogs.
    let Some(root) = path.ancestors().nth(4).filter(|p| {
        p.file_name().is_some_and(|n| n == "sessions")
            || *p == crate::user_data_layout::sessions_dir_path()
    }) else {
        return Ok(());
    };
    let _guard = LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|_| "catalog lock poisoned")?;
    let mut conn = open(root)?;
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    enqueue(&tx, root, path)?;
    let relative = path
        .strip_prefix(root)
        .map_err(|e| e.to_string())?
        .to_string_lossy()
        .replace('\\', "/");
    for id in run_ids {
        tx.execute(
            "INSERT OR IGNORE INTO run_locations VALUES(?1,?2)",
            params![id, relative],
        )
        .map_err(|e| e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())
}

fn refresh(conn: &mut Connection, root: &Path, session_id: Option<&str>) -> Result<(), String> {
    let paths: Vec<String> = {
        let sql = if session_id.is_some() {
            "SELECT sources.path FROM sources JOIN dirty USING(path) WHERE session_id=?1"
        } else {
            "SELECT path FROM dirty"
        };
        let mut stmt = conn.prepare(sql).map_err(|e| e.to_string())?;
        let values = stmt
            .query_map(rusqlite::params_from_iter(session_id), |r| r.get(0))
            .map_err(|e| e.to_string())?
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?;
        values
    };
    for relative in paths {
        let path = root.join(&relative);
        let projected = if path.exists() {
            Some((|| {
                let high_water: Option<u64> = conn
                    .query_row(
                        "SELECT source_high_water FROM sources WHERE path=?1",
                        [&relative],
                        |r| r.get(0),
                    )
                    .map_err(|e| e.to_string())?;
                message_log::read_state::catalog_projection(&path, root, high_water)
            })())
        } else {
            None
        };
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        if !matches!(&projected, Some(Ok((_, _, _, false)))) {
            tx.execute("DELETE FROM runs WHERE path=?1", [&relative])
                .map_err(|e| e.to_string())?;
            tx.execute("DELETE FROM run_locations WHERE path=?1", [&relative])
                .map_err(|e| e.to_string())?;
        }
        match projected {
            Some(Ok((item, runs, source_high_water, _full))) => {
                tx.execute(
                    "UPDATE sources SET summary=?2,error=NULL,source_high_water=?3 WHERE path=?1",
                    params![
                        relative,
                        serde_json::to_string(&item).map_err(|e| e.to_string())?,
                        source_high_water
                    ],
                )
                .map_err(|e| e.to_string())?;
                for run in runs {
                    tx.execute(
                        "INSERT INTO runs VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(run_id,path) DO UPDATE SET status=excluded.status,started=excluded.started,payload=excluded.payload",
                        params![
                            run.agent_run_id,
                            relative,
                            run.session_id,
                            run.status,
                            run.started_at_ms,
                            serde_json::to_string(&run).map_err(|e| e.to_string())?
                        ],
                    )
                    .map_err(|e| e.to_string())?;
                    tx.execute(
                        "INSERT OR IGNORE INTO run_locations VALUES(?1,?2)",
                        params![run.agent_run_id, relative],
                    )
                    .map_err(|e| e.to_string())?;
                }
            }
            Some(Err(error)) => {
                tx.execute(
                    "UPDATE sources SET summary=NULL,error=?2 WHERE path=?1",
                    params![relative, error],
                )
                .map_err(|e| e.to_string())?;
                eprintln!(
                    "session_catalog_source_invalid: path={} error={error}",
                    path.display()
                );
            }
            None => {
                tx.execute("DELETE FROM sources WHERE path=?1", [&relative])
                    .map_err(|e| e.to_string())?;
            }
        }
        let id = path
            .file_stem()
            .and_then(|v| v.to_str())
            .ok_or("invalid Session path")?;
        let summary: Option<String> = tx.query_row("SELECT summary FROM sources WHERE session_id=?1 AND summary IS NOT NULL AND (SELECT count(*) FROM sources WHERE session_id=?1)=1", [id], |r|r.get(0)).optional().map_err(|e|e.to_string())?;
        paging::publish(&tx, id, summary.as_deref())?;
        tx.execute("DELETE FROM dirty WHERE path=?1", [&relative])
            .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn with_index<T>(
    root: &Path,
    action: impl FnOnce(&mut Connection) -> Result<T, String>,
) -> Result<T, String> {
    let _guard = LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|_| "catalog lock poisoned")?;
    let mut conn = open(root)?;
    initialize(&mut conn, root)?;
    action(&mut conn)
}

pub(crate) fn sessions(root: &Path) -> Result<Vec<SessionFileItem>, String> {
    let _source = message_log::lock_session_logs_for_read()?;
    with_index(root, |conn| {
        refresh(conn, root, None)?;
        conn.execute(
            "UPDATE state SET runs_imported=1 WHERE id=1 AND runs_imported=0",
            [],
        )
        .map_err(|e| e.to_string())?;
        let mut stmt = conn.prepare("SELECT summary FROM sources WHERE summary IS NOT NULL AND session_id NOT IN (SELECT session_id FROM sources GROUP BY session_id HAVING count(*)>1)").map_err(|e| e.to_string())?;
        let json = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        let mut items = json
            .into_iter()
            .map(|v| serde_json::from_str::<SessionFileItem>(&v).map_err(|e| e.to_string()))
            .collect::<Result<Vec<_>, _>>()?;
        let parents: std::collections::HashSet<_> = items
            .iter()
            .filter(|v| v.session_kind == "main")
            .map(|v| v.id.clone())
            .collect();
        items.retain(|v| {
            v.session_kind != "subagent"
                || v.parent_session_id
                    .as_ref()
                    .is_some_and(|id| parents.contains(id))
        });
        Ok(items)
    })
}

pub(crate) fn session(root: &Path, id: &str) -> Result<Option<SessionFileItem>, String> {
    let _source = message_log::lock_session_logs_for_read()?;
    with_index(root, |conn| {
        refresh(conn, root, Some(id))?;
        let values = source_paths(conn, id)?;
        if values.len() > 1 {
            return Err(format!("multiple session logs found for sessionId={id}"));
        }
        let value: Option<String> = conn
            .query_row(
                "SELECT summary FROM sources WHERE session_id=?1 AND summary IS NOT NULL",
                [id],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        let item: Option<SessionFileItem> = value
            .map(|v| serde_json::from_str(&v).map_err(|e| e.to_string()))
            .transpose()?;
        if let Some(item) = &item {
            if item.session_kind == "subagent" {
                let Some(parent) = item.parent_session_id.as_deref() else {
                    return Ok(None);
                };
                refresh(conn, root, Some(parent))?;
                let count: i64 = conn.query_row("SELECT count(*) FROM sources WHERE session_id=?1 AND summary IS NOT NULL AND json_extract(summary,'$.session_kind')='main'", [parent], |r| r.get(0)).map_err(|e| e.to_string())?;
                if count != 1 {
                    return Ok(None);
                }
            }
        }
        Ok(item)
    })
}

fn source_paths(conn: &Connection, id: &str) -> Result<Vec<String>, String> {
    let mut stmt = conn
        .prepare("SELECT path FROM sources WHERE session_id=?1")
        .map_err(|e| e.to_string())?;
    let values = stmt
        .query_map([id], |r| r.get(0))
        .map_err(|e| e.to_string())?
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string());
    values
}

/// Caller holds a source read or write lock. Does not project any source logs.
pub(crate) fn path_unlocked(root: &Path, id: &str) -> Result<Option<PathBuf>, String> {
    with_index(root, |conn| {
        let paths = source_paths(conn, id)?;
        if paths.len() > 1 {
            return Err(format!("multiple session logs found for sessionId={id}"));
        }
        Ok(paths.first().map(|p| root.join(p)).filter(|p| p.is_file()))
    })
}

pub(crate) fn runs(
    session_id: Option<&str>,
    run_id: Option<&str>,
    active_only: bool,
) -> Result<Vec<message_log::ProjectedAgentRun>, String> {
    let _source = message_log::lock_session_logs_for_read()?;
    let root = crate::user_data_layout::sessions_dir_path();
    with_index(&root, |conn| {
        // Initial import also builds run locators. Subsequent single-run reads
        // only reconcile sources recorded for that identity before source append.
        if let Some(id) = run_id {
            let imported: bool = conn
                .query_row("SELECT runs_imported FROM state WHERE id=1", [], |r| {
                    r.get(0)
                })
                .map_err(|e| e.to_string())?;
            if !imported {
                refresh(conn, &root, None)?;
                conn.execute(
                    "UPDATE state SET runs_imported=1 WHERE id=1 AND runs_imported=0",
                    [],
                )
                .map_err(|e| e.to_string())?;
            }
            let ids: Vec<String> = {
                let mut stmt = conn.prepare("SELECT DISTINCT session_id FROM sources JOIN run_locations USING(path) WHERE run_id=?1").map_err(|e| e.to_string())?;
                let values = stmt
                    .query_map([id], |r| r.get(0))
                    .map_err(|e| e.to_string())?
                    .collect::<Result<_, _>>()
                    .map_err(|e| e.to_string())?;
                values
            };
            for session in ids {
                refresh(conn, &root, Some(&session))?;
            }
        } else {
            refresh(conn, &root, session_id)?;
            if session_id.is_none() {
                conn.execute(
                    "UPDATE state SET runs_imported=1 WHERE id=1 AND runs_imported=0",
                    [],
                )
                .map_err(|e| e.to_string())?;
            }
        }
        let mut sql = String::from("SELECT payload FROM runs WHERE 1=1");
        let mut args = Vec::<String>::new();
        if let Some(id) = session_id {
            sql.push_str(" AND session_id=?");
            args.push(id.into());
        }
        if let Some(id) = run_id {
            sql.push_str(" AND run_id=?");
            args.push(id.into());
        }
        if active_only {
            sql.push_str(" AND status IN ('running','stalled')");
        }
        sql.push_str(" ORDER BY started,run_id");
        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        let json = stmt
            .query_map(rusqlite::params_from_iter(args), |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        if run_id.is_some() && json.len() > 1 {
            return Err("duplicate agentRunId in Session catalog".into());
        }
        json.into_iter()
            .map(|v| serde_json::from_str(&v).map_err(|e| e.to_string()))
            .collect()
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_files::{SessionFiles, SessionMetadataPatch};
    use std::ffi::OsString;

    struct Fixture {
        root: PathBuf,
        old_data: Option<OsString>,
        old_sessions: Option<OsString>,
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "centaeris-catalog-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir_all(&root).unwrap();
            let f = Self {
                root,
                old_data: std::env::var_os("CENTAERIS_DESKTOP_DATA_DIR"),
                old_sessions: std::env::var_os("CENTAERIS_MESSAGE_LOG_SESSIONS_DIR"),
            };
            std::env::set_var("CENTAERIS_DESKTOP_DATA_DIR", &f.root);
            std::env::set_var(
                "CENTAERIS_MESSAGE_LOG_SESSIONS_DIR",
                f.root.join("sessions"),
            );
            f
        }
        fn store(&self) -> SessionFiles {
            SessionFiles::new(self.root.join("sessions"))
        }
        fn create(&self, name: &str) -> SessionFileItem {
            self.store()
                .create(Some(name), self.root.to_str().unwrap(), 1_800_000_000_000)
                .unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            for (key, value) in [
                ("CENTAERIS_DESKTOP_DATA_DIR", &self.old_data),
                ("CENTAERIS_MESSAGE_LOG_SESSIONS_DIR", &self.old_sessions),
            ] {
                if let Some(value) = value {
                    std::env::set_var(key, value);
                } else {
                    std::env::remove_var(key);
                }
            }
            fs::remove_dir_all(&self.root).unwrap();
        }
    }
    fn clear_reads() {
        message_log::TEST_DOCUMENT_READS.with(|r| r.borrow_mut().clear());
    }
    fn reads() -> Vec<PathBuf> {
        message_log::TEST_DOCUMENT_READS.with(|r| r.borrow().clone())
    }

    #[test]
    fn warm_append_and_summary_do_not_reread_session_history() {
        let _env = message_log::test_env_mutex()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let f = Fixture::new();
        let a = f.create("incremental");
        f.store().list().unwrap();
        clear_reads();
        message_log::append_agent_run_started(
            &a.id,
            "turn-tail",
            "run-tail",
            "tail",
            1_800_000_000_010,
        )
        .unwrap();
        let item = f.store().get(&a.id).unwrap();
        assert_eq!(item.message_count, 1);
        assert_eq!(item.last_message.as_deref(), Some("tail"));
        assert_eq!(
            message_log::project_agent_run("run-tail")
                .unwrap()
                .unwrap()
                .status,
            "running"
        );
        assert!(
            reads().is_empty(),
            "warm append/list read the historical document: {:?}",
            reads()
        );
        message_log::read_state::evict_all();
        clear_reads();
        message_log::append_agent_run_started(
            &a.id,
            "turn-second",
            "run-second",
            "second",
            1_800_000_000_020,
        )
        .unwrap();
        assert_eq!(reads(), vec![f.root.join(&a.session_path)]);
        clear_reads();
        assert_eq!(f.store().get(&a.id).unwrap().message_count, 2);
        assert!(reads().is_empty());
        // Replay is a no-op; conflicting identity must still fail.
        message_log::append_agent_run_started(
            &a.id,
            "turn-tail",
            "run-tail",
            "tail",
            1_800_000_000_010,
        )
        .unwrap();
        assert!(message_log::append_agent_run_started(
            &a.id,
            "turn-tail",
            "run-tail",
            "different",
            1_800_000_000_010
        )
        .is_err());
        assert_eq!(f.store().get(&a.id).unwrap().message_count, 2);
    }

    #[test]
    fn catalog_cursor_scope_generation_and_v1_upgrade_are_explicit() {
        let _env = message_log::test_env_mutex()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let f = Fixture::new();
        let a = f.create("migration");
        let request = |mode: &str, cursor| paging::Request {
            mode: mode.into(),
            cwd: None,
            cursor,
            limit: Some(1),
            session_id: None,
        };
        let first = paging::query(&f.root.join("sessions"), request("recent", None)).unwrap();
        assert_eq!(first.items[0].id, a.id);
        assert!(paging::query(
            &f.root.join("sessions"),
            request("recent", Some(first.revision.clone()))
        )
        .is_err());
        let source = fs::read(f.root.join(&a.session_path)).unwrap();
        {
            let conn = open(&f.root.join("sessions")).unwrap();
            conn.execute_batch(
                "DROP TABLE catalog_clock; DROP TABLE catalog_entries; PRAGMA user_version=1;",
            )
            .unwrap();
        }
        let migrated = paging::query(&f.root.join("sessions"), request("recent", None)).unwrap();
        assert_eq!(migrated.items[0].id, a.id);
        assert_eq!(fs::read(f.root.join(&a.session_path)).unwrap(), source);
        assert!(
            paging::query(
                &f.root.join("sessions"),
                request("changes", Some(first.revision))
            )
            .unwrap()
            .reset
        );
        let baseline = migrated.revision;
        let b = f.create("new");
        let c = f.create("newer");
        let delta =
            paging::query(&f.root.join("sessions"), request("changes", Some(baseline))).unwrap();
        assert_eq!(delta.items.len(), 1);
        assert!(delta.next_cursor.is_some());
        let tail = paging::query(
            &f.root.join("sessions"),
            request("changes", Some(delta.revision)),
        )
        .unwrap();
        assert_eq!(tail.items.len(), 1);
        assert!(tail.next_cursor.is_none());
        let ids =
            std::collections::HashSet::from([delta.items[0].id.clone(), tail.items[0].id.clone()]);
        assert_eq!(ids, std::collections::HashSet::from([b.id, c.id]));
        assert!(serde_json::from_value::<paging::Request>(
            serde_json::json!({"mode":"recent","unknown":true})
        )
        .is_err());
    }

    #[test]
    fn streaming_tail_and_metadata_match_a_cold_projection() {
        let _env = message_log::test_env_mutex()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let f = Fixture::new();
        let a = f.create("stream");
        message_log::append_agent_run_started(&a.id, "turn", "run", "first", 1_800_000_000_001)
            .unwrap();
        f.store().get(&a.id).unwrap();
        clear_reads();
        let events = message_log::append_turn_supplements(
            &a.id,
            "turn",
            "run",
            &[centaeris_core::session::supplement::DurableTurnSupplement {
                supplement_id: "sup".into(),
                sequence: 1,
                message: "next".into(),
                created_at_ms: 1_800_000_000_002,
                claim_token: None,
                claim_lease_owner: None,
            }],
        )
        .unwrap();
        assert_eq!(events.len(), 1);
        let warm = f.store().get(&a.id).unwrap();
        assert_eq!(warm.message_count, 2);
        assert_eq!(warm.last_message.as_deref(), Some("next"));
        assert!(reads().is_empty());
        message_log::read_state::evict_all();
        mark_dirty(&f.root.join(&a.session_path), &[]).unwrap();
        let cold = f.store().get(&a.id).unwrap();
        assert_eq!(
            serde_json::to_value(warm).unwrap(),
            serde_json::to_value(cold).unwrap()
        );
    }

    #[test]
    fn catalog_pages_are_bounded_and_changes_include_deletes() {
        let _env = message_log::test_env_mutex()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let f = Fixture::new();
        let a = f.create("a");
        let b = f.create("b");
        let request = |cursor| crate::session_catalog::paging::Request {
            mode: "recent".into(),
            cwd: Some(a.cwd.clone()),
            cursor,
            limit: Some(1),
            session_id: None,
        };
        let first = paging::query(&f.root.join("sessions"), request(None)).unwrap();
        assert_eq!(first.items.len(), 1);
        clear_reads();
        let second =
            paging::query(&f.root.join("sessions"), request(first.next_cursor.clone())).unwrap();
        assert_eq!(second.items.len(), 1);
        assert_ne!(first.items[0].id, second.items[0].id);
        assert!(second.next_cursor.is_none());
        assert!(reads().is_empty());
        f.store().delete(&b.id).unwrap();
        let changes = paging::query(
            &f.root.join("sessions"),
            paging::Request {
                mode: "changes".into(),
                cwd: None,
                cursor: Some(first.revision.clone()),
                limit: Some(100),
                session_id: None,
            },
        )
        .unwrap();
        assert_eq!(changes.deleted_ids, vec![b.id]);
        assert!(
            paging::query(&f.root.join("sessions"), request(first.next_cursor))
                .unwrap()
                .reset
        );
        assert!(paging::query(
            &f.root.join("sessions"),
            paging::Request {
                mode: "recent".into(),
                cwd: None,
                cursor: None,
                limit: Some(1000),
                session_id: None,
            }
        )
        .is_err());
    }

    #[test]
    fn single_run_lookup_and_restart_do_not_read_unrelated_sources() {
        let _env = message_log::test_env_mutex()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let f = Fixture::new();
        let a = f.create("a");
        let b = f.create("b");
        message_log::append_agent_run_started(&a.id, "turn-a", "run-a", "input", 1_800_000_000_001)
            .unwrap();
        message_log::append_agent_run_started(&b.id, "turn-b", "run-b", "input", 1_800_000_000_001)
            .unwrap();
        f.store().list().unwrap();
        clear_reads();
        assert_eq!(
            message_log::project_agent_run("run-a")
                .unwrap()
                .unwrap()
                .session_id,
            a.id
        );
        assert_eq!(runs(Some(&a.id), None, false).unwrap().len(), 1);
        assert!(reads().is_empty());
        // An unrelated dirty source must not be repaired by an ID lookup.
        let b_path = f.root.join(&b.session_path);
        mark_dirty(&b_path, &["run-b"]).unwrap();
        clear_reads();
        assert!(message_log::project_agent_run("run-a").unwrap().is_some());
        assert!(message_log::project_agent_run("missing").unwrap().is_none());
        assert!(reads().is_empty());
        assert_eq!(
            message_log::project_agent_run("run-b")
                .unwrap()
                .unwrap()
                .cwd
                .as_deref(),
            Some(b.cwd.as_str())
        );
        assert!(reads().iter().all(|path| path == &b_path));
        f.store().delete(&b.id).unwrap();
        assert!(message_log::project_agent_run("run-b").unwrap().is_none());
    }

    #[test]
    fn durable_intents_repair_only_interrupted_sources_and_index_can_be_rebuilt() {
        let _env = message_log::test_env_mutex()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let f = Fixture::new();
        let a = f.create("old-name");
        let b = f.create("untouched");
        f.store().list().unwrap();
        let path = f.root.join(&a.session_path);
        let original = fs::read_to_string(&path).unwrap();
        // Crash after intent, before source mutation.
        mark_dirty(&path, &[]).unwrap();
        assert_eq!(f.store().get(&a.id).unwrap().title, "old-name");
        // Crash after durable source mutation, before index projection.
        mark_dirty(&path, &[]).unwrap();
        fs::write(&path, original.replace("old-name", "new-name")).unwrap();
        clear_reads();
        assert_eq!(f.store().get(&a.id).unwrap().title, "new-name");
        assert_eq!(reads(), vec![path.clone()]);
        assert_eq!(f.store().get(&b.id).unwrap().title, "untouched");
        // Interrupted deletion is also repaired without reading other sources.
        mark_dirty(&path, &[]).unwrap();
        fs::remove_file(&path).unwrap();
        clear_reads();
        assert!(f.store().get(&a.id).is_err());
        assert!(reads().is_empty());
        let b_path = f.root.join(&b.session_path);
        let before = fs::read(&b_path).unwrap();
        fs::remove_file(f.root.join("runtime/session-catalog.sqlite3")).unwrap();
        assert_eq!(f.store().list().unwrap().len(), 1);
        assert_eq!(fs::read(b_path).unwrap(), before);
        f.store()
            .update(
                &b.id,
                SessionMetadataPatch {
                    is_pinned: Some(true),
                    ..Default::default()
                },
                1_800_000_000_002,
            )
            .unwrap();
        assert!(f.store().get(&b.id).unwrap().is_pinned);
    }

    #[test]
    fn import_resumes_dirty_work_and_rejects_unknown_index_versions() {
        let _env = message_log::test_env_mutex()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let f = Fixture::new();
        let a = f.create("a");
        f.create("b");
        fs::remove_file(f.root.join("runtime/session-catalog.sqlite3")).unwrap();
        {
            let mut conn = open(&f.root.join("sessions")).unwrap();
            initialize(&mut conn, &f.root.join("sessions")).unwrap();
        }
        assert_eq!(f.store().get(&a.id).unwrap().title, "a");
        clear_reads();
        assert_eq!(f.store().list().unwrap().len(), 2);
        assert_eq!(reads().len(), 1);
        let conn = open(&f.root.join("sessions")).unwrap();
        conn.execute_batch("PRAGMA user_version=99").unwrap();
        assert!(f
            .store()
            .list()
            .unwrap_err()
            .contains("unsupported session catalog version"));
        drop(conn);
    }

    #[test]
    fn catalog_payloads_reject_unknown_fields() {
        let _env = message_log::test_env_mutex()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let f = Fixture::new();
        let item = f.create("a");
        f.store().list().unwrap();
        let conn = open(&f.root.join("sessions")).unwrap();
        conn.execute(
            "UPDATE sources SET summary=json_set(summary,'$.unknownField',1)",
            [],
        )
        .unwrap();
        assert!(f
            .store()
            .get(&item.id)
            .unwrap_err()
            .contains("unknown field"));
    }

    #[test]
    fn lookup_and_active_queries_use_index_searches() {
        let _env = message_log::test_env_mutex()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let f = Fixture::new();
        let conn = open(&f.root.join("sessions")).unwrap();
        for (query, expected) in [
            ("EXPLAIN QUERY PLAN SELECT path FROM sources WHERE session_id='a'", "sources_session"),
            ("EXPLAIN QUERY PLAN SELECT payload FROM runs WHERE run_id='a' ORDER BY started,run_id", "sqlite_autoindex_runs"),
            ("EXPLAIN QUERY PLAN SELECT payload FROM runs WHERE status IN ('running','stalled') ORDER BY started,run_id", "runs_status"),
        ] {
            let mut stmt = conn.prepare(query).unwrap();
            let plan = stmt.query_map([], |r| r.get::<_,String>(3)).unwrap().collect::<Result<Vec<_>,_>>().unwrap().join(" ");
            assert!(plan.contains(expected),"{plan}");
        }
    }

    #[test]
    #[ignore = "100 MiB unrelated source growth acceptance"]
    fn hundred_megabytes_of_unrelated_history_do_not_enter_warm_reads() {
        let _env = message_log::test_env_mutex()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let f = Fixture::new();
        let current = f.create("current");
        let historical = f.create("historical");
        let large = "x".repeat(100 * 1024 * 1024);
        message_log::append_agent_run_started(
            &historical.id,
            "old-turn",
            "old-run",
            &large,
            1_800_000_000_001,
        )
        .unwrap();
        drop(large);
        f.store().list().unwrap();
        assert!(
            fs::metadata(f.root.join(&historical.session_path))
                .unwrap()
                .len()
                >= 100 * 1024 * 1024
        );
        clear_reads();
        for _ in 0..5 {
            assert_eq!(f.store().get(&current.id).unwrap().title, "current");
            assert_eq!(f.store().list().unwrap().len(), 2);
            assert!(message_log::project_agent_run("old-run").unwrap().is_some());
        }
        assert!(reads().is_empty());
    }
}
