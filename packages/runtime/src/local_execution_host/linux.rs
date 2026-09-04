use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use centaeris_core::execution::sandbox::{
    NetworkSandboxPolicy, SandboxErr, SandboxPolicy, SandboxType,
};

use super::{CompletionMarker, PreparedSandboxCommand};

static MARKER_NONCE: AtomicU64 = AtomicU64::new(0);

pub(super) fn sandbox_type() -> SandboxType {
    SandboxType::LinuxBubblewrap
}

pub(super) fn ensure_available() -> Result<PathBuf, SandboxErr> {
    let path = find_on_path("bwrap").ok_or_else(|| SandboxErr::Unavailable {
        reason: "bubblewrap is required for local execution on Linux".to_string(),
        sandbox_type: Some(sandbox_type()),
    })?;
    let status = Command::new(path.as_path())
        .args([
            "--unshare-user",
            "--unshare-pid",
            "--as-pid-1",
            "--unshare-net",
            "--unshare-ipc",
            "--unshare-uts",
            "--new-session",
            "--die-with-parent",
            "--ro-bind",
            "/",
            "/",
            "--proc",
            "/proc",
            "--dev",
            "/dev",
            "--tmpfs",
            "/tmp",
            "--tmpfs",
            "/var/tmp",
            "--tmpfs",
            "/run",
            "--",
            "/bin/true",
        ])
        .status()
        .map_err(|error| SandboxErr::Unavailable {
            reason: format!("start bubblewrap capability probe failed: {error}"),
            sandbox_type: Some(sandbox_type()),
        })?;
    if !status.success() {
        return Err(SandboxErr::Unavailable {
            reason: "bubblewrap capability probe failed".to_string(),
            sandbox_type: Some(sandbox_type()),
        });
    }
    Ok(path)
}

pub(super) fn prepare_command(
    runtime_executable: &Path,
    program: &Path,
    args: &[String],
    cwd: &Path,
    env_overrides: &std::collections::HashMap<String, String>,
    policy: &SandboxPolicy,
    preserve_background: bool,
) -> Result<PreparedSandboxCommand, SandboxErr> {
    if matches!(policy.network, NetworkSandboxPolicy::Allowlist { .. }) {
        return Err(SandboxErr::Unavailable {
            reason: "local Linux sandbox does not support network allowlists".to_string(),
            sandbox_type: Some(sandbox_type()),
        });
    }
    super::materialize_denied_paths(policy, sandbox_type())?;
    let bwrap = ensure_available()?;
    let mut command = Command::new(bwrap);
    command
        .args([
            "--unshare-user",
            "--unshare-pid",
            "--as-pid-1",
            "--unshare-ipc",
            "--unshare-uts",
            "--unshare-cgroup-try",
            "--new-session",
            "--die-with-parent",
        ])
        .args(matches!(policy.network, NetworkSandboxPolicy::Disabled).then_some("--unshare-net"))
        .args(["--ro-bind", "/", "/", "--proc", "/proc", "--dev", "/dev"])
        .args(["--tmpfs", "/tmp", "--tmpfs", "/var/tmp", "--tmpfs", "/run"]);

    for root in &policy.filesystem.read_only_roots {
        if !policy.filesystem.writable_roots.contains(root) {
            command.arg("--ro-bind").arg(root).arg(root);
        }
    }
    for root in &policy.filesystem.writable_roots {
        command.arg("--bind").arg(root).arg(root);
    }
    for path in &policy.filesystem.denied_write_paths {
        if path.exists() {
            command.arg("--ro-bind").arg(path).arg(path);
        }
    }
    for path in &policy.filesystem.denied_read_paths {
        if path.is_dir() {
            command
                .arg("--tmpfs")
                .arg(path)
                .arg("--remount-ro")
                .arg(path);
        } else if path.is_file() {
            command.arg("--ro-bind").arg("/dev/null").arg(path);
        }
    }

    let completion = if preserve_background {
        let completion = create_completion_marker()?;
        command
            .arg("--bind")
            .arg(completion.root.as_path())
            .arg(completion.root.as_path());
        Some(completion)
    } else {
        None
    };

    command.arg("--chdir").arg(cwd).arg("--");
    if let Some(completion) = completion.as_ref() {
        command
            .arg(runtime_executable)
            .arg("--local-sandbox-supervisor")
            .arg(completion.path.as_path())
            .arg("--")
            .arg(program)
            .args(args);
    } else {
        command.arg(program).args(args);
    }
    command.current_dir(cwd).envs(env_overrides);
    Ok(PreparedSandboxCommand {
        command,
        completion,
        stdin_input: None,
    })
}

fn create_completion_marker() -> Result<CompletionMarker, SandboxErr> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| SandboxErr::Io(format!("system clock is invalid: {error}")))?
        .as_nanos();
    let nonce = MARKER_NONCE.fetch_add(1, Ordering::Relaxed);
    let root = env::temp_dir().join(format!(
        "centaeris-sandbox-supervisor-{}-{timestamp}-{nonce}",
        std::process::id()
    ));
    fs::create_dir(&root).map_err(|error| {
        SandboxErr::Io(format!("create sandbox supervisor state failed: {error}"))
    })?;
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).map_err(|error| {
        SandboxErr::Io(format!("protect sandbox supervisor state failed: {error}"))
    })?;
    Ok(CompletionMarker {
        path: root.join("shell-exit"),
        root,
        control_path: None,
        secret: super::completion_secret()?,
    })
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    env::var_os("PATH").and_then(|path| {
        env::split_paths(&path)
            .map(|directory| directory.join(name))
            .find(|candidate| candidate.is_file())
    })
}

pub(crate) fn run_supervisor(arguments: &[String]) -> Result<i32, String> {
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
