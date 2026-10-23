//! Private SDK translation; fixed helper frames use protected guest files rather
//! than putting their potentially large binary bytes in adapter JSON.
use super::*;
use crate::e2b_adapter::bulk::{BulkFileRequest, SpoolingAdapterProcess, MAX_TRANSFER_BYTES};
use crate::e2b_adapter::{AdapterFailure, AdapterOutcome};
use base64::Engine;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

const CONTROL_TIMEOUT: Duration = Duration::from_secs(30);
const HELPER_PATH: &str = "/opt/centaeris/bin/execution_agent";

pub(crate) struct E2bSdkTransport {
    ordinary: Arc<SpoolingAdapterProcess>,
    control: Arc<SpoolingAdapterProcess>,
}

impl E2bSdkTransport {
    pub(crate) fn new(
        ordinary: Arc<SpoolingAdapterProcess>,
        control: Arc<SpoolingAdapterProcess>,
    ) -> Self {
        Self { ordinary, control }
    }

    fn control_helper(
        &self,
        binding: &E2bSandboxBinding,
        kind: TrustedHelperKind,
    ) -> Result<E2bCommandOutput, E2bHostError> {
        if !matches!(
            kind,
            TrustedHelperKind::PrepareWorkspace
                | TrustedHelperKind::AgentProcessState
                | TrustedHelperKind::QuiesceAgentProcesses
        ) {
            return Err(E2bHostError::NotDispatched(
                "e2b_control_helper_unsupported",
            ));
        }
        let result = call(
            &self.control,
            "invokeControlHelper",
            json!({"sandboxId":binding.sandbox_id,"helper":kind.entrypoint(),"expectedTemplateId":binding.template_id,"expectedMetadata":binding.metadata,"workspaceRoot":"/mnt/data","workspaceUser":"agent","timeoutSeconds":20,"maxOutputBytes":COMMAND_OUTPUT_LIMIT}),
            CONTROL_TIMEOUT,
        )?;
        let receipt: RawCommandReceipt = serde_json::from_value(result)
            .map_err(|_| E2bHostError::Unknown("e2b_control_receipt_invalid"))?;
        if receipt.helper.as_deref() != Some(kind.entrypoint()) {
            return Err(E2bHostError::Unknown("e2b_control_receipt_invalid"));
        }
        receipt.output(&binding.sandbox_id, None)
    }

    fn shell(
        &self,
        binding: &E2bSandboxBinding,
        script: String,
        user: &str,
        envs: Value,
        timeout: Duration,
    ) -> Result<i32, E2bHostError> {
        let seconds = seconds(timeout)?;
        if script.is_empty() || script.len() > COMMAND_OUTPUT_LIMIT || script.contains('\0') {
            return Err(E2bHostError::NotDispatched("e2b_shell_request_invalid"));
        }
        let result = call(
            &self.ordinary,
            "runCommand",
            json!({"sandboxId":binding.sandbox_id,"command":script,"user":user,"cwd":"/mnt/data","envs":envs,"timeoutSeconds":seconds}),
            CONTROL_TIMEOUT,
        )?;
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Start {
            sandbox_id: String,
            pid: i32,
        }
        let receipt: Start = serde_json::from_value(result)
            .map_err(|_| E2bHostError::Unknown("e2b_command_start_invalid"))?;
        if receipt.sandbox_id != binding.sandbox_id || receipt.pid <= 0 {
            return Err(E2bHostError::Unknown("e2b_command_start_invalid"));
        }
        Ok(receipt.pid)
    }

    fn completed_shell(
        &self,
        binding: &E2bSandboxBinding,
        script: String,
        timeout: Duration,
    ) -> Result<(), E2bHostError> {
        let pid = self.shell(binding, script, "root", helper_environment(), timeout)?;
        let output = self.wait(binding, pid, timeout.saturating_add(Duration::from_secs(5)))?;
        if output.exit_code != 0 {
            return Err(E2bHostError::Unknown("e2b_helper_process_failed"));
        }
        Ok(())
    }
    fn helper_frame(
        &self,
        binding: &E2bSandboxBinding,
        kind: TrustedHelperKind,
        input: &[u8],
        output_limit: usize,
        observation: Option<&WorkspaceProtection>,
    ) -> Result<Vec<u8>, E2bHostError> {
        let mut helper_stderr = Vec::new();
        let mut downloaded_bytes = None;
        let result = (|| {
            if matches!(
                kind,
                TrustedHelperKind::PrepareWorkspace
                    | TrustedHelperKind::AgentProcessState
                    | TrustedHelperKind::QuiesceAgentProcesses
            ) {
                if !input.is_empty() {
                    return Err(E2bHostError::NotDispatched(
                        "e2b_control_helper_input_invalid",
                    ));
                }
                let output = self.control_helper(binding, kind)?;
                if output.exit_code != 0 || output.stdout.len() > output_limit {
                    return Err(E2bHostError::Unknown("e2b_control_helper_failed"));
                }
                return Ok(output.stdout);
            }
            if matches!(
                kind,
                TrustedHelperKind::WorkspaceWatch | TrustedHelperKind::WorkspaceGenerationRpc
            ) || input.len() as u64 > kind.input_limit()
                || input.len() as u64 > MAX_TRANSFER_BYTES
                || output_limit == 0
                || output_limit as u64 > kind.output_limit()
                || output_limit as u64 > MAX_TRANSFER_BYTES
            {
                return Err(E2bHostError::NotDispatched("e2b_helper_budget_unsupported"));
            }
            let timeout = Duration::from_millis(kind.timeout_ms());
            let directory = format!("/run/centaeris/e2b-{}", Uuid::new_v4());
            let request_path = format!("{directory}/request");
            let response_path = format!("{directory}/response");
            // No recursive operation: the parent is root0700, the fresh directory
            // is create-only, and all three paths are generated here, never by users.
            self.completed_shell(
                binding,
                format!("umask 077; mkdir -m 700 -- {}", quote(&directory)),
                CONTROL_TIMEOUT,
            )?;
            self.ordinary
                .upload_reader(
                    &BulkFileRequest {
                        sandbox_id: &binding.sandbox_id,
                        path: &request_path,
                        user: Some("root"),
                        max_bytes: input.len() as u64,
                        timeout,
                    },
                    &mut Cursor::new(input),
                )
                .map_err(|_| E2bHostError::Unknown("e2b_helper_input_staging_unconfirmed"))?;
            // Bound only the helper's stdout, independently of files restored or
            // written in the workspace. One extra byte detects a complete but
            // oversized response; the SDK download keeps the original exact cap.
            let capture_limit = output_limit + 1;
            // The root broker opens the request before exec and owns the private
            // output sink. Filesystem operations run as the frozen workload
            // identity, matching Bash writes and restored snapshot ownership.
            let helper = if matches!(kind, TrustedHelperKind::FileSystem) {
                [
                    "/usr/bin/setpriv",
                    "--reuid=10001",
                    "--regid=10001",
                    "--clear-groups",
                    "--",
                    HELPER_PATH,
                ]
                .into_iter()
                .map(quote)
                .collect::<Vec<_>>()
                .join(" ")
            } else {
                quote(HELPER_PATH)
            };
            let script = format!(
                "set -e -o pipefail; ( exec {} {} < {} ) | /usr/bin/head -c {capture_limit} > {}; test \"$(/usr/bin/wc -c < {})\" -le {output_limit}",
                helper,
                quote(kind.entrypoint()),
                quote(&request_path),
                quote(&response_path),
                quote(&response_path)
            );
            let mut environment = helper_environment();
            if let Some(observation) = observation.filter(|value| !value.operation_id().is_empty())
            {
                environment["WORKSPACE_PERF_OBSERVATIONS"] = "1".into();
                environment["WORKSPACE_PERF_OPERATION_ID"] = observation.operation_id().into();
            }
            let pid = self.shell(binding, script, "root", environment, timeout)?;
            let completed =
                self.wait(binding, pid, timeout.saturating_add(Duration::from_secs(5)))?;
            helper_stderr = completed.stderr;
            if completed.exit_code != 0 {
                return Err(E2bHostError::Unknown("e2b_helper_process_failed"));
            }
            let mut output = Vec::new();
            self.ordinary
                .download_into(
                    &BulkFileRequest {
                        sandbox_id: &binding.sandbox_id,
                        path: &response_path,
                        user: Some("root"),
                        max_bytes: output_limit as u64,
                        timeout,
                    },
                    &mut output,
                )
                .map_err(|_| E2bHostError::Unknown("e2b_helper_output_staging_unconfirmed"))?;
            downloaded_bytes = Some(output.len() as u64);
            // Only a verified, fully downloaded result permits normal staging-file
            // cleanup. Unknown branches keep their files for whole-VM finalization.
            self.completed_shell(
                binding,
                format!(
                    "rm -- {} {}; rmdir -- {}",
                    quote(&request_path),
                    quote(&response_path),
                    quote(&directory)
                ),
                CONTROL_TIMEOUT,
            )?;
            Ok(output)
        })();
        if let Some(observation) = observation {
            // The existing strict forwarder accepts only two bound numeric
            // counters. Bounded command stderr is never printed or returned.
            observation.forward_sandbox(&helper_stderr);
            if let Some(bytes) = downloaded_bytes {
                observation.count(ProtectionMetric::SdkDownloadFrameBytes, bytes, true);
            } else {
                // SDK/Python staging and failed partial transfers are not
                // inferred from the intended output cap or current buffer.
                observation.unavailable(ProtectionMetric::SdkDownloadFrameBytes);
            }
        }
        result
    }
}

impl E2bHostTransport for E2bSdkTransport {
    fn inspect(&self, binding: &E2bSandboxBinding) -> Result<E2bSandboxObservation, E2bHostError> {
        // The fixed operation performs fresh GET and validates exact identity,
        // running state, kill policy and literal auto-resume=false before Start.
        let output = self.control_helper(binding, TrustedHelperKind::AgentProcessState)?;
        if output.exit_code != 0 {
            return Err(E2bHostError::Unknown("e2b_helper_health_failed"));
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct State {
            active_process_count: u64,
        }
        let state: State = serde_json::from_slice(&output.stdout)
            .map_err(|_| E2bHostError::Unknown("e2b_helper_health_invalid"))?;
        let _ = state.active_process_count;
        Ok(E2bSandboxObservation {
            binding: binding.clone(),
            running: true,
            on_timeout_kill: true,
            auto_resume: false,
        })
    }
    fn helper(
        &self,
        binding: &E2bSandboxBinding,
        kind: TrustedHelperKind,
        input: &[u8],
        output_limit: usize,
    ) -> Result<Vec<u8>, E2bHostError> {
        self.helper_frame(binding, kind, input, output_limit, None)
    }
    fn helper_observed(
        &self,
        binding: &E2bSandboxBinding,
        kind: TrustedHelperKind,
        input: &[u8],
        output_limit: usize,
        observation: &WorkspaceProtection,
    ) -> Result<Vec<u8>, E2bHostError> {
        self.helper_frame(binding, kind, input, output_limit, Some(observation))
    }
    fn start(
        &self,
        binding: &E2bSandboxBinding,
        request: &ExecutionCommandRequest,
    ) -> Result<i32, E2bHostError> {
        let mut script = format!("exec {}", quote(&request.program));
        for arg in &request.args {
            script.push(' ');
            script.push_str(&quote(arg));
        }
        self.shell(
            binding,
            script,
            "agent",
            serde_json::to_value(&request.env)
                .map_err(|_| E2bHostError::NotDispatched("e2b_command_environment_invalid"))?,
            Duration::from_millis(request.timeout_ms),
        )
    }
    fn wait(
        &self,
        binding: &E2bSandboxBinding,
        pid: i32,
        timeout: Duration,
    ) -> Result<E2bCommandOutput, E2bHostError> {
        let result = call(
            &self.ordinary,
            "waitCommand",
            json!({"sandboxId":binding.sandbox_id,"pid":pid,"timeoutSeconds":seconds(timeout)?,"maxOutputBytes":COMMAND_OUTPUT_LIMIT}),
            timeout,
        )?;
        let receipt: RawCommandReceipt = serde_json::from_value(result)
            .map_err(|_| E2bHostError::Unknown("e2b_command_receipt_invalid"))?;
        if receipt.helper.is_some() {
            return Err(E2bHostError::Unknown("e2b_command_receipt_invalid"));
        }
        receipt.output(&binding.sandbox_id, Some(pid))
    }
    fn quiesce(&self, binding: &E2bSandboxBinding) -> Result<(), E2bHostError> {
        let output = self.control_helper(binding, TrustedHelperKind::QuiesceAgentProcesses)?;
        if output.exit_code != 0 {
            return Err(E2bHostError::Unknown("e2b_quiescence_unconfirmed"));
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RawCommandReceipt {
    sandbox_id: String,
    pid: i32,
    exit_code: i32,
    stdout: String,
    stderr: String,
    stdout_bytes_base64: String,
    stderr_bytes_base64: String,
    stdout_byte_length: usize,
    stderr_byte_length: usize,
    #[serde(default)]
    helper: Option<String>,
}
impl RawCommandReceipt {
    fn output(self, sandbox_id: &str, pid: Option<i32>) -> Result<E2bCommandOutput, E2bHostError> {
        if self.sandbox_id != sandbox_id
            || self.pid <= 0
            || pid.is_some_and(|p| p != self.pid)
            || self
                .stdout_byte_length
                .saturating_add(self.stderr_byte_length)
                > COMMAND_OUTPUT_LIMIT
        {
            return Err(E2bHostError::Unknown("e2b_command_receipt_invalid"));
        }
        let decode = |value: &str| {
            base64::engine::general_purpose::STANDARD
                .decode(value)
                .map_err(|_| E2bHostError::Unknown("e2b_command_bytes_invalid"))
        };
        let stdout = decode(&self.stdout_bytes_base64)?;
        let stderr = decode(&self.stderr_bytes_base64)?;
        if stdout.len() != self.stdout_byte_length || stderr.len() != self.stderr_byte_length {
            return Err(E2bHostError::Unknown("e2b_command_bytes_invalid"));
        }
        // Display strings are not byte evidence and are deliberately discarded.
        let _ = (self.stdout, self.stderr);
        Ok(E2bCommandOutput {
            sandbox_id: self.sandbox_id,
            pid: self.pid,
            exit_code: self.exit_code,
            stdout,
            stderr,
        })
    }
}
fn call(
    process: &SpoolingAdapterProcess,
    operation: &str,
    payload: Value,
    timeout: Duration,
) -> Result<Value, E2bHostError> {
    let response = process
        .call(&Uuid::new_v4().to_string(), operation, payload, timeout)
        .map_err(|error| match error {
            AdapterFailure::NotDispatched(_) => {
                E2bHostError::NotDispatched("e2b_adapter_not_dispatched")
            }
            AdapterFailure::Unknown(_) => E2bHostError::Unknown("e2b_adapter_result_unknown"),
        })?;
    match response.outcome {
        AdapterOutcome::Succeeded => response
            .result
            .ok_or(E2bHostError::Unknown("e2b_adapter_result_missing")),
        AdapterOutcome::NotDispatched => {
            Err(E2bHostError::NotDispatched("e2b_adapter_not_dispatched"))
        }
        AdapterOutcome::Unknown => Err(E2bHostError::Unknown("e2b_adapter_result_unknown")),
    }
}
fn seconds(timeout: Duration) -> Result<u64, E2bHostError> {
    let seconds = timeout.as_millis().div_ceil(1000);
    if seconds == 0 || seconds > 86400 {
        return Err(E2bHostError::NotDispatched("e2b_deadline_invalid"));
    }
    Ok(seconds as u64)
}
fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
fn helper_environment() -> Value {
    json!({"CENTAERIS_WORKSPACE_ROOT":"/mnt/data","CENTAERIS_WORKSPACE_USER":"10001:10001"})
}

#[cfg(test)]
mod tests;
