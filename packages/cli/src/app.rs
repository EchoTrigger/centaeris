use std::{
    collections::{HashMap, HashSet, VecDeque},
    env,
    error::Error,
    io,
    ops::Range,
    path::PathBuf,
    process::{self, Command},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use base64::{engine::general_purpose, Engine as _};
use crossterm::{
    cursor::SetCursorStyle,
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
        MouseEventKind,
    },
    execute,
    terminal::{
        disable_raw_mode, enable_raw_mode, BeginSynchronizedUpdate, EndSynchronizedUpdate,
        EnterAlternateScreen, LeaveAlternateScreen, SetTitle,
    },
    QueueableCommand,
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
    FilterType, Resize, ResizeEncodeRender,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};

mod commands;
mod dashboard;
mod editing;
mod inline_images;
mod input_events;
mod markdown_table;
mod media;
mod model_picker;
mod mouse;
mod output_preview;
mod paging;
mod picker_view;
mod presentation;
mod reasoning;
mod run_errors;
mod runs;
mod session_picker;
mod session_transcript;
mod session_views;
mod settings;
mod status_usage;
mod subagents;
mod terminal;
mod text_layout;
mod theme;
mod transcript;
mod video_decode;
mod view;

use crate::tool_projection::{
    empty_tool_result_detail, stable_tool_title, tool_operation_paths, DiffRow, DiffRowKind,
    TextResultLine, ToolActionKind, ToolImage, ToolProjection, ToolResultBlock, ToolTranscriptLine,
};
#[cfg(test)]
use crate::tool_projection::{ToolOperation, ToolResultState, DIFF_PREVIEW_MAX_ROWS};
use centaeris_cli_runtime::rpc_client::{
    RetainedImageResponse, RuntimeClient, RuntimeEvent, RuntimeResponse, SessionCommand,
    SessionItemResponse,
};
#[cfg(test)]
use commands::SLASH_COMMANDS;
use commands::{
    command_completion_suffix, matching_commands, selected_matching_command, slash_command_name,
};
use paging::TranscriptPagingState;
use session_transcript::*;
use session_views::SessionState;
use theme::theme;
use transcript::{SubagentTranscriptLine, TranscriptLine};
use view::*;

const EVENT_POLL_INTERVAL: Duration = Duration::from_millis(100);
const STATUS_REDRAW_INTERVAL: Duration = Duration::from_millis(120);
const HOME_RISK_RECENT_WORKSPACE_LIMIT: usize = 4;
const IMAGE_PREVIEW_ZOOM_FACTOR: f32 = 1.5;
const IMAGE_PREVIEW_MAX_ZOOM_STEPS: u8 = 8;
const IMAGE_PREVIEW_RESIZE_SETTLE: Duration = Duration::from_millis(75);
const TRANSCRIPT_PROJECTION_POLL_INTERVAL: Duration = Duration::from_millis(50);
const TOOL_IMAGE_MAX_DECODED_PIXELS: u64 = 8_388_608;
const TOOL_IMAGE_MAX_DECODED_BYTES: u64 = 32 * 1024 * 1024;
const TOOL_IMAGE_CACHE_MAX_DECODED_BYTES: u64 = 64 * 1024 * 1024;
#[derive(Debug)]
struct AppConfig {
    workspace_root: PathBuf,
    session_cwd: String,
    warn_home_on_first_prompt: bool,
}

#[derive(Clone, Debug)]
struct TuiSession {
    id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DraftImageAttachment {
    start: usize,
    end: usize,
    local_path: PathBuf,
}

enum ImagePreview {
    Pixels(Box<PixelImagePreview>),
    Details(ImagePreviewDetails),
}

struct ImagePreviewDetails {
    path: PathBuf,
    width: u32,
    height: u32,
    mime_type: &'static str,
    byte_len: u64,
}

struct PixelImagePreview {
    path: PathBuf,
    request_tx: mpsc::Sender<ImagePreviewRequest>,
    response_rx: mpsc::Receiver<Result<ImagePreviewResult, String>>,
    generation: u64,
    requested_bounds: Rect,
    prepared_bounds: Rect,
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
    bounds: Rect,
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

type TextSelection = text_layout::Selection;

#[derive(Clone, Debug, PartialEq, Eq)]
enum MouseDrag {
    Input,
    Transcript,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TuiModelChoice {
    model: String,
    display_name: String,
    thinking_modes: Vec<String>,
    default_thinking_mode: Option<String>,
    preferred_thinking_mode: Option<String>,
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
    stage: model_picker::Stage,
    query: String,
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

struct App {
    session: SessionState,
    foreground_generation: u64,
    edit_depth: usize,
    model_context_window: Option<u64>,
    model_provider_id: Option<String>,
    model_display: Option<String>,
    model_effort: Option<String>,
    runtime: Option<RuntimeClient>,
    pending_model_request: Option<PendingModelRequest>,
    runtime_config_refresh_pending: bool,
    model_credential_prompt: Option<ModelCredentialPrompt>,
    model_panel: Option<TuiModelPanel>,
    model_catalog: Vec<TuiModelProvider>,
    model_saved_composer: Option<editing::ComposerDraft>,
    model_saved_panel: Option<TuiModelPanel>,
    model_provider_hit_regions: Vec<Rect>,
    model_list_area: Option<Rect>,
    model_list_offset: usize,
    session_picker: Option<session_picker::SessionPicker>,
    settings_panel: Option<settings::Panel>,
    ui_preferences: settings::Preferences,
    session_catalog: session_picker::Catalog,
    dashboard: Option<dashboard::Dashboard>,
    subagent_dock: subagents::DockLayout,
    subagent_links: Vec<(Rect, String)>,
    external_editor_requested: bool,
    input_area: Option<Rect>,
    frame_caret: Option<Position>,
    mouse_state: mouse::State,
    clipboard: Box<dyn crate::clipboard::TextClipboard>,
    panel_area: Option<Rect>,
    image_preview_area: Option<Rect>,
    image_policy: terminal::ImagePolicy,
    media: media::State,
    inline_images: HashMap<String, inline_images::Cached<inline_images::Prepared>>,
    inline_image_cache_order: VecDeque<(String, u64)>,
    inline_image_cache_bytes: u64,
    inline_image_errors: HashMap<String, inline_images::Cached<String>>,
    pending_inline_image: Option<inline_images::Pending>,
    parked_sessions: HashMap<String, SessionState>,
    transcript_area: Option<Rect>,
    transcript_viewport: Option<Rect>,
    mouse_drag: Option<MouseDrag>,
    render_width: u16,
}

enum ModelCommand {
    Refresh,
    List,
    SettingsList,
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
    Credential {
        provider_id: String,
        provider_name: String,
    },
    Effort {
        mode: String,
    },
}

enum PendingModelRequest {
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum RuntimeDisplayState {
    #[default]
    Idle,
    ConnectionLost,
    PendingPhase,
    Thinking,
    Responding,
    ToolRunning,
    Compacting,
    Waiting(WaitReason),
    WaitingUser,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WaitReason {
    Unspecified,
    Model,
    Tasks,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EasterEgg {
    Wired,
    LongTunnel,
    Tachikoma,
    KeepsakeClover,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum StatusEasterEgg {
    #[default]
    Available,
    Showing(EasterEgg),
    Used,
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

struct SessionRestore {
    transcript: Vec<TranscriptLine>,
    transcript_paging: TranscriptPagingState,
    active_agent_runs: Vec<ActiveAgentRun>,
    replay_items: Vec<Value>,
}

pub(crate) fn run(workspace: Option<PathBuf>) -> Result<(), Box<dyn Error>> {
    let config = resolve_config(workspace)
        .map_err(|message| io::Error::new(io::ErrorKind::InvalidInput, message))?;
    let mut app = App::new(config);
    app.runtime = Some(RuntimeClient::start_embedded(&tui_viewer_id())?);
    app.ui_preferences = settings::Preferences::load(
        centaeris_cli_runtime::profile_root()
            .join("config")
            .join("tui.json"),
    );
    let result = (|| -> Result<(), Box<dyn Error>> {
        start_model_command(&mut app, ModelCommand::Refresh)?;
        let mut surface = terminal::Surface::enter()?;
        let result = run_event_loop(&mut surface, &mut app);
        result.and(surface.leave().map_err(Into::into))
    })();
    let shutdown = app
        .runtime
        .as_mut()
        .map_or(Ok(()), RuntimeClient::shutdown_embedded);
    let preferences = app.ui_preferences.finish_write();
    result
        .and(shutdown.map_err(Into::into))
        .and(preferences.map_err(Into::into))
}

fn resolve_config(workspace: Option<PathBuf>) -> Result<AppConfig, String> {
    let default_workspace = env::current_dir()
        .map_err(|error| format!("cannot read current directory as default workspace: {error}"))?;
    resolve_config_with_default_and_home(workspace, default_workspace, user_home_dir())
}

#[cfg(test)]
fn resolve_config_with_default(
    workspace: Option<PathBuf>,
    default_workspace: PathBuf,
) -> Result<AppConfig, String> {
    resolve_config_with_default_and_home(workspace, default_workspace, user_home_dir())
}

fn resolve_config_with_default_and_home(
    workspace_root: Option<PathBuf>,
    default_workspace: PathBuf,
    home_dir: Option<PathBuf>,
) -> Result<AppConfig, String> {
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

impl Drop for App {
    fn drop(&mut self) {
        clear_composer(self);
        if let Some(draft) = self.model_saved_composer.take() {
            draft.restore(self);
            clear_composer(self);
        }
    }
}

impl App {
    fn new(config: AppConfig) -> Self {
        Self {
            session: SessionState::startup(config),
            foreground_generation: 0,
            edit_depth: 0,
            model_context_window: None,
            model_provider_id: None,
            model_display: None,
            model_effort: None,
            runtime: None,
            pending_model_request: None,
            runtime_config_refresh_pending: false,
            model_credential_prompt: None,
            model_panel: None,
            model_catalog: Vec::new(),
            model_saved_composer: None,
            model_saved_panel: None,
            model_provider_hit_regions: Vec::new(),
            model_list_area: None,
            model_list_offset: 0,
            session_picker: None,
            settings_panel: None,
            ui_preferences: settings::Preferences::default(),
            session_catalog: session_picker::Catalog::default(),
            dashboard: None,
            subagent_dock: subagents::DockLayout::default(),
            subagent_links: Vec::new(),
            external_editor_requested: false,
            input_area: None,
            frame_caret: None,
            mouse_state: mouse::State::default(),
            clipboard: Box::<crate::clipboard::SystemClipboard>::default(),
            panel_area: None,
            image_preview_area: None,
            image_policy: terminal::ImagePolicy::default(),
            media: media::State::default(),
            inline_images: HashMap::new(),
            inline_image_cache_order: VecDeque::new(),
            inline_image_cache_bytes: 0,
            inline_image_errors: HashMap::new(),
            pending_inline_image: None,
            parked_sessions: HashMap::new(),
            transcript_area: None,
            transcript_viewport: None,
            mouse_drag: None,
            render_width: 80,
        }
    }

    fn command_panel_open(&self) -> bool {
        !self.secret_input_active()
            && self.session.image_preview.is_none()
            && self.model_panel.is_none()
            && self.session.home_risk_panel.is_none()
            && self.session.read_only_session.is_none()
            && self.session.message.is_none()
            && (self.session.show_help
                || self.session.input.starts_with('/')
                    && !self.session.input.chars().any(char::is_whitespace))
    }

    fn secret_input_active(&self) -> bool {
        self.model_credential_prompt.is_some()
    }
}

fn run_event_loop(surface: &mut terminal::Surface, app: &mut App) -> Result<(), Box<dyn Error>> {
    let mut redraw = true;
    let mut last_drawn_size = None;
    let mut last_draw_at = Instant::now();
    let mut input_events = input_events::InputEvents::default();
    let mut image_policy_pending = true;
    'events: loop {
        let size = surface.terminal.size()?;
        if last_drawn_size.is_some() && last_drawn_size != Some(size) {
            mouse::resize(app);
        }
        redraw |= last_drawn_size != Some(size);
        redraw |= drain_model_request(app);
        redraw |= settings::poll(app);
        redraw |= session_picker::poll(app);
        redraw |= drain_runtime_events(app);
        redraw |= session_transcript::poll_history(app);
        let parent = app
            .session
            .active_session
            .as_ref()
            .map(|session| session.id.as_str());
        redraw |= app.session.subagents.poll(app.runtime.as_ref(), parent);
        redraw |= dashboard::drain_replies(app);
        redraw |= dashboard::poll_header(app);
        redraw |= status_usage::poll(app);
        redraw |= drain_image_preview(app);
        redraw |= output_preview::poll(app);
        redraw |= session_transcript::poll_history_images(app);
        redraw |= inline_images::poll(app);
        redraw |= media::poll(app);
        surface.clear_video_images(app.media.take_clear_ids())?;
        if materialize_assistant_prefix(app) {
            invalidate_transcript_layout(&mut app.session);
        }
        redraw |= mouse::drag_scroll_tick(app);
        let now = Instant::now();
        redraw |= periodic_redraw_due(app, last_draw_at, now);
        if redraw {
            let area = Rect::new(0, 0, size.width, size.height);
            app.render_width = area.width;
            let view = build_cached_transcript_view(app, area.width);
            let layout = FrameLayout::new(app, area);
            prepare_image_preview_bounds(app, layout.preview_history());
            let drawn = surface.draw(|frame| {
                if frame.area() == area {
                    render_prepared(frame, app, &view, layout)
                } else {
                    None
                }
            })?;
            if drawn == area {
                last_drawn_size = Some(size);
                last_draw_at = now;
                redraw = media::stop_if_hidden(app);
                if let Some(viewport) = app.transcript_viewport {
                    let drawn_revision = app.session.transcript_revision;
                    output_preview::prepare_visible(app, &view, viewport.height);
                    redraw |= app.session.transcript_revision != drawn_revision;
                    if let Some(area) = app.transcript_area {
                        redraw |= inline_images::prepare_visible(app, &view, area.height);
                        media::prepare_visible(app, &view, area.height);
                    }
                }
            } else {
                // The size changed during the draw. Poll input before trying the new geometry.
                mouse::resize(app);
            }
        }

        // Show the first frame before querying a supported terminal on stdin.
        if std::mem::take(&mut image_policy_pending) {
            app.image_policy = terminal::ImagePolicy::detect().map_err(io::Error::other)?;
            if app.image_policy.scrollback {
                invalidate_transcript_layout(&mut app.session);
                redraw = true;
            }
        }

        for event in input_events.poll(EVENT_POLL_INTERVAL)? {
            if handle_terminal_event(event, app) {
                break 'events;
            }
            if std::mem::take(&mut app.external_editor_requested) {
                media::stop(app);
                surface.clear_video_images(app.media.take_clear_ids())?;
                if let Err(error) =
                    surface.with_suspended(|| editing::event(app, launch_external_editor))?
                {
                    show_message(app, error);
                }
            }
            redraw = true;
        }
    }
    media::stop(app);
    surface.clear_video_images(app.media.take_clear_ids())?;
    Ok(())
}

fn handle_terminal_event(event: Event, app: &mut App) -> bool {
    match event {
        Event::Key(key) if key.kind != KeyEventKind::Release => return handle_key(key, app),
        Event::Paste(text) => handle_paste(app, text),
        Event::Mouse(mouse) => handle_mouse(mouse, app),
        Event::Resize(_, _) => mouse::resize(app),
        _ => {}
    }
    false
}

fn clamped_cursor(app: &App) -> usize {
    app.session.input_cursor.min(app.session.input.len())
}

fn input_selection_range(app: &App) -> Option<Range<usize>> {
    let anchor = app
        .session
        .input_selection_anchor?
        .min(app.session.input.len());
    let cursor = clamped_cursor(app);
    if anchor == cursor {
        return None;
    }
    let mut range = anchor.min(cursor)..anchor.max(cursor);
    for image in &app.session.draft_image_attachments {
        if image.start < range.end && range.start < image.end {
            range.start = range.start.min(image.start);
            range.end = range.end.max(image.end);
        }
    }
    Some(range)
}

fn set_input_cursor(app: &mut App, cursor: usize) {
    app.session.input_cursor = cursor.min(app.session.input.len());
    app.session.input_selection_anchor = None;
}

fn delete_input_selection(app: &mut App) -> bool {
    let Some(mut range) = input_selection_range(app) else {
        app.session.input_selection_anchor = None;
        return false;
    };
    loop {
        let before = range.clone();
        for attachment in &app.session.draft_image_attachments {
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
    app.session.input.replace_range(range.clone(), "");
    let mut remaining = Vec::with_capacity(app.session.draft_image_attachments.len());
    for mut attachment in app.session.draft_image_attachments.drain(..) {
        if attachment.start < range.end && range.start < attachment.end {
            app.session
                .retired_image_paths
                .insert(attachment.local_path);
            continue;
        }
        if attachment.start >= range.end {
            attachment.start -= removed_len;
            attachment.end -= removed_len;
        }
        remaining.push(attachment);
    }
    app.session.draft_image_attachments = remaining;
    app.session.input_cursor = range.start;
    app.session.input_selection_anchor = None;
    true
}

fn insert_text_at_cursor(app: &mut App, text: &str) {
    delete_input_selection(app);
    let cursor = normalized_input_cursor(app);
    app.session.input.insert_str(cursor, text);
    for attachment in &mut app.session.draft_image_attachments {
        if attachment.start >= cursor {
            attachment.start += text.len();
            attachment.end += text.len();
        }
    }
    app.session.input_cursor = cursor + text.len();
    app.session.input_selection_anchor = None;
}

fn normalized_input_cursor(app: &App) -> usize {
    let cursor = clamped_cursor(app);
    app.session
        .draft_image_attachments
        .iter()
        .find(|attachment| attachment.start < cursor && cursor < attachment.end)
        .map(|attachment| attachment.end)
        .unwrap_or(cursor)
}

fn remove_draft_image(app: &mut App, index: usize) {
    let attachment = app.session.draft_image_attachments.remove(index);
    let removed_len = attachment.end - attachment.start;
    app.session
        .input
        .replace_range(attachment.start..attachment.end, "");
    for item in &mut app.session.draft_image_attachments {
        if item.start >= attachment.end {
            item.start -= removed_len;
            item.end -= removed_len;
        }
    }
    app.session.input_cursor = attachment.start;
    app.session.input_selection_anchor = None;
    app.session
        .retired_image_paths
        .insert(attachment.local_path);
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
        .session
        .draft_image_attachments
        .iter()
        .position(|attachment| attachment.start < cursor && cursor <= attachment.end)
    {
        remove_draft_image(app, index);
        return;
    }
    let start = editing::previous(&app.session.input, cursor);
    app.session.input.replace_range(start..cursor, "");
    for attachment in &mut app.session.draft_image_attachments {
        if attachment.start >= cursor {
            attachment.start -= cursor - start;
            attachment.end -= cursor - start;
        }
    }
    app.session.input_cursor = start;
    app.session.input_selection_anchor = None;
}

fn delete_at_cursor(app: &mut App) {
    if delete_input_selection(app) {
        return;
    }
    let cursor = normalized_input_cursor(app);
    if cursor >= app.session.input.len() {
        return;
    }
    if let Some(index) = app
        .session
        .draft_image_attachments
        .iter()
        .position(|attachment| attachment.start <= cursor && cursor < attachment.end)
    {
        remove_draft_image(app, index);
        return;
    }
    let end = editing::next(&app.session.input, cursor);
    app.session.input.replace_range(cursor..end, "");
    for attachment in &mut app.session.draft_image_attachments {
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
    app.session.input_selection_anchor = None;
    let cursor = normalized_input_cursor(app);
    if cursor == 0 {
        return;
    }
    if let Some(attachment) = app
        .session
        .draft_image_attachments
        .iter()
        .find(|attachment| attachment.end == cursor)
    {
        app.session.input_cursor = attachment.start;
        return;
    }
    app.session.input_cursor = editing::previous(&app.session.input, cursor);
}

fn move_cursor_right(app: &mut App) {
    if let Some(range) = input_selection_range(app) {
        set_input_cursor(app, range.end);
        return;
    }
    app.session.input_selection_anchor = None;
    let cursor = normalized_input_cursor(app);
    if cursor >= app.session.input.len() {
        return;
    }
    if let Some(attachment) = app
        .session
        .draft_image_attachments
        .iter()
        .find(|attachment| attachment.start == cursor)
    {
        app.session.input_cursor = attachment.end;
        return;
    }
    app.session.input_cursor = editing::next(&app.session.input, cursor);
}

fn handle_paste(app: &mut App, text: String) {
    mouse::cancel_capture(app);
    if settings::visible(app) {
        settings::paste(app, &text);
        return;
    }
    if app.session.subagents.detail_open() {
        return;
    }
    if dashboard::visible(app) {
        dashboard::paste(app, &input_events::normalize_newlines(text));
        return;
    }
    editing::event(app, |app| handle_paste_text(app, text));
}

fn handle_paste_text(app: &mut App, text: String) {
    let text = input_events::normalize_newlines(text);
    if app.session_picker.is_some() {
        session_picker::paste(app, &text);
        return;
    }
    if app.session.read_only_session.is_some() {
        return;
    }
    app.session.input_scroll_override = false;
    if app.secret_input_active() {
        insert_text_at_cursor(app, &text);
        return;
    }
    if app.session.pending_question.is_some()
        || app.model_panel.is_some()
        || app.session.image_preview.is_some()
        || app.session.home_risk_panel.is_some()
        || app.command_panel_open()
    {
        return;
    }
    app.session.message = None;
    app.session.show_help = false;
    app.session.selected_command = 0;
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
    if app.secret_input_active() {
        return;
    }
    match crate::clipboard::image_from_pasted_text(app.session.input.as_str()) {
        Ok(Some(path)) => {
            app.session.input.clear();
            app.session.input_cursor = 0;
            app.session.input_selection_anchor = None;
            insert_draft_image(app, path);
        }
        Ok(None) => {}
        Err(error) => show_message(app, error),
    }
}

fn clear_composer(app: &mut App) {
    close_image_preview(app);
    clear_session_composer(&mut app.session);
}

fn clear_session_composer(state: &mut SessionState) {
    state.image_preview = None;
    state.input = String::new();
    state.input_cursor = 0;
    state.input_selection_anchor = None;
    state.next_image_number = 1;
    for attachment in state.draft_image_attachments.drain(..) {
        state.retired_image_paths.insert(attachment.local_path);
    }
    for path in state.retired_image_paths.drain() {
        let _ = std::fs::remove_file(path);
    }
    state.edit_history.clear();
    state.input_layout = None;
    state.input_scroll = 0;
    state.input_scroll_override = false;
}

fn handle_clipboard_paste(app: &mut App) {
    if app.session.pending_question.is_some()
        || app.model_panel.is_some()
        || app.session.image_preview.is_some()
        || app.session.home_risk_panel.is_some()
        || app.command_panel_open()
    {
        return;
    }
    match crate::clipboard::paste() {
        Ok(crate::clipboard::ClipboardContent::Text(text)) => handle_paste(app, text),
        Ok(crate::clipboard::ClipboardContent::Image(path)) => {
            if app.secret_input_active() {
                let _ = std::fs::remove_file(path);
            } else {
                insert_draft_image(app, path);
            }
        }
        Err(error) => show_message(app, error),
    }
}

fn insert_draft_image(app: &mut App, path: PathBuf) {
    if app.session.draft_image_attachments.len() >= 8 {
        let _ = std::fs::remove_file(path);
        show_message(app, "A message can contain at most 8 images".to_string());
        return;
    }
    let placeholder = loop {
        let placeholder = format!("[Image #{}]", app.session.next_image_number);
        app.session.next_image_number += 1;
        if !app.session.input.contains(placeholder.as_str()) {
            break placeholder;
        }
    };
    let start = normalized_input_cursor(app);
    insert_text_at_cursor(app, placeholder.as_str());
    app.session
        .draft_image_attachments
        .push(DraftImageAttachment {
            start,
            end: start + placeholder.len(),
            local_path: path,
        });
    insert_text_at_cursor(app, " ");
    app.session.message = None;
}

fn open_draft_image_preview(app: &mut App, column: u16, row: u16) -> Result<bool, String> {
    let Some(position) = raw_input_byte_from_mouse(app, column, row) else {
        return Ok(false);
    };
    let Some(attachment) = app
        .session
        .draft_image_attachments
        .iter()
        .find(|attachment| attachment.start <= position && position < attachment.end)
    else {
        return Ok(false);
    };
    let path = attachment.local_path.clone();
    let preview = match app.image_policy.picker.protocol_type() {
        ProtocolType::Kitty | ProtocolType::Iterm2 => ImagePreview::Pixels(Box::new(
            open_pixel_image_preview(path, app.image_policy.picker.clone())?,
        )),
        ProtocolType::Halfblocks | ProtocolType::Sixel => {
            ImagePreview::Details(read_image_preview_details(path)?)
        }
    };
    app.session.image_preview = Some(preview);
    app.image_preview_area = None;
    app.session.input_selection_anchor = None;
    app.session.transcript_selection = None;
    app.mouse_drag = None;
    Ok(true)
}

// The text preview reads file metadata and image headers on click, without decoding pixels.
fn read_image_preview_details(path: PathBuf) -> Result<ImagePreviewDetails, String> {
    let file = std::fs::File::open(&path)
        .map_err(|error| format!("open image preview failed: {error}"))?;
    let byte_len = file
        .metadata()
        .map_err(|error| format!("read image size failed: {error}"))?
        .len();
    let reader = image::ImageReader::new(io::BufReader::new(file))
        .with_guessed_format()
        .map_err(|error| format!("read image format failed: {error}"))?;
    let format = reader
        .format()
        .ok_or_else(|| "unrecognized image preview format".to_string())?;
    let (width, height) = reader
        .into_dimensions()
        .map_err(|error| format!("read image dimensions failed: {error}"))?;
    Ok(ImagePreviewDetails {
        path,
        width,
        height,
        mime_type: format.to_mime_type(),
        byte_len,
    })
}

fn open_pixel_image_preview(path: PathBuf, picker: Picker) -> Result<PixelImagePreview, String> {
    start_pixel_image_preview(path, picker, None)
}

fn start_pixel_image_preview(
    path: PathBuf,
    picker: Picker,
    decoded: Option<image::DynamicImage>,
) -> Result<PixelImagePreview, String> {
    let worker_path = path.clone();
    let (request_tx, request_rx) = mpsc::channel();
    let (response_tx, response_rx) = mpsc::channel();
    thread::Builder::new()
        .name("centaeris-tui-image-preview".to_string())
        .spawn(move || image_preview_worker(worker_path, picker, decoded, request_rx, response_tx))
        .map_err(|error| format!("start image preview worker failed: {error}"))?;
    Ok(PixelImagePreview {
        path,
        request_tx,
        response_rx,
        generation: 0,
        requested_bounds: Rect::default(),
        prepared_bounds: Rect::default(),
        protocol: None,
        render_size: Rect::default(),
        image_area: Rect::default(),
        view: ImagePreviewView::FIT,
        drag: None,
    })
}

fn close_image_preview(app: &mut App) {
    app.session.image_preview = None;
    app.image_preview_area = None;
}

fn image_preview_worker(
    path: PathBuf,
    picker: Picker,
    decoded: Option<image::DynamicImage>,
    request_rx: mpsc::Receiver<ImagePreviewRequest>,
    response_tx: mpsc::Sender<Result<ImagePreviewResult, String>>,
) {
    let source = if let Some(decoded) = decoded {
        decoded.to_rgba8()
    } else {
        match image::open(&path) {
            Ok(image) => image.to_rgba8(),
            Err(error) => {
                let _ = response_tx.send(Err(format!("decode image preview failed: {error}")));
                return;
            }
        }
    };
    let font_size = picker.font_size();
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
            font_size,
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
    font_size: (u16, u16),
    background_color: image::Rgba<u8>,
    protocol_type: &StatefulProtocolType,
) -> Result<ImagePreviewResult, String> {
    let render_size =
        fit_image_preview_size(source.width(), source.height(), font_size, request.bounds);
    if render_size.width == 0 || render_size.height == 0 {
        return Err("image preview has no renderable area".to_string());
    }
    let crop_ratio = image_preview_crop_ratio(request.view.zoom_steps);
    let crop_width = ((source.width() as f32 * crop_ratio).round() as u32).clamp(1, source.width());
    let crop_height =
        ((source.height() as f32 * crop_ratio).round() as u32).clamp(1, source.height());
    let crop_x = image_preview_crop_offset(source.width(), crop_width, request.view.center_x);
    let crop_y = image_preview_crop_offset(source.height(), crop_height, request.view.center_y);
    let target_width = u32::from(render_size.width) * u32::from(font_size.0);
    let target_height = u32::from(render_size.height) * u32::from(font_size.1);
    let source_view = source.view(crop_x, crop_y, crop_width, crop_height);
    let prepared = image::imageops::resize(
        &*source_view,
        target_width,
        target_height,
        FilterType::Lanczos3,
    );
    let image_source = ImageSource::new(
        image::DynamicImage::ImageRgba8(prepared),
        font_size,
        background_color,
    );
    let mut protocol = StatefulProtocol::new(image_source, font_size, protocol_type.clone());
    protocol.resize_encode(&Resize::Fit(Some(FilterType::Lanczos3)), render_size);
    match protocol.last_encoding_result() {
        Some(Ok(())) => Ok(ImagePreviewResult {
            generation: request.generation,
            bounds: request.bounds,
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

fn prepare_image_preview_bounds(app: &mut App, area: Option<Rect>) {
    let Some(ImagePreview::Pixels(preview)) = app.session.image_preview.as_mut() else {
        return;
    };
    let bounds = area.map_or_else(Rect::default, |area| image_preview_bounds(area).1);
    if preview.requested_bounds != bounds {
        preview.requested_bounds = bounds;
        preview.drag = None;
        request_image_preview_render(preview);
    }
}

fn request_image_preview_render(preview: &mut PixelImagePreview) {
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
    let Some(ImagePreview::Pixels(preview)) = app.session.image_preview.as_mut() else {
        return false;
    };
    let mut redraw = false;
    let mut failure = None;
    loop {
        match preview.response_rx.try_recv() {
            Ok(Ok(result))
                if result.generation == preview.generation
                    && result.bounds == preview.requested_bounds =>
            {
                preview.prepared_bounds = result.bounds;
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

fn zoom_image_preview(preview: &mut PixelImagePreview, delta: i8, anchor: Option<Position>) {
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

fn finish_image_preview_drag(preview: &mut PixelImagePreview, position: Position) {
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
    mouse::cancel_capture(app);
    if settings::visible(app) {
        settings::key(app, key);
        return false;
    }
    if app.session.subagents.handle_detail_key(key) {
        return false;
    }
    if dashboard::hotkey(key)
        && app.session_picker.is_none()
        && app.session.image_preview.is_none()
        && app.model_panel.is_none()
        && app.session.home_risk_panel.is_none()
        && !app.secret_input_active()
    {
        dashboard::toggle(app);
        return false;
    }
    if dashboard::visible(app) {
        dashboard::key(app, key);
        return false;
    }
    if key.code == KeyCode::Esc && key.modifiers.is_empty() && media::stop(app) {
        return false;
    }
    if app.session.read_only_session.is_some()
        && app.session_picker.is_none()
        && app.session.image_preview.is_none()
    {
        return session_picker::read_only_key(app, key);
    }
    if app.session.subagents.dock_focused() {
        if app.session.subagents.handle_dock_key(
            key,
            &app.subagent_dock,
            app.runtime.as_ref(),
            app.session.read_only_session.is_none(),
        ) {
            app.mouse_state.input_focused = !app.session.subagents.dock_focused();
            return false;
        }
        app.session.subagents.blur_dock();
    }
    if mouse::editor_enabled(app)
        && key.modifiers.contains(KeyModifiers::CONTROL)
        && !key.modifiers.contains(KeyModifiers::ALT)
        && matches!(key.code, KeyCode::Char('z' | 'Z' | 'y' | 'Y'))
    {
        editing::undo(
            app,
            key.modifiers.contains(KeyModifiers::SHIFT)
                || matches!(key.code, KeyCode::Char('y' | 'Y')),
        );
        return false;
    }
    app.session.input_scroll_override = false;
    app.mouse_state.input_focused = true;
    editing::event(app, |app| handle_key_event(key, app))
}

fn handle_key_event(key: KeyEvent, app: &mut App) -> bool {
    if app.session_picker.is_some() {
        session_picker::key(app, key);
        return false;
    }
    if app.session.image_preview.is_some() {
        if matches!(key.code, KeyCode::Esc)
            || matches!(key.code, KeyCode::Char('c'))
                && key.modifiers.contains(KeyModifiers::CONTROL)
        {
            close_image_preview(app);
        } else if let Some(ImagePreview::Pixels(preview)) = app.session.image_preview.as_mut() {
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
    if app.model_panel.is_some() {
        return handle_model_panel_key(key, app);
    }
    if app.session.home_risk_panel.is_some() {
        return handle_home_risk_key(key, app);
    }
    if app.session.pending_question.is_some() {
        return handle_question_key(key, app);
    }
    if !matches!(key.code, KeyCode::Esc) {
        app.session.pending_esc_stop = false;
    }
    match key.code {
        KeyCode::Char('m')
            if key.modifiers == KeyModifiers::CONTROL && !app.secret_input_active() =>
        {
            if let Err(error) = start_model_command(app, ModelCommand::List) {
                show_message(app, error);
            }
            false
        }
        KeyCode::Char('c')
            if key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::ALT) =>
        {
            handle_ctrl_c(app)
        }
        KeyCode::Char('v')
            if key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::ALT) =>
        {
            handle_clipboard_paste(app);
            false
        }
        KeyCode::Esc if app.model_credential_prompt.is_some() => {
            cancel_model_credential(app);
            false
        }
        KeyCode::Esc => {
            if has_active_agent_run(&app.session) {
                if app.session.pending_esc_stop {
                    app.session.pending_esc_stop = false;
                    if let Err(error) = stop_controlled_tasks(app, "tui_esc_stopped") {
                        show_message(app, error);
                    }
                } else {
                    app.session.pending_esc_stop = true;
                }
            } else {
                app.session.pending_esc_stop = false;
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
        KeyCode::PageUp => {
            scroll_transcript(app, -8);
            false
        }
        KeyCode::PageDown => {
            scroll_transcript(app, 8);
            false
        }
        KeyCode::Home if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.session.transcript_scroll = 0;
            app.session.transcript_follow_bottom = false;
            if let Err(error) = load_older_transcript_page(app) {
                show_message(app, error);
            }
            false
        }
        KeyCode::End if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.session.transcript_scroll = app.session.transcript_max_scroll;
            app.session.transcript_follow_bottom = true;
            false
        }
        KeyCode::Backspace => {
            app.session.message = None;
            backspace_at_cursor(app);
            app.session.show_help = false;
            app.session.selected_command = 0;
            false
        }
        KeyCode::Left
        | KeyCode::Right
        | KeyCode::Home
        | KeyCode::End
        | KeyCode::Up
        | KeyCode::Down => {
            editing::navigate(app, key);
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
        KeyCode::Enter if app.command_panel_open() => {
            if let Some(command) =
                selected_matching_command(&app.session.input, app.session.selected_command)
            {
                app.session.input = command.name.to_string();
                set_input_cursor(app, app.session.input.len());
                app.session.show_help = false;
                app.session.selected_command = 0;
            }
            handle_enter(app)
        }
        KeyCode::Enter => handle_enter(app),
        KeyCode::Char('g')
            if key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::ALT) =>
        {
            if app.model_credential_prompt.is_some() {
                show_message(app, "External editor is disabled for API keys".to_string());
            } else if !app.session.draft_image_attachments.is_empty() {
                show_message(
                    app,
                    "External editor is unavailable while the draft contains images".to_string(),
                );
            } else {
                app.external_editor_requested = true;
            }
            false
        }
        KeyCode::Char(ch) if editing::accepts_text(key) => {
            app.session.message = None;
            insert_text_at_cursor(app, &ch.to_string());
            promote_exact_input_image_path(app);
            app.session.show_help = false;
            app.session.selected_command = 0;
            false
        }
        _ => false,
    }
}

fn input_byte_from_mouse(app: &App, column: u16, row: u16) -> Option<usize> {
    let byte = raw_input_byte_from_mouse(app, column, row)?;
    Some(
        app.session
            .draft_image_attachments
            .iter()
            .find(|image| image.start < byte && byte < image.end)
            .map_or(byte, |image| image.end),
    )
}

fn raw_input_byte_from_mouse(app: &App, column: u16, row: u16) -> Option<usize> {
    let area = app.input_area?;
    let layout = app.session.input_layout.as_ref()?;
    let row = usize::from(
        row.saturating_sub(area.y)
            .min(area.height.saturating_sub(1)),
    ) + app.session.input_scroll;
    let byte = layout.offset_at(
        row,
        usize::from(column.saturating_sub(area.x).min(area.width)),
    );
    Some(input_source_offset(app, byte))
}

fn transcript_point_from_mouse(app: &App, column: u16, row: u16) -> Option<usize> {
    let area = app.transcript_area?;
    let layout = app.session.text_layout.as_ref()?;
    Some(
        layout.offset_at(
            app.session.text_scroll
                + usize::from(
                    row.saturating_sub(area.y)
                        .min(area.height.saturating_sub(1)),
                ),
            usize::from(column.saturating_sub(area.x).min(area.width)),
        ),
    )
}

fn selected_transcript_text(app: &App) -> Option<String> {
    app.session
        .text_layout
        .as_ref()?
        .selected(app.session.transcript_selection?)
}

fn handle_panel_click(mouse: MouseEvent, app: &mut App) -> bool {
    let position = Position::new(mouse.column, mouse.row);
    let Some(area) = app.panel_area.filter(|area| area.contains(position)) else {
        return false;
    };
    if app.model_panel.is_some() {
        if app.pending_model_request.is_some() {
            return true;
        }
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
            if let Some(row) = model_picker::rows(app).get(index) {
                model_picker::select(app, row.index);
                if let Err(error) = activate_selected_model(app) {
                    show_message(app, error);
                }
            }
            return true;
        }
        return true;
    }
    if app.session.message.is_some()
        && app.session.pending_question.is_none()
        && !app.command_panel_open()
    {
        app.session.message = None;
        return true;
    }
    let row = mouse.row.saturating_sub(area.y) as usize;

    if let Some(panel) = app.session.home_risk_panel.as_ref() {
        let option_count = panel.workspaces.len() + 1;
        if let Some(index) = row.checked_sub(2).filter(|index| *index < option_count) {
            if let Some(panel) = app.session.home_risk_panel.as_mut() {
                panel.selected = index;
                panel.notice = None;
            }
            if let Err(error) = confirm_home_risk_selection(app) {
                set_home_risk_notice(app, error);
            }
        }
        return true;
    }
    if let Some(question) = app.session.pending_question.as_ref() {
        let options = question_options_area(area);
        if options.contains(position) {
            let index = usize::from(position.y - options.y);
            let Some(answer) = question
                .options
                .get(index + app.session.question_scroll)
                .cloned()
            else {
                return true;
            };
            let submit = !question.multi_select;
            clear_composer(app);
            insert_text_at_cursor(app, answer.as_str());
            if submit {
                handle_question_enter(app);
            }
        }
        return true;
    }
    if app.command_panel_open() {
        let count = matching_commands(&app.session.input).len();
        if count > 0 {
            let selected = app.session.selected_command.min(count - 1);
            let offset = selected.saturating_sub((area.height as usize).saturating_sub(1));
            let index = offset + row;
            if index < count {
                app.session.selected_command = index;
                complete_selected_command(app);
                if matches!(
                    slash_command_name(&app.session.input),
                    Some("/model" | "/effort" | "/resume")
                ) {
                    handle_enter(app);
                }
            }
        }
        return true;
    }
    false
}

fn handle_mouse(event: MouseEvent, app: &mut App) {
    editing::event(app, |app| mouse::handle(app, event));
}

fn scroll_transcript(app: &mut App, delta: i32) {
    if delta < 0 && app.session.transcript_scroll == 0 {
        if let Err(error) = load_older_transcript_page(app) {
            show_message(app, error);
        }
    }
    let next = if delta.is_negative() {
        app.session
            .transcript_scroll
            .saturating_sub(u64::from(delta.unsigned_abs()))
    } else {
        app.session
            .transcript_scroll
            .saturating_add(delta as u64)
            .min(app.session.transcript_max_scroll)
    };
    app.session.transcript_scroll = next;
    app.session.transcript_follow_bottom = next == app.session.transcript_max_scroll;
}

fn prepend_anchor_scroll(current: u64, previous_total_rows: u64, next_total_rows: u64) -> u64 {
    current.saturating_add(next_total_rows.saturating_sub(previous_total_rows))
}

/// Ctrl+G：调用 `$VISUAL`/`$EDITOR`/默认编辑器编辑当前输入，退出后读回。
fn launch_external_editor(app: &mut App) -> Result<(), String> {
    let editor = env::var("VISUAL")
        .or_else(|_| env::var("EDITOR"))
        .unwrap_or_else(|_| default_editor());
    let temp_dir = std::env::temp_dir();
    let draft_path = temp_dir.join(format!("centaeris-tui-draft-{}.txt", std::process::id()));
    std::fs::write(draft_path.as_path(), app.session.input.as_str())
        .map_err(|error| format!("write editor draft failed: {error}"))?;
    let edited = (|| {
        let status = Command::new(editor)
            .arg(draft_path.as_path())
            .status()
            .map_err(|error| format!("launch editor failed: {error}"))?;
        if !status.success() {
            return Err(format!("editor exited with {status}"));
        }
        std::fs::read_to_string(draft_path.as_path())
            .map_err(|error| format!("read editor draft failed: {error}"))
    })();
    let _ = std::fs::remove_file(draft_path.as_path());
    app.session.input = edited?.replace("\r\n", "\n");
    set_input_cursor(app, app.session.input.len());
    app.session.message = None;
    app.session.show_help = false;
    app.session.selected_command = 0;
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
            app.session.message = None;
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
            set_input_cursor(app, app.session.input.len());
            false
        }
        KeyCode::Delete => {
            delete_at_cursor(app);
            false
        }
        KeyCode::Enter => handle_question_enter(app),
        KeyCode::Char(ch) => {
            app.session.message = None;
            insert_text_at_cursor(app, &ch.to_string());
            false
        }
        _ => false,
    }
}

fn open_home_risk_panel(app: &mut App) -> Result<(), String> {
    ensure_runtime(app)?;
    let runtime = app
        .runtime
        .as_mut()
        .ok_or_else(|| "runtime client is not connected".to_string())?;
    let workspace_response = runtime.request("workspace_get", json!({}))?;
    let session_response = runtime.request("session/list", json!({ "request": {} }))?;
    let workspaces = recent_workspace_choices(
        workspace_response,
        session_response,
        &app.session.session_cwd,
    )?;
    app.session.home_risk_panel = Some(HomeRiskPanel {
        workspaces,
        selected: 0,
        notice: None,
    });
    app.session.message = None;
    app.session.show_help = false;
    app.session.selected_command = 0;
    Ok(())
}

fn recent_workspace_choices(
    workspace_response: Value,
    session_response: Value,
    current_workspace: &str,
) -> Result<Vec<TuiWorkspaceChoice>, String> {
    let snapshot = serde_json::from_value::<TuiWorkspaceSnapshot>(workspace_response)
        .map_err(|error| format!("invalid workspace catalog: {error}"))?;
    let sessions = serde_json::from_value::<Vec<SessionItemResponse>>(session_response)
        .map_err(|error| format!("invalid session list: {error}"))?;
    let mut choices = HashMap::new();
    // For workspace_get, cancelled indicates that there is no configured workspace.
    for workspace in snapshot.workspaces {
        if workspace.root.trim().is_empty() || workspace.name.trim().is_empty() {
            return Err("workspace_get returned an empty workspace root or name".to_string());
        }
        if workspace.root != current_workspace {
            choices.insert(workspace.root.clone(), workspace);
        }
    }
    for session in sessions
        .into_iter()
        .filter(|session| session.session_kind == "main")
    {
        let Some(path) = session
            .cwd
            .and_then(|cwd| PathBuf::from(cwd).canonicalize().ok())
        else {
            continue;
        };
        let root = display_path(&path);
        if root == current_workspace || !path.is_dir() {
            continue;
        }
        choices
            .entry(root.clone())
            .and_modify(|choice| choice.updated_at = choice.updated_at.max(session.updated_at))
            .or_insert_with(|| TuiWorkspaceChoice {
                name: path
                    .file_name()
                    .map_or_else(|| root.clone(), |name| name.to_string_lossy().into_owned()),
                root,
                active_session_id: None,
                sort_order: 0,
                updated_at: session.updated_at,
            });
    }
    let mut workspaces: Vec<_> = choices.into_values().collect();
    workspaces.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then_with(|| left.sort_order.cmp(&right.sort_order))
            .then_with(|| left.root.cmp(&right.root))
    });
    workspaces.truncate(HOME_RISK_RECENT_WORKSPACE_LIMIT);
    Ok(workspaces)
}

fn move_home_risk_selection(app: &mut App, delta: isize) {
    let Some(panel) = app.session.home_risk_panel.as_mut() else {
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
    let selected_workspace = app.session.home_risk_panel.as_ref().and_then(|panel| {
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
    app.session.home_risk_pending = false;
    app.session.home_risk_panel = None;
    let message = app.session.input.trim_end().to_string();
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
    app.session.workspace_root = active_root.clone();
    app.session.session_cwd = active_root;
    Ok(())
}

fn set_home_risk_notice(app: &mut App, error: String) {
    if let Some(panel) = app.session.home_risk_panel.as_mut() {
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
            app.session.home_risk_panel = None;
        }
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.session.home_risk_panel = None;
        }
        _ => {}
    }
    false
}

fn should_open_home_risk_panel(app: &App, input: &str) -> bool {
    app.session.home_risk_pending
        && app.session.active_session.is_none()
        && !input.trim().is_empty()
}

fn handle_enter(app: &mut App) -> bool {
    if app.model_credential_prompt.is_some() {
        if let Err(error) = submit_model_credential(app) {
            show_message(app, error);
        }
        return false;
    }
    let input_owned = app.session.input.trim_end().to_string();
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
            clear_composer(app);
            insert_text_at_cursor(app, "/");
            app.session.show_help = true;
            app.session.message = None;
            app.session.selected_command = 0;
            false
        }
        Some("/exit") => prepare_exit(app),
        Some("/resume") => {
            match session_picker::open(app, slash_command_args(input)) {
                Ok(()) => clear_composer(app),
                Err(error) => show_message(app, error),
            }
            false
        }
        Some("/dashboard" | "/sessions") => {
            clear_composer(app);
            dashboard::toggle(app);
            false
        }
        Some("/settings" | "/config") => {
            clear_composer(app);
            app.session.message = None;
            settings::open(app);
            false
        }
        Some("/new" | "/clear") => {
            session_views::new_session(app);
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
        Some("/stop") => {
            clear_composer(app);
            if let Err(error) = stop_controlled_tasks(app, "tui_user_stop") {
                show_message(app, error);
            }
            false
        }
        Some(name) => {
            app.session.message = Some(format!("Unknown slash command: {name}"));
            false
        }
    }
}

fn handle_question_enter(app: &mut App) -> bool {
    let input_owned = app.session.input.trim_end().to_string();
    let input = input_owned.as_str();
    if input == "/exit" {
        return prepare_exit(app);
    }
    if matches!(
        slash_command_name(input),
        Some("/new" | "/clear" | "/resume" | "/dashboard" | "/sessions")
    ) {
        return handle_enter(app);
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
        cancel_model_credential(app);
        return false;
    }
    if app.model_panel.is_some() {
        close_model_panel(app);
        return false;
    }
    if app.session.show_help || app.session.message.is_some() {
        app.session.show_help = false;
        app.session.message = None;
        return false;
    }
    if !app.session.input.is_empty() {
        clear_composer(app);
        app.session.selected_command = 0;
        return false;
    }
    if has_active_agent_run(&app.session) {
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

fn has_active_agent_run(state: &SessionState) -> bool {
    state.active_agent_run_id.is_some() || !state.active_agent_run_ids.is_empty()
}

fn periodic_redraw_due(app: &App, last_draw_at: Instant, now: Instant) -> bool {
    (has_active_agent_run(&app.session)
        || session_picker::has_working_row(app)
        || dashboard::has_working_session(app))
        && now.saturating_duration_since(last_draw_at) >= STATUS_REDRAW_INTERVAL
}

fn stop_active_agent_runs(app: &mut App, reason: &str) -> Result<(), String> {
    let task_ids = active_agent_run_ids(&app.session);
    if task_ids.is_empty() {
        return Err("No running task to stop".to_string());
    }
    let session_id = app
        .session
        .active_session
        .as_ref()
        .map(|session| session.id.clone())
        .ok_or_else(|| "No active session to stop".to_string())?;
    ensure_runtime(app)?;
    for task_id in task_ids {
        cancel_agent_run(app, session_id.as_str(), task_id.as_str(), reason)?;
    }
    // A cancellation receipt is not a terminal event. Keep the run and its
    // tool observations until the Runtime publishes the final outcome.
    Ok(())
}

fn stop_controlled_tasks(app: &mut App, reason: &str) -> Result<(), String> {
    if has_active_agent_run(&app.session) {
        stop_active_agent_runs(app, reason)?;
    }
    Ok(())
}

fn active_agent_run_ids(state: &SessionState) -> Vec<String> {
    let mut agent_run_ids = state
        .active_agent_run_ids
        .iter()
        .cloned()
        .collect::<Vec<_>>();
    if let Some(agent_run_id) = state.active_agent_run_id.as_ref() {
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
        .session_request(
            SessionCommand::Cancel,
            json!({
                "agentRunId": agent_run_id,
                "sessionId": session_id,
                "reason": reason,
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
    let cancel_accepted = response
        .get("cancelAccepted")
        .and_then(Value::as_bool)
        .ok_or_else(|| {
            "_centaeris/session/agent-runs/cancel response missing boolean cancelAccepted"
                .to_string()
        })?;
    if !cancel_accepted && !is_terminal_status(status) {
        return Err(format!(
            "_centaeris/session/agent-runs/cancel did not cancel active AgentRun: {agent_run_id}"
        ));
    }
    if is_terminal_status(status) {
        // A terminal receipt may arrive without the final transcript update.
        // Reuse the authoritative history projection instead of sealing the
        // currently visible partial text as a fabricated cancellation.
        refresh_current_session_observation(app)?;
    }
    Ok(())
}

fn show_message(app: &mut App, message: impl Into<String>) {
    app.session.message = Some(message.into());
    app.session.show_help = false;
    app.session.home_risk_panel = None;
    app.session.selected_command = 0;
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
                    let thinking_modes = model
                        .get("modelThinkingModes")
                        .and_then(Value::as_array)
                        .ok_or_else(|| "available model missing modelThinkingModes".to_string())?
                        .iter()
                        .map(|mode| {
                            mode.as_str().map(str::to_owned).ok_or_else(|| {
                                "modelThinkingModes must contain strings".to_string()
                            })
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    let mode = |field| -> Result<Option<String>, String> {
                        match model.get(field) {
                            Some(Value::Null) => Ok(None),
                            Some(Value::String(value)) => Ok(Some(value.clone())),
                            _ => Err(format!("available model missing or invalid {field}")),
                        }
                    };
                    Ok(TuiModelChoice {
                        model: model_id.to_string(),
                        display_name: model
                            .get("displayName")
                            .and_then(Value::as_str)
                            .unwrap_or(model_id)
                            .to_string(),
                        thinking_modes,
                        default_thinking_mode: mode("modelThinkingMode")?,
                        preferred_thinking_mode: mode("preferredThinkingMode")?,
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
        stage: model_picker::Stage::Models,
        query: String::new(),
    })
}

fn open_model_panel(app: &mut App, response: &Value) -> Result<(), String> {
    let panel = model_panel_from_config(response)?;
    app.model_catalog = panel.providers.clone();
    app.model_panel = Some(panel);
    app.model_provider_hit_regions.clear();
    app.model_list_area = None;
    app.model_list_offset = 0;
    app.session.message = None;
    app.session.show_help = false;
    app.session.selected_command = 0;
    Ok(())
}

fn close_model_panel(app: &mut App) {
    if app.pending_model_request.is_some() {
        return;
    }
    app.model_panel = None;
    app.model_provider_hit_regions.clear();
    app.model_list_area = None;
    app.model_list_offset = 0;
    app.session.message = None;
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
    panel.stage = model_picker::Stage::Models;
    panel.query.clear();
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
    app.session.message = None;
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
    model_picker::move_selection(app, delta);
}

fn begin_model_credential(app: &mut App, provider_id: String, provider_name: String) {
    app.model_saved_composer = Some(editing::ComposerDraft::take(app));
    app.model_saved_panel = app.model_panel.take();
    app.model_provider_hit_regions.clear();
    app.model_list_area = None;
    app.model_credential_prompt = Some(ModelCredentialPrompt {
        provider_id,
        provider_name,
    });
    app.session.message = None;
}

fn restore_model_composer(app: &mut App) {
    if let Some(draft) = app.model_saved_composer.take() {
        draft.restore(app);
    } else {
        clear_composer(app);
    }
    app.model_panel = app.model_saved_panel.take();
}

fn cancel_model_credential(app: &mut App) {
    app.model_credential_prompt = None;
    restore_model_composer(app);
    app.session.message = None;
}

fn model_mutation_failed(app: &mut App, mutation: &ModelMutation, error: impl std::fmt::Display) {
    if matches!(mutation, ModelMutation::Credential { .. }) {
        restore_model_composer(app);
    }
    show_message(app, error.to_string());
}

fn request_model_selection(
    app: &mut App,
    provider_id: String,
    model: String,
    thinking_mode: Option<String>,
) -> Result<(), String> {
    if app.pending_model_request.is_some() {
        return Err("Model configuration request is already in progress".into());
    }
    let mut request = json!({ "modelProviderId": provider_id, "model": model });
    if let Some(mode) = thinking_mode {
        request["modelThinkingMode"] = Value::String(mode);
    }
    let response = app
        .runtime
        .as_mut()
        .ok_or_else(|| "runtime client is not connected".to_string())?
        .request_async("agent_runtime_config_set", json!({"request": request}))?;
    app.pending_model_request = Some(PendingModelRequest::Setting {
        response,
        mutation: ModelMutation::Selection,
    });
    app.session.message = Some("Applying model and reasoning effort…".to_string());
    Ok(())
}

fn activate_selected_model(app: &mut App) -> Result<(), String> {
    model_picker::activate(app)
}

fn handle_model_panel_key(key: KeyEvent, app: &mut App) -> bool {
    if app.pending_model_request.is_some() {
        return false;
    }
    match key.code {
        KeyCode::Esc => model_picker::back(app),
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            close_model_panel(app)
        }
        KeyCode::Tab
            if app
                .model_panel
                .as_ref()
                .is_some_and(|panel| panel.stage == model_picker::Stage::Models) =>
        {
            move_model_provider(app, 1)
        }
        KeyCode::BackTab
            if app
                .model_panel
                .as_ref()
                .is_some_and(|panel| panel.stage == model_picker::Stage::Models) =>
        {
            move_model_provider(app, -1)
        }
        KeyCode::Up => move_model_selection(app, -1),
        KeyCode::Down => move_model_selection(app, 1),
        KeyCode::Enter => {
            if let Err(error) = activate_selected_model(app) {
                show_message(app, error);
            }
        }
        KeyCode::Backspace => model_picker::filter(app, None),
        KeyCode::Char(ch) if editing::accepts_text(key) => model_picker::filter(app, Some(ch)),
        _ => {}
    }
    false
}

fn submit_model_credential(app: &mut App) -> Result<(), String> {
    let prompt = app
        .model_credential_prompt
        .clone()
        .ok_or_else(|| "model credential prompt is not active".to_string())?;
    let api_key = model_api_key_input(app.session.input.as_str())?;
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
            provider_id: prompt.provider_id,
            provider_name: prompt.provider_name,
        },
    });
    app.model_credential_prompt = None;
    clear_composer(app);
    app.session.input_cursor = 0;
    app.session.message = None;
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

fn slash_command_args(input: &str) -> &str {
    let Some(command) = slash_command_name(input) else {
        return "";
    };
    input.get(command.len()..).unwrap_or_default().trim()
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
        "operationId": operation_id,
        "sessionId": session.id,
        "message": message,
        "attachments": attachments.into_iter().map(|attachment| json!({
            "placeholder": &message[attachment.start..attachment.end],
            "localPath": attachment.local_path.to_string_lossy(),
        })).collect::<Vec<_>>(),
    })
}

fn start_prompt(app: &mut App, message: String) -> Result<(), String> {
    let message = message.trim_end().to_string();
    if message.trim().is_empty() {
        return Ok(());
    }
    if app.session.pending_history.is_some() {
        return dashboard::queue_loading_prompt(app, message);
    }
    if app.session.active_agent_run_id.is_some() {
        if !app.session.draft_image_attachments.is_empty() {
            return Err("Images cannot be added while an AgentRun is active".to_string());
        }
        return send_supplement(app, message);
    }

    commit_assistant_buffer(&mut app.session);
    ensure_runtime(app)?;
    let session = ensure_active_session(app, message.as_str())?;
    let operation_id = new_runtime_operation_id()?;
    let result = app
        .runtime
        .as_mut()
        .ok_or_else(|| "runtime client is not connected".to_string())?
        .session_request(
            SessionCommand::Send,
            build_prompt_request(
                &session,
                message.as_str(),
                app.session.draft_image_attachments.as_slice(),
                operation_id.as_str(),
            ),
        )?;
    apply_prompt_started(&mut app.session, message, &result)?;
    app.session.transcript_follow_bottom = true;
    for attachment in app.session.draft_image_attachments.drain(..) {
        let _ = std::fs::remove_file(attachment.local_path);
    }
    clear_composer(app);
    app.session.input_cursor = 0;
    app.session.message = None;
    Ok(())
}

fn apply_prompt_started(
    state: &mut SessionState,
    message: String,
    result: &Value,
) -> Result<(), String> {
    let agent_run_id = result
        .get("agentRunId")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| "session/prompt response missing agentRunId".to_string())?;
    state.transcript.push(TranscriptLine::User(message));
    state.process_state = RuntimeDisplayState::PendingPhase;
    state.active_agent_run_id = Some(agent_run_id.clone());
    state.active_agent_run_ids.clear();
    state.active_agent_run_ids.insert(agent_run_id);
    state.completed_agent_run_ids.clear();
    state.seen_subagent_ids.clear();
    state.live_subagent_ids.clear();
    state.status_easter_egg = StatusEasterEgg::Available;
    state.agent_run_started_at = Some(Instant::now());
    invalidate_transcript_layout(state);
    if let Some(items) = result.get("streamItems").and_then(Value::as_array) {
        for item in items {
            apply_stream_payload(state, item);
        }
    }
    Ok(())
}

fn send_supplement(app: &mut App, message: String) -> Result<(), String> {
    let session = app
        .session
        .active_session
        .clone()
        .ok_or_else(|| "Cannot supplement without an active session".to_string())?;
    let active_agent_run_id = app
        .session
        .active_agent_run_id
        .clone()
        .ok_or_else(|| "Cannot supplement without an active AgentRun".to_string())?;

    commit_assistant_buffer(&mut app.session);
    let result = app
        .runtime
        .as_mut()
        .ok_or_else(|| "runtime client is not connected".to_string())?
        .session_request(
            SessionCommand::Supplement,
            json!({
                "sessionId": session.id.clone(),
                "agentRunId": active_agent_run_id.clone(),
                "message": message,
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
    app.session.message = None;
    app.session.transcript_follow_bottom = true;
    app.session.process_state = RuntimeDisplayState::PendingPhase;
    update_status_easter_egg(&mut app.session);
    if app.session.agent_run_started_at.is_none() {
        app.session.agent_run_started_at = Some(Instant::now());
    }
    if let Some(items) = result.get("streamItems").and_then(Value::as_array) {
        for item in items {
            apply_stream_payload(&mut app.session, item);
        }
    }
    Ok(())
}

fn ensure_active_session(app: &mut App, title: &str) -> Result<TuiSession, String> {
    if let Some(session) = app.session.active_session.clone() {
        return Ok(session);
    }
    let operation_id = new_runtime_operation_id()?;
    let runtime = app
        .runtime
        .as_mut()
        .ok_or_else(|| "runtime client is not connected".to_string())?;
    let response = runtime.session_request(
        SessionCommand::Create,
        build_session_create_request(
            title,
            app.session.session_cwd.as_str(),
            operation_id.as_str(),
        ),
    )?;
    let session = tui_session_from_response(&response)?;
    if !runtime.claim_session_writer(&session.id)? {
        return Err("New session is already open in another window".into());
    }
    app.session.active_session = Some(session.clone());
    app.session.catalog_entry = Some(session_picker::SessionEntry {
        id: session.id.clone(),
        title: response
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or(title)
            .to_owned(),
        cwd: app.session.session_cwd.clone(),
        is_pinned: false,
        updated_at: response
            .get("updatedAt")
            .and_then(Value::as_i64)
            .unwrap_or(0),
    });
    Ok(session)
}

fn build_session_create_request(title: &str, cwd: &str, operation_id: &str) -> Value {
    json!({
        "operationId": operation_id,
        "title": title,
        "cwd": cwd,
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
    if app.runtime.is_none() {
        return Err("Local runtime is unavailable".to_string());
    }
    Ok(())
}

fn refresh_current_session_observation(app: &mut App) -> Result<(), String> {
    // Runtime requests are synchronous on the TUI thread: the selected session
    // cannot change between obtaining this snapshot and applying it.
    let Some(session) = app.session.active_session.clone() else {
        app.session.process_state = RuntimeDisplayState::Idle;
        return Ok(());
    };
    let restore = match load_session_history(app, &session) {
        Ok(restore) => restore,
        Err(error) => {
            app.runtime = None;
            app.session.process_state = RuntimeDisplayState::ConnectionLost;
            return Err(error);
        }
    };
    app.session.transcript = restore.transcript;
    app.session.transcript_history_len = app.session.transcript.len();
    app.session.transcript_paging = Some(restore.transcript_paging);
    session_transcript::reset_history_images(&mut app.session);
    clear_assistant_buffer(&mut app.session);
    app.session.pending_question = None;
    restore_active_agent_runs(&mut app.session, restore.active_agent_runs);
    status_usage::restore(app);
    for item in restore.replay_items {
        apply_stream_payload(&mut app.session, &item);
    }
    invalidate_transcript_layout(&mut app.session);
    Ok(())
}

fn tui_viewer_id() -> String {
    format!("tui-{}", process::id())
}

fn start_model_command(app: &mut App, command: ModelCommand) -> Result<(), String> {
    match app.pending_model_request.as_mut() {
        Some(PendingModelRequest::LoadingConfig {
            command: pending_command,
            ..
        }) if matches!(&*pending_command, ModelCommand::Refresh) => {
            *pending_command = command;
            return Ok(());
        }
        Some(_) => {
            return Err("Model configuration request is already in progress".to_string());
        }
        None => {}
    }
    request_model_configuration(app, command)
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
        PendingModelRequest::LoadingConfig { response, command } => match response.try_recv() {
            Ok(Some(config)) => {
                let open_picker = !matches!(command, ModelCommand::SettingsList)
                    || settings::take_model_request(app);
                match model_panel_from_config(&config) {
                    Ok(panel) => app.model_catalog = panel.providers,
                    Err(error) => {
                        if open_picker {
                            show_message(app, error);
                        }
                        return open_picker;
                    }
                }
                apply_model_display(app, &config);
                match command {
                    ModelCommand::Refresh => {
                        apply_model_display(app, &config);
                        if app.model_panel.is_some() {
                            if let Err(error) = open_model_panel(app, &config) {
                                show_message(app, error);
                            }
                        }
                    }
                    ModelCommand::SettingsList if !open_picker => {}
                    ModelCommand::List | ModelCommand::SettingsList => {
                        apply_model_display(app, &config);
                        match open_model_panel(app, &config) {
                            Ok(()) => {}
                            Err(error) => show_message(app, error),
                        }
                    }
                    ModelCommand::Effort(None) => {
                        if let Err(error) = model_picker::open_effort(app, &config) {
                            show_message(app, error);
                        }
                    }
                    ModelCommand::Effort(Some(mode)) => {
                        let result = app
                        .runtime
                        .as_mut()
                        .ok_or_else(|| "runtime client is not connected".to_string())
                        .and_then(|runtime| {
                            runtime.request_async(
                                "agent_runtime_config_set",
                                json!({ "request": {
                                    "modelProviderId": config.get("modelProviderId").and_then(Value::as_str).ok_or("No active model")?,
                                    "model": config.get("model").and_then(Value::as_str).ok_or("No active model")?,
                                    "modelThinkingMode": mode
                                } }),
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
                    ModelCommand::Select(query) => match model_picker::resolve_choice(app, &query)
                        .or_else(|error| {
                            resolve_model_request(&config, &query)
                                .map(|(provider, model)| (provider, model, None))
                                .map_err(|_| error)
                        }) {
                        Ok((provider_id, model, mode)) => {
                            if let Err(error) =
                                request_model_selection(app, provider_id, model, mode)
                            {
                                show_message(app, error);
                            }
                        }
                        Err(model_error) => match resolve_model_provider_request(&config, &query) {
                            Ok(Some((prompt, false))) => {
                                begin_model_credential(
                                    app,
                                    prompt.provider_id,
                                    prompt.provider_name,
                                );
                            }
                            Ok(Some((_, true))) | Ok(None) => show_message(app, model_error),
                            Err(error) => show_message(app, error),
                        },
                    },
                }
            }
            Ok(None) => {
                app.pending_model_request =
                    Some(PendingModelRequest::LoadingConfig { response, command });
                return false;
            }
            Err(error) => {
                if !matches!(command, ModelCommand::SettingsList)
                    || settings::take_model_request(app)
                {
                    show_message(app, error);
                }
            }
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
                    Err(error) => model_mutation_failed(app, &mutation, error),
                }
            }
            Ok(None) => {
                app.pending_model_request =
                    Some(PendingModelRequest::Setting { response, mutation });
                return false;
            }
            Err(error) => model_mutation_failed(app, &mutation, error),
        },
        PendingModelRequest::Confirming { response, mutation } => {
            match response.try_recv() {
                Ok(Some(config)) => {
                    match model_panel_from_config(&config) {
                        Ok(panel) => app.model_catalog = panel.providers,
                        Err(error) => {
                            model_mutation_failed(app, &mutation, error);
                            return true;
                        }
                    }
                    apply_model_display(app, &config);
                    match mutation {
                        ModelMutation::Selection => {
                            app.model_panel = None;
                            let current =
                                app.model_display.as_deref().unwrap_or("<not configured>");
                            let effort = app.model_effort.as_deref().unwrap_or("provider default");
                            let suffix = if has_active_agent_run(&app.session) {
                                "; next run only; current run keeps its configuration"
                            } else {
                                ""
                            };
                            show_message(
                                app,
                                format!("Model: {current} · reasoning: {effort}{suffix}"),
                            );
                        }
                        ModelMutation::Credential {
                            provider_id,
                            provider_name,
                        } => {
                            restore_model_composer(app);
                            if let Err(error) = open_model_panel(app, &config) {
                                show_message(app, error);
                            } else {
                                if let Some(index) = app.model_panel.as_ref().and_then(|panel| {
                                    panel
                                        .providers
                                        .iter()
                                        .position(|provider| provider.provider_id == provider_id)
                                }) {
                                    select_model_provider(app, index);
                                }
                                show_message(app, format!("{provider_name} configured; choose model and reasoning effort"));
                            }
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
                Err(error) => model_mutation_failed(app, &mutation, error),
            }
        }
    }
    true
}

fn apply_model_display(app: &mut App, response: &Value) {
    let provider_id = response.get("modelProviderId").and_then(Value::as_str);
    let model = response.get("model").and_then(Value::as_str);
    let model_changed = app.model_provider_id.as_deref() != provider_id
        || app.model_display.as_deref() != model.or(provider_id);
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
    let window = response.get("modelContextTokens").and_then(Value::as_u64);
    app.model_context_window = window.filter(|window| *window > 0);
    if model_changed {
        app.session.status_usage.record_context(None);
    }
}

fn submit_question_answer(app: &mut App, answer_text: String) -> Result<(), String> {
    let answer_text = answer_text.trim().to_string();
    if answer_text.is_empty() {
        return Err("question answer cannot be empty".to_string());
    }
    let question = app
        .session
        .pending_question
        .clone()
        .ok_or_else(|| "No pending question".to_string())?;
    let session = app
        .session
        .active_session
        .clone()
        .ok_or_else(|| "Cannot answer question without an active session".to_string())?;
    ensure_runtime(app)?;
    let result = app
        .runtime
        .as_mut()
        .ok_or_else(|| "runtime client is not connected".to_string())?
        .session_request(
            SessionCommand::AnswerQuestion,
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
    app.session.pending_question = None;
    if app.session.active_agent_run_id.as_deref() != Some(agent_run_id.as_str()) {
        app.session.status_easter_egg = StatusEasterEgg::Available;
    }
    app.session.active_agent_run_id = Some(agent_run_id.clone());
    app.session.active_agent_run_ids.clear();
    app.session.active_agent_run_ids.insert(agent_run_id);
    app.session.seen_subagent_ids.clear();
    app.session.live_subagent_ids.clear();
    clear_composer(app);
    app.session.message = None;
    app.session.process_state = RuntimeDisplayState::PendingPhase;
    update_status_easter_egg(&mut app.session);
    if app.session.agent_run_started_at.is_none() {
        app.session.agent_run_started_at = Some(Instant::now());
    }
    if let Some(items) = result.get("streamItems").and_then(Value::as_array) {
        for item in items {
            apply_stream_payload(&mut app.session, item);
        }
    }
    Ok(())
}

fn question_answer_request(session_id: &str, question_id: &str, answer_text: &str) -> Value {
    json!({
        "sessionId": session_id,
        "questionId": question_id,
        "answerText": answer_text,
        "answers": [],
    })
}

fn tui_session_from_response(value: &Value) -> Result<TuiSession, String> {
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "session response missing id".to_string())?;
    Ok(TuiSession { id: id.to_owned() })
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
        match event {
            RuntimeEvent::SessionUpdate(update) => {
                changed |= session_views::route_update(app, &update);
            }
            RuntimeEvent::RuntimeConfigChanged => {
                app.runtime_config_refresh_pending = true;
                changed = true;
            }
            RuntimeEvent::Error(error) => {
                app.session.transcript.push(TranscriptLine::Error(error));
                app.runtime = None;
                // Connection loss says nothing about the run or its tools.
                app.session.process_state = RuntimeDisplayState::ConnectionLost;
                update_status_easter_egg(&mut app.session);
                invalidate_transcript_layout(&mut app.session);
                changed = true;
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

fn apply_session_update(state: &mut SessionState, update: &Value) {
    let Some(session) = state.active_session.as_ref() else {
        return;
    };
    if update.get("sessionId").and_then(Value::as_str) != Some(session.id.as_str()) {
        return;
    }
    let update_agent_run_id = update.get("agentRunId").and_then(Value::as_str);
    if state.active_agent_run_id.is_none()
        && state.active_agent_run_ids.is_empty()
        && matches!(
            update
                .pointer("/payload/event/type")
                .and_then(Value::as_str),
            Some("AgentRunStarted" | "ModelRequestStart")
        )
    {
        if let Some(id) = update_agent_run_id.filter(|id| !id.is_empty()) {
            state.active_agent_run_id = Some(id.to_string());
            state.active_agent_run_ids.insert(id.to_string());
            state.agent_run_started_at = Some(Instant::now());
            state.process_state = RuntimeDisplayState::PendingPhase;
            state.status_easter_egg = StatusEasterEgg::Available;
        }
    }
    if !state.active_agent_run_ids.is_empty() {
        if !update_agent_run_id
            .is_some_and(|agent_run_id| state.active_agent_run_ids.contains(agent_run_id))
        {
            let expected = state
                .active_agent_run_ids
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            state.transcript.push(TranscriptLine::Error(format!(
                "session/update agentRunId mismatch: expected one of [{expected}], got {}",
                update_agent_run_id.unwrap_or("<missing>")
            )));
            return;
        }
    } else if let Some(active_agent_run_id) = state.active_agent_run_id.as_deref() {
        if update_agent_run_id != Some(active_agent_run_id) {
            state.transcript.push(TranscriptLine::Error(format!(
                "session/update agentRunId mismatch: expected {active_agent_run_id}, got {}",
                update_agent_run_id.unwrap_or("<missing>")
            )));
            return;
        }
    }
    let Some(payload) = update.get("payload") else {
        state.transcript.push(TranscriptLine::Error(
            "session/update missing payload".to_string(),
        ));
        return;
    };
    apply_stream_payload_for_agent_run(state, payload, update_agent_run_id);
}

fn apply_stream_payload(state: &mut SessionState, payload: &Value) {
    apply_stream_payload_for_agent_run(
        state,
        payload,
        payload.get("agentRunId").and_then(Value::as_str),
    );
}

fn apply_stream_payload_for_agent_run(
    state: &mut SessionState,
    payload: &Value,
    agent_run_id: Option<&str>,
) {
    match payload.get("type").and_then(Value::as_str) {
        Some(kind @ ("runtime_event" | "session_event")) => {
            if let Some(event) = payload.get("event") {
                // Bash's committed session events already carry the call and its result.
                // Runtime summaries arrive later and can reopen the settled row under a new ID.
                if kind == "runtime_event"
                    && event.get("toolName").and_then(Value::as_str) == Some("bash")
                    && matches!(
                        event.get("type").and_then(Value::as_str),
                        Some("ToolCall" | "ToolResult")
                    )
                {
                    return;
                }
                apply_session_event(state, event, agent_run_id);
            } else {
                state.transcript.push(TranscriptLine::Error(
                    "runtime/session event payload missing event".to_string(),
                ));
            }
        }
        Some("error") => {
            let message = payload
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("runtime error");
            state
                .transcript
                .push(TranscriptLine::Error(message.to_string()));
            finish_active_agent_run(state);
        }
        Some(other) => state.transcript.push(TranscriptLine::Error(format!(
            "unsupported stream payload: {other}"
        ))),
        None => state.transcript.push(TranscriptLine::Error(
            "stream payload missing type".to_string(),
        )),
    }
}

fn apply_session_event(state: &mut SessionState, event: &Value, agent_run_id: Option<&str>) {
    state.presentation_run_id = agent_run_id.map(str::to_string);
    apply_session_event_inner(state, event, agent_run_id);
    state.presentation_run_id = None;
    update_status_easter_egg(state);
}

fn apply_session_event_inner(state: &mut SessionState, event: &Value, agent_run_id: Option<&str>) {
    let Some(event_type) = event.get("type").and_then(Value::as_str) else {
        state.transcript.push(TranscriptLine::Error(
            "session_event missing event.type".to_string(),
        ));
        return;
    };
    let Some(event_id) = event.get("id").and_then(Value::as_str) else {
        state.transcript.push(TranscriptLine::Error(format!(
            "{event_type} missing event.id"
        )));
        return;
    };
    if !state.seen_stream_event_ids.insert(event_id.to_string()) {
        return;
    }
    if matches!(
        event_type,
        "AgentRunCompleted" | "AgentRunFailed" | "AgentRunInterrupted"
    ) {
        let reason_type = event.pointer("/payload/reasonType").and_then(Value::as_str);
        if event_type == "AgentRunInterrupted" {
            let reason_type = reason_type.unwrap_or_default();
            if !matches!(
                reason_type,
                "cancelled" | "stopped" | "shutdown" | "provider_interrupted"
            ) {
                state.transcript.push(TranscriptLine::Error(format!(
                    "AgentRunInterrupted has unsupported reasonType: {}",
                    if reason_type.is_empty() {
                        "<missing>"
                    } else {
                        reason_type
                    }
                )));
                finish_active_agent_run(state);
                return;
            }
        }
        if let Some(agent_run_id) = agent_run_id {
            mark_agent_run_terminal(state, agent_run_id);
            state.transcript.retain(|item| !matches!(item, TranscriptLine::RunItem { run_id, line, .. }
                if run_id == agent_run_id && matches!(line.content(), TranscriptLine::RunFailure { .. })));
            state.transcript.push(TranscriptLine::RunBoundary {
                run_id: agent_run_id.into(),
                state: match event_type {
                    "AgentRunCompleted" => runs::RunState::Completed,
                    "AgentRunFailed" => runs::RunState::Failed,
                    _ => runs::RunState::interrupted(reason_type),
                },
                reason: run_errors::detail(
                    event
                        .pointer("/payload/message")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                    reason_type,
                )
                .into(),
            });
        } else {
            state.transcript.push(TranscriptLine::Error(format!(
                "{event_type} missing stream agentRunId"
            )));
            finish_active_agent_run(state);
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
    runs::record_activity(state, event, agent_run_id);
    let is_primary_status = state
        .active_agent_run_id
        .as_deref()
        .is_none_or(|primary| agent_run_id.is_none_or(|run_id| run_id == primary));
    if let Some(process_state) = event
        .get("processState")
        .and_then(Value::as_str)
        .filter(|_| is_primary_status && !event_type.starts_with("Subagent"))
    {
        state.process_state = runtime_display_state(process_state);
    }
    let is_tool_flow_event = matches!(
        event_type,
        "ToolCall" | "ToolResult" | "ToolCallPreparing" | "ToolCallReady" | "ToolProgress"
    );
    if !is_tool_flow_event && !event_type.starts_with("Subagent") {
        seal_active_tool_calls(state);
        state.active_tool_label = None;
    }
    match event_type {
        "AgentRunStarted" => {}
        "ModelRequestStart" => {
            let purpose = payload.get("purpose").and_then(Value::as_str);
            let context_token_estimate =
                payload.get("contextTokenEstimate").and_then(Value::as_u64);
            let initial_content = payload.get("initialContent").and_then(Value::as_str);
            if !purpose.is_some_and(|purpose| matches!(purpose, "main" | "compaction"))
                || context_token_estimate.is_none()
                || initial_content.is_none()
            {
                state.transcript.push(TranscriptLine::Error(
                    "ModelRequestStart requires purpose, contextTokenEstimate, and initialContent"
                        .to_string(),
                ));
                return;
            }
            replace_assistant_buffer(state, initial_content.unwrap_or_default().to_string());
            if is_primary_status {
                if purpose == Some("compaction") {
                    state.process_state = RuntimeDisplayState::Compacting;
                } else {
                    state.status_usage.record_context(context_token_estimate);
                    state
                        .status_usage
                        .request_output(state.active_agent_run_id.is_some());
                }
            }
        }
        "PromptCompaction" => {
            if is_primary_status && state.process_state == RuntimeDisplayState::Compacting {
                state.process_state = RuntimeDisplayState::PendingPhase;
            }
        }
        "Reasoning" | "ReasoningBlock" => {
            if let Err(error) = reasoning::apply_reasoning(state, payload) {
                state.transcript.push(TranscriptLine::Error(error));
            }
        }
        "ModelSnapshot" => {
            if let Err(error) = reasoning::apply_snapshot(state, event, agent_run_id) {
                state.transcript.push(TranscriptLine::Error(error));
            } else if is_primary_status
                && !state.assistant_buffer.trim().is_empty()
                && has_active_agent_run(state)
                && state.process_state != RuntimeDisplayState::Compacting
            {
                state.process_state = RuntimeDisplayState::Responding;
            }
        }
        "ModelStatus" => {}
        "ModelTextDelta" => {
            if let Some(delta) = raw_payload_string(payload, "delta") {
                if is_primary_status
                    && !delta.trim().is_empty()
                    && state.process_state != RuntimeDisplayState::Compacting
                {
                    state.process_state = RuntimeDisplayState::Responding;
                }
                state.assistant_run_id = agent_run_id.map(str::to_string);
                state.assistant_buffer.push_str(delta.as_str());
            }
        }
        "ModelTextReplace" => {
            let content = raw_payload_string(payload, "content").unwrap_or_default();
            if is_primary_status
                && !content.trim().is_empty()
                && state.process_state != RuntimeDisplayState::Compacting
            {
                state.process_state = RuntimeDisplayState::Responding;
            }
            replace_assistant_buffer(state, content);
        }
        "Final" => {
            for line in state
                .transcript
                .iter_mut()
                .rev()
                .take_while(|line| !matches!(line, TranscriptLine::User(_)))
            {
                if let TranscriptLine::Reasoning { final_started, .. } = line {
                    *final_started = true;
                }
            }
            if let Some(content) = raw_payload_string(payload, "content") {
                replace_assistant_buffer(state, content);
            }
            state.assistant_run_id = agent_run_id.map(str::to_string);
            state.assistant_is_final = true;
            if let Some(start) = state.assistant_stream_start {
                for line in &mut state.transcript[start..] {
                    if let TranscriptLine::RunItem { final_answer, .. } = line {
                        *final_answer = true;
                    }
                }
            }
        }
        "Status" => {
            if payload.get("stage").and_then(Value::as_str) == Some("model_process_summary") {
                if let Some(message) = raw_payload_string(payload, "message") {
                    discard_assistant_stream(state);
                    state
                        .transcript
                        .push(TranscriptLine::Summary(message).for_run(agent_run_id, false));
                }
            }
        }
        "TurnSupplement" => {
            let Some(message) = raw_payload_string(payload, "message") else {
                state.transcript.push(TranscriptLine::Error(
                    "TurnSupplement missing payload.message".to_string(),
                ));
                return;
            };
            commit_assistant_buffer(state);
            state.transcript.push(TranscriptLine::Supplement(message));
        }
        "ToolCall" | "ToolResult" => apply_tool_event(state, event_type, event, payload),
        "ToolCallPreparing" | "ToolCallReady" | "ToolProgress" => {
            apply_tool_activity(state, event_type, event)
        }
        "SubagentSpawned" | "SubagentProgress" | "SubagentToolGroup" | "SubagentResult"
        | "SubagentFailed" | "SubagentCancelled" => {
            let Some(subagent_id) = payload.get("subagentId").and_then(Value::as_str) else {
                state.transcript.push(TranscriptLine::Error(format!(
                    "{event_type} missing payload.subagentId"
                )));
                return;
            };
            let task_id = payload
                .get("runtimeJobId")
                .and_then(Value::as_str)
                .unwrap_or(subagent_id);
            if agent_run_id == state.active_agent_run_id.as_deref() {
                state.seen_subagent_ids.insert(subagent_id.to_string());
                if matches!(
                    event_type,
                    "SubagentResult" | "SubagentFailed" | "SubagentCancelled"
                ) {
                    state.live_subagent_ids.remove(task_id);
                    if state.live_subagent_ids.is_empty()
                        && state.process_state == RuntimeDisplayState::ToolRunning
                        && !state.tool_projection.has_open_calls()
                    {
                        state.process_state = RuntimeDisplayState::PendingPhase;
                    }
                } else {
                    state.live_subagent_ids.insert(task_id.to_string());
                    if state.process_state == RuntimeDisplayState::PendingPhase {
                        state.process_state = RuntimeDisplayState::ToolRunning;
                    }
                }
            }
            if let Some(line) = subagent_transcript_line(event_type, payload) {
                let line = TranscriptLine::Subagent(line).for_run(agent_run_id, false);
                if !state.tool_projection.has_open_calls() {
                    state.transcript.push(line);
                } else {
                    state.pending_subagent_lines.push(line);
                }
            }
        }
        "RuntimeError" | "Error" => {
            let message = raw_payload_string(payload, "message")
                .unwrap_or_else(|| "runtime error".to_string());
            if event_type == "RuntimeError" {
                if let Some(run_id) = agent_run_id {
                    if state.transcript.iter().any(|line| matches!(line, TranscriptLine::RunBoundary { run_id: terminal_run, state, .. }
                        if terminal_run == run_id && state.is_terminal())) {
                        return;
                    }
                    // The Runtime sends this before its durable terminal event.
                    // Replace provisional copy without closing that observation.
                    state.transcript.retain(|item| !matches!(item, TranscriptLine::RunItem { run_id: existing, line, .. }
                        if existing == run_id && matches!(line.content(), TranscriptLine::RunFailure { .. })));
                    state.transcript.push(
                        TranscriptLine::RunFailure {
                            key: format!("run:{run_id}"),
                            reason: message,
                        }
                        .for_run(Some(run_id), false),
                    );
                } else {
                    state.transcript.push(TranscriptLine::Error(message));
                    finish_active_agent_run(state);
                }
            } else {
                state.transcript.push(TranscriptLine::Error(message));
                finish_active_agent_run(state);
            }
        }
        "QuestionRequired" => match parse_pending_question(payload) {
            Ok(question) => {
                state.pending_question = Some(question);
                state.question_scroll = 0;
                clear_session_composer(state);
                state.process_state = RuntimeDisplayState::WaitingUser;
            }
            Err(error) => {
                state.transcript.push(TranscriptLine::Error(error));
            }
        },
        "RuntimeWaitChanged" if is_primary_status => {
            let reason = payload.get("transitionReason").and_then(Value::as_str);
            state.process_state = match (payload.get("status").and_then(Value::as_str), reason) {
                (Some("waiting"), Some("await_question")) => RuntimeDisplayState::WaitingUser,
                (Some("waiting"), _) if !state.live_subagent_ids.is_empty() => {
                    RuntimeDisplayState::Waiting(WaitReason::Tasks)
                }
                (Some("waiting"), _) => RuntimeDisplayState::Waiting(WaitReason::Unspecified),
                _ => RuntimeDisplayState::PendingPhase,
            };
        }
        "RuntimeWaitChanged" | "AgentRunInterventionChanged" | "SourceAssociation" | "Artifact" => {
        }
        other => state.transcript.push(TranscriptLine::Error(format!(
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

fn apply_tool_event(state: &mut SessionState, event_type: &str, event: &Value, payload: &Value) {
    let update = match state
        .tool_projection
        .apply_event(event_type, event, payload)
    {
        Ok(update) => update,
        Err(error) => {
            push_tool_protocol_error(state, event_type, event, error.as_str());
            return;
        }
    };
    if update.started.is_some() {
        commit_assistant_buffer(state);
    }
    if let Some(tool) = update.started {
        state
            .status_usage
            .request_output(state.active_agent_run_id.is_some());
        state
            .transcript
            .push(TranscriptLine::tool(tool).for_run(state.presentation_run_id.as_deref(), false));
    }
    if let Some(tool) = update.settled {
        settle_tool_line(state, tool);
    }
    state.active_tool_label = update.active_label;
    if state.presentation_run_id == state.active_agent_run_id {
        if state.active_tool_label.is_some() {
            state.process_state = RuntimeDisplayState::ToolRunning;
        } else if state.process_state == RuntimeDisplayState::ToolRunning {
            state.process_state = RuntimeDisplayState::PendingPhase;
        }
    }
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
    decode_tool_image(&bytes, expected)
}

fn decode_tool_image(bytes: &[u8], expected: &ToolImage) -> Result<image::DynamicImage, String> {
    let decoded_pixels = u64::from(expected.width_px).saturating_mul(u64::from(expected.height_px));
    let decoded_bytes = decoded_pixels.saturating_mul(4);
    if decoded_pixels > TOOL_IMAGE_MAX_DECODED_PIXELS
        || decoded_bytes > TOOL_IMAGE_MAX_DECODED_BYTES
    {
        return Err("ToolResult image exceeds the decoded image budget".to_string());
    }
    if bytes.len() as u64 != expected.byte_length
        || format!("sha256:{:x}", Sha256::digest(bytes)) != expected.sha256
    {
        return Err("Stored image bytes do not match the ToolResult image".to_string());
    }
    let decoded = image::load_from_memory(bytes)
        .map_err(|error| format!("decode tool image failed: {error}"))?;
    if decoded.dimensions() != (expected.width_px, expected.height_px) {
        return Err("Image dimensions do not match the ToolResult image".to_string());
    }
    Ok(decoded)
}

fn apply_tool_activity(state: &mut SessionState, event_type: &str, event: &Value) {
    match state.tool_projection.set_activity(event) {
        Ok(label) => state.active_tool_label = Some(label),
        Err(error) => push_tool_protocol_error(state, event_type, event, error.as_str()),
    }
}

fn seal_active_tool_calls(state: &mut SessionState) {
    for tool in state.tool_projection.seal() {
        settle_tool_line(state, tool);
    }
    state.active_tool_label = None;
}

fn settle_tool_line(state: &mut SessionState, tool: ToolTranscriptLine) {
    let Some(index) = state.transcript.iter().rposition(
        |line| matches!(line.content(), TranscriptLine::Tool(existing) if existing.key == tool.key),
    ) else {
        state.transcript.push(TranscriptLine::Error(format!(
            "Protocol error: settled tool call missing transcript row: {}",
            tool.key
        )));
        state.tool_protocol_error = true;
        return;
    };
    *state.transcript[index].content_mut() = TranscriptLine::tool(tool);
    if !state.pending_subagent_lines.is_empty() {
        state.transcript.append(&mut state.pending_subagent_lines);
    }
}

fn push_tool_protocol_error(
    state: &mut SessionState,
    event_type: &str,
    event: &Value,
    detail: &str,
) {
    let event_id = event
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("<missing event id>");
    state.transcript.push(TranscriptLine::Error(format!(
        "Protocol error: {event_type} {event_id} {detail}"
    )));
    state.tool_protocol_error = true;
}

fn mark_agent_run_terminal(state: &mut SessionState, agent_run_id: &str) {
    state
        .completed_agent_run_ids
        .insert(agent_run_id.to_string());
    if state.active_agent_run_ids.is_empty()
        || state
            .active_agent_run_ids
            .iter()
            .all(|agent_run_id| state.completed_agent_run_ids.contains(agent_run_id))
    {
        finish_active_agent_run(state);
    }
}

fn finish_active_agent_run(state: &mut SessionState) {
    seal_active_tool_calls(state);
    commit_assistant_buffer(state);
    state.active_agent_run_id = None;
    state.active_agent_run_ids.clear();
    state.completed_agent_run_ids.clear();
    state.seen_subagent_ids.clear();
    state.live_subagent_ids.clear();
    state.agent_run_started_at = None;
    state.tool_projection.clear();
    state.active_tool_label = None;
    state.pending_subagent_lines.clear();
    state.tool_protocol_error = false;
    state.status_easter_egg = StatusEasterEgg::Available;
    state.process_state = if state.pending_question.is_some() {
        RuntimeDisplayState::WaitingUser
    } else {
        RuntimeDisplayState::Idle
    };
}

fn fnv1a32(value: &str) -> u32 {
    value.as_bytes().iter().fold(0x811c9dc5, |hash, byte| {
        (hash ^ u32::from(*byte)).wrapping_mul(0x01000193)
    })
}

fn eligible_status_easter_egg(state: &SessionState) -> Option<EasterEgg> {
    if state.tool_protocol_error || state.pending_question.is_some() {
        return None;
    }
    match state.process_state {
        RuntimeDisplayState::Thinking => Some(EasterEgg::Wired),
        RuntimeDisplayState::Responding => Some(EasterEgg::LongTunnel),
        RuntimeDisplayState::Compacting => Some(EasterEgg::KeepsakeClover),
        RuntimeDisplayState::ToolRunning
            if state.seen_subagent_ids.len() >= 3 && !state.live_subagent_ids.is_empty() =>
        {
            Some(EasterEgg::Tachikoma)
        }
        _ => None,
    }
}

fn update_status_easter_egg(state: &mut SessionState) {
    let candidate = eligible_status_easter_egg(state);
    match state.status_easter_egg {
        StatusEasterEgg::Showing(egg) if candidate != Some(egg) => {
            state.status_easter_egg = StatusEasterEgg::Used;
        }
        StatusEasterEgg::Available => {
            let (Some(agent_run_id), Some(candidate)) =
                (state.active_agent_run_id.as_deref(), candidate)
            else {
                return;
            };
            let selected = if candidate == EasterEgg::Tachikoma {
                fnv1a32(&format!("tachikoma:{agent_run_id}")).is_multiple_of(16)
            } else {
                let choices = [
                    EasterEgg::Wired,
                    EasterEgg::LongTunnel,
                    EasterEgg::KeepsakeClover,
                ];
                fnv1a32(&format!("runtime-egg:{agent_run_id}")).is_multiple_of(32)
                    && choices[(fnv1a32(&format!("runtime-egg-choice:{agent_run_id}")) >> 16)
                        as usize
                        % choices.len()]
                        == candidate
            };
            if selected {
                state.status_easter_egg = StatusEasterEgg::Showing(candidate);
            }
        }
        StatusEasterEgg::Showing(_) | StatusEasterEgg::Used => {}
    }
}

fn clear_assistant_buffer(state: &mut SessionState) {
    state.assistant_buffer.clear();
    state.assistant_emitted_bytes = 0;
    state.assistant_tail_in_code_block = false;
    state.assistant_stream_started = false;
    state.assistant_stream_start = None;
    state.assistant_run_id = None;
    state.assistant_is_final = false;
}

fn invalidate_transcript_layout(state: &mut SessionState) {
    state.transcript_revision = state.transcript_revision.wrapping_add(1);
}

fn replace_assistant_buffer(state: &mut SessionState, content: String) {
    if state.assistant_buffer == content {
        return;
    }
    discard_assistant_stream(state);
    state.assistant_buffer = content;
    state.assistant_run_id = state.presentation_run_id.clone();
}

fn discard_assistant_stream(state: &mut SessionState) {
    let Some(start) = state.assistant_stream_start else {
        clear_assistant_buffer(state);
        return;
    };
    if start < state.transcript.len() {
        state.transcript.truncate(start);
    }
    clear_assistant_buffer(state);
}

fn commit_assistant_buffer(state: &mut SessionState) {
    materialize_assistant_tail(state, /*separator*/ true);
    clear_assistant_buffer(state);
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
        "tool_running" | "executing" => RuntimeDisplayState::ToolRunning,
        "compressing" => RuntimeDisplayState::Compacting,
        "provider_waiting" => RuntimeDisplayState::Waiting(WaitReason::Model),
        "waiting_user" => RuntimeDisplayState::WaitingUser,
        "waiting" => RuntimeDisplayState::Waiting(WaitReason::Unspecified),
        _ => RuntimeDisplayState::PendingPhase,
    }
}

fn runtime_display_state_from_agent_run_status(status: &str) -> RuntimeDisplayState {
    match status.trim() {
        "waiting_user" => RuntimeDisplayState::WaitingUser,
        "queued" | "running" | "stalled" => RuntimeDisplayState::PendingPhase,
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
    let len = matching_commands(&app.session.input).len();
    if len == 0 {
        app.session.selected_command = 0;
        return;
    }

    let current = app.session.selected_command.min(len - 1);
    app.session.selected_command = match delta.cmp(&0) {
        std::cmp::Ordering::Less => current.checked_sub(1).unwrap_or(len - 1),
        std::cmp::Ordering::Greater => (current + 1) % len,
        std::cmp::Ordering::Equal => current,
    };
}

fn complete_selected_command(app: &mut App) {
    let Some(command) =
        selected_matching_command(app.session.input.as_str(), app.session.selected_command)
    else {
        app.session.message = Some(format!(
            "Unknown slash command prefix: {}",
            app.session.input
        ));
        return;
    };
    let Some(prefix) = slash_command_name(app.session.input.as_str()) else {
        return;
    };
    if !command.name.starts_with(prefix) {
        app.session.message = Some(format!(
            "Unknown slash command prefix: {}",
            app.session.input
        ));
        return;
    }

    let rest = app.session.input.get(prefix.len()..).unwrap_or_default();
    app.session.input = if rest.trim().is_empty() {
        format!("{} ", command.name)
    } else {
        format!("{}{}", command.name, rest)
    };
    set_input_cursor(app, app.session.input.len());
    app.session.message = None;
    app.session.show_help = false;
    app.session.selected_command = 0;
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
