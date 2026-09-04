use std::{
    collections::{HashMap, HashSet},
    env,
    error::Error,
    io::{self, Stdout},
    ops::Range,
    path::{Path, PathBuf},
    process::{self, Command},
    sync::mpsc,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use base64::{engine::general_purpose, Engine as _};
use crossterm::{
    cursor::SetCursorStyle,
    event::{
        self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
        MouseEventKind,
    },
    execute,
    terminal::{
        disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen, SetTitle,
    },
};
use image::GenericImageView as _;
use ratatui::{
    backend::CrosstermBackend,
    layout::{Position, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, List, ListItem, ListState, Paragraph, Widget, Wrap},
    Frame, Terminal,
};
use ratatui_image::{
    picker::{Picker, ProtocolType},
    protocol::{ImageSource, StatefulProtocol, StatefulProtocolType},
    FilterType, Resize, ResizeEncodeRender, StatefulImage,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};

mod commands;
mod theme;
mod transcript;
mod view;

use crate::runtime_client::{RuntimeClient, RuntimeEvent, RuntimeResponse};
use crate::tool_projection::{
    empty_tool_result_detail, stable_tool_title, tool_operation_paths, tool_outcome, DiffRow,
    DiffRowKind, TextResultLine, ToolActionKind, ToolImage, ToolOutcome, ToolProjection,
    ToolResultBlock, ToolTranscriptLine,
};
#[cfg(test)]
use crate::tool_projection::{ToolOperation, ToolResultState, DIFF_PREVIEW_MAX_ROWS};
#[cfg(test)]
use commands::SLASH_COMMANDS;
use commands::{
    command_completion_suffix, command_exists, matching_commands, selected_matching_command,
    slash_command_name,
};
use theme::theme;
use transcript::{SubagentTranscriptLine, TranscriptLine};
use view::*;

const EVENT_POLL_INTERVAL: Duration = Duration::from_millis(100);
const CLI_USAGE: &str = "Usage: centa [--workspace <path>]";
const STATUS_REDRAW_INTERVAL: Duration = Duration::from_secs(1);
const RUNTIME_REPLACEMENT_EXIT_TIMEOUT: Duration = Duration::from_secs(2);
const RUNTIME_REPLACEMENT_IDLE_WAIT: Duration = Duration::from_secs(7);
const HOME_RISK_RECENT_WORKSPACE_LIMIT: usize = 4;
const INLINE_IMAGE_ROWS: u16 = 12;
const IMAGE_PREVIEW_ZOOM_FACTOR: f32 = 1.5;
const IMAGE_PREVIEW_MAX_ZOOM_STEPS: u8 = 8;
const IMAGE_PREVIEW_RESIZE_SETTLE: Duration = Duration::from_millis(75);
type TranscriptProjection = (HashMap<String, Vec<TranscriptLine>>, Vec<String>);

#[derive(Debug)]
struct AppConfig {
    workspace_root: PathBuf,
    session_cwd: String,
    warn_home_on_first_prompt: bool,
}

#[derive(Clone, Debug)]
struct TuiSession {
    id: String,
    title: String,
    updated_at: i64,
    last_message: Option<String>,
    cwd: String,
    session_kind: TuiSessionKind,
    activity_state: TuiSessionActivityState,
    is_unread: bool,
    is_pinned: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TuiSessionKind {
    Main,
    Subagent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TuiSessionActivityState {
    Idle,
    Inactive,
}

impl TuiSessionActivityState {
    fn label(self) -> &'static str {
        match self {
            Self::Idle => "Idle",
            Self::Inactive => "Inactive",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ContextUsage {
    used_tokens: Option<u64>,
    max_context_tokens: Option<u32>,
    used_percentage: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DraftImageAttachment {
    start: usize,
    end: usize,
    local_path: PathBuf,
}

struct ImagePreview {
    path: PathBuf,
    request_tx: mpsc::Sender<ImagePreviewRequest>,
    response_rx: mpsc::Receiver<Result<ImagePreviewResult, String>>,
    generation: u64,
    requested_bounds: Rect,
    protocol: Option<StatefulProtocol>,
    render_size: Rect,
    image_area: Rect,
    view: ImagePreviewView,
    drag: Option<ImagePreviewDrag>,
}

#[derive(Clone, Copy)]
struct ImagePreviewView {
    zoom_steps: u8,
    center_x: f32,
    center_y: f32,
}

impl ImagePreviewView {
    const FIT: Self = Self {
        zoom_steps: 0,
        center_x: 0.5,
        center_y: 0.5,
    };
}

#[derive(Clone, Copy)]
struct ImagePreviewRequest {
    generation: u64,
    bounds: Rect,
    view: ImagePreviewView,
}

struct ImagePreviewResult {
    generation: u64,
    protocol: StatefulProtocol,
    render_size: Rect,
}

#[derive(Clone, Copy)]
struct ImagePreviewDrag {
    start: Position,
    image_area: Rect,
    view: ImagePreviewView,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TuiWorkspaceImageResponse {
    root: String,
    path: String,
    name: String,
    content: String,
    byte_len: u64,
    encoding: String,
    content_kind: String,
    mime_type: Option<String>,
    data_url: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ToolGroupHitRegion {
    key: String,
    row: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct TextPoint {
    row: u16,
    column: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TextSelection {
    anchor: TextPoint,
    head: TextPoint,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RenderedTextRow {
    text: String,
    byte_at_column: Vec<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum MouseDrag {
    Input,
    Transcript { tool_group_key: Option<String> },
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TuiMcpCatalog {
    schema: String,
    servers: Vec<TuiMcpServer>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TuiMcpServer {
    plugin_name: String,
    plugin_display_name: String,
    server_id: String,
    plugin_enabled: bool,
    status: TuiMcpServerStatus,
    configurable: bool,
    configured: bool,
    transport: TuiMcpTransport,
    endpoint: Option<String>,
    tool_names: Vec<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
enum TuiMcpServerStatus {
    Ready,
    NeedsConfiguration,
    Disabled,
    Unsupported,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
enum TuiMcpTransport {
    Stdio,
    StreamableHttp,
}

struct TuiMcpPanel {
    servers: Vec<TuiMcpServer>,
    selected: usize,
    configuring: Option<usize>,
    notice: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TuiModelChoice {
    model: String,
    display_name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TuiModelProvider {
    provider_id: String,
    name: String,
    configured: bool,
    models: Vec<TuiModelChoice>,
}

struct TuiModelPanel {
    providers: Vec<TuiModelProvider>,
    selected_provider: usize,
    selected_model: usize,
    active_provider_id: Option<String>,
    active_model: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TuiWorkspaceSnapshot {
    active_workspace_root: Option<String>,
    workspaces: Vec<TuiWorkspaceChoice>,
    cancelled: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TuiWorkspaceChoice {
    root: String,
    name: String,
    active_session_id: Option<String>,
    sort_order: i64,
    updated_at: i64,
}

struct HomeRiskPanel {
    workspaces: Vec<TuiWorkspaceChoice>,
    selected: usize,
    notice: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SessionWorkspace {
    root: String,
    name: String,
}

struct App {
    workspace_root: String,
    session_cwd: String,
    model_provider_id: Option<String>,
    model_display: Option<String>,
    model_effort: Option<String>,
    context_usage: Option<ContextUsage>,
    runtime: Option<RuntimeClient>,
    pending_model_request: Option<PendingModelRequest>,
    runtime_config_refresh_pending: bool,
    model_credential_prompt: Option<ModelCredentialPrompt>,
    model_panel: Option<TuiModelPanel>,
    model_provider_hit_regions: Vec<Rect>,
    model_list_area: Option<Rect>,
    model_list_offset: usize,
    mcp_panel: Option<TuiMcpPanel>,
    home_risk_pending: bool,
    home_risk_panel: Option<HomeRiskPanel>,
    input: String,
    input_cursor: usize,
    input_selection_anchor: Option<usize>,
    input_area: Option<Rect>,
    panel_area: Option<Rect>,
    draft_image_attachments: Vec<DraftImageAttachment>,
    next_image_number: usize,
    image_picker: Picker,
    image_preview: Option<ImagePreview>,
    image_preview_area: Option<Rect>,
    inline_images: HashMap<String, StatefulProtocol>,
    inline_image_errors: HashMap<String, String>,
    pending_esc_stop: bool,
    message: Option<String>,
    show_help: bool,
    show_state: bool,
    selected_command: usize,
    session_catalog: Vec<TuiSession>,
    sessions: Vec<TuiSession>,
    session_workspaces: Vec<SessionWorkspace>,
    selected_session_workspace: usize,
    session_workspace_hit_regions: Vec<Rect>,
    session_list_area: Option<Rect>,
    session_list_offset: usize,
    session_action_area: Option<Rect>,
    selected_session: usize,
    session_picker_open: bool,
    active_session: Option<TuiSession>,
    transcript: Vec<TranscriptLine>,
    transcript_scroll: u16,
    transcript_max_scroll: u16,
    transcript_follow_bottom: bool,
    expanded_tool_groups: HashSet<String>,
    focused_tool_group: Option<String>,
    tool_group_hit_regions: Vec<ToolGroupHitRegion>,
    transcript_area: Option<Rect>,
    transcript_rows: Vec<RenderedTextRow>,
    transcript_selection: Option<TextSelection>,
    mouse_drag: Option<MouseDrag>,
    tool_projection: ToolProjection,
    pending_subagent_lines: Vec<TranscriptLine>,
    assistant_buffer: String,
    assistant_emitted_bytes: usize,
    assistant_tail_in_code_block: bool,
    assistant_stream_started: bool,
    assistant_stream_start: Option<usize>,
    render_width: u16,
    active_tool_label: Option<String>,
    active_agent_run_id: Option<String>,
    active_agent_run_ids: HashSet<String>,
    completed_agent_run_ids: HashSet<String>,
    seen_subagent_ids: HashSet<String>,
    live_subagent_ids: HashSet<String>,
    seen_stream_event_ids: HashSet<String>,
    agent_run_started_at: Option<Instant>,
    process_state: RuntimeDisplayState,
    runtime_easter_egg: Option<&'static str>,
    pending_question: Option<PendingQuestion>,
    pending_delete: Option<String>,
    rename_session_id: Option<String>,
    tool_protocol_error: bool,
}

enum ModelCommand {
    Refresh,
    List,
    Select(String),
    Effort(Option<String>),
}

#[derive(Clone)]
struct ModelCredentialPrompt {
    provider_id: String,
    provider_name: String,
}

enum ModelMutation {
    Selection,
    Credential { provider_name: String },
    Effort { mode: String },
}

enum PendingModelRequest {
    Connecting {
        response_rx: mpsc::Receiver<Result<RuntimeClient, String>>,
        command: ModelCommand,
    },
    LoadingConfig {
        response: RuntimeResponse,
        command: ModelCommand,
    },
    Setting {
        response: RuntimeResponse,
        mutation: ModelMutation,
    },
    Confirming {
        response: RuntimeResponse,
        mutation: ModelMutation,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum RuntimeDisplayState {
    Idle,
    Thinking,
    ToolRunning,
    ProviderWaiting,
    WaitingUser,
    Working,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PendingQuestion {
    id: String,
    question: String,
    options: Vec<String>,
    multi_select: bool,
    required: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ActiveAgentRun {
    agent_run_id: String,
    status: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SessionRestore {
    transcript: Vec<TranscriptLine>,
    active_agent_runs: Vec<ActiveAgentRun>,
    replay_items: Vec<Value>,
}

pub(crate) fn run() -> Result<(), Box<dyn Error>> {
    install_panic_hook();
    let arguments = env::args().skip(1).collect::<Vec<_>>();
    if let Some(output) = local_cli_output(arguments.as_slice()) {
        println!("{output}");
        return Ok(());
    }
    let config = parse_args(arguments).map_err(|message| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{message}\n{CLI_USAGE}"),
        )
    })?;
    let mut terminal = setup_terminal()?;
    let app = App::new(config);
    let result = run_event_loop(&mut terminal, app);
    restore_terminal(&mut terminal)?;
    result
}

fn local_cli_output(arguments: &[String]) -> Option<String> {
    match arguments {
        [argument] if matches!(argument.as_str(), "--help" | "-h") => Some(CLI_USAGE.to_string()),
        [argument] if matches!(argument.as_str(), "--version" | "-V") => {
            Some(format!("centa {}", env!("CARGO_PKG_VERSION")))
        }
        _ => None,
    }
}

/// panic 时恢复终端，避免 alternate screen/raw mode 泄漏到 shell。
fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let _ = restore_terminal_surface();
        eprintln!("{info}");
    }));
}

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<AppConfig, String> {
    let default_workspace = env::current_dir()
        .map_err(|error| format!("cannot read current directory as default workspace: {error}"))?;
    parse_args_with_default_and_home(args, default_workspace, user_home_dir())
}

#[cfg(test)]
fn parse_args_with_default(
    args: impl IntoIterator<Item = String>,
    default_workspace: PathBuf,
) -> Result<AppConfig, String> {
    parse_args_with_default_and_home(args, default_workspace, user_home_dir())
}

fn parse_args_with_default_and_home(
    args: impl IntoIterator<Item = String>,
    default_workspace: PathBuf,
    home_dir: Option<PathBuf>,
) -> Result<AppConfig, String> {
    let mut workspace_root = None;
    let mut args = args.into_iter();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--workspace" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--workspace requires a path".to_string())?;
                workspace_root = Some(PathBuf::from(value));
            }
            _ => return Err(format!("unknown argument: {arg}")),
        }
    }

    let workspace_was_explicit = workspace_root.is_some();
    let workspace_root = workspace_root.unwrap_or(default_workspace);
    let workspace_root = workspace_root
        .canonicalize()
        .map_err(|error| format!("workspace path is not readable: {error}"))?;
    if !workspace_root.is_dir() {
        return Err(format!(
            "workspace path is not a directory: {}",
            workspace_root.display()
        ));
    }

    let warn_home_on_first_prompt = if workspace_was_explicit {
        false
    } else if let Some(home_dir) = home_dir {
        home_dir
            .canonicalize()
            .map_err(|error| format!("cannot read user home directory: {error}"))?
            == workspace_root
    } else {
        false
    };
    Ok(AppConfig {
        session_cwd: display_path(workspace_root.as_path()),
        workspace_root,
        warn_home_on_first_prompt,
    })
}

fn user_home_dir() -> Option<PathBuf> {
    env::var_os("USERPROFILE")
        .or_else(|| env::var_os("HOME"))
        .map(PathBuf::from)
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>, Box<dyn Error>> {
    enable_raw_mode()?;
    if let Err(error) = execute!(
        io::stdout(),
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste,
        SetCursorStyle::SteadyBar,
        SetTitle("Centaeris")
    ) {
        let _ = restore_terminal_surface();
        return Err(error.into());
    }
    let stdout = io::stdout();
    let backend = CrosstermBackend::new(stdout);
    match Terminal::new(backend) {
        Ok(terminal) => Ok(terminal),
        Err(error) => {
            let _ = restore_terminal_surface();
            Err(error.into())
        }
    }
}

fn restore_terminal(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
) -> Result<(), Box<dyn Error>> {
    restore_terminal_surface()?;
    terminal.show_cursor()?;
    Ok(())
}

fn restore_terminal_surface() -> io::Result<()> {
    let raw_mode_result = disable_raw_mode();
    let screen_result = execute!(
        io::stdout(),
        DisableMouseCapture,
        DisableBracketedPaste,
        SetCursorStyle::DefaultUserShape,
        LeaveAlternateScreen
    );
    raw_mode_result?;
    screen_result
}

impl App {
    fn new(config: AppConfig) -> Self {
        Self {
            workspace_root: display_path(config.workspace_root.as_path()),
            session_cwd: config.session_cwd,
            model_provider_id: None,
            model_display: None,
            model_effort: None,
            context_usage: None,
            runtime: None,
            pending_model_request: None,
            runtime_config_refresh_pending: false,
            model_credential_prompt: None,
            model_panel: None,
            model_provider_hit_regions: Vec::new(),
            model_list_area: None,
            model_list_offset: 0,
            mcp_panel: None,
            home_risk_pending: config.warn_home_on_first_prompt,
            home_risk_panel: None,
            input: String::new(),
            input_cursor: 0,
            input_selection_anchor: None,
            input_area: None,
            panel_area: None,
            draft_image_attachments: Vec::new(),
            next_image_number: 1,
            image_picker: halfblock_image_picker(),
            image_preview: None,
            image_preview_area: None,
            inline_images: HashMap::new(),
            inline_image_errors: HashMap::new(),
            pending_esc_stop: false,
            message: None,
            show_help: false,
            show_state: false,
            selected_command: 0,
            session_catalog: Vec::new(),
            sessions: Vec::new(),
            session_workspaces: Vec::new(),
            selected_session_workspace: 0,
            session_workspace_hit_regions: Vec::new(),
            session_list_area: None,
            session_list_offset: 0,
            session_action_area: None,
            selected_session: 0,
            session_picker_open: false,
            active_session: None,
            transcript: Vec::new(),
            transcript_scroll: 0,
            transcript_max_scroll: 0,
            transcript_follow_bottom: true,
            expanded_tool_groups: HashSet::new(),
            focused_tool_group: None,
            tool_group_hit_regions: Vec::new(),
            transcript_area: None,
            transcript_rows: Vec::new(),
            transcript_selection: None,
            mouse_drag: None,
            tool_projection: ToolProjection::default(),
            pending_subagent_lines: Vec::new(),
            assistant_buffer: String::new(),
            assistant_emitted_bytes: 0,
            assistant_tail_in_code_block: false,
            assistant_stream_started: false,
            assistant_stream_start: None,
            render_width: 80,
            active_tool_label: None,
            active_agent_run_id: None,
            active_agent_run_ids: HashSet::new(),
            completed_agent_run_ids: HashSet::new(),
            seen_subagent_ids: HashSet::new(),
            live_subagent_ids: HashSet::new(),
            seen_stream_event_ids: HashSet::new(),
            agent_run_started_at: None,
            process_state: RuntimeDisplayState::Idle,
            runtime_easter_egg: None,
            pending_question: None,
            pending_delete: None,
            rename_session_id: None,
            tool_protocol_error: false,
        }
    }

    fn command_panel_open(&self) -> bool {
        !self.session_picker_open
            && self.image_preview.is_none()
            && self.model_panel.is_none()
            && self.home_risk_panel.is_none()
            && self.mcp_panel.is_none()
            && self.message.is_none()
            && (self.show_help || self.input.starts_with('/'))
    }

    fn secret_input_active(&self) -> bool {
        self.model_credential_prompt.is_some()
            || self
                .mcp_panel
                .as_ref()
                .is_some_and(|panel| panel.configuring.is_some())
    }
}

fn halfblock_image_picker() -> Picker {
    let mut picker = Picker::from_fontsize((10, 20));
    picker.set_protocol_type(ProtocolType::Halfblocks);
    picker
}

fn terminal_image_picker() -> Result<Picker, String> {
    Picker::from_query_stdio()
        .map_err(|error| format!("detect terminal image protocol failed: {error}"))
}

fn run_event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    mut app: App,
) -> Result<(), Box<dyn Error>> {
    let mut redraw = true;
    let mut last_draw_at = Instant::now();
    let mut image_picker_detection_pending = true;
    let mut runtime_bootstrap_pending = true;
    loop {
        terminal.autoresize()?;
        let width = terminal.size()?.width;
        redraw |= width != app.render_width;
        app.render_width = width;
        redraw |= drain_model_request(&mut app);
        redraw |= drain_runtime_events(&mut app);
        redraw |= drain_image_preview(&mut app);
        materialize_assistant_prefix(&mut app);
        let now = Instant::now();
        redraw |= periodic_redraw_due(&app, last_draw_at, now);
        if redraw {
            let transcript_view = build_transcript_view(&app, width);
            terminal.draw(|frame| render(frame, &mut app, &transcript_view))?;
            redraw = false;
            last_draw_at = now;
        }

        if image_picker_detection_pending {
            image_picker_detection_pending = false;
            app.image_picker = terminal_image_picker().map_err(io::Error::other)?;
            continue;
        }

        if runtime_bootstrap_pending {
            runtime_bootstrap_pending = false;
            if let Err(error) = start_model_command(&mut app, ModelCommand::Refresh) {
                app.message = Some(error);
                redraw = true;
            }
            continue;
        }

        if !event::poll(EVENT_POLL_INTERVAL)? {
            continue;
        }

        match event::read()? {
            Event::Key(key) => {
                if key.kind == KeyEventKind::Release {
                    continue;
                }
                if handle_key(key, &mut app) {
                    break;
                }
                redraw = true;
            }
            Event::Paste(text) => {
                handle_paste(&mut app, text);
                redraw = true;
            }
            Event::Mouse(mouse) => {
                handle_mouse(mouse, &mut app);
                redraw = true;
            }
            Event::Resize(_, _) => redraw = true,
            _ => {}
        }
    }

    Ok(())
}

fn clamped_cursor(app: &App) -> usize {
    app.input_cursor.min(app.input.len())
}

fn input_selection_range(app: &App) -> Option<Range<usize>> {
    let anchor = app.input_selection_anchor?.min(app.input.len());
    let cursor = clamped_cursor(app);
    (anchor != cursor).then(|| anchor.min(cursor)..anchor.max(cursor))
}

fn set_input_cursor(app: &mut App, cursor: usize) {
    app.input_cursor = cursor.min(app.input.len());
    app.input_selection_anchor = None;
}

fn delete_input_selection(app: &mut App) -> bool {
    let Some(mut range) = input_selection_range(app) else {
        app.input_selection_anchor = None;
        return false;
    };
    loop {
        let before = range.clone();
        for attachment in &app.draft_image_attachments {
            if attachment.start < range.end && range.start < attachment.end {
                range.start = range.start.min(attachment.start);
                range.end = range.end.max(attachment.end);
            }
        }
        if range == before {
            break;
        }
    }
    let removed_len = range.end - range.start;
    app.input.replace_range(range.clone(), "");
    let mut remaining = Vec::with_capacity(app.draft_image_attachments.len());
    for mut attachment in app.draft_image_attachments.drain(..) {
        if attachment.start < range.end && range.start < attachment.end {
            let _ = std::fs::remove_file(attachment.local_path);
            continue;
        }
        if attachment.start >= range.end {
            attachment.start -= removed_len;
            attachment.end -= removed_len;
        }
        remaining.push(attachment);
    }
    app.draft_image_attachments = remaining;
    app.input_cursor = range.start;
    app.input_selection_anchor = None;
    true
}

fn insert_text_at_cursor(app: &mut App, text: &str) {
    delete_input_selection(app);
    let cursor = normalized_input_cursor(app);
    app.input.insert_str(cursor, text);
    for attachment in &mut app.draft_image_attachments {
        if attachment.start >= cursor {
            attachment.start += text.len();
            attachment.end += text.len();
        }
    }
    app.input_cursor = cursor + text.len();
    app.input_selection_anchor = None;
}

fn normalized_input_cursor(app: &App) -> usize {
    let cursor = clamped_cursor(app);
    app.draft_image_attachments
        .iter()
        .find(|attachment| attachment.start < cursor && cursor < attachment.end)
        .map(|attachment| attachment.end)
        .unwrap_or(cursor)
}

fn remove_draft_image(app: &mut App, index: usize) {
    let attachment = app.draft_image_attachments.remove(index);
    let removed_len = attachment.end - attachment.start;
    app.input
        .replace_range(attachment.start..attachment.end, "");
    for item in &mut app.draft_image_attachments {
        if item.start >= attachment.end {
            item.start -= removed_len;
            item.end -= removed_len;
        }
    }
    app.input_cursor = attachment.start;
    app.input_selection_anchor = None;
    let _ = std::fs::remove_file(attachment.local_path);
}

fn backspace_at_cursor(app: &mut App) {
    if delete_input_selection(app) {
        return;
    }
    let cursor = normalized_input_cursor(app);
    if cursor == 0 {
        return;
    }
    if let Some(index) = app
        .draft_image_attachments
        .iter()
        .position(|attachment| attachment.start < cursor && cursor <= attachment.end)
    {
        remove_draft_image(app, index);
        return;
    }
    let start = app.input[..cursor]
        .char_indices()
        .next_back()
        .map(|(index, _)| index)
        .unwrap_or(0);
    app.input.replace_range(start..cursor, "");
    for attachment in &mut app.draft_image_attachments {
        if attachment.start >= cursor {
            attachment.start -= cursor - start;
            attachment.end -= cursor - start;
        }
    }
    app.input_cursor = start;
    app.input_selection_anchor = None;
}

fn delete_at_cursor(app: &mut App) {
    if delete_input_selection(app) {
        return;
    }
    let cursor = normalized_input_cursor(app);
    if cursor >= app.input.len() {
        return;
    }
    if let Some(index) = app
        .draft_image_attachments
        .iter()
        .position(|attachment| attachment.start <= cursor && cursor < attachment.end)
    {
        remove_draft_image(app, index);
        return;
    }
    let end = cursor
        + app.input[cursor..]
            .chars()
            .next()
            .map(|character| character.len_utf8())
            .unwrap_or(0);
    app.input.replace_range(cursor..end, "");
    for attachment in &mut app.draft_image_attachments {
        if attachment.start >= end {
            attachment.start -= end - cursor;
            attachment.end -= end - cursor;
        }
    }
}

fn move_cursor_left(app: &mut App) {
    if let Some(range) = input_selection_range(app) {
        set_input_cursor(app, range.start);
        return;
    }
    app.input_selection_anchor = None;
    let cursor = normalized_input_cursor(app);
    if cursor == 0 {
        return;
    }
    if let Some(attachment) = app
        .draft_image_attachments
        .iter()
        .find(|attachment| attachment.end == cursor)
    {
        app.input_cursor = attachment.start;
        return;
    }
    app.input_cursor = app.input[..cursor]
        .char_indices()
        .next_back()
        .map(|(index, _)| index)
        .unwrap_or(0);
}

fn move_cursor_right(app: &mut App) {
    if let Some(range) = input_selection_range(app) {
        set_input_cursor(app, range.end);
        return;
    }
    app.input_selection_anchor = None;
    let cursor = normalized_input_cursor(app);
    if cursor >= app.input.len() {
        return;
    }
    if let Some(attachment) = app
        .draft_image_attachments
        .iter()
        .find(|attachment| attachment.start == cursor)
    {
        app.input_cursor = attachment.end;
        return;
    }
    app.input_cursor = cursor
        + app.input[cursor..]
            .chars()
            .next()
            .map(|character| character.len_utf8())
            .unwrap_or(0);
}

fn handle_paste(app: &mut App, text: String) {
    if app.pending_question.is_some()
        || app.session_picker_open
        || app.model_panel.is_some()
        || app.image_preview.is_some()
        || app.home_risk_panel.is_some()
        || app.command_panel_open()
    {
        return;
    }
    app.message = None;
    app.show_help = false;
    app.selected_command = 0;
    match crate::clipboard::image_from_pasted_text(text.as_str()) {
        Ok(Some(path)) => {
            insert_draft_image(app, path);
            return;
        }
        Ok(None) => {}
        Err(error) => {
            show_message(app, error);
            return;
        }
    }
    insert_text_at_cursor(app, text.as_str());
}

fn promote_exact_input_image_path(app: &mut App) {
    match crate::clipboard::image_from_pasted_text(app.input.as_str()) {
        Ok(Some(path)) => {
            app.input.clear();
            app.input_cursor = 0;
            app.input_selection_anchor = None;
            insert_draft_image(app, path);
        }
        Ok(None) => {}
        Err(error) => show_message(app, error),
    }
}

fn clear_composer(app: &mut App) {
    close_image_preview(app);
    app.input = String::new();
    app.input_cursor = 0;
    app.input_selection_anchor = None;
    app.next_image_number = 1;
    for attachment in app.draft_image_attachments.drain(..) {
        let _ = std::fs::remove_file(attachment.local_path);
    }
}

fn handle_clipboard_paste(app: &mut App) {
    if app.pending_question.is_some()
        || app.session_picker_open
        || app.model_panel.is_some()
        || app.image_preview.is_some()
        || app.home_risk_panel.is_some()
        || app.command_panel_open()
    {
        return;
    }
    match crate::clipboard::paste() {
        Ok(crate::clipboard::ClipboardContent::Text(text)) => handle_paste(app, text),
        Ok(crate::clipboard::ClipboardContent::Image(path)) => insert_draft_image(app, path),
        Err(error) => show_message(app, error),
    }
}

fn insert_draft_image(app: &mut App, path: PathBuf) {
    if app.draft_image_attachments.len() >= 8 {
        let _ = std::fs::remove_file(path);
        show_message(app, "A message can contain at most 8 images".to_string());
        return;
    }
    let placeholder = loop {
        let placeholder = format!("[Image #{}]", app.next_image_number);
        app.next_image_number += 1;
        if !app.input.contains(placeholder.as_str()) {
            break placeholder;
        }
    };
    let start = normalized_input_cursor(app);
    insert_text_at_cursor(app, placeholder.as_str());
    app.draft_image_attachments.push(DraftImageAttachment {
        start,
        end: start + placeholder.len(),
        local_path: path,
    });
    app.message = None;
}

fn open_draft_image_preview(app: &mut App, column: u16, row: u16) -> Result<bool, String> {
    let Some(position) = raw_input_byte_from_mouse(app, column, row) else {
        return Ok(false);
    };
    let Some(attachment) = app
        .draft_image_attachments
        .iter()
        .find(|attachment| attachment.start <= position && position < attachment.end)
    else {
        return Ok(false);
    };
    let path = attachment.local_path.clone();
    let worker_path = path.clone();
    let picker = app.image_picker.clone();
    let (request_tx, request_rx) = mpsc::channel();
    let (response_tx, response_rx) = mpsc::channel();
    thread::Builder::new()
        .name("centaeris-tui-image-preview".to_string())
        .spawn(move || image_preview_worker(worker_path, picker, request_rx, response_tx))
        .map_err(|error| format!("start image preview worker failed: {error}"))?;
    app.image_preview = Some(ImagePreview {
        path,
        request_tx,
        response_rx,
        generation: 0,
        requested_bounds: Rect::default(),
        protocol: None,
        render_size: Rect::default(),
        image_area: Rect::default(),
        view: ImagePreviewView::FIT,
        drag: None,
    });
    app.image_preview_area = None;
    app.input_selection_anchor = None;
    app.transcript_selection = None;
    app.mouse_drag = None;
    Ok(true)
}

fn close_image_preview(app: &mut App) {
    app.image_preview = None;
    app.image_preview_area = None;
}

fn image_preview_worker(
    path: PathBuf,
    picker: Picker,
    request_rx: mpsc::Receiver<ImagePreviewRequest>,
    response_tx: mpsc::Sender<Result<ImagePreviewResult, String>>,
) {
    let reader = match image::ImageReader::open(path.as_path()) {
        Ok(reader) => reader,
        Err(error) => {
            let _ = response_tx.send(Err(format!("open image preview failed: {error}")));
            return;
        }
    };
    let source = match reader.decode() {
        Ok(image) => image.to_rgba8(),
        Err(error) => {
            let _ = response_tx.send(Err(format!("decode image preview failed: {error}")));
            return;
        }
    };
    let layout_font_size = picker.font_size();
    let protocol_font_size = if picker.protocol_type() == ProtocolType::Halfblocks {
        (1, 2)
    } else {
        layout_font_size
    };
    let template = picker.new_resize_protocol(image::DynamicImage::new_rgba8(1, 1));
    let background_color = template.background_color();
    let protocol_type = template.protocol_type_owned();
    while let Ok(mut request) = request_rx.recv() {
        loop {
            match request_rx.recv_timeout(IMAGE_PREVIEW_RESIZE_SETTLE) {
                Ok(latest) => request = latest,
                Err(mpsc::RecvTimeoutError::Timeout) => break,
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
        }
        let result = prepare_image_preview(
            &source,
            request,
            layout_font_size,
            protocol_font_size,
            background_color,
            &protocol_type,
        );
        let failed = result.is_err();
        if response_tx.send(result).is_err() || failed {
            return;
        }
    }
}

fn prepare_image_preview(
    source: &image::RgbaImage,
    request: ImagePreviewRequest,
    layout_font_size: (u16, u16),
    protocol_font_size: (u16, u16),
    background_color: image::Rgba<u8>,
    protocol_type: &StatefulProtocolType,
) -> Result<ImagePreviewResult, String> {
    let render_size = fit_image_preview_size(
        source.width(),
        source.height(),
        layout_font_size,
        request.bounds,
    );
    if render_size.width == 0 || render_size.height == 0 {
        return Err("image preview has no renderable area".to_string());
    }
    let crop_ratio = image_preview_crop_ratio(request.view.zoom_steps);
    let crop_width = ((source.width() as f32 * crop_ratio).round() as u32).clamp(1, source.width());
    let crop_height =
        ((source.height() as f32 * crop_ratio).round() as u32).clamp(1, source.height());
    let crop_x = image_preview_crop_offset(source.width(), crop_width, request.view.center_x);
    let crop_y = image_preview_crop_offset(source.height(), crop_height, request.view.center_y);
    let target_width = u32::from(render_size.width) * u32::from(protocol_font_size.0);
    let target_height = u32::from(render_size.height) * u32::from(protocol_font_size.1);
    let source_view = source.view(crop_x, crop_y, crop_width, crop_height);
    let prepared = image::imageops::resize(
        &*source_view,
        target_width,
        target_height,
        FilterType::Lanczos3,
    );
    let image_source = ImageSource::new(
        image::DynamicImage::ImageRgba8(prepared),
        protocol_font_size,
        background_color,
    );
    let mut protocol =
        StatefulProtocol::new(image_source, protocol_font_size, protocol_type.clone());
    protocol.resize_encode(&Resize::Fit(Some(FilterType::Lanczos3)), render_size);
    match protocol.last_encoding_result() {
        Some(Ok(())) => Ok(ImagePreviewResult {
            generation: request.generation,
            protocol,
            render_size,
        }),
        Some(Err(error)) => Err(format!("prepare image preview failed: {error}")),
        None => Err("image preview encoder returned no result".to_string()),
    }
}

fn fit_image_preview_size(
    image_width_px: u32,
    image_height_px: u32,
    font_size_px: (u16, u16),
    bounds: Rect,
) -> Rect {
    if image_width_px == 0 || image_height_px == 0 || bounds.width == 0 || bounds.height == 0 {
        return Rect::default();
    }
    let max_width_px = u32::from(bounds.width) * u32::from(font_size_px.0);
    let max_height_px = u32::from(bounds.height) * u32::from(font_size_px.1);
    let scale = (max_width_px as f64 / image_width_px as f64)
        .min(max_height_px as f64 / image_height_px as f64)
        .min(1.0);
    let width_px = (image_width_px as f64 * scale).round().max(1.0) as u32;
    let height_px = (image_height_px as f64 * scale).round().max(1.0) as u32;
    let mut size = ImageSource::round_pixel_size_to_cells(width_px, height_px, font_size_px);
    size.width = size.width.min(bounds.width);
    size.height = size.height.min(bounds.height);
    size
}

fn image_preview_crop_ratio(zoom_steps: u8) -> f32 {
    IMAGE_PREVIEW_ZOOM_FACTOR.powi(-i32::from(zoom_steps))
}

fn image_preview_crop_offset(source_size: u32, crop_size: u32, center: f32) -> u32 {
    ((center * source_size as f32 - crop_size as f32 / 2.0).round() as i64)
        .clamp(0, i64::from(source_size - crop_size)) as u32
}

fn request_image_preview_render(preview: &mut ImagePreview) {
    if preview.requested_bounds.width == 0 || preview.requested_bounds.height == 0 {
        return;
    }
    let generation = preview.generation.wrapping_add(1);
    if preview
        .request_tx
        .send(ImagePreviewRequest {
            generation,
            bounds: preview.requested_bounds,
            view: preview.view,
        })
        .is_ok()
    {
        preview.generation = generation;
    }
}

fn drain_image_preview(app: &mut App) -> bool {
    let Some(preview) = app.image_preview.as_mut() else {
        return false;
    };
    let mut redraw = false;
    let mut failure = None;
    loop {
        match preview.response_rx.try_recv() {
            Ok(Ok(result)) if result.generation == preview.generation => {
                preview.protocol = Some(result.protocol);
                preview.render_size = result.render_size;
                redraw = true;
            }
            Ok(Ok(_)) => {}
            Ok(Err(error)) => failure = Some(error),
            Err(mpsc::TryRecvError::Disconnected) => {
                failure = Some("image preview worker closed".to_string());
            }
            Err(mpsc::TryRecvError::Empty) => break,
        }
        if failure.is_some() {
            break;
        }
    }
    if let Some(error) = failure {
        close_image_preview(app);
        show_message(app, error);
        true
    } else {
        redraw
    }
}

fn zoom_image_preview(preview: &mut ImagePreview, delta: i8, anchor: Option<Position>) {
    let old_steps = preview.view.zoom_steps;
    let new_steps = if delta > 0 {
        old_steps
            .saturating_add(1)
            .min(IMAGE_PREVIEW_MAX_ZOOM_STEPS)
    } else {
        old_steps.saturating_sub(1)
    };
    if new_steps == old_steps {
        return;
    }
    preview.drag = None;
    if new_steps == 0 {
        preview.view = ImagePreviewView::FIT;
        request_image_preview_render(preview);
        return;
    }
    let (anchor_x, anchor_y) = anchor
        .filter(|position| preview.image_area.contains(*position))
        .map(|position| {
            (
                (f32::from(position.x - preview.image_area.x) + 0.5)
                    / f32::from(preview.image_area.width.max(1)),
                (f32::from(position.y - preview.image_area.y) + 0.5)
                    / f32::from(preview.image_area.height.max(1)),
            )
        })
        .unwrap_or((0.5, 0.5));
    let old_crop_ratio = image_preview_crop_ratio(old_steps);
    let new_crop_ratio = image_preview_crop_ratio(new_steps);
    let source_x = preview.view.center_x + (anchor_x - 0.5) * old_crop_ratio;
    let source_y = preview.view.center_y + (anchor_y - 0.5) * old_crop_ratio;
    preview.view.zoom_steps = new_steps;
    preview.view.center_x =
        clamp_image_preview_center(source_x - (anchor_x - 0.5) * new_crop_ratio, new_crop_ratio);
    preview.view.center_y =
        clamp_image_preview_center(source_y - (anchor_y - 0.5) * new_crop_ratio, new_crop_ratio);
    request_image_preview_render(preview);
}

fn clamp_image_preview_center(center: f32, crop_ratio: f32) -> f32 {
    let half = (crop_ratio / 2.0).min(0.5);
    center.clamp(half, 1.0 - half)
}

fn finish_image_preview_drag(preview: &mut ImagePreview, position: Position) {
    let Some(drag) = preview.drag.take() else {
        return;
    };
    if drag.view.zoom_steps == 0 {
        return;
    }
    let delta_x = i32::from(position.x) - i32::from(drag.start.x);
    let delta_y = i32::from(position.y) - i32::from(drag.start.y);
    if delta_x == 0 && delta_y == 0 {
        return;
    }
    let crop_ratio = image_preview_crop_ratio(drag.view.zoom_steps);
    let center_x =
        drag.view.center_x - delta_x as f32 / f32::from(drag.image_area.width.max(1)) * crop_ratio;
    let center_y =
        drag.view.center_y - delta_y as f32 / f32::from(drag.image_area.height.max(1)) * crop_ratio;
    let center_x = clamp_image_preview_center(center_x, crop_ratio);
    let center_y = clamp_image_preview_center(center_y, crop_ratio);
    if (center_x - preview.view.center_x).abs() < f32::EPSILON
        && (center_y - preview.view.center_y).abs() < f32::EPSILON
    {
        return;
    }
    preview.view.center_x = center_x;
    preview.view.center_y = center_y;
    request_image_preview_render(preview);
}

fn handle_key(key: KeyEvent, app: &mut App) -> bool {
    if app.image_preview.is_some() {
        if matches!(key.code, KeyCode::Esc)
            || matches!(key.code, KeyCode::Char('c'))
                && key.modifiers.contains(KeyModifiers::CONTROL)
        {
            close_image_preview(app);
        } else if let Some(preview) = app.image_preview.as_mut() {
            match key.code {
                KeyCode::Char('+') | KeyCode::Char('=') => zoom_image_preview(preview, 1, None),
                KeyCode::Char('-') => zoom_image_preview(preview, -1, None),
                KeyCode::Char('0') if preview.view.zoom_steps > 0 => {
                    preview.view = ImagePreviewView::FIT;
                    request_image_preview_render(preview);
                }
                _ => {}
            }
        }
        return false;
    }
    if app.pending_delete.is_some() {
        return handle_delete_key(key, app);
    }
    if app.rename_session_id.is_some() {
        return handle_rename_key(key, app);
    }
    if app.session_picker_open {
        return handle_session_picker_key(key, app);
    }
    if app.model_panel.is_some() {
        return handle_model_panel_key(key, app);
    }
    if app.home_risk_panel.is_some() {
        return handle_home_risk_key(key, app);
    }
    if app.pending_question.is_some() {
        return handle_question_key(key, app);
    }
    if app.mcp_panel.is_some() {
        return handle_mcp_key(key, app);
    }
    if !matches!(key.code, KeyCode::Esc) {
        app.pending_esc_stop = false;
    }
    match key.code {
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => handle_ctrl_c(app),
        KeyCode::Char('v') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            handle_clipboard_paste(app);
            false
        }
        KeyCode::Esc if app.model_credential_prompt.is_some() => {
            app.model_credential_prompt = None;
            clear_composer(app);
            app.input_cursor = 0;
            app.message = None;
            false
        }
        KeyCode::Esc if app.show_state => {
            app.show_state = false;
            clear_composer(app);
            app.input_cursor = 0;
            false
        }
        KeyCode::Esc if app.focused_tool_group.is_some() => {
            app.focused_tool_group = None;
            false
        }
        KeyCode::Esc => {
            if has_active_agent_run(app) {
                if app.pending_esc_stop {
                    app.pending_esc_stop = false;
                    if let Err(error) = stop_controlled_tasks(app, "tui_esc_stopped") {
                        show_message(app, error);
                    }
                } else {
                    app.pending_esc_stop = true;
                }
            } else {
                app.pending_esc_stop = false;
            }
            false
        }
        KeyCode::Up if app.command_panel_open() => {
            move_command_selection(app, -1);
            false
        }
        KeyCode::Down if app.command_panel_open() => {
            move_command_selection(app, 1);
            false
        }
        KeyCode::Tab if app.command_panel_open() => {
            complete_selected_command(app);
            false
        }
        KeyCode::Tab => {
            move_visible_tool_group_focus(app, 1);
            false
        }
        KeyCode::BackTab => {
            move_visible_tool_group_focus(app, -1);
            false
        }
        KeyCode::PageUp => {
            scroll_transcript(app, -8);
            false
        }
        KeyCode::PageDown => {
            scroll_transcript(app, 8);
            false
        }
        KeyCode::Home if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.transcript_scroll = 0;
            app.transcript_follow_bottom = false;
            app.focused_tool_group = None;
            false
        }
        KeyCode::End if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.transcript_scroll = app.transcript_max_scroll;
            app.transcript_follow_bottom = true;
            app.focused_tool_group = None;
            false
        }
        KeyCode::Char('o') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            toggle_focused_or_latest_tool_group(app);
            false
        }
        KeyCode::Backspace => {
            app.message = None;
            backspace_at_cursor(app);
            app.show_help = false;
            app.selected_command = 0;
            false
        }
        KeyCode::Left => {
            move_cursor_left(app);
            false
        }
        KeyCode::Right => {
            move_cursor_right(app);
            false
        }
        KeyCode::Home => {
            set_input_cursor(app, 0);
            false
        }
        KeyCode::End => {
            set_input_cursor(app, app.input.len());
            false
        }
        KeyCode::Delete => {
            delete_at_cursor(app);
            false
        }
        KeyCode::Enter if app.model_credential_prompt.is_some() => handle_enter(app),
        KeyCode::Enter
            if key.modifiers.contains(KeyModifiers::SHIFT)
                || key.modifiers.contains(KeyModifiers::ALT) =>
        {
            insert_text_at_cursor(app, "\n");
            false
        }
        KeyCode::Enter if app.focused_tool_group.is_some() && app.input.is_empty() => {
            toggle_focused_or_latest_tool_group(app);
            false
        }
        KeyCode::Enter => handle_enter(app),
        KeyCode::Char('g') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            if app.model_credential_prompt.is_some() {
                show_message(app, "External editor is disabled for API keys".to_string());
            } else if let Err(error) = launch_external_editor(app) {
                show_message(app, error);
            }
            false
        }
        KeyCode::Char(ch) => {
            app.focused_tool_group = None;
            app.message = None;
            insert_text_at_cursor(app, &ch.to_string());
            promote_exact_input_image_path(app);
            app.show_help = false;
            app.selected_command = 0;
            false
        }
        _ => false,
    }
}

fn input_byte_at_position(input: &str, usable: usize, target: TextPoint) -> usize {
    let usable = usable.max(1);
    let mut row = 0usize;
    let mut column = 0usize;
    for (index, character) in input.char_indices() {
        if character == '\n' {
            if row == target.row as usize {
                return index;
            }
            row += 1;
            column = 0;
            continue;
        }
        let width = character_width(character);
        if column + width > usable {
            if row == target.row as usize {
                return index;
            }
            row += 1;
            column = 0;
        }
        if row == target.row as usize {
            if target.column as usize <= column {
                return index;
            }
            if (target.column as usize) < column + width {
                return index + character.len_utf8();
            }
        } else if row > target.row as usize {
            return index;
        }
        column += width;
    }
    input.len()
}

fn input_byte_from_mouse(app: &App, column: u16, row: u16) -> Option<usize> {
    let position = raw_input_byte_from_mouse(app, column, row)?;
    Some(
        app.draft_image_attachments
            .iter()
            .find(|attachment| attachment.start < position && position < attachment.end)
            .map(|attachment| attachment.end)
            .unwrap_or(position),
    )
}

fn raw_input_byte_from_mouse(app: &App, column: u16, row: u16) -> Option<usize> {
    let area = app.input_area?;
    let point = TextPoint {
        row: row
            .saturating_sub(area.y)
            .min(area.height.saturating_sub(1)),
        column: column
            .saturating_sub(area.x)
            .min(area.width.saturating_sub(1)),
    };
    let usable = area.width.max(1) as usize;
    let position = if app.secret_input_active() {
        let masked = input_for_display(app, app.input.as_str());
        let masked_byte = input_byte_at_position(masked.as_str(), usable, point);
        let character_index = masked[..masked_byte].chars().count();
        app.input
            .char_indices()
            .nth(character_index)
            .map(|(index, _)| index)
            .unwrap_or(app.input.len())
    } else {
        input_byte_at_position(app.input.as_str(), usable, point)
    };
    Some(position)
}

fn transcript_point_from_mouse(app: &App, column: u16, row: u16) -> Option<TextPoint> {
    let area = app.transcript_area?;
    Some(TextPoint {
        row: row
            .saturating_sub(area.y)
            .min(area.height.saturating_sub(1)),
        column: column.saturating_sub(area.x).min(area.width),
    })
}

fn selected_transcript_text(app: &App) -> Option<String> {
    let selection = app.transcript_selection?;
    let (start, end) = if selection.anchor <= selection.head {
        (selection.anchor, selection.head)
    } else {
        (selection.head, selection.anchor)
    };
    if start == end {
        return None;
    }
    let mut lines = Vec::new();
    for row in start.row..=end.row {
        let Some(rendered) = app.transcript_rows.get(row as usize) else {
            continue;
        };
        let from = if row == start.row { start.column } else { 0 };
        let to = if row == end.row {
            end.column
        } else {
            rendered.byte_at_column.len().saturating_sub(1) as u16
        };
        let from = rendered
            .byte_at_column
            .get(from as usize)
            .copied()
            .unwrap_or(rendered.text.len());
        let to = rendered
            .byte_at_column
            .get(to as usize)
            .copied()
            .unwrap_or(rendered.text.len());
        lines.push(rendered.text[from.min(to)..to.max(from)].to_string());
    }
    let selected = lines.join("\n");
    (!selected.is_empty()).then_some(selected)
}

fn handle_panel_click(mouse: MouseEvent, app: &mut App) -> bool {
    let Some(outer) = app
        .panel_area
        .filter(|area| area.contains(Position::new(mouse.column, mouse.row)))
    else {
        return false;
    };
    if app.session_picker_open {
        let position = Position::new(mouse.column, mouse.row);
        if let Some(index) = app
            .session_workspace_hit_regions
            .iter()
            .position(|area| area.contains(position))
        {
            app.selected_session_workspace = index;
            app.message = None;
            refresh_visible_sessions(app);
            return true;
        }
        if app.pending_delete.is_some()
            && app
                .session_action_area
                .is_some_and(|area| area.contains(position))
        {
            let area = app.session_action_area.expect("action area was checked");
            let code = if mouse.column < area.x.saturating_add(area.width / 2) {
                KeyCode::Enter
            } else {
                KeyCode::Esc
            };
            handle_delete_key(KeyEvent::new(code, KeyModifiers::NONE), app);
            return true;
        }
        if let Some(area) = app.session_list_area.filter(|area| area.contains(position)) {
            let index = app.session_list_offset + mouse.row.saturating_sub(area.y) as usize;
            if index < app.sessions.len() {
                app.selected_session = index;
                if let Err(error) = activate_selected_session(app) {
                    show_message(app, error);
                }
            }
            return true;
        }
        return true;
    }
    if app.model_panel.is_some() {
        let position = Position::new(mouse.column, mouse.row);
        if let Some(index) = app
            .model_provider_hit_regions
            .iter()
            .position(|area| area.contains(position))
        {
            select_model_provider(app, index);
            return true;
        }
        if let Some(area) = app.model_list_area.filter(|area| area.contains(position)) {
            let index = app.model_list_offset + mouse.row.saturating_sub(area.y) as usize;
            let model_count = selected_model_provider(app)
                .map(|provider| provider.models.len())
                .unwrap_or(0);
            if index < model_count.max(1) {
                app.model_panel
                    .as_mut()
                    .expect("model panel was checked")
                    .selected_model = index.min(model_count.saturating_sub(1));
                if let Err(error) = activate_selected_model(app) {
                    show_message(app, error);
                }
            }
            return true;
        }
        return true;
    }
    if app.message.is_some()
        && app.pending_question.is_none()
        && app.mcp_panel.is_none()
        && !app.session_picker_open
        && !app.command_panel_open()
    {
        app.message = None;
        return true;
    }
    let max_width = if app.command_panel_open() { 76 } else { 94 };
    let area = Rect::new(
        outer.x.saturating_add(2),
        outer.y,
        outer.width.saturating_sub(2).min(max_width),
        outer.height,
    );
    if !area.contains(Position::new(mouse.column, mouse.row)) {
        return false;
    }
    let row = mouse.row.saturating_sub(area.y) as usize;

    if let Some(panel) = app.home_risk_panel.as_ref() {
        let option_count = panel.workspaces.len() + 1;
        if let Some(index) = row.checked_sub(2).filter(|index| *index < option_count) {
            if let Some(panel) = app.home_risk_panel.as_mut() {
                panel.selected = index;
                panel.notice = None;
            }
            if let Err(error) = confirm_home_risk_selection(app) {
                set_home_risk_notice(app, error);
            }
        }
        return true;
    }
    if app.pending_delete.is_some() {
        if row + 1 == area.height as usize {
            let code = if mouse.column < area.x.saturating_add(area.width / 2) {
                KeyCode::Enter
            } else {
                KeyCode::Esc
            };
            handle_delete_key(KeyEvent::new(code, KeyModifiers::NONE), app);
        }
        return true;
    }
    if app.rename_session_id.is_some() {
        if row + 1 == area.height as usize {
            let code = if mouse.column < area.x.saturating_add(area.width / 2) {
                KeyCode::Enter
            } else {
                KeyCode::Esc
            };
            handle_rename_key(KeyEvent::new(code, KeyModifiers::NONE), app);
        }
        return true;
    }
    if let Some(question) = app.pending_question.as_ref() {
        if let Some(index) = row
            .checked_sub(2)
            .filter(|index| *index < question.options.len())
        {
            let answer = question.options[index].clone();
            let submit = !question.multi_select;
            clear_composer(app);
            insert_text_at_cursor(app, answer.as_str());
            if submit {
                handle_question_enter(app);
            }
        }
        return true;
    }
    if let Some(panel) = app.mcp_panel.as_ref() {
        if panel.configuring.is_some() {
            let code = match row {
                0 => Some(KeyCode::Esc),
                3 => Some(KeyCode::Enter),
                _ => None,
            };
            if let Some(code) = code {
                handle_mcp_key(KeyEvent::new(code, KeyModifiers::NONE), app);
            }
        } else if let Some(index) = row
            .checked_sub(1)
            .filter(|index| *index < panel.servers.len())
        {
            if let Some(panel) = app.mcp_panel.as_mut() {
                panel.selected = index;
            }
            handle_mcp_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), app);
        }
        return true;
    }
    if app.command_panel_open() {
        let commands = matching_commands(app.input.as_str());
        if !commands.is_empty() {
            let selected = app.selected_command.min(commands.len() - 1);
            let offset = selected.saturating_sub(area.height as usize - 1);
            let index = offset + row;
            if index < commands.len() {
                app.selected_command = index;
                complete_selected_command(app);
            }
        }
        return true;
    }
    false
}

fn handle_mouse(mouse: MouseEvent, app: &mut App) {
    if app.image_preview.is_some() {
        let position = Position::new(mouse.column, mouse.row);
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let image_area = app
                    .image_preview
                    .as_ref()
                    .map(|preview| preview.image_area)
                    .unwrap_or_default();
                if image_area.contains(position) {
                    if let Some(preview) = app.image_preview.as_mut() {
                        preview.drag = Some(ImagePreviewDrag {
                            start: position,
                            image_area,
                            view: preview.view,
                        });
                    }
                } else if app
                    .image_preview_area
                    .is_some_and(|area| !area.contains(position))
                {
                    close_image_preview(app);
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if let Some(preview) = app.image_preview.as_mut() {
                    finish_image_preview_drag(preview, position);
                }
            }
            MouseEventKind::ScrollUp => {
                if let Some(preview) = app
                    .image_preview
                    .as_mut()
                    .filter(|preview| preview.image_area.contains(position))
                {
                    zoom_image_preview(preview, 1, Some(position));
                }
            }
            MouseEventKind::ScrollDown => {
                if let Some(preview) = app
                    .image_preview
                    .as_mut()
                    .filter(|preview| preview.image_area.contains(position))
                {
                    zoom_image_preview(preview, -1, Some(position));
                }
            }
            _ => {}
        }
        return;
    }
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            if app
                .input_area
                .is_some_and(|area| area.contains(Position::new(mouse.column, mouse.row)))
            {
                match open_draft_image_preview(app, mouse.column, mouse.row) {
                    Ok(true) => return,
                    Ok(false) => {}
                    Err(error) => {
                        show_message(app, error);
                        return;
                    }
                }
                if let Some(cursor) = input_byte_from_mouse(app, mouse.column, mouse.row) {
                    app.transcript_selection = None;
                    app.input_cursor = cursor;
                    app.input_selection_anchor = Some(cursor);
                    app.mouse_drag = Some(MouseDrag::Input);
                }
                return;
            }
            if handle_panel_click(mouse, app) {
                app.mouse_drag = None;
                app.input_selection_anchor = None;
                app.transcript_selection = None;
                return;
            }
            if app
                .transcript_area
                .is_some_and(|area| area.contains(Position::new(mouse.column, mouse.row)))
            {
                app.input_selection_anchor = None;
                let point = transcript_point_from_mouse(app, mouse.column, mouse.row)
                    .expect("transcript area was checked");
                let tool_group_key = app
                    .tool_group_hit_regions
                    .iter()
                    .find(|region| region.row == mouse.row)
                    .map(|region| region.key.clone());
                app.transcript_selection = Some(TextSelection {
                    anchor: point,
                    head: point,
                });
                app.mouse_drag = Some(MouseDrag::Transcript { tool_group_key });
                return;
            }
            app.input_selection_anchor = None;
            app.transcript_selection = None;
            app.mouse_drag = None;
            app.focused_tool_group = None;
        }
        MouseEventKind::Drag(MouseButton::Left) => match app.mouse_drag {
            Some(MouseDrag::Input) => {
                if let Some(cursor) = input_byte_from_mouse(app, mouse.column, mouse.row) {
                    app.input_cursor = cursor;
                }
            }
            Some(MouseDrag::Transcript { .. }) => {
                if let Some(point) = transcript_point_from_mouse(app, mouse.column, mouse.row) {
                    if let Some(selection) = app.transcript_selection.as_mut() {
                        selection.head = point;
                    }
                }
            }
            None => {}
        },
        MouseEventKind::Up(MouseButton::Left) => match app.mouse_drag.take() {
            Some(MouseDrag::Input) => {
                if let Some(cursor) = input_byte_from_mouse(app, mouse.column, mouse.row) {
                    app.input_cursor = cursor;
                }
            }
            Some(MouseDrag::Transcript { tool_group_key }) => {
                if let Some(point) = transcript_point_from_mouse(app, mouse.column, mouse.row) {
                    if let Some(selection) = app.transcript_selection.as_mut() {
                        selection.head = point;
                    }
                }
                if let Some(selected) = selected_transcript_text(app) {
                    if let Err(error) = crate::clipboard::copy_text(selected.as_str()) {
                        show_message(app, error);
                    }
                } else if let Some(key) = tool_group_key {
                    toggle_tool_group(app, key);
                } else {
                    app.transcript_selection = None;
                }
            }
            None => {}
        },
        MouseEventKind::ScrollUp => {
            app.transcript_selection = None;
            scroll_transcript(app, -3);
        }
        MouseEventKind::ScrollDown => {
            app.transcript_selection = None;
            scroll_transcript(app, 3);
        }
        _ => {}
    }
}

fn scroll_transcript(app: &mut App, delta: i32) {
    let next = (i32::from(app.transcript_scroll) + delta)
        .clamp(0, i32::from(app.transcript_max_scroll)) as u16;
    app.transcript_scroll = next;
    app.transcript_follow_bottom = next == app.transcript_max_scroll;
    app.focused_tool_group = None;
}

fn move_visible_tool_group_focus(app: &mut App, delta: isize) {
    let keys = app
        .tool_group_hit_regions
        .iter()
        .map(|region| region.key.as_str())
        .collect::<Vec<_>>();
    if keys.is_empty() {
        app.focused_tool_group = None;
        return;
    }
    let current = app
        .focused_tool_group
        .as_deref()
        .and_then(|focused| keys.iter().position(|key| *key == focused));
    let index = match (current, delta.is_negative()) {
        (Some(index), true) => index.checked_sub(1).unwrap_or(keys.len() - 1),
        (Some(index), false) => (index + 1) % keys.len(),
        (None, true) => keys.len() - 1,
        (None, false) => 0,
    };
    app.focused_tool_group = Some(keys[index].to_string());
}

fn toggle_focused_or_latest_tool_group(app: &mut App) {
    let key = app.focused_tool_group.clone().or_else(|| {
        app.tool_group_hit_regions
            .last()
            .map(|region| region.key.clone())
    });
    if let Some(key) = key {
        toggle_tool_group(app, key);
    }
}

fn toggle_tool_group(app: &mut App, key: String) {
    if !app.expanded_tool_groups.remove(&key) {
        app.expanded_tool_groups.insert(key.clone());
    }
    app.focused_tool_group = Some(key);
}

/// Ctrl+G：调用 `$VISUAL`/`$EDITOR`/默认编辑器编辑当前输入，退出后读回。
fn launch_external_editor(app: &mut App) -> Result<(), String> {
    if !app.draft_image_attachments.is_empty() {
        return Err("External editor is unavailable while the draft contains images".to_string());
    }
    let editor = env::var("VISUAL")
        .or_else(|_| env::var("EDITOR"))
        .unwrap_or_else(|_| default_editor());
    let temp_dir = std::env::temp_dir();
    let draft_path = temp_dir.join(format!("centaeris-tui-draft-{}.txt", std::process::id()));
    std::fs::write(draft_path.as_path(), app.input.as_str())
        .map_err(|error| format!("write editor draft failed: {error}"))?;
    let status = Command::new(editor)
        .arg(draft_path.as_path())
        .status()
        .map_err(|error| format!("launch editor failed: {error}"))?;
    if !status.success() {
        return Err(format!("editor exited with {status}"));
    }
    let edited = std::fs::read_to_string(draft_path.as_path())
        .map_err(|error| format!("read editor draft failed: {error}"))?;
    let _ = std::fs::remove_file(draft_path.as_path());
    app.input = edited.replace("\r\n", "\n");
    set_input_cursor(app, app.input.len());
    app.message = None;
    app.show_help = false;
    app.selected_command = 0;
    Ok(())
}

fn default_editor() -> String {
    if cfg!(windows) {
        "notepad".to_string()
    } else {
        "vi".to_string()
    }
}

fn handle_question_key(key: KeyEvent, app: &mut App) -> bool {
    match key.code {
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            if let Err(error) = stop_controlled_tasks(app, "tui_user_stop") {
                show_message(app, error);
            }
            false
        }
        KeyCode::Esc => {
            if let Err(error) = stop_controlled_tasks(app, "tui_esc_stopped") {
                show_message(app, error);
            }
            false
        }
        KeyCode::Backspace => {
            app.message = None;
            backspace_at_cursor(app);
            false
        }
        KeyCode::Left => {
            move_cursor_left(app);
            false
        }
        KeyCode::Right => {
            move_cursor_right(app);
            false
        }
        KeyCode::Home => {
            set_input_cursor(app, 0);
            false
        }
        KeyCode::End => {
            set_input_cursor(app, app.input.len());
            false
        }
        KeyCode::Delete => {
            delete_at_cursor(app);
            false
        }
        KeyCode::Enter => handle_question_enter(app),
        KeyCode::Char(ch) => {
            app.message = None;
            insert_text_at_cursor(app, &ch.to_string());
            false
        }
        _ => false,
    }
}

fn open_home_risk_panel(app: &mut App) -> Result<(), String> {
    ensure_runtime(app)?;
    let response = app
        .runtime
        .as_mut()
        .ok_or_else(|| "runtime client is not connected".to_string())?
        .request("workspace_get", json!({}))?;
    let workspaces = recent_workspace_choices(response, app.session_cwd.as_str())?;
    app.home_risk_panel = Some(HomeRiskPanel {
        workspaces,
        selected: 0,
        notice: None,
    });
    app.message = None;
    app.show_help = false;
    app.show_state = false;
    app.selected_command = 0;
    Ok(())
}

fn recent_workspace_choices(
    response: Value,
    current_workspace: &str,
) -> Result<Vec<TuiWorkspaceChoice>, String> {
    let mut snapshot = serde_json::from_value::<TuiWorkspaceSnapshot>(response)
        .map_err(|error| format!("invalid workspace catalog: {error}"))?;
    if snapshot.cancelled {
        return Err("workspace_get unexpectedly returned cancelled=true".to_string());
    }
    for workspace in &snapshot.workspaces {
        if workspace.root.trim().is_empty() || workspace.name.trim().is_empty() {
            return Err("workspace_get returned an empty workspace root or name".to_string());
        }
    }
    snapshot
        .workspaces
        .retain(|workspace| workspace.root != current_workspace);
    snapshot.workspaces.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then_with(|| left.sort_order.cmp(&right.sort_order))
            .then_with(|| left.root.cmp(&right.root))
    });
    snapshot
        .workspaces
        .truncate(HOME_RISK_RECENT_WORKSPACE_LIMIT);
    Ok(snapshot.workspaces)
}

fn move_home_risk_selection(app: &mut App, delta: isize) {
    let Some(panel) = app.home_risk_panel.as_mut() else {
        return;
    };
    let len = panel.workspaces.len() + 1;
    panel.selected = match delta.cmp(&0) {
        std::cmp::Ordering::Less => panel.selected.checked_sub(1).unwrap_or(len - 1),
        std::cmp::Ordering::Greater => (panel.selected + 1) % len,
        std::cmp::Ordering::Equal => panel.selected,
    };
    panel.notice = None;
}

fn confirm_home_risk_selection(app: &mut App) -> Result<(), String> {
    let selected_workspace = app.home_risk_panel.as_ref().and_then(|panel| {
        panel
            .workspaces
            .get(panel.selected.min(panel.workspaces.len()))
            .cloned()
    });
    if let Some(workspace) = selected_workspace {
        let response = app
            .runtime
            .as_mut()
            .ok_or_else(|| "runtime client is not connected".to_string())?
            .request(
                "workspace_activate",
                json!({"request": {"root": workspace.root.as_str()}}),
            )?;
        apply_workspace_activation(app, workspace.root.as_str(), response)?;
    }
    app.home_risk_pending = false;
    app.home_risk_panel = None;
    let message = app.input.trim_end().to_string();
    start_prompt(app, message)
}

fn apply_workspace_activation(
    app: &mut App,
    expected_root: &str,
    response: Value,
) -> Result<(), String> {
    let snapshot = serde_json::from_value::<TuiWorkspaceSnapshot>(response)
        .map_err(|error| format!("invalid workspace activation response: {error}"))?;
    if snapshot.cancelled {
        return Err("workspace_activate unexpectedly returned cancelled=true".to_string());
    }
    let active_root = snapshot
        .active_workspace_root
        .filter(|root| root == expected_root)
        .ok_or_else(|| "workspace_activate did not activate the selected workspace".to_string())?;
    app.workspace_root = active_root.clone();
    app.session_cwd = active_root;
    Ok(())
}

fn set_home_risk_notice(app: &mut App, error: String) {
    if let Some(panel) = app.home_risk_panel.as_mut() {
        panel.notice = Some(error);
    } else {
        show_message(app, error);
    }
}

fn handle_home_risk_key(key: KeyEvent, app: &mut App) -> bool {
    match key.code {
        KeyCode::Up => move_home_risk_selection(app, -1),
        KeyCode::Down => move_home_risk_selection(app, 1),
        KeyCode::Enter => {
            if let Err(error) = confirm_home_risk_selection(app) {
                set_home_risk_notice(app, error);
            }
        }
        KeyCode::Esc => {
            app.home_risk_panel = None;
        }
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.home_risk_panel = None;
        }
        _ => {}
    }
    false
}

fn should_open_home_risk_panel(app: &App, input: &str) -> bool {
    app.home_risk_pending && app.active_session.is_none() && !input.trim().is_empty()
}

fn handle_enter(app: &mut App) -> bool {
    if app.model_credential_prompt.is_some() {
        if let Err(error) = submit_model_credential(app) {
            show_message(app, error);
        }
        return false;
    }
    let input_owned = app.input.trim_end().to_string();
    let input = input_owned.as_str();
    if input.is_empty() {
        return false;
    }

    match slash_command_name(input) {
        None => {
            let result = if should_open_home_risk_panel(app, input) {
                open_home_risk_panel(app)
            } else {
                start_prompt(app, input.to_string())
            };
            if let Err(error) = result {
                show_message(app, error);
            }
            false
        }
        Some("/") | Some("/help") => {
            app.show_help = true;
            app.message = None;
            app.selected_command = 0;
            false
        }
        Some("/exit") => prepare_exit(app),
        Some("/new") => {
            reset_to_welcome(app);
            false
        }
        Some("/resume") => {
            if let Err(error) = resume_session(app, slash_command_args(input)) {
                show_message(app, error);
            }
            false
        }
        Some("/model") => {
            if let Err(error) = handle_model_command(app, slash_command_args(input)) {
                show_message(app, error);
            }
            false
        }
        Some("/effort") => {
            if let Err(error) = handle_effort_command(app, slash_command_args(input)) {
                show_message(app, error);
            }
            false
        }
        Some("/state") => {
            clear_composer(app);
            app.input_cursor = 0;
            app.message = None;
            app.show_help = false;
            app.show_state = true;
            if let Err(error) = refresh_context_usage(app) {
                show_message(app, error);
            }
            false
        }
        Some("/stop") => {
            clear_composer(app);
            if let Err(error) = stop_controlled_tasks(app, "tui_user_stop") {
                show_message(app, error);
            }
            false
        }
        Some("/plugins") => {
            if let Err(error) = handle_plugins_command(app, slash_command_args(input)) {
                show_message(app, error);
            }
            false
        }
        Some("/mcp") => {
            if let Err(error) = open_mcp_panel(app, slash_command_args(input)) {
                show_message(app, error);
            }
            false
        }
        Some("/clear") => {
            clear_composer(app);
            app.message = None;
            app.show_help = false;
            app.show_state = false;
            app.selected_command = 0;
            false
        }
        Some(name) if command_exists(name) => {
            app.message = Some(format!(
                "Command not connected yet in this welcome scaffold: {name}"
            ));
            false
        }
        Some(name) => {
            app.message = Some(format!("Unknown slash command: {name}"));
            false
        }
    }
}

fn handle_question_enter(app: &mut App) -> bool {
    let input_owned = app.input.trim_end().to_string();
    let input = input_owned.as_str();
    if input == "/exit" {
        return prepare_exit(app);
    }
    if input == "/stop" {
        clear_composer(app);
        if let Err(error) = stop_controlled_tasks(app, "tui_user_stop") {
            show_message(app, error);
        }
        return false;
    }
    if let Err(error) = submit_question_answer(app, input.to_string()) {
        show_message(app, error);
    }
    false
}

fn handle_ctrl_c(app: &mut App) -> bool {
    if app.model_credential_prompt.is_some() {
        app.model_credential_prompt = None;
        clear_composer(app);
        app.input_cursor = 0;
        app.message = None;
        return false;
    }
    if app.session_picker_open {
        close_session_picker(app);
        return false;
    }
    if app.model_panel.is_some() {
        close_model_panel(app);
        return false;
    }
    if app.show_state {
        app.show_state = false;
        clear_composer(app);
        app.input_cursor = 0;
        return false;
    }
    if app.show_help || app.message.is_some() {
        app.show_help = false;
        app.message = None;
        return false;
    }
    if !app.input.is_empty() {
        clear_composer(app);
        app.selected_command = 0;
        return false;
    }
    if has_active_agent_run(app) {
        if let Err(error) = stop_controlled_tasks(app, "tui_user_stop") {
            show_message(app, error);
        }
        return false;
    }
    true
}

fn prepare_exit(app: &mut App) -> bool {
    let Some(runtime) = app.runtime.as_mut() else {
        return true;
    };
    if !runtime.is_connected() {
        return true;
    }
    if let Err(error) = runtime.request("app_exit", json!({})) {
        if !runtime.is_connected() {
            return true;
        }
        show_message(app, error);
        return false;
    }
    true
}

fn has_active_agent_run(app: &App) -> bool {
    app.active_agent_run_id.is_some() || !app.active_agent_run_ids.is_empty()
}

fn periodic_redraw_due(app: &App, last_draw_at: Instant, now: Instant) -> bool {
    has_active_agent_run(app)
        && now.saturating_duration_since(last_draw_at) >= STATUS_REDRAW_INTERVAL
}

fn stop_active_agent_runs(app: &mut App, reason: &str) -> Result<(), String> {
    let task_ids = active_agent_run_ids(app);
    if task_ids.is_empty() {
        return Err("No running task to stop".to_string());
    }
    let session_id = app
        .active_session
        .as_ref()
        .map(|session| session.id.clone())
        .ok_or_else(|| "No active session to stop".to_string())?;
    ensure_runtime(app)?;
    for task_id in task_ids {
        cancel_agent_run(app, session_id.as_str(), task_id.as_str(), reason)?;
    }
    finish_active_agent_run(app);
    app.pending_question = None;
    app.process_state = RuntimeDisplayState::Idle;
    Ok(())
}

fn stop_controlled_tasks(app: &mut App, reason: &str) -> Result<(), String> {
    if has_active_agent_run(app) {
        stop_active_agent_runs(app, reason)?;
    }
    Ok(())
}

fn active_agent_run_ids(app: &App) -> Vec<String> {
    let mut agent_run_ids = app.active_agent_run_ids.iter().cloned().collect::<Vec<_>>();
    if let Some(agent_run_id) = app.active_agent_run_id.as_ref() {
        if !agent_run_ids.iter().any(|item| item == agent_run_id) {
            agent_run_ids.push(agent_run_id.clone());
        }
    }
    agent_run_ids.sort();
    agent_run_ids
}

fn cancel_agent_run(
    app: &mut App,
    session_id: &str,
    agent_run_id: &str,
    reason: &str,
) -> Result<(), String> {
    let response = app
        .runtime
        .as_mut()
        .ok_or_else(|| "runtime client is not connected".to_string())?
        .request(
            "_centaeris/session/agent-runs/cancel",
            json!({
                "request": {
                    "agentRunId": agent_run_id,
                    "sessionId": session_id,
                    "reason": reason,
                }
            }),
        )?;
    let Some(agent_run) = response.get("agentRun").filter(|value| !value.is_null()) else {
        return Err(format!(
            "_centaeris/session/agent-runs/cancel did not find AgentRun: {agent_run_id}"
        ));
    };
    let status = agent_run
        .get("status")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            "_centaeris/session/agent-runs/cancel response agentRun missing status".to_string()
        })?;
    if response.get("cancelled").and_then(Value::as_bool) == Some(false)
        && !is_terminal_status(status)
    {
        return Err(format!(
            "_centaeris/session/agent-runs/cancel did not cancel active AgentRun: {agent_run_id}"
        ));
    }
    if is_terminal_status(status) {
        mark_agent_run_terminal(app, agent_run_id);
    }
    Ok(())
}

fn show_message(app: &mut App, message: impl Into<String>) {
    app.message = Some(message.into());
    app.show_help = false;
    app.home_risk_panel = None;
    app.mcp_panel = None;
    app.selected_command = 0;
}

fn open_mcp_panel(app: &mut App, args: &str) -> Result<(), String> {
    if !args.trim().is_empty() {
        return Err(format!("unsupported /mcp command: {args}"));
    }
    ensure_runtime(app)?;
    let response = app
        .runtime
        .as_mut()
        .ok_or_else(|| "runtime client is not connected".to_string())?
        .request("mcp/catalog", json!({"request": {}}))?;
    let catalog = parse_mcp_catalog(response)?;
    clear_composer(app);
    app.message = None;
    app.show_help = false;
    app.mcp_panel = Some(TuiMcpPanel {
        servers: catalog.servers,
        selected: 0,
        configuring: None,
        notice: None,
    });
    Ok(())
}

fn parse_mcp_catalog(response: Value) -> Result<TuiMcpCatalog, String> {
    let catalog = serde_json::from_value::<TuiMcpCatalog>(response)
        .map_err(|error| format!("invalid Native MCP catalog: {error}"))?;
    if catalog.schema != "native.mcp.catalog.v1" {
        return Err(format!(
            "unsupported Native MCP catalog schema: {}",
            catalog.schema
        ));
    }
    Ok(catalog)
}

fn handle_mcp_key(key: KeyEvent, app: &mut App) -> bool {
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        app.mcp_panel = None;
        clear_composer(app);
        return false;
    }
    let configuring = app
        .mcp_panel
        .as_ref()
        .is_some_and(|panel| panel.configuring.is_some());
    if configuring {
        match key.code {
            KeyCode::Esc => {
                if let Some(panel) = app.mcp_panel.as_mut() {
                    panel.configuring = None;
                    panel.notice = None;
                }
                clear_composer(app);
            }
            KeyCode::Enter => {
                if let Err(error) = submit_mcp_credential(app) {
                    if let Some(panel) = app.mcp_panel.as_mut() {
                        panel.notice = Some(error);
                    }
                }
            }
            KeyCode::Backspace => backspace_at_cursor(app),
            KeyCode::Delete => delete_at_cursor(app),
            KeyCode::Left => move_cursor_left(app),
            KeyCode::Right => move_cursor_right(app),
            KeyCode::Home => set_input_cursor(app, 0),
            KeyCode::End => set_input_cursor(app, app.input.len()),
            KeyCode::Char(ch) => insert_text_at_cursor(app, ch.to_string().as_str()),
            _ => {}
        }
        return false;
    }

    match key.code {
        KeyCode::Esc => {
            app.mcp_panel = None;
            clear_composer(app);
        }
        KeyCode::Up | KeyCode::Down => {
            let delta = if key.code == KeyCode::Up { -1 } else { 1 };
            if let Some(panel) = app.mcp_panel.as_mut() {
                if !panel.servers.is_empty() {
                    panel.selected = (panel.selected as isize + delta)
                        .rem_euclid(panel.servers.len() as isize)
                        as usize;
                    panel.notice = None;
                }
            }
        }
        KeyCode::Enter => {
            if let Some(panel) = app.mcp_panel.as_mut() {
                let selected = panel.selected.min(panel.servers.len().saturating_sub(1));
                if let Some(server) = panel.servers.get(selected) {
                    if server.configurable
                        && !matches!(
                            server.status,
                            TuiMcpServerStatus::Disabled | TuiMcpServerStatus::Unsupported
                        )
                    {
                        panel.configuring = Some(selected);
                        panel.notice = None;
                        clear_composer(app);
                    } else {
                        panel.notice = Some(
                            "This server is package-managed; use /plugins for enablement."
                                .to_string(),
                        );
                    }
                }
            }
        }
        KeyCode::Char(' ') => {
            if let Some(panel) = app.mcp_panel.as_mut() {
                panel.notice = Some("MCP enablement is atomic at /plugins.".to_string());
            }
        }
        _ => {}
    }
    false
}

fn submit_mcp_credential(app: &mut App) -> Result<(), String> {
    let token = app.input.as_str();
    if token.is_empty()
        || token.len() > 4_096
        || !token.is_ascii()
        || !token.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
    {
        return Err("MCP API key must contain 1-4096 visible ASCII characters".to_string());
    }
    let (plugin_name, server_id) = app
        .mcp_panel
        .as_ref()
        .and_then(|panel| panel.configuring.and_then(|index| panel.servers.get(index)))
        .map(|server| (server.plugin_name.clone(), server.server_id.clone()))
        .ok_or_else(|| "MCP credential prompt is not active".to_string())?;
    let token = token.to_string();
    ensure_runtime(app)?;
    // ponytail: credential tests block input up to the declared startup timeout; add an
    // async response state only if this becomes an observed interaction problem.
    let response = app
        .runtime
        .as_mut()
        .ok_or_else(|| "runtime client is not connected".to_string())?
        .request(
            "mcp/configure",
            json!({
                "request": {
                    "pluginName": plugin_name,
                    "serverId": server_id,
                    "bearerToken": token,
                }
            }),
        )?;
    let catalog = parse_mcp_catalog(response)?;
    clear_composer(app);
    app.mcp_panel = Some(TuiMcpPanel {
        servers: catalog.servers,
        selected: 0,
        configuring: None,
        notice: Some("Saved; applies to the next run.".to_string()),
    });
    Ok(())
}

fn handle_plugins_command(app: &mut App, args: &str) -> Result<(), String> {
    let parts = args.split_whitespace().collect::<Vec<_>>();
    match parts.as_slice() {
        [] => {}
        ["reload"] => {
            ensure_runtime(app)?;
            let response = app
                .runtime
                .as_mut()
                .ok_or_else(|| "runtime client is not connected".to_string())?
                .request("plugin/reload", json!({"request": {}}))?;
            show_message(app, format_plugin_snapshot(&response)?);
            clear_composer(app);
            return Ok(());
        }
        ["enable", id] => return set_plugin_enabled_from_tui(app, id, true),
        ["disable", id] => return set_plugin_enabled_from_tui(app, id, false),
        _ => return Err(format!("unsupported /plugins command: {args}")),
    }
    ensure_runtime(app)?;
    let response = app
        .runtime
        .as_mut()
        .ok_or_else(|| "runtime client is not connected".to_string())?
        .request("plugin/list", json!({"request": {}}))?;
    show_message(app, format_plugin_list(&response)?);
    clear_composer(app);
    Ok(())
}

fn handle_model_command(app: &mut App, args: &str) -> Result<(), String> {
    let args = args.trim();
    let command = if args.is_empty() {
        ModelCommand::List
    } else {
        ModelCommand::Select(args.to_string())
    };
    start_model_command(app, command)?;
    clear_composer(app);
    Ok(())
}

fn handle_effort_command(app: &mut App, args: &str) -> Result<(), String> {
    let mode = match args.trim() {
        "" => None,
        mode => Some(mode.to_string()),
    };
    start_model_command(app, ModelCommand::Effort(mode))?;
    clear_composer(app);
    Ok(())
}

fn resolve_model_request(response: &Value, query: &str) -> Result<(String, String), String> {
    let items = response
        .get("selectableModels")
        .and_then(Value::as_array)
        .ok_or_else(|| "agent_runtime_config_get response missing selectableModels".to_string())?;
    let mut matches = items
        .iter()
        .filter(|item| {
            let model = item
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let provider_id = item
                .get("providerId")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let display_name = item
                .get("displayName")
                .and_then(Value::as_str)
                .unwrap_or_default();
            model == query
                || display_name == query
                || provider_id == query
                || format!("{provider_id}/{model}") == query
        })
        .collect::<Vec<_>>();
    if matches.is_empty() {
        matches = items
            .iter()
            .filter(|item| {
                let model = item
                    .get("model")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                model.to_lowercase().contains(&query.to_lowercase())
            })
            .collect::<Vec<_>>();
    }
    match matches.len() {
        0 => Err(format!("no selectable model matches: {query}")),
        1 => {
            let item = matches[0];
            let provider_id = item
                .get("providerId")
                .and_then(Value::as_str)
                .ok_or_else(|| "selectable model missing providerId".to_string())?
                .to_string();
            let model = item
                .get("model")
                .and_then(Value::as_str)
                .ok_or_else(|| "selectable model missing model".to_string())?
                .to_string();
            Ok((provider_id, model))
        }
        _ => {
            let names = matches
                .iter()
                .map(|item| {
                    let provider_id = item
                        .get("providerId")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let model = item
                        .get("model")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    format!("{provider_id}/{model}")
                })
                .collect::<Vec<_>>()
                .join(", ");
            Err(format!(
                "model query is ambiguous: {query} (matches: {names})"
            ))
        }
    }
}

fn resolve_model_provider_request(
    response: &Value,
    query: &str,
) -> Result<Option<(ModelCredentialPrompt, bool)>, String> {
    let providers = response
        .get("modelProviders")
        .and_then(Value::as_array)
        .ok_or_else(|| "agent_runtime_config_get response missing modelProviders".to_string())?;
    let matches = providers
        .iter()
        .filter(|provider| {
            provider.get("providerId").and_then(Value::as_str) == Some(query)
                || provider.get("name").and_then(Value::as_str) == Some(query)
        })
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [] => Ok(None),
        [provider] => {
            let provider_id = provider
                .get("providerId")
                .and_then(Value::as_str)
                .ok_or_else(|| "model provider missing providerId".to_string())?;
            let provider_name = provider
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| "model provider missing name".to_string())?;
            let configured = provider
                .get("configured")
                .and_then(Value::as_bool)
                .ok_or_else(|| "model provider missing configured".to_string())?;
            Ok(Some((
                ModelCredentialPrompt {
                    provider_id: provider_id.to_string(),
                    provider_name: provider_name.to_string(),
                },
                configured,
            )))
        }
        _ => Err(format!("model provider query is ambiguous: {query}")),
    }
}

fn model_panel_from_config(response: &Value) -> Result<TuiModelPanel, String> {
    let active_provider_id = response
        .get("modelProviderId")
        .and_then(Value::as_str)
        .map(str::to_string);
    let active_model = response
        .get("model")
        .and_then(Value::as_str)
        .map(str::to_string);
    let providers = response
        .get("modelProviders")
        .and_then(Value::as_array)
        .ok_or_else(|| "agent_runtime_config_get response missing modelProviders".to_string())?;
    let mut parsed = Vec::with_capacity(providers.len());
    for provider in providers {
        let provider_id = provider
            .get("providerId")
            .and_then(Value::as_str)
            .ok_or_else(|| "model provider missing providerId".to_string())?;
        let name = provider
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| "model provider missing name".to_string())?;
        let configured = provider
            .get("configured")
            .and_then(Value::as_bool)
            .ok_or_else(|| "model provider missing configured".to_string())?;
        let models = provider
            .get("models")
            .and_then(Value::as_array)
            .ok_or_else(|| "model provider missing models".to_string())?;
        let models = if configured {
            models
                .iter()
                .map(|model| {
                    let model_id = model
                        .get("model")
                        .and_then(Value::as_str)
                        .ok_or_else(|| "available model missing model".to_string())?;
                    Ok(TuiModelChoice {
                        model: model_id.to_string(),
                        display_name: model
                            .get("displayName")
                            .and_then(Value::as_str)
                            .unwrap_or(model_id)
                            .to_string(),
                    })
                })
                .collect::<Result<Vec<_>, String>>()?
        } else {
            Vec::new()
        };
        parsed.push(TuiModelProvider {
            provider_id: provider_id.to_string(),
            name: name.to_string(),
            configured,
            models,
        });
    }
    let selected_provider = active_provider_id
        .as_deref()
        .and_then(|active| {
            parsed
                .iter()
                .position(|provider| provider.provider_id == active)
        })
        .or_else(|| parsed.iter().position(|provider| provider.configured))
        .unwrap_or(0);
    let selected_model = parsed
        .get(selected_provider)
        .filter(|provider| Some(provider.provider_id.as_str()) == active_provider_id.as_deref())
        .and_then(|provider| {
            active_model.as_deref().and_then(|active| {
                provider
                    .models
                    .iter()
                    .position(|model| model.model == active)
            })
        })
        .unwrap_or(0);
    Ok(TuiModelPanel {
        providers: parsed,
        selected_provider,
        selected_model,
        active_provider_id,
        active_model,
    })
}

fn open_model_panel(app: &mut App, response: &Value) -> Result<(), String> {
    app.model_panel = Some(model_panel_from_config(response)?);
    app.model_provider_hit_regions.clear();
    app.model_list_area = None;
    app.model_list_offset = 0;
    app.message = None;
    app.show_help = false;
    app.show_state = false;
    app.selected_command = 0;
    Ok(())
}

fn close_model_panel(app: &mut App) {
    app.model_panel = None;
    app.model_provider_hit_regions.clear();
    app.model_list_area = None;
    app.model_list_offset = 0;
    app.message = None;
    clear_composer(app);
}

fn selected_model_provider(app: &App) -> Option<&TuiModelProvider> {
    let panel = app.model_panel.as_ref()?;
    panel.providers.get(
        panel
            .selected_provider
            .min(panel.providers.len().saturating_sub(1)),
    )
}

fn select_model_provider(app: &mut App, index: usize) {
    let Some(panel) = app.model_panel.as_mut() else {
        return;
    };
    if index >= panel.providers.len() {
        return;
    }
    panel.selected_provider = index;
    panel.selected_model = panel
        .providers
        .get(index)
        .filter(|provider| {
            Some(provider.provider_id.as_str()) == panel.active_provider_id.as_deref()
        })
        .and_then(|provider| {
            panel.active_model.as_deref().and_then(|active| {
                provider
                    .models
                    .iter()
                    .position(|model| model.model == active)
            })
        })
        .unwrap_or(0);
    app.message = None;
}

fn move_model_provider(app: &mut App, delta: isize) {
    let Some(panel) = app.model_panel.as_ref() else {
        return;
    };
    if panel.providers.is_empty() {
        return;
    }
    let len = panel.providers.len();
    let current = panel.selected_provider.min(len - 1);
    let next = match delta.cmp(&0) {
        std::cmp::Ordering::Less => current.checked_sub(1).unwrap_or(len - 1),
        std::cmp::Ordering::Greater => (current + 1) % len,
        std::cmp::Ordering::Equal => current,
    };
    select_model_provider(app, next);
}

fn move_model_selection(app: &mut App, delta: isize) {
    let Some(panel) = app.model_panel.as_mut() else {
        return;
    };
    let Some(provider) = panel.providers.get(panel.selected_provider) else {
        return;
    };
    if provider.models.is_empty() {
        panel.selected_model = 0;
        return;
    }
    let len = provider.models.len();
    let current = panel.selected_model.min(len - 1);
    panel.selected_model = match delta.cmp(&0) {
        std::cmp::Ordering::Less => current.checked_sub(1).unwrap_or(len - 1),
        std::cmp::Ordering::Greater => (current + 1) % len,
        std::cmp::Ordering::Equal => current,
    };
    app.message = None;
}

fn begin_model_credential(app: &mut App, provider_id: String, provider_name: String) {
    clear_composer(app);
    app.model_panel = None;
    app.model_provider_hit_regions.clear();
    app.model_list_area = None;
    app.model_credential_prompt = Some(ModelCredentialPrompt {
        provider_id,
        provider_name: provider_name.clone(),
    });
    app.message = Some(format!(
        "Enter the API key for {provider_name} · Esc to cancel"
    ));
}

fn request_model_selection(
    app: &mut App,
    provider_id: String,
    model: String,
) -> Result<(), String> {
    let response = app
        .runtime
        .as_mut()
        .ok_or_else(|| "runtime client is not connected".to_string())?
        .request_async(
            "agent_runtime_config_set",
            json!({
                "request": {
                    "modelProviderId": provider_id,
                    "model": model,
                }
            }),
        )?;
    app.pending_model_request = Some(PendingModelRequest::Setting {
        response,
        mutation: ModelMutation::Selection,
    });
    app.message = Some("Switching model…".to_string());
    Ok(())
}

fn activate_selected_model(app: &mut App) -> Result<(), String> {
    let provider = selected_model_provider(app)
        .cloned()
        .ok_or_else(|| "No model providers are available".to_string())?;
    if !provider.configured {
        begin_model_credential(app, provider.provider_id, provider.name);
        return Ok(());
    }
    let selected = app
        .model_panel
        .as_ref()
        .map(|panel| panel.selected_model)
        .unwrap_or(0);
    let model = provider
        .models
        .get(selected.min(provider.models.len().saturating_sub(1)))
        .ok_or_else(|| format!("{} has no selectable models", provider.name))?;
    request_model_selection(app, provider.provider_id, model.model.clone())
}

fn handle_model_panel_key(key: KeyEvent, app: &mut App) -> bool {
    match key.code {
        KeyCode::Esc => close_model_panel(app),
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            close_model_panel(app)
        }
        KeyCode::Tab => move_model_provider(app, 1),
        KeyCode::BackTab => move_model_provider(app, -1),
        KeyCode::Up => move_model_selection(app, -1),
        KeyCode::Down => move_model_selection(app, 1),
        KeyCode::Enter => {
            if let Err(error) = activate_selected_model(app) {
                show_message(app, error);
            }
        }
        _ => {}
    }
    false
}

fn submit_model_credential(app: &mut App) -> Result<(), String> {
    let prompt = app
        .model_credential_prompt
        .clone()
        .ok_or_else(|| "model credential prompt is not active".to_string())?;
    let api_key = model_api_key_input(app.input.as_str())?;
    if app.pending_model_request.is_some() {
        return Err("Model configuration request is already in progress".to_string());
    }
    ensure_runtime(app)?;
    let response = app
        .runtime
        .as_mut()
        .ok_or_else(|| "runtime client is not connected".to_string())?
        .request_async(
            "agent_runtime_config_set",
            json!({
                "request": {
                    "modelProviderId": prompt.provider_id,
                    "modelApiKey": api_key,
                }
            }),
        )?;
    app.pending_model_request = Some(PendingModelRequest::Setting {
        response,
        mutation: ModelMutation::Credential {
            provider_name: prompt.provider_name,
        },
    });
    app.model_credential_prompt = None;
    clear_composer(app);
    app.input_cursor = 0;
    app.message = None;
    Ok(())
}

fn model_api_key_input(input: &str) -> Result<String, String> {
    let api_key = input.trim();
    if api_key.is_empty() {
        return Err("API key is required".to_string());
    }
    if api_key.contains(['\r', '\n']) {
        return Err("API key must be one line".to_string());
    }
    Ok(api_key.to_string())
}

fn set_plugin_enabled_from_tui(app: &mut App, id: &str, enabled: bool) -> Result<(), String> {
    if id.trim().is_empty() {
        return Err("plugin id is required".to_string());
    }
    ensure_runtime(app)?;
    resolve_plugin(app, id)?;
    let response = app
        .runtime
        .as_mut()
        .ok_or_else(|| "runtime client is not connected".to_string())?
        .request(
            "plugin/set_enabled",
            json!({
                "request": {
                    "id": id,
                    "enabled": enabled,
                }
            }),
        )?;
    show_message(app, format_plugin_snapshot(&response)?);
    clear_composer(app);
    Ok(())
}

fn resolve_plugin(app: &mut App, id: &str) -> Result<(), String> {
    let response = app
        .runtime
        .as_mut()
        .ok_or_else(|| "runtime client is not connected".to_string())?
        .request("plugin/list", json!({"request": {}}))?;
    let items = response
        .as_array()
        .ok_or_else(|| "plugin/list response must be an array".to_string())?;
    let matches = items
        .iter()
        .filter(|item| item.get("id").and_then(Value::as_str) == Some(id))
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [_] => Ok(()),
        [] => Err(format!("plugin not found: {id}")),
        _ => Err(format!(
            "plugin id is ambiguous, use a unique id before toggling: {id}"
        )),
    }
}

fn format_plugin_list(response: &Value) -> Result<String, String> {
    let items = response
        .as_array()
        .ok_or_else(|| "plugin/list response must be an array".to_string())?;
    if items.is_empty() {
        return Ok("No user plugins found".to_string());
    }
    let mut lines = Vec::with_capacity(items.len() + 1);
    lines.push("Plugins".to_string());
    for item in items {
        let id = item
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| "plugin item missing id".to_string())?;
        let name = item.get("name").and_then(Value::as_str).unwrap_or(id);
        let enabled = item
            .get("enabled")
            .and_then(Value::as_bool)
            .ok_or_else(|| format!("plugin item missing enabled: {id}"))?;
        let state = if enabled { "enabled" } else { "disabled" };
        let error_count = item
            .get("errors")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0);
        if error_count == 0 {
            lines.push(format!("- {id} [{state}] {name}"));
        } else {
            lines.push(format!("- {id} [{state}] {name} ({error_count} error)"));
        }
    }
    Ok(lines.join("\n"))
}

fn format_plugin_snapshot(response: &Value) -> Result<String, String> {
    let enabled_plugins = json_string_array_len(response, "enabledPlugins")?;
    let disabled_plugins = json_string_array_len(response, "disabledPlugins")?;
    Ok(format!(
        "Plugins reloaded: enabled={enabled_plugins}, disabled={disabled_plugins}"
    ))
}

fn json_string_array_len(value: &Value, field: &str) -> Result<usize, String> {
    value
        .get(field)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("plugin snapshot missing {field}"))?
        .iter()
        .map(|item| {
            item.as_str()
                .ok_or_else(|| format!("plugin snapshot {field} item must be a string"))
        })
        .collect::<Result<Vec<_>, _>>()
        .map(|items| items.len())
}

fn slash_command_args(input: &str) -> &str {
    let Some(command) = slash_command_name(input) else {
        return "";
    };
    input.get(command.len()..).unwrap_or_default().trim()
}

fn reset_to_welcome(app: &mut App) {
    app.active_session = None;
    app.active_agent_run_id = None;
    app.active_agent_run_ids.clear();
    app.completed_agent_run_ids.clear();
    app.seen_subagent_ids.clear();
    app.live_subagent_ids.clear();
    app.agent_run_started_at = None;
    app.process_state = RuntimeDisplayState::Idle;
    app.runtime_easter_egg = None;
    app.transcript.clear();
    app.inline_images.clear();
    app.inline_image_errors.clear();
    reset_transcript_view(app);
    app.tool_projection.clear();
    app.active_tool_label = None;
    app.pending_subagent_lines.clear();
    clear_assistant_buffer(app);
    clear_composer(app);
    app.message = None;
    app.show_help = false;
    app.show_state = false;
    close_session_picker(app);
    close_model_panel(app);
    app.home_risk_panel = None;
    app.selected_command = 0;
    app.pending_question = None;
    app.tool_protocol_error = false;
}

fn build_prompt_request(
    session: &TuiSession,
    message: &str,
    attachments: &[DraftImageAttachment],
    operation_id: &str,
) -> Value {
    let mut attachments = attachments.iter().collect::<Vec<_>>();
    attachments.sort_by_key(|attachment| attachment.start);
    json!({
        "request": {
            "operationId": operation_id,
            "sessionId": session.id,
            "message": message,
            "attachments": attachments.into_iter().map(|attachment| json!({
                "placeholder": &message[attachment.start..attachment.end],
                "localPath": attachment.local_path.to_string_lossy(),
            })).collect::<Vec<_>>(),
        }
    })
}

fn start_prompt(app: &mut App, message: String) -> Result<(), String> {
    let message = message.trim_end().to_string();
    if message.trim().is_empty() {
        return Ok(());
    }
    if app.active_agent_run_id.is_some() {
        if !app.draft_image_attachments.is_empty() {
            return Err("Images cannot be added while an AgentRun is active".to_string());
        }
        return send_supplement(app, message);
    }

    commit_assistant_buffer(app);
    app.show_state = false;
    ensure_runtime(app)?;
    let session = ensure_active_session(app, message.as_str())?;
    let operation_id = new_runtime_operation_id()?;
    let result = app
        .runtime
        .as_mut()
        .ok_or_else(|| "runtime client is not connected".to_string())?
        .request(
            "session/prompt",
            build_prompt_request(
                &session,
                message.as_str(),
                app.draft_image_attachments.as_slice(),
                operation_id.as_str(),
            ),
        )?;
    app.transcript.push(TranscriptLine::User(message.clone()));
    app.transcript_follow_bottom = true;
    app.focused_tool_group = None;
    for attachment in app.draft_image_attachments.drain(..) {
        let _ = std::fs::remove_file(attachment.local_path);
    }
    clear_composer(app);
    app.input_cursor = 0;
    app.message = None;
    app.process_state = RuntimeDisplayState::Working;
    let agent_run_id = result
        .get("agentRunId")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| "session/prompt response missing agentRunId".to_string())?;
    app.active_agent_run_id = Some(agent_run_id.clone());
    app.active_agent_run_ids.clear();
    app.active_agent_run_ids.insert(agent_run_id);
    app.completed_agent_run_ids.clear();
    app.seen_subagent_ids.clear();
    app.live_subagent_ids.clear();
    app.runtime_easter_egg = None;
    app.agent_run_started_at = Some(Instant::now());
    if let Some(items) = result.get("streamItems").and_then(Value::as_array) {
        for item in items {
            apply_stream_payload(app, item);
        }
    }
    Ok(())
}

fn send_supplement(app: &mut App, message: String) -> Result<(), String> {
    let session = app
        .active_session
        .clone()
        .ok_or_else(|| "Cannot supplement without an active session".to_string())?;
    let active_agent_run_id = app
        .active_agent_run_id
        .clone()
        .ok_or_else(|| "Cannot supplement without an active AgentRun".to_string())?;

    commit_assistant_buffer(app);
    let result = app
        .runtime
        .as_mut()
        .ok_or_else(|| "runtime client is not connected".to_string())?
        .request(
            "_centaeris/session/supplement",
            json!({
                "request": {
                    "sessionId": session.id.clone(),
                    "agentRunId": active_agent_run_id.clone(),
                    "message": message,
                }
            }),
        )?;
    if result.get("accepted").and_then(Value::as_bool) != Some(true) {
        return Err("_centaeris/session/supplement rejected supplement input".to_string());
    }
    let response_agent_run_id = result
        .get("agentRunId")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| "_centaeris/session/supplement response missing agentRunId".to_string())?;
    let response_session_id = result
        .get("sessionId")
        .and_then(Value::as_str)
        .ok_or_else(|| "_centaeris/session/supplement response missing sessionId".to_string())?;
    if response_agent_run_id != active_agent_run_id || response_session_id != session.id {
        return Err(format!(
            "_centaeris/session/supplement identity mismatch: expectedSession={} actualSession={response_session_id} expectedAgentRun={active_agent_run_id} actualAgentRun={response_agent_run_id}",
            session.id
        ));
    }
    clear_composer(app);
    app.message = None;
    app.transcript_follow_bottom = true;
    app.focused_tool_group = None;
    app.process_state = RuntimeDisplayState::Working;
    if app.agent_run_started_at.is_none() {
        app.agent_run_started_at = Some(Instant::now());
    }
    if let Some(items) = result.get("streamItems").and_then(Value::as_array) {
        for item in items {
            apply_stream_payload(app, item);
        }
    }
    Ok(())
}

fn ensure_active_session(app: &mut App, title: &str) -> Result<TuiSession, String> {
    if let Some(session) = app.active_session.clone() {
        return Ok(session);
    }
    let operation_id = new_runtime_operation_id()?;
    let response = app
        .runtime
        .as_mut()
        .ok_or_else(|| "runtime client is not connected".to_string())?
        .request(
            "session/new",
            build_session_create_request(title, app.session_cwd.as_str(), operation_id.as_str()),
        )?;
    let session = tui_session_from_response(&response)?;
    app.active_session = Some(session.clone());
    Ok(session)
}

fn build_session_create_request(title: &str, cwd: &str, operation_id: &str) -> Value {
    json!({
        "request": {
            "operationId": operation_id,
            "title": title,
            "cwd": cwd,
        }
    })
}

fn new_runtime_operation_id() -> Result<String, String> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|error| format!("generate Runtime operation identity failed: {error}"))?;
    Ok(bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>())
}

fn ensure_runtime(app: &mut App) -> Result<(), String> {
    if app.runtime.is_some() {
        return Ok(());
    }
    if app.pending_model_request.is_some() {
        return Err("Runtime Server is still connecting; try again in a moment".to_string());
    }
    app.runtime = Some(start_initialized_runtime()?);
    Ok(())
}

fn start_initialized_runtime() -> Result<RuntimeClient, String> {
    let (mut runtime, descriptor) = connect_initialized_runtime()?;
    if runtime.validate_initialize_descriptor(&descriptor).is_ok() {
        return Ok(runtime);
    }

    let _ = runtime.request_with_timeout("app_exit", json!({}), RUNTIME_REPLACEMENT_EXIT_TIMEOUT);
    drop(runtime);
    thread::sleep(RUNTIME_REPLACEMENT_IDLE_WAIT);

    let (runtime, descriptor) = connect_initialized_runtime().map_err(replacement_failed)?;
    runtime
        .validate_initialize_descriptor(&descriptor)
        .map_err(replacement_failed)?;
    Ok(runtime)
}

fn connect_initialized_runtime() -> Result<(RuntimeClient, Value), String> {
    let mut runtime = RuntimeClient::start()?;
    let descriptor = runtime.request(
        "initialize",
        json!({
            "request": {
                "clientKind": "tui",
                "viewerId": tui_viewer_id(),
            }
        }),
    )?;
    Ok((runtime, descriptor))
}

fn replacement_failed(error: String) -> String {
    format!(
        "Runtime handshake remained incompatible after one orderly replacement attempt: {error}; fully exit other Centaeris Desktop/TUI hosts and retry"
    )
}

fn tui_viewer_id() -> String {
    format!("tui-{}", process::id())
}

fn start_model_command(app: &mut App, command: ModelCommand) -> Result<(), String> {
    match app.pending_model_request.as_mut() {
        Some(
            PendingModelRequest::Connecting {
                command: pending_command,
                ..
            }
            | PendingModelRequest::LoadingConfig {
                command: pending_command,
                ..
            },
        ) if matches!(&*pending_command, ModelCommand::Refresh) => {
            *pending_command = command;
            return Ok(());
        }
        Some(_) => {
            return Err("Model configuration request is already in progress".to_string());
        }
        None => {}
    }
    if app.runtime.is_some() {
        return request_model_configuration(app, command);
    }
    let (response_tx, response_rx) = mpsc::channel();
    thread::Builder::new()
        .name("centaeris-tui-runtime-connect".to_string())
        .spawn(move || {
            let _ = response_tx.send(start_initialized_runtime());
        })
        .map_err(|error| format!("start Runtime Server connection worker failed: {error}"))?;
    app.pending_model_request = Some(PendingModelRequest::Connecting {
        response_rx,
        command,
    });
    Ok(())
}

fn request_model_configuration(app: &mut App, command: ModelCommand) -> Result<(), String> {
    let response = app
        .runtime
        .as_mut()
        .ok_or_else(|| "runtime client is not connected".to_string())?
        .request_async("agent_runtime_config_get", json!({ "request": {} }))?;
    app.pending_model_request = Some(PendingModelRequest::LoadingConfig { response, command });
    Ok(())
}

fn drain_model_request(app: &mut App) -> bool {
    let Some(pending) = app.pending_model_request.take() else {
        return false;
    };
    match pending {
        PendingModelRequest::Connecting {
            response_rx,
            command,
        } => match response_rx.try_recv() {
            Ok(Ok(runtime)) => {
                app.runtime = Some(runtime);
                if let Err(error) = request_model_configuration(app, command) {
                    show_message(app, error);
                }
            }
            Ok(Err(error)) => show_message(app, error),
            Err(mpsc::TryRecvError::Empty) => {
                app.pending_model_request = Some(PendingModelRequest::Connecting {
                    response_rx,
                    command,
                });
                return false;
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                show_message(app, "Runtime Server connection worker closed".to_string());
            }
        },
        PendingModelRequest::LoadingConfig { response, command } => match response.try_recv() {
            Ok(Some(config)) => match command {
                ModelCommand::Refresh => {
                    apply_model_display(app, &config);
                    if app.model_panel.is_some() {
                        if let Err(error) = open_model_panel(app, &config) {
                            show_message(app, error);
                        }
                    }
                }
                ModelCommand::List => {
                    apply_model_display(app, &config);
                    match open_model_panel(app, &config) {
                        Ok(()) => {}
                        Err(error) => show_message(app, error),
                    }
                }
                ModelCommand::Effort(None) => {
                    let mode = config
                        .get("modelThinkingMode")
                        .and_then(Value::as_str)
                        .unwrap_or("<not configured>");
                    show_message(app, format!("Model effort: {mode}"));
                }
                ModelCommand::Effort(Some(mode)) => {
                    let result = app
                        .runtime
                        .as_mut()
                        .ok_or_else(|| "runtime client is not connected".to_string())
                        .and_then(|runtime| {
                            runtime.request_async(
                                "agent_runtime_config_set",
                                json!({ "request": { "modelThinkingMode": mode } }),
                            )
                        });
                    match result {
                        Ok(response) => {
                            app.pending_model_request = Some(PendingModelRequest::Setting {
                                response,
                                mutation: ModelMutation::Effort { mode },
                            });
                        }
                        Err(error) => show_message(app, error),
                    }
                }
                ModelCommand::Select(query) => match resolve_model_request(&config, &query) {
                    Ok((provider_id, model)) => {
                        if let Err(error) = request_model_selection(app, provider_id, model) {
                            show_message(app, error);
                        }
                    }
                    Err(model_error) => match resolve_model_provider_request(&config, &query) {
                        Ok(Some((prompt, false))) => {
                            begin_model_credential(app, prompt.provider_id, prompt.provider_name);
                        }
                        Ok(Some((_, true))) | Ok(None) => show_message(app, model_error),
                        Err(error) => show_message(app, error),
                    },
                },
            },
            Ok(None) => {
                app.pending_model_request =
                    Some(PendingModelRequest::LoadingConfig { response, command });
                return false;
            }
            Err(error) => show_message(app, error),
        },
        PendingModelRequest::Setting { response, mutation } => match response.try_recv() {
            Ok(Some(_)) => {
                let result = app
                    .runtime
                    .as_mut()
                    .ok_or_else(|| "runtime client is not connected".to_string())
                    .and_then(|runtime| {
                        runtime.request_async("agent_runtime_config_get", json!({ "request": {} }))
                    });
                match result {
                    Ok(response) => {
                        app.pending_model_request =
                            Some(PendingModelRequest::Confirming { response, mutation });
                    }
                    Err(error) => show_message(app, error),
                }
            }
            Ok(None) => {
                app.pending_model_request =
                    Some(PendingModelRequest::Setting { response, mutation });
                return false;
            }
            Err(error) => show_message(app, error),
        },
        PendingModelRequest::Confirming { response, mutation } => match response.try_recv() {
            Ok(Some(config)) => {
                apply_model_display(app, &config);
                match mutation {
                    ModelMutation::Selection => {
                        app.model_panel = None;
                        let current = app.model_display.as_deref().unwrap_or("<not configured>");
                        show_message(app, format!("Model switched to {current}"));
                    }
                    ModelMutation::Credential { provider_name } => {
                        show_message(
                            app,
                            format!("{provider_name} configured; run /model to choose a model"),
                        );
                    }
                    ModelMutation::Effort { mode } => {
                        show_message(app, format!("Model effort set to {mode}"));
                    }
                }
            }
            Ok(None) => {
                app.pending_model_request =
                    Some(PendingModelRequest::Confirming { response, mutation });
                return false;
            }
            Err(error) => show_message(app, error),
        },
    }
    true
}

fn apply_model_display(app: &mut App, response: &Value) {
    let provider_id = response.get("modelProviderId").and_then(Value::as_str);
    let model = response.get("model").and_then(Value::as_str);
    app.model_provider_id = provider_id.map(str::to_string);
    app.model_effort = response
        .get("modelThinkingMode")
        .and_then(Value::as_str)
        .map(str::to_string);
    app.model_display = match (provider_id, model) {
        (_, Some(model)) => Some(model.to_string()),
        (Some(provider_id), None) => Some(provider_id.to_string()),
        (None, None) => None,
    };
}

fn refresh_context_usage(app: &mut App) -> Result<(), String> {
    let Some(session) = app.active_session.clone() else {
        app.context_usage = None;
        return Ok(());
    };
    ensure_runtime(app)?;
    let response = app
        .runtime
        .as_mut()
        .ok_or_else(|| "runtime client is not connected".to_string())?
        .request(
            "agent_context_usage_get",
            json!({ "request": { "sessionId": session.id } }),
        )?;
    apply_context_usage(app, &response);
    Ok(())
}

fn apply_context_usage(app: &mut App, response: &Value) {
    let usage = ContextUsage {
        used_tokens: response.get("usedTokens").and_then(Value::as_u64),
        max_context_tokens: response
            .get("maxContextTokens")
            .and_then(Value::as_u64)
            .map(|v| v as u32),
        used_percentage: response
            .get("usedPercentage")
            .and_then(Value::as_u64)
            .map(|v| v as u32),
    };
    app.context_usage = if usage.used_percentage.is_none()
        && usage.used_tokens.is_none()
        && usage.max_context_tokens.is_none()
    {
        None
    } else {
        Some(usage)
    };
}

fn resume_session(app: &mut App, query: &str) -> Result<(), String> {
    ensure_runtime(app)?;
    let response = app
        .runtime
        .as_mut()
        .ok_or_else(|| "runtime client is not connected".to_string())?
        .request("session/list", json!({ "request": {} }))?;
    let sessions = tui_session_catalog(&response)?;

    if query.is_empty() {
        let workspace_response = app
            .runtime
            .as_mut()
            .ok_or_else(|| "runtime client is not connected".to_string())?
            .request("workspace_get", json!({}))?;
        let workspaces = session_workspace_choices(
            workspace_response,
            app.session_cwd.as_str(),
            sessions.as_slice(),
        )?;
        open_session_picker(app, sessions, workspaces);
        return Ok(());
    }

    let exact_matches = sessions
        .iter()
        .filter(|session| {
            session.session_kind == TuiSessionKind::Main
                && session.cwd == app.session_cwd
                && session.id == query
        })
        .cloned()
        .collect::<Vec<_>>();
    let matches = if exact_matches.is_empty() {
        sessions
            .into_iter()
            .filter(|session| {
                session.session_kind == TuiSessionKind::Main
                    && session.cwd == app.session_cwd
                    && session_matches_query(session, query)
            })
            .collect::<Vec<_>>()
    } else {
        exact_matches
    };

    match matches.len() {
        0 => Err(format!("No session matches: {query}")),
        1 => activate_session(app, matches[0].clone()),
        _ => {
            open_session_picker(
                app,
                matches,
                vec![SessionWorkspace {
                    root: app.session_cwd.clone(),
                    name: session_workspace_name(app.session_cwd.as_str()),
                }],
            );
            Ok(())
        }
    }
}

fn tui_session_catalog(response: &Value) -> Result<Vec<TuiSession>, String> {
    response
        .as_array()
        .ok_or_else(|| "session/list response must be an array".to_string())?
        .iter()
        .map(tui_session_from_response)
        .collect()
}

fn session_workspace_choices(
    response: Value,
    current_workspace: &str,
    sessions: &[TuiSession],
) -> Result<Vec<SessionWorkspace>, String> {
    let snapshot = serde_json::from_value::<TuiWorkspaceSnapshot>(response)
        .map_err(|error| format!("invalid workspace catalog: {error}"))?;
    if snapshot.cancelled {
        return Err("workspace_get unexpectedly returned cancelled=true".to_string());
    }
    for workspace in &snapshot.workspaces {
        if workspace.root.trim().is_empty() || workspace.name.trim().is_empty() {
            return Err("workspace_get returned an empty workspace root or name".to_string());
        }
    }
    let mut session_roots = Vec::new();
    for root in sessions
        .iter()
        .filter(|session| session.session_kind == TuiSessionKind::Main)
        .map(|session| session.cwd.as_str())
    {
        if !session_roots.contains(&root) {
            session_roots.push(root);
        }
    }
    let mut workspaces = snapshot
        .workspaces
        .into_iter()
        .filter(|workspace| {
            workspace.root == current_workspace || session_roots.contains(&workspace.root.as_str())
        })
        .map(|workspace| SessionWorkspace {
            root: workspace.root,
            name: workspace.name,
        })
        .collect::<Vec<_>>();
    for root in session_roots {
        if !workspaces.iter().any(|workspace| workspace.root == root) {
            workspaces.push(SessionWorkspace {
                root: root.to_string(),
                name: session_workspace_name(root),
            });
        }
    }
    if !workspaces
        .iter()
        .any(|workspace| workspace.root == current_workspace)
    {
        workspaces.push(SessionWorkspace {
            root: current_workspace.to_string(),
            name: session_workspace_name(current_workspace),
        });
    }
    let current = workspaces
        .iter()
        .position(|workspace| workspace.root == current_workspace)
        .ok_or_else(|| "current workspace missing from session workspace list".to_string())?;
    workspaces.swap(0, current);
    Ok(workspaces)
}

fn session_workspace_name(root: &str) -> String {
    Path::new(root)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or(root)
        .to_string()
}

fn open_session_picker(
    app: &mut App,
    sessions: Vec<TuiSession>,
    workspaces: Vec<SessionWorkspace>,
) {
    app.session_catalog = sessions;
    app.session_workspaces = workspaces;
    app.selected_session_workspace = 0;
    app.session_picker_open = true;
    refresh_visible_sessions(app);
    app.message = None;
    app.show_help = false;
    app.show_state = false;
    app.selected_command = 0;
}

fn close_session_picker(app: &mut App) {
    app.session_picker_open = false;
    app.session_catalog.clear();
    app.sessions.clear();
    app.session_workspaces.clear();
    app.selected_session_workspace = 0;
    app.session_workspace_hit_regions.clear();
    app.session_list_area = None;
    app.session_list_offset = 0;
    app.session_action_area = None;
    app.selected_session = 0;
    app.pending_delete = None;
    app.rename_session_id = None;
    clear_composer(app);
}

fn refresh_visible_sessions(app: &mut App) {
    let Some(workspace) = app.session_workspaces.get(app.selected_session_workspace) else {
        app.sessions.clear();
        app.selected_session = 0;
        return;
    };
    app.sessions = app
        .session_catalog
        .iter()
        .filter(|session| {
            session.session_kind == TuiSessionKind::Main && session.cwd == workspace.root
        })
        .cloned()
        .collect();
    app.selected_session = 0;
}

fn move_session_workspace(app: &mut App, delta: isize) {
    if app.session_workspaces.is_empty() {
        return;
    }
    let len = app.session_workspaces.len();
    let current = app.selected_session_workspace.min(len - 1);
    app.selected_session_workspace = match delta.cmp(&0) {
        std::cmp::Ordering::Less => current.checked_sub(1).unwrap_or(len - 1),
        std::cmp::Ordering::Greater => (current + 1) % len,
        std::cmp::Ordering::Equal => current,
    };
    app.message = None;
    refresh_visible_sessions(app);
}

fn move_session_selection(app: &mut App, delta: isize) {
    if app.sessions.is_empty() {
        app.selected_session = 0;
        return;
    }

    let len = app.sessions.len();
    let current = app.selected_session.min(len - 1);
    app.selected_session = match delta.cmp(&0) {
        std::cmp::Ordering::Less => current.checked_sub(1).unwrap_or(len - 1),
        std::cmp::Ordering::Greater => (current + 1) % len,
        std::cmp::Ordering::Equal => current,
    };
}

fn handle_session_picker_key(key: KeyEvent, app: &mut App) -> bool {
    match key.code {
        KeyCode::Esc => close_session_picker(app),
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            close_session_picker(app)
        }
        KeyCode::Up => move_session_selection(app, -1),
        KeyCode::Down => move_session_selection(app, 1),
        KeyCode::Tab => move_session_workspace(app, 1),
        KeyCode::BackTab => move_session_workspace(app, -1),
        KeyCode::Char('d') => begin_delete_selected_session(app),
        KeyCode::Char('r') => begin_rename_selected_session(app),
        KeyCode::Char('p') => {
            if let Err(error) = toggle_pin_selected_session(app) {
                show_message(app, error);
            }
        }
        KeyCode::Enter => {
            if let Err(error) = activate_selected_session(app) {
                show_message(app, error);
            }
        }
        _ => {}
    }
    false
}

fn selected_session(app: &App) -> Option<TuiSession> {
    app.sessions
        .get(
            app.selected_session
                .min(app.sessions.len().saturating_sub(1)),
        )
        .cloned()
}

fn handle_delete_key(key: KeyEvent, app: &mut App) -> bool {
    match key.code {
        KeyCode::Char('y') | KeyCode::Enter => {
            if let Err(error) = confirm_delete_session(app) {
                show_message(app, error);
            }
            false
        }
        KeyCode::Char('n') | KeyCode::Esc => {
            app.pending_delete = None;
            false
        }
        _ => false,
    }
}

fn handle_rename_key(key: KeyEvent, app: &mut App) -> bool {
    match key.code {
        KeyCode::Enter => {
            let title = app.input.trim().to_string();
            let session_id = app.rename_session_id.take();
            clear_composer(app);
            let Some(session_id) = session_id else {
                return false;
            };
            if title.is_empty() {
                app.message = Some("Rename requires a non-empty title".to_string());
                return false;
            }
            if let Err(error) = update_session_metadata(app, session_id.as_str(), Some(title), None)
            {
                show_message(app, error);
            }
            false
        }
        KeyCode::Esc => {
            app.rename_session_id = None;
            clear_composer(app);
            app.input_cursor = 0;
            false
        }
        KeyCode::Char(ch) => {
            insert_text_at_cursor(app, &ch.to_string());
            false
        }
        KeyCode::Backspace => {
            backspace_at_cursor(app);
            false
        }
        KeyCode::Left => {
            move_cursor_left(app);
            false
        }
        KeyCode::Right => {
            move_cursor_right(app);
            false
        }
        KeyCode::Home => {
            set_input_cursor(app, 0);
            false
        }
        KeyCode::End => {
            set_input_cursor(app, app.input.len());
            false
        }
        KeyCode::Delete => {
            delete_at_cursor(app);
            false
        }
        _ => false,
    }
}

fn begin_delete_selected_session(app: &mut App) {
    let Some(session) = selected_session(app) else {
        return;
    };
    app.pending_delete = Some(session.id.clone());
    clear_composer(app);
}

fn confirm_delete_session(app: &mut App) -> Result<(), String> {
    let session_id = app
        .pending_delete
        .take()
        .ok_or_else(|| "no pending session delete".to_string())?;
    ensure_runtime(app)?;
    app.runtime
        .as_mut()
        .ok_or_else(|| "runtime client is not connected".to_string())?
        .request(
            "_centaeris/session/delete",
            json!({ "request": { "sessionId": session_id } }),
        )?;
    clear_deleted_active_session(app, session_id.as_str());
    refresh_session_list(app)?;
    Ok(())
}

fn clear_deleted_active_session(app: &mut App, session_id: &str) {
    if app
        .active_session
        .as_ref()
        .is_some_and(|session| session.id == session_id)
    {
        reset_to_welcome(app);
        app.context_usage = None;
    }
}

fn begin_rename_selected_session(app: &mut App) {
    let Some(session) = selected_session(app) else {
        return;
    };
    app.rename_session_id = Some(session.id.clone());
    app.input = session.title.clone();
}

fn toggle_pin_selected_session(app: &mut App) -> Result<(), String> {
    let session = selected_session(app).ok_or_else(|| "no session selected".to_string())?;
    update_session_metadata(app, session.id.as_str(), None, Some(!session.is_pinned))
}

fn update_session_metadata(
    app: &mut App,
    session_id: &str,
    title: Option<String>,
    is_pinned: Option<bool>,
) -> Result<(), String> {
    ensure_runtime(app)?;
    app.runtime
        .as_mut()
        .ok_or_else(|| "runtime client is not connected".to_string())?
        .request(
            "_centaeris/session/update_metadata",
            json!({
                "request": {
                    "sessionId": session_id,
                    "title": title,
                    "isPinned": is_pinned,
                }
            }),
        )?;
    refresh_session_list(app)?;
    Ok(())
}

fn refresh_session_list(app: &mut App) -> Result<(), String> {
    let selected_session_id = selected_session(app).map(|session| session.id);
    ensure_runtime(app)?;
    let response = app
        .runtime
        .as_mut()
        .ok_or_else(|| "runtime client is not connected".to_string())?
        .request("session/list", json!({ "request": {} }))?;
    app.session_catalog = tui_session_catalog(&response)?;
    refresh_visible_sessions(app);
    app.selected_session = selected_session_id
        .as_deref()
        .and_then(|session_id| {
            app.sessions
                .iter()
                .position(|session| session.id == session_id)
        })
        .unwrap_or(0);
    Ok(())
}

fn submit_question_answer(app: &mut App, answer_text: String) -> Result<(), String> {
    let answer_text = answer_text.trim().to_string();
    if answer_text.is_empty() {
        return Err("question answer cannot be empty".to_string());
    }
    let question = app
        .pending_question
        .clone()
        .ok_or_else(|| "No pending question".to_string())?;
    let session = app
        .active_session
        .clone()
        .ok_or_else(|| "Cannot answer question without an active session".to_string())?;
    ensure_runtime(app)?;
    let result = app
        .runtime
        .as_mut()
        .ok_or_else(|| "runtime client is not connected".to_string())?
        .request(
            "_centaeris/session/answer_question",
            question_answer_request(
                session.id.as_str(),
                question.id.as_str(),
                answer_text.as_str(),
            ),
        )?;
    let agent_run_id = result
        .get("agentRunId")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| {
            "_centaeris/session/answer_question response missing agentRunId".to_string()
        })?;
    app.pending_question = None;
    app.active_agent_run_id = Some(agent_run_id.clone());
    app.active_agent_run_ids.clear();
    app.active_agent_run_ids.insert(agent_run_id);
    app.seen_subagent_ids.clear();
    app.live_subagent_ids.clear();
    app.runtime_easter_egg = None;
    clear_composer(app);
    app.message = None;
    app.process_state = RuntimeDisplayState::Working;
    if app.agent_run_started_at.is_none() {
        app.agent_run_started_at = Some(Instant::now());
    }
    if let Some(items) = result.get("streamItems").and_then(Value::as_array) {
        for item in items {
            apply_stream_payload(app, item);
        }
    }
    Ok(())
}

fn question_answer_request(session_id: &str, question_id: &str, answer_text: &str) -> Value {
    json!({
        "request": {
            "sessionId": session_id,
            "questionId": question_id,
            "answerText": answer_text,
            "answers": [],
        }
    })
}

fn activate_selected_session(app: &mut App) -> Result<(), String> {
    let session = app
        .sessions
        .get(
            app.selected_session
                .min(app.sessions.len().saturating_sub(1)),
        )
        .cloned()
        .ok_or_else(|| "No session selected".to_string())?;
    activate_session(app, session)
}

fn activate_session(app: &mut App, session: TuiSession) -> Result<(), String> {
    if has_active_agent_run(app) {
        return Err(
            "Current task is still running; use /stop before switching sessions".to_string(),
        );
    }
    let restore = load_session_history(app, &session)?;
    let activated = app
        .runtime
        .as_mut()
        .ok_or_else(|| "runtime client is not connected".to_string())?
        .request(
            "_centaeris/session/activate",
            json!({"request": {"sessionId": session.id}}),
        )?;
    let activated_session = tui_session_from_response(&activated)?;
    if activated_session.id != session.id {
        return Err("_centaeris/session/activate returned a different session".to_string());
    }
    if session.cwd != app.workspace_root {
        let workspace_response = app
            .runtime
            .as_mut()
            .ok_or_else(|| "runtime client is not connected".to_string())?
            .request(
                "workspace_activate",
                json!({"request": {"root": session.cwd.as_str()}}),
            )?;
        apply_workspace_activation(app, session.cwd.as_str(), workspace_response)?;
    }
    app.active_session = Some(session.clone());
    app.transcript = restore.transcript;
    app.inline_images.clear();
    app.inline_image_errors.clear();
    cache_transcript_images(app);
    reset_transcript_view(app);
    clear_assistant_buffer(app);
    clear_composer(app);
    app.message = None;
    app.show_state = false;
    close_session_picker(app);
    restore_active_agent_runs(app, restore.active_agent_runs);
    for item in restore.replay_items {
        apply_stream_payload(app, &item);
    }
    if app.transcript.is_empty() {
        show_message(
            app,
            format!("Resumed empty session: {}", session_summary(&session)),
        );
    }
    if let Err(error) = refresh_context_usage(app) {
        app.message = Some(error);
    }
    Ok(())
}

fn tui_session_from_response(value: &Value) -> Result<TuiSession, String> {
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "session response missing id".to_string())?;
    Ok(TuiSession {
        id: id.to_string(),
        title: value
            .get("title")
            .and_then(Value::as_str)
            .ok_or_else(|| "session response missing title".to_string())?
            .to_string(),
        updated_at: value
            .get("updatedAt")
            .and_then(Value::as_i64)
            .ok_or_else(|| "session response missing updatedAt".to_string())?,
        last_message: value
            .get("lastMessage")
            .and_then(Value::as_str)
            .map(str::to_string),
        cwd: value
            .get("cwd")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| "session response missing cwd".to_string())?
            .to_string(),
        session_kind: match value.get("sessionKind").and_then(Value::as_str) {
            Some("main") => TuiSessionKind::Main,
            Some("subagent") => TuiSessionKind::Subagent,
            Some(value) => return Err(format!("unsupported sessionKind: {value}")),
            None => return Err("session response missing sessionKind".to_string()),
        },
        activity_state: match value.get("activityState").and_then(Value::as_str) {
            Some("idle") => TuiSessionActivityState::Idle,
            Some("inactive") => TuiSessionActivityState::Inactive,
            Some(value) => return Err(format!("unsupported session activityState: {value}")),
            None => return Err("session response missing activityState".to_string()),
        },
        is_unread: value
            .get("isUnread")
            .and_then(Value::as_bool)
            .ok_or_else(|| "session response missing isUnread".to_string())?,
        is_pinned: value
            .get("isPinned")
            .and_then(Value::as_bool)
            .ok_or_else(|| "session response missing isPinned".to_string())?,
    })
}

fn load_session_history(app: &mut App, session: &TuiSession) -> Result<SessionRestore, String> {
    ensure_runtime(app)?;
    attach_session_viewer(app, session.id.as_str())?;
    let active_agent_runs = active_agent_runs(app, session.id.as_str())?;
    let runtime = app
        .runtime
        .as_mut()
        .ok_or_else(|| "runtime client is not connected".to_string())?;
    let load_response = runtime.request(
        "session/load",
        json!({
            "request": {
                "sessionId": session.id.as_str(),
            }
        }),
    )?;
    let projection_response = runtime.request(
        "_centaeris/session/project",
        json!({
            "request": {
                "sessionId": session.id.as_str(),
            }
        }),
    )?;
    let transcript = transcript_from_session_restore_response(
        session.id.as_str(),
        &load_response,
        &projection_response,
    )?;
    let replay_items = replay_active_agent_runs(app, &active_agent_runs)?;
    Ok(SessionRestore {
        transcript,
        active_agent_runs,
        replay_items,
    })
}

fn attach_session_viewer(app: &mut App, session_id: &str) -> Result<(), String> {
    let response = app
        .runtime
        .as_mut()
        .ok_or_else(|| "runtime client is not connected".to_string())?
        .request(
            "_centaeris/session/agent-runs/attach",
            json!({
                "request": {
                    "sessionId": session_id,
                    "viewerId": tui_viewer_id(),
                }
            }),
        )?;
    let transition = response
        .get("transitionReason")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            "_centaeris/session/agent-runs/attach response missing transitionReason".to_string()
        })?;
    if transition != "viewer_attached" {
        return Err(format!(
            "_centaeris/session/agent-runs/attach returned unsupported transitionReason: {transition}"
        ));
    }
    Ok(())
}

fn active_agent_runs(app: &mut App, session_id: &str) -> Result<Vec<ActiveAgentRun>, String> {
    let response = app
        .runtime
        .as_mut()
        .ok_or_else(|| "runtime client is not connected".to_string())?
        .request(
            "_centaeris/session/agent-runs",
            json!({
                "request": {
                    "sessionId": session_id,
                    "includeTerminal": false,
                }
            }),
        )?;
    active_agent_runs_from_response(&response)
}

fn active_agent_runs_from_response(response: &Value) -> Result<Vec<ActiveAgentRun>, String> {
    let agent_runs = response
        .get("agentRuns")
        .and_then(Value::as_array)
        .ok_or_else(|| "_centaeris/session/agent-runs response missing agentRuns".to_string())?;
    let mut active_agent_runs = Vec::new();
    for agent_run in agent_runs {
        let status = agent_run
            .get("status")
            .and_then(Value::as_str)
            .ok_or_else(|| "_centaeris/session/agent-runs item missing status".to_string())?;
        if !is_active_session_message_status(status) {
            continue;
        }
        let agent_run_id = agent_run
            .get("agentRunId")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                "_centaeris/session/agent-runs active item missing agentRunId".to_string()
            })?;
        active_agent_runs.push(ActiveAgentRun {
            agent_run_id: agent_run_id.to_string(),
            status: status.to_string(),
        });
    }
    Ok(active_agent_runs)
}

fn replay_active_agent_runs(
    app: &mut App,
    active_agent_runs: &[ActiveAgentRun],
) -> Result<Vec<Value>, String> {
    let mut replay_items = Vec::new();
    for agent_run in active_agent_runs {
        let response = app
            .runtime
            .as_mut()
            .ok_or_else(|| "runtime client is not connected".to_string())?
            .request(
                "_centaeris/session/agent-runs/replay",
                json!({
                    "request": {
                        "agentRunId": agent_run.agent_run_id.as_str(),
                    }
                }),
            )?;
        let agent_run_id = response
            .get("agentRunId")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                "_centaeris/session/agent-runs/replay response missing agentRunId".to_string()
            })?;
        if agent_run_id != agent_run.agent_run_id.as_str() {
            return Err(format!(
                "_centaeris/session/agent-runs/replay agentRunId mismatch: expected {}, got {agent_run_id}",
                agent_run.agent_run_id
            ));
        }
        let items = response
            .get("items")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                "_centaeris/session/agent-runs/replay response missing items".to_string()
            })?;
        replay_items.extend(items.iter().cloned());
    }
    Ok(replay_items)
}

fn restore_active_agent_runs(app: &mut App, active_agent_runs: Vec<ActiveAgentRun>) {
    app.active_agent_run_id = None;
    app.active_agent_run_ids.clear();
    app.completed_agent_run_ids.clear();
    app.seen_subagent_ids.clear();
    app.live_subagent_ids.clear();
    app.agent_run_started_at = None;
    app.tool_projection.clear();
    app.pending_subagent_lines.clear();
    app.active_tool_label = None;
    app.tool_protocol_error = false;
    app.process_state = RuntimeDisplayState::Idle;
    app.runtime_easter_egg = None;

    let Some(first) = active_agent_runs.first() else {
        return;
    };
    app.active_agent_run_id = Some(first.agent_run_id.clone());
    app.active_agent_run_ids = active_agent_runs
        .iter()
        .map(|agent_run| agent_run.agent_run_id.clone())
        .collect::<HashSet<_>>();
    app.agent_run_started_at = Some(Instant::now());
    app.process_state = runtime_display_state_from_agent_run_status(first.status.as_str());
}

fn transcript_from_session_restore_response(
    expected_session_id: &str,
    load_response: &Value,
    projection_response: &Value,
) -> Result<Vec<TranscriptLine>, String> {
    let messages = session_load_messages(expected_session_id, load_response)?;
    let assistant_keys = messages
        .iter()
        .filter(|message| message.get("role").and_then(Value::as_str) == Some("assistant"))
        .filter_map(message_agent_run_key)
        .collect::<HashSet<_>>();
    let (mut process_lines, process_order) =
        process_lines_from_session_projection_response(expected_session_id, projection_response)?;

    let mut transcript = Vec::new();
    for message in messages {
        let key = message_agent_run_key(message);
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .ok_or_else(|| "session/load message missing role".to_string())?;
        let content = message
            .get("content")
            .and_then(Value::as_str)
            .ok_or_else(|| "session/load message missing content".to_string())?;
        match role {
            "user" => {
                if !content.trim().is_empty() {
                    transcript.push(TranscriptLine::User(content.to_string()));
                }
                if key
                    .as_ref()
                    .is_some_and(|key| !assistant_keys.contains(key.as_str()))
                {
                    append_process_lines(&mut transcript, key.as_ref(), &mut process_lines);
                }
            }
            "assistant" => {
                append_process_lines(&mut transcript, key.as_ref(), &mut process_lines);
                if !content.trim().is_empty() {
                    transcript.push(TranscriptLine::Summary(content.to_string()));
                }
            }
            other => return Err(format!("session/load unsupported message role: {other}")),
        }
    }
    for key in process_order {
        append_process_lines(&mut transcript, Some(&key), &mut process_lines);
    }
    Ok(transcript)
}

fn session_load_messages<'a>(
    expected_session_id: &str,
    response: &'a Value,
) -> Result<&'a Vec<Value>, String> {
    let session_id = response
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| "session/load response missing id".to_string())?;
    if session_id != expected_session_id {
        return Err(format!(
            "session/load id mismatch: expected {expected_session_id}, got {session_id}"
        ));
    }
    let messages = response
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| "session/load response missing messages".to_string())?;
    Ok(messages)
}

fn process_lines_from_session_projection_response(
    expected_session_id: &str,
    response: &Value,
) -> Result<TranscriptProjection, String> {
    let session_id = response
        .get("session")
        .and_then(|session| session.get("id"))
        .and_then(Value::as_str)
        .ok_or_else(|| "_centaeris/session/project response missing session.id".to_string())?;
    if session_id != expected_session_id {
        return Err(format!(
            "_centaeris/session/project id mismatch: expected {expected_session_id}, got {session_id}"
        ));
    }
    let agent_run_replays = response
        .get("agentRunReplays")
        .and_then(Value::as_array)
        .ok_or_else(|| "_centaeris/session/project response missing agentRunReplays".to_string())?;
    let mut by_key = HashMap::new();
    let mut order = Vec::new();
    for replay in agent_run_replays {
        let session_id = replay
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or_else(|| "session/project agentRunReplay missing sessionId".to_string())?;
        if session_id != expected_session_id {
            return Err(format!(
                "session/project agentRunReplay session mismatch: expected {expected_session_id}, got {session_id}"
            ));
        }
        let status = replay
            .get("status")
            .and_then(Value::as_str)
            .ok_or_else(|| "session/project taskReplay missing status".to_string())?;
        if is_active_session_message_status(status) {
            continue;
        }
        let key = replay_agent_run_key(replay)?;
        let items = replay
            .get("items")
            .and_then(Value::as_array)
            .ok_or_else(|| "session/project taskReplay missing items".to_string())?;
        let replay_lines = replay_process_lines(items)?;
        if !replay_lines.is_empty() {
            order.push(key.clone());
            by_key.insert(key, replay_lines);
        }
    }
    Ok((by_key, order))
}

fn replay_process_lines(items: &[Value]) -> Result<Vec<TranscriptLine>, String> {
    let mut lines = Vec::new();
    let mut tools = ToolProjection::default();
    for item in items {
        let payload_type = item
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| "session/project replay item missing type".to_string())?;
        if payload_type != "session_event" {
            return Err(format!(
                "session/project unsupported replay payload: {payload_type}"
            ));
        }
        let event = item
            .get("event")
            .ok_or_else(|| "session/project session_event missing event".to_string())?;
        if event
            .get("visibility")
            .and_then(Value::as_str)
            .is_some_and(|visibility| visibility != "user")
        {
            continue;
        }
        let event_type = event
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| "session/project session_event missing event.type".to_string())?;
        let payload = event.get("payload").unwrap_or(&Value::Null);
        match event_type {
            "Status"
                if payload.get("stage").and_then(Value::as_str)
                    == Some("model_process_summary") =>
            {
                let message = raw_payload_string(payload, "message").ok_or_else(|| {
                    "session/project Status(model_process_summary) missing payload.message"
                        .to_string()
                })?;
                lines.push(TranscriptLine::Summary(message));
            }
            "TurnSupplement" => {
                let message = raw_payload_string(payload, "message").ok_or_else(|| {
                    "session/project TurnSupplement missing payload.message".to_string()
                })?;
                lines.push(TranscriptLine::Supplement(message));
            }
            "ToolCall" | "ToolResult" => {
                let update = tools.apply_event(event_type, event, payload)?;
                if let Some(tool) = update.started {
                    lines.push(TranscriptLine::Tool(tool));
                }
                if let Some(tool) = update.settled {
                    settle_replay_tool_line(&mut lines, tool)?;
                }
            }
            "SubagentSpawned" | "SubagentProgress" | "SubagentToolGroup" | "SubagentResult"
            | "SubagentFailed" | "SubagentCancelled" => {
                if let Some(line) = subagent_transcript_line(event_type, payload) {
                    lines.push(TranscriptLine::Subagent(line));
                }
            }
            "Error" | "RuntimeError" => {
                let message = raw_payload_string(payload, "message")
                    .or_else(|| raw_payload_string(payload, "error"))
                    .unwrap_or_else(|| event_type.to_string());
                lines.push(TranscriptLine::Error(message));
            }
            "Status" | "Final" => {}
            event_type if is_known_non_summary_replay_event(event_type) => {}
            other => {
                return Err(format!(
                    "session/project unsupported session_event type: {other}"
                ));
            }
        }
    }
    for tool in tools.seal() {
        settle_replay_tool_line(&mut lines, tool)?;
    }
    Ok(lines)
}

fn settle_replay_tool_line(
    lines: &mut [TranscriptLine],
    tool: ToolTranscriptLine,
) -> Result<(), String> {
    let key = tool.key.clone();
    let line = lines
        .iter_mut()
        .rev()
        .find(|line| matches!(line, TranscriptLine::Tool(existing) if existing.key == key))
        .ok_or_else(|| format!("session/project ToolResult missing ToolCall row: {key}"))?;
    *line = TranscriptLine::Tool(tool);
    Ok(())
}

fn is_known_non_summary_replay_event(event_type: &str) -> bool {
    matches!(
        event_type,
        "ModelRequestStart"
            | "ModelStatus"
            | "ToolCallReady"
            | "ToolProgress"
            | "PromptCompaction"
            | "QuestionRequired"
    )
}

fn append_process_lines(
    transcript: &mut Vec<TranscriptLine>,
    key: Option<&String>,
    process_lines: &mut HashMap<String, Vec<TranscriptLine>>,
) {
    if let Some(lines) = key.and_then(|key| process_lines.remove(key.as_str())) {
        transcript.extend(lines);
    }
}

fn message_agent_run_key(message: &Value) -> Option<String> {
    let turn_id = message.get("turnId").and_then(Value::as_str)?;
    let agent_run_id = message.get("agentRunId").and_then(Value::as_str)?;
    Some(agent_run_key(turn_id, agent_run_id))
}

fn replay_agent_run_key(replay: &Value) -> Result<String, String> {
    let turn_id = replay
        .get("turnId")
        .and_then(Value::as_str)
        .ok_or_else(|| "session/project agentRunReplay missing turnId".to_string())?;
    let agent_run_id = replay
        .get("agentRunId")
        .and_then(Value::as_str)
        .ok_or_else(|| "session/project agentRunReplay missing agentRunId".to_string())?;
    Ok(agent_run_key(turn_id, agent_run_id))
}

fn agent_run_key(turn_id: &str, agent_run_id: &str) -> String {
    format!("{turn_id}\u{1f}{agent_run_id}")
}

fn is_active_session_message_status(status: &str) -> bool {
    matches!(
        status.trim(),
        "running" | "queued" | "waiting_user" | "stalled"
    )
}

fn session_matches_query(session: &TuiSession, query: &str) -> bool {
    let normalized_query = query.to_lowercase();
    session
        .id
        .to_lowercase()
        .starts_with(normalized_query.as_str())
        || session
            .title
            .to_lowercase()
            .starts_with(normalized_query.as_str())
}

fn parse_pending_question(payload: &Value) -> Result<PendingQuestion, String> {
    let request = payload
        .get("questionRequest")
        .or_else(|| payload.get("question"))
        .ok_or_else(|| "QuestionRequired missing questionRequest".to_string())?;
    let id = string_field(request, "id")
        .or_else(|| string_field(request, "questionId"))
        .ok_or_else(|| "questionRequest missing id".to_string())?;
    let question = string_field(request, "question")
        .or_else(|| string_field(request, "prompt"))
        .or_else(|| string_field(payload, "message"))
        .ok_or_else(|| "questionRequest missing question".to_string())?;
    Ok(PendingQuestion {
        id,
        question,
        options: string_array_field(request, "options"),
        multi_select: request
            .get("multiSelect")
            .or_else(|| request.get("multi_select"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
        required: request
            .get("required")
            .and_then(Value::as_bool)
            .unwrap_or(true),
    })
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(ToString::to_string)
}

fn string_array_field(value: &Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn drain_runtime_events(app: &mut App) -> bool {
    let mut changed = false;
    while let Some(event) = app.runtime.as_ref().and_then(RuntimeClient::try_recv_event) {
        changed = true;
        match event {
            RuntimeEvent::SessionUpdate(update) => apply_session_update(app, &update),
            RuntimeEvent::RuntimeConfigChanged => {
                app.runtime_config_refresh_pending = true;
            }
            RuntimeEvent::Error(error) => {
                seal_active_tool_calls(app);
                commit_assistant_buffer(app);
                app.transcript.push(TranscriptLine::Error(error));
                app.runtime = None;
                app.active_agent_run_id = None;
                app.active_agent_run_ids.clear();
                app.completed_agent_run_ids.clear();
                app.tool_projection.clear();
                app.pending_subagent_lines.clear();
                app.agent_run_started_at = None;
                app.tool_protocol_error = false;
                app.process_state = RuntimeDisplayState::Idle;
            }
        }
    }
    if app.runtime_config_refresh_pending && app.pending_model_request.is_none() {
        app.runtime_config_refresh_pending = false;
        if let Err(error) = request_model_configuration(app, ModelCommand::Refresh) {
            show_message(app, error);
        }
        changed = true;
    }
    changed
}

fn apply_session_update(app: &mut App, update: &Value) {
    let update_agent_run_id = update.get("agentRunId").and_then(Value::as_str);
    if !app.active_agent_run_ids.is_empty() {
        if !update_agent_run_id
            .is_some_and(|agent_run_id| app.active_agent_run_ids.contains(agent_run_id))
        {
            let expected = app
                .active_agent_run_ids
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            app.transcript.push(TranscriptLine::Error(format!(
                "session/update agentRunId mismatch: expected one of [{expected}], got {}",
                update_agent_run_id.unwrap_or("<missing>")
            )));
            return;
        }
    } else if let Some(active_agent_run_id) = app.active_agent_run_id.as_deref() {
        if update_agent_run_id != Some(active_agent_run_id) {
            app.transcript.push(TranscriptLine::Error(format!(
                "session/update agentRunId mismatch: expected {active_agent_run_id}, got {}",
                update_agent_run_id.unwrap_or("<missing>")
            )));
            return;
        }
    }
    let Some(payload) = update.get("payload") else {
        app.transcript.push(TranscriptLine::Error(
            "session/update missing payload".to_string(),
        ));
        return;
    };
    apply_stream_payload_for_agent_run(app, payload, update_agent_run_id);
}

fn apply_stream_payload(app: &mut App, payload: &Value) {
    apply_stream_payload_for_agent_run(
        app,
        payload,
        payload.get("agentRunId").and_then(Value::as_str),
    );
}

fn apply_stream_payload_for_agent_run(app: &mut App, payload: &Value, agent_run_id: Option<&str>) {
    match payload.get("type").and_then(Value::as_str) {
        Some("runtime_event" | "session_event") => {
            if let Some(event) = payload.get("event") {
                apply_session_event(app, event, agent_run_id);
            } else {
                app.transcript.push(TranscriptLine::Error(
                    "runtime/session event payload missing event".to_string(),
                ));
            }
        }
        Some("error") => {
            let message = payload
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("runtime error");
            app.transcript
                .push(TranscriptLine::Error(message.to_string()));
            finish_active_agent_run(app);
        }
        Some(other) => app.transcript.push(TranscriptLine::Error(format!(
            "unsupported stream payload: {other}"
        ))),
        None => app.transcript.push(TranscriptLine::Error(
            "stream payload missing type".to_string(),
        )),
    }
}

fn apply_session_event(app: &mut App, event: &Value, agent_run_id: Option<&str>) {
    let Some(event_type) = event.get("type").and_then(Value::as_str) else {
        app.transcript.push(TranscriptLine::Error(
            "session_event missing event.type".to_string(),
        ));
        return;
    };
    let Some(event_id) = event.get("id").and_then(Value::as_str) else {
        app.transcript.push(TranscriptLine::Error(format!(
            "{event_type} missing event.id"
        )));
        return;
    };
    if !app.seen_stream_event_ids.insert(event_id.to_string()) {
        return;
    }
    if matches!(
        event_type,
        "AgentRunCompleted" | "AgentRunFailed" | "AgentRunInterrupted"
    ) {
        if event_type == "AgentRunInterrupted" {
            let reason_type = event
                .pointer("/payload/reasonType")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !matches!(
                reason_type,
                "cancelled" | "stopped" | "shutdown" | "provider_interrupted"
            ) {
                app.transcript.push(TranscriptLine::Error(format!(
                    "AgentRunInterrupted has unsupported reasonType: {}",
                    if reason_type.is_empty() {
                        "<missing>"
                    } else {
                        reason_type
                    }
                )));
                finish_active_agent_run(app);
                return;
            }
        }
        if let Some(agent_run_id) = agent_run_id {
            mark_agent_run_terminal(app, agent_run_id);
        } else {
            app.transcript.push(TranscriptLine::Error(format!(
                "{event_type} missing stream agentRunId"
            )));
            finish_active_agent_run(app);
        }
        return;
    }
    if event
        .get("visibility")
        .and_then(Value::as_str)
        .is_some_and(|visibility| visibility != "user")
    {
        return;
    }
    let payload = event.get("payload").unwrap_or(&Value::Null);
    if let Some(process_state) = event.get("processState").and_then(Value::as_str) {
        app.process_state = runtime_display_state(process_state);
    }
    let projection_state = if event_type == "ModelRequestStart"
        && payload.get("purpose").and_then(Value::as_str) == Some("compaction")
    {
        Some("compressing")
    } else {
        event.get("processState").and_then(Value::as_str)
    };
    if let Some(process_state) = projection_state {
        update_runtime_easter_egg(app, agent_run_id, process_state);
    }
    let is_tool_flow_event = matches!(
        event_type,
        "ToolCall" | "ToolResult" | "ToolCallPreparing" | "ToolCallReady" | "ToolProgress"
    );
    if !is_tool_flow_event && !event_type.starts_with("Subagent") {
        seal_active_tool_calls(app);
        app.active_tool_label = None;
    }
    match event_type {
        "ModelRequestStart" => {
            let purpose = payload.get("purpose").and_then(Value::as_str);
            let context_token_estimate =
                payload.get("contextTokenEstimate").and_then(Value::as_u64);
            let initial_content = payload.get("initialContent").and_then(Value::as_str);
            if !purpose.is_some_and(|purpose| matches!(purpose, "main" | "compaction"))
                || context_token_estimate.is_none()
                || initial_content.is_none()
            {
                app.transcript.push(TranscriptLine::Error(
                    "ModelRequestStart requires purpose, contextTokenEstimate, and initialContent"
                        .to_string(),
                ));
                return;
            }
            replace_assistant_buffer(app, initial_content.unwrap_or_default().to_string());
        }
        "ModelStatus" => {}
        "ModelTextDelta" => {
            if let Some(delta) = raw_payload_string(payload, "delta") {
                app.assistant_buffer.push_str(delta.as_str());
            }
        }
        "ModelTextReplace" => {
            replace_assistant_buffer(
                app,
                raw_payload_string(payload, "content").unwrap_or_default(),
            );
        }
        "Final" => {
            if let Some(content) = raw_payload_string(payload, "content") {
                replace_assistant_buffer(app, content);
            }
        }
        "Status" => {
            if payload.get("stage").and_then(Value::as_str) == Some("model_process_summary") {
                if let Some(message) = raw_payload_string(payload, "message") {
                    discard_assistant_stream(app);
                    app.transcript.push(TranscriptLine::Summary(message));
                }
            }
        }
        "TurnSupplement" => {
            let Some(message) = raw_payload_string(payload, "message") else {
                app.transcript.push(TranscriptLine::Error(
                    "TurnSupplement missing payload.message".to_string(),
                ));
                return;
            };
            commit_assistant_buffer(app);
            app.transcript.push(TranscriptLine::Supplement(message));
        }
        "ToolCall" | "ToolResult" => apply_tool_event(app, event_type, event, payload),
        "ToolCallPreparing" | "ToolCallReady" | "ToolProgress" => {
            apply_tool_activity(app, event_type, event)
        }
        "SubagentSpawned" | "SubagentProgress" | "SubagentToolGroup" | "SubagentResult"
        | "SubagentFailed" | "SubagentCancelled" => {
            let Some(subagent_id) = payload.get("subagentId").and_then(Value::as_str) else {
                app.transcript.push(TranscriptLine::Error(format!(
                    "{event_type} missing payload.subagentId"
                )));
                return;
            };
            if agent_run_id == app.active_agent_run_id.as_deref() {
                app.seen_subagent_ids.insert(subagent_id.to_string());
                if matches!(
                    event_type,
                    "SubagentResult" | "SubagentFailed" | "SubagentCancelled"
                ) {
                    app.live_subagent_ids.remove(subagent_id);
                } else {
                    app.live_subagent_ids.insert(subagent_id.to_string());
                }
            }
            if let Some(line) = subagent_transcript_line(event_type, payload) {
                let line = TranscriptLine::Subagent(line);
                if !app.tool_projection.has_open_calls() {
                    app.transcript.push(line);
                } else {
                    app.pending_subagent_lines.push(line);
                }
            }
        }
        "RuntimeError" | "Error" => {
            let message = raw_payload_string(payload, "message")
                .unwrap_or_else(|| "runtime error".to_string());
            app.transcript.push(TranscriptLine::Error(message));
            finish_active_agent_run(app);
        }
        "QuestionRequired" => match parse_pending_question(payload) {
            Ok(question) => {
                app.pending_question = Some(question);
                clear_composer(app);
                app.process_state = RuntimeDisplayState::WaitingUser;
            }
            Err(error) => {
                app.transcript.push(TranscriptLine::Error(error));
            }
        },
        "AgentRunInterventionChanged" | "RuntimeWaitChanged" | "Citation" | "Artifact" => {}
        other => app.transcript.push(TranscriptLine::Error(format!(
            "unsupported runtime/session event type: {other}"
        ))),
    }
}

fn subagent_transcript_line(event_type: &str, payload: &Value) -> Option<SubagentTranscriptLine> {
    let subagent_id = payload.get("subagentId").and_then(Value::as_str);
    let title = payload
        .get("description")
        .and_then(Value::as_str)
        .filter(|title| !title.trim().is_empty())
        .map(str::to_string)
        .or_else(|| {
            payload
                .get("title")
                .and_then(Value::as_str)
                .filter(|title| !title.trim().is_empty())
                .map(str::to_string)
        })
        .or_else(|| subagent_id.map(str::to_string))
        .unwrap_or_else(|| "subagent".to_string());
    let summary = payload
        .get("summary")
        .and_then(Value::as_str)
        .filter(|summary| !summary.trim().is_empty())
        .or_else(|| payload.get("message").and_then(Value::as_str))
        .unwrap_or_default();
    Some(SubagentTranscriptLine {
        title,
        summary: summary.to_string(),
        status: event_type.trim_start_matches("Subagent").to_lowercase(),
    })
}

fn apply_tool_event(app: &mut App, event_type: &str, event: &Value, payload: &Value) {
    let update = match app.tool_projection.apply_event(event_type, event, payload) {
        Ok(update) => update,
        Err(error) => {
            push_tool_protocol_error(app, event_type, event, error.as_str());
            return;
        }
    };
    if update.started.is_some() {
        commit_assistant_buffer(app);
    }
    if let Some(tool) = update.started {
        app.transcript.push(TranscriptLine::Tool(tool));
    }
    if let Some(tool) = update.settled {
        cache_tool_images(app, tool.images.as_slice());
        settle_tool_line(app, tool);
    }
    app.active_tool_label = update.active_label;
}

fn cache_transcript_images(app: &mut App) {
    let images = app
        .transcript
        .iter()
        .filter_map(|line| match line {
            TranscriptLine::Tool(tool) => Some(tool.images.as_slice()),
            _ => None,
        })
        .flatten()
        .cloned()
        .collect::<Vec<_>>();
    cache_tool_images(app, images.as_slice());
}

fn cache_tool_images(app: &mut App, images: &[ToolImage]) {
    for image in images {
        if app.inline_images.contains_key(image.key.as_str())
            || app.inline_image_errors.contains_key(image.key.as_str())
        {
            continue;
        }
        match load_tool_image(app, image) {
            Ok(protocol) => {
                app.inline_images.insert(image.key.clone(), protocol);
            }
            Err(error) => {
                app.inline_image_errors.insert(image.key.clone(), error);
            }
        }
    }
}

fn load_tool_image(app: &mut App, image: &ToolImage) -> Result<StatefulProtocol, String> {
    ensure_runtime(app)?;
    let session_id = app
        .active_session
        .as_ref()
        .map(|session| session.id.clone())
        .ok_or_else(|| "inline image requires an active session".to_string())?;
    let response = app
        .runtime
        .as_mut()
        .ok_or_else(|| "runtime client is not connected".to_string())?
        .request(
            "workspace_read_file",
            json!({
                "request": {
                    "sessionId": session_id,
                    "workspaceRoot": app.workspace_root,
                    "path": image.path,
                }
            }),
        )?;
    let decoded = decode_workspace_tool_image(response, app.workspace_root.as_str(), image)?;
    Ok(app.image_picker.new_resize_protocol(decoded))
}

fn decode_workspace_tool_image(
    response: Value,
    expected_root: &str,
    expected: &ToolImage,
) -> Result<image::DynamicImage, String> {
    let response = serde_json::from_value::<TuiWorkspaceImageResponse>(response)
        .map_err(|error| format!("invalid workspace image response: {error}"))?;
    if response.root != expected_root
        || response.path.replace('\\', "/") != expected.path.replace('\\', "/")
        || response.name.trim().is_empty()
        || !response.content.is_empty()
        || response.content_kind != "image"
        || response.encoding != "base64"
        || response.mime_type.as_deref() != Some(expected.content_type.as_str())
        || response.byte_len != expected.byte_length
    {
        return Err("workspace image response does not match the ToolResult image".to_string());
    }
    let prefix = format!("data:{};base64,", expected.content_type);
    let encoded = response
        .data_url
        .as_deref()
        .and_then(|value| value.strip_prefix(prefix.as_str()))
        .ok_or_else(|| "workspace image response has an invalid dataUrl".to_string())?;
    let bytes = general_purpose::STANDARD
        .decode(encoded)
        .map_err(|error| format!("decode workspace image failed: {error}"))?;
    if bytes.len() as u64 != expected.byte_length
        || format!("sha256:{:x}", Sha256::digest(bytes.as_slice())) != expected.sha256
    {
        return Err("workspace image bytes do not match the ToolResult image".to_string());
    }
    let decoded = image::load_from_memory(bytes.as_slice())
        .map_err(|error| format!("decode workspace image failed: {error}"))?;
    if decoded.dimensions() != (expected.width_px, expected.height_px) {
        return Err("workspace image dimensions do not match the ToolResult image".to_string());
    }
    Ok(decoded)
}

fn apply_tool_activity(app: &mut App, event_type: &str, event: &Value) {
    match app.tool_projection.set_activity(event) {
        Ok(label) => app.active_tool_label = Some(label),
        Err(error) => push_tool_protocol_error(app, event_type, event, error.as_str()),
    }
}

fn seal_active_tool_calls(app: &mut App) {
    for tool in app.tool_projection.seal() {
        settle_tool_line(app, tool);
    }
    app.active_tool_label = None;
}

fn settle_tool_line(app: &mut App, tool: ToolTranscriptLine) {
    let Some(index) = app.transcript.iter().rposition(
        |line| matches!(line, TranscriptLine::Tool(existing) if existing.key == tool.key),
    ) else {
        app.transcript.push(TranscriptLine::Error(format!(
            "Protocol error: settled tool call missing transcript row: {}",
            tool.key
        )));
        app.tool_protocol_error = true;
        return;
    };
    app.transcript[index] = TranscriptLine::Tool(tool);
    if !app.pending_subagent_lines.is_empty() {
        app.transcript.append(&mut app.pending_subagent_lines);
    }
}

fn push_tool_protocol_error(app: &mut App, event_type: &str, event: &Value, detail: &str) {
    let event_id = event
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("<missing event id>");
    app.transcript.push(TranscriptLine::Error(format!(
        "Protocol error: {event_type} {event_id} {detail}"
    )));
    app.tool_protocol_error = true;
}

fn mark_agent_run_terminal(app: &mut App, agent_run_id: &str) {
    app.completed_agent_run_ids.insert(agent_run_id.to_string());
    if app.active_agent_run_ids.is_empty()
        || app
            .active_agent_run_ids
            .iter()
            .all(|agent_run_id| app.completed_agent_run_ids.contains(agent_run_id))
    {
        finish_active_agent_run(app);
    }
}

fn finish_active_agent_run(app: &mut App) {
    seal_active_tool_calls(app);
    commit_assistant_buffer(app);
    app.active_agent_run_id = None;
    app.active_agent_run_ids.clear();
    app.completed_agent_run_ids.clear();
    app.seen_subagent_ids.clear();
    app.live_subagent_ids.clear();
    app.agent_run_started_at = None;
    app.tool_projection.clear();
    app.active_tool_label = None;
    app.pending_subagent_lines.clear();
    app.tool_protocol_error = false;
    app.runtime_easter_egg = None;
    app.process_state = if app.pending_question.is_some() {
        RuntimeDisplayState::WaitingUser
    } else {
        RuntimeDisplayState::Idle
    };
    if app.active_session.is_some() {
        let _ = refresh_context_usage(app);
    }
}

fn fnv1a32(value: &str) -> u32 {
    value.as_bytes().iter().fold(0x811c9dc5, |hash, byte| {
        (hash ^ u32::from(*byte)).wrapping_mul(0x01000193)
    })
}

fn update_runtime_easter_egg(app: &mut App, agent_run_id: Option<&str>, process_state: &str) {
    if agent_run_id != app.active_agent_run_id.as_deref() {
        return;
    }
    let Some(agent_run_id) = agent_run_id else {
        return;
    };
    app.runtime_easter_egg = runtime_easter_egg(agent_run_id, process_state);
}

fn runtime_easter_egg(agent_run_id: &str, process_state: &str) -> Option<&'static str> {
    if !fnv1a32(format!("runtime-egg:{agent_run_id}").as_str()).is_multiple_of(32) {
        return None;
    }
    let choices = [
        ("thinking", "a faint signal crossed the Wired…"),
        ("synthesizing", "Do You Remember Love?"),
        ("compressing", "keeping what should not be forgotten…"),
        ("recovering", "escaping convergence, one more time…"),
    ];
    let selected = choices[(fnv1a32(format!("runtime-egg-choice:{agent_run_id}").as_str()) >> 16)
        as usize
        % choices.len()];
    (selected.0 == process_state).then_some(selected.1)
}

fn tachikoma_easter_egg_count(app: &App) -> Option<usize> {
    let agent_run_id = app.active_agent_run_id.as_deref()?;
    let live_children = app.live_subagent_ids.len();
    (app.seen_subagent_ids.len() >= 3
        && live_children > 0
        && fnv1a32(format!("tachikoma:{agent_run_id}").as_str()).is_multiple_of(16))
    .then_some(live_children)
}

fn clear_assistant_buffer(app: &mut App) {
    app.assistant_buffer.clear();
    app.assistant_emitted_bytes = 0;
    app.assistant_tail_in_code_block = false;
    app.assistant_stream_started = false;
    app.assistant_stream_start = None;
}

fn reset_transcript_view(app: &mut App) {
    app.transcript_scroll = 0;
    app.transcript_max_scroll = 0;
    app.transcript_follow_bottom = true;
    app.expanded_tool_groups.clear();
    app.focused_tool_group = None;
    app.tool_group_hit_regions.clear();
}

fn replace_assistant_buffer(app: &mut App, content: String) {
    if app.assistant_buffer == content {
        return;
    }
    discard_assistant_stream(app);
    app.assistant_buffer = content;
}

fn discard_assistant_stream(app: &mut App) {
    let Some(start) = app.assistant_stream_start else {
        clear_assistant_buffer(app);
        return;
    };
    if start < app.transcript.len() {
        app.transcript.truncate(start);
    }
    clear_assistant_buffer(app);
}

fn commit_assistant_buffer(app: &mut App) {
    materialize_assistant_tail(app, /*separator*/ true);
    clear_assistant_buffer(app);
}

fn raw_payload_string(payload: &Value, key: &str) -> Option<String> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|value| !value.is_empty())
}

fn runtime_display_state(raw: &str) -> RuntimeDisplayState {
    match raw.trim() {
        "thinking" => RuntimeDisplayState::Thinking,
        "tool_running" => RuntimeDisplayState::ToolRunning,
        "provider_waiting" => RuntimeDisplayState::ProviderWaiting,
        "waiting_user" => RuntimeDisplayState::WaitingUser,
        _ => RuntimeDisplayState::Working,
    }
}

fn runtime_display_state_from_agent_run_status(status: &str) -> RuntimeDisplayState {
    match status.trim() {
        "waiting_user" => RuntimeDisplayState::WaitingUser,
        "queued" | "running" | "stalled" => RuntimeDisplayState::Working,
        _ => RuntimeDisplayState::Idle,
    }
}

fn is_terminal_status(status: &str) -> bool {
    matches!(
        status.trim(),
        "done" | "completed" | "succeeded" | "failed" | "cancelled" | "canceled" | "stopped"
    )
}

fn move_command_selection(app: &mut App, delta: isize) {
    let commands = matching_commands(app.input.as_str());
    if commands.is_empty() {
        app.selected_command = 0;
        return;
    }

    let len = commands.len();
    let current = app.selected_command.min(len - 1);
    app.selected_command = match delta.cmp(&0) {
        std::cmp::Ordering::Less => current.checked_sub(1).unwrap_or(len - 1),
        std::cmp::Ordering::Greater => (current + 1) % len,
        std::cmp::Ordering::Equal => current,
    };
}

fn complete_selected_command(app: &mut App) {
    let Some(command) = selected_matching_command(app.input.as_str(), app.selected_command) else {
        app.message = Some(format!("Unknown slash command prefix: {}", app.input));
        return;
    };
    let Some(prefix) = slash_command_name(app.input.as_str()) else {
        return;
    };
    if !command.name.starts_with(prefix) {
        app.message = Some(format!("Unknown slash command prefix: {}", app.input));
        return;
    }

    let rest = app.input.get(prefix.len()..).unwrap_or_default();
    app.input = if rest.trim().is_empty() {
        format!("{} ", command.name)
    } else {
        format!("{}{}", command.name, rest)
    };
    set_input_cursor(app, app.input.len());
    app.message = None;
    app.show_help = false;
    app.selected_command = 0;
}

fn display_path(path: &std::path::Path) -> String {
    let raw = path.to_string_lossy().to_string();
    if let Some(rest) = raw.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{rest}");
    }
    raw.strip_prefix(r"\\?\").map(str::to_string).unwrap_or(raw)
}

#[cfg(test)]
mod tests;
