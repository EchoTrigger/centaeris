use crate::runtime_rpc_transport::EventWriter;
use crate::session_files::{
    SessionFileDiagnostic, SessionFileItem, SessionFiles, SessionMetadataPatch,
};
use crate::{
    agent_runs, agent_runtime, message_log, operation_receipts, user_data_layout, workspaces,
};
use centaeris_core::runtime::contracts::current_timestamp_ms;
use centaeris_core::session::store::SessionDataStorePort;
use serde::{Deserialize, Serialize};
use std::fmt::{Display, Formatter};
use std::sync::{Mutex, OnceLock};

const SESSION_CREATE_METHOD: &str = "session/new";
static SESSION_CREATE_OPERATION_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SessionGetRequest {
    pub(crate) session_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SessionListRequest {}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SessionDiagnosticsRequest {}

#[cfg(test)]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SessionCreateRequest {
    pub(crate) title: Option<String>,
    pub(crate) cwd: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SessionCreateCommandRequest {
    #[serde(deserialize_with = "operation_receipts::deserialize_operation_id")]
    pub(crate) operation_id: String,
    pub(crate) title: Option<String>,
    pub(crate) cwd: String,
}

#[derive(Debug)]
pub(crate) struct SessionCreateCommandError {
    code: &'static str,
    message: String,
}

impl SessionCreateCommandError {
    pub(crate) fn code(&self) -> &'static str {
        self.code
    }

    pub(crate) fn message(&self) -> &str {
        self.message.as_str()
    }

    fn failed(message: impl Into<String>) -> Self {
        Self {
            code: "session_failed",
            message: message.into(),
        }
    }

    fn conflict(operation_id: &str) -> Self {
        Self {
            code: "operation_id_conflict",
            message: format!(
                "operationId was already used for a different {SESSION_CREATE_METHOD} request: {operation_id}"
            ),
        }
    }
}

impl Display for SessionCreateCommandError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.message.as_str())
    }
}

impl std::error::Error for SessionCreateCommandError {}

impl From<String> for SessionCreateCommandError {
    fn from(message: String) -> Self {
        Self::failed(message)
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SessionDeleteRequest {
    pub(crate) session_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionDeleteResponse {
    pub(crate) deleted_session_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SessionActivateRequest {
    pub(crate) session_id: String,
    pub(crate) selected_at_ms: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SessionUpdateRequest {
    pub(crate) session_id: String,
    pub(crate) title: Option<String>,
    pub(crate) is_pinned: Option<bool>,
    pub(crate) is_unread: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SessionReorderRequest {
    pub(crate) section: String,
    pub(crate) ordered_session_ids: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SessionItemResponse {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) updated_at: i64,
    pub(crate) last_message: Option<String>,
    pub(crate) cwd: Option<String>,
    pub(crate) session_kind: String,
    pub(crate) parent_session_id: Option<String>,
    pub(crate) runtime_job_id: Option<String>,
    pub(crate) sort_order: i64,
    pub(crate) is_pinned: bool,
    pub(crate) is_unread: bool,
    pub(crate) message_count: usize,
    pub(crate) activity_state: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PersistedChatMessageResponse {
    pub(crate) id: String,
    pub(crate) session_id: String,
    pub(crate) turn_id: String,
    pub(crate) role: String,
    pub(crate) content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) status: Option<String>,
    pub(crate) created_at_ms: i64,
    pub(crate) updated_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) agent_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) image_data: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionDataResponse {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
    pub(crate) session_kind: String,
    pub(crate) parent_session_id: Option<String>,
    pub(crate) runtime_job_id: Option<String>,
    pub(crate) messages: Vec<PersistedChatMessageResponse>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AgentRuntimeBinding {
    pub(crate) cwd: String,
}

pub(crate) fn list(_request: SessionListRequest) -> Result<Vec<SessionItemResponse>, String> {
    let idle_session_ids = agent_runs::idle_session_ids()?;
    session_files()?
        .list()?
        .iter()
        .map(|item| to_response_with_activity(item, &idle_session_ids))
        .collect()
}

pub(crate) fn diagnostics(
    _request: SessionDiagnosticsRequest,
) -> Result<Vec<SessionFileDiagnostic>, String> {
    session_files()?.diagnostics()
}

#[cfg(test)]
pub(crate) fn create(request: SessionCreateRequest) -> Result<SessionItemResponse, String> {
    let cwd = workspaces::normalize_workspace_root_text(request.cwd.as_str())
        .ok_or_else(|| format!("session cwd must be an existing directory: {}", request.cwd))?;
    let item = session_files()?.create(
        request.title.as_deref(),
        cwd.as_str(),
        current_timestamp_ms(),
    )?;
    to_response(&item)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CanonicalSessionCreateRequest<'a> {
    title: &'a str,
    cwd: &'a str,
}

pub(crate) fn create_command(
    request: SessionCreateCommandRequest,
) -> Result<SessionItemResponse, SessionCreateCommandError> {
    let _guard = SESSION_CREATE_OPERATION_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|_| SessionCreateCommandError::failed("session/new operation lock poisoned"))?;
    let (title, cwd, request_digest) = canonical_session_create_request(&request)?;
    if let Some(receipt) =
        operation_receipts::read(SESSION_CREATE_METHOD, request.operation_id.as_str())?
    {
        if receipt.request_digest != request_digest {
            return Err(SessionCreateCommandError::conflict(
                request.operation_id.as_str(),
            ));
        }
        return serde_json::from_value(receipt.result).map_err(|error| {
            SessionCreateCommandError::failed(format!(
                "decode persisted session/new result failed: {error}"
            ))
        });
    }
    let item =
        create_session_for_command(request.operation_id.as_str(), title.as_str(), cwd.as_str())?;
    if item.title != title || item.cwd != cwd || item.session_kind != "main" {
        return Err(SessionCreateCommandError::conflict(
            request.operation_id.as_str(),
        ));
    }
    let response = to_response(&item)?;
    operation_receipts::write(
        SESSION_CREATE_METHOD,
        request.operation_id.as_str(),
        request_digest,
        serde_json::to_value(&response).map_err(|error| {
            SessionCreateCommandError::failed(format!(
                "serialize session/new receipt result failed: {error}"
            ))
        })?,
    )?;
    Ok(response)
}

fn canonical_session_create_request(
    request: &SessionCreateCommandRequest,
) -> Result<(String, String, String), SessionCreateCommandError> {
    let title = crate::session_files::normalize_title(request.title.as_deref());
    let cwd = workspaces::normalize_workspace_root_text(request.cwd.as_str()).ok_or_else(|| {
        SessionCreateCommandError::failed(format!(
            "session cwd must be an existing directory: {}",
            request.cwd
        ))
    })?;
    let request_digest = operation_receipts::request_digest(&CanonicalSessionCreateRequest {
        title: title.as_str(),
        cwd: cwd.as_str(),
    })?;
    Ok((title, cwd, request_digest))
}

fn create_session_for_command(
    operation_id: &str,
    title: &str,
    cwd: &str,
) -> Result<SessionFileItem, SessionCreateCommandError> {
    let session_id =
        operation_receipts::deterministic_identity("session-", SESSION_CREATE_METHOD, operation_id);
    session_files()?
        .create_with_id(
            session_id.as_str(),
            Some(title),
            cwd,
            current_timestamp_ms(),
        )
        .map_err(Into::into)
}

#[cfg(test)]
fn create_session_before_receipt_for_test(
    request: &SessionCreateCommandRequest,
) -> Result<SessionItemResponse, String> {
    let (title, cwd, _) =
        canonical_session_create_request(request).map_err(|error| error.to_string())?;
    let item =
        create_session_for_command(request.operation_id.as_str(), title.as_str(), cwd.as_str())
            .map_err(|error| error.to_string())?;
    to_response(&item)
}

pub(crate) fn delete(
    event_writer: &EventWriter,
    request: SessionDeleteRequest,
) -> Result<SessionDeleteResponse, String> {
    let session_id = request.session_id.trim();
    if session_id.is_empty() {
        return Err("sessionId is required".to_string());
    }
    session_files()?.get(session_id)?;
    let requested_session_ids = vec![session_id.to_string()];
    event_writer.with_session_deletion(requested_session_ids.as_slice(), || {
        agent_runtime::cancel_agent_run(
            event_writer.clone(),
            agent_runs::AgentRunCancelRequest {
                agent_run_id: None,
                session_id: Some(session_id.to_string()),
                reason: Some("session_deleted".to_string()),
            },
        )?;
        event_writer.wait_until_sessions_inactive(requested_session_ids.as_slice())?;
        let deletion_items = session_files()?.deletion_items(session_id)?;
        let child_session_ids = deletion_items
            .iter()
            .filter(|item| item.id != session_id)
            .map(|item| item.id.clone())
            .collect::<Vec<_>>();
        let delete_items = || {
            for child_session_id in &child_session_ids {
                agent_runtime::cancel_agent_run(
                    event_writer.clone(),
                    agent_runs::AgentRunCancelRequest {
                        agent_run_id: None,
                        session_id: Some(child_session_id.clone()),
                        reason: Some("session_deleted".to_string()),
                    },
                )?;
            }
            event_writer.wait_until_sessions_inactive(child_session_ids.as_slice())?;
            for item in &deletion_items {
                agent_runs::ensure_session_can_be_deleted(item.id.as_str())?;
            }
            let runtime_store = agent_runtime::agent_runtime_store_actor().map_err(|error| {
                format!("open runtime store for session deletion failed: {error}")
            })?;
            for item in &deletion_items {
                workspaces::unbind_deleted_session(item.id.as_str())?;
                runtime_store.delete_session_data(item.id.as_str())?;
            }
            let deleted = session_files()?.delete(session_id)?;
            for item in &deleted {
                agent_runs::forget_session(item.id.as_str())?;
            }
            Ok(SessionDeleteResponse {
                deleted_session_id: session_id.to_string(),
            })
        };
        if child_session_ids.is_empty() {
            delete_items()
        } else {
            event_writer.with_session_deletion(child_session_ids.as_slice(), delete_items)
        }
    })
}

pub(crate) fn update(request: SessionUpdateRequest) -> Result<SessionItemResponse, String> {
    session_files()?
        .update(
            request.session_id.as_str(),
            SessionMetadataPatch {
                title: request.title,
                is_pinned: request.is_pinned,
                is_unread: request.is_unread,
            },
            current_timestamp_ms(),
        )
        .and_then(|item| to_response(&item))
}

pub(crate) fn reorder(request: SessionReorderRequest) -> Result<Vec<SessionItemResponse>, String> {
    let items = session_files()?.reorder(
        request.section.as_str(),
        request.ordered_session_ids.as_slice(),
        current_timestamp_ms(),
    )?;
    let idle_session_ids = agent_runs::idle_session_ids()?;
    items
        .iter()
        .map(|item| to_response_with_activity(item, &idle_session_ids))
        .collect()
}

pub(crate) fn activate(request: SessionActivateRequest) -> Result<SessionItemResponse, String> {
    let item = session_files()?.get(request.session_id.as_str())?;
    workspaces::bind_active_session_to_workspace_selected_at(
        item.id.as_str(),
        Some(item.cwd.as_str()),
        request.selected_at_ms.unwrap_or_else(current_timestamp_ms),
        true,
    )?;
    to_response(&item)
}

pub(crate) fn get(request: SessionGetRequest) -> Result<SessionDataResponse, String> {
    let messages = message_log::project_chat_messages(request.session_id.as_str())?;
    get_with_projected_messages(request.session_id.as_str(), messages)
}

pub(crate) fn get_with_projected_messages(
    session_id: &str,
    messages: Vec<message_log::ProjectedChatMessage>,
) -> Result<SessionDataResponse, String> {
    let item = session_files()?.get(session_id)?;
    Ok(SessionDataResponse {
        id: item.id,
        title: item.title,
        created_at: item.created_at,
        updated_at: item.updated_at,
        session_kind: item.session_kind,
        parent_session_id: item.parent_session_id,
        runtime_job_id: item.runtime_job_id,
        messages: messages
            .into_iter()
            .map(to_persisted_message_response)
            .collect(),
    })
}

pub(crate) fn ensure_agent_session(
    session_id: &str,
    parent_session_id: &str,
    runtime_job_id: &str,
    title: &str,
    cwd: &str,
    created_at_ms: i64,
) -> Result<SessionItemResponse, String> {
    session_files()?
        .create_agent_session(
            session_id,
            parent_session_id,
            runtime_job_id,
            title,
            cwd,
            created_at_ms,
        )
        .and_then(|item| to_response(&item))
}

pub(crate) fn agent_runtime_binding(session_id: &str) -> Result<Option<(String, String)>, String> {
    let item = session_files()?.get(session_id)?;
    if item.session_kind == "main" {
        return Ok(None);
    }
    if item.session_kind != "subagent" {
        return Err(format!("unsupported sessionKind: {}", item.session_kind));
    }
    Ok(Some((
        item.parent_session_id
            .ok_or_else(|| format!("subagent session parentSessionId missing: {session_id}"))?,
        item.runtime_job_id
            .ok_or_else(|| format!("subagent session runtimeJobId missing: {session_id}"))?,
    )))
}

pub(crate) fn find_agent_runtime_binding(
    session_id: &str,
) -> Result<Option<(String, String)>, String> {
    let session_id = session_id.trim();
    if session_id.is_empty() {
        return Err("sessionId is required".to_string());
    }
    let Some(item) = session_files()?
        .list()?
        .into_iter()
        .find(|item| item.id == session_id)
    else {
        return Ok(None);
    };
    if item.session_kind != "subagent" {
        return Err(format!(
            "Agent child session must use sessionKind=subagent: {session_id}"
        ));
    }
    Ok(Some((
        item.parent_session_id
            .ok_or_else(|| format!("subagent session parentSessionId missing: {session_id}"))?,
        item.runtime_job_id
            .ok_or_else(|| format!("subagent session runtimeJobId missing: {session_id}"))?,
    )))
}

pub(crate) fn record_session_activity(
    session_id: &str,
    title_candidate: Option<&str>,
    last_message: Option<&str>,
    updated_at_ms: i64,
) -> Result<(), String> {
    session_files()?.record_activity(session_id, title_candidate, last_message, updated_at_ms)
}

pub(crate) fn cwd_for_session_id(session_id: &str) -> Result<String, String> {
    let item = session_files()?.get(session_id)?;
    resolve_cwd(&item)
}

pub(crate) fn persisted_cwd_for_session_id(session_id: &str) -> Result<String, String> {
    Ok(session_files()?.get(session_id)?.cwd)
}

pub(crate) fn runtime_binding_for_session_id(
    session_id: &str,
) -> Result<AgentRuntimeBinding, String> {
    let item = session_files()?.get(session_id)?;
    Ok(AgentRuntimeBinding {
        cwd: resolve_cwd(&item)?,
    })
}

fn session_files() -> Result<SessionFiles, String> {
    user_data_layout::ensure_user_data_layout()?;
    Ok(SessionFiles::new(user_data_layout::sessions_dir_path()))
}

fn resolve_cwd(item: &SessionFileItem) -> Result<String, String> {
    let cwd = item.cwd.trim();
    workspaces::normalize_workspace_root_text(cwd)
        .ok_or_else(|| format!("session cwd is not an existing directory: {cwd}"))
}

fn to_response(item: &SessionFileItem) -> Result<SessionItemResponse, String> {
    let idle_session_ids = agent_runs::idle_session_ids()?;
    to_response_with_activity(item, &idle_session_ids)
}

fn to_response_with_activity(
    item: &SessionFileItem,
    idle_session_ids: &std::collections::HashSet<String>,
) -> Result<SessionItemResponse, String> {
    Ok(SessionItemResponse {
        id: item.id.clone(),
        title: item.title.clone(),
        updated_at: item.updated_at,
        last_message: item.last_message.clone(),
        cwd: Some(item.cwd.clone()),
        session_kind: item.session_kind.clone(),
        parent_session_id: item.parent_session_id.clone(),
        runtime_job_id: item.runtime_job_id.clone(),
        sort_order: item.sort_order.unwrap_or(-item.updated_at),
        is_pinned: item.is_pinned,
        is_unread: item.is_unread,
        message_count: item.message_count,
        activity_state: if idle_session_ids.contains(item.id.as_str()) {
            "idle"
        } else {
            "inactive"
        }
        .to_string(),
    })
}

fn to_persisted_message_response(
    message: message_log::ProjectedChatMessage,
) -> PersistedChatMessageResponse {
    PersistedChatMessageResponse {
        id: message.id,
        session_id: message.session_id,
        turn_id: message.turn_id,
        role: message.role,
        content: message.content,
        status: message.status,
        created_at_ms: message.created_at_ms,
        updated_at_ms: message.updated_at_ms,
        agent_run_id: message.agent_run_id,
        image_data: message.image_data,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime_rpc_transport::RuntimeServerClientHub;
    use crate::runtime_server::RuntimeClientKind;
    use centaeris_core::runtime::TurnControl;
    use std::fs;
    use std::sync::{Arc, Barrier};

    fn with_session_create_test_env<T>(
        label: &str,
        test: impl FnOnce(&std::path::Path) -> Result<T, String>,
    ) -> T {
        let guard = message_log::test_env_mutex()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = std::env::temp_dir().join(format!(
            "centaeris-{label}-{}-{}",
            std::process::id(),
            current_timestamp_ms()
        ));
        let workspace = root.join("workspace");
        fs::create_dir_all(workspace.as_path()).expect("workspace");
        let previous_data_dir = std::env::var_os("CENTAERIS_DESKTOP_DATA_DIR");
        std::env::set_var("CENTAERIS_DESKTOP_DATA_DIR", &root);
        let result = test(workspace.as_path());
        match previous_data_dir {
            Some(value) => std::env::set_var("CENTAERIS_DESKTOP_DATA_DIR", value),
            None => std::env::remove_var("CENTAERIS_DESKTOP_DATA_DIR"),
        }
        let _ = fs::remove_dir_all(root);
        drop(guard);
        result.unwrap_or_else(|error| panic!("{label}: {error}"))
    }

    fn create_command_request(
        operation_id: &str,
        title: &str,
        workspace: &std::path::Path,
    ) -> SessionCreateCommandRequest {
        serde_json::from_value(serde_json::json!({
            "operationId": operation_id,
            "title": title,
            "cwd": workspace.to_string_lossy(),
        }))
        .expect("valid session/new command request")
    }

    #[test]
    fn session_requests_reject_unknown_fields() {
        assert!(
            serde_json::from_value::<SessionCreateCommandRequest>(serde_json::json!({
                "operationId": "strict-fields-1",
                "cwd": "D:/repo",
                "banana": true
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<SessionCreateCommandRequest>(serde_json::json!({
                "operationId": "strict-fields-2",
                "workingDirectory": "D:/repo"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<SessionListRequest>(serde_json::json!({
                "unexpectedField": true
            }))
            .is_err()
        );
        assert!(serde_json::from_value::<SessionListRequest>(serde_json::json!({})).is_ok());
        assert!(
            serde_json::from_value::<SessionDeleteRequest>(serde_json::json!({
                "sessionId": "session-1",
                "banana": true
            }))
            .is_err()
        );
    }

    #[test]
    fn session_create_requires_cwd() {
        assert!(
            serde_json::from_value::<SessionCreateRequest>(serde_json::json!({
                "title": "missing workspace"
            }))
            .is_err()
        );
    }

    #[test]
    fn session_create_command_requires_a_bounded_opaque_operation_id() {
        for value in [
            serde_json::json!({"title": "missing", "cwd": "D:/repo"}),
            serde_json::json!({"operationId": "", "cwd": "D:/repo"}),
            serde_json::json!({"operationId": " leading", "cwd": "D:/repo"}),
            serde_json::json!({"operationId": "path/segment", "cwd": "D:/repo"}),
            serde_json::json!({"operationId": "x".repeat(129), "cwd": "D:/repo"}),
        ] {
            assert!(serde_json::from_value::<SessionCreateCommandRequest>(value).is_err());
        }
        assert!(
            serde_json::from_value::<SessionCreateCommandRequest>(serde_json::json!({
                "operationId": "session-new:client_01.2",
                "cwd": "D:/repo"
            }))
            .is_ok()
        );
    }

    #[test]
    fn duplicate_session_create_operation_returns_the_original_durable_session() {
        with_session_create_test_env("session-create-operation", |workspace| {
            let request = || {
                create_command_request(
                    "session-new-operation-1",
                    "lost response fixture",
                    workspace,
                )
            };
            let committed_before_response_loss =
                create_command(request()).map_err(|error| error.to_string())?;
            let retried_after_reconnect =
                create_command(request()).map_err(|error| error.to_string())?;
            if retried_after_reconnect.id != committed_before_response_loss.id {
                return Err("duplicate operationId created a second Session".to_string());
            }

            let reopened_items = SessionFiles::new(user_data_layout::sessions_dir_path()).list()?;
            if reopened_items.len() != 1
                || reopened_items[0].id != committed_before_response_loss.id
            {
                return Err(
                    "duplicate operationId did not resolve to one durable Session".to_string(),
                );
            }
            Ok::<(), String>(())
        });
    }

    #[test]
    fn duplicate_session_create_compares_normalized_request_payloads() {
        with_session_create_test_env("session-create-normalized", |workspace| {
            let first = create_command(create_command_request(
                "normalized-operation",
                "  normalized title  ",
                workspace,
            ))
            .map_err(|error| error.to_string())?;
            let second = create_command(create_command_request(
                "normalized-operation",
                "normalized title",
                workspace.join(".").as_path(),
            ))
            .map_err(|error| error.to_string())?;
            if first.id != second.id {
                return Err("equivalent normalized payload did not replay".to_string());
            }
            Ok(())
        });
    }

    #[test]
    fn duplicate_session_create_operation_rejects_a_different_normalized_payload() {
        with_session_create_test_env("session-create-conflict", |workspace| {
            create_command(create_command_request("same-operation", "first", workspace))
                .map_err(|error| error.to_string())?;
            let error = create_command(create_command_request(
                "same-operation",
                "second",
                workspace,
            ))
            .expect_err("operationId reuse with a different payload must fail");
            if error.code() != "operation_id_conflict" {
                return Err(format!("unexpected conflict code: {}", error.code()));
            }
            if SessionFiles::new(user_data_layout::sessions_dir_path())
                .list()?
                .len()
                != 1
            {
                return Err("conflicting operation created another Session".to_string());
            }

            let other_workspace = workspace
                .parent()
                .ok_or_else(|| "test workspace has no parent".to_string())?
                .join("other-workspace");
            fs::create_dir_all(other_workspace.as_path())
                .map_err(|error| format!("create other workspace failed: {error}"))?;
            create_command(create_command_request(
                "same-operation-different-cwd",
                "same title",
                workspace,
            ))
            .map_err(|error| error.to_string())?;
            let cwd_error = create_command(create_command_request(
                "same-operation-different-cwd",
                "same title",
                other_workspace.as_path(),
            ))
            .expect_err("operationId reuse with a different cwd must fail");
            if cwd_error.code() != "operation_id_conflict" {
                return Err(format!(
                    "unexpected cwd conflict code: {}",
                    cwd_error.code()
                ));
            }
            Ok(())
        });
    }

    #[test]
    fn session_create_recovers_when_the_session_committed_before_its_receipt() {
        with_session_create_test_env("session-create-crash-window", |workspace| {
            let request = create_command_request("crash-window", "recovered", workspace);
            let committed = create_session_before_receipt_for_test(&request)?;
            let recovered = create_command(request).map_err(|error| error.to_string())?;
            if recovered.id != committed.id {
                return Err("crash recovery created a second Session".to_string());
            }
            if SessionFiles::new(user_data_layout::sessions_dir_path())
                .list()?
                .len()
                != 1
            {
                return Err("crash recovery left more than one Session".to_string());
            }
            Ok(())
        });
    }

    #[test]
    fn concurrent_duplicate_session_create_commits_once() {
        with_session_create_test_env("session-create-concurrent", |workspace| {
            let workspace = Arc::new(workspace.to_path_buf());
            let barrier = Arc::new(Barrier::new(3));
            let mut workers = Vec::new();
            for _ in 0..2 {
                let workspace = Arc::clone(&workspace);
                let barrier = Arc::clone(&barrier);
                workers.push(std::thread::spawn(move || {
                    barrier.wait();
                    create_command(create_command_request(
                        "concurrent-operation",
                        "one Session",
                        workspace.as_path(),
                    ))
                }));
            }
            barrier.wait();
            let first = workers
                .remove(0)
                .join()
                .map_err(|_| "first worker panicked")?
                .map_err(|error| error.to_string())?;
            let second = workers
                .remove(0)
                .join()
                .map_err(|_| "second worker panicked")?
                .map_err(|error| error.to_string())?;
            if first.id != second.id {
                return Err("concurrent duplicates returned different Sessions".to_string());
            }
            if SessionFiles::new(user_data_layout::sessions_dir_path())
                .list()?
                .len()
                != 1
            {
                return Err("concurrent duplicate created multiple Sessions".to_string());
            }
            Ok(())
        });
    }

    #[test]
    fn deleted_session_create_receipt_does_not_recreate_the_session() {
        with_session_create_test_env("session-create-deleted", |workspace| {
            let request = || create_command_request("deleted-operation", "deleted", workspace);
            let created = create_command(request()).map_err(|error| error.to_string())?;
            session_files()?.delete(created.id.as_str())?;
            let replayed = create_command(request()).map_err(|error| error.to_string())?;
            if replayed.id != created.id {
                return Err("deleted operation did not return its original receipt".to_string());
            }
            if !SessionFiles::new(user_data_layout::sessions_dir_path())
                .list()?
                .is_empty()
            {
                return Err("replaying a deleted operation recreated the Session".to_string());
            }
            Ok(())
        });
    }

    #[test]
    fn electron_and_protocol_clients_share_session_jsonl() {
        let guard = message_log::test_env_mutex()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = std::env::temp_dir().join(format!(
            "centaeris-shared-session-{}-{}",
            std::process::id(),
            current_timestamp_ms()
        ));
        let workspace = root.join("workspace");
        fs::create_dir_all(workspace.as_path()).expect("workspace");
        let previous_data_dir = std::env::var_os("CENTAERIS_DESKTOP_DATA_DIR");
        let previous_log_dir = std::env::var_os("CENTAERIS_MESSAGE_LOG_SESSIONS_DIR");
        std::env::set_var("CENTAERIS_DESKTOP_DATA_DIR", &root);
        std::env::set_var("CENTAERIS_MESSAGE_LOG_SESSIONS_DIR", root.join("sessions"));
        let result = (|| {
            let electron_created = create(SessionCreateRequest {
                title: Some("electron-created".to_string()),
                cwd: workspace.to_string_lossy().to_string(),
            })?;
            let core_loaded = session_files()?.get(electron_created.id.as_str())?;
            if core_loaded.id != electron_created.id {
                return Err("core did not load Electron-created session".to_string());
            }

            let protocol_created = session_files()?.create(
                Some("protocol-created"),
                workspace.to_string_lossy().as_ref(),
                current_timestamp_ms(),
            )?;
            message_log::append_user_message(
                protocol_created.id.as_str(),
                "turn-count",
                "agent-run-count",
                "hello",
                current_timestamp_ms(),
            )?;
            let first_usage = centaeris_core::runtime::contracts::ProviderTokenUsageV1 {
                input_tokens: Some(10),
                output_tokens: Some(2),
                total_tokens: Some(12),
                prompt_cache_hit_tokens: Some(4),
                prompt_cache_miss_tokens: Some(6),
            };
            message_log::append_provider_usage(
                protocol_created.id.as_str(),
                "turn-count",
                "agent-run-count",
                &first_usage,
                current_timestamp_ms(),
            )?;
            message_log::append_provider_usage(
                protocol_created.id.as_str(),
                "turn-count",
                "agent-run-count",
                &first_usage,
                current_timestamp_ms(),
            )?;
            message_log::append_provider_usage(
                protocol_created.id.as_str(),
                "turn-count:2",
                "agent-run-count",
                &centaeris_core::runtime::contracts::ProviderTokenUsageV1 {
                    input_tokens: Some(20),
                    output_tokens: Some(3),
                    total_tokens: Some(23),
                    prompt_cache_hit_tokens: None,
                    prompt_cache_miss_tokens: None,
                },
                current_timestamp_ms(),
            )?;
            let state = message_log::project_agent_context_state(protocol_created.id.as_str())?;
            let usage = state
                .provider_usage
                .ok_or_else(|| "provider usage projection missing".to_string())?;
            if usage.latest_turn_id != "turn-count:2"
                || usage.session.input_tokens != Some(30)
                || usage.session.output_tokens != Some(5)
            {
                return Err("provider usage projection mismatch".to_string());
            }
            message_log::append_assistant_message(
                protocol_created.id.as_str(),
                "turn-count",
                Some("agent-run-count"),
                "hello back",
                "done",
                current_timestamp_ms(),
            )?;
            let listed = list(SessionListRequest {})?;
            let listed_item = listed
                .iter()
                .find(|item| item.id == protocol_created.id)
                .ok_or_else(|| "Electron did not list protocol-created session".to_string())?;
            if listed_item.message_count != 2 {
                return Err(
                    "session list must count one user and one assistant message".to_string()
                );
            }
            let loaded = get(SessionGetRequest {
                session_id: protocol_created.id,
            })?;
            if loaded.messages.len() != 2 {
                return Err("persisted user and assistant messages must count as two".to_string());
            }
            Ok::<(), String>(())
        })();
        assert!(
            !root.join("workspaces").exists(),
            "user data layout must not create a managed default workspace"
        );
        match previous_data_dir {
            Some(value) => std::env::set_var("CENTAERIS_DESKTOP_DATA_DIR", value),
            None => std::env::remove_var("CENTAERIS_DESKTOP_DATA_DIR"),
        }
        match previous_log_dir {
            Some(value) => std::env::set_var("CENTAERIS_MESSAGE_LOG_SESSIONS_DIR", value),
            None => std::env::remove_var("CENTAERIS_MESSAGE_LOG_SESSIONS_DIR"),
        }
        let _ = fs::remove_dir_all(root);
        drop(guard);
        result.expect("shared session");
    }

    #[test]
    fn direct_delete_stops_running_agent_run_then_removes_session_owned_products() {
        let guard = message_log::test_env_mutex()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = std::env::temp_dir().join(format!(
            "centaeris-session-delete-{}-{}",
            std::process::id(),
            current_timestamp_ms()
        ));
        let workspace = root.join("workspace");
        fs::create_dir_all(workspace.as_path()).expect("workspace");
        let previous_data_dir = std::env::var_os("CENTAERIS_DESKTOP_DATA_DIR");
        let previous_log_dir = std::env::var_os("CENTAERIS_MESSAGE_LOG_SESSIONS_DIR");
        let previous_runtime_db = std::env::var_os("CENTAERIS_AGENT_RUNTIME_DB_PATH");
        std::env::set_var("CENTAERIS_DESKTOP_DATA_DIR", &root);
        std::env::set_var("CENTAERIS_MESSAGE_LOG_SESSIONS_DIR", root.join("sessions"));
        std::env::set_var(
            "CENTAERIS_AGENT_RUNTIME_DB_PATH",
            root.join("runtime").join("runtime.sqlite3"),
        );
        let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
        let _runtime_guard = runtime.enter();
        agent_runtime::agent_runtime_store_actor().expect("runtime store actor");
        let result = runtime.block_on(async {
            (|| {
                let created = create(SessionCreateRequest {
                    title: Some("delete fixture".to_string()),
                    cwd: workspace.to_string_lossy().to_string(),
                })?;
                let session_id = created.id.clone();
                let session_log_path =
                    root.join(session_files()?.get(session_id.as_str())?.session_path);
                let child = session_files()?.create_agent_session(
                    "session-agent-delete-fixture",
                    session_id.as_str(),
                    "subagent.run:delete-fixture",
                    "delete child fixture",
                    workspace.to_string_lossy().as_ref(),
                    current_timestamp_ms(),
                )?;
                let child_log_path = root.join(child.session_path.as_str());
                workspaces::bind_active_session_to_workspace_selected_at(
                    session_id.as_str(),
                    Some(workspace.to_string_lossy().as_ref()),
                    current_timestamp_ms(),
                    false,
                )?;
                message_log::append_agent_run_started(
                    session_id.as_str(),
                    "turn-delete",
                    "agent-run-delete",
                    "run",
                    current_timestamp_ms(),
                )?;
                let clients = Arc::new(RuntimeServerClientHub::default());
                let (event_writer, _outbound) = clients
                    .connect()
                    .expect("connect delete client")
                    .expect("runtime server is accepting delete client");
                event_writer
                    .register_client(RuntimeClientKind::Desktop, "delete-viewer")
                    .expect("register delete client");
                let turn_control = TurnControl::new();
                let lease = event_writer.start_agent_run(
                    session_id.as_str(),
                    "agent-run-delete",
                    "turn-delete",
                    turn_control.clone(),
                )?;
                agent_runs::attach(agent_runs::AgentRunAttachRequest {
                    agent_run_id: Some("agent-run-delete".to_string()),
                    session_id: Some(session_id.clone()),
                    viewer_id: Some("delete-viewer".to_string()),
                })?;
                let terminal_event_writer = event_writer.clone();
                let terminal_session_id = session_id.clone();
                runtime.spawn(async move {
                    assert!(!turn_control
                        .wait_for_pending_or_close()
                        .await
                        .expect("wait for Session delete cancellation"));
                    assert!(
                        agent_runs::cancel(agent_runs::AgentRunCancelRequest {
                            agent_run_id: Some("agent-run-delete".to_string()),
                            session_id: Some(terminal_session_id),
                            reason: Some("session_deleted".to_string()),
                        })
                        .expect("commit deleted Session AgentRun terminal")
                        .cancelled
                    );
                    terminal_event_writer
                        .finish_agent_run(lease.lease_id.as_str())
                        .expect("finish deleted Session AgentRun lease");
                });

                let deleted = delete(
                    &event_writer,
                    SessionDeleteRequest {
                        session_id: session_id.clone(),
                    },
                )?;
                if deleted.deleted_session_id != session_id {
                    return Err("delete response identity mismatch".to_string());
                }
                if session_files()?.get(session_id.as_str()).is_ok() {
                    return Err("deleted Session still exists".to_string());
                }
                if session_files()?.get(child.id.as_str()).is_ok() {
                    return Err("deleted child Session still exists".to_string());
                }
                if session_log_path.exists() {
                    return Err("deleted Session log still exists".to_string());
                }
                if child_log_path.exists() {
                    return Err("deleted child Session log still exists".to_string());
                }
                let workspace_item = workspaces::get()?
                    .workspaces
                    .into_iter()
                    .find(|item| {
                        item.root
                            == workspaces::normalize_workspace_root_text(
                                workspace.to_string_lossy().as_ref(),
                            )
                            .unwrap_or_default()
                    })
                    .ok_or_else(|| "workspace binding fixture missing".to_string())?;
                if workspace_item.active_session_id.is_some() {
                    return Err("deleted session workspace binding still exists".to_string());
                }
                let detached =
                    agent_runs::detach_viewer(agent_runs::AgentRunDetachViewerRequest {
                        viewer_id: "delete-viewer".to_string(),
                    })?;
                if detached.detached_count != 0 {
                    return Err("deleted session viewer registry still exists".to_string());
                }
                if !workspace.join(".").exists() {
                    return Err("real workspace was deleted".to_string());
                }
                Ok::<(), String>(())
            })()
        });
        match previous_data_dir {
            Some(value) => std::env::set_var("CENTAERIS_DESKTOP_DATA_DIR", value),
            None => std::env::remove_var("CENTAERIS_DESKTOP_DATA_DIR"),
        }
        match previous_log_dir {
            Some(value) => std::env::set_var("CENTAERIS_MESSAGE_LOG_SESSIONS_DIR", value),
            None => std::env::remove_var("CENTAERIS_MESSAGE_LOG_SESSIONS_DIR"),
        }
        match previous_runtime_db {
            Some(value) => std::env::set_var("CENTAERIS_AGENT_RUNTIME_DB_PATH", value),
            None => std::env::remove_var("CENTAERIS_AGENT_RUNTIME_DB_PATH"),
        }
        let _ = fs::remove_dir_all(root);
        drop(guard);
        result.expect("direct session delete");
    }

    #[test]
    fn session_creation_rejects_missing_cwd() {
        let guard = message_log::test_env_mutex()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = std::env::temp_dir().join(format!(
            "centaeris-session-without-workspace-{}-{}",
            std::process::id(),
            current_timestamp_ms()
        ));
        let previous_data_dir = std::env::var_os("CENTAERIS_DESKTOP_DATA_DIR");
        let previous_log_dir = std::env::var_os("CENTAERIS_MESSAGE_LOG_SESSIONS_DIR");
        std::env::set_var("CENTAERIS_DESKTOP_DATA_DIR", &root);
        std::env::set_var("CENTAERIS_MESSAGE_LOG_SESSIONS_DIR", root.join("sessions"));
        let result = (|| {
            session_files()?.create(
                Some("no workspace"),
                root.join("banana").to_string_lossy().as_ref(),
                current_timestamp_ms(),
            )
        })();
        match previous_data_dir {
            Some(value) => std::env::set_var("CENTAERIS_DESKTOP_DATA_DIR", value),
            None => std::env::remove_var("CENTAERIS_DESKTOP_DATA_DIR"),
        }
        match previous_log_dir {
            Some(value) => std::env::set_var("CENTAERIS_MESSAGE_LOG_SESSIONS_DIR", value),
            None => std::env::remove_var("CENTAERIS_MESSAGE_LOG_SESSIONS_DIR"),
        }
        let _ = fs::remove_dir_all(root);
        drop(guard);

        let error = result.expect_err("missing cwd must fail");
        assert!(error.contains("not a directory"));
    }
}
