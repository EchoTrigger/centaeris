//! Required Docker storage semantics for managed execution instances.

pub(crate) fn require_overlayfs(driver: Option<&str>) -> Result<(), &'static str> {
    match driver {
        Some("overlay2" | "overlayfs") => Ok(()),
        _ => Err("docker_overlayfs_required"),
    }
}

/// A guest-private root overlay is not the Docker writable layer. Require an
/// explicit shared backing policy before new managed runsc instances are created.
pub(crate) fn require_workspace_backing(
    runtime_name: &str,
    runtime: &serde_json::Value,
) -> Result<(), &'static str> {
    if runtime_name == "runc" {
        return Ok(());
    }
    let rejected = "runsc_docker_workspace_backing_required";
    if runtime_name != "runsc" {
        return Err(rejected);
    }
    let primary = runtime.get("runtimeArgs").filter(|value| !value.is_null());
    let alternate = runtime.get("Args").filter(|value| !value.is_null());
    let arguments = match (primary, alternate) {
        (Some(value), None) | (None, Some(value)) => value.as_array().ok_or(rejected)?,
        _ => return Err(rejected),
    };
    let mut explicit = 0;
    for argument in arguments {
        let argument = argument.as_str().ok_or(rejected)?;
        if argument.trim_start_matches('-').starts_with("overlay") {
            if argument != "--overlay2=none" {
                return Err(rejected);
            }
            explicit += 1;
        }
    }
    if explicit != 1 {
        return Err(rejected);
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classic_and_containerd_overlayfs_backends_are_supported() {
        for driver in ["overlay2", "overlayfs"] {
            assert_eq!(require_overlayfs(Some(driver)), Ok(()), "{driver}");
        }
    }

    #[test]
    fn non_overlayfs_and_missing_backend_never_pass_host_preflight() {
        for driver in [
            None,
            Some(""),
            Some("vfs"),
            Some("btrfs"),
            Some("zfs"),
            Some("OVERLAYFS"),
        ] {
            assert_eq!(
                require_overlayfs(driver),
                Err("docker_overlayfs_required"),
                "{driver:?}"
            );
        }
    }
}

#[cfg(test)]
mod workspace_backing_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn explicit_single_runsc_overlay_none_is_required_for_host_visible_workspace_writes() {
        for field in ["runtimeArgs", "Args"] {
            let mut runtime = json!({});
            runtime[field] = json!([
                "--platform=ptrace",
                "--overlay2=none",
                "--in-sandbox-cgroup=v2"
            ]);
            assert_eq!(require_workspace_backing("runsc", &runtime), Ok(()));
        }
        for args in [
            json!([]),
            json!(["--platform=ptrace"]),
            json!(["--overlay2=root:self"]),
            json!(["--overlay2=none", "--overlay2=root:self"]),
            json!(["--overlay2=none", "--overlay2=none"]),
            json!(["--overlay2=none", "--overlay=true"]),
            json!(["--overlay2", "none"]),
            json!(["--overlay2=none", null]),
            json!(["--overlay2=none", "-overlay2=root:memory"]),
        ] {
            assert_eq!(
                require_workspace_backing("runsc", &json!({"runtimeArgs":args})),
                Err("runsc_docker_workspace_backing_required"),
                "{args}"
            );
        }
        for runtime in [
            json!(null),
            json!({}),
            json!({"runtimeArgs":null}),
            json!({"runtimeArgs":"--overlay2=none"}),
            json!({"runtimeArgs":["--overlay2=none"],"Args":["--overlay2=root:self"]}),
        ] {
            assert_eq!(
                require_workspace_backing("runsc", &runtime),
                Err("runsc_docker_workspace_backing_required"),
                "{runtime}"
            );
        }
    }
    #[test]
    fn direct_runc_does_not_require_gvisor_filesystem_flags() {
        assert_eq!(require_workspace_backing("runc", &json!({})), Ok(()));
    }
    #[test]
    #[ignore = "requires explicitly selected owned Docker daemon; read-only runtime policy observation"]
    fn docker_runsc_workspace_backing_policy_is_explicit_in_the_real_daemon() {
        let info = crate::docker_engine::info().unwrap();
        require_workspace_backing("runsc", &info["Runtimes"]["runsc"]).unwrap();
    }
}
