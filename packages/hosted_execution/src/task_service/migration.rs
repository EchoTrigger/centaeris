//! Explicit forward import of private v2 state. This is not a wire compatibility reader.
//! The caller holds the exclusive service lease and keeps immutable source backups.
//! Existing task/cgroup/output and receipt identities never change. Legacy owners
//! cannot Start or Input under v3; they require authorized recovery or cleanup.
//! Running records remain uncertain and retain their cleanup/resource obligations.
use super::*;
use rusqlite::{Connection, OptionalExtension, params};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct V2Fence {
    environment_instance_id: String,
    controller_id: String,
    controller_epoch: u64,
    controller_purpose: EnvironmentTaskControllerPurpose,
}
impl V2Fence {
    fn convert(self) -> EnvironmentTaskFence {
        EnvironmentTaskFence {
            environment_instance_id: self.environment_instance_id,
            controller_owner: EnvironmentTaskOwner::AgentRun {
                run_id: self.controller_id,
            },
            controller_epoch: self.controller_epoch,
            controller_purpose: self.controller_purpose,
        }
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct V2Record {
    digest: String,
    creator_controller_id: String,
    creator_epoch: u64,
    snapshot: EnvironmentTaskSnapshot,
}
impl V2Record {
    fn convert(self) -> Record {
        Record {
            digest: self.digest,
            creator_owner: EnvironmentTaskOwner::AgentRun {
                run_id: self.creator_controller_id,
            },
            identity_version: TaskIdentityVersion::LegacyV2,
            creator_epoch: self.creator_epoch,
            snapshot: self.snapshot,
        }
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct V2Journal {
    schema: String,
    instance: String,
    fence: Option<V2Fence>,
    takeover_pending: bool,
    closed_run: Option<String>,
    tasks: BTreeMap<String, V2Record>,
    receipts: BTreeMap<String, Receipt>,
}
fn remember_owner(journal: &mut Journal, owner: &EnvironmentTaskOwner) {
    if !journal.import_upgrade_owners.contains(owner) {
        journal.import_upgrade_owners.push(owner.clone());
    }
}
fn read_v2(bytes: &[u8]) -> Result<Journal, String> {
    let old: V2Journal = serde_json::from_slice(bytes).map_err(|_| "task_journal_invalid")?;
    if old.schema != "environment.task.v2" {
        return Err("task_journal_schema_unsupported".into());
    }
    let mut journal = Journal {
        poisoned: false,
        index_root: None,
        import_upgrade_owners: Vec::new(),
        schema: ENVIRONMENT_TASK_SCHEMA.into(),
        instance: old.instance,
        fence: old.fence.map(V2Fence::convert),
        takeover_pending: old.takeover_pending,
        closed_owner: old
            .closed_run
            .map(|run_id| EnvironmentTaskOwner::AgentRun { run_id }),
        tasks: old
            .tasks
            .into_iter()
            .map(|(id, record)| (id, record.convert()))
            .collect(),
        receipts: old.receipts,
    };
    journal.validate_import()?;
    let owners: Vec<_> = journal
        .tasks
        .values()
        .map(|record| record.creator_owner.clone())
        .chain(
            journal
                .fence
                .iter()
                .map(|fence| fence.controller_owner.clone()),
        )
        .collect();
    for owner in owners {
        remember_owner(&mut journal, &owner);
    }
    for (key, receipt) in &journal.receipts {
        if key.starts_with("endOperation:")
            || matches!(
                receipt.result,
                Some(EnvironmentTaskResult::OperationEnded { .. })
            )
        {
            return Err("task_journal_invalid".into());
        }
    }
    Ok(journal)
}

pub(super) fn task_identity_v2(
    fence: &EnvironmentTaskFence,
    operation_id: &str,
) -> Result<String, String> {
    let EnvironmentTaskOwner::AgentRun { run_id } = &fence.controller_owner else {
        return Err("task_index_invalid".into());
    };
    fence.validate_operation_id(operation_id)?;
    let (_, opaque) = operation_id
        .split_once(':')
        .ok_or("task_invalid_operation_id")?;
    Ok(format!(
        "task-{}",
        digest(&(&fence.environment_instance_id, run_id, opaque))?
    ))
}

pub(super) fn read_journal(bytes: &[u8]) -> Result<Journal, String> {
    let schema: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| "task_journal_invalid")?;
    match schema["schema"].as_str() {
        Some("environment.task.v2") => read_v2(bytes),
        Some(ENVIRONMENT_TASK_SCHEMA) => {
            serde_json::from_slice(bytes).map_err(|_| "task_journal_invalid".into())
        }
        _ => Err("task_journal_schema_unsupported".into()),
    }
}

fn preserve(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if path.exists() {
        if fs::read(path).map_err(|_| "task_migration_backup_unavailable")? != bytes {
            return Err("task_journal_history_conflict".into());
        }
        return Ok(());
    }
    let mut backup = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .map_err(|_| "task_migration_backup_unavailable")?;
    backup
        .write_all(bytes)
        .and_then(|_| backup.sync_all())
        .map_err(|_| "task_migration_backup_unavailable")?;
    #[cfg(unix)]
    File::open(path.parent().ok_or("task_migration_backup_unavailable")?)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| "task_migration_backup_unavailable")?;
    Ok(())
}

/// The service's exclusive control lease must be held by the caller.
pub(super) fn migrate_index(root: &Path, instance: &str) -> Result<(), String> {
    let path = root.join("history.sqlite");
    if !path.exists() {
        return Ok(());
    }
    let metadata = fs::symlink_metadata(&path).map_err(|_| "task_index_unavailable")?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("task_index_invalid".into());
    }
    let mut connection = Connection::open(&path).map_err(|_| "task_index_unavailable")?;
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(|_| "task_index_invalid")?;
    if version == 2 {
        return Ok(());
    }
    store::validate_version(&connection, 1)?;
    let value: Option<String> = connection
        .query_row(
            "SELECT value FROM metadata WHERE key='journal'",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| "task_index_invalid")?;
    let mut journal = if let Some(value) = value {
        read_v2(value.as_bytes())?
    } else {
        let count: i64 = connection.query_row("SELECT (SELECT COUNT(*) FROM tasks)+(SELECT COUNT(*) FROM operations)+(SELECT COUNT(*) FROM closed_runs)", [], |row| row.get(0)).map_err(|_| "task_index_invalid")?;
        if count != 0 {
            return Err("task_index_invalid".into());
        }
        Journal {
            poisoned: false,
            index_root: None,
            import_upgrade_owners: Vec::new(),
            schema: ENVIRONMENT_TASK_SCHEMA.into(),
            instance: instance.into(),
            fence: None,
            takeover_pending: false,
            closed_owner: None,
            tasks: BTreeMap::new(),
            receipts: BTreeMap::new(),
        }
    };
    if journal.instance != instance || !journal.tasks.is_empty() || !journal.receipts.is_empty() {
        return Err("task_index_invalid".into());
    }
    let mut records = BTreeMap::new();
    {
        let mut query = connection
            .prepare("SELECT id,record,live,hot FROM tasks ORDER BY id")
            .map_err(|_| "task_index_invalid")?;
        let mut rows = query.query([]).map_err(|_| "task_index_invalid")?;
        while let Some(row) = rows.next().map_err(|_| "task_index_invalid")? {
            let id: String = row.get(0).map_err(|_| "task_index_invalid")?;
            let text: String = row.get(1).map_err(|_| "task_index_invalid")?;
            let (digest, owner, epoch, operation_id, environment, state, exit_code, flags): (
                String,
                String,
                u64,
                String,
                bool,
                EnvironmentTaskState,
                Option<i32>,
                u8,
            ) = serde_json::from_str(&text).map_err(|_| "task_index_invalid")?;
            if flags > 3 {
                return Err("task_index_invalid".into());
            }
            let live: i64 = row.get(2).map_err(|_| "task_index_invalid")?;
            let hot: i64 = row.get(3).map_err(|_| "task_index_invalid")?;
            let record = V2Record {
                digest,
                creator_controller_id: owner.clone(),
                creator_epoch: epoch,
                snapshot: EnvironmentTaskSnapshot {
                    task_id: id.clone(),
                    operation_id,
                    scope: if environment {
                        EnvironmentTaskScope::Environment
                    } else {
                        EnvironmentTaskScope::Run { run_id: owner }
                    },
                    state,
                    exit_code,
                    cleanup_complete: flags & 1 != 0,
                    output_complete: flags & 2 != 0,
                },
            }
            .convert();
            record.validate(instance, &id)?;
            if !(0..=1).contains(&hot)
                || live != i64::from(!record.snapshot.cleanup_complete)
                || (live == 1 && hot == 0)
            {
                return Err("task_index_invalid".into());
            }
            remember_owner(&mut journal, &record.creator_owner);
            records.insert(id, (record, hot));
        }
    }
    let mut ended = Vec::new();
    {
        let mut query = connection
            .prepare("SELECT id FROM closed_runs")
            .map_err(|_| "task_index_invalid")?;
        for row in query
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|_| "task_index_invalid")?
        {
            let owner = EnvironmentTaskOwner::AgentRun {
                run_id: row.map_err(|_| "task_index_invalid")?,
            };
            EnvironmentTaskFence {
                environment_instance_id: instance.into(),
                controller_owner: owner.clone(),
                controller_epoch: 1,
                controller_purpose: EnvironmentTaskControllerPurpose::Cleanup,
            }
            .validate()?;
            ended.push(owner);
        }
    }
    // Keep receipt bytes and their hashed identities unchanged. Validate them before
    // changing the index so malformed state cannot be published as migrated history.
    #[derive(Deserialize)]
    enum V2Result {
        Mutated(String, EnvironmentTaskState, Option<i32>, u8),
        RunEnded(Vec<String>, bool),
    }
    {
        let mut query = connection
            .prepare("SELECT key,receipt,task_id FROM operations")
            .map_err(|_| "task_index_invalid")?;
        let mut rows = query.query([]).map_err(|_| "task_index_invalid")?;
        while let Some(row) = rows.next().map_err(|_| "task_index_invalid")? {
            let key: Vec<u8> = row.get(0).map_err(|_| "task_index_invalid")?;
            let text: String = row.get(1).map_err(|_| "task_index_invalid")?;
            let target: Option<Vec<u8>> = row.get(2).map_err(|_| "task_index_invalid")?;
            let (digest, result): (String, Option<V2Result>) =
                serde_json::from_str(&text).map_err(|_| "task_index_invalid")?;
            if key.len() != 32
                || target.as_ref().is_some_and(|value| value.len() != 32)
                || digest.len() != 64
                || !digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
                return Err("task_index_invalid".into());
            }
            match result {
                Some(V2Result::Mutated(id, state, exit, flags)) => {
                    let _ = (state, exit);
                    if flags > 3 || !records.contains_key(&id) {
                        return Err("task_index_invalid".into());
                    }
                }
                Some(V2Result::RunEnded(ids, complete)) => {
                    let _ = complete;
                    if ids.len() > ENVIRONMENT_TASK_MAX_RECORDS {
                        return Err("task_index_invalid".into());
                    }
                }
                None => {}
            }
        }
    }
    let original = fs::read(&path).map_err(|_| "task_index_unavailable")?;
    let backup = root.join("history.v1.sqlite");
    let backup_extra = if backup.exists() {
        0
    } else {
        original.len() as u64
    };
    // The capped replacement plus its rollback growth share the private control
    // budget with the original source and its immutable backup.
    if store::bytes(root)?
        .saturating_add(backup_extra)
        .saturating_add(INDEX_BYTES as u64)
        > CONTROL_ROOT_BYTES as u64
    {
        return Err("task_migration_capacity".into());
    }
    preserve(&backup, &original)?;
    connection
        .execute_batch("PRAGMA synchronous=FULL;")
        .map_err(|_| "task_index_unavailable")?;
    let page_size: usize = connection
        .pragma_query_value(None, "page_size", |row| row.get(0))
        .map_err(|_| "task_index_invalid")?;
    connection
        .pragma_update(None, "max_page_count", INDEX_BYTES / page_size)
        .map_err(|_| "task_migration_capacity")?;
    let tx = connection
        .transaction()
        .map_err(|_| "task_index_unavailable")?;
    tx.execute_batch(
        "CREATE TABLE owner_barriers(owner TEXT PRIMARY KEY,reason TEXT NOT NULL) WITHOUT ROWID;",
    )
    .map_err(|_| "task_journal_write_failed")?;
    for owner in ended {
        tx.execute(
            "INSERT OR IGNORE INTO owner_barriers VALUES(?1,'ended')",
            [serde_json::to_string(&owner).map_err(|_| "task_encoding_failed")?],
        )
        .map_err(|_| "task_journal_write_failed")?;
    }
    for owner in &journal.import_upgrade_owners {
        tx.execute(
            "INSERT OR IGNORE INTO owner_barriers VALUES(?1,'upgradeRequired')",
            [serde_json::to_string(owner).map_err(|_| "task_encoding_failed")?],
        )
        .map_err(|_| "task_journal_write_failed")?;
    }
    for (id, (record, _)) in records {
        tx.execute(
            "UPDATE tasks SET record=?1 WHERE id=?2",
            params![store::record_text(&record)?, id],
        )
        .map_err(|_| "task_journal_write_failed")?;
    }
    tx.execute("INSERT INTO metadata VALUES('journal',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [serde_json::to_string(&journal).map_err(|_| "task_encoding_failed")?]).map_err(|_| "task_journal_write_failed")?;
    tx.execute_batch("DROP TABLE closed_runs; PRAGMA user_version=2;")
        .map_err(|_| "task_journal_write_failed")?;
    tx.commit().map_err(|_| "task_journal_commit_failed".into())
}
