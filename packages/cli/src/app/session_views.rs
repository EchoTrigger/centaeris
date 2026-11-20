use super::*;

#[derive(Default)]
pub(super) struct SessionState {
    pub(super) workspace_root: String,
    pub(super) session_cwd: String,
    pub(super) home_risk_pending: bool,
    pub(super) home_risk_panel: Option<HomeRiskPanel>,
    pub(super) input: String,
    pub(super) input_cursor: usize,
    pub(super) input_selection_anchor: Option<usize>,
    pub(super) input_layout: Option<text_layout::TextLayout>,
    pub(super) input_scroll: usize,
    pub(super) input_scroll_override: bool,
    pub(super) retired_image_paths: HashSet<PathBuf>,
    pub(super) draft_image_attachments: Vec<DraftImageAttachment>,
    pub(super) next_image_number: usize,
    pub(super) image_preview: Option<ImagePreview>,
    pub(super) output_preview: HashMap<String, output_preview::OutputPreview>,
    pub(super) expanded_bash_tools: HashSet<String>,
    pub(super) pending_esc_stop: bool,
    pub(super) message: Option<String>,
    pub(super) show_help: bool,
    pub(super) selected_command: usize,
    pub(super) active_session: Option<TuiSession>,
    pub(super) catalog_entry: Option<session_picker::SessionEntry>,
    pub(super) dashboard_replies: VecDeque<dashboard::QueuedReply>,
    pub(super) subagents: subagents::State,
    pub(super) read_only_session: Option<session_picker::SessionEntry>,
    pub(super) pending_history: Option<session_transcript::PendingHistory>,
    pub(super) history_load_failed: bool,
    pub(super) transcript: Vec<TranscriptLine>,
    pub(super) transcript_paging: Option<TranscriptPagingState>,
    pub(super) transcript_history_len: usize,
    pub(super) history_images: session_transcript::HistoryImages,
    pub(super) transcript_revision: u64,
    pub(super) transcript_layout_cache: Option<TranscriptLayoutCache>,
    pub(super) transcript_scroll: u64,
    pub(super) transcript_max_scroll: u64,
    pub(super) transcript_follow_bottom: bool,
    pub(super) snapshot_revisions: HashMap<String, u64>,
    pub(super) text_layout: Option<std::sync::Arc<text_layout::TextLayout>>,
    pub(super) text_scroll: usize,
    pub(super) transcript_selection: Option<TextSelection>,
    pub(super) tool_projection: ToolProjection,
    pub(super) pending_subagent_lines: Vec<TranscriptLine>,
    pub(super) assistant_buffer: String,
    pub(super) assistant_emitted_bytes: usize,
    pub(super) assistant_tail_in_code_block: bool,
    pub(super) assistant_stream_started: bool,
    pub(super) assistant_stream_start: Option<usize>,
    pub(super) presentation_run_id: Option<String>,
    pub(super) assistant_run_id: Option<String>,
    pub(super) assistant_is_final: bool,
    pub(super) run_activity: HashMap<String, runs::RunState>,
    pub(super) active_tool_label: Option<String>,
    pub(super) active_agent_run_id: Option<String>,
    pub(super) active_agent_run_ids: HashSet<String>,
    pub(super) completed_agent_run_ids: HashSet<String>,
    pub(super) seen_subagent_ids: HashSet<String>,
    pub(super) live_subagent_ids: HashSet<String>,
    pub(super) seen_stream_event_ids: HashSet<String>,
    pub(super) agent_run_started_at: Option<Instant>,
    pub(super) process_state: RuntimeDisplayState,
    pub(super) status_easter_egg: StatusEasterEgg,
    pub(super) pending_question: Option<PendingQuestion>,
    pub(super) question_scroll: usize,
    pub(super) tool_protocol_error: bool,
    pub(super) edit_history: editing::History,
    pub(super) status_usage: status_usage::StatusUsage,
    pub(super) refresh_pending: bool,
}

impl Drop for SessionState {
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

impl SessionState {
    pub(super) fn empty() -> Self {
        let mut state = Self::default();
        state.next_image_number = 1;
        state.transcript_follow_bottom = true;
        state
    }

    pub(super) fn startup(config: AppConfig) -> Self {
        let mut state = Self::empty();
        state.workspace_root = display_path(&config.workspace_root);
        state.session_cwd = config.session_cwd;
        state.home_risk_pending = config.warn_home_on_first_prompt;
        state
    }

    pub(super) fn activity(&self) -> session_picker::Activity {
        session_picker::Activity::from_run(
            self.active_agent_run_id.is_some() || !self.active_agent_run_ids.is_empty(),
            self.process_state,
        )
    }
}

pub(super) fn park_foreground(app: &mut App) {
    mouse::resize(app);
    close_image_preview(app);
    app.inline_images.clear();
    app.inline_image_cache_order.clear();
    app.inline_image_cache_bytes = 0;
    app.inline_image_errors.clear();
    app.foreground_generation = app.foreground_generation.wrapping_add(1);
    let view = std::mem::replace(&mut app.session, SessionState::empty());
    if let Some(session) = view
        .active_session
        .as_ref()
        .filter(|_| view.read_only_session.is_none() || view.history_load_failed)
    {
        app.parked_sessions.insert(session.id.clone(), view);
    } else {
        dashboard::park_origin(app, view);
    }
}

pub(super) fn new_session(app: &mut App) {
    clear_composer(app);
    open_new(app);
}

pub(super) fn open_new(app: &mut App) {
    let cwd = app.session.session_cwd.clone();
    let workspace = app.session.workspace_root.clone();
    let warn_home = app.session.home_risk_pending;
    park_foreground(app);
    app.session.session_cwd = cwd;
    app.session.workspace_root = workspace;
    app.session.home_risk_pending = warn_home;
}

pub(super) fn loaded<'a>(app: &'a App, id: &str) -> Option<&'a SessionState> {
    if is_foreground(app, id) {
        Some(&app.session)
    } else {
        app.parked_sessions.get(id)
    }
}

pub(super) fn loaded_mut<'a>(app: &'a mut App, id: &str) -> Option<&'a mut SessionState> {
    if is_foreground(app, id) {
        Some(&mut app.session)
    } else {
        app.parked_sessions.get_mut(id)
    }
}

fn is_foreground(app: &App, id: &str) -> bool {
    app.session
        .active_session
        .as_ref()
        .is_some_and(|session| session.id == id)
}

pub(super) fn restore(app: &mut App, id: &str) {
    let Some(view) = app.parked_sessions.remove(id) else {
        return;
    };
    park_foreground(app);
    app.session = view;
    status_usage::restore(app);
    if std::mem::take(&mut app.session.refresh_pending) {
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
    let mut child_changed = app.session.subagents.observe_session_update(update);
    for view in app.parked_sessions.values_mut() {
        child_changed |= view.subagents.observe_session_update(update);
    }
    let committed =
        update.pointer("/payload/type").and_then(Value::as_str) == Some("session_event");
    if app
        .session
        .active_session
        .as_ref()
        .is_some_and(|session| session.id == id)
    {
        if app.session.read_only_session.is_some() {
            return child_changed;
        }
        if let Some(pending) = &mut app.session.pending_history {
            pending.observe(update);
            return child_changed;
        }
        invalidate_transcript_layout(&mut app.session);
        apply_session_update(&mut app.session, update);
        if committed {
            if let Err(error) = refresh_transcript_patches(app) {
                show_message(app, error);
            }
        }
        true
    } else if let Some(view) = app.parked_sessions.get_mut(id) {
        if view.read_only_session.is_some() {
            return child_changed;
        }
        if let Some(pending) = &mut view.pending_history {
            pending.observe(update);
            return child_changed;
        }
        let previous_activity = view.activity();
        invalidate_transcript_layout(view);
        apply_session_update(view, update);
        // Reconcile the durable transcript once when the user returns to it.
        view.refresh_pending |= committed;
        let changed = previous_activity != view.activity();
        child_changed || dashboard::visible(app) || changed && app.session_picker.is_some()
    } else {
        child_changed
    }
}
