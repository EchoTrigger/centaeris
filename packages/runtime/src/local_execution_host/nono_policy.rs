use centaeris_core::execution::{ExecutionError, ExecutionPolicy, NetworkPolicy};
use nono::{AccessMode, CapabilitySet};
use std::path::{Path, PathBuf};

pub(super) fn unavailable(error: impl std::fmt::Display) -> ExecutionError {
    ExecutionError::PolicyUnavailable {
        reason: error.to_string(),
    }
}

pub(super) fn capabilities(
    policy: &ExecutionPolicy,
    program: &Path,
    runtime: &Path,
    scratch: Option<&Path>,
    control: Option<&Path>,
) -> Result<CapabilitySet, ExecutionError> {
    let runtime = canonical(runtime)?;
    for root in &policy.filesystem.writable_roots {
        if runtime.starts_with(canonical(root)?) {
            return Err(unavailable(
                "the trusted Runtime executable must be outside writable execution roots",
            ));
        }
    }
    if matches!(policy.network, NetworkPolicy::Allowlist { .. }) {
        return Err(unavailable(
            "local nono execution does not support domain allowlists",
        ));
    }
    let mut caps = CapabilitySet::new();
    if matches!(policy.network, NetworkPolicy::Disabled) {
        caps = caps.block_network();
    }
    #[cfg(target_os = "macos")]
    {
        // Seatbelt treats Unix socket connect/bind as network operations,
        // independent of file grants. Keep these Host restrictions inside
        // nono's capability application instead of installing another sandbox.
        caps.add_platform_rule("(deny network-outbound (remote unix-socket))")
            .map_err(unavailable)?;
        caps.add_platform_rule("(deny network-bind (local unix-socket))")
            .map_err(unavailable)?;
        if matches!(policy.network, NetworkPolicy::PublicInternet) {
            // macOS DNS uses this OS service; these exact-path exceptions do
            // not grant access to arbitrary Host or workspace Unix sockets.
            for rule in [
                "(allow network-outbound (path \"/private/var/run/mDNSResponder\"))",
                "(allow network-outbound (path \"/var/run/mDNSResponder\"))",
            ] {
                caps.add_platform_rule(rule).map_err(unavailable)?;
            }
        }
    }
    // Host prerequisites, not model-visible grants. Do not expose /, HOME,
    // /proc, /run or the entire temporary directory to executed commands.
    #[cfg(target_os = "linux")]
    let prerequisites = &["/usr", "/bin", "/lib", "/lib64", "/etc/ssl/certs"][..];
    #[cfg(target_os = "macos")]
    let prerequisites = &[
        "/System",
        "/usr",
        "/bin",
        "/sbin",
        "/Library",
        "/private/etc",
        "/opt/homebrew",
        "/Applications/Xcode.app",
    ][..];
    for path in prerequisites {
        if Path::new(path).exists() {
            caps = caps
                .allow_path(path, AccessMode::Read)
                .map_err(unavailable)?;
        }
    }
    for path in [
        "/etc/ld.so.cache",
        "/etc/passwd",
        "/etc/group",
        "/etc/nsswitch.conf",
        "/etc/resolv.conf",
        "/etc/hosts",
        "/etc/localtime",
        "/dev/urandom",
        "/dev/random",
    ] {
        if Path::new(path).exists() {
            caps = caps
                .allow_file(path, AccessMode::Read)
                .map_err(unavailable)?;
        }
    }
    caps = caps
        .allow_file("/dev/null", AccessMode::ReadWrite)
        .map_err(unavailable)?;
    for path in [program, runtime.as_path()] {
        caps = caps
            .allow_file(path, AccessMode::Read)
            .map_err(unavailable)?;
    }
    for path in &policy.filesystem.read_only_roots {
        caps = caps
            .allow_path(path, AccessMode::Read)
            .map_err(unavailable)?;
    }
    for path in &policy.filesystem.writable_roots {
        caps = caps
            .allow_path(path, AccessMode::ReadWrite)
            .map_err(unavailable)?;
    }
    if let Some(scratch) = scratch {
        caps = caps
            .allow_path(scratch, AccessMode::ReadWrite)
            .map_err(unavailable)?;
    }
    if let Some(control) = control {
        caps = caps
            .allow_path(control, AccessMode::ReadWrite)
            .map_err(unavailable)?;
    }
    validate_layout(policy, &caps)?;
    Ok(caps)
}

fn canonical(path: &Path) -> Result<PathBuf, ExecutionError> {
    path.canonicalize().map_err(|error| {
        unavailable(format!(
            "resolve execution resource {}: {error}",
            path.display()
        ))
    })
}

fn overlap(left: &Path, right: &Path) -> bool {
    left.starts_with(right) || right.starts_with(left)
}

fn validate_layout(policy: &ExecutionPolicy, caps: &CapabilitySet) -> Result<(), ExecutionError> {
    let reads = policy
        .filesystem
        .denied_read_paths
        .iter()
        .map(|p| canonical(p))
        .collect::<Result<Vec<_>, _>>()?;
    let writes = policy
        .filesystem
        .denied_write_paths
        .iter()
        .map(|p| canonical(p))
        .collect::<Result<Vec<_>, _>>()?;
    let declared_writes = policy
        .filesystem
        .writable_roots
        .iter()
        .map(|p| canonical(p))
        .collect::<Result<Vec<_>, _>>()?;
    let readonly = policy
        .filesystem
        .read_only_roots
        .iter()
        .map(|p| canonical(p))
        .collect::<Result<Vec<_>, _>>()?;
    for grant in caps.fs_capabilities() {
        if reads.iter().any(|denied| overlap(denied, &grant.resolved)) {
            return Err(unavailable("nono cannot subtract a denied read path from an allowed root; move protected resources outside granted roots"));
        }
        if grant.access.contains(AccessMode::Write)
            && (writes.iter().any(|denied| overlap(denied, &grant.resolved))
                || readonly.iter().any(|root| {
                    !declared_writes.contains(root) && root.starts_with(&grant.resolved)
                }))
        {
            return Err(unavailable("nono writable root overlaps protected resources; use separate writable and read-only directories"));
        }
    }
    Ok(())
}
