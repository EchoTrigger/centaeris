use super::*;
use centaeris_cli_runtime::session_catalog_workspace_key;
use ratatui::layout::{Constraint, Flex, Layout, Margin};
use ratatui::style::Color;
use ratatui::widgets::{Block, Padding};
use serde::Deserialize;
use std::collections::HashSet;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const PAGE_SIZE: usize = 40;
const BODY_SEARCH_LIMIT: usize = 20;
const BODY_SEARCH_DELAY: Duration = Duration::from_millis(250);
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
            Self::Idle => (" ", theme().muted),
            Self::Working => (WORKING_FRAMES[frame], Color::Rgb(187, 154, 247)),
            Self::NeedsInput => ("!", Color::Rgb(224, 175, 104)),
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
    pub(super) id: String,
    pub(super) title: String,
    pub(super) cwd: String,
    #[serde(default)]
    pub(super) is_pinned: bool,
    #[serde(default)]
    pub(super) updated_at: i64,
}

#[derive(Deserialize)]
struct BodyHit {
    #[serde(flatten)]
    entry: SessionEntry,
    snippet: Option<String>,
}

#[derive(Deserialize)]
struct BodySearchResponse {
    items: Vec<BodyHit>,
}

#[derive(Default)]
struct BodySearch {
    due: Option<Instant>,
    response: Option<RuntimeResponse>,
    hits: Vec<BodyHit>,
}

#[derive(Clone, Copy)]
struct Match<'a> {
    entry: &'a SessionEntry,
    hit: Option<&'a BodyHit>,
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
    search: Rect,
    list: Rect,
    rows: Rect,
    first_row: usize,
    row_sessions: Vec<Option<usize>>,
    arm_delete: Rect,
    rename: Rect,
    pin: Rect,
    confirm_delete: Rect,
    cancel: Rect,
}

fn workspace_label(cwd: &str) -> String {
    let mut parts = std::path::Path::new(cwd)
        .components()
        .rev()
        .filter_map(|part| match part {
            std::path::Component::Normal(name) => name.to_str(),
            _ => None,
        });
    match (parts.next(), parts.next()) {
        (Some(name), Some(parent)) => format!("{parent}-{name}"),
        (Some(name), None) => name.to_owned(),
        _ => cwd.to_owned(),
    }
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
    request: Option<Request>,
    confirm: Option<SessionEntry>,
    rename: Option<RenameDraft>,
    notice: Option<String>,
    query: String,
    searching: bool,
    body_search: BodySearch,
    expanded: HashSet<String>,
    selected: usize,
    opened_at: Instant,
    hits: HitRegions,
}

impl SessionPicker {
    fn matches<'a>(&'a self, catalog: &'a Catalog) -> impl Iterator<Item = Match<'a>> {
        let query = self.query.trim().to_lowercase();
        let mut matches = catalog
            .entries
            .iter()
            .filter(|entry| {
                query.is_empty()
                    || entry.title.to_lowercase().contains(&query)
                    || entry.id.to_lowercase().contains(&query)
            })
            .collect::<Vec<_>>();
        matches.sort_by_cached_key(|entry| {
            let key = session_catalog_workspace_key(&entry.cwd);
            (key != catalog.workspace_key, workspace_label(&key), key)
        });
        let mut seen: HashSet<&str> = matches.iter().map(|entry| entry.id.as_str()).collect();
        matches
            .into_iter()
            .map(|entry| Match { entry, hit: None })
            .chain(self.body_search.hits.iter().filter_map(move |hit| {
                seen.insert(&hit.entry.id).then_some(Match {
                    entry: &hit.entry,
                    hit: Some(hit),
                })
            }))
    }

    fn selected_entry<'a>(&'a self, catalog: &'a Catalog) -> Option<&'a SessionEntry> {
        self.matches(catalog)
            .nth(self.selected)
            .map(|item| item.entry)
    }

    fn restore_selection(&mut self, catalog: &Catalog, id: Option<&str>) {
        let selected = id.and_then(|id| self.matches(catalog).position(|item| item.entry.id == id));
        self.selected = selected.unwrap_or_else(|| {
            self.selected
                .min(self.matches(catalog).count().saturating_sub(1))
        });
        self.hits.invalidate_clicks();
    }

    fn schedule_search(&mut self) {
        // Each picker owns the receiver for its current query. An earlier reply has no receiver
        // after the query is edited or the picker is closed.
        self.body_search = BodySearch {
            due: (!self.query.trim().is_empty()).then(|| Instant::now() + BODY_SEARCH_DELAY),
            ..BodySearch::default()
        };
    }

    fn query_changed(&mut self) {
        self.selected = 0;
        self.expanded.clear();
        self.notice = None;
        self.hits.invalidate_clicks();
        self.schedule_search();
    }

    fn move_selection(&mut self, catalog: &Catalog, delta: isize) {
        self.selected = self
            .selected
            .saturating_add_signed(delta)
            .min(self.matches(catalog).count().saturating_sub(1));
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

#[derive(Default)]
pub(super) struct Catalog {
    pub(super) entries: Vec<SessionEntry>,
    workspace_key: String,
    cwd: Option<String>,
    next: Option<PageCursor>,
    request: Option<RuntimeResponse>,
    pub(super) notice: Option<String>,
}

impl Catalog {
    pub(super) fn scoped_to(&self, cwd: &str) -> bool {
        self.cwd.is_some() && self.workspace_key == session_catalog_workspace_key(cwd)
    }

    pub(super) fn loading(&self) -> bool {
        self.request.is_some()
    }

    pub(super) fn has_more(&self) -> bool {
        self.next.is_some()
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
    app.session.show_help = false;
    app.session.message = None;
    mouse::cancel_capture(app);
    let mut picker = SessionPicker {
        request: None,
        confirm: None,
        rename: None,
        notice: None,
        query: query.trim().to_owned(),
        searching: !query.trim().is_empty(),
        body_search: BodySearch::default(),
        expanded: HashSet::new(),
        selected: 0,
        opened_at: Instant::now(),
        hits: HitRegions::default(),
    };
    picker.schedule_search();
    app.session_picker = Some(picker);
    reload_catalog(app);
    Ok(())
}

pub(super) fn reload_catalog(app: &mut App) {
    let workspace = &app.session.workspace_root;
    app.session_catalog = Catalog {
        workspace_key: session_catalog_workspace_key(workspace),
        cwd: (app.session_picker.is_none() && dashboard::visible(app)).then(|| workspace.clone()),
        next: Some(first_page()),
        ..Catalog::default()
    };
    request_page(app);
}

pub(super) fn request_page(app: &mut App) {
    let catalog = &mut app.session_catalog;
    let Some(next) = catalog.next.as_ref().filter(|_| catalog.request.is_none()) else {
        return;
    };
    let result = app
        .runtime
        .as_mut()
        .ok_or_else(|| "Runtime is disconnected".to_string())
        .and_then(|runtime| {
            runtime.request_async(
                "session/catalog",
                json!({"request": {
                    "cwd": catalog.cwd,
                    "mode": next.mode,
                    "cursor": next.cursor,
                    "limit": PAGE_SIZE
                }}),
            )
        });
    match result {
        Ok(response) => {
            catalog.request = Some(response);
            catalog.notice = None;
        }
        Err(error) => catalog.notice = Some(error),
    }
}

pub(super) fn poll(app: &mut App) -> bool {
    let catalog_changed =
        (app.session_picker.is_some() || dashboard::visible(app)) && poll_catalog(app);
    let management_changed = poll_management(app);
    let search_changed = poll_body_search(app);
    catalog_changed || management_changed || search_changed
}

fn poll_body_search(app: &mut App) -> bool {
    let Some(picker) = app.session_picker.as_mut() else {
        return false;
    };
    if picker
        .body_search
        .due
        .is_some_and(|due| due <= Instant::now())
    {
        picker.body_search.due = None;
        let result = app
            .runtime
            .as_mut()
            .ok_or_else(|| "Runtime is disconnected".to_string())
            .and_then(|runtime| {
                runtime.request_async(
                    "_centaeris/session/search",
                    json!({"request": {"query": picker.query.trim(), "limit": BODY_SEARCH_LIMIT}}),
                )
            });
        match result {
            Ok(response) => picker.body_search.response = Some(response),
            Err(error) => picker.notice = Some(error),
        }
        return true;
    }
    let Some(response) = picker.body_search.response.as_ref() else {
        return false;
    };
    let result = match response.try_recv() {
        Ok(None) => return false,
        Ok(Some(value)) => serde_json::from_value::<BodySearchResponse>(value)
            .map_err(|error| format!("Invalid session search result: {error}")),
        Err(error) => Err(error.to_string()),
    };
    picker.body_search.response = None;
    match result {
        Ok(result) => {
            let catalog = &app.session_catalog;
            let id = picker.selected_entry(catalog).map(|entry| entry.id.clone());
            picker.body_search.hits = result.items;
            picker.restore_selection(catalog, id.as_deref());
        }
        Err(error) => picker.notice = Some(error),
    }
    true
}

fn poll_catalog(app: &mut App) -> bool {
    let Some(request) = app.session_catalog.request.as_ref() else {
        return false;
    };
    let page = match request.try_recv() {
        Ok(None) => return false,
        Ok(Some(value)) => serde_json::from_value::<CatalogPage>(value)
            .map_err(|error| format!("Invalid session catalog: {error}")),
        Err(error) => Err(error.to_string()),
    };
    app.session_catalog.request = None;
    match page {
        Ok(page) => receive_page(app, page),
        Err(error) => app.session_catalog.notice = Some(error),
    }
    true
}

fn poll_management(app: &mut App) -> bool {
    let Some(request) = app
        .session_picker
        .as_ref()
        .and_then(|picker| picker.request.as_ref())
    else {
        return false;
    };
    let result = match request {
        Request::Delete { response, .. } | Request::Update { response, .. } => response.try_recv(),
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
        (Request::Delete { target, .. }, Ok(value)) => match delete_receipt(value, &target.id) {
            Ok(deleted) => {
                app.parked_sessions.retain(|id, _| !deleted.contains(id));
                picker.selected = 0;
                picker.hits = HitRegions::default();
                picker.expanded.retain(|id| !deleted.contains(id));
                picker.schedule_search();
                if app
                    .session
                    .active_session
                    .as_ref()
                    .is_some_and(|s| deleted.contains(&s.id))
                {
                    app.session.active_session = None;
                    app.session.catalog_entry = None;
                    app.session.dashboard_replies.clear();
                    app.session.read_only_session = None;
                    app.session.transcript.clear();
                    app.session.transcript_paging = None;
                    app.session.transcript_history_len = 0;
                    reset_session_view(app, Vec::new());
                    status_usage::restore(app);
                }
                reload_catalog(app);
            }
            Err(error) => picker.notice = Some(error),
        },
        (Request::Update { id, .. }, Ok(value)) => {
            match serde_json::from_value::<SessionEntry>(value) {
                Ok(entry) if entry.id == id => {
                    if let Some(state) = session_views::loaded_mut(app, &id) {
                        state.catalog_entry = Some(entry);
                    }
                    let picker = app.session_picker.as_mut().expect("open picker");
                    picker.selected = 0;
                    picker.hits = HitRegions::default();
                    picker.schedule_search();
                    reload_catalog(app);
                }
                Ok(_) => picker.notice = Some("Session update returned a different session".into()),
                Err(error) => picker.notice = Some(format!("Invalid session update: {error}")),
            }
        }
    }
    true
}

fn receive_page(app: &mut App, page: CatalogPage) {
    let catalog = &mut app.session_catalog;
    let next = catalog.next.take().expect("requested page cursor");
    if page.reset {
        if let Some(picker) = app.session_picker.as_mut() {
            picker.selected = 0;
        }
        reload_catalog(app);
        return;
    }
    let selected = app
        .session_picker
        .as_ref()
        .and_then(|picker| picker.selected_entry(catalog))
        .map(|entry| entry.id.clone());
    for entry in page.items {
        if !catalog.entries.iter().any(|known| known.id == entry.id) {
            catalog.entries.push(entry);
        }
    }
    if let Some(picker) = app.session_picker.as_mut() {
        picker.restore_selection(catalog, selected.as_deref());
    }
    let start_recent = next.mode == "pinned" && page.next_cursor.is_none();
    catalog.next = if start_recent {
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
    if start_recent
        || app
            .session_picker
            .as_ref()
            .is_some_and(|picker| !picker.query.is_empty())
        || dashboard::fetch_pinned_pages(app) && next.mode == "pinned"
    {
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
        .session
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
    app.session.output_preview.clear();
    app.session.expanded_bash_tools.clear();
    app.session.transcript_scroll = 0;
    app.session.transcript_max_scroll = 0;
    app.session.transcript_follow_bottom = true;
    app.session.transcript_selection = None;
    app.session.snapshot_revisions.clear();
    app.session.run_activity.clear();
    app.session.seen_stream_event_ids.clear();
    app.session.presentation_run_id = None;
    app.session.pending_question = None;
    app.session.pending_history = None;
    app.session.history_load_failed = false;
    app.session.subagents = subagents::State::default();
    clear_assistant_buffer(&mut app.session);
    restore_active_agent_runs(&mut app.session, active_runs);
    invalidate_transcript_layout(&mut app.session);
}

pub(super) fn detach_writer(
    runtime: &mut RuntimeClient,
    session: &TuiSession,
) -> Result<(), String> {
    let detached = runtime
        .session_request(
            SessionCommand::Unsubscribe,
            json!({"sessionId": session.id, "viewerId": tui_viewer_id()}),
        )
        .map_err(String::from);
    let released = runtime.release_session_writer(&session.id);
    detached.and(released)
}

pub(super) fn open_history(app: &mut App, entry: &SessionEntry) -> Result<(), String> {
    activate(app, entry, Activation::OpenHistory)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Activation {
    OpenHistory,
    RetryWriter,
    Fork,
}

enum History {
    Pending(PendingHistory),
    Ready(Box<SessionRestore>),
}

fn activate(app: &mut App, entry: &SessionEntry, activation: Activation) -> Result<(), String> {
    if app.session.read_only_session.is_none()
        && app
            .session
            .active_session
            .as_ref()
            .is_some_and(|session| session.id == entry.id)
    {
        app.session_picker = None;
        return Ok(());
    }
    if app.session.active_session.is_none()
        && !app.session.input.is_empty()
        && !dashboard::preserves_origin(app)
    {
        // A draft created before the first prompt needs a catalog entry to return to.
        ensure_active_session(app, "New session")?;
    }
    let session = TuiSession {
        id: entry.id.clone(),
    };
    if app.parked_sessions.contains_key(&entry.id) {
        return activate_loaded(app, &entry.id);
    }
    let claimed = app
        .runtime
        .as_mut()
        .ok_or("Runtime is disconnected")?
        .claim_session_writer(&entry.id)?;
    if !claimed && activation == Activation::Fork {
        return Err("New fork is already open in another window".into());
    }
    let prepared = (|| {
        if activation == Activation::OpenHistory {
            let (active_runs, entry) = if claimed {
                attach_session_viewer(app, &session.id)?;
                let active_runs = active_agent_runs(app, &session.id)?;
                (active_runs, bind_session(app, &session)?)
            } else {
                (Vec::new(), entry.clone())
            };
            let runtime = app.runtime.as_mut().ok_or("Runtime is disconnected")?;
            let history = PendingHistory::start(runtime, &session.id, active_runs)?;
            Ok((History::Pending(history), entry))
        } else if claimed {
            let (restore, entry) = load_writable_session(app, &session)?;
            Ok((History::Ready(Box::new(restore)), entry))
        } else {
            let restore = load_read_only_session_history(app, &session)?;
            Ok((History::Ready(Box::new(restore)), entry.clone()))
        }
    })();
    let (history, activated) = match prepared {
        Ok(prepared) => prepared,
        Err(error) => {
            if claimed {
                if let Some(runtime) = app.runtime.as_mut() {
                    let _ = detach_writer(runtime, &session);
                }
            }
            return Err(error);
        }
    };
    let same_read_only_session = app
        .session
        .read_only_session
        .as_ref()
        .is_some_and(|source| source.id == entry.id);
    let saved = same_read_only_session.then(|| {
        (
            editing::ComposerDraft::take(app),
            std::mem::take(&mut app.session.dashboard_replies),
        )
    });
    session_views::park_foreground(app);
    if same_read_only_session {
        app.parked_sessions.remove(&entry.id);
    }
    app.session.read_only_session = (!claimed).then(|| activated.clone());
    reset_session_view(app, Vec::new());
    app.session.session_cwd.clone_from(&activated.cwd);
    app.session.workspace_root.clone_from(&activated.cwd);
    app.session.catalog_entry = Some(activated);
    app.session.home_risk_pending = false;
    app.session.active_session = Some(session);
    app.session.message = None;
    match history {
        History::Pending(pending) => app.session.pending_history = Some(pending),
        History::Ready(restore) => {
            let restore = *restore;
            restore_active_agent_runs(&mut app.session, restore.active_agent_runs);
            app.session.transcript = restore.transcript;
            app.session.transcript_history_len = app.session.transcript.len();
            app.session.transcript_paging = Some(restore.transcript_paging);
            session_transcript::reset_history_images(&mut app.session);
            for payload in restore.replay_items {
                apply_stream_payload(&mut app.session, &payload);
            }
            note_empty_history(&mut app.session);
        }
    }
    status_usage::restore(app);
    app.session_picker = None;
    app.input_area = None;
    app.panel_area = None;
    if let Some((draft, replies)) = saved {
        draft.restore(app);
        app.session.dashboard_replies = replies;
    }
    Ok(())
}

pub(super) fn activate_loaded(app: &mut App, id: &str) -> Result<(), String> {
    if app
        .parked_sessions
        .get(id)
        .is_some_and(|state| state.read_only_session.is_some())
    {
        session_views::restore(app, id);
        return Ok(());
    }
    let session = TuiSession { id: id.to_owned() };
    let activated = bind_session(app, &session)?;
    session_views::restore(app, id);
    app.session.catalog_entry = Some(activated);
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
        updated_at: 0,
    };
    activate(app, &target, Activation::Fork)?;
    clear_composer(app);
    insert_text_at_cursor(app, &fork.draft);
    app.session.message = None;
    app.session.next_image_number = fork
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
    app.session.draft_image_attachments = ranges
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
        .session
        .read_only_session
        .as_ref()
        .expect("read-only session")
        .clone();
    let result = match key.code {
        KeyCode::Char('r' | 'R')
            if key.kind == KeyEventKind::Press
                && app.session.pending_history.is_none()
                && !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
        {
            let mode = if app.session.history_load_failed {
                Activation::OpenHistory
            } else {
                Activation::RetryWriter
            };
            activate(app, &source, mode)
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
            app.session.transcript_scroll = 0;
            app.session.transcript_follow_bottom = false;
            load_older_transcript_page(app).map(|_| ())
        }
        KeyCode::End if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.session.transcript_scroll = app.session.transcript_max_scroll;
            app.session.transcript_follow_bottom = true;
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
        .and_then(|picker| picker.selected_entry(&app.session_catalog))
        .cloned();
    if let Some(entry) = entry {
        if let Err(error) = open_history(app, &entry) {
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
    let before = picker.query.len();
    picker
        .query
        .extend(text.chars().filter(|c| !c.is_control()));
    if picker.query.len() != before {
        picker.query_changed();
        request_page(app);
    }
}

fn begin_rename(picker: &mut SessionPicker, catalog: &Catalog) {
    let entry = picker.selected_entry(catalog).cloned();
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
        .and_then(|picker| picker.selected_entry(&app.session_catalog))
        .cloned();
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
        && key.code == KeyCode::Char('/')
        && !picker.query.trim().is_empty()
    {
        if picker.body_search.response.is_none() {
            picker.notice = None;
            picker.body_search.due = Some(Instant::now());
            poll_body_search(app);
        }
        return;
    }
    if key.kind == KeyEventKind::Press
        && key.modifiers == KeyModifiers::CONTROL
        && picker.request.is_none()
        && !app.session_catalog.loading()
    {
        match key.code {
            KeyCode::Char('r') => {
                begin_rename(picker, &app.session_catalog);
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
            picker.query_changed();
        }
        KeyCode::Esc => app.session_picker = None,
        KeyCode::Up | KeyCode::Down if picker.searching => picker.searching = false,
        KeyCode::Up => picker.move_selection(&app.session_catalog, -1),
        KeyCode::Down | KeyCode::PageDown => {
            let step = if key.code == KeyCode::Down {
                1
            } else {
                picker.hits.list.height.max(1) as isize
            };
            picker.move_selection(&app.session_catalog, step);
            if picker.selected + 1 >= picker.matches(&app.session_catalog).count() {
                request_page(app);
            }
        }
        KeyCode::PageUp => picker.move_selection(
            &app.session_catalog,
            -(picker.hits.list.height.max(1) as isize),
        ),
        KeyCode::Enter => activate_selected(app),
        KeyCode::Backspace => {
            if let Some((start, _)) = picker.query.grapheme_indices(true).next_back() {
                picker.query.truncate(start);
                picker.query_changed();
            }
        }
        KeyCode::Char('d') if !picker.searching && key.modifiers.is_empty() => {
            let target = picker
                .selected_entry(&app.session_catalog)
                .filter(|_| picker.request.is_none() && !app.session_catalog.loading())
                .cloned();
            picker.confirm_delete(target);
        }
        KeyCode::Right | KeyCode::Left | KeyCode::Char('e' | 'E')
            if !picker.searching
                && !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
        {
            if let Some(id) = picker
                .selected_entry(&app.session_catalog)
                .map(|entry| entry.id.clone())
            {
                if matches!(key.code, KeyCode::Right | KeyCode::Char('e')) {
                    picker.expanded.insert(id);
                } else {
                    picker.expanded.remove(&id);
                }
                picker.hits.invalidate_clicks();
            }
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
        } else if picker.hits.rename.contains(position)
            && picker.request.is_none()
            && !app.session_catalog.loading()
        {
            begin_rename(picker, &app.session_catalog);
        } else if picker.hits.pin.contains(position)
            && picker.request.is_none()
            && !app.session_catalog.loading()
        {
            toggle_pin(app);
        } else if picker.confirm.is_none()
            && picker.hits.arm_delete.contains(position)
            && picker.request.is_none()
            && !app.session_catalog.loading()
        {
            let target = picker.selected_entry(&app.session_catalog).cloned();
            picker.confirm_delete(target);
        } else if picker.confirm.is_none() && picker.hits.search.contains(position) {
            picker.searching = true;
        } else if picker.confirm.is_none() && picker.hits.rows.contains(position) {
            let row = picker.hits.first_row + usize::from(event.row - picker.hits.rows.y);
            if let Some(selected) = picker.hits.row_sessions.get(row).copied().flatten() {
                picker.selected = selected;
                activate_selected(app);
            }
        }
    } else if picker.confirm.is_none() && picker.hits.list.contains(position) {
        match event.kind {
            MouseEventKind::ScrollUp => picker.move_selection(&app.session_catalog, -3),
            MouseEventKind::ScrollDown => {
                picker.move_selection(&app.session_catalog, 3);
                if picker.selected + 1 >= picker.matches(&app.session_catalog).count() {
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
        picker.hits.first_row..picker.hits.row_sessions.len(),
        picker
            .hits
            .row_sessions
            .iter()
            .position(|index| *index == Some(picker.selected))
            .unwrap_or(0),
    ))
}

pub(super) fn scroll_to(app: &mut App, selected: usize) {
    let picker = app.session_picker.as_mut().expect("open picker");
    if picker.confirm.is_some() || picker.deleting() {
        return;
    }
    let total = picker.matches(&app.session_catalog).count();
    picker.selected = picker
        .hits
        .row_sessions
        .iter()
        .skip(selected)
        .find_map(|index| *index)
        .unwrap_or_else(|| total.saturating_sub(1));
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
        .session
        .active_session
        .as_ref()
        .is_some_and(|session| session.id == id)
    {
        return Activity::from_run(
            has_active_agent_run(&app.session),
            app.session.process_state,
        );
    }
    app.parked_sessions
        .get(id)
        .map_or(Activity::Idle, SessionState::activity)
}

pub(super) fn has_working_row(app: &App) -> bool {
    app.session_picker.as_ref().is_some_and(|picker| {
        let entries: Vec<_> = picker.matches(&app.session_catalog).collect();
        picker
            .hits
            .row_sessions
            .iter()
            .skip(picker.hits.first_row)
            .take(usize::from(picker.hits.rows.height))
            .filter_map(|index| entries.get((*index)?))
            .any(|entry| activity(app, &entry.entry.id) == Activity::Working)
    })
}

fn activity_time(updated_at: i64, now: i64) -> String {
    if updated_at <= 0 {
        return String::new();
    }
    let minutes = now.saturating_sub(updated_at).max(0) / 60_000;
    match minutes {
        0 => "now".into(),
        1..=59 => format!("{minutes}m ago"),
        60..=1_439 => format!("{}h ago", minutes / 60),
        _ => format!("{}d ago", minutes / 1_440),
    }
}

fn session_row(
    item: Match<'_>,
    width: u16,
    expanded: bool,
    activity: Span<'static>,
    now: i64,
) -> ListItem<'static> {
    let entry = item.entry;
    let fold = if expanded { "⌄ " } else { "› " };
    let pin = if entry.is_pinned { "* " } else { "" };
    let badge = if item.hit.and_then(|hit| hit.snippet.as_ref()).is_some() {
        " match"
    } else {
        ""
    };
    let time = activity_time(entry.updated_at, now);
    let width = usize::from(width);
    let fixed = fold.width() + activity.width() + 1 + pin.len() + badge.len() + time.len();
    let title = fit_middle_columns(&entry.title, width.saturating_sub(fixed + 1));
    let remaining = width.saturating_sub(fixed + title.width());
    let muted = Style::default().fg(theme().muted);
    ListItem::new(Line::from(vec![
        Span::styled(fold, muted),
        activity,
        Span::raw(" "),
        Span::raw(pin),
        Span::raw(title),
        Span::styled(badge, muted),
        Span::raw(" ".repeat(remaining)),
        Span::styled(time, muted),
    ]))
}

pub(super) fn render(frame: &mut Frame, area: Rect, app: &mut App) {
    let picker = app.session_picker.as_ref().expect("open picker");
    if area.width < 32 || area.height < 10 {
        invalidate_geometry(app);
        frame.render_widget(Paragraph::new("Terminal too small."), area);
        return;
    }
    let inner = picker_view::content(frame, area);
    let window_width = ((u32::from(area.width) * 65 / 100) as u16)
        .clamp(48, 120)
        .min(inner.width);
    let [window] = Layout::horizontal([Constraint::Length(window_width)])
        .flex(Flex::Center)
        .areas(inner);
    let window = window.inner(Margin {
        horizontal: 0,
        vertical: if area.height >= 20 { 3 } else { 0 },
    });
    let muted = Style::default().fg(theme().muted);
    let border = Block::bordered()
        .border_style(muted)
        .title(Line::from(vec![
            Span::styled("─ ", muted),
            Span::styled(
                "Resume session",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled(" ", muted),
        ]))
        .padding(Padding::new(1, 1, u16::from(window.height >= 12), 0));
    let panel = border.inner(window);
    frame.render_widget(border, window);
    let width = panel.width;
    let [search, divider, list, status, actions] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(if picker.confirm.is_some() { 2 } else { 1 }),
        Constraint::Length(1),
    ])
    .areas(panel);
    frame.render_widget(
        Paragraph::new("─".repeat(usize::from(width))).style(muted),
        divider,
    );
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
        app.frame_caret = Some(Position::new(editor.x + (cursor - scroll) as u16, editor.y));
    } else {
        const LABEL: &str = "search: ";
        let query = fit_middle_columns(&picker.query, width.saturating_sub(9).into());
        if picker.searching && !picker.editing_list() {
            app.frame_caret = Some(Position::new(
                search.x + (LABEL.len() + query.width()) as u16,
                search.y,
            ));
        }
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(LABEL, muted),
                Span::raw(query),
            ])),
            search,
        );
    }

    let matches: Vec<_> = picker.matches(&app.session_catalog).collect();
    let selected = picker.selected.min(matches.len().saturating_sub(1));
    let list_body = Rect {
        width: list.width.saturating_sub(1),
        ..list
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |age| age.as_millis() as i64);
    let frame_index = (picker.opened_at.elapsed().as_millis() / STATUS_REDRAW_INTERVAL.as_millis())
        as usize
        % WORKING_FRAMES.len();
    let mut selected_row = 0;
    let mut selected_end = 0;
    let mut rows = Vec::new();
    let mut items = Vec::new();
    let mut previous_workspace = None;
    let mut has_content_section = false;
    let heading = |label: &str| {
        ListItem::new(fit_middle_columns(label, usize::from(list_body.width)))
            .style(muted.add_modifier(Modifier::BOLD))
    };
    for (index, item) in matches.iter().copied().enumerate() {
        let entry = item.entry;
        if item.hit.is_some() {
            if !has_content_section {
                rows.push(None);
                items.push(heading("Extended search results"));
                has_content_section = true;
            }
        } else {
            let key = session_catalog_workspace_key(&entry.cwd);
            if previous_workspace.as_ref() != Some(&key) {
                rows.push(None);
                items.push(heading(&workspace_label(&entry.cwd)));
                previous_workspace = Some(key);
            }
        }
        let expanded = picker.expanded.contains(&entry.id);
        let row = session_row(
            item,
            list_body.width,
            expanded,
            activity(app, &entry.id).span(frame_index),
            now,
        );
        if index == selected {
            selected_row = rows.len();
        }
        rows.push(Some(index));
        items.push(if index == selected {
            row.style(row_highlight(true))
        } else {
            row
        });
        if expanded {
            let snippet = item.hit.and_then(|hit| hit.snippet.as_deref());
            let detail_width = usize::from(list_body.width.saturating_sub(4));
            if let Some(snippet) = snippet {
                rows.push(Some(index));
                items.push(ListItem::new(format!("    {snippet}")).style(muted));
            }
            for text in [format!("ID  {}", entry.id), format!("CWD {}", entry.cwd)] {
                rows.push(Some(index));
                items.push(
                    ListItem::new(format!("    {}", fit_middle_columns(&text, detail_width)))
                        .style(muted),
                );
            }
        }
        if index == selected {
            selected_end = rows.len();
        }
    }
    let searching_body = picker.body_search.due.is_some() || picker.body_search.response.is_some();
    if !has_content_section && searching_body {
        rows.push(None);
        items.push(heading("Searching session content…"));
    }
    let offset = selected_end
        .saturating_sub(usize::from(list_body.height))
        .min(selected_row);
    let mut list_state = ListState::default()
        .with_offset(offset)
        .with_selected((!matches.is_empty()).then_some(selected_row));
    frame.render_stateful_widget(List::new(items), list_body, &mut list_state);
    let first_row = list_state.offset();
    let count = rows
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
    } else if let Some(notice) = picker
        .notice
        .as_ref()
        .or(app.session_catalog.notice.as_ref())
    {
        notice.clone()
    } else if app.session_catalog.loading() {
        "Loading sessions…".into()
    } else if matches.is_empty() && !searching_body {
        "No matching sessions".into()
    } else if picker.rename.is_none() {
        "↑↓ Enter · e Expand · / Search · Esc Back".into()
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
        search: if picker.editing_list() {
            Rect::default()
        } else {
            search
        },
        list: if !picker.editing_list() {
            list
        } else {
            Rect::default()
        },
        rows: visible_rows,
        first_row,
        row_sessions: rows,
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
    } else if count > 0 && picker.request.is_none() && !app.session_catalog.loading() {
        let pin = if matches
            .get(selected)
            .is_some_and(|item| item.entry.is_pinned)
        {
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
