use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use centaeris_core::execution::{ExecutionError, ExecutionPolicy, NetworkPolicy};

use super::PreparedSandboxCommand;

pub(super) fn policy_enforced() -> bool {
    false
}

pub(super) fn check_available(
    _runtime: &Path,
    policy: &ExecutionPolicy,
) -> Result<(), ExecutionError> {
    validate_policy(policy)
}

pub(super) fn prepare_command(
    _runtime_executable: &Path,
    program: &Path,
    args: &[String],
    cwd: &Path,
    environment: &HashMap<String, String>,
    policy: &ExecutionPolicy,
    _preserve_background: bool,
) -> Result<PreparedSandboxCommand, ExecutionError> {
    validate_policy(policy)?;
    let mut command = Command::new(program);
    command.args(args).current_dir(cwd).envs(environment);
    Ok(PreparedSandboxCommand {
        command,
        stdin_input: None,
    })
}

fn validate_policy(policy: &ExecutionPolicy) -> Result<(), ExecutionError> {
    if matches!(policy.network, NetworkPolicy::PublicInternet) {
        return Ok(());
    }
    Err(ExecutionError::PolicyUnavailable {
        reason: "local host execution supports only the publicInternet network policy".to_string(),
    })
}
