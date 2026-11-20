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
            let updated = apply_edits(previous_content, &args.edits)?;
            let diff = write_diff_preview(display_path, Some(previous_content), &updated);
            Ok(PreparedMutation {
                content: updated.into_bytes(),
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

fn normalize_to_lf(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn local_line_ending(text: &str, start: usize) -> &'static str {
    let ending = text[start..]
        .find(['\r', '\n'])
        .map(|i| start + i)
        .or_else(|| text[..start].rfind(['\r', '\n']));
    let Some(i) = ending else { return "\n" };
    if text[i..].starts_with("\r\n") || text[..=i].ends_with("\r\n") {
        "\r\n"
    } else if text.as_bytes()[i] == b'\r' {
        "\r"
    } else {
        "\n"
    }
}

/// Translates successive, increasing LF-normalized byte offsets into original offsets.
fn original_offsets(original: &str) -> impl FnMut(usize) -> usize + '_ {
    // Each CRLF before a match boundary accounts for one extra original byte.
    let mut crlf = original.match_indices("\r\n").peekable();
    let mut extra = 0;
    move |offset: usize| {
        while crlf.next_if(|(index, _)| *index < offset + extra).is_some() {
            extra += 1;
        }
        offset + extra
    }
}

fn apply_edits(bytes: &[u8], edits: &[EditItemRequest]) -> Result<String, FileToolError> {
    let text = std::str::from_utf8(bytes).map_err(|error| {
        FileToolError::new(
            FileToolErrorKind::InvalidInput,
            format!(
                "edit only supports valid UTF-8 text; invalid UTF-8 at byte {}",
                error.valid_up_to()
            ),
        )
    })?;
    let original = text.strip_prefix('\u{feff}').unwrap_or(text);
    let matches = match_edits_against_original(original, edits)?;
    let mut original_offset = original_offsets(original);
    let mut updated = String::with_capacity(text.len());
    updated.push_str(&text[..text.len() - original.len()]);
    let mut cursor = 0;
    for edit in matches {
        let start = original_offset(edit.start);
        let end = original_offset(edit.end);
        updated.push_str(&original[cursor..start]);
        if edit.new_text.contains('\n') {
            updated.push_str(
                &edit
                    .new_text
                    .replace('\n', local_line_ending(original, start)),
            );
        } else {
            updated.push_str(edit.new_text);
        }
        cursor = end;
    }
    updated.push_str(&original[cursor..]);
    Ok(updated)
}

fn match_edits_against_original<'a>(
    original: &str,
    edits: &'a [EditItemRequest],
) -> Result<Vec<MatchedEdit<'a>>, FileToolError> {
    let content = if original.contains('\r') {
        std::borrow::Cow::Owned(normalize_to_lf(original))
    } else {
        std::borrow::Cow::Borrowed(original)
    };
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
        if let Some(relative) = content[remaining_start..].find(edit.old_text.as_str()) {
            let second = remaining_start + relative;
            // Report the same line numbers as read, including in files with lone CRs.
            let mut original_offset = original_offsets(original);
            let first_byte = original_offset(start);
            let second_byte = original_offset(second);
            let first_line = 1 + original[..first_byte]
                .bytes()
                .filter(|&b| b == b'\n')
                .count();
            let second_line = first_line
                + original[first_byte..second_byte]
                    .bytes()
                    .filter(|&b| b == b'\n')
                    .count();
            return Err(FileToolError::new(
                FileToolErrorKind::EditTextMismatch,
                format!(
                    "edits[{request_index}].old_text matched more than one location in the original file (first matches start on lines {first_line} and {second_line}); include only enough surrounding context to make it unique"
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
    fn edit_preserves_bom_and_bytes_outside_mixed_line_endings() {
        let source = "\u{feff}未改\nleft\rrename\r\nold\r\n尾\nlast";
        let edits = [
            EditItemRequest {
                old_text: "rename\nold".into(),
                new_text: "new\nblock".into(),
            },
            EditItemRequest {
                old_text: "last".into(),
                new_text: "a\nb".into(),
            },
        ];
        assert_eq!(
            apply_edits(source.as_bytes(), &edits).unwrap(),
            "\u{feff}未改\nleft\rnew\r\nblock\r\n尾\na\nb"
        );
    }

    #[test]
    fn ambiguous_edit_reports_read_line_numbers() {
        let error = apply_edits(
            b"first\r\nmid\rpiece\nmatch\r\nmatch",
            &[EditItemRequest {
                old_text: "match".into(),
                new_text: "new".into(),
            }],
        )
        .unwrap_err();
        assert!(error
            .to_local_error("edit")
            .content
            .contains("lines 3 and 4"));
    }

    #[test]
    fn edit_rejects_invalid_utf8_without_lossy_rewrite() {
        let error = apply_edits(&[0x66, 0x80, 0x6f], &[]).expect_err("invalid UTF-8 must fail");
        assert!(error
            .to_local_error("edit")
            .content
            .contains("invalid UTF-8"));
    }
}
