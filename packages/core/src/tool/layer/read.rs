use crate::execution::{ExecutionFileSystemOperation, ExecutionFileSystemOutput};
use crate::model::prepared_prompt::{
    inspect_model_input_image, ExecutionModelInputImageRefV1, ModelInputImageSourceRefV1,
};
use crate::tool::inputs::{
    canonical_virtual_path, DeferredInputResolutionError, DeferredInputResolutionFailureKind,
    ResolvedInput,
};
use crate::tool::{READ_MAX_BYTES, READ_MAX_LINES};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use super::mutation::sha256_bytes;
use super::outcome::{
    DirectoryListEntryOutcome, DirectoryListOutcome, FileImageReadOutcome, FileReadBatchOutcome,
    FileReadOutcome, FileToolError, FileToolErrorKind, FileToolOutcome,
};
use super::{
    parse_tool_args, LocalToolError, LocalToolHandler, LocalToolOutput, ToolRuntimeContext,
};

const MAX_DIRECT_READ_BYTES: u64 = 8 * 1024 * 1024;
const MAX_BATCH_INPUTS: usize = 4;
const DEFAULT_DIRECTORY_ENTRIES: usize = 100;
const MAX_DIRECTORY_ENTRIES: usize = 200;
const MAX_DIRECTORY_SCAN_ENTRIES: usize = 10_000;

#[derive(Debug)]
pub(super) struct ReadToolHandler;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadRequest {
    path: Option<String>,
    input_ref: Option<String>,
    input_refs: Option<Vec<String>>,
    operation: Option<ReadOperation>,
    recursive: Option<bool>,
    offset: Option<usize>,
    limit: Option<usize>,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ReadOperation {
    Read,
    List,
}

#[derive(Debug)]
enum ReadTarget {
    Path(String),
    InputRef(String),
    InputRefs(Vec<String>),
}

enum ImageReadSource {
    ExecutionFile(String),
    InputRef(String),
}

impl ReadToolHandler {
    #[cfg(test)]
    pub(super) fn new() -> Self {
        Self
    }
}

impl LocalToolHandler for ReadToolHandler {
    fn name(&self) -> &'static str {
        "read"
    }

    fn invoke(
        &self,
        args_json: &str,
        runtime_context: &ToolRuntimeContext,
    ) -> Result<LocalToolOutput, LocalToolError> {
        let poll_args = parse_tool_args(self.name(), args_json)
            .map_err(|message| FileToolError::new(FileToolErrorKind::InvalidInput, message))
            .map_err(|error| error.to_local_error(self.name()))?;
        let args: ReadRequest = serde_json::from_value(poll_args.clone())
            .map_err(|error| FileToolError::new(FileToolErrorKind::InvalidInput, error.to_string()))
            .map_err(|error| error.to_local_error(self.name()))?;
        let operation = args.operation.unwrap_or(ReadOperation::Read);
        if operation == ReadOperation::List {
            let path = match (args.path, args.input_ref, args.input_refs) {
                (Some(path), None, None) if !path.trim().is_empty() => path,
                _ => {
                    return Err(FileToolError::new(
                        FileToolErrorKind::InvalidInput,
                        "read operation=list requires exactly one non-empty path",
                    )
                    .to_local_error(self.name()))
                }
            };
            let limit = args.limit.unwrap_or(DEFAULT_DIRECTORY_ENTRIES);
            if limit == 0 || limit > MAX_DIRECTORY_ENTRIES {
                return Err(FileToolError::new(
                    FileToolErrorKind::InvalidInput,
                    "read directory limit must be between 1 and 200",
                )
                .to_local_error(self.name()));
            }
            return execute_directory_list(
                path.as_str(),
                args.recursive.unwrap_or(false),
                args.offset.unwrap_or(0),
                limit,
                runtime_context,
            )
            .map(|outcome| FileToolOutcome::DirectoryList(outcome).into_local_output())
            .map_err(|error| error.to_local_error(self.name()));
        }
        if args.recursive.is_some() {
            return Err(FileToolError::new(
                FileToolErrorKind::InvalidInput,
                "read recursive is only valid with operation=list",
            )
            .to_local_error(self.name()));
        }
        let target = match (args.path, args.input_ref, args.input_refs) {
            (Some(path), None, None) if !path.trim().is_empty() => ReadTarget::Path(path),
            (None, Some(input_ref), None) if !input_ref.trim().is_empty() => {
                ReadTarget::InputRef(input_ref)
            }
            (None, None, Some(input_refs))
                if !input_refs.is_empty()
                    && input_refs.len() <= MAX_BATCH_INPUTS
                    && input_refs.iter().all(|value| !value.trim().is_empty())
                    && {
                        let unique = input_refs.iter().collect::<std::collections::HashSet<_>>();
                        unique.len() == input_refs.len()
                    } =>
            {
                if args.offset.is_some() || args.limit.is_some() {
                    return Err(FileToolError::new(
                        FileToolErrorKind::InvalidInput,
                        "read input_refs cannot be combined with offset or limit",
                    )
                    .to_local_error(self.name()));
                }
                ReadTarget::InputRefs(input_refs)
            }
            _ => {
                return Err(FileToolError::new(
                    FileToolErrorKind::InvalidInput,
                    "read requires exactly one of path, input_ref, or input_refs (maximum four unique refs)",
                )
                .to_local_error(self.name()))
            }
        };
        if let Some(output) = execute_hosted_knowledge_read(
            &target,
            args.offset,
            args.limit,
            poll_args,
            runtime_context,
        )
        .map_err(|error| error.to_local_error(self.name()))?
        {
            return Ok(output);
        }
        match target {
            ReadTarget::InputRefs(input_refs) => input_refs
                .into_iter()
                .map(|input_ref| {
                    execute_read(ReadTarget::InputRef(input_ref), None, None, runtime_context)
                        .and_then(|outcome| match outcome {
                            FileToolOutcome::Read(read) => Ok(*read),
                            _ => Err(FileToolError::new(
                                FileToolErrorKind::Unknown,
                                "read batch produced a non-read outcome",
                            )),
                        })
                })
                .collect::<Result<Vec<_>, _>>()
                .and_then(|items| {
                    FileToolOutcome::ReadBatch(FileReadBatchOutcome {
                        schema: "read_batch_result.v1",
                        items,
                    })
                    .into_read_output(runtime_context.current_tool_call_id().ok().as_deref())
                    .map_err(|message| FileToolError::new(FileToolErrorKind::Unknown, message))
                }),
            target => {
                execute_read(target, args.offset, args.limit, runtime_context).and_then(|outcome| {
                    outcome
                        .into_read_output(runtime_context.current_tool_call_id().ok().as_deref())
                        .map_err(|message| FileToolError::new(FileToolErrorKind::Unknown, message))
                })
            }
        }
        .map_err(|error| error.to_local_error(self.name()))
    }
}

fn execute_hosted_knowledge_read(
    target: &ReadTarget,
    offset: Option<usize>,
    limit: Option<usize>,
    poll_args: serde_json::Value,
    runtime_context: &ToolRuntimeContext,
) -> Result<Option<LocalToolOutput>, FileToolError> {
    let input_refs = match target {
        ReadTarget::Path(_) => return Ok(None),
        ReadTarget::InputRef(input_ref) => vec![input_ref.as_str()],
        ReadTarget::InputRefs(input_refs) => input_refs.iter().map(String::as_str).collect(),
    };
    let binding = runtime_context
        .execution_host_binding()
        .map_err(|message| FileToolError::new(FileToolErrorKind::Io, message))?;
    if binding.mode() != crate::execution::ExecutionHostMode::Remote {
        return Ok(None);
    }
    let state = runtime_context
        .resolved_input_manifest
        .as_ref()
        .ok_or_else(|| {
            FileToolError::new(
                FileToolErrorKind::ResolvedInputRequired,
                "read input_ref requires the resolved input manifest",
            )
        })?;
    for input_ref in &input_refs {
        state.declared_input_by_ref(input_ref).ok_or_else(|| {
            FileToolError::new(
                FileToolErrorKind::ResolvedInputRequired,
                "read input_ref is not declared by this AgentRun",
            )
        })?;
    }
    let inputs = input_refs
        .into_iter()
        .map(|input_ref| state.resolve_input(input_ref).map_err(deferred_input_error))
        .collect::<Result<Vec<_>, _>>()?;
    let port = runtime_context
        .resolved_input_reader
        .as_ref()
        .ok_or_else(|| {
            FileToolError::new(
                FileToolErrorKind::Io,
                "authorized input_ref requires the hosted Knowledge pipeline",
            )
        })?;
    let tool_call_id = runtime_context
        .current_tool_call_id()
        .map_err(|message| FileToolError::new(FileToolErrorKind::Io, message))?;
    let output = port
        .read(super::ResolvedInputReadRequest {
            inputs,
            offset,
            limit,
            poll_args,
            tool_call_id,
        })
        .map_err(|message| FileToolError::new(FileToolErrorKind::Io, message))?;
    promote_hosted_read_continuation(target, output)
        .map(Some)
        .map_err(|message| FileToolError::new(FileToolErrorKind::Io, message))
}

fn promote_hosted_read_continuation(
    target: &ReadTarget,
    mut output: LocalToolOutput,
) -> Result<LocalToolOutput, String> {
    let Some(next_offset) = output.details.get("nextOffset") else {
        return Ok(output);
    };
    if next_offset.is_null() {
        return Ok(output);
    }
    let next_offset = next_offset
        .as_u64()
        .ok_or_else(|| "hosted Read nextOffset must be a non-negative integer".to_string())?;
    let input_ref = match target {
        ReadTarget::InputRef(input_ref) => input_ref,
        ReadTarget::Path(_) | ReadTarget::InputRefs(_) => {
            return Err("hosted Read continuation requires one input_ref".to_string());
        }
    };
    let input_ref =
        serde_json::to_string(input_ref).map_err(|error| format!("encode input_ref: {error}"))?;
    let call = format!("read(input_ref={input_ref}, offset={next_offset})");
    let window = match (
        output.details.get("pageStart").and_then(Value::as_u64),
        output.details.get("pageEnd").and_then(Value::as_u64),
    ) {
        (Some(page_start), Some(page_end)) => format!("pages {page_start}-{page_end}"),
        _ => match (
            output.details.get("startLine").and_then(Value::as_u64),
            output.details.get("endLine").and_then(Value::as_u64),
        ) {
            (Some(start_line), Some(end_line)) => {
                format!("lines {start_line}-{end_line}")
            }
            _ => "a bounded window".to_string(),
        },
    };
    output.content = format!(
        "Read {window}; truncated. Continue with {call}.\n{}\nContinuation: {call}.",
        output.content
    );
    Ok(output)
}

fn execute_directory_list(
    path: &str,
    recursive: bool,
    offset: usize,
    limit: usize,
    runtime_context: &ToolRuntimeContext,
) -> Result<DirectoryListOutcome, FileToolError> {
    let manifest = runtime_context.resolved_input_manifest.as_deref();
    let manifest_inputs = if canonical_virtual_path(path).is_ok() {
        manifest
            .map(|state| {
                state
                    .inputs_by_virtual_path_prefix(path)
                    .map_err(|message| FileToolError::new(FileToolErrorKind::InvalidInput, message))
            })
            .transpose()?
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let manifest_path = manifest.is_some() && !manifest_inputs.is_empty();
    let (display_path, mut entries) = if manifest_path {
        let entries = manifest_directory_entries(
            manifest.expect("manifest path requires manifest"),
            path,
            recursive,
        )?;
        (path.to_string(), entries)
    } else {
        let binding = runtime_context
            .execution_host_binding()
            .map_err(|message| {
                FileToolError::new(FileToolErrorKind::Io, message).with_model_path(path)
            })?;
        let output = binding
            .run_file_system_operation(
                path,
                ExecutionFileSystemOperation::ListDirectory {
                    recursive,
                    max_entries: MAX_DIRECTORY_SCAN_ENTRIES,
                },
            )
            .map_err(|error| FileToolError::from_execution_host(error).with_model_path(path))?;
        let ExecutionFileSystemOutput::ListDirectory(output) = output else {
            return Err(FileToolError::new(
                FileToolErrorKind::Io,
                "execution host returned the wrong filesystem result for directory listing",
            ));
        };
        let entries = output
            .entries
            .into_iter()
            .map(|entry| DirectoryListEntryOutcome {
                path: entry.path,
                kind: entry.kind.as_str(),
                size_bytes: entry.size_bytes,
                sha256: None,
                input_ref: None,
            })
            .collect::<Vec<_>>();
        let display_path = output.identity.display_path;
        (display_path, entries)
    };
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    if offset > entries.len() {
        return Err(FileToolError::new(
            FileToolErrorKind::InvalidInput,
            "read directory offset exceeds available entries",
        ));
    }
    let total_entries = entries.len();
    let selected = entries
        .into_iter()
        .skip(offset)
        .take(limit)
        .collect::<Vec<_>>();
    let consumed = offset.saturating_add(selected.len());
    let truncated = consumed < total_entries;
    Ok(DirectoryListOutcome {
        schema: "directory_listing_result.v1",
        path: display_path,
        recursive,
        offset,
        limit,
        total_entries,
        truncated,
        next_offset: truncated.then_some(consumed),
        entries: selected,
    })
}

fn manifest_directory_entries(
    manifest: &crate::tool::inputs::ResolvedInputState,
    virtual_directory: &str,
    recursive: bool,
) -> Result<Vec<DirectoryListEntryOutcome>, FileToolError> {
    let prefix = crate::tool::inputs::canonical_virtual_path(virtual_directory)
        .map_err(|message| FileToolError::new(FileToolErrorKind::InvalidInput, message))?;
    if prefix.is_empty() {
        return Err(FileToolError::new(
            FileToolErrorKind::ResolvedInputRequired,
            "read operation=list requires an authorized manifest directory",
        ));
    }
    let inputs = manifest
        .inputs_by_virtual_path_prefix(prefix.as_str())
        .map_err(|message| FileToolError::new(FileToolErrorKind::InvalidInput, message))?;
    if inputs.is_empty() {
        return Err(FileToolError::new(
            FileToolErrorKind::ResolvedInputRequired,
            "read directory has no authorized manifest entries",
        ));
    }
    if inputs.len() > MAX_DIRECTORY_SCAN_ENTRIES {
        return Err(FileToolError::new(
            FileToolErrorKind::InvalidInput,
            "read directory exceeds the bounded scan limit",
        ));
    }
    let prefix_with_separator = format!("{}/", prefix.trim_end_matches('/'));
    let mut entries = BTreeMap::new();
    for input in inputs {
        let suffix = input
            .virtual_path
            .strip_prefix(prefix_with_separator.as_str())
            .ok_or_else(|| {
                FileToolError::new(
                    FileToolErrorKind::Unknown,
                    "authorized directory prefix mismatch",
                )
            })?;
        if recursive || !suffix.contains('/') {
            entries.insert(
                input.virtual_path.clone(),
                DirectoryListEntryOutcome {
                    path: input.virtual_path,
                    kind: "file",
                    size_bytes: Some(input.size_bytes),
                    sha256: Some(input.sha256),
                    input_ref: Some(input.input_ref),
                },
            );
        } else if let Some(first) = suffix.split('/').next() {
            let directory_path = format!("{prefix_with_separator}{first}");
            entries
                .entry(directory_path.clone())
                .or_insert(DirectoryListEntryOutcome {
                    path: directory_path,
                    kind: "directory",
                    size_bytes: None,
                    sha256: None,
                    input_ref: None,
                });
        }
    }
    Ok(entries.into_values().collect())
}

fn execute_read(
    target: ReadTarget,
    offset: Option<usize>,
    limit: Option<usize>,
    runtime_context: &ToolRuntimeContext,
) -> Result<FileToolOutcome, FileToolError> {
    let (input_ref, path) = match &target {
        ReadTarget::Path(path) => (None, Some(path.as_str())),
        ReadTarget::InputRef(input_ref) => (Some(input_ref.as_str()), None),
        ReadTarget::InputRefs(_) => {
            return Err(FileToolError::new(
                FileToolErrorKind::InvalidInput,
                "nested read batch target is invalid",
            ))
        }
    };
    let resolved_input = resolve_manifest_input(runtime_context, input_ref, path)?;
    let lookup_path = resolved_input
        .as_ref()
        .map(|input| input.virtual_path.as_str())
        .or(path)
        .ok_or_else(|| {
            FileToolError::new(
                FileToolErrorKind::ResolvedInputRequired,
                "read input_ref is not available in the resolved input manifest",
            )
        })?;
    if resolved_input.is_none() {
        return execute_workspace_read(lookup_path, offset, limit, runtime_context);
    }
    let binding = runtime_context
        .execution_host_binding()
        .map_err(|message| {
            FileToolError::new(FileToolErrorKind::Io, message).with_model_path(lookup_path)
        })?;
    let (
        display_path,
        snapshot_identity,
        file_hash,
        content,
        document_route,
        document_used_ocr,
        page_start,
        page_end,
    ) = if binding.mode() == crate::execution::ExecutionHostMode::Remote {
        let output = binding
            .run_file_system_operation(
                lookup_path,
                ExecutionFileSystemOperation::ReadFile {
                    max_bytes: MAX_DIRECT_READ_BYTES as usize,
                },
            )
            .map_err(|error| {
                FileToolError::from_execution_host(error).with_model_path(lookup_path)
            })?;
        let ExecutionFileSystemOutput::ReadFile(output) = output else {
            return Err(FileToolError::new(
                FileToolErrorKind::Io,
                "execution host returned the wrong filesystem result for resolved input",
            ));
        };
        if resolved_input
            .as_ref()
            .is_some_and(|input| output.file_hash != input.sha256)
        {
            return Err(FileToolError::new(
                FileToolErrorKind::StaleInput,
                "resolved input hash mismatch",
            ));
        }
        if let Some(outcome) = try_image_read_outcome(
            output.bytes.as_slice(),
            output.identity.display_path.as_str(),
            output.file_hash.as_str(),
            ImageReadSource::ExecutionFile(lookup_path.to_string()),
            offset,
            limit,
            runtime_context,
            output.identity.key.as_str(),
        )? {
            return Ok(outcome);
        }
        if is_pdf_path(Path::new(lookup_path)) || is_binary_document_path(Path::new(lookup_path)) {
            return Err(FileToolError::new(
                FileToolErrorKind::Io,
                "binary document requires the hosted Processing Pipeline",
            )
            .with_model_path(lookup_path));
        }
        if output.bytes.contains(&0) {
            return Err(FileToolError::new(
                FileToolErrorKind::InvalidInput,
                "binary file is not directly readable",
            )
            .with_model_path(lookup_path));
        }
        (
            output.identity.display_path,
            output.identity.key,
            output.file_hash,
            String::from_utf8_lossy(output.bytes.as_slice()).into_owned(),
            None,
            None,
            None,
            None,
        )
    } else {
        let resolved = resolve_manifest_physical_path(runtime_context, lookup_path)?;
        let display_path = resolved.display_path.clone();
        if !resolved.path.is_file() {
            return Err(FileToolError::new(
                FileToolErrorKind::InvalidInput,
                format!("Read path is not a file: {}", resolved.path.display()),
            ));
        }
        let bytes = fs::read(resolved.path.as_path()).map_err(|error| {
            FileToolError::new(
                FileToolErrorKind::Io,
                format!("read resolved input for content hash failed: {error}"),
            )
            .with_model_path(display_path.as_str())
        })?;
        let file_hash = sha256_bytes(bytes.as_slice());
        if resolved_input
            .as_ref()
            .is_some_and(|input| file_hash != input.sha256)
        {
            return Err(FileToolError::new(
                FileToolErrorKind::StaleInput,
                "resolved input hash mismatch",
            ));
        }
        if let Some(outcome) = try_image_read_outcome(
            bytes.as_slice(),
            display_path.as_str(),
            file_hash.as_str(),
            ImageReadSource::InputRef(
                resolved_input
                    .as_ref()
                    .expect("resolved input branch requires input")
                    .input_ref
                    .clone(),
            ),
            offset,
            limit,
            runtime_context,
            resolved.path.to_string_lossy().as_ref(),
        )? {
            return Ok(outcome);
        }
        if is_pdf_path(resolved.path.as_path()) || is_binary_document_path(resolved.path.as_path())
        {
            return Err(FileToolError::new(
                FileToolErrorKind::InvalidInput,
                "binary document requires the hosted Processing Pipeline",
            )
            .with_model_path(display_path.as_str()));
        }
        let content = read_bounded_text(resolved.path.as_path(), display_path.as_str())?;
        let (document_route, document_used_ocr, page_start, page_end) = (None, None, None, None);
        (
            display_path,
            resolved.path.to_string_lossy().into_owned(),
            file_hash,
            content,
            document_route,
            document_used_ocr,
            page_start,
            page_end,
        )
    };
    let total_bytes = content.len();
    let all_lines: Vec<&str> = content.lines().collect();
    let offset = offset.unwrap_or(0);
    if offset > all_lines.len() {
        return Err(FileToolError::new(
            FileToolErrorKind::InvalidInput,
            "Read start line exceeds available content",
        ));
    }
    let limit = limit.unwrap_or(READ_MAX_LINES);
    if limit == 0 || limit > READ_MAX_LINES {
        return Err(FileToolError::new(
            FileToolErrorKind::InvalidInput,
            format!("read file limit must be between 1 and {READ_MAX_LINES}"),
        ));
    }
    let selection = select_bounded_read_lines(all_lines.as_slice(), offset, limit);
    let selected = selection.lines;
    let start_line = offset.saturating_add(1);
    let end_line = offset.saturating_add(selected.len());
    let citation_ref = resolved_input
        .as_ref()
        .filter(|input| input.citation_allowed)
        .filter(|_| !selected.is_empty())
        .map(|input| {
            build_citation_ref(
                runtime_context
                    .resolved_input_manifest
                    .as_ref()
                    .expect("resolved input requires manifest")
                    .agent_run_id(),
                input,
                start_line,
                end_line,
                None,
            )
        });
    runtime_context
        .record_file_read_snapshot(snapshot_identity.as_str(), file_hash.clone())
        .map_err(|message| FileToolError::new(FileToolErrorKind::Io, message))?;
    Ok(FileToolOutcome::Read(Box::new(FileReadOutcome {
        path: display_path,
        start_line,
        end_line,
        total_lines: all_lines.len(),
        total_bytes,
        output_bytes: selection.output_bytes,
        max_lines: limit,
        max_bytes: READ_MAX_BYTES,
        truncated: selection.truncated,
        truncated_by: selection.truncated_by,
        first_line_exceeds_limit: selection.first_line_exceeds_limit,
        next_offset: selection.next_offset,
        file_hash,
        content: selected.join("\n"),
        input_ref: resolved_input.as_ref().map(|input| input.input_ref.clone()),
        display_name: resolved_input
            .as_ref()
            .map(|input| input.display_name.clone()),
        owner_ref: resolved_input
            .as_ref()
            .map(|input| input.object_ref.clone()),
        owner_kind: resolved_input
            .as_ref()
            .map(|input| input.owner_kind.clone()),
        evidence_kind: resolved_input
            .as_ref()
            .map(|input| input.evidence_kind.clone()),
        owner_sha256: resolved_input.as_ref().map(|input| input.sha256.clone()),
        citation_ref,
        page_start,
        page_end,
        document_route,
        document_used_ocr,
    })))
}

fn execute_workspace_read(
    path: &str,
    offset: Option<usize>,
    limit: Option<usize>,
    runtime_context: &ToolRuntimeContext,
) -> Result<FileToolOutcome, FileToolError> {
    if is_pdf_path(Path::new(path))
        || (is_binary_document_path(Path::new(path))
            && !is_supported_model_image_path(Path::new(path)))
    {
        return Err(FileToolError::new(
            FileToolErrorKind::InvalidInput,
            "binary document requires the hosted Processing Pipeline",
        )
        .with_model_path(path));
    }
    let binding = runtime_context
        .execution_host_binding()
        .map_err(|message| {
            FileToolError::new(FileToolErrorKind::Io, message).with_model_path(path)
        })?;
    let output = binding
        .run_file_system_operation(
            path,
            ExecutionFileSystemOperation::ReadFile {
                max_bytes: MAX_DIRECT_READ_BYTES as usize,
            },
        )
        .map_err(|error| FileToolError::from_execution_host(error).with_model_path(path))?;
    let ExecutionFileSystemOutput::ReadFile(output) = output else {
        return Err(FileToolError::new(
            FileToolErrorKind::Io,
            "execution host returned the wrong filesystem result for file read",
        ));
    };
    let display_path = output.identity.display_path.clone();
    let file_hash = output.file_hash.clone();
    if let Some(outcome) = try_image_read_outcome(
        output.bytes.as_slice(),
        display_path.as_str(),
        file_hash.as_str(),
        ImageReadSource::ExecutionFile(display_path.clone()),
        offset,
        limit,
        runtime_context,
        output.identity.key.as_str(),
    )? {
        return Ok(outcome);
    }
    let content = String::from_utf8_lossy(output.bytes.as_slice()).into_owned();
    let (document_route, document_used_ocr, page_start, page_end) = (None, None, None, None);
    let total_bytes = content.len();
    let all_lines = content.lines().collect::<Vec<_>>();
    let offset = offset.unwrap_or(0);
    if offset > all_lines.len() {
        return Err(FileToolError::new(
            FileToolErrorKind::InvalidInput,
            "Read start line exceeds available content",
        ));
    }
    let limit = limit.unwrap_or(READ_MAX_LINES);
    if limit == 0 || limit > READ_MAX_LINES {
        return Err(FileToolError::new(
            FileToolErrorKind::InvalidInput,
            format!("read file limit must be between 1 and {READ_MAX_LINES}"),
        ));
    }
    let selection = select_bounded_read_lines(all_lines.as_slice(), offset, limit);
    let selected = selection.lines;
    let start_line = offset.saturating_add(1);
    let end_line = offset.saturating_add(selected.len());
    runtime_context
        .record_file_read_snapshot(output.identity.key.as_str(), file_hash.clone())
        .map_err(|message| FileToolError::new(FileToolErrorKind::Io, message))?;
    Ok(FileToolOutcome::Read(Box::new(FileReadOutcome {
        path: display_path,
        start_line,
        end_line,
        total_lines: all_lines.len(),
        total_bytes,
        output_bytes: selection.output_bytes,
        max_lines: limit,
        max_bytes: READ_MAX_BYTES,
        truncated: selection.truncated,
        truncated_by: selection.truncated_by,
        first_line_exceeds_limit: selection.first_line_exceeds_limit,
        next_offset: selection.next_offset,
        file_hash,
        content: selected.join("\n"),
        input_ref: None,
        display_name: None,
        owner_ref: None,
        owner_kind: None,
        evidence_kind: None,
        owner_sha256: None,
        citation_ref: None,
        page_start,
        page_end,
        document_route,
        document_used_ocr,
    })))
}

#[expect(
    clippy::too_many_arguments,
    reason = "image read projection keeps source and snapshot fields explicit"
)]
fn try_image_read_outcome(
    bytes: &[u8],
    display_path: &str,
    file_hash: &str,
    source: ImageReadSource,
    offset: Option<usize>,
    limit: Option<usize>,
    runtime_context: &ToolRuntimeContext,
    snapshot_identity: &str,
) -> Result<Option<FileToolOutcome>, FileToolError> {
    if !looks_like_supported_image(bytes) && !is_supported_model_image_path(Path::new(display_path))
    {
        return Ok(None);
    }
    if offset.is_some() || limit.is_some() {
        return Err(FileToolError::new(
            FileToolErrorKind::InvalidInput,
            "read image does not accept offset or limit",
        ));
    }
    let (content_type, width_px, height_px) = inspect_model_input_image(bytes)
        .map_err(|message| FileToolError::new(FileToolErrorKind::InvalidInput, message))?;
    let tool_call_id = runtime_context
        .current_tool_call_id()
        .map_err(|message| FileToolError::new(FileToolErrorKind::Unknown, message))?;
    let placeholder = format!("[Image observation: {tool_call_id}]");
    let source = match source {
        ImageReadSource::ExecutionFile(path) => {
            let image = ExecutionModelInputImageRefV1 {
                path,
                content_type: content_type.to_string(),
                sha256: file_hash.to_string(),
                byte_length: bytes.len() as u64,
                width_px,
                height_px,
                placeholder,
            };
            image
                .validate()
                .map_err(|message| FileToolError::new(FileToolErrorKind::InvalidInput, message))?;
            ModelInputImageSourceRefV1::ExecutionFile { image }
        }
        ImageReadSource::InputRef(input_ref) => ModelInputImageSourceRefV1::InputRef {
            input_ref,
            content_type: content_type.to_string(),
            placeholder,
        },
    };
    runtime_context
        .record_file_read_snapshot(snapshot_identity, file_hash.to_string())
        .map_err(|message| FileToolError::new(FileToolErrorKind::Io, message))?;
    Ok(Some(FileToolOutcome::ImageRead(FileImageReadOutcome {
        schema: "image_read_result_v1",
        path: display_path.to_string(),
        content_type: content_type.to_string(),
        byte_length: bytes.len() as u64,
        width_px,
        height_px,
        file_hash: file_hash.to_string(),
        model_input_images: vec![source],
    })))
}

fn looks_like_supported_image(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x89PNG\r\n\x1a\n")
        || bytes.starts_with(b"\xff\xd8\xff")
        || (bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP")
}

fn is_supported_model_image_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "png" | "jpg" | "jpeg" | "webp"
            )
        })
}

struct BoundedReadSelection<'a> {
    lines: Vec<&'a str>,
    output_bytes: usize,
    truncated: bool,
    truncated_by: Option<&'static str>,
    first_line_exceeds_limit: bool,
    next_offset: Option<usize>,
}

fn select_bounded_read_lines<'a>(
    all_lines: &'a [&'a str],
    offset: usize,
    max_lines: usize,
) -> BoundedReadSelection<'a> {
    let mut lines = Vec::with_capacity(max_lines.min(all_lines.len().saturating_sub(offset)));
    let mut output_bytes = 0usize;
    let mut truncated_by = None;
    let mut first_line_exceeds_limit = false;

    for line in all_lines.iter().skip(offset) {
        if lines.len() == max_lines {
            truncated_by = Some("lines");
            break;
        }
        let separator_bytes = usize::from(!lines.is_empty());
        let next_bytes = output_bytes
            .saturating_add(separator_bytes)
            .saturating_add(line.len());
        if next_bytes > READ_MAX_BYTES {
            truncated_by = Some("bytes");
            first_line_exceeds_limit = lines.is_empty();
            break;
        }
        lines.push(*line);
        output_bytes = next_bytes;
    }

    let end_offset = offset.saturating_add(lines.len());
    let truncated = end_offset < all_lines.len();
    if !truncated {
        truncated_by = None;
        first_line_exceeds_limit = false;
    }
    let next_offset = (truncated && !first_line_exceeds_limit).then_some(end_offset);
    BoundedReadSelection {
        lines,
        output_bytes,
        truncated,
        truncated_by,
        first_line_exceeds_limit,
        next_offset,
    }
}

fn read_bounded_text(path: &Path, label: &str) -> Result<String, FileToolError> {
    let size = std::fs::metadata(path)
        .map_err(|error| {
            FileToolError::new(
                FileToolErrorKind::Io,
                format!("read file metadata failed: {error}"),
            )
            .with_model_path(label)
        })?
        .len();
    if size > MAX_DIRECT_READ_BYTES {
        return Err(FileToolError::new(
            FileToolErrorKind::InvalidInput,
            format!("Read {label} exceeds the bounded direct-read limit"),
        ));
    }
    let bytes = fs::read(path).map_err(|error| {
        FileToolError::new(FileToolErrorKind::Io, format!("read file failed: {error}"))
            .with_model_path(label)
    })?;
    if bytes.contains(&0) {
        return Err(FileToolError::new(
            FileToolErrorKind::InvalidInput,
            format!("binary file not supported: {label}"),
        ));
    }
    Ok(String::from_utf8_lossy(bytes.as_slice()).into_owned())
}

fn resolve_manifest_input(
    runtime_context: &ToolRuntimeContext,
    input_ref: Option<&str>,
    path: Option<&str>,
) -> Result<Option<ResolvedInput>, FileToolError> {
    let Some(manifest) = runtime_context.resolved_input_manifest.as_deref() else {
        return Ok(None);
    };
    let by_ref = match input_ref {
        Some(value) => manifest
            .input_by_ref(value)
            .map_err(|message| FileToolError::new(FileToolErrorKind::InvalidInput, message))?,
        None => None,
    };
    let by_path = match path {
        Some(value) => manifest
            .input_by_virtual_path(value)
            .map_err(|message| FileToolError::new(FileToolErrorKind::InvalidInput, message))?,
        None => None,
    };
    if by_path.is_some()
        && runtime_context
            .execution_host_binding()
            .map_err(|message| FileToolError::new(FileToolErrorKind::Io, message))?
            .mode()
            == crate::execution::ExecutionHostMode::Remote
    {
        return Err(FileToolError::new(
            FileToolErrorKind::InvalidInput,
            "resolved input path alias is unsupported; use input_ref",
        ));
    }
    if path.is_some() && by_path.is_none() {
        if input_ref.is_none() {
            return Ok(None);
        }
        return Err(FileToolError::new(
            FileToolErrorKind::ResolvedInputRequired,
            "Read target is not present in the resolved input manifest",
        ));
    }
    if let (Some(left), Some(right)) = (by_ref.as_ref(), by_path.as_ref()) {
        if left != right {
            return Err(FileToolError::new(
                FileToolErrorKind::ResolvedInputRequired,
                "Read input_ref/path binding mismatch",
            ));
        }
    }
    let resolved_input_ref = by_ref
        .as_ref()
        .or(by_path.as_ref())
        .map(|input| input.input_ref.as_str())
        .or(input_ref);
    match resolved_input_ref {
        Some(value) => manifest
            .resolve_input(value)
            .map(Some)
            .map_err(deferred_input_error),
        None => Ok(None),
    }
}

fn resolve_manifest_physical_path(
    runtime_context: &ToolRuntimeContext,
    virtual_path: &str,
) -> Result<ResolvedManifestPath, FileToolError> {
    let root = runtime_context
        .resolved_input_root
        .as_ref()
        .ok_or_else(|| {
            FileToolError::new(
                FileToolErrorKind::ResolvedInputRequired,
                "resolved input physical root is not configured",
            )
        })?;
    let canonical_virtual_path = crate::tool::inputs::canonical_virtual_path(virtual_path)
        .map_err(|message| FileToolError::new(FileToolErrorKind::InvalidInput, message))?;
    let relative = Path::new(canonical_virtual_path.as_str());
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(FileToolError::new(
            FileToolErrorKind::InvalidInput,
            "resolved input virtual path is invalid",
        ));
    }
    let path = root.join(relative);
    let canonical = path.canonicalize().map_err(|error| {
        FileToolError::new(
            FileToolErrorKind::AssetUnavailable,
            format!("resolved input is unavailable: {error}"),
        )
        .with_model_path(virtual_path)
    })?;
    if !canonical.starts_with(root) {
        return Err(FileToolError::new(
            FileToolErrorKind::InvalidInput,
            "resolved input escaped its physical root",
        ));
    }
    Ok(ResolvedManifestPath {
        path: canonical,
        display_path: canonical_virtual_path,
    })
}

struct ResolvedManifestPath {
    path: PathBuf,
    display_path: String,
}

fn deferred_input_error(error: DeferredInputResolutionError) -> FileToolError {
    let kind = match error.kind {
        DeferredInputResolutionFailureKind::AssetRemoved => FileToolErrorKind::AssetRemoved,
        DeferredInputResolutionFailureKind::AccessRevoked => FileToolErrorKind::AccessRevoked,
        DeferredInputResolutionFailureKind::SourceDeleted => FileToolErrorKind::SourceDeleted,
        DeferredInputResolutionFailureKind::StaleGeneration => FileToolErrorKind::StaleGeneration,
        DeferredInputResolutionFailureKind::AssetUnavailable => FileToolErrorKind::AssetUnavailable,
        DeferredInputResolutionFailureKind::HostUnavailable => FileToolErrorKind::Io,
    };
    FileToolError::new(kind, error.message)
}

pub(super) fn build_citation_ref(
    agent_run_id: &str,
    input: &ResolvedInput,
    start_line: usize,
    end_line: usize,
    page: Option<u64>,
) -> String {
    let locator = page
        .map(|value| format!("page:{value}"))
        .unwrap_or_else(|| format!("lines:{start_line}-{end_line}"));
    let mut digest = Sha256::new();
    for value in [
        agent_run_id,
        input.input_ref.as_str(),
        input.object_ref.as_str(),
        input.sha256.as_str(),
        locator.as_str(),
    ] {
        digest.update(value.as_bytes());
        digest.update([0]);
    }
    format!("citation:{:x}", digest.finalize())
}

fn is_pdf_path(path: &Path) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .map(|value| value.eq_ignore_ascii_case("pdf"))
        .unwrap_or(false)
}

fn is_binary_document_path(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|value| value.to_str())
            .map(|value| value.to_ascii_lowercase())
            .as_deref(),
        Some(
            "doc"
                | "docx"
                | "ppt"
                | "pptx"
                | "xls"
                | "xlsx"
                | "odt"
                | "ods"
                | "odp"
                | "png"
                | "jpg"
                | "jpeg"
                | "tif"
                | "tiff"
        )
    )
}

#[cfg(test)]
fn file_content_hash_for_bytes(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::sandbox::{
        SandboxErr, SandboxPolicy, SandboxTransformRequest, SandboxType,
    };
    use crate::execution::{
        ExecutionFileSystemError, ExecutionFileSystemErrorKind, ExecutionFileSystemRequest,
        ExecutionHostCommandOutput, ExecutionHostHealth, ExecutionHostRunner, ExecutionHostStatus,
    };
    use crate::tool::inputs::{
        DeclaredInput, DeferredInputResolutionError, DeferredInputResolutionFailureKind,
        DeferredInputResolverPort, ResolvedInput, ResolvedInputManifest, ResolvedInputState,
        DECLARED_INPUT_SCHEMA, RESOLVED_INPUT_MANIFEST_SCHEMA, RESOLVED_INPUT_SCHEMA,
    };
    use crate::tool::layer::{ResolvedInputReadRequest, ResolvedInputReaderPort};
    use serde_json::json;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    fn test_handler() -> ReadToolHandler {
        ReadToolHandler::new()
    }

    fn read_limit_test_root(case: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "centaeris-read-limit-{case}-{}-{}",
            std::process::id(),
            crate::runtime::contracts::current_timestamp_ms()
        ))
    }

    #[test]
    fn read_defaults_to_two_thousand_lines_and_returns_a_rolling_offset() {
        let root = read_limit_test_root("lines");
        std::fs::create_dir_all(root.as_path()).expect("create read workspace");
        let content = (0..2_005)
            .map(|index| format!("line-{index:04}"))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(root.join("large.txt"), content).expect("write large file");
        let context = ToolRuntimeContext::with_cwd(root.clone()).expect("workspace context");

        let first = test_handler()
            .invoke(json!({"path": "large.txt"}).to_string().as_str(), &context)
            .expect("first read window");
        assert_eq!(first.details["startLine"], 1);
        assert_eq!(first.details["endLine"], READ_MAX_LINES);
        assert_eq!(first.details["maxLines"], READ_MAX_LINES);
        assert_eq!(first.details["maxBytes"], READ_MAX_BYTES);
        assert_eq!(first.details["truncatedBy"], "lines");
        assert_eq!(first.details["nextOffset"], READ_MAX_LINES);
        assert_eq!(
            first.details["content"]
                .as_str()
                .expect("read content")
                .lines()
                .count(),
            READ_MAX_LINES
        );
        assert!(first.content.lines().next().is_some_and(|line| line
            .starts_with("Read large.txt: lines 1-2000 of 2005 ")
            && line.ends_with(
                "truncated=true by lines. Continue with read(path=\"large.txt\", offset=2000)."
            )));
        assert!(first
            .content
            .contains("Continuation: read(path=\"large.txt\", offset=2000)."));

        let second = test_handler()
            .invoke(
                json!({"path": "large.txt", "offset": READ_MAX_LINES})
                    .to_string()
                    .as_str(),
                &context,
            )
            .expect("second read window");
        assert_eq!(second.details["startLine"], 2_001);
        assert_eq!(second.details["endLine"], 2_005);
        assert_eq!(second.details["truncated"], false);
        assert_eq!(
            second.details["content"].as_str(),
            Some("line-2000\nline-2001\nline-2002\nline-2003\nline-2004")
        );

        std::fs::remove_dir_all(root).expect("cleanup read workspace");
    }

    #[test]
    fn read_returns_only_complete_lines_within_the_fifty_kibibyte_cap() {
        let root = read_limit_test_root("bytes");
        std::fs::create_dir_all(root.as_path()).expect("create read workspace");
        let line = "x".repeat(1_024);
        let content = std::iter::repeat_n(line, 60).collect::<Vec<_>>().join("\n");
        std::fs::write(root.join("wide.txt"), content).expect("write wide file");
        let context = ToolRuntimeContext::with_cwd(root.clone()).expect("workspace context");

        let output = test_handler()
            .invoke(json!({"path": "wide.txt"}).to_string().as_str(), &context)
            .expect("bounded byte read");
        let returned = output.details["content"].as_str().expect("read content");
        let output_bytes = output.details["outputBytes"]
            .as_u64()
            .expect("output bytes") as usize;
        assert_eq!(returned.len(), output_bytes);
        assert!(output_bytes <= READ_MAX_BYTES);
        assert_eq!(output.details["truncatedBy"], "bytes");
        assert_eq!(output.details["firstLineExceedsLimit"], false);
        assert_eq!(output.details["nextOffset"], output.details["endLine"]);
        assert!(returned.lines().all(|item| item.len() == 1_024));

        std::fs::remove_dir_all(root).expect("cleanup read workspace");
    }

    #[test]
    fn read_reports_an_oversized_first_line_without_returning_a_partial_line() {
        let root = read_limit_test_root("oversized-line");
        std::fs::create_dir_all(root.as_path()).expect("create read workspace");
        std::fs::write(root.join("one-line.txt"), "x".repeat(READ_MAX_BYTES + 1))
            .expect("write oversized line");
        let context = ToolRuntimeContext::with_cwd(root.clone()).expect("workspace context");

        let output = test_handler()
            .invoke(
                json!({"path": "one-line.txt"}).to_string().as_str(),
                &context,
            )
            .expect("oversized line result");
        assert_eq!(output.details["content"], "");
        assert_eq!(output.details["outputBytes"], 0);
        assert_eq!(output.details["firstLineExceedsLimit"], true);
        assert!(output.details.get("nextOffset").is_none());
        assert!(output.content.contains("never returns partial lines"));

        std::fs::remove_dir_all(root).expect("cleanup read workspace");
    }

    fn test_input(bytes: &[u8]) -> ResolvedInput {
        ResolvedInput {
            schema: RESOLVED_INPUT_SCHEMA.to_string(),
            input_ref: "input_1".to_string(),
            object_ref: "srcobj_1".to_string(),
            owner_kind: "sourceObject".to_string(),
            virtual_path: "sources/srcobj_1/notice.md".to_string(),
            display_name: "notice.md".to_string(),
            content_type: "text/markdown".to_string(),
            size_bytes: bytes.len() as u64,
            sha256: super::file_content_hash_for_bytes(bytes),
            source_version: "1".to_string(),
            evidence_kind: "workspaceSource".to_string(),
            citation_allowed: true,
        }
    }

    fn declared_input(input: &ResolvedInput) -> DeclaredInput {
        DeclaredInput {
            schema: DECLARED_INPUT_SCHEMA.to_string(),
            input_ref: input.input_ref.clone(),
            display_name: input.display_name.clone(),
            content_type: input.content_type.clone(),
            input_identity: crate::tool::inputs::InputIdentityV1 {
                owner_kind: input.owner_kind.clone(),
                owner_id: input.object_ref.clone(),
                generation: input
                    .source_version
                    .parse()
                    .expect("numeric content generation"),
                sha256: input.sha256.clone(),
            },
            size_bytes: input.size_bytes,
        }
    }

    fn manifest_state(
        input: ResolvedInput,
        agent_run_id: &str,
        resolver: Option<Arc<dyn DeferredInputResolverPort>>,
        inputs: Vec<ResolvedInput>,
    ) -> Arc<ResolvedInputState> {
        let authorization_digest = format!("sha256:{}", "a".repeat(64));
        Arc::new(
            ResolvedInputState::new(
                agent_run_id.to_string(),
                authorization_digest.clone(),
                vec![declared_input(&input)],
                ResolvedInputManifest {
                    schema: RESOLVED_INPUT_MANIFEST_SCHEMA.to_string(),
                    agent_run_id: agent_run_id.to_string(),
                    authorization_digest,
                    inputs,
                },
                resolver,
            )
            .expect("manifest state"),
        )
    }

    struct TestDeferredResolver {
        result: Result<ResolvedInput, DeferredInputResolutionError>,
        calls: AtomicUsize,
    }

    struct TestRemoteRunner;

    impl ExecutionHostRunner for TestRemoteRunner {
        fn status(&self, _policy: &SandboxPolicy) -> Result<ExecutionHostStatus, SandboxErr> {
            Ok(ExecutionHostStatus::remote(
                SandboxType::OciContainer,
                ExecutionHostHealth::Ready,
                None,
            ))
        }

        fn run_file_system_operation(
            &self,
            _request: ExecutionFileSystemRequest,
        ) -> Result<ExecutionFileSystemOutput, ExecutionFileSystemError> {
            Err(ExecutionFileSystemError::new(
                ExecutionFileSystemErrorKind::InvalidPath,
                "attachment must not reach the execution filesystem",
            ))
        }

        fn run_host_command(
            &self,
            _operation_id: Option<&str>,
            _request: SandboxTransformRequest,
            _cancellation_probe: Option<&crate::execution::ExecutionCancellationProbe>,
        ) -> Result<ExecutionHostCommandOutput, SandboxErr> {
            unreachable!("attachment read must not invoke Bash")
        }
    }

    struct TestKnowledgePort {
        reads: AtomicUsize,
    }

    impl ResolvedInputReaderPort for TestKnowledgePort {
        fn read(&self, request: ResolvedInputReadRequest) -> Result<LocalToolOutput, String> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            assert_eq!(request.inputs[0].input_ref, "input_1");
            Ok(LocalToolOutput::success(
                "knowledge text",
                json!({
                    "source": "knowledge",
                    "startLine": 1,
                    "endLine": 2000,
                    "nextOffset": 2000,
                    "pageStart": 1,
                    "pageEnd": 40,
                }),
            ))
        }
    }

    impl DeferredInputResolverPort for TestDeferredResolver {
        fn resolve_deferred_input(
            &self,
            reference: &DeclaredInput,
        ) -> Result<ResolvedInput, DeferredInputResolutionError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            match &self.result {
                Ok(input) => {
                    assert_eq!(reference.input_ref, input.input_ref);
                    Ok(input.clone())
                }
                Err(error) => Err(error.clone()),
            }
        }
    }

    #[test]
    fn read_revalidates_a_deferred_input_while_preserving_the_append_only_ledger() {
        let root = std::env::temp_dir().join(format!(
            "centaeris-deferred-read-{}",
            crate::runtime::contracts::current_timestamp_ms()
        ));
        let inputs = root.join("inputs");
        let file = inputs.join("sources/srcobj_1/notice.md");
        std::fs::create_dir_all(file.parent().expect("file parent")).expect("create inputs");
        let bytes = b"deferred input\n";
        std::fs::write(file.as_path(), bytes).expect("write input materialized by host");
        let input = test_input(bytes);
        let resolver = Arc::new(TestDeferredResolver {
            result: Ok(input.clone()),
            calls: AtomicUsize::new(0),
        });
        let manifest = manifest_state(input, "agent_run_1", Some(resolver.clone()), Vec::new());
        let context = ToolRuntimeContext::with_cwd(inputs.clone())
            .expect("workspace context")
            .with_resolved_input_manifest(manifest)
            .with_resolved_input_root(inputs)
            .expect("resolved input root")
            .with_tool_invocation("call_read", "read");
        let handler = test_handler();
        handler
            .invoke(
                json!({"input_ref": "input_1"}).to_string().as_str(),
                &context,
            )
            .expect("first deferred read");
        handler
            .invoke(
                json!({"input_ref": "input_1"}).to_string().as_str(),
                &context,
            )
            .expect("second resolved read");
        assert_eq!(resolver.calls.load(Ordering::SeqCst), 2);
        std::fs::remove_dir_all(root).expect("cleanup deferred read test");
    }

    #[test]
    fn deferred_removed_input_returns_an_asset_removed_tool_result() {
        let input = test_input(b"deleted");
        let resolver = Arc::new(TestDeferredResolver {
            result: Err(DeferredInputResolutionError::new(
                DeferredInputResolutionFailureKind::AssetRemoved,
                "asset was deleted",
            )),
            calls: AtomicUsize::new(0),
        });
        let manifest = manifest_state(input, "agent_run_1", Some(resolver), Vec::new());
        let root = std::env::temp_dir().join(format!(
            "centaeris-deferred-deleted-read-{}",
            crate::runtime::contracts::current_timestamp_ms()
        ));
        std::fs::create_dir_all(root.as_path()).expect("create temp root");
        let context = ToolRuntimeContext::with_cwd(root.clone())
            .expect("workspace context")
            .with_resolved_input_manifest(manifest);
        let output = test_handler()
            .invoke(
                json!({"input_ref": "input_1"}).to_string().as_str(),
                &context,
            )
            .expect_err("deleted input must reject the Read tool");
        assert!(output.details.to_string().contains("asset_removed"));
        std::fs::remove_dir_all(root).expect("cleanup deleted read test");
    }

    #[test]
    fn deferred_source_deleted_input_returns_an_exact_tool_result() {
        let input = test_input(b"deleted source");
        let resolver = Arc::new(TestDeferredResolver {
            result: Err(DeferredInputResolutionError::new(
                DeferredInputResolutionFailureKind::SourceDeleted,
                "source was deleted",
            )),
            calls: AtomicUsize::new(0),
        });
        let manifest = manifest_state(input, "agent_run_1", Some(resolver), Vec::new());
        let root = std::env::temp_dir().join(format!(
            "centaeris-deferred-source-deleted-read-{}",
            crate::runtime::contracts::current_timestamp_ms()
        ));
        std::fs::create_dir_all(root.as_path()).expect("create temp root");
        let context = ToolRuntimeContext::with_cwd(root.clone())
            .expect("workspace context")
            .with_resolved_input_manifest(manifest);
        let output = test_handler()
            .invoke(
                json!({"input_ref": "input_1"}).to_string().as_str(),
                &context,
            )
            .expect_err("deleted Source input must reject the Read tool");
        assert!(output.details.to_string().contains("source_deleted"));
        assert!(output
            .content
            .contains("Source containing this attached session file was deleted"));
        std::fs::remove_dir_all(root).expect("cleanup deleted Source read test");
    }

    #[test]
    fn remote_input_ref_uses_knowledge_and_rejects_its_virtual_path_alias() {
        let input = test_input(b"remote source");
        let manifest = manifest_state(input.clone(), "agent_run_1", None, vec![input]);
        let workspace = std::env::temp_dir();
        let binding = Arc::new(
            crate::execution::ExecutionHostBinding::new(
                crate::execution::ExecutionHostMode::Remote,
                Arc::new(TestRemoteRunner),
                workspace.clone(),
                SandboxPolicy::workspace_write_no_network(workspace),
            )
            .expect("remote execution host"),
        );
        let handler = ReadToolHandler::new();
        let knowledge = Arc::new(TestKnowledgePort {
            reads: AtomicUsize::new(0),
        });
        let context = ToolRuntimeContext::default()
            .with_execution_host_binding(binding)
            .with_resolved_input_manifest(manifest)
            .with_resolved_input_reader(knowledge.clone())
            .with_tool_invocation("call_1", "read");

        let output = handler
            .invoke(
                json!({"input_ref": "input_1"}).to_string().as_str(),
                &context,
            )
            .expect("remote inputRef must use Knowledge");
        assert!(output.content.starts_with(
            "Read pages 1-40; truncated. Continue with read(input_ref=\"input_1\", offset=2000).\nknowledge text"
        ));
        assert!(output
            .content
            .ends_with("Continuation: read(input_ref=\"input_1\", offset=2000)."));
        assert_eq!(knowledge.reads.load(Ordering::SeqCst), 1);

        let error = handler
            .invoke(
                json!({"path": "sources/srcobj_1/notice.md"})
                    .to_string()
                    .as_str(),
                &context,
            )
            .expect_err("remote input path alias must fail loudly");
        assert!(error.content.contains("use input_ref"));
    }

    #[test]
    fn read_returns_stable_citation_and_rejects_stale_or_unknown_input() {
        let root = std::env::temp_dir().join(format!(
            "centaeris-resolved-read-{}",
            crate::runtime::contracts::current_timestamp_ms()
        ));
        let inputs = root.join("inputs");
        let file = inputs.join("sources/srcobj_1/notice.md");
        std::fs::create_dir_all(file.parent().expect("file parent")).expect("create inputs");
        let bytes = "第一行\n术前注意事项\n第三行\n".as_bytes();
        std::fs::write(file.as_path(), bytes).expect("write input");
        let input = test_input(bytes);
        let manifest = manifest_state(input.clone(), "agent_run_1", None, vec![input]);
        let context = ToolRuntimeContext::with_cwd(inputs.clone())
            .expect("workspace context")
            .with_resolved_input_manifest(manifest)
            .with_resolved_input_root(inputs.clone())
            .expect("resolved input root")
            .with_tool_invocation("call_read", "read");
        let knowledge_handler = test_handler();
        let path_handler = test_handler();

        let first = knowledge_handler
            .invoke(
                json!({"input_ref": "input_1", "offset": 1, "limit": 2})
                    .to_string()
                    .as_str(),
                &context,
            )
            .expect("read input");
        let second = path_handler
            .invoke(
                json!({"path": "sources/srcobj_1/notice.md", "offset": 1, "limit": 2})
                    .to_string()
                    .as_str(),
                &context,
            )
            .expect("read virtual path");
        let first = first.details;
        let second = second.details;
        assert_eq!(first["inputRef"], "input_1");
        assert_eq!(first["citationRef"], second["citationRef"]);
        assert_eq!(first["ownerRef"], "srcobj_1");
        assert_eq!(first["evidenceKind"], "workspaceSource");

        let mut user_input = test_input(bytes);
        user_input.object_ref = "library_1".to_string();
        user_input.owner_kind = "userLibraryObject".to_string();
        user_input.evidence_kind = "userProvided".to_string();
        let user_context = ToolRuntimeContext::with_cwd(inputs.clone())
            .expect("workspace context")
            .with_resolved_input_manifest(manifest_state(
                user_input.clone(),
                "agent_run_1",
                None,
                vec![user_input],
            ))
            .with_resolved_input_root(inputs.clone())
            .expect("resolved input root")
            .with_tool_invocation("call_read", "read");
        let user_read = knowledge_handler
            .invoke(
                json!({"input_ref": "input_1"}).to_string().as_str(),
                &user_context,
            )
            .expect("read user input")
            .details;
        assert_eq!(user_read["ownerKind"], "userLibraryObject");
        assert_eq!(user_read["evidenceKind"], "userProvided");
        assert!(user_read["citationRef"].as_str().is_some());

        let mut artifact_input = test_input(bytes);
        artifact_input.object_ref = "artifact_1".to_string();
        artifact_input.owner_kind = "artifact".to_string();
        artifact_input.evidence_kind = "generatedArtifact".to_string();
        artifact_input.citation_allowed = false;
        let artifact_context = ToolRuntimeContext::with_cwd(inputs.clone())
            .expect("workspace context")
            .with_resolved_input_manifest(manifest_state(
                artifact_input.clone(),
                "agent_run_1",
                None,
                vec![artifact_input],
            ))
            .with_resolved_input_root(inputs.clone())
            .expect("resolved input root")
            .with_tool_invocation("call_read", "read");
        let artifact_read = knowledge_handler
            .invoke(
                json!({"input_ref": "input_1"}).to_string().as_str(),
                &artifact_context,
            )
            .expect("read artifact input")
            .details;
        assert_eq!(artifact_read["ownerKind"], "artifact");
        assert!(artifact_read["citationRef"].is_null());

        let mut next_agent_run_input = test_input(bytes);
        next_agent_run_input.object_ref = "library_1".to_string();
        next_agent_run_input.owner_kind = "userLibraryObject".to_string();
        next_agent_run_input.evidence_kind = "userProvided".to_string();
        let next_agent_run_context = ToolRuntimeContext::with_cwd(inputs.clone())
            .expect("workspace context")
            .with_resolved_input_manifest(manifest_state(
                next_agent_run_input.clone(),
                "agent_run_2",
                None,
                vec![next_agent_run_input],
            ))
            .with_resolved_input_root(inputs.clone())
            .expect("resolved input root")
            .with_tool_invocation("call_read", "read");
        let next_run = knowledge_handler
            .invoke(
                json!({"input_ref": "input_1"}).to_string().as_str(),
                &next_agent_run_context,
            )
            .expect("read next run")
            .details;
        assert_ne!(user_read["citationRef"], next_run["citationRef"]);

        let unknown = knowledge_handler
            .invoke(
                json!({"input_ref": "banana"}).to_string().as_str(),
                &context,
            )
            .expect_err("unknown input must fail");
        assert!(unknown.details.to_string().contains("asset_unavailable"));
        std::fs::write(file.as_path(), "tampered").expect("tamper input");
        let stale = knowledge_handler
            .invoke(
                json!({"input_ref": "input_1"}).to_string().as_str(),
                &context,
            )
            .expect_err("stale input must fail");
        assert!(stale.details.to_string().contains("stale_input"));

        std::fs::remove_dir_all(root).expect("cleanup read test");
    }

    #[test]
    fn read_batch_returns_one_citation_per_input_and_rejects_partial_contracts() {
        let root = std::env::temp_dir().join(format!(
            "centaeris-resolved-read-batch-{}",
            crate::runtime::contracts::current_timestamp_ms()
        ));
        let inputs = root.join("inputs");
        std::fs::create_dir_all(inputs.join("sources/srcobj_1")).expect("first input root");
        std::fs::create_dir_all(inputs.join("sources/srcobj_2")).expect("second input root");
        let first_bytes = b"first source\n";
        let second_bytes = b"second source\n";
        std::fs::write(inputs.join("sources/srcobj_1/notice.md"), first_bytes)
            .expect("first input");
        std::fs::write(inputs.join("sources/srcobj_2/policy.md"), second_bytes)
            .expect("second input");
        let first = test_input(first_bytes);
        let mut second = test_input(second_bytes);
        second.input_ref = "input_2".to_string();
        second.object_ref = "srcobj_2".to_string();
        second.virtual_path = "sources/srcobj_2/policy.md".to_string();
        second.display_name = "policy.md".to_string();
        let authorization_digest = format!("sha256:{}", "a".repeat(64));
        let state = Arc::new(
            ResolvedInputState::new(
                "agent_run_batch".to_string(),
                authorization_digest.clone(),
                vec![declared_input(&first), declared_input(&second)],
                ResolvedInputManifest {
                    schema: RESOLVED_INPUT_MANIFEST_SCHEMA.to_string(),
                    agent_run_id: "agent_run_batch".to_string(),
                    authorization_digest,
                    inputs: vec![first, second],
                },
                None,
            )
            .expect("batch manifest state"),
        );
        let context = ToolRuntimeContext::with_cwd(inputs.clone())
            .expect("workspace context")
            .with_resolved_input_manifest(state)
            .with_resolved_input_root(inputs)
            .expect("resolved input root")
            .with_tool_invocation("call_read", "read");
        let handler = test_handler();
        let output = handler
            .invoke(
                json!({"input_refs": ["input_1", "input_2"]})
                    .to_string()
                    .as_str(),
                &context,
            )
            .expect("batch read")
            .details;
        assert_eq!(output["schema"], "read_batch_result.v1");
        assert_eq!(output["items"].as_array().expect("batch items").len(), 2);
        assert!(output["items"][0]["citationRef"].as_str().is_some());
        assert!(output["items"][1]["citationRef"].as_str().is_some());
        assert_ne!(
            output["items"][0]["citationRef"],
            output["items"][1]["citationRef"]
        );

        for invalid in [
            json!({"input_refs": ["input_1", "input_1"]}),
            json!({"input_refs": ["input_1"], "offset": 1}),
        ] {
            let rejected = handler
                .invoke(invalid.to_string().as_str(), &context)
                .expect_err("invalid batch must reject");
            assert!(rejected.details.to_string().contains("invalid_input"));
        }
        std::fs::remove_dir_all(root).expect("cleanup batch read test");
    }

    #[test]
    fn resolved_binary_document_requires_the_document_execution_capability() {
        let root = std::env::temp_dir().join(format!(
            "centaeris-resolved-pdf-read-{}",
            crate::runtime::contracts::current_timestamp_ms()
        ));
        let inputs = root.join("inputs");
        let file = inputs.join("sources/srcobj_1/scan.pdf");
        std::fs::create_dir_all(file.parent().expect("file parent")).expect("create inputs");
        let bytes = b"%PDF-1.4 scan fixture";
        std::fs::write(file.as_path(), bytes).expect("write pdf");
        let mut input = test_input(bytes);
        input.virtual_path = "sources/srcobj_1/scan.pdf".to_string();
        input.display_name = "scan.pdf".to_string();
        input.content_type = "application/pdf".to_string();
        let manifest = manifest_state(input.clone(), "agent_run_1", None, vec![input]);
        let context = ToolRuntimeContext::with_cwd(inputs.clone())
            .expect("workspace context")
            .with_resolved_input_manifest(manifest)
            .with_resolved_input_root(inputs)
            .expect("resolved input root")
            .with_tool_invocation("call_read", "read");
        let error = test_handler()
            .invoke(
                json!({"input_ref": "input_1"}).to_string().as_str(),
                &context,
            )
            .expect_err("binary document must require hosted processing");
        assert!(error
            .details
            .to_string()
            .contains("hosted Processing Pipeline"));

        std::fs::remove_dir_all(root).expect("cleanup pdf read test");
    }

    #[test]
    fn directory_list_separates_manifest_inputs_from_signed_writable_paths() {
        let root = std::env::temp_dir().join(format!(
            "centaeris-resolved-directory-list-{}-{}",
            std::process::id(),
            crate::runtime::contracts::current_timestamp_ms()
        ));
        let workspace = root.join("work");
        let inputs = root.join("inputs");
        std::fs::create_dir_all(workspace.join("project")).expect("project root");
        std::fs::create_dir_all(inputs.join("source")).expect("source root");

        let mut manifest_inputs = Vec::new();
        let mut declared_inputs = Vec::new();
        for index in 0..205usize {
            let content = format!("source {index}\n");
            let input_ref = format!("input_{index:03}");
            let virtual_path = format!("source/{index:03}.md");
            std::fs::write(inputs.join(virtual_path.as_str()), content.as_bytes())
                .expect("manifest input");
            let input = ResolvedInput {
                schema: RESOLVED_INPUT_SCHEMA.to_string(),
                input_ref,
                object_ref: format!("source_object_{index}"),
                owner_kind: "sourceObject".to_string(),
                virtual_path,
                display_name: format!("{index:03}.md"),
                content_type: "text/markdown".to_string(),
                size_bytes: content.len() as u64,
                sha256: super::file_content_hash_for_bytes(content.as_bytes()),
                source_version: "1".to_string(),
                evidence_kind: "workspaceSource".to_string(),
                citation_allowed: true,
            };
            declared_inputs.push(declared_input(&input));
            manifest_inputs.push(input);
        }
        std::fs::write(inputs.join("source/extra.md"), b"not authorized\n")
            .expect("extra physical file");
        let authorization_digest = format!("sha256:{}", "a".repeat(64));
        let state = Arc::new(
            ResolvedInputState::new(
                "agent_run_list".to_string(),
                authorization_digest.clone(),
                declared_inputs,
                ResolvedInputManifest {
                    schema: RESOLVED_INPUT_MANIFEST_SCHEMA.to_string(),
                    agent_run_id: "agent_run_list".to_string(),
                    authorization_digest,
                    inputs: manifest_inputs,
                },
                None,
            )
            .expect("manifest state"),
        );
        let context = ToolRuntimeContext::with_cwd(workspace.clone())
            .expect("workspace context")
            .with_resolved_input_manifest(state)
            .with_resolved_input_root(inputs)
            .expect("resolved input root")
            .with_tool_invocation("call_read", "read");
        let handler = test_handler();

        let first_page = handler
            .invoke(
                json!({"operation": "list", "path": "source", "limit": 200})
                    .to_string()
                    .as_str(),
                &context,
            )
            .expect("first manifest page")
            .details;
        assert_eq!(first_page["totalEntries"], 205);
        assert_eq!(first_page["nextOffset"], 200);
        assert_eq!(
            first_page["entries"].as_array().expect("entries").len(),
            200
        );
        assert!(first_page.to_string().find("extra.md").is_none());
        assert!(handler
            .invoke(
                json!({"operation": "list", "path": "/workspace/source", "limit": 200})
                    .to_string()
                    .as_str(),
                &context,
            )
            .is_err());
        let reusable_path = first_page["entries"][0]["path"]
            .as_str()
            .expect("display path")
            .to_string();
        assert!(handler
            .invoke(
                json!({"path": reusable_path}).to_string().as_str(),
                &context
            )
            .is_ok());

        let empty_project = handler
            .invoke(
                json!({"operation": "list", "path": "project"})
                    .to_string()
                    .as_str(),
                &context,
            )
            .expect("empty project list")
            .details;
        assert_eq!(empty_project["totalEntries"], 0);
        for path in [".".to_string(), workspace.to_string_lossy().to_string()] {
            let workspace_root = handler
                .invoke(
                    json!({"operation": "list", "path": path})
                        .to_string()
                        .as_str(),
                    &context,
                )
                .expect("workspace root list")
                .details;
            assert_eq!(workspace_root["entries"][0]["path"], "project");
        }
        assert!(handler
            .invoke(
                json!({"operation": "list", "path": "banana"})
                    .to_string()
                    .as_str(),
                &context,
            )
            .is_err());
        std::fs::remove_dir_all(root).expect("cleanup directory list test");
    }
}
