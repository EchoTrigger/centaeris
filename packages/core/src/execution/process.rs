use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::policy::{ExecutionPolicy, NetworkPolicy};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExecutionPolicySummary {
    pub enforced: bool,
    pub network: NetworkPolicy,
    pub workspace_root: String,
    pub read_only_root_count: usize,
    pub writable_root_count: usize,
    pub denied_read_path_count: usize,
    pub denied_write_path_count: usize,
}

/// Failure categories are execution facts, independent of the Host backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutionError {
    Denied { reason: String },
    PolicyUnavailable { reason: String },
    HostUnavailable { reason: String },
    CancellationIndeterminate { reason: String },
    Io(String),
}

impl fmt::Display for ExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.user_visible_message().as_str())
    }
}
impl std::error::Error for ExecutionError {}
impl ExecutionError {
    pub fn internal_debug_message(&self) -> String {
        match self {
            Self::Denied { reason } => format!("execution policy denied: {reason}"),
            Self::PolicyUnavailable { reason } => format!("execution policy unavailable: {reason}"),
            Self::HostUnavailable { reason } => format!("execution host unavailable: {reason}"),
            Self::CancellationIndeterminate { reason } => {
                format!("execution cancellation indeterminate: {reason}")
            }
            Self::Io(reason) => format!("execution io error: {reason}"),
        }
    }
    pub fn model_visible_message(&self) -> String {
        match self {
            Self::Denied { .. } => "execution policy denied the requested operation".into(),
            Self::HostUnavailable { reason } => reason.clone(),
            Self::PolicyUnavailable { .. } => {
                "execution policy unavailable; refusing to degrade to an unisolated process".into()
            }
            Self::CancellationIndeterminate { .. } => {
                "execution cancellation outcome is indeterminate".into()
            }
            Self::Io(_) => "execution encountered an internal I/O error".into(),
        }
    }
    pub fn user_visible_message(&self) -> String {
        match self {
            Self::Denied { .. } => "Execution policy denied the operation".into(),
            Self::HostUnavailable { reason } => reason.clone(),
            Self::PolicyUnavailable { .. } => "Execution policy cannot be enforced".into(),
            Self::CancellationIndeterminate { .. } => {
                "Execution cancellation could not be verified".into()
            }
            Self::Io(_) => "Execution I/O error".into(),
        }
    }
    pub fn is_cancellation_indeterminate(&self) -> bool {
        matches!(self, Self::CancellationIndeterminate { .. })
    }
}

#[derive(Debug, Clone)]
pub struct ExecutionCommandRequest {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: HashMap<String, String>,
    pub timeout_ms: u64,
    pub policy: ExecutionPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExecutionAttempt {
    pub transition_reason: String,
    pub policy: ExecutionPolicySummary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExecutionProcessOutput {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub stdout_decode: ProcessOutputDecodeSummary,
    pub stderr_decode: ProcessOutputDecodeSummary,
    pub timed_out: bool,
    pub attempt: ExecutionAttempt,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runtime_diagnostics: Vec<RuntimeOutputDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessOutputDecodeSummary {
    pub encoding: String,
    pub status: String,
    pub raw_byte_length: usize,
    pub invalid_at: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeOutputDiagnostic {
    pub source: String,
    pub stream: String,
    pub severity: String,
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedProcessOutput {
    pub stdout: String,
    pub stderr: String,
    pub diagnostics: Vec<RuntimeOutputDiagnostic>,
}

pub fn normalize_process_output(stdout: String, stderr: String) -> NormalizedProcessOutput {
    NormalizedProcessOutput {
        stdout,
        stderr,
        diagnostics: Vec::new(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedProcessOutput {
    pub text: String,
    pub summary: ProcessOutputDecodeSummary,
}

pub fn decode_process_output(bytes: &[u8]) -> DecodedProcessOutput {
    if bytes.is_empty() {
        return decoded_output(String::new(), "empty", "ok", 0, None);
    }
    if bytes.starts_with(&[0xFF, 0xFE]) {
        return decode_utf16_units_lossy(&bytes[2..], false, "utf16le-bom", bytes.len());
    }
    if bytes.starts_with(&[0xFE, 0xFF]) {
        return decode_utf16_units_lossy(&bytes[2..], true, "utf16be-bom", bytes.len());
    }
    if looks_like_utf16(bytes, false) {
        return decode_utf16_units_lossy(bytes, false, "utf16le", bytes.len());
    }
    if looks_like_utf16(bytes, true) {
        return decode_utf16_units_lossy(bytes, true, "utf16be", bytes.len());
    }
    match std::str::from_utf8(bytes) {
        Ok(text) => decoded_output(text.to_string(), "utf8", "ok", bytes.len(), None),
        Err(error) => decoded_output(
            String::from_utf8_lossy(bytes).into_owned(),
            "unknown",
            "lossy",
            bytes.len(),
            Some(error.valid_up_to()),
        ),
    }
}

fn decoded_output(
    text: String,
    encoding: &str,
    status: &str,
    raw_byte_length: usize,
    invalid_at: Option<usize>,
) -> DecodedProcessOutput {
    DecodedProcessOutput {
        text,
        summary: ProcessOutputDecodeSummary {
            encoding: encoding.to_string(),
            status: status.to_string(),
            raw_byte_length,
            invalid_at,
        },
    }
}

fn looks_like_utf16(bytes: &[u8], big_endian: bool) -> bool {
    if bytes.len() < 4 || !bytes.len().is_multiple_of(2) {
        return false;
    }
    let zero_count = bytes
        .chunks_exact(2)
        .filter(|pair| {
            if big_endian {
                pair[0] == 0 && pair[1] != 0
            } else {
                pair[0] != 0 && pair[1] == 0
            }
        })
        .count();
    zero_count.saturating_mul(2) >= bytes.len() / 2
}

fn decode_utf16_units_lossy(
    bytes: &[u8],
    big_endian: bool,
    encoding: &str,
    raw_byte_length: usize,
) -> DecodedProcessOutput {
    let mut had_invalid_unit = false;
    let text = std::char::decode_utf16(bytes.chunks_exact(2).map(|pair| {
        if big_endian {
            u16::from_be_bytes([pair[0], pair[1]])
        } else {
            u16::from_le_bytes([pair[0], pair[1]])
        }
    }))
    .map(|item| match item {
        Ok(ch) => ch,
        Err(_) => {
            had_invalid_unit = true;
            char::REPLACEMENT_CHARACTER
        }
    })
    .collect::<String>();
    let invalid_at = (!bytes.len().is_multiple_of(2)).then(|| raw_byte_length.saturating_sub(1));
    decoded_output(
        text,
        encoding,
        if invalid_at.is_some() || had_invalid_unit {
            "lossy"
        } else {
            "ok"
        },
        raw_byte_length,
        invalid_at,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_contract_preserves_facts_and_rejects_backend_fields() {
        let decoded = decode_process_output(b"permission denied by arbitrary command output");
        let output = ExecutionProcessOutput {
            exit_code: Some(7),
            stdout: decoded.text.clone(),
            stderr: String::new(),
            stdout_decode: decoded.summary,
            stderr_decode: decode_process_output(b"").summary,
            timed_out: false,
            attempt: ExecutionAttempt {
                transition_reason: "host_private_backend_diagnostic".into(),
                policy: ExecutionPolicySummary {
                    enforced: true,
                    network: NetworkPolicy::Disabled,
                    workspace_root: "/workspace".into(),
                    read_only_root_count: 1,
                    writable_root_count: 1,
                    denied_read_path_count: 0,
                    denied_write_path_count: 0,
                },
            },
            runtime_diagnostics: Vec::new(),
        };
        let wire = serde_json::to_value(&output).unwrap();
        assert_eq!(wire["exitCode"], 7);
        assert_eq!(wire["stdoutDecode"]["rawByteLength"], output.stdout.len());
        assert_eq!(wire["attempt"]["policy"]["enforced"], true);
        assert_eq!(
            serde_json::from_value::<ExecutionProcessOutput>(wire.clone()).unwrap(),
            output
        );
        assert_eq!(
            super::super::classify_execution_host_failure(
                output.exit_code,
                output.timed_out,
                &output.stdout,
                &output.stderr,
            ),
            super::super::ExecutionHostFailureKind::CommandFailed
        );
        for pointer in ["/attempt", "/attempt/policy"] {
            let mut invalid = wire.clone();
            invalid
                .pointer_mut(pointer)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert("sandboxType".into(), "gvisor".into());
            assert!(serde_json::from_value::<ExecutionProcessOutput>(invalid).is_err());
        }
    }

    #[test]
    fn decodes_utf16_output_without_a_host_backend() {
        let decoded = decode_process_output(&[0xFF, 0xFE, b'o', 0, b'k', 0]);
        assert_eq!(decoded.text, "ok");
        assert_eq!(decoded.summary.encoding, "utf16le-bom");
    }
}
