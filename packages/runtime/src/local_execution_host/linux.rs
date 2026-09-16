mod isolation;
mod process_scope;
pub(super) use process_scope::ProcessScope;

use std::env;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use centaeris_core::execution::{ExecutionError, ExecutionPolicy};

use super::{CompletionMarker, PreparedSandboxCommand};

static MARKER_NONCE: AtomicU64 = AtomicU64::new(0);

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
        &std::collections::HashMap::new(),
        policy,
        false,
    )?;
    let status = prepared
        .command
        .status()
        .map_err(|error| ExecutionError::PolicyUnavailable {
            reason: format!("nono capability probe failed: {error}"),
        })?;
    if !status.success() {
        return Err(ExecutionError::PolicyUnavailable {
            reason: "nono capability probe failed".to_string(),
        });
    }
    Ok(())
}

pub(super) fn prepare_command(
    runtime_executable: &Path,
    program: &Path,
    args: &[String],
    cwd: &Path,
    env_overrides: &std::collections::HashMap<String, String>,
    policy: &ExecutionPolicy,
    preserve_background: bool,
) -> Result<PreparedSandboxCommand, ExecutionError> {
    let scope = ProcessScope::new()?;
    let completion = if preserve_background {
        Some(create_completion_marker()?)
    } else {
        None
    };
    let sandbox = isolation::prepare(
        policy,
        program,
        runtime_executable,
        &scope.scratch,
        completion.as_ref().map(|marker| marker.root.as_path()),
    )?;
    let mut command = if let Some(completion) = &completion {
        let mut command = Command::new(runtime_executable);
        command
            .arg("--local-sandbox-supervisor")
            .arg(&completion.path)
            .arg("--")
            .arg(program)
            .args(args);
        command
    } else {
        let mut command = Command::new(program);
        command.args(args);
        command
    };
    command.current_dir(cwd).envs(env_overrides);
    for key in ["TMPDIR", "TMP", "TEMP"] {
        command.env(key, &scope.scratch);
    }
    scope.attach(&mut command)?;
    isolation::attach(&mut command, sandbox);
    Ok(PreparedSandboxCommand {
        command,
        completion,
        stdin_input: None,
        scope: Some(scope),
    })
}

fn create_completion_marker() -> Result<CompletionMarker, ExecutionError> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| ExecutionError::Io(format!("system clock is invalid: {error}")))?
        .as_nanos();
    let nonce = MARKER_NONCE.fetch_add(1, Ordering::Relaxed);
    let root = env::temp_dir().join(format!(
        "centaeris-sandbox-supervisor-{}-{timestamp}-{nonce}",
        std::process::id()
    ));
    fs::create_dir(&root).map_err(|error| {
        ExecutionError::Io(format!("create sandbox supervisor state failed: {error}"))
    })?;
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).map_err(|error| {
        ExecutionError::Io(format!("protect sandbox supervisor state failed: {error}"))
    })?;
    Ok(CompletionMarker {
        path: root.join("shell-exit"),
        root,
        control_path: None,
        secret: super::completion_secret()?,
    })
}

pub(crate) fn run_supervisor(arguments: &[String]) -> Result<i32, String> {
    // Become the reaper before starting user code. cgroup ownership, rather
    // than a PID namespace or process group, handles forced termination.
    if unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) } != 0 {
        return Err(format!(
            "configure command reaper: {}",
            std::io::Error::last_os_error()
        ));
    }
    let mut secret = String::new();
    std::io::stdin()
        .read_line(&mut secret)
        .map_err(|error| format!("sandbox supervisor failed to read completion secret: {error}"))?;
    let secret = secret.trim_end_matches(['\r', '\n']);
    if secret.len() != 64 || !secret.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("sandbox supervisor completion secret is invalid".to_string());
    }
    let (marker, command) = arguments
        .split_first()
        .ok_or_else(|| "sandbox supervisor completion path is required".to_string())?;
    let command = command
        .strip_prefix(&["--".to_string()])
        .ok_or_else(|| "sandbox supervisor command separator is required".to_string())?;
    let (program, args) = command
        .split_first()
        .ok_or_else(|| "sandbox supervisor command is required".to_string())?;
    let mut child = Command::new(program)
        .args(args)
        .spawn()
        .map_err(|error| format!("sandbox supervisor failed to start command: {error}"))?;
    let status = child
        .wait()
        .map_err(|error| format!("sandbox supervisor failed to wait for command: {error}"))?;
    use std::os::unix::process::ExitStatusExt;
    let exit_code = status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(libc::SIGKILL));
    let marker_path = Path::new(marker);
    let staging = marker_path.with_extension(format!("tmp-{}", std::process::id()));
    fs::write(
        &staging,
        format!(
            "{exit_code}:{}\n",
            super::completion_digest(secret, exit_code)
        ),
    )
    .and_then(|_| fs::rename(&staging, marker_path))
    .map_err(|error| format!("sandbox supervisor failed to report command exit: {error}"))?;

    loop {
        let waited = unsafe { libc::waitpid(-1, std::ptr::null_mut(), 0) };
        if waited > 0 {
            continue;
        }
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        if error.raw_os_error() == Some(libc::ECHILD) {
            break;
        }
        return Err(format!(
            "sandbox supervisor failed to reap descendants: {error}"
        ));
    }
    let _ = fs::remove_file(marker_path);
    let _ = fs::remove_dir(marker_path.parent().unwrap_or(Path::new("/")));
    Ok(exit_code)
}
