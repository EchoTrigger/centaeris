use crate::runtime_command_registry::{runtime_commands, RuntimeOperationKind, RuntimeRetryPolicy};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostCommandDescriptor {
    pub command: &'static str,
    pub scope: HostCommandScope,
    pub operation_kind: &'static str,
    pub retry_policy: &'static str,
    pub reconcile_method: Option<&'static str>,
}

macro_rules! define_host_command_descriptors {
    ($( $variant:ident, $command:literal, $scope:ident, $operation_kind:ident, $retry_policy:ident, $reconcile_method:expr; )*) => {
        pub const RUNTIME_COMMANDS: &[HostCommandDescriptor] = &[
            $(
                HostCommandDescriptor {
                    command: $command,
                    scope: HostCommandScope::$scope,
                    operation_kind: RuntimeOperationKind::$operation_kind.as_str(),
                    retry_policy: RuntimeRetryPolicy::$retry_policy.as_str(),
                    reconcile_method: $reconcile_method,
                },
            )*
        ];
    };
}

runtime_commands!(define_host_command_descriptors);

pub fn command_scope(command: &str) -> Option<HostCommandScope> {
    let command = command.trim();
    RUNTIME_COMMANDS
        .iter()
        .find(|descriptor| descriptor.command == command)
        .map(|descriptor| descriptor.scope)
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
