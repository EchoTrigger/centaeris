#[cfg(test)]
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
#[cfg(test)]
use std::fs;

#[cfg(test)]
use super::edit::execute_edit;
#[cfg(test)]
use super::mutation::{acquire_file_write_lease, sha256_bytes};
#[cfg(test)]
use super::read::ReadToolHandler;
#[cfg(test)]
use super::test_support::{
    context_with_file_mutation_commit, FailingFileMutationCommitPort,
    RecordingFileMutationCommitPort,
};
#[cfg(test)]
use super::write::execute_write;
use super::{
    bash_local_tool_output, extract_string_arg, parse_tool_args, LocalToolError, LocalToolHandler,
    LocalToolOutput, ToolRuntimeContext, EXECUTION_CANCELLATION_INDETERMINATE,
};
use crate::execution::sandbox::{normalize_process_output, SandboxErr};
use crate::execution::{ExecutionHostFailureKind, ExecutionHostMode, MAX_EXECUTION_TIMEOUT_MS};
#[cfg(test)]
use crate::session::reliability::{
    AcquireResourceClaimDisposition, AcquireResourceClaimRequest, AcquireResourceClaimResult,
    ReleaseResourceClaimRequest, ResourceClaimRecord, ResourceClaimStorePort,
};
use crate::tool::ToolFailureKind;

const BASH_DEFAULT_TIMEOUT_MS: u64 = 60_000;
const BASH_MAX_TIMEOUT_MS: u64 = MAX_EXECUTION_TIMEOUT_MS;

#[derive(Debug)]
pub(super) struct BashToolHandler {}

impl BashToolHandler {
    pub(super) fn new() -> Self {
        Self {}
    }
}

impl LocalToolHandler for BashToolHandler {
    fn name(&self) -> &'static str {
        "bash"
    }

    fn invoke(
        &self,
        args_json: &str,
        runtime_context: &ToolRuntimeContext,
    ) -> Result<LocalToolOutput, LocalToolError> {
        execute_bash(args_json, runtime_context)
    }
}

fn execute_bash(
    args_json: &str,
    runtime_context: &ToolRuntimeContext,
) -> Result<LocalToolOutput, LocalToolError> {
    let args = parse_tool_args("bash", args_json)?;
    validate_bash_args(&args)?;
    let command = extract_string_arg(&args, &["command"])
        .ok_or_else(|| "command is required for Bash".to_string())?;
    let binding = runtime_context.execution_host_binding()?;
    let timeout_ms = bash_timeout_ms(&args)?;
    let (program, program_args, bash_dialect) = bash_program_and_args(command.as_str());
    let execution = match binding.run_command(
        program,
        program_args,
        std::collections::HashMap::new(),
        timeout_ms,
    ) {
        Ok(execution) => execution,
        Err(err) => {
            return Ok(bash_execution_host_error_result(
                command.as_str(),
                bash_dialect,
                ".",
                timeout_ms,
                binding.mode(),
                "run_host_command",
                &err,
            ));
        }
    };
    let execution_process = execution.process;
    let failure_kind = execution.failure_kind;
    let input_state_changes = execution.input_state_changes;
    let executed = matches!(
        failure_kind,
        ExecutionHostFailureKind::None
            | ExecutionHostFailureKind::CommandFailed
            | ExecutionHostFailureKind::TimedOut
            | ExecutionHostFailureKind::Cancelled
    );
    let mut runtime_diagnostics = execution_process.runtime_diagnostics;
    let output = normalize_process_output(execution_process.stdout, execution_process.stderr);
    runtime_diagnostics.extend(output.diagnostics);
    let stdout_chars = output.stdout.chars().count();
    let stderr_chars = output.stderr.chars().count();
    let resolved_matches_stdout = output.stdout.clone();
    let mut payload = json!({
        "schema": "bash_result_v1",
        "executed": executed,
        "command": command,
        "bashDialect": bash_dialect,
        "cwd": ".",
        "timeoutMs": timeout_ms,
        "exitCode": execution_process.exit_code,
        "timedOut": execution_process.timed_out,
        "stdout": output.stdout,
        "stderr": output.stderr,
        "stdoutChars": stdout_chars,
        "stderrChars": stderr_chars,
        "stdoutDecode": execution_process.stdout_decode,
        "stderrDecode": execution_process.stderr_decode,
        "runtimeDiagnostics": runtime_diagnostics,
        "inputStateChanges": input_state_changes,
        "executionHost": {
            "mode": binding.mode(),
            "failureKind": failure_kind,
            "attempt": execution_process.attempt,
        },
    });
    attach_resolved_input_matches(
        &mut payload,
        command.as_str(),
        resolved_matches_stdout.as_str(),
        runtime_context,
    );
    Ok(bash_local_tool_output(payload))
}

fn attach_resolved_input_matches(
    payload: &mut Value,
    command: &str,
    stdout: &str,
    runtime_context: &ToolRuntimeContext,
) {
    let Some(manifest) = runtime_context.resolved_input_manifest.as_deref() else {
        return;
    };
    if !command.split_whitespace().any(|token| {
        token
            .trim_matches(|character| matches!(character, '\'' | '"'))
            .replace('\\', "/")
            .rsplit('/')
            .next()
            .map(|name| name.eq_ignore_ascii_case("rga"))
            .unwrap_or(false)
    }) {
        return;
    }
    let mut matches = Vec::new();
    for line in stdout.lines() {
        if matches.len() >= 80 {
            break;
        }
        let Some((raw_path, remainder)) = line.trim().split_once(':') else {
            continue;
        };
        let Ok(Some(input)) = manifest.input_by_virtual_path(raw_path) else {
            continue;
        };
        let line_number = remainder
            .split_once(':')
            .and_then(|(value, _)| value.parse::<u64>().ok());
        if matches.iter().any(|item: &Value| {
            item.get("inputRef").and_then(Value::as_str) == Some(input.input_ref.as_str())
                && item.get("line").and_then(Value::as_u64) == line_number
        }) {
            continue;
        }
        matches.push(json!({
            "inputRef": input.input_ref,
            "objectRef": input.object_ref,
            "virtualPath": input.virtual_path,
            "displayName": input.display_name,
            "evidenceKind": input.evidence_kind,
            "line": line_number,
        }));
    }
    if let Some(object) = payload.as_object_mut() {
        object.insert("resolvedInputMatches".to_string(), Value::Array(matches));
    }
}

fn bash_execution_host_error_result(
    command: &str,
    bash_dialect: &str,
    cwd: &str,
    timeout_ms: u64,
    execution_host_mode: ExecutionHostMode,
    phase: &str,
    err: &SandboxErr,
) -> LocalToolOutput {
    let raw_detail = err.internal_debug_message();
    let diagnostic_id = execution_host_diagnostic_id(phase, raw_detail.as_str());
    let tool_failure_kind = execution_host_tool_failure_kind(err);
    let mut output = bash_local_tool_output(json!({
        "schema": "bash_result_v1",
        "executed": false,
        "command": command,
        "bashDialect": bash_dialect,
        "cwd": cwd,
        "timeoutMs": timeout_ms,
        "exitCode": Value::Null,
        "timedOut": false,
        "stdout": "",
        "stderr": "",
        "runtimeDiagnostics": [
            {
                "source": "execution_host",
                "stream": "internal",
                "severity": "error",
                "code": format!("bash_{phase}_failed"),
                "message": raw_detail,
                "diagnosticId": diagnostic_id,
            }
        ],
        "executionHost": {
            "mode": execution_host_mode,
            "failureKind": execution_host_failure_kind(err),
            "phase": phase,
        },
        "toolError": {
            "kind": tool_failure_kind.as_str(),
            "modelMessage": err.model_visible_message(),
            "userMessage": err.user_visible_message(),
            "diagnosticId": diagnostic_id,
            "retryable": matches!(
                tool_failure_kind,
                ToolFailureKind::SandboxUnavailable
                    | ToolFailureKind::HostUnavailable
            ),
        },
    }));
    if err.is_cancellation_indeterminate() {
        output.transition_reason = EXECUTION_CANCELLATION_INDETERMINATE.to_string();
    }
    output
}

fn execution_host_tool_failure_kind(err: &SandboxErr) -> ToolFailureKind {
    match err {
        SandboxErr::Denied { .. } => ToolFailureKind::PermissionDenied,
        SandboxErr::Unavailable {
            sandbox_type: None, ..
        } => ToolFailureKind::HostUnavailable,
        SandboxErr::Unavailable { .. } => ToolFailureKind::SandboxUnavailable,
        SandboxErr::CancellationIndeterminate { .. } => ToolFailureKind::HostUnavailable,
        SandboxErr::Io(_) => ToolFailureKind::HostUnavailable,
    }
}

fn execution_host_failure_kind(err: &SandboxErr) -> ExecutionHostFailureKind {
    match err {
        SandboxErr::Denied { .. } => ExecutionHostFailureKind::SandboxUnavailable,
        SandboxErr::Unavailable {
            sandbox_type: None, ..
        } => ExecutionHostFailureKind::HostUnavailable,
        SandboxErr::Unavailable { .. } => ExecutionHostFailureKind::SandboxUnavailable,
        SandboxErr::CancellationIndeterminate { .. } => ExecutionHostFailureKind::HostUnavailable,
        SandboxErr::Io(_) => ExecutionHostFailureKind::HostUnavailable,
    }
}

fn bash_timeout_ms(args: &Value) -> Result<u64, String> {
    let Some(value) = args.get("timeout_ms") else {
        return Ok(BASH_DEFAULT_TIMEOUT_MS);
    };
    let timeout_ms = value
        .as_u64()
        .ok_or_else(|| "timeout_ms must be a positive integer for Bash".to_string())?;
    if timeout_ms == 0 || timeout_ms > BASH_MAX_TIMEOUT_MS {
        return Err(format!(
            "timeout_ms must be between 1 and {BASH_MAX_TIMEOUT_MS} for Bash"
        ));
    }
    Ok(timeout_ms)
}

fn validate_bash_args(args: &Value) -> Result<(), String> {
    let object = args
        .as_object()
        .ok_or_else(|| "Bash arguments must be a JSON object".to_string())?;
    for key in object.keys() {
        if !matches!(key.as_str(), "command" | "description" | "timeout_ms") {
            return Err(format!("Bash arguments contain unknown field: {key}"));
        }
    }
    if args.get("command").is_some_and(|value| !value.is_string()) {
        return Err("Bash command must be a string".to_string());
    }
    if let Some(description) = args.get("description") {
        let description = description
            .as_str()
            .ok_or_else(|| "Bash description must be a string".to_string())?;
        let length = description.chars().count();
        if description.trim().is_empty() || length > 160 {
            return Err("Bash description must contain 1 to 160 characters".to_string());
        }
    }
    Ok(())
}

fn execution_host_diagnostic_id(phase: &str, raw_detail: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"bash_execution_host_error_v1");
    hasher.update(phase.as_bytes());
    hasher.update(raw_detail.as_bytes());
    let hex = format!("{:x}", hasher.finalize());
    format!("execution-host:{}", &hex[..16])
}

fn bash_program_and_args(command: &str) -> (String, Vec<String>, &'static str) {
    (
        "bash".to_string(),
        vec!["-c".to_string(), command.to_string()],
        "bash",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct TestResourceClaimStore {
        claim: Mutex<Option<ResourceClaimRecord>>,
    }

    impl ResourceClaimStorePort for TestResourceClaimStore {
        fn acquire_resource_claim(
            &self,
            req: AcquireResourceClaimRequest,
        ) -> Result<AcquireResourceClaimResult, String> {
            let mut claim = self.claim.lock().map_err(|_| "claim lock poisoned")?;
            let disposition = match claim.as_ref() {
                Some(existing) if existing.owner != req.owner => {
                    return Ok(AcquireResourceClaimResult {
                        disposition: AcquireResourceClaimDisposition::Conflict,
                        claim: existing.clone(),
                    });
                }
                Some(_) => AcquireResourceClaimDisposition::AlreadyOwned,
                None => AcquireResourceClaimDisposition::Acquired,
            };
            let record = ResourceClaimRecord {
                resource_kind: req.resource_kind,
                resource_key: req.resource_key,
                owner: req.owner,
                owner_kind: req.owner_kind,
                session_id: req.session_id,
                branch_id: req.branch_id,
                expires_at_ms: req.now_ms + i64::try_from(req.ttl_ms).unwrap(),
                metadata_json: req.metadata_json,
                created_at_ms: req.now_ms,
                updated_at_ms: req.now_ms,
            };
            *claim = Some(record.clone());
            Ok(AcquireResourceClaimResult {
                disposition,
                claim: record,
            })
        }

        fn get_resource_claim(
            &self,
            resource_kind: &str,
            resource_key: &str,
        ) -> Result<Option<ResourceClaimRecord>, String> {
            let claim = self.claim.lock().map_err(|_| "claim lock poisoned")?;
            Ok(claim
                .as_ref()
                .filter(|claim| {
                    claim.resource_kind == resource_kind && claim.resource_key == resource_key
                })
                .cloned())
        }

        fn release_resource_claim(&self, req: ReleaseResourceClaimRequest) -> Result<bool, String> {
            let mut claim = self.claim.lock().map_err(|_| "claim lock poisoned")?;
            if claim.as_ref().is_some_and(|claim| {
                claim.resource_kind == req.resource_kind
                    && claim.resource_key == req.resource_key
                    && claim.owner == req.owner
            }) {
                *claim = None;
                return Ok(true);
            }
            Ok(false)
        }

        fn reclaim_expired_resource_claims(&self, now_ms: i64) -> Result<usize, String> {
            let mut claim = self.claim.lock().map_err(|_| "claim lock poisoned")?;
            if claim
                .as_ref()
                .is_some_and(|claim| claim.expires_at_ms <= now_ms)
            {
                *claim = None;
                return Ok(1);
            }
            Ok(0)
        }
    }

    fn test_file_hash(path: &std::path::Path) -> String {
        sha256_bytes(fs::read(path).expect("read test file for hash").as_slice())
    }

    fn test_path_identity(path: &std::path::Path) -> String {
        path.to_string_lossy().to_string()
    }

    #[test]
    fn bash_command_enables_pipefail_before_login_command() {
        let (program, args, dialect) = bash_program_and_args("false | true");

        assert_eq!(program, "bash");
        assert_eq!(args, vec!["-c", "false | true"]);
        assert_eq!(dialect, "bash");
    }

    #[test]
    fn remote_host_unavailable_is_a_structured_bash_failure() {
        let error = SandboxErr::Unavailable {
            reason: "execution service stopped".to_string(),
            sandbox_type: None,
        };
        let output = bash_execution_host_error_result(
            "printf should-not-run",
            "bash",
            ".",
            1_000,
            ExecutionHostMode::Remote,
            "run",
            &error,
        );
        let result = output.details;

        assert_eq!(result["executed"], false);
        assert_eq!(result["executionHost"]["mode"], "remote");
        assert_eq!(result["toolError"]["kind"], "host_unavailable");
        assert!(result["toolError"]["modelMessage"]
            .as_str()
            .unwrap_or_default()
            .contains("execution service stopped"));
    }

    #[test]
    fn identified_sandbox_unavailable_refuses_to_degrade() {
        let error = SandboxErr::Unavailable {
            reason: "backend stopped".to_string(),
            sandbox_type: Some(crate::execution::sandbox::SandboxType::Gvisor),
        };
        let output = bash_execution_host_error_result(
            "printf should-not-run",
            "bash",
            ".",
            1_000,
            ExecutionHostMode::Remote,
            "run",
            &error,
        );

        assert_eq!(output.details["toolError"]["kind"], "sandbox_unavailable");
        assert!(output.details["toolError"]["modelMessage"]
            .as_str()
            .unwrap_or_default()
            .contains("refusing to degrade"));
    }

    #[test]
    fn bash_timeout_defaults_to_sixty_seconds_and_accepts_one_hour() {
        assert_eq!(
            bash_timeout_ms(&json!({ "command": "sleep 30" })).expect("default timeout"),
            BASH_DEFAULT_TIMEOUT_MS
        );
        assert_eq!(
            bash_timeout_ms(&json!({
                "command": "sleep 3600",
                "timeout_ms": 3_600_000
            }))
            .expect("explicit one-hour timeout"),
            BASH_MAX_TIMEOUT_MS
        );
    }

    #[test]
    fn bash_timeout_rejects_invalid_values_without_clamping() {
        assert!(bash_timeout_ms(&json!({ "timeout_ms": 0 })).is_err());
        assert!(bash_timeout_ms(&json!({ "timeout_ms": 3_600_001 })).is_err());
        assert!(bash_timeout_ms(&json!({ "timeout_ms": "60000" })).is_err());
    }

    #[test]
    fn bash_description_is_optional_but_exactly_bounded_when_present() {
        assert!(validate_bash_args(&json!({
            "command": "cargo test",
            "description": "Run the focused Core gate"
        }))
        .is_ok());
        assert!(validate_bash_args(&json!({ "command": "true", "description": " " })).is_err());
        assert!(validate_bash_args(&json!({
            "command": "true",
            "description": "x".repeat(161)
        }))
        .is_err());
    }

    #[test]
    fn bash_args_reject_removed_background_fields() {
        let error = validate_bash_args(&json!({
            "command": "sleep 60",
            "runInBackground": true
        }))
        .expect_err("removed background field must fail");

        assert!(error.contains("unknown field: runInBackground"));
    }

    #[test]
    fn bash_args_reject_model_selected_cwd_and_environment() {
        assert!(validate_bash_args(&json!({ "command": "pwd", "cwd": "/tmp" })).is_err());
        assert!(validate_bash_args(&json!({
            "command": "env",
            "env": { "HOME": "/tmp" }
        }))
        .is_err());
    }

    #[test]
    fn file_write_lease_rejects_concurrent_owner_for_same_path() {
        let path = std::env::temp_dir().join(format!(
            "centaeris-file-write-lease-{}",
            crate::runtime::contracts::current_timestamp_ms()
        ));
        let identity = test_path_identity(path.as_path());
        let first = acquire_file_write_lease(identity.as_str(), "task-a")
            .expect("first lease should succeed");
        let error = acquire_file_write_lease(identity.as_str(), "task-b")
            .expect_err("second lease should fail while first is held");
        assert!(error.contains("file write conflict"));
        assert!(error.contains("task-a"));
        drop(first);

        let second = acquire_file_write_lease(identity.as_str(), "task-b")
            .expect("lease should be available after first drops");
        drop(second);
    }

    #[test]
    fn write_tool_rejects_file_with_existing_write_lease() {
        let workspace_root = std::env::temp_dir().join(format!(
            "centaeris-write-lease-workspace-{}",
            crate::runtime::contracts::current_timestamp_ms()
        ));
        fs::create_dir_all(workspace_root.as_path()).expect("create workspace root");
        let workspace_root = workspace_root
            .canonicalize()
            .expect("canonicalize workspace root");
        let target = workspace_root.join("conflict.txt");
        let target_identity = test_path_identity(target.as_path());
        let lease = acquire_file_write_lease(target_identity.as_str(), "task-a")
            .expect("pre-existing lease should be created");
        let runtime_context = ToolRuntimeContext::with_cwd(workspace_root.clone())
            .expect("workspace root context")
            .with_execution_owner("task-b");

        let error = execute_write(
            json!({
                "path": "conflict.txt",
                "content": "second writer"
            })
            .to_string()
            .as_str(),
            &runtime_context,
        )
        .expect_err("write should fail while another owner holds the lease");

        drop(lease);
        fs::remove_dir_all(workspace_root.as_path()).expect("cleanup workspace root");
        assert!(error.content.contains("file write conflict"));
        assert!(error.details.to_string().contains("task-a"));
    }

    #[test]
    fn write_tool_requires_read_snapshot_when_overwriting_existing_file() {
        let workspace_root = std::env::temp_dir().join(format!(
            "centaeris-write-stale-guard-workspace-{}",
            crate::runtime::contracts::current_timestamp_ms()
        ));
        fs::create_dir_all(workspace_root.as_path()).expect("create workspace root");
        let workspace_root = workspace_root
            .canonicalize()
            .expect("canonicalize workspace root");
        fs::write(workspace_root.join("existing.txt"), "original").expect("write existing file");
        let runtime_context = context_with_file_mutation_commit(
            ToolRuntimeContext::with_cwd(workspace_root.clone())
                .expect("workspace root context")
                .with_execution_owner("task-a"),
            "call-write-stale-hash",
            "write",
        );

        let error = execute_write(
            json!({
                "path": "existing.txt",
                "content": "replacement"
            })
            .to_string()
            .as_str(),
            &runtime_context,
        )
        .expect_err("overwrite without a prior read should fail");

        fs::remove_dir_all(workspace_root.as_path()).expect("cleanup workspace root");
        assert!(error.content.contains("file mutation rejected"));
        assert!(error.content.contains("read the existing file"));
        assert!(error
            .details
            .to_string()
            .contains("read snapshot is required"));
    }

    #[test]
    fn write_tool_does_not_apply_when_file_mutation_commit_fails() {
        let workspace_root = std::env::temp_dir().join(format!(
            "centaeris-write-commit-fail-workspace-{}",
            crate::runtime::contracts::current_timestamp_ms()
        ));
        fs::create_dir_all(workspace_root.as_path()).expect("create workspace root");
        let workspace_root = workspace_root
            .canonicalize()
            .expect("canonicalize workspace root");
        let runtime_context = ToolRuntimeContext::with_cwd(workspace_root.clone())
            .expect("workspace root context")
            .with_execution_owner("task-a")
            .with_tool_invocation("call-write-commit-fail", "write")
            .with_file_mutation_commit_port(Arc::new(FailingFileMutationCommitPort));

        let error = execute_write(
            json!({
                "path": "commit-failed.txt",
                "content": "must not apply"
            })
            .to_string()
            .as_str(),
            &runtime_context,
        )
        .expect_err("commit failure should fail before filesystem apply");

        assert!(error
            .content
            .contains("file mutation durable commit failed"));
        assert!(
            !workspace_root.join("commit-failed.txt").exists(),
            "file must not be created when durable commit fails"
        );
        fs::remove_dir_all(workspace_root.as_path()).expect("cleanup workspace root");
    }

    #[test]
    fn write_tool_rejects_stale_read_snapshot_and_accepts_current_snapshot() {
        let workspace_root = std::env::temp_dir().join(format!(
            "centaeris-write-stale-hash-workspace-{}",
            crate::runtime::contracts::current_timestamp_ms()
        ));
        fs::create_dir_all(workspace_root.as_path()).expect("create workspace root");
        let workspace_root = workspace_root
            .canonicalize()
            .expect("canonicalize workspace root");
        let target = workspace_root.join("existing.txt");
        fs::write(target.as_path(), "original").expect("write existing file");
        let current_hash = test_file_hash(target.as_path());
        let commit_port = RecordingFileMutationCommitPort::shared();
        let runtime_context = ToolRuntimeContext::with_cwd(workspace_root.clone())
            .expect("workspace root context")
            .with_execution_owner("task-a")
            .with_tool_invocation("call-write-stale", "write")
            .with_file_mutation_commit_port(commit_port.clone());
        runtime_context
            .record_file_read_snapshot(test_path_identity(target.as_path()).as_str(), current_hash)
            .expect("record initial read snapshot");
        fs::write(target.as_path(), "external change").expect("mutate after read");

        let error = execute_write(
            json!({
                "path": "existing.txt",
                "content": "replacement"
            })
            .to_string()
            .as_str(),
            &runtime_context,
        )
        .expect_err("stale read snapshot should fail");
        assert!(error.content.contains("changed since the last read"));

        fs::write(target.as_path(), "original").expect("restore source");
        let current_hash = test_file_hash(target.as_path());
        runtime_context
            .record_file_read_snapshot(test_path_identity(target.as_path()).as_str(), current_hash)
            .expect("record current read snapshot");

        let output = execute_write(
            json!({
                "path": "existing.txt",
                "content": "replacement"
            })
            .to_string()
            .as_str(),
            &runtime_context,
        )
        .expect("current read snapshot should allow overwrite");
        let payload = output.details;
        assert_eq!(payload.get("created").and_then(Value::as_bool), Some(false));
        assert_eq!(
            payload.get("fileHash").and_then(Value::as_str),
            Some(sha256_bytes(b"replacement").as_str())
        );
        assert_eq!(payload.get("addedLines").and_then(Value::as_u64), Some(1));
        assert_eq!(payload.get("removedLines").and_then(Value::as_u64), Some(1));
        let diff_preview = payload
            .get("diffPreview")
            .and_then(Value::as_str)
            .expect("write diff preview");
        assert!(diff_preview.contains("-original"));
        assert!(diff_preview.contains("+replacement"));
        let requests = commit_port.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].tool_call_id, "call-write-stale");
        assert_eq!(requests[0].tool_name, "write");
        assert_eq!(requests[0].operation, "overwrite");
        assert_eq!(requests[0].added_lines, Some(1));
        assert_eq!(requests[0].removed_lines, Some(1));

        fs::remove_dir_all(workspace_root.as_path()).expect("cleanup workspace root");
    }

    #[test]
    fn file_tools_reject_absolute_paths_outside_policy_roots() {
        let test_id = crate::runtime::contracts::current_timestamp_ms();
        let workspace_root =
            std::env::temp_dir().join(format!("centaeris-runtime-tmp-workspace-{test_id}"));
        fs::create_dir_all(workspace_root.as_path()).expect("create workspace root");
        let workspace_root = workspace_root
            .canonicalize()
            .expect("canonicalize workspace root");
        let runtime_context = context_with_file_mutation_commit(
            ToolRuntimeContext::with_cwd(workspace_root.clone())
                .expect("workspace root context")
                .with_execution_owner("task-a"),
            "call-write-runtime-tmp",
            "write",
        );

        let outside_path = workspace_root
            .parent()
            .expect("workspace parent")
            .join(format!("centaeris-runtime-outside-{test_id}.txt"));
        fs::write(&outside_path, "outside evidence\n").expect("create outside fixture");
        let write_error = execute_write(
            json!({
                "path": outside_path.with_extension("created.txt").to_string_lossy(),
                "content": "outside evidence\n"
            })
            .to_string()
            .as_str(),
            &runtime_context,
        )
        .expect_err("sandbox policy must reject an outside write");
        assert_eq!(write_error.error.kind, ToolFailureKind::PermissionDenied);

        let read_error = ReadToolHandler::new()
            .invoke(
                json!({ "path": outside_path.to_string_lossy() })
                    .to_string()
                    .as_str(),
                &runtime_context,
            )
            .expect_err("sandbox policy must reject an outside read");

        assert_eq!(read_error.error.kind, ToolFailureKind::PermissionDenied);
        fs::remove_file(outside_path).expect("cleanup outside file");
        fs::remove_dir_all(workspace_root.as_path()).expect("cleanup workspace root");
    }

    #[test]
    fn write_tool_allows_hidden_workspace_directory() {
        let test_id = crate::runtime::contracts::current_timestamp_ms();
        let workspace_root =
            std::env::temp_dir().join(format!("centaeris-hidden-write-workspace-{test_id}"));
        fs::create_dir_all(workspace_root.as_path()).expect("create workspace root");
        let workspace_root = workspace_root
            .canonicalize()
            .expect("canonicalize workspace root");
        let runtime_context = context_with_file_mutation_commit(
            ToolRuntimeContext::with_cwd(workspace_root.clone())
                .expect("workspace root context")
                .with_execution_owner("task-a"),
            "call-edit-stale-hash",
            "edit",
        );

        execute_write(
            json!({
                "path": ".workspace-cache/example.txt",
                "content": "example\n"
            })
            .to_string()
            .as_str(),
            &runtime_context,
        )
        .expect("hidden workspace directories should not be denied by name");

        assert!(workspace_root
            .join(".workspace-cache")
            .join("example.txt")
            .is_file());
        fs::remove_dir_all(workspace_root.as_path()).expect("cleanup workspace root");
    }

    #[test]
    fn edit_tool_requires_read_snapshot_for_update() {
        let workspace_root = std::env::temp_dir().join(format!(
            "centaeris-edit-missing-snapshot-workspace-{}",
            crate::runtime::contracts::current_timestamp_ms()
        ));
        fs::create_dir_all(workspace_root.as_path()).expect("create workspace root");
        let workspace_root = workspace_root
            .canonicalize()
            .expect("canonicalize workspace root");
        fs::write(workspace_root.join("edit.txt"), "alpha\nbeta\n").expect("write source");
        let runtime_context = context_with_file_mutation_commit(
            ToolRuntimeContext::with_cwd(workspace_root.clone())
                .expect("workspace root context")
                .with_execution_owner("task-a"),
            "call-edit-stale",
            "edit",
        );
        let error = execute_edit(
            json!({
                "path": "edit.txt",
                "edits": [{
                    "old_text": "alpha\nbeta",
                    "new_text": "alpha\nbravo"
                }]
            })
            .to_string()
            .as_str(),
            &runtime_context,
        )
        .expect_err("edit without a prior read should fail");

        fs::remove_dir_all(workspace_root.as_path()).expect("cleanup workspace root");
        assert!(error.content.contains("file mutation rejected"));
        assert!(error.content.contains("read the existing file"));
        assert!(error
            .details
            .to_string()
            .contains("read snapshot is required"));
    }

    #[test]
    fn edit_tool_rejects_stale_read_snapshot_and_accepts_current_snapshot() {
        let workspace_root = std::env::temp_dir().join(format!(
            "centaeris-edit-stale-snapshot-workspace-{}",
            crate::runtime::contracts::current_timestamp_ms()
        ));
        fs::create_dir_all(workspace_root.as_path()).expect("create workspace root");
        let workspace_root = workspace_root
            .canonicalize()
            .expect("canonicalize workspace root");
        let target = workspace_root.join("edit.txt");
        fs::write(target.as_path(), "alpha\nbeta\n").expect("write source");
        let current_hash = test_file_hash(target.as_path());
        let runtime_context = context_with_file_mutation_commit(
            ToolRuntimeContext::with_cwd(workspace_root.clone())
                .expect("workspace root context")
                .with_execution_owner("task-a"),
            "call-edit-current-hash",
            "edit",
        );
        runtime_context
            .record_file_read_snapshot(test_path_identity(target.as_path()).as_str(), current_hash)
            .expect("record initial read snapshot");
        fs::write(target.as_path(), "alpha\nexternal\n").expect("mutate after read");
        let error = execute_edit(
            json!({
                "path": "edit.txt",
                "edits": [{
                    "old_text": "alpha\nbeta",
                    "new_text": "alpha\nbravo"
                }]
            })
            .to_string()
            .as_str(),
            &runtime_context,
        )
        .expect_err("stale read snapshot should fail");
        assert!(error.content.contains("changed since the last read"));

        fs::write(target.as_path(), "alpha\nbeta\n").expect("restore edit source");
        let current_hash = test_file_hash(target.as_path());
        runtime_context
            .record_file_read_snapshot(test_path_identity(target.as_path()).as_str(), current_hash)
            .expect("record current read snapshot");
        let output = execute_edit(
            json!({
                "path": "edit.txt",
                "edits": [{
                    "old_text": "alpha\nbeta",
                    "new_text": "alpha\nbravo"
                }]
            })
            .to_string()
            .as_str(),
            &runtime_context,
        )
        .expect("current read snapshot should allow edit");
        let payload = output.details;
        assert_eq!(
            payload.get("schema").and_then(Value::as_str),
            Some("edit_result_v1")
        );
        let diff_preview = payload
            .get("diffPreview")
            .and_then(Value::as_str)
            .expect("edit diff preview");
        assert!(diff_preview.contains("-beta"));
        assert!(diff_preview.contains("+bravo"));
        assert_eq!(
            fs::read_to_string(target.as_path()).expect("read edited file"),
            "alpha\nbravo\n"
        );

        fs::remove_dir_all(workspace_root.as_path()).expect("cleanup workspace root");
    }

    #[test]
    fn edit_tool_applies_disjoint_replacements_atomically_with_one_commit() {
        let workspace_root = std::env::temp_dir().join(format!(
            "centaeris-edit-atomic-workspace-{}-{}",
            std::process::id(),
            crate::runtime::contracts::current_timestamp_ms()
        ));
        fs::create_dir_all(workspace_root.as_path()).expect("create workspace root");
        let workspace_root = workspace_root
            .canonicalize()
            .expect("canonicalize workspace root");
        let target = workspace_root.join("edit.txt");
        let original = "alpha\none\nmiddle\ntwo\nomega\n";
        fs::write(target.as_path(), original).expect("write source");
        let current_hash = test_file_hash(target.as_path());
        let commit_port = RecordingFileMutationCommitPort::shared();
        let runtime_context = ToolRuntimeContext::with_cwd(workspace_root.clone())
            .expect("workspace root context")
            .with_execution_owner("task-a")
            .with_tool_invocation("call-edit-atomic", "edit")
            .with_file_mutation_commit_port(commit_port.clone());
        runtime_context
            .record_file_read_snapshot(test_path_identity(target.as_path()).as_str(), current_hash)
            .expect("record read snapshot");

        let output = execute_edit(
            json!({
                "path": "edit.txt",
                "edits": [
                    { "old_text": "two", "new_text": "second" },
                    { "old_text": "one", "new_text": "first" }
                ]
            })
            .to_string()
            .as_str(),
            &runtime_context,
        )
        .expect("atomic edit");

        assert_eq!(output.details["replacementsApplied"], 2);
        assert_eq!(
            fs::read_to_string(target.as_path()).expect("read edited file"),
            "alpha\nfirst\nmiddle\nsecond\nomega\n"
        );
        let requests = commit_port.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].tool_call_id, "call-edit-atomic");

        fs::remove_dir_all(workspace_root.as_path()).expect("cleanup workspace root");
    }

    #[test]
    fn edit_tool_rejects_missing_ambiguous_or_overlapping_batches_before_commit() {
        let workspace_root = std::env::temp_dir().join(format!(
            "centaeris-edit-atomic-reject-workspace-{}-{}",
            std::process::id(),
            crate::runtime::contracts::current_timestamp_ms()
        ));
        fs::create_dir_all(workspace_root.as_path()).expect("create workspace root");
        let workspace_root = workspace_root
            .canonicalize()
            .expect("canonicalize workspace root");
        let target = workspace_root.join("edit.txt");
        let original = "aaa alpha beta gamma delta delta\n";
        fs::write(target.as_path(), original).expect("write source");
        let current_hash = test_file_hash(target.as_path());
        let commit_port = RecordingFileMutationCommitPort::shared();
        let runtime_context = ToolRuntimeContext::with_cwd(workspace_root.clone())
            .expect("workspace root context")
            .with_execution_owner("task-a")
            .with_tool_invocation("call-edit-atomic-reject", "edit")
            .with_file_mutation_commit_port(commit_port.clone());
        runtime_context
            .record_file_read_snapshot(test_path_identity(target.as_path()).as_str(), current_hash)
            .expect("record read snapshot");

        let invalid_batches = [
            json!({
                "path": "edit.txt",
                "edits": [
                    { "old_text": "alpha", "new_text": "ALPHA" },
                    { "old_text": "missing", "new_text": "present" }
                ]
            }),
            json!({
                "path": "edit.txt",
                "edits": [{ "old_text": "delta", "new_text": "DELTA" }]
            }),
            json!({
                "path": "edit.txt",
                "edits": [{ "old_text": "aa", "new_text": "AA" }]
            }),
            json!({
                "path": "edit.txt",
                "edits": [
                    { "old_text": "alpha beta", "new_text": "first" },
                    { "old_text": "beta gamma", "new_text": "second" }
                ]
            }),
        ];
        for args in invalid_batches {
            let error = execute_edit(args.to_string().as_str(), &runtime_context)
                .expect_err("invalid atomic batch must fail");
            assert_eq!(
                error
                    .details
                    .pointer("/toolError/fileErrorKind")
                    .and_then(Value::as_str),
                Some("edit_text_mismatch")
            );
            assert_eq!(
                fs::read_to_string(target.as_path()).expect("read unchanged file"),
                original
            );
        }
        assert!(commit_port.requests().is_empty());

        fs::remove_dir_all(workspace_root.as_path()).expect("cleanup workspace root");
    }

    #[test]
    fn builtin_tool_handlers_validate_canonical_argument_shapes() {
        let workspace_root = std::env::temp_dir().join(format!(
            "centaeris-tool-invalid-arguments-workspace-{}-{}",
            std::process::id(),
            crate::runtime::contracts::current_timestamp_ms()
        ));
        fs::create_dir_all(workspace_root.as_path()).expect("create workspace root");
        fs::write(workspace_root.join("source.txt"), "alpha\n").expect("write source");
        let runtime_context =
            ToolRuntimeContext::with_cwd(workspace_root.clone()).expect("workspace root context");

        assert!(ReadToolHandler::new()
            .invoke(
                json!({ "path": "source.txt", "banana": "unknown-value" })
                    .to_string()
                    .as_str(),
                &runtime_context,
            )
            .is_err());
        assert!(execute_write(
            json!({ "path": "new.txt", "content": "new", "banana": "unknown-value" })
                .to_string()
                .as_str(),
            &runtime_context,
        )
        .is_err());
        assert!(execute_write(
            json!({
                "path": "new.txt",
                "content": "new",
                "expectedFileHash": "sha256:model-must-not-supply-this"
            })
            .to_string()
            .as_str(),
            &runtime_context,
        )
        .is_err());
        assert!(execute_edit(
            json!({
                "path": "source.txt",
                "edits": [{ "old_text": "alpha", "new_text": "beta" }],
                "banana": "unknown-value"
            })
            .to_string()
            .as_str(),
            &runtime_context,
        )
        .is_err());
        assert!(execute_edit(
            json!({
                "path": "source.txt",
                "edits": [{ "old_text": "alpha", "new_text": "beta" }],
                "fileHash": "sha256:model-must-not-supply-this"
            })
            .to_string()
            .as_str(),
            &runtime_context,
        )
        .is_err());
        assert!(execute_edit(
            json!({ "path": "source.txt", "banana": "unknown-value" })
                .to_string()
                .as_str(),
            &runtime_context,
        )
        .is_err());
        assert!(execute_edit(
            json!({
                "path": "source.txt",
                "edits": [{ "old_text": "alpha", "new_text": "beta", "banana": "unknown-value" }]
            })
            .to_string()
            .as_str(),
            &runtime_context,
        )
        .is_err());
        let too_many_edits = (0..=crate::tool::EDIT_MAX_ITEMS)
            .map(|index| json!({ "old_text": format!("old-{index}"), "new_text": "new" }))
            .collect::<Vec<_>>();
        assert!(execute_edit(
            json!({ "path": "source.txt", "edits": too_many_edits })
                .to_string()
                .as_str(),
            &runtime_context,
        )
        .is_err());
        assert!(validate_bash_args(&json!({
            "command": "true",
            "banana": "unknown-value"
        }))
        .is_err());

        fs::remove_dir_all(workspace_root.as_path()).expect("cleanup workspace root");
    }

    #[test]
    fn edit_tool_reports_structured_exact_text_mismatch() {
        let workspace_root = std::env::temp_dir().join(format!(
            "centaeris-edit-text-mismatch-workspace-{}",
            crate::runtime::contracts::current_timestamp_ms()
        ));
        fs::create_dir_all(workspace_root.as_path()).expect("create workspace root");
        let workspace_root = workspace_root
            .canonicalize()
            .expect("canonicalize workspace root");
        let target = workspace_root.join("edit.txt");
        fs::write(target.as_path(), "alpha\nbeta\n").expect("write source");
        let current_hash = test_file_hash(target.as_path());
        let runtime_context = context_with_file_mutation_commit(
            ToolRuntimeContext::with_cwd(workspace_root.clone())
                .expect("workspace root context")
                .with_execution_owner("task-a"),
            "call-edit-text-mismatch",
            "edit",
        );
        runtime_context
            .record_file_read_snapshot(test_path_identity(target.as_path()).as_str(), current_hash)
            .expect("record read snapshot");

        let error = execute_edit(
            json!({
                "path": "edit.txt",
                "edits": [{
                    "old_text": "missing text",
                    "new_text": "replacement text"
                }]
            })
            .to_string()
            .as_str(),
            &runtime_context,
        )
        .expect_err("non-matching exact edit should fail");
        let payload = error.details;

        fs::remove_dir_all(workspace_root.as_path()).expect("cleanup workspace root");
        assert_eq!(
            payload
                .pointer("/toolError/fileErrorKind")
                .and_then(Value::as_str),
            Some("edit_text_mismatch")
        );
        assert_eq!(
            payload.pointer("/toolError/kind").and_then(Value::as_str),
            Some("invalid_input")
        );
    }

    #[test]
    fn write_tool_rejects_file_with_existing_durable_claim() {
        let test_id = crate::runtime::contracts::current_timestamp_ms();
        let workspace_root =
            std::env::temp_dir().join(format!("centaeris-write-durable-claim-workspace-{test_id}"));
        fs::create_dir_all(workspace_root.as_path()).expect("create workspace root");
        let workspace_root = workspace_root
            .canonicalize()
            .expect("canonicalize workspace root");
        let target = workspace_root.join("conflict.txt");
        let resource_key = test_path_identity(target.as_path());
        let store = Arc::new(TestResourceClaimStore::default());
        store
            .acquire_resource_claim(AcquireResourceClaimRequest {
                resource_kind: "file".to_string(),
                resource_key: resource_key.clone(),
                owner: "task-a".to_string(),
                owner_kind: "tool_runtime".to_string(),
                session_id: None,
                branch_id: None,
                now_ms: crate::runtime::contracts::current_timestamp_ms(),
                ttl_ms: 30_000,
                metadata_json: "{}".to_string(),
            })
            .expect("pre-existing durable claim should be created");
        let runtime_context = ToolRuntimeContext::with_cwd(workspace_root.clone())
            .expect("workspace root context")
            .with_execution_owner("task-b")
            .with_resource_claim_store(store.clone());

        let error = execute_write(
            json!({
                "path": "conflict.txt",
                "content": "second writer"
            })
            .to_string()
            .as_str(),
            &runtime_context,
        )
        .expect_err("write should fail while another owner holds the durable claim");

        fs::remove_dir_all(workspace_root.as_path()).expect("cleanup workspace root");
        assert!(error.content.contains("file write conflict"));
        assert!(error.details.to_string().contains("task-a"));
    }

    #[test]
    fn write_tool_releases_durable_claim_after_success() {
        let test_id = crate::runtime::contracts::current_timestamp_ms();
        let workspace_root = std::env::temp_dir().join(format!(
            "centaeris-write-durable-release-workspace-{test_id}"
        ));
        fs::create_dir_all(workspace_root.as_path()).expect("create workspace root");
        let workspace_root = workspace_root
            .canonicalize()
            .expect("canonicalize workspace root");
        let target = workspace_root.join("result.txt");
        let resource_key = test_path_identity(target.as_path());
        let store = Arc::new(TestResourceClaimStore::default());
        let runtime_context = context_with_file_mutation_commit(
            ToolRuntimeContext::with_cwd(workspace_root.clone())
                .expect("workspace root context")
                .with_execution_owner("task-a")
                .with_resource_claim_store(store.clone()),
            "call-write-durable-release",
            "write",
        );

        execute_write(
            json!({
                "path": "result.txt",
                "content": "durable claim release"
            })
            .to_string()
            .as_str(),
            &runtime_context,
        )
        .expect("write should succeed");

        let claim = store
            .get_resource_claim("file", resource_key.as_str())
            .expect("load claim");
        fs::remove_dir_all(workspace_root.as_path()).expect("cleanup workspace root");
        assert!(
            claim.is_none(),
            "successful write should release the durable claim"
        );
    }
}
