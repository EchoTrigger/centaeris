use crate::{agent_runs, message_log, sessions};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

const SESSION_PROJECTION_SCHEMA_VERSION: &str = "session_projection.v1";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub(crate) struct SessionProjectionGetRequest {
    pub(crate) session_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionProjectionResponse {
    pub(crate) schema_version: &'static str,
    pub(crate) session: sessions::SessionDataResponse,
    pub(crate) agent_runs: Vec<agent_runs::AgentRunSummary>,
    pub(crate) agent_run_replays: Vec<AgentRunReplayProjection>,
    pub(crate) active_agent_run_id: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AgentRunReplayProjection {
    pub(crate) agent_run_id: String,
    pub(crate) session_id: String,
    pub(crate) turn_id: String,
    pub(crate) status: String,
    pub(crate) started_at_ms: i64,
    pub(crate) updated_at_ms: i64,
    pub(crate) completed_at_ms: Option<i64>,
    pub(crate) next_cursor: u64,
    pub(crate) items: Vec<serde_json::Value>,
}

pub(crate) fn get(
    request: SessionProjectionGetRequest,
) -> Result<SessionProjectionResponse, String> {
    let session_id = request.session_id.trim().to_string();
    if session_id.is_empty() {
        return Err(String::from("sessionId is required"));
    }
    let log_projection = message_log::project_session_log(session_id.as_str())?;
    let session =
        sessions::get_with_projected_messages(session_id.as_str(), log_projection.messages)?;
    let replay_by_agent_run_id = log_projection
        .agent_run_replays
        .into_iter()
        .map(|replay| (replay.agent_run_id.clone(), replay))
        .collect::<HashMap<_, _>>();
    let agent_runs = log_projection
        .agent_runs
        .iter()
        .map(agent_runs::into_summary)
        .collect::<Vec<_>>();
    let mut agent_run_replays = Vec::new();
    for agent_run in &log_projection.agent_runs {
        let replay = replay_by_agent_run_id
            .get(agent_run.agent_run_id.as_str())
            .ok_or_else(|| {
                format!(
                    "session projection missing replay for {}",
                    agent_run.agent_run_id
                )
            })?;
        agent_run_replays.push(AgentRunReplayProjection {
            agent_run_id: agent_run.agent_run_id.clone(),
            session_id: agent_run.session_id.clone(),
            turn_id: agent_run.turn_id.clone(),
            status: agent_run.status.clone(),
            started_at_ms: agent_run.started_at_ms,
            updated_at_ms: agent_run.updated_at_ms,
            completed_at_ms: agent_run.completed_at_ms,
            next_cursor: replay.next_cursor,
            items: replay.items.clone(),
        });
    }
    let active_agent_run_id = log_projection
        .agent_runs
        .iter()
        .find(|agent_run| {
            !matches!(
                agent_run.status.as_str(),
                "succeeded" | "failed" | "cancelled" | "stopped"
            )
        })
        .map(|agent_run| agent_run.agent_run_id.clone());
    Ok(SessionProjectionResponse {
        schema_version: SESSION_PROJECTION_SCHEMA_VERSION,
        session,
        agent_runs,
        agent_run_replays,
        active_agent_run_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_projection_keeps_stopped_process_and_later_final_turns() {
        let guard = message_log::test_env_mutex()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp_root = std::env::temp_dir().join(format!(
            "centaeris-session-projection-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&temp_root).expect("temp root");
        let workspace_root = temp_root.to_string_lossy().to_string();
        let previous_data_dir = std::env::var_os("CENTAERIS_DESKTOP_DATA_DIR");
        let previous_log_dir = std::env::var_os("CENTAERIS_MESSAGE_LOG_SESSIONS_DIR");
        std::env::set_var("CENTAERIS_DESKTOP_DATA_DIR", &temp_root);
        std::env::set_var(
            "CENTAERIS_MESSAGE_LOG_SESSIONS_DIR",
            temp_root.join("sessions"),
        );
        let result = (|| {
            let session = sessions::create(sessions::SessionCreateRequest {
                title: Some(String::from("projection fixture")),
                cwd: workspace_root.clone(),
            })?;
            let session_id = session.id;
            message_log::append_agent_run_started(
                session_id.as_str(),
                "turn-1",
                "agent_run-1",
                "first",
                1,
            )?;
            message_log::append_file_mutation_pre_apply_fact(
                session_id.as_str(),
                "turn-1",
                "agent_run-1",
                serde_json::json!({
                    "schema": "file_mutation_pre_apply_fact_v1",
                    "toolName": "write",
                    "toolCallId": "call-1",
                    "operation": "create",
                    "path": "note.txt",
                    "targetPath": null,
                    "previousFileHash": null,
                    "readSnapshotHash": null,
                    "fileHash": null,
                    "bytesWritten": null,
                    "addedLines": null,
                    "removedLines": null,
                    "sessionId": session_id,
                    "executionOwner": "agent_run-1",
                }),
            )?;
            let terminal_at_ms = centaeris_core::runtime::contracts::current_timestamp_ms() + 1;
            message_log::append_agent_run_terminal(
                session_id.as_str(),
                "turn-1",
                "agent_run-1",
                "stopped",
                None,
                terminal_at_ms,
            )?;
            message_log::append_agent_run_started(
                session_id.as_str(),
                "turn-2",
                "agent_run-2",
                "continue",
                terminal_at_ms + 1,
            )?;
            message_log::append_assistant_message(
                session_id.as_str(),
                "turn-2",
                Some("agent_run-2"),
                "final answer",
                "done",
                terminal_at_ms + 2,
            )?;
            message_log::append_agent_run_terminal(
                session_id.as_str(),
                "turn-2",
                "agent_run-2",
                "succeeded",
                None,
                terminal_at_ms + 3,
            )?;
            get(SessionProjectionGetRequest { session_id })
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
        let projection = result.expect("projection");
        assert_eq!(projection.session.messages.len(), 3);
        assert_eq!(projection.agent_runs.len(), 2);
        let stopped = projection
            .agent_run_replays
            .iter()
            .find(|agent_run| agent_run.agent_run_id == "agent_run-1")
            .expect("stopped agent_run replay");
        assert_eq!(stopped.status, "stopped");
        assert!(stopped.completed_at_ms.is_some());
        assert_eq!(stopped.next_cursor, 3);
        assert_eq!(stopped.items[0]["type"], "session_event");
        assert_eq!(stopped.items[2]["event"]["type"], "AgentRunInterrupted");
        assert!(projection.active_agent_run_id.is_none());
    }
}
