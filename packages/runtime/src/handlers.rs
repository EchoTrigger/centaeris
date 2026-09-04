use crate::agent_runs;
use crate::agent_runtime;
use crate::commands::RuntimeHostCommand;
use crate::desktop_file_preview;
use crate::errors::RuntimeHostError;
use crate::mcp;
use crate::plugins;
use crate::protocol::HostCommandRequest;
use crate::runtime_bridge;
use crate::runtime_config;
use crate::runtime_garbage;
use crate::runtime_ops;
use crate::runtime_rpc_transport::EventWriter;
use crate::session_projection;
use crate::sessions;
use crate::sidecars;
use crate::skills;
use crate::workspace_git;
use crate::workspaces;
use std::sync::{Arc, Mutex};

#[derive(Default)]
pub(crate) struct RuntimeHostState {
    sidecar_store: sidecars::SidecarStoreState,
}

fn workspace_runtime_host_error(error: String) -> RuntimeHostError {
    RuntimeHostError::new(workspaces::runtime_host_error_code(error.as_str()), error)
}

fn runtime_config_runtime_host_error(error: String) -> RuntimeHostError {
    RuntimeHostError::new(runtime_config::error_code(error.as_str()), error)
}

pub(crate) fn handle_request(
    state: &mut RuntimeHostState,
    request: HostCommandRequest,
    event_writer: EventWriter,
) -> Result<serde_json::Value, RuntimeHostError> {
    let command = RuntimeHostCommand::parse(request.command.as_str())?;
    match command {
        RuntimeHostCommand::Initialize => {
            let payload = runtime_bridge::deserialize_request(request.payload)?;
            let response = runtime_bridge::initialize(&event_writer, payload)?;
            serde_json::to_value(response).map_err(|error| {
                RuntimeHostError::new("serialize_response_failed", error.to_string())
            })
        }
        RuntimeHostCommand::SessionList => {
            let payload = runtime_bridge::deserialize_request(request.payload)?;
            let response = sessions::list(payload)
                .map_err(|error| RuntimeHostError::new("session_failed", error))?;
            serde_json::to_value(response).map_err(|error| {
                RuntimeHostError::new("serialize_response_failed", error.to_string())
            })
        }
        RuntimeHostCommand::SessionDiagnostics => {
            let payload = runtime_bridge::deserialize_request(request.payload)?;
            let response = sessions::diagnostics(payload)
                .map_err(|error| RuntimeHostError::new("session_failed", error))?;
            serde_json::to_value(response).map_err(|error| {
                RuntimeHostError::new("serialize_response_failed", error.to_string())
            })
        }
        RuntimeHostCommand::SessionGet => {
            let payload = runtime_bridge::deserialize_request(request.payload)?;
            let response = sessions::get(payload)
                .map_err(|error| RuntimeHostError::new("session_failed", error))?;
            serde_json::to_value(response).map_err(|error| {
                RuntimeHostError::new("serialize_response_failed", error.to_string())
            })
        }
        RuntimeHostCommand::SessionCreate => {
            let payload = runtime_bridge::deserialize_request(request.payload)?;
            let response = sessions::create(payload)
                .map_err(|error| RuntimeHostError::new("session_failed", error))?;
            serde_json::to_value(response).map_err(|error| {
                RuntimeHostError::new("serialize_response_failed", error.to_string())
            })
        }
        RuntimeHostCommand::SessionActivate => {
            let payload = runtime_bridge::deserialize_request(request.payload)?;
            let response = sessions::activate(payload)
                .map_err(|error| RuntimeHostError::new("session_failed", error))?;
            serde_json::to_value(response).map_err(|error| {
                RuntimeHostError::new("serialize_response_failed", error.to_string())
            })
        }
        RuntimeHostCommand::SessionDelete => {
            let payload = runtime_bridge::deserialize_request(request.payload)?;
            let response = sessions::delete(&event_writer, payload)
                .map_err(|error| RuntimeHostError::new("session_failed", error))?;
            serde_json::to_value(response).map_err(|error| {
                RuntimeHostError::new("serialize_response_failed", error.to_string())
            })
        }
        RuntimeHostCommand::SessionProjectionGet => {
            let payload = runtime_bridge::deserialize_request(request.payload)?;
            let response = session_projection::get(payload)
                .map_err(|error| RuntimeHostError::new("session_projection_failed", error))?;
            serde_json::to_value(response).map_err(|error| {
                RuntimeHostError::new("serialize_response_failed", error.to_string())
            })
        }
        RuntimeHostCommand::SessionUpdate => {
            let payload = runtime_bridge::deserialize_request(request.payload)?;
            let response = sessions::update(payload)
                .map_err(|error| RuntimeHostError::new("session_failed", error))?;
            serde_json::to_value(response).map_err(|error| {
                RuntimeHostError::new("serialize_response_failed", error.to_string())
            })
        }
        RuntimeHostCommand::SessionReorder => {
            let payload = runtime_bridge::deserialize_request(request.payload)?;
            let response = sessions::reorder(payload)
                .map_err(|error| RuntimeHostError::new("session_failed", error))?;
            serde_json::to_value(response).map_err(|error| {
                RuntimeHostError::new("serialize_response_failed", error.to_string())
            })
        }
        RuntimeHostCommand::AgentRuntimeConfigGet => {
            let payload = runtime_bridge::deserialize_request(request.payload)?;
            let response =
                runtime_config::get(payload).map_err(runtime_config_runtime_host_error)?;
            serde_json::to_value(response).map_err(|error| {
                RuntimeHostError::new("serialize_response_failed", error.to_string())
            })
        }
        RuntimeHostCommand::AgentRuntimeConfigReset => {
            let payload = runtime_bridge::deserialize_request(request.payload)?;
            let response =
                runtime_config::reset(payload).map_err(runtime_config_runtime_host_error)?;
            let response = serde_json::to_value(response).map_err(|error| {
                RuntimeHostError::new("serialize_response_failed", error.to_string())
            })?;
            event_writer.emit("runtime/config-changed", serde_json::json!({}))?;
            Ok(response)
        }
        RuntimeHostCommand::AgentRuntimeConfigSet => {
            let payload = runtime_bridge::deserialize_request(request.payload)?;
            let response =
                runtime_config::set(payload).map_err(runtime_config_runtime_host_error)?;
            let response = serde_json::to_value(response).map_err(|error| {
                RuntimeHostError::new("serialize_response_failed", error.to_string())
            })?;
            event_writer.emit("runtime/config-changed", serde_json::json!({}))?;
            Ok(response)
        }
        RuntimeHostCommand::AgentRuntimeModelTest => Err(RuntimeHostError::new(
            "model_test_requires_async_handler",
            "agent_runtime_model_test",
        )),
        RuntimeHostCommand::McpCatalog => {
            let payload = runtime_bridge::deserialize_request(request.payload)?;
            let response = mcp::catalog(payload)
                .map_err(|error| RuntimeHostError::new("mcp_failed", error))?;
            serde_json::to_value(response).map_err(|error| {
                RuntimeHostError::new("serialize_response_failed", error.to_string())
            })
        }
        RuntimeHostCommand::McpConfigure => Err(RuntimeHostError::new(
            "mcp_configure_requires_async_handler",
            "mcp/configure",
        )),
        RuntimeHostCommand::PluginList => {
            let payload = runtime_bridge::deserialize_request(request.payload)?;
            let response = plugins::list(payload)
                .map_err(|error| RuntimeHostError::new("plugin_failed", error))?;
            serde_json::to_value(response).map_err(|error| {
                RuntimeHostError::new("serialize_response_failed", error.to_string())
            })
        }
        RuntimeHostCommand::PluginDetail => {
            let payload = runtime_bridge::deserialize_request(request.payload)?;
            let response = plugins::detail(payload)
                .map_err(|error| RuntimeHostError::new("plugin_failed", error))?;
            serde_json::to_value(response).map_err(|error| {
                RuntimeHostError::new("serialize_response_failed", error.to_string())
            })
        }
        RuntimeHostCommand::PluginInstall => {
            let payload = runtime_bridge::deserialize_request(request.payload)?;
            let response = plugins::install(payload)
                .map_err(|error| RuntimeHostError::new("plugin_failed", error))?;
            serde_json::to_value(response).map_err(|error| {
                RuntimeHostError::new("serialize_response_failed", error.to_string())
            })
        }
        RuntimeHostCommand::PluginRemove => {
            let payload = runtime_bridge::deserialize_request(request.payload)?;
            let response = plugins::remove(payload)
                .map_err(|error| RuntimeHostError::new("plugin_failed", error))?;
            serde_json::to_value(response).map_err(|error| {
                RuntimeHostError::new("serialize_response_failed", error.to_string())
            })
        }
        RuntimeHostCommand::PluginSetEnabled => {
            let payload = runtime_bridge::deserialize_request(request.payload)?;
            let response = plugins::set_enabled(payload)
                .map_err(|error| RuntimeHostError::new("plugin_failed", error))?;
            serde_json::to_value(response).map_err(|error| {
                RuntimeHostError::new("serialize_response_failed", error.to_string())
            })
        }
        RuntimeHostCommand::PluginReload => {
            let response =
                plugins::reload().map_err(|error| RuntimeHostError::new("plugin_failed", error))?;
            serde_json::to_value(response).map_err(|error| {
                RuntimeHostError::new("serialize_response_failed", error.to_string())
            })
        }
        RuntimeHostCommand::PluginSourceRef => {
            let payload = runtime_bridge::deserialize_request(request.payload)?;
            let response = plugins::source_ref(payload)
                .map_err(|error| RuntimeHostError::new("plugin_failed", error))?;
            serde_json::to_value(response).map_err(|error| {
                RuntimeHostError::new("serialize_response_failed", error.to_string())
            })
        }
        RuntimeHostCommand::PluginCatalogState => {
            let response = plugins::current_catalog_state()
                .map_err(|error| RuntimeHostError::new("plugin_failed", error))?;
            serde_json::to_value(response).map_err(|error| {
                RuntimeHostError::new("serialize_response_failed", error.to_string())
            })
        }
        RuntimeHostCommand::SkillSourceList => {
            let payload = runtime_bridge::deserialize_request(request.payload)?;
            serde_json::to_value(
                skills::list_sources(payload)
                    .map_err(|error| RuntimeHostError::new("skill_failed", error))?,
            )
            .map_err(|error| RuntimeHostError::new("serialize_response_failed", error.to_string()))
        }
        RuntimeHostCommand::SkillSourceAdd => {
            let payload = runtime_bridge::deserialize_request(request.payload)?;
            serde_json::to_value(
                skills::add_source(payload)
                    .map_err(|error| RuntimeHostError::new("skill_failed", error))?,
            )
            .map_err(|error| RuntimeHostError::new("serialize_response_failed", error.to_string()))
        }
        RuntimeHostCommand::SkillSourceRemove => {
            let payload = runtime_bridge::deserialize_request(request.payload)?;
            serde_json::to_value(
                skills::remove_source(payload)
                    .map_err(|error| RuntimeHostError::new("skill_failed", error))?,
            )
            .map_err(|error| RuntimeHostError::new("serialize_response_failed", error.to_string()))
        }
        RuntimeHostCommand::SkillSourceSetEnabled => {
            let payload = runtime_bridge::deserialize_request(request.payload)?;
            serde_json::to_value(
                skills::set_source_enabled(payload)
                    .map_err(|error| RuntimeHostError::new("skill_failed", error))?,
            )
            .map_err(|error| RuntimeHostError::new("serialize_response_failed", error.to_string()))
        }
        RuntimeHostCommand::SkillSourceRef => {
            let payload = runtime_bridge::deserialize_request(request.payload)?;
            serde_json::to_value(
                skills::source_ref(payload)
                    .map_err(|error| RuntimeHostError::new("skill_failed", error))?,
            )
            .map_err(|error| RuntimeHostError::new("serialize_response_failed", error.to_string()))
        }
        RuntimeHostCommand::SkillCatalog | RuntimeHostCommand::SkillReload => {
            let payload = runtime_bridge::deserialize_request(request.payload)?;
            serde_json::to_value(
                skills::catalog(payload)
                    .map_err(|error| RuntimeHostError::new("skill_failed", error))?,
            )
            .map_err(|error| RuntimeHostError::new("serialize_response_failed", error.to_string()))
        }
        RuntimeHostCommand::SkillDetail => {
            let payload = runtime_bridge::deserialize_request(request.payload)?;
            serde_json::to_value(
                skills::detail(payload)
                    .map_err(|error| RuntimeHostError::new("skill_failed", error))?,
            )
            .map_err(|error| RuntimeHostError::new("serialize_response_failed", error.to_string()))
        }
        RuntimeHostCommand::SkillSetEnabled => {
            let payload = runtime_bridge::deserialize_request(request.payload)?;
            serde_json::to_value(
                skills::set_enabled(payload)
                    .map_err(|error| RuntimeHostError::new("skill_failed", error))?,
            )
            .map_err(|error| RuntimeHostError::new("serialize_response_failed", error.to_string()))
        }
        RuntimeHostCommand::AgentStateGet => {
            let payload = runtime_bridge::deserialize_request(request.payload)?;
            let response = runtime_ops::agent_state_get(payload)
                .map_err(|error| RuntimeHostError::new("agent_state_failed", error))?;
            serde_json::to_value(response).map_err(|error| {
                RuntimeHostError::new("serialize_response_failed", error.to_string())
            })
        }
        RuntimeHostCommand::AgentContextUsageGet => {
            let payload = runtime_bridge::deserialize_request(request.payload)?;
            let response = runtime_ops::agent_context_usage_get(payload)
                .map_err(|error| RuntimeHostError::new("agent_context_usage_failed", error))?;
            serde_json::to_value(response).map_err(|error| {
                RuntimeHostError::new("serialize_response_failed", error.to_string())
            })
        }
        RuntimeHostCommand::AgentContextCompact => Err(RuntimeHostError::new(
            "async_handler_required",
            "_centaeris/session/compact_context must be handled by handle_request_async",
        )),
        RuntimeHostCommand::AgentRuntimeGarbageCollect => {
            let payload = runtime_bridge::deserialize_request(request.payload)?;
            let response = runtime_garbage::collect(payload)
                .map_err(|error| RuntimeHostError::new("runtime_garbage_failed", error))?;
            serde_json::to_value(response).map_err(|error| {
                RuntimeHostError::new("serialize_response_failed", error.to_string())
            })
        }
        RuntimeHostCommand::TranscriptProjection => {
            let payload = runtime_bridge::deserialize_request(request.payload)?;
            serde_json::to_value(agent_runtime::project_session_events_to_transcript(payload))
                .map_err(|error| {
                    RuntimeHostError::new("serialize_response_failed", error.to_string())
                })
        }
        RuntimeHostCommand::AgentInput => {
            let payload = runtime_bridge::deserialize_request(request.payload)?;
            let response = agent_runtime::input(event_writer, payload)
                .map_err(|error| RuntimeHostError::new("session_prompt_failed", error))?;
            serde_json::to_value(response).map_err(|error| {
                RuntimeHostError::new("serialize_response_failed", error.to_string())
            })
        }
        RuntimeHostCommand::AgentSupplement => {
            let payload = runtime_bridge::deserialize_request(request.payload)?;
            let response = agent_runtime::supplement(event_writer, payload)
                .map_err(|error| RuntimeHostError::new("session_supplement_failed", error))?;
            serde_json::to_value(response).map_err(|error| {
                RuntimeHostError::new("serialize_response_failed", error.to_string())
            })
        }
        RuntimeHostCommand::AgentAnswerNow => {
            let payload = runtime_bridge::deserialize_request(request.payload)?;
            let response = agent_runtime::answer_now(event_writer, payload)
                .map_err(|error| RuntimeHostError::new("session_answer_now_failed", error))?;
            serde_json::to_value(response).map_err(|error| {
                RuntimeHostError::new("serialize_response_failed", error.to_string())
            })
        }
        RuntimeHostCommand::AgentQuestionAnswer => Err(RuntimeHostError::new(
            "async_handler_required",
            "_centaeris/session/answer_question must be handled by handle_request_async",
        )),
        RuntimeHostCommand::AgentRunList => {
            let payload = runtime_bridge::deserialize_request(request.payload)?;
            let response = agent_runs::list(payload)
                .map_err(|error| RuntimeHostError::new("agent_task_failed", error))?;
            serde_json::to_value(response).map_err(|error| {
                RuntimeHostError::new("serialize_response_failed", error.to_string())
            })
        }
        RuntimeHostCommand::AgentRunStreamReplay => {
            let payload = runtime_bridge::deserialize_request(request.payload)?;
            let response = agent_runs::replay(payload)
                .map_err(|error| RuntimeHostError::new("agent_task_failed", error))?;
            serde_json::to_value(response).map_err(|error| {
                RuntimeHostError::new("serialize_response_failed", error.to_string())
            })
        }
        RuntimeHostCommand::AgentRunAttach => {
            let payload: agent_runs::AgentRunAttachRequest =
                runtime_bridge::deserialize_request(request.payload)?;
            event_writer.require_viewer_id(payload.viewer_id.as_deref().unwrap_or_default())?;
            let response = agent_runs::attach(payload)
                .map_err(|error| RuntimeHostError::new("agent_task_failed", error))?;
            serde_json::to_value(response).map_err(|error| {
                RuntimeHostError::new("serialize_response_failed", error.to_string())
            })
        }
        RuntimeHostCommand::AgentRunDetach => {
            let payload: agent_runs::AgentRunDetachRequest =
                runtime_bridge::deserialize_request(request.payload)?;
            event_writer.require_viewer_id(payload.viewer_id.as_deref().unwrap_or_default())?;
            let response = agent_runs::detach(payload)
                .map_err(|error| RuntimeHostError::new("agent_task_failed", error))?;
            serde_json::to_value(response).map_err(|error| {
                RuntimeHostError::new("serialize_response_failed", error.to_string())
            })
        }
        RuntimeHostCommand::AgentRunDetachViewer => {
            let payload: agent_runs::AgentRunDetachViewerRequest =
                runtime_bridge::deserialize_request(request.payload)?;
            event_writer.require_viewer_id(payload.viewer_id.as_str())?;
            let response = agent_runs::detach_viewer(payload)
                .map_err(|error| RuntimeHostError::new("agent_task_failed", error))?;
            serde_json::to_value(response).map_err(|error| {
                RuntimeHostError::new("serialize_response_failed", error.to_string())
            })
        }
        RuntimeHostCommand::AgentRunCancel => {
            let payload = runtime_bridge::deserialize_request(request.payload)?;
            let response = agent_runtime::cancel_agent_run(event_writer, payload)
                .map_err(|error| RuntimeHostError::new("agent_task_failed", error))?;
            serde_json::to_value(response).map_err(|error| {
                RuntimeHostError::new("serialize_response_failed", error.to_string())
            })
        }
        RuntimeHostCommand::AgentRuntimeJobList => Err(RuntimeHostError::new(
            "async_handler_required",
            "agent_runtime_job_list must be handled by handle_request_async",
        )),
        RuntimeHostCommand::AgentRuntimeJobGet => Err(RuntimeHostError::new(
            "async_handler_required",
            "agent_runtime_job_get must be handled by handle_request_async",
        )),
        RuntimeHostCommand::AgentDeadLetterList => Err(RuntimeHostError::new(
            "async_handler_required",
            "agent_dead_letter_list must be handled by handle_request_async",
        )),
        RuntimeHostCommand::AgentDeadLetterGet => Err(RuntimeHostError::new(
            "async_handler_required",
            "agent_dead_letter_get must be handled by handle_request_async",
        )),
        RuntimeHostCommand::AgentDeadLetterDismiss => Err(RuntimeHostError::new(
            "async_handler_required",
            "agent_dead_letter_dismiss must be handled by handle_request_async",
        )),
        RuntimeHostCommand::AgentDeadLetterReplay => Err(RuntimeHostError::new(
            "async_handler_required",
            "agent_dead_letter_replay must be handled by handle_request_async",
        )),
        RuntimeHostCommand::ProcessCapture => {
            let payload = serde_json::from_value(request.payload)?;
            let response = runtime_bridge::process_capture(payload)?;
            serde_json::to_value(response).map_err(|error| {
                RuntimeHostError::new("serialize_response_failed", error.to_string())
            })
        }
        RuntimeHostCommand::SidecarStart => {
            let payload = runtime_bridge::deserialize_request(request.payload)?;
            let response = sidecars::start(&mut state.sidecar_store, payload)
                .map_err(|error| RuntimeHostError::new("sidecar_failed", error))?;
            serde_json::to_value(response).map_err(|error| {
                RuntimeHostError::new("serialize_response_failed", error.to_string())
            })
        }
        RuntimeHostCommand::SidecarStop => {
            let payload = runtime_bridge::deserialize_request(request.payload)?;
            let response = sidecars::stop(&mut state.sidecar_store, payload)
                .map_err(|error| RuntimeHostError::new("sidecar_failed", error))?;
            serde_json::to_value(response).map_err(|error| {
                RuntimeHostError::new("serialize_response_failed", error.to_string())
            })
        }
        RuntimeHostCommand::SidecarList => {
            serde_json::to_value(sidecars::list(&mut state.sidecar_store)).map_err(|error| {
                RuntimeHostError::new("serialize_response_failed", error.to_string())
            })
        }
        RuntimeHostCommand::WorkspaceGet => {
            let response = workspaces::get().map_err(workspace_runtime_host_error)?;
            serde_json::to_value(response).map_err(|error| {
                RuntimeHostError::new("serialize_response_failed", error.to_string())
            })
        }
        RuntimeHostCommand::WorkspaceActivate => {
            let payload = runtime_bridge::deserialize_request(request.payload)?;
            let response = workspaces::activate(payload).map_err(workspace_runtime_host_error)?;
            serde_json::to_value(response).map_err(|error| {
                RuntimeHostError::new("serialize_response_failed", error.to_string())
            })
        }
        RuntimeHostCommand::WorkspaceOpenFolder => {
            let response = workspaces::open_folder().map_err(workspace_runtime_host_error)?;
            serde_json::to_value(response).map_err(|error| {
                RuntimeHostError::new("serialize_response_failed", error.to_string())
            })
        }
        RuntimeHostCommand::WorkspaceRevealFolder => {
            let payload = runtime_bridge::deserialize_request(request.payload)?;
            let response =
                workspaces::reveal_folder(payload).map_err(workspace_runtime_host_error)?;
            serde_json::to_value(response).map_err(|error| {
                RuntimeHostError::new("serialize_response_failed", error.to_string())
            })
        }
        RuntimeHostCommand::WorkspaceRename => {
            let payload = runtime_bridge::deserialize_request(request.payload)?;
            let response = workspaces::rename(payload).map_err(workspace_runtime_host_error)?;
            serde_json::to_value(response).map_err(|error| {
                RuntimeHostError::new("serialize_response_failed", error.to_string())
            })
        }
        RuntimeHostCommand::WorkspaceRemove => {
            let payload = runtime_bridge::deserialize_request(request.payload)?;
            let response = workspaces::remove(payload).map_err(workspace_runtime_host_error)?;
            serde_json::to_value(response).map_err(|error| {
                RuntimeHostError::new("serialize_response_failed", error.to_string())
            })
        }
        RuntimeHostCommand::WorkspaceReset => {
            let payload = runtime_bridge::deserialize_request(request.payload)?;
            let response =
                workspaces::reset_catalog(payload).map_err(workspace_runtime_host_error)?;
            serde_json::to_value(response).map_err(|error| {
                RuntimeHostError::new("serialize_response_failed", error.to_string())
            })
        }
        RuntimeHostCommand::WorkspaceFileTree => {
            let payload = runtime_bridge::deserialize_optional_request(request.payload)?;
            let response = workspaces::file_tree(payload).map_err(workspace_runtime_host_error)?;
            serde_json::to_value(response).map_err(|error| {
                RuntimeHostError::new("serialize_response_failed", error.to_string())
            })
        }
        RuntimeHostCommand::WorkspaceReadFile => {
            let payload = runtime_bridge::deserialize_request(request.payload)?;
            let response = workspaces::read_file(payload).map_err(workspace_runtime_host_error)?;
            serde_json::to_value(response).map_err(|error| {
                RuntimeHostError::new("serialize_response_failed", error.to_string())
            })
        }
        RuntimeHostCommand::WorkspaceGitStatusGet => {
            let payload = runtime_bridge::deserialize_request(request.payload)?;
            let response = workspace_git::status(payload)
                .map_err(|error| RuntimeHostError::new("workspace_git_failed", error))?;
            serde_json::to_value(response).map_err(|error| {
                RuntimeHostError::new("serialize_response_failed", error.to_string())
            })
        }
        RuntimeHostCommand::WorkspaceGitDiffGet => {
            let payload = runtime_bridge::deserialize_request(request.payload)?;
            let response = workspace_git::diff(payload)
                .map_err(|error| RuntimeHostError::new("workspace_git_failed", error))?;
            serde_json::to_value(response).map_err(|error| {
                RuntimeHostError::new("serialize_response_failed", error.to_string())
            })
        }
        RuntimeHostCommand::WorkspaceGitFileDiffGet => {
            let payload = runtime_bridge::deserialize_request(request.payload)?;
            let response = workspace_git::file_diff(payload)
                .map_err(|error| RuntimeHostError::new("workspace_git_failed", error))?;
            serde_json::to_value(response).map_err(|error| {
                RuntimeHostError::new("serialize_response_failed", error.to_string())
            })
        }
        RuntimeHostCommand::WorkspaceGitHubCliStatusGet => {
            let response = workspace_git::github_cli_status();
            serde_json::to_value(response).map_err(|error| {
                RuntimeHostError::new("serialize_response_failed", error.to_string())
            })
        }
        RuntimeHostCommand::DesktopFilePreviewRead => {
            let payload = runtime_bridge::deserialize_request(request.payload)?;
            let response = desktop_file_preview::read(payload)
                .map_err(|error| RuntimeHostError::new("desktop_file_preview_failed", error))?;
            serde_json::to_value(response).map_err(|error| {
                RuntimeHostError::new("serialize_response_failed", error.to_string())
            })
        }
        RuntimeHostCommand::AppExit => {
            event_writer.detach_registered_viewer()?;
            let dispositions = event_writer
                .owner_exited()
                .map_err(RuntimeHostError::transport)?;
            agent_runtime::interrupt_owner_agent_runs(event_writer, dispositions)
                .map_err(|error| RuntimeHostError::new("owner_exit_interrupt_failed", error))?;
            Ok(serde_json::json!({ "ok": true }))
        }
    }
}

pub(crate) async fn handle_request_async(
    state: Arc<Mutex<RuntimeHostState>>,
    request: HostCommandRequest,
    event_writer: EventWriter,
) -> Result<serde_json::Value, RuntimeHostError> {
    let command = RuntimeHostCommand::parse(request.command.as_str())?;
    match command {
        RuntimeHostCommand::McpConfigure => {
            let payload = runtime_bridge::deserialize_request(request.payload)?;
            let response = mcp::configure(payload)
                .await
                .map_err(|error| RuntimeHostError::new("mcp_failed", error))?;
            serde_json::to_value(response).map_err(|error| {
                RuntimeHostError::new("serialize_response_failed", error.to_string())
            })
        }
        RuntimeHostCommand::AgentRuntimeModelTest => {
            let payload = runtime_bridge::deserialize_request(request.payload)?;
            let response = agent_runtime::test_model(payload)
                .await
                .map_err(|error| RuntimeHostError::new("model_test_failed", error))?;
            serde_json::to_value(response).map_err(|error| {
                RuntimeHostError::new("serialize_response_failed", error.to_string())
            })
        }
        RuntimeHostCommand::AgentRuntimeJobList => {
            let payload = runtime_bridge::deserialize_request(request.payload)?;
            let response = runtime_ops::runtime_job_list(payload)
                .await
                .map_err(|error| RuntimeHostError::new("runtime_job_failed", error))?;
            serde_json::to_value(response).map_err(|error| {
                RuntimeHostError::new("serialize_response_failed", error.to_string())
            })
        }
        RuntimeHostCommand::AgentRuntimeJobGet => {
            let payload = runtime_bridge::deserialize_request(request.payload)?;
            let response = runtime_ops::runtime_job_get(payload)
                .await
                .map_err(|error| RuntimeHostError::new("runtime_job_failed", error))?;
            serde_json::to_value(response).map_err(|error| {
                RuntimeHostError::new("serialize_response_failed", error.to_string())
            })
        }
        RuntimeHostCommand::AgentDeadLetterList => {
            let payload = runtime_bridge::deserialize_request(request.payload)?;
            let response = runtime_ops::dead_letter_list(payload)
                .await
                .map_err(|error| RuntimeHostError::new("dead_letter_failed", error))?;
            serde_json::to_value(response).map_err(|error| {
                RuntimeHostError::new("serialize_response_failed", error.to_string())
            })
        }
        RuntimeHostCommand::AgentDeadLetterGet => {
            let payload = runtime_bridge::deserialize_request(request.payload)?;
            let response = runtime_ops::dead_letter_get(payload)
                .await
                .map_err(|error| RuntimeHostError::new("dead_letter_failed", error))?;
            serde_json::to_value(response).map_err(|error| {
                RuntimeHostError::new("serialize_response_failed", error.to_string())
            })
        }
        RuntimeHostCommand::AgentDeadLetterDismiss => {
            let payload = runtime_bridge::deserialize_request(request.payload)?;
            let response = runtime_ops::dead_letter_dismiss(payload)
                .await
                .map_err(|error| RuntimeHostError::new("dead_letter_failed", error))?;
            serde_json::to_value(response).map_err(|error| {
                RuntimeHostError::new("serialize_response_failed", error.to_string())
            })
        }
        RuntimeHostCommand::AgentDeadLetterReplay => {
            let payload = runtime_bridge::deserialize_request(request.payload)?;
            let response = runtime_ops::dead_letter_replay(payload)
                .await
                .map_err(|error| RuntimeHostError::new("dead_letter_failed", error))?;
            serde_json::to_value(response).map_err(|error| {
                RuntimeHostError::new("serialize_response_failed", error.to_string())
            })
        }
        RuntimeHostCommand::AgentQuestionAnswer => {
            let payload = runtime_bridge::deserialize_request(request.payload)?;
            let response = agent_runtime::question_answer_async(event_writer, payload)
                .await
                .map_err(|error| RuntimeHostError::new("session_answer_question_failed", error))?;
            serde_json::to_value(response).map_err(|error| {
                RuntimeHostError::new("serialize_response_failed", error.to_string())
            })
        }
        RuntimeHostCommand::AgentContextCompact => {
            let payload = runtime_bridge::deserialize_request(request.payload)?;
            let response = agent_runtime::compact_context(event_writer, payload)
                .await
                .map_err(|error| RuntimeHostError::new("context_compaction_failed", error))?;
            serde_json::to_value(response).map_err(|error| {
                RuntimeHostError::new("serialize_response_failed", error.to_string())
            })
        }
        _ if command_owns_store_lock(command) => tokio::task::spawn_blocking(move || {
            let mut isolated_state = RuntimeHostState::default();
            handle_request(&mut isolated_state, request, event_writer)
        })
        .await
        .map_err(|error| {
            RuntimeHostError::transport(format!(
                "independent store handler blocking owner join failed: {error}"
            ))
        })?,
        _ => tokio::task::spawn_blocking(move || {
            let mut state_guard = state
                .lock()
                .map_err(|_| RuntimeHostError::transport("Runtime Host state lock poisoned"))?;
            handle_request(&mut state_guard, request, event_writer)
        })
        .await
        .map_err(|error| {
            RuntimeHostError::transport(format!(
                "Runtime Host sync handler blocking owner join failed: {error}"
            ))
        })?,
    }
}

fn command_owns_store_lock(command: RuntimeHostCommand) -> bool {
    matches!(
        command,
        RuntimeHostCommand::AgentRuntimeConfigGet
            | RuntimeHostCommand::AgentRuntimeConfigReset
            | RuntimeHostCommand::AgentRuntimeConfigSet
            | RuntimeHostCommand::McpCatalog
            | RuntimeHostCommand::PluginList
            | RuntimeHostCommand::PluginDetail
            | RuntimeHostCommand::PluginInstall
            | RuntimeHostCommand::PluginRemove
            | RuntimeHostCommand::PluginSetEnabled
            | RuntimeHostCommand::PluginReload
            | RuntimeHostCommand::PluginSourceRef
            | RuntimeHostCommand::PluginCatalogState
            | RuntimeHostCommand::SkillSourceList
            | RuntimeHostCommand::SkillSourceAdd
            | RuntimeHostCommand::SkillSourceRemove
            | RuntimeHostCommand::SkillSourceSetEnabled
            | RuntimeHostCommand::SkillSourceRef
            | RuntimeHostCommand::SkillCatalog
            | RuntimeHostCommand::SkillReload
            | RuntimeHostCommand::SkillDetail
            | RuntimeHostCommand::SkillSetEnabled
    )
}

pub(crate) async fn handle_stateless_request_async(
    request: &HostCommandRequest,
) -> Result<Option<serde_json::Value>, RuntimeHostError> {
    let command = RuntimeHostCommand::parse(request.command.as_str())?;
    match command {
        RuntimeHostCommand::ProcessCapture => {
            let request = request.clone();
            tokio::task::spawn_blocking(move || handle_stateless_request(&request))
                .await
                .map_err(|error| {
                    RuntimeHostError::transport(format!(
                        "Runtime Host stateless blocking owner join failed: {error}"
                    ))
                })?
        }
        _ => Ok(None),
    }
}

pub(crate) fn handle_stateless_request(
    request: &HostCommandRequest,
) -> Result<Option<serde_json::Value>, RuntimeHostError> {
    let command = RuntimeHostCommand::parse(request.command.as_str())?;
    match command {
        RuntimeHostCommand::ProcessCapture => {
            let payload = serde_json::from_value(request.payload.clone())?;
            let response = runtime_bridge::process_capture(payload)?;
            Ok(Some(serde_json::to_value(response).map_err(|error| {
                RuntimeHostError::new("serialize_response_failed", error.to_string())
            })?))
        }
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_settings_stores_do_not_wait_for_shared_runtime_host_state() {
        assert!(command_owns_store_lock(
            RuntimeHostCommand::AgentRuntimeConfigGet
        ));
        assert!(command_owns_store_lock(RuntimeHostCommand::PluginDetail));
        assert!(command_owns_store_lock(RuntimeHostCommand::SkillCatalog));
        assert!(!command_owns_store_lock(RuntimeHostCommand::SidecarStart));
        assert!(!command_owns_store_lock(RuntimeHostCommand::WorkspaceGet));
    }
}
