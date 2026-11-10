//! Host-neutral identities and tool-backed source associations.

use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

/// Identifies one immutable revision of a Host-owned source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceIdentityV1 {
    pub source_ref: String,
    pub revision: String,
    pub sha256: String,
}

impl SourceIdentityV1 {
    /// Validates the opaque source identity and content digest.
    pub fn validate(&self) -> Result<(), String> {
        require_identifier("sourceRef", &self.source_ref)?;
        if self.source_ref.contains('/') || self.source_ref.contains('\\') {
            return Err("sourceRef must be an opaque ref, not a path".to_string());
        }
        require_identifier("revision", &self.revision)?;
        let digest = self.sha256.strip_prefix("sha256:").unwrap_or_default();
        if digest.len() != 64
            || !digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err("sha256 must contain 64 lowercase hexadecimal characters".to_string());
        }
        Ok(())
    }
}

/// Selects an inclusive, one-based line range in a source revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceLineRangeV1 {
    pub start_line: u64,
    pub end_line: u64,
}

impl SourceLineRangeV1 {
    /// Validates that both line numbers form a nonempty ordered range.
    pub fn validate(&self) -> Result<(), String> {
        if self.start_line == 0 || self.end_line < self.start_line {
            return Err("source line range is invalid".to_string());
        }
        Ok(())
    }
}

/// Associates an admitted source revision with a successful tool call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceAssociationV1 {
    pub association_id: String,
    pub input_ref: String,
    pub source_identity: SourceIdentityV1,
    pub source_tool_name: String,
    pub source_tool_call_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<SourceLineRangeV1>,
}

impl SourceAssociationV1 {
    /// Validates the association independently of the Session tool ledger.
    pub fn validate(&self) -> Result<(), String> {
        require_identifier("associationId", &self.association_id)?;
        require_identifier("inputRef", &self.input_ref)?;
        require_identifier("sourceToolName", &self.source_tool_name)?;
        require_identifier("sourceToolCallId", &self.source_tool_call_id)?;
        self.source_identity.validate()?;
        if let Some(range) = &self.range {
            range.validate()?;
        }
        Ok(())
    }
}

fn require_identifier(name: &str, value: &str) -> Result<(), String> {
    if value.trim().is_empty() || value.trim() != value {
        return Err(format!("{name} is required without outer whitespace"));
    }
    if !value.nfc().eq(value.chars()) {
        return Err(format!("{name} must use NFC Unicode normalization"));
    }
    Ok(())
}
