use serde::Deserialize;

use crate::tool::WORKSPACE_MUTATION_MAX_BYTES;

use super::mutation::{
    execute_file_mutation, write_diff_preview, CommittedMutation, MutationKind, PreparedMutation,
};
use super::outcome::{FileToolError, FileToolErrorKind, FileToolOutcome, FileWriteOutcome};
use super::{
    parse_tool_args, LocalToolError, LocalToolHandler, LocalToolOutput, ToolRuntimeContext,
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
    execute_file_mutation(
        raw_path,
        runtime_context,
        MutationKind::Write,
        |display_path, previous_content| {
            let diff = write_diff_preview(display_path, previous_content, &content);
            Ok(PreparedMutation {
                content: content.into_bytes(),
                diff,
            })
        },
        |CommittedMutation {
             display_path,
             existed,
             previous_file_hash,
             file_hash,
             bytes_written,
             diff,
         }| {
            FileToolOutcome::Write(FileWriteOutcome {
                schema: "write_result_v1",
                path: display_path,
                created: !existed,
                previous_file_hash,
                file_hash,
                bytes_written,
                added_lines: diff.added_lines,
                removed_lines: diff.removed_lines,
                diff_preview: diff.text,
            })
        },
    )
}
