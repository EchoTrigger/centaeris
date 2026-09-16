use std::collections::HashMap;
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;

use super::nono_policy::{capabilities, unavailable};
use super::PreparedSandboxCommand;
use centaeris_core::execution::{ExecutionError, ExecutionPolicy};
use nono::Sandbox;

pub(super) fn policy_enforced() -> bool {
    true
}

pub(super) fn check_available(
    runtime: &Path,
    policy: &ExecutionPolicy,
) -> Result<(), ExecutionError> {
    let mut prepared = prepare_command(
        runtime,
        Path::new("/usr/bin/true"),
        &[],
        &policy.filesystem.workspace_root,
        &HashMap::new(),
        policy,
        false,
    )?;
    let status = prepared
        .command
        .stdout(std::process::Stdio::null())
        .status()
        .map_err(unavailable)?;
    if !status.success() {
        return Err(unavailable("macOS nono capability probe failed"));
    }
    Ok(())
}

pub(super) fn prepare_command(
    runtime: &Path,
    program: &Path,
    args: &[String],
    cwd: &Path,
    environment: &HashMap<String, String>,
    policy: &ExecutionPolicy,
    _preserve_background: bool,
) -> Result<PreparedSandboxCommand, ExecutionError> {
    // Validate before spawning. The child repeats preparation immediately
    // before applying Seatbelt, so no parent thread is sandboxed.
    capabilities(policy, program, runtime, None, None)?;
    let policy_json = serde_json::to_string(policy).map_err(unavailable)?;
    let mut command = Command::new(runtime);
    command
        .arg("--local-macos-launcher")
        .arg(policy_json)
        .arg("--")
        .arg(program)
        .args(args)
        .current_dir(cwd)
        .envs(environment);
    Ok(PreparedSandboxCommand {
        command,
        completion: None,
        stdin_input: None,
    })
}

pub(super) fn run_launcher(arguments: &[String]) -> Result<(), String> {
    if arguments.len() < 3 || arguments[1] != "--" {
        return Err("macOS launcher requires a policy and a command after --".into());
    }
    let policy: ExecutionPolicy = serde_json::from_str(&arguments[0]).map_err(|e| e.to_string())?;
    let runtime = std::env::current_exe().map_err(|e| e.to_string())?;
    let caps = capabilities(&policy, Path::new(&arguments[2]), &runtime, None, None)
        .map_err(|e| e.internal_debug_message())?;
    // This entry runs before Tokio or any worker thread exists. nono's macOS
    // API allocates while constructing SBPL, so it must not run in pre_exec.
    Sandbox::apply_auto(&caps).map_err(|e| e.to_string())?;
    std::io::stdout()
        .write_all(super::POLICY_HANDSHAKE)
        .map_err(|e| e.to_string())?;
    std::io::stdout().flush().map_err(|e| e.to_string())?;
    Err(Command::new(&arguments[2])
        .args(&arguments[3..])
        .exec()
        .to_string())
}
