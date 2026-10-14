//! Resident task supervisor. The Runtime controller is only a fenced client.
use centaeris_core::execution::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

#[cfg(any(target_os = "linux", test))]
mod instance;

const CONTROL_JOURNAL_BYTES: usize = 2 * 1024 * 1024;
// Ordinary admission cannot consume space needed for bounded task completion and controls.
#[cfg(any(target_os = "linux", test))]
const CONTROL_COMPLETION_RESERVE_BYTES: usize = 256 * 1024;
#[cfg(any(target_os = "linux", test))]
const INPUT_RECEIPT_LIMIT: usize = 256;
struct TaskStorage {
    control_root: PathBuf,
    output_root: PathBuf,
    output_budget_bytes: u64,
}
impl TaskStorage {
    fn new(
        control_root: PathBuf,
        output_root: PathBuf,
        output_budget_bytes: u64,
    ) -> Result<Self, String> {
        let control =
            fs::canonicalize(&control_root).map_err(|_| "task_control_root_unavailable")?;
        let output = if output_root.exists() {
            fs::canonicalize(&output_root).map_err(|_| "task_output_root_unavailable")?
        } else {
            fs::canonicalize(output_root.parent().ok_or("task_output_root_invalid")?)
                .map_err(|_| "task_output_root_invalid")?
                .join(output_root.file_name().ok_or("task_output_root_invalid")?)
        };
        if control.starts_with(&output) || output.starts_with(&control) {
            return Err("task_storage_roots_overlap".into());
        }
        Ok(Self {
            control_root,
            output_root,
            output_budget_bytes,
        })
    }
    fn open_output(&self, id: &str, slots: usize) -> Result<OutputLog, String> {
        if id.is_empty()
            || !id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err("task_output_identity_invalid".into());
        }
        let required = slots
            .checked_mul(HEADER + CHUNK)
            .ok_or("task_output_invalid_capacity")? as u64;
        let path = self.output_root.join(format!("{id}.output"));
        let mut used = 0u64;
        let mut existing = 0;
        for entry in fs::read_dir(&self.output_root).map_err(|_| "task_output_root_unavailable")? {
            let entry = entry.map_err(|_| "task_output_budget_unavailable")?;
            let metadata = entry
                .metadata()
                .map_err(|_| "task_output_budget_unavailable")?;
            if !metadata.is_file() {
                return Err("task_output_invalid_entry".into());
            }
            used = used
                .checked_add(metadata.len())
                .ok_or("task_output_capacity")?;
            if entry.path() == path {
                existing = metadata.len();
            }
        }
        if used
            .saturating_sub(existing)
            .checked_add(required)
            .ok_or("task_output_capacity")?
            > self.output_budget_bytes
        {
            return Err("task_output_capacity".into());
        }
        OutputLog::open(&path, slots)
    }
}

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::{relay_once, serve, serve_instance};

#[cfg(any(target_os = "linux", test))]
fn digest(value: &impl Serialize) -> Result<String, String> {
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(value).map_err(|_| "task_encoding_failed")?)
    ))
}
#[cfg(any(target_os = "linux", test))]
fn task_identity(fence: &EnvironmentTaskFence, operation_id: &str) -> Result<String, String> {
    fence.validate_operation_id(operation_id)?;
    let (_, opaque) = operation_id
        .split_once(':')
        .ok_or("task_invalid_operation_id")?;
    Ok(format!(
        "task-{}",
        digest(&(&fence.environment_instance_id, &fence.controller_id, opaque))?
    ))
}
#[cfg(any(target_os = "linux", test))]
fn input_identity(
    fence: &EnvironmentTaskFence,
    task_id: &str,
    operation_id: &str,
) -> Result<String, String> {
    fence.validate_operation_id(operation_id)?;
    let (_, opaque) = operation_id
        .split_once(':')
        .ok_or("task_invalid_operation_id")?;
    Ok(format!(
        "input:{task_id}:{}",
        digest(&(&fence.environment_instance_id, &fence.controller_id, opaque))?
    ))
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Record {
    digest: String,
    creator_controller_id: String,
    creator_epoch: u64,
    snapshot: EnvironmentTaskSnapshot,
}
#[cfg(any(target_os = "linux", test))]
impl Record {
    fn can_retire(&self, epoch: u64, controller_id: &str, owned_live: bool) -> bool {
        !owned_live
            && self.snapshot.cleanup_complete
            && matches!(
                self.snapshot.state,
                EnvironmentTaskState::Exited | EnvironmentTaskState::Unknown
            )
            && self.creator_epoch < epoch
            && self.creator_controller_id != controller_id
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Receipt {
    digest: String,
    result: Option<EnvironmentTaskResult>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Journal {
    #[serde(skip)]
    poisoned: bool,
    schema: String,
    instance: String,
    fence: Option<EnvironmentTaskFence>,
    takeover_pending: bool,
    closed_run: Option<String>,
    tasks: BTreeMap<String, Record>,
    receipts: BTreeMap<String, Receipt>,
}
impl Journal {
    #[cfg(test)]
    fn takeover_persist(&mut self, f: EnvironmentTaskFence, root: &Path) -> Result<(), String> {
        self.takeover(f)?;
        self.persist(root)
    }
    fn open(root: &Path, instance: &str) -> Result<Self, String> {
        let file = root.join("journal.json");
        let mut j = if file.exists() {
            if fs::metadata(&file)
                .map_err(|_| "task_journal_read_failed")?
                .len()
                > CONTROL_JOURNAL_BYTES as u64
            {
                return Err("task_journal_too_large".into());
            }
            let bytes = fs::read(file).map_err(|_| "task_journal_read_failed")?;
            let j: Self = serde_json::from_slice(&bytes).map_err(|_| "task_journal_invalid")?;
            if j.schema != ENVIRONMENT_TASK_SCHEMA || j.instance != instance {
                return Err("task_instance_changed".into());
            }
            j
        } else {
            Self {
                poisoned: false,
                schema: ENVIRONMENT_TASK_SCHEMA.into(),
                instance: instance.into(),
                fence: None,
                takeover_pending: false,
                closed_run: None,
                tasks: BTreeMap::new(),
                receipts: BTreeMap::new(),
            }
        };
        for r in j.tasks.values_mut() {
            if matches!(
                r.snapshot.state,
                EnvironmentTaskState::Accepted | EnvironmentTaskState::Running
            ) {
                r.snapshot.state = EnvironmentTaskState::Unknown;
            }
        }
        j.save(root)?;
        Ok(j)
    }
    fn save(&self, root: &Path) -> Result<(), String> {
        let bytes = serde_json::to_vec(self).map_err(|_| "task_journal_write_failed")?;
        if bytes.len() > CONTROL_JOURNAL_BYTES {
            return Err("task_journal_too_large".into());
        }
        let mut f = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(root.join("journal.pending"))
            .map_err(|_| "task_journal_write_failed")?;
        f.write_all(&bytes)
            .map_err(|_| "task_journal_write_failed")?;
        f.sync_all().map_err(|_| "task_journal_sync_failed")?;
        fs::rename(root.join("journal.pending"), root.join("journal.json"))
            .map_err(|_| "task_journal_commit_failed")?;
        #[cfg(unix)]
        File::open(root)
            .and_then(|f| f.sync_all())
            .map_err(|_| "task_journal_sync_failed")?;
        Ok(())
    }
    fn persist(&mut self, root: &Path) -> Result<(), String> {
        if self.poisoned {
            return Err("task_journal_unavailable".into());
        }
        let result = self.save(root);
        if result
            .as_ref()
            .is_err_and(|error| error != "task_journal_too_large")
        {
            self.poisoned = true;
        }
        result
    }
    fn takeover(&mut self, f: EnvironmentTaskFence) -> Result<(), String> {
        if self.poisoned {
            return Err("task_journal_unavailable".into());
        }
        if f.environment_instance_id != self.instance
            || f.controller_id.is_empty()
            || f.controller_id.len() > 128
            || f.controller_epoch == 0
        {
            return Err("task_invalid_fence".into());
        }
        if let Some(old) = &self.fence {
            if old == &f {
                return Ok(());
            }
            if old.controller_epoch >= f.controller_epoch {
                return Err("task_controller_fenced".into());
            }
        }
        if self
            .fence
            .as_ref()
            .is_none_or(|old| old.controller_id != f.controller_id)
        {
            self.receipts.clear();
            if self.closed_run.as_ref() != Some(&f.controller_id) {
                self.closed_run = None;
            }
        }
        self.fence = Some(f);
        Ok(())
    }
    #[cfg(any(target_os = "linux", test))]
    fn preflight(&self, extra: usize) -> Result<(), String> {
        let size = serde_json::to_vec(self)
            .map_err(|_| "task_encoding_failed")?
            .len();
        if size.saturating_add(extra) > CONTROL_JOURNAL_BYTES - CONTROL_COMPLETION_RESERVE_BYTES {
            Err("task_journal_capacity".into())
        } else {
            Ok(())
        }
    }
    #[cfg(any(target_os = "linux", test))]
    fn check_admission(&self) -> Result<(), String> {
        if self
            .fence
            .as_ref()
            .is_some_and(|f| self.closed_run.as_ref() == Some(&f.controller_id))
        {
            return Err("task_run_closed".into());
        }
        if self
            .tasks
            .values()
            .filter(|r| !r.snapshot.cleanup_complete)
            .count()
            >= ENVIRONMENT_TASK_MAX_ACTIVE
        {
            return Err("task_capacity".into());
        }
        if self.tasks.len() >= ENVIRONMENT_TASK_MAX_RECORDS {
            return Err("task_history_capacity".into());
        }
        Ok(())
    }
    #[cfg(any(target_os = "linux", test))]
    fn reserve_receipt(
        &mut self,
        key: &str,
        d: &str,
    ) -> Result<Option<EnvironmentTaskResult>, String> {
        if let Some(receipt) = self.receipts.get(key) {
            if receipt.digest != d {
                return Err("task_operation_conflict".into());
            }
            if let Some(result) = &receipt.result {
                return Ok(Some(result.clone()));
            }
            // Only exact-tree stop/Run closure may retry an indeterminate action: both
            // are idempotent and never spawn or deliver input.
            if key.starts_with("terminate:") || key.starts_with("endRun:") {
                return Ok(None);
            }
            return Err("task_operation_unknown".into());
        }
        if key.starts_with("input:")
            && self
                .receipts
                .keys()
                .filter(|key| key.starts_with("input:"))
                .count()
                >= INPUT_RECEIPT_LIMIT
        {
            return Err("task_input_receipt_capacity".into());
        }
        let mut candidate = self.clone();
        candidate.receipts.insert(
            key.into(),
            Receipt {
                digest: d.into(),
                result: None,
            },
        );
        if key.starts_with("input:") {
            candidate.preflight(4096)?;
        } else if serde_json::to_vec(&candidate)
            .map_err(|_| "task_encoding_failed")?
            .len()
            .saturating_add(if key.starts_with("endRun:") {
                ENVIRONMENT_TASK_MAX_RECORDS * 132 + 1024
            } else {
                4096
            })
            > CONTROL_JOURNAL_BYTES
        {
            return Err("task_journal_capacity".into());
        }
        self.receipts = candidate.receipts;
        Ok(None)
    }
    #[cfg(any(target_os = "linux", test))]
    fn retire_task(&mut self, id: &str, epoch: u64, owned_live: bool) -> bool {
        let eligible = self.fence.as_ref().is_some_and(|f| {
            self.tasks
                .get(id)
                .is_some_and(|r| r.can_retire(epoch, &f.controller_id, owned_live))
        });
        if eligible {
            self.tasks.remove(id);
        }
        eligible
    }
    fn check(&self, f: &EnvironmentTaskFence) -> Result<(), String> {
        if self.poisoned {
            return Err("task_journal_unavailable".into());
        }
        if self.takeover_pending {
            return Err("task_takeover_pending".into());
        }
        if self.fence.as_ref() == Some(f) {
            Ok(())
        } else {
            Err("task_controller_fenced".into())
        }
    }
    fn existing(&self, id: &str, d: &str) -> Result<Option<EnvironmentTaskSnapshot>, String> {
        match self.tasks.get(id) {
            Some(r) if r.digest == d => Ok(Some(r.snapshot.clone())),
            Some(_) => Err("task_operation_conflict".into()),
            None => Ok(None),
        }
    }
}

const CHUNK: usize = 4096;
const HEADER: usize = 8 + 2 + 1 + 32;
/// Fixed-size ring; overwritten or torn records are visible as cursor gaps.
struct OutputLog {
    file: File,
    slots: usize,
    next: u64,
    corrupt: bool,
}
impl OutputLog {
    fn open(path: &Path, slots: usize) -> Result<Self, String> {
        if slots == 0 {
            return Err("task_output_invalid_capacity".into());
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(|_| "task_output_open_failed")?;
        let size = (slots * (HEADER + CHUNK)) as u64;
        if file
            .metadata()
            .map_err(|_| "task_output_read_failed")?
            .len()
            > size
        {
            return Err("task_output_invalid_size".into());
        }
        file.set_len(size)
            .map_err(|_| "task_output_resize_failed")?;
        let mut log = Self {
            file,
            slots,
            next: 0,
            corrupt: false,
        };
        for i in 0..slots {
            if let Some(c) = log.slot(i)? {
                log.next = log.next.max(c.cursor + 1);
            }
        }
        Ok(log)
    }
    fn slot(&mut self, slot: usize) -> Result<Option<EnvironmentTaskOutputChunk>, String> {
        self.file
            .seek(SeekFrom::Start((slot * (HEADER + CHUNK)) as u64))
            .map_err(|_| "task_output_read_failed")?;
        let mut header = [0; HEADER];
        self.file
            .read_exact(&mut header)
            .map_err(|_| "task_output_read_failed")?;
        let len = u16::from_le_bytes([header[8], header[9]]) as usize;
        if len == 0 || len > CHUNK || header[10] > 1 {
            if header.iter().any(|byte| *byte != 0) {
                self.corrupt = true;
            }
            return Ok(None);
        }
        let mut data = vec![0; len];
        self.file
            .read_exact(&mut data)
            .map_err(|_| "task_output_read_failed")?;
        let mut hasher = Sha256::new();
        hasher.update(&header[..11]);
        hasher.update(&data);
        if hasher.finalize().as_slice() != &header[11..] {
            self.corrupt = true;
            return Ok(None);
        }
        Ok(Some(EnvironmentTaskOutputChunk {
            cursor: u64::from_le_bytes(header[..8].try_into().expect("fixed header")),
            stream: if header[10] == 0 {
                EnvironmentTaskStream::Stdout
            } else {
                EnvironmentTaskStream::Stderr
            },
            data,
        }))
    }
    fn append(&mut self, stream: EnvironmentTaskStream, bytes: &[u8]) -> Result<(), String> {
        for data in bytes.chunks(CHUNK) {
            if data.is_empty() {
                continue;
            }
            let mut header = [0; HEADER];
            header[..8].copy_from_slice(&self.next.to_le_bytes());
            header[8..10].copy_from_slice(&(data.len() as u16).to_le_bytes());
            header[10] = if stream == EnvironmentTaskStream::Stdout {
                0
            } else {
                1
            };
            let mut h = Sha256::new();
            h.update(&header[..11]);
            h.update(data);
            header[11..].copy_from_slice(&h.finalize());
            self.file
                .seek(SeekFrom::Start(
                    ((self.next % self.slots as u64) as usize * (HEADER + CHUNK)) as u64,
                ))
                .map_err(|_| "task_output_write_failed")?;
            self.file
                .write_all(&header)
                .and_then(|_| self.file.write_all(data))
                .and_then(|_| self.file.sync_data())
                .map_err(|_| "task_output_write_failed")?;
            self.next = self
                .next
                .checked_add(1)
                .ok_or("task_output_cursor_exhausted")?;
        }
        Ok(())
    }
    fn read(
        &mut self,
        cursor: u64,
        max: usize,
    ) -> Result<(Vec<EnvironmentTaskOutputChunk>, u64, u64, bool, bool), String> {
        if cursor > self.next || !(CHUNK..=ENVIRONMENT_TASK_READ_BYTES).contains(&max) {
            return Err("task_invalid_output_cursor".into());
        }
        let earliest = self.next.saturating_sub(self.slots as u64);
        let mut at = cursor.max(earliest);
        let mut gap = cursor < earliest || self.corrupt;
        let mut bytes = 0;
        let mut out = Vec::new();
        while at < self.next {
            if let Some(c) = self.slot((at % self.slots as u64) as usize)? {
                if c.cursor == at {
                    if bytes + c.data.len() > max {
                        break;
                    }
                    bytes += c.data.len();
                    out.push(c);
                } else {
                    gap = true;
                }
            } else {
                gap = true;
            }
            at += 1;
        }
        Ok((out, earliest, at, gap, at < self.next))
    }
}

fn task_query_result(
    task: EnvironmentTaskSnapshot,
    log: Option<&mut OutputLog>,
    cursor: u64,
    max: usize,
) -> Result<EnvironmentTaskResult, String> {
    if !(CHUNK..=ENVIRONMENT_TASK_READ_BYTES).contains(&max) {
        return Err("task_invalid_output_cursor".into());
    }
    let retained = match log {
        Some(log) => log.read(cursor, max),
        None => Err("task_output_unavailable".into()),
    };
    let (output, earliest_cursor, next_cursor, gap, has_more) = match retained {
        Ok(output) => output,
        Err(error) if error == "task_invalid_output_cursor" => return Err(error),
        Err(_) => (Vec::new(), cursor, cursor, true, false),
    };
    Ok(EnvironmentTaskResult::Task {
        task,
        output,
        earliest_cursor,
        next_cursor,
        gap,
        has_more,
    })
}
#[cfg(test)]
mod tests;
