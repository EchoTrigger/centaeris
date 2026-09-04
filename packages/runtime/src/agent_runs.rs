use crate::message_log;
use centaeris_core::runtime::contracts::current_timestamp_ms;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

const AGENT_RUN_STATUS_RUNNING: &str = "running";
const AGENT_RUN_STATUS_STALLED: &str = "stalled";
const AGENT_RUN_STATUS_CANCELLED: &str = "cancelled";
const AGENT_RUN_STATUS_SUCCEEDED: &str = "succeeded";
const AGENT_RUN_STATUS_FAILED: &str = "failed";
static VIEWER_REGISTRY: OnceLock<Mutex<ViewerRegistry>> = OnceLock::new();

#[derive(Clone, Debug)]
struct ViewerBinding {
    session_id: String,
    agent_run_id: Option<String>,
}

#[derive(Debug, Default)]
struct ViewerRegistry {
    viewers_by_id: HashMap<String, ViewerBinding>,
    session_viewers: HashMap<String, HashSet<String>>,
    agent_run_viewers: HashMap<String, HashSet<String>>,
    detached_session_at_ms: HashMap<String, i64>,
    unread_agent_run_ids: HashSet<String>,
    unread_agent_run_session_ids: HashMap<String, String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ViewerDetachTransition {
    Detached,
    NotAttached,
    BindingChanged,
}

impl ViewerDetachTransition {
    fn as_str(self) -> &'static str {
        match self {
            Self::Detached => "viewer_detached",
            Self::NotAttached => "viewer_not_attached",
            Self::BindingChanged => "viewer_binding_changed",
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AgentRunListRequest {
    pub(crate) session_id: Option<String>,
    pub(crate) include_terminal: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AgentRunStreamReplayRequest {
    pub(crate) agent_run_id: String,
    pub(crate) cursor: Option<u64>,
    pub(crate) limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AgentRunAttachRequest {
    pub(crate) agent_run_id: Option<String>,
    pub(crate) session_id: Option<String>,
    pub(crate) viewer_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AgentRunDetachRequest {
    pub(crate) agent_run_id: Option<String>,
    pub(crate) session_id: Option<String>,
    pub(crate) viewer_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AgentRunDetachViewerRequest {
    pub(crate) viewer_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AgentRunCancelRequest {
    pub(crate) agent_run_id: Option<String>,
    pub(crate) session_id: Option<String>,
    pub(crate) reason: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AgentRunSummary {
    pub(crate) agent_run_id: String,
    pub(crate) session_id: String,
    pub(crate) turn_id: String,
    pub(crate) agent_run_kind: String,
    pub(crate) cwd: Option<String>,
    pub(crate) status: String,
    pub(crate) unread: bool,
    pub(crate) started_at_ms: i64,
    pub(crate) updated_at_ms: i64,
    pub(crate) completed_at_ms: Option<i64>,
    pub(crate) last_event_at_ms: Option<i64>,
    pub(crate) stall_reason: Option<String>,
    pub(crate) watchdog: Option<serde_json::Value>,
    pub(crate) error: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AgentRunListResponse {
    pub(crate) agent_runs: Vec<AgentRunSummary>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AgentRunStreamReplayResponse {
    pub(crate) agent_run_id: String,
    pub(crate) cwd: Option<String>,
    pub(crate) items: Vec<serde_json::Value>,
    pub(crate) next_cursor: Option<u64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AgentRunAttachResponse {
    pub(crate) agent_run: Option<AgentRunSummary>,
    pub(crate) viewer_id: String,
    pub(crate) transition_reason: String,
    pub(crate) attached_viewer_count: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AgentRunDetachResponse {
    pub(crate) agent_run: Option<AgentRunSummary>,
    pub(crate) viewer_id: String,
    pub(crate) transition_reason: String,
    pub(crate) attached_viewer_count: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AgentRunDetachViewerResponse {
    pub(crate) detached_count: usize,
    pub(crate) viewer_id: String,
    pub(crate) transition_reason: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AgentRunCancelResponse {
    pub(crate) agent_run: Option<AgentRunSummary>,
    pub(crate) cancelled: bool,
}

pub(crate) fn list(request: AgentRunListRequest) -> Result<AgentRunListResponse, String> {
    let include_terminal = request.include_terminal.unwrap_or(false);
    let session_filter = request
        .session_id
        .as_deref()
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(ToString::to_string);
    let mut agent_runs = message_log::project_agent_runs()?;
    if let Some(session_id) = session_filter.as_deref() {
        agent_runs.retain(|item| item.session_id == session_id);
    }
    if !include_terminal {
        agent_runs.retain(|item| !is_agent_run_terminal(item.status.as_str()));
    }
    mark_unread_for_detached_terminal_agent_runs(agent_runs.as_slice())?;
    let summaries = agent_runs
        .iter()
        .map(summary_with_viewer_state)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(AgentRunListResponse {
        agent_runs: summaries,
    })
}

pub(crate) fn idle_session_ids() -> Result<HashSet<String>, String> {
    let mut session_ids = message_log::project_agent_runs()?
        .into_iter()
        .filter(|agent_run| !is_agent_run_terminal(agent_run.status.as_str()))
        .map(|agent_run| agent_run.session_id)
        .collect::<HashSet<_>>();
    let registry = viewer_registry()?;
    session_ids.extend(
        registry
            .session_viewers
            .iter()
            .filter(|(_, viewers)| !viewers.is_empty())
            .map(|(session_id, _)| session_id.clone()),
    );
    Ok(session_ids)
}

pub(crate) fn replay(
    request: AgentRunStreamReplayRequest,
) -> Result<AgentRunStreamReplayResponse, String> {
    let replay = message_log::replay_agent_run(
        request.agent_run_id.as_str(),
        request.cursor,
        request.limit,
    )?;
    Ok(AgentRunStreamReplayResponse {
        agent_run_id: replay.agent_run_id,
        cwd: replay.cwd,
        items: replay.items,
        next_cursor: replay.next_cursor,
    })
}

pub(crate) fn attach(request: AgentRunAttachRequest) -> Result<AgentRunAttachResponse, String> {
    let viewer_id = required_viewer_id(request.viewer_id.as_deref())?;
    let agent_run = find_agent_run(
        request.agent_run_id.as_deref(),
        request.session_id.as_deref(),
    )?;
    let session_id = agent_run
        .as_ref()
        .map(|agent_run| agent_run.session_id.clone())
        .or_else(|| normalize_optional_string(request.session_id.as_deref()))
        .ok_or_else(|| String::from("sessionId or agentRunId is required for viewer attach"))?;
    let agent_run_id = agent_run
        .as_ref()
        .map(|agent_run| agent_run.agent_run_id.clone());
    let attached_viewer_count = attach_viewer_to_session(
        viewer_id.as_str(),
        session_id.as_str(),
        agent_run_id.as_deref(),
    )?;
    Ok(AgentRunAttachResponse {
        agent_run: agent_run
            .as_ref()
            .map(summary_with_viewer_state)
            .transpose()?,
        viewer_id,
        transition_reason: String::from("viewer_attached"),
        attached_viewer_count,
    })
}

pub(crate) fn detach(request: AgentRunDetachRequest) -> Result<AgentRunDetachResponse, String> {
    let viewer_id = required_viewer_id(request.viewer_id.as_deref())?;
    let agent_run = find_agent_run(
        request.agent_run_id.as_deref(),
        request.session_id.as_deref(),
    )?;
    let detach_transition =
        detach_viewer_binding(viewer_id.as_str(), request.session_id.as_deref())?;
    Ok(AgentRunDetachResponse {
        agent_run: agent_run
            .as_ref()
            .map(summary_with_viewer_state)
            .transpose()?,
        viewer_id,
        transition_reason: detach_transition.as_str().to_string(),
        attached_viewer_count: agent_run
            .as_ref()
            .map(|agent_run| session_viewer_count(agent_run.session_id.as_str()))
            .transpose()?
            .unwrap_or(0),
    })
}

pub(crate) fn detach_viewer(
    request: AgentRunDetachViewerRequest,
) -> Result<AgentRunDetachViewerResponse, String> {
    let viewer_id = required_viewer_id(Some(request.viewer_id.as_str()))?;
    let detach_transition = detach_viewer_binding(viewer_id.as_str(), None)?;
    let detached_count = usize::from(detach_transition == ViewerDetachTransition::Detached);
    Ok(AgentRunDetachViewerResponse {
        detached_count,
        viewer_id,
        transition_reason: detach_transition.as_str().to_string(),
    })
}

pub(crate) fn cancel(request: AgentRunCancelRequest) -> Result<AgentRunCancelResponse, String> {
    let reason = request
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("user_interrupt");
    let Some(agent_run) = find_agent_run(
        request.agent_run_id.as_deref(),
        request.session_id.as_deref(),
    )?
    else {
        return Ok(AgentRunCancelResponse {
            agent_run: None,
            cancelled: false,
        });
    };
    if is_agent_run_terminal(agent_run.status.as_str()) {
        return Ok(AgentRunCancelResponse {
            agent_run: Some(into_summary(&agent_run)),
            cancelled: false,
        });
    }
    let cancelled_at_ms = current_timestamp_ms();
    let updated = message_log::append_agent_run_terminal(
        agent_run.session_id.as_str(),
        agent_run.turn_id.as_str(),
        agent_run.agent_run_id.as_str(),
        AGENT_RUN_STATUS_CANCELLED,
        Some(reason),
        cancelled_at_ms,
    )?;
    Ok(AgentRunCancelResponse {
        agent_run: Some(into_summary(&updated)),
        cancelled: true,
    })
}

pub(crate) fn request_cancel(
    request: AgentRunCancelRequest,
) -> Result<AgentRunCancelResponse, String> {
    let Some(agent_run) = find_agent_run(
        request.agent_run_id.as_deref(),
        request.session_id.as_deref(),
    )?
    else {
        return Ok(AgentRunCancelResponse {
            agent_run: None,
            cancelled: false,
        });
    };
    Ok(AgentRunCancelResponse {
        cancelled: !is_agent_run_terminal(agent_run.status.as_str()),
        agent_run: Some(into_summary(&agent_run)),
    })
}

pub(crate) fn start_agent_run(
    agent_run_id: &str,
    session_id: &str,
    turn_id: &str,
) -> Result<AgentRunSummary, String> {
    let agent_run = message_log::project_agent_run(agent_run_id)?.ok_or_else(|| {
        format!("agent_run_started missing before agent_run transition: {agent_run_id}")
    })?;
    if agent_run.session_id != session_id || agent_run.turn_id != turn_id {
        return Err(format!(
            "started AgentRun identity mismatch: {agent_run_id}"
        ));
    }
    Ok(into_summary(&agent_run))
}

pub(crate) fn finish_agent_run(
    agent_run_id: &str,
    session_id: &str,
    turn_id: &str,
) -> Result<Option<AgentRunSummary>, String> {
    update_agent_run_terminal(
        agent_run_id,
        session_id,
        turn_id,
        AGENT_RUN_STATUS_SUCCEEDED,
        None,
    )
}

pub(crate) fn fail_agent_run(
    agent_run_id: &str,
    session_id: &str,
    turn_id: &str,
    error: impl Into<String>,
) -> Result<Option<AgentRunSummary>, String> {
    update_agent_run_terminal(
        agent_run_id,
        session_id,
        turn_id,
        AGENT_RUN_STATUS_FAILED,
        Some(error.into()),
    )
}

pub(crate) fn ensure_session_can_be_deleted(session_id: &str) -> Result<(), String> {
    let session_id = required_string(session_id, "sessionId")?;
    if let Some(agent_run) = message_log::project_agent_runs()?
        .into_iter()
        .find(|agent_run| {
            agent_run.session_id == session_id
                && matches!(
                    agent_run.status.as_str(),
                    AGENT_RUN_STATUS_RUNNING | AGENT_RUN_STATUS_STALLED
                )
        })
    {
        return Err(format!(
            "session cannot be deleted while agent_run {} is {}",
            agent_run.agent_run_id, agent_run.status
        ));
    }
    Ok(())
}

pub(crate) fn forget_session(session_id: &str) -> Result<(), String> {
    let session_id = required_string(session_id, "sessionId")?;
    let mut registry = viewer_registry()?;
    let viewer_ids = registry
        .session_viewers
        .remove(session_id.as_str())
        .unwrap_or_default();
    for viewer_id in viewer_ids {
        if let Some(binding) = registry.viewers_by_id.remove(viewer_id.as_str()) {
            if let Some(agent_run_id) = binding.agent_run_id.as_deref() {
                if let Some(viewers) = registry.agent_run_viewers.get_mut(agent_run_id) {
                    viewers.remove(viewer_id.as_str());
                    if viewers.is_empty() {
                        registry.agent_run_viewers.remove(agent_run_id);
                    }
                }
            }
        }
    }
    registry.detached_session_at_ms.remove(session_id.as_str());
    clear_unread_for_session_locked(&mut registry, session_id.as_str());
    Ok(())
}

fn update_agent_run_terminal(
    agent_run_id: &str,
    session_id: &str,
    turn_id: &str,
    status: &str,
    error: Option<String>,
) -> Result<Option<AgentRunSummary>, String> {
    let normalized_agent_run_id = required_string(agent_run_id, "agentRunId")?;
    message_log::project_agent_run(normalized_agent_run_id.as_str())?
        .ok_or_else(|| format!("AgentRun not found: {normalized_agent_run_id}"))?;
    let updated = message_log::append_agent_run_terminal(
        session_id,
        turn_id,
        normalized_agent_run_id.as_str(),
        status,
        error.as_deref(),
        current_timestamp_ms(),
    )?;
    Ok(Some(into_summary(&updated)))
}

fn find_agent_run(
    agent_run_id: Option<&str>,
    session_id: Option<&str>,
) -> Result<Option<message_log::ProjectedAgentRun>, String> {
    if let Some(agent_run_id) = agent_run_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return message_log::project_agent_run(agent_run_id);
    }
    let normalized_session_id = session_id.map(str::trim).filter(|value| !value.is_empty());
    let Some(normalized_session_id) = normalized_session_id else {
        return Ok(None);
    };
    Ok(message_log::project_agent_runs()?
        .into_iter()
        .filter(|agent_run| agent_run.session_id == normalized_session_id)
        .max_by(|left, right| {
            left.updated_at_ms
                .cmp(&right.updated_at_ms)
                .then_with(|| left.started_at_ms.cmp(&right.started_at_ms))
                .then_with(|| right.agent_run_id.cmp(&left.agent_run_id))
        }))
}

fn viewer_registry() -> Result<std::sync::MutexGuard<'static, ViewerRegistry>, String> {
    VIEWER_REGISTRY
        .get_or_init(|| Mutex::new(ViewerRegistry::default()))
        .lock()
        .map_err(|_| String::from("AgentRun viewer registry lock poisoned"))
}

fn required_viewer_id(value: Option<&str>) -> Result<String, String> {
    value
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| String::from("viewerId is required"))
}

fn detach_viewer_binding(
    viewer_id: &str,
    expected_session_id: Option<&str>,
) -> Result<ViewerDetachTransition, String> {
    let mut registry = viewer_registry()?;
    let Some(binding) = registry.viewers_by_id.get(viewer_id).cloned() else {
        return Ok(ViewerDetachTransition::NotAttached);
    };
    if let Some(expected_session_id) = expected_session_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if binding.session_id != expected_session_id {
            return Ok(ViewerDetachTransition::BindingChanged);
        }
    }
    registry.viewers_by_id.remove(viewer_id);
    remove_viewer_from_registry_indexes(&mut registry, viewer_id, &binding);
    if registry
        .session_viewers
        .get(binding.session_id.as_str())
        .is_none_or(HashSet::is_empty)
    {
        registry
            .detached_session_at_ms
            .insert(binding.session_id, current_timestamp_ms());
    }
    Ok(ViewerDetachTransition::Detached)
}

fn attach_viewer_to_session(
    viewer_id: &str,
    session_id: &str,
    agent_run_id: Option<&str>,
) -> Result<usize, String> {
    let mut registry = viewer_registry()?;
    if let Some(previous) = registry.viewers_by_id.remove(viewer_id) {
        remove_viewer_from_registry_indexes(&mut registry, viewer_id, &previous);
        if registry
            .session_viewers
            .get(previous.session_id.as_str())
            .is_none_or(HashSet::is_empty)
        {
            registry
                .detached_session_at_ms
                .insert(previous.session_id, current_timestamp_ms());
        }
    }
    let binding = ViewerBinding {
        session_id: session_id.to_string(),
        agent_run_id: agent_run_id.map(str::to_string),
    };
    registry
        .session_viewers
        .entry(session_id.to_string())
        .or_default()
        .insert(viewer_id.to_string());
    if let Some(agent_run_id) = agent_run_id {
        registry
            .agent_run_viewers
            .entry(agent_run_id.to_string())
            .or_default()
            .insert(viewer_id.to_string());
    }
    registry
        .viewers_by_id
        .insert(viewer_id.to_string(), binding);
    registry.detached_session_at_ms.remove(session_id);
    clear_unread_for_session_locked(&mut registry, session_id);
    Ok(registry
        .session_viewers
        .get(session_id)
        .map(HashSet::len)
        .unwrap_or(0))
}

fn remove_viewer_from_registry_indexes(
    registry: &mut ViewerRegistry,
    viewer_id: &str,
    binding: &ViewerBinding,
) {
    if let Some(viewers) = registry
        .session_viewers
        .get_mut(binding.session_id.as_str())
    {
        viewers.remove(viewer_id);
    }
    if registry
        .session_viewers
        .get(binding.session_id.as_str())
        .is_some_and(HashSet::is_empty)
    {
        registry.session_viewers.remove(binding.session_id.as_str());
    }
    if let Some(agent_run_id) = binding.agent_run_id.as_deref() {
        if let Some(viewers) = registry.agent_run_viewers.get_mut(agent_run_id) {
            viewers.remove(viewer_id);
        }
        if registry
            .agent_run_viewers
            .get(agent_run_id)
            .is_some_and(HashSet::is_empty)
        {
            registry.agent_run_viewers.remove(agent_run_id);
        }
    }
}

fn clear_unread_for_session_locked(registry: &mut ViewerRegistry, session_id: &str) {
    let cleared = registry
        .unread_agent_run_session_ids
        .iter()
        .filter(|&(_, agent_run_session_id)| agent_run_session_id == session_id)
        .map(|(agent_run_id, _)| agent_run_id.clone())
        .collect::<Vec<_>>();
    for agent_run_id in cleared {
        registry.unread_agent_run_ids.remove(agent_run_id.as_str());
        registry
            .unread_agent_run_session_ids
            .remove(agent_run_id.as_str());
    }
}

fn mark_unread_for_detached_terminal_agent_runs(
    agent_runs: &[message_log::ProjectedAgentRun],
) -> Result<(), String> {
    let mut registry = viewer_registry()?;
    for agent_run in agent_runs {
        if !is_agent_run_terminal(agent_run.status.as_str()) {
            continue;
        }
        if registry
            .session_viewers
            .get(agent_run.session_id.as_str())
            .is_some_and(|viewers| !viewers.is_empty())
        {
            continue;
        }
        let Some(detached_at_ms) = registry
            .detached_session_at_ms
            .get(agent_run.session_id.as_str())
            .copied()
        else {
            continue;
        };
        if agent_run.updated_at_ms >= detached_at_ms {
            registry
                .unread_agent_run_ids
                .insert(agent_run.agent_run_id.clone());
            registry
                .unread_agent_run_session_ids
                .insert(agent_run.agent_run_id.clone(), agent_run.session_id.clone());
        }
    }
    Ok(())
}

fn session_viewer_count(session_id: &str) -> Result<usize, String> {
    let registry = viewer_registry()?;
    Ok(registry
        .session_viewers
        .get(session_id)
        .map(HashSet::len)
        .unwrap_or(0))
}

pub(crate) fn into_summary(agent_run: &message_log::ProjectedAgentRun) -> AgentRunSummary {
    AgentRunSummary {
        agent_run_id: agent_run.agent_run_id.clone(),
        session_id: agent_run.session_id.clone(),
        turn_id: agent_run.turn_id.clone(),
        agent_run_kind: String::from("agent"),
        cwd: agent_run.cwd.clone(),
        status: agent_run.status.clone(),
        unread: false,
        started_at_ms: agent_run.started_at_ms,
        updated_at_ms: agent_run.updated_at_ms,
        completed_at_ms: agent_run.completed_at_ms,
        last_event_at_ms: agent_run.last_event_at_ms,
        stall_reason: None,
        watchdog: None,
        error: agent_run.error.clone(),
    }
}

fn summary_with_viewer_state(
    agent_run: &message_log::ProjectedAgentRun,
) -> Result<AgentRunSummary, String> {
    let mut summary = into_summary(agent_run);
    summary.unread = viewer_registry()?
        .unread_agent_run_ids
        .contains(agent_run.agent_run_id.as_str());
    Ok(summary)
}

fn is_agent_run_terminal(status: &str) -> bool {
    status != AGENT_RUN_STATUS_RUNNING && status != AGENT_RUN_STATUS_STALLED
}

fn normalize_optional_string(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(ToString::to_string)
}

fn required_string(raw: &str, field_name: &str) -> Result<String, String> {
    let value = raw.trim();
    if value.is_empty() {
        return Err(format!("{field_name} is required"));
    }
    Ok(value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions;

    #[test]
    fn viewer_registry_marks_detached_terminal_agent_run_unread_and_attach_clears_it() {
        let guard = message_log::test_env_mutex()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp_root = std::env::temp_dir().join(format!(
            "centaeris-agent-run-viewer-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&temp_root).expect("temp root");
        let cwd = temp_root.to_string_lossy().to_string();
        let previous_data_dir = std::env::var_os("CENTAERIS_DESKTOP_DATA_DIR");
        let previous_log_dir = std::env::var_os("CENTAERIS_MESSAGE_LOG_SESSIONS_DIR");
        std::env::set_var("CENTAERIS_DESKTOP_DATA_DIR", &temp_root);
        std::env::set_var(
            "CENTAERIS_MESSAGE_LOG_SESSIONS_DIR",
            temp_root.join("sessions"),
        );
        let result = (|| {
            *viewer_registry()? = ViewerRegistry::default();
            let session = sessions::create(sessions::SessionCreateRequest {
                title: Some(String::from("viewer fixture")),
                cwd: cwd.clone(),
            })?;
            let session_id = session.id;
            let activity_state = || -> Result<String, String> {
                sessions::list(sessions::SessionListRequest {})?
                    .into_iter()
                    .find(|session| session.id == session_id)
                    .map(|session| session.activity_state)
                    .ok_or_else(|| "viewer fixture missing from session/list".to_string())
            };
            if activity_state()? != "inactive" {
                return Err(
                    "session without a viewer or active AgentRun must be inactive".to_string(),
                );
            }
            attach(AgentRunAttachRequest {
                agent_run_id: None,
                session_id: Some(session_id.clone()),
                viewer_id: Some(String::from("desktop-main")),
            })?;
            if activity_state()? != "idle" {
                return Err("attached session must be idle".to_string());
            }
            detach(AgentRunDetachRequest {
                agent_run_id: None,
                session_id: Some(session_id.clone()),
                viewer_id: Some(String::from("desktop-main")),
            })?;
            if activity_state()? != "inactive" {
                return Err(
                    "detached session without an active AgentRun must be inactive".to_string(),
                );
            }
            message_log::append_agent_run_started(
                session_id.as_str(),
                "turn-viewer",
                "agent_run-viewer",
                "run",
                1,
            )?;
            if activity_state()? != "idle" {
                return Err("session with an active AgentRun must be idle".to_string());
            }
            let attached = attach(AgentRunAttachRequest {
                agent_run_id: Some(String::from("agent_run-viewer")),
                session_id: Some(session_id.clone()),
                viewer_id: Some(String::from("desktop-main")),
            })?;
            if attached.attached_viewer_count != 1 {
                return Err(format!(
                    "expected one attached viewer, got {}",
                    attached.attached_viewer_count
                ));
            }
            let detached = detach(AgentRunDetachRequest {
                agent_run_id: Some(String::from("agent_run-viewer")),
                session_id: Some(session_id.clone()),
                viewer_id: Some(String::from("desktop-main")),
            })?;
            if detached.transition_reason != "viewer_detached" {
                return Err(format!(
                    "unexpected detach transition {}",
                    detached.transition_reason
                ));
            }
            let cancelled = cancel(AgentRunCancelRequest {
                agent_run_id: Some(String::from("agent_run-viewer")),
                session_id: Some(session_id.clone()),
                reason: Some(String::from("host_owner_exited")),
            })?;
            if !cancelled.cancelled {
                return Err(String::from("expected detached agent_run to be cancelled"));
            }
            let cancel_response = serde_json::to_value(&cancelled)
                .map_err(|error| format!("serialize cancel response failed: {error}"))?;
            if cancel_response.as_object().map(|object| object.len()) != Some(2)
                || cancel_response.get("agentRun").is_none()
                || cancel_response.get("cancelled").is_none()
            {
                return Err(String::from(
                    "cancel response must contain only agentRun and cancelled",
                ));
            }
            let terminal = message_log::terminal_agent_run_stream_projection("agent_run-viewer")?;
            if terminal
                .pointer("/event/type")
                .and_then(serde_json::Value::as_str)
                != Some("AgentRunInterrupted")
            {
                return Err(String::from(
                    "cancelled agent_run should project its committed interruption",
                ));
            }
            let listed = list(AgentRunListRequest {
                session_id: Some(session_id.clone()),
                include_terminal: Some(true),
            })?;
            let cancelled = listed
                .agent_runs
                .iter()
                .find(|agent_run| agent_run.agent_run_id == "agent_run-viewer")
                .ok_or_else(|| String::from("missing cancelled AgentRun"))?;
            if !cancelled.unread {
                return Err(String::from("detached terminal AgentRun should be unread"));
            }
            attach(AgentRunAttachRequest {
                agent_run_id: Some(String::from("agent_run-viewer")),
                session_id: Some(session_id.clone()),
                viewer_id: Some(String::from("desktop-main")),
            })?;
            let listed_after_attach = list(AgentRunListRequest {
                session_id: Some(session_id),
                include_terminal: Some(true),
            })?;
            let cleared = listed_after_attach
                .agent_runs
                .iter()
                .find(|agent_run| agent_run.agent_run_id == "agent_run-viewer")
                .ok_or_else(|| String::from("missing cleared AgentRun"))?;
            if cleared.unread {
                return Err(String::from("attach should clear unread"));
            }
            Ok::<(), String>(())
        })();
        match previous_data_dir {
            Some(value) => std::env::set_var("CENTAERIS_DESKTOP_DATA_DIR", value),
            None => std::env::remove_var("CENTAERIS_DESKTOP_DATA_DIR"),
        }
        match previous_log_dir {
            Some(value) => std::env::set_var("CENTAERIS_MESSAGE_LOG_SESSIONS_DIR", value),
            None => std::env::remove_var("CENTAERIS_MESSAGE_LOG_SESSIONS_DIR"),
        }
        std::fs::remove_dir_all(&temp_root).ok();
        drop(guard);
        result.expect("viewer registry test");
    }
}
