use serde::Deserialize;

use crate::tool::{
    EDIT_MAX_ARGS_BYTES, EDIT_MAX_ITEMS, EDIT_MAX_NEW_TEXT_BYTES, EDIT_MAX_OLD_TEXT_BYTES,
};

use super::mutation::{
    execute_file_mutation, write_diff_preview, CommittedMutation, MutationKind, PreparedMutation,
};
use super::outcome::{
    FileEditOperationOutcome, FileEditOutcome, FileToolError, FileToolErrorKind, FileToolOutcome,
};
use super::{
    parse_tool_args, LocalToolError, LocalToolHandler, LocalToolOutput, ToolRuntimeContext,
};
#[derive(Debug)]
pub(super) struct EditToolHandler;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EditRequest {
    path: String,
    edits: Vec<EditItemRequest>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EditItemRequest {
    old_text: String,
    new_text: String,
}

struct MatchedEdit<'a> {
    request_index: usize,
    start: usize,
    end: usize,
    new_text: &'a str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OriginalLineEnding {
    Lf,
    CrLf,
}

struct NormalizedEditFile {
    had_utf8_bom: bool,
    line_ending: OriginalLineEnding,
    content: String,
}

impl LocalToolHandler for EditToolHandler {
    fn name(&self) -> &'static str {
        "edit"
    }

    fn invoke(
        &self,
        args_json: &str,
        runtime_context: &ToolRuntimeContext,
    ) -> Result<LocalToolOutput, LocalToolError> {
        execute_edit(args_json, runtime_context)
    }
}

pub(super) fn execute_edit(
    args_json: &str,
    runtime_context: &ToolRuntimeContext,
) -> Result<LocalToolOutput, LocalToolError> {
    execute_edit_outcome(args_json, runtime_context)
        .map(FileToolOutcome::into_local_output)
        .map_err(|error| error.to_local_error("edit"))
}

fn execute_edit_outcome(
    args_json: &str,
    runtime_context: &ToolRuntimeContext,
) -> Result<FileToolOutcome, FileToolError> {
    if args_json.len() > EDIT_MAX_ARGS_BYTES {
        return Err(FileToolError::new(
            FileToolErrorKind::InvalidInput,
            format!(
                "edit arguments exceed the {EDIT_MAX_ARGS_BYTES}-byte limit; split the change into smaller targeted replacements"
            ),
        ));
    }
    let mut args: EditRequest = parse_tool_args("edit", args_json)
        .and_then(|value| serde_json::from_value(value).map_err(|error| error.to_string()))
        .map_err(|message| FileToolError::new(FileToolErrorKind::InvalidInput, message))?;
    let raw_path = args.path.trim();
    if raw_path.is_empty() {
        return Err(FileToolError::new(
            FileToolErrorKind::InvalidInput,
            "path is required for edit",
        ));
    }
    validate_and_normalize_edits(args.edits.as_mut_slice())?;

    execute_file_mutation(
        raw_path,
        runtime_context,
        MutationKind::Edit,
        |display_path, previous_content| {
            let previous_content = previous_content.expect("edit requires an existing file");
            let normalized_file = normalize_edit_file(previous_content)?;
            let matched_edits = match_edits_against_original(
                normalized_file.content.as_str(),
                args.edits.as_slice(),
            )?;
            let mut updated_normalized = String::with_capacity(normalized_file.content.len());
            let mut cursor = 0usize;
            for edit in &matched_edits {
                updated_normalized.push_str(&normalized_file.content[cursor..edit.start]);
                updated_normalized.push_str(edit.new_text);
                cursor = edit.end;
            }
            updated_normalized.push_str(&normalized_file.content[cursor..]);
            let updated_content = restore_edit_file(&normalized_file, updated_normalized.as_str());
            let diff = write_diff_preview(
                display_path,
                Some(previous_content),
                updated_normalized.as_str(),
            );
            Ok(PreparedMutation {
                content: updated_content,
                diff,
            })
        },
        |CommittedMutation {
             display_path,
             previous_file_hash,
             file_hash,
             diff,
             ..
         }| {
            let operation = FileEditOperationOutcome {
                operation_type: "update",
                path: display_path,
                target_path: None,
                previous_file_hash,
                file_hash: Some(file_hash),
                added_lines: diff.added_lines,
                removed_lines: diff.removed_lines,
            };
            FileToolOutcome::Edit(FileEditOutcome {
                schema: "edit_result_v1",
                files_changed: 1,
                replacements_applied: args.edits.len(),
                added_lines: diff.added_lines,
                removed_lines: diff.removed_lines,
                diff_preview: diff.text,
                operations: vec![operation],
            })
        },
    )
}

fn validate_and_normalize_edits(edits: &mut [EditItemRequest]) -> Result<(), FileToolError> {
    if edits.is_empty() || edits.len() > EDIT_MAX_ITEMS {
        return Err(FileToolError::new(
            FileToolErrorKind::InvalidInput,
            format!("edits must contain between 1 and {EDIT_MAX_ITEMS} replacements"),
        ));
    }
    for (index, edit) in edits.iter_mut().enumerate() {
        if edit.old_text.is_empty() {
            return Err(FileToolError::new(
                FileToolErrorKind::InvalidInput,
                format!("edits[{index}].old_text must not be empty"),
            ));
        }
        if edit.old_text.len() > EDIT_MAX_OLD_TEXT_BYTES {
            return Err(FileToolError::new(
                FileToolErrorKind::InvalidInput,
                format!(
                    "edits[{index}].old_text exceeds the {EDIT_MAX_OLD_TEXT_BYTES}-byte limit; keep only enough unchanged context to make the target unique"
                ),
            ));
        }
        if edit.new_text.len() > EDIT_MAX_NEW_TEXT_BYTES {
            return Err(FileToolError::new(
                FileToolErrorKind::InvalidInput,
                format!(
                    "edits[{index}].new_text exceeds the {EDIT_MAX_NEW_TEXT_BYTES}-byte limit; split the change into smaller targeted replacements"
                ),
            ));
        }
        edit.old_text = normalize_to_lf(edit.old_text.as_str());
        edit.new_text = normalize_to_lf(edit.new_text.as_str());
        if edit.old_text == edit.new_text {
            return Err(FileToolError::new(
                FileToolErrorKind::InvalidInput,
                format!("edits[{index}].new_text must differ from old_text"),
            ));
        }
    }
    Ok(())
}

fn normalize_edit_file(bytes: &[u8]) -> Result<NormalizedEditFile, FileToolError> {
    let had_utf8_bom = bytes.starts_with(&[0xEF, 0xBB, 0xBF]);
    let text_bytes = if had_utf8_bom { &bytes[3..] } else { bytes };
    let text = std::str::from_utf8(text_bytes).map_err(|error| {
        FileToolError::new(
            FileToolErrorKind::InvalidInput,
            format!(
                "edit only supports valid UTF-8 text; the target contains invalid UTF-8 at byte {}",
                error.valid_up_to()
            ),
        )
    })?;
    let line_ending = if text.contains("\r\n") {
        OriginalLineEnding::CrLf
    } else {
        OriginalLineEnding::Lf
    };
    Ok(NormalizedEditFile {
        had_utf8_bom,
        line_ending,
        content: normalize_to_lf(text),
    })
}

fn normalize_to_lf(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn restore_edit_file(file: &NormalizedEditFile, normalized: &str) -> Vec<u8> {
    let restored = match file.line_ending {
        OriginalLineEnding::Lf => normalized.to_string(),
        OriginalLineEnding::CrLf => normalized.replace('\n', "\r\n"),
    };
    let mut bytes = Vec::with_capacity(restored.len() + usize::from(file.had_utf8_bom) * 3);
    if file.had_utf8_bom {
        bytes.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
    }
    bytes.extend_from_slice(restored.as_bytes());
    bytes
}

fn match_edits_against_original<'a>(
    content: &str,
    edits: &'a [EditItemRequest],
) -> Result<Vec<MatchedEdit<'a>>, FileToolError> {
    let mut matched = Vec::with_capacity(edits.len());
    for (request_index, edit) in edits.iter().enumerate() {
        let Some(start) = content.find(edit.old_text.as_str()) else {
            return Err(FileToolError::new(
                FileToolErrorKind::EditTextMismatch,
                format!(
                    "edits[{request_index}].old_text did not match current file content; re-read and rebuild the atomic edit"
                ),
            ));
        };
        let next_char_bytes = content[start..]
            .chars()
            .next()
            .expect("non-empty old_text match must start at a character")
            .len_utf8();
        let remaining_start = start.saturating_add(next_char_bytes);
        if content[remaining_start..].contains(edit.old_text.as_str()) {
            return Err(FileToolError::new(
                FileToolErrorKind::EditTextMismatch,
                format!(
                    "edits[{request_index}].old_text matched more than one location in the original file; include only enough surrounding context to make it unique"
                ),
            ));
        }
        matched.push(MatchedEdit {
            request_index,
            start,
            end: start.saturating_add(edit.old_text.len()),
            new_text: edit.new_text.as_str(),
        });
    }
    matched.sort_by_key(|edit| edit.start);
    for pair in matched.windows(2) {
        let previous = &pair[0];
        let current = &pair[1];
        if previous.end > current.start {
            return Err(FileToolError::new(
                FileToolErrorKind::EditTextMismatch,
                format!(
                    "edits[{}] overlaps edits[{}] in the original file; merge overlapping or nearby replacements into one item",
                    previous.request_index, current.request_index
                ),
            ));
        }
    }
    Ok(matched)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit_args(old_text: &str, new_text: &str) -> String {
        serde_json::json!({
            "path": "edit.txt",
            "edits": [{ "old_text": old_text, "new_text": new_text }]
        })
        .to_string()
    }

    #[test]
    fn edit_executor_enforces_utf8_byte_limits_after_argument_limit() {
        let context = ToolRuntimeContext::default();
        for (args, rejected_field) in [
            (edit_args(&"中".repeat(2_730), "replacement"), None),
            (
                edit_args(&"中".repeat(2_731), "replacement"),
                Some("old_text"),
            ),
            (edit_args("target", &"中".repeat(10_922)), None),
            (edit_args("target", &"中".repeat(10_923)), Some("new_text")),
        ] {
            assert!(args.len() < EDIT_MAX_ARGS_BYTES);
            let error =
                execute_edit(args.as_str(), &context).expect_err("test context cannot edit");
            if let Some(field) = rejected_field {
                assert!(error.content.contains(field), "{error:?}");
                assert!(error.content.contains("byte limit"), "{error:?}");
            } else {
                assert!(
                    error.details.to_string().contains("execution host binding"),
                    "{error:?}"
                );
            }
        }
    }

    #[test]
    fn edit_normalization_preserves_bom_and_crlf() {
        let source = b"\xEF\xBB\xBFalpha\r\nbeta\r\n";
        let normalized = normalize_edit_file(source).expect("normalize source");
        assert!(normalized.had_utf8_bom);
        assert_eq!(normalized.line_ending, OriginalLineEnding::CrLf);
        assert_eq!(normalized.content, "alpha\nbeta\n");
        assert_eq!(
            restore_edit_file(&normalized, "alpha\nbravo\n"),
            b"\xEF\xBB\xBFalpha\r\nbravo\r\n"
        );
    }

    #[test]
    fn edit_rejects_invalid_utf8_without_lossy_rewrite() {
        let error = normalize_edit_file(&[0x66, 0x80, 0x6f])
            .err()
            .expect("invalid UTF-8 must fail");
        assert!(error
            .to_local_error("edit")
            .content
            .contains("invalid UTF-8"));
    }
}
