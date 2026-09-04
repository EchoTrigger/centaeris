use serde::Serialize;
use serde_json::{json, Value};

use crate::execution::{ExecutionFileSystemError, ExecutionFileSystemErrorKind};
use crate::model::prepared_prompt::ModelInputImageSourceRefV1;
use crate::tool::{ToolErrorInfo, ToolFailureKind};

use super::{LocalToolError, LocalToolOutput, ToolExecutionFact};

pub(super) const FILE_TOOL_REJECTED_SCHEMA: &str = "file_tool_rejected_v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FileToolErrorKind {
    InvalidInput,
    ReadSnapshotMissing,
    ReadSnapshotMismatch,
    WriteConflict,
    EditTextMismatch,
    DurableCommitMissing,
    DurableCommitFailed,
    PermissionDenied,
    AssetRemoved,
    AccessRevoked,
    SourceDeleted,
    StaleGeneration,
    AssetUnavailable,
    ResolvedInputRequired,
    StaleInput,
    Io,
    Unknown,
}

impl FileToolErrorKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::InvalidInput => "invalid_input",
            Self::ReadSnapshotMissing => "read_snapshot_missing",
            Self::ReadSnapshotMismatch => "read_snapshot_mismatch",
            Self::WriteConflict => "write_conflict",
            Self::EditTextMismatch => "edit_text_mismatch",
            Self::DurableCommitMissing => "durable_commit_missing",
            Self::DurableCommitFailed => "durable_commit_failed",
            Self::PermissionDenied => "permission_denied",
            Self::AssetRemoved => "asset_removed",
            Self::AccessRevoked => "access_revoked",
            Self::SourceDeleted => "source_deleted",
            Self::StaleGeneration => "stale_generation",
            Self::AssetUnavailable => "asset_unavailable",
            Self::ResolvedInputRequired => "resolved_input_required",
            Self::StaleInput => "stale_input",
            Self::Io => "io",
            Self::Unknown => "unknown",
        }
    }

    fn tool_failure_kind(self) -> ToolFailureKind {
        match self {
            Self::PermissionDenied => ToolFailureKind::PermissionDenied,
            Self::Io
            | Self::WriteConflict
            | Self::DurableCommitMissing
            | Self::DurableCommitFailed => ToolFailureKind::HostUnavailable,
            Self::Unknown => ToolFailureKind::Unknown,
            Self::InvalidInput
            | Self::AssetRemoved
            | Self::AccessRevoked
            | Self::SourceDeleted
            | Self::StaleGeneration
            | Self::AssetUnavailable
            | Self::ResolvedInputRequired
            | Self::StaleInput
            | Self::ReadSnapshotMissing
            | Self::ReadSnapshotMismatch
            | Self::EditTextMismatch => ToolFailureKind::InvalidInput,
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct FileToolError {
    kind: FileToolErrorKind,
    message: String,
    model_path: Option<String>,
    host_diagnostic: Option<String>,
}

impl FileToolError {
    pub(super) fn new(kind: FileToolErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            model_path: None,
            host_diagnostic: None,
        }
    }

    pub(super) fn from_execution_host(error: ExecutionFileSystemError) -> Self {
        let kind = match error.kind {
            ExecutionFileSystemErrorKind::InvalidPath
            | ExecutionFileSystemErrorKind::NotFile
            | ExecutionFileSystemErrorKind::NotDirectory
            | ExecutionFileSystemErrorKind::UnsupportedEntry
            | ExecutionFileSystemErrorKind::TooLarge
            | ExecutionFileSystemErrorKind::NotFound => FileToolErrorKind::InvalidInput,
            ExecutionFileSystemErrorKind::PermissionDenied => FileToolErrorKind::PermissionDenied,
            ExecutionFileSystemErrorKind::AssetRemoved => FileToolErrorKind::AssetRemoved,
            ExecutionFileSystemErrorKind::AccessRevoked => FileToolErrorKind::AccessRevoked,
            ExecutionFileSystemErrorKind::SourceDeleted => FileToolErrorKind::SourceDeleted,
            ExecutionFileSystemErrorKind::StaleGeneration => FileToolErrorKind::StaleGeneration,
            ExecutionFileSystemErrorKind::Conflict => FileToolErrorKind::ReadSnapshotMismatch,
            ExecutionFileSystemErrorKind::HostUnavailable | ExecutionFileSystemErrorKind::Io => {
                FileToolErrorKind::Io
            }
        };
        Self {
            kind,
            message: error.message,
            model_path: None,
            host_diagnostic: error.diagnostic,
        }
    }

    pub(super) fn with_model_path(mut self, raw_path: &str) -> Self {
        self.model_path = provider_neutral_model_path(raw_path);
        self
    }

    pub(super) fn to_local_error(&self, tool_name: &str) -> LocalToolError {
        let tool_error = self.tool_error_info();
        let details = json!({
            "schema": FILE_TOOL_REJECTED_SCHEMA,
            "toolName": tool_name,
            "path": self.model_path.as_deref(),
            "message": self.message,
            "toolError": {
                "kind": tool_error.kind.as_str(),
                "fileErrorKind": self.kind.as_str(),
                "modelMessage": tool_error.model_message,
                "userMessage": tool_error.user_message,
                "diagnosticId": tool_error.diagnostic_id,
                "retryable": tool_error.retryable,
            },
            "fileFact": {
                "schema": "file_tool_rejected_fact_v1",
                "toolName": tool_name,
                "errorKind": self.kind.as_str(),
                "toolFailureKind": self.kind.tool_failure_kind().as_str(),
            },
            "hostDiagnostic": self.host_diagnostic,
        });
        LocalToolError::new(tool_error.model_message.clone(), details, tool_error)
    }

    fn tool_error_info(&self) -> ToolErrorInfo {
        let (model_message, user_message) = self.safe_messages();
        let retryable = matches!(
            self.kind,
            FileToolErrorKind::Io | FileToolErrorKind::WriteConflict
        );
        ToolErrorInfo::new(self.kind.tool_failure_kind(), model_message, user_message)
            .with_retryable(retryable)
    }

    fn safe_messages(&self) -> (String, String) {
        match self.kind {
            FileToolErrorKind::ReadSnapshotMissing => (
                "file mutation rejected; read the existing file before editing or overwriting it"
                    .to_string(),
                "File must be read before mutation".to_string(),
            ),
            FileToolErrorKind::ReadSnapshotMismatch => (
                "file mutation rejected; file changed since the last read, so re-read it and retry"
                    .to_string(),
                "File changed since last read".to_string(),
            ),
            FileToolErrorKind::WriteConflict => (
                "file write conflict; another runtime owner currently holds the file claim"
                    .to_string(),
                "File write conflict".to_string(),
            ),
            FileToolErrorKind::EditTextMismatch => (
                "Every edits[].old_text must match current file content exactly once without overlap; re-read and rebuild the atomic edit with enough surrounding context"
                    .to_string(),
                "Edit text did not apply".to_string(),
            ),
            FileToolErrorKind::DurableCommitMissing => (
                "file mutation commit port is missing; refusing to apply filesystem mutation without durable session log commit"
                    .to_string(),
                "File mutation commit port missing".to_string(),
            ),
            FileToolErrorKind::DurableCommitFailed => (
                "file mutation durable commit failed; filesystem mutation was not applied"
                    .to_string(),
                "File mutation commit failed".to_string(),
            ),
            FileToolErrorKind::PermissionDenied => (
                "file tool mutation was denied by policy or host permissions".to_string(),
                "File tool permission denied".to_string(),
            ),
            FileToolErrorKind::AssetRemoved => (
                "attached session file was deleted; ask the user to select another file".to_string(),
                "Attached session file was deleted".to_string(),
            ),
            FileToolErrorKind::AccessRevoked => (
                "access to the attached session file was revoked; continue without it or ask for new authorization"
                    .to_string(),
                "Attached session file access was revoked".to_string(),
            ),
            FileToolErrorKind::SourceDeleted => (
                "the Source containing this attached session file was deleted; continue without it or ask the user to select another file"
                    .to_string(),
                "Attached session file Source was deleted".to_string(),
            ),
            FileToolErrorKind::StaleGeneration => (
                "the attached Session file changed after this AgentRun was authorized; start a new AgentRun to use the new generation"
                    .to_string(),
                "Attached session file generation changed".to_string(),
            ),
            FileToolErrorKind::AssetUnavailable => (
                "attached session file is unavailable or no longer authorized".to_string(),
                "Attached session file is unavailable".to_string(),
            ),
            FileToolErrorKind::ResolvedInputRequired => (
                "Read target is not an authorized resolved input or workspace file".to_string(),
                "Read target is not authorized".to_string(),
            ),
            FileToolErrorKind::StaleInput => (
                "resolved input bytes changed after the AgentRun context was frozen".to_string(),
                "Authorized input changed; start a new AgentRun".to_string(),
            ),
            FileToolErrorKind::Io => (
                actionable_io_model_message(self.message.as_str(), self.model_path.as_deref()),
                "File tool I/O failed".to_string(),
            ),
            FileToolErrorKind::InvalidInput => invalid_input_messages(self.message.as_str()),
            FileToolErrorKind::Unknown => (
                "file tool execution encountered an unexpected error".to_string(),
                "File tool execution error".to_string(),
            ),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct FileReadOutcome {
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub total_lines: usize,
    pub total_bytes: usize,
    pub output_bytes: usize,
    pub max_lines: usize,
    pub max_bytes: usize,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncated_by: Option<&'static str>,
    pub first_line_exceeds_limit: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_offset: Option<usize>,
    pub file_hash: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub citation_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_start: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_end: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document_route: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document_used_ocr: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct FileReadBatchOutcome {
    pub schema: &'static str,
    pub items: Vec<FileReadOutcome>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct FileImageReadOutcome {
    pub schema: &'static str,
    pub path: String,
    pub content_type: String,
    pub byte_length: u64,
    pub width_px: u32,
    pub height_px: u32,
    pub file_hash: String,
    pub model_input_images: Vec<ModelInputImageSourceRefV1>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct DirectoryListEntryOutcome {
    pub path: String,
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct DirectoryListOutcome {
    pub schema: &'static str,
    pub path: String,
    pub recursive: bool,
    pub offset: usize,
    pub limit: usize,
    pub total_entries: usize,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_offset: Option<usize>,
    pub entries: Vec<DirectoryListEntryOutcome>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct FileWriteOutcome {
    pub schema: &'static str,
    pub path: String,
    pub created: bool,
    pub previous_file_hash: Option<String>,
    pub file_hash: String,
    pub bytes_written: usize,
    pub added_lines: usize,
    pub removed_lines: usize,
    pub diff_preview: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct FileEditOutcome {
    pub schema: &'static str,
    pub files_changed: usize,
    pub replacements_applied: usize,
    pub added_lines: usize,
    pub removed_lines: usize,
    pub diff_preview: String,
    pub operations: Vec<FileEditOperationOutcome>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct FileEditOperationOutcome {
    #[serde(rename = "type")]
    pub operation_type: &'static str,
    pub path: String,
    pub target_path: Option<String>,
    pub previous_file_hash: Option<String>,
    pub file_hash: Option<String>,
    pub added_lines: usize,
    pub removed_lines: usize,
}

#[derive(Debug, Clone)]
pub(super) enum FileToolOutcome {
    Read(Box<FileReadOutcome>),
    ReadBatch(FileReadBatchOutcome),
    ImageRead(FileImageReadOutcome),
    DirectoryList(DirectoryListOutcome),
    Write(FileWriteOutcome),
    Edit(FileEditOutcome),
}

impl FileToolOutcome {
    pub(super) fn into_local_output(self) -> LocalToolOutput {
        let content = self.model_content();
        let details = self.details();
        LocalToolOutput::success(content, details)
    }

    pub(super) fn into_read_output(
        self,
        tool_call_id: Option<&str>,
    ) -> Result<LocalToolOutput, String> {
        let facts = match &self {
            Self::Read(outcome) => read_citation_fact(outcome, tool_call_id)?
                .into_iter()
                .collect(),
            Self::ReadBatch(outcome) => outcome
                .items
                .iter()
                .filter_map(|item| read_citation_fact(item, tool_call_id).transpose())
                .collect::<Result<Vec<_>, _>>()?,
            _ => Vec::new(),
        };
        Ok(self.into_local_output().with_facts(facts))
    }

    fn details(&self) -> Value {
        let mut value =
            match self {
                Self::Read(outcome) => serde_json::to_value(outcome)
                    .expect("serialize FileReadOutcome should not fail"),
                Self::ReadBatch(outcome) => serde_json::to_value(outcome)
                    .expect("serialize FileReadBatchOutcome should not fail"),
                Self::ImageRead(outcome) => serde_json::to_value(outcome)
                    .expect("serialize FileImageReadOutcome should not fail"),
                Self::DirectoryList(outcome) => serde_json::to_value(outcome)
                    .expect("serialize DirectoryListOutcome should not fail"),
                Self::Write(outcome) => serde_json::to_value(outcome)
                    .expect("serialize FileWriteOutcome should not fail"),
                Self::Edit(outcome) => serde_json::to_value(outcome)
                    .expect("serialize FileEditOutcome should not fail"),
            };
        if let Some(object) = value.as_object_mut() {
            object.insert("fileFact".to_string(), self.file_fact());
        }
        value
    }

    fn model_content(&self) -> String {
        match self {
            Self::Read(outcome) => file_read_content(outcome),
            Self::ReadBatch(outcome) => outcome
                .items
                .iter()
                .map(file_read_content)
                .collect::<Vec<_>>()
                .join("\n\n"),
            Self::ImageRead(outcome) => format!(
                "Observed image {} ({}; {}x{}; {} bytes).",
                outcome.path,
                outcome.content_type,
                outcome.width_px,
                outcome.height_px,
                outcome.byte_length
            ),
            Self::DirectoryList(outcome) => directory_list_content(outcome),
            Self::Write(outcome) => format!(
                "Wrote {} ({} bytes; created={}).",
                outcome.path, outcome.bytes_written, outcome.created
            ),
            Self::Edit(outcome) => {
                let paths = outcome
                    .operations
                    .iter()
                    .map(|operation| operation.path.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(
                    "Atomically applied {} exact replacement(s) to {} file(s): {}. Added {} line(s); removed {} line(s).",
                    outcome.replacements_applied,
                    outcome.files_changed,
                    paths,
                    outcome.added_lines,
                    outcome.removed_lines
                )
            }
        }
    }

    fn file_fact(&self) -> Value {
        match self {
            Self::Read(outcome) => json!({
                "schema": "file_read_fact_v1",
                "toolName": "read",
                "path": outcome.path,
                "fileHash": outcome.file_hash,
                "startLine": outcome.start_line,
                "endLine": outcome.end_line,
                "totalLines": outcome.total_lines,
                "totalBytes": outcome.total_bytes,
                "outputBytes": outcome.output_bytes,
                "maxLines": outcome.max_lines,
                "maxBytes": outcome.max_bytes,
                "truncated": outcome.truncated,
                "truncatedBy": outcome.truncated_by,
                "firstLineExceedsLimit": outcome.first_line_exceeds_limit,
                "nextOffset": outcome.next_offset,
                "inputRef": outcome.input_ref,
                "displayName": outcome.display_name,
                "ownerRef": outcome.owner_ref,
                "ownerKind": outcome.owner_kind,
                "evidenceKind": outcome.evidence_kind,
                "ownerSha256": outcome.owner_sha256,
                "citationRef": outcome.citation_ref,
                "pageStart": outcome.page_start,
                "pageEnd": outcome.page_end,
                "documentRoute": outcome.document_route,
                "documentUsedOcr": outcome.document_used_ocr,
            }),
            Self::ReadBatch(outcome) => json!({
                "schema": "file_read_batch_fact_v1",
                "toolName": "read",
                "items": outcome.items.iter().map(read_file_fact).collect::<Vec<_>>(),
            }),
            Self::ImageRead(outcome) => json!({
                "schema": "file_image_read_fact_v1",
                "toolName": "read",
                "path": outcome.path,
                "contentType": outcome.content_type,
                "byteLength": outcome.byte_length,
                "widthPx": outcome.width_px,
                "heightPx": outcome.height_px,
                "fileHash": outcome.file_hash,
            }),
            Self::DirectoryList(outcome) => json!({
                "schema": "directory_listing_fact_v1",
                "toolName": "read",
                "path": outcome.path,
                "recursive": outcome.recursive,
                "offset": outcome.offset,
                "limit": outcome.limit,
                "totalEntries": outcome.total_entries,
                "truncated": outcome.truncated,
                "nextOffset": outcome.next_offset,
                "entries": outcome.entries,
            }),
            Self::Write(outcome) => json!({
                "schema": "file_write_fact_v1",
                "toolName": "write",
                "path": outcome.path,
                "created": outcome.created,
                "previousFileHash": outcome.previous_file_hash,
                "fileHash": outcome.file_hash,
                "bytesWritten": outcome.bytes_written,
            }),
            Self::Edit(outcome) => json!({
                "schema": "file_edit_fact_v1",
                "toolName": "edit",
                "filesChanged": outcome.files_changed,
                "replacementsApplied": outcome.replacements_applied,
                "addedLines": outcome.added_lines,
                "removedLines": outcome.removed_lines,
                "operations": outcome.operations,
            }),
        }
    }
}

fn read_citation_fact(
    outcome: &FileReadOutcome,
    tool_call_id: Option<&str>,
) -> Result<Option<ToolExecutionFact>, String> {
    let Some(citation_id) = outcome.citation_ref.as_deref() else {
        return Ok(None);
    };
    let tool_call_id = tool_call_id
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "Read citation is missing toolCallId".to_string())?;
    let locator = match (outcome.page_start, outcome.page_end) {
        (Some(start), Some(end)) => json!({"pageStart": start, "pageEnd": end}),
        _ => json!({
            "startLine": outcome.start_line,
            "endLine": outcome.end_line,
        }),
    };
    Ok(Some(ToolExecutionFact::CitationRecorded(json!({
        "citationId": citation_id,
        "inputRef": required_read_citation_value(&outcome.input_ref, "inputRef")?,
        "ownerRef": required_read_citation_value(&outcome.owner_ref, "ownerRef")?,
        "ownerKind": required_read_citation_value(&outcome.owner_kind, "ownerKind")?,
        "displayName": required_read_citation_value(&outcome.display_name, "displayName")?,
        "evidenceKind": required_read_citation_value(&outcome.evidence_kind, "evidenceKind")?,
        "ownerSha256": required_read_citation_value(&outcome.owner_sha256, "ownerSha256")?,
        "sourceToolName": "read",
        "sourceToolCallId": tool_call_id,
        "locator": locator,
    }))))
}

fn required_read_citation_value<'a>(
    value: &'a Option<String>,
    name: &str,
) -> Result<&'a str, String> {
    value
        .as_deref()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("Read citation is missing {name}"))
}

fn file_read_content(outcome: &FileReadOutcome) -> String {
    let window = match (outcome.page_start, outcome.page_end) {
        (Some(page_start), Some(page_end)) => format!(
            "pages {page_start}-{page_end}; lines {}-{} of {}",
            outcome.start_line, outcome.end_line, outcome.total_lines
        ),
        _ => format!(
            "lines {}-{} of {}",
            outcome.start_line, outcome.end_line, outcome.total_lines
        ),
    };
    let continuation = outcome
        .next_offset
        .map(|next_offset| read_continuation_call(outcome, next_offset));
    let mut summary = format!(
        "Read {}: {} ({} bytes returned; bounded to {} lines or {} bytes); truncated={}{}.",
        outcome.path,
        window,
        outcome.output_bytes,
        outcome.max_lines,
        outcome.max_bytes,
        outcome.truncated,
        outcome
            .truncated_by
            .map(|reason| format!(" by {reason}"))
            .unwrap_or_default()
    );
    if let Some(call) = continuation.as_deref() {
        summary.push_str(format!(" Continue with {call}.").as_str());
    }
    let mut sections = vec![summary];
    if outcome.first_line_exceeds_limit {
        sections.push(format!(
            "Line {} alone exceeds the {}-byte read limit. The read tool never returns partial lines; inspect this line with a byte-bounded bash command such as sed piped to head -c.",
            outcome.start_line, outcome.max_bytes
        ));
    } else if outcome.content.is_empty() {
        sections.push("(file range is empty)".to_string());
    } else {
        sections.push(outcome.content.clone());
    }
    if let Some(call) = continuation {
        sections.push(format!("Continuation: {call}."));
    } else if outcome.first_line_exceeds_limit {
        sections.push(format!(
            "Continuation: use bash for the oversized line, then continue later lines with read path={} and offset={}.",
            outcome.path, outcome.start_line
        ));
    } else {
        sections.push("Continuation: this file read is complete.".to_string());
    }
    sections.join("\n")
}

fn read_continuation_call(outcome: &FileReadOutcome, next_offset: usize) -> String {
    let (argument_name, argument_value) = outcome
        .input_ref
        .as_deref()
        .map(|input_ref| ("input_ref", input_ref))
        .unwrap_or(("path", outcome.path.as_str()));
    let argument_value =
        serde_json::to_string(argument_value).expect("serialize read continuation argument");
    format!("read({argument_name}={argument_value}, offset={next_offset})")
}

fn directory_list_content(outcome: &DirectoryListOutcome) -> String {
    let mut lines = vec![format!(
        "Listed {}: returned={} offset={} total={}; recursive={}; truncated={}.",
        outcome.path,
        outcome.entries.len(),
        outcome.offset,
        outcome.total_entries,
        outcome.recursive,
        outcome.truncated
    )];
    if outcome.entries.is_empty() {
        lines.push("(directory is empty)".to_string());
    } else {
        lines.extend(outcome.entries.iter().map(|entry| {
            let size = entry
                .size_bytes
                .map(|bytes| format!(" sizeBytes={bytes}"))
                .unwrap_or_default();
            format!("- {}: {}{}", entry.kind, entry.path, size)
        }));
    }
    if let Some(next_offset) = outcome.next_offset {
        lines.push(format!(
            "Continuation: list path={} with offset={next_offset} and limit={}.",
            outcome.path, outcome.limit
        ));
    } else {
        lines.push("Continuation: this directory listing is complete.".to_string());
    }
    lines.join("\n")
}

fn read_file_fact(outcome: &FileReadOutcome) -> Value {
    json!({
        "schema": "file_read_fact_v1",
        "toolName": "read",
        "path": outcome.path,
        "fileHash": outcome.file_hash,
        "startLine": outcome.start_line,
        "endLine": outcome.end_line,
        "totalLines": outcome.total_lines,
        "totalBytes": outcome.total_bytes,
        "outputBytes": outcome.output_bytes,
        "maxLines": outcome.max_lines,
        "maxBytes": outcome.max_bytes,
        "truncated": outcome.truncated,
        "truncatedBy": outcome.truncated_by,
        "firstLineExceedsLimit": outcome.first_line_exceeds_limit,
        "nextOffset": outcome.next_offset,
        "inputRef": outcome.input_ref,
        "displayName": outcome.display_name,
        "ownerRef": outcome.owner_ref,
        "ownerKind": outcome.owner_kind,
        "evidenceKind": outcome.evidence_kind,
        "ownerSha256": outcome.owner_sha256,
        "citationRef": outcome.citation_ref,
        "pageStart": outcome.page_start,
        "pageEnd": outcome.page_end,
        "documentRoute": outcome.document_route,
        "documentUsedOcr": outcome.document_used_ocr,
    })
}

fn invalid_input_messages(raw_message: &str) -> (String, String) {
    let lower = raw_message.to_ascii_lowercase();
    if lower.contains("binary file not supported") {
        return (
            "Read target contains unsupported binary data; use a supported document input or a text file"
                .to_string(),
            "Read target is unsupported binary data".to_string(),
        );
    }
    if lower.contains("read path is not a file") || lower.contains("not a file") {
        return (
            "Read target is not a file; provide a file path instead of a directory".to_string(),
            "Read target is not a file".to_string(),
        );
    }
    if lower.contains("toolruntimecontext.workingdirectory is required") {
        return (
            "ToolRuntimeContext.cwd is required for local file tools".to_string(),
            "Working directory is required".to_string(),
        );
    }
    if lower.contains("path is required") {
        return (
            "tool input is missing a required path argument".to_string(),
            "Missing required path".to_string(),
        );
    }
    let message = raw_message.trim();
    if message.is_empty() {
        return (
            "tool input is invalid; revise the file tool arguments and retry".to_string(),
            "Invalid file tool input".to_string(),
        );
    }
    (message.to_string(), "Invalid file tool input".to_string())
}

fn actionable_io_model_message(raw_message: &str, model_path: Option<&str>) -> String {
    let lower = raw_message.to_ascii_lowercase();
    if lower.contains("execution workspace binding is required") {
        return "ExecutionHost workspace binding is unavailable; the Runtime must configure a working directory before file tools can run"
            .to_string();
    }
    let operation = if lower.contains("read directory entry") {
        "Directory entry read"
    } else if lower.contains("read directory") {
        "Directory read"
    } else if lower.contains("inspect") && lower.contains("directory") {
        "Directory entry inspection"
    } else if lower.contains("create parent dir") {
        "Parent directory creation"
    } else if lower.contains("write edited file") {
        "Edited file write"
    } else if lower.contains("write file") {
        "File write"
    } else if lower.contains("read file") || lower.contains("document read") {
        "File read"
    } else if lower.contains("document execution") {
        "Document extraction"
    } else if lower.contains("resolve") {
        "Path resolution"
    } else if lower.contains("delete file") {
        "File deletion"
    } else {
        "File I/O"
    };
    let reason = if lower.contains("permission denied")
        || lower.contains("access is denied")
        || lower.contains("os error 5")
        || (!cfg!(windows) && lower.contains("os error 13"))
    {
        "permission was denied"
    } else if lower.contains("not found")
        || lower.contains("no such file")
        || lower.contains("cannot find")
        || lower.contains("could not find")
        || lower.contains("os error 2")
        || (cfg!(windows) && lower.contains("os error 3"))
    {
        "the file or directory was not found"
    } else if lower.contains("already exists")
        || (!cfg!(windows) && lower.contains("os error 17"))
        || (cfg!(windows) && (lower.contains("os error 80") || lower.contains("os error 183")))
    {
        "the target already exists"
    } else if lower.contains("being used by another process")
        || lower.contains("used by another process")
        || lower.contains("resource busy")
        || lower.contains("would block")
        || (cfg!(windows) && lower.contains("os error 32"))
        || (!cfg!(windows) && lower.contains("os error 16"))
    {
        "the path is in use by another process"
    } else if lower.contains("no space left")
        || lower.contains("disk full")
        || lower.contains("storage full")
        || (cfg!(windows) && lower.contains("os error 112"))
        || (!cfg!(windows) && lower.contains("os error 28"))
    {
        "storage is full"
    } else if lower.contains("read-only file system")
        || lower.contains("write protected")
        || (!cfg!(windows) && lower.contains("os error 30"))
    {
        "the filesystem is read-only"
    } else if lower.contains("not a directory") {
        "a required path component is not a directory"
    } else if lower.contains("is a directory") {
        "the target is a directory rather than a file"
    } else if lower.contains("timed out") || lower.contains("timeout") {
        "the I/O operation timed out"
    } else if lower.contains("unavailable") || lower.contains("service stopped") {
        "the required host service is unavailable"
    } else {
        "the operating system rejected the operation for an unclassified I/O reason"
    };
    let target = model_path.unwrap_or("the requested path");
    format!("{operation} for {target} failed because {reason}.")
}

fn provider_neutral_model_path(raw_path: &str) -> Option<String> {
    let normalized = raw_path.trim().replace('\\', "/");
    if normalized.is_empty()
        || normalized.len() > 512
        || normalized.chars().any(char::is_control)
        || normalized.split('/').any(|segment| segment == "..")
    {
        return None;
    }
    if normalized.starts_with('/')
        || normalized.starts_with("//")
        || normalized.as_bytes().get(1) == Some(&b':')
    {
        return None;
    }
    Some(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execution_host_diagnostic_is_not_exposed_to_the_model() {
        let error = FileToolError::from_execution_host(
            ExecutionFileSystemError::new(
                ExecutionFileSystemErrorKind::PermissionDenied,
                "file mutation was denied",
            )
            .with_diagnostic(
                r#"Access is denied at C:\Users\tester\AppData\Local\Temp\secret.txt"#,
            ),
        )
        .with_model_path("reports/output.txt")
        .to_local_error("write");

        assert!(error.content.contains("denied"));
        assert!(!error.content.contains("C:\\Users"));
        assert!(!error.content.contains("AppData"));
        assert!(error.details["hostDiagnostic"]
            .as_str()
            .unwrap_or_default()
            .contains("AppData"));
    }

    #[test]
    fn resolved_input_read_produces_a_typed_citation_fact() {
        let output = FileToolOutcome::Read(Box::new(FileReadOutcome {
            path: "sources/object/report.pdf".to_string(),
            start_line: 4,
            end_line: 8,
            total_lines: 20,
            total_bytes: 100,
            output_bytes: 20,
            max_lines: 2_000,
            max_bytes: 50 * 1024,
            truncated: true,
            truncated_by: Some("lines"),
            first_line_exceeds_limit: false,
            next_offset: Some(8),
            file_hash: format!("sha256:{}", "a".repeat(64)),
            content: "evidence".to_string(),
            input_ref: Some("input_1".to_string()),
            display_name: Some("report.pdf".to_string()),
            owner_ref: Some("source_1".to_string()),
            owner_kind: Some("sourceObject".to_string()),
            evidence_kind: Some("workspaceSource".to_string()),
            owner_sha256: Some(format!("sha256:{}", "b".repeat(64))),
            citation_ref: Some(format!("citation:{}", "c".repeat(64))),
            page_start: Some(2),
            page_end: Some(2),
            document_route: None,
            document_used_ocr: None,
        }))
        .into_read_output(Some("call_read"))
        .expect("read output");

        assert!(output.content.lines().next().is_some_and(|line| {
            line.contains("pages 2-2; lines 4-8 of 20")
                && line.ends_with("Continue with read(input_ref=\"input_1\", offset=8).")
        }));
        assert!(output
            .content
            .contains("Continuation: read(input_ref=\"input_1\", offset=8)."));

        let [ToolExecutionFact::CitationRecorded(payload)] = output.facts.as_slice() else {
            panic!("expected one citation fact")
        };
        assert_eq!(payload["sourceToolCallId"], "call_read");
        assert_eq!(payload["locator"], json!({"pageStart": 2, "pageEnd": 2}));
    }
}
