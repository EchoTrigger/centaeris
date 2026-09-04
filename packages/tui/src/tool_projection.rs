use serde::Deserialize;
use serde_json::Value;

const TOOL_RESULT_MAX_LINES: usize = 6;
const TOOL_RESULT_HEAD_LINES: usize = 2;
const TOOL_RESULT_TAIL_LINES: usize = 3;
pub(crate) const DIFF_PREVIEW_MAX_ROWS: usize = 4;
const DIFF_GROUP_MAX_ROWS: usize = 80;
const DIFF_CONTEXT_LINES: usize = 2;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ToolProjection {
    open_calls: Vec<ToolCall>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ToolTranscriptLine {
    pub(crate) key: String,
    pub(crate) action_kind: ToolActionKind,
    pub(crate) subject: String,
    pub(crate) operations: Vec<ToolOperation>,
    pub(crate) result_blocks: Vec<ToolResultBlock>,
    pub(crate) images: Vec<ToolImage>,
    pub(crate) result_states: Vec<ToolResultState>,
    pub(crate) interrupted: bool,
    pub(crate) running: bool,
    pub(crate) command: Option<String>,
    pub(crate) description_title: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ToolCall {
    call_id: String,
    tool_name: String,
    display_target: Option<String>,
    command: Option<String>,
    description: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ToolResult {
    call_id: String,
    result_state: ToolResultState,
    model_content: Option<String>,
    hint_lines: Vec<String>,
    operations: Vec<ToolOperation>,
    images: Vec<ToolImage>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ToolImage {
    pub(crate) key: String,
    pub(crate) path: String,
    pub(crate) content_type: String,
    pub(crate) sha256: String,
    pub(crate) byte_length: u64,
    pub(crate) width_px: u32,
    pub(crate) height_px: u32,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "sourceKind", rename_all = "camelCase", deny_unknown_fields)]
enum ToolImageSource {
    InputRef {
        input_ref: String,
        content_type: String,
        placeholder: String,
    },
    ExecutionFile {
        image: ExecutionToolImage,
    },
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ExecutionToolImage {
    path: String,
    content_type: String,
    sha256: String,
    byte_length: u64,
    width_px: u32,
    height_px: u32,
    placeholder: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ToolOperation {
    pub(crate) call_id: String,
    pub(crate) tool_name: Option<String>,
    pub(crate) kind: String,
    pub(crate) title: String,
    pub(crate) status: Option<String>,
    pub(crate) path: Option<String>,
    pub(crate) start_line: Option<u32>,
    pub(crate) end_line: Option<u32>,
    pub(crate) total_lines: Option<u32>,
    pub(crate) next_offset: Option<u32>,
    pub(crate) truncated_by: Option<String>,
    pub(crate) command_preview: Option<String>,
    pub(crate) query: Option<String>,
    pub(crate) added: Option<u64>,
    pub(crate) removed: Option<u64>,
    pub(crate) output_preview: Option<String>,
    pub(crate) error: Option<String>,
    pub(crate) text: Option<String>,
    pub(crate) diff_rows: Option<Vec<DiffRow>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ToolActionKind {
    Browser,
    Plugin,
    Host,
    Edit,
    Command,
    Read,
    Search,
    Tool,
}

impl ToolActionKind {
    pub(crate) fn active_label(self) -> &'static str {
        match self {
            Self::Browser => "Using browser",
            Self::Plugin | Self::Tool => "Calling tool",
            Self::Host => "Running host operation",
            Self::Edit => "Editing files",
            Self::Command => "Running command",
            Self::Read => "Reading files",
            Self::Search => "Searching files",
        }
    }

    fn running_verb(self) -> &'static str {
        match self {
            Self::Browser => "Using",
            Self::Plugin | Self::Host | Self::Tool => "Calling",
            Self::Edit => "Editing",
            Self::Command => "Running",
            Self::Read => "Reading",
            Self::Search => "Searching",
        }
    }

    pub(crate) fn succeeded_verb(self) -> &'static str {
        match self {
            Self::Browser => "Used",
            Self::Plugin | Self::Host | Self::Tool => "Called",
            Self::Command => "Ran",
            Self::Edit => "Edited",
            Self::Read => "Read",
            Self::Search => "Searched",
        }
    }

    pub(crate) fn action_noun(self) -> &'static str {
        match self {
            Self::Browser => "Browser action",
            Self::Plugin => "Plugin",
            Self::Host => "Host operation",
            Self::Edit => "Edit",
            Self::Command => "Command",
            Self::Read => "Read",
            Self::Search => "Search",
            Self::Tool => "Tool",
        }
    }

    fn default_subject(self) -> &'static str {
        match self {
            Self::Browser => "browser action",
            Self::Plugin => "plugin",
            Self::Host => "host operation",
            Self::Edit => "files",
            Self::Command => "command",
            Self::Read => "files",
            Self::Search => "files",
            Self::Tool => "tool",
        }
    }

    pub(crate) fn failure_verb(self) -> &'static str {
        match self {
            Self::Browser => "use the browser",
            Self::Plugin => "run the plugin",
            Self::Host => "run the host operation",
            Self::Edit => "edit",
            Self::Command => "run",
            Self::Read => "read",
            Self::Search => "search",
            Self::Tool => "run the tool",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ToolResultState {
    SuccessWithOutput,
    SuccessNoOutput,
    SuccessNoMatches,
    Failed,
    Denied,
    Aborted,
}

impl ToolResultState {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "successWithOutput" => Ok(Self::SuccessWithOutput),
            "successNoOutput" => Ok(Self::SuccessNoOutput),
            "successNoMatches" => Ok(Self::SuccessNoMatches),
            "failed" => Ok(Self::Failed),
            "denied" => Ok(Self::Denied),
            "aborted" => Ok(Self::Aborted),
            other => Err(format!("unsupported resultState: {other}")),
        }
    }

    fn accepts_event_status(self, status: &str) -> bool {
        match self {
            Self::Failed => status == "error",
            _ => status == "done",
        }
    }

    fn requires_write_preview(self) -> bool {
        !matches!(self, Self::Failed | Self::Denied | Self::Aborted)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ToolOutcome {
    Succeeded,
    Failed,
    Denied,
    Aborted,
}

pub(crate) fn tool_outcome(states: &[ToolResultState]) -> ToolOutcome {
    if states.contains(&ToolResultState::Failed) {
        ToolOutcome::Failed
    } else if states.contains(&ToolResultState::Denied) {
        ToolOutcome::Denied
    } else if states.contains(&ToolResultState::Aborted) {
        ToolOutcome::Aborted
    } else {
        ToolOutcome::Succeeded
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ToolResultBlock {
    Text {
        lines: Vec<TextResultLine>,
    },
    Diff {
        path: String,
        rows: Vec<DiffRow>,
        hidden_lines: usize,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TextResultLine {
    Text(String),
    Hidden(usize),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DiffRow {
    pub(crate) line_number: Option<usize>,
    pub(crate) kind: DiffRowKind,
    pub(crate) text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DiffRowKind {
    Context,
    Insert,
    Delete,
    Hidden(usize),
}

#[derive(Debug)]
pub(crate) struct ToolProjectionUpdate {
    pub(crate) started: Option<ToolTranscriptLine>,
    pub(crate) settled: Option<ToolTranscriptLine>,
    pub(crate) active_label: Option<String>,
}

impl ToolProjection {
    pub(crate) fn apply_event(
        &mut self,
        event_type: &str,
        event: &Value,
        payload: &Value,
    ) -> Result<ToolProjectionUpdate, String> {
        match event_type {
            "ToolCall" => self.apply_call(decode_tool_call(event, payload)?),
            "ToolResult" => self.apply_result(decode_tool_result(event, payload)?),
            other => Err(format!("unsupported tool event type: {other}")),
        }
    }

    pub(crate) fn set_activity(&self, event: &Value) -> Result<String, String> {
        let tool_name = required_event_string(event, "toolName")?;
        Ok(tool_action_kind(tool_name.as_str())
            .active_label()
            .to_string())
    }

    pub(crate) fn seal(&mut self) -> Vec<ToolTranscriptLine> {
        self.open_calls
            .drain(..)
            .map(|call| seal_call(call, None))
            .collect()
    }

    pub(crate) fn has_open_calls(&self) -> bool {
        !self.open_calls.is_empty()
    }

    pub(crate) fn active_label(&self) -> Option<String> {
        self.open_calls.last().map(|call| {
            tool_action_kind(call.tool_name.as_str())
                .active_label()
                .to_string()
        })
    }

    pub(crate) fn clear(&mut self) {
        self.open_calls.clear();
    }

    #[cfg(test)]
    pub(crate) fn open_call_ids(&self) -> Vec<&str> {
        self.open_calls
            .iter()
            .map(|call| call.call_id.as_str())
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn open_tool_name(&self, call_id: &str) -> Option<&str> {
        self.open_calls
            .iter()
            .find(|call| call.call_id == call_id)
            .map(|call| call.tool_name.as_str())
    }

    fn apply_call(&mut self, call: ToolCall) -> Result<ToolProjectionUpdate, String> {
        if self
            .open_calls
            .iter()
            .any(|item| item.call_id == call.call_id)
        {
            return Err(format!("duplicate ToolCall callId: {}", call.call_id));
        }
        let action_kind = tool_action_kind(call.tool_name.as_str());
        let started = running_call_line(&call);
        self.open_calls.push(call);
        Ok(ToolProjectionUpdate {
            started: Some(started),
            settled: None,
            active_label: Some(action_kind.active_label().to_string()),
        })
    }

    fn apply_result(&mut self, result: ToolResult) -> Result<ToolProjectionUpdate, String> {
        let Some(index) = self
            .open_calls
            .iter()
            .position(|call| call.call_id == result.call_id)
        else {
            return Err(format!("ToolResult without ToolCall: {}", result.call_id));
        };
        let call = self.open_calls.remove(index);
        Ok(ToolProjectionUpdate {
            started: None,
            settled: Some(seal_call(call, Some(result))),
            active_label: self.active_label(),
        })
    }
}

pub(crate) fn stable_tool_title(tool: &ToolTranscriptLine) -> String {
    let subject = stable_tool_subject(tool);
    if tool.action_kind == ToolActionKind::Command && tool.description_title {
        return subject;
    }
    if tool.running {
        return format!("{} {subject}", tool.action_kind.running_verb());
    }
    if tool.interrupted {
        return format!("{} stopped: {subject}", tool.action_kind.action_noun());
    }
    match tool_outcome(&tool.result_states) {
        ToolOutcome::Succeeded => {
            format!("{} {subject}", tool.action_kind.succeeded_verb())
        }
        ToolOutcome::Failed => {
            format!("Failed to {} {subject}", tool.action_kind.failure_verb())
        }
        ToolOutcome::Denied => {
            format!("{} denied: {subject}", tool.action_kind.action_noun())
        }
        ToolOutcome::Aborted => {
            format!("{} cancelled: {subject}", tool.action_kind.action_noun())
        }
    }
}

pub(crate) fn empty_tool_result_detail(tool: &ToolTranscriptLine) -> Option<String> {
    if tool.running {
        return None;
    }
    if tool.interrupted {
        return Some("Stopped".to_string());
    }
    if tool.action_kind == ToolActionKind::Edit {
        return None;
    }
    if !tool.result_states.is_empty()
        && tool
            .result_states
            .iter()
            .all(|state| *state == ToolResultState::SuccessNoOutput)
    {
        return Some("No output".to_string());
    }
    if tool
        .result_states
        .contains(&ToolResultState::SuccessNoMatches)
    {
        return Some("No matches".to_string());
    }
    if !tool.result_blocks.is_empty() {
        return None;
    }
    if tool.action_kind == ToolActionKind::Read
        && tool
            .operations
            .iter()
            .any(|operation| operation.next_offset.is_some() || operation.truncated_by.is_some())
    {
        return tool.operations.iter().find_map(partial_read_detail);
    }
    if tool.result_states.contains(&ToolResultState::Failed) {
        return Some("No error output".to_string());
    }
    if tool.result_states.contains(&ToolResultState::Denied) {
        return Some("Permission denied".to_string());
    }
    if tool.result_states.contains(&ToolResultState::Aborted) {
        return Some("Cancelled".to_string());
    }
    None
}

fn decode_tool_call(event: &Value, payload: &Value) -> Result<ToolCall, String> {
    ensure_event_status("ToolCall", event)?;
    let input = payload.get("normalizedInput").and_then(Value::as_object);
    Ok(ToolCall {
        call_id: required_payload_string(payload, "callId")?,
        tool_name: required_event_string(event, "toolName")?,
        display_target: payload
            .get("displayTarget")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        command: payload
            .get("command")
            .and_then(Value::as_str)
            .or_else(|| {
                input
                    .and_then(|input| input.get("command"))
                    .and_then(Value::as_str)
            })
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        description: payload
            .get("description")
            .and_then(Value::as_str)
            .or_else(|| {
                input
                    .and_then(|input| input.get("description"))
                    .and_then(Value::as_str)
            })
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
    })
}

fn decode_tool_result(event: &Value, payload: &Value) -> Result<ToolResult, String> {
    let status = ensure_event_status("ToolResult", event)?;
    let tool_name = required_event_string(event, "toolName")?;
    let result_state =
        ToolResultState::parse(required_payload_string(payload, "resultState")?.as_str())?;
    if !result_state.accepts_event_status(status.as_str()) {
        return Err("event.status does not match payload.resultState".to_string());
    }
    let call_id = required_payload_string(payload, "callId")?;
    let operations = payload
        .get("operations")
        .and_then(Value::as_array)
        .ok_or_else(|| "missing payload.operations".to_string())?;
    if operations.is_empty() {
        return Err("payload.operations is empty".to_string());
    }
    let operations = operations
        .iter()
        .map(|operation| {
            decode_tool_operation(
                operation,
                result_state,
                call_id.as_str(),
                tool_name.as_str(),
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let hint_lines = payload
        .get("hintLines")
        .map(|value| {
            value
                .as_array()
                .ok_or_else(|| "payload.hintLines is not an array".to_string())?
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .map(str::to_string)
                        .ok_or_else(|| "payload.hintLines contains a non-string value".to_string())
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?
        .unwrap_or_default();
    let image_sources = serde_json::from_value::<Vec<ToolImageSource>>(
        payload
            .get("modelInputImages")
            .cloned()
            .ok_or_else(|| "missing payload.modelInputImages".to_string())?,
    )
    .map_err(|error| format!("invalid payload.modelInputImages: {error}"))?;
    let mut images = Vec::new();
    for (index, source) in image_sources.into_iter().enumerate() {
        match source {
            ToolImageSource::InputRef {
                input_ref,
                content_type,
                placeholder,
            } => validate_image_source_fields(
                input_ref.as_str(),
                content_type.as_str(),
                placeholder.as_str(),
            )?,
            ToolImageSource::ExecutionFile { image } => {
                validate_execution_tool_image(&image)?;
                images.push(ToolImage {
                    key: format!("tool_image:{call_id}:{index}"),
                    path: image.path,
                    content_type: image.content_type,
                    sha256: image.sha256,
                    byte_length: image.byte_length,
                    width_px: image.width_px,
                    height_px: image.height_px,
                });
            }
        }
    }
    Ok(ToolResult {
        call_id,
        result_state,
        model_content: payload
            .get("modelContent")
            .and_then(Value::as_str)
            .map(str::to_string),
        hint_lines,
        operations,
        images,
    })
}

fn validate_image_source_fields(
    identity: &str,
    content_type: &str,
    placeholder: &str,
) -> Result<(), String> {
    if identity.trim().is_empty()
        || placeholder.trim().is_empty()
        || !matches!(content_type, "image/png" | "image/jpeg" | "image/webp")
    {
        return Err("payload.modelInputImages contains an invalid image reference".to_string());
    }
    Ok(())
}

fn validate_execution_tool_image(image: &ExecutionToolImage) -> Result<(), String> {
    validate_image_source_fields(
        image.path.as_str(),
        image.content_type.as_str(),
        image.placeholder.as_str(),
    )?;
    let digest = image.sha256.strip_prefix("sha256:");
    if image.byte_length == 0
        || image.width_px == 0
        || image.height_px == 0
        || digest.is_none_or(|digest| {
            digest.len() != 64
                || !digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
    {
        return Err(
            "payload.modelInputImages contains invalid execution image metadata".to_string(),
        );
    }
    Ok(())
}

fn decode_tool_operation(
    value: &Value,
    result_state: ToolResultState,
    expected_call_id: &str,
    expected_tool_name: &str,
) -> Result<ToolOperation, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "payload.operations contains a non-object value".to_string())?;
    let call_id = required_object_string(object, "callId")?;
    if call_id != expected_call_id {
        return Err(format!(
            "{expected_call_id} has operation with mismatched callId"
        ));
    }
    let tool_name = required_object_string(object, "toolName")?;
    if tool_name != expected_tool_name {
        return Err(format!(
            "{expected_call_id} has operation with mismatched toolName"
        ));
    }
    let wire_kind = optional_object_string(object, "kind");
    if expected_tool_name == "bash" {
        if wire_kind.as_deref() != Some("command") {
            return Err("bash operation requires kind=command".to_string());
        }
    } else if wire_kind.is_some() {
        return Err(format!(
            "non-bash operation must not carry kind: {expected_tool_name}"
        ));
    }
    let kind = wire_kind.unwrap_or_else(|| expected_tool_name.to_string());
    let title = optional_object_string(object, "title").unwrap_or_else(|| kind.clone());
    let path = optional_object_string(object, "path");
    let start_line = optional_object_u32(object, "startLine")?;
    let end_line = optional_object_u32(object, "endLine")?;
    let total_lines = optional_object_u32(object, "totalLines")?;
    let next_offset = optional_object_u32(object, "nextOffset")?;
    let truncated_by = optional_object_string(object, "truncatedBy");
    if kind != "read" && (total_lines.is_some() || next_offset.is_some() || truncated_by.is_some())
    {
        return Err("read coverage field on non-read operation".to_string());
    }
    if kind == "read" {
        if end_line.is_some_and(|end| start_line.is_some_and(|start| end < start)) {
            return Err("read operation endLine precedes startLine".to_string());
        }
        if total_lines.is_some_and(|total| end_line.is_some_and(|end| total < end)) {
            return Err("read operation totalLines precedes endLine".to_string());
        }
    }
    let diff_preview = optional_object_string(object, "diffPreview");
    let is_write = matches!(kind.as_str(), "edit" | "write" | "delete");
    let diff_rows = if is_write && result_state.requires_write_preview() {
        let path = path
            .as_deref()
            .ok_or_else(|| "successful write operation missing path".to_string())?;
        if path.is_empty() {
            return Err("successful write operation missing path".to_string());
        }
        let diff_preview = diff_preview
            .as_deref()
            .ok_or_else(|| "successful write operation missing diffPreview".to_string())?;
        Some(parse_diff_rows(diff_preview)?)
    } else if !is_write && diff_preview.is_some() {
        return Err("diffPreview on non-write operation".to_string());
    } else {
        None
    };
    Ok(ToolOperation {
        call_id,
        tool_name: Some(tool_name),
        kind,
        title,
        status: optional_object_string(object, "status"),
        path,
        start_line,
        end_line,
        total_lines,
        next_offset,
        truncated_by,
        command_preview: optional_object_string(object, "commandPreview"),
        query: optional_object_string(object, "query"),
        added: object.get("added").and_then(Value::as_u64),
        removed: object.get("removed").and_then(Value::as_u64),
        output_preview: optional_object_string(object, "outputPreview"),
        error: optional_object_string(object, "error"),
        text: optional_object_string(object, "text"),
        diff_rows,
    })
}

fn ensure_event_status(event_type: &str, event: &Value) -> Result<String, String> {
    let status = required_event_string(event, "status")?;
    let valid = match event_type {
        "ToolCall" => status == "running",
        "ToolResult" => matches!(status.as_str(), "done" | "error"),
        _ => false,
    };
    if !valid {
        return Err(format!("unsupported status: {status}"));
    }
    Ok(status)
}

fn required_event_string(event: &Value, key: &str) -> Result<String, String> {
    event
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| format!("missing event.{key}"))
}

fn required_payload_string(payload: &Value, key: &str) -> Result<String, String> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| format!("missing payload.{key}"))
}

fn required_object_string(
    object: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<String, String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| format!("operation missing {key}"))
}

fn optional_object_string(object: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn optional_object_u32(
    object: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<Option<u32>, String> {
    let Some(value) = object.get(key) else {
        return Ok(None);
    };
    let value = value
        .as_u64()
        .ok_or_else(|| format!("operation {key} is not an unsigned integer"))?;
    u32::try_from(value)
        .map(Some)
        .map_err(|_| format!("operation {key} is out of range"))
}

fn tool_action_kind(tool_name: &str) -> ToolActionKind {
    let normalized = tool_name.replace([' ', '_', '-'], "").to_lowercase();
    if normalized.contains("write")
        || normalized.contains("edit")
        || normalized.contains("patch")
        || normalized.contains("apply")
    {
        ToolActionKind::Edit
    } else if normalized.contains("bash")
        || normalized.contains("script")
        || normalized.contains("exec")
        || normalized.contains("shell")
    {
        ToolActionKind::Command
    } else if normalized.contains("read") || normalized.contains("open") {
        ToolActionKind::Read
    } else if normalized.contains("search")
        || normalized.contains("grep")
        || normalized.contains("glob")
        || normalized.contains("rg")
    {
        ToolActionKind::Search
    } else if normalized.contains("browser") || normalized.contains("http") {
        ToolActionKind::Browser
    } else if normalized.contains("plugin") {
        ToolActionKind::Plugin
    } else if normalized.contains("host") {
        ToolActionKind::Host
    } else {
        ToolActionKind::Tool
    }
}

fn running_call_line(call: &ToolCall) -> ToolTranscriptLine {
    let action_kind = tool_action_kind(call.tool_name.as_str());
    ToolTranscriptLine {
        key: format!("tool_call:{}", call.call_id),
        action_kind,
        subject: call
            .display_target
            .clone()
            .or_else(|| call.command.clone())
            .unwrap_or_else(|| call.tool_name.clone()),
        operations: Vec::new(),
        result_blocks: Vec::new(),
        images: Vec::new(),
        result_states: Vec::new(),
        interrupted: false,
        running: true,
        command: call.command.clone(),
        description_title: call.description.is_some(),
    }
}

fn seal_call(call: ToolCall, result: Option<ToolResult>) -> ToolTranscriptLine {
    let action_kind = tool_action_kind(call.tool_name.as_str());
    let operations = result
        .as_ref()
        .map(|result| result.operations.clone())
        .unwrap_or_default();
    let command = call.command.clone().or_else(|| {
        operations.iter().find_map(|operation| {
            operation
                .command_preview
                .as_deref()
                .and_then(parse_command_preview)
        })
    });
    let subject = call
        .display_target
        .clone()
        .or_else(|| operations.iter().find_map(tool_operation_subject))
        .or_else(|| command.clone())
        .unwrap_or_else(|| action_kind.default_subject().to_string());
    let result_blocks = result
        .as_ref()
        .map(|result| collect_tool_result_blocks(std::slice::from_ref(result)))
        .unwrap_or_default();
    let result_states = result
        .as_ref()
        .map(|result| vec![result.result_state])
        .unwrap_or_default();
    let images = result
        .as_ref()
        .map(|result| result.images.clone())
        .unwrap_or_default();
    ToolTranscriptLine {
        key: format!("tool_call:{}", call.call_id),
        action_kind,
        subject,
        operations,
        result_blocks,
        images,
        result_states,
        interrupted: result.is_none(),
        running: false,
        command,
        description_title: call.description.is_some(),
    }
}

fn tool_operation_subject(operation: &ToolOperation) -> Option<String> {
    if let Some(command) = operation
        .command_preview
        .as_deref()
        .and_then(parse_command_preview)
    {
        return Some(command);
    }
    if matches!(operation.kind.as_str(), "read" | "write" | "edit") {
        return operation.path.clone();
    }
    operation
        .query
        .clone()
        .or_else(|| Some(operation.title.clone()))
        .or_else(|| operation.tool_name.clone())
}

fn parse_command_preview(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    if let Ok(Value::Object(object)) = serde_json::from_str::<Value>(raw) {
        for key in ["command", "cmd", "script"] {
            if let Some(command) = object.get(key).and_then(Value::as_str) {
                let command = command.trim();
                if !command.is_empty() {
                    return Some(command.to_string());
                }
            }
        }
    }
    Some(raw.to_string())
}

fn stable_tool_subject(tool: &ToolTranscriptLine) -> String {
    match tool.action_kind {
        ToolActionKind::Edit => {
            let paths = tool_operation_paths(tool.operations.as_slice());
            let target = if paths.len() > 1 {
                format!("{} files", paths.len())
            } else {
                paths
                    .first()
                    .cloned()
                    .unwrap_or_else(|| tool.subject.clone())
            };
            format!(
                "{target}{}",
                tool_edit_count_suffix(tool.operations.as_slice())
            )
        }
        ToolActionKind::Read => tool
            .operations
            .first()
            .and_then(read_operation_subject)
            .unwrap_or_else(|| tool.subject.clone()),
        _ => tool.subject.clone(),
    }
}

fn read_operation_subject(operation: &ToolOperation) -> Option<String> {
    let path = operation.path.as_deref()?;
    match (operation.start_line, operation.end_line) {
        (Some(start_line), Some(end_line)) if start_line == end_line => {
            Some(format!("{path} · line {start_line}"))
        }
        (Some(start_line), Some(end_line)) => {
            Some(format!("{path} · lines {start_line}–{end_line}"))
        }
        _ => Some(path.to_string()),
    }
}

fn partial_read_detail(operation: &ToolOperation) -> Option<String> {
    if operation.kind != "read" {
        return None;
    }
    let read_lines = match (operation.start_line, operation.end_line) {
        (Some(start_line), Some(end_line)) => end_line.checked_sub(start_line)?.saturating_add(1),
        _ => return Some("Partial read".to_string()),
    };
    match operation.total_lines {
        Some(total_lines) => Some(format!(
            "Partial read · {read_lines} of {total_lines} lines"
        )),
        None => Some(format!("Partial read · {read_lines} lines")),
    }
}

pub(crate) fn tool_operation_paths(operations: &[ToolOperation]) -> Vec<String> {
    let mut paths = Vec::new();
    for path in operations
        .iter()
        .filter_map(|operation| operation.path.as_deref())
    {
        if !paths.iter().any(|existing| existing == path) {
            paths.push(path.to_string());
        }
    }
    paths
}

fn tool_edit_count_suffix(operations: &[ToolOperation]) -> String {
    let has_counts = operations
        .iter()
        .any(|operation| operation.added.is_some() || operation.removed.is_some());
    if !has_counts {
        return String::new();
    }
    let added = operations
        .iter()
        .filter_map(|operation| operation.added)
        .sum::<u64>();
    let removed = operations
        .iter()
        .filter_map(|operation| operation.removed)
        .sum::<u64>();
    format!(" (+{added} -{removed})")
}

fn collect_tool_result_blocks(results: &[ToolResult]) -> Vec<ToolResultBlock> {
    let mut blocks = Vec::new();
    let mut lines = Vec::new();
    let mut diff_rows_used = 0usize;
    for result in results {
        if let Some(content) = result.model_content.as_deref() {
            push_result_text(&mut lines, content);
        }
        for hint in &result.hint_lines {
            push_result_text(&mut lines, hint);
        }
        for operation in &result.operations {
            for text in [
                result
                    .model_content
                    .is_none()
                    .then_some(operation.output_preview.as_deref())
                    .flatten(),
                operation.error.as_deref(),
                operation.text.as_deref(),
            ]
            .into_iter()
            .flatten()
            {
                push_result_text(&mut lines, text);
            }
            if let (Some(path), Some(rows)) =
                (operation.path.as_deref(), operation.diff_rows.as_ref())
            {
                flush_text_block(&mut blocks, &mut lines);
                blocks.push(diff_result_block(path, rows.clone(), &mut diff_rows_used));
            }
        }
    }
    flush_text_block(&mut blocks, &mut lines);
    blocks
}

fn push_result_text(lines: &mut Vec<String>, text: &str) {
    for line in text.lines() {
        let trimmed = line.trim();
        if !trimmed.is_empty() && !trimmed.starts_with("[Full tool result:") {
            lines.push(trimmed.to_string());
        }
    }
}

fn flush_text_block(blocks: &mut Vec<ToolResultBlock>, lines: &mut Vec<String>) {
    if lines.is_empty() {
        return;
    }
    blocks.push(ToolResultBlock::Text {
        lines: bounded_text_result_lines(std::mem::take(lines)),
    });
}

fn bounded_text_result_lines(lines: Vec<String>) -> Vec<TextResultLine> {
    if lines.len() <= TOOL_RESULT_MAX_LINES {
        return lines.into_iter().map(TextResultLine::Text).collect();
    }
    let hidden = lines
        .len()
        .saturating_sub(TOOL_RESULT_HEAD_LINES + TOOL_RESULT_TAIL_LINES);
    let mut out = lines
        .iter()
        .take(TOOL_RESULT_HEAD_LINES)
        .cloned()
        .map(TextResultLine::Text)
        .collect::<Vec<_>>();
    out.push(TextResultLine::Hidden(hidden));
    out.extend(
        lines
            .iter()
            .skip(lines.len().saturating_sub(TOOL_RESULT_TAIL_LINES))
            .cloned()
            .map(TextResultLine::Text),
    );
    out
}

fn diff_result_block(
    path: &str,
    rows: Vec<DiffRow>,
    diff_rows_used: &mut usize,
) -> ToolResultBlock {
    let rows = trim_diff_context(rows);
    let base_hidden = hidden_line_total(rows.as_slice());
    let (rows, block_hidden) = cap_diff_rows(rows, DIFF_PREVIEW_MAX_ROWS);
    let remaining = DIFF_GROUP_MAX_ROWS.saturating_sub(*diff_rows_used);
    let (rows, group_hidden) = cap_diff_rows(rows, remaining);
    *diff_rows_used = (*diff_rows_used).saturating_add(rows.len());
    ToolResultBlock::Diff {
        path: path.to_string(),
        rows,
        hidden_lines: base_hidden
            .saturating_add(block_hidden)
            .saturating_add(group_hidden),
    }
}

fn parse_diff_rows(diff_preview: &str) -> Result<Vec<DiffRow>, String> {
    let mut rows = Vec::new();
    let mut old_line = 0usize;
    let mut new_line = 0usize;
    let mut in_hunk = false;
    for raw in diff_preview.lines() {
        if let Some((old_start, new_start)) = parse_unified_hunk_header(raw) {
            if in_hunk && !rows.is_empty() {
                rows.push(hidden_diff_row(0));
            }
            old_line = old_start;
            new_line = new_start;
            in_hunk = true;
            continue;
        }
        if is_replacement_hunk_header(raw) {
            if in_hunk && !rows.is_empty() {
                rows.push(hidden_diff_row(0));
            }
            old_line = 0;
            new_line = 0;
            in_hunk = true;
            continue;
        }
        if !in_hunk || raw.starts_with("\\ No newline") {
            continue;
        }
        if let Some(text) = raw.strip_prefix('+') {
            rows.push(DiffRow {
                line_number: (new_line > 0).then_some(new_line),
                kind: DiffRowKind::Insert,
                text: text.trim_end().to_string(),
            });
            new_line = new_line.saturating_add(1);
        } else if let Some(text) = raw.strip_prefix('-') {
            rows.push(DiffRow {
                line_number: (old_line > 0).then_some(old_line),
                kind: DiffRowKind::Delete,
                text: text.trim_end().to_string(),
            });
            old_line = old_line.saturating_add(1);
        } else if let Some(text) = raw.strip_prefix(' ') {
            rows.push(DiffRow {
                line_number: (new_line > 0).then_some(new_line),
                kind: DiffRowKind::Context,
                text: text.trim_end().to_string(),
            });
            old_line = old_line.saturating_add(1);
            new_line = new_line.saturating_add(1);
        }
    }
    if rows.is_empty() {
        return Err("diffPreview is neither unified nor replacement patch".to_string());
    }
    Ok(rows)
}

fn parse_unified_hunk_header(raw: &str) -> Option<(usize, usize)> {
    let mut parts = raw.split_whitespace();
    if parts.next()? != "@@" {
        return None;
    }
    let old_start = parse_hunk_start(parts.next()?, '-')?;
    let new_start = parse_hunk_start(parts.next()?, '+')?;
    Some((old_start, new_start))
}

fn is_replacement_hunk_header(raw: &str) -> bool {
    let trimmed = raw.trim();
    trimmed.starts_with("@@ replacement ") && trimmed.ends_with(" @@")
}

fn parse_hunk_start(raw: &str, prefix: char) -> Option<usize> {
    let range = raw.strip_prefix(prefix)?;
    range.split(',').next()?.parse().ok()
}

fn trim_diff_context(rows: Vec<DiffRow>) -> Vec<DiffRow> {
    let mut out = Vec::new();
    let mut segment = Vec::new();
    for row in rows {
        if matches!(row.kind, DiffRowKind::Hidden(0)) {
            append_trimmed_diff_segment(&mut out, segment.as_slice());
            segment.clear();
            if !out.is_empty() {
                out.push(row);
            }
        } else {
            segment.push(row);
        }
    }
    append_trimmed_diff_segment(&mut out, segment.as_slice());
    out
}

fn append_trimmed_diff_segment(out: &mut Vec<DiffRow>, segment: &[DiffRow]) {
    if segment.is_empty() {
        return;
    }
    let changed = segment
        .iter()
        .enumerate()
        .filter_map(|(index, row)| {
            matches!(row.kind, DiffRowKind::Insert | DiffRowKind::Delete).then_some(index)
        })
        .collect::<Vec<_>>();
    if changed.is_empty() {
        out.extend(segment.iter().cloned());
        return;
    }
    let mut hidden = 0usize;
    for (index, row) in segment.iter().enumerate() {
        let keep = !matches!(row.kind, DiffRowKind::Context)
            || changed
                .iter()
                .any(|changed_index| changed_index.abs_diff(index) <= DIFF_CONTEXT_LINES);
        if keep {
            if hidden > 0 {
                out.push(hidden_diff_row(hidden));
                hidden = 0;
            }
            out.push(row.clone());
        } else {
            hidden = hidden.saturating_add(1);
        }
    }
    if hidden > 0 {
        out.push(hidden_diff_row(hidden));
    }
}

fn cap_diff_rows(rows: Vec<DiffRow>, max_rows: usize) -> (Vec<DiffRow>, usize) {
    if rows.len() <= max_rows {
        return (rows, 0);
    }
    if max_rows <= 1 {
        return (vec![hidden_diff_row(rows.len())], rows.len());
    }
    let payload_rows = max_rows - 1;
    let head_rows = payload_rows / 2;
    let tail_rows = payload_rows.saturating_sub(head_rows);
    let hidden = rows.len().saturating_sub(head_rows + tail_rows);
    let mut out = rows.iter().take(head_rows).cloned().collect::<Vec<_>>();
    out.push(hidden_diff_row(hidden));
    out.extend(
        rows.iter()
            .skip(rows.len().saturating_sub(tail_rows))
            .cloned(),
    );
    (out, hidden)
}

fn hidden_diff_row(hidden_lines: usize) -> DiffRow {
    DiffRow {
        line_number: None,
        kind: DiffRowKind::Hidden(hidden_lines),
        text: String::new(),
    }
}

fn hidden_line_total(rows: &[DiffRow]) -> usize {
    rows.iter()
        .filter_map(|row| match row.kind {
            DiffRowKind::Hidden(hidden) => Some(hidden),
            _ => None,
        })
        .sum()
}
