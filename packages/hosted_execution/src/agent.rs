#[cfg(target_os = "linux")]
use std::collections::BTreeMap;
use std::collections::{BTreeSet, HashSet};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

#[cfg(target_os = "linux")]
use std::io::{Seek, SeekFrom};

use centaeris_core::execution::{
    run_direct_execution_file_system_operation, run_scoped_execution_file_system_operation,
    ExecutionFileSystemError, ExecutionFileSystemErrorKind, ExecutionFileSystemOperation,
    ExecutionFileSystemOutput, ExecutionFileSystemRequest, MAX_EXECUTION_INPUT_BYTES,
    MAX_PUBLISHED_ARTIFACT_BYTES, WORKSPACE_DATA_ROOT,
};
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;

use crate::memory::{is_memory_uri, run_memory_file_system_operation};
#[cfg(any(target_os = "linux", test))]
use crate::protocol::SANDBOX_WORKSPACE_GENERATION_SCHEMA;
use crate::protocol::{
    SandboxArtifactMetadata, SandboxArtifactRequest, SandboxFileSystemRequest,
    SandboxFileSystemResult, SandboxInputInventory, SandboxInputRevokeRequest,
    SandboxMaterializedInput, SandboxWorkspaceGeneration, SandboxWorkspaceSnapshotFile,
    SandboxWorkspaceSnapshotManifest, SANDBOX_INPUT_INVENTORY_SCHEMA,
    SANDBOX_WORKSPACE_GENERATION_QUERY_LINE, SANDBOX_WORKSPACE_SNAPSHOT_SCHEMA,
};

const STATE_ROOT: &str = "/run/centaeris";
const INPUT_INVENTORY_PATH: &str = "/run/centaeris/inputs.json";
#[cfg(target_os = "linux")]
const WORKSPACE_GENERATION_SOCKET: &str = "/run/centaeris/workspace-generation.sock";
const MAX_JSON_BYTES: usize = 1024 * 1024;
const PLUGIN_ROOT: &str = "/opt/centaeris/plugins";
const SYSTEM_SKILL_ROOT: &str = "/opt/centaeris/system-skills";
const SNAPSHOT_PATH_MAX_BYTES: usize = 4 * 1024;
const SNAPSHOT_PATH_MAX_DEPTH: usize = 64;
#[cfg(target_os = "linux")]
const SNAPSHOT_IO_BUFFER_BYTES: usize = 64 * 1024;
#[cfg(target_os = "linux")]
const AGENT_UID: u32 = 10_001;
#[cfg(target_os = "linux")]
const AGENT_GID: u32 = 10_001;

#[derive(Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AgentProcessState {
    active_process_count: usize,
}

pub fn run_agent_process_state_once() -> Result<(), String> {
    let state = observe_agent_process_state(agent_process_ids)?;
    serde_json::to_writer(std::io::stdout(), &state)
        .map_err(|error| format!("write agent process state failed: {error}"))
}

fn observe_agent_process_state(
    inspect: impl FnOnce() -> Result<Vec<i32>, String>,
) -> Result<AgentProcessState, String> {
    Ok(AgentProcessState {
        active_process_count: inspect()?.len(),
    })
}

pub fn run_filesystem_once() -> Result<(), String> {
    let request: SandboxFileSystemRequest = read_json(MAX_JSON_BYTES)?;
    let result = run_file_system_request(request);
    serde_json::to_writer(std::io::stdout(), &SandboxFileSystemResult::from(result))
        .map_err(|error| format!("encode filesystem helper result failed: {error}"))
}

fn run_file_system_request(
    request: SandboxFileSystemRequest,
) -> Result<ExecutionFileSystemOutput, ExecutionFileSystemError> {
    if is_memory_uri(request.path.as_str()) {
        run_memory_file_system_operation(request)
    } else if request.path.contains("://") {
        Err(ExecutionFileSystemError::new(
            ExecutionFileSystemErrorKind::InvalidPath,
            "filesystem URI scheme is unsupported",
        ))
    } else if request.path == SYSTEM_SKILL_ROOT
        || request.path.starts_with(&format!("{SYSTEM_SKILL_ROOT}/"))
    {
        run_read_only_file_system_operation(request, SYSTEM_SKILL_ROOT, "System Skill")
    } else if request.path.starts_with(&format!("{PLUGIN_ROOT}/")) {
        run_plugin_file_system_operation(request)
    } else {
        run_scoped_execution_file_system_operation(ExecutionFileSystemRequest {
            operation_id: None,
            cwd: PathBuf::from(WORKSPACE_DATA_ROOT),
            policy: centaeris_core::execution::ExecutionPolicy::workspace_write_no_network(
                WORKSPACE_DATA_ROOT,
            ),
            model_path: request.path,
            operation: request.operation,
        })
    }
}

fn run_plugin_file_system_operation(
    request: SandboxFileSystemRequest,
) -> Result<centaeris_core::execution::ExecutionFileSystemOutput, ExecutionFileSystemError> {
    run_read_only_file_system_operation(request, PLUGIN_ROOT, "activated Plugin package")
}

fn run_read_only_file_system_operation(
    request: SandboxFileSystemRequest,
    root: &str,
    label: &str,
) -> Result<centaeris_core::execution::ExecutionFileSystemOutput, ExecutionFileSystemError> {
    if !matches!(
        &request.operation,
        ExecutionFileSystemOperation::ReadFile { .. }
            | ExecutionFileSystemOperation::ListDirectory { .. }
    ) {
        return Err(ExecutionFileSystemError::new(
            ExecutionFileSystemErrorKind::PermissionDenied,
            format!("{label} is read-only"),
        ));
    }
    validate_read_only_path(request.path.as_str(), root, label)?;
    run_direct_execution_file_system_operation(ExecutionFileSystemRequest {
        operation_id: None,
        cwd: PathBuf::from(WORKSPACE_DATA_ROOT),
        policy: centaeris_core::execution::ExecutionPolicy::read_only_no_network(PLUGIN_ROOT),
        model_path: request.path,
        operation: request.operation,
    })
}

fn validate_read_only_path(
    path: &str,
    root_path: &str,
    label: &str,
) -> Result<(), ExecutionFileSystemError> {
    if path != root_path {
        let relative = path.strip_prefix(&format!("{root_path}/")).ok_or_else(|| {
            ExecutionFileSystemError::new(
                ExecutionFileSystemErrorKind::InvalidPath,
                format!("{label} path is invalid"),
            )
        })?;
        if relative.contains('\\')
            || relative.chars().any(char::is_control)
            || relative
                .split('/')
                .any(|component| component.is_empty() || matches!(component, "." | ".."))
        {
            return Err(ExecutionFileSystemError::new(
                ExecutionFileSystemErrorKind::InvalidPath,
                format!("{label} path is invalid"),
            ));
        }
    }
    let root = Path::new(root_path).canonicalize().map_err(|error| {
        ExecutionFileSystemError::new(
            ExecutionFileSystemErrorKind::HostUnavailable,
            format!("{label} projection is unavailable"),
        )
        .with_diagnostic(error.to_string())
    })?;
    let target = Path::new(path).canonicalize().map_err(|error| {
        let kind = if error.kind() == std::io::ErrorKind::NotFound {
            ExecutionFileSystemErrorKind::NotFound
        } else {
            ExecutionFileSystemErrorKind::Io
        };
        ExecutionFileSystemError::new(kind, format!("{label} path is unavailable"))
            .with_diagnostic(error.to_string())
    })?;
    if !target.starts_with(root) {
        return Err(ExecutionFileSystemError::new(
            ExecutionFileSystemErrorKind::PermissionDenied,
            format!("{label} path escaped the read-only projection"),
        ));
    }
    Ok(())
}

pub fn run_input_inventory_once() -> Result<(), String> {
    require_root()?;
    let inventory = load_inventory()?;
    serde_json::to_writer(std::io::stdout(), &inventory)
        .map_err(|error| format!("encode input inventory failed: {error}"))
}

pub fn run_materialize_input_once() -> Result<(), String> {
    require_root()?;
    secure_roots()?;
    let frame = read_stdin(MAX_EXECUTION_INPUT_BYTES as usize + MAX_JSON_BYTES + 4)?;
    let (input, bytes) = decode_frame::<SandboxMaterializedInput>(frame.as_slice())?;
    validate_materialized_input(&input)?;
    if input.state.is_some() {
        return Err("materialized input request must be active".to_string());
    }
    if bytes.len() as u64 != input.size_bytes
        || format!("sha256:{:x}", Sha256::digest(bytes)) != input.sha256
    {
        return Err("materialized input integrity mismatch".to_string());
    }

    let mut inventory = load_inventory()?;
    if let Some(existing) = inventory
        .inputs
        .iter()
        .find(|existing| existing.input_ref == input.input_ref)
    {
        return if existing == &input && existing.state.is_none() {
            Ok(())
        } else {
            Err("materialized input identity conflict".to_string())
        };
    }
    let target = data_target(input.virtual_path.as_str())?;
    install_protected_input(target.as_path(), bytes)?;
    inventory.inputs.push(input);
    inventory
        .inputs
        .sort_by(|left, right| left.input_ref.cmp(&right.input_ref));
    save_inventory(&inventory)
}

pub fn run_revoke_input_once() -> Result<(), String> {
    require_root()?;
    secure_roots()?;
    let request: SandboxInputRevokeRequest = read_json(MAX_JSON_BYTES)?;
    let target = data_target(request.virtual_path.as_str())?;
    let mut inventory = load_inventory()?;
    let input = inventory
        .inputs
        .iter_mut()
        .find(|input| input.input_ref == request.input_ref)
        .ok_or_else(|| "sandbox input is not materialized".to_string())?;
    if input.virtual_path != request.virtual_path {
        return Err("sandbox input identity conflict".to_string());
    }
    match input.state {
        Some(state) if state == request.state => return Ok(()),
        Some(_) => return Err("sandbox input state conflict".to_string()),
        None => {}
    }
    install_tombstone(target.as_path())?;
    input.state = Some(request.state);
    save_inventory(&inventory)
}

pub fn run_read_artifact_once() -> Result<(), String> {
    require_root()?;
    secure_roots()?;
    let request: SandboxArtifactRequest = read_json(MAX_JSON_BYTES)?;
    if request.max_bytes == 0 || request.max_bytes > MAX_PUBLISHED_ARTIFACT_BYTES {
        return Err("artifact size limit is invalid".to_string());
    }
    let relative = normalized_data_path(request.path.as_str())?;
    if load_inventory()?
        .inputs
        .iter()
        .any(|input| input.state.is_none() && input.virtual_path == relative)
    {
        return Err("protected input cannot be published as an artifact".to_string());
    }
    let filename = Path::new(relative.as_str())
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "artifact filename is invalid".to_string())?
        .to_string();
    let (bytes, sha256) = read_artifact(relative.as_str(), request.max_bytes)?;
    let metadata = SandboxArtifactMetadata {
        filename,
        size_bytes: bytes.len() as u64,
        sha256,
    };
    write_frame(&metadata, bytes.as_slice())
}

pub fn run_snapshot_collect_once() -> Result<(), String> {
    require_root()?;
    secure_roots()?;
    let manifest = collect_snapshot_manifest(&load_inventory()?)?;
    if manifest.files.is_empty() {
        return Ok(());
    }
    write_snapshot_frame(&manifest)
}

pub fn run_snapshot_restore_once() -> Result<(), String> {
    require_root()?;
    secure_roots()?;
    let mut stdin = std::io::stdin().lock();
    let mut length = [0_u8; 4];
    stdin
        .read_exact(&mut length)
        .map_err(|error| format!("read snapshot manifest length failed: {error}"))?;
    let manifest_length = u32::from_be_bytes(length) as usize;
    if manifest_length == 0 || manifest_length > MAX_JSON_BYTES {
        return Err("snapshot manifest length is invalid".to_string());
    }
    let mut manifest_bytes = vec![0_u8; manifest_length];
    stdin
        .read_exact(manifest_bytes.as_mut_slice())
        .map_err(|error| format!("read snapshot manifest failed: {error}"))?;
    let manifest =
        serde_json::from_slice::<SandboxWorkspaceSnapshotManifest>(manifest_bytes.as_slice())
            .map_err(|error| format!("decode snapshot manifest failed: {error}"))?;
    if canonical_snapshot_manifest_bytes(&manifest)? != manifest_bytes {
        return Err("snapshot manifest is not canonical".to_string());
    }
    let inputs = load_inventory()?;
    for file in &manifest.files {
        if inputs
            .inputs
            .iter()
            .any(|input| snapshot_paths_conflict(file.path.as_str(), input.virtual_path.as_str()))
        {
            return Err("snapshot path conflicts with a materialized input".to_string());
        }
        restore_snapshot_file(&mut stdin, file)?;
    }
    let mut trailing = [0_u8; 1];
    if stdin
        .read(&mut trailing)
        .map_err(|error| format!("read snapshot trailing bytes failed: {error}"))?
        != 0
    {
        return Err("snapshot frame has trailing bytes".to_string());
    }
    seal_snapshot_directories(&manifest)?;
    Ok(())
}

pub fn run_workspace_watch() -> Result<(), String> {
    require_root()?;
    secure_roots()?;
    workspace_watch_loop()
}

pub fn run_workspace_generation_once() -> Result<(), String> {
    require_root()?;
    let generation = query_workspace_generation()?;
    generation.validate()?;
    serde_json::to_writer(std::io::stdout(), &generation)
        .map_err(|error| format!("encode workspace generation failed: {error}"))
}

pub fn run_workspace_generation_rpc() -> Result<(), String> {
    require_root()?;
    workspace_generation_rpc_loop(
        std::io::stdin(),
        std::io::stdout(),
        query_workspace_generation,
    )
}

fn workspace_generation_rpc_loop(
    input: impl Read,
    output: impl Write,
    mut query: impl FnMut() -> Result<SandboxWorkspaceGeneration, String>,
) -> Result<(), String> {
    let mut reader = BufReader::new(input);
    let mut writer = BufWriter::new(output);
    loop {
        let mut request = Vec::new();
        let read = reader
            .by_ref()
            .take(MAX_JSON_BYTES as u64 + 2)
            .read_until(b'\n', &mut request)
            .map_err(|error| format!("read workspace generation RPC request failed: {error}"))?;
        if read == 0 {
            return Ok(());
        }
        if request != SANDBOX_WORKSPACE_GENERATION_QUERY_LINE {
            return Err("workspace generation RPC request is invalid".to_string());
        }
        let generation = query()?;
        generation.validate()?;
        serde_json::to_writer(&mut writer, &generation)
            .map_err(|error| format!("encode workspace generation RPC response failed: {error}"))?;
        writer
            .write_all(b"\n")
            .and_then(|_| writer.flush())
            .map_err(|error| format!("write workspace generation RPC response failed: {error}"))?;
    }
}

pub fn run_quiesce_agent_processes_once() -> Result<(), String> {
    require_agent()?;
    quiesce_agent_processes()
}

fn collect_snapshot_manifest(
    inventory: &SandboxInputInventory,
) -> Result<SandboxWorkspaceSnapshotManifest, String> {
    let protected_paths = inventory
        .inputs
        .iter()
        .map(|input| input.virtual_path.as_str())
        .collect::<BTreeSet<_>>();
    let mut paths = Vec::new();
    collect_snapshot_paths(
        Path::new(WORKSPACE_DATA_ROOT),
        "",
        &protected_paths,
        &mut paths,
    )?;
    paths.sort_unstable();
    let files = paths
        .iter()
        .map(|path| snapshot_file_metadata(path.as_str()))
        .collect::<Result<Vec<_>, _>>()?;
    collected_snapshot_manifest(files)
}

fn collected_snapshot_manifest(
    files: Vec<SandboxWorkspaceSnapshotFile>,
) -> Result<SandboxWorkspaceSnapshotManifest, String> {
    let manifest = SandboxWorkspaceSnapshotManifest {
        schema: SANDBOX_WORKSPACE_SNAPSHOT_SCHEMA.to_string(),
        files,
    };
    if manifest.files.is_empty() {
        return Ok(manifest);
    }
    canonical_snapshot_manifest_bytes(&manifest)?;
    Ok(manifest)
}

fn collect_snapshot_paths(
    directory: &Path,
    prefix: &str,
    protected_paths: &BTreeSet<&str>,
    paths: &mut Vec<String>,
) -> Result<(), String> {
    for entry in fs::read_dir(directory)
        .map_err(|error| format!("read workspace directory failed: {error}"))?
    {
        let entry = entry.map_err(|error| format!("read workspace entry failed: {error}"))?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| "workspace path is not valid UTF-8".to_string())?;
        validate_snapshot_component(name.as_str())?;
        let relative = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        validate_snapshot_path(relative.as_str())?;
        let metadata = fs::symlink_metadata(entry.path())
            .map_err(|error| format!("inspect workspace entry failed: {error}"))?;
        if metadata.file_type().is_symlink() {
            return Err("workspace snapshot does not support symlinks".to_string());
        }
        if metadata.is_dir() {
            collect_snapshot_paths(
                entry.path().as_path(),
                relative.as_str(),
                protected_paths,
                paths,
            )?;
            continue;
        }
        if !metadata.is_file() {
            return Err("workspace snapshot does not support this node type".to_string());
        }
        if protected_paths.contains(relative.as_str()) {
            validate_registered_input_file(&metadata)?;
            continue;
        }
        paths.push(relative);
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn snapshot_file_metadata(relative_path: &str) -> Result<SandboxWorkspaceSnapshotFile, String> {
    use std::os::unix::fs::MetadataExt;

    let mut file = open_artifact_beneath(relative_path)?;
    let before = file
        .metadata()
        .map_err(|_| "workspace snapshot source is unavailable".to_string())?;
    validate_snapshot_file_metadata(&before)?;
    let (size_bytes, sha256) = hash_snapshot_file(&mut file)?;
    let after = file
        .metadata()
        .map_err(|_| "workspace snapshot source changed".to_string())?;
    if file_version(&before) != file_version(&after) || size_bytes != before.size() {
        return Err("workspace snapshot source changed".to_string());
    }
    Ok(SandboxWorkspaceSnapshotFile {
        path: relative_path.to_string(),
        size_bytes,
        sha256,
        executable: before.mode() & 0o111 != 0,
    })
}

#[cfg(not(target_os = "linux"))]
fn snapshot_file_metadata(_relative_path: &str) -> Result<SandboxWorkspaceSnapshotFile, String> {
    Err("sandbox helper requires Linux".to_string())
}

#[cfg(target_os = "linux")]
fn hash_snapshot_file(file: &mut File) -> Result<(u64, String), String> {
    let mut digest = Sha256::new();
    let mut size_bytes = 0_u64;
    let mut buffer = [0_u8; SNAPSHOT_IO_BUFFER_BYTES];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|error| format!("read workspace snapshot source failed: {error}"))?;
        if count == 0 {
            break;
        }
        size_bytes = size_bytes
            .checked_add(count as u64)
            .ok_or_else(|| "workspace snapshot size overflow".to_string())?;
        digest.update(&buffer[..count]);
    }
    Ok((size_bytes, format!("sha256:{:x}", digest.finalize())))
}

fn write_snapshot_frame(manifest: &SandboxWorkspaceSnapshotManifest) -> Result<(), String> {
    let manifest_bytes = canonical_snapshot_manifest_bytes(manifest)?;
    let manifest_length = u32::try_from(manifest_bytes.len())
        .map_err(|_| "snapshot manifest is too large".to_string())?;
    let mut stdout = std::io::stdout().lock();
    stdout
        .write_all(&manifest_length.to_be_bytes())
        .and_then(|_| stdout.write_all(manifest_bytes.as_slice()))
        .map_err(|error| format!("write snapshot manifest failed: {error}"))?;
    for expected in &manifest.files {
        copy_snapshot_file(expected, &mut stdout)?;
    }
    stdout
        .flush()
        .map_err(|error| format!("flush workspace snapshot failed: {error}"))
}

#[cfg(target_os = "linux")]
fn copy_snapshot_file(
    expected: &SandboxWorkspaceSnapshotFile,
    output: &mut impl Write,
) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;

    let mut file = open_artifact_beneath(expected.path.as_str())?;
    let before = file
        .metadata()
        .map_err(|_| "workspace snapshot source is unavailable".to_string())?;
    validate_snapshot_file_metadata(&before)?;
    if before.size() != expected.size_bytes || (before.mode() & 0o111 != 0) != expected.executable {
        return Err("workspace snapshot source changed".to_string());
    }
    let mut digest = Sha256::new();
    let mut size_bytes = 0_u64;
    let mut buffer = [0_u8; SNAPSHOT_IO_BUFFER_BYTES];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|error| format!("read workspace snapshot source failed: {error}"))?;
        if count == 0 {
            break;
        }
        output
            .write_all(&buffer[..count])
            .map_err(|error| format!("write workspace snapshot file failed: {error}"))?;
        size_bytes = size_bytes
            .checked_add(count as u64)
            .ok_or_else(|| "workspace snapshot size overflow".to_string())?;
        digest.update(&buffer[..count]);
    }
    let after = file
        .metadata()
        .map_err(|_| "workspace snapshot source changed".to_string())?;
    if file_version(&before) != file_version(&after)
        || size_bytes != expected.size_bytes
        || format!("sha256:{:x}", digest.finalize()) != expected.sha256
    {
        return Err("workspace snapshot source changed".to_string());
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn copy_snapshot_file(
    _expected: &SandboxWorkspaceSnapshotFile,
    _output: &mut impl Write,
) -> Result<(), String> {
    Err("sandbox helper requires Linux".to_string())
}

fn canonical_snapshot_manifest_bytes(
    manifest: &SandboxWorkspaceSnapshotManifest,
) -> Result<Vec<u8>, String> {
    validate_snapshot_manifest(manifest)?;
    let bytes = serde_json::to_vec(manifest)
        .map_err(|error| format!("encode snapshot manifest failed: {error}"))?;
    if bytes.len() > MAX_JSON_BYTES {
        return Err("snapshot manifest is too large".to_string());
    }
    Ok(bytes)
}

fn validate_snapshot_manifest(manifest: &SandboxWorkspaceSnapshotManifest) -> Result<(), String> {
    if manifest.schema != SANDBOX_WORKSPACE_SNAPSHOT_SCHEMA || manifest.files.is_empty() {
        return Err("snapshot manifest is invalid".to_string());
    }
    let mut previous = None;
    let mut expanded_size_bytes = 0_u64;
    for file in &manifest.files {
        validate_snapshot_path(file.path.as_str())?;
        validate_snapshot_sha256(file.sha256.as_str())?;
        if previous.is_some_and(|value: &str| value >= file.path.as_str())
            || previous.is_some_and(|value: &str| {
                file.path
                    .strip_prefix(value)
                    .is_some_and(|suffix| suffix.starts_with('/'))
            })
        {
            return Err("snapshot manifest paths are not sorted and unique".to_string());
        }
        expanded_size_bytes = expanded_size_bytes
            .checked_add(file.size_bytes)
            .ok_or_else(|| "snapshot expanded size overflow".to_string())?;
        previous = Some(file.path.as_str());
    }
    Ok(())
}

fn validate_snapshot_path(path: &str) -> Result<(), String> {
    let normalized = normalized_data_path(path)?;
    if normalized == "."
        || normalized != path
        || path.len() > SNAPSHOT_PATH_MAX_BYTES
        || path.split('/').count() > SNAPSHOT_PATH_MAX_DEPTH
        || path.nfc().collect::<String>() != path
    {
        return Err("snapshot path is invalid".to_string());
    }
    Ok(())
}

fn validate_snapshot_component(component: &str) -> Result<(), String> {
    if component.is_empty()
        || component == "."
        || component == ".."
        || component.contains(['/', '\\'])
        || component.chars().any(char::is_control)
        || component.nfc().collect::<String>() != component
    {
        return Err("workspace snapshot path is invalid".to_string());
    }
    Ok(())
}

fn validate_snapshot_sha256(value: &str) -> Result<(), String> {
    if value.len() != 71
        || !value.starts_with("sha256:")
        || !value[7..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("snapshot SHA-256 is invalid".to_string());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn validate_snapshot_file_metadata(metadata: &fs::Metadata) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;

    if !metadata.is_file() || metadata.nlink() != 1 || metadata.uid() != AGENT_UID {
        return Err("workspace snapshot source is unsafe".to_string());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn validate_registered_input_file(metadata: &fs::Metadata) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;

    if !metadata.is_file() || metadata.uid() != 0 || metadata.nlink() != 1 {
        return Err("registered sandbox input is unsafe".to_string());
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn validate_registered_input_file(_metadata: &fs::Metadata) -> Result<(), String> {
    Err("sandbox helper requires Linux".to_string())
}

fn snapshot_paths_conflict(left: &str, right: &str) -> bool {
    left == right
        || left
            .strip_prefix(right)
            .is_some_and(|suffix| suffix.starts_with('/'))
        || right
            .strip_prefix(left)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

#[cfg(target_os = "linux")]
fn restore_snapshot_file(
    input: &mut impl Read,
    expected: &SandboxWorkspaceSnapshotFile,
) -> Result<(), String> {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::MetadataExt;

    let target = Path::new(WORKSPACE_DATA_ROOT).join(expected.path.as_str());
    let parent = target
        .parent()
        .ok_or_else(|| "snapshot target has no parent".to_string())?;
    create_snapshot_directories(parent)?;
    match fs::symlink_metadata(target.as_path()) {
        Ok(_) => return Err("snapshot restore target already exists".to_string()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("inspect snapshot restore target failed: {error}")),
    }
    let mut output = File::options()
        .write(true)
        .create_new(true)
        .open(target.as_path())
        .map_err(|error| format!("create snapshot restore target failed: {error}"))?;
    let mode = if expected.executable { 0o755 } else { 0o644 };
    if unsafe { libc::fchmod(output.as_raw_fd(), mode) } != 0 {
        return Err(format!(
            "seal snapshot restore target failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    if unsafe { libc::fchown(output.as_raw_fd(), AGENT_UID, AGENT_GID) } != 0 {
        return Err(format!(
            "assign snapshot restore ownership failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    let mut digest = Sha256::new();
    let mut remaining = expected.size_bytes;
    let mut buffer = [0_u8; SNAPSHOT_IO_BUFFER_BYTES];
    while remaining != 0 {
        let wanted = buffer.len().min(remaining as usize);
        input
            .read_exact(&mut buffer[..wanted])
            .map_err(|error| format!("read snapshot file bytes failed: {error}"))?;
        output
            .write_all(&buffer[..wanted])
            .map_err(|error| format!("write snapshot restore target failed: {error}"))?;
        digest.update(&buffer[..wanted]);
        remaining -= wanted as u64;
    }
    output
        .sync_all()
        .map_err(|error| format!("sync snapshot restore target failed: {error}"))?;
    let metadata = output
        .metadata()
        .map_err(|error| format!("inspect snapshot restore target failed: {error}"))?;
    if !metadata.is_file()
        || metadata.uid() != AGENT_UID
        || metadata.gid() != AGENT_GID
        || metadata.nlink() != 1
        || metadata.size() != expected.size_bytes
        || (metadata.mode() & 0o111 != 0) != expected.executable
        || format!("sha256:{:x}", digest.finalize()) != expected.sha256
    {
        return Err("snapshot restore file integrity mismatch".to_string());
    }
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("sync snapshot restore directory failed: {error}"))
}

#[cfg(not(target_os = "linux"))]
fn restore_snapshot_file(
    _input: &mut impl Read,
    _expected: &SandboxWorkspaceSnapshotFile,
) -> Result<(), String> {
    Err("sandbox helper requires Linux".to_string())
}

#[cfg(target_os = "linux")]
fn create_snapshot_directories(target: &Path) -> Result<(), String> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let root = Path::new(WORKSPACE_DATA_ROOT);
    if !target.starts_with(root) {
        return Err("snapshot restore parent escaped /mnt/data".to_string());
    }
    let mut current = root.to_path_buf();
    for component in target
        .strip_prefix(root)
        .map_err(|_| "snapshot restore parent escaped /mnt/data".to_string())?
        .components()
    {
        current.push(component);
        match fs::symlink_metadata(current.as_path()) {
            Ok(metadata)
                if metadata.is_dir()
                    && !metadata.file_type().is_symlink()
                    && metadata.uid() == 0
                    && metadata.gid() == 0
                    && metadata.mode() & 0o7777 == 0o755 => {}
            Ok(_) => return Err("snapshot restore parent contains an unsupported node".to_string()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(current.as_path())
                    .map_err(|error| format!("create snapshot restore parent failed: {error}"))?;
                fs::set_permissions(current.as_path(), fs::Permissions::from_mode(0o755))
                    .map_err(|error| format!("protect snapshot restore parent failed: {error}"))?;
                let metadata = fs::symlink_metadata(current.as_path())
                    .map_err(|error| format!("inspect snapshot restore parent failed: {error}"))?;
                if metadata.uid() != 0 || metadata.gid() != 0 {
                    return Err("snapshot restore parent initial ownership is invalid".to_string());
                }
            }
            Err(error) => return Err(format!("inspect snapshot restore parent failed: {error}")),
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn seal_snapshot_directories(manifest: &SandboxWorkspaceSnapshotManifest) -> Result<(), String> {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::MetadataExt;

    let root = Path::new(WORKSPACE_DATA_ROOT);
    let mut directories = BTreeSet::new();
    for file in &manifest.files {
        let mut parent = Path::new(file.path.as_str()).parent();
        while let Some(relative) = parent.filter(|path| !path.as_os_str().is_empty()) {
            directories.insert(root.join(relative));
            parent = relative.parent();
        }
    }
    for path in directories.into_iter().rev() {
        let metadata = fs::symlink_metadata(path.as_path())
            .map_err(|error| format!("inspect snapshot restore parent failed: {error}"))?;
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || metadata.uid() != 0
            || metadata.gid() != 0
            || metadata.mode() & 0o7777 != 0o755
        {
            return Err("snapshot restore parent final ownership is invalid".to_string());
        }
        let directory = File::open(path.as_path())
            .map_err(|error| format!("open snapshot restore parent failed: {error}"))?;
        if unsafe { libc::fchown(directory.as_raw_fd(), AGENT_UID, AGENT_GID) } != 0 {
            return Err(format!(
                "assign snapshot restore parent ownership failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        let metadata = directory
            .metadata()
            .map_err(|error| format!("inspect snapshot restore parent failed: {error}"))?;
        if metadata.uid() != AGENT_UID
            || metadata.gid() != AGENT_GID
            || metadata.mode() & 0o7777 != 0o755
        {
            return Err("snapshot restore parent final ownership is invalid".to_string());
        }
        directory
            .sync_all()
            .map_err(|error| format!("sync snapshot restore parent failed: {error}"))?;
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn seal_snapshot_directories(_manifest: &SandboxWorkspaceSnapshotManifest) -> Result<(), String> {
    Err("sandbox helper requires Linux".to_string())
}

#[cfg(any(target_os = "linux", test))]
struct WorkspaceWatchState {
    instance_epoch: String,
    generation: u64,
    diagnostic: Option<String>,
}

#[cfg(any(target_os = "linux", test))]
impl WorkspaceWatchState {
    fn mark_change(&mut self) {
        if self.diagnostic.is_some() {
            return;
        }
        match self.generation.checked_add(1) {
            Some(generation) => self.generation = generation,
            None => self.mark_unknown("workspace generation counter overflow"),
        }
    }

    fn mark_unknown(&mut self, reason: impl Into<String>) {
        if self.diagnostic.is_none() {
            let reason = reason.into();
            eprintln!("workspace generation became unknown: {reason}; forceCollect=true");
            self.diagnostic = Some(reason);
        }
    }

    fn response(&self) -> SandboxWorkspaceGeneration {
        SandboxWorkspaceGeneration {
            schema: SANDBOX_WORKSPACE_GENERATION_SCHEMA.to_string(),
            generation: self.diagnostic.is_none().then(|| {
                centaeris_core::execution::ExecutionWorkspaceGenerationV1 {
                    instance_epoch: self.instance_epoch.clone(),
                    generation: self.generation,
                }
            }),
            diagnostic: self.diagnostic.clone(),
        }
    }
}

#[cfg(target_os = "linux")]
fn workspace_watch_loop() -> Result<(), String> {
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;

    let descriptor = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
    if descriptor < 0 {
        return Err(format!(
            "initialize workspace generation watcher failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    let mut inotify = unsafe { File::from_raw_fd(descriptor) };
    let mut watches = BTreeMap::new();
    add_workspace_watches(
        inotify.as_raw_fd(),
        Path::new(WORKSPACE_DATA_ROOT),
        &mut watches,
    )?;
    let root_watch = watches
        .iter()
        .find_map(|(watch, path)| (path == Path::new(WORKSPACE_DATA_ROOT)).then_some(*watch))
        .ok_or_else(|| "workspace root watcher is missing".to_string())?;

    let epoch = fs::read_to_string("/proc/sys/kernel/random/uuid")
        .map_err(|error| format!("read workspace watcher epoch failed: {error}"))?;
    let epoch = epoch.trim();
    if epoch.len() != 36
        || epoch.bytes().enumerate().any(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte != b'-'
            } else {
                !byte.is_ascii_hexdigit() || byte.is_ascii_uppercase()
            }
        })
    {
        return Err("workspace watcher epoch is invalid".to_string());
    }
    let mut state = WorkspaceWatchState {
        instance_epoch: epoch.to_string(),
        generation: 0,
        diagnostic: None,
    };

    let listener = UnixListener::bind(WORKSPACE_GENERATION_SOCKET)
        .map_err(|error| format!("bind workspace generation socket failed: {error}"))?;
    fs::set_permissions(
        WORKSPACE_GENERATION_SOCKET,
        fs::Permissions::from_mode(0o600),
    )
    .map_err(|error| format!("protect workspace generation socket failed: {error}"))?;
    listener
        .set_nonblocking(true)
        .map_err(|error| format!("configure workspace generation socket failed: {error}"))?;

    let mut watching = true;
    loop {
        let mut descriptors = [
            libc::pollfd {
                fd: if watching { inotify.as_raw_fd() } else { -1 },
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: listener.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let result = unsafe { libc::poll(descriptors.as_mut_ptr(), descriptors.len() as _, -1) };
        if result < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            state.mark_unknown(format!("poll workspace watcher failed: {error}"));
            watching = false;
            continue;
        }
        if descriptors[0].revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
            state.mark_unknown("workspace watcher descriptor failed");
            watching = false;
        } else if descriptors[0].revents & libc::POLLIN != 0 {
            drain_workspace_events(&mut inotify, root_watch, &mut watches, &mut state);
        }
        if descriptors[1].revents & libc::POLLIN == 0 {
            continue;
        }
        loop {
            let (mut stream, _) = match listener.accept() {
                Ok(stream) => stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) => {
                    state
                        .mark_unknown(format!("accept workspace generation query failed: {error}"));
                    break;
                }
            };
            if watching {
                drain_workspace_events(&mut inotify, root_watch, &mut watches, &mut state);
            }
            let response = serde_json::to_vec(&state.response())
                .map_err(|error| format!("encode workspace generation failed: {error}"))?;
            if let Err(error) = stream.write_all(response.as_slice()) {
                eprintln!("write workspace generation response failed: {error}");
            }
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn workspace_watch_loop() -> Result<(), String> {
    Err("workspace generation watcher requires Linux".to_string())
}

#[cfg(target_os = "linux")]
fn add_workspace_watches(
    descriptor: i32,
    directory: &Path,
    watches: &mut BTreeMap<i32, PathBuf>,
) -> Result<(), String> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let path = CString::new(directory.as_os_str().as_bytes())
        .map_err(|_| "workspace watcher path contains NUL".to_string())?;
    let mask = libc::IN_ATTRIB
        | libc::IN_CLOSE_WRITE
        | libc::IN_CREATE
        | libc::IN_DELETE
        | libc::IN_DELETE_SELF
        | libc::IN_MODIFY
        | libc::IN_MOVE_SELF
        | libc::IN_MOVED_FROM
        | libc::IN_MOVED_TO
        | libc::IN_UNMOUNT
        | libc::IN_ONLYDIR
        | libc::IN_DONT_FOLLOW;
    let watch = unsafe { libc::inotify_add_watch(descriptor, path.as_ptr(), mask) };
    if watch < 0 {
        let error = std::io::Error::last_os_error();
        // The parent watch already records a vanished descendant's change.
        // A missing root still fails the root-watch check during startup.
        if error.kind() == std::io::ErrorKind::NotFound {
            return Ok(());
        }
        return Err(format!("watch workspace directory failed: {error}"));
    }
    watches.insert(watch, directory.to_path_buf());
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("scan workspace watcher directory failed: {error}")),
    };
    for entry in entries {
        let entry =
            entry.map_err(|error| format!("scan workspace watcher entry failed: {error}"))?;
        let metadata = match fs::symlink_metadata(entry.path()) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(format!("inspect workspace watcher entry failed: {error}"));
            }
        };
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            add_workspace_watches(descriptor, entry.path().as_path(), watches)?;
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn drain_workspace_events(
    inotify: &mut File,
    root_watch: i32,
    watches: &mut BTreeMap<i32, PathBuf>,
    state: &mut WorkspaceWatchState,
) {
    use std::ffi::OsStr;
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;

    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = match inotify.read(&mut buffer) {
            Ok(0) => return,
            Ok(count) => count,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => {
                state.mark_unknown(format!("read workspace watcher failed: {error}"));
                return;
            }
        };
        let mut offset = 0;
        while offset < count {
            let header_size = std::mem::size_of::<libc::inotify_event>();
            if count - offset < header_size {
                state.mark_unknown("workspace watcher returned a truncated event");
                return;
            }
            let event = unsafe {
                std::ptr::read_unaligned(buffer.as_ptr().add(offset).cast::<libc::inotify_event>())
            };
            let record_size = match header_size.checked_add(event.len as usize) {
                Some(size) if size <= count - offset => size,
                _ => {
                    state.mark_unknown("workspace watcher returned an invalid event");
                    return;
                }
            };
            state.mark_change();
            if event.mask & libc::IN_Q_OVERFLOW != 0 {
                state.mark_unknown("workspace watcher queue overflow");
            }
            if event.wd == root_watch
                && event.mask
                    & (libc::IN_DELETE_SELF
                        | libc::IN_MOVE_SELF
                        | libc::IN_UNMOUNT
                        | libc::IN_IGNORED)
                    != 0
            {
                state.mark_unknown("workspace root watcher was replaced or removed");
            }
            let base = watches.get(&event.wd).cloned();
            if event.mask & libc::IN_IGNORED != 0 {
                watches.remove(&event.wd);
            }
            if event.mask & libc::IN_ISDIR != 0
                && event.mask & (libc::IN_CREATE | libc::IN_MOVED_TO) != 0
            {
                let name = &buffer[offset + header_size..offset + record_size];
                let name = &name[..name
                    .iter()
                    .position(|byte| *byte == 0)
                    .unwrap_or(name.len())];
                match base.filter(|_| !name.is_empty()) {
                    Some(base) => {
                        let path = base.join(OsStr::from_bytes(name));
                        if let Err(error) =
                            add_workspace_watches(inotify.as_raw_fd(), path.as_path(), watches)
                        {
                            state.mark_unknown(error);
                        }
                    }
                    None => {
                        state.mark_unknown("workspace watcher could not bind a new directory event")
                    }
                }
            }
            offset += record_size;
        }
    }
}

#[cfg(target_os = "linux")]
fn query_workspace_generation() -> Result<SandboxWorkspaceGeneration, String> {
    query_workspace_generation_when_quiescent(
        || {
            let processes = agent_process_ids()
                .map_err(|error| format!("verify workspace quiescence failed: {error}"))?;
            if processes.is_empty() {
                Ok(())
            } else {
                Err("workspace has active agent processes; collect must quiesce them".to_string())
            }
        },
        read_workspace_generation,
    )
}

#[cfg(any(target_os = "linux", test))]
fn query_workspace_generation_when_quiescent(
    mut require_quiescent: impl FnMut() -> Result<(), String>,
    read_generation: impl FnOnce() -> Result<SandboxWorkspaceGeneration, String>,
) -> Result<SandboxWorkspaceGeneration, String> {
    let unknown = |reason| SandboxWorkspaceGeneration {
        schema: SANDBOX_WORKSPACE_GENERATION_SCHEMA.to_string(),
        generation: None,
        diagnostic: Some(reason),
    };
    // inotify does not observe writes through a live MAP_SHARED mapping.
    // A user process must finish (and close its writable files) before its
    // generation can justify reusing a snapshot. Unknown remains query-local.
    if let Err(reason) = require_quiescent() {
        return Ok(unknown(reason));
    }
    let generation = read_generation()?;
    if let Err(reason) = require_quiescent() {
        return Ok(unknown(reason));
    }
    Ok(generation)
}

#[cfg(target_os = "linux")]
fn read_workspace_generation() -> Result<SandboxWorkspaceGeneration, String> {
    use std::os::unix::net::UnixStream;

    let stream = UnixStream::connect(WORKSPACE_GENERATION_SOCKET)
        .map_err(|error| format!("connect workspace generation watcher failed: {error}"))?;
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(2)))
        .map_err(|error| format!("configure workspace generation query failed: {error}"))?;
    let mut bytes = Vec::new();
    stream
        .take(MAX_JSON_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("read workspace generation failed: {error}"))?;
    if bytes.len() > MAX_JSON_BYTES {
        return Err("workspace generation exceeds its bounded size".to_string());
    }
    serde_json::from_slice(bytes.as_slice())
        .map_err(|error| format!("decode workspace generation failed: {error}"))
}

#[cfg(not(target_os = "linux"))]
fn query_workspace_generation() -> Result<SandboxWorkspaceGeneration, String> {
    Err("workspace generation watcher requires Linux".to_string())
}

#[cfg(target_os = "linux")]
fn quiesce_agent_processes() -> Result<(), String> {
    signal_agent_processes(libc::SIGTERM)?;
    if wait_for_agent_processes(1_000)? {
        return Ok(());
    }
    signal_agent_processes(libc::SIGKILL)?;
    if wait_for_agent_processes(1_000)? {
        Ok(())
    } else {
        Err("sandbox user processes did not quiesce".to_string())
    }
}

#[cfg(not(target_os = "linux"))]
fn quiesce_agent_processes() -> Result<(), String> {
    Err("sandbox helper requires Linux".to_string())
}

#[cfg(target_os = "linux")]
fn signal_agent_processes(signal: i32) -> Result<(), String> {
    for pid in agent_process_ids()? {
        if unsafe { libc::kill(pid, signal) } != 0
            && std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
        {
            return Err(format!(
                "signal sandbox user process failed: {}",
                std::io::Error::last_os_error()
            ));
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn wait_for_agent_processes(timeout_ms: u64) -> Result<bool, String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    loop {
        if agent_process_ids()?.is_empty() {
            return Ok(true);
        }
        if std::time::Instant::now() >= deadline {
            return Ok(false);
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[cfg(target_os = "linux")]
fn agent_process_ids() -> Result<Vec<i32>, String> {
    let mut result = Vec::new();
    for entry in fs::read_dir("/proc").map_err(|error| format!("read /proc failed: {error}"))? {
        let entry = entry.map_err(|error| format!("read /proc entry failed: {error}"))?;
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let Ok(pid) = name.parse::<i32>() else {
            continue;
        };
        let status = match fs::read_to_string(entry.path().join("status")) {
            Ok(value) => value,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("read sandbox process status failed: {error}")),
        };
        if is_other_agent_process(pid, std::process::id() as i32, status.as_str()) {
            result.push(pid);
        }
    }
    Ok(result)
}

#[cfg(not(target_os = "linux"))]
fn agent_process_ids() -> Result<Vec<i32>, String> {
    Err("agent process state requires Linux".to_string())
}

#[cfg(any(target_os = "linux", test))]
fn is_other_agent_process(pid: i32, current_pid: i32, status: &str) -> bool {
    pid != current_pid && is_agent_process_status(status)
}

#[cfg(any(target_os = "linux", test))]
fn is_agent_process_status(status: &str) -> bool {
    let is_agent = status
        .lines()
        .find_map(|line| line.strip_prefix("Uid:"))
        .and_then(|line| line.split_whitespace().next())
        == Some("10001");
    let is_zombie = status
        .lines()
        .find_map(|line| line.strip_prefix("State:"))
        .and_then(|line| line.split_whitespace().next())
        == Some("Z");
    is_agent && !is_zombie
}

#[cfg(target_os = "linux")]
fn require_agent() -> Result<(), String> {
    if unsafe { libc::geteuid() } != AGENT_UID {
        return Err("sandbox quiesce helper requires agent identity".to_string());
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn require_agent() -> Result<(), String> {
    Err("sandbox helper requires Linux".to_string())
}

fn read_json<T: DeserializeOwned>(max_bytes: usize) -> Result<T, String> {
    let bytes = read_stdin(max_bytes)?;
    serde_json::from_slice(bytes.as_slice())
        .map_err(|error| format!("decode sandbox helper request failed: {error}"))
}

fn read_stdin(max_bytes: usize) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    std::io::stdin()
        .take(max_bytes as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("read sandbox helper request failed: {error}"))?;
    if bytes.len() > max_bytes {
        return Err("sandbox helper request exceeds its bounded size".to_string());
    }
    Ok(bytes)
}

fn decode_frame<T: DeserializeOwned>(frame: &[u8]) -> Result<(T, &[u8]), String> {
    if frame.len() < 4 {
        return Err("sandbox helper frame is truncated".to_string());
    }
    let metadata_len = u32::from_be_bytes(
        frame[..4]
            .try_into()
            .map_err(|_| "sandbox helper frame is invalid".to_string())?,
    ) as usize;
    if metadata_len == 0 || metadata_len > MAX_JSON_BYTES || frame.len() < 4 + metadata_len {
        return Err("sandbox helper frame metadata is invalid".to_string());
    }
    let metadata = serde_json::from_slice(&frame[4..4 + metadata_len])
        .map_err(|error| format!("decode sandbox helper frame failed: {error}"))?;
    Ok((metadata, &frame[4 + metadata_len..]))
}

fn write_frame<T: serde::Serialize>(metadata: &T, bytes: &[u8]) -> Result<(), String> {
    let metadata = serde_json::to_vec(metadata)
        .map_err(|error| format!("encode sandbox helper frame failed: {error}"))?;
    let metadata_len = u32::try_from(metadata.len())
        .map_err(|_| "sandbox helper frame metadata is too large".to_string())?;
    let mut stdout = std::io::stdout().lock();
    stdout
        .write_all(&metadata_len.to_be_bytes())
        .and_then(|_| stdout.write_all(metadata.as_slice()))
        .and_then(|_| stdout.write_all(bytes))
        .map_err(|error| format!("write sandbox helper frame failed: {error}"))
}

fn load_inventory() -> Result<SandboxInputInventory, String> {
    let bytes = match fs::read(INPUT_INVENTORY_PATH) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(SandboxInputInventory::default())
        }
        Err(error) => return Err(format!("read sandbox input inventory failed: {error}")),
    };
    if bytes.len() > MAX_JSON_BYTES {
        return Err("sandbox input inventory exceeds its bounded size".to_string());
    }
    let inventory = serde_json::from_slice::<SandboxInputInventory>(bytes.as_slice())
        .map_err(|error| format!("decode sandbox input inventory failed: {error}"))?;
    validate_inventory(&inventory)?;
    Ok(inventory)
}

fn save_inventory(inventory: &SandboxInputInventory) -> Result<(), String> {
    validate_inventory(inventory)?;
    let bytes = serde_json::to_vec(inventory)
        .map_err(|error| format!("encode sandbox input inventory failed: {error}"))?;
    let temporary = Path::new(STATE_ROOT).join(format!(".inputs-{}.tmp", std::process::id()));
    let mut file = File::options()
        .write(true)
        .create_new(true)
        .open(temporary.as_path())
        .map_err(|error| format!("create sandbox input inventory failed: {error}"))?;
    let result = (|| {
        file.write_all(bytes.as_slice())
            .map_err(|error| format!("write sandbox input inventory failed: {error}"))?;
        file.sync_all()
            .map_err(|error| format!("sync sandbox input inventory failed: {error}"))?;
        fs::rename(temporary.as_path(), INPUT_INVENTORY_PATH)
            .map_err(|error| format!("install sandbox input inventory failed: {error}"))?;
        File::open(STATE_ROOT)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| format!("sync sandbox input inventory directory failed: {error}"))
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

fn validate_inventory(inventory: &SandboxInputInventory) -> Result<(), String> {
    if inventory.schema != SANDBOX_INPUT_INVENTORY_SCHEMA {
        return Err("sandbox input inventory schema mismatch".to_string());
    }
    let mut refs = HashSet::new();
    let mut paths = HashSet::new();
    let mut previous = None;
    for input in &inventory.inputs {
        validate_materialized_input(input)?;
        if previous.is_some_and(|value: &str| value >= input.input_ref.as_str())
            || !refs.insert(input.input_ref.as_str())
            || !paths.insert(input.virtual_path.as_str())
        {
            return Err("sandbox input inventory is not sorted and unique".to_string());
        }
        previous = Some(input.input_ref.as_str());
    }
    Ok(())
}

fn validate_materialized_input(input: &SandboxMaterializedInput) -> Result<(), String> {
    if input.input_ref.trim().is_empty()
        || input.input_ref.len() > 160
        || input.source_version.trim().is_empty()
        || input.size_bytes > MAX_EXECUTION_INPUT_BYTES
        || input.sha256.len() != 71
        || !input.sha256.starts_with("sha256:")
        || !input.sha256[7..]
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err("sandbox materialized input metadata is invalid".to_string());
    }
    normalized_data_path(input.virtual_path.as_str())?;
    Ok(())
}

fn normalized_data_path(path: &str) -> Result<String, String> {
    let relative = if path == WORKSPACE_DATA_ROOT || path == "." {
        return Ok(".".to_string());
    } else if let Some(relative) = path.strip_prefix("/mnt/data/") {
        relative
    } else if path.starts_with('/') {
        return Err("sandbox path escaped /mnt/data".to_string());
    } else {
        path
    };
    if relative.is_empty()
        || relative.contains('\\')
        || relative.chars().any(char::is_control)
        || relative
            .split('/')
            .any(|component| component.is_empty() || matches!(component, "." | ".."))
    {
        return Err("sandbox path contains an invalid component".to_string());
    }
    Ok(relative.to_string())
}

fn data_target(path: &str) -> Result<PathBuf, String> {
    let relative = normalized_data_path(path)?;
    if relative == "." {
        return Err("sandbox input cannot target /mnt/data root".to_string());
    }
    Ok(Path::new(WORKSPACE_DATA_ROOT).join(relative))
}

#[cfg(unix)]
fn require_root() -> Result<(), String> {
    if unsafe { libc::geteuid() } != 0 {
        return Err("sandbox input helper requires root".to_string());
    }
    Ok(())
}

#[cfg(not(unix))]
fn require_root() -> Result<(), String> {
    Err("sandbox helper requires Linux".to_string())
}

#[cfg(unix)]
fn secure_roots() -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;

    let data = fs::symlink_metadata(WORKSPACE_DATA_ROOT)
        .map_err(|error| format!("inspect /mnt/data failed: {error}"))?;
    let state = fs::symlink_metadata(STATE_ROOT)
        .map_err(|error| format!("inspect sandbox state root failed: {error}"))?;
    if !data.is_dir()
        || data.file_type().is_symlink()
        || data.uid() != 0
        || data.gid() != 0
        || data.mode() & 0o7777 != 0o1777
        || !state.is_dir()
        || state.file_type().is_symlink()
        || state.uid() != 0
        || state.mode() & 0o7777 != 0o700
    {
        return Err("sandbox data or state root permissions are invalid".to_string());
    }
    Ok(())
}

#[cfg(not(unix))]
fn secure_roots() -> Result<(), String> {
    Err("sandbox helper requires Linux".to_string())
}

#[cfg(unix)]
fn install_protected_input(target: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;

    let parent = target
        .parent()
        .ok_or_else(|| "sandbox input target has no parent".to_string())?;
    create_symlink_free_directories(Path::new(WORKSPACE_DATA_ROOT), parent)?;
    match fs::symlink_metadata(target) {
        Ok(_) => return Err("sandbox input target already exists".to_string()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("inspect sandbox input target failed: {error}")),
    }
    let staging = parent.join(format!(".centaeris-input-{}.tmp", std::process::id()));
    let mut file = File::options()
        .write(true)
        .create_new(true)
        .open(staging.as_path())
        .map_err(|error| format!("create sandbox input failed: {error}"))?;
    let result = (|| {
        file.write_all(bytes)
            .map_err(|error| format!("write sandbox input failed: {error}"))?;
        file.sync_all()
            .map_err(|error| format!("sync sandbox input failed: {error}"))?;
        fs::set_permissions(staging.as_path(), fs::Permissions::from_mode(0o444))
            .map_err(|error| format!("seal sandbox input failed: {error}"))?;
        rename_noreplace(staging.as_path(), target)?;
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| format!("sync sandbox input directory failed: {error}"))
    })();
    if result.is_err() {
        let _ = fs::remove_file(staging);
    }
    result
}

#[cfg(not(unix))]
fn install_protected_input(_target: &Path, _bytes: &[u8]) -> Result<(), String> {
    Err("sandbox helper requires Linux".to_string())
}

#[cfg(unix)]
fn create_symlink_free_directories(root: &Path, target: &Path) -> Result<(), String> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    if !target.starts_with(root) {
        return Err("sandbox input parent escaped /mnt/data".to_string());
    }
    let mut current = root.to_path_buf();
    for component in target
        .strip_prefix(root)
        .map_err(|_| "sandbox input parent escaped /mnt/data".to_string())?
        .components()
    {
        current.push(component);
        match fs::symlink_metadata(current.as_path()) {
            Ok(metadata)
                if metadata.is_dir()
                    && !metadata.file_type().is_symlink()
                    && metadata.uid() == 0
                    && metadata.gid() == 0
                    && metadata.mode() & 0o7777 == 0o755 => {}
            Ok(_) => return Err("sandbox input parent contains an unsupported node".to_string()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(current.as_path())
                    .map_err(|error| format!("create sandbox input parent failed: {error}"))?;
                fs::set_permissions(current.as_path(), fs::Permissions::from_mode(0o755))
                    .map_err(|error| format!("protect sandbox input parent failed: {error}"))?;
                let metadata = fs::symlink_metadata(current.as_path())
                    .map_err(|error| format!("inspect sandbox input parent failed: {error}"))?;
                if metadata.uid() != 0 || metadata.gid() != 0 {
                    return Err("sandbox input parent ownership is invalid".to_string());
                }
            }
            Err(error) => return Err(format!("inspect sandbox input parent failed: {error}")),
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn rename_noreplace(source: &Path, target: &Path) -> Result<(), String> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let source = CString::new(source.as_os_str().as_bytes())
        .map_err(|_| "sandbox input path contains NUL".to_string())?;
    let target = CString::new(target.as_os_str().as_bytes())
        .map_err(|_| "sandbox input path contains NUL".to_string())?;
    if unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            target.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    } == 0
    {
        Ok(())
    } else {
        Err(format!(
            "install sandbox input failed: {}",
            std::io::Error::last_os_error()
        ))
    }
}

#[cfg(all(unix, not(target_os = "linux")))]
fn rename_noreplace(_source: &Path, _target: &Path) -> Result<(), String> {
    Err("sandbox helper requires Linux renameat2".to_string())
}

#[cfg(unix)]
fn install_tombstone(target: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;

    let parent = target
        .parent()
        .ok_or_else(|| "sandbox input target has no parent".to_string())?;
    let staging = parent.join(format!(".centaeris-revoked-{}.tmp", std::process::id()));
    let file = File::options()
        .write(true)
        .create_new(true)
        .open(staging.as_path())
        .map_err(|error| format!("create sandbox input tombstone failed: {error}"))?;
    file.sync_all()
        .map_err(|error| format!("sync sandbox input tombstone failed: {error}"))?;
    fs::set_permissions(staging.as_path(), fs::Permissions::from_mode(0o000))
        .map_err(|error| format!("seal sandbox input tombstone failed: {error}"))?;
    fs::rename(staging.as_path(), target)
        .map_err(|error| format!("install sandbox input tombstone failed: {error}"))?;
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("sync sandbox input directory failed: {error}"))
}

#[cfg(not(unix))]
fn install_tombstone(_target: &Path) -> Result<(), String> {
    Err("sandbox helper requires Linux".to_string())
}

#[cfg(target_os = "linux")]
fn read_artifact(relative_path: &str, max_bytes: u64) -> Result<(Vec<u8>, String), String> {
    use std::os::unix::fs::MetadataExt;

    let mut file = open_artifact_beneath(relative_path)?;
    let before = file
        .metadata()
        .map_err(|_| "artifact source is unavailable".to_string())?;
    if !before.is_file() || before.nlink() != 1 || before.uid() != 10_001 {
        return Err("artifact source is unsafe".to_string());
    }
    if before.size() > max_bytes {
        return Err("artifact is too large".to_string());
    }
    // Publication content is buffered in memory with a 64 MiB ceiling.
    let mut bytes = Vec::with_capacity(before.size() as usize);
    Read::take(&mut file, max_bytes + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("read artifact failed: {error}"))?;
    if bytes.len() as u64 > max_bytes {
        return Err("artifact is too large".to_string());
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|error| format!("rewind artifact failed: {error}"))?;
    let after = file
        .metadata()
        .map_err(|_| "artifact source changed".to_string())?;
    if file_version(&before) != file_version(&after) || bytes.len() as u64 != before.size() {
        return Err("artifact source changed".to_string());
    }
    let sha256 = format!("sha256:{:x}", Sha256::digest(&bytes));
    Ok((bytes, sha256))
}

#[cfg(not(target_os = "linux"))]
fn read_artifact(_relative_path: &str, _max_bytes: u64) -> Result<(Vec<u8>, String), String> {
    Err("sandbox helper requires Linux".to_string())
}

#[cfg(target_os = "linux")]
fn open_artifact_beneath(relative_path: &str) -> Result<File, String> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

    let components = relative_path.split('/').collect::<Vec<_>>();
    if components.is_empty()
        || components
            .iter()
            .any(|component| component.is_empty() || matches!(*component, "." | ".."))
    {
        return Err("artifact source path is unsafe".to_string());
    }
    let root = CString::new(WORKSPACE_DATA_ROOT)
        .map_err(|_| "artifact data root is invalid".to_string())?;
    let root_fd = unsafe {
        libc::open(
            root.as_ptr(),
            libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if root_fd < 0 {
        return Err("artifact data root is unavailable".to_string());
    }
    let mut directory = unsafe { OwnedFd::from_raw_fd(root_fd) };
    for component in &components[..components.len() - 1] {
        let component =
            CString::new(*component).map_err(|_| "artifact source path is unsafe".to_string())?;
        let descriptor = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                component.as_ptr(),
                libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if descriptor < 0 {
            return Err("artifact source path is unsafe".to_string());
        }
        directory = unsafe { OwnedFd::from_raw_fd(descriptor) };
    }
    let filename = CString::new(components[components.len() - 1])
        .map_err(|_| "artifact source path is unsafe".to_string())?;
    let descriptor = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            filename.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if descriptor < 0 {
        return Err("artifact source is unavailable or unsafe".to_string());
    }
    Ok(unsafe { File::from_raw_fd(descriptor) })
}

#[cfg(target_os = "linux")]
fn file_version(metadata: &fs::Metadata) -> (u64, u64, u64, u64, i64, i64, i64, i64) {
    use std::os::unix::fs::MetadataExt;
    (
        metadata.dev(),
        metadata.ino(),
        metadata.nlink(),
        metadata.size(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virtual_paths_stay_beneath_data_root() {
        assert_eq!(
            normalized_data_path("reports/a.txt").unwrap(),
            "reports/a.txt"
        );
        assert_eq!(
            normalized_data_path("/mnt/data/reports/a.txt").unwrap(),
            "reports/a.txt"
        );
        assert!(normalized_data_path("../secret").is_err());
        assert!(normalized_data_path("a\\b").is_err());
    }

    #[test]
    fn filesystem_router_rejects_unknown_uri_scheme() {
        let error = run_file_system_request(SandboxFileSystemRequest {
            path: "banana://workspace/file.md".to_string(),
            operation: ExecutionFileSystemOperation::InspectMutationPath,
        })
        .expect_err("unknown URI scheme must loud-fail before workspace path handling");

        assert_eq!(error.kind, ExecutionFileSystemErrorKind::InvalidPath);
        assert_eq!(error.message, "filesystem URI scheme is unsupported");
    }

    #[test]
    fn read_root_system_catalog_mutations_rejected_before_dispatch() {
        for operation in [
            ExecutionFileSystemOperation::InspectMutationPath,
            ExecutionFileSystemOperation::WriteFile {
                content: vec![],
                observed_file_hash: None,
                create_only: true,
            },
            ExecutionFileSystemOperation::DeleteFile {
                expected_file_hash: format!("sha256:{}", "a".repeat(64)),
            },
        ] {
            let error = run_file_system_request(SandboxFileSystemRequest {
                path: SYSTEM_SKILL_ROOT.to_string(),
                operation,
            })
            .expect_err("catalog root must stay read-only");
            assert_eq!(error.kind, ExecutionFileSystemErrorKind::PermissionDenied);
            assert_eq!(error.message, "System Skill is read-only");
        }
    }

    #[test]
    fn read_root_projection_accepts_exact_catalog_root() {
        static NEXT_ROOT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let fixture = std::env::temp_dir().join(format!(
            "centaeris-readonly-catalog-{}-{}",
            std::process::id(),
            NEXT_ROOT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        ));
        fs::create_dir(&fixture).expect("create owned projection fixture");
        let root = fixture.to_string_lossy();
        let result = validate_read_only_path(root.as_ref(), root.as_ref(), "System Skill");
        fs::remove_dir(&fixture).expect("remove owned empty fixture");
        result.expect("exact projection root must be a valid read target");
        for path in [
            "/opt/centaeris",
            "/opt/centaeris/system-skills/",
            "/opt/centaeris/system-skills/../private",
            "/opt/centaeris/system-skills//memory",
        ] {
            assert_eq!(
                validate_read_only_path(path, SYSTEM_SKILL_ROOT, "System Skill")
                    .unwrap_err()
                    .kind,
                ExecutionFileSystemErrorKind::InvalidPath
            );
        }
    }

    #[test]
    fn plugin_paths_require_the_read_only_projection_shape() {
        assert!(
            validate_read_only_path("/opt/centaeris/plugins/../secret", PLUGIN_ROOT, "Plugin",)
                .is_err()
        );
        assert!(validate_read_only_path(
            "/opt/centaeris/plugins/banana\\SKILL.md",
            PLUGIN_ROOT,
            "Plugin",
        )
        .is_err());
    }

    #[test]
    fn snapshot_manifest_requires_canonical_non_overlapping_paths() {
        let hash = format!("sha256:{}", "a".repeat(64));
        let manifest = SandboxWorkspaceSnapshotManifest {
            schema: SANDBOX_WORKSPACE_SNAPSHOT_SCHEMA.to_string(),
            files: vec![SandboxWorkspaceSnapshotFile {
                path: "report.txt".to_string(),
                size_bytes: 0,
                sha256: hash.clone(),
                executable: false,
            }],
        };
        assert_eq!(
            String::from_utf8(canonical_snapshot_manifest_bytes(&manifest).expect("manifest"))
                .expect("utf8"),
            format!(
                "{{\"schema\":\"workspace.snapshot.v1\",\"files\":[{{\"path\":\"report.txt\",\"sizeBytes\":0,\"sha256\":\"{hash}\",\"executable\":false}}]}}"
            )
        );

        let mut overlapping = manifest.clone();
        overlapping.files.push(SandboxWorkspaceSnapshotFile {
            path: "report.txt/child".to_string(),
            size_bytes: 1,
            sha256: hash,
            executable: false,
        });
        assert!(validate_snapshot_manifest(&overlapping).is_err());
    }

    #[test]
    fn empty_collected_workspace_has_no_snapshot_frame() {
        let manifest = collected_snapshot_manifest(Vec::new()).expect("empty workspace");

        assert!(manifest.files.is_empty());
        assert!(canonical_snapshot_manifest_bytes(&manifest).is_err());
    }

    #[test]
    fn agent_process_observer_reports_live_count_without_altering_processes() {
        let processes = vec![41, 42];
        let calls = std::cell::Cell::new(0);
        let state = observe_agent_process_state(|| {
            calls.set(calls.get() + 1);
            Ok(processes.clone())
        })
        .unwrap();
        assert_eq!(state.active_process_count, 2);
        assert_eq!(processes, [41, 42]);
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn agent_process_observer_propagates_unknown_state_instead_of_claiming_idle() {
        assert_eq!(
            observe_agent_process_state(|| Err("process inventory unavailable".into())),
            Err("process inventory unavailable".into())
        );
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn agent_process_state_rejects_unsupported_platform_without_claiming_idle() {
        assert_eq!(
            run_agent_process_state_once(),
            Err("agent process state requires Linux".into())
        );
    }

    #[test]
    fn agent_process_state_has_exact_camel_case_nonnegative_count_contract() {
        let state = AgentProcessState {
            active_process_count: 2,
        };
        assert_eq!(
            serde_json::to_value(&state).unwrap(),
            serde_json::json!({"activeProcessCount": 2})
        );
        for invalid in [
            serde_json::json!({"active_process_count": 2}),
            serde_json::json!({"activeProcessCount": 2, "other": 1}),
            serde_json::json!({"activeProcessCount": -1}),
        ] {
            assert!(serde_json::from_value::<AgentProcessState>(invalid).is_err());
        }
        assert_eq!(
            serde_json::from_value::<AgentProcessState>(serde_json::json!({
                "activeProcessCount": 0
            }))
            .unwrap()
            .active_process_count,
            0
        );
    }

    #[test]
    fn agent_process_observer_excludes_itself_other_users_and_zombies() {
        let live = "Name:\thelper\nState:\tS (sleeping)\nUid:\t10001\t10001\t10001\t10001\n";
        let root = "Name:\twatcher\nState:\tS (sleeping)\nUid:\t0\t0\t0\t0\n";
        let zombie = "Name:\thelper\nState:\tZ (zombie)\nUid:\t10001\t10001\t10001\t10001\n";
        assert!(!is_other_agent_process(41, 41, live));
        assert!(is_other_agent_process(42, 41, live));
        assert!(!is_other_agent_process(42, 41, root));
        assert!(!is_other_agent_process(42, 41, zombie));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn agent_process_observer_keeps_a_real_agent_uid_child_alive() {
        use std::os::unix::process::CommandExt;
        if !matches!(unsafe { libc::geteuid() }, 0 | AGENT_UID) {
            eprintln!("real agent UID fixture requires root or agent UID");
            return;
        }
        struct OwnedChild(std::process::Child);
        impl Drop for OwnedChild {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let mut child = OwnedChild(
            std::process::Command::new("/bin/sleep")
                .arg("30")
                .gid(AGENT_GID)
                .uid(AGENT_UID)
                .spawn()
                .expect("start owned agent UID process"),
        );
        let state = observe_agent_process_state(agent_process_ids).unwrap();
        assert!(state.active_process_count >= 1);
        assert!(agent_process_ids()
            .unwrap()
            .contains(&(child.0.id() as i32)));
        assert!(child.0.try_wait().unwrap().is_none());
    }

    #[test]
    fn quiesce_recognizes_agent_process_status_only() {
        let agent = "Name:\tbash\nState:\tS (sleeping)\nUid:\t10001\t10001\t10001\t10001\n";
        let root = "Name:\tbash\nState:\tS (sleeping)\nUid:\t0\t0\t0\t0\n";
        let zombie = "Name:\tsleep\nState:\tZ (zombie)\nUid:\t10001\t10001\t10001\t10001\n";

        assert!(is_agent_process_status(agent));
        assert!(!is_agent_process_status(root));
        assert!(!is_agent_process_status(zombie));
    }

    #[test]
    fn workspace_generation_covers_write_revert_and_overflow() {
        let mut state = WorkspaceWatchState {
            instance_epoch: "watcher-1".to_string(),
            generation: 0,
            diagnostic: None,
        };

        state.mark_change(); // write
        state.mark_change(); // revert
        let changed = state.response();
        changed.validate().expect("trusted generation");
        assert_eq!(changed.generation.expect("known").generation, 2);

        state.mark_unknown("workspace watcher queue overflow");
        let overflow = state.response();
        overflow.validate().expect("explicit unknown generation");
        assert!(overflow.generation.is_none());
        assert_eq!(
            overflow.diagnostic.as_deref(),
            Some("workspace watcher queue overflow")
        );
        state.mark_change();
        assert!(state.response().generation.is_none());
    }

    #[test]
    fn workspace_generation_requires_quiescence_before_and_after_read_without_poisoning() {
        use std::cell::Cell;

        let state = WorkspaceWatchState {
            instance_epoch: "watcher-test".to_string(),
            generation: 7,
            diagnostic: None,
        };
        for blocked_check in [0, 1] {
            for reason in ["active agent process", "read /proc failed"] {
                let checks = Cell::new(0);
                let reads = Cell::new(0);
                let response = query_workspace_generation_when_quiescent(
                    || {
                        let index = checks.get();
                        checks.set(index + 1);
                        if index == blocked_check {
                            Err(reason.to_string())
                        } else {
                            Ok(())
                        }
                    },
                    || {
                        reads.set(reads.get() + 1);
                        Ok(state.response())
                    },
                )
                .expect("quiescence failure is an explicit query-local Unknown");
                response.validate().expect("valid Unknown");
                assert!(response.generation.is_none());
                assert_eq!(response.diagnostic.as_deref(), Some(reason));
                assert_eq!(checks.get(), blocked_check + 1);
                assert_eq!(reads.get(), blocked_check);
            }
        }

        let checks = Cell::new(0);
        let recovered = query_workspace_generation_when_quiescent(
            || {
                checks.set(checks.get() + 1);
                Ok(())
            },
            || Ok(state.response()),
        )
        .expect("quiescent foreground completion restores Known without restarting watcher");
        assert_eq!(checks.get(), 2);
        assert_eq!(recovered, state.response());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn workspace_generation_drains_vanished_directories_and_new_tree_writes() {
        use std::os::fd::{AsRawFd, FromRawFd};
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!(
            "centaeris-workspace-watch-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        let descriptor = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        assert!(descriptor >= 0);
        let mut inotify = unsafe { File::from_raw_fd(descriptor) };
        let mut watches = BTreeMap::new();
        add_workspace_watches(inotify.as_raw_fd(), &root, &mut watches).unwrap();
        let root_watch = *watches.keys().next().unwrap();
        let mut state = WorkspaceWatchState {
            instance_epoch: "watcher-test".to_string(),
            generation: 0,
            diagnostic: None,
        };

        // All events wait for drain: CREATE must not require a directory that
        // has already disappeared by the time the foreground tool exits.
        let vanished = root.join("ephemeral");
        fs::create_dir(&vanished).unwrap();
        fs::write(vanished.join("file"), b"temporary").unwrap();
        fs::remove_file(vanished.join("file")).unwrap();
        fs::remove_dir(&vanished).unwrap();
        drain_workspace_events(&mut inotify, root_watch, &mut watches, &mut state);
        assert!(state.diagnostic.is_none(), "{:?}", state.diagnostic);
        assert!(state.generation > 0);

        let nested = root.join("new/nested");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("file"), b"before drain").unwrap();
        let before_new_tree = state.generation;
        drain_workspace_events(&mut inotify, root_watch, &mut watches, &mut state);
        assert!(state.diagnostic.is_none(), "{:?}", state.diagnostic);
        assert!(state.generation > before_new_tree);
        assert!(watches.values().any(|path| path == &nested));

        let before_revert = state.generation;
        fs::write(nested.join("file"), b"changed").unwrap();
        fs::write(nested.join("file"), b"before drain").unwrap();
        drain_workspace_events(&mut inotify, root_watch, &mut watches, &mut state);
        assert!(state.diagnostic.is_none(), "{:?}", state.diagnostic);
        assert!(state.generation > before_revert);

        let link = root.join("replaced-directory");
        symlink(&nested, &link).unwrap();
        assert!(add_workspace_watches(inotify.as_raw_fd(), &link, &mut watches).is_err());
        assert!(!watches.values().any(|path| path == &link));

        fs::remove_dir_all(&root).unwrap();
        drain_workspace_events(&mut inotify, root_watch, &mut watches, &mut state);
        assert!(state.response().generation.is_none());
        assert_eq!(
            state.diagnostic.as_deref(),
            Some("workspace root watcher was replaced or removed")
        );
    }

    #[test]
    fn workspace_generation_rpc_serves_42_bounded_queries_and_fails_loudly() {
        let input = SANDBOX_WORKSPACE_GENERATION_QUERY_LINE.repeat(42);
        let mut output = Vec::new();
        let mut generation = 0_u64;
        workspace_generation_rpc_loop(input.as_slice(), &mut output, || {
            generation += 1;
            Ok(SandboxWorkspaceGeneration {
                schema: SANDBOX_WORKSPACE_GENERATION_SCHEMA.to_string(),
                generation: Some(centaeris_core::execution::ExecutionWorkspaceGenerationV1 {
                    instance_epoch: "watcher-test".to_string(),
                    generation,
                }),
                diagnostic: None,
            })
        })
        .expect("serve generation RPC queries");
        let responses = output
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice::<SandboxWorkspaceGeneration>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(responses.len(), 42);
        assert_eq!(
            responses[41]
                .generation
                .as_ref()
                .expect("known generation")
                .generation,
            42
        );

        let mut query_called = false;
        let error = workspace_generation_rpc_loop(b"{}\n".as_slice(), Vec::new(), || {
            query_called = true;
            Err("must not run".to_string())
        })
        .expect_err("non-canonical RPC request must fail");
        assert_eq!(error, "workspace generation RPC request is invalid");
        assert!(!query_called);
    }
}
