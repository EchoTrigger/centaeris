use crate::errors::RuntimeHostError;
use crate::host_protocol;
use crate::runtime_command_registry::runtime_commands;

macro_rules! define_runtime_host_commands {
    ($( $variant:ident, $command:literal, $scope:ident, $operation_kind:ident, $retry_policy:ident, $reconcile_method:expr; )*) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub(crate) enum RuntimeHostCommand {
            $( $variant, )*
        }

        impl RuntimeHostCommand {
            pub(crate) fn parse(raw: &str) -> Result<Self, RuntimeHostError> {
                match raw.trim() {
                    $( $command => Ok(Self::$variant), )*
                    "" => Err(RuntimeHostError::invalid_request("command is required")),
                    value if host_protocol::is_registered_host_command(value) => Err(
                        RuntimeHostError::new("sidecar_command_not_implemented", value),
                    ),
                    value => Err(RuntimeHostError::unknown_command(value)),
                }
            }

            #[cfg(test)]
            pub(crate) const fn command(self) -> &'static str {
                match self {
                    $( Self::$variant => $command, )*
                }
            }
        }
    };
}

runtime_commands!(define_runtime_host_commands);

#[cfg(test)]
mod tests {
    use super::RuntimeHostCommand;
    use crate::host_protocol::{self, RUNTIME_COMMANDS};
    use std::collections::HashSet;

    #[test]
    fn known_runtime_host_commands_are_registered_in_host_protocol() {
        let mut unique = HashSet::new();
        for descriptor in RUNTIME_COMMANDS {
            assert!(unique.insert(descriptor.command));
            assert_eq!(
                host_protocol::command_scope(descriptor.command),
                Some(descriptor.scope)
            );
            let parsed = RuntimeHostCommand::parse(descriptor.command)
                .expect("registered command must parse");
            assert_eq!(parsed.command(), descriptor.command);
        }
    }
}
