use std::collections::HashMap;
use std::env;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "linux")]
use linux as platform;
#[cfg(target_os = "macos")]
use macos as platform;
#[cfg(target_os = "windows")]
use windows as platform;

#[cfg(target_os = "windows")]
pub use windows::run_windows_host_launcher;

#[cfg(unix)]
use std::os::unix::{io::AsRawFd, process::CommandExt};
#[cfg(target_os = "windows")]
use std::os::windows::{io::AsRawHandle, process::CommandExt};

#[cfg(target_os = "windows")]
use windows_sys::Win32::{
    Foundation::{CloseHandle, ERROR_BROKEN_PIPE, ERROR_PIPE_NOT_CONNECTED, HANDLE},
    System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    },
    System::Pipes::PeekNamedPipe,
};

const EXIT_STDIO_GRACE: Duration = Duration::from_millis(100);
const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(25);
use centaeris_core::execution::sandbox::{
    decode_process_output, SandboxAttempt, SandboxErr, SandboxPolicy, SandboxPolicySummary,
    SandboxTransformRequest, SandboxType, SandboxedProcessOutput,
};

#[cfg(not(target_os = "windows"))]
use centaeris_core::execution::run_direct_execution_file_system_operation;
#[cfg(any(test, target_os = "windows"))]
use centaeris_core::execution::run_policy_scoped_execution_file_system_operation;
use centaeris_core::execution::{
    classify_execution_host_failure, ExecutionCancellationProbe, ExecutionFileSystemError,
    ExecutionFileSystemOutput, ExecutionFileSystemRequest, ExecutionHostCommandOutput,
    ExecutionHostHealth, ExecutionHostKind, ExecutionHostRunner, ExecutionHostStatus,
};
use sha2::{Digest, Sha256};

#[cfg(not(target_os = "windows"))]
const LOCAL_FILESYSTEM_FRAME_LIMIT_BYTES: usize = 64 * 1024 * 1024;
#[cfg(not(target_os = "windows"))]
const LOCAL_FILESYSTEM_TIMEOUT_MS: u64 = 60_000;
#[derive(Debug, Clone)]
pub struct LocalExecutionHostRunner {
    bash_path: PathBuf,
    runtime_executable: PathBuf,
    environment_overrides: HashMap<String, String>,
    #[cfg(test)]
    embedded_test: bool,
}

struct PreparedSandboxCommand {
    command: Command,
    completion: Option<CompletionMarker>,
    stdin_input: Option<Vec<u8>>,
}

struct CompletionMarker {
    root: PathBuf,
    path: PathBuf,
    control_path: Option<PathBuf>,
    secret: String,
}

#[cfg(target_os = "linux")]
pub fn run_linux_supervisor(arguments: &[String]) -> Result<i32, String> {
    linux::run_supervisor(arguments)
}

impl LocalExecutionHostRunner {
    pub fn new(explicit_bash_path: Option<PathBuf>) -> Result<Self, SandboxErr> {
        let runtime_executable = env::current_exe().map_err(|error| SandboxErr::Unavailable {
            reason: format!("resolve local Runtime executable failed: {error}"),
            sandbox_type: Some(platform::sandbox_type()),
        })?;
        Self::new_with_runtime_executable(explicit_bash_path, runtime_executable)
    }

    pub fn new_with_runtime_executable(
        explicit_bash_path: Option<PathBuf>,
        runtime_executable: PathBuf,
    ) -> Result<Self, SandboxErr> {
        if !runtime_executable.is_file() {
            return Err(SandboxErr::Unavailable {
                reason: format!(
                    "local Runtime executable is unavailable: {}",
                    runtime_executable.display()
                ),
                sandbox_type: Some(platform::sandbox_type()),
            });
        }
        Ok(Self {
            bash_path: resolve_bash_path(explicit_bash_path)?,
            runtime_executable,
            environment_overrides: HashMap::new(),
            #[cfg(test)]
            embedded_test: false,
        })
    }

    #[cfg(test)]
    pub fn new_embedded_test(explicit_bash_path: Option<PathBuf>) -> Result<Self, SandboxErr> {
        let mut runner = Self::new(explicit_bash_path)?;
        runner.embedded_test = true;
        Ok(runner)
    }

    pub fn bash_description(&self) -> &'static str {
        if cfg!(target_os = "windows") {
            "bash (Git for Windows)"
        } else {
            "bash"
        }
    }

    pub fn with_environment_overrides(
        mut self,
        environment_overrides: HashMap<String, String>,
    ) -> Result<Self, SandboxErr> {
        if environment_overrides
            .iter()
            .any(|(key, value)| key.is_empty() || key.contains(['\0', '=']) || value.contains('\0'))
        {
            return Err(SandboxErr::Unavailable {
                reason: "local execution environment override is invalid".to_string(),
                sandbox_type: Some(platform::sandbox_type()),
            });
        }
        self.environment_overrides = environment_overrides;
        Ok(self)
    }

    fn run_command(
        &self,
        mut req: SandboxTransformRequest,
        cancellation_probe: Option<&ExecutionCancellationProbe>,
    ) -> Result<ExecutionHostCommandOutput, SandboxErr> {
        req.env.extend(self.environment_overrides.clone());
        let program = if req.program == "bash" {
            self.bash_path.clone()
        } else {
            PathBuf::from(req.program.as_str())
        };
        validate_local_policy(req.cwd.as_path(), &req.policy)?;
        #[cfg(test)]
        let preserve_background = !self.embedded_test || cfg!(target_os = "windows");
        #[cfg(not(test))]
        let preserve_background = true;
        let mut prepared = platform::prepare_command(
            self.runtime_executable.as_path(),
            program.as_path(),
            req.args.as_slice(),
            req.cwd.as_path(),
            &req.env,
            &req.policy,
            preserve_background,
        )?;
        let stdin_input = prepared.stdin_input.take().or_else(|| {
            prepared
                .completion
                .as_ref()
                .map(|completion| format!("{}\n", completion.secret).into_bytes())
        });
        prepared
            .command
            .stdin(if stdin_input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        configure_local_process(&mut prepared.command);

        let sandbox_type = platform::sandbox_type();

        let output = run_local_command_with_timeout(
            &mut prepared.command,
            req.timeout_ms,
            cancellation_probe,
            prepared.completion.as_ref(),
            sandbox_type,
            stdin_input.as_deref(),
        )?;
        let policy = policy_summary(&req.policy, sandbox_type);
        let process = SandboxedProcessOutput {
            exit_code: output.exit_code,
            stdout: output.stdout,
            stderr: output.stderr,
            stdout_decode: output.stdout_decode,
            stderr_decode: output.stderr_decode,
            timed_out: output.timed_out,
            attempt: SandboxAttempt {
                sandbox_type,
                transition_reason: platform_transition_reason().to_string(),
                policy,
            },
            runtime_diagnostics: Vec::new(),
        };
        let failure_kind = classify_execution_host_failure(
            process.exit_code,
            process.timed_out,
            process.stdout.as_str(),
            process.stderr.as_str(),
        );
        Ok(ExecutionHostCommandOutput {
            process,
            failure_kind,
            input_state_changes: Vec::new(),
        })
    }
}

impl ExecutionHostRunner for LocalExecutionHostRunner {
    fn bash_description(&self) -> &'static str {
        self.bash_description()
    }

    fn kind(&self) -> ExecutionHostKind {
        if cfg!(target_os = "windows") {
            ExecutionHostKind::LocalProcess
        } else {
            ExecutionHostKind::SandboxedProcess
        }
    }

    fn status(&self, policy: &SandboxPolicy) -> Result<ExecutionHostStatus, SandboxErr> {
        validate_local_policy(policy.filesystem.workspace_root.as_path(), policy)?;
        #[cfg(target_os = "windows")]
        platform::validate_policy(policy)?;
        platform::ensure_available()?;
        Ok(ExecutionHostStatus {
            kind: self.kind(),
            sandbox_type: platform::sandbox_type(),
            health: ExecutionHostHealth::Ready,
            detail: Some(format!("bash={}", self.bash_path.display())),
        })
    }

    fn run_file_system_operation(
        &self,
        request: ExecutionFileSystemRequest,
    ) -> Result<ExecutionFileSystemOutput, ExecutionFileSystemError> {
        #[cfg(test)]
        if self.embedded_test {
            validate_local_policy(request.cwd.as_path(), &request.policy)
                .map_err(filesystem_sandbox_error)?;
            return run_policy_scoped_execution_file_system_operation(request);
        }
        #[cfg(target_os = "windows")]
        {
            validate_local_policy(request.cwd.as_path(), &request.policy)
                .map_err(filesystem_sandbox_error)?;
            run_policy_scoped_execution_file_system_operation(request)
        }
        #[cfg(not(target_os = "windows"))]
        self.run_file_system_helper(request)
    }

    fn run_host_command(
        &self,
        _operation_id: Option<&str>,
        req: SandboxTransformRequest,
        cancellation_probe: Option<&ExecutionCancellationProbe>,
    ) -> Result<ExecutionHostCommandOutput, SandboxErr> {
        self.run_command(req, cancellation_probe)
    }
}

impl LocalExecutionHostRunner {
    #[cfg(not(target_os = "windows"))]
    fn run_file_system_helper(
        &self,
        request: ExecutionFileSystemRequest,
    ) -> Result<ExecutionFileSystemOutput, ExecutionFileSystemError> {
        validate_local_policy(request.cwd.as_path(), &request.policy)
            .map_err(filesystem_sandbox_error)?;
        let input = serde_json::to_vec(&request).map_err(|error| {
            ExecutionFileSystemError::new(
                centaeris_core::execution::ExecutionFileSystemErrorKind::HostUnavailable,
                "encode local filesystem sandbox request failed",
            )
            .with_diagnostic(error.to_string())
        })?;
        if input.len() > LOCAL_FILESYSTEM_FRAME_LIMIT_BYTES {
            return Err(ExecutionFileSystemError::new(
                centaeris_core::execution::ExecutionFileSystemErrorKind::TooLarge,
                "local filesystem sandbox request is too large",
            ));
        }
        let mut prepared = platform::prepare_command(
            self.runtime_executable.as_path(),
            self.runtime_executable.as_path(),
            &["--local-sandbox-filesystem-helper".to_string()],
            request.cwd.as_path(),
            &std::collections::HashMap::new(),
            &request.policy,
            false,
        )
        .map_err(filesystem_sandbox_error)?;
        prepared
            .command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        configure_local_process(&mut prepared.command);
        let output = run_local_command_with_timeout(
            &mut prepared.command,
            LOCAL_FILESYSTEM_TIMEOUT_MS,
            None,
            None,
            platform::sandbox_type(),
            Some(input.as_slice()),
        )
        .map_err(filesystem_sandbox_error)?;
        if output.timed_out || output.exit_code != Some(0) {
            return Err(ExecutionFileSystemError::new(
                centaeris_core::execution::ExecutionFileSystemErrorKind::HostUnavailable,
                "local filesystem sandbox helper failed",
            )
            .with_diagnostic(output.stderr));
        }
        serde_json::from_str::<Result<ExecutionFileSystemOutput, ExecutionFileSystemError>>(
            output.stdout.as_str(),
        )
        .map_err(|error| {
            ExecutionFileSystemError::new(
                centaeris_core::execution::ExecutionFileSystemErrorKind::HostUnavailable,
                "decode local filesystem sandbox response failed",
            )
            .with_diagnostic(error.to_string())
        })?
    }
}

fn filesystem_sandbox_error(error: SandboxErr) -> ExecutionFileSystemError {
    let kind = match error {
        SandboxErr::Denied { .. } => {
            centaeris_core::execution::ExecutionFileSystemErrorKind::PermissionDenied
        }
        _ => centaeris_core::execution::ExecutionFileSystemErrorKind::HostUnavailable,
    };
    ExecutionFileSystemError::new(kind, error.model_visible_message())
        .with_diagnostic(error.internal_debug_message())
}

#[cfg(not(target_os = "windows"))]
pub fn run_file_system_helper() -> Result<(), String> {
    let mut input = Vec::new();
    std::io::stdin()
        .lock()
        .take((LOCAL_FILESYSTEM_FRAME_LIMIT_BYTES + 1) as u64)
        .read_to_end(&mut input)
        .map_err(|error| format!("read local filesystem sandbox request failed: {error}"))?;
    if input.is_empty() || input.len() > LOCAL_FILESYSTEM_FRAME_LIMIT_BYTES {
        return Err("local filesystem sandbox request size is invalid".to_string());
    }
    let request = serde_json::from_slice::<ExecutionFileSystemRequest>(input.as_slice())
        .map_err(|error| format!("decode local filesystem sandbox request failed: {error}"))?;
    let result = run_direct_execution_file_system_operation(request);
    serde_json::to_writer(std::io::stdout().lock(), &result)
        .map_err(|error| format!("encode local filesystem sandbox response failed: {error}"))
}

pub fn resolve_bash_path(explicit_bash_path: Option<PathBuf>) -> Result<PathBuf, SandboxErr> {
    #[cfg(target_os = "windows")]
    return windows::resolve_git_bash_path(explicit_bash_path);

    #[cfg(not(target_os = "windows"))]
    if let Some(path) = explicit_bash_path {
        return executable_file(path.as_path()).ok_or_else(|| SandboxErr::Unavailable {
            reason: format!(
                "configured Bash executable is unavailable: {}",
                path.display()
            ),
            sandbox_type: None,
        });
    }

    #[cfg(not(target_os = "windows"))]
    {
        if let Some(path) = executable_file(Path::new("/bin/bash")).or_else(|| find_on_path("bash"))
        {
            return Ok(path);
        }
        Err(SandboxErr::Unavailable {
            reason: "Bash is required but no Bash executable was found".to_string(),
            sandbox_type: None,
        })
    }
}

#[cfg(not(target_os = "windows"))]
fn executable_file(path: &Path) -> Option<PathBuf> {
    path.is_file().then(|| path.to_path_buf())
}

#[cfg(not(target_os = "windows"))]
fn find_on_path(name: &str) -> Option<PathBuf> {
    env::var_os("PATH").and_then(|path| {
        env::split_paths(&path)
            .map(|directory| directory.join(name))
            .find_map(|candidate| executable_file(candidate.as_path()))
    })
}

fn policy_summary(policy: &SandboxPolicy, sandbox_type: SandboxType) -> SandboxPolicySummary {
    SandboxPolicySummary {
        sandbox_type,
        enforced: sandbox_type != SandboxType::HostProcess,
        network: policy.network.clone(),
        workspace_root: policy
            .filesystem
            .workspace_root
            .to_string_lossy()
            .to_string(),
        read_only_root_count: policy.filesystem.read_only_roots.len(),
        writable_root_count: policy.filesystem.writable_roots.len(),
        denied_read_path_count: policy.filesystem.denied_read_paths.len(),
        denied_write_path_count: policy.filesystem.denied_write_paths.len(),
    }
}

fn validate_local_policy(cwd: &Path, policy: &SandboxPolicy) -> Result<(), SandboxErr> {
    policy
        .network
        .validate()
        .map_err(|reason| SandboxErr::Denied {
            reason,
            sandbox_type: platform::sandbox_type(),
        })?;
    let canonical_cwd = cwd
        .canonicalize()
        .map_err(|error| SandboxErr::Unavailable {
            reason: format!("canonicalize local sandbox working directory failed: {error}"),
            sandbox_type: Some(platform::sandbox_type()),
        })?;
    let canonical_workspace = policy
        .filesystem
        .workspace_root
        .canonicalize()
        .map_err(|error| SandboxErr::Unavailable {
            reason: format!("canonicalize local sandbox workspace failed: {error}"),
            sandbox_type: Some(platform::sandbox_type()),
        })?;
    if canonical_cwd != canonical_workspace {
        return Err(SandboxErr::Denied {
            reason: "local sandbox working directory must equal the policy workspace root"
                .to_string(),
            sandbox_type: platform::sandbox_type(),
        });
    }
    if std::iter::once(&policy.filesystem.workspace_root)
        .chain(policy.filesystem.tmp_root.iter())
        .chain(
            policy
                .filesystem
                .read_only_roots
                .iter()
                .chain(policy.filesystem.writable_roots.iter())
                .chain(policy.filesystem.denied_read_paths.iter())
                .chain(policy.filesystem.denied_write_paths.iter()),
        )
        .any(|path| !path.is_absolute())
    {
        return Err(SandboxErr::Denied {
            reason: "local sandbox policy paths must be absolute".to_string(),
            sandbox_type: platform::sandbox_type(),
        });
    }
    materialize_temporary_root(policy)?;
    for root in policy
        .filesystem
        .read_only_roots
        .iter()
        .chain(policy.filesystem.writable_roots.iter())
        .chain(policy.filesystem.tmp_root.iter())
    {
        if !root.is_dir() {
            return Err(SandboxErr::Denied {
                reason: format!(
                    "local sandbox filesystem root is not an existing directory: {}",
                    root.display()
                ),
                sandbox_type: platform::sandbox_type(),
            });
        }
    }
    Ok(())
}

fn materialize_temporary_root(policy: &SandboxPolicy) -> Result<(), SandboxErr> {
    let Some(root) = policy.filesystem.tmp_root.as_deref() else {
        return Ok(());
    };
    let configured_temp =
        env::temp_dir()
            .canonicalize()
            .map_err(|error| SandboxErr::Unavailable {
                reason: format!("canonicalize local temporary directory failed: {error}"),
                sandbox_type: Some(platform::sandbox_type()),
            })?;
    let mut ancestor = root.to_path_buf();
    while !ancestor.exists() {
        if !ancestor.pop() {
            return Err(SandboxErr::Denied {
                reason: format!(
                    "local sandbox temporary root is invalid: {}",
                    root.display()
                ),
                sandbox_type: platform::sandbox_type(),
            });
        }
    }
    let canonical_ancestor = ancestor
        .canonicalize()
        .map_err(|error| SandboxErr::Unavailable {
            reason: format!("canonicalize local sandbox temporary root failed: {error}"),
            sandbox_type: Some(platform::sandbox_type()),
        })?;
    if canonical_ancestor == configured_temp || canonical_ancestor.starts_with(&configured_temp) {
        std::fs::create_dir_all(root).map_err(|error| {
            SandboxErr::Io(format!(
                "create local sandbox temporary root failed: {error}"
            ))
        })?;
    } else {
        return Err(SandboxErr::Denied {
            reason: "local sandbox temporary root must remain within the configured OS temporary directory".to_string(),
            sandbox_type: platform::sandbox_type(),
        });
    }
    let canonical_root = root
        .canonicalize()
        .map_err(|error| SandboxErr::Unavailable {
            reason: format!("canonicalize materialized sandbox temporary root failed: {error}"),
            sandbox_type: Some(platform::sandbox_type()),
        })?;
    if canonical_root == configured_temp
        || !canonical_root.starts_with(&configured_temp)
        || !canonical_root.is_dir()
    {
        return Err(SandboxErr::Denied {
            reason: "local sandbox temporary root must be an exact child directory of the configured OS temporary directory".to_string(),
            sandbox_type: platform::sandbox_type(),
        });
    }
    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn materialize_denied_paths(
    policy: &SandboxPolicy,
    sandbox_type: SandboxType,
) -> Result<(), SandboxErr> {
    for path in policy
        .filesystem
        .denied_read_paths
        .iter()
        .chain(policy.filesystem.denied_write_paths.iter())
    {
        if !path.exists()
            && path.file_name().and_then(|name| name.to_str()) == Some(".centaeris")
            && path.parent() == Some(policy.filesystem.workspace_root.as_path())
        {
            std::fs::create_dir(path).map_err(|error| {
                SandboxErr::Io(format!(
                    "create protected workspace metadata directory failed: {error}"
                ))
            })?;
        }
        if !path.exists() {
            return Err(SandboxErr::Denied {
                reason: format!("denied sandbox path does not exist: {}", path.display()),
                sandbox_type,
            });
        }
    }
    Ok(())
}

fn platform_transition_reason() -> &'static str {
    match platform::sandbox_type() {
        SandboxType::LinuxBubblewrap => "linux_bubblewrap",
        SandboxType::MacOsSeatbelt => "macos_seatbelt",
        SandboxType::HostProcess => "windows_git_bash_host",
        _ => unreachable!("local platform sandbox type"),
    }
}

fn configure_local_process(command: &mut Command) {
    #[cfg(target_os = "windows")]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(unix)]
    command.process_group(0);
}

struct CapturedProcessOutput {
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
    stdout_decode: centaeris_core::execution::sandbox::ProcessOutputDecodeSummary,
    stderr_decode: centaeris_core::execution::sandbox::ProcessOutputDecodeSummary,
    timed_out: bool,
}

fn run_local_command_with_timeout(
    command: &mut Command,
    timeout_ms: u64,
    cancellation_probe: Option<&ExecutionCancellationProbe>,
    completion: Option<&CompletionMarker>,
    sandbox_type: SandboxType,
    stdin_input: Option<&[u8]>,
) -> Result<CapturedProcessOutput, SandboxErr> {
    #[cfg(target_os = "windows")]
    let process_job = WindowsProcessJob::new()?;
    let mut child = command
        .spawn()
        .map_err(|error| SandboxErr::Io(format!("spawn local command failed: {error}")))?;
    #[cfg(target_os = "windows")]
    if let Err(error) = process_job.assign(&child) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }
    if let Some(input) = stdin_input {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| SandboxErr::Io("local process stdin was not captured".to_string()))?;
        if let Err(error) = stdin.write_all(input) {
            #[cfg(target_os = "windows")]
            let _ = terminate_windows_process_job(&mut child, &process_job, completion);
            #[cfg(unix)]
            terminate_process_tree(&mut child);
            let _ = child.wait();
            return Err(SandboxErr::Io(format!(
                "write local process input failed: {error}"
            )));
        }
    }
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| SandboxErr::Io("local process stdout was not captured".to_string()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| SandboxErr::Io("local process stderr was not captured".to_string()))?;
    let mut stdout = stdout;
    let mut stderr = stderr;
    #[cfg(unix)]
    if let Err(error) =
        configure_nonblocking_output(&stdout).and_then(|_| configure_nonblocking_output(&stderr))
    {
        terminate_process_tree(&mut child);
        let _ = child.wait();
        return Err(error);
    }
    // ponytail: V1 buffers complete process output in memory; replace this with a
    // streaming ToolResult sink when measured command output can exceed the Host budget.
    let mut stdout_output = ReadOutput::default();
    let mut stderr_output = ReadOutput::default();
    let mut stdout_eof = false;
    let mut stderr_eof = false;
    let mut last_output_at = None;
    let deadline = Instant::now()
        .checked_add(Duration::from_millis(timeout_ms))
        .unwrap_or_else(Instant::now);
    let mut child_exit_code = None;
    let mut child_exit_at = None;
    let (exit_code, timed_out) = loop {
        let now = Instant::now();
        let drain_result = (|| {
            let stdout_drain = if stdout_eof {
                OutputDrain::default()
            } else {
                drain_available_output(&mut stdout, &mut stdout_output)?
            };
            let stderr_drain = if stderr_eof {
                OutputDrain::default()
            } else {
                drain_available_output(&mut stderr, &mut stderr_output)?
            };
            Ok::<_, SandboxErr>((stdout_drain, stderr_drain))
        })();
        let (stdout_drain, stderr_drain) = match drain_result {
            Ok(drains) => drains,
            Err(error) => {
                #[cfg(target_os = "windows")]
                let _ = terminate_windows_process_job(&mut child, &process_job, completion);
                #[cfg(unix)]
                terminate_process_tree(&mut child);
                let _ = child.wait();
                return Err(error);
            }
        };
        stdout_eof |= stdout_drain.eof;
        stderr_eof |= stderr_drain.eof;
        if stdout_drain.bytes_read > 0 || stderr_drain.bytes_read > 0 {
            last_output_at = Some(now);
        }
        if child_exit_code.is_none() {
            if let Some(completion) = completion {
                if let Some(exit_code) = read_completion_marker(completion)? {
                    child_exit_code = Some(Some(exit_code));
                    child_exit_at = Some(now);
                }
            }
        }
        if child_exit_code.is_none() {
            match child.try_wait() {
                Ok(Some(status)) => {
                    child_exit_code = Some(status.code());
                    child_exit_at = Some(now);
                }
                Ok(None) => {}
                Err(error) => {
                    return Err(SandboxErr::Io(format!(
                        "poll local command status failed: {error}"
                    )))
                }
            }
        }
        if child_exit_code.is_none() {
            if let Some(probe) = cancellation_probe {
                let cancellation_reason = match probe() {
                    Ok(reason) => reason,
                    Err(error) => Some(format!("cancellation probe failed: {error}")),
                };
                if let Some(reason) = cancellation_reason {
                    #[cfg(target_os = "windows")]
                    let termination =
                        terminate_cancelled_process(&mut child, &process_job, completion);
                    #[cfg(unix)]
                    let termination = terminate_cancelled_process(&mut child);
                    return Err(SandboxErr::CancellationIndeterminate {
                        reason: format!("{reason}; {termination}"),
                        sandbox_type: Some(sandbox_type),
                    });
                }
            }
        }
        if let Some(exit_at) = child_exit_at {
            let quiet_since = last_output_at
                .filter(|output_at| *output_at > exit_at)
                .unwrap_or(exit_at);
            if (stdout_eof && stderr_eof) || now.duration_since(quiet_since) >= EXIT_STDIO_GRACE {
                break (child_exit_code.flatten(), false);
            }
        }
        if now >= deadline {
            #[cfg(target_os = "windows")]
            terminate_windows_process_job(&mut child, &process_job, completion)?;
            #[cfg(unix)]
            terminate_process_tree(&mut child);
            if child_exit_code.is_none() {
                child.wait().map_err(|error| {
                    SandboxErr::Io(format!("wait for timed out local command failed: {error}"))
                })?;
            }
            break (None, true);
        }
        sleep(PROCESS_POLL_INTERVAL);
    };
    #[cfg(target_os = "windows")]
    if !timed_out {
        process_job.allow_children_to_outlive_job()?;
    }
    let stdout = stdout_output;
    let stderr = stderr_output;
    let mut stdout_decoded = decode_process_output(stdout.bytes.as_slice());
    let mut stderr_decoded = decode_process_output(stderr.bytes.as_slice());
    stdout_decoded.summary.raw_byte_length = stdout.total_bytes;
    stderr_decoded.summary.raw_byte_length = stderr.total_bytes;
    if timed_out {
        stderr_decoded.text = format!(
            "command timed out after {timeout_ms}ms\n{}",
            stderr_decoded.text
        );
    }
    Ok(CapturedProcessOutput {
        exit_code,
        stdout: stdout_decoded.text,
        stderr: stderr_decoded.text,
        stdout_decode: stdout_decoded.summary,
        stderr_decode: stderr_decoded.summary,
        timed_out,
    })
}

fn read_completion_marker(marker: &CompletionMarker) -> Result<Option<i32>, SandboxErr> {
    let value = match std::fs::read_to_string(marker.path.as_path()) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(SandboxErr::Io(format!(
                "read sandbox supervisor completion failed: {error}"
            )))
        }
    };
    let Some((exit_code, digest)) = value.trim().split_once(':') else {
        return Ok(None);
    };
    let Ok(exit_code) = exit_code.parse::<i32>() else {
        return Ok(None);
    };
    if digest != completion_digest(marker.secret.as_str(), exit_code) {
        return Ok(None);
    }
    cleanup_completion_marker(marker);
    Ok(Some(exit_code))
}

fn cleanup_completion_marker(marker: &CompletionMarker) {
    let _ = std::fs::remove_file(marker.path.as_path());
    if let Some(control_path) = marker.control_path.as_deref() {
        let _ = std::fs::remove_file(control_path);
    }
    let _ = std::fs::remove_dir(marker.root.as_path());
}

fn completion_digest(secret: &str, exit_code: i32) -> String {
    format!("{:x}", Sha256::digest(format!("{secret}:{exit_code}")))
}

#[cfg(target_os = "linux")]
fn completion_secret() -> Result<String, SandboxErr> {
    let mut bytes = [0u8; 32];
    #[cfg(unix)]
    {
        use std::io::Read as _;
        std::fs::File::open("/dev/urandom")
            .and_then(|mut file| file.read_exact(&mut bytes))
            .map_err(|error| {
                SandboxErr::Io(format!("generate completion secret failed: {error}"))
            })?;
    }
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

#[cfg(target_os = "windows")]
fn terminate_cancelled_process(
    child: &mut std::process::Child,
    job: &WindowsProcessJob,
    completion: Option<&CompletionMarker>,
) -> String {
    match terminate_windows_process_job(child, job, completion) {
        Ok(()) => match child.wait() {
            Ok(_) => "local process tree termination completed".to_string(),
            Err(error) => format!("local process tree terminated but wait failed: {error}"),
        },
        Err(error) => format!(
            "local process tree termination could not be verified: {}",
            error.internal_debug_message()
        ),
    }
}

#[cfg(target_os = "windows")]
fn terminate_windows_process_job(
    child: &mut std::process::Child,
    job: &WindowsProcessJob,
    completion: Option<&CompletionMarker>,
) -> Result<(), SandboxErr> {
    if let Some(completion) = completion {
        platform::terminate(completion)?;
    }
    job.terminate()?;
    let _ = child.wait();
    if let Some(completion) = completion {
        cleanup_completion_marker(completion);
    }
    Ok(())
}

#[cfg(unix)]
fn terminate_cancelled_process(child: &mut std::process::Child) -> String {
    terminate_process_tree(child);
    match child.wait() {
        Ok(_) => "local process tree termination completed".to_string(),
        Err(error) => format!("local process tree termination could not be verified: {error}"),
    }
}

#[cfg(unix)]
fn terminate_process_tree(child: &mut std::process::Child) {
    let process_group_id = child.id() as i32;
    if unsafe { libc::kill(-process_group_id, libc::SIGKILL) } != 0 {
        let _ = child.kill();
    }
}

#[cfg(target_os = "windows")]
struct WindowsProcessJob(HANDLE);

#[cfg(target_os = "windows")]
impl WindowsProcessJob {
    fn new() -> Result<Self, SandboxErr> {
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(SandboxErr::Io(format!(
                "create local process job failed: {}",
                std::io::Error::last_os_error()
            )));
        }
        let job = Self(handle);
        job.set_kill_on_close(true)?;
        Ok(job)
    }

    fn assign(&self, child: &std::process::Child) -> Result<(), SandboxErr> {
        self.assign_handle(child.as_raw_handle() as HANDLE)
    }

    fn assign_handle(&self, process: HANDLE) -> Result<(), SandboxErr> {
        let assigned = unsafe { AssignProcessToJobObject(self.0, process) };
        if assigned == 0 {
            return Err(SandboxErr::Io(format!(
                "assign local process to job failed: {}",
                std::io::Error::last_os_error()
            )));
        }
        Ok(())
    }

    fn terminate(&self) -> Result<(), SandboxErr> {
        if unsafe { TerminateJobObject(self.0, 1) } == 0 {
            return Err(SandboxErr::Io(format!(
                "terminate local process job failed: {}",
                std::io::Error::last_os_error()
            )));
        }
        Ok(())
    }

    fn allow_children_to_outlive_job(&self) -> Result<(), SandboxErr> {
        self.set_kill_on_close(false)
    }

    fn set_kill_on_close(&self, enabled: bool) -> Result<(), SandboxErr> {
        let mut limits = unsafe { std::mem::zeroed::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() };
        limits.BasicLimitInformation.LimitFlags = if enabled {
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        } else {
            0
        };
        let configured = unsafe {
            SetInformationJobObject(
                self.0,
                JobObjectExtendedLimitInformation,
                std::ptr::from_ref(&limits).cast(),
                std::mem::size_of_val(&limits) as u32,
            )
        };
        if configured == 0 {
            return Err(SandboxErr::Io(format!(
                "configure local process job failed: {}",
                std::io::Error::last_os_error()
            )));
        }
        Ok(())
    }
}

#[cfg(target_os = "windows")]
impl Drop for WindowsProcessJob {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

#[derive(Default)]
struct ReadOutput {
    bytes: Vec<u8>,
    total_bytes: usize,
}

impl ReadOutput {
    fn append(&mut self, input: &[u8]) {
        self.total_bytes = self.total_bytes.saturating_add(input.len());
        self.bytes.extend_from_slice(input);
    }
}

#[derive(Default)]
struct OutputDrain {
    bytes_read: usize,
    eof: bool,
}

#[cfg(unix)]
fn configure_nonblocking_output(reader: &impl AsRawFd) -> Result<(), SandboxErr> {
    let file_descriptor = reader.as_raw_fd();
    let flags = unsafe { libc::fcntl(file_descriptor, libc::F_GETFL) };
    if flags < 0
        || unsafe { libc::fcntl(file_descriptor, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
    {
        return Err(SandboxErr::Io(format!(
            "configure local process output failed: {}",
            std::io::Error::last_os_error()
        )));
    }
    Ok(())
}

#[cfg(unix)]
fn drain_available_output(
    reader: &mut (impl Read + AsRawFd),
    output: &mut ReadOutput,
) -> Result<OutputDrain, SandboxErr> {
    let mut drain = OutputDrain::default();
    let mut chunk = [0u8; 8 * 1024];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) => {
                drain.eof = true;
                break;
            }
            Ok(read) => {
                drain.bytes_read = drain.bytes_read.saturating_add(read);
                output.append(&chunk[..read]);
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => {
                return Err(SandboxErr::Io(format!(
                    "read local process output failed: {error}"
                )))
            }
        }
    }
    Ok(drain)
}

#[cfg(target_os = "windows")]
fn drain_available_output(
    reader: &mut (impl Read + AsRawHandle),
    output: &mut ReadOutput,
) -> Result<OutputDrain, SandboxErr> {
    let mut drain = OutputDrain::default();
    let mut chunk = [0u8; 8 * 1024];
    loop {
        let mut available_bytes = 0u32;
        let peeked = unsafe {
            PeekNamedPipe(
                reader.as_raw_handle() as HANDLE,
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                &mut available_bytes,
                std::ptr::null_mut(),
            )
        };
        if peeked == 0 {
            let error = std::io::Error::last_os_error();
            if matches!(
                error.raw_os_error(),
                Some(code) if code == ERROR_BROKEN_PIPE as i32 || code == ERROR_PIPE_NOT_CONNECTED as i32
            ) {
                drain.eof = true;
                break;
            }
            return Err(SandboxErr::Io(format!(
                "inspect local process output failed: {error}"
            )));
        }
        if available_bytes == 0 {
            break;
        }
        let read_capacity = chunk.len().min(available_bytes as usize);
        let read = reader.read(&mut chunk[..read_capacity]).map_err(|error| {
            SandboxErr::Io(format!("read local process output failed: {error}"))
        })?;
        if read == 0 {
            drain.eof = true;
            break;
        }
        drain.bytes_read = drain.bytes_read.saturating_add(read);
        output.append(&chunk[..read]);
    }
    Ok(drain)
}

#[cfg(test)]
fn read_all_output(mut reader: impl Read) -> Result<ReadOutput, SandboxErr> {
    let mut output = ReadOutput::default();
    let mut chunk = [0u8; 8 * 1024];
    loop {
        let read = reader.read(&mut chunk).map_err(|error| {
            SandboxErr::Io(format!("read local process output failed: {error}"))
        })?;
        if read == 0 {
            break;
        }
        output.append(&chunk[..read]);
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use centaeris_core::execution::ExecutionHostFailureKind;
    use std::io::Cursor;

    #[test]
    fn explicit_missing_bash_path_fails_loudly() {
        let error = resolve_bash_path(Some(PathBuf::from("banana/missing/bash")))
            .expect_err("missing Bash path must fail");
        assert!(matches!(error, SandboxErr::Unavailable { .. }));
    }

    #[test]
    fn local_failure_classification_is_preserved() {
        assert_eq!(
            classify_execution_host_failure(Some(127), false, "", "command not found"),
            ExecutionHostFailureKind::CommandFailed
        );
    }

    #[test]
    fn command_output_reader_preserves_all_bytes() {
        let expected = vec![b'x'; 1024 * 1024 + 17];
        let output = read_all_output(Cursor::new(expected.clone())).expect("complete read");

        assert_eq!(output.bytes, expected);
        assert_eq!(output.total_bytes, 1024 * 1024 + 17);
    }

    #[test]
    fn completion_marker_rejects_an_unauthenticated_exit_status() {
        let root = env::temp_dir().join(format!(
            "centaeris-completion-marker-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock")
                .as_nanos()
        ));
        std::fs::create_dir(&root).expect("create completion root");
        let marker = CompletionMarker {
            path: root.join("shell-exit"),
            root,
            control_path: None,
            secret: "banana".to_string(),
        };
        std::fs::write(&marker.path, "0:banana\n").expect("write forged marker");
        assert_eq!(read_completion_marker(&marker).unwrap(), None);
        std::fs::write(
            &marker.path,
            format!("0:{}\n", completion_digest(&marker.secret, 0)),
        )
        .expect("write authentic marker");
        assert_eq!(read_completion_marker(&marker).unwrap(), Some(0));
    }

    #[test]
    fn local_policy_rejects_a_missing_declared_root() {
        let workspace = env::temp_dir().join(format!(
            "centaeris-policy-root-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock")
                .as_nanos()
        ));
        std::fs::create_dir(&workspace).expect("create workspace");
        let mut policy = SandboxPolicy::workspace_write_no_network(&workspace);
        policy
            .filesystem
            .read_only_roots
            .push(workspace.join("banana"));

        let error = validate_local_policy(&workspace, &policy)
            .expect_err("missing declared root must loud-fail");
        assert!(matches!(error, SandboxErr::Denied { .. }));
        std::fs::remove_dir_all(workspace).expect("remove workspace");
    }

    #[test]
    fn local_policy_rejects_the_whole_os_temporary_directory() {
        let workspace = env::temp_dir().join(format!(
            "centaeris-policy-root-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock")
                .as_nanos()
        ));
        std::fs::create_dir(&workspace).expect("create workspace");
        let mut policy = SandboxPolicy::workspace_write_no_network(&workspace);
        policy.filesystem.tmp_root = Some(env::temp_dir());

        let error = validate_local_policy(&workspace, &policy)
            .expect_err("the whole OS temporary directory must loud-fail");
        assert!(matches!(error, SandboxErr::Denied { .. }));
        std::fs::remove_dir_all(workspace).expect("remove workspace");
    }

    #[test]
    fn local_sandbox_rejects_network_allowlist_without_running_the_command() {
        let workspace = env::temp_dir().join(format!(
            "centaeris-allowlist-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock")
                .as_nanos()
        ));
        std::fs::create_dir(&workspace).expect("create workspace");
        let marker = workspace.join("must-not-exist");
        let runner = LocalExecutionHostRunner::new(None).expect("create local runner");
        let error = runner
            .run_host_command(
                None,
                SandboxTransformRequest {
                    program: "bash".to_string(),
                    args: vec!["-c".to_string(), "printf RAN > must-not-exist".to_string()],
                    cwd: workspace.clone(),
                    env: std::collections::HashMap::new(),
                    timeout_ms: 1_000,
                    policy: SandboxPolicy::workspace_write_with_network_allowlist(
                        &workspace,
                        vec!["example.com".to_string()],
                    ),
                },
                None,
            )
            .expect_err("unsupported allowlist must loud-fail");

        assert!(matches!(error, SandboxErr::Unavailable { .. }));
        assert!(!marker.exists());
        std::fs::remove_dir_all(workspace).expect("remove workspace");
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn background_process_inheriting_output_returns_after_parent_shell_exits() {
        let marker_path = env::temp_dir().join(format!(
            "centaeris-local-background-{}-{}.txt",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock")
                .as_nanos()
        ));
        let bash_marker_path = marker_path.to_string_lossy().replace('\\', "/");
        let mut command = Command::new(resolve_bash_path(None).expect("resolve Bash"));
        command
            .args([
                "-c",
                "printf 'DONE\\n'; ( sleep 0.3; printf ALIVE > \"$1\" ) &",
                "_",
                bash_marker_path.as_str(),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        configure_local_process(&mut command);

        let started = Instant::now();
        let output = run_local_command_with_timeout(
            &mut command,
            5_000,
            None,
            None,
            platform::sandbox_type(),
            None,
        )
        .expect("background command must return after the parent shell exits");

        assert!(!output.timed_out);
        assert_eq!(output.exit_code, Some(0));
        assert_eq!(output.stdout, "DONE\n");
        assert!(started.elapsed() < Duration::from_secs(1));

        let marker_deadline = Instant::now() + Duration::from_secs(2);
        while !marker_path.exists() && Instant::now() < marker_deadline {
            sleep(PROCESS_POLL_INTERVAL);
        }
        assert_eq!(
            std::fs::read_to_string(&marker_path).expect("background marker"),
            "ALIVE"
        );
        std::fs::remove_file(marker_path).expect("remove background marker");
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn foreground_shell_and_its_process_group_stop_at_command_timeout() {
        let mut command = Command::new(resolve_bash_path(None).expect("resolve Bash"));
        command
            .args(["-c", "sleep 30 & wait"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        configure_local_process(&mut command);

        let started = Instant::now();
        let output = run_local_command_with_timeout(
            &mut command,
            250,
            None,
            None,
            platform::sandbox_type(),
            None,
        )
        .expect("foreground command must terminate at deadline");

        assert!(output.timed_out);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn cancellation_terminates_an_in_flight_local_process() {
        let mut command = Command::new(resolve_bash_path(None).expect("resolve Bash"));
        command
            .args(["-c", "sleep 30 & wait"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        configure_local_process(&mut command);
        let cancellation_probe = || Ok(Some("user_interrupt".to_string()));

        let started = Instant::now();
        let error = match run_local_command_with_timeout(
            &mut command,
            30_000,
            Some(&cancellation_probe),
            None,
            platform::sandbox_type(),
            None,
        ) {
            Ok(_) => panic!("cancelled process must not report a completed tool result"),
            Err(error) => error,
        };

        assert!(error.is_cancellation_indeterminate());
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
