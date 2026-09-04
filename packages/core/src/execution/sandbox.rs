use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SandboxType {
    LinuxBubblewrap,
    MacOsSeatbelt,
    HostProcess,
    Gvisor,
    OciContainer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "mode", rename_all = "camelCase")]
#[derive(Default)]
pub enum NetworkSandboxPolicy {
    #[default]
    PublicInternet,
    Disabled,
    Allowlist {
        #[serde(rename = "allowedDomains")]
        allowed_domains: Vec<String>,
    },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NetworkSandboxPolicyWire {
    mode: NetworkSandboxModeWire,
    allowed_domains: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
enum NetworkSandboxModeWire {
    PublicInternet,
    Disabled,
    Allowlist,
}

impl<'de> Deserialize<'de> for NetworkSandboxPolicy {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = NetworkSandboxPolicyWire::deserialize(deserializer)?;
        let policy = match (wire.mode, wire.allowed_domains) {
            (NetworkSandboxModeWire::PublicInternet, None) => Self::PublicInternet,
            (NetworkSandboxModeWire::Disabled, None) => Self::Disabled,
            (NetworkSandboxModeWire::Allowlist, Some(allowed_domains)) => {
                Self::Allowlist { allowed_domains }
            }
            (NetworkSandboxModeWire::PublicInternet, Some(_)) => {
                return Err(serde::de::Error::custom(
                    "publicInternet network policy must not contain allowedDomains",
                ));
            }
            (NetworkSandboxModeWire::Disabled, Some(_)) => {
                return Err(serde::de::Error::custom(
                    "disabled network policy must not contain allowedDomains",
                ));
            }
            (NetworkSandboxModeWire::Allowlist, None) => {
                return Err(serde::de::Error::custom(
                    "allowlist network policy requires allowedDomains",
                ));
            }
        };
        policy.validate().map_err(serde::de::Error::custom)?;
        Ok(policy)
    }
}

impl NetworkSandboxPolicy {
    pub fn validate(&self) -> Result<(), String> {
        let Self::Allowlist { allowed_domains } = self else {
            return Ok(());
        };
        if allowed_domains.is_empty() {
            return Err(
                "network allowlist must contain at least one domain; use disabled for no network"
                    .to_string(),
            );
        }
        for domain in allowed_domains {
            let value = domain.strip_prefix("*.").unwrap_or(domain.as_str());
            let valid = !value.is_empty()
                && value.len() <= 253
                && value.is_ascii()
                && value.contains('.')
                && !value.starts_with('.')
                && !value.ends_with('.')
                && value.split('.').all(|label| {
                    !label.is_empty()
                        && label.len() <= 63
                        && !label.starts_with('-')
                        && !label.ends_with('-')
                        && label
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                });
            if !valid || value.parse::<std::net::IpAddr>().is_ok() {
                return Err(format!(
                    "network allowlist contains an invalid domain: {domain}"
                ));
            }
        }
        Ok(())
    }

    pub fn uses_managed_egress(&self) -> bool {
        !matches!(self, Self::Disabled)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FileSystemSandboxPolicy {
    pub workspace_root: PathBuf,
    pub read_only_roots: Vec<PathBuf>,
    pub writable_roots: Vec<PathBuf>,
    pub denied_read_paths: Vec<PathBuf>,
    pub denied_write_paths: Vec<PathBuf>,
    pub tmp_root: Option<PathBuf>,
}

impl FileSystemSandboxPolicy {
    pub fn workspace_write(workspace_root: impl Into<PathBuf>) -> Self {
        let workspace_root = workspace_root.into();
        Self {
            read_only_roots: vec![workspace_root.clone()],
            writable_roots: vec![workspace_root.clone()],
            denied_read_paths: Vec::new(),
            denied_write_paths: Vec::new(),
            tmp_root: None,
            workspace_root,
        }
    }

    pub fn read_only(workspace_root: impl Into<PathBuf>) -> Self {
        let workspace_root = workspace_root.into();
        Self {
            read_only_roots: vec![workspace_root.clone()],
            writable_roots: Vec::new(),
            denied_read_paths: Vec::new(),
            denied_write_paths: Vec::new(),
            tmp_root: None,
            workspace_root,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SandboxPolicy {
    pub filesystem: FileSystemSandboxPolicy,
    pub network: NetworkSandboxPolicy,
}

impl SandboxPolicy {
    pub fn workspace_write_public_internet(workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            filesystem: FileSystemSandboxPolicy::workspace_write(workspace_root),
            network: NetworkSandboxPolicy::PublicInternet,
        }
    }

    pub fn workspace_write_no_network(workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            filesystem: FileSystemSandboxPolicy::workspace_write(workspace_root),
            network: NetworkSandboxPolicy::Disabled,
        }
    }

    pub fn workspace_write_with_network_allowlist(
        workspace_root: impl Into<PathBuf>,
        allowed_domains: Vec<String>,
    ) -> Self {
        Self {
            filesystem: FileSystemSandboxPolicy::workspace_write(workspace_root),
            network: NetworkSandboxPolicy::Allowlist { allowed_domains },
        }
    }

    pub fn read_only_no_network(workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            filesystem: FileSystemSandboxPolicy::read_only(workspace_root),
            network: NetworkSandboxPolicy::Disabled,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SandboxPolicySummary {
    pub sandbox_type: SandboxType,
    pub enforced: bool,
    pub network: NetworkSandboxPolicy,
    pub workspace_root: String,
    pub read_only_root_count: usize,
    pub writable_root_count: usize,
    pub denied_read_path_count: usize,
    pub denied_write_path_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SandboxErr {
    Denied {
        reason: String,
        sandbox_type: SandboxType,
    },
    Unavailable {
        reason: String,
        sandbox_type: Option<SandboxType>,
    },
    CancellationIndeterminate {
        reason: String,
        sandbox_type: Option<SandboxType>,
    },
    Io(String),
}

impl fmt::Display for SandboxErr {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.user_visible_message().as_str())
    }
}

impl std::error::Error for SandboxErr {}

impl SandboxErr {
    pub fn internal_debug_message(&self) -> String {
        match self {
            Self::Denied {
                reason,
                sandbox_type,
            } => format!("sandbox denied by {sandbox_type:?}: {reason}"),
            Self::Unavailable {
                reason,
                sandbox_type,
            } => format!("sandbox unavailable ({sandbox_type:?}): {reason}"),
            Self::CancellationIndeterminate {
                reason,
                sandbox_type,
            } => format!("execution cancellation indeterminate ({sandbox_type:?}): {reason}"),
            Self::Io(reason) => format!("sandbox io error: {reason}"),
        }
    }

    pub fn model_visible_message(&self) -> String {
        match self {
            Self::Denied { .. } => "sandbox policy denied the requested operation".to_string(),
            Self::Unavailable {
                reason,
                sandbox_type: None,
            } => reason.clone(),
            Self::Unavailable {
                reason,
                sandbox_type: Some(SandboxType::HostProcess),
            } => reason.clone(),
            Self::Unavailable { .. } => {
                "sandbox unavailable; refusing to degrade to an unsandboxed process".to_string()
            }
            Self::CancellationIndeterminate { .. } => {
                "execution cancellation outcome is indeterminate".to_string()
            }
            Self::Io(_) => "sandbox encountered an internal I/O error".to_string(),
        }
    }

    pub fn user_visible_message(&self) -> String {
        match self {
            Self::Denied { .. } => "Sandbox policy denied the operation".to_string(),
            Self::Unavailable {
                reason,
                sandbox_type: None,
            } => reason.clone(),
            Self::Unavailable {
                reason,
                sandbox_type: Some(SandboxType::HostProcess),
            } => reason.clone(),
            Self::Unavailable { .. } => "Sandbox runtime is unavailable".to_string(),
            Self::CancellationIndeterminate { .. } => {
                "Execution cancellation could not be verified".to_string()
            }
            Self::Io(_) => "Sandbox I/O error".to_string(),
        }
    }

    pub fn is_cancellation_indeterminate(&self) -> bool {
        matches!(self, Self::CancellationIndeterminate { .. })
    }
}

#[derive(Debug, Clone)]
pub struct SandboxTransformRequest {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: HashMap<String, String>,
    pub timeout_ms: u64,
    pub policy: SandboxPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SandboxAttempt {
    pub sandbox_type: SandboxType,
    pub transition_reason: String,
    pub policy: SandboxPolicySummary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SandboxedProcessOutput {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub stdout_decode: ProcessOutputDecodeSummary,
    pub stderr_decode: ProcessOutputDecodeSummary,
    pub timed_out: bool,
    pub attempt: SandboxAttempt,
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
    fn rejects_unsupported_platform_sandbox_values() {
        let error = serde_json::from_str::<SandboxType>("\"banana\"")
            .expect_err("unknown backend must loud-fail");
        assert!(error.to_string().contains("unknown variant"));
    }

    #[test]
    fn platform_sandbox_evidence_uses_exact_camel_case_values() {
        assert_eq!(
            serde_json::to_string(&SandboxType::LinuxBubblewrap).unwrap(),
            "\"linuxBubblewrap\""
        );
        assert_eq!(
            serde_json::to_string(&SandboxType::MacOsSeatbelt).unwrap(),
            "\"macOsSeatbelt\""
        );
        assert_eq!(
            serde_json::to_string(&SandboxType::HostProcess).unwrap(),
            "\"hostProcess\""
        );
    }

    #[test]
    fn workspace_metadata_is_writable_by_default() {
        let policy = FileSystemSandboxPolicy::workspace_write("/workspace");
        assert!(policy.denied_read_paths.is_empty());
        assert!(policy.denied_write_paths.is_empty());
    }

    #[test]
    fn decodes_utf16_output_without_a_host_backend() {
        let decoded = decode_process_output(&[0xFF, 0xFE, b'o', 0, b'k', 0]);
        assert_eq!(decoded.text, "ok");
        assert_eq!(decoded.summary.encoding, "utf16le-bom");
    }
}
