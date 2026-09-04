use std::collections::HashMap;
use std::env;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use centaeris_core::execution::sandbox::{
    NetworkSandboxPolicy, SandboxErr, SandboxPolicy, SandboxType,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

use super::{CompletionMarker, PreparedSandboxCommand};

const HOST_LAUNCH_SCHEMA: &str = "centaeris_windows_host_launch_v1";
const MAX_HELPER_INPUT_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
struct HostLaunchRequest {
    schema: String,
    program: PathBuf,
    args: Vec<String>,
    cwd: PathBuf,
    environment_overrides: HashMap<String, String>,
}

impl HostLaunchRequest {
    fn validate(&self) -> Result<(), String> {
        if self.schema != HOST_LAUNCH_SCHEMA {
            return Err(format!(
                "unsupported Windows host launch schema: {}",
                self.schema
            ));
        }
        let program = validate_git_bash(self.program.as_path()).ok_or_else(|| {
            format!(
                "Windows host launch requires a verified Git for Windows Bash executable: {}",
                self.program.display()
            )
        })?;
        if program != self.program {
            return Err("Windows host launch program must be canonical".to_string());
        }
        if !self.cwd.is_absolute() || !self.cwd.is_dir() {
            return Err(format!(
                "Windows host launch working directory is unavailable: {}",
                self.cwd.display()
            ));
        }
        if self.args.len() > 4_096
            || self
                .args
                .iter()
                .any(|argument| argument.len() > MAX_HELPER_INPUT_BYTES || argument.contains('\0'))
        {
            return Err("Windows host launch command limits were exceeded".to_string());
        }
        for (key, value) in &self.environment_overrides {
            if key.is_empty() || key.contains(['=', '\0']) || value.contains('\0') {
                return Err(format!(
                    "Windows host launch environment override is invalid: {key}"
                ));
            }
        }
        Ok(())
    }
}

pub(super) fn sandbox_type() -> SandboxType {
    SandboxType::HostProcess
}

pub(super) fn ensure_available() -> Result<PathBuf, SandboxErr> {
    resolve_git_bash_path(None)
}

pub(super) fn validate_policy(policy: &SandboxPolicy) -> Result<(), SandboxErr> {
    if matches!(policy.network, NetworkSandboxPolicy::PublicInternet) {
        return Ok(());
    }
    Err(SandboxErr::Unavailable {
        reason: "Windows Git Bash HostProcess supports only the publicInternet network policy"
            .to_string(),
        sandbox_type: Some(sandbox_type()),
    })
}

pub(super) fn prepare_command(
    runtime_executable: &Path,
    program: &Path,
    args: &[String],
    cwd: &Path,
    environment_overrides: &HashMap<String, String>,
    policy: &SandboxPolicy,
    _preserve_background: bool,
) -> Result<PreparedSandboxCommand, SandboxErr> {
    validate_policy(policy)?;
    let program = validate_git_bash(program).ok_or_else(|| SandboxErr::Denied {
        reason: format!(
            "Windows host execution only accepts a verified Git for Windows Bash executable: {}",
            program.display()
        ),
        sandbox_type: sandbox_type(),
    })?;
    let request = HostLaunchRequest {
        schema: HOST_LAUNCH_SCHEMA.to_string(),
        program,
        args: args.to_vec(),
        cwd: cwd.to_path_buf(),
        environment_overrides: environment_overrides.clone(),
    };
    request.validate().map_err(SandboxErr::Io)?;
    let input = serde_json::to_vec(&request)
        .map_err(|error| SandboxErr::Io(format!("encode Windows host launch failed: {error}")))?;
    if input.is_empty() || input.len() > MAX_HELPER_INPUT_BYTES {
        return Err(SandboxErr::Io(
            "Windows host launch request size is invalid".to_string(),
        ));
    }
    let mut command = Command::new(runtime_executable);
    command.arg("--windows-host-launcher").current_dir(cwd);
    Ok(PreparedSandboxCommand {
        command,
        completion: None,
        stdin_input: Some(input),
    })
}

pub(super) fn terminate(_completion: &CompletionMarker) -> Result<(), SandboxErr> {
    Ok(())
}

pub fn run_windows_host_launcher() -> Result<i32, String> {
    let request: HostLaunchRequest = read_helper_input()?;
    request.validate()?;
    let mut command = Command::new(request.program);
    command
        .args(request.args)
        .current_dir(request.cwd)
        .envs(request.environment_overrides)
        .stdin(Stdio::null());
    super::configure_local_process(&mut command);
    command
        .status()
        .map_err(|error| format!("run Windows Git Bash host command failed: {error}"))
        .map(|status| status.code().unwrap_or(1))
}

pub(super) fn resolve_git_bash_path(
    explicit_bash_path: Option<PathBuf>,
) -> Result<PathBuf, SandboxErr> {
    if let Some(path) = explicit_bash_path {
        return validate_git_bash(path.as_path()).ok_or_else(|| SandboxErr::Unavailable {
            reason: format!(
                "configured Git for Windows Bash executable is unavailable or invalid: {}",
                path.display()
            ),
            sandbox_type: Some(sandbox_type()),
        });
    }

    let mut candidates = Vec::new();
    for variable in ["ProgramW6432", "ProgramFiles", "ProgramFiles(x86)"] {
        if let Some(root) = env::var_os(variable) {
            candidates.push(PathBuf::from(root).join("Git/bin/bash.exe"));
        }
    }
    if let Some(root) = env::var_os("LOCALAPPDATA") {
        candidates.push(PathBuf::from(root).join("Programs/Git/bin/bash.exe"));
    }
    if let Some(git) = find_on_path("git.exe") {
        if let Some(parent) = git.parent() {
            candidates.push(parent.join("../bin/bash.exe"));
            candidates.push(parent.join("../../bin/bash.exe"));
        }
    }
    if let Some(bash) = find_on_path("bash.exe") {
        candidates.push(bash);
    }

    candidates
        .iter()
        .find_map(|candidate| validate_git_bash(candidate.as_path()))
        .ok_or_else(|| SandboxErr::Unavailable {
            reason: "Git for Windows is required for local execution on Windows; install it from https://git-scm.com/download/win"
                .to_string(),
            sandbox_type: Some(sandbox_type()),
        })
}

fn validate_git_bash(path: &Path) -> Option<PathBuf> {
    if !path.is_absolute() || !path.is_file() {
        return None;
    }
    let canonical = path.canonicalize().ok()?;
    let directory = canonical.parent()?;
    [
        directory.join("msys-2.0.dll"),
        directory.join("../usr/bin/msys-2.0.dll"),
        directory.join("../../usr/bin/msys-2.0.dll"),
    ]
    .iter()
    .any(|candidate| candidate.is_file())
    .then_some(canonical)
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    env::var_os("PATH").and_then(|path| {
        env::split_paths(&path)
            .map(|directory| directory.join(name))
            .find(|candidate| candidate.is_file())
    })
}

fn read_helper_input<T: DeserializeOwned>() -> Result<T, String> {
    let mut input = Vec::new();
    std::io::stdin()
        .lock()
        .take((MAX_HELPER_INPUT_BYTES + 1) as u64)
        .read_to_end(&mut input)
        .map_err(|error| format!("read Windows host launch request failed: {error}"))?;
    if input.is_empty() || input.len() > MAX_HELPER_INPUT_BYTES {
        return Err("Windows host launch request size is invalid".to_string());
    }
    serde_json::from_slice(input.as_slice())
        .map_err(|error| format!("decode Windows host launch request failed: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_git_bash_path_must_be_absolute() {
        let error = resolve_git_bash_path(Some(PathBuf::from("banana/bash.exe")))
            .expect_err("relative Bash path must fail");
        assert!(matches!(error, SandboxErr::Unavailable { .. }));
    }

    #[test]
    fn host_launch_request_rejects_unknown_fields() {
        let error = serde_json::from_value::<HostLaunchRequest>(serde_json::json!({
            "schema": HOST_LAUNCH_SCHEMA,
            "program": "C:\\Git\\bin\\bash.exe",
            "args": [],
            "cwd": "C:\\workspace",
            "environment_overrides": {},
            "banana": true
        }))
        .expect_err("unknown host launch fields must fail");
        assert!(error.to_string().contains("banana"));
    }
}
