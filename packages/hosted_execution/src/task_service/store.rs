//! Instance-private execution index. The caller holds the service's exclusive lock.
use super::*;
use rusqlite::{params, Connection, OptionalExtension};

const VERSION: i64 = 1;
fn db(root: &Path, initialize: bool) -> Result<Connection, String> {
    let path = root.join("history.sqlite");
    let exists = path.exists();
    if !exists && !initialize {
        return Err("task_index_unavailable".into());
    }
    if exists
        && fs::symlink_metadata(&path)
            .map_err(|_| "task_index_unavailable")?
            .file_type()
            .is_symlink()
    {
        return Err("task_index_invalid".into());
    }
    let connection = Connection::open(&path).map_err(|_| "task_index_unavailable")?;
    if !exists {
        connection.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL; PRAGMA temp_store=MEMORY; BEGIN;
            CREATE TABLE metadata(key TEXT PRIMARY KEY,value TEXT NOT NULL) WITHOUT ROWID;
            CREATE TABLE tasks(id TEXT PRIMARY KEY,record TEXT NOT NULL,live INTEGER NOT NULL,hot INTEGER NOT NULL) WITHOUT ROWID;
            CREATE TABLE operations(key BLOB PRIMARY KEY,receipt TEXT NOT NULL,task_id BLOB) WITHOUT ROWID;
            CREATE TABLE closed_runs(id TEXT PRIMARY KEY) WITHOUT ROWID;
            PRAGMA user_version=1; COMMIT;").map_err(|_|"task_index_unavailable")?;
    }
    validate(&connection)?;
    connection
        .execute_batch("PRAGMA synchronous=FULL; PRAGMA temp_store=MEMORY;")
        .map_err(|_| "task_index_unavailable")?;
    Ok(connection)
}

fn validate(connection: &Connection) -> Result<(), String> {
    let mode: String = connection
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .map_err(|_| "task_index_invalid")?;
    if mode != "delete" {
        return Err("task_index_unsupported_schema".into());
    }
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .map_err(|_| "task_index_invalid")?;
    if version != VERSION {
        return Err("task_index_unsupported_schema".into());
    }
    for (table, expected) in [
        ("metadata", vec!["key", "value"]),
        ("tasks", vec!["id", "record", "live", "hot"]),
        ("operations", vec!["key", "receipt", "task_id"]),
        ("closed_runs", vec!["id"]),
    ] {
        let mut query = connection
            .prepare(&format!("PRAGMA table_info({table})"))
            .map_err(|_| "task_index_invalid")?;
        let names = query
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(5)?,
                ))
            })
            .map_err(|_| "task_index_invalid")?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| "task_index_invalid")?;
        let expected = expected
            .iter()
            .enumerate()
            .map(|(index, name)| {
                (
                    name.to_string(),
                    if table == "operations" && (*name == "key" || *name == "task_id") {
                        "BLOB"
                    } else if *name == "live" || *name == "hot" {
                        "INTEGER"
                    } else {
                        "TEXT"
                    }
                    .to_string(),
                    if *name == "task_id" { 0i64 } else { 1i64 },
                    if index == 0 { 1i64 } else { 0i64 },
                )
            })
            .collect::<Vec<_>>();
        if names != expected {
            return Err("task_index_invalid".into());
        }
    }
    let mut tables=connection.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name").map_err(|_|"task_index_invalid")?;
    let names = tables
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|_| "task_index_invalid")?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "task_index_invalid")?;
    if names != ["closed_runs", "metadata", "operations", "tasks"] {
        return Err("task_index_invalid".into());
    }
    drop(tables);
    let unexpected:i64=connection.query_row("SELECT COUNT(*) FROM sqlite_master WHERE type IN ('trigger','view','index') AND name NOT LIKE 'sqlite_%'",[],|r|r.get(0)).map_err(|_|"task_index_invalid")?;
    if unexpected != 0 {
        return Err("task_index_invalid".into());
    }
    let mut table_list = connection
        .prepare("PRAGMA table_list")
        .map_err(|_| "task_index_invalid")?;
    let tables = table_list
        .query_map([], |r| Ok((r.get::<_, String>(1)?, r.get::<_, i64>(4)?)))
        .map_err(|_| "task_index_invalid")?;
    for table in tables {
        let (name, without_rowid) = table.map_err(|_| "task_index_invalid")?;
        if ["metadata", "tasks", "operations", "closed_runs"].contains(&name.as_str())
            && without_rowid != 1
        {
            return Err("task_index_invalid".into());
        }
    }
    Ok(())
}

pub(super) fn closed(root: &Path, run: &str) -> Result<bool, String> {
    Ok(db(root, false)?
        .query_row("SELECT 1 FROM closed_runs WHERE id=?1", [run], |r| {
            r.get::<_, i64>(0)
        })
        .optional()
        .map_err(|_| "task_index_unavailable")?
        .is_some())
}

fn encode(value: &impl Serialize) -> Result<String, String> {
    serde_json::to_string(value).map_err(|_| "task_encoding_failed".into())
}
fn decode<T: serde::de::DeserializeOwned>(value: String) -> Result<T, String> {
    serde_json::from_str(&value).map_err(|_| "task_index_invalid".into())
}

type StoredRecord = (
    String,
    String,
    u64,
    String,
    bool,
    EnvironmentTaskState,
    Option<i32>,
    u8,
);
fn record_text(record: &Record) -> Result<String, String> {
    encode(&(
        &record.digest,
        &record.creator_controller_id,
        record.creator_epoch,
        &record.snapshot.operation_id,
        matches!(record.snapshot.scope, EnvironmentTaskScope::Environment),
        &record.snapshot.state,
        record.snapshot.exit_code,
        u8::from(record.snapshot.cleanup_complete)
            | (u8::from(record.snapshot.output_complete) << 1),
    ))
}
fn record_value(instance: &str, id: String, value: String) -> Result<Record, String> {
    let (
        digest,
        creator_controller_id,
        creator_epoch,
        operation_id,
        environment_scope,
        state,
        exit_code,
        flags,
    ): StoredRecord = decode(value)?;
    if flags > 3 {
        return Err("task_index_invalid".into());
    }
    let cleanup_complete = flags & 1 != 0;
    let output_complete = flags & 2 != 0;
    let record = Record {
        digest,
        creator_controller_id: creator_controller_id.clone(),
        creator_epoch,
        snapshot: EnvironmentTaskSnapshot {
            task_id: id,
            operation_id,
            scope: if environment_scope {
                EnvironmentTaskScope::Environment
            } else {
                EnvironmentTaskScope::Run {
                    run_id: creator_controller_id,
                }
            },
            state,
            exit_code,
            cleanup_complete,
            output_complete,
        },
    };
    record.validate(instance, &record.snapshot.task_id)?;
    Ok(record)
}

pub(super) fn load(root: &Path) -> Result<Option<Journal>, String> {
    let connection = db(root, true)?;
    let value: Option<String> = connection
        .query_row("SELECT value FROM metadata WHERE key='journal'", [], |r| {
            r.get(0)
        })
        .optional()
        .map_err(|_| "task_index_unavailable")?;
    let Some(value) = value else {
        let facts:i64=connection.query_row("SELECT (SELECT COUNT(*) FROM tasks)+(SELECT COUNT(*) FROM operations)+(SELECT COUNT(*) FROM closed_runs)",[],|row|row.get(0)).map_err(|_|"task_index_invalid")?;
        if facts != 0 {
            return Err("task_index_invalid".into());
        }
        return Ok(None);
    };
    let mut journal: Journal = decode(value)?;
    journal.validate_import()?;
    if !journal.tasks.is_empty() || !journal.receipts.is_empty() {
        return Err("task_index_invalid".into());
    }
    journal.index_root = Some(root.to_owned());
    let mut statement = connection
        .prepare("SELECT id,record,hot,live FROM tasks ORDER BY id")
        .map_err(|_| "task_index_unavailable")?;
    let records = statement
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })
        .map_err(|_| "task_index_unavailable")?;
    for entry in records {
        let (id, value, hot, live) = entry.map_err(|_| "task_index_unavailable")?;
        let record = record_value(&journal.instance, id.clone(), value)?;
        if !(0..=1).contains(&hot)
            || live != i64::from(!record.snapshot.cleanup_complete)
            || (live == 1 && hot == 0)
        {
            return Err("task_index_invalid".into());
        }
        if hot == 1 {
            journal.tasks.insert(id, record);
        }
    }
    if journal.tasks.len() > ENVIRONMENT_TASK_MAX_RECORDS {
        return Err("task_index_invalid".into());
    }
    Ok(Some(journal))
}

pub(super) fn task(root: &Path, id: &str) -> Result<Option<Record>, String> {
    let connection = db(root, false)?;
    let metadata: String = connection
        .query_row(
            "SELECT value FROM metadata WHERE key='journal'",
            [],
            |row| row.get(0),
        )
        .map_err(|_| "task_index_invalid")?;
    let journal: Journal = decode(metadata)?;
    journal.validate_import()?;
    let value: Option<String> = connection
        .query_row("SELECT record FROM tasks WHERE id=?1", [id], |row| {
            row.get(0)
        })
        .optional()
        .map_err(|_| "task_index_unavailable")?;
    value
        .map(|value| record_value(&journal.instance, id.to_owned(), value))
        .transpose()
}
#[derive(Serialize, Deserialize)]
enum StoredResult {
    Mutated(String, EnvironmentTaskState, Option<i32>, u8),
    RunEnded(Vec<String>, bool),
}
fn receipt_text(receipt: &Receipt) -> Result<String, String> {
    let result = match &receipt.result {
        None => None,
        Some(EnvironmentTaskResult::Mutated { task }) => Some(StoredResult::Mutated(
            task.task_id.clone(),
            task.state.clone(),
            task.exit_code,
            u8::from(task.cleanup_complete) | (u8::from(task.output_complete) << 1),
        )),
        Some(EnvironmentTaskResult::RunEnded {
            task_ids,
            cleanup_complete,
        }) => Some(StoredResult::RunEnded(task_ids.clone(), *cleanup_complete)),
        _ => return Err("task_receipt_invalid".into()),
    };
    encode(&(&receipt.digest, result))
}
fn operation_task(key: &str) -> Option<Vec<u8>> {
    let rest = key
        .strip_prefix("input:")
        .or_else(|| key.strip_prefix("terminate:"))?;
    Some(Sha256::digest(rest.split(':').next().unwrap_or(rest).as_bytes()).to_vec())
}
pub(super) fn receipt(root: &Path, key: &str) -> Result<Option<Receipt>, String> {
    let value: Option<String> = db(root, false)?
        .query_row(
            "SELECT receipt FROM operations WHERE key=?1",
            [Sha256::digest(key.as_bytes()).as_slice()],
            |r| r.get(0),
        )
        .optional()
        .map_err(|_| "task_index_unavailable")?;
    let Some(value) = value else { return Ok(None) };
    let (digest, result): (String, Option<StoredResult>) = decode(value)?;
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("task_receipt_invalid".into());
    }
    let result = match result {
        None => None,
        Some(StoredResult::RunEnded(task_ids, cleanup_complete)) => {
            if task_ids.len() > ENVIRONMENT_TASK_MAX_RECORDS {
                return Err("task_receipt_invalid".into());
            }
            Some(EnvironmentTaskResult::RunEnded {
                task_ids,
                cleanup_complete,
            })
        }
        Some(StoredResult::Mutated(id, state, exit_code, flags)) => {
            let mut task = task(root, &id)?.ok_or("task_receipt_invalid")?.snapshot;
            task.state = state;
            task.exit_code = exit_code;
            if flags > 3 {
                return Err("task_index_invalid".into());
            }
            task.cleanup_complete = flags & 1 != 0;
            task.output_complete = flags & 2 != 0;
            Some(EnvironmentTaskResult::Mutated { task })
        }
    };
    Ok(Some(Receipt { digest, result }))
}

pub(super) fn live_bytes(root: &Path) -> Result<u64, String> {
    let connection = db(root, false)?;
    let pages: u64 = connection
        .pragma_query_value(None, "page_count", |row| row.get(0))
        .map_err(|_| "task_index_invalid")?;
    let free: u64 = connection
        .pragma_query_value(None, "freelist_count", |row| row.get(0))
        .map_err(|_| "task_index_invalid")?;
    let size: u64 = connection
        .pragma_query_value(None, "page_size", |row| row.get(0))
        .map_err(|_| "task_index_invalid")?;
    Ok(pages.saturating_sub(free).saturating_mul(size))
}
pub(super) fn bytes(root: &Path) -> Result<u64, String> {
    let mut total = 0u64;
    for entry in fs::read_dir(root).map_err(|_| "task_index_unavailable")? {
        let metadata = entry
            .map_err(|_| "task_index_unavailable")?
            .metadata()
            .map_err(|_| "task_index_unavailable")?;
        if metadata.is_file() {
            total = total
                .checked_add(charged_bytes(&metadata))
                .ok_or("task_journal_capacity")?;
        }
    }
    Ok(total)
}

fn write_error(error: rusqlite::Error) -> String {
    if error.sqlite_error_code() == Some(rusqlite::ErrorCode::DiskFull) {
        "task_index_capacity".into()
    } else {
        "task_journal_write_failed".into()
    }
}
pub(super) fn save(root: &Path, journal: &Journal) -> Result<(), String> {
    let mut metadata = journal.clone();
    metadata.tasks.clear();
    metadata.receipts.clear();
    let value = encode(&metadata)?;
    if value.len() > CONTROL_JOURNAL_BYTES {
        return Err("task_journal_too_large".into());
    }
    let mut connection = db(root, false)?;
    // Cap the complete database, including free pages. Rollback journal growth is
    // separately reserved before admission; SQLite never spills temporary tables.
    connection
        .pragma_update(
            None,
            "max_page_count",
            INDEX_BYTES
                / connection
                    .pragma_query_value(None, "page_size", |r| r.get::<_, usize>(0))
                    .map_err(|_| "task_index_invalid")?,
        )
        .map_err(|_| "task_index_unavailable")?;
    let tx = connection
        .transaction()
        .map_err(|_| "task_index_unavailable")?;
    tx.execute("INSERT INTO metadata VALUES('journal',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[value])
        .map_err(write_error)?;
    if let Some(run) = &journal.closed_run {
        tx.execute("INSERT OR IGNORE INTO closed_runs VALUES(?1)", [run])
            .map_err(write_error)?;
    }
    for (id, record) in &journal.tasks {
        record.validate(&journal.instance, id)?;
        tx.execute("INSERT INTO tasks VALUES(?1,?2,?3,1) ON CONFLICT(id) DO UPDATE SET record=excluded.record,live=excluded.live,hot=1",params![id,record_text(record)?,!record.snapshot.cleanup_complete])
            .map_err(write_error)?;
    }
    for (key, receipt) in &journal.receipts {
        tx.execute("INSERT INTO operations VALUES(?1,?2,?3) ON CONFLICT(key) DO UPDATE SET receipt=excluded.receipt",params![Sha256::digest(key.as_bytes()).as_slice(),receipt_text(receipt)?,operation_task(key)])
            .map_err(write_error)?;
    }
    tx.commit().map_err(write_error)
}

pub(super) fn cool(root: &Path, id: &str) -> Result<(), String> {
    db(root, false)?
        .execute("UPDATE tasks SET hot=0 WHERE id=?1 AND live=0", [id])
        .map_err(|_| "task_index_unavailable")?;
    Ok(())
}

pub(super) fn inspection(root: &Path, output: &Path) -> Result<serde_json::Value, String> {
    let mut connection = Connection::open_with_flags(
        root.join("history.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .map_err(|_| "task_index_unavailable")?;
    validate(&connection)?;
    let tx = connection
        .transaction()
        .map_err(|_| "task_index_unavailable")?;
    let value: String = tx
        .query_row("SELECT value FROM metadata WHERE key='journal'", [], |r| {
            r.get(0)
        })
        .map_err(|_| "task_index_invalid")?;
    let journal: Journal = decode(value)?;
    let mut query = tx
        .prepare("SELECT id,record,hot,live FROM tasks")
        .map_err(|_| "task_index_invalid")?;
    let mut rows = query.query([]).map_err(|_| "task_index_invalid")?;
    let (mut indexed, mut run, mut hot, mut active) = (0u64, 0u64, 0u64, 0u64);
    while let Some(row) = rows.next().map_err(|_| "task_index_invalid")? {
        let record = record_value(
            &journal.instance,
            row.get(0).map_err(|_| "task_index_invalid")?,
            row.get(1).map_err(|_| "task_index_invalid")?,
        )?;
        indexed += 1;
        hot += row.get::<_, u64>(2).map_err(|_| "task_index_invalid")?;
        active += row.get::<_, u64>(3).map_err(|_| "task_index_invalid")?;
        if journal
            .fence
            .as_ref()
            .is_some_and(|f| record.creator_controller_id == f.controller_id)
        {
            run += 1;
        }
    }
    Ok(
        serde_json::json!({"schema":"environment.task.store.inspection.v1","instanceId":journal.instance,"controllerId":journal.fence.as_ref().map(|f|&f.controller_id),"controllerEpoch":journal.fence.as_ref().map(|f|f.controller_epoch),"hotTasks":hot,"activeTasks":active,"indexedTasks":indexed,"indexedRunTasks":run,"controlDirectoryBytes":bytes(root)?,"outputDirectoryBytes":bytes(output)?}),
    )
}

/// A bounded scan: callers release each result before asking for the next one.
/// Only a closed Run with no uncertain input may lose its task-side replay data.
pub(super) fn next_closed(
    root: &Path,
    after: &str,
    current: &str,
) -> Result<Option<(String, Record)>, String> {
    let connection = db(root, false)?;
    let metadata: String = connection
        .query_row(
            "SELECT value FROM metadata WHERE key='journal'",
            [],
            |row| row.get(0),
        )
        .map_err(|_| "task_index_invalid")?;
    let journal: Journal = decode(metadata)?;
    journal.validate_import()?;
    let mut query = connection
        .prepare("SELECT id,record FROM tasks WHERE id>?1 AND live=0 AND hot=0 ORDER BY id")
        .map_err(|_| "task_index_unavailable")?;
    let mut rows = query.query([after]).map_err(|_| "task_index_unavailable")?;
    while let Some(row) = rows.next().map_err(|_| "task_index_unavailable")? {
        let id: String = row.get(0).map_err(|_| "task_index_invalid")?;
        let record = record_value(
            &journal.instance,
            id.clone(),
            row.get(1).map_err(|_| "task_index_invalid")?,
        )?;
        let EnvironmentTaskScope::Run { run_id } = &record.snapshot.scope else {
            continue;
        };
        if run_id == current || !closed(root, run_id)? {
            continue;
        }
        let mut receipts = connection
            .prepare("SELECT receipt FROM operations WHERE task_id=?1")
            .map_err(|_| "task_index_unavailable")?;
        let entries = receipts
            .query_map([Sha256::digest(id.as_bytes()).as_slice()], |r| {
                r.get::<_, String>(0)
            })
            .map_err(|_| "task_index_unavailable")?;
        let mut uncertain = false;
        for value in entries {
            let (_, result): (String, Option<StoredResult>) =
                decode(value.map_err(|_| "task_index_invalid")?)?;
            uncertain |= result.is_none();
        }
        if !uncertain {
            return Ok(Some((id, record)));
        }
    }
    Ok(None)
}
pub(super) fn forget_closed(root: &Path, id: &str) -> Result<(), String> {
    let mut connection = db(root, false)?;
    let tx = connection
        .transaction()
        .map_err(|_| "task_index_unavailable")?;
    tx.execute(
        "DELETE FROM operations WHERE task_id=?1",
        [Sha256::digest(id.as_bytes()).as_slice()],
    )
    .map_err(|_| "task_index_unavailable")?;
    tx.execute("DELETE FROM tasks WHERE id=?1 AND live=0", [id])
        .map_err(|_| "task_index_unavailable")?;
    tx.commit().map_err(|_| "task_index_unavailable".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (PathBuf, Journal) {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "centaeris-index-test-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let mut journal = Journal::open(&root, "instance").unwrap();
        journal
            .takeover(EnvironmentTaskFence {
                environment_instance_id: "instance".into(),
                controller_id: "run".into(),
                controller_epoch: 1,
                controller_purpose: EnvironmentTaskControllerPurpose::Execute,
            })
            .unwrap();
        journal.persist(&root).unwrap();
        (root, journal)
    }
    fn record(journal: &Journal, n: usize) -> Record {
        let fence = journal.fence.as_ref().unwrap();
        let operation = fence.operation_id(&format!("operation-{n}"));
        let id = task_identity(fence, &operation).unwrap();
        Record {
            digest: digest(&n).unwrap(),
            creator_controller_id: fence.controller_id.clone(),
            creator_epoch: 1,
            snapshot: EnvironmentTaskSnapshot {
                task_id: id,
                operation_id: operation,
                scope: EnvironmentTaskScope::Run {
                    run_id: fence.controller_id.clone(),
                },
                state: EnvironmentTaskState::Exited,
                exit_code: Some(0),
                cleanup_complete: true,
                output_complete: true,
            },
        }
    }
    #[test]
    fn malformed_constraints_and_unexpected_triggers_are_rejected_without_repair() {
        for change in [
            "DROP TABLE operations; CREATE TABLE operations(key BLOB,receipt TEXT,task_id BLOB)",
            "CREATE TRIGGER tamper AFTER INSERT ON metadata BEGIN DELETE FROM tasks; END",
        ] {
            let (root, _) = fixture();
            let db = Connection::open(root.join("history.sqlite")).unwrap();
            db.execute_batch(change).unwrap();
            drop(db);
            let before = fs::read(root.join("history.sqlite")).unwrap();
            assert!(Journal::open(&root, "instance").is_err());
            assert_eq!(fs::read(root.join("history.sqlite")).unwrap(), before);
            fs::remove_dir_all(root).unwrap();
        }
    }
    #[test]
    fn cold_corrupt_identity_is_rejected_instead_of_reinterpreted_or_repaired() {
        let (root, mut journal) = fixture();
        let mut task = record(&journal, 0);
        let id = task.snapshot.task_id.clone();
        journal.tasks.insert(id.clone(), task.clone());
        journal.persist(&root).unwrap();
        journal.retire_task(&id, 1, false);
        task.snapshot.operation_id = "1:another-operation".into();
        let db = Connection::open(root.join("history.sqlite")).unwrap();
        db.execute(
            "UPDATE tasks SET record=?1 WHERE id=?2",
            params![record_text(&task).unwrap(), id],
        )
        .unwrap();
        drop(db);
        assert!(super::task(&root, &id).is_err());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn incomplete_output_quality_does_not_pin_finished_task_or_claim_complete_output() {
        let (root, mut journal) = fixture();
        let mut task = record(&journal, 0);
        task.snapshot.state = EnvironmentTaskState::Unknown;
        task.snapshot.output_complete = false;
        let id = task.snapshot.task_id.clone();
        journal.tasks.insert(id.clone(), task);
        journal.persist(&root).unwrap();
        assert!(!journal.retire_task(&id, 1, true));
        assert!(journal.retire_task(&id, 1, false));
        assert!(
            !super::task(&root, &id)
                .unwrap()
                .unwrap()
                .snapshot
                .output_complete
        );
        assert_eq!(inspection(&root, &root).unwrap()["activeTasks"], 0);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn deleted_index_pages_restore_admission_without_a_second_database_copy() {
        let (root, journal) = fixture();
        let mut connection = db(&root, false).unwrap();
        let tx = connection.transaction().unwrap();
        for n in 0..30000 {
            let task = record(&journal, n);
            tx.execute(
                "INSERT INTO tasks VALUES(?1,?2,0,0)",
                params![task.snapshot.task_id, record_text(&task).unwrap()],
            )
            .unwrap();
            if n % 100 == 0 {
                let pages: u64 = tx
                    .pragma_query_value(None, "page_count", |row| row.get(0))
                    .unwrap();
                let size: u64 = tx
                    .pragma_query_value(None, "page_size", |row| row.get(0))
                    .unwrap();
                if pages * size > (INDEX_BYTES - CONTROL_COMPLETION_RESERVE_BYTES + 8192) as u64 {
                    break;
                }
            }
        }
        tx.commit().unwrap();
        assert_eq!(
            journal.preflight(4096).unwrap_err(),
            "task_journal_capacity"
        );
        let before = fs::metadata(root.join("history.sqlite")).unwrap().len();
        connection.execute("DELETE FROM tasks", []).unwrap();
        drop(connection);
        journal.preflight(4096).unwrap();
        assert_eq!(
            fs::metadata(root.join("history.sqlite")).unwrap().len(),
            before
        );
        assert!(!journal.poisoned);
        fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod commit_tests {
    use super::*;
    fn fixture() -> (PathBuf, Journal) {
        let root = std::env::temp_dir().join(format!(
            "centaeris-index-commit-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        let mut journal = Journal::open(&root, "instance").unwrap();
        journal
            .takeover(EnvironmentTaskFence {
                environment_instance_id: "instance".into(),
                controller_id: "run".into(),
                controller_epoch: 1,
                controller_purpose: EnvironmentTaskControllerPurpose::Execute,
            })
            .unwrap();
        journal.persist(&root).unwrap();
        (root, journal)
    }
    #[test]
    fn rejected_receipt_commit_does_not_publish_memory_intent_or_poison_controls() {
        let (root, mut journal) = fixture();
        let before = fs::read(root.join("history.sqlite")).unwrap();
        fs::create_dir(root.join("history.sqlite-journal")).unwrap();
        assert!(journal
            .reserve_receipt("input:task:rejected", &digest(&"input").unwrap())
            .is_err());
        assert!(journal.receipts.is_empty());
        assert!(!journal.poisoned);
        fs::remove_dir(root.join("history.sqlite-journal")).unwrap();
        assert_eq!(fs::read(root.join("history.sqlite")).unwrap(), before);
        assert!(journal
            .reserve_receipt("terminate:task", &digest(&"stop").unwrap())
            .is_ok());
        assert!(journal
            .reserve_receipt("endRun:run", &digest(&"end").unwrap())
            .is_ok());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn sqlite_full_rolls_back_and_preserves_controls_without_poisoning_existing_journal() {
        let (root, mut journal) = fixture();
        let before = fs::read(root.join("history.sqlite")).unwrap();
        let mut candidate = journal.clone();
        candidate.receipts.insert(
            "endRun:oversized-storage-fault".into(),
            Receipt {
                digest: digest(&"fault").unwrap(),
                result: Some(EnvironmentTaskResult::RunEnded {
                    task_ids: vec!["x".repeat(69); 100000],
                    cleanup_complete: true,
                }),
            },
        );
        assert_eq!(candidate.persist(&root).unwrap_err(), "task_index_capacity");
        assert!(!candidate.poisoned);
        assert_eq!(fs::read(root.join("history.sqlite")).unwrap(), before);
        assert!(journal
            .reserve_receipt("terminate:task", &digest(&"stop").unwrap())
            .is_ok());
        assert!(journal
            .reserve_receipt("endRun:run", &digest(&"end").unwrap())
            .is_ok());
        fs::remove_dir_all(root).unwrap();
    }
}
