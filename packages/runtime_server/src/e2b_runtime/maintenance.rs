//! Independently authorized synchronous native SDK commands/files.
//! Dispatch responsibility is durable before an SDK callback; no PID is a task.
use super::*;
use crate::e2b_adapter::bulk::BulkFileRequest;
use crate::e2b_execution_host::E2bHostTransport;
use crate::environment_authorization::provider_maintenance::ProviderMaintenanceAuthorization;
use crate::postgres_store::provider_maintenance::direct::{
    DirectCompletion, DirectDispatchState, DirectOutput, NativeDirectoryEntry,
};
use base64::Engine;
use centaeris_core::execution::ExecutionCommandRequest;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Cursor;
use unicode_normalization::UnicodeNormalization;

pub(crate) const MAX_NATIVE_READ_BYTES: u64 =
    ((crate::MAX_HTTP_BODY_BYTES - 64 * 1024) / 4 * 3) as u64;
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DirectCommand {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: String,
    pub timeout_ms: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub(crate) enum NativeFileOperation {
    WriteFile {
        path: String,
    },
    ReadFile {
        path: String,
        max_bytes: u64,
    },
    ListDirectory {
        path: String,
        recursive: bool,
        max_entries: usize,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct NativeFileFrame {
    pub request: NativeFileOperation,
    pub content_length: u64,
    pub content_digest: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum DirectRequest {
    Command(DirectCommand),
    Filesystem(NativeFileFrame),
}
fn canonical_path(path: &str) -> bool {
    path.starts_with('/')
        && !path.starts_with("//")
        && path.len() <= 4096
        && !path.contains('\\')
        && !path.chars().any(char::is_control)
        && path.nfc().collect::<String>() == path
        && (path == "/"
            || !path
                .split('/')
                .skip(1)
                .any(|p| p.is_empty() || p == "." || p == ".."))
        && !["/run/centaeris", "/run/centaeris-host"]
            .iter()
            .any(|root| path == *root || path.starts_with(&format!("{root}/")))
}
impl DirectRequest {
    pub(crate) fn validate(
        &self,
        auth: &ProviderMaintenanceAuthorization,
        raw: &[u8],
    ) -> Result<(), &'static str> {
        let encoded = crate::agent_run_authorization::canonical_json(
            serde_json::to_value(self).map_err(|_| "provider_maintenance_request_invalid")?,
        )
        .map_err(|_| "provider_maintenance_request_invalid")?;
        if encoded.len() > 64 * 1024
            || format!("sha256:{:x}", Sha256::digest(encoded.as_bytes()))
                != auth.payload.request_digest
        {
            return Err("provider_maintenance_request_binding_rejected");
        }
        match self {
            Self::Command(command) => {
                if !raw.is_empty()
                    || command.program.is_empty()
                    || command.program.contains('\0')
                    || command.args.iter().any(|arg| arg.contains('\0'))
                    || !canonical_path(&command.cwd)
                    || command.timeout_ms == 0
                    || command.timeout_ms > 3_600_000
                {
                    return Err("provider_maintenance_command_invalid");
                }
            }
            Self::Filesystem(frame) => {
                if frame.content_length > 32 * 1024 * 1024
                    || frame.content_length != raw.len() as u64
                    || frame.content_digest != format!("sha256:{:x}", Sha256::digest(raw))
                {
                    return Err("provider_maintenance_file_content_invalid");
                }
                let path = match &frame.request {
                    NativeFileOperation::WriteFile { path } => path,
                    NativeFileOperation::ReadFile { path, max_bytes } => {
                        if frame.content_length != 0
                            || *max_bytes == 0
                            || *max_bytes > MAX_NATIVE_READ_BYTES
                        {
                            return Err("provider_maintenance_file_budget_invalid");
                        }
                        path
                    }
                    NativeFileOperation::ListDirectory {
                        path, max_entries, ..
                    } => {
                        if frame.content_length != 0 || *max_entries == 0 || *max_entries > 10000 {
                            return Err("provider_maintenance_file_budget_invalid");
                        }
                        path
                    }
                };
                if !canonical_path(path) {
                    return Err("provider_maintenance_file_path_invalid");
                }
            }
        }
        Ok(())
    }
}

fn execute_native(
    process: Arc<SpoolingAdapterProcess>,
    profile: &EnvironmentProfile,
    binding: &E2bSandboxBinding,
    request: &DirectRequest,
    raw: &[u8],
    request_timeout: Duration,
) -> Result<DirectOutput, E2bHostError> {
    let transport = E2bSdkTransport::new(process.clone(), process.clone(), profile.clone());
    transport.inspect(binding)?;
    match request {
        DirectRequest::Command(command) => {
            let request = ExecutionCommandRequest {
                program: command.program.clone(),
                args: command.args.clone(),
                cwd: command.cwd.clone().into(),
                env: HashMap::new(),
                timeout_ms: command.timeout_ms,
                policy: crate::e2b_execution_host::execution_policy_for_profile(profile),
            };
            let pid = transport.start(binding, &request)?;
            // This wait covers the original signed command deadline. A short
            // provider identity/request timeout never truncates a 900s verifier.
            let output = transport.wait(
                binding,
                pid,
                Duration::from_millis(command.timeout_ms).saturating_add(Duration::from_secs(5)),
            )?;
            Ok(DirectOutput::Command {
                exit_code: output.exit_code,
                stdout_byte_length: output.stdout.len() as u64,
                stdout_bytes_base64: base64::engine::general_purpose::STANDARD
                    .encode(output.stdout),
                stderr_byte_length: output.stderr.len() as u64,
                stderr_bytes_base64: base64::engine::general_purpose::STANDARD
                    .encode(output.stderr),
            })
        }
        DirectRequest::Filesystem(frame) => match &frame.request {
            NativeFileOperation::WriteFile { path } => {
                let receipt = process
                    .upload_reader(
                        &BulkFileRequest {
                            sandbox_id: &binding.sandbox_id,
                            path,
                            user: Some(profile.user()),
                            max_bytes: raw.len() as u64,
                            timeout: request_timeout,
                        },
                        &mut Cursor::new(raw),
                    )
                    .map_err(|_| {
                        E2bHostError::Unknown("provider_maintenance_file_write_unconfirmed")
                    })?;
                if receipt.byte_length != frame.content_length
                    || format!("sha256:{}", receipt.sha256) != frame.content_digest
                {
                    return Err(E2bHostError::Unknown(
                        "provider_maintenance_file_write_unconfirmed",
                    ));
                }
                Ok(DirectOutput::FileWritten {
                    byte_length: receipt.byte_length,
                    sha256: frame.content_digest.clone(),
                })
            }
            NativeFileOperation::ReadFile { path, max_bytes } => {
                let mut bytes = Vec::new();
                let receipt = process
                    .download_into(
                        &BulkFileRequest {
                            sandbox_id: &binding.sandbox_id,
                            path,
                            user: Some(profile.user()),
                            max_bytes: *max_bytes,
                            timeout: request_timeout,
                        },
                        &mut bytes,
                    )
                    .map_err(|_| {
                        E2bHostError::Unknown("provider_maintenance_file_read_unconfirmed")
                    })?;
                Ok(DirectOutput::FileRead {
                    byte_length: receipt.byte_length,
                    sha256: format!("sha256:{}", receipt.sha256),
                    data_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
                })
            }
            NativeFileOperation::ListDirectory {
                path,
                recursive,
                max_entries,
            } => {
                let response = process
                    .call(
                        &uuid::Uuid::new_v4().to_string(),
                        "listDirectory",
                        json!({"sandboxId":binding.sandbox_id,"path":path,
                    "recursive":recursive,"maxEntries":max_entries,"user":profile.user()}),
                        request_timeout,
                    )
                    .map_err(|_| {
                        E2bHostError::Unknown("provider_maintenance_file_list_unconfirmed")
                    })?;
                if response.outcome != AdapterOutcome::Succeeded {
                    return Err(E2bHostError::Unknown(
                        "provider_maintenance_file_list_unconfirmed",
                    ));
                }
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase", deny_unknown_fields)]
                struct Listed {
                    sandbox_id: String,
                    entries: Vec<NativeDirectoryEntry>,
                }
                let listed: Listed = serde_json::from_value(response.result.ok_or(
                    E2bHostError::Unknown("provider_maintenance_file_list_unconfirmed"),
                )?)
                .map_err(|_| E2bHostError::Unknown("provider_maintenance_file_list_unconfirmed"))?;
                let prefix = format!("{}/", path.trim_end_matches('/'));
                if listed.sandbox_id != binding.sandbox_id
                    || listed.entries.len() > *max_entries
                    || listed.entries.iter().any(|entry| {
                        !canonical_path(&entry.path)
                            || entry.path == *path
                            || !entry.path.starts_with(&prefix)
                            || (!recursive && entry.path[prefix.len()..].contains('/'))
                    })
                {
                    return Err(E2bHostError::Unknown(
                        "provider_maintenance_file_list_unconfirmed",
                    ));
                }
                Ok(DirectOutput::DirectoryListed {
                    entries: listed.entries,
                })
            }
        },
    }
}

impl E2bRuntime {
    pub(crate) fn maintenance_direct(
        &self,
        auth: &ProviderMaintenanceAuthorization,
        key: &[u8],
        request: &DirectRequest,
        raw: &[u8],
    ) -> Result<DirectCompletion, E2bRuntimeError> {
        request
            .validate(auth, raw)
            .map_err(E2bRuntimeError::Rejected)?;
        if !self.config.profile.task_image()
            || auth.payload.provider_target.provider_config_digest
                != self.config.provider_config_digest
            || self
                .config
                .profile
                .digest()
                .map_err(|_| E2bRuntimeError::Rejected("provider_maintenance_profile_rejected"))?
                != auth.payload.profile_digest
        {
            return Err(E2bRuntimeError::Rejected(
                "provider_maintenance_profile_rejected",
            ));
        }
        match self
            .store
            .claim_direct_provider_maintenance(auth, key, &self.config.provider_config_digest)
            .map_err(|_| E2bRuntimeError::Unknown("provider_maintenance_dispatch_unconfirmed"))?
        {
            DirectDispatchState::Completed(value) => return Ok(*value),
            DirectDispatchState::Unknown => {
                return Err(E2bRuntimeError::Unknown(
                    "provider_maintenance_outcome_unknown",
                ))
            }
            DirectDispatchState::Fresh => {}
        }
        // Claim has committed and released its transaction before this launch.
        let process = self
            .launcher
            .launch(&self.config, E2bAdapterLane::Ordinary)?;
        require_capabilities(
            &process,
            E2bAdapterLane::Ordinary,
            self.config.request_timeout,
        )?;
        // Reacquire the existing current Environment/operation pins. No nested
        // claim transaction is held, and command wait uses its own signed timeout.
        let output = self
            .store
            .with_current_provider_maintenance(
                auth,
                key,
                &self.config.provider_config_digest,
                |target| {
                    if target.profile != self.config.profile {
                        return Err(E2bHostError::NotDispatched(
                            "provider_maintenance_profile_rejected",
                        ));
                    }
                    let EnvironmentProfile::E2b(profile) = &target.profile else {
                        unreachable!("validated provider profile")
                    };
                    let binding = E2bSandboxBinding {
                        sandbox_id: target.target.sandbox_id.clone(),
                        template_id: profile.template_ref.template_id.clone(),
                        metadata: expected_metadata(&target.target.identity),
                    };
                    execute_native(
                        process.clone(),
                        &target.profile,
                        &binding,
                        request,
                        raw,
                        self.config.request_timeout,
                    )
                },
            )
            .map_err(|_| E2bRuntimeError::Unknown("provider_maintenance_outcome_unknown"))?;
        let completion = DirectCompletion::new(auth, output)
            .map_err(|_| E2bRuntimeError::Unknown("provider_maintenance_result_unconfirmed"))?;
        self.store
            .record_direct_provider_maintenance_completion(
                auth,
                key,
                &self.config.provider_config_digest,
                &completion,
            )
            .map_err(|_| E2bRuntimeError::Unknown("provider_maintenance_completion_unconfirmed"))?;
        Ok(completion)
    }
}
#[cfg(test)]
mod tests;
