use super::*;

pub(super) fn load_session_history(
    app: &mut App,
    session: &TuiSession,
) -> Result<SessionRestore, String> {
    ensure_runtime(app)?;
    attach_session_viewer(app, session.id.as_str())?;
    let active_agent_runs = active_agent_runs(app, session.id.as_str())?;
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
    let transcript_paging = TranscriptPagingState::open(page)?;
    let transcript = transcript_paging.materialize_history(!active_agent_runs.is_empty());
    let replay_items = replay_active_agent_run_live_snapshots(app, &active_agent_runs)?;
    Ok(SessionRestore {
        transcript,
        transcript_paging,
        active_agent_runs,
        replay_items,
    })
}

pub(super) fn load_older_transcript_page(app: &mut App) -> Result<bool, String> {
    let previous_total_rows = build_cached_transcript_view(app, app.render_width).total_rows;
    let previous_scroll = app.transcript_scroll;
    let Some(mut paging_state) = app.transcript_paging.take() else {
        return Ok(false);
    };
    let Some(older_cursor) = paging_state.older_cursor().map(str::to_string) else {
        app.transcript_paging = Some(paging_state);
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
            app.transcript_paging = Some(paging_state);
            return Err(error);
        }
    };
    if let Err(error) = paging_state.apply_older_page(page) {
        app.transcript_paging = Some(paging_state);
        return Err(error);
    }
    app.transcript_paging = Some(paging_state);
    sync_materialized_transcript_history(app);
    let next_total_rows = build_cached_transcript_view(app, app.render_width).total_rows;
    app.transcript_scroll =
        prepend_anchor_scroll(previous_scroll, previous_total_rows, next_total_rows);
    app.transcript_follow_bottom = false;
    Ok(true)
}

pub(super) fn refresh_transcript_patches(app: &mut App) -> Result<bool, String> {
    let Some(mut paging_state) = app.transcript_paging.take() else {
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
    app.transcript_paging = Some(paging_state);
    let changed = result?;
    if changed || !has_active_agent_run(app) {
        sync_materialized_transcript_history(app);
    }
    Ok(changed)
}

pub(super) fn sync_materialized_transcript_history(app: &mut App) {
    let live_overlay_active = has_active_agent_run(app);
    let overlay = if live_overlay_active {
        app.transcript
            .get(app.transcript_history_len.min(app.transcript.len())..)
            .unwrap_or_default()
            .to_vec()
    } else {
        Vec::new()
    };
    let Some(paging_state) = app.transcript_paging.as_ref() else {
        return;
    };
    let mut transcript = paging_state.materialize_history(live_overlay_active);
    app.transcript_history_len = transcript.len();
    transcript.extend(overlay);
    app.transcript = transcript;
    invalidate_transcript_layout(app);
}

pub(super) fn attach_session_viewer(app: &mut App, session_id: &str) -> Result<(), String> {
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

pub(super) fn active_agent_runs(
    app: &mut App,
    session_id: &str,
) -> Result<Vec<ActiveAgentRun>, String> {
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
    let mut live_snapshots = Vec::new();
    for agent_run in active_agent_runs {
        let response = app
            .runtime
            .as_mut()
            .ok_or_else(|| "runtime client is not connected".to_string())?
            .request(
                "_centaeris/session/agent-runs/live-snapshot",
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

pub(super) fn restore_active_agent_runs(app: &mut App, active_agent_runs: Vec<ActiveAgentRun>) {
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

pub(super) fn is_active_session_message_status(status: &str) -> bool {
    matches!(
        status.trim(),
        "running" | "queued" | "waiting_user" | "stalled"
    )
}
