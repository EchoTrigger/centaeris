use std::path::PathBuf;
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::ToolExecutionResult;
use crate::execution::{
    ExecutionFileSystemErrorKind, ExecutionFileSystemOperation, ExecutionFileSystemOutput,
    ExecutionHostBinding, ExecutionHostMode,
};
use crate::tool::{ToolErrorInfo, ToolFailureKind};

pub(crate) const MODEL_TOOL_RESULT_MAX_BYTES: usize = 50 * 1024;
const MODEL_PREVIEW_EDGE_BYTES: usize = 24 * 1024;
const CAPTURE_SCHEMA: &str = "temporary_tool_result_capture_v1";
static CAPTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ToolResultCapture {
    pub full_output_path: Option<String>,
    pub output_start_byte: Option<u64>,
    pub output_byte_length: u64,
    pub output_complete: bool,
}

pub(crate) fn seal_tool_result(
    mut result: ToolExecutionResult,
    args_json: &str,
    session_id: Option<&str>,
    execution_host: Option<&ExecutionHostBinding>,
) -> ToolExecutionResult {
    let output_byte_length = result.content.len() as u64;
    if result.content.len() <= MODEL_TOOL_RESULT_MAX_BYTES {
        return result;
    }

    match execution_host
        .ok_or_else(|| "ExecutionHost binding is unavailable".to_string())
        .and_then(|host| spill_tool_result(&result, args_json, session_id, host))
    {
        Ok((path, output_start_byte)) => {
            let preview = bounded_preview(result.content.as_str());
            let capture = ToolResultCapture {
                full_output_path: Some(path.clone()),
                output_start_byte: Some(output_start_byte),
                output_byte_length,
                output_complete: true,
            };
            insert_capture(&mut result.details, &capture);
            result.content = format!(
                "{preview}\n\n[Full tool result: {} | content starts at byte {} | {} bytes. Use read or bash tools to inspect it in sections.]",
                path,
                output_start_byte,
                output_byte_length,
            );
        }
        Err(error) => {
            let capture = ToolResultCapture {
                full_output_path: None,
                output_start_byte: None,
                output_byte_length,
                output_complete: false,
            };
            insert_capture(&mut result.details, &capture);
            result.status = "error".to_string();
            result.content = format!(
                "Tool result capture failed after execution; the complete output is unavailable: {error}"
            );
            result.error = Some(ToolErrorInfo::new(
                ToolFailureKind::HostUnavailable,
                "Tool result capture failed after execution",
                "Tool result capture failed",
            ));
            result.transition_reason = Some("tool_result_capture_failed".to_string());
        }
    }
    result
}

pub(crate) fn tool_result_capture(result: &ToolExecutionResult) -> ToolResultCapture {
    let Some(capture) = result
        .details
        .get("toolResultCapture")
        .and_then(Value::as_object)
    else {
        return ToolResultCapture {
            full_output_path: None,
            output_start_byte: None,
            output_byte_length: result.content.len() as u64,
            output_complete: true,
        };
    };
    ToolResultCapture {
        full_output_path: capture
            .get("fullOutputPath")
            .and_then(Value::as_str)
            .map(str::to_string),
        output_start_byte: capture.get("outputStartByte").and_then(Value::as_u64),
        output_byte_length: capture
            .get("outputByteLength")
            .and_then(Value::as_u64)
            .unwrap_or(result.content.len() as u64),
        output_complete: capture
            .get("outputComplete")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    }
}

fn spill_tool_result(
    result: &ToolExecutionResult,
    args_json: &str,
    session_id: Option<&str>,
    execution_host: &ExecutionHostBinding,
) -> Result<(String, u64), String> {
    let mut spill_policy = execution_host.policy().clone();
    if execution_host.mode() == ExecutionHostMode::Local {
        let capture_root = capture_directory(execution_host.mode(), session_id);
        if spill_policy.filesystem.tmp_root.as_ref() != Some(&capture_root)
            || !spill_policy
                .filesystem
                .read_only_roots
                .contains(&capture_root)
        {
            return Err(
                "Local spill root is not predeclared as read-only by the SandboxPolicy".to_string(),
            );
        }
        if !spill_policy
            .filesystem
            .writable_roots
            .contains(&capture_root)
        {
            spill_policy.filesystem.writable_roots.push(capture_root);
        }
    }
    let spill_host = execution_host.with_policy(spill_policy);
    let input = serde_json::from_str::<Value>(args_json)
        .unwrap_or_else(|_| Value::String(args_json.to_string()));
    let description = input
        .get("description")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let header = serde_json::to_string(&json!({
        "schema": "tool_result_spill_v1",
        "toolName": result.tool_name,
        "callId": result.tool_call_id,
        "description": description,
        "input": input,
    }))
    .map_err(|error| format!("serialize tool result header failed: {error}"))?;
    let prefix = format!("{header}\n--- tool result ---\n");
    let output_start_byte = prefix.len() as u64;
    let footer = serde_json::to_string(&json!({
        "status": result.status,
        "resultState": result.result_state().as_str(),
        "outputByteLength": result.content.len(),
    }))
    .map_err(|error| format!("serialize tool result footer failed: {error}"))?;
    // ponytail: V1 hands one complete buffer to the existing ExecutionHost filesystem
    // contract; add a streaming spill operation when measured outputs exceed Host memory.
    let mut bytes = prefix.into_bytes();
    bytes.extend_from_slice(result.content.as_bytes());
    bytes.extend_from_slice(format!("\n--- tool result metadata ---\n{footer}\n").as_bytes());
    for _ in 0..8 {
        let model_path = create_capture_path(execution_host.mode(), session_id, result)?;
        match spill_host.run_file_system_operation(
            model_path.as_str(),
            ExecutionFileSystemOperation::WriteFile {
                content: bytes.clone(),
                expected_file_hash: None,
                create_only: true,
            },
        ) {
            Ok(ExecutionFileSystemOutput::WriteFile(write)) => {
                return Ok((write.identity.display_path, output_start_byte));
            }
            Ok(_) => return Err("ExecutionHost returned an invalid spill result".to_string()),
            Err(error) if error.kind == ExecutionFileSystemErrorKind::Conflict => continue,
            Err(error) => return Err(format!("ExecutionHost spill failed: {error}")),
        }
    }
    Err("could not allocate a unique tool result path".to_string())
}

pub(super) fn expose_local_capture_root(
    policy: &mut crate::execution::sandbox::SandboxPolicy,
    mode: ExecutionHostMode,
    session_id: Option<&str>,
) {
    if mode != ExecutionHostMode::Local {
        return;
    }
    let root = capture_directory(mode, session_id);
    policy.filesystem.tmp_root = Some(root.clone());
    if !policy.filesystem.read_only_roots.contains(&root) {
        policy.filesystem.read_only_roots.push(root);
    }
}

fn capture_directory(mode: ExecutionHostMode, session_id: Option<&str>) -> PathBuf {
    let session_key = session_id
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("unscoped");
    let digest = Sha256::digest(session_key.as_bytes());
    let session_directory = format!("{:x}", digest)[..24].to_string();
    match mode {
        ExecutionHostMode::Local => std::env::temp_dir()
            .join("agent-tool-results")
            .join(session_directory),
        ExecutionHostMode::Remote => PathBuf::from(".agent-tool-results").join(session_directory),
    }
}

fn create_capture_path(
    mode: ExecutionHostMode,
    session_id: Option<&str>,
    result: &ToolExecutionResult,
) -> Result<String, String> {
    let sequence = CAPTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let timestamp_ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("system clock before Unix epoch: {error}"))?
        .as_nanos();
    let identity = format!(
        "{}:{}:{}:{}:{}",
        process::id(),
        timestamp_ns,
        sequence,
        result.tool_call_id,
        result.tool_name,
    );
    let digest = Sha256::digest(identity.as_bytes());
    Ok(format!(
        "{}/tool-result-{}.log",
        capture_directory(mode, session_id)
            .to_string_lossy()
            .replace('\\', "/"),
        &format!("{:x}", digest)[..32],
    ))
}

fn insert_capture(details: &mut Value, capture: &ToolResultCapture) {
    if !details.is_object() {
        *details = json!({ "executorDetails": details.take() });
    }
    details
        .as_object_mut()
        .expect("details was normalized")
        .insert(
            "toolResultCapture".to_string(),
            json!({
                "schema": CAPTURE_SCHEMA,
                "fullOutputPath": capture.full_output_path,
                "outputStartByte": capture.output_start_byte,
                "outputByteLength": capture.output_byte_length,
                "outputComplete": capture.output_complete,
            }),
        );
}

fn bounded_preview(content: &str) -> String {
    let head_end = previous_char_boundary(content, MODEL_PREVIEW_EDGE_BYTES.min(content.len()));
    let tail_start = next_char_boundary(
        content,
        content.len().saturating_sub(MODEL_PREVIEW_EDGE_BYTES),
    );
    format!(
        "{}\n\n... [{} bytes omitted; complete output spilled] ...\n\n{}",
        &content[..head_end],
        tail_start.saturating_sub(head_end),
        &content[tail_start..],
    )
}

fn previous_char_boundary(value: &str, mut index: usize) -> usize {
    while index > 0 && !value.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn next_char_boundary(value: &str, mut index: usize) -> usize {
    while index < value.len() && !value.is_char_boundary(index) {
        index += 1;
    }
    index
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use std::fs;
    use std::path::PathBuf;

    use super::*;
    use crate::tool::layer::ToolRuntimeContext;

    fn result(content: String) -> ToolExecutionResult {
        ToolExecutionResult {
            tool_call_id: "call_capture".to_string(),
            tool_name: "bash".to_string(),
            status: "ok".to_string(),
            content,
            details: json!({}),
            facts: Vec::new(),
            error: None,
            started_at_ms: 1,
            completed_at_ms: 2,
            latency_ms: 1,
            parallel_group: None,
            transition_reason: Some("local_tool_exec".to_string()),
        }
    }

    #[test]
    fn spills_large_result_without_losing_the_exact_output() {
        let workspace = std::env::temp_dir().join(format!(
            "tool-result-capture-test-{}-{}",
            process::id(),
            CAPTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir_all(workspace.as_path()).expect("create workspace");
        let context = ToolRuntimeContext::with_cwd(workspace.clone())
            .expect("runtime context")
            .with_session_id("capture_test_session");
        let binding = context.execution_host_binding().expect("execution binding");
        let exact = format!("HEAD{}TAIL", "界".repeat(20_000));
        let sealed = seal_tool_result(
            result(exact.clone()),
            r#"{"command":"test","description":"Capture test"}"#,
            Some("capture_test_session"),
            Some(binding.as_ref()),
        );
        let capture = tool_result_capture(&sealed);
        let path = PathBuf::from(capture.full_output_path.expect("spill path"));
        let spilled = binding
            .run_file_system_operation(
                path.to_string_lossy(),
                ExecutionFileSystemOperation::ReadFile {
                    max_bytes: exact.len() + MODEL_TOOL_RESULT_MAX_BYTES,
                },
            )
            .expect("read spill through the original binding");
        let ExecutionFileSystemOutput::ReadFile(spilled) = spilled else {
            panic!("spill read returned the wrong filesystem output")
        };
        let start = capture.output_start_byte.expect("content offset") as usize;
        let end = start + capture.output_byte_length as usize;

        let header = std::str::from_utf8(&spilled.bytes[..start]).expect("UTF-8 spill header");
        assert!(header.contains("Capture test"));
        assert!(header.contains(r#""command":"test""#));
        assert_eq!(&spilled.bytes[start..end], exact.as_bytes());
        let mutation = binding
            .run_file_system_operation(
                path.to_string_lossy(),
                ExecutionFileSystemOperation::WriteFile {
                    content: b"tampered".to_vec(),
                    expected_file_hash: Some(spilled.file_hash),
                    create_only: false,
                },
            )
            .expect_err("published spill must be read-only");
        assert_eq!(
            mutation.kind,
            ExecutionFileSystemErrorKind::PermissionDenied
        );
        assert!(sealed.content.contains("Full tool result:"));
        assert!(sealed.content.len() <= MODEL_TOOL_RESULT_MAX_BYTES);
        fs::remove_file(path).expect("remove spill file");
        fs::remove_dir_all(workspace).expect("remove workspace");
    }

    #[test]
    fn leaves_a_fifty_kibibyte_result_inline() {
        let exact = "x".repeat(MODEL_TOOL_RESULT_MAX_BYTES);
        let sealed = seal_tool_result(result(exact.clone()), "{}", None, None);

        assert_eq!(sealed.content, exact);
        assert_eq!(tool_result_capture(&sealed).full_output_path, None);
    }

    #[test]
    fn local_spill_requires_a_predeclared_read_only_root() {
        let workspace = std::env::temp_dir().join(format!(
            "tool-result-policy-test-{}-{}",
            process::id(),
            CAPTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir_all(workspace.as_path()).expect("create workspace");
        let binding = ExecutionHostBinding::new_test_local(
            workspace.clone(),
            crate::execution::sandbox::SandboxPolicy::workspace_write_no_network(&workspace),
        )
        .expect("execution binding");

        let sealed = seal_tool_result(
            result("x".repeat(MODEL_TOOL_RESULT_MAX_BYTES + 1)),
            "{}",
            Some("banana"),
            Some(&binding),
        );

        assert_eq!(sealed.status, "error");
        assert!(!tool_result_capture(&sealed).output_complete);
        assert!(sealed.content.contains("not predeclared as read-only"));
        fs::remove_dir_all(workspace).expect("remove workspace");
    }

    #[test]
    fn large_result_fails_loudly_without_an_execution_host() {
        let sealed = seal_tool_result(
            result("x".repeat(MODEL_TOOL_RESULT_MAX_BYTES + 1)),
            "{}",
            None,
            None,
        );

        assert_eq!(sealed.status, "error");
        assert_eq!(sealed.result_state().as_str(), "failed");
        assert!(!tool_result_capture(&sealed).output_complete);
    }
}
