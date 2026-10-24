//! A fixed OCI workload inside a provider-owned VM. This is not the legacy
//! host runsc route. Validation never substitutes for live namespace/cgroup proof.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

pub const GUEST_WORKLOAD_SCHEMA: &str = "environment.guest.workload.v1";
const WORKLOAD_CGROUP: &str = "/centaeris-guest/workload";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GuestImageUser {
    pub uid: u32,
    pub gid: u32,
    pub additional_gids: Vec<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GuestImageProcess {
    pub user: GuestImageUser,
    pub cwd: String,
    pub env: Vec<String>,
}

/// Installed by the trusted template builder, never supplied by a task request.
/// imageDigest records build provenance; it does not hash a mutable rootfs at runtime.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GuestWorkloadManifest {
    pub schema: String,
    pub image_digest: String,
    pub spec_digest: String,
    pub image_process: GuestImageProcess,
    /// A path inside the workload rootfs, independent of the original WORKDIR.
    pub workspace_root: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct IdMapping {
    #[serde(rename = "containerID")]
    container_id: u32,
    #[serde(rename = "hostID")]
    host_id: u32,
    size: u32,
}

impl GuestWorkloadManifest {
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let manifest: Self = serde_json::from_slice(bytes).map_err(|_| "guest_manifest_invalid")?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema != GUEST_WORKLOAD_SCHEMA {
            return Err("guest_manifest_schema_unsupported".into());
        }
        let path = |value: &str| {
            crate::host_supervisor::absolute_path(value)
                && !value.chars().any(char::is_control)
                && value != "/run/centaeris"
                && !value.starts_with("/run/centaeris/")
        };
        let digest = |value: &str| {
            value.strip_prefix("sha256:").is_some_and(|hex| {
                hex.len() == 64
                    && hex
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
        };
        if !digest(&self.image_digest)
            || !digest(&self.spec_digest)
            || !path(&self.image_process.cwd)
            || !path(&self.workspace_root)
            || self.image_process.user.uid == u32::MAX
            || self.image_process.user.gid == u32::MAX
            || self.image_process.user.additional_gids.contains(&u32::MAX)
        {
            return Err("guest_manifest_invalid".into());
        }
        let mut keys = BTreeSet::new();
        for entry in &self.image_process.env {
            let (key, value) = entry
                .split_once('=')
                .ok_or("guest_image_environment_invalid")?;
            if key.is_empty()
                || key.contains('\0')
                || value.contains('\0')
                || key.starts_with("CENTAERIS_")
                || !keys.insert(key)
            {
                return Err("guest_image_environment_invalid".into());
            }
        }
        Ok(())
    }

    /// Validate a protected, digest-frozen OCI bundle before opening its manager.
    /// Namespace creation, egress/NAT and rootfs mapping are template obligations.
    pub fn validate_bundle(&self, spec: &Value) -> Result<(), String> {
        self.validate()?;
        if spec_digest(spec)? != self.spec_digest {
            return Err("guest_bundle_changed".into());
        }
        let process = &spec["process"];
        let user: GuestImageUser = serde_json::from_value(process["user"].clone())
            .map_err(|_| "guest_image_process_changed")?;
        let mut expected_env = self.image_process.env.clone();
        expected_env.push(format!("CENTAERIS_WORKSPACE_ROOT={}", self.workspace_root));
        expected_env.push(format!(
            "CENTAERIS_WORKSPACE_USER={}:{}",
            user.uid, user.gid
        ));
        if user != self.image_process.user
            || process["cwd"] != self.image_process.cwd
            || process["env"] != serde_json::json!(expected_env)
        {
            return Err("guest_image_process_changed".into());
        }
        crate::host_supervisor::task_process(
            process,
            "/bin/true",
            &[],
            &self.image_process.cwd,
            &Default::default(),
        )?;
        let required = BTreeSet::from(["pid", "mount", "network", "ipc", "uts", "user", "cgroup"]);
        let namespaces = spec["linux"]["namespaces"]
            .as_array()
            .ok_or("guest_namespaces_required")?;
        let mut found = BTreeSet::new();
        for ns in namespaces {
            let kind = ns["type"].as_str().ok_or("guest_namespaces_required")?;
            if !required.contains(kind)
                || ns.get("path").is_some()
                || !found.insert(kind)
                || ns.as_object().is_none_or(|object| object.len() != 1)
            {
                return Err("guest_private_namespaces_required".into());
            }
        }
        if found != required {
            return Err("guest_private_namespaces_required".into());
        }
        let uids = mappings(spec, "uidMappings")?;
        let gids = mappings(spec, "gidMappings")?;
        if mapped(&uids, user.uid).is_none()
            || mapped(&gids, user.gid).is_none()
            || user
                .additional_gids
                .iter()
                .any(|gid| mapped(&gids, *gid).is_none())
        {
            return Err("guest_image_user_unmapped".into());
        }
        if spec["root"]["path"] != "rootfs"
            || spec["root"]["readonly"] != false
            || spec["linux"]["cgroupsPath"] != WORKLOAD_CGROUP
            || spec.get("hooks").is_some()
            || spec["linux"]
                .get("rootfsPropagation")
                .is_some_and(|v| v != "private" && v != "rprivate")
        {
            return Err("guest_oci_profile_unsupported".into());
        }
        validate_security(spec)?;
        validate_mounts(spec)?;
        Ok(())
    }
}

fn spec_digest(spec: &Value) -> Result<String, String> {
    Ok(format!(
        "sha256:{:x}",
        Sha256::digest(serde_json::to_vec(spec).map_err(|_| "guest_spec_invalid")?)
    ))
}

fn mappings(spec: &Value, field: &str) -> Result<Vec<IdMapping>, String> {
    let maps: Vec<IdMapping> = serde_json::from_value(spec["linux"][field].clone())
        .map_err(|_| "guest_user_mapping_invalid")?;
    if maps.is_empty() || maps.len() > 16 {
        return Err("guest_user_mapping_invalid".into());
    }
    for (index, map) in maps.iter().enumerate() {
        let inner_end = map
            .container_id
            .checked_add(map.size)
            .ok_or("guest_user_mapping_invalid")?;
        let outer_end = map
            .host_id
            .checked_add(map.size)
            .ok_or("guest_user_mapping_invalid")?;
        if map.size == 0
            || map.host_id < 65536
            || maps[..index].iter().any(|other| {
                (map.container_id < other.container_id.saturating_add(other.size)
                    && other.container_id < inner_end)
                    || (map.host_id < other.host_id.saturating_add(other.size)
                        && other.host_id < outer_end)
            })
        {
            return Err("guest_user_mapping_invalid".into());
        }
    }
    if mapped(&maps, 0).is_none() {
        return Err("guest_root_mapping_required".into());
    }
    Ok(maps)
}

fn mapped(maps: &[IdMapping], id: u32) -> Option<u32> {
    maps.iter().find_map(|map| {
        id.checked_sub(map.container_id)
            .filter(|offset| *offset < map.size)
            .and_then(|offset| map.host_id.checked_add(offset))
    })
}

fn validate_security(spec: &Value) -> Result<(), String> {
    let ordinary = [
        "CAP_CHOWN",
        "CAP_DAC_OVERRIDE",
        "CAP_FOWNER",
        "CAP_FSETID",
        "CAP_KILL",
        "CAP_SETGID",
        "CAP_SETUID",
        "CAP_SETPCAP",
        "CAP_NET_BIND_SERVICE",
        "CAP_NET_RAW",
        "CAP_SYS_CHROOT",
        "CAP_MKNOD",
        "CAP_AUDIT_WRITE",
        "CAP_SETFCAP",
    ];
    for field in [
        "bounding",
        "effective",
        "permitted",
        "inheritable",
        "ambient",
    ] {
        let caps = spec["process"]["capabilities"][field]
            .as_array()
            .ok_or("guest_capabilities_invalid")?;
        if caps
            .iter()
            .any(|cap| cap.as_str().is_none_or(|cap| !ordinary.contains(&cap)))
        {
            return Err("guest_capabilities_rejected".into());
        }
    }
    let seccomp = &spec["linux"]["seccomp"];
    if !matches!(
        seccomp["defaultAction"].as_str(),
        Some("SCMP_ACT_ERRNO" | "SCMP_ACT_KILL" | "SCMP_ACT_KILL_PROCESS" | "SCMP_ACT_KILL_THREAD")
    ) {
        return Err("guest_seccomp_required".into());
    }
    let devices = spec["linux"]["resources"]["devices"]
        .as_array()
        .ok_or("guest_devices_rejected")?;
    if devices.first().is_none_or(|rule| {
        rule["allow"] != false
            || rule.get("type").is_some_and(|kind| kind != "a")
            || rule["access"] != "rwm"
            || rule.get("major").is_some()
            || rule.get("minor").is_some()
    }) {
        return Err("guest_devices_rejected".into());
    }
    let allowed = |value: &Value| {
        value["type"] == "c"
            && matches!(
                (value["major"].as_u64(), value["minor"].as_u64()),
                (Some(1), Some(3 | 5 | 7 | 8 | 9)) | (Some(5), Some(0 | 2)) | (Some(136), _)
            )
    };
    if devices
        .iter()
        .skip(1)
        .any(|rule| rule["allow"] != true || !allowed(rule))
        || spec["linux"].get("devices").is_some_and(|entries| {
            entries
                .as_array()
                .is_none_or(|entries| entries.iter().any(|device| !allowed(device)))
        })
    {
        return Err("guest_devices_rejected".into());
    }
    Ok(())
}

fn validate_mounts(spec: &Value) -> Result<(), String> {
    let mounts = spec["mounts"].as_array().ok_or("guest_mounts_rejected")?;
    let required = [
        "/proc",
        "/dev",
        "/dev/pts",
        "/dev/shm",
        "/sys",
        "/sys/fs/cgroup",
    ];
    let mut found = BTreeSet::new();
    for mount in mounts {
        let dest = mount["destination"]
            .as_str()
            .ok_or("guest_mounts_rejected")?;
        let kind = mount["type"].as_str().ok_or("guest_mounts_rejected")?;
        let options = mount["options"].as_array().ok_or("guest_mounts_rejected")?;
        let has = |value: &str| options.iter().any(|option| option == value);
        let expected = match dest {
            "/proc" => "proc",
            "/dev" | "/dev/shm" => "tmpfs",
            "/dev/pts" => "devpts",
            "/dev/mqueue" => "mqueue",
            "/sys" => "sysfs",
            "/sys/fs/cgroup" => "cgroup",
            _ => return Err("guest_mounts_rejected".into()),
        };
        if kind != expected
            || !found.insert(dest)
            || !has("nosuid")
            || has("bind")
            || has("rbind")
            || has("move")
            || has("remount")
            || (["/sys", "/sys/fs/cgroup"].contains(&dest) && (!has("ro") || has("rw")))
        {
            return Err("guest_mounts_rejected".into());
        }
    }
    if required.iter().any(|path| !found.contains(path)) {
        return Err("guest_mounts_rejected".into());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::{run_broker, run_client, GuestTaskService};
#[cfg(test)]
mod tests;
