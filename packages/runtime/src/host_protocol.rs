#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostCommandScope {
    SharedRuntime,
    ExecutionHost,
    HostSurface,
}

pub const CENTAERIS_RUNTIME_PROTOCOL_NAME: &str = "centaeris.runtime";
pub const CENTAERIS_RUNTIME_PROTOCOL_VERSION: u32 = 1;

pub const CENTAERIS_RUNTIME_PROTOCOL_CAPABILITIES: &[&str] = &[
    "json_rpc_2_over_jsonl",
    "session_log",
    "question_resume",
    "agent_run_intervention",
    "stream_replay",
    "runtime_store_actor",
];

pub const CENTAERIS_RUNTIME_PROTOCOL_EVENTS: &[&str] =
    &["session/update", "runtime/config-changed"];

pub const CENTAERIS_RUNTIME_PROTOCOL_PROJECTIONS: &[&str] = &[
    "runtime_event",
    "session_event",
    "session_projection",
    "agent_state",
    "headless_transcript",
];

pub const SHARED_RUNTIME_COMMANDS: &[&str] = &[
    "agent_context_usage_get",
    "_centaeris/session/compact_context",
    "agent_dead_letter_dismiss",
    "agent_dead_letter_get",
    "agent_dead_letter_list",
    "agent_dead_letter_replay",
    "_centaeris/session/activate",
    "_centaeris/session/answer_now",
    "_centaeris/session/answer_question",
    "_centaeris/session/delete",
    "_centaeris/session/diagnostics",
    "_centaeris/session/project",
    "_centaeris/session/reorder",
    "_centaeris/session/agent-runs",
    "_centaeris/session/agent-runs/replay",
    "_centaeris/session/agent-runs/attach",
    "_centaeris/session/agent-runs/detach",
    "_centaeris/session/agent-runs/detach-viewer",
    "_centaeris/session/agent-runs/cancel",
    "_centaeris/session/supplement",
    "_centaeris/session/update_metadata",
    "agent_runtime_config_get",
    "agent_runtime_config_reset",
    "agent_runtime_config_set",
    "agent_runtime_model_test",
    "mcp/catalog",
    "mcp/configure",
    "agent_runtime_garbage_collect",
    "agent_runtime_job_get",
    "agent_runtime_job_list",
    "agent_state_get",
    "transcript/project",
    "plugin/catalog_state",
    "skill/source/list",
    "skill/source/add",
    "skill/source/remove",
    "skill/source/set_enabled",
    "skill/source/ref",
    "skill/catalog",
    "skill/detail",
    "skill/set_enabled",
    "skill/reload",
    "plugin/detail",
    "plugin/list",
    "plugin/reload",
    "plugin/set_enabled",
    "plugin/source_ref",
    "session/list",
    "session/load",
    "session/new",
    "session/prompt",
];

pub const EXECUTION_HOST_COMMANDS: &[&str] = &[
    "process_capture",
    "sidecar_list",
    "sidecar_start",
    "sidecar_stop",
    "workspace_file_tree",
    "workspace_read_file",
];

pub const HOST_SURFACE_COMMANDS: &[&str] = &[
    "app_exit",
    "desktop_file_preview_read",
    "initialize",
    "plugin/install",
    "plugin/remove",
    "workspace_activate",
    "workspace_get",
    "workspace_git_diff_get",
    "workspace_git_file_diff_get",
    "workspace_git_github_cli_status_get",
    "workspace_git_status_get",
    "workspace_open_folder",
    "workspace_remove",
    "workspace_reset",
    "workspace_rename",
    "workspace_reveal_folder",
];

pub fn command_scope(command: &str) -> Option<HostCommandScope> {
    let command = command.trim();
    if SHARED_RUNTIME_COMMANDS.contains(&command) {
        Some(HostCommandScope::SharedRuntime)
    } else if EXECUTION_HOST_COMMANDS.contains(&command) {
        Some(HostCommandScope::ExecutionHost)
    } else if HOST_SURFACE_COMMANDS.contains(&command) {
        Some(HostCommandScope::HostSurface)
    } else {
        None
    }
}

pub fn is_registered_host_command(command: &str) -> bool {
    command_scope(command).is_some()
}

#[cfg(test)]
mod tests {
    use super::{
        command_scope, HostCommandScope, CENTAERIS_RUNTIME_PROTOCOL_EVENTS,
        CENTAERIS_RUNTIME_PROTOCOL_NAME, CENTAERIS_RUNTIME_PROTOCOL_PROJECTIONS,
        CENTAERIS_RUNTIME_PROTOCOL_VERSION,
    };

    #[test]
    fn classifies_shared_runtime_commands() {
        assert_eq!(
            command_scope("session/list"),
            Some(HostCommandScope::SharedRuntime)
        );
        assert_eq!(
            command_scope("plugin/list"),
            Some(HostCommandScope::SharedRuntime)
        );
    }

    #[test]
    fn keeps_host_surface_out_of_shared_runtime() {
        assert_eq!(
            command_scope("desktop_file_preview_read"),
            Some(HostCommandScope::HostSurface)
        );
        assert_eq!(
            command_scope("workspace_file_tree"),
            Some(HostCommandScope::ExecutionHost)
        );
        assert_eq!(command_scope("missing_command"), None);
    }

    #[test]
    fn centaeris_runtime_protocol_v1_descriptor_keeps_current_native_surface() {
        assert_eq!(CENTAERIS_RUNTIME_PROTOCOL_NAME, "centaeris.runtime");
        assert_eq!(CENTAERIS_RUNTIME_PROTOCOL_VERSION, 1);
        assert!(CENTAERIS_RUNTIME_PROTOCOL_EVENTS.contains(&"session/update"));
        assert!(CENTAERIS_RUNTIME_PROTOCOL_EVENTS.contains(&"runtime/config-changed"));
        for projection in ["runtime_event", "session_event"] {
            assert!(CENTAERIS_RUNTIME_PROTOCOL_PROJECTIONS.contains(&projection));
        }
    }
}
