//! Core execution port backed by the pinned E2B SDK transport.
//! Command receipts are runner-local observations, never durable replay authority.
use crate::agent_run_authorization::{EnvironmentProfile, ExecutionNetwork};
use centaeris_core::execution::*;
use hosted_execution::host_supervisor::TrustedHelperKind;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::io::Cursor;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

pub(crate) mod authority;
pub(crate) mod sdk_transport;
#[cfg(test)]
mod tests;

const MAX_COMMAND_EVIDENCE: usize = 128;
const COMMAND_OUTPUT_LIMIT: usize = 64 * 1024;
const CONTROL_PROBE_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct E2bSandboxBinding {
    pub sandbox_id: String,
    pub template_id: String,
    pub metadata: BTreeMap<String, String>,
}

#[derive(Clone, Debug)]
pub(crate) struct E2bSandboxObservation {
    pub binding: E2bSandboxBinding,
    pub running: bool,
    pub on_timeout_kill: bool,
    pub auto_resume: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct E2bCommandOutput {
    pub sandbox_id: String,
    pub pid: i32,
    pub exit_code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum E2bHostError {
    NotDispatched(&'static str),
    Unknown(&'static str),
}

pub(crate) trait E2bHostTransport: Send + Sync {
    fn inspect(&self, binding: &E2bSandboxBinding) -> Result<E2bSandboxObservation, E2bHostError>;
    fn helper(
        &self,
        binding: &E2bSandboxBinding,
        kind: TrustedHelperKind,
        input: &[u8],
        output_limit: usize,
    ) -> Result<Vec<u8>, E2bHostError>;
    fn start(
        &self,
        binding: &E2bSandboxBinding,
        request: &ExecutionCommandRequest,
    ) -> Result<i32, E2bHostError>;
    fn wait(
        &self,
        binding: &E2bSandboxBinding,
        pid: i32,
        timeout: Duration,
    ) -> Result<E2bCommandOutput, E2bHostError>;
    fn quiesce(&self, binding: &E2bSandboxBinding) -> Result<(), E2bHostError>;
}

pub(crate) enum E2bDispatchResult {
    Observation(E2bSandboxObservation),
    Process(i32),
    Helper(Vec<u8>),
}

/// The store implementation must pin current controller and Job authority while
/// invoking this callback. Process waits occur after it returns. There is no
/// permissive production implementation: an in-memory gate cannot fence another
/// Runtime process or a concurrent cleanup owner.
pub(crate) trait E2bDispatchAuthority: Send + Sync {
    fn dispatch(
        &self,
        operation: &mut (dyn FnMut() -> Result<E2bDispatchResult, E2bHostError> + Send),
    ) -> Result<E2bDispatchResult, E2bHostError>;
}

struct CommandRecord {
    digest: String,
    state: Mutex<CommandState>,
}
enum CommandState {
    Prepared,
    Running(i32),
    Completed(Box<ExecutionHostCommandOutput>),
    NotDispatched(&'static str),
    Unknown,
}

pub(crate) struct E2bExecutionHostRunner {
    profile: EnvironmentProfile,
    binding: E2bSandboxBinding,
    policy: ExecutionPolicy,
    transport: Arc<dyn E2bHostTransport>,
    authority: Arc<dyn E2bDispatchAuthority>,
    commands: Mutex<HashMap<String, Arc<CommandRecord>>>,
    unknown_helper_effects: AtomicBool,
}

impl E2bExecutionHostRunner {
    pub(crate) fn new(
        profile: EnvironmentProfile,
        binding: E2bSandboxBinding,
        transport: Arc<dyn E2bHostTransport>,
        authority: Arc<dyn E2bDispatchAuthority>,
    ) -> Result<Self, String> {
        profile.validate()?;
        let EnvironmentProfile::E2b(value) = &profile else {
            return Err("e2b_host_profile_unsupported".into());
        };
        if value.lifetime != "perRun"
            || value.workspace_root != "/mnt/data"
            || value.user != "agent"
            || value.template_ref.template_id != binding.template_id
        {
            return Err("e2b_host_profile_unsupported".into());
        }
        let keys = [
            "centaerisAttemptId",
            "environmentId",
            "instanceId",
            "executionId",
            "agentRunId",
            "sessionId",
            "workspaceId",
            "profileDigest",
            "authorizationDigest",
            "processingJobId",
        ];
        if binding.sandbox_id.is_empty()
            || binding.sandbox_id.len() > 128
            || !binding
                .sandbox_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            || binding.metadata.len() != keys.len()
            || keys.iter().any(|key| {
                binding.metadata.get(*key).is_none_or(|v| {
                    v.is_empty() || v.len() > 4096 || v.chars().any(char::is_control)
                })
            })
            || binding.metadata.get("profileDigest") != Some(&profile.digest()?)
        {
            return Err("e2b_host_binding_invalid".into());
        }
        let mut policy = match profile.network() {
            ExecutionNetwork::Disabled => ExecutionPolicy::workspace_write_no_network("/mnt/data"),
            ExecutionNetwork::PublicInternet => {
                ExecutionPolicy::workspace_write_public_internet("/mnt/data")
            }
        };
        policy.filesystem.denied_read_paths =
            vec!["/run/centaeris".into(), "/run/centaeris-host".into()];
        policy.filesystem.denied_write_paths = policy.filesystem.denied_read_paths.clone();
        Ok(Self {
            profile,
            binding,
            policy,
            transport,
            authority,
            commands: Mutex::new(HashMap::new()),
            unknown_helper_effects: AtomicBool::new(false),
        })
    }

    pub(crate) fn helper_effects_known(&self) -> bool {
        !self.unknown_helper_effects.load(Ordering::Acquire)
    }
    pub(crate) fn execution_policy(&self) -> ExecutionPolicy {
        self.policy.clone()
    }
    pub(crate) fn workspace_root(&self) -> &str {
        self.profile.workspace_root()
    }
    pub(crate) fn execution_id(&self) -> &str {
        &self.binding.metadata["executionId"]
    }
    pub(crate) fn binding(&self) -> &E2bSandboxBinding {
        &self.binding
    }

    fn validate_policy(&self, policy: &ExecutionPolicy) -> Result<(), ExecutionError> {
        if policy != &self.policy {
            return Err(ExecutionError::Denied {
                reason: "e2b_host_policy_mismatch".into(),
            });
        }
        Ok(())
    }

    fn dispatch(
        &self,
        operation: impl FnOnce() -> Result<E2bDispatchResult, E2bHostError> + Send,
    ) -> Result<E2bDispatchResult, E2bHostError> {
        let mut once = Some(operation);
        self.authority.dispatch(&mut || {
            once.take()
                .ok_or(E2bHostError::Unknown("e2b_dispatch_callback_replayed"))?()
        })
    }

    /// An uncertain helper can still be mutating as root. Agent UID quiescence
    /// does not settle it, so this runner never clears this latch from an empty
    /// process observation or a successful later quiescence request.
    pub(crate) fn invoke_helper(
        &self,
        kind: TrustedHelperKind,
        input: &[u8],
        output_limit: usize,
    ) -> Result<Vec<u8>, E2bHostError> {
        if !self.helper_effects_known()
            && !matches!(
                kind,
                TrustedHelperKind::AgentProcessState | TrustedHelperKind::QuiesceAgentProcesses
            )
        {
            return Err(E2bHostError::Unknown("e2b_prior_helper_effects_unknown"));
        }
        if input.len() as u64 > kind.input_limit()
            || output_limit as u64 > kind.output_limit()
            || output_limit == 0
        {
            return Err(E2bHostError::NotDispatched("e2b_helper_budget_invalid"));
        }
        let result = self.dispatch(|| {
            self.transport
                .helper(&self.binding, kind, input, output_limit)
                .map(E2bDispatchResult::Helper)
        });
        match result {
            Ok(E2bDispatchResult::Helper(bytes)) if bytes.len() <= output_limit => Ok(bytes),
            Err(error @ E2bHostError::NotDispatched(_)) => Err(error),
            Err(error @ E2bHostError::Unknown(_)) => {
                self.unknown_helper_effects.store(true, Ordering::Release);
                Err(error)
            }
            _ => {
                self.unknown_helper_effects.store(true, Ordering::Release);
                Err(E2bHostError::Unknown("e2b_helper_result_invalid"))
            }
        }
    }
    fn request_digest(&self, request: &ExecutionCommandRequest) -> Result<String, ExecutionError> {
        self.validate_policy(&request.policy)?;
        let EnvironmentProfile::E2b(profile) = &self.profile else {
            unreachable!("constructor validated provider")
        };
        if request.cwd != std::path::Path::new(self.workspace_root())
            || request.program.is_empty()
            || request.program.contains('\0')
            || request.args.iter().any(|v| v.contains('\0'))
            || request
                .env
                .iter()
                .any(|(k, v)| k.is_empty() || k.contains(['\0', '=']) || v.contains('\0'))
            || request.timeout_ms == 0
            || request.timeout_ms > u64::from(profile.timeout_seconds) * 1000
        {
            return Err(ExecutionError::Denied {
                reason: "e2b_command_request_invalid".into(),
            });
        }
        let env: BTreeMap<_, _> = request.env.iter().collect();
        let bytes = serde_json::to_vec(&(
            &self.binding.sandbox_id,
            &request.program,
            &request.args,
            request.cwd.to_str(),
            env,
            request.timeout_ms,
            &request.policy,
        ))
        .map_err(|_| unavailable("e2b_command_request_invalid"))?;
        Ok(format!("sha256:{:x}", Sha256::digest(bytes)))
    }

    fn record(
        &self,
        operation_id: &str,
        digest: &str,
        create: bool,
    ) -> Result<Arc<CommandRecord>, ExecutionError> {
        if operation_id.is_empty()
            || operation_id.len() > 512
            || operation_id.chars().any(char::is_control)
        {
            return Err(ExecutionError::Denied {
                reason: "e2b_command_operation_invalid".into(),
            });
        }
        let mut records = self
            .commands
            .lock()
            .map_err(|_| unavailable("e2b_command_evidence_unavailable"))?;
        if let Some(record) = records.get(operation_id) {
            if record.digest != digest {
                return Err(ExecutionError::Denied {
                    reason: "e2b_command_operation_binding_mismatch".into(),
                });
            }
            return Ok(record.clone());
        }
        if records.len() >= MAX_COMMAND_EVIDENCE {
            return Err(ExecutionError::CapacityRejected {
                stage: ExecutionOperationStage::Start,
                reason: "e2b_command_evidence_capacity".into(),
            });
        }
        let record = Arc::new(CommandRecord {
            digest: digest.into(),
            // Missing recovery evidence closes this identity as Unknown, so a
            // later direct run call cannot turn that uncertainty into dispatch.
            state: Mutex::new(if create {
                CommandState::Prepared
            } else {
                CommandState::Unknown
            }),
        });
        records.insert(operation_id.into(), record.clone());
        Ok(record)
    }

    fn wait_for_command(
        &self,
        pid: i32,
        request: &ExecutionCommandRequest,
        probe: Option<&ExecutionCancellationProbe>,
    ) -> Result<ExecutionHostCommandOutput, ExecutionError> {
        let timeout =
            Duration::from_millis(request.timeout_ms).saturating_add(Duration::from_secs(5));
        let (sender, receiver) = mpsc::sync_channel(1);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                let _ = sender.send(self.transport.wait(&self.binding, pid, timeout));
            });
            let mut cancelled = false;
            let mut control_failed = false;
            loop {
                match receiver.recv_timeout(CONTROL_PROBE_INTERVAL) {
                    Ok(result) => {
                        if control_failed {
                            return Err(ExecutionError::CancellationIndeterminate {
                                reason: "e2b_command_control_unconfirmed".into(),
                            });
                        }
                        let output =
                            result.map_err(|_| indeterminate("e2b_command_output_unknown"))?;
                        if output.sandbox_id != self.binding.sandbox_id
                            || output.pid != pid
                            || output.stdout.len().saturating_add(output.stderr.len())
                                > COMMAND_OUTPUT_LIMIT
                        {
                            return Err(indeterminate("e2b_command_output_binding_invalid"));
                        }
                        let stdout = decode_process_output(&output.stdout);
                        let stderr = decode_process_output(&output.stderr);
                        let failure_kind = if cancelled {
                            ExecutionHostFailureKind::Cancelled
                        } else if output.exit_code == 0 {
                            ExecutionHostFailureKind::None
                        } else {
                            ExecutionHostFailureKind::CommandFailed
                        };
                        return Ok(ExecutionHostCommandOutput {
                            process: ExecutionProcessOutput {
                                exit_code: Some(output.exit_code),
                                stdout: stdout.text,
                                stderr: stderr.text,
                                stdout_decode: stdout.summary,
                                stderr_decode: stderr.summary,
                                timed_out: false,
                                attempt: ExecutionAttempt {
                                    transition_reason: "e2b_command_completed".into(),
                                    policy: policy_summary(&self.policy),
                                },
                                runtime_diagnostics: Vec::new(),
                            },
                            failure_kind,
                            input_state_changes: Vec::new(),
                        });
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        return Err(indeterminate("e2b_command_wait_unavailable"))
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        if cancelled || control_failed {
                            continue;
                        }
                        let Some(probe) = probe else {
                            continue;
                        };
                        match probe() {
                            Ok(None) => continue,
                            Ok(Some(_)) => cancelled = true,
                            Err(_) => control_failed = true,
                        }
                        if self
                            .dispatch(|| {
                                self.transport
                                    .quiesce(&self.binding)
                                    .map(|()| E2bDispatchResult::Helper(Vec::new()))
                            })
                            .is_err()
                        {
                            control_failed = true;
                        }
                    }
                }
            }
        })
    }
}

impl ExecutionHostRunner for E2bExecutionHostRunner {
    fn kind(&self) -> ExecutionHostKind {
        ExecutionHostKind::RemoteHost
    }
    fn status(&self, policy: &ExecutionPolicy) -> Result<ExecutionHostStatus, ExecutionError> {
        self.validate_policy(policy)?;
        let observed = match self.dispatch(|| {
            self.transport
                .inspect(&self.binding)
                .map(E2bDispatchResult::Observation)
        }) {
            Ok(E2bDispatchResult::Observation(observed)) => observed,
            _ => return Err(unavailable("e2b_host_inspect_unavailable")),
        };
        if observed.binding != self.binding
            || !observed.running
            || !observed.on_timeout_kill
            || observed.auto_resume
        {
            return Err(unavailable("e2b_host_observation_mismatch"));
        }
        Ok(ExecutionHostStatus::remote(
            true,
            ExecutionHostHealth::Ready,
            None,
        ))
    }
    fn run_file_system_operation(
        &self,
        request: ExecutionFileSystemRequest,
    ) -> Result<ExecutionFileSystemOutput, ExecutionFileSystemError> {
        self.validate_policy(&request.policy).map_err(|_| {
            ExecutionFileSystemError::new(
                ExecutionFileSystemErrorKind::PermissionDenied,
                "e2b_host_policy_mismatch",
            )
        })?;
        if request.cwd != std::path::Path::new(self.workspace_root())
            || request.model_path.contains("://")
        {
            return Err(ExecutionFileSystemError::new(
                ExecutionFileSystemErrorKind::InvalidPath,
                "e2b_filesystem_path_unsupported",
            ));
        }
        let mut input = Vec::new();
        hosted_execution::protocol::write_filesystem_request(&mut input, request)
            .map_err(|_| fs_unavailable("e2b_filesystem_request_invalid"))?;
        let limit = TrustedHelperKind::FileSystem.output_limit() as usize;
        let output = self
            .invoke_helper(TrustedHelperKind::FileSystem, &input, limit)
            .map_err(|_| fs_unavailable("e2b_filesystem_helper_unavailable"))?;
        hosted_execution::protocol::read_filesystem_result(&mut Cursor::new(output))
            .map_err(|_| fs_unavailable("e2b_filesystem_frame_invalid"))?
            .into_result()
    }
    fn run_host_command(
        &self,
        operation_id: Option<&str>,
        request: ExecutionCommandRequest,
        probe: Option<&ExecutionCancellationProbe>,
    ) -> Result<ExecutionHostCommandOutput, ExecutionError> {
        let digest = self.request_digest(&request)?;
        let operation_id =
            operation_id.ok_or_else(|| unavailable("e2b_command_operation_required"))?;
        let record = self.record(operation_id, &digest, true)?;
        let mut state = record
            .state
            .lock()
            .map_err(|_| indeterminate("e2b_command_evidence_unavailable"))?;
        match &*state {
            CommandState::Completed(output) => return Ok(*output.clone()),
            CommandState::Running(_) | CommandState::Unknown => {
                return Err(indeterminate("e2b_command_original_unresolved"))
            }
            CommandState::Prepared | CommandState::NotDispatched(_) => (),
        }
        if let Some(probe) = probe {
            match probe() {
                Ok(None) => (),
                Ok(Some(_)) => {
                    *state = CommandState::NotDispatched("e2b_cancelled_before_dispatch");
                    return Err(ExecutionError::Denied {
                        reason: "e2b_cancelled_before_dispatch".into(),
                    });
                }
                Err(_) => {
                    *state = CommandState::NotDispatched("e2b_control_unavailable_before_dispatch");
                    return Err(unavailable("e2b_control_unavailable_before_dispatch"));
                }
            }
        }
        let started = self.dispatch(|| {
            self.transport
                .start(&self.binding, &request)
                .map(E2bDispatchResult::Process)
        });
        let pid = match started {
            Ok(E2bDispatchResult::Process(pid)) if pid > 0 => pid,
            Err(E2bHostError::NotDispatched(reason)) => {
                *state = CommandState::NotDispatched(reason);
                return Err(unavailable(reason));
            }
            _ => {
                *state = CommandState::Unknown;
                return Err(indeterminate("e2b_command_start_unknown"));
            }
        };
        *state = CommandState::Running(pid);
        let result = self.wait_for_command(pid, &request, probe);
        *state = match &result {
            Ok(output) => CommandState::Completed(Box::new(output.clone())),
            Err(_) => CommandState::Unknown,
        };
        result
    }
    fn recover_host_command(
        &self,
        operation_id: &str,
        request: ExecutionCommandRequest,
        _probe: Option<&ExecutionCancellationProbe>,
    ) -> Result<ExecutionHostCommandRecovery, ExecutionError> {
        let digest = self.request_digest(&request)?;
        let record = self.record(operation_id, &digest, false)?;

        let state = record
            .state
            .lock()
            .map_err(|_| indeterminate("e2b_command_evidence_unavailable"))?;
        Ok(match &*state {
            CommandState::Completed(output) => {
                ExecutionHostCommandRecovery::Completed(output.clone())
            }
            CommandState::NotDispatched(reason) => ExecutionHostCommandRecovery::NotAccepted {
                reason: (*reason).into(),
            },
            CommandState::Running(pid) => ExecutionHostCommandRecovery::Unknown {
                reason: format!("e2b_original_command_pending:{pid}"),
            },
            CommandState::Prepared | CommandState::Unknown => {
                ExecutionHostCommandRecovery::Unknown {
                    reason: "e2b_original_command_unresolved".into(),
                }
            }
        })
    }
}

fn unavailable(reason: &str) -> ExecutionError {
    ExecutionError::PolicyUnavailable {
        reason: reason.into(),
    }
}
fn indeterminate(reason: &str) -> ExecutionError {
    ExecutionError::OperationIndeterminate {
        stage: ExecutionOperationStage::Query,
        reason: reason.into(),
    }
}
fn fs_unavailable(reason: &str) -> ExecutionFileSystemError {
    ExecutionFileSystemError::new(ExecutionFileSystemErrorKind::HostUnavailable, reason)
}
fn policy_summary(policy: &ExecutionPolicy) -> ExecutionPolicySummary {
    ExecutionPolicySummary {
        enforced: true,
        network: policy.network.clone(),
        workspace_root: policy
            .filesystem
            .workspace_root
            .to_string_lossy()
            .into_owned(),
        read_only_root_count: policy.filesystem.read_only_roots.len(),
        writable_root_count: policy.filesystem.writable_roots.len(),
        denied_read_path_count: policy.filesystem.denied_read_paths.len(),
        denied_write_path_count: policy.filesystem.denied_write_paths.len(),
    }
}
