#[cfg(test)]
use std::path::PathBuf;
#[cfg(test)]
use std::process::Command;
#[cfg(test)]
use std::sync::Arc;

use serde::{Deserialize, Serialize};

#[cfg(test)]
use crate::execution::sandbox::{decode_process_output, SandboxAttempt, SandboxPolicySummary};
use crate::execution::sandbox::{
    SandboxErr, SandboxPolicy, SandboxTransformRequest, SandboxType, SandboxedProcessOutput,
};

mod filesystem;
pub mod sandbox;

pub use filesystem::{
    run_direct_execution_file_system_operation, run_policy_scoped_execution_file_system_operation,
    run_scoped_execution_file_system_operation, ExecutionDirectoryEntry,
    ExecutionDirectoryEntryKind, ExecutionDirectoryListOutput, ExecutionFileDeleteOutput,
    ExecutionFileIdentity, ExecutionFileReadOutput, ExecutionFileSystemError,
    ExecutionFileSystemErrorKind, ExecutionFileSystemOperation, ExecutionFileSystemOutput,
    ExecutionFileSystemRequest, ExecutionFileWriteOutput, ExecutionHostBinding,
    ExecutionPathInspectionOutput, ExecutionPathKind,
};
pub const WORKSPACE_DATA_ROOT: &str = "/mnt/data";
pub const WORKSPACE_HOME: &str = "/home/agent";
pub const MAX_EXECUTION_INPUT_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_PUBLISHED_ARTIFACT_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExecutionHostKind {
    SandboxedProcess,
    LocalProcess,
    RemoteHost,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExecutionHostHealth {
    Ready,
    Starting,
    Degraded,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExecutionHostFailureKind {
    None,
    CommandFailed,
    TimedOut,
    Cancelled,
    SandboxUnavailable,
    HostUnavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExecutionHostMode {
    Local,
    Remote,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionHostStatus {
    pub kind: ExecutionHostKind,
    pub sandbox_type: SandboxType,
    pub health: ExecutionHostHealth,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionHostCommandOutput {
    pub process: SandboxedProcessOutput,
    pub failure_kind: ExecutionHostFailureKind,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_state_changes: Vec<ExecutionInputStateChange>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExecutionWorkspaceGenerationV1 {
    pub instance_epoch: String,
    pub generation: u64,
}

impl ExecutionWorkspaceGenerationV1 {
    pub fn validate(&self) -> Result<(), String> {
        if self.instance_epoch.is_empty()
            || self.instance_epoch.len() > 160
            || self.instance_epoch.chars().any(char::is_control)
        {
            return Err("execution_workspace_generation_epoch_invalid".to_string());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExecutionWorkspaceGeneration {
    Known {
        token: ExecutionWorkspaceGenerationV1,
    },
    Unknown {
        reason: String,
    },
}

impl ExecutionWorkspaceGeneration {
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Known { token } => token.validate(),
            Self::Unknown { reason } if !reason.trim().is_empty() => Ok(()),
            Self::Unknown { .. } => {
                Err("execution_workspace_generation_reason_invalid".to_string())
            }
        }
    }

    pub fn token(&self) -> Option<&ExecutionWorkspaceGenerationV1> {
        match self {
            Self::Known { token } => Some(token),
            Self::Unknown { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionInputStateChange {
    pub input_ref: String,
    pub state: ExecutionInputState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionInputState {
    AssetRemoved,
    AccessRevoked,
    SourceDeleted,
    StaleGeneration,
}

pub const MAX_EXECUTION_TIMEOUT_MS: u64 = 60 * 60 * 1000;

pub type ExecutionCancellationProbe = dyn Fn() -> Result<Option<String>, String> + Send + Sync;

pub trait ExecutionHostRunner: Send + Sync {
    fn bash_description(&self) -> &'static str {
        "bash"
    }

    fn kind(&self) -> ExecutionHostKind {
        ExecutionHostKind::SandboxedProcess
    }

    fn status(&self, policy: &SandboxPolicy) -> Result<ExecutionHostStatus, SandboxErr>;

    fn workspace_generation(&self) -> ExecutionWorkspaceGeneration {
        ExecutionWorkspaceGeneration::Unknown {
            reason: "execution host does not provide a trusted workspace generation".to_string(),
        }
    }

    fn run_file_system_operation(
        &self,
        request: ExecutionFileSystemRequest,
    ) -> Result<ExecutionFileSystemOutput, ExecutionFileSystemError>;

    fn run_host_command(
        &self,
        operation_id: Option<&str>,
        req: SandboxTransformRequest,
        cancellation_probe: Option<&ExecutionCancellationProbe>,
    ) -> Result<ExecutionHostCommandOutput, SandboxErr>;
}

#[cfg(test)]
struct TestExecutionHostRunner {
    bash_path: PathBuf,
}

#[cfg(test)]
impl TestExecutionHostRunner {
    fn new(explicit_bash_path: Option<PathBuf>) -> Result<Self, SandboxErr> {
        let explicit_bash_path = explicit_bash_path
            .or_else(|| std::env::var_os("CENTAERIS_TEST_BASH_PATH").map(PathBuf::from));
        if let Some(path) = explicit_bash_path.as_deref() {
            if !path.is_file() {
                return Err(SandboxErr::Unavailable {
                    reason: format!(
                        "configured test Bash executable is unavailable: {}",
                        path.display()
                    ),
                    sandbox_type: None,
                });
            }
        }
        Ok(Self {
            bash_path: explicit_bash_path.unwrap_or_else(|| PathBuf::from("bash")),
        })
    }
}

#[cfg(test)]
impl ExecutionHostRunner for TestExecutionHostRunner {
    fn bash_description(&self) -> &'static str {
        "bash"
    }

    fn status(&self, _policy: &SandboxPolicy) -> Result<ExecutionHostStatus, SandboxErr> {
        Ok(ExecutionHostStatus::transient_ready(test_sandbox_type()))
    }

    fn run_file_system_operation(
        &self,
        request: ExecutionFileSystemRequest,
    ) -> Result<ExecutionFileSystemOutput, ExecutionFileSystemError> {
        run_policy_scoped_execution_file_system_operation(request)
    }

    fn run_host_command(
        &self,
        _operation_id: Option<&str>,
        req: SandboxTransformRequest,
        cancellation_probe: Option<&ExecutionCancellationProbe>,
    ) -> Result<ExecutionHostCommandOutput, SandboxErr> {
        let sandbox_type = test_sandbox_type();
        if let Some(probe) = cancellation_probe {
            let reason =
                probe().unwrap_or_else(|error| Some(format!("cancellation probe failed: {error}")));
            if let Some(reason) = reason {
                return Err(SandboxErr::CancellationIndeterminate {
                    reason,
                    sandbox_type: Some(sandbox_type),
                });
            }
        }
        // ponytail: Core uses only short fixture commands; timeout and process-tree behavior stay in Host adapter tests.
        let program = if req.program == "bash" {
            self.bash_path.as_path()
        } else {
            std::path::Path::new(req.program.as_str())
        };
        let output = Command::new(program)
            .args(req.args.iter())
            .current_dir(req.cwd.as_path())
            .envs(req.env.iter())
            .output()
            .map_err(|error| SandboxErr::Io(format!("run Core test command failed: {error}")))?;
        let stdout = decode_process_output(output.stdout.as_slice());
        let stderr = decode_process_output(output.stderr.as_slice());
        let exit_code = output.status.code();
        Ok(ExecutionHostCommandOutput {
            process: SandboxedProcessOutput {
                exit_code,
                stdout: stdout.text,
                stderr: stderr.text,
                stdout_decode: stdout.summary,
                stderr_decode: stderr.summary,
                timed_out: false,
                attempt: SandboxAttempt {
                    sandbox_type,
                    transition_reason: "core_test_execution_host".to_string(),
                    policy: SandboxPolicySummary {
                        sandbox_type,
                        enforced: sandbox_type != SandboxType::HostProcess,
                        network: req.policy.network,
                        workspace_root: req
                            .policy
                            .filesystem
                            .workspace_root
                            .to_string_lossy()
                            .to_string(),
                        read_only_root_count: req.policy.filesystem.read_only_roots.len(),
                        writable_root_count: req.policy.filesystem.writable_roots.len(),
                        denied_read_path_count: req.policy.filesystem.denied_read_paths.len(),
                        denied_write_path_count: req.policy.filesystem.denied_write_paths.len(),
                    },
                },
                runtime_diagnostics: Vec::new(),
            },
            failure_kind: classify_execution_host_failure(exit_code, false, "", ""),
            input_state_changes: Vec::new(),
        })
    }
}

#[cfg(all(test, target_os = "linux"))]
fn test_sandbox_type() -> SandboxType {
    SandboxType::LinuxBubblewrap
}

#[cfg(all(test, target_os = "macos"))]
fn test_sandbox_type() -> SandboxType {
    SandboxType::MacOsSeatbelt
}

#[cfg(all(test, target_os = "windows"))]
fn test_sandbox_type() -> SandboxType {
    SandboxType::OciContainer
}

impl ExecutionHostStatus {
    pub fn transient_ready(sandbox_type: SandboxType) -> Self {
        Self {
            kind: ExecutionHostKind::SandboxedProcess,
            sandbox_type,
            health: ExecutionHostHealth::Ready,
            detail: None,
        }
    }

    pub fn remote(
        sandbox_type: SandboxType,
        health: ExecutionHostHealth,
        detail: Option<String>,
    ) -> Self {
        Self {
            kind: ExecutionHostKind::RemoteHost,
            sandbox_type,
            health,
            detail,
        }
    }
}
pub fn classify_execution_host_failure(
    exit_code: Option<i32>,
    timed_out: bool,
    _stdout: &str,
    _stderr: &str,
) -> ExecutionHostFailureKind {
    if timed_out {
        return ExecutionHostFailureKind::TimedOut;
    }
    if exit_code == Some(0) {
        return ExecutionHostFailureKind::None;
    }
    ExecutionHostFailureKind::CommandFailed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execution_host_kind_uses_exact_wire_values() {
        assert_eq!(
            serde_json::to_string(&ExecutionHostKind::LocalProcess).unwrap(),
            "\"localProcess\""
        );
        assert!(serde_json::from_str::<ExecutionHostKind>("\"banana\"").is_err());
    }

    struct TestRemoteRunner;

    impl ExecutionHostRunner for TestRemoteRunner {
        fn kind(&self) -> ExecutionHostKind {
            ExecutionHostKind::RemoteHost
        }

        fn status(&self, _policy: &SandboxPolicy) -> Result<ExecutionHostStatus, SandboxErr> {
            Ok(ExecutionHostStatus::remote(
                SandboxType::OciContainer,
                ExecutionHostHealth::Ready,
                None,
            ))
        }

        fn run_file_system_operation(
            &self,
            request: ExecutionFileSystemRequest,
        ) -> Result<ExecutionFileSystemOutput, ExecutionFileSystemError> {
            run_direct_execution_file_system_operation(request)
        }

        fn run_host_command(
            &self,
            _operation_id: Option<&str>,
            _req: SandboxTransformRequest,
            _cancellation_probe: Option<&ExecutionCancellationProbe>,
        ) -> Result<ExecutionHostCommandOutput, SandboxErr> {
            Err(SandboxErr::Unavailable {
                reason: "test runner does not execute".to_string(),
                sandbox_type: None,
            })
        }
    }

    #[test]
    fn execution_host_failure_classifies_timeout_before_exit_code() {
        assert_eq!(
            classify_execution_host_failure(Some(0), true, "", ""),
            ExecutionHostFailureKind::TimedOut
        );
    }

    #[test]
    fn execution_host_binding_uses_injected_remote_runner() {
        let policy = SandboxPolicy::workspace_write_no_network(std::env::temp_dir());
        let binding = ExecutionHostBinding::new(
            ExecutionHostMode::Remote,
            Arc::new(TestRemoteRunner),
            std::env::temp_dir(),
            policy,
        )
        .expect("remote binding");

        assert_eq!(binding.mode(), ExecutionHostMode::Remote);
        assert_eq!(binding.kind(), ExecutionHostKind::RemoteHost);
    }
}
