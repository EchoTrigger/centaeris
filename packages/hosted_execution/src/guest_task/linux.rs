//! Fixed-path guest factory. The template must create the OCI target, userns,
//! cgroup and egress network first; this module never falls back to host exec.
use super::*;
use crate::host_supervisor::linux::{
    command_json_unlocked, protected, read_bundle_spec, start_time, COMMAND_LOCK,
};
use crate::task_service::linux::{launcher::OciTarget, HelperBinary, NativeManager};
use centaeris_core::execution::{
    EnvironmentTaskEnvelope, EnvironmentTaskRequest, EnvironmentTaskResponse,
};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::Path,
    sync::Arc,
};

const MANIFEST: &str = "/etc/centaeris-guest/workload.json";
const STATE_ROOT: &str = "/var/lib/centaeris-guest";
const BUNDLE: &str = "/var/lib/centaeris-guest/workload";
const RUNTIME: &str = "/usr/local/libexec/centaeris/runc";
const HELPER: &str = "/usr/local/libexec/centaeris/execution_agent";
const RUNTIME_ROOT: &str = "/run/centaeris-guest/runc";
const CONTAINER: &str = "workload";
const MAX_CONFIG_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NodeIdentity {
    device: u64,
    inode: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Registration {
    environment_instance_id: String,
    manifest_digest: String,
    init_pid: u32,
    init_start_time: u64,
    root: NodeIdentity,
    namespaces: BTreeMap<String, NodeIdentity>,
}

/// A resident broker must own this service for the entire task lifetime.
/// A one-shot SDK command is only a broker client, never a task supervisor.
pub struct GuestTaskService {
    registration: Registration,
    manager: NativeManager,
}

impl GuestTaskService {
    /// The only caller input is a validated Core Takeover; all paths and the
    /// original image identity come from the root-private template installation.
    pub fn open(initial: &EnvironmentTaskEnvelope) -> Result<Self, String> {
        initial.validate()?;
        initial.request.validate_authority()?;
        let EnvironmentTaskRequest::Takeover {
            environment_instance_id,
            ..
        } = &initial.request
        else {
            return Err("guest_initial_takeover_required".into());
        };
        // SAFETY: geteuid has no arguments or side effects.
        if unsafe { libc::geteuid() } != 0 {
            return Err("guest_broker_requires_root".into());
        }
        let (registration, target) = attest(environment_instance_id)?;
        private_directory(Path::new(STATE_ROOT))?;
        for name in ["control", "output", "launch", "helper-output"] {
            private_directory(&Path::new(STATE_ROOT).join(name))?;
        }
        freeze_registration(Path::new(STATE_ROOT), &registration)?;
        let helper = Arc::new(HelperBinary::load(HELPER)?);
        let manager = NativeManager::open(
            Path::new(STATE_ROOT).join("control"),
            Path::new(STATE_ROOT).join("output"),
            Path::new("/sys/fs/cgroup")
                .join(WORKLOAD_CGROUP.trim_start_matches('/'))
                .join("tasks"),
            environment_instance_id,
            target,
            Path::new(STATE_ROOT).join("helper-output"),
            Some(helper),
        )?;
        Ok(Self {
            registration,
            manager,
        })
    }

    pub fn handle(&self, envelope: EnvironmentTaskEnvelope) -> EnvironmentTaskResponse {
        EnvironmentTaskResponse::from_result((|| {
            envelope.validate()?;
            envelope.request.validate_authority()?;
            if crate::host_supervisor::request_instance(&envelope.request)
                != self.registration.environment_instance_id
            {
                return Err("guest_registered_instance_changed".into());
            }
            let (current, _) = attest(&self.registration.environment_instance_id)?;
            if current != self.registration {
                return Err("guest_registered_target_changed".into());
            }
            self.manager.handle(envelope.request)
        })())
    }
}

fn node(path: &Path) -> Result<NodeIdentity, String> {
    let metadata = fs::metadata(path).map_err(|_| "guest_target_identity_unknown")?;
    Ok(NodeIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

fn rootfs_node(path: &Path, spec: &Value) -> Result<NodeIdentity, String> {
    let metadata = fs::symlink_metadata(path).map_err(|_| "guest_workload_root_untrusted")?;
    let uid = mapped(&mappings(spec, "uidMappings")?, 0).ok_or("guest_root_mapping_required")?;
    let gid = mapped(&mappings(spec, "gidMappings")?, 0).ok_or("guest_root_mapping_required")?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != uid
        || metadata.gid() != gid
    {
        return Err("guest_workload_root_untrusted".into());
    }
    Ok(NodeIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

fn private_existing(path: &Path, directory: bool) -> Result<(), String> {
    protected(path, directory)?;
    if fs::symlink_metadata(path)
        .map_err(|_| "guest_private_path_unavailable")?
        .mode()
        & 0o077
        != 0
    {
        return Err("guest_private_path_required".into());
    }
    Ok(())
}

fn private_directory(path: &Path) -> Result<(), String> {
    if fs::symlink_metadata(path).is_err() {
        protected(path.parent().ok_or("guest_private_path_invalid")?, true)?;
        match fs::DirBuilder::new().mode(0o700).create(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err("guest_private_path_unavailable".into()),
        }
    }
    private_existing(path, true)
}

fn read_private(path: &Path) -> Result<Vec<u8>, String> {
    private_existing(path, false)?;
    private_existing(path.parent().ok_or("guest_private_path_invalid")?, true)?;
    let mut bytes = Vec::new();
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|_| "guest_private_path_unavailable")?
        .take(MAX_CONFIG_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "guest_configuration_unavailable")?;
    if bytes.len() as u64 > MAX_CONFIG_BYTES {
        return Err("guest_configuration_oversized".into());
    }
    Ok(bytes)
}

fn attest(instance: &str) -> Result<(Registration, OciTarget), String> {
    let manifest_bytes = read_private(Path::new(MANIFEST))?;
    let manifest = GuestWorkloadManifest::parse(&manifest_bytes)?;
    private_existing(Path::new(STATE_ROOT), true)?;
    private_existing(Path::new(RUNTIME_ROOT), true)?;
    protected(Path::new(RUNTIME), false)?;
    let spec = read_bundle_spec(Path::new(BUNDLE))?;
    manifest.validate_bundle(&spec)?;
    let _commands = COMMAND_LOCK
        .lock()
        .map_err(|_| "guest_runtime_unavailable")?;
    let state = command_json_unlocked(RUNTIME, &["--root", RUNTIME_ROOT, "state", CONTAINER])?;
    let pid = state["pid"]
        .as_u64()
        .and_then(|pid| u32::try_from(pid).ok())
        .filter(|pid| *pid > 1)
        .ok_or("guest_target_unavailable")?;
    if state["id"] != CONTAINER || state["status"] != "running" || state["bundle"] != BUNDLE {
        return Err("guest_target_identity_rejected".into());
    }
    let began = start_time(pid)?;
    let namespaces = verify_namespaces(pid)?;
    verify_mappings(pid, &spec)?;
    let status = fs::read_to_string(format!("/proc/{pid}/status"))
        .map_err(|_| "guest_image_user_unknown")?;
    verify_process_user(&status, &spec, &manifest.image_process.user)?;
    let root = rootfs_node(&Path::new(BUNDLE).join("rootfs"), &spec)?;
    if node(Path::new(&format!("/proc/{pid}/root")))? != root {
        return Err("guest_workload_root_changed".into());
    }
    let membership =
        fs::read_to_string(format!("/proc/{pid}/cgroup")).map_err(|_| "guest_cgroup_unknown")?;
    if membership.lines().find_map(|line| line.strip_prefix("0::")) != Some(WORKLOAD_CGROUP) {
        return Err("guest_cgroup_rejected".into());
    }
    if start_time(pid)? != began {
        return Err("guest_target_identity_rejected".into());
    }
    let registration = Registration {
        environment_instance_id: instance.into(),
        manifest_digest: format!("sha256:{:x}", Sha256::digest(&manifest_bytes)),
        init_pid: pid,
        init_start_time: began,
        root,
        namespaces,
    };
    let target = OciTarget {
        runtime: crate::host_supervisor::HostOciRuntime::Runc,
        helper_binary: None,
        runtime_path: RUNTIME.into(),
        runtime_root: RUNTIME_ROOT.into(),
        container_id: CONTAINER.into(),
        process: spec["process"].clone(),
        launch_root: Path::new(STATE_ROOT).join("launch"),
        init_pid: pid,
        init_start_time: began,
        bundle: BUNDLE.into(),
        spec_digest: manifest.spec_digest.trim_start_matches("sha256:").into(),
    };
    Ok((registration, target))
}

fn verify_namespaces(pid: u32) -> Result<BTreeMap<String, NodeIdentity>, String> {
    let mut result = BTreeMap::new();
    for name in ["pid", "mnt", "net", "ipc", "uts", "user", "cgroup"] {
        let target = node(Path::new(&format!("/proc/{pid}/ns/{name}")))?;
        if target == node(Path::new(&format!("/proc/self/ns/{name}")))? {
            return Err("guest_private_namespaces_required".into());
        }
        result.insert(name.into(), target);
    }
    Ok(result)
}

fn verify_mappings(pid: u32, spec: &Value) -> Result<(), String> {
    for (file, field) in [("uid_map", "uidMappings"), ("gid_map", "gidMappings")] {
        let value = fs::read_to_string(format!("/proc/{pid}/{file}"))
            .map_err(|_| "guest_user_mapping_unknown")?;
        if parse_kernel_mappings(&value)? != mappings(spec, field)? {
            return Err("guest_user_mapping_changed".into());
        }
    }
    Ok(())
}

fn verify_process_user(status: &str, spec: &Value, user: &GuestImageUser) -> Result<(), String> {
    let values = |name: &str| -> Result<Vec<u32>, String> {
        let mut lines = status.lines().filter_map(|line| line.strip_prefix(name));
        let value = lines.next().ok_or("guest_image_user_unknown")?;
        if lines.next().is_some() {
            return Err("guest_image_user_unknown".into());
        }
        value
            .split_whitespace()
            .map(|value| value.parse().map_err(|_| "guest_image_user_unknown".into()))
            .collect()
    };
    let uids = mappings(spec, "uidMappings")?;
    let gids = mappings(spec, "gidMappings")?;
    for (name, expected) in [
        ("Uid:", mapped(&uids, user.uid)),
        ("Gid:", mapped(&gids, user.gid)),
    ] {
        let expected = expected.ok_or("guest_image_user_unmapped")?;
        if values(name)? != [expected; 4] {
            return Err("guest_image_user_changed".into());
        }
    }
    let mut expected = user
        .additional_gids
        .iter()
        .map(|gid| mapped(&gids, *gid).ok_or("guest_image_user_unmapped"))
        .collect::<Result<Vec<_>, _>>()?;
    let mut observed = values("Groups:")?;
    expected.sort_unstable();
    observed.sort_unstable();
    if expected != observed {
        return Err("guest_image_user_changed".into());
    }
    Ok(())
}

fn parse_kernel_mappings(value: &str) -> Result<Vec<IdMapping>, String> {
    value
        .lines()
        .map(|line| {
            let fields = line
                .split_whitespace()
                .map(str::parse::<u32>)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| "guest_user_mapping_unknown")?;
            if fields.len() != 3 {
                return Err("guest_user_mapping_unknown".into());
            }
            Ok(IdMapping {
                container_id: fields[0],
                host_id: fields[1],
                size: fields[2],
            })
        })
        .collect()
}

fn freeze_registration(root: &Path, registration: &Registration) -> Result<(), String> {
    let path = root.join("workload-registration.json");
    let encoded = serde_json::to_vec(registration).map_err(|_| "guest_registration_invalid")?;
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&path)
    {
        Ok(mut file) => {
            file.write_all(&encoded)
                .and_then(|()| file.sync_all())
                .map_err(|_| "guest_registration_unavailable")?;
            File::open(root)
                .and_then(|file| file.sync_all())
                .map_err(|_| "guest_registration_unavailable")?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let expected: Registration = serde_json::from_slice(&read_private(&path)?)
                .map_err(|_| "guest_registration_invalid")?;
            if expected != *registration {
                return Err("guest_registered_target_changed".into());
            }
        }
        Err(_) => return Err("guest_registration_unavailable".into()),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    fn temporary() -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "centaeris-guest-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        path
    }
    fn registration() -> Registration {
        Registration {
            environment_instance_id: "instance-a".into(),
            manifest_digest: format!("sha256:{}", "a".repeat(64)),
            init_pid: 200,
            init_start_time: 300,
            root: NodeIdentity {
                device: 1,
                inode: 2,
            },
            namespaces: BTreeMap::from([(
                "user".into(),
                NodeIdentity {
                    device: 3,
                    inode: 4,
                },
            )]),
        }
    }
    #[test]
    fn actual_current_namespace_is_never_an_attested_workload() {
        assert_eq!(
            verify_namespaces(std::process::id()).unwrap_err(),
            "guest_private_namespaces_required"
        );
    }
    #[test]
    fn observed_init_user_must_match_the_original_image_in_the_outer_mapping() {
        let spec = crate::guest_task::tests::bundle();
        let frozen = crate::guest_task::tests::manifest(&spec);
        verify_process_user(
            "Uid:\t100000 100000 100000 100000\nGid:\t100000 100000 100000 100000\nGroups:\t\n",
            &spec,
            &frozen.image_process.user,
        )
        .unwrap();
        assert_eq!(
            verify_process_user(
                "Uid:\t0 0 0 0\nGid:\t0 0 0 0\nGroups:\t\n",
                &spec,
                &frozen.image_process.user
            )
            .unwrap_err(),
            "guest_image_user_changed"
        );
        let actual = fs::read_to_string("/proc/self/status").unwrap();
        assert!(verify_process_user(&actual, &spec, &frozen.image_process.user).is_err());
    }

    #[test]
    #[ignore = "requires Linux root and CAP_CHOWN for a disposable shifted rootfs"]
    fn workload_root_rejects_symlinks_and_requires_shifted_container_root_ownership() {
        use std::{ffi::CString, os::unix::ffi::OsStrExt};
        let root = temporary();
        let path = root.join("rootfs");
        fs::create_dir(&path).unwrap();
        let spec = crate::guest_task::tests::bundle();
        assert_eq!(
            rootfs_node(&path, &spec).unwrap_err(),
            "guest_workload_root_untrusted"
        );
        let name = CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: this test owns the disposable path and changes no shared host state.
        assert_eq!(unsafe { libc::chown(name.as_ptr(), 100000, 100000) }, 0);
        assert_eq!(rootfs_node(&path, &spec).unwrap(), node(&path).unwrap());
        let link = root.join("root-link");
        symlink(&path, &link).unwrap();
        assert_eq!(
            rootfs_node(&link, &spec).unwrap_err(),
            "guest_workload_root_untrusted"
        );
        assert_eq!(unsafe { libc::chown(name.as_ptr(), 100000, 100001) }, 0);
        assert_eq!(
            rootfs_node(&path, &spec).unwrap_err(),
            "guest_workload_root_untrusted"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn kernel_id_mapping_observation_preserves_all_columns_and_rejects_malformed_rows() {
        assert_eq!(
            parse_kernel_mappings("         0     100000      65536\n").unwrap(),
            vec![IdMapping {
                container_id: 0,
                host_id: 100000,
                size: 65536
            }]
        );
        for invalid in [
            "0 100000",
            "0 100000 65536 extra",
            "0 x 65536",
            "0 100000 4294967296",
        ] {
            assert!(parse_kernel_mappings(invalid).is_err());
        }
    }
    #[test]
    #[ignore = "requires Linux root for disposable root-owned guest controls"]
    fn frozen_registration_accepts_identical_reopen_and_rejects_instance_or_target_replacement() {
        let root = temporary();
        let initial = registration();
        freeze_registration(&root, &initial).unwrap();
        freeze_registration(&root, &initial).unwrap();
        let before = fs::read(root.join("workload-registration.json")).unwrap();
        let mut changes = vec![];
        let mut changed = initial.clone();
        changed.environment_instance_id = "instance-b".into();
        changes.push(changed);
        let mut changed = initial.clone();
        changed.manifest_digest = format!("sha256:{}", "b".repeat(64));
        changes.push(changed);
        let mut changed = initial.clone();
        changed.init_start_time += 1;
        changes.push(changed);
        let mut changed = initial.clone();
        changed.root.inode += 1;
        changes.push(changed);
        let mut changed = initial.clone();
        changed.namespaces.get_mut("user").unwrap().inode += 1;
        changes.push(changed);
        for changed in changes {
            assert_eq!(
                freeze_registration(&root, &changed).unwrap_err(),
                "guest_registered_target_changed"
            );
        }
        assert_eq!(
            fs::read(root.join("workload-registration.json")).unwrap(),
            before
        );
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    #[ignore = "requires Linux root for disposable root-owned guest controls"]
    fn private_control_rejects_public_permissions_and_symlinks() {
        let root = temporary();
        let public = root.join("public");
        fs::create_dir(&public).unwrap();
        fs::set_permissions(&public, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            private_existing(&public, true).unwrap_err(),
            "guest_private_path_required"
        );
        let link = root.join("link");
        symlink(&public, &link).unwrap();
        assert!(private_existing(&link, true).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}

mod broker;

pub fn run_broker() -> Result<(), String> {
    broker::run_broker()
}
pub fn run_client() -> Result<(), String> {
    broker::run_client()
}
