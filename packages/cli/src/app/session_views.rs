use super::*;

// The foreground uses the same reducer as a parked session. Keep the fields
// it reads or writes together when routing an update through that reducer.
macro_rules! session_view {
    ($($field:ident: $ty:ty),+ $(,)?) => {
        #[derive(Default)]
        pub(super) struct SessionView {
            $($field: $ty,)+
            refresh_pending: bool,
            edit_history: editing::History,
            status_usage: status_usage::StatusUsage,
        }

        impl SessionView {
            fn swap(&mut self, app: &mut App) {
                $(std::mem::swap(&mut app.$field, &mut self.$field);)+
                app.edit_history.swap_session(&mut self.edit_history);
                app.status_usage.swap_session(&mut self.status_usage);
            }
        }
    };
}

session_view! {
    workspace_root: String,
    session_cwd: String,
    home_risk_pending: bool,
    home_risk_panel: Option<HomeRiskPanel>,
    input: String,
    input_cursor: usize,
    input_selection_anchor: Option<usize>,
    input_layout: Option<text_layout::TextLayout>,
    input_scroll: usize,
    input_scroll_override: bool,
    retired_image_paths: HashSet<PathBuf>,
    draft_image_attachments: Vec<DraftImageAttachment>,
    next_image_number: usize,
    image_preview: Option<ImagePreview>,
    image_preview_area: Option<Rect>,
    output_preview: HashMap<String, output_preview::OutputPreview>,
    expanded_bash_tools: HashSet<String>,
    pending_esc_stop: bool,
    message: Option<String>,
    show_help: bool,
    selected_command: usize,
    active_session: Option<TuiSession>,
    read_only_session: Option<session_picker::SessionEntry>,
    transcript: Vec<TranscriptLine>,
    transcript_paging: Option<TranscriptPagingState>,
    transcript_history_len: usize,
    transcript_revision: u64,
    transcript_layout_cache: Option<TranscriptLayoutCache>,
    transcript_scroll: u64,
    transcript_max_scroll: u64,
    transcript_follow_bottom: bool,
    snapshot_revisions: HashMap<String, u64>,
    text_layout: Option<std::sync::Arc<text_layout::TextLayout>>,
    text_scroll: usize,
    transcript_selection: Option<TextSelection>,
    tool_projection: ToolProjection,
    pending_subagent_lines: Vec<TranscriptLine>,
    assistant_buffer: String,
    assistant_emitted_bytes: usize,
    assistant_tail_in_code_block: bool,
    assistant_stream_started: bool,
    assistant_stream_start: Option<usize>,
    presentation_run_id: Option<String>,
    assistant_run_id: Option<String>,
    assistant_is_final: bool,
    run_activity: HashMap<String, runs::RunState>,
    active_tool_label: Option<String>,
    active_agent_run_id: Option<String>,
    active_agent_run_ids: HashSet<String>,
    completed_agent_run_ids: HashSet<String>,
    seen_subagent_ids: HashSet<String>,
    live_subagent_ids: HashSet<String>,
    seen_stream_event_ids: HashSet<String>,
    agent_run_started_at: Option<Instant>,
    process_state: RuntimeDisplayState,
    status_easter_egg: StatusEasterEgg,
    pending_question: Option<PendingQuestion>,
    tool_protocol_error: bool,
}

impl Drop for SessionView {
    fn drop(&mut self) {
        for path in self
            .draft_image_attachments
            .drain(..)
            .map(|image| image.local_path)
            .chain(self.retired_image_paths.drain())
        {
            let _ = std::fs::remove_file(path);
        }
    }
}

impl SessionView {
    pub(super) fn activity(&self) -> session_picker::Activity {
        session_picker::Activity::from_run(
            self.active_agent_run_id.is_some() || !self.active_agent_run_ids.is_empty(),
            self.process_state,
        )
    }
}

pub(super) fn park_foreground(app: &mut App) {
    mouse::resize(app);
    app.input_area = None;
    app.panel_area = None;
    app.transcript_area = None;
    app.transcript_viewport = None;
    close_image_preview(app);
    app.inline_images.clear();
    app.inline_image_cache_order.clear();
    app.inline_image_cache_bytes = 0;
    app.inline_image_errors.clear();
    let mut view = SessionView::default();
    view.next_image_number = 1;
    view.transcript_follow_bottom = true;
    view.swap(app);
    if view.read_only_session.is_none() {
        if let Some(session) = view.active_session.as_ref() {
            app.parked_sessions.insert(session.id.clone(), view);
        }
    }
}

pub(super) fn new_session(app: &mut App) {
    let cwd = app.session_cwd.clone();
    let workspace = app.workspace_root.clone();
    let warn_home = app.home_risk_pending;
    clear_composer(app);
    park_foreground(app);
    app.session_cwd = cwd;
    app.workspace_root = workspace;
    app.home_risk_pending = warn_home;
}

pub(super) fn restore(app: &mut App, id: &str) {
    let Some(mut view) = app.parked_sessions.remove(id) else {
        return;
    };
    park_foreground(app);
    view.swap(app);
    status_usage::restore(app);
    if view.refresh_pending {
        if let Err(error) = refresh_transcript_patches(app) {
            show_message(app, error);
        }
    }
    app.session_picker = None;
}

pub(super) fn route_update(app: &mut App, update: &Value) -> bool {
    let Some(id) = update.get("sessionId").and_then(Value::as_str) else {
        return false;
    };
    let committed =
        update.pointer("/payload/type").and_then(Value::as_str) == Some("session_event");
    if app
        .active_session
        .as_ref()
        .is_some_and(|session| session.id == id)
    {
        if app.read_only_session.is_some() {
            return false;
        }
        invalidate_transcript_layout(app);
        apply_session_update(app, update);
        if committed {
            if let Err(error) = refresh_transcript_patches(app) {
                show_message(app, error);
            }
        }
        return true;
    }
    if let Some(mut view) = app.parked_sessions.remove(id) {
        let previous_activity = view.activity();
        view.swap(app);
        invalidate_transcript_layout(app);
        apply_session_update(app, update);
        view.swap(app);
        // Reconcile the durable transcript once when the user returns to it.
        view.refresh_pending |= committed;
        let activity_changed = previous_activity != view.activity();
        app.parked_sessions.insert(id.to_owned(), view);
        return activity_changed && app.session_picker.is_some();
    }
    false
}
