use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

const LIVE_TEXT_SCHEMA: &str = "runtime.live_text.v1";
const MAX_LIVE_TEXT_DIAGNOSTICS: usize = 128;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveTextJournalKey {
    pub session_id: String,
    pub turn_id: String,
    pub agent_run_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LiveTextOperation {
    Append { text: String },
    Replace { text: String },
}

#[derive(Debug)]
pub struct LiveTextJournal {
    path: PathBuf,
    file: File,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveredLiveText {
    pub key: LiveTextJournalKey,
    pub content: String,
    path: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveTextJournalDiagnostic {
    pub code: &'static str,
    pub path: String,
    pub agent_run_id: Option<String>,
    pub message: String,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct JournalHeader {
    schema: String,
    kind: String,
    session_id: String,
    turn_id: String,
    agent_run_id: String,
}

impl LiveTextJournal {
    pub fn create(root: &Path, key: LiveTextJournalKey) -> Result<Self, String> {
        let key = normalize_key(key)?;
        fs::create_dir_all(root).map_err(|error| {
            format!(
                "create live text journal directory {} failed: {error}",
                root.display()
            )
        })?;
        let path = root.join(format!("{}.live", key.agent_run_id));
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(path.as_path())
            .map_err(|error| {
                format!(
                    "create live text journal {} failed: {error}",
                    path.display()
                )
            })?;
        write_record(
            &mut file,
            &JournalHeader {
                schema: LIVE_TEXT_SCHEMA.to_string(),
                kind: "header".to_string(),
                session_id: key.session_id,
                turn_id: key.turn_id,
                agent_run_id: key.agent_run_id,
            },
        )?;
        file.sync_data().map_err(|error| {
            format!("sync live text journal {} failed: {error}", path.display())
        })?;
        Ok(Self { path, file })
    }

    pub fn append(&mut self, operations: &[LiveTextOperation]) -> Result<(), String> {
        if operations.is_empty() {
            return Ok(());
        }
        for operation in operations {
            write_operation(&mut self.file, operation)?;
        }
        self.file.sync_data().map_err(|error| {
            format!(
                "sync live text journal {} failed: {error}",
                self.path.display()
            )
        })
    }

    pub fn seal(self) -> Result<(), String> {
        let path = self.path.clone();
        drop(self.file);
        fs::remove_file(path.as_path()).map_err(|error| {
            format!(
                "remove sealed live text journal {} failed: {error}",
                path.display()
            )
        })
    }

    #[cfg(test)]
    pub fn recover(root: &Path) -> Result<Vec<RecoveredLiveText>, String> {
        if !root.exists() {
            return Ok(Vec::new());
        }
        let mut recovered = Vec::new();
        for entry in fs::read_dir(root).map_err(|error| {
            format!(
                "read live text journal directory {} failed: {error}",
                root.display()
            )
        })? {
            let entry =
                entry.map_err(|error| format!("read live text journal entry failed: {error}"))?;
            let path = entry.path();
            if !path.is_file() {
                return Err(format!(
                    "live text journal entry is not a file: {}",
                    path.display()
                ));
            }
            if path.extension().and_then(|value| value.to_str()) != Some("live") {
                return Err(format!(
                    "live text journal has unsupported file name: {}",
                    path.display()
                ));
            }
            recovered.push(read_journal(path)?);
        }
        recovered.sort_by(|left, right| left.key.agent_run_id.cmp(&right.key.agent_run_id));
        Ok(recovered)
    }

    pub fn recover_isolated(
        root: &Path,
    ) -> Result<(Vec<RecoveredLiveText>, Vec<LiveTextJournalDiagnostic>), String> {
        if !root.exists() {
            return Ok((Vec::new(), Vec::new()));
        }
        let mut recovered = Vec::new();
        let mut diagnostics = Vec::new();
        for entry in fs::read_dir(root).map_err(|error| {
            format!(
                "read live text journal directory {} failed: {error}",
                root.display()
            )
        })? {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    push_journal_diagnostic(
                        &mut diagnostics,
                        "live_text_entry_unreadable",
                        root,
                        None,
                        error.to_string(),
                    );
                    continue;
                }
            };
            let path = entry.path();
            let agent_run_id = path
                .file_stem()
                .and_then(|value| value.to_str())
                .map(ToOwned::to_owned);
            let result = if !path.is_file() {
                Err(format!(
                    "live text journal entry is not a file: {}",
                    path.display()
                ))
            } else if path.extension().and_then(|value| value.to_str()) != Some("live") {
                Err(format!(
                    "live text journal has unsupported file name: {}",
                    path.display()
                ))
            } else {
                read_journal(path.clone())
            };
            match result {
                Ok(item) => recovered.push(item),
                Err(error) => push_journal_diagnostic(
                    &mut diagnostics,
                    "live_text_journal_invalid",
                    path.as_path(),
                    agent_run_id,
                    error,
                ),
            }
        }
        recovered.sort_by(|left, right| left.key.agent_run_id.cmp(&right.key.agent_run_id));
        Ok((recovered, diagnostics))
    }
}

fn push_journal_diagnostic(
    diagnostics: &mut Vec<LiveTextJournalDiagnostic>,
    code: &'static str,
    path: &Path,
    agent_run_id: Option<String>,
    message: String,
) {
    if diagnostics.len() >= MAX_LIVE_TEXT_DIAGNOSTICS {
        return;
    }
    diagnostics.push(LiveTextJournalDiagnostic {
        code,
        path: path.to_string_lossy().chars().take(1024).collect(),
        agent_run_id,
        message: message.chars().take(1024).collect(),
    });
}

impl RecoveredLiveText {
    pub fn seal(self) -> Result<(), String> {
        fs::remove_file(self.path.as_path()).map_err(|error| {
            format!(
                "remove recovered live text journal {} failed: {error}",
                self.path.display()
            )
        })
    }
}

fn write_record<TValue: Serialize>(file: &mut File, value: &TValue) -> Result<(), String> {
    let encoded = serde_json::to_string(value)
        .map_err(|error| format!("serialize live text journal record failed: {error}"))?;
    file.write_all(encoded.as_bytes())
        .and_then(|_| file.write_all(b"\n"))
        .map_err(|error| format!("write live text journal record failed: {error}"))
}

fn write_operation(file: &mut File, operation: &LiveTextOperation) -> Result<(), String> {
    let (kind, text) = match operation {
        LiveTextOperation::Append { text } => (b'A', text),
        LiveTextOperation::Replace { text } => (b'R', text),
    };
    let byte_length =
        u32::try_from(text.len()).map_err(|_| "live text operation exceeds 4 GiB".to_string())?;
    file.write_all(&[kind])
        .and_then(|_| file.write_all(byte_length.to_be_bytes().as_slice()))
        .and_then(|_| file.write_all(text.as_bytes()))
        .map_err(|error| format!("write live text journal operation failed: {error}"))
}

fn read_journal(path: PathBuf) -> Result<RecoveredLiveText, String> {
    let bytes = fs::read(path.as_path())
        .map_err(|error| format!("read live text journal {} failed: {error}", path.display()))?;
    let header_end = bytes
        .iter()
        .position(|byte| *byte == b'\n')
        .ok_or_else(|| {
            format!(
                "live text journal has an incomplete header: {}",
                path.display()
            )
        })?;
    let header_line = std::str::from_utf8(&bytes[..header_end]).map_err(|error| {
        format!(
            "decode live text journal header {} failed: {error}",
            path.display()
        )
    })?;
    let header: JournalHeader = serde_json::from_str(header_line).map_err(|error| {
        format!(
            "decode live text journal header {} failed: {error}",
            path.display()
        )
    })?;
    if header.schema != LIVE_TEXT_SCHEMA || header.kind != "header" {
        return Err(format!(
            "live text journal header is invalid: {}",
            path.display()
        ));
    }
    let key = normalize_key(LiveTextJournalKey {
        session_id: header.session_id,
        turn_id: header.turn_id,
        agent_run_id: header.agent_run_id,
    })?;
    if path.file_stem().and_then(|value| value.to_str()) != Some(key.agent_run_id.as_str()) {
        return Err(format!(
            "live text journal file identity mismatch: {}",
            path.display()
        ));
    }
    let mut content = String::new();
    let mut offset = header_end + 1;
    while bytes.len().saturating_sub(offset) >= 5 {
        let kind = bytes[offset];
        let byte_length = u32::from_be_bytes(
            bytes[offset + 1..offset + 5]
                .try_into()
                .expect("live text operation length has four bytes"),
        ) as usize;
        let text_start = offset + 5;
        let Some(text_end) = text_start.checked_add(byte_length) else {
            return Err(format!(
                "live text journal operation length overflows: {}",
                path.display()
            ));
        };
        if text_end > bytes.len() {
            break;
        }
        let text = std::str::from_utf8(&bytes[text_start..text_end]).map_err(|error| {
            format!(
                "decode live text journal operation {} failed: {error}",
                path.display()
            )
        })?;
        match kind {
            b'A' => content.push_str(text),
            b'R' => content = text.to_string(),
            _ => {
                return Err(format!(
                    "live text journal has unsupported operation kind: {}",
                    path.display()
                ));
            }
        }
        offset = text_end;
    }
    Ok(RecoveredLiveText { key, content, path })
}

fn normalize_key(key: LiveTextJournalKey) -> Result<LiveTextJournalKey, String> {
    Ok(LiveTextJournalKey {
        session_id: required_identifier(key.session_id.as_str(), "sessionId")?,
        turn_id: required_identifier(key.turn_id.as_str(), "turnId")?,
        agent_run_id: required_file_identifier(key.agent_run_id.as_str(), "agentRunId")?,
    })
}

fn required_identifier(value: &str, field: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(format!("{field} must not be empty"));
    }
    Ok(value.to_string())
}

fn required_file_identifier(value: &str, field: &str) -> Result<String, String> {
    let value = required_identifier(value, field)?;
    if value.contains(['/', '\\', ':']) {
        return Err(format!("{field} contains an unsupported path character"));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_root(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("centaeris-live-text-{label}-{nonce}"))
    }

    fn key() -> LiveTextJournalKey {
        LiveTextJournalKey {
            session_id: "chat-1".to_string(),
            turn_id: "turn-1".to_string(),
            agent_run_id: "agent-run-1".to_string(),
        }
    }

    #[test]
    fn recovers_append_and_replace_without_writing_session_history() {
        let root = test_root("recover");
        let mut journal = LiveTextJournal::create(root.as_path(), key()).expect("create journal");
        journal
            .append(&[
                LiveTextOperation::Append {
                    text: "old".to_string(),
                },
                LiveTextOperation::Replace {
                    text: "new".to_string(),
                },
                LiveTextOperation::Append {
                    text: " text".to_string(),
                },
            ])
            .expect("append operations");
        drop(journal);

        let recovered = LiveTextJournal::recover(root.as_path()).expect("recover journal");
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].key, key());
        assert_eq!(recovered[0].content, "new text");
        recovered
            .into_iter()
            .next()
            .expect("recovered journal")
            .seal()
            .expect("seal");
        assert!(LiveTextJournal::recover(root.as_path())
            .expect("recover sealed")
            .is_empty());
        fs::remove_dir_all(root).expect("remove test root");
    }

    #[test]
    fn framed_one_kib_batches_have_near_unity_retained_bytes() {
        let root = test_root("amplification");
        let text = "x".repeat(1024);
        let operations = vec![LiveTextOperation::Append { text: text.clone() }; 1024];
        let logical_bytes = text.len() * operations.len();
        let mut journal = LiveTextJournal::create(root.as_path(), key()).expect("create journal");
        journal.append(operations.as_slice()).expect("append text");
        let physical_bytes = fs::metadata(journal.path.as_path())
            .expect("journal metadata")
            .len();
        let amplification = physical_bytes as f64 / logical_bytes as f64;
        println!(
            "live journal retained bytes: logical={}, physical={}, amplification={amplification:.6}",
            logical_bytes, physical_bytes
        );
        assert!(
            amplification <= 1.01,
            "framed journal amplification is too high"
        );
        journal.seal().expect("seal journal");
        fs::remove_dir_all(root).expect("remove test root");
    }

    #[test]
    fn ignores_uncommitted_journal_tail() {
        let root = test_root("tail");
        let mut journal = LiveTextJournal::create(root.as_path(), key()).expect("create journal");
        journal
            .append(&[LiveTextOperation::Append {
                text: "confirmed".to_string(),
            }])
            .expect("append confirmed text");
        drop(journal);
        OpenOptions::new()
            .append(true)
            .open(root.join("agent-run-1.live"))
            .expect("open journal")
            .write_all(b"\x41\x00\x00")
            .expect("write uncommitted tail");

        let recovered = LiveTextJournal::recover(root.as_path()).expect("recover journal");
        assert_eq!(recovered[0].content, "confirmed");
        recovered
            .into_iter()
            .next()
            .expect("recovered journal")
            .seal()
            .expect("seal");
        fs::remove_dir_all(root).expect("remove test root");
    }

    #[test]
    fn complete_unknown_operation_fails_loudly() {
        let root = test_root("unknown-operation");
        let journal = LiveTextJournal::create(root.as_path(), key()).expect("create journal");
        drop(journal);
        OpenOptions::new()
            .append(true)
            .open(root.join("agent-run-1.live"))
            .expect("open journal")
            .write_all(b"\x58\x00\x00\x00\x01x")
            .expect("write unknown operation");

        assert!(LiveTextJournal::recover(root.as_path())
            .expect_err("unknown operation must fail")
            .contains("unsupported operation kind"));
        fs::remove_dir_all(root).expect("remove test root");
    }

    #[test]
    fn isolated_recovery_keeps_corrupt_journal_and_recovers_healthy_one() {
        let root = test_root("isolated");
        let journal = LiveTextJournal::create(root.as_path(), key()).expect("create journal");
        drop(journal);
        let corrupt = root.join("broken.live");
        fs::write(corrupt.as_path(), b"future schema\n").expect("write corrupt journal");

        let (recovered, diagnostics) =
            LiveTextJournal::recover_isolated(root.as_path()).expect("isolated recovery");
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].key.agent_run_id, "agent-run-1");
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code, "live_text_journal_invalid");
        assert_eq!(diagnostics[0].agent_run_id.as_deref(), Some("broken"));
        assert!(corrupt.is_file());
        fs::remove_dir_all(root).expect("remove test root");
    }
}
