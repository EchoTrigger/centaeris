//! Recover a stopped original's writable-root workspace without starting a guest.
use super::*;
use crate::postgres_store::execution_instances::RetainedInstanceAllocation;

trait CaptureIo {
    fn observe(
        &mut self,
        allocation: &RetainedInstanceAllocation,
        profile: &EnvironmentProfile,
    ) -> Result<serde_json::Value, String>;
    fn download(&mut self, id: &str, root: &str, limit: u64) -> Result<fs::File, String>;
}
struct DockerCaptureIo;
impl CaptureIo for DockerCaptureIo {
    fn observe(
        &mut self,
        allocation: &RetainedInstanceAllocation,
        profile: &EnvironmentProfile,
    ) -> Result<serde_json::Value, String> {
        if runsc_power::observe_recovery_target(allocation, profile)?
            != Some(runsc_power::RunscPowerObservation::Stopped)
        {
            return Err("stopped_workspace_target_required".into());
        }
        crate::docker_engine::inspect(
            allocation
                .container_id
                .as_deref()
                .ok_or("stopped_workspace_identity_missing")?,
        )?
        .ok_or_else(|| "stopped_workspace_target_missing".into())
    }
    fn download(&mut self, id: &str, root: &str, limit: u64) -> Result<fs::File, String> {
        crate::docker_engine::download_archive(id, root, limit)
    }
}

/// Capture the pinned original without guest execution. The caller holds cleanup
/// authority and commits this artifact before any physical retirement.
pub(super) fn capture_stopped_workspace(
    allocation: &RetainedInstanceAllocation,
    profile: &EnvironmentProfile,
    input_limit: u64,
) -> Result<ImmutableWorkspaceSnapshot, String> {
    capture_with(&mut DockerCaptureIo, allocation, profile, input_limit)
}

fn capture_with(
    io: &mut impl CaptureIo,
    allocation: &RetainedInstanceAllocation,
    profile: &EnvironmentProfile,
    input_limit: u64,
) -> Result<ImmutableWorkspaceSnapshot, String> {
    profile.validate()?;
    if profile.digest()? != allocation.profile_digest || input_limit == 0 {
        return Err("stopped_workspace_profile_or_budget_rejected".into());
    }
    let id = allocation
        .container_id
        .as_deref()
        .filter(|id| id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or("stopped_workspace_identity_missing")?;
    let before = io.observe(allocation, profile)?;
    validate_stopped_backing(&before, allocation, profile)?;
    let mut archive = io.download(id, &profile.workspace_root, input_limit)?;
    let artifact = snapshot_from_tar(
        &mut archive,
        &profile.workspace_root,
        input_limit,
        profile.resources.workspace_bytes,
    )?;
    let after = io.observe(allocation, profile)?;
    validate_stopped_backing(&after, allocation, profile)?;
    for pointer in ["/Mounts", "/HostConfig/Mounts", "/HostConfig/Tmpfs"] {
        if before.pointer(pointer) != after.pointer(pointer) {
            return Err("stopped_workspace_backing_changed".into());
        }
    }
    Ok(artifact)
}

fn canonical_absolute(path: &str) -> bool {
    path.starts_with('/')
        && !path.contains('\\')
        && !path.chars().any(char::is_control)
        && path.nfc().collect::<String>() == path
        && (path == "/"
            || path
                .split('/')
                .skip(1)
                .all(|part| !matches!(part, "" | "." | "..")))
}
fn overlaps(left: &str, right: &str) -> bool {
    left == "/"
        || right == "/"
        || left == right
        || left
            .strip_prefix(right)
            .is_some_and(|tail| tail.starts_with('/'))
        || right
            .strip_prefix(left)
            .is_some_and(|tail| tail.starts_with('/'))
}
fn validate_stopped_backing(
    facts: &serde_json::Value,
    allocation: &RetainedInstanceAllocation,
    profile: &EnvironmentProfile,
) -> Result<(), String> {
    if runsc_power::physical_state(facts, allocation, profile)?
        != runsc_power::RunscPowerObservation::Stopped
    {
        return Err("stopped_workspace_target_required".into());
    }
    if facts["Config"]["Labels"][environment_container::WORKSPACE_BACKING_LABEL].as_str()
        != Some(environment_container::WORKSPACE_BACKING_DOCKER_OVERLAYFS)
    {
        return Err("stopped_workspace_backing_unproven".into());
    }
    let root = &profile.workspace_root;
    if root == "/" {
        return Err("stopped_workspace_root_unsupported".into());
    }
    let actual = facts["Mounts"]
        .as_array()
        .ok_or("stopped_workspace_mounts_unknown")?;
    for mount in actual {
        let path = mount["Destination"]
            .as_str()
            .ok_or("stopped_workspace_mounts_unknown")?;
        if !canonical_absolute(path) || overlaps(path, root) {
            return Err("stopped_workspace_mounted_root_rejected".into());
        }
    }
    let configured = &facts["HostConfig"]["Mounts"];
    if !configured.is_null() {
        for mount in configured
            .as_array()
            .ok_or("stopped_workspace_mounts_unknown")?
        {
            let path = mount["Target"]
                .as_str()
                .ok_or("stopped_workspace_mounts_unknown")?;
            if !canonical_absolute(path) || overlaps(path, root) {
                return Err("stopped_workspace_mounted_root_rejected".into());
            }
        }
    }
    let tmpfs = &facts["HostConfig"]["Tmpfs"];
    if !tmpfs.is_null() {
        for path in tmpfs
            .as_object()
            .ok_or("stopped_workspace_mounts_unknown")?
            .keys()
        {
            if !canonical_absolute(path) || overlaps(path, root) {
                return Err("stopped_workspace_mounted_root_rejected".into());
            }
        }
    }
    Ok(())
}

// Bound virtual-tree bookkeeping and extension metadata before tar decodes them.
const ARCHIVE_ENTRY_LIMIT: usize = SESSION_WORKSPACE_MANIFEST_LIMIT / 32;
fn preflight_tar(raw: &mut fs::File, input_limit: u64) -> Result<(), String> {
    let length = raw
        .metadata()
        .map_err(|_| "stopped_workspace_archive_unavailable")?
        .len();
    if length < 1024 || length > input_limit || length % 512 != 0 {
        return Err("stopped_workspace_archive_size_rejected".into());
    }
    raw.rewind()
        .map_err(|_| "stopped_workspace_archive_unavailable")?;
    let mut end = 0;
    let mut metadata_bytes = 0_u64;
    {
        let mut archive = tar::Archive::new(&mut *raw);
        for (count, item) in archive
            .entries()
            .map_err(|_| "stopped_workspace_archive_invalid")?
            .raw(true)
            .enumerate()
        {
            if count >= ARCHIVE_ENTRY_LIMIT {
                return Err("stopped_workspace_entry_budget_exceeded".into());
            }
            let mut entry = item.map_err(|_| "stopped_workspace_archive_invalid")?;
            let kind = entry.header().entry_type();
            let size = entry.size();
            end = entry
                .raw_file_position()
                .checked_add(
                    size.checked_add(511)
                        .ok_or("stopped_workspace_archive_invalid")?
                        / 512
                        * 512,
                )
                .ok_or("stopped_workspace_archive_invalid")?;
            if end > length.saturating_sub(1024) {
                return Err("stopped_workspace_archive_truncated".into());
            }
            if kind.is_dir() && size != 0 {
                return Err("stopped_workspace_directory_invalid".into());
            }
            if kind.is_pax_local_extensions() || kind.is_gnu_longname() {
                metadata_bytes = metadata_bytes
                    .checked_add(size)
                    .ok_or("stopped_workspace_metadata_budget_exceeded")?;
                if metadata_bytes > SESSION_WORKSPACE_MANIFEST_LIMIT as u64 {
                    return Err("stopped_workspace_metadata_budget_exceeded".into());
                }
                let mut bytes = Vec::new();
                entry
                    .read_to_end(&mut bytes)
                    .map_err(|_| "stopped_workspace_archive_truncated")?;
                if kind.is_pax_local_extensions() {
                    let mut keys = HashSet::new();
                    for field in tar::PaxExtensions::new(&bytes) {
                        let field = field.map_err(|_| "stopped_workspace_metadata_invalid")?;
                        let key = field
                            .key()
                            .map_err(|_| "stopped_workspace_metadata_invalid")?;
                        if !keys.insert(key.to_string())
                            || key == "size"
                            || key == "linkpath"
                            || key.starts_with("GNU.sparse.")
                        {
                            return Err("stopped_workspace_metadata_unsupported".into());
                        }
                    }
                }
            } else if !kind.is_file() && !kind.is_dir() {
                return Err("stopped_workspace_node_unsupported".into());
            }
        }
    }
    raw.seek(std::io::SeekFrom::Start(end))
        .map_err(|_| "stopped_workspace_archive_unavailable")?;
    let mut buffer = [0; SESSION_WORKSPACE_IO_BUFFER_BYTES];
    loop {
        let count = raw
            .read(&mut buffer)
            .map_err(|_| "stopped_workspace_archive_unavailable")?;
        if count == 0 {
            break;
        }
        if buffer[..count].iter().any(|byte| *byte != 0) {
            return Err("stopped_workspace_archive_trailing_data".into());
        }
    }
    raw.rewind()
        .map_err(|_| "stopped_workspace_archive_unavailable")?;
    Ok(())
}

fn relative_path(path: &str, root: &str, directory: bool) -> Result<Option<String>, String> {
    let path = if directory {
        path.strip_suffix('/').unwrap_or(path)
    } else {
        path
    };
    if path.trim() != path
        || path.len() > 4096
        || path.contains('\\')
        || path.chars().any(char::is_control)
        || path.nfc().collect::<String>() != path
        || path.split('/').any(|part| matches!(part, "" | "." | ".."))
    {
        return Err("stopped_workspace_archive_path_rejected".into());
    }
    if path == root && directory {
        return Ok(None);
    }
    let relative = path
        .strip_prefix(root)
        .and_then(|tail| tail.strip_prefix('/'))
        .filter(|path| !path.is_empty())
        .ok_or("stopped_workspace_archive_root_rejected")?;
    if relative.split('/').count() > 64 {
        return Err("stopped_workspace_archive_path_rejected".into());
    }
    Ok(Some(relative.into()))
}

fn snapshot_from_tar(
    raw: &mut fs::File,
    root: &str,
    input_limit: u64,
    expanded_limit: u64,
) -> Result<ImmutableWorkspaceSnapshot, String> {
    preflight_tar(raw, input_limit)?;
    let root = root
        .rsplit('/')
        .next()
        .filter(|root| !root.is_empty())
        .ok_or("stopped_workspace_root_unsupported")?;
    let mut nodes = BTreeMap::new();
    let mut files = BTreeMap::new();
    let mut root_seen = false;
    let mut expanded = 0_u64;
    let mut manifest_budget = 0_usize;
    {
        let mut archive = tar::Archive::new(&mut *raw);
        for item in archive
            .entries()
            .map_err(|_| "stopped_workspace_archive_invalid")?
        {
            let mut entry = item.map_err(|_| "stopped_workspace_archive_invalid")?;
            let directory = entry.header().entry_type().is_dir();
            if !directory && !entry.header().entry_type().is_file() {
                return Err("stopped_workspace_node_unsupported".into());
            }
            let bytes = entry.path_bytes();
            let path = std::str::from_utf8(&bytes)
                .map_err(|_| "stopped_workspace_archive_path_rejected")?;
            let Some(path) = relative_path(path, root, directory)? else {
                if root_seen {
                    return Err("stopped_workspace_duplicate_path".into());
                }
                root_seen = true;
                continue;
            };
            if nodes.insert(path.clone(), directory).is_some() {
                return Err("stopped_workspace_duplicate_path".into());
            }
            if directory {
                continue;
            }
            expanded = expanded
                .checked_add(entry.size())
                .ok_or("stopped_workspace_logical_budget_exceeded")?;
            if expanded > expanded_limit {
                return Err("stopped_workspace_logical_budget_exceeded".into());
            }
            let offset = entry.raw_file_position();
            let mut digest = Sha256::new();
            let mut length = 0_u64;
            let mut buffer = [0; SESSION_WORKSPACE_IO_BUFFER_BYTES];
            loop {
                let count = entry
                    .read(&mut buffer)
                    .map_err(|_| "stopped_workspace_archive_truncated")?;
                if count == 0 {
                    break;
                }
                length += count as u64;
                digest.update(&buffer[..count]);
            }
            if length != entry.size() {
                return Err("stopped_workspace_archive_truncated".into());
            }
            let file = SandboxWorkspaceSnapshotFile {
                path: path.clone(),
                size_bytes: length,
                sha256: format!("sha256:{:x}", digest.finalize()),
                executable: entry
                    .header()
                    .mode()
                    .map_err(|_| "stopped_workspace_mode_invalid")?
                    & 0o111
                    != 0,
            };
            manifest_budget = manifest_budget
                .checked_add(
                    serde_json::to_vec(&file)
                        .map_err(|_| "stopped_workspace_manifest_invalid")?
                        .len()
                        + 1,
                )
                .ok_or("stopped_workspace_manifest_budget_exceeded")?;
            if manifest_budget > SESSION_WORKSPACE_MANIFEST_LIMIT {
                return Err("stopped_workspace_manifest_budget_exceeded".into());
            }
            files.insert(path, (file, offset));
        }
    }
    if !root_seen {
        return Err("stopped_workspace_root_directory_missing".into());
    }
    for path in nodes.keys() {
        for (offset, _) in path.match_indices('/') {
            if nodes.get(&path[..offset]) == Some(&false) {
                return Err("stopped_workspace_path_conflict".into());
            }
        }
    }
    let manifest = SandboxWorkspaceSnapshotManifest {
        schema: SANDBOX_WORKSPACE_SNAPSHOT_SCHEMA.into(),
        files: files.values().map(|(file, _)| file.clone()).collect(),
    };
    let mut frame = tempfile::tempfile().map_err(|_| "stopped_workspace_spool_unavailable")?;
    if !manifest.files.is_empty() {
        validate_workspace_snapshot_manifest(&manifest)?;
        let bytes =
            serde_json::to_vec(&manifest).map_err(|_| "stopped_workspace_manifest_invalid")?;
        if bytes.len() > SESSION_WORKSPACE_MANIFEST_LIMIT {
            return Err("stopped_workspace_manifest_budget_exceeded".into());
        }
        frame
            .write_all(&(bytes.len() as u32).to_be_bytes())
            .and_then(|_| frame.write_all(&bytes))
            .map_err(|_| "stopped_workspace_spool_unavailable")?;
        for (file, offset) in files.values() {
            raw.seek(std::io::SeekFrom::Start(*offset))
                .map_err(|_| "stopped_workspace_archive_unavailable")?;
            copy_exact(
                &mut (&mut *raw).take(file.size_bytes),
                &mut frame,
                file.size_bytes,
                &file.sha256,
            )?;
        }
    }
    frame
        .rewind()
        .map_err(|_| "stopped_workspace_spool_unavailable")?;
    let (descriptor, manifest) = inspect_workspace_snapshot_with_manifest(&mut frame)?;
    frame
        .rewind()
        .map_err(|_| "stopped_workspace_spool_unavailable")?;
    Ok(ImmutableWorkspaceSnapshot {
        file: frame,
        descriptor,
        manifest,
        file_source: None,
        observation: WorkspaceProtection::new(),
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_run_authorization::EnvironmentResources;
    use centaeris_core::execution::EnvironmentTaskOwner;
    use std::collections::VecDeque;

    fn fixture() -> (
        RetainedInstanceAllocation,
        EnvironmentProfile,
        serde_json::Value,
    ) {
        let profile = EnvironmentProfile {
            schema: "runtime.environment_profile.v1".into(),
            image_digest: format!("sha256:{}", "a".repeat(64)),
            workspace_root: "/app".into(),
            user: "10001:10001".into(),
            network: ExecutionNetwork::Disabled,
            resources: EnvironmentResources {
                memory_bytes: 1024 * 1024 * 1024,
                cpu_milli: 1000,
                pids_limit: 128,
                workspace_bytes: 1024 * 1024,
                storage_bytes: 1024 * 1024 * 1024,
            },
        };
        let allocation = RetainedInstanceAllocation {
            allocation_execution_id: "execution".into(),
            allocation_agent_run_id: Some("run".into()),
            allocation_owner: EnvironmentTaskOwner::AgentRun {
                run_id: "run".into(),
            },
            instance_id: "instance".into(),
            environment_id: "environment".into(),
            host_id: "daemon".into(),
            image_digest: profile.image_digest.clone(),
            authorization_digest: "authorization".into(),
            binding_digest: "binding".into(),
            profile_digest: profile.digest().unwrap(),
            container_id: Some("c".repeat(64)),
            reused: true,
        };
        let mut facts = serde_json::json!({"Id": allocation.container_id, "Image": allocation.image_digest,
            "State":{"Running":false,"Paused":false,"Restarting":false,"Dead":false},
            "Mounts":[],"Config":{"User":profile.user,"WorkingDir":"/","Labels":{
                "centaeris.managed":"true","centaeris.environment_id":allocation.environment_id,
                "centaeris.instance_id":allocation.instance_id,"centaeris.execution_id":allocation.allocation_execution_id,
                "centaeris.agent_run_id":"run","centaeris.allocation_owner":serde_json::to_string(&allocation.allocation_owner).unwrap(),
                "centaeris.environment_authorization_digest":allocation.binding_digest,"centaeris.environment_profile_digest":allocation.profile_digest}},
            "HostConfig":{"Runtime":"runsc","Memory":profile.resources.memory_bytes,"CpuQuota":100000,
                "CpuPeriod":100000,"PidsLimit":128,"NetworkMode":"none","Mounts":[],"Tmpfs":null}});
        facts["Config"]["Labels"][environment_container::WORKSPACE_BACKING_LABEL] =
            environment_container::WORKSPACE_BACKING_DOCKER_OVERLAYFS.into();
        (allocation, profile, facts)
    }

    fn archive(entries: &[(&str, tar::EntryType, &[u8], u32)]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (path, kind, bytes, mode) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(*kind);
            header.set_size(bytes.len() as u64);
            header.set_mode(*mode);
            header.set_cksum();
            builder.append_data(&mut header, path, *bytes).unwrap();
        }
        builder.into_inner().unwrap()
    }
    fn spool(bytes: &[u8]) -> fs::File {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(bytes).unwrap();
        file.rewind().unwrap();
        file
    }
    struct FakeIo {
        facts: VecDeque<Result<serde_json::Value, String>>,
        archive: Option<Result<fs::File, String>>,
        downloads: Vec<(String, String, u64)>,
    }
    impl CaptureIo for FakeIo {
        fn observe(
            &mut self,
            _: &RetainedInstanceAllocation,
            _: &EnvironmentProfile,
        ) -> Result<serde_json::Value, String> {
            self.facts
                .pop_front()
                .expect("every observation accounted for")
        }
        fn download(&mut self, id: &str, path: &str, limit: u64) -> Result<fs::File, String> {
            self.downloads.push((id.into(), path.into(), limit));
            self.archive.take().expect("only one download")
        }
    }
    fn io(facts: &serde_json::Value, bytes: &[u8]) -> FakeIo {
        FakeIo {
            facts: VecDeque::from([Ok(facts.clone()), Ok(facts.clone())]),
            archive: Some(Ok(spool(bytes))),
            downloads: vec![],
        }
    }

    #[test]
    fn canonical_frame_preserves_sorted_binary_contents_hashes_and_execute_bit() {
        let (a, p, facts) = fixture();
        let bytes = archive(&[
            ("app/", tar::EntryType::Directory, b"", 0o755),
            ("app/z.bin", tar::EntryType::Regular, b"\0\xff\x80", 0o600),
            ("app/a.sh", tar::EntryType::Regular, b"echo ok\n", 0o751),
        ]);
        let mut docker = io(&facts, &bytes);
        let mut artifact = capture_with(&mut docker, &a, &p, 100_000).unwrap();
        assert_eq!(
            docker.downloads,
            vec![(a.container_id.clone().unwrap(), "/app".into(), 100_000)]
        );
        let manifest = artifact.manifest.as_ref().unwrap();
        assert_eq!(
            manifest
                .files
                .iter()
                .map(|f| f.path.as_str())
                .collect::<Vec<_>>(),
            vec!["a.sh", "z.bin"]
        );
        assert!(manifest.files[0].executable);
        assert!(!manifest.files[1].executable);
        assert_eq!(
            manifest.files[1].sha256,
            format!("sha256:{:x}", Sha256::digest(b"\0\xff\x80"))
        );
        assert_eq!(artifact.descriptor.expanded_size_bytes, 11);
        assert_eq!(
            inspect_workspace_snapshot(&mut artifact.file).unwrap(),
            artifact.descriptor
        );
    }
    #[test]
    fn valid_empty_directory_is_empty_snapshot_but_missing_or_empty_response_is_failure() {
        let (a, p, facts) = fixture();
        let empty = archive(&[("app/", tar::EntryType::Directory, b"", 0o755)]);
        let artifact = capture_with(&mut io(&facts, &empty), &a, &p, 100_000).unwrap();
        assert_eq!(artifact.descriptor.size_bytes, 0);
        assert!(artifact.manifest.is_none());
        for bytes in [
            vec![],
            vec![0; 1024],
            archive(&[("other/", tar::EntryType::Directory, b"", 0o755)]),
        ] {
            assert!(capture_with(&mut io(&facts, &bytes), &a, &p, 100_000).is_err());
        }
        let mut missing = io(&facts, &empty);
        missing.archive = Some(Err("Docker archive path missing".into()));
        assert!(capture_with(&mut missing, &a, &p, 100_000).is_err());
    }
    #[test]
    fn unsafe_archive_nodes_paths_duplicates_and_truncation_fail_closed() {
        let (a, p, facts) = fixture();
        for kind in [
            tar::EntryType::Symlink,
            tar::EntryType::Link,
            tar::EntryType::Char,
            tar::EntryType::Block,
            tar::EntryType::Fifo,
        ] {
            let bytes = archive(&[
                ("app/", tar::EntryType::Directory, b"", 0o755),
                ("app/node", kind, b"", 0o600),
            ]);
            assert!(
                capture_with(&mut io(&facts, &bytes), &a, &p, 100_000).is_err(),
                "unsafe {kind:?}"
            );
        }
        for nodes in [
            vec![
                ("app/", tar::EntryType::Directory, b"".as_slice(), 0o755),
                ("app/a", tar::EntryType::Regular, b"x".as_slice(), 0o600),
                ("app/a", tar::EntryType::Regular, b"y".as_slice(), 0o600),
            ],
            vec![
                ("app/", tar::EntryType::Directory, b"".as_slice(), 0o755),
                ("other/a", tar::EntryType::Regular, b"x".as_slice(), 0o600),
            ],
            vec![
                ("app/", tar::EntryType::Directory, b"".as_slice(), 0o755),
                ("app/a", tar::EntryType::Regular, b"x".as_slice(), 0o600),
                ("app/a/b", tar::EntryType::Regular, b"x".as_slice(), 0o600),
            ],
        ] {
            let bytes = archive(&nodes);
            assert!(capture_with(&mut io(&facts, &bytes), &a, &p, 100_000).is_err());
        }
        let bytes = archive(&[
            ("app/", tar::EntryType::Directory, b"", 0o755),
            ("app/a", tar::EntryType::Regular, b"abc", 0o600),
        ]);
        for end in [0, 512, 1025, bytes.len() - 512] {
            assert!(
                capture_with(&mut io(&facts, &bytes[..end]), &a, &p, 100_000).is_err(),
                "truncation {end}"
            );
        }
    }
    #[test]
    fn raw_and_logical_budgets_are_enforced() {
        let (a, p, facts) = fixture();
        let bytes = archive(&[
            ("app/", tar::EntryType::Directory, b"", 0o755),
            ("app/a", tar::EntryType::Regular, b"abc", 0o600),
        ]);
        assert!(capture_with(&mut io(&facts, &bytes), &a, &p, 1).is_err());
        assert!(snapshot_from_tar(&mut spool(&bytes), "/app", 100_000, 2).is_err());
        assert!(snapshot_from_tar(&mut spool(&bytes), "/app", 100_000, 0).is_err());
    }
    #[test]
    fn host_unknown_or_raced_power_identity_and_mount_changes_never_publish_a_snapshot() {
        let (a, p, facts) = fixture();
        let bytes = archive(&[("app/", tar::EntryType::Directory, b"", 0o755)]);
        for pointer in ["/State/Running", "/Id", "/HostConfig/Mounts"] {
            let mut changed = facts.clone();
            if let Some(v) = changed.pointer_mut(pointer) {
                *v = match pointer {
                    "/State/Running" => true.into(),
                    "/Id" => "foreign".into(),
                    _ => serde_json::json!([{"Target":"/other","Type":"bind"}]),
                }
            }
            let mut docker = io(&facts, &bytes);
            docker.facts[1] = Ok(changed);
            assert!(
                capture_with(&mut docker, &a, &p, 100_000).is_err(),
                "changed {pointer}"
            );
        }
        let mut docker = io(&facts, &bytes);
        docker.facts[1] = Err("host unavailable".into());
        assert!(capture_with(&mut docker, &a, &p, 100_000).is_err());
    }
    #[test]
    fn overlapping_mounts_and_tmpfs_are_rejected_before_archive_download() {
        let (a, p, facts) = fixture();
        for (pointer, value) in [
            (
                "/Mounts",
                serde_json::json!([{"Destination":"/app","Type":"volume"}]),
            ),
            (
                "/HostConfig/Mounts",
                serde_json::json!([{"Target":"/app/sub","Type":"bind"}]),
            ),
            ("/HostConfig/Tmpfs", serde_json::json!({"/":"rw"})),
            ("/Mounts", serde_json::Value::Null),
        ] {
            let mut changed = facts.clone();
            *changed.pointer_mut(pointer).unwrap() = value;
            let mut docker = io(&changed, &[]);
            assert!(capture_with(&mut docker, &a, &p, 100_000).is_err());
            assert!(docker.downloads.is_empty());
        }
    }
    #[test]
    fn bounded_long_paths_are_decoded_without_extraction_or_omission() {
        let (a, p, facts) = fixture();
        let name = "x".repeat(150);
        let path = format!("app/{name}");
        let bytes = archive(&[
            ("app/", tar::EntryType::Directory, b"", 0o755),
            (&path, tar::EntryType::Regular, b"x", 0o600),
        ]);
        let artifact = capture_with(&mut io(&facts, &bytes), &a, &p, 100_000).unwrap();
        assert_eq!(artifact.manifest.unwrap().files[0].path, name);
    }

    #[test]
    fn escaped_noncanonical_paths_and_nonzero_trailing_data_are_rejected() {
        let (a, p, facts) = fixture();
        for path in [
            b"app/../secret".as_slice(),
            b"/app/a".as_slice(),
            b"app/a//b".as_slice(),
            b"app/a\\b".as_slice(),
        ] {
            let mut builder = tar::Builder::new(Vec::new());
            let mut directory = tar::Header::new_gnu();
            directory.set_size(0);
            directory.set_mode(0o755);
            directory.set_entry_type(tar::EntryType::Directory);
            directory.set_cksum();
            builder
                .append_data(&mut directory, "app/", std::io::empty())
                .unwrap();
            let mut header = tar::Header::new_gnu();
            header.set_size(1);
            header.set_mode(0o600);
            header.set_entry_type(tar::EntryType::Regular);
            header.as_mut_bytes()[..path.len()].copy_from_slice(path);
            header.set_cksum();
            builder.append(&header, b"x".as_slice()).unwrap();
            let bytes = builder.into_inner().unwrap();
            assert!(capture_with(&mut io(&facts, &bytes), &a, &p, 100_000).is_err());
        }
        let mut bytes = archive(&[("app/", tar::EntryType::Directory, b"", 0o755)]);
        *bytes.last_mut().unwrap() = 1;
        assert!(capture_with(&mut io(&facts, &bytes), &a, &p, 100_000).is_err());
    }

    #[test]
    fn manifest_and_tree_entry_budgets_bound_metadata_memory() {
        let mut builder = tar::Builder::new(Vec::new());
        for index in 0..7000 {
            let path = format!("app/{index:05}{}", "x".repeat(80));
            let mut header = tar::Header::new_gnu();
            header.set_size(0);
            header.set_mode(0o600);
            header.set_entry_type(tar::EntryType::Regular);
            header.set_cksum();
            builder
                .append_data(&mut header, path, std::io::empty())
                .unwrap();
        }
        let mut header = tar::Header::new_gnu();
        header.set_size(0);
        header.set_mode(0o755);
        header.set_entry_type(tar::EntryType::Directory);
        header.set_cksum();
        builder
            .append_data(&mut header, "app/", std::io::empty())
            .unwrap();
        let bytes = builder.into_inner().unwrap();
        assert!(
            matches!(snapshot_from_tar(&mut spool(&bytes),"/app",20*1024*1024,0),Err(error) if error=="stopped_workspace_manifest_budget_exceeded")
        );
        let mut builder = tar::Builder::new(Vec::new());
        for index in 0..=ARCHIVE_ENTRY_LIMIT {
            let mut header = tar::Header::new_gnu();
            header.set_size(0);
            header.set_mode(0o755);
            header.set_entry_type(tar::EntryType::Directory);
            header.set_cksum();
            builder
                .append_data(&mut header, format!("app/d{index}"), std::io::empty())
                .unwrap();
        }
        let bytes = builder.into_inner().unwrap();
        assert!(
            matches!(snapshot_from_tar(&mut spool(&bytes),"/app",20*1024*1024,0),Err(error) if error=="stopped_workspace_entry_budget_exceeded")
        );
    }
    #[test]
    fn legacy_or_foreign_backing_policy_never_turns_a_stale_lower_view_into_a_snapshot() {
        let (allocation, profile, facts) = fixture();
        for value in [serde_json::Value::Null, "guestOverlay".into()] {
            for payload in [b"".as_slice(), b"stale image data".as_slice()] {
                let mut unproven = facts.clone();
                if value.is_null() {
                    unproven["Config"]["Labels"]
                        .as_object_mut()
                        .unwrap()
                        .remove(environment_container::WORKSPACE_BACKING_LABEL);
                } else {
                    unproven["Config"]["Labels"][environment_container::WORKSPACE_BACKING_LABEL] =
                        value.clone();
                }
                let bytes = if payload.is_empty() {
                    archive(&[("app/", tar::EntryType::Directory, b"", 0o755)])
                } else {
                    archive(&[
                        ("app/", tar::EntryType::Directory, b"", 0o755),
                        ("app/stale", tar::EntryType::Regular, payload, 0o600),
                    ])
                };
                let mut docker = io(&unproven, &bytes);
                assert!(
                    matches!(capture_with(&mut docker,&allocation,&profile,100_000),Err(error) if error=="stopped_workspace_backing_unproven")
                );
                assert!(
                    docker.downloads.is_empty(),
                    "unproven lower view must not even be read"
                );
            }
        }
    }
}
