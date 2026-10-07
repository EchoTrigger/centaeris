#[cfg(target_os = "linux")]
use std::fs;
#[cfg(target_os = "linux")]
use std::fs::{File, OpenOptions};
#[cfg(target_os = "linux")]
use std::io::Write;
#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd;
#[cfg(target_os = "linux")]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(target_os = "linux")]
use std::path::Path;
use std::path::PathBuf;
#[cfg(target_os = "linux")]
use std::sync::atomic::{AtomicU64, Ordering};

use centaeris_core::execution::{
    run_scoped_execution_file_system_operation, ExecutionDirectoryEntry, ExecutionFileIdentity,
    ExecutionFileSystemError, ExecutionFileSystemErrorKind, ExecutionFileSystemOperation,
    ExecutionFileSystemOutput, ExecutionFileSystemRequest, ExecutionFileWriteOutput,
};
#[cfg(target_os = "linux")]
use sha2::{Digest, Sha256};

use crate::protocol::SandboxFileSystemRequest;

pub const MEMORY_CONTAINER_ROOT: &str = "/var/lib/centaeris/memory";
pub const MEMORY_URI_ROOT: &str = "plastic-memories://self/";
const MEMORY_TOPICS_URI_ROOT: &str = "plastic-memories://self/topics/";
const MEMORY_LOCK_FILE: &str = ".memory.lock";
const MEMORY_TEMP_PREFIX: &str = ".memory-write-";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryPath {
    Root,
    Index,
    Topics,
    Topic(String),
}

impl MemoryPath {
    pub fn parse(value: &str) -> Result<Self, String> {
        if value != value.trim()
            || value.contains(['%', '?', '#', '\\'])
            || value.chars().any(char::is_control)
        {
            return Err("plastic_memories_uri_invalid".to_string());
        }
        match value {
            MEMORY_URI_ROOT => Ok(Self::Root),
            "plastic-memories://self/MEMORY.md" => Ok(Self::Index),
            MEMORY_TOPICS_URI_ROOT => Ok(Self::Topics),
            _ => {
                let filename = value
                    .strip_prefix(MEMORY_TOPICS_URI_ROOT)
                    .ok_or_else(|| "plastic_memories_uri_invalid".to_string())?;
                let slug = filename
                    .strip_suffix(".md")
                    .ok_or_else(|| "plastic_memories_uri_invalid".to_string())?;
                if !valid_topic_slug(slug) {
                    return Err("plastic_memories_uri_invalid".to_string());
                }
                Ok(Self::Topic(slug.to_string()))
            }
        }
    }

    pub fn uri(&self) -> String {
        match self {
            Self::Root => MEMORY_URI_ROOT.to_string(),
            Self::Index => "plastic-memories://self/MEMORY.md".to_string(),
            Self::Topics => MEMORY_TOPICS_URI_ROOT.to_string(),
            Self::Topic(slug) => format!("{MEMORY_TOPICS_URI_ROOT}{slug}.md"),
        }
    }

    fn relative_path(&self) -> PathBuf {
        match self {
            Self::Root => PathBuf::from("."),
            Self::Index => PathBuf::from("MEMORY.md"),
            Self::Topics => PathBuf::from("topics"),
            Self::Topic(slug) => PathBuf::from("topics").join(format!("{slug}.md")),
        }
    }

    pub fn is_file(&self) -> bool {
        matches!(self, Self::Index | Self::Topic(_))
    }
}

pub fn is_memory_uri(value: &str) -> bool {
    value.starts_with("plastic-memories:")
}

pub fn run_memory_file_system_operation(
    request: SandboxFileSystemRequest,
) -> Result<ExecutionFileSystemOutput, ExecutionFileSystemError> {
    let memory_path = MemoryPath::parse(request.path.as_str()).map_err(|_| invalid_uri())?;
    let uri = memory_path.uri();
    let operation = request.operation;
    let result = match operation {
        ExecutionFileSystemOperation::WriteFile {
            content,
            observed_file_hash,
            create_only,
        } if memory_path.is_file() => atomic_write(
            &memory_path,
            content.as_slice(),
            observed_file_hash.as_deref(),
            create_only,
        )
        .map(ExecutionFileSystemOutput::WriteFile),
        ExecutionFileSystemOperation::WriteFile { .. }
        | ExecutionFileSystemOperation::DeleteFile { .. } => Err(ExecutionFileSystemError::new(
            ExecutionFileSystemErrorKind::PermissionDenied,
            "Agent Memory only permits writes to MEMORY.md or topics/<lower-kebab>.md",
        )),
        operation => run_scoped_execution_file_system_operation(ExecutionFileSystemRequest {
            operation_id: None,
            cwd: PathBuf::from(MEMORY_CONTAINER_ROOT),
            policy: centaeris_core::execution::ExecutionPolicy::workspace_write_no_network(
                MEMORY_CONTAINER_ROOT,
            ),
            model_path: memory_path.relative_path().to_string_lossy().to_string(),
            operation,
        }),
    };
    result
        .and_then(|output| remap_output(output, &memory_path))
        .map_err(|error| sanitize_error(error, uri.as_str()))
}

fn valid_topic_slug(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.split('-').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        })
}

fn remap_output(
    output: ExecutionFileSystemOutput,
    memory_path: &MemoryPath,
) -> Result<ExecutionFileSystemOutput, ExecutionFileSystemError> {
    let identity = memory_identity(memory_path);
    Ok(match output {
        ExecutionFileSystemOutput::InspectMutationPath(mut output) => {
            output.identity = identity;
            ExecutionFileSystemOutput::InspectMutationPath(output)
        }
        ExecutionFileSystemOutput::ReadFile(mut output) => {
            output.identity = identity;
            ExecutionFileSystemOutput::ReadFile(output)
        }
        ExecutionFileSystemOutput::ListDirectory(mut output) => {
            output.identity = identity;
            output.entries = output
                .entries
                .into_iter()
                .filter_map(remap_entry)
                .collect::<Result<Vec<_>, _>>()?;
            ExecutionFileSystemOutput::ListDirectory(output)
        }
        ExecutionFileSystemOutput::WriteFile(mut output) => {
            output.identity = identity;
            ExecutionFileSystemOutput::WriteFile(output)
        }
        ExecutionFileSystemOutput::DeleteFile(_) => {
            return Err(ExecutionFileSystemError::new(
                ExecutionFileSystemErrorKind::PermissionDenied,
                "Agent Memory deletion is unsupported",
            ))
        }
    })
}

fn remap_entry(
    mut entry: ExecutionDirectoryEntry,
) -> Option<Result<ExecutionDirectoryEntry, ExecutionFileSystemError>> {
    let filename = entry.path.rsplit('/').next().unwrap_or_default();
    if filename == MEMORY_LOCK_FILE || filename.starts_with(MEMORY_TEMP_PREFIX) {
        return None;
    }
    let path = match entry.path.as_str() {
        "MEMORY.md" => MemoryPath::Index,
        "topics" => MemoryPath::Topics,
        value => {
            let Some(filename) = value.strip_prefix("topics/") else {
                return Some(Err(unsupported_entry()));
            };
            let Some(slug) = filename.strip_suffix(".md") else {
                return Some(Err(unsupported_entry()));
            };
            if !valid_topic_slug(slug) {
                return Some(Err(unsupported_entry()));
            }
            MemoryPath::Topic(slug.to_string())
        }
    };
    entry.path = path.uri();
    Some(Ok(entry))
}

fn memory_identity(path: &MemoryPath) -> ExecutionFileIdentity {
    let uri = path.uri();
    ExecutionFileIdentity {
        key: uri.clone(),
        display_path: uri,
    }
}

#[cfg(target_os = "linux")]
fn atomic_write(
    memory_path: &MemoryPath,
    content: &[u8],
    expected_file_hash: Option<&str>,
    create_only: bool,
) -> Result<ExecutionFileWriteOutput, ExecutionFileSystemError> {
    let root = memory_root()?;
    atomic_write_at(
        root.as_path(),
        memory_path,
        content,
        expected_file_hash,
        create_only,
    )
}

#[cfg(target_os = "linux")]
fn atomic_write_at(
    root: &Path,
    memory_path: &MemoryPath,
    content: &[u8],
    expected_file_hash: Option<&str>,
    create_only: bool,
) -> Result<ExecutionFileWriteOutput, ExecutionFileSystemError> {
    static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

    if create_only && expected_file_hash.is_some() {
        return Err(ExecutionFileSystemError::new(
            ExecutionFileSystemErrorKind::InvalidPath,
            "create-only Memory writes cannot include an expected file hash",
        ));
    }
    let lock_path = root.join(MEMORY_LOCK_FILE);
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(lock_path)
        .map_err(|error| io_error("open Agent Memory mutation lock", error))?;
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(io_error(
            "lock Agent Memory mutation",
            std::io::Error::last_os_error(),
        ));
    }
    let result = (|| {
        let target = root.join(memory_path.relative_path());
        validate_mutation_target(root, target.as_path())?;
        let existed = match fs::symlink_metadata(target.as_path()) {
            Ok(metadata) if metadata.file_type().is_file() => true,
            Ok(_) => {
                return Err(ExecutionFileSystemError::new(
                    ExecutionFileSystemErrorKind::NotFile,
                    "Agent Memory write target is not a regular file",
                ))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => return Err(io_error("inspect Agent Memory target", error)),
        };
        if create_only && existed {
            return Err(ExecutionFileSystemError::new(
                ExecutionFileSystemErrorKind::Conflict,
                "Agent Memory write target already exists",
            ));
        }
        let previous_file_hash = if existed {
            Some(sha256(
                fs::read(target.as_path())
                    .map_err(|error| io_error("read Agent Memory before write", error))?
                    .as_slice(),
            ))
        } else {
            None
        };
        if previous_file_hash.as_deref() != expected_file_hash {
            return Err(ExecutionFileSystemError::new(
                ExecutionFileSystemErrorKind::Conflict,
                "Agent Memory changed before mutation",
            ));
        }
        let parent = target.parent().ok_or_else(invalid_uri)?;
        let temporary = (0..16)
            .find_map(|_| {
                let path = parent.join(format!(
                    "{MEMORY_TEMP_PREFIX}{}-{}.tmp",
                    std::process::id(),
                    NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
                ));
                match OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(path.as_path())
                {
                    Ok(file) => Some(Ok((path, file))),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => None,
                    Err(error) => Some(Err(io_error("create Agent Memory temporary file", error))),
                }
            })
            .transpose()?
            .ok_or_else(|| {
                ExecutionFileSystemError::new(
                    ExecutionFileSystemErrorKind::Io,
                    "Agent Memory temporary file names are exhausted",
                )
            })?;
        install_atomic(
            temporary.0.as_path(),
            temporary.1,
            target.as_path(),
            content,
        )?;
        Ok(ExecutionFileWriteOutput {
            identity: memory_identity(memory_path),
            created: !existed,
        })
    })();
    let unlock = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_UN) };
    if unlock != 0 && result.is_ok() {
        return Err(io_error(
            "unlock Agent Memory mutation",
            std::io::Error::last_os_error(),
        ));
    }
    result
}

#[cfg(target_os = "linux")]
fn install_atomic(
    temporary: &Path,
    mut file: File,
    target: &Path,
    content: &[u8],
) -> Result<(), ExecutionFileSystemError> {
    let result = (|| {
        file.write_all(content)
            .map_err(|error| io_error("write Agent Memory temporary file", error))?;
        file.sync_all()
            .map_err(|error| io_error("sync Agent Memory temporary file", error))?;
        fs::rename(temporary, target)
            .map_err(|error| io_error("install Agent Memory file", error))?;
        File::open(target.parent().ok_or_else(invalid_uri)?)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| io_error("sync Agent Memory directory", error))
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

#[cfg(not(target_os = "linux"))]
fn atomic_write(
    _memory_path: &MemoryPath,
    _content: &[u8],
    _expected_file_hash: Option<&str>,
    _create_only: bool,
) -> Result<ExecutionFileWriteOutput, ExecutionFileSystemError> {
    Err(ExecutionFileSystemError::new(
        ExecutionFileSystemErrorKind::HostUnavailable,
        "Agent Memory writes require the Linux hosted ExecutionHost",
    ))
}

#[cfg(target_os = "linux")]
fn memory_root() -> Result<PathBuf, ExecutionFileSystemError> {
    let root = Path::new(MEMORY_CONTAINER_ROOT);
    let metadata =
        fs::symlink_metadata(root).map_err(|error| io_error("inspect Agent Memory root", error))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(ExecutionFileSystemError::new(
            ExecutionFileSystemErrorKind::HostUnavailable,
            "Agent Memory root is invalid",
        ));
    }
    root.canonicalize()
        .map_err(|error| io_error("resolve Agent Memory root", error))
}

#[cfg(target_os = "linux")]
fn validate_mutation_target(root: &Path, target: &Path) -> Result<(), ExecutionFileSystemError> {
    let parent = target.parent().ok_or_else(invalid_uri)?;
    let expected_parent = if target.file_name().and_then(|name| name.to_str()) == Some("MEMORY.md")
    {
        root.to_path_buf()
    } else {
        root.join("topics")
    };
    let metadata = fs::symlink_metadata(parent)
        .map_err(|error| io_error("inspect Agent Memory parent", error))?;
    let canonical = parent
        .canonicalize()
        .map_err(|error| io_error("resolve Agent Memory parent", error))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() || canonical != expected_parent {
        return Err(ExecutionFileSystemError::new(
            ExecutionFileSystemErrorKind::PermissionDenied,
            "Agent Memory target escaped its fixed tree",
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn sha256(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn sanitize_error(error: ExecutionFileSystemError, uri: &str) -> ExecutionFileSystemError {
    let message = match error.kind {
        ExecutionFileSystemErrorKind::NotFound => {
            format!("Agent Memory path does not exist: {uri}")
        }
        ExecutionFileSystemErrorKind::NotFile => format!("Agent Memory path is not a file: {uri}"),
        ExecutionFileSystemErrorKind::NotDirectory => {
            format!("Agent Memory path is not a directory: {uri}")
        }
        _ => error.message,
    };
    ExecutionFileSystemError::new(error.kind, message)
}

fn invalid_uri() -> ExecutionFileSystemError {
    ExecutionFileSystemError::new(
        ExecutionFileSystemErrorKind::InvalidPath,
        "plastic-memories URI is invalid",
    )
}

fn unsupported_entry() -> ExecutionFileSystemError {
    ExecutionFileSystemError::new(
        ExecutionFileSystemErrorKind::UnsupportedEntry,
        "Agent Memory contains an unsupported entry",
    )
}

#[cfg(target_os = "linux")]
fn io_error(operation: &str, error: std::io::Error) -> ExecutionFileSystemError {
    let kind = match error.kind() {
        std::io::ErrorKind::NotFound => ExecutionFileSystemErrorKind::NotFound,
        std::io::ErrorKind::PermissionDenied => ExecutionFileSystemErrorKind::PermissionDenied,
        _ => ExecutionFileSystemErrorKind::Io,
    };
    ExecutionFileSystemError::new(kind, format!("{operation} failed"))
        .with_diagnostic(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_parser_accepts_only_the_agent_private_memory_tree() {
        assert_eq!(MemoryPath::parse(MEMORY_URI_ROOT), Ok(MemoryPath::Root));
        assert_eq!(
            MemoryPath::parse("plastic-memories://self/MEMORY.md"),
            Ok(MemoryPath::Index)
        );
        assert_eq!(
            MemoryPath::parse("plastic-memories://self/topics/banana.md"),
            Ok(MemoryPath::Topic("banana".to_string()))
        );
        for invalid in [
            "plastic-memories://other/MEMORY.md",
            "plastic-memories://self/topics",
            "plastic-memories://self/topics/Bad.md",
            "plastic-memories://self/topics/a--b.md",
            "plastic-memories://self/topics/%2e%2e.md",
            "plastic-memories://self/topics/a.md?banana=1",
            "plastic-memories://self/../MEMORY.md",
        ] {
            assert!(MemoryPath::parse(invalid).is_err(), "{invalid}");
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn memory_write_is_atomic_private_and_hash_guarded() {
        use std::os::unix::fs::PermissionsExt;
        use std::time::{SystemTime, UNIX_EPOCH};

        let root = std::env::temp_dir().join(format!(
            "centaeris-memory-write-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        fs::create_dir_all(root.join("topics")).expect("Memory root");
        let memory_path = MemoryPath::Index;
        let first = atomic_write_at(root.as_path(), &memory_path, b"first", None, true)
            .expect("create Memory");
        assert!(first.created);
        assert_eq!(fs::read(root.join("MEMORY.md")).expect("read"), b"first");
        assert_eq!(
            fs::metadata(root.join("MEMORY.md"))
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );

        let left_root = root.clone();
        let right_root = root.clone();
        let expected = sha256(b"first");
        let left_expected = expected.clone();
        let left = std::thread::spawn(move || {
            atomic_write_at(
                left_root.as_path(),
                &MemoryPath::Index,
                b"left",
                Some(left_expected.as_str()),
                false,
            )
        });
        let right = std::thread::spawn(move || {
            atomic_write_at(
                right_root.as_path(),
                &MemoryPath::Index,
                b"right",
                Some(expected.as_str()),
                false,
            )
        });
        let results = [left.join().expect("left"), right.join().expect("right")];
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| result
                    .as_ref()
                    .is_err_and(|error| error.kind == ExecutionFileSystemErrorKind::Conflict))
                .count(),
            1
        );
        let _ = fs::remove_dir_all(root);
    }
}
