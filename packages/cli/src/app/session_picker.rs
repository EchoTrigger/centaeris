use super::*;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::Color;
use serde::Deserialize;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const PAGE_SIZE: usize = 40;
const WORKING_FRAMES: [&str; 4] = ["⋅", ":", "⸬", "⁙"];

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Activity {
    Idle,
    Working,
    NeedsInput,
}

impl Activity {
    pub(super) fn from_run(active: bool, state: RuntimeDisplayState) -> Self {
        if !active {
            Self::Idle
        } else if state == RuntimeDisplayState::WaitingUser {
            Self::NeedsInput
        } else {
            Self::Working
        }
    }

    fn span(self, frame: usize) -> Span<'static> {
        let (symbol, color) = match self {
            Self::Idle => ("◇", theme().muted),
            Self::Working => (WORKING_FRAMES[frame], Color::Rgb(187, 154, 247)),
            Self::NeedsInput => ("◆", Color::Rgb(224, 175, 104)),
        };
        Span::styled(symbol, Style::default().fg(color))
    }
}

pub(super) fn row_highlight(selected: bool) -> Style {
    if selected {
        Style::default()
            .bg(Color::Rgb(54, 54, 54))
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().bg(Color::Rgb(36, 36, 36))
    }
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SessionEntry {
    id: String,
    title: String,
    cwd: String,
    is_pinned: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ForkedSession {
    id: String,
    title: String,
    cwd: String,
    draft: String,
    attachments: Vec<ForkDraftImage>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ForkDraftImage {
    placeholder: String,
    local_path: PathBuf,
}

impl Drop for ForkDraftImage {
    fn drop(&mut self) {
        if !self.local_path.as_os_str().is_empty() {
            let _ = std::fs::remove_file(&self.local_path);
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CatalogPage {
    items: Vec<SessionEntry>,
    next_cursor: Option<String>,
    reset: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeleteReceipt {
    deleted_session_id: String,
    deleted_session_ids: Vec<String>,
}

struct PageCursor {
    mode: &'static str,
    cursor: Option<String>,
}

enum Request {
    Page(RuntimeResponse),
    Delete {
        target: SessionEntry,
        response: RuntimeResponse,
    },
    Update {
        id: String,
        response: RuntimeResponse,
    },
}

struct RenameDraft {
    id: String,
    title: String,
    cursor: usize,
    select_all: bool,
}

impl RenameDraft {
    fn insert(&mut self, text: &str) {
        if self.select_all {
            self.title.clear();
            self.cursor = 0;
            self.select_all = false;
        }
        self.title.insert_str(self.cursor, text);
        self.cursor += text.len();
    }
}

#[derive(Default)]
struct HitRegions {
    list: Rect,
    rows: Rect,
    first_row: usize,
    arm_delete: Rect,
    rename: Rect,
    pin: Rect,
    confirm_delete: Rect,
    cancel: Rect,
}

impl HitRegions {
    fn invalidate_clicks(&mut self) {
        self.rows = Rect::default();
        self.arm_delete = Rect::default();
        self.rename = Rect::default();
        self.pin = Rect::default();
        self.confirm_delete = Rect::default();
        self.cancel = Rect::default();
    }
}

pub(super) struct SessionPicker {
    entries: Vec<SessionEntry>,
    next: Option<PageCursor>,
    request: Option<Request>,
    confirm: Option<SessionEntry>,
    rename: Option<RenameDraft>,
    notice: Option<String>,
    query: String,
    searching: bool,
    selected: usize,
    opened_at: Instant,
    hits: HitRegions,
}

impl SessionPicker {
    fn matches(&self) -> impl Iterator<Item = &SessionEntry> {
        let query = self.query.trim().to_lowercase();
        self.entries.iter().filter(move |entry| {
            query.is_empty()
                || entry.title.to_lowercase().contains(&query)
                || entry.id.to_lowercase().contains(&query)
                || entry.cwd.to_lowercase().contains(&query)
        })
    }

    fn move_selection(&mut self, delta: isize) {
        self.selected = self
            .selected
            .saturating_add_signed(delta)
            .min(self.matches().count().saturating_sub(1));
        self.hits.invalidate_clicks();
    }

    fn confirm_delete(&mut self, target: Option<SessionEntry>) {
        self.confirm = target;
        self.hits.invalidate_clicks();
    }

    fn deleting(&self) -> bool {
        matches!(self.request, Some(Request::Delete { .. }))
    }

    fn updating(&self) -> bool {
        matches!(self.request, Some(Request::Update { .. }))
    }

    fn editing_list(&self) -> bool {
        self.confirm.is_some() || self.rename.is_some() || self.deleting() || self.updating()
    }
}

fn first_page() -> PageCursor {
    PageCursor {
        mode: "pinned",
        cursor: None,
    }
}

pub(super) fn open(app: &mut App, query: &str) -> Result<(), String> {
    ensure_runtime(app)?;
    app.show_help = false;
    app.message = None;
    mouse::cancel_capture(app);
    app.session_picker = Some(SessionPicker {
        entries: Vec::new(),
        next: Some(first_page()),
        request: None,
        confirm: None,
        rename: None,
        notice: None,
        query: query.trim().to_owned(),
        searching: !query.trim().is_empty(),
        selected: 0,
        opened_at: Instant::now(),
        hits: HitRegions::default(),
    });
    request_page(app);
    Ok(())
}

fn request_page(app: &mut App) {
    let Some(picker) = app.session_picker.as_mut() else {
        return;
    };
    let Some(next) = picker.next.as_ref().filter(|_| picker.request.is_none()) else {
        return;
    };
    let result = app
        .runtime
        .as_mut()
        .ok_or_else(|| "Runtime is disconnected".to_string())
        .and_then(|runtime| {
            runtime.request_async(
                "session/catalog",
                json!({"request": {"mode": next.mode, "cursor": next.cursor, "limit": PAGE_SIZE}}),
            )
        });
    match result {
        Ok(response) => {
            picker.request = Some(Request::Page(response));
            picker.notice = None;
        }
        Err(error) => picker.notice = Some(error),
    }
}

pub(super) fn poll(app: &mut App) -> bool {
    let Some(request) = app
        .session_picker
        .as_ref()
        .and_then(|picker| picker.request.as_ref())
    else {
        return false;
    };
    let result = match request {
        Request::Page(response)
        | Request::Delete { response, .. }
        | Request::Update { response, .. } => response.try_recv(),
    };
    let result = match result {
        Ok(None) => return false,
        Ok(Some(value)) => Ok(value),
        Err(error) => Err(error),
    };
    let picker = app.session_picker.as_mut().expect("open picker");
    let request = picker.request.take().expect("polled request");
    match (request, result) {
        (_, Err(error)) => picker.notice = Some(error.to_string()),
        (Request::Page(_), Ok(value)) => match serde_json::from_value::<CatalogPage>(value) {
            Ok(page) => receive_page(app, page),
            Err(error) => picker.notice = Some(format!("Invalid session catalog: {error}")),
        },
        (Request::Delete { target, .. }, Ok(value)) => match delete_receipt(value, &target.id) {
            Ok(deleted) => {
                app.parked_sessions.retain(|id, _| !deleted.contains(id));
                picker.selected = 0;
                picker.next = Some(first_page());
                picker.entries.clear();
                picker.hits = HitRegions::default();
                if app
                    .active_session
                    .as_ref()
                    .is_some_and(|s| deleted.contains(&s.id))
                {
                    app.active_session = None;
                    app.read_only_session = None;
                    app.transcript.clear();
                    app.transcript_paging = None;
                    app.transcript_history_len = 0;
                    reset_session_view(app, Vec::new());
                    status_usage::restore(app);
                }
                request_page(app);
            }
            Err(error) => picker.notice = Some(error),
        },
        (Request::Update { id, .. }, Ok(value)) => {
            match serde_json::from_value::<SessionEntry>(value) {
                Ok(entry) if entry.id == id => {
                    picker.selected = 0;
                    picker.next = Some(first_page());
                    picker.entries.clear();
                    picker.hits = HitRegions::default();
                    request_page(app);
                }
                Ok(_) => picker.notice = Some("Session update returned a different session".into()),
                Err(error) => picker.notice = Some(format!("Invalid session update: {error}")),
            }
        }
    }
    true
}

fn receive_page(app: &mut App, page: CatalogPage) {
    let picker = app.session_picker.as_mut().expect("open picker");
    let next = picker.next.take().expect("requested page cursor");
    if page.reset {
        picker.entries.clear();
        picker.selected = 0;
        picker.next = Some(first_page());
        request_page(app);
        return;
    }
    for entry in page.items {
        if !picker.entries.iter().any(|known| known.id == entry.id) {
            picker.entries.push(entry);
        }
    }
    let start_recent = next.mode == "pinned" && page.next_cursor.is_none();
    picker.next = if start_recent {
        Some(PageCursor {
            mode: "recent",
            cursor: None,
        })
    } else {
        page.next_cursor.map(|cursor| PageCursor {
            mode: next.mode,
            cursor: Some(cursor),
        })
    };
    if start_recent || !picker.query.is_empty() {
        request_page(app);
    }
}

fn delete_receipt(value: Value, expected_id: &str) -> Result<Vec<String>, String> {
    let receipt: DeleteReceipt = serde_json::from_value(value)
        .map_err(|error| format!("Invalid session deletion receipt: {error}"))?;
    if receipt.deleted_session_id != expected_id
        || !receipt
            .deleted_session_ids
            .iter()
            .any(|id| id == expected_id)
    {
        return Err("Session deletion returned a different session".into());
    }
    Ok(receipt.deleted_session_ids)
}

fn delete(app: &mut App, target: SessionEntry) {
    if app
        .read_only_session
        .as_ref()
        .is_some_and(|entry| entry.id == target.id)
    {
        app.session_picker.as_mut().expect("open picker").notice =
            Some("This session is open read-only".into());
        return;
    }
    let result = app
        .runtime
        .as_mut()
        .ok_or_else(|| "Runtime is disconnected".to_string())
        .and_then(|runtime| {
            runtime.request_async(
                "_centaeris/session/delete",
                json!({"request": {"sessionId": target.id}}),
            )
        });
    let picker = app.session_picker.as_mut().expect("open picker");
    match result {
        Ok(response) => {
            picker.notice = None;
            picker.request = Some(Request::Delete { target, response });
        }
        Err(error) => picker.notice = Some(error),
    }
}

fn update_metadata(app: &mut App, id: String, patch: Value) {
    let mut request = patch;
    request["sessionId"] = json!(id);
    let result = app
        .runtime
        .as_mut()
        .ok_or_else(|| "Runtime is disconnected".to_string())
        .and_then(|runtime| {
            runtime.request_async(
                "_centaeris/session/update_metadata",
                json!({"request": request}),
            )
        });
    let picker = app.session_picker.as_mut().expect("open picker");
    match result {
        Ok(response) => {
            picker.notice = None;
            picker.request = Some(Request::Update { id, response });
            picker.hits.invalidate_clicks();
        }
        Err(error) => picker.notice = Some(error),
    }
}

fn reset_session_view(app: &mut App, active_runs: Vec<ActiveAgentRun>) {
    app.inline_images.clear();
    app.inline_image_cache_order.clear();
    app.inline_image_cache_bytes = 0;
    app.inline_image_errors.clear();
    app.output_preview.clear();
    app.expanded_bash_tools.clear();
    app.transcript_scroll = 0;
    app.transcript_max_scroll = 0;
    app.transcript_follow_bottom = true;
    app.transcript_selection = None;
    app.snapshot_revisions.clear();
    app.run_activity.clear();
    app.seen_stream_event_ids.clear();
    app.presentation_run_id = None;
    app.pending_question = None;
    clear_assistant_buffer(app);
    restore_active_agent_runs(app, active_runs);
    invalidate_transcript_layout(app);
}

fn detach_writer(app: &mut App, session: &TuiSession) -> Result<(), String> {
    let runtime = app.runtime.as_mut().ok_or("Runtime is disconnected")?;
    let detached = runtime
        .session_request(
            SessionCommand::Unsubscribe,
            json!({"sessionId": session.id, "viewerId": tui_viewer_id()}),
        )
        .map_err(String::from);
    let released = runtime.release_session_writer(&session.id);
    detached.and(released)
}

fn activate(app: &mut App, entry: &SessionEntry, allow_read_only: bool) -> Result<(), String> {
    if app.read_only_session.is_none()
        && app
            .active_session
            .as_ref()
            .is_some_and(|session| session.id == entry.id)
    {
        app.session_picker = None;
        return Ok(());
    }
    if app.active_session.is_none() && !app.input.is_empty() {
        // A draft created before the first prompt needs a catalog entry to return to.
        ensure_active_session(app, "New session")?;
    }
    let session = TuiSession {
        id: entry.id.clone(),
    };
    if app.parked_sessions.contains_key(&entry.id) {
        bind_session(app, &session)?;
        session_views::restore(app, &entry.id);
        return Ok(());
    }
    let claimed = app
        .runtime
        .as_mut()
        .ok_or("Runtime is disconnected")?
        .claim_session_writer(&entry.id)?;
    let (restore, activated) = if claimed {
        match load_writable_session(app, &session) {
            Ok(loaded) => loaded,
            Err(error) => {
                let _ = detach_writer(app, &session);
                return Err(error);
            }
        }
    } else if allow_read_only {
        (
            load_read_only_session_history(app, &session)?,
            entry.clone(),
        )
    } else {
        return Err("New fork is already open in another window".into());
    };
    session_views::park_foreground(app);
    app.read_only_session = (!claimed).then(|| activated.clone());
    reset_session_view(app, restore.active_agent_runs);
    app.session_cwd.clone_from(&activated.cwd);
    app.workspace_root = activated.cwd;
    app.home_risk_pending = false;
    app.active_session = Some(session);
    status_usage::restore(app);
    app.transcript = restore.transcript;
    app.transcript_history_len = app.transcript.len();
    app.transcript_paging = Some(restore.transcript_paging);
    for payload in restore.replay_items {
        apply_stream_payload(app, &payload);
    }
    app.session_picker = None;
    app.input_area = None;
    app.panel_area = None;
    app.message = None;
    if claimed && app.transcript.is_empty() {
        show_message(app, format!("Resumed empty session: {}", activated.title));
    }
    Ok(())
}

fn load_writable_session(
    app: &mut App,
    session: &TuiSession,
) -> Result<(SessionRestore, SessionEntry), String> {
    let restore = load_session_history(app, session)?;
    let entry = bind_session(app, session)?;
    Ok((restore, entry))
}

fn bind_session(app: &mut App, session: &TuiSession) -> Result<SessionEntry, String> {
    let response = app
        .runtime
        .as_mut()
        .ok_or("Runtime is disconnected")?
        .request(
            "_centaeris/session/activate",
            json!({"request": {"sessionId": session.id}}),
        )?;
    let entry: SessionEntry =
        serde_json::from_value(response).map_err(|error| error.to_string())?;
    if entry.id != session.id || entry.cwd.is_empty() {
        return Err("Runtime returned a different session or an empty workspace".into());
    }
    Ok(entry)
}

fn fork_read_only_session(app: &mut App, source: &SessionEntry) -> Result<(), String> {
    let response = app
        .runtime
        .as_mut()
        .ok_or("Runtime is disconnected")?
        .fork_session_before_user_turn(&source.id)?;
    let fork: ForkedSession = serde_json::from_value(response)
        .map_err(|error| format!("Invalid session fork receipt: {error}"))?;
    if fork.id == source.id || fork.id.is_empty() || fork.cwd.is_empty() {
        return Err("Runtime returned an invalid fork session".into());
    }
    let ranges = fork
        .attachments
        .iter()
        .map(|image| {
            let mut matches = fork.draft.match_indices(image.placeholder.as_str());
            let Some((start, placeholder)) = matches.next() else {
                return Err("Fork draft is missing an image placeholder");
            };
            if placeholder.is_empty() || matches.next().is_some() {
                return Err("Fork draft contains an ambiguous image placeholder");
            }
            Ok(start..start + placeholder.len())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let target = SessionEntry {
        id: fork.id,
        title: fork.title,
        cwd: fork.cwd,
        is_pinned: false,
    };
    activate(app, &target, /*allow_read_only*/ false)?;
    clear_composer(app);
    insert_text_at_cursor(app, &fork.draft);
    app.message = None;
    app.next_image_number = fork
        .attachments
        .iter()
        .filter_map(|image| {
            image
                .placeholder
                .strip_prefix("[Image #")?
                .strip_suffix(']')?
                .parse::<usize>()
                .ok()?
                .checked_add(1)
        })
        .max()
        .unwrap_or(1);
    app.draft_image_attachments = ranges
        .into_iter()
        .zip(fork.attachments)
        .map(|(range, mut image)| DraftImageAttachment {
            start: range.start,
            end: range.end,
            local_path: std::mem::take(&mut image.local_path),
        })
        .collect();
    Ok(())
}

pub(super) fn read_only_key(app: &mut App, key: KeyEvent) -> bool {
    let source = app
        .read_only_session
        .as_ref()
        .expect("read-only session")
        .clone();
    let result = match key.code {
        KeyCode::Char('r' | 'R')
            if key.kind == KeyEventKind::Press
                && !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
        {
            activate(app, &source, /*allow_read_only*/ true)
        }
        KeyCode::Char('f' | 'F')
            if key.kind == KeyEventKind::Press
                && !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
        {
            fork_read_only_session(app, &source)
        }
        KeyCode::Esc => open(app, ""),
        KeyCode::Char('\\' | '4') if key.modifiers == KeyModifiers::CONTROL => open(app, ""),
        KeyCode::Char('c') if key.modifiers == KeyModifiers::CONTROL => return prepare_exit(app),
        KeyCode::PageUp => {
            scroll_transcript(app, -8);
            Ok(())
        }
        KeyCode::PageDown => {
            scroll_transcript(app, 8);
            Ok(())
        }
        KeyCode::Home if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.transcript_scroll = 0;
            app.transcript_follow_bottom = false;
            load_older_transcript_page(app).map(|_| ())
        }
        KeyCode::End if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.transcript_scroll = app.transcript_max_scroll;
            app.transcript_follow_bottom = true;
            Ok(())
        }
        _ => Ok(()),
    };
    if let Err(error) = result {
        show_message(app, error);
    }
    false
}

fn activate_selected(app: &mut App) {
    let entry = app
        .session_picker
        .as_ref()
        .and_then(|picker| picker.matches().nth(picker.selected).cloned());
    if let Some(entry) = entry {
        if let Err(error) = activate(app, &entry, /*allow_read_only*/ true) {
            app.session_picker.as_mut().expect("open picker").notice = Some(error);
        }
    }
}

pub(super) fn paste(app: &mut App, text: &str) {
    let Some(picker) = app
        .session_picker
        .as_mut()
        .filter(|p| p.confirm.is_none() && !p.deleting())
    else {
        return;
    };
    picker.searching = true;
    if let Some(rename) = picker.rename.as_mut() {
        let title: String = text
            .chars()
            .filter(|character| !character.is_control())
            .collect();
        if !title.is_empty() {
            rename.insert(&title);
        }
        return;
    }
    if picker.editing_list() {
        return;
    }
    picker
        .query
        .extend(text.chars().filter(|c| !c.is_control()));
    picker.selected = 0;
    picker.hits.invalidate_clicks();
    request_page(app);
}

fn begin_rename(picker: &mut SessionPicker) {
    let entry = picker.matches().nth(picker.selected).cloned();
    if let Some(entry) = entry {
        picker.rename = Some(RenameDraft {
            id: entry.id,
            cursor: entry.title.len(),
            title: entry.title,
            select_all: true,
        });
        picker.notice = None;
        picker.hits.invalidate_clicks();
    }
}

fn toggle_pin(app: &mut App) {
    let entry = app
        .session_picker
        .as_ref()
        .and_then(|picker| picker.matches().nth(picker.selected).cloned());
    if let Some(entry) = entry {
        update_metadata(app, entry.id, json!({"isPinned": !entry.is_pinned}));
    }
}

fn rename_key(app: &mut App, key: KeyEvent) {
    let picker = app.session_picker.as_mut().expect("open picker");
    let draft = picker.rename.as_mut().expect("open rename editor");
    match key.code {
        KeyCode::Esc => picker.rename = None,
        KeyCode::Char('c') if key.modifiers == KeyModifiers::CONTROL => picker.rename = None,
        KeyCode::Char('a') if key.modifiers == KeyModifiers::CONTROL => draft.select_all = true,
        KeyCode::Enter => {
            if draft.title.trim().is_empty() {
                picker.notice = Some("Session title is required".into());
                return;
            }
            let draft = picker.rename.take().expect("open rename editor");
            update_metadata(app, draft.id, json!({"title": draft.title.trim()}));
        }
        KeyCode::Backspace | KeyCode::Delete if draft.select_all => draft.insert(""),
        KeyCode::Backspace if draft.cursor > 0 => {
            let start = editing::previous(&draft.title, draft.cursor);
            draft.title.drain(start..draft.cursor);
            draft.cursor = start;
        }
        KeyCode::Delete if draft.cursor < draft.title.len() => {
            let end = editing::next(&draft.title, draft.cursor);
            draft.title.drain(draft.cursor..end);
        }
        KeyCode::Left | KeyCode::Right | KeyCode::Home | KeyCode::End => {
            draft.cursor = match key.code {
                KeyCode::Home => 0,
                KeyCode::End => draft.title.len(),
                KeyCode::Left if draft.select_all => 0,
                KeyCode::Right if draft.select_all => draft.title.len(),
                KeyCode::Left => editing::previous(&draft.title, draft.cursor),
                _ => editing::next(&draft.title, draft.cursor),
            };
            draft.select_all = false;
        }
        KeyCode::Char(character) if !character.is_control() && editing::accepts_text(key) => {
            let mut utf8 = [0; 4];
            draft.insert(character.encode_utf8(&mut utf8));
        }
        _ => {}
    }
}

pub(super) fn key(app: &mut App, key: KeyEvent) {
    let picker = app.session_picker.as_mut().expect("open picker");
    if picker.deleting() || picker.updating() {
        return;
    }
    if picker.rename.is_some() {
        rename_key(app, key);
        return;
    }
    if picker.confirm.is_some() {
        if key.kind == KeyEventKind::Press && key.modifiers.is_empty() {
            match key.code {
                KeyCode::Char('y') if !picker.hits.confirm_delete.is_empty() => {
                    let target = picker.confirm.take().expect("confirmed target");
                    delete(app, target);
                }
                KeyCode::Char('n') | KeyCode::Esc => picker.confirm_delete(None),
                _ => {}
            }
        }
        return;
    }
    if key.modifiers == KeyModifiers::CONTROL && key.code == KeyCode::Char('c') {
        app.session_picker = None;
        return;
    }
    if key.kind == KeyEventKind::Press
        && key.modifiers == KeyModifiers::CONTROL
        && picker.request.is_none()
    {
        match key.code {
            KeyCode::Char('r') => {
                begin_rename(picker);
                return;
            }
            KeyCode::Char('t') => {
                toggle_pin(app);
                return;
            }
            _ => {}
        }
    }
    match key.code {
        KeyCode::Esc if picker.searching => {
            picker.searching = false;
        }
        KeyCode::Esc if !picker.query.is_empty() => {
            picker.query.clear();
            picker.selected = 0;
            picker.hits.invalidate_clicks();
        }
        KeyCode::Esc => app.session_picker = None,
        KeyCode::Up | KeyCode::Down if picker.searching => picker.searching = false,
        KeyCode::Up => picker.move_selection(-1),
        KeyCode::Down | KeyCode::PageDown => {
            let step = if key.code == KeyCode::Down {
                1
            } else {
                picker.hits.list.height.max(1) as isize
            };
            picker.move_selection(step);
            if picker.selected + 1 >= picker.matches().count() {
                request_page(app);
            }
        }
        KeyCode::PageUp => picker.move_selection(-(picker.hits.list.height.max(1) as isize)),
        KeyCode::Enter => activate_selected(app),
        KeyCode::Backspace => {
            if let Some((start, _)) = picker.query.grapheme_indices(true).next_back() {
                picker.query.truncate(start);
                picker.selected = 0;
                picker.hits.invalidate_clicks();
            }
        }
        KeyCode::Char('d') if !picker.searching && key.modifiers.is_empty() => {
            let target = picker
                .matches()
                .nth(picker.selected)
                .filter(|_| picker.request.is_none())
                .cloned();
            picker.confirm_delete(target);
        }
        KeyCode::Char('/') if !picker.searching && key.modifiers.is_empty() => {
            picker.searching = true
        }
        KeyCode::Char(c)
            if !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
        {
            let mut utf8 = [0; 4];
            paste(app, c.encode_utf8(&mut utf8));
        }
        _ => {}
    }
}

pub(super) fn mouse(app: &mut App, event: MouseEvent) {
    let picker = app.session_picker.as_mut().expect("open picker");
    let position = Position::new(event.column, event.row);
    if picker.deleting() || picker.updating() || picker.rename.is_some() {
        return;
    }
    if event.kind == MouseEventKind::Down(MouseButton::Left) {
        if picker.hits.cancel.contains(position) {
            picker.confirm_delete(None);
        } else if picker.hits.confirm_delete.contains(position) {
            if let Some(target) = picker.confirm.take() {
                delete(app, target);
            }
        } else if picker.hits.rename.contains(position) && picker.request.is_none() {
            begin_rename(picker);
        } else if picker.hits.pin.contains(position) && picker.request.is_none() {
            toggle_pin(app);
        } else if picker.confirm.is_none()
            && picker.hits.arm_delete.contains(position)
            && picker.request.is_none()
        {
            let target = picker.matches().nth(picker.selected).cloned();
            picker.confirm_delete(target);
        } else if picker.confirm.is_none() && picker.hits.rows.contains(position) {
            picker.selected = picker.hits.first_row + usize::from(event.row - picker.hits.rows.y);
            activate_selected(app);
        }
    } else if picker.confirm.is_none() && picker.hits.list.contains(position) {
        match event.kind {
            MouseEventKind::ScrollUp => picker.move_selection(-3),
            MouseEventKind::ScrollDown => {
                picker.move_selection(3);
                if picker.selected + 1 >= picker.matches().count() {
                    request_page(app);
                }
            }
            _ => {}
        }
    }
}

pub(super) fn list_view(app: &App) -> Option<(Rect, Range<usize>, usize)> {
    let picker = app.session_picker.as_ref()?;
    Some((
        picker.hits.list,
        picker.hits.first_row..picker.matches().count(),
        picker.selected,
    ))
}

pub(super) fn scroll_to(app: &mut App, selected: usize) {
    let picker = app.session_picker.as_mut().expect("open picker");
    if picker.confirm.is_some() || picker.deleting() {
        return;
    }
    let total = picker.matches().count();
    picker.selected = selected.min(total.saturating_sub(1));
    picker.hits.invalidate_clicks();
    if picker.selected + 1 >= total {
        request_page(app);
    }
}

pub(super) fn invalidate_geometry(app: &mut App) {
    if let Some(picker) = app.session_picker.as_mut() {
        picker.hits = HitRegions::default();
    }
}

fn activity(app: &App, id: &str) -> Activity {
    if !app
        .runtime
        .as_ref()
        .is_some_and(RuntimeClient::is_connected)
    {
        return Activity::Idle;
    }
    if app
        .active_session
        .as_ref()
        .is_some_and(|session| session.id == id)
    {
        return Activity::from_run(has_active_agent_run(app), app.process_state);
    }
    app.parked_sessions
        .get(id)
        .map_or(Activity::Idle, session_views::SessionView::activity)
}

pub(super) fn has_working_row(app: &App) -> bool {
    app.session_picker.as_ref().is_some_and(|picker| {
        picker
            .matches()
            .skip(picker.hits.first_row)
            .take(usize::from(picker.hits.rows.height))
            .any(|entry| activity(app, &entry.id) == Activity::Working)
    })
}

pub(super) fn render(frame: &mut Frame, area: Rect, app: &mut App) {
    let picker = app.session_picker.as_ref().expect("open picker");
    if area.width < 32 || area.height < 10 {
        invalidate_geometry(app);
        frame.render_widget(Paragraph::new("Terminal too small."), area);
        return;
    }
    let panel = picker_view::content(frame, area);
    let width = panel.width;
    let [heading, search, list, status, actions] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(if picker.confirm.is_some() { 2 } else { 1 }),
        Constraint::Length(1),
    ])
    .areas(panel);
    let muted = Style::default().fg(theme().muted);
    picker_view::title(frame, heading, "Resume session");
    if let Some(rename) = picker.rename.as_ref() {
        const LABEL: &str = "Rename: ";
        let [label, editor] =
            Layout::horizontal([Constraint::Length(LABEL.len() as u16), Constraint::Min(1)])
                .areas(search);
        frame.render_widget(Paragraph::new(LABEL).style(muted), label);
        let cursor = rename.title[..rename.cursor].width();
        let scroll = cursor.saturating_sub(usize::from(editor.width.saturating_sub(1)));
        let style = if rename.select_all {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default()
        };
        frame.render_widget(
            Paragraph::new(rename.title.as_str())
                .style(style)
                .scroll((0, scroll as u16)),
            editor,
        );
        frame.set_cursor_position((editor.x + (cursor - scroll) as u16, editor.y));
    } else {
        let query = fit_middle_columns(&picker.query, width.saturating_sub(8).into());
        frame.render_widget(
            Paragraph::new(format!("Filter: {query}")).style(muted),
            search,
        );
    }

    let matches: Vec<_> = picker.matches().collect();
    let list_body = picker_view::text_area(list, matches.len());
    let selected = picker.selected.min(matches.len().saturating_sub(1));
    let folder_width = list_body.width.saturating_sub(24).min(list_body.width / 3);
    let title_width = usize::from(list_body.width.saturating_sub(folder_width + 6)).min(32);
    let frame_index = (picker.opened_at.elapsed().as_millis() / STATUS_REDRAW_INTERVAL.as_millis())
        as usize
        % WORKING_FRAMES.len();
    let items = matches
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let title = if entry.is_pinned {
                format!("* {}", entry.title)
            } else {
                entry.title.clone()
            };
            let activity = activity(app, &entry.id);
            let folder = fit_middle_columns(&entry.cwd, folder_width.into());
            let selected = index == selected;
            let item = picker_view::row_with_prefix(
                &title,
                folder,
                title_width,
                selected,
                [activity.span(frame_index), Span::raw(" ")],
            );
            if selected {
                item.style(row_highlight(true))
            } else {
                item
            }
        })
        .collect::<Vec<_>>();
    let first_row = picker_view::render_list(frame, list_body, items, selected);
    let count = matches
        .len()
        .saturating_sub(first_row)
        .min(usize::from(list_body.height)) as u16;
    let visible_rows = Rect {
        height: count,
        ..list_body
    };
    let message = if picker.deleting() {
        "Deleting session…".into()
    } else if picker.updating() {
        "Updating session…".into()
    } else if let Some(notice) = picker.notice.as_ref() {
        notice.clone()
    } else if picker.request.is_some() {
        "Loading sessions…".into()
    } else if matches.is_empty() {
        "No matching sessions".into()
    } else if picker.rename.is_none() {
        "↑↓ Enter · / Search · Esc Back".into()
    } else {
        String::new()
    };
    let lines = if let Some(target) = picker.confirm.as_ref() {
        let title = fit_middle_columns(&target.title, width.saturating_sub(8).into());
        vec![
            Line::from(format!("Delete {title}?")),
            Line::from("Includes children and runs."),
        ]
    } else {
        vec![Line::from(fit_middle_columns(&message, width.into()))]
    };
    frame.render_widget(Paragraph::new(lines).style(muted), status);
    let [first_action, second_action, third_action] = Layout::horizontal([
        Constraint::Length(11),
        Constraint::Length(11),
        Constraint::Min(1),
    ])
    .areas(actions);
    let mut hits = HitRegions {
        list: if !picker.editing_list() {
            list
        } else {
            Rect::default()
        },
        rows: visible_rows,
        first_row,
        ..HitRegions::default()
    };
    if picker.confirm.is_some() {
        frame.render_widget(Paragraph::new("n Cancel").style(muted), first_action);
        frame.render_widget(
            Paragraph::new("y Delete").style(Style::default().add_modifier(Modifier::BOLD)),
            second_action,
        );
        hits.cancel = first_action;
        hits.confirm_delete = second_action;
    } else if picker.rename.is_some() {
        frame.render_widget(
            Paragraph::new("Enter Save · Esc Cancel").style(muted),
            actions,
        );
    } else if count > 0 && picker.request.is_none() {
        let pin = if matches.get(selected).is_some_and(|entry| entry.is_pinned) {
            "^T Unpin"
        } else {
            "^T Pin"
        };
        frame.render_widget(Paragraph::new("d Delete").style(muted), first_action);
        frame.render_widget(Paragraph::new("^R Rename").style(muted), second_action);
        frame.render_widget(Paragraph::new(pin).style(muted), third_action);
        hits.arm_delete = first_action;
        hits.rename = second_action;
        hits.pin = third_action;
    }
    app.session_picker.as_mut().expect("open picker").hits = hits;
}
