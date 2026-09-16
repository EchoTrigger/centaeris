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
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod nono_policy;
#[cfg(target_os = "linux")]
use linux as platform;
#[cfg(target_os = "macos")]
use macos as platform;
#[cfg(unix)]
use std::os::unix::{io::AsRawFd, process::CommandExt};
const EXIT_STDIO_GRACE: Duration = Duration::from_millis(100);
const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(25);
#[cfg(any(target_os = "macos", test))]
const POLICY_HANDSHAKE: &[u8] = b"centaeris-nono-applied\n";
use centaeris_core::execution::{
    decode_process_output, ExecutionAttempt, ExecutionCommandRequest, ExecutionError,
    ExecutionPolicy, ExecutionPolicySummary, ExecutionProcessOutput,
};

use centaeris_core::execution::run_direct_execution_file_system_operation;
#[cfg(test)]
use centaeris_core::execution::run_policy_scoped_execution_file_system_operation;
use centaeris_core::execution::{
    classify_execution_host_failure, ExecutionCancellationProbe, ExecutionFileSystemError,
    ExecutionFileSystemOutput, ExecutionFileSystemRequest, ExecutionHostCommandOutput,
    ExecutionHostHealth, ExecutionHostKind, ExecutionHostRunner, ExecutionHostStatus,
};
use sha2::{Digest, Sha256};

const LOCAL_FILESYSTEM_FRAME_LIMIT_BYTES: usize = 64 * 1024 * 1024;
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
    #[cfg(target_os = "linux")]
    scope: Option<linux::ProcessScope>,
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

impl Drop for CompletionMarker {
    fn drop(&mut self) {
        cleanup_completion_marker(self);
    }
}

#[cfg(target_os = "linux")]
pub fn run_linux_supervisor(arguments: &[String]) -> Result<i32, String> {
    linux::run_supervisor(arguments)
}

#[cfg(target_os = "macos")]
pub fn run_macos_launcher(arguments: &[String]) -> Result<(), String> {
    macos::run_launcher(arguments)
}

impl LocalExecutionHostRunner {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn spawn_owned_process(
        &self,
        program: String,
        args: Vec<String>,
        cwd: PathBuf,
        mut environment: HashMap<String, String>,
        policy: ExecutionPolicy,
    ) -> Result<LocalOwnedProcess, ExecutionError> {
        environment.extend(self.environment_overrides.clone());
        let req = ExecutionCommandRequest {
            program,
            args,
            cwd,
            env: environment,
            policy,
            timeout_ms: 0,
        };
        validate_local_policy(&req.cwd, &req.policy)?;
        let program = self.resolve_program(&req)?;
        let mut prepared = platform::prepare_command(
            &self.runtime_executable,
            &program,
            &req.args,
            &req.cwd,
            &req.env,
            &req.policy,
            false,
        )?;
        prepared
            .command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        configure_local_process(&mut prepared.command);
        #[cfg(target_os = "linux")]
        let scope = prepared
            .scope
            .take()
            .ok_or_else(|| ExecutionError::PolicyUnavailable {
                reason: "owned Linux process requires a lifecycle scope".to_string(),
            })?;
        let child = prepared
            .command
            .spawn()
            .map_err(|e| ExecutionError::Io(e.to_string()))?;
        Ok(LocalOwnedProcess {
            child,
            #[cfg(target_os = "linux")]
            scope,
        })
    }

    pub fn new(explicit_bash_path: Option<PathBuf>) -> Result<Self, ExecutionError> {
        let runtime_executable =
            env::current_exe().map_err(|error| ExecutionError::PolicyUnavailable {
                reason: format!("resolve local Runtime executable failed: {error}"),
            })?;
        Self::new_with_runtime_executable(explicit_bash_path, runtime_executable)
    }

    pub fn new_with_runtime_executable(
        explicit_bash_path: Option<PathBuf>,
        runtime_executable: PathBuf,
    ) -> Result<Self, ExecutionError> {
        if !runtime_executable.is_file() {
            return Err(ExecutionError::PolicyUnavailable {
                reason: format!(
                    "local Runtime executable is unavailable: {}",
                    runtime_executable.display()
                ),
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
    pub fn new_embedded_test(explicit_bash_path: Option<PathBuf>) -> Result<Self, ExecutionError> {
        let mut runner = Self::new(explicit_bash_path)?;
        runner.embedded_test = true;
        Ok(runner)
    }

    fn resolve_program(&self, req: &ExecutionCommandRequest) -> Result<PathBuf, ExecutionError> {
        if req.program == "bash" {
            return Ok(self.bash_path.clone());
        }
        let requested = Path::new(&req.program);
        if requested.is_absolute() {
            return Ok(requested.to_path_buf());
        }
        if requested.components().count() > 1 {
            return Ok(req.cwd.join(requested));
        }
        let search = req
            .env
            .get("PATH")
            .map(std::ffi::OsString::from)
            .or_else(|| env::var_os("PATH"));
        search
            .and_then(|paths| {
                env::split_paths(&paths)
                    .map(|root| root.join(requested))
                    .find(|path| path.is_file())
            })
            .ok_or_else(|| ExecutionError::HostUnavailable {
                reason: format!("execution program not found: {}", req.program),
            })
    }

    pub fn bash_description(&self) -> &'static str {
        "bash"
    }

    pub fn with_environment_overrides(
        mut self,
        environment_overrides: HashMap<String, String>,
    ) -> Result<Self, ExecutionError> {
        if environment_overrides
            .iter()
            .any(|(key, value)| key.is_empty() || key.contains(['\0', '=']) || value.contains('\0'))
        {
            return Err(ExecutionError::PolicyUnavailable {
                reason: "local execution environment override is invalid".to_string(),
            });
        }
        self.environment_overrides = environment_overrides;
        Ok(self)
    }

    fn run_command(
        &self,
        req: ExecutionCommandRequest,
        cancellation_probe: Option<&ExecutionCancellationProbe>,
    ) -> Result<ExecutionHostCommandOutput, ExecutionError> {
        self.run_command_input(req, cancellation_probe, None)
    }

    /// Feed a hook event while draining output. Each captured stream keeps at
    /// most 64 KiB; decode summaries retain the original byte counts.
    pub fn run_command_with_stdin(
        &self,
        req: ExecutionCommandRequest,
        input: &[u8],
    ) -> Result<ExecutionHostCommandOutput, ExecutionError> {
        self.run_command_input(req, None, Some(input))
    }

    fn run_command_input(
        &self,
        mut req: ExecutionCommandRequest,
        cancellation_probe: Option<&ExecutionCancellationProbe>,
        input: Option<&[u8]>,
    ) -> Result<ExecutionHostCommandOutput, ExecutionError> {
        req.env.extend(self.environment_overrides.clone());
        let program = self.resolve_program(&req)?;
        validate_local_policy(req.cwd.as_path(), &req.policy)?;
        #[cfg(test)]
        let preserve_background = !self.embedded_test;
        #[cfg(not(test))]
        let preserve_background = true;
        let preserve_background = preserve_background && input.is_none();
        let mut prepared = platform::prepare_command(
            self.runtime_executable.as_path(),
            program.as_path(),
            req.args.as_slice(),
            req.cwd.as_path(),
            &req.env,
            &req.policy,
            preserve_background,
        )?;
        let stdin_input = input
            .map(<[u8]>::to_vec)
            .or_else(|| prepared.stdin_input.take())
            .or_else(|| {
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

        let policy_enforced = platform::policy_enforced();

        let output = run_local_command_with_timeout(
            &mut prepared.command,
            req.timeout_ms,
            cancellation_probe,
            prepared.completion.as_ref(),
            stdin_input.as_deref().map(|bytes| ProcessInput {
                bytes,
                output_limit: input.map(|_| 64 * 1024),
            }),
            #[cfg(target_os = "linux")]
            prepared.scope.take(),
            #[cfg(target_os = "macos")]
            true,
        )?;
        let policy = policy_summary(&req.policy, policy_enforced);
        let process = ExecutionProcessOutput {
            exit_code: output.exit_code,
            stdout: output.stdout,
            stderr: output.stderr,
            stdout_decode: output.stdout_decode,
            stderr_decode: output.stderr_decode,
            timed_out: output.timed_out,
            attempt: ExecutionAttempt {
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

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub struct LocalOwnedProcess {
    child: std::process::Child,
    #[cfg(target_os = "linux")]
    scope: linux::ProcessScope,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl LocalOwnedProcess {
    pub fn id(&self) -> u32 {
        self.child.id()
    }
    pub fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        self.child.try_wait()
    }
    pub fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        self.child.wait()
    }
    pub fn kill(&mut self) -> std::io::Result<()> {
        #[cfg(target_os = "linux")]
        return self
            .scope
            .terminate()
            .map_err(|e| std::io::Error::other(e.internal_debug_message()));
        #[cfg(target_os = "macos")]
        {
            if unsafe { libc::kill(-(self.child.id() as i32), libc::SIGKILL) } == 0 {
                return Ok(());
            }
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                Ok(())
            } else {
                Err(error)
            }
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl Drop for LocalOwnedProcess {
    fn drop(&mut self) {
        if self.kill().is_ok() {
            let _ = self.child.wait();
        }
    }
}

impl ExecutionHostRunner for LocalExecutionHostRunner {
    fn bash_description(&self) -> &'static str {
        self.bash_description()
    }

    fn kind(&self) -> ExecutionHostKind {
        ExecutionHostKind::SandboxedProcess
    }

    fn status(&self, policy: &ExecutionPolicy) -> Result<ExecutionHostStatus, ExecutionError> {
        validate_local_policy(policy.filesystem.workspace_root.as_path(), policy)?;
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        platform::check_available(&self.runtime_executable, policy)?;
        Ok(ExecutionHostStatus {
            kind: self.kind(),
            policy_enforced: platform::policy_enforced(),
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
        self.run_file_system_helper(request)
    }

    fn run_host_command(
        &self,
        _operation_id: Option<&str>,
        req: ExecutionCommandRequest,
        cancellation_probe: Option<&ExecutionCancellationProbe>,
    ) -> Result<ExecutionHostCommandOutput, ExecutionError> {
        self.run_command(req, cancellation_probe)
    }
}

impl LocalExecutionHostRunner {
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
            Some(ProcessInput {
                bytes: input.as_slice(),
                output_limit: None,
            }),
            #[cfg(target_os = "linux")]
            prepared.scope.take(),
            #[cfg(target_os = "macos")]
            true,
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

fn filesystem_sandbox_error(error: ExecutionError) -> ExecutionFileSystemError {
    let kind = match error {
        ExecutionError::Denied { .. } => {
            centaeris_core::execution::ExecutionFileSystemErrorKind::PermissionDenied
        }
        _ => centaeris_core::execution::ExecutionFileSystemErrorKind::HostUnavailable,
    };
    ExecutionFileSystemError::new(kind, error.model_visible_message())
        .with_diagnostic(error.internal_debug_message())
}

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

pub fn resolve_bash_path(explicit_bash_path: Option<PathBuf>) -> Result<PathBuf, ExecutionError> {
    if let Some(path) = explicit_bash_path {
        return executable_file(path.as_path()).ok_or_else(|| ExecutionError::HostUnavailable {
            reason: format!(
                "configured Bash executable is unavailable: {}",
                path.display()
            ),
        });
    }

    {
        if let Some(path) = executable_file(Path::new("/bin/bash")).or_else(|| find_on_path("bash"))
        {
            return Ok(path);
        }
        Err(ExecutionError::HostUnavailable {
            reason: "Bash is required but no Bash executable was found".to_string(),
        })
    }
}

fn executable_file(path: &Path) -> Option<PathBuf> {
    path.is_file().then(|| path.to_path_buf())
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    env::var_os("PATH").and_then(|path| {
        env::split_paths(&path)
            .map(|directory| directory.join(name))
            .find_map(|candidate| executable_file(candidate.as_path()))
    })
}

fn policy_summary(policy: &ExecutionPolicy, policy_enforced: bool) -> ExecutionPolicySummary {
    ExecutionPolicySummary {
        enforced: policy_enforced,
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

fn validate_local_policy(cwd: &Path, policy: &ExecutionPolicy) -> Result<(), ExecutionError> {
    policy
        .network
        .validate()
        .map_err(|reason| ExecutionError::Denied { reason })?;
    let canonical_cwd = cwd
        .canonicalize()
        .map_err(|error| ExecutionError::PolicyUnavailable {
            reason: format!("canonicalize local sandbox working directory failed: {error}"),
        })?;
    let canonical_workspace = policy
        .filesystem
        .workspace_root
        .canonicalize()
        .map_err(|error| ExecutionError::PolicyUnavailable {
            reason: format!("canonicalize local sandbox workspace failed: {error}"),
        })?;
    if canonical_cwd != canonical_workspace {
        return Err(ExecutionError::Denied {
            reason: "local sandbox working directory must equal the policy workspace root"
                .to_string(),
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
        return Err(ExecutionError::Denied {
            reason: "local sandbox policy paths must be absolute".to_string(),
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
            return Err(ExecutionError::Denied {
                reason: format!(
                    "local sandbox filesystem root is not an existing directory: {}",
                    root.display()
                ),
            });
        }
    }
    Ok(())
}

fn materialize_temporary_root(policy: &ExecutionPolicy) -> Result<(), ExecutionError> {
    let Some(root) = policy.filesystem.tmp_root.as_deref() else {
        return Ok(());
    };
    let configured_temp =
        env::temp_dir()
            .canonicalize()
            .map_err(|error| ExecutionError::PolicyUnavailable {
                reason: format!("canonicalize local temporary directory failed: {error}"),
            })?;
    let mut ancestor = root.to_path_buf();
    while !ancestor.exists() {
        if !ancestor.pop() {
            return Err(ExecutionError::Denied {
                reason: format!(
                    "local sandbox temporary root is invalid: {}",
                    root.display()
                ),
            });
        }
    }
    let canonical_ancestor =
        ancestor
            .canonicalize()
            .map_err(|error| ExecutionError::PolicyUnavailable {
                reason: format!("canonicalize local sandbox temporary root failed: {error}"),
            })?;
    if canonical_ancestor == configured_temp || canonical_ancestor.starts_with(&configured_temp) {
        std::fs::create_dir_all(root).map_err(|error| {
            ExecutionError::Io(format!(
                "create local sandbox temporary root failed: {error}"
            ))
        })?;
    } else {
        return Err(ExecutionError::Denied {
            reason: "local sandbox temporary root must remain within the configured OS temporary directory".to_string(),

        });
    }
    let canonical_root =
        root.canonicalize()
            .map_err(|error| ExecutionError::PolicyUnavailable {
                reason: format!("canonicalize materialized sandbox temporary root failed: {error}"),
            })?;
    if canonical_root == configured_temp
        || !canonical_root.starts_with(&configured_temp)
        || !canonical_root.is_dir()
    {
        return Err(ExecutionError::Denied {
            reason: "local sandbox temporary root must be an exact child directory of the configured OS temporary directory".to_string(),

        });
    }
    Ok(())
}

fn platform_transition_reason() -> &'static str {
    #[cfg(target_os = "linux")]
    {
        "linux_nono"
    }
    #[cfg(target_os = "macos")]
    {
        "macos_nono"
    }
}

fn configure_local_process(command: &mut Command) {
    #[cfg(unix)]
    command.process_group(0);
}

struct CapturedProcessOutput {
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
    stdout_decode: centaeris_core::execution::ProcessOutputDecodeSummary,
    stderr_decode: centaeris_core::execution::ProcessOutputDecodeSummary,
    timed_out: bool,
}

#[derive(Clone, Copy)]
struct ProcessInput<'a> {
    bytes: &'a [u8],
    output_limit: Option<usize>,
}

fn run_local_command_with_timeout(
    command: &mut Command,
    timeout_ms: u64,
    cancellation_probe: Option<&ExecutionCancellationProbe>,
    completion: Option<&CompletionMarker>,
    stdin_input: Option<ProcessInput<'_>>,
    #[cfg(target_os = "linux")] scope: Option<linux::ProcessScope>,
    #[cfg(target_os = "macos")] policy_handshake: bool,
) -> Result<CapturedProcessOutput, ExecutionError> {
    let mut child = command
        .spawn()
        .map_err(|error| ExecutionError::Io(format!("spawn local command failed: {error}")))?;
    // Feed input concurrently with output draining: a hook may write more
    // than a pipe buffer before it starts reading its event.
    let stdin_writer = if let Some(input) = stdin_input {
        let mut stdin = child.stdin.take().ok_or_else(|| {
            ExecutionError::Io("local process stdin was not captured".to_string())
        })?;
        let bytes = input.bytes.to_vec();
        Some(std::thread::spawn(move || stdin.write_all(&bytes)))
    } else {
        None
    };
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| ExecutionError::Io("local process stdout was not captured".to_string()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| ExecutionError::Io("local process stderr was not captured".to_string()))?;
    let mut stdout = stdout;
    let mut stderr = stderr;
    #[cfg(unix)]
    if let Err(error) =
        configure_nonblocking_output(&stdout).and_then(|_| configure_nonblocking_output(&stderr))
    {
        {
            #[cfg(target_os = "linux")]
            if let Some(scope) = &scope {
                scope.terminate()?;
            }
            terminate_process_tree(&mut child);
        }
        let _ = child.wait();
        return Err(error);
    }
    let limit = stdin_input.and_then(|input| input.output_limit);
    #[cfg(target_os = "macos")]
    let stdout_limit = limit.map(|limit| {
        limit
            + if policy_handshake {
                POLICY_HANDSHAKE.len()
            } else {
                0
            }
    });
    #[cfg(not(target_os = "macos"))]
    let stdout_limit = limit;
    let mut stdout_output = ReadOutput {
        limit: stdout_limit,
        ..Default::default()
    };
    let mut stderr_output = ReadOutput {
        limit,
        ..Default::default()
    };
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
            Ok::<_, ExecutionError>((stdout_drain, stderr_drain))
        })();
        let (stdout_drain, stderr_drain) = match drain_result {
            Ok(drains) => drains,
            Err(error) => {
                #[cfg(unix)]
                {
                    #[cfg(target_os = "linux")]
                    if let Some(scope) = &scope {
                        scope.terminate()?;
                    }
                    terminate_process_tree(&mut child);
                }
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
                    return Err(ExecutionError::Io(format!(
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
                    #[cfg(unix)]
                    let termination = {
                        #[cfg(target_os = "linux")]
                        if let Some(scope) = &scope {
                            scope.terminate()?;
                        }
                        terminate_cancelled_process(&mut child)
                    };
                    return Err(ExecutionError::CancellationIndeterminate {
                        reason: format!("{reason}; {termination}"),
                    });
                }
            }
        }
        if let Some(exit_at) = child_exit_at {
            let quiet_since = last_output_at
                .filter(|output_at| *output_at > exit_at)
                .unwrap_or(exit_at);
            let input_finished = stdin_writer
                .as_ref()
                .is_none_or(|writer| writer.is_finished());
            if input_finished
                && ((stdout_eof && stderr_eof)
                    || now.duration_since(quiet_since) >= EXIT_STDIO_GRACE)
            {
                break (child_exit_code.flatten(), false);
            }
        }
        if now >= deadline {
            #[cfg(unix)]
            {
                #[cfg(target_os = "linux")]
                if let Some(scope) = &scope {
                    scope.terminate()?;
                }
                terminate_process_tree(&mut child);
            }
            if child_exit_code.is_none() {
                child.wait().map_err(|error| {
                    ExecutionError::Io(format!("wait for timed out local command failed: {error}"))
                })?;
            }
            break (None, true);
        }
        sleep(PROCESS_POLL_INTERVAL);
    };
    #[cfg(target_os = "linux")]
    if !timed_out {
        if let Some(scope) = scope {
            scope.reap_in_background(child);
        }
    }
    if let Some(writer) = stdin_writer.filter(|writer| writer.is_finished()) {
        let result = writer
            .join()
            .map_err(|_| ExecutionError::Io("local stdin writer panicked".into()))?;
        if !timed_out {
            result.map_err(|error| {
                ExecutionError::Io(format!("write local process input failed: {error}"))
            })?;
        }
    }
    #[cfg(target_os = "macos")]
    if policy_handshake {
        strip_policy_handshake(&mut stdout_output)?;
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

fn read_completion_marker(marker: &CompletionMarker) -> Result<Option<i32>, ExecutionError> {
    let value = match std::fs::read_to_string(marker.path.as_path()) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(ExecutionError::Io(format!(
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
fn completion_secret() -> Result<String, ExecutionError> {
    let mut bytes = [0u8; 32];
    #[cfg(unix)]
    {
        use std::io::Read as _;
        std::fs::File::open("/dev/urandom")
            .and_then(|mut file| file.read_exact(&mut bytes))
            .map_err(|error| {
                ExecutionError::Io(format!("generate completion secret failed: {error}"))
            })?;
    }
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
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

#[derive(Default)]
struct ReadOutput {
    bytes: Vec<u8>,
    total_bytes: usize,
    limit: Option<usize>,
}

impl ReadOutput {
    fn append(&mut self, input: &[u8]) {
        self.total_bytes = self.total_bytes.saturating_add(input.len());
        let remaining = self
            .limit
            .map_or(input.len(), |limit| limit.saturating_sub(self.bytes.len()));
        self.bytes
            .extend_from_slice(&input[..input.len().min(remaining)]);
    }
}

#[cfg(any(target_os = "macos", test))]
fn strip_policy_handshake(output: &mut ReadOutput) -> Result<(), ExecutionError> {
    if !output.bytes.starts_with(POLICY_HANDSHAKE) {
        return Err(ExecutionError::PolicyUnavailable {
            reason: "macOS launcher did not confirm nono enforcement".into(),
        });
    }
    output.bytes.drain(..POLICY_HANDSHAKE.len());
    output.total_bytes = output.total_bytes.saturating_sub(POLICY_HANDSHAKE.len());
    Ok(())
}

#[derive(Default)]
struct OutputDrain {
    bytes_read: usize,
    eof: bool,
}

#[cfg(unix)]
fn configure_nonblocking_output(reader: &impl AsRawFd) -> Result<(), ExecutionError> {
    let file_descriptor = reader.as_raw_fd();
    let flags = unsafe { libc::fcntl(file_descriptor, libc::F_GETFL) };
    if flags < 0
        || unsafe { libc::fcntl(file_descriptor, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
    {
        return Err(ExecutionError::Io(format!(
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
) -> Result<OutputDrain, ExecutionError> {
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
                return Err(ExecutionError::Io(format!(
                    "read local process output failed: {error}"
                )))
            }
        }
    }
    Ok(drain)
}

#[cfg(test)]
fn read_all_output(mut reader: impl Read) -> Result<ReadOutput, ExecutionError> {
    let mut output = ReadOutput::default();
    let mut chunk = [0u8; 8 * 1024];
    loop {
        let read = reader.read(&mut chunk).map_err(|error| {
            ExecutionError::Io(format!("read local process output failed: {error}"))
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
    #[test]
    fn policy_handshake_is_required_and_removed_before_decoding_output() {
        let mut output = ReadOutput::default();
        output.append(POLICY_HANDSHAKE);
        output.append(&[0xff, 0xfe, b'O', 0, b'K', 0]);
        strip_policy_handshake(&mut output).unwrap();
        assert_eq!(decode_process_output(&output.bytes).text, "OK");
        assert_eq!(output.total_bytes, 6);
        let mut missing = ReadOutput::default();
        missing.append(b"startup failed");
        assert!(matches!(
            strip_policy_handshake(&mut missing),
            Err(ExecutionError::PolicyUnavailable { .. })
        ));
    }
    #[test]
    fn abandoned_completion_marker_removes_only_its_owned_control_files() {
        let root = env::temp_dir().join(format!(
            "centaeris-abandoned-marker-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let marker = CompletionMarker {
            path: root.join("exit"),
            control_path: Some(root.join("control")),
            root: root.clone(),
            secret: "test-only".into(),
        };
        std::fs::write(&marker.path, "0").unwrap();
        std::fs::write(marker.control_path.as_ref().unwrap(), "stop").unwrap();
        drop(marker);
        let removed = !root.exists();
        if !removed {
            std::fs::remove_dir_all(root).unwrap();
        }
        assert!(
            removed,
            "failed command preparation must not leak supervisor state"
        );
    }
    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn missing_denied_paths_never_create_workspace_directories() {
        let workspace = std::env::temp_dir().join(format!(
            "centaeris-missing-deny-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&workspace).unwrap();
        let missing = workspace.join(".centaeris");
        let mut policy = ExecutionPolicy::workspace_write_no_network(&workspace);
        policy.filesystem.denied_read_paths.push(missing.clone());
        let runner = LocalExecutionHostRunner::new(None).unwrap();
        let result = runner.status(&policy);
        let created = missing.exists();
        std::fs::remove_dir_all(&workspace).unwrap();
        assert!(
            !created,
            "validation must not create a reserved workspace directory"
        );
        assert!(result.is_err());
    }

    use super::*;
    use centaeris_core::execution::ExecutionHostFailureKind;
    use std::io::Cursor;

    #[test]
    fn explicit_missing_bash_path_fails_loudly() {
        let error = resolve_bash_path(Some(PathBuf::from("banana/missing/bash")))
            .expect_err("missing Bash path must fail");
        assert!(matches!(error, ExecutionError::HostUnavailable { .. }));
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
        let mut policy = ExecutionPolicy::workspace_write_no_network(&workspace);
        policy
            .filesystem
            .read_only_roots
            .push(workspace.join("banana"));

        let error = validate_local_policy(&workspace, &policy)
            .expect_err("missing declared root must loud-fail");
        assert!(matches!(error, ExecutionError::Denied { .. }));
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
        let mut policy = ExecutionPolicy::workspace_write_no_network(&workspace);
        policy.filesystem.tmp_root = Some(env::temp_dir());

        let error = validate_local_policy(&workspace, &policy)
            .expect_err("the whole OS temporary directory must loud-fail");
        assert!(matches!(error, ExecutionError::Denied { .. }));
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
                ExecutionCommandRequest {
                    program: "bash".to_string(),
                    args: vec!["-c".to_string(), "printf RAN > must-not-exist".to_string()],
                    cwd: workspace.clone(),
                    env: std::collections::HashMap::new(),
                    timeout_ms: 1_000,
                    policy: ExecutionPolicy::workspace_write_with_network_allowlist(
                        &workspace,
                        vec!["example.com".to_string()],
                    ),
                },
                None,
            )
            .expect_err("unsupported allowlist must loud-fail");

        assert!(matches!(error, ExecutionError::PolicyUnavailable { .. }));
        assert!(!marker.exists());
        std::fs::remove_dir_all(workspace).expect("remove workspace");
    }

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
            None,
            #[cfg(target_os = "linux")]
            None,
            #[cfg(target_os = "macos")]
            false,
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
            None,
            #[cfg(target_os = "linux")]
            None,
            #[cfg(target_os = "macos")]
            false,
        )
        .expect("foreground command must terminate at deadline");

        assert!(output.timed_out);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

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
            None,
            #[cfg(target_os = "linux")]
            None,
            #[cfg(target_os = "macos")]
            false,
        ) {
            Ok(_) => panic!("cancelled process must not report a completed tool result"),
            Err(error) => error,
        };

        assert!(error.is_cancellation_indeterminate());
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
