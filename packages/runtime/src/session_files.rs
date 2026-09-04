use std::collections::HashSet;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};

use crate::message_log;
use centaeris_core::session::{
    reduce_events, SessionManifestV1, SessionMetadataV1, SessionRecordType,
};
use serde::Serialize;

static SESSION_FILES_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
const MAX_SESSION_FILE_DIAGNOSTICS: usize = 128;

#[derive(Clone, Debug)]
pub struct SessionFileItem {
    pub id: String,
    pub title: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub last_message: Option<String>,
    pub cwd: String,
    pub session_path: String,
    pub session_kind: String,
    pub parent_session_id: Option<String>,
    pub runtime_job_id: Option<String>,
    pub sort_order: Option<i64>,
    pub is_pinned: bool,
    pub is_unread: bool,
    pub message_count: usize,
}

#[derive(Clone, Debug, Default)]
pub struct SessionMetadataPatch {
    pub title: Option<String>,
    pub is_pinned: Option<bool>,
    pub is_unread: Option<bool>,
}

#[derive(Clone, Debug)]
pub struct SessionFiles {
    sessions_dir: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionFileDiagnostic {
    pub code: &'static str,
    pub session_id: Option<String>,
    pub path: String,
    pub message: String,
}

impl SessionFiles {
    pub fn new(sessions_dir: PathBuf) -> Self {
        Self { sessions_dir }
    }

    pub fn list(&self) -> Result<Vec<SessionFileItem>, String> {
        let _guard = lock_session_files()?;
        let mut items = self.load_all()?;
        items.sort_by(|left, right| {
            resolved_sort_order(left)
                .cmp(&resolved_sort_order(right))
                .then_with(|| right.updated_at.cmp(&left.updated_at))
        });
        Ok(items)
    }

    pub fn diagnostics(&self) -> Result<Vec<SessionFileDiagnostic>, String> {
        let _guard = lock_session_files()?;
        let (_, mut diagnostics) = self.load_all_isolated()?;
        diagnostics.sort_by(|left, right| {
            left.code
                .cmp(right.code)
                .then_with(|| left.session_id.cmp(&right.session_id))
                .then_with(|| left.path.cmp(&right.path))
        });
        Ok(diagnostics)
    }

    pub fn create(
        &self,
        title: Option<&str>,
        cwd: &str,
        now_ms: i64,
    ) -> Result<SessionFileItem, String> {
        validate_positive_timestamp(now_ms)?;
        let cwd = normalize_cwd(Path::new(cwd))?;
        let _guard = lock_session_files()?;
        let items = self.load_all()?;
        for _ in 0..16 {
            let session_id = random_session_id()?;
            let relative_path = session_log_relative_path(session_id.as_str(), now_ms)?;
            let path = self.path_from_relative(relative_path.as_str())?;
            let metadata = SessionMetadataV1 {
                record_id: String::new(),
                title: normalize_title(title),
                cwd: cwd.clone(),
                session_kind: "main".to_string(),
                parent_session_id: None,
                runtime_job_id: None,
                sort_order: Some(next_sort_order(items.as_slice(), false)),
                is_pinned: false,
                is_unread: false,
            };
            let manifest = SessionManifestV1::new(
                session_id,
                now_ms,
                centaeris_core::runtime::CORE_PROTOCOL_VERSION,
            )
            .map_err(|error| error.to_string())?;
            match message_log::create_session_document(path.as_path(), manifest, metadata) {
                Ok(()) => return self.load_path(path.as_path()),
                Err(message_log::CreateSessionDocumentError::AlreadyExists) => continue,
                Err(error) => return Err(error.into_string()),
            }
        }
        Err("create Session exhausted random identity retries".to_string())
    }

    pub fn create_agent_session(
        &self,
        session_id: &str,
        parent_session_id: &str,
        runtime_job_id: &str,
        title: &str,
        cwd: &str,
        now_ms: i64,
    ) -> Result<SessionFileItem, String> {
        let session_id = required_session_id(session_id)?;
        sanitize_path_segment(session_id.as_str())?;
        let parent_session_id = required_session_id(parent_session_id)?;
        let runtime_job_id = required_text(runtime_job_id, "runtimeJobId")?;
        validate_positive_timestamp(now_ms)?;
        let cwd = normalize_cwd(Path::new(cwd))?;
        let _guard = lock_session_files()?;
        let items = self.load_all()?;
        let parent = items
            .iter()
            .find(|item| item.id == parent_session_id)
            .ok_or_else(|| format!("parent session not found: {parent_session_id}"))?;
        if parent.session_kind != "main" {
            return Err(format!(
                "subagent session parent must be main: {parent_session_id}"
            ));
        }
        if let Some(existing) = items.iter().find(|item| item.id == session_id) {
            if existing.session_kind == "subagent"
                && existing.parent_session_id.as_deref() == Some(parent_session_id.as_str())
                && existing.runtime_job_id.as_deref() == Some(runtime_job_id.as_str())
            {
                return Ok(existing.clone());
            }
            return Err(format!("session identity conflict: {session_id}"));
        }
        let relative_path = session_log_relative_path(session_id.as_str(), now_ms)?;
        let path = self.path_from_relative(relative_path.as_str())?;
        let metadata = SessionMetadataV1 {
            record_id: String::new(),
            title: normalize_title(Some(title)),
            cwd,
            session_kind: "subagent".to_string(),
            parent_session_id: Some(parent_session_id),
            runtime_job_id: Some(runtime_job_id),
            sort_order: None,
            is_pinned: false,
            is_unread: false,
        };
        let manifest = SessionManifestV1::new(
            session_id,
            now_ms,
            centaeris_core::runtime::CORE_PROTOCOL_VERSION,
        )
        .map_err(|error| error.to_string())?;
        message_log::create_session_document(path.as_path(), manifest, metadata)
            .map_err(message_log::CreateSessionDocumentError::into_string)?;
        self.load_path(path.as_path())
    }

    pub fn get(&self, session_id: &str) -> Result<SessionFileItem, String> {
        let session_id = required_session_id(session_id)?;
        let _guard = lock_session_files()?;
        self.load_all()?
            .into_iter()
            .find(|item| item.id == session_id)
            .ok_or_else(|| format!("session not found: {session_id}"))
    }

    pub fn deletion_items(&self, session_id: &str) -> Result<Vec<SessionFileItem>, String> {
        let session_id = required_session_id(session_id)?;
        let _guard = lock_session_files()?;
        deletion_items(self.load_all()?.as_slice(), session_id.as_str())
    }

    pub fn delete(&self, session_id: &str) -> Result<Vec<SessionFileItem>, String> {
        let session_id = required_session_id(session_id)?;
        let _guard = lock_session_files()?;
        let removed = deletion_items(self.load_all()?.as_slice(), session_id.as_str())?;
        for item in &removed {
            let path = self.path_from_relative(item.session_path.as_str())?;
            message_log::delete_session_document(path.as_path(), item.id.as_str())?;
        }
        Ok(removed)
    }

    pub fn update(
        &self,
        session_id: &str,
        patch: SessionMetadataPatch,
        now_ms: i64,
    ) -> Result<SessionFileItem, String> {
        validate_positive_timestamp(now_ms)?;
        let session_id = required_session_id(session_id)?;
        let _guard = lock_session_files()?;
        let items = self.load_all()?;
        let mut item = items
            .iter()
            .find(|item| item.id == session_id)
            .cloned()
            .ok_or_else(|| format!("session not found: {session_id}"))?;
        let mut changed = false;
        if let Some(title) = patch.title.as_deref() {
            let title = normalize_title(Some(title));
            if item.title != title {
                item.title = title;
                changed = true;
            }
        }
        if let Some(is_pinned) = patch.is_pinned {
            if item.is_pinned != is_pinned {
                item.is_pinned = is_pinned;
                item.sort_order = Some(next_sort_order(items.as_slice(), is_pinned));
                changed = true;
            }
        }
        if let Some(is_unread) = patch.is_unread {
            if item.is_unread != is_unread {
                item.is_unread = is_unread;
                changed = true;
            }
        }
        if changed {
            self.append_metadata(&item, now_ms)?;
            item.updated_at = item.updated_at.max(now_ms);
        }
        Ok(item)
    }

    pub fn reorder(
        &self,
        section: &str,
        ordered_session_ids: &[String],
        now_ms: i64,
    ) -> Result<Vec<SessionFileItem>, String> {
        let section = normalize_section(section)?;
        validate_positive_timestamp(now_ms)?;
        let ordered_ids = ordered_session_ids
            .iter()
            .map(|item| item.trim().to_string())
            .filter(|item| !item.is_empty())
            .collect::<Vec<_>>();
        if ordered_ids.is_empty() {
            return Ok(Vec::new());
        }
        if ordered_ids.iter().collect::<HashSet<_>>().len() != ordered_ids.len() {
            return Err("orderedSessionIds must not contain duplicates".to_string());
        }
        let _guard = lock_session_files()?;
        let mut items = self.load_all()?;
        for session_id in &ordered_ids {
            let item = items
                .iter()
                .find(|item| item.id == *session_id)
                .ok_or_else(|| format!("sessionId not found: {session_id}"))?;
            if section_key(item) != section {
                return Err(format!(
                    "sessionId cannot be moved across sections: {session_id}"
                ));
            }
        }
        for (index, session_id) in ordered_ids.iter().enumerate() {
            let item = items
                .iter_mut()
                .find(|item| item.id == *session_id)
                .expect("validated session id");
            let next = index as i64;
            if item.sort_order != Some(next) {
                item.sort_order = Some(next);
                self.append_metadata(item, now_ms)?;
                item.updated_at = item.updated_at.max(now_ms);
            }
        }
        let mut section_items = items
            .into_iter()
            .filter(|item| section_key(item) == section)
            .collect::<Vec<_>>();
        section_items.sort_by_key(resolved_sort_order);
        Ok(section_items)
    }

    pub fn record_activity(
        &self,
        session_id: &str,
        title_candidate: Option<&str>,
        _last_message: Option<&str>,
        updated_at_ms: i64,
    ) -> Result<(), String> {
        validate_positive_timestamp(updated_at_ms)?;
        let session_id = required_session_id(session_id)?;
        let _guard = lock_session_files()?;
        let mut item = self
            .load_all()?
            .into_iter()
            .find(|item| item.id == session_id)
            .ok_or_else(|| format!("session not found: {session_id}"))?;
        if should_replace_default_title(item.title.as_str()) {
            if let Some(title) = title_candidate
                .map(compact_title)
                .filter(|value| !value.trim().is_empty())
            {
                item.title = title;
                self.append_metadata(&item, updated_at_ms)?;
            }
        }
        Ok(())
    }

    fn load_all(&self) -> Result<Vec<SessionFileItem>, String> {
        message_log::cleanup_orphan_observation_content_directories(self.sessions_dir.as_path())?;
        let (items, diagnostics) = self.load_all_isolated()?;
        for diagnostic in diagnostics {
            eprintln!(
                "session_file_isolated: code={} sessionId={} path={} error={}",
                diagnostic.code,
                diagnostic.session_id.as_deref().unwrap_or("unknown"),
                diagnostic.path,
                diagnostic.message
            );
        }
        Ok(items)
    }

    fn load_all_isolated(
        &self,
    ) -> Result<(Vec<SessionFileItem>, Vec<SessionFileDiagnostic>), String> {
        let mut items = Vec::new();
        let mut diagnostics = Vec::new();
        for path in session_log_file_paths(self.sessions_dir.as_path())? {
            match self.load_path(path.as_path()) {
                Ok(item) => items.push(item),
                Err(error) => push_session_diagnostic(
                    &mut diagnostics,
                    "session_file_invalid",
                    path.file_stem()
                        .and_then(|value| value.to_str())
                        .map(ToOwned::to_owned),
                    path.to_string_lossy().replace('\\', "/"),
                    error,
                ),
            }
        }
        let mut session_ids = HashSet::new();
        items.retain(|item| {
            if session_ids.insert(item.id.clone()) {
                true
            } else {
                push_session_diagnostic(
                    &mut diagnostics,
                    "session_identity_duplicate",
                    Some(item.id.clone()),
                    item.session_path.clone(),
                    format!("duplicate sessionId in Session logs: {}", item.id),
                );
                false
            }
        });
        let main_session_ids = items
            .iter()
            .filter(|item| item.session_kind == "main")
            .map(|item| item.id.clone())
            .collect::<HashSet<_>>();
        items.retain(|item| {
            if item.session_kind != "subagent"
                || item
                    .parent_session_id
                    .as_ref()
                    .is_some_and(|parent| main_session_ids.contains(parent))
            {
                true
            } else {
                push_session_diagnostic(
                    &mut diagnostics,
                    "session_parent_missing",
                    Some(item.id.clone()),
                    item.session_path.clone(),
                    format!(
                        "subagent Session parent is missing or not main: {}",
                        item.id
                    ),
                );
                false
            }
        });
        Ok((items, diagnostics))
    }

    fn load_path(&self, path: &Path) -> Result<SessionFileItem, String> {
        let document = message_log::read_session_document(path)?;
        let first = document
            .records
            .first()
            .ok_or_else(|| format!("session_meta is missing: {}", path.display()))?;
        if first.event_type != SessionRecordType::SessionMeta {
            return Err(format!(
                "first Session record must be session_meta: {}",
                path.display()
            ));
        }
        let metadata = document
            .records
            .iter()
            .rfind(|record| record.event_type == SessionRecordType::SessionMeta)
            .ok_or_else(|| format!("session_meta is missing: {}", path.display()))
            .and_then(|record| {
                serde_json::from_value::<SessionMetadataV1>(record.payload.clone())
                    .map_err(|error| format!("decode session_meta failed: {error}"))
            })?;
        metadata.validate()?;
        let filename_id = path
            .file_stem()
            .and_then(|value| value.to_str())
            .ok_or_else(|| format!("Session log filename is invalid: {}", path.display()))?;
        if filename_id != document.manifest.session_id {
            return Err(format!(
                "Session manifest identity mismatch: {}",
                path.display()
            ));
        }
        let projection = reduce_events(
            document.manifest.session_id.as_str(),
            document.records.iter(),
        )?;
        let latest_message = projection
            .messages
            .values()
            .max_by_key(|message| message.updated_at_ms);
        let relative = path
            .strip_prefix(self.sessions_dir.as_path())
            .map_err(|_| {
                format!(
                    "Session log is outside sessions directory: {}",
                    path.display()
                )
            })?;
        Ok(SessionFileItem {
            id: document.manifest.session_id,
            title: metadata.title,
            created_at: document.manifest.created_at_ms,
            updated_at: document
                .records
                .iter()
                .map(|record| record.created_at_ms)
                .max()
                .unwrap_or(document.manifest.created_at_ms),
            last_message: latest_message.map(|message| compact_preview(message.text.as_str())),
            cwd: metadata.cwd,
            session_path: format!("sessions/{}", relative.to_string_lossy().replace('\\', "/")),
            session_kind: metadata.session_kind,
            parent_session_id: metadata.parent_session_id,
            runtime_job_id: metadata.runtime_job_id,
            sort_order: metadata.sort_order,
            is_pinned: metadata.is_pinned,
            is_unread: metadata.is_unread,
            message_count: projection.messages.len(),
        })
    }

    fn append_metadata(&self, item: &SessionFileItem, now_ms: i64) -> Result<(), String> {
        let path = self.path_from_relative(item.session_path.as_str())?;
        message_log::append_session_metadata(
            path.as_path(),
            item.id.as_str(),
            SessionMetadataV1 {
                record_id: String::new(),
                title: item.title.clone(),
                cwd: item.cwd.clone(),
                session_kind: item.session_kind.clone(),
                parent_session_id: item.parent_session_id.clone(),
                runtime_job_id: item.runtime_job_id.clone(),
                sort_order: item.sort_order,
                is_pinned: item.is_pinned,
                is_unread: item.is_unread,
            },
            now_ms,
        )
    }

    fn path_from_relative(&self, relative_path: &str) -> Result<PathBuf, String> {
        let relative = relative_path
            .strip_prefix("sessions/")
            .ok_or_else(|| "sessionPath must start with sessions/".to_string())?;
        Ok(self
            .sessions_dir
            .join(relative.replace('/', std::path::MAIN_SEPARATOR_STR)))
    }
}

fn push_session_diagnostic(
    diagnostics: &mut Vec<SessionFileDiagnostic>,
    code: &'static str,
    session_id: Option<String>,
    path: String,
    message: String,
) {
    if diagnostics.len() >= MAX_SESSION_FILE_DIAGNOSTICS {
        return;
    }
    diagnostics.push(SessionFileDiagnostic {
        code,
        session_id,
        path: path.chars().take(1024).collect(),
        message: message.chars().take(1024).collect(),
    });
}

fn deletion_items(
    items: &[SessionFileItem],
    session_id: &str,
) -> Result<Vec<SessionFileItem>, String> {
    let target = items
        .iter()
        .find(|item| item.id == session_id)
        .ok_or_else(|| format!("session not found: {session_id}"))?;
    Ok(items
        .iter()
        .filter(|item| {
            item.id == session_id
                || (target.session_kind == "main"
                    && item.parent_session_id.as_deref() == Some(session_id))
        })
        .cloned()
        .collect())
}

fn session_log_file_paths(sessions_dir: &Path) -> Result<Vec<PathBuf>, String> {
    if !sessions_dir.exists() {
        return Ok(Vec::new());
    }
    let mut paths = Vec::new();
    for year in fs::read_dir(sessions_dir)
        .map_err(|error| format!("read sessions directory failed: {error}"))?
    {
        let year = year.map_err(|error| format!("read sessions entry failed: {error}"))?;
        if !year.path().is_dir() {
            continue;
        }
        for month in fs::read_dir(year.path())
            .map_err(|error| format!("read sessions year directory failed: {error}"))?
        {
            let month =
                month.map_err(|error| format!("read sessions month entry failed: {error}"))?;
            if !month.path().is_dir() {
                continue;
            }
            for day in fs::read_dir(month.path())
                .map_err(|error| format!("read sessions month directory failed: {error}"))?
            {
                let day =
                    day.map_err(|error| format!("read sessions day entry failed: {error}"))?;
                if !day.path().is_dir() {
                    continue;
                }
                for file in fs::read_dir(day.path())
                    .map_err(|error| format!("read sessions day directory failed: {error}"))?
                {
                    let path = file
                        .map_err(|error| format!("read Session log entry failed: {error}"))?
                        .path();
                    if path.extension().and_then(|value| value.to_str()) == Some("jsonl") {
                        paths.push(path);
                    }
                }
            }
        }
    }
    paths.sort();
    Ok(paths)
}

fn normalize_cwd(path: &Path) -> Result<String, String> {
    if !path.is_dir() {
        return Err(format!(
            "working directory is not a directory: {}",
            path.display()
        ));
    }
    let canonical = fs::canonicalize(path)
        .map_err(|error| format!("canonicalize working directory failed: {error}"))?;
    let raw = canonical.to_string_lossy().to_string();
    if let Some(rest) = raw.strip_prefix(r"\\?\UNC\") {
        return Ok(format!(r"\\{rest}"));
    }
    Ok(raw.strip_prefix(r"\\?\").map(str::to_string).unwrap_or(raw))
}

fn required_session_id(raw: &str) -> Result<String, String> {
    required_text(raw, "sessionId")
}

fn required_text(raw: &str, field: &str) -> Result<String, String> {
    let value = raw.trim();
    if value.is_empty() {
        Err(format!("{field} is required"))
    } else {
        Ok(value.to_string())
    }
}

fn normalize_title(raw: Option<&str>) -> String {
    let title = raw.unwrap_or("").trim();
    if title.is_empty() {
        "New session".to_string()
    } else {
        title.to_string()
    }
}

fn compact_title(content: &str) -> String {
    let normalized = content.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= 32 {
        normalize_title(Some(normalized.as_str()))
    } else {
        normalize_title(Some(
            format!("{}...", normalized.chars().take(32).collect::<String>()).as_str(),
        ))
    }
}

fn compact_preview(content: &str) -> String {
    let normalized = content.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= 120 {
        normalized
    } else {
        format!("{}...", normalized.chars().take(120).collect::<String>())
    }
}

fn should_replace_default_title(title: &str) -> bool {
    title.trim().is_empty() || title.trim() == "New session"
}

fn normalize_section(section: &str) -> Result<&str, String> {
    match section.trim() {
        "pinned" => Ok("pinned"),
        "recent" => Ok("recent"),
        _ => Err("invalid session reorder section".to_string()),
    }
}

fn section_key(item: &SessionFileItem) -> &str {
    if item.is_pinned {
        "pinned"
    } else {
        "recent"
    }
}

fn resolved_sort_order(item: &SessionFileItem) -> i64 {
    item.sort_order.unwrap_or(-item.updated_at)
}

fn next_sort_order(items: &[SessionFileItem], is_pinned: bool) -> i64 {
    let section = if is_pinned { "pinned" } else { "recent" };
    items
        .iter()
        .filter(|item| section_key(item) == section)
        .filter_map(|item| item.sort_order)
        .min()
        .map(|value| value.saturating_sub(1))
        .unwrap_or(0)
}

fn session_log_relative_path(session_id: &str, epoch_ms: i64) -> Result<String, String> {
    let days = epoch_ms.div_euclid(86_400_000);
    let (year, month, day) = civil_from_days(days);
    Ok(format!(
        "sessions/{:02}/{month:02}/{day:02}/{}.jsonl",
        year.rem_euclid(100),
        sanitize_path_segment(session_id)?
    ))
}

fn sanitize_path_segment(value: &str) -> Result<String, String> {
    if value.is_empty()
        || !value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        return Err("sessionId contains invalid path characters".to_string());
    }
    Ok(value.to_string())
}

fn civil_from_days(days_since_unix_epoch: i64) -> (i32, u32, u32) {
    let z = days_since_unix_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    if month <= 2 {
        year += 1;
    }
    (year as i32, month as u32, day as u32)
}

fn validate_positive_timestamp(value: i64) -> Result<(), String> {
    if value <= 0 {
        Err("Session timestamp must be positive".to_string())
    } else {
        Ok(())
    }
}

fn random_session_id() -> Result<String, String> {
    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes)
        .map_err(|error| format!("generate Session identity failed: {error}"))?;
    let mut id = String::with_capacity(24);
    id.push_str("session-");
    for byte in bytes {
        write!(&mut id, "{byte:02x}").expect("writing to String cannot fail");
    }
    Ok(id)
}

fn lock_session_files() -> Result<MutexGuard<'static, ()>, String> {
    SESSION_FILES_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|_| "Session files lock poisoned".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jsonl_alone_restores_metadata_and_creates_no_lock_files() {
        let root = std::env::temp_dir().join(format!(
            "centaeris-session-files-{}-{}",
            std::process::id(),
            centaeris_core::runtime::contracts::current_timestamp_ms()
        ));
        let workspace = root.join("workspace");
        let sessions = root.join("sessions");
        fs::create_dir_all(workspace.as_path()).expect("workspace");
        let store = SessionFiles::new(sessions.clone());
        let created = store
            .create(
                Some("portable"),
                workspace.to_string_lossy().as_ref(),
                1_800_000_000_000,
            )
            .expect("create");
        let random_part = created
            .id
            .strip_prefix("session-")
            .expect("Session identity prefix");
        assert_eq!(random_part.len(), 16);
        assert!(random_part
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)));
        let updated = store
            .update(
                created.id.as_str(),
                SessionMetadataPatch {
                    is_pinned: Some(true),
                    ..SessionMetadataPatch::default()
                },
                1_800_000_000_001,
            )
            .expect("update");
        assert!(updated.is_pinned);

        let copy_root = root.join("copy").join("sessions");
        let source = store
            .path_from_relative(created.session_path.as_str())
            .expect("source");
        let destination = copy_root
            .join("26")
            .join("08")
            .join("banana")
            .join(format!("{}.jsonl", created.id));
        fs::create_dir_all(destination.parent().expect("destination parent")).expect("copy dir");
        fs::copy(&source, destination).expect("copy Session JSONL");
        let restored = SessionFiles::new(copy_root).list().expect("restore");
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].title, "portable");
        assert!(restored[0].is_pinned);
        assert_eq!(
            fs::read_dir(source.parent().expect("Session directory"))
                .expect("read Session directory")
                .count(),
            1
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn corrupt_session_is_preserved_and_does_not_block_list_or_create() {
        let root = std::env::temp_dir().join(format!(
            "centaeris-session-files-isolation-{}-{}",
            std::process::id(),
            centaeris_core::runtime::contracts::current_timestamp_ms()
        ));
        let workspace = root.join("workspace");
        let sessions = root.join("sessions");
        fs::create_dir_all(workspace.as_path()).expect("workspace");
        let store = SessionFiles::new(sessions);
        let healthy = store
            .create(
                Some("healthy"),
                workspace.to_string_lossy().as_ref(),
                1_800_000_100_000,
            )
            .expect("create healthy session");
        let healthy_path = store
            .path_from_relative(healthy.session_path.as_str())
            .expect("healthy path");
        let corrupt_path = healthy_path
            .parent()
            .expect("session day directory")
            .join("corrupt.jsonl");
        fs::write(
            corrupt_path.as_path(),
            "{\"schemaVersion\":\"future.v2\"}\n",
        )
        .expect("corrupt fixture");

        let (listed, diagnostics) = store.load_all_isolated().expect("isolated list");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, healthy.id);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code, "session_file_invalid");
        assert_eq!(diagnostics[0].session_id.as_deref(), Some("corrupt"));
        assert!(diagnostics[0].message.contains("unsupported"));
        assert_eq!(store.diagnostics().expect("query diagnostics"), diagnostics);
        assert!(corrupt_path.is_file(), "corrupt source must be preserved");

        let created_after = store
            .create(
                Some("created after corruption"),
                workspace.to_string_lossy().as_ref(),
                1_800_000_100_001,
            )
            .expect("create after corrupt session");
        assert_ne!(created_after.id, healthy.id);
        assert_eq!(store.list().expect("list healthy sessions").len(), 2);
        assert!(
            corrupt_path.is_file(),
            "listing must not remove corrupt source"
        );
        fs::remove_dir_all(root).expect("remove root");
    }
}
