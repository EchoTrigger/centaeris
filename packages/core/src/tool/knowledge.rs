use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::session::external_context::canonical_json_string;
use crate::tool::inputs::InputIdentityV1;

pub const CITATION_SCHEMA: &str = "knowledge.citation.v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum KnowledgeLocatorV1 {
    TextSpan {
        page_start: Option<u32>,
        page_end: Option<u32>,
        start_byte: u64,
        end_byte: u64,
        start_line: u32,
        end_line: u32,
    },
    PageRegion {
        page: u32,
        bbox: [u16; 4],
    },
    TableCell {
        page: u32,
        table_id: String,
        start_row: u32,
        end_row: u32,
        start_column: u32,
        end_column: u32,
    },
}

impl KnowledgeLocatorV1 {
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::TextSpan {
                page_start,
                page_end,
                start_byte,
                end_byte,
                start_line,
                end_line,
            } => {
                let valid_pages = match (page_start, page_end) {
                    (None, None) => true,
                    (Some(start), Some(end)) => *start > 0 && start <= end,
                    _ => false,
                };
                if !valid_pages
                    || start_byte >= end_byte
                    || *start_line == 0
                    || start_line > end_line
                {
                    return Err("invalid textSpan locator".to_string());
                }
            }
            Self::PageRegion { page, bbox } => {
                if *page == 0 {
                    return Err("pageRegion page must be positive".to_string());
                }
                validate_bbox(*bbox)?;
            }
            Self::TableCell {
                page,
                table_id,
                start_row,
                end_row,
                start_column,
                end_column,
            } => {
                if *page == 0
                    || *start_row > *end_row
                    || *start_column > *end_column
                    || table_id.trim().is_empty()
                {
                    return Err("invalid tableCell locator".to_string());
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CitationV1 {
    pub schema: String,
    pub citation_id: String,
    pub input_identity: InputIdentityV1,
    pub representation_id: String,
    pub spec_digest: String,
    pub locator: KnowledgeLocatorV1,
    pub evidence_sha256: String,
    pub source_tool_name: String,
    pub source_tool_call_id: String,
    pub session_id: String,
    pub agent_run_id: String,
}

impl CitationV1 {
    #[expect(
        clippy::too_many_arguments,
        reason = "citation constructor keeps provenance identity fields explicit"
    )]
    pub fn new(
        input_identity: InputIdentityV1,
        representation_id: String,
        spec_digest: String,
        locator: KnowledgeLocatorV1,
        evidence_sha256: String,
        source_tool_name: String,
        source_tool_call_id: String,
        session_id: String,
        agent_run_id: String,
    ) -> Result<Self, String> {
        let mut citation = Self {
            schema: CITATION_SCHEMA.to_string(),
            citation_id: String::new(),
            input_identity,
            representation_id,
            spec_digest,
            locator,
            evidence_sha256,
            source_tool_name,
            source_tool_call_id,
            session_id,
            agent_run_id,
        };
        citation.validate_without_id()?;
        let value = serde_json::to_value(serde_json::json!({
            "inputIdentity": citation.input_identity,
            "representationId": citation.representation_id,
            "specDigest": citation.spec_digest,
            "locator": citation.locator,
            "evidenceSha256": citation.evidence_sha256,
            "sourceToolName": citation.source_tool_name,
            "sessionId": citation.session_id,
            "agentRunId": citation.agent_run_id,
        }))
        .map_err(|error| format!("serialize citation identity failed: {error}"))?;
        citation.citation_id = format!(
            "citation:{}",
            sha256(canonical_json_string(&value).as_bytes())
                .strip_prefix("sha256:")
                .expect("sha256 helper always prefixes its digest")
        );
        Ok(citation)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema != CITATION_SCHEMA {
            return Err("citation schema mismatch".to_string());
        }
        self.validate_without_id()?;
        let rebuilt = Self::new(
            self.input_identity.clone(),
            self.representation_id.clone(),
            self.spec_digest.clone(),
            self.locator.clone(),
            self.evidence_sha256.clone(),
            self.source_tool_name.clone(),
            self.source_tool_call_id.clone(),
            self.session_id.clone(),
            self.agent_run_id.clone(),
        )?;
        if rebuilt.citation_id != self.citation_id {
            return Err("citationId does not match citation identity".to_string());
        }
        Ok(())
    }

    fn validate_without_id(&self) -> Result<(), String> {
        self.input_identity.validate()?;
        require_prefixed_sha256(
            "representationId",
            self.representation_id.as_str(),
            "representation:",
        )?;
        validate_sha256(self.spec_digest.as_str(), "specDigest")?;
        self.locator.validate()?;
        validate_sha256(self.evidence_sha256.as_str(), "evidenceSha256")?;
        for (field, value) in [
            ("sourceToolName", self.source_tool_name.as_str()),
            ("sourceToolCallId", self.source_tool_call_id.as_str()),
            ("sessionId", self.session_id.as_str()),
            ("agentRunId", self.agent_run_id.as_str()),
        ] {
            require_identity(field, value)?;
        }
        Ok(())
    }
}

fn validate_bbox([left, top, right, bottom]: [u16; 4]) -> Result<(), String> {
    if right > 10_000 || bottom > 10_000 || left >= right || top >= bottom {
        return Err("bbox must be a non-empty 0-10000 rectangle".to_string());
    }
    Ok(())
}

fn require_identity(field: &str, value: &str) -> Result<(), String> {
    if value.trim().is_empty() || value.trim() != value || value.len() > 256 {
        return Err(format!("{field} must be a bounded non-empty identity"));
    }
    Ok(())
}

fn validate_sha256(value: &str, field: &str) -> Result<(), String> {
    require_prefixed_sha256(field, value, "")
}

fn require_prefixed_sha256(field: &str, value: &str, prefix: &str) -> Result<(), String> {
    let Some(hex) = value
        .strip_prefix(prefix)
        .and_then(|value| value.strip_prefix("sha256:"))
    else {
        return Err(format!("{field} must use {prefix}sha256:<hex> format"));
    };
    if hex.len() != 64
        || !hex.bytes().all(|value| value.is_ascii_hexdigit())
        || hex.bytes().any(|value| value.is_ascii_uppercase())
    {
        return Err(format!("{field} must contain lowercase SHA-256 hex"));
    }
    Ok(())
}

fn sha256(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input_identity() -> InputIdentityV1 {
        InputIdentityV1 {
            owner_kind: "sourceObject".to_string(),
            owner_id: "src_1".to_string(),
            generation: 1,
            sha256: format!("sha256:{}", "a".repeat(64)),
        }
    }

    #[test]
    fn citation_validates_stable_evidence() {
        let spec_digest = format!("sha256:{}", "d".repeat(64));
        let representation_id = format!("representation:sha256:{}", "b".repeat(64));
        let citation = CitationV1::new(
            input_identity(),
            representation_id,
            spec_digest,
            KnowledgeLocatorV1::PageRegion {
                page: 1,
                bbox: [100, 200, 5_000, 600],
            },
            format!("sha256:{}", "e".repeat(64)),
            "read".to_string(),
            "call_1".to_string(),
            "session_1".to_string(),
            "agent_run_1".to_string(),
        )
        .expect("citation");
        citation.validate().expect("valid citation");
        assert!(citation.citation_id.starts_with("citation:"));
        assert_eq!(citation.citation_id.len(), "citation:".len() + 64);

        let retried = CitationV1::new(
            citation.input_identity.clone(),
            citation.representation_id.clone(),
            citation.spec_digest.clone(),
            citation.locator.clone(),
            citation.evidence_sha256.clone(),
            citation.source_tool_name.clone(),
            "call_retry".to_string(),
            citation.session_id.clone(),
            citation.agent_run_id.clone(),
        )
        .expect("retried citation");
        assert_eq!(citation.citation_id, retried.citation_id);
    }

    #[test]
    fn invalid_locator_values_fail_loudly() {
        let invalid = KnowledgeLocatorV1::PageRegion {
            page: 1,
            bbox: [0, 0, 10_001, 100],
        };
        assert!(invalid.validate().is_err());

        let locator = serde_json::from_value::<KnowledgeLocatorV1>(serde_json::json!({
            "kind": "textSpan",
            "pageStart": 2,
            "pageEnd": 4,
            "startByte": 10,
            "endByte": 30,
            "startLine": 4,
            "endLine": 8
        }))
        .expect("multi-page textSpan locator");
        locator.validate().expect("valid multi-page locator");
        assert!(
            serde_json::from_value::<KnowledgeLocatorV1>(serde_json::json!({
                "kind": "textSpan",
                "page": 2,
                "startByte": 10,
                "endByte": 30,
                "startLine": 4,
                "endLine": 8
            }))
            .is_err()
        );
    }
}
