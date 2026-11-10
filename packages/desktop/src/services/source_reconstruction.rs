//! Reconstructs generic runtime sources from application-owned historical evidence.

use std::borrow::Cow;
use std::path::Path;

use centaeris_core::session::{parse_wire_record, SequencedSessionRecord};
use centaeris_core::source::{SourceAssociationV1, SourceIdentityV1};
use rusqlite::Connection;
use serde::Deserialize;
use serde_json::Value;

mod locator;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CitationEvidence {
    citation_id: String,
    input_ref: String,
    owner_ref: String,
    owner_kind: String,
    display_name: String,
    evidence_kind: String,
    owner_sha256: String,
    source_tool_name: String,
    source_tool_call_id: String,
    locator: Value,
    owner_generation: Option<u64>,
    representation_id: Option<String>,
    spec_digest: Option<String>,
    evidence_sha256: Option<String>,
}

fn reconstruct_evidence(payload: &Value) -> Result<Value, String> {
    let evidence: CitationEvidence = serde_json::from_value(payload.clone())
        .map_err(|error| format!("stored citation evidence is invalid: {error}"))?;
    if !matches!(
        evidence.owner_kind.as_str(),
        "sourceObject" | "userLibraryObject" | "artifact"
    ) || !matches!(
        evidence.evidence_kind.as_str(),
        "workspaceSource" | "userProvided" | "generatedArtifact"
    ) || evidence.display_name.trim().is_empty()
        || evidence.owner_ref.trim().is_empty()
    {
        return Err("stored citation identity is invalid".into());
    }
    validate_digest(&evidence.citation_id, "citation:")?;
    let range = locator::line_range(&evidence.locator)?;
    let derived = [
        evidence.owner_generation.is_some(),
        evidence.representation_id.is_some(),
        evidence.spec_digest.is_some(),
        evidence.evidence_sha256.is_some(),
    ];
    if evidence.locator.get("kind").is_some()
        || [
            "ownerGeneration",
            "representationId",
            "specDigest",
            "evidenceSha256",
        ]
        .iter()
        .any(|field| payload.get(field).is_some())
    {
        if !derived.iter().all(|value| *value) || evidence.owner_generation == Some(0) {
            return Err("stored citation derived identity is incomplete".into());
        }
        for (value, prefix) in [
            (
                evidence.representation_id.as_deref(),
                "representation:sha256:",
            ),
            (evidence.spec_digest.as_deref(), "sha256:"),
            (evidence.evidence_sha256.as_deref(), "sha256:"),
        ] {
            validate_digest(value.expect("complete derived identity"), prefix)?;
        }
    }
    let association = SourceAssociationV1 {
        association_id: evidence.citation_id,
        input_ref: evidence.input_ref,
        source_identity: SourceIdentityV1 {
            source_ref: format!("{}:{}", evidence.owner_kind, evidence.owner_ref),
            revision: evidence
                .owner_generation
                .map(|value| value.to_string())
                .unwrap_or_else(|| evidence.owner_sha256.clone()),
            sha256: evidence.owner_sha256,
        },
        source_tool_name: evidence.source_tool_name,
        source_tool_call_id: evidence.source_tool_call_id,
        range,
    };
    association.validate()?;
    serde_json::to_value(association).map_err(|error| error.to_string())
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

/// Reads a stored record without changing its authoritative bytes or offsets.
pub(crate) fn parse_stored_record(value: &Value) -> Result<SequencedSessionRecord, String> {
    let view = stored_record_view(value)?;
    parse_wire_record(view.as_ref()).map_err(|error| error.to_string())
}

pub(crate) fn stored_record_view(value: &Value) -> Result<Cow<'_, Value>, String> {
    if value.get("type").and_then(Value::as_str) != Some("citation_recorded") {
        return Ok(Cow::Borrowed(value));
    }
    let mut reconstructed = value.clone();
    reconstructed["type"] = Value::String("source_associated".into());
    reconstructed["payload"] = reconstruct_evidence(&value["payload"])?;
    Ok(Cow::Owned(reconstructed))
}

/// Advances stored provider receipts before the runtime can resume their jobs.
pub(crate) fn migrate_external_facts(path: &Path) -> Result<(), String> {
    let mut connection = Connection::open(path).map_err(|error| error.to_string())?;
    let transaction = connection
        .transaction()
        .map_err(|error| error.to_string())?;
    let rows = {
        let mut query = transaction.prepare(
            "SELECT object_id,metadata_json FROM external_context_objects WHERE EXISTS \
             (SELECT 1 FROM json_each(metadata_json,'$.toolExecutionFacts') WHERE json_extract(value,'$.kind')='citation_recorded')"
        ).map_err(|error| error.to_string())?;
        let rows = query
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|error| error.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?
    };
    for (id, raw) in rows {
        let mut metadata: Value = serde_json::from_str(&raw).map_err(|error| error.to_string())?;
        let mut evidence = Vec::new();
        for fact in metadata["toolExecutionFacts"]
            .as_array_mut()
            .ok_or("stored provider facts must be an array")?
        {
            if fact.get("kind").and_then(Value::as_str) == Some("citation_recorded") {
                if fact.as_object().is_none_or(|fields| fields.len() != 2) {
                    return Err("stored citation fact fields mismatch".into());
                }
                let payload = reconstruct_evidence(&fact["payload"])?;
                evidence.push(fact["payload"].clone());
                *fact = serde_json::json!({"kind":"source_associated","payload":payload});
            }
        }
        if metadata.get("citationEvidence").is_some() {
            return Err("stored citation evidence migration conflict".into());
        }
        metadata["citationEvidence"] = Value::Array(evidence);
        let encoded = serde_json::to_string(&metadata).map_err(|error| error.to_string())?;
        transaction
            .execute(
                "UPDATE external_context_objects SET metadata_json=?1 WHERE object_id=?2",
                rusqlite::params![encoded, id],
            )
            .map_err(|error| error.to_string())?;
    }
    transaction.commit().map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn evidence() -> Value {
        json!({"citationId":format!("citation:{}", "a".repeat(64)),"inputRef":"input-1",
            "ownerRef":"object-1","ownerKind":"sourceObject","displayName":"report.txt",
            "evidenceKind":"workspaceSource","ownerSha256":format!("sha256:{}", "b".repeat(64)),
            "sourceToolName":"read","sourceToolCallId":"call-1","locator":{"startLine":2,"endLine":7}})
    }

    #[test]
    fn stored_sources_reconstruct_without_rewriting_authoritative_records() {
        let wire = json!({"schemaVersion":"session.event.v1","eventVersion":1,"sequence":4,
            "type":"citation_recorded","eventId":"source-event","sessionId":"session-1",
            "agentRunId":"run-1","turnId":"turn-1","createdAtMs":1,"payload":evidence()});
        let before = serde_json::to_vec(&wire).unwrap();
        assert!(parse_wire_record(&wire).is_err());
        let reconstructed = parse_stored_record(&wire).unwrap();
        assert_eq!(reconstructed.sequence, 4);
        assert_eq!(reconstructed.event.event_id, "source-event");
        assert_eq!(
            reconstructed.event.payload["sourceIdentity"]["sourceRef"],
            "sourceObject:object-1"
        );
        assert_eq!(
            reconstructed.event.payload["range"],
            json!({"startLine":2,"endLine":7})
        );
        assert_eq!(serde_json::to_vec(&wire).unwrap(), before);
        let mut invalid = wire;
        invalid["payload"]["ownerGeneration"] = Value::Null;
        assert!(parse_stored_record(&invalid).is_err());
    }

    #[test]
    fn provider_receipt_migration_is_atomic_and_repeatable() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("runtime.sqlite3");
        let connection = Connection::open(&path).unwrap();
        connection.execute_batch("CREATE TABLE external_context_objects (object_id TEXT PRIMARY KEY,metadata_json TEXT,content BLOB)").unwrap();
        let metadata = json!({"retained":true,"toolExecutionFacts":[{"kind":"citation_recorded","payload":evidence()}]});
        connection
            .execute(
                "INSERT INTO external_context_objects VALUES ('receipt',?1,?2)",
                rusqlite::params![metadata.to_string(), b"unchanged\0body".as_slice()],
            )
            .unwrap();
        migrate_external_facts(&path).unwrap();
        migrate_external_facts(&path).unwrap();
        let (raw, body): (String, Vec<u8>) = connection
            .query_row(
                "SELECT metadata_json,content FROM external_context_objects",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        let metadata: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(metadata["retained"], true);
        assert_eq!(metadata["citationEvidence"], json!([evidence()]));
        assert_eq!(
            metadata["toolExecutionFacts"][0]["kind"],
            "source_associated"
        );
        assert_eq!(body, b"unchanged\0body");
    }
}
