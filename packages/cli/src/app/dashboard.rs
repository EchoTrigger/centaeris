use super::*;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::Color;
use ratatui::widgets::Block;

use session_picker::{Activity, SessionEntry};

const LIST_FLOOR_ROWS: u16 = 12;
const PEEK_MIN_ROWS: u16 = 8;
const PEEK_BODY_MAX_ROWS: u16 = 28;

pub(super) struct Dashboard {
    visible: bool,
    origin: Origin,
    selection: Selection,
    inactive_expanded: bool,
    composer_focused: bool,
    draft: Draft,
    notice: Option<String>,
    hits: HitRegions,
    opened_at: Instant,
    branch: Option<String>,
    branch_reply: Option<mpsc::Receiver<Option<String>>>,
}

enum Origin {
    Session(String),
    Transient(Option<Box<SessionState>>),
}

#[derive(Clone, Default, PartialEq, Eq)]
enum Selection {
    #[default]
    NewAgent,
    Inactive,
    Session(String),
}

#[derive(Default)]
struct Draft {
    text: String,
    cursor: usize,
}

impl Draft {
    fn insert(&mut self, text: &str) {
        self.text.insert_str(self.cursor, text);
        self.cursor += text.len();
    }

    fn edit(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Left => self.cursor = editing::previous(&self.text, self.cursor),
            KeyCode::Right => self.cursor = editing::next(&self.text, self.cursor),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.text.len(),
            KeyCode::Backspace => {
                let from = editing::previous(&self.text, self.cursor);
                self.text.replace_range(from..self.cursor, "");
                self.cursor = from;
            }
            KeyCode::Delete => {
                let to = editing::next(&self.text, self.cursor);
                self.text.replace_range(self.cursor..to, "");
            }
            KeyCode::Enter
                if key
                    .modifiers
                    .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) =>
            {
                self.insert("\n");
            }
            KeyCode::Char(ch) if !ch.is_control() && editing::accepts_text(key) => {
                self.insert(ch.encode_utf8(&mut [0; 4]));
            }
            _ => {}
        }
    }
}

#[derive(Default)]
struct HitRegions {
    list: Rect,
    rows: Vec<(Rect, Selection)>,
    composer: Rect,
}

impl Dashboard {
    fn select(&mut self, selection: Selection) {
        if selection != self.selection {
            self.selection = selection;
            self.draft = Draft::default();
            self.notice = None;
        }
    }
}

pub(super) fn visible(app: &App) -> bool {
    app.dashboard
        .as_ref()
        .is_some_and(|dashboard| dashboard.visible)
}

pub(super) fn hotkey(key: KeyEvent) -> bool {
    key.kind == KeyEventKind::Press
        && key.modifiers == KeyModifiers::CONTROL
        && matches!(key.code, KeyCode::Char('\\' | '4'))
}

pub(super) fn fetch_pinned_pages(app: &App) -> bool {
    visible(app)
}

pub(super) fn preserves_origin(app: &App) -> bool {
    app.dashboard
        .as_ref()
        .is_some_and(|dashboard| matches!(dashboard.origin, Origin::Transient(None)))
}

pub(super) fn park_origin(app: &mut App, state: SessionState) {
    if let Some(Origin::Transient(saved @ None)) = app
        .dashboard
        .as_mut()
        .map(|dashboard| &mut dashboard.origin)
    {
        *saved = Some(Box::new(state));
    }
}

pub(super) fn toggle(app: &mut App) {
    if visible(app) {
        if let Err(error) = close(app) {
            app.dashboard.as_mut().expect("open dashboard").notice = Some(error);
        }
    } else if let Some(dashboard) = app.dashboard.as_mut() {
        dashboard.visible = true;
        dashboard.refresh_branch(&app.session.workspace_root);
        mouse::resize(app);
        if !app.session_catalog.scoped_to(&app.session.workspace_root) {
            session_picker::reload_catalog(app);
        }
    } else {
        let origin = app
            .session
            .active_session
            .as_ref()
            .filter(|_| app.session.read_only_session.is_none() || app.session.history_load_failed)
            .map_or(Origin::Transient(None), |session| {
                Origin::Session(session.id.clone())
            });
        app.dashboard = Some(Dashboard {
            visible: true,
            origin,
            selection: Selection::default(),
            inactive_expanded: false,
            composer_focused: true,
            draft: Draft::default(),
            notice: None,
            hits: HitRegions::default(),
            opened_at: Instant::now(),
            branch: None,
            branch_reply: None,
        });
        if let Some(dashboard) = &mut app.dashboard {
            dashboard.refresh_branch(&app.session.workspace_root);
        }
        mouse::resize(app);
        session_picker::reload_catalog(app);
    }
}

impl Dashboard {
    fn refresh_branch(&mut self, workspace: &str) {
        self.branch = None;
        let path = PathBuf::from(workspace);
        let (reply, result) = mpsc::sync_channel(1);
        self.branch_reply = Some(result);
        thread::spawn(move || {
            let _ = reply.send(
                centaeris_cli_runtime::workspace_git_branch(&path)
                    .ok()
                    .flatten(),
            );
        });
    }
}

pub(super) fn poll_header(app: &mut App) -> bool {
    let Some(dashboard) = app.dashboard.as_mut() else {
        return false;
    };
    let Some(reply) = &dashboard.branch_reply else {
        return false;
    };
    match reply.try_recv() {
        Ok(branch) => {
            dashboard.branch = branch;
            dashboard.branch_reply = None;
            dashboard.visible
        }
        Err(mpsc::TryRecvError::Disconnected) => {
            dashboard.branch_reply = None;
            false
        }
        Err(mpsc::TryRecvError::Empty) => false,
    }
}

fn close(app: &mut App) -> Result<(), String> {
    let dashboard = app.dashboard.as_mut().expect("open dashboard");
    match &mut dashboard.origin {
        Origin::Session(id) if app.parked_sessions.contains_key(id) => {
            let id = id.clone();
            session_picker::activate_loaded(app, &id)?;
        }
        Origin::Transient(saved) => {
            if let Some(state) = saved.take() {
                app.dashboard = None;
                session_views::park_foreground(app);
                app.session = *state;
                status_usage::restore(app);
            }
        }
        Origin::Session(_) => {}
    }
    app.dashboard = None;
    mouse::resize(app);
    Ok(())
}

fn writable<'a>(app: &'a App, id: &str) -> Option<&'a SessionState> {
    session_views::loaded(app, id).filter(|state| state.read_only_session.is_none())
}

fn composer_available(app: &App, selection: &Selection) -> bool {
    match selection {
        Selection::NewAgent => true,
        Selection::Session(id) if app.ui_preferences.dashboard_preview() => writable(app, id)
            .is_some_and(|state| {
                state.pending_history.is_none()
                    && state.pending_question.is_none()
                    && state.process_state != RuntimeDisplayState::WaitingUser
                    && app
                        .runtime
                        .as_ref()
                        .is_some_and(RuntimeClient::is_connected)
            }),
        Selection::Session(_) | Selection::Inactive => false,
    }
}

fn entry<'a>(app: &'a App, id: &str) -> Option<&'a SessionEntry> {
    app.session_catalog
        .entries
        .iter()
        .find(|entry| entry.id == id)
        .or_else(|| session_views::loaded(app, id)?.catalog_entry.as_ref())
}

fn open_session(app: &mut App, id: &str) -> Result<(), String> {
    let entry = entry(app, id)
        .cloned()
        .ok_or("Session is no longer in the catalog")?;
    session_picker::open_history(app, &entry)?;
    let dashboard = app.dashboard.as_mut().expect("open dashboard");
    dashboard.visible = false;
    dashboard.notice = None;
    mouse::resize(app);
    Ok(())
}

fn new_agent(app: &mut App, message: &str, attach: bool) -> Result<(), String> {
    let message = message.trim_end();
    let command = slash_command_name(message);
    if command.is_some() && !matches!(command, Some("/new" | "/clear")) {
        return Err("Open a session to run that command".into());
    }
    session_views::open_new(app);
    app.dashboard.as_mut().expect("open dashboard").visible = false;
    if message.is_empty() || command.is_some() {
        return Ok(());
    }
    insert_text_at_cursor(app, message);
    if should_open_home_risk_panel(app, message) {
        return open_home_risk_panel(app);
    }
    start_prompt(app, message.to_owned())?;
    if !attach {
        let id = app
            .session
            .active_session
            .as_ref()
            .expect("started session")
            .id
            .clone();
        let dashboard = app.dashboard.as_mut().expect("open dashboard");
        dashboard.select(Selection::Session(id));
        dashboard.visible = true;
    }
    Ok(())
}

pub(super) struct QueuedReply {
    session_id: String,
    message: String,
    attachments: Vec<DraftImageAttachment>,
    operation_id: String,
    retry_at: Option<Instant>,
    error: Option<String>,
}

impl Drop for QueuedReply {
    fn drop(&mut self) {
        for attachment in &self.attachments {
            let _ = std::fs::remove_file(&attachment.local_path);
        }
    }
}

pub(super) fn queue_loading_prompt(app: &mut App, message: String) -> Result<(), String> {
    let operation_id = new_runtime_operation_id()?;
    let session_id = app
        .session
        .active_session
        .as_ref()
        .ok_or("No session")?
        .id
        .clone();
    let attachments = std::mem::take(&mut app.session.draft_image_attachments);
    for attachment in &attachments {
        app.session
            .retired_image_paths
            .remove(&attachment.local_path);
    }
    app.session.dashboard_replies.push_back(QueuedReply {
        session_id,
        message,
        attachments,
        operation_id,
        retry_at: None,
        error: None,
    });
    clear_composer(app);
    app.session.message = None;
    Ok(())
}

fn enqueue_reply(app: &mut App, id: &str, message: String) -> Result<(), String> {
    let operation_id = new_runtime_operation_id()?;
    let state = session_views::loaded_mut(app, id)
        .filter(|state| state.read_only_session.is_none())
        .ok_or("Open the session before replying")?;
    state.dashboard_replies.push_back(QueuedReply {
        session_id: id.to_owned(),
        message,
        attachments: Vec::new(),
        operation_id,
        retry_at: None,
        error: None,
    });
    drain_replies(app);
    Ok(())
}

pub(super) fn drain_replies(app: &mut App) -> bool {
    let Some(runtime) = app
        .runtime
        .as_mut()
        .filter(|runtime| runtime.is_connected())
    else {
        return false;
    };
    let mut changed = false;
    for state in std::iter::once(&mut app.session).chain(app.parked_sessions.values_mut()) {
        if dispatch_next(runtime, state) {
            if let Some(error) = state
                .dashboard_replies
                .front()
                .and_then(|reply| reply.error.as_deref())
            {
                state.message = Some(format!("Reply failed · {error}"));
            }
            changed = true;
        }
    }
    changed
}

fn dispatch_next(runtime: &mut RuntimeClient, state: &mut SessionState) -> bool {
    if state.read_only_session.is_some()
        || state.pending_history.is_some()
        || has_active_agent_run(state)
        || state.pending_question.is_some()
        || state.process_state == RuntimeDisplayState::WaitingUser
    {
        return false;
    }
    let Some(session) = state.active_session.as_ref() else {
        return false;
    };
    let Some(reply) = state.dashboard_replies.front_mut().filter(|reply| {
        reply.error.is_none() && reply.retry_at.is_none_or(|time| Instant::now() >= time)
    }) else {
        return false;
    };
    if reply.session_id != session.id {
        reply.error = Some("Reply session changed".into());
        return true;
    }
    let response = runtime.session_request(
        SessionCommand::Send,
        build_prompt_request(
            session,
            &reply.message,
            &reply.attachments,
            &reply.operation_id,
        ),
    );
    let response = match response {
        Ok(response) if response.get("sessionId").and_then(Value::as_str) == Some(&session.id) => {
            response
        }
        Ok(_) => {
            reply.error = Some("Runtime returned a different reply session".into());
            return true;
        }
        Err(error)
            if error.code() == "session_prompt_failed"
                && error
                    .message()
                    .starts_with("session already has an active AgentRun: agentRunId=") =>
        {
            // The terminal event can arrive before the previous run releases its admission lease.
            // The Host rejected this start. The same operation ID resumes its existing receipt.
            reply.retry_at = Some(Instant::now() + EVENT_POLL_INTERVAL);
            return false;
        }
        Err(error) => {
            reply.error = Some(error.to_string());
            return true;
        }
    };
    if response
        .get("agentRunId")
        .and_then(Value::as_str)
        .is_none_or(str::is_empty)
    {
        reply.error = Some("Runtime reply is missing its run ID".into());
        return true;
    }
    let mut reply = state
        .dashboard_replies
        .pop_front()
        .expect("dispatched reply");
    commit_assistant_buffer(state);
    // The normal composer may hold an unsent draft (including media) and an independent viewport.
    apply_prompt_started(state, std::mem::take(&mut reply.message), &response)
        .expect("validated run receipt");
    true
}

fn action(app: &mut App, attach: bool, use_draft: bool) {
    let dashboard = app.dashboard.as_ref().expect("open dashboard");
    let selection = dashboard.selection.clone();
    if use_draft && !dashboard.draft.text.trim().is_empty() && !composer_available(app, &selection)
    {
        app.dashboard.as_mut().expect("open dashboard").notice =
            Some("Open the session to continue".into());
        return;
    }
    let message = if use_draft {
        dashboard.draft.text.trim_end().to_owned()
    } else {
        String::new()
    };
    let result = match selection {
        Selection::NewAgent => {
            let result = new_agent(app, &message, attach);
            let dashboard = app.dashboard.as_mut().expect("open dashboard");
            if result.is_ok() || !dashboard.visible {
                dashboard.draft = Draft::default();
            }
            result
        }
        Selection::Inactive => {
            let dashboard = app.dashboard.as_mut().expect("open dashboard");
            dashboard.inactive_expanded = !dashboard.inactive_expanded;
            if dashboard.inactive_expanded {
                session_picker::request_page(app);
            }
            Ok(())
        }
        Selection::Session(id) if !message.trim().is_empty() => enqueue_reply(app, &id, message)
            .and_then(|()| {
                let dashboard = app.dashboard.as_mut().expect("open dashboard");
                dashboard.draft = Draft::default();
                if attach {
                    let failed = writable(app, &id)
                        .and_then(|state| state.dashboard_replies.front())
                        .is_some_and(|reply| reply.error.is_some());
                    if !failed {
                        return open_session(app, &id);
                    }
                }
                Ok(())
            }),
        Selection::Session(id) => open_session(app, &id),
    };
    let dashboard = app.dashboard.as_mut().expect("open dashboard");
    if !dashboard.visible {
        if let Err(error) = result {
            show_message(app, error);
        }
    } else {
        dashboard.notice = result.err();
    }
}

pub(super) fn key(app: &mut App, key: KeyEvent) {
    let dashboard = app.dashboard.as_ref().expect("open dashboard");
    let focused = dashboard.composer_focused && composer_available(app, &dashboard.selection);
    let empty = dashboard.draft.text.trim().is_empty();
    match key.code {
        KeyCode::Esc if !empty => {
            app.dashboard.as_mut().expect("open dashboard").draft = Draft::default();
        }
        KeyCode::Esc if dashboard.selection != Selection::NewAgent => {
            select(app, Selection::NewAgent)
        }
        KeyCode::Esc => toggle(app),
        KeyCode::Tab
            if key.modifiers.is_empty() && composer_available(app, &dashboard.selection) =>
        {
            app.dashboard
                .as_mut()
                .expect("open dashboard")
                .composer_focused = !focused;
        }
        KeyCode::Up | KeyCode::Down if key.modifiers.is_empty() && (!focused || empty) => {
            move_selection(app, if key.code == KeyCode::Up { -1 } else { 1 });
        }
        KeyCode::PageUp | KeyCode::PageDown if key.modifiers.is_empty() => {
            let height = isize::try_from(dashboard.hits.list.height.max(1)).unwrap_or(1);
            move_selection(
                app,
                if key.code == KeyCode::PageUp {
                    -height
                } else {
                    height
                },
            );
        }
        KeyCode::Right if key.modifiers.is_empty() && (!focused || empty) => {
            action(app, true, false)
        }
        KeyCode::Enter if key.modifiers.is_empty() && key.kind == KeyEventKind::Press => {
            action(
                app,
                false,
                focused || dashboard.selection == Selection::NewAgent,
            );
        }
        KeyCode::Char('s')
            if key.modifiers == KeyModifiers::CONTROL
                && key.kind == KeyEventKind::Press
                && dashboard.selection != Selection::Inactive =>
        {
            action(app, true, composer_available(app, &dashboard.selection));
        }
        KeyCode::Char('v') if key.modifiers == KeyModifiers::CONTROL && focused => {
            match arboard::Clipboard::new().and_then(|mut clipboard| clipboard.get_text()) {
                Ok(text) => paste(app, &input_events::normalize_newlines(text)),
                Err(error) => {
                    app.dashboard.as_mut().expect("open dashboard").notice = Some(error.to_string())
                }
            }
        }
        _ if composer_available(app, &dashboard.selection) => {
            let dashboard = app.dashboard.as_mut().expect("open dashboard");
            if focused
                || matches!(key.code, KeyCode::Char(ch) if !ch.is_control() && editing::accepts_text(key))
            {
                dashboard.composer_focused = true;
                dashboard.draft.edit(key);
            }
        }
        _ => {}
    }
}

pub(super) fn paste(app: &mut App, text: &str) {
    let dashboard = app.dashboard.as_ref().expect("open dashboard");
    if dashboard.composer_focused && composer_available(app, &dashboard.selection) {
        app.dashboard
            .as_mut()
            .expect("open dashboard")
            .draft
            .insert(text);
    }
}

fn select(app: &mut App, selection: Selection) {
    let dashboard = app.dashboard.as_mut().expect("open dashboard");
    dashboard.select(selection);
}

fn move_selection(app: &mut App, step: isize) {
    let dashboard = app.dashboard.as_ref().expect("open dashboard");
    let mut selections = rows(app, dashboard.inactive_expanded)
        .into_iter()
        .filter_map(|row| row.selection())
        .collect::<Vec<_>>();
    let at = selections
        .iter()
        .position(|item| *item == dashboard.selection)
        .unwrap_or(0);
    let next = at
        .saturating_add_signed(step)
        .min(selections.len().saturating_sub(1));
    let load_more = step > 0 && next + 1 == selections.len() && dashboard.inactive_expanded;
    select(app, selections.swap_remove(next));
    if load_more {
        session_picker::request_page(app);
    }
}

pub(super) fn invalidate_geometry(app: &mut App) {
    if let Some(dashboard) = app.dashboard.as_mut() {
        dashboard.hits = HitRegions::default();
    }
}

pub(super) fn mouse(app: &mut App, event: MouseEvent) {
    let dashboard = app.dashboard.as_ref().expect("open dashboard");
    let position = Position::new(event.column, event.row);
    match event.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            if let Some((_, target)) = dashboard
                .hits
                .rows
                .iter()
                .find(|(rect, _)| rect.contains(position))
            {
                select(app, target.clone());
                action(app, true, false);
            } else if dashboard.hits.composer.contains(position) {
                let dashboard = app.dashboard.as_mut().expect("open dashboard");
                dashboard.composer_focused = true;
            }
        }
        MouseEventKind::ScrollUp if dashboard.hits.list.contains(position) => {
            move_selection(app, -3)
        }
        MouseEventKind::ScrollDown if dashboard.hits.list.contains(position) => {
            move_selection(app, 3)
        }
        _ => {}
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Group {
    Pinned,
    Working,
    Awaiting,
    Idle,
    Inactive,
}

impl Group {
    const ORDER: [Self; 5] = [
        Self::Pinned,
        Self::Working,
        Self::Awaiting,
        Self::Idle,
        Self::Inactive,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Pinned => "Pinned",
            Self::Working => "Working",
            Self::Awaiting => "Awaiting input",
            Self::Idle => "Idle",
            Self::Inactive => "Inactive",
        }
    }

    fn marker(self, tick: usize) -> Span<'static> {
        match self {
            Self::Working => Span::styled(
                ["⋅", ":", "⸬", "⁙"][tick % 4],
                Style::default().fg(Color::Rgb(187, 154, 247)),
            ),
            Self::Awaiting => Span::styled("●", Style::default().fg(Color::Yellow)),
            _ => Span::styled("○", Style::default().fg(theme().muted)),
        }
    }
}

enum Row<'a> {
    NewAgent,
    Heading(Group, usize),
    Session(&'a SessionEntry, Group),
}

impl Row<'_> {
    fn selection(&self) -> Option<Selection> {
        match self {
            Self::NewAgent => Some(Selection::NewAgent),
            Self::Heading(Group::Inactive, _) => Some(Selection::Inactive),
            Self::Heading(_, _) => None,
            Self::Session(entry, _) => Some(Selection::Session(entry.id.clone())),
        }
    }
}

fn rows(app: &App, inactive_expanded: bool) -> Vec<Row<'_>> {
    let mut entries = Vec::new();
    let mut seen = HashSet::new();
    let session_entries = std::iter::once(&app.session)
        .chain(app.parked_sessions.values())
        .filter_map(|state| state.catalog_entry.as_ref());
    for entry in app.session_catalog.entries.iter().chain(session_entries) {
        if seen.insert(entry.id.as_str()) {
            let activity = match writable(app, &entry.id) {
                Some(state) if state.pending_question.is_some() => Group::Awaiting,
                Some(state) => match state.activity() {
                    Activity::Working => Group::Working,
                    Activity::NeedsInput => Group::Awaiting,
                    Activity::Idle => Group::Idle,
                },
                None => Group::Inactive,
            };
            let group = if entry.is_pinned {
                Group::Pinned
            } else {
                activity
            };
            entries.push((entry, group, activity));
        }
    }
    let mut rows = vec![Row::NewAgent];
    for group in Group::ORDER {
        let members = entries.iter().filter(|(_, section, _)| *section == group);
        let count = members.clone().count();
        if count == 0 && !(group == Group::Inactive && app.session_catalog.has_more()) {
            continue;
        }
        rows.push(Row::Heading(group, count));
        if group != Group::Inactive || inactive_expanded {
            rows.extend(members.map(|(entry, _, activity)| Row::Session(entry, *activity)));
        }
    }
    rows
}

pub(super) fn render(frame: &mut Frame, area: Rect, app: &mut App) {
    let dashboard = app.dashboard.as_ref().expect("open dashboard");
    if area.width < 32 || area.height < 10 {
        invalidate_geometry(app);
        frame.render_widget(Paragraph::new("Terminal too small."), area);
        return;
    }
    frame.render_widget(Clear, area);
    let state = match &dashboard.selection {
        Selection::Session(id) if app.ui_preferences.dashboard_preview() => {
            session_views::loaded(app, id).filter(|state| state.pending_history.is_none())
        }
        _ => None,
    };
    let composer = composer_available(app, &dashboard.selection);
    let draft_layout =
        text_layout::TextLayout::plain(&dashboard.draft.text, composer_text_width(area.width));
    let draft_caret = draft_layout.caret(dashboard.draft.cursor);
    let panel_budget = area.height.saturating_sub(3);
    let composer_height = if composer {
        let max_rows = usize::from(panel_budget).min(3);
        draft_layout.height().max(draft_caret.0 + 1).min(max_rows) as u16
    } else {
        0
    };
    let max_peek_rows = ((u32::from(area.height) * 3 / 8) as u16)
        .min(area.height.saturating_sub(2 + LIST_FLOOR_ROWS));
    let preview = state
        .filter(|_| max_peek_rows >= PEEK_MIN_ROWS)
        .map(|session| peek_lines(session, area.width));
    let peek_height = preview.as_ref().map_or(0, |(prompt, lines)| {
        let body_rows = lines.len().min(usize::from(PEEK_BODY_MAX_ROWS)) as u16;
        let desired = 1 + u16::from(prompt.is_some()) + body_rows + composer_height;
        desired.clamp(PEEK_MIN_ROWS, max_peek_rows) - composer_height
    });
    let panel_height = peek_height + composer_height;
    let [heading, list, panel, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(panel_height),
        Constraint::Length(1),
    ])
    .areas(area);
    let muted = Style::default().fg(theme().muted);
    let rows = rows(app, dashboard.inactive_expanded);
    let tick = Instant::now()
        .duration_since(dashboard.opened_at)
        .as_millis()
        / STATUS_REDRAW_INTERVAL.as_millis();
    render_header(frame, heading, app, dashboard, &rows, tick as usize);
    let title_width = usize::from(list.width.saturating_sub(5) / 2);
    let description_width = usize::from(list.width.saturating_sub(title_width as u16 + 4));
    let mut selections = Vec::with_capacity(rows.len());
    let items = rows
        .into_iter()
        .map(|row| {
            let selection = row.selection();
            let selected = selection.as_ref() == Some(&dashboard.selection);
            selections.push(selection);
            match row {
                Row::NewAgent => {
                    dashboard_row("+ New Agent", "", title_width, selected, Span::raw(" "))
                }
                Row::Heading(Group::Inactive, count) => {
                    let action = if dashboard.inactive_expanded {
                        "Collapse"
                    } else {
                        "Expand"
                    };
                    let more = if app.session_catalog.has_more() {
                        "+"
                    } else {
                        ""
                    };
                    dashboard_row(
                        &format!("Inactive ({count}{more})"),
                        action,
                        title_width,
                        selected,
                        Span::raw(" "),
                    )
                }
                Row::Heading(group, count) => {
                    ListItem::new(Line::styled(format!("  {}  {count}", group.label()), muted))
                }
                Row::Session(entry, activity) => {
                    let status = writable(app, &entry.id)
                        .filter(|state| has_active_agent_run(state))
                        .map_or(activity.label(), peek_status);
                    let detail = if centaeris_cli_runtime::session_catalog_workspace_key(&entry.cwd)
                        == centaeris_cli_runtime::session_catalog_workspace_key(
                            &app.session.workspace_root,
                        ) {
                        status.to_owned()
                    } else {
                        format!("{status} · {}", entry.cwd)
                    };
                    let detail = fit_middle_columns(&detail, description_width);
                    dashboard_row(
                        &entry.title,
                        &detail,
                        title_width,
                        selected,
                        activity.marker(tick as usize),
                    )
                }
            }
        })
        .collect::<Vec<_>>();
    let selected = selections
        .iter()
        .position(|selection| selection.as_ref() == Some(&dashboard.selection))
        .unwrap_or(0);
    let offset = picker_view::render_list(frame, list, items, selected);
    let hits = selections
        .into_iter()
        .skip(offset)
        .take(usize::from(list.height))
        .enumerate()
        .filter_map(|(index, selection)| {
            Some((
                Rect::new(list.x, list.y + index as u16, list.width, 1),
                selection?,
            ))
        })
        .collect();
    let mut input = Rect::default();
    let mut caret = None;
    if panel.height > 0 {
        let background = if composer && dashboard.composer_focused {
            28
        } else {
            21
        };
        frame.render_widget(
            Block::default()
                .style(Style::default().bg(Color::Rgb(background, background, background))),
            panel,
        );
        let [peek, reply] = Layout::vertical([
            Constraint::Length(peek_height),
            Constraint::Length(composer_height),
        ])
        .areas(panel);
        if let (Some(state), Some((prompt, lines))) = (state, preview) {
            render_peek(frame, peek, state, prompt, lines);
        }
        if composer_height > 0 {
            caret = render_composer(frame, reply, dashboard, &draft_layout, draft_caret);
            input = reply;
        }
    }
    let hint = match dashboard.selection {
        Selection::Inactive if dashboard.inactive_expanded => {
            "↑↓ Select · Enter Collapse · Ctrl+\\ Back"
        }
        Selection::Inactive => "↑↓ Select · Enter Expand · Ctrl+\\ Back",
        Selection::Session(_) if !composer => "↑↓ Select · Enter Open session · Ctrl+\\ Back",
        _ if dashboard.composer_focused && !dashboard.draft.text.trim().is_empty() => {
            "Enter Send · Ctrl+S Send & open · Tab List · Ctrl+\\ Back"
        }
        _ => "↑↓ Select · Enter Open · Tab Compose · Ctrl+\\ Back",
    };
    let footer_text = dashboard
        .notice
        .as_deref()
        .or(app.session_catalog.notice.as_deref())
        .unwrap_or(if app.session_catalog.loading() {
            "Loading sessions…"
        } else {
            hint
        });
    frame.render_widget(Paragraph::new(footer_text).style(muted), footer);
    app.dashboard.as_mut().expect("open dashboard").hits = HitRegions {
        list,
        rows: hits,
        composer: input,
    };
    app.frame_caret = caret;
}

pub(super) fn has_working_session(app: &App) -> bool {
    visible(app)
        && std::iter::once(&app.session)
            .chain(app.parked_sessions.values())
            .any(|state| state.activity() == Activity::Working)
}

fn render_composer(
    frame: &mut Frame,
    area: Rect,
    dashboard: &Dashboard,
    layout: &text_layout::TextLayout,
    (row, column): (usize, usize),
) -> Option<Position> {
    let text = composer_text_area(area);
    render_composer_prefix(frame, area, dashboard.composer_focused);
    let first = (row + 1).saturating_sub(usize::from(text.height));
    if dashboard.draft.text.is_empty() {
        let prompt = if dashboard.selection == Selection::NewAgent {
            "New agent prompt"
        } else {
            "Reply"
        };
        frame.render_widget(
            Paragraph::new(prompt).style(Style::default().fg(theme().muted)),
            text,
        );
    } else {
        let lines = layout
            .lines
            .iter()
            .skip(first)
            .take(usize::from(text.height))
            .cloned()
            .collect::<Vec<_>>();
        frame.render_widget(Paragraph::new(lines), text);
    }
    dashboard.composer_focused.then(|| {
        Position::new(
            (text.x + column as u16).min(text.right().saturating_sub(1)),
            text.y + (row - first) as u16,
        )
    })
}

fn render_header(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    dashboard: &Dashboard,
    rows: &[Row<'_>],
    tick: usize,
) {
    let mut counts = [0; 3];
    for row in rows {
        match row {
            Row::Session(_, Group::Awaiting) => counts[0] += 1,
            Row::Session(_, Group::Working) => counts[1] += 1,
            Row::Session(_, Group::Idle) => counts[2] += 1,
            _ => {}
        }
    }
    let mut summary = Vec::new();
    for (group, (count, label)) in [Group::Awaiting, Group::Working, Group::Idle]
        .into_iter()
        .zip(counts.into_iter().zip(["awaiting", "working", "idle"]))
    {
        if count > 0 {
            if !summary.is_empty() {
                summary.push(Span::styled("  ·  ", Style::default().fg(theme().muted)));
            }
            summary.push(group.marker(tick));
            summary.push(Span::styled(
                format!(" {count} {label}"),
                Style::default().fg(Color::Gray),
            ));
        }
    }
    let summary = Line::from(summary);
    let summary_width = if summary.width() + 14 < usize::from(area.width) {
        summary.width() as u16
    } else {
        0
    };
    let [location, activity] =
        Layout::horizontal([Constraint::Min(1), Constraint::Length(summary_width)]).areas(area);
    let path = &app.session.workspace_root;
    let text = dashboard
        .branch
        .as_ref()
        .map_or_else(|| path.clone(), |branch| format!("{path}  ·  {branch}"));
    picker_view::title(
        frame,
        location,
        fit_middle_columns(&text, usize::from(location.width.saturating_sub(1))),
    );
    frame.render_widget(Paragraph::new(summary), activity);
}

fn dashboard_row(
    label: &str,
    detail: &str,
    width: usize,
    selected: bool,
    activity: Span<'static>,
) -> ListItem<'static> {
    let title = Span::raw(fit_middle_columns(label, width));
    let gap = width.saturating_sub(title.width()) + 2;
    ListItem::new(Line::from(vec![
        Span::raw(" "),
        activity,
        Span::raw(" "),
        title,
        Span::raw(" ".repeat(gap)),
        Span::styled(detail.to_owned(), Style::default().fg(Color::Gray)),
    ]))
    .style(if selected {
        session_picker::row_highlight(true)
    } else {
        Style::default()
    })
}

fn peek_lines(state: &SessionState, width: u16) -> (Option<Line<'static>>, Vec<Line<'static>>) {
    let last_user = state
        .transcript
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, item)| match item.content() {
            TranscriptLine::User(text) => Some((index, text)),
            _ => None,
        });
    let (start, prompt) = last_user.map_or((0, None), |(index, text)| {
        let line = view::user_prompt_lines(text).next().map(|(line, _)| line);
        (index + 1, line)
    });
    let turn = &state.transcript[start..];
    let mut lines = Vec::new();
    let mut group_start = 0;
    for (index, item) in turn.iter().enumerate() {
        let block = match item.content() {
            TranscriptLine::Tool(tool) if tool.is_bash => {
                vec![presentation::bash_header(tool, width, false)]
            }
            TranscriptLine::Tool(tool) => vec![presentation::tool_header(tool).0],
            TranscriptLine::ReferencedAnswer { key } => {
                let text = output_preview::assistant_answer_text(state, key);
                render_summary_lines(
                    text.as_deref()
                        .unwrap_or("Open session to read the full response"),
                    width,
                )
            }
            _ => continue,
        };
        lines.extend(view::transcript_to_lines(&turn[group_start..index], width));
        lines.extend(block);
        group_start = index + 1;
    }
    lines.extend(view::transcript_to_lines(&turn[group_start..], width));
    lines.extend(build_assistant_live_lines(state, width));
    let mut lines = text_layout::TextLayout::new(lines, width, false).lines;
    while lines
        .last()
        .is_some_and(|line| line.spans.iter().all(|span| span.content.trim().is_empty()))
    {
        lines.pop();
    }
    if lines.is_empty() {
        if let Some(question) = &state.pending_question {
            lines = text_layout::TextLayout::new(
                render_summary_lines(&question.question, width),
                width,
                false,
            )
            .lines;
        }
    }
    (prompt, lines)
}

fn render_peek(
    frame: &mut Frame,
    area: Rect,
    state: &SessionState,
    prompt: Option<Line<'static>>,
    lines: Vec<Line<'static>>,
) {
    let [heading, prompt_area, body] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(u16::from(prompt.is_some())),
        Constraint::Min(1),
    ])
    .areas(area);
    let mut title = if has_active_agent_run(state) || state.pending_question.is_some() {
        peek_status(state).to_owned()
    } else if lines.is_empty() {
        "Idle".to_owned()
    } else {
        "Response".to_owned()
    };
    if let Some(error) = state
        .dashboard_replies
        .front()
        .and_then(|reply| reply.error.as_ref())
    {
        title = format!("Reply failed · {error}");
    } else if !state.dashboard_replies.is_empty() {
        title.push_str(&format!(" · {} queued", state.dashboard_replies.len()));
    }
    let time = state
        .agent_run_started_at
        .filter(|_| has_active_agent_run(state))
        .map_or_else(String::new, |started| {
            format!("  {}", view::format_elapsed(started.elapsed()))
        });
    let time_width = if time.len() + 14 < usize::from(heading.width) {
        time.len() as u16
    } else {
        0
    };
    let [label, duration] =
        Layout::horizontal([Constraint::Min(1), Constraint::Length(time_width)]).areas(heading);
    picker_view::title(
        frame,
        label,
        fit_middle_columns(&title, usize::from(label.width)),
    );
    frame.render_widget(
        Paragraph::new(time).style(Style::default().fg(theme().muted)),
        duration,
    );
    if let Some(prompt) = prompt {
        frame.render_widget(Paragraph::new(prompt), prompt_area);
    }
    let budget = usize::from(body.height.min(PEEK_BODY_MAX_ROWS));
    let clipped = lines.len() > budget && budget > 1;
    let tail = budget.saturating_sub(usize::from(clipped));
    let first = lines.len().saturating_sub(tail);
    let mut visible = Vec::with_capacity(budget);
    if clipped {
        visible.push(Line::styled("  …", Style::default().fg(theme().muted)));
    }
    visible.extend(lines.into_iter().skip(first));
    frame.render_widget(Paragraph::new(visible), body);
}

fn peek_status(state: &SessionState) -> &str {
    if state.pending_question.is_some() {
        return "Awaiting input";
    }
    match state.process_state {
        RuntimeDisplayState::Thinking => "Thinking",
        RuntimeDisplayState::Responding => "Responding",
        RuntimeDisplayState::ToolRunning => state
            .tool_projection
            .active_description()
            .or(state.active_tool_label.as_deref())
            .unwrap_or("Running tools"),
        RuntimeDisplayState::Compacting => "Compacting",
        RuntimeDisplayState::Waiting(WaitReason::Model) => "Waiting for model",
        RuntimeDisplayState::Waiting(WaitReason::Tasks) => "Waiting for tasks",
        RuntimeDisplayState::Waiting(_) => "Waiting",
        RuntimeDisplayState::WaitingUser => "Awaiting input",
        RuntimeDisplayState::ConnectionLost => "Connection lost",
        RuntimeDisplayState::PendingPhase | RuntimeDisplayState::Idle => "Working",
    }
}
