use serde::Deserialize;

use crate::execution::{
    ExecutionFileSystemOperation, ExecutionFileSystemOutput, ExecutionPathKind,
};
use crate::tool::WORKSPACE_MUTATION_MAX_BYTES;

use super::mutation::{acquire_file_write_guard, sha256_bytes, write_diff_preview};
use super::outcome::{FileToolError, FileToolErrorKind, FileToolOutcome, FileWriteOutcome};
use super::{
    parse_tool_args, FileMutationCommitRequest, LocalToolError, LocalToolHandler, LocalToolOutput,
    ToolRuntimeContext,
};
#[derive(Debug)]
pub(super) struct WriteToolHandler;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WriteRequest {
    path: String,
    content: String,
}

impl LocalToolHandler for WriteToolHandler {
    fn name(&self) -> &'static str {
        "write"
    }

    fn invoke(
        &self,
        args_json: &str,
        runtime_context: &ToolRuntimeContext,
    ) -> Result<LocalToolOutput, LocalToolError> {
        execute_write(args_json, runtime_context)
    }
}

pub(super) fn execute_write(
    args_json: &str,
    runtime_context: &ToolRuntimeContext,
) -> Result<LocalToolOutput, LocalToolError> {
    execute_write_outcome(args_json, runtime_context)
        .map(FileToolOutcome::into_local_output)
        .map_err(|error| error.to_local_error("write"))
}

fn execute_write_outcome(
    args_json: &str,
    runtime_context: &ToolRuntimeContext,
) -> Result<FileToolOutcome, FileToolError> {
    let args: WriteRequest = parse_tool_args("write", args_json)
        .and_then(|value| serde_json::from_value(value).map_err(|error| error.to_string()))
        .map_err(|message| FileToolError::new(FileToolErrorKind::InvalidInput, message))?;
    let raw_path = args.path.trim();
    if raw_path.is_empty() {
        return Err(FileToolError::new(
            FileToolErrorKind::InvalidInput,
            "path is required for Write",
        ));
    }
    let content = args.content;
    if content.len() > WORKSPACE_MUTATION_MAX_BYTES {
        return Err(FileToolError::new(
            FileToolErrorKind::InvalidInput,
            format!(
                "write content exceeds the {WORKSPACE_MUTATION_MAX_BYTES}-byte workspace mutation limit"
            ),
        ));
    }
    let binding = runtime_context
        .execution_host_binding()
        .map_err(|message| {
            FileToolError::new(FileToolErrorKind::Io, message).with_model_path(raw_path)
        })?;
    let inspection = binding
        .run_file_system_operation(raw_path, ExecutionFileSystemOperation::InspectMutationPath)
        .map_err(|error| FileToolError::from_execution_host(error).with_model_path(raw_path))?;
    let ExecutionFileSystemOutput::InspectMutationPath(inspection) = inspection else {
        return Err(FileToolError::new(
            FileToolErrorKind::Io,
            "execution host returned the wrong filesystem result for write inspection",
        ));
    };
    if inspection.exists && inspection.kind != Some(ExecutionPathKind::File) {
        return Err(FileToolError::new(
            FileToolErrorKind::InvalidInput,
            format!(
                "write target is not a file: {}",
                inspection.identity.display_path
            ),
        ));
    }
    let display_path = inspection.identity.display_path.clone();
    let path_identity = inspection.identity.key.clone();
    let _lease =
        acquire_file_write_guard(path_identity.as_str(), runtime_context).map_err(|message| {
            FileToolError::new(FileToolErrorKind::WriteConflict, message)
                .with_model_path(display_path.as_str())
        })?;
    let existed = inspection.exists;
    let (previous_file_hash, read_snapshot_hash, previous_content) = if existed {
        let read_snapshot_hash = runtime_context
            .require_file_read_snapshot(path_identity.as_str(), raw_path)
            .map_err(|message| {
                FileToolError::new(FileToolErrorKind::ReadSnapshotMissing, message)
                    .with_model_path(display_path.as_str())
            })?;
        let read = binding
            .run_file_system_operation(
                raw_path,
                ExecutionFileSystemOperation::ReadFile {
                    max_bytes: WORKSPACE_MUTATION_MAX_BYTES,
                },
            )
            .map_err(|error| {
                FileToolError::from_execution_host(error).with_model_path(display_path.as_str())
            })?;
        let ExecutionFileSystemOutput::ReadFile(read) = read else {
            return Err(FileToolError::new(
                FileToolErrorKind::Io,
                "execution host returned the wrong filesystem result before write",
            ));
        };
        if read.file_hash != read_snapshot_hash {
            return Err(FileToolError::new(
                FileToolErrorKind::ReadSnapshotMismatch,
                format!("file mutation rejected: {raw_path} changed since the last read"),
            ));
        }
        (
            Some(read.file_hash),
            Some(read_snapshot_hash),
            Some(read.bytes),
        )
    } else {
        (None, None, None)
    };
    let diff = write_diff_preview(
        display_path.as_str(),
        previous_content.as_deref(),
        content.as_str(),
    );
    let file_hash = sha256_bytes(content.as_bytes());
    runtime_context
        .commit_file_mutation(FileMutationCommitRequest {
            schema: "file_mutation_pre_apply_commit_v1".to_string(),
            tool_call_id: runtime_context.current_tool_call_id().map_err(|message| {
                FileToolError::new(FileToolErrorKind::DurableCommitMissing, message)
            })?,
            tool_name: runtime_context.current_tool_name().map_err(|message| {
                FileToolError::new(FileToolErrorKind::DurableCommitMissing, message)
            })?,
            operation: if existed { "overwrite" } else { "create" }.to_string(),
            path: display_path.clone(),
            target_path: None,
            previous_file_hash: previous_file_hash.clone(),
            read_snapshot_hash,
            file_hash: Some(file_hash.clone()),
            bytes_written: Some(content.len()),
            added_lines: Some(diff.added_lines),
            removed_lines: Some(diff.removed_lines),
            session_id: runtime_context.session_id.clone(),
            execution_owner: runtime_context.write_lease_owner().to_string(),
        })
        .map_err(|message| {
            FileToolError::new(FileToolErrorKind::DurableCommitFailed, message)
                .with_model_path(display_path.as_str())
        })?;
    let written = binding
        .run_file_system_operation(
            raw_path,
            ExecutionFileSystemOperation::WriteFile {
                content: content.as_bytes().to_vec(),
                expected_file_hash: previous_file_hash.clone(),
                create_only: !existed,
            },
        )
        .map_err(|error| {
            FileToolError::from_execution_host(error).with_model_path(display_path.as_str())
        })?;
    let ExecutionFileSystemOutput::WriteFile(written) = written else {
        return Err(FileToolError::new(
            FileToolErrorKind::Io,
            "execution host returned the wrong filesystem result for write",
        ));
    };
    if written.file_hash != file_hash || written.previous_file_hash != previous_file_hash {
        return Err(FileToolError::new(
            FileToolErrorKind::Io,
            "execution host write result failed hash verification",
        ));
    }
    runtime_context
        .record_file_read_snapshot(written.identity.key.as_str(), file_hash.clone())
        .map_err(|message| {
            FileToolError::new(FileToolErrorKind::Io, message)
                .with_model_path(display_path.as_str())
        })?;
    Ok(FileToolOutcome::Write(FileWriteOutcome {
        schema: "write_result_v1",
        path: display_path,
        created: !existed,
        previous_file_hash,
        file_hash,
        bytes_written: content.len(),
        added_lines: diff.added_lines,
        removed_lines: diff.removed_lines,
        diff_preview: diff.text,
    }))
}
