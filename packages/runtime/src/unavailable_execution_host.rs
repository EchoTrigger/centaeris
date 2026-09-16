//! Build-time Windows facade. Execution is available only in the WSL2 Runtime.
use centaeris_core::execution::{
    ExecutionCancellationProbe, ExecutionCommandRequest, ExecutionError, ExecutionFileSystemError,
    ExecutionFileSystemErrorKind, ExecutionFileSystemOutput, ExecutionFileSystemRequest,
    ExecutionHostCommandOutput, ExecutionHostKind, ExecutionHostRunner, ExecutionHostStatus,
    ExecutionPolicy,
};
use std::{collections::HashMap, path::PathBuf};

const REASON: &str = "Native Windows execution is unavailable; use the Linux Runtime in WSL2.";

#[derive(Debug, Clone)]
pub struct LocalExecutionHostRunner;

impl LocalExecutionHostRunner {
    // Construction keeps host-independent Runtime state tests available on Windows.
    // Every operation fails closed; this facade cannot spawn a process or access files.
    pub fn new(_: Option<PathBuf>) -> Result<Self, ExecutionError> {
        Ok(Self)
    }
    pub fn new_with_runtime_executable(
        _: Option<PathBuf>,
        _: PathBuf,
    ) -> Result<Self, ExecutionError> {
        Ok(Self)
    }
    pub fn with_environment_overrides(
        self,
        _: HashMap<String, String>,
    ) -> Result<Self, ExecutionError> {
        Ok(self)
    }
    pub fn bash_description(&self) -> &'static str {
        "bash (WSL2 required)"
    }
}

impl ExecutionHostRunner for LocalExecutionHostRunner {
    fn bash_description(&self) -> &'static str {
        self.bash_description()
    }
    fn kind(&self) -> ExecutionHostKind {
        ExecutionHostKind::SandboxedProcess
    }
    fn status(&self, _: &ExecutionPolicy) -> Result<ExecutionHostStatus, ExecutionError> {
        Err(ExecutionError::HostUnavailable {
            reason: REASON.into(),
        })
    }
    fn run_host_command(
        &self,
        _: Option<&str>,
        _: ExecutionCommandRequest,
        _: Option<&ExecutionCancellationProbe>,
    ) -> Result<ExecutionHostCommandOutput, ExecutionError> {
        Err(ExecutionError::HostUnavailable {
            reason: REASON.into(),
        })
    }
    fn run_file_system_operation(
        &self,
        _: ExecutionFileSystemRequest,
    ) -> Result<ExecutionFileSystemOutput, ExecutionFileSystemError> {
        Err(ExecutionFileSystemError::new(
            ExecutionFileSystemErrorKind::HostUnavailable,
            REASON,
        ))
    }
}
