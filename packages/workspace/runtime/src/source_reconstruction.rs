//! Reconstructs current Core facts from authoritative, historical product records.

use centaeris_core::source::{SourceAssociationV1, SourceIdentityV1, SourceLineRangeV1};
use serde::Deserialize;
use serde_json::Value;

/// Changes only the loaded view. Stored product evidence and event identities remain intact.
pub(crate) fn session_record(wire: &mut Value) -> Result<(), String> {
    if wire.get("type").and_then(Value::as_str) != Some("citation_recorded") {
        return Ok(());
    }
    let payload = association_payload(&wire["payload"])?;
    wire["type"] = Value::String("source_associated".into());
    wire["payload"] = payload;
    Ok(())
}

/// Projects queued facts only after their original external object has been loaded.
pub(crate) fn external_facts(metadata: &mut Value) -> Result<(), String> {
    let Some(facts) = metadata.get_mut("toolExecutionFacts") else {
        return Ok(());
    };
    let facts = facts
        .as_array_mut()
        .ok_or("stored toolExecutionFacts must be an array")?;
    for fact in facts {
        if fact.get("kind").and_then(Value::as_str) != Some("citation_recorded") {
            continue;
        }
        let fields = fact
            .as_object()
            .ok_or("stored tool fact must be an object")?;
        if fields.len() != 2 || !fields.contains_key("payload") {
            return Err("stored citation fact fields mismatch".into());
        }
        let payload = association_payload(&fact["payload"])?;
        fact["kind"] = Value::String("source_associated".into());
        fact["payload"] = payload;
    }
    Ok(())
}

fn association_payload(payload: &Value) -> Result<Value, String> {
    let citation: HistoricalCitation = serde_json::from_value(payload.clone())
        .map_err(|error| format!("stored citation payload is invalid: {error}"))?;
    citation.validate(payload)?;
    let association = SourceAssociationV1 {
        association_id: citation.citation_id,
        input_ref: citation.input_ref,
        source_identity: SourceIdentityV1 {
            source_ref: format!("{}:{}", citation.owner_kind, citation.owner_ref),
            revision: citation
                .owner_generation
                .map(|generation| generation.to_string())
                .unwrap_or_else(|| citation.owner_sha256.clone()),
            sha256: citation.owner_sha256,
        },
        source_tool_name: citation.source_tool_name,
        source_tool_call_id: citation.source_tool_call_id,
        range: citation.locator.line_range(),
    };
    association.validate()?;
    serde_json::to_value(association).map_err(|error| error.to_string())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HistoricalCitation {
    citation_id: String,
    input_ref: String,
    owner_ref: String,
    owner_kind: String,
    display_name: String,
    evidence_kind: String,
    owner_sha256: String,
    source_tool_name: String,
    source_tool_call_id: String,
    locator: HistoricalLocator,
    owner_generation: Option<u64>,
    representation_id: Option<String>,
    spec_digest: Option<String>,
    evidence_sha256: Option<String>,
}

impl HistoricalCitation {
    fn validate(&self, payload: &Value) -> Result<(), String> {
        if !matches!(
            self.owner_kind.as_str(),
            "sourceObject" | "userLibraryObject" | "artifact"
        ) || !matches!(
            self.evidence_kind.as_str(),
            "workspaceSource" | "userProvided" | "generatedArtifact"
        ) || self.display_name.trim().is_empty()
            || self.owner_ref.trim().is_empty()
        {
            return Err("stored citation product identity is invalid".into());
        }
        validate_digest(&self.citation_id, "citation:")?;
        validate_digest(&self.owner_sha256, "sha256:")?;
        self.locator.validate()?;
        let derived = [
            self.owner_generation.is_some(),
            self.representation_id.is_some(),
            self.spec_digest.is_some(),
            self.evidence_sha256.is_some(),
        ];
        if self.locator.is_derived()
            || [
                "ownerGeneration",
                "representationId",
                "specDigest",
                "evidenceSha256",
            ]
            .iter()
            .any(|field| payload.get(field).is_some())
        {
            if !derived.iter().all(|present| *present) || self.owner_generation == Some(0) {
                return Err("stored citation derived identity is incomplete".into());
            }
            for (value, prefix) in [
                (self.representation_id.as_deref(), "representation:sha256:"),
                (self.spec_digest.as_deref(), "sha256:"),
                (self.evidence_sha256.as_deref(), "sha256:"),
            ] {
                validate_digest(
                    value.ok_or("stored citation derived digest is missing")?,
                    prefix,
                )?;
            }
        }
        Ok(())
    }
}

fn validate_digest(value: &str, prefix: &str) -> Result<(), String> {
    let digest = value.strip_prefix(prefix).unwrap_or_default();
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("stored citation digest is invalid".into());
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(untagged)]
enum HistoricalLocator {
    Lines(HistoricalLines),
    Pages(HistoricalPages),
    Derived(HistoricalDocumentLocator),
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HistoricalLines {
    start_line: u64,
    end_line: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HistoricalPages {
    page_start: u64,
    page_end: u64,
}

#[derive(Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
enum HistoricalDocumentLocator {
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

impl HistoricalLocator {
    fn is_derived(&self) -> bool {
        matches!(self, Self::Derived(_))
    }

    fn line_range(&self) -> Option<SourceLineRangeV1> {
        let (start_line, end_line) = match self {
            Self::Lines(lines) => (lines.start_line, lines.end_line),
            Self::Derived(HistoricalDocumentLocator::TextSpan {
                start_line,
                end_line,
                ..
            }) => (u64::from(*start_line), u64::from(*end_line)),
            _ => return None,
        };
        Some(SourceLineRangeV1 {
            start_line,
            end_line,
        })
    }

    fn validate(&self) -> Result<(), String> {
        let valid = match self {
            Self::Lines(lines) => lines.start_line > 0 && lines.end_line >= lines.start_line,
            Self::Pages(pages) => pages.page_start > 0 && pages.page_end >= pages.page_start,
            Self::Derived(HistoricalDocumentLocator::TextSpan {
                page_start,
                page_end,
                start_byte,
                end_byte,
                start_line,
                end_line,
            }) => {
                let pages_valid = match (page_start, page_end) {
                    (None, None) => true,
                    (Some(start), Some(end)) => *start > 0 && start <= end,
                    _ => false,
                };
                pages_valid && start_byte < end_byte && *start_line > 0 && start_line <= end_line
            }
            Self::Derived(HistoricalDocumentLocator::PageRegion {
                page,
                bbox: [left, top, right, bottom],
            }) => {
                *page > 0 && *right <= 10_000 && *bottom <= 10_000 && left < right && top < bottom
            }
            Self::Derived(HistoricalDocumentLocator::TableCell {
                page,
                table_id,
                start_row,
                end_row,
                start_column,
                end_column,
            }) => {
                *page > 0
                    && !table_id.trim().is_empty()
                    && start_row <= end_row
                    && start_column <= end_column
            }
        };
        if !valid {
            return Err("stored citation locator is invalid".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn historical_record(locator: Value) -> Value {
        json!({
            "schemaVersion":"session.event.v1", "eventVersion":1, "type":"citation_recorded",
            "eventId":"citation-event", "sessionId":"session", "turnId":"turn", "agentRunId":"run", "sequence":7, "createdAtMs":1,
            "payload":{
                "citationId":format!("citation:{}", "a".repeat(64)), "inputRef":"input",
                "ownerRef":"source", "ownerKind":"sourceObject", "displayName":"source.pdf",
                "evidenceKind":"workspaceSource", "ownerSha256":format!("sha256:{}", "b".repeat(64)),
                "sourceToolName":"read", "sourceToolCallId":"call", "locator":locator,
            }
        })
    }

    #[test]
    fn reconstructs_lines_without_changing_authoritative_evidence() {
        let original = historical_record(json!({"startLine":2,"endLine":8}));
        let original_bytes = serde_json::to_vec(&original).unwrap();
        let mut view = original.clone();
        session_record(&mut view).unwrap();
        assert_eq!(view["type"], "source_associated");
        assert_eq!(view["eventId"], original["eventId"]);
        assert_eq!(view["sequence"], original["sequence"]);
        assert_eq!(view["payload"]["range"], json!({"startLine":2,"endLine":8}));
        assert_eq!(
            view["payload"]["sourceIdentity"]["sourceRef"],
            "sourceObject:source"
        );
        centaeris_core::session::parse_wire_record(&view).unwrap();
        let projected = view.clone();
        session_record(&mut view).unwrap();
        assert_eq!(view, projected);
        assert_eq!(serde_json::to_vec(&original).unwrap(), original_bytes);
    }

    #[test]
    fn retains_product_page_and_table_evidence_in_the_original_record() {
        for locator in [
            json!({"pageStart":2,"pageEnd":4}),
            json!({"kind":"pageRegion","page":2,"bbox":[0,0,100,200]}),
            json!({"kind":"tableCell","page":2,"tableId":"t","startRow":0,"endRow":1,"startColumn":0,"endColumn":2}),
            json!({"kind":"textSpan","pageStart":2,"pageEnd":4,"startByte":2,"endByte":20,"startLine":1,"endLine":3}),
        ] {
            let mut original = historical_record(locator.clone());
            if locator.get("kind").is_some() {
                original["payload"]["ownerGeneration"] = json!(3);
                original["payload"]["representationId"] =
                    json!(format!("representation:sha256:{}", "c".repeat(64)));
                original["payload"]["specDigest"] = json!(format!("sha256:{}", "d".repeat(64)));
                original["payload"]["evidenceSha256"] = json!(format!("sha256:{}", "e".repeat(64)));
            }
            let mut view = original.clone();
            session_record(&mut view).unwrap();
            centaeris_core::session::parse_wire_record(&view).unwrap();
            assert_eq!(original["payload"]["locator"], locator);
            assert_eq!(
                view["payload"]["range"].is_object(),
                locator["kind"] == "textSpan"
            );
        }
    }

    #[test]
    fn invalid_historical_evidence_is_rejected_before_projection() {
        for (field, invalid) in [
            ("ownerKind", json!("other")),
            ("ownerRef", json!(" ")),
            ("ownerGeneration", Value::Null),
            ("evidenceKind", json!("other")),
            ("ownerSha256", json!("sha256:bad")),
            ("locator", json!({"pageStart":4,"pageEnd":2})),
            (
                "locator",
                json!({"kind":"pageRegion","page":1,"bbox":[0,0,10001,100]}),
            ),
        ] {
            let mut record = historical_record(json!({"startLine":1,"endLine":2}));
            record["payload"][field] = invalid;
            let original = record.clone();
            assert!(session_record(&mut record).is_err(), "field {field}");
            assert_eq!(record, original);
        }
    }

    #[test]
    fn reconstructs_queued_facts_without_interpreting_unrelated_metadata() {
        let record = historical_record(json!({"startLine":1,"endLine":2}));
        let mut metadata = json!({"other":{"kind":"citation_recorded"}, "toolExecutionFacts":[{"kind":"citation_recorded","payload":record["payload"]}]});
        external_facts(&mut metadata).unwrap();
        assert_eq!(metadata["other"]["kind"], "citation_recorded");
        let facts: Vec<centaeris_core::tool::layer::ToolExecutionFact> =
            serde_json::from_value(metadata["toolExecutionFacts"].clone()).unwrap();
        assert!(matches!(
            facts.as_slice(),
            [centaeris_core::tool::layer::ToolExecutionFact::SourceAssociated(_)]
        ));
    }
}
