use super::*;
use centaeris_cli_runtime::rpc_client::TranscriptToolMediaRefs;

const HISTORY_IMAGE_CALLS_PER_READ: usize = 128;

#[derive(Default)]
pub(super) struct HistoryImages {
    scan_pending: bool,
    checked_calls: HashSet<String>,
    pending: Option<mpsc::Receiver<Result<TranscriptToolMediaRefs, String>>>,
}

pub(super) fn reset_history_images(state: &mut session_views::SessionState) {
    state.history_images = HistoryImages {
        scan_pending: true,
        ..HistoryImages::default()
    };
}

pub(super) fn poll_history_images(app: &mut App) -> bool {
    let received = app
        .session
        .history_images
        .pending
        .as_ref()
        .map(mpsc::Receiver::try_recv);
    let mut changed = false;
    match received {
        Some(Err(mpsc::TryRecvError::Empty)) => return false,
        Some(result) => {
            app.session.history_images.pending = None;
            changed = match result {
                Ok(Ok(refs)) => match apply_history_media_refs(&mut app.session, refs) {
                    Ok(changed) => changed,
                    Err(error) => {
                        show_message(app, error);
                        true
                    }
                },
                Ok(Err(error)) => {
                    show_message(app, error);
                    true
                }
                Err(_) => false,
            };
        }
        None => {}
    }
    let state = &mut app.session;
    if !state.history_images.scan_pending {
        return changed;
    }
    let (Some(paging), Some(runtime)) = (&state.transcript_paging, &app.runtime) else {
        return changed;
    };
    let call_ids: Vec<_> = state
        .transcript
        .iter()
        .take(state.transcript_history_len)
        .filter_map(|line| match line.content() {
            TranscriptLine::Tool(tool) if !tool.running => tool.key.strip_prefix("tool_call:"),
            _ => None,
        })
        .filter(|id| state.history_images.checked_calls.insert((*id).to_owned()))
        .take(HISTORY_IMAGE_CALLS_PER_READ)
        .map(str::to_owned)
        .collect();
    state.history_images.scan_pending = call_ids.len() == HISTORY_IMAGE_CALLS_PER_READ;
    if !call_ids.is_empty() {
        match runtime.transcript_tool_media_refs_async(paging.session_id().to_owned(), call_ids) {
            Ok(response) => state.history_images.pending = Some(response),
            Err(error) => {
                show_message(app, error);
                changed = true;
            }
        }
    }
    changed
}

pub(super) fn apply_history_media_refs(
    state: &mut session_views::SessionState,
    refs: TranscriptToolMediaRefs,
) -> Result<bool, String> {
    let mut media = refs
        .into_iter()
        .map(|(id, refs, videos)| {
            let images = crate::tool_projection::execution_images(&id, refs)?;
            Ok((format!("tool_call:{id}"), (images, videos)))
        })
        .collect::<Result<HashMap<_, _>, String>>()?;
    let mut changed = false;
    for line in state
        .transcript
        .iter_mut()
        .take(state.transcript_history_len)
    {
        if let TranscriptLine::Tool(tool) = line.content_mut() {
            if let Some((images, videos)) = media.remove(&tool.key) {
                if tool.images.is_empty() && !images.is_empty() {
                    tool.images = images;
                    changed = true;
                }
                if tool.video_paths.is_empty() && !videos.is_empty() {
                    tool.video_paths = videos;
                    changed = true;
                }
            }
        }
    }
    if changed {
        invalidate_transcript_layout(state);
    }
    Ok(changed)
}

pub(super) fn load_session_history(
    app: &mut App,
    session: &TuiSession,
) -> Result<SessionRestore, String> {
    ensure_runtime(app)?;
    attach_session_viewer(app, session.id.as_str())?;
    let active_agent_runs = active_agent_runs(app, session.id.as_str())?;
    let transcript_paging = load_transcript_snapshot(app, session)?;
    let transcript = transcript_paging.materialize_history(!active_agent_runs.is_empty());
    let replay_items = replay_active_agent_run_live_snapshots(app, &active_agent_runs)?;
    Ok(SessionRestore {
        transcript,
        transcript_paging,
        active_agent_runs,
        replay_items,
    })
}

pub(super) fn load_read_only_session_history(
    app: &mut App,
    session: &TuiSession,
) -> Result<SessionRestore, String> {
    let transcript_paging = load_transcript_snapshot(app, session)?;
    Ok(SessionRestore {
        transcript: transcript_paging.materialize_history(false),
        transcript_paging,
        active_agent_runs: Vec::new(),
        replay_items: Vec::new(),
    })
}

pub(super) struct PendingHistory {
    read: paging::PageRead,
    response: Option<RuntimeResponse>,
    next_poll: Instant,
    active_runs: Vec<ActiveAgentRun>,
    updates: Vec<Value>,
}

impl PendingHistory {
    pub(super) fn start(
        runtime: &mut RuntimeClient,
        session_id: &str,
        active_runs: Vec<ActiveAgentRun>,
    ) -> Result<Self, String> {
        let read = paging::PageRead::new(session_id, None, None);
        let response = runtime.request_async("transcript/page", read.request(None))?;
        Ok(Self {
            read,
            response: Some(response),
            next_poll: Instant::now(),
            active_runs,
            updates: Vec::new(),
        })
    }

    pub(super) fn observe(&mut self, update: &Value) {
        self.updates.push(update.clone());
    }

    fn poll_page(
        &mut self,
        runtime: &mut RuntimeClient,
    ) -> Result<Option<TranscriptPagingState>, String> {
        if let Some(response) = &self.response {
            let Some(value) = response.try_recv()? else {
                return Ok(None);
            };
            self.response = None;
            if let Some(page) = self.read.receive(value)? {
                return TranscriptPagingState::open(page).map(Some);
            }
            self.next_poll = Instant::now() + TRANSCRIPT_PROJECTION_POLL_INTERVAL;
        } else if Instant::now() >= self.next_poll {
            self.response =
                Some(runtime.request_async("transcript/page", self.read.request(None))?);
        }
        Ok(None)
    }
}

pub(super) fn poll_history(app: &mut App) -> bool {
    let mut changed = poll_session_history(&mut app.session, app.runtime.as_mut());
    if changed {
        status_usage::restore(app);
        if std::mem::take(&mut app.session.refresh_pending) {
            if let Err(error) = refresh_transcript_patches(app) {
                show_message(app, error);
            }
        }
    }
    for state in app.parked_sessions.values_mut() {
        changed |= poll_session_history(state, app.runtime.as_mut());
    }
    changed
}

fn poll_session_history(state: &mut SessionState, runtime: Option<&mut RuntimeClient>) -> bool {
    let Some(mut pending) = state.pending_history.take() else {
        return false;
    };
    let session = state.active_session.as_ref().expect("history target");
    let mut runtime = runtime;
    let result = (|| {
        let runtime = runtime.as_deref_mut().ok_or("Runtime is disconnected")?;
        let Some(paging) = pending.poll_page(runtime)? else {
            return Ok::<_, String>(None);
        };
        let replay = replay_live_snapshots(runtime, &pending.active_runs)?;
        Ok(Some((paging, replay)))
    })();
    match result {
        Ok(None) => {
            state.pending_history = Some(pending);
            return false;
        }
        Ok(Some((paging, replay))) => {
            state.transcript = paging.materialize_history(!pending.active_runs.is_empty());
            state.transcript_history_len = state.transcript.len();
            state.transcript_paging = Some(paging);
            reset_history_images(state);
            restore_active_agent_runs(state, pending.active_runs);
            for payload in replay {
                apply_stream_payload(state, &payload);
            }
            for update in pending.updates {
                apply_session_update(state, &update);
                state.refresh_pending |= update.pointer("/payload/type").and_then(Value::as_str)
                    == Some("session_event");
            }
            note_empty_history(state);
        }
        Err(error) => {
            state.history_load_failed = true;
            state.message = Some(error);
            if state.read_only_session.is_none() {
                if let Some(runtime) = runtime {
                    if let Err(error) = session_picker::detach_writer(runtime, session) {
                        state.message = Some(format!(
                            "{} · {error}",
                            state.message.as_deref().unwrap_or_default()
                        ));
                    }
                }
                state.read_only_session.clone_from(&state.catalog_entry);
            }
        }
    }
    invalidate_transcript_layout(state);
    true
}

pub(super) fn note_empty_history(state: &mut SessionState) {
    if state.read_only_session.is_none() && state.transcript.is_empty() {
        if let Some(entry) = &state.catalog_entry {
            state.message = Some(format!("Resumed empty session: {}", entry.title));
        }
    }
}

fn load_transcript_snapshot(
    app: &mut App,
    session: &TuiSession,
) -> Result<TranscriptPagingState, String> {
    let page = {
        let runtime = app
            .runtime
            .as_mut()
            .ok_or_else(|| "runtime client is not connected".to_string())?;
        paging::request_transcript_page_with(
            session.id.as_str(),
            None,
            None,
            None,
            |request| {
                runtime
                    .request("transcript/page", request)
                    .map_err(|error| error.to_string())
            },
            || thread::sleep(TRANSCRIPT_PROJECTION_POLL_INTERVAL),
        )?
    };
    TranscriptPagingState::open(page)
}

pub(super) fn load_older_transcript_page(app: &mut App) -> Result<bool, String> {
    let previous_total_rows = build_cached_transcript_view(app, app.render_width).total_rows;
    let previous_scroll = app.session.transcript_scroll;
    let Some(mut paging_state) = app.session.transcript_paging.take() else {
        return Ok(false);
    };
    let Some(older_cursor) = paging_state.older_cursor().map(str::to_string) else {
        app.session.transcript_paging = Some(paging_state);
        return Ok(false);
    };
    let page_result = match app.runtime.as_mut() {
        Some(runtime) => paging::request_transcript_page_with(
            paging_state.session_id(),
            Some(paging_state.projection_generation()),
            Some(paging_state.source_high_water()),
            Some(older_cursor.as_str()),
            |request| {
                runtime
                    .request("transcript/page", request)
                    .map_err(|error| error.to_string())
            },
            || thread::sleep(TRANSCRIPT_PROJECTION_POLL_INTERVAL),
        ),
        None => Err("runtime client is not connected".to_string()),
    };
    let page = match page_result {
        Ok(page) => page,
        Err(error) => {
            app.session.transcript_paging = Some(paging_state);
            return Err(error);
        }
    };
    if let Err(error) = paging_state.apply_older_page(page) {
        app.session.transcript_paging = Some(paging_state);
        return Err(error);
    }
    app.session.transcript_paging = Some(paging_state);
    sync_materialized_transcript_history(app);
    let next_total_rows = build_cached_transcript_view(app, app.render_width).total_rows;
    app.session.transcript_scroll =
        prepend_anchor_scroll(previous_scroll, previous_total_rows, next_total_rows);
    app.session.transcript_follow_bottom = false;
    Ok(true)
}

pub(super) fn refresh_transcript_patches(app: &mut App) -> Result<bool, String> {
    let Some(mut paging_state) = app.session.transcript_paging.take() else {
        return Ok(false);
    };
    let result = match app.runtime.as_mut() {
        Some(runtime) => paging::request_transcript_patches_with(
            &mut paging_state,
            |request| {
                runtime
                    .request("transcript/patches", request)
                    .map_err(|error| error.to_string())
            },
            || thread::sleep(TRANSCRIPT_PROJECTION_POLL_INTERVAL),
        ),
        None => Err("runtime client is not connected".to_string()),
    };
    app.session.transcript_paging = Some(paging_state);
    let changed = result?;
    if changed || !has_active_agent_run(&app.session) {
        sync_materialized_transcript_history(app);
    }
    Ok(changed)
}

pub(super) fn sync_materialized_transcript_history(app: &mut App) {
    let live_overlay_active = has_active_agent_run(&app.session);
    let stream_offset = app
        .session
        .assistant_stream_start
        .and_then(|start| start.checked_sub(app.session.transcript_history_len));
    let overlay = if live_overlay_active {
        app.session
            .transcript
            .get(
                app.session
                    .transcript_history_len
                    .min(app.session.transcript.len())..,
            )
            .unwrap_or_default()
            .to_vec()
    } else {
        Vec::new()
    };
    let Some(paging_state) = app.session.transcript_paging.as_ref() else {
        return;
    };
    let mut transcript = paging_state.materialize_history(live_overlay_active);
    // Keep previously resolved image references while replacing history summaries.
    let images_by_tool: HashMap<_, _> = app
        .session
        .transcript
        .iter()
        .filter_map(|line| match line.content() {
            TranscriptLine::Tool(tool) if !tool.images.is_empty() => {
                Some((tool.key.as_str(), &tool.images))
            }
            _ => None,
        })
        .collect();
    if !images_by_tool.is_empty() {
        for line in &mut transcript {
            if let TranscriptLine::Tool(tool) = line.content_mut() {
                if let Some(images) = images_by_tool.get(tool.key.as_str()) {
                    tool.images.clone_from(images);
                }
            }
        }
    }
    app.session.transcript_history_len = transcript.len();
    if live_overlay_active {
        app.session.assistant_stream_start =
            stream_offset.map(|offset| app.session.transcript_history_len + offset);
    }
    transcript.extend(overlay);
    app.session.transcript = transcript;
    app.session.history_images.scan_pending = true;
    invalidate_transcript_layout(&mut app.session);
}

pub(super) fn attach_session_viewer(app: &mut App, session_id: &str) -> Result<(), String> {
    let response = app
        .runtime
        .as_mut()
        .ok_or_else(|| "runtime client is not connected".to_string())?
        .session_request(
            SessionCommand::Subscribe,
            json!({
                "sessionId": session_id,
                "viewerId": tui_viewer_id(),
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

pub(super) fn active_agent_runs(
    app: &mut App,
    session_id: &str,
) -> Result<Vec<ActiveAgentRun>, String> {
    let response = app
        .runtime
        .as_mut()
        .ok_or_else(|| "runtime client is not connected".to_string())?
        .session_request(
            SessionCommand::ListRuns,
            json!({
                "sessionId": session_id,
                "includeTerminal": false,
            }),
        )?;
    active_agent_runs_from_response(&response)
}

pub(super) fn active_agent_runs_from_response(
    response: &Value,
) -> Result<Vec<ActiveAgentRun>, String> {
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

pub(super) fn replay_active_agent_run_live_snapshots(
    app: &mut App,
    active_agent_runs: &[ActiveAgentRun],
) -> Result<Vec<Value>, String> {
    let runtime = app
        .runtime
        .as_mut()
        .ok_or_else(|| "runtime client is not connected".to_string())?;
    replay_live_snapshots(runtime, active_agent_runs)
}

fn replay_live_snapshots(
    runtime: &mut RuntimeClient,
    active_agent_runs: &[ActiveAgentRun],
) -> Result<Vec<Value>, String> {
    let mut live_snapshots = Vec::new();
    for agent_run in active_agent_runs {
        let response = runtime.session_request(
            SessionCommand::LiveSnapshot,
            json!({
                "agentRunId": agent_run.agent_run_id.as_str(),
            }),
        )?;
        let agent_run_id = response
            .get("agentRunId")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                "_centaeris/session/agent-runs/live-snapshot response missing agentRunId"
                    .to_string()
            })?;
        if agent_run_id != agent_run.agent_run_id.as_str() {
            return Err(format!(
                "_centaeris/session/agent-runs/live-snapshot agentRunId mismatch: expected {}, got {agent_run_id}",
                agent_run.agent_run_id
            ));
        }
        if let Some(snapshot) = response
            .get("liveSnapshot")
            .filter(|value| !value.is_null())
        {
            live_snapshots.push(snapshot.clone());
        }
    }
    Ok(live_snapshots)
}

pub(super) fn restore_active_agent_runs(
    state: &mut SessionState,
    active_agent_runs: Vec<ActiveAgentRun>,
) {
    state.active_agent_run_id = None;
    state.active_agent_run_ids.clear();
    state.completed_agent_run_ids.clear();
    state.seen_subagent_ids.clear();
    state.live_subagent_ids.clear();
    state.agent_run_started_at = None;
    state.tool_projection.clear();
    state.pending_subagent_lines.clear();
    state.active_tool_label = None;
    state.tool_protocol_error = false;
    state.process_state = RuntimeDisplayState::Idle;
    state.status_easter_egg = StatusEasterEgg::Used;

    let Some(first) = active_agent_runs.first() else {
        return;
    };
    for run in &active_agent_runs {
        state.run_activity.insert(
            run.agent_run_id.clone(),
            if run.status == "waiting_user" {
                runs::RunState::WaitingInput
            } else {
                runs::RunState::Running
            },
        );
    }
    state.active_agent_run_id = Some(first.agent_run_id.clone());
    state.active_agent_run_ids = active_agent_runs
        .iter()
        .map(|agent_run| agent_run.agent_run_id.clone())
        .collect::<HashSet<_>>();
    state.agent_run_started_at = Some(Instant::now());
    state.process_state = runtime_display_state_from_agent_run_status(first.status.as_str());
}

pub(super) fn is_active_session_message_status(status: &str) -> bool {
    matches!(
        status.trim(),
        "running" | "queued" | "waiting_user" | "stalled"
    )
}
