use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use centaeris_core::execution::sandbox::{
    NetworkSandboxPolicy, SandboxErr, SandboxPolicy, SandboxType,
};

use super::PreparedSandboxCommand;

static PROBE_NONCE: AtomicU64 = AtomicU64::new(0);

pub(super) fn sandbox_type() -> SandboxType {
    SandboxType::MacOsSeatbelt
}

pub(super) fn ensure_available() -> Result<PathBuf, SandboxErr> {
    let path = PathBuf::from("/usr/bin/sandbox-exec");
    if !path.is_file() {
        return Err(SandboxErr::Unavailable {
            reason: "/usr/bin/sandbox-exec is required for local execution on macOS".to_string(),
            sandbox_type: Some(sandbox_type()),
        });
    }
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| SandboxErr::Io(format!("system clock is invalid: {error}")))?
        .as_nanos();
    let probe_root = std::env::temp_dir().join(format!(
        "centaeris-seatbelt-probe-{}-{timestamp}-{}",
        std::process::id(),
        PROBE_NONCE.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&probe_root).map_err(|error| SandboxErr::Unavailable {
        reason: format!("create macOS Seatbelt capability probe failed: {error}"),
        sandbox_type: Some(sandbox_type()),
    })?;
    let probe = (|| {
        let profile = build_profile(&SandboxPolicy::read_only_no_network(&probe_root))?;
        Command::new(path.as_path())
            .arg("-p")
            .arg(profile)
            .arg("/usr/bin/true")
            .status()
            .map_err(|error| SandboxErr::Unavailable {
                reason: format!("start macOS Seatbelt capability probe failed: {error}"),
                sandbox_type: Some(sandbox_type()),
            })
    })();
    let cleanup = std::fs::remove_dir(&probe_root).map_err(|error| {
        SandboxErr::Io(format!(
            "remove macOS Seatbelt capability probe failed: {error}"
        ))
    });
    let status = probe?;
    cleanup?;
    if !status.success() {
        return Err(SandboxErr::Unavailable {
            reason: "macOS Seatbelt capability probe failed".to_string(),
            sandbox_type: Some(sandbox_type()),
        });
    }
    Ok(path)
}

pub(super) fn prepare_command(
    _runtime_executable: &Path,
    program: &Path,
    args: &[String],
    cwd: &Path,
    env_overrides: &std::collections::HashMap<String, String>,
    policy: &SandboxPolicy,
    _preserve_background: bool,
) -> Result<PreparedSandboxCommand, SandboxErr> {
    if matches!(policy.network, NetworkSandboxPolicy::Allowlist { .. }) {
        return Err(SandboxErr::Unavailable {
            reason: "local macOS sandbox does not support network allowlists".to_string(),
            sandbox_type: Some(sandbox_type()),
        });
    }
    super::materialize_denied_paths(policy, sandbox_type())?;
    let mut command = Command::new(ensure_available()?);
    command
        .arg("-p")
        .arg(build_profile(policy)?)
        .arg(program)
        .args(args)
        .current_dir(cwd)
        .envs(env_overrides);
    Ok(PreparedSandboxCommand {
        command,
        completion: None,
        stdin_input: None,
    })
}

fn build_profile(policy: &SandboxPolicy) -> Result<String, SandboxErr> {
    let mut lines = vec![
        "(version 1)".to_string(),
        "(deny default)".to_string(),
        "(allow process-exec)".to_string(),
        "(allow process-fork)".to_string(),
        "(allow signal (target same-sandbox))".to_string(),
        "(allow process-info* (target same-sandbox))".to_string(),
        "(allow sysctl-read)".to_string(),
        "(allow mach-lookup (global-name \"com.apple.system.opendirectoryd.libinfo\") (global-name \"com.apple.PowerManagement.control\") (global-name \"com.apple.cfprefsd.daemon\") (global-name \"com.apple.cfprefsd.agent\") (local-name \"com.apple.cfprefsd.agent\"))".to_string(),
        "(allow user-preference-read)".to_string(),
        "(allow file-write* (literal \"/dev/null\"))".to_string(),
    ];
    lines.push(allow_all_except(
        "file-read*",
        &policy.filesystem.denied_read_paths,
    )?);
    for root in &policy.filesystem.writable_roots {
        lines.push(allow_subpath_except(
            "file-write*",
            root,
            &policy.filesystem.denied_write_paths,
        )?);
    }
    if matches!(policy.network, NetworkSandboxPolicy::PublicInternet) {
        lines.extend([
            "(allow system-socket (require-all (socket-domain AF_SYSTEM) (socket-protocol 2)))".to_string(),
            "(allow mach-lookup (global-name \"com.apple.bsd.dirhelper\") (global-name \"com.apple.system.opendirectoryd.membership\") (global-name \"com.apple.SecurityServer\") (global-name \"com.apple.networkd\") (global-name \"com.apple.ocspd\") (global-name \"com.apple.trustd.agent\") (global-name \"com.apple.SystemConfiguration.DNSConfiguration\") (global-name \"com.apple.SystemConfiguration.configd\"))".to_string(),
            "(allow network-outbound (remote ip \"*:*\"))".to_string(),
            "(allow network-bind (local ip \"*:*\"))".to_string(),
            "(allow network-inbound (local ip \"localhost:*\"))".to_string(),
        ]);
    }
    Ok(lines.join("\n"))
}

fn allow_all_except(operation: &str, denied_paths: &[PathBuf]) -> Result<String, SandboxErr> {
    if denied_paths.is_empty() {
        return Ok(format!("(allow {operation})"));
    }
    Ok(format!(
        "(allow {operation} (require-all {}))",
        denied_paths
            .iter()
            .map(|path| Ok(format!("(require-not {})", profile_filter(path)?)))
            .collect::<Result<Vec<_>, SandboxErr>>()?
            .join(" ")
    ))
}

fn allow_subpath_except(
    operation: &str,
    root: &Path,
    denied_paths: &[PathBuf],
) -> Result<String, SandboxErr> {
    let mut filters = vec![format!("(subpath \"{}\")", profile_path(root)?)];
    filters.extend(
        denied_paths
            .iter()
            .map(|path| Ok(format!("(require-not {})", profile_filter(path)?)))
            .collect::<Result<Vec<_>, SandboxErr>>()?,
    );
    Ok(format!(
        "(allow {operation} (require-all {}))",
        filters.join(" ")
    ))
}

fn profile_filter(path: &Path) -> Result<String, SandboxErr> {
    Ok(format!(
        "({} \"{}\")",
        if path.is_dir() { "subpath" } else { "literal" },
        profile_path(path)?
    ))
}

fn profile_path(path: &Path) -> Result<String, SandboxErr> {
    let source = path.to_str().ok_or_else(|| SandboxErr::Denied {
        reason: format!(
            "macOS sandbox profile path is not valid UTF-8: {}",
            path.display()
        ),
        sandbox_type: sandbox_type(),
    })?;
    if source.chars().any(char::is_control) {
        return Err(SandboxErr::Denied {
            reason: format!(
                "macOS sandbox profile path contains a control character: {}",
                path.display()
            ),
            sandbox_type: sandbox_type(),
        });
    }
    let canonical = path.canonicalize().map_err(|error| SandboxErr::Denied {
        reason: format!(
            "canonicalize macOS sandbox profile path failed for {}: {error}",
            path.display()
        ),
        sandbox_type: sandbox_type(),
    })?;
    let value = canonical.to_str().ok_or_else(|| SandboxErr::Denied {
        reason: format!(
            "macOS sandbox profile path is not valid UTF-8: {}",
            canonical.display()
        ),
        sandbox_type: sandbox_type(),
    })?;
    if value.chars().any(char::is_control) {
        return Err(SandboxErr::Denied {
            reason: format!(
                "macOS sandbox profile path contains a control character: {}",
                canonical.display()
            ),
            sandbox_type: sandbox_type(),
        });
    }
    Ok(value.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seatbelt_profile_denies_network_and_protected_paths() {
        let workspace = std::env::temp_dir().join(format!(
            "centaeris-seatbelt-profile-test-{}-{}",
            std::process::id(),
            PROBE_NONCE.fetch_add(1, Ordering::Relaxed)
        ));
        let protected = workspace.join(".centaeris");
        std::fs::create_dir_all(&protected).unwrap();
        let policy = SandboxPolicy::workspace_write_no_network(&workspace);
        let profile = build_profile(&policy).unwrap();
        let workspace_profile = profile_path(&workspace).unwrap();
        let protected_profile = profile_path(&protected).unwrap();
        assert!(!profile.contains("(allow network-"));
        assert!(!profile.contains("(allow process*)"));
        assert!(!profile.contains("(allow mach-lookup)"));
        assert!(profile.contains(&format!(
            "(allow file-read* (require-all (require-not (subpath \"{protected_profile}\"))))"
        )));
        assert!(profile.contains(&format!(
            "(allow file-write* (require-all (subpath \"{workspace_profile}\") (require-not (subpath \"{protected_profile}\"))))"
        )));
        std::fs::remove_dir_all(&workspace).unwrap();
    }

    #[test]
    fn seatbelt_profile_rejects_path_injection() {
        let mut policy = SandboxPolicy::workspace_write_no_network("/tmp/workspace");
        policy
            .filesystem
            .writable_roots
            .push(PathBuf::from("/tmp/banana\n(allow default)"));
        assert!(matches!(
            build_profile(&policy),
            Err(SandboxErr::Denied { .. })
        ));
    }

    #[test]
    fn seatbelt_temporary_root_is_writable_only_when_explicitly_granted() {
        let workspace = PathBuf::from("/tmp/workspace");
        let temporary_root = PathBuf::from("/tmp/agent-tool-results/session");
        let mut policy = SandboxPolicy::workspace_write_no_network(&workspace);
        policy.filesystem.tmp_root = Some(temporary_root.clone());
        policy
            .filesystem
            .read_only_roots
            .push(temporary_root.clone());

        let read_only_profile = build_profile(&policy).unwrap();
        let temporary_profile = profile_path(&temporary_root).unwrap();
        assert!(!read_only_profile.contains(&format!(
            "(allow file-write* (require-all (subpath \"{temporary_profile}\")"
        )));

        policy.filesystem.writable_roots.push(temporary_root);
        let writable_profile = build_profile(&policy).unwrap();
        assert!(writable_profile.contains(&format!(
            "(allow file-write* (require-all (subpath \"{temporary_profile}\")"
        )));
    }
}
