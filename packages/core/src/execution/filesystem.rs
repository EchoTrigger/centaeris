use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::execution::sandbox::SandboxPolicy;
use crate::execution::sandbox::{SandboxErr, SandboxTransformRequest};

use super::{
    ExecutionCancellationProbe, ExecutionHostCommandOutput, ExecutionHostKind, ExecutionHostMode,
    ExecutionHostRunner,
};

#[derive(Clone)]
pub struct ExecutionHostBinding {
    mode: ExecutionHostMode,
    runner: Arc<dyn ExecutionHostRunner>,
    cwd: PathBuf,
    policy: SandboxPolicy,
    operation_scope: Option<String>,
    cancellation_probe: Option<Arc<ExecutionCancellationProbe>>,
}

impl std::fmt::Debug for ExecutionHostBinding {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExecutionHostBinding")
            .field("execution_host_mode", &self.mode)
            .field("cwd", &self.cwd)
            .field("policy", &self.policy)
            .finish()
    }
}

impl ExecutionHostBinding {
    pub fn new(
        mode: ExecutionHostMode,
        runner: Arc<dyn ExecutionHostRunner>,
        cwd: PathBuf,
        policy: SandboxPolicy,
    ) -> Result<Self, String> {
        let cwd = match mode {
            ExecutionHostMode::Local => {
                let canonical = cwd
                    .canonicalize()
                    .map_err(|error| format!("canonicalize working directory failed: {error}"))?;
                if !canonical.is_dir() {
                    return Err(format!(
                        "working directory is not a directory: {}",
                        canonical.display()
                    ));
                }
                canonical
            }
            ExecutionHostMode::Remote => {
                if !cwd.is_absolute() {
                    return Err("remote working directory must be absolute".to_string());
                }
                cwd
            }
        };
        Ok(Self {
            mode,
            runner,
            cwd,
            policy,
            operation_scope: None,
            cancellation_probe: None,
        })
    }

    pub fn mode(&self) -> ExecutionHostMode {
        self.mode
    }

    pub fn kind(&self) -> ExecutionHostKind {
        self.runner.kind()
    }

    pub fn bash_description(&self) -> &'static str {
        self.runner.bash_description()
    }

    pub fn cwd(&self) -> &Path {
        self.cwd.as_path()
    }

    pub(crate) fn policy(&self) -> &SandboxPolicy {
        &self.policy
    }

    pub fn run_file_system_operation(
        &self,
        model_path: impl Into<String>,
        operation: ExecutionFileSystemOperation,
    ) -> Result<ExecutionFileSystemOutput, ExecutionFileSystemError> {
        let model_path = model_path.into();
        validate_execution_model_path(model_path.as_str())?;
        self.runner
            .run_file_system_operation(ExecutionFileSystemRequest {
                operation_id: self
                    .stable_operation_id("filesystem", &(model_path.as_str(), &operation)),
                cwd: self.cwd.clone(),
                policy: self.policy.clone(),
                model_path,
                operation,
            })
    }

    pub fn run_command(
        &self,
        program: String,
        args: Vec<String>,
        env: std::collections::HashMap<String, String>,
        timeout_ms: u64,
    ) -> Result<ExecutionHostCommandOutput, SandboxErr> {
        let stable_env = env.iter().collect::<std::collections::BTreeMap<_, _>>();
        let operation_id = self.stable_operation_id(
            "process",
            &(program.as_str(), args.as_slice(), stable_env, timeout_ms),
        );
        self.runner.run_host_command(
            operation_id.as_deref(),
            SandboxTransformRequest {
                program,
                args,
                cwd: self.cwd.clone(),
                env,
                timeout_ms,
                policy: self.policy.clone(),
            },
            self.cancellation_probe.as_deref(),
        )
    }

    pub(crate) fn with_policy(&self, policy: SandboxPolicy) -> Self {
        Self {
            mode: self.mode,
            runner: self.runner.clone(),
            cwd: self.cwd.clone(),
            policy,
            operation_scope: self.operation_scope.clone(),
            cancellation_probe: self.cancellation_probe.clone(),
        }
    }

    pub(crate) fn with_operation_scope(&self, operation_scope: Option<String>) -> Self {
        Self {
            mode: self.mode,
            runner: self.runner.clone(),
            cwd: self.cwd.clone(),
            policy: self.policy.clone(),
            operation_scope,
            cancellation_probe: self.cancellation_probe.clone(),
        }
    }

    pub(crate) fn with_cancellation_probe(
        self,
        cancellation_probe: Option<Arc<ExecutionCancellationProbe>>,
    ) -> Self {
        Self {
            cancellation_probe,
            ..self
        }
    }

    pub(crate) fn with_cwd(&self, cwd: PathBuf, policy: SandboxPolicy) -> Result<Self, String> {
        let binding = Self::new(self.mode, self.runner.clone(), cwd, policy)?;
        Ok(Self {
            operation_scope: self.operation_scope.clone(),
            cancellation_probe: self.cancellation_probe.clone(),
            ..binding
        })
    }

    #[cfg(test)]
    pub(crate) fn new_test_local(cwd: PathBuf, policy: SandboxPolicy) -> Result<Self, String> {
        let runner = Arc::new(
            super::TestExecutionHostRunner::new(None)
                .map_err(|error| error.internal_debug_message())?,
        );
        Self::new(ExecutionHostMode::Local, runner, cwd, policy)
    }

    fn stable_operation_id<T: Serialize>(&self, kind: &str, payload: &T) -> Option<String> {
        let scope = self.operation_scope.as_deref()?;
        let encoded = serde_json::to_vec(&(scope, kind, payload)).ok()?;
        Some(format!("op_{:x}", Sha256::digest(encoded)))
    }
}

fn validate_execution_model_path(model_path: &str) -> Result<(), ExecutionFileSystemError> {
    let trimmed = model_path.trim();
    if trimmed.is_empty() {
        return Err(ExecutionFileSystemError::new(
            ExecutionFileSystemErrorKind::InvalidPath,
            "tool path cannot be empty",
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExecutionFileSystemRequest {
    pub operation_id: Option<String>,
    pub cwd: PathBuf,
    pub policy: SandboxPolicy,
    pub model_path: String,
    pub operation: ExecutionFileSystemOperation,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "type",
    deny_unknown_fields
)]
pub enum ExecutionFileSystemOperation {
    InspectMutationPath,
    ReadFile {
        max_bytes: usize,
    },
    ListDirectory {
        recursive: bool,
        max_entries: usize,
    },
    WriteFile {
        content: Vec<u8>,
        expected_file_hash: Option<String>,
        create_only: bool,
    },
    DeleteFile {
        expected_file_hash: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExecutionFileIdentity {
    pub key: String,
    pub display_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExecutionFileReadOutput {
    pub identity: ExecutionFileIdentity,
    pub bytes: Vec<u8>,
    pub file_hash: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionPathKind {
    File,
    Directory,
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExecutionPathInspectionOutput {
    pub identity: ExecutionFileIdentity,
    pub exists: bool,
    pub kind: Option<ExecutionPathKind>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionDirectoryEntryKind {
    File,
    Directory,
}

impl ExecutionDirectoryEntryKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Directory => "directory",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExecutionDirectoryEntry {
    pub path: String,
    pub kind: ExecutionDirectoryEntryKind,
    pub size_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExecutionDirectoryListOutput {
    pub identity: ExecutionFileIdentity,
    pub entries: Vec<ExecutionDirectoryEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExecutionFileWriteOutput {
    pub identity: ExecutionFileIdentity,
    pub previous_file_hash: Option<String>,
    pub file_hash: String,
    pub created: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExecutionFileDeleteOutput {
    pub identity: ExecutionFileIdentity,
    pub previous_file_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionFileSystemOutput {
    InspectMutationPath(ExecutionPathInspectionOutput),
    ReadFile(ExecutionFileReadOutput),
    ListDirectory(ExecutionDirectoryListOutput),
    WriteFile(ExecutionFileWriteOutput),
    DeleteFile(ExecutionFileDeleteOutput),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionFileSystemErrorKind {
    InvalidPath,
    AssetRemoved,
    AccessRevoked,
    SourceDeleted,
    StaleGeneration,
    NotFound,
    NotFile,
    NotDirectory,
    UnsupportedEntry,
    TooLarge,
    PermissionDenied,
    Conflict,
    HostUnavailable,
    Io,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExecutionFileSystemError {
    pub kind: ExecutionFileSystemErrorKind,
    pub message: String,
    pub diagnostic: Option<String>,
}

impl ExecutionFileSystemError {
    pub fn new(kind: ExecutionFileSystemErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            diagnostic: None,
        }
    }

    pub fn with_diagnostic(mut self, diagnostic: impl Into<String>) -> Self {
        self.diagnostic = Some(diagnostic.into());
        self
    }
}

impl std::fmt::Display for ExecutionFileSystemError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.message)?;
        if let Some(diagnostic) = self.diagnostic.as_deref() {
            write!(formatter, " ({diagnostic})")?;
        }
        Ok(())
    }
}

impl std::error::Error for ExecutionFileSystemError {}

#[derive(Clone, Copy)]
enum FileSystemScopeMode {
    Direct,
    WorkingDirectory,
    Policy,
}

enum FileSystemScope {
    Direct,
    WorkingDirectory,
    Policy {
        allowed_roots: Vec<PathBuf>,
        denied_paths: Vec<PathBuf>,
    },
}

impl FileSystemScope {
    fn new(
        mode: FileSystemScopeMode,
        policy: &SandboxPolicy,
        cwd: &Path,
        mutation: bool,
    ) -> Result<Self, ExecutionFileSystemError> {
        match mode {
            FileSystemScopeMode::Direct => Ok(Self::Direct),
            FileSystemScopeMode::WorkingDirectory => Ok(Self::WorkingDirectory),
            FileSystemScopeMode::Policy => {
                let allowed = if mutation {
                    policy.filesystem.writable_roots.iter().collect::<Vec<_>>()
                } else {
                    policy
                        .filesystem
                        .read_only_roots
                        .iter()
                        .chain(policy.filesystem.writable_roots.iter())
                        .collect::<Vec<_>>()
                };
                let denied = if mutation {
                    &policy.filesystem.denied_write_paths
                } else {
                    &policy.filesystem.denied_read_paths
                };
                let allowed_roots = allowed
                    .into_iter()
                    .map(|path| canonical_policy_path(path.as_path(), "allowed root"))
                    .collect::<Result<Vec<_>, _>>()?;
                let denied_paths = denied
                    .iter()
                    .map(|path| canonical_policy_path(path.as_path(), "denied path"))
                    .collect::<Result<Vec<_>, _>>()?;
                let scope = Self::Policy {
                    allowed_roots,
                    denied_paths,
                };
                if !scope.allows(cwd, cwd) {
                    return Err(ExecutionFileSystemError::new(
                        ExecutionFileSystemErrorKind::PermissionDenied,
                        "working directory is outside the sandbox filesystem policy",
                    ));
                }
                Ok(scope)
            }
        }
    }

    fn allows(&self, cwd: &Path, path: &Path) -> bool {
        match self {
            Self::Direct => true,
            Self::WorkingDirectory => path.starts_with(cwd),
            Self::Policy {
                allowed_roots,
                denied_paths,
            } => {
                allowed_roots.iter().any(|root| path.starts_with(root))
                    && !denied_paths.iter().any(|denied| path.starts_with(denied))
            }
        }
    }

    fn denied_error(&self, model_path: &str) -> ExecutionFileSystemError {
        let (kind, message) = match self {
            Self::Policy { .. } => (
                ExecutionFileSystemErrorKind::PermissionDenied,
                format!(
                    "sandbox policy denied filesystem path: {}",
                    model_path.trim()
                ),
            ),
            _ => (
                ExecutionFileSystemErrorKind::InvalidPath,
                format!(
                    "tool path escaped the working directory: {}",
                    model_path.trim()
                ),
            ),
        };
        ExecutionFileSystemError::new(kind, message)
    }
}

fn canonical_policy_path(path: &Path, label: &str) -> Result<PathBuf, ExecutionFileSystemError> {
    let mut ancestor = path.to_path_buf();
    let mut missing = Vec::new();
    while !ancestor.exists() {
        let name = ancestor.file_name().ok_or_else(|| {
            ExecutionFileSystemError::new(
                ExecutionFileSystemErrorKind::HostUnavailable,
                format!("sandbox {label} is unavailable: {}", path.display()),
            )
        })?;
        missing.push(name.to_os_string());
        if !ancestor.pop() {
            return Err(ExecutionFileSystemError::new(
                ExecutionFileSystemErrorKind::HostUnavailable,
                format!("sandbox {label} is unavailable: {}", path.display()),
            ));
        }
    }
    let mut canonical = ancestor.canonicalize().map_err(|error| {
        ExecutionFileSystemError::new(
            ExecutionFileSystemErrorKind::HostUnavailable,
            format!("sandbox {label} is unavailable: {}", path.display()),
        )
        .with_diagnostic(error.to_string())
    })?;
    for component in missing.into_iter().rev() {
        canonical.push(component);
    }
    Ok(canonical)
}

pub fn run_direct_execution_file_system_operation(
    request: ExecutionFileSystemRequest,
) -> Result<ExecutionFileSystemOutput, ExecutionFileSystemError> {
    run_execution_file_system_operation(request, FileSystemScopeMode::Direct)
}

pub fn run_scoped_execution_file_system_operation(
    request: ExecutionFileSystemRequest,
) -> Result<ExecutionFileSystemOutput, ExecutionFileSystemError> {
    run_execution_file_system_operation(request, FileSystemScopeMode::WorkingDirectory)
}

pub fn run_policy_scoped_execution_file_system_operation(
    request: ExecutionFileSystemRequest,
) -> Result<ExecutionFileSystemOutput, ExecutionFileSystemError> {
    run_execution_file_system_operation(request, FileSystemScopeMode::Policy)
}

fn run_execution_file_system_operation(
    request: ExecutionFileSystemRequest,
    scope_mode: FileSystemScopeMode,
) -> Result<ExecutionFileSystemOutput, ExecutionFileSystemError> {
    let cwd = request.cwd.canonicalize().map_err(|error| {
        ExecutionFileSystemError::new(
            ExecutionFileSystemErrorKind::HostUnavailable,
            "working directory is unavailable",
        )
        .with_diagnostic(format!(
            "canonicalize working directory {} failed: {error}",
            request.cwd.display()
        ))
    })?;
    if !cwd.is_dir() {
        return Err(ExecutionFileSystemError::new(
            ExecutionFileSystemErrorKind::HostUnavailable,
            "working directory is not a directory",
        ));
    }
    let mutation = matches!(
        request.operation,
        ExecutionFileSystemOperation::InspectMutationPath
            | ExecutionFileSystemOperation::WriteFile { .. }
            | ExecutionFileSystemOperation::DeleteFile { .. }
    );
    let scope = FileSystemScope::new(scope_mode, &request.policy, cwd.as_path(), mutation)?;
    match request.operation {
        ExecutionFileSystemOperation::InspectMutationPath => {
            inspect_mutation_path(cwd.as_path(), request.model_path.as_str(), &scope)
                .map(ExecutionFileSystemOutput::InspectMutationPath)
        }
        ExecutionFileSystemOperation::ReadFile { max_bytes } => read_file(
            cwd.as_path(),
            request.model_path.as_str(),
            max_bytes,
            &scope,
        )
        .map(ExecutionFileSystemOutput::ReadFile),
        ExecutionFileSystemOperation::ListDirectory {
            recursive,
            max_entries,
        } => list_directory(
            cwd.as_path(),
            request.model_path.as_str(),
            recursive,
            max_entries,
            &scope,
        )
        .map(ExecutionFileSystemOutput::ListDirectory),
        ExecutionFileSystemOperation::WriteFile {
            content,
            expected_file_hash,
            create_only,
        } => write_file(
            cwd.as_path(),
            request.model_path.as_str(),
            content.as_slice(),
            expected_file_hash.as_deref(),
            create_only,
            &scope,
        )
        .map(ExecutionFileSystemOutput::WriteFile),
        ExecutionFileSystemOperation::DeleteFile { expected_file_hash } => delete_file(
            cwd.as_path(),
            request.model_path.as_str(),
            expected_file_hash.as_str(),
            &scope,
        )
        .map(ExecutionFileSystemOutput::DeleteFile),
    }
}

fn inspect_mutation_path(
    cwd: &Path,
    model_path: &str,
    scope: &FileSystemScope,
) -> Result<ExecutionPathInspectionOutput, ExecutionFileSystemError> {
    let resolved = resolve_mutation_path(cwd, model_path, scope)?;
    let kind = if resolved.path.is_file() {
        Some(ExecutionPathKind::File)
    } else if resolved.path.is_dir() {
        Some(ExecutionPathKind::Directory)
    } else if resolved.path.exists() {
        Some(ExecutionPathKind::Other)
    } else {
        None
    };
    Ok(ExecutionPathInspectionOutput {
        identity: resolved.identity(),
        exists: kind.is_some(),
        kind,
    })
}

fn read_file(
    cwd: &Path,
    model_path: &str,
    max_bytes: usize,
    scope: &FileSystemScope,
) -> Result<ExecutionFileReadOutput, ExecutionFileSystemError> {
    if max_bytes == 0 {
        return Err(ExecutionFileSystemError::new(
            ExecutionFileSystemErrorKind::InvalidPath,
            "read maxBytes must be positive",
        ));
    }
    let resolved = resolve_existing_path(cwd, model_path, scope)?;
    if !resolved.path.is_file() {
        return Err(ExecutionFileSystemError::new(
            ExecutionFileSystemErrorKind::NotFile,
            format!("read target is not a file: {}", resolved.display_path),
        ));
    }
    let metadata = fs::metadata(resolved.path.as_path())
        .map_err(|error| io_error("inspect file", model_path, error))?;
    if metadata.len() > max_bytes as u64 {
        return Err(ExecutionFileSystemError::new(
            ExecutionFileSystemErrorKind::TooLarge,
            format!(
                "read target exceeds the {max_bytes} byte limit: {}",
                resolved.display_path
            ),
        ));
    }
    let bytes = fs::read(resolved.path.as_path())
        .map_err(|error| io_error("read file", model_path, error))?;
    Ok(ExecutionFileReadOutput {
        identity: resolved.identity(),
        file_hash: sha256_bytes(bytes.as_slice()),
        bytes,
    })
}

fn list_directory(
    cwd: &Path,
    model_path: &str,
    recursive: bool,
    max_entries: usize,
    scope: &FileSystemScope,
) -> Result<ExecutionDirectoryListOutput, ExecutionFileSystemError> {
    if max_entries == 0 {
        return Err(ExecutionFileSystemError::new(
            ExecutionFileSystemErrorKind::InvalidPath,
            "directory maxEntries must be positive",
        ));
    }
    let resolved = resolve_existing_path(cwd, model_path, scope)?;
    if !resolved.path.is_dir() {
        return Err(ExecutionFileSystemError::new(
            ExecutionFileSystemErrorKind::NotDirectory,
            format!("list target is not a directory: {}", resolved.display_path),
        ));
    }
    let mut pending = VecDeque::from([resolved.path.clone()]);
    let mut entries = Vec::new();
    while let Some(directory) = pending.pop_front() {
        let mut children = fs::read_dir(directory.as_path())
            .map_err(|error| io_error("read directory", model_path, error))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| io_error("read directory entry", model_path, error))?;
        children.sort_by_key(|entry| entry.file_name());
        for child in children {
            if entries.len() >= max_entries {
                return Err(ExecutionFileSystemError::new(
                    ExecutionFileSystemErrorKind::TooLarge,
                    format!("directory exceeds the bounded scan limit of {max_entries} entries"),
                ));
            }
            let path = child.path();
            let metadata = fs::symlink_metadata(path.as_path())
                .map_err(|error| io_error("inspect directory entry", model_path, error))?;
            if metadata.file_type().is_symlink() {
                return Err(ExecutionFileSystemError::new(
                    ExecutionFileSystemErrorKind::UnsupportedEntry,
                    "directory contains a symbolic link",
                ));
            }
            let display_path = display_path(cwd, path.as_path());
            if metadata.file_type().is_dir() {
                entries.push(ExecutionDirectoryEntry {
                    path: display_path,
                    kind: ExecutionDirectoryEntryKind::Directory,
                    size_bytes: None,
                });
                if recursive {
                    pending.push_back(path);
                }
            } else if metadata.file_type().is_file() {
                entries.push(ExecutionDirectoryEntry {
                    path: display_path,
                    kind: ExecutionDirectoryEntryKind::File,
                    size_bytes: Some(metadata.len()),
                });
            } else {
                return Err(ExecutionFileSystemError::new(
                    ExecutionFileSystemErrorKind::UnsupportedEntry,
                    "directory contains an unsupported entry type",
                ));
            }
        }
    }
    Ok(ExecutionDirectoryListOutput {
        identity: resolved.identity(),
        entries,
    })
}

fn write_file(
    cwd: &Path,
    model_path: &str,
    content: &[u8],
    expected_file_hash: Option<&str>,
    create_only: bool,
    scope: &FileSystemScope,
) -> Result<ExecutionFileWriteOutput, ExecutionFileSystemError> {
    if create_only && expected_file_hash.is_some() {
        return Err(ExecutionFileSystemError::new(
            ExecutionFileSystemErrorKind::InvalidPath,
            "create-only writes cannot include an expected file hash",
        ));
    }
    let resolved = resolve_mutation_path(cwd, model_path, scope)?;
    let existed = resolved.path.exists();
    if existed && !resolved.path.is_file() {
        return Err(ExecutionFileSystemError::new(
            ExecutionFileSystemErrorKind::NotFile,
            format!("write target is not a file: {}", resolved.display_path),
        ));
    }
    if create_only && existed {
        return Err(ExecutionFileSystemError::new(
            ExecutionFileSystemErrorKind::Conflict,
            format!("write target already exists: {}", resolved.display_path),
        ));
    }
    let previous_file_hash = if existed {
        let bytes = fs::read(resolved.path.as_path())
            .map_err(|error| io_error("read file before write", model_path, error))?;
        Some(sha256_bytes(bytes.as_slice()))
    } else {
        None
    };
    if previous_file_hash.as_deref() != expected_file_hash {
        return Err(ExecutionFileSystemError::new(
            ExecutionFileSystemErrorKind::Conflict,
            format!(
                "write target changed before mutation: {}",
                resolved.display_path
            ),
        ));
    }
    let parent = resolved.path.parent().ok_or_else(|| {
        ExecutionFileSystemError::new(
            ExecutionFileSystemErrorKind::InvalidPath,
            "write target has no parent directory",
        )
    })?;
    fs::create_dir_all(parent)
        .map_err(|error| io_error("create parent directory", model_path, error))?;
    fs::write(resolved.path.as_path(), content)
        .map_err(|error| io_error("write file", model_path, error))?;
    Ok(ExecutionFileWriteOutput {
        identity: resolved.identity(),
        previous_file_hash,
        file_hash: sha256_bytes(content),
        created: !existed,
    })
}

fn delete_file(
    cwd: &Path,
    model_path: &str,
    expected_file_hash: &str,
    scope: &FileSystemScope,
) -> Result<ExecutionFileDeleteOutput, ExecutionFileSystemError> {
    if expected_file_hash.trim().is_empty() {
        return Err(ExecutionFileSystemError::new(
            ExecutionFileSystemErrorKind::InvalidPath,
            "delete expectedFileHash must not be empty",
        ));
    }
    let resolved = resolve_existing_path(cwd, model_path, scope)?;
    if !resolved.path.is_file() {
        return Err(ExecutionFileSystemError::new(
            ExecutionFileSystemErrorKind::NotFile,
            format!("delete target is not a file: {}", resolved.display_path),
        ));
    }
    let bytes = fs::read(resolved.path.as_path())
        .map_err(|error| io_error("read file before delete", model_path, error))?;
    let previous_file_hash = sha256_bytes(bytes.as_slice());
    if previous_file_hash != expected_file_hash {
        return Err(ExecutionFileSystemError::new(
            ExecutionFileSystemErrorKind::Conflict,
            format!(
                "delete target changed before mutation: {}",
                resolved.display_path
            ),
        ));
    }
    fs::remove_file(resolved.path.as_path())
        .map_err(|error| io_error("delete file", model_path, error))?;
    Ok(ExecutionFileDeleteOutput {
        identity: resolved.identity(),
        previous_file_hash,
    })
}

struct ResolvedPath {
    path: PathBuf,
    key: String,
    display_path: String,
}

impl ResolvedPath {
    fn identity(&self) -> ExecutionFileIdentity {
        ExecutionFileIdentity {
            key: self.key.clone(),
            display_path: self.display_path.clone(),
        }
    }
}

fn resolve_existing_path(
    cwd: &Path,
    model_path: &str,
    scope: &FileSystemScope,
) -> Result<ResolvedPath, ExecutionFileSystemError> {
    let candidate = model_path_candidate(cwd, model_path)?;
    let canonical = candidate
        .canonicalize()
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => ExecutionFileSystemError::new(
                ExecutionFileSystemErrorKind::NotFound,
                format!("path does not exist: {}", model_path.trim()),
            ),
            std::io::ErrorKind::PermissionDenied => ExecutionFileSystemError::new(
                ExecutionFileSystemErrorKind::PermissionDenied,
                format!(
                    "permission denied while resolving path: {}",
                    model_path.trim()
                ),
            ),
            _ => io_error("resolve path", model_path, error),
        })?;
    ensure_path_scope(cwd, canonical.as_path(), model_path, scope)?;
    Ok(resolved_path(cwd, canonical))
}

fn resolve_mutation_path(
    cwd: &Path,
    model_path: &str,
    scope: &FileSystemScope,
) -> Result<ResolvedPath, ExecutionFileSystemError> {
    let candidate = model_path_candidate(cwd, model_path)?;
    let mut ancestor = candidate.clone();
    let mut missing = Vec::new();
    while !ancestor.exists() {
        let name = ancestor.file_name().ok_or_else(|| {
            ExecutionFileSystemError::new(
                ExecutionFileSystemErrorKind::InvalidPath,
                format!("cannot resolve mutation path: {}", model_path.trim()),
            )
        })?;
        missing.push(name.to_os_string());
        if !ancestor.pop() {
            return Err(ExecutionFileSystemError::new(
                ExecutionFileSystemErrorKind::InvalidPath,
                format!("cannot resolve mutation path: {}", model_path.trim()),
            ));
        }
    }
    let mut canonical = ancestor
        .canonicalize()
        .map_err(|error| io_error("resolve mutation path", model_path, error))?;
    for component in missing.into_iter().rev() {
        canonical.push(component);
    }
    ensure_path_scope(cwd, canonical.as_path(), model_path, scope)?;
    Ok(resolved_path(cwd, canonical))
}

fn ensure_path_scope(
    cwd: &Path,
    canonical_path: &Path,
    model_path: &str,
    scope: &FileSystemScope,
) -> Result<(), ExecutionFileSystemError> {
    if scope.allows(cwd, canonical_path) {
        return Ok(());
    }
    Err(scope.denied_error(model_path))
}

fn model_path_candidate(cwd: &Path, model_path: &str) -> Result<PathBuf, ExecutionFileSystemError> {
    let trimmed = model_path.trim();
    if trimmed.is_empty() {
        return Err(ExecutionFileSystemError::new(
            ExecutionFileSystemErrorKind::InvalidPath,
            "tool path cannot be empty",
        ));
    }
    validate_execution_model_path(trimmed)?;
    let path = PathBuf::from(trimmed);
    Ok(if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    })
}

fn resolved_path(cwd: &Path, path: PathBuf) -> ResolvedPath {
    ResolvedPath {
        key: path.to_string_lossy().to_string(),
        display_path: display_path(cwd, path.as_path()),
        path,
    }
}

fn display_path(cwd: &Path, path: &Path) -> String {
    if path == cwd {
        return ".".to_string();
    }
    path.strip_prefix(cwd)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn io_error(operation: &str, model_path: &str, error: std::io::Error) -> ExecutionFileSystemError {
    let kind = match error.kind() {
        std::io::ErrorKind::NotFound => ExecutionFileSystemErrorKind::NotFound,
        std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::ReadOnlyFilesystem => {
            ExecutionFileSystemErrorKind::PermissionDenied
        }
        _ => ExecutionFileSystemErrorKind::Io,
    };
    ExecutionFileSystemError::new(kind, format!("{operation} failed: {}", model_path.trim()))
        .with_diagnostic(error.to_string())
}

fn sha256_bytes(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::{ExecutionHostHealth, ExecutionHostRunner, ExecutionHostStatus};
    use std::collections::HashMap;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct CancellationAwareRunner;

    #[test]
    fn read_only_filesystem_is_a_permission_denial() {
        let error = io_error(
            "write file",
            "banana",
            std::io::Error::from(std::io::ErrorKind::ReadOnlyFilesystem),
        );

        assert_eq!(error.kind, ExecutionFileSystemErrorKind::PermissionDenied);
    }

    impl ExecutionHostRunner for CancellationAwareRunner {
        fn status(&self, _policy: &SandboxPolicy) -> Result<ExecutionHostStatus, SandboxErr> {
            Ok(ExecutionHostStatus::remote(
                crate::execution::sandbox::SandboxType::OciContainer,
                ExecutionHostHealth::Ready,
                None,
            ))
        }

        fn run_file_system_operation(
            &self,
            _request: ExecutionFileSystemRequest,
        ) -> Result<ExecutionFileSystemOutput, ExecutionFileSystemError> {
            unreachable!("filesystem is not used by this check")
        }

        fn run_host_command(
            &self,
            operation_id: Option<&str>,
            _req: SandboxTransformRequest,
            cancellation_probe: Option<&ExecutionCancellationProbe>,
        ) -> Result<ExecutionHostCommandOutput, SandboxErr> {
            assert!(operation_id.is_some());
            let reason = cancellation_probe.expect("cancellation probe")()
                .expect("probe result")
                .expect("cancellation reason");
            Err(SandboxErr::Unavailable {
                reason,
                sandbox_type: None,
            })
        }
    }

    fn temp_directory(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "centaeris-execution-fs-{label}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(path.as_path()).expect("create temp directory");
        path
    }

    fn request(
        cwd: &Path,
        model_path: impl Into<String>,
        operation: ExecutionFileSystemOperation,
    ) -> ExecutionFileSystemRequest {
        ExecutionFileSystemRequest {
            operation_id: None,
            cwd: cwd.to_path_buf(),
            policy: SandboxPolicy::workspace_write_no_network(cwd),
            model_path: model_path.into(),
            operation,
        }
    }

    #[test]
    fn filesystem_operation_protocol_uses_camel_case_variant_fields() {
        let value = serde_json::to_value(ExecutionFileSystemOperation::WriteFile {
            content: vec![1],
            expected_file_hash: None,
            create_only: true,
        })
        .expect("serialize operation");
        assert_eq!(value["type"], "writeFile");
        assert!(value.get("expectedFileHash").is_some());
        assert_eq!(value["createOnly"], true);
        assert!(value.get("expected_file_hash").is_none());
        assert!(
            serde_json::from_value::<ExecutionFileSystemOperation>(serde_json::json!({
                "type": "writeFile",
                "content": [1],
                "expected_file_hash": null,
                "create_only": true
            }))
            .is_err()
        );

        let request = request(
            Path::new("/workspace"),
            "README.md",
            ExecutionFileSystemOperation::ReadFile { max_bytes: 1024 },
        );
        let mut request_value = serde_json::to_value(&request).expect("serialize request");
        assert!(request_value.get("operationId").is_some());
        assert!(request_value.get("modelPath").is_some());
        assert!(request_value.get("policy").is_some());
        assert!(request_value.get("model_path").is_none());
        request_value["policy"]["banana"] = serde_json::Value::Bool(true);
        assert!(serde_json::from_value::<ExecutionFileSystemRequest>(request_value).is_err());
    }

    #[test]
    fn command_binding_forwards_runtime_cancellation_probe() {
        let cwd = temp_directory("cancellation-probe");
        let binding = ExecutionHostBinding::new(
            ExecutionHostMode::Remote,
            Arc::new(CancellationAwareRunner),
            cwd.clone(),
            SandboxPolicy::workspace_write_no_network(cwd.clone()),
        )
        .expect("execution binding")
        .with_operation_scope(Some("call_1".to_string()))
        .with_cancellation_probe(Some(Arc::new(|| {
            Ok(Some("agent_run_cancel_requested".to_string()))
        })));

        let error = binding
            .run_command("bash".to_string(), vec![], HashMap::new(), 1_000)
            .expect_err("test runner returns the forwarded cancellation reason");

        let SandboxErr::Unavailable { reason, .. } = error else {
            panic!("expected unavailable error");
        };
        assert_eq!(reason, "agent_run_cancel_requested");
        fs::remove_dir_all(cwd).expect("remove fixture");
    }

    #[test]
    fn direct_file_system_resolves_relative_paths_from_cwd() {
        let cwd = temp_directory("relative");
        fs::write(cwd.join("hello.txt"), b"hello").expect("write fixture");

        let output = run_direct_execution_file_system_operation(request(
            cwd.as_path(),
            "hello.txt",
            ExecutionFileSystemOperation::ReadFile { max_bytes: 1024 },
        ))
        .expect("read relative path");
        let ExecutionFileSystemOutput::ReadFile(output) = output else {
            panic!("expected read output");
        };

        assert_eq!(output.bytes, b"hello");
        assert_eq!(output.identity.display_path, "hello.txt");
        assert!(Path::new(output.identity.key.as_str()).is_absolute());
        fs::remove_dir_all(cwd).expect("remove fixture");
    }

    #[test]
    fn direct_file_system_accepts_parent_and_absolute_paths() {
        let parent = temp_directory("outside");
        let cwd = parent.join("workspace");
        fs::create_dir_all(cwd.as_path()).expect("create cwd");
        let outside = parent.join("outside.txt");
        fs::write(outside.as_path(), b"outside").expect("write outside fixture");

        for model_path in [
            "../outside.txt".to_string(),
            outside.to_string_lossy().to_string(),
        ] {
            let output = run_direct_execution_file_system_operation(request(
                cwd.as_path(),
                model_path,
                ExecutionFileSystemOperation::ReadFile { max_bytes: 1024 },
            ))
            .expect("read path outside cwd");
            let ExecutionFileSystemOutput::ReadFile(output) = output else {
                panic!("expected read output");
            };
            assert_eq!(output.bytes, b"outside");
        }

        fs::remove_dir_all(parent).expect("remove fixture");
    }

    #[test]
    fn scoped_file_system_rejects_parent_and_absolute_paths() {
        let parent = temp_directory("scoped-outside");
        let cwd = parent.join("workspace");
        fs::create_dir_all(cwd.as_path()).expect("create cwd");
        let outside = parent.join("outside.txt");
        fs::write(outside.as_path(), b"outside").expect("write outside fixture");

        for model_path in [
            "../outside.txt".to_string(),
            outside.to_string_lossy().to_string(),
        ] {
            let error = run_scoped_execution_file_system_operation(request(
                cwd.as_path(),
                model_path,
                ExecutionFileSystemOperation::ReadFile { max_bytes: 1024 },
            ))
            .expect_err("scoped filesystem must reject a path outside cwd");
            assert_eq!(error.kind, ExecutionFileSystemErrorKind::InvalidPath);
        }

        fs::remove_dir_all(parent).expect("remove fixture");
    }

    #[test]
    fn policy_scoped_file_system_enforces_denials_and_explicit_root_extensions() {
        let parent = temp_directory("policy-scoped");
        let cwd = parent.join("workspace");
        let additional = parent.join("additional");
        fs::create_dir_all(cwd.join(".centaeris")).expect("create protected directory");
        fs::create_dir_all(&additional).expect("create additional root");
        fs::write(cwd.join(".centaeris/secret.txt"), b"secret").expect("write secret");
        fs::write(additional.join("allowed.txt"), b"allowed").expect("write additional file");
        fs::write(parent.join("outside.txt"), b"outside").expect("write outside file");

        let ExecutionFileSystemOutput::ReadFile(metadata) =
            run_policy_scoped_execution_file_system_operation(request(
                cwd.as_path(),
                ".centaeris/secret.txt",
                ExecutionFileSystemOperation::ReadFile { max_bytes: 1024 },
            ))
            .expect("workspace metadata must remain readable")
        else {
            panic!("expected read output");
        };
        assert_eq!(metadata.bytes, b"secret");

        let ExecutionFileSystemOutput::WriteFile(_) =
            run_policy_scoped_execution_file_system_operation(request(
                cwd.as_path(),
                ".centaeris/secret.txt",
                ExecutionFileSystemOperation::WriteFile {
                    content: b"tampered".to_vec(),
                    expected_file_hash: Some(metadata.file_hash),
                    create_only: false,
                },
            ))
            .expect("workspace metadata must remain writable")
        else {
            panic!("expected write output");
        };
        assert_eq!(
            fs::read(cwd.join(".centaeris/secret.txt")).expect("read updated metadata"),
            b"tampered"
        );

        let outside_read = run_policy_scoped_execution_file_system_operation(request(
            cwd.as_path(),
            "../outside.txt",
            ExecutionFileSystemOperation::ReadFile { max_bytes: 1024 },
        ))
        .expect_err("policy-scoped filesystem must reject outside reads");
        assert_eq!(
            outside_read.kind,
            ExecutionFileSystemErrorKind::PermissionDenied
        );

        let mut additional_request = request(
            cwd.as_path(),
            additional.join("allowed.txt").to_string_lossy(),
            ExecutionFileSystemOperation::ReadFile { max_bytes: 1024 },
        );
        additional_request
            .policy
            .filesystem
            .read_only_roots
            .push(additional);
        let ExecutionFileSystemOutput::ReadFile(output) =
            run_policy_scoped_execution_file_system_operation(additional_request)
                .expect("explicit read-only root must remain extensible")
        else {
            panic!("expected read output");
        };
        assert_eq!(output.bytes, b"allowed");

        fs::remove_dir_all(parent).expect("remove fixture");
    }

    #[cfg(unix)]
    #[test]
    fn scoped_file_system_rejects_symlink_escape_for_all_operation_kinds() {
        use std::os::unix::fs::symlink;

        let parent = temp_directory("scoped-symlink");
        let cwd = parent.join("workspace");
        let outside_directory = parent.join("outside");
        fs::create_dir_all(cwd.as_path()).expect("create cwd");
        fs::create_dir_all(outside_directory.as_path()).expect("create outside directory");
        fs::write(outside_directory.join("outside.txt"), b"outside")
            .expect("write outside fixture");
        symlink(
            outside_directory.join("outside.txt"),
            cwd.join("escape.txt"),
        )
        .expect("create file symlink");
        symlink(outside_directory.as_path(), cwd.join("escape-dir"))
            .expect("create directory symlink");

        let operations = [
            (
                "escape.txt",
                ExecutionFileSystemOperation::ReadFile { max_bytes: 1024 },
            ),
            (
                "escape-dir",
                ExecutionFileSystemOperation::ListDirectory {
                    recursive: false,
                    max_entries: 16,
                },
            ),
            (
                "escape-dir/new.txt",
                ExecutionFileSystemOperation::WriteFile {
                    content: b"new".to_vec(),
                    expected_file_hash: None,
                    create_only: true,
                },
            ),
            (
                "escape.txt",
                ExecutionFileSystemOperation::DeleteFile {
                    expected_file_hash: sha256_bytes(b"outside"),
                },
            ),
        ];
        for (model_path, operation) in operations {
            let error = run_scoped_execution_file_system_operation(request(
                cwd.as_path(),
                model_path,
                operation,
            ))
            .expect_err("scoped filesystem must reject a symlink escape");
            assert_eq!(error.kind, ExecutionFileSystemErrorKind::InvalidPath);
        }
        assert_eq!(
            fs::read(outside_directory.join("outside.txt")).expect("read outside fixture"),
            b"outside"
        );
        assert!(!outside_directory.join("new.txt").exists());

        fs::remove_dir_all(parent).expect("remove fixture");
    }

    #[test]
    fn direct_file_system_keeps_hash_validation_runtime_owned() {
        let cwd = temp_directory("hash");
        let path = cwd.join("state.txt");
        fs::write(path.as_path(), b"before").expect("write fixture");

        let read = run_direct_execution_file_system_operation(request(
            cwd.as_path(),
            "state.txt",
            ExecutionFileSystemOperation::ReadFile { max_bytes: 1024 },
        ))
        .expect("read fixture");
        let ExecutionFileSystemOutput::ReadFile(read) = read else {
            panic!("expected read output");
        };

        let write = run_direct_execution_file_system_operation(request(
            cwd.as_path(),
            "state.txt",
            ExecutionFileSystemOperation::WriteFile {
                content: b"after".to_vec(),
                expected_file_hash: Some(read.file_hash),
                create_only: false,
            },
        ))
        .expect("guarded write");
        let ExecutionFileSystemOutput::WriteFile(write) = write else {
            panic!("expected write output");
        };
        assert_eq!(
            write.previous_file_hash.as_deref(),
            Some("sha256:6db7d803e74f1ffa7d8f5adc0bf95b3e15bf4c8373fffadf546227cc6c6742cb")
        );
        assert_eq!(fs::read(path).expect("read result"), b"after");
        fs::remove_dir_all(cwd).expect("remove fixture");
    }

    #[test]
    fn stale_hash_rejects_mutation() {
        let cwd = temp_directory("stale");
        fs::write(cwd.join("state.txt"), b"current").expect("write fixture");

        let error = run_direct_execution_file_system_operation(request(
            cwd.as_path(),
            "state.txt",
            ExecutionFileSystemOperation::WriteFile {
                content: b"after".to_vec(),
                expected_file_hash: Some("sha256:banana".to_string()),
                create_only: false,
            },
        ))
        .expect_err("stale hash must fail");

        assert_eq!(error.kind, ExecutionFileSystemErrorKind::Conflict);
        fs::remove_dir_all(cwd).expect("remove fixture");
    }
}
