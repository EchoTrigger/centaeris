use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    validate_event_shape, SequencedSessionRecord, SessionLogRecord, SessionRecordType,
    SESSION_EVENT_SCHEMA_VERSION,
};

pub const SESSION_MANIFEST_SCHEMA_VERSION: &str = "session.manifest.v1";
pub const SESSION_PROTOCOL_MAJOR: u32 = 1;
pub const SESSION_INTEGRITY_MODE: &str = "record";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionManifestV1 {
    pub schema_version: String,
    pub session_id: String,
    pub protocol_major: u32,
    pub created_at_ms: i64,
    pub writer_version: String,
    pub required_features: Vec<String>,
    pub integrity_mode: String,
}

impl SessionManifestV1 {
    pub fn new(
        session_id: impl Into<String>,
        created_at_ms: i64,
        writer_version: impl Into<String>,
    ) -> Result<Self, SessionProtocolError> {
        let manifest = Self {
            schema_version: SESSION_MANIFEST_SCHEMA_VERSION.to_string(),
            session_id: session_id.into(),
            protocol_major: SESSION_PROTOCOL_MAJOR,
            created_at_ms,
            writer_version: writer_version.into(),
            required_features: Vec::new(),
            integrity_mode: SESSION_INTEGRITY_MODE.to_string(),
        };
        validate_manifest(&manifest)?;
        Ok(manifest)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SessionRecordWireV1 {
    schema_version: String,
    event_version: u32,
    sequence: u64,
    #[serde(rename = "type")]
    event_type: SessionRecordType,
    event_id: String,
    session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    turn_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent_run_id: Option<String>,
    created_at_ms: i64,
    payload: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionProtocolErrorKind {
    Unsupported,
    Corrupted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionProtocolError {
    pub kind: SessionProtocolErrorKind,
    pub message: String,
}

impl SessionProtocolError {
    pub fn code(&self) -> &'static str {
        match self.kind {
            SessionProtocolErrorKind::Unsupported => "unsupported_session_protocol",
            SessionProtocolErrorKind::Corrupted => "corrupted_session",
        }
    }

    fn unsupported(message: impl Into<String>) -> Self {
        Self {
            kind: SessionProtocolErrorKind::Unsupported,
            message: message.into(),
        }
    }

    fn corrupted(message: impl Into<String>) -> Self {
        Self {
            kind: SessionProtocolErrorKind::Corrupted,
            message: message.into(),
        }
    }
}

impl fmt::Display for SessionProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code(), self.message)
    }
}

impl std::error::Error for SessionProtocolError {}

pub fn parse_manifest(value: &Value) -> Result<SessionManifestV1, SessionProtocolError> {
    let schema = value
        .get("schemaVersion")
        .and_then(Value::as_str)
        .ok_or_else(|| SessionProtocolError::corrupted("manifest schemaVersion is required"))?;
    if schema != SESSION_MANIFEST_SCHEMA_VERSION {
        return Err(SessionProtocolError::unsupported(format!(
            "unsupported manifest schemaVersion: {schema}"
        )));
    }
    let manifest = serde_json::from_value::<SessionManifestV1>(value.clone()).map_err(|error| {
        SessionProtocolError::corrupted(format!("decode manifest failed: {error}"))
    })?;
    validate_manifest(&manifest)?;
    Ok(manifest)
}

pub fn parse_wire_record(value: &Value) -> Result<SequencedSessionRecord, SessionProtocolError> {
    let schema = value
        .get("schemaVersion")
        .and_then(Value::as_str)
        .ok_or_else(|| SessionProtocolError::corrupted("record schemaVersion is required"))?;
    if schema != SESSION_EVENT_SCHEMA_VERSION {
        return Err(SessionProtocolError::unsupported(format!(
            "unsupported event schemaVersion: {schema}"
        )));
    }
    let event_type = value
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| SessionProtocolError::corrupted("record type is required"))?;
    if !SessionRecordType::allowed_type_names().contains(&event_type) {
        return Err(SessionProtocolError::unsupported(format!(
            "unsupported event type: {event_type}"
        )));
    }
    let event_type = serde_json::from_value::<SessionRecordType>(Value::String(event_type.into()))
        .map_err(|error| {
            SessionProtocolError::corrupted(format!("decode event type failed: {error}"))
        })?;
    let event_version = value
        .get("eventVersion")
        .and_then(Value::as_u64)
        .ok_or_else(|| SessionProtocolError::corrupted("record eventVersion is required"))?;
    if event_version != u64::from(event_type.event_version()) {
        return Err(SessionProtocolError::unsupported(format!(
            "unsupported {} eventVersion: {event_version}",
            event_type.as_str()
        )));
    }
    let wire = serde_json::from_value::<SessionRecordWireV1>(value.clone()).map_err(|error| {
        SessionProtocolError::corrupted(format!("decode session record failed: {error}"))
    })?;
    if wire.sequence == 0 {
        return Err(SessionProtocolError::corrupted(
            "record sequence must be positive",
        ));
    }
    let record = SessionLogRecord {
        schema_version: wire.schema_version,
        event_version: wire.event_version,
        event_type: wire.event_type,
        event_id: wire.event_id,
        session_id: wire.session_id,
        turn_id: wire.turn_id,
        agent_run_id: wire.agent_run_id,
        created_at_ms: wire.created_at_ms,
        payload: wire.payload,
    };
    validate_event_shape(&record).map_err(SessionProtocolError::corrupted)?;
    Ok(SequencedSessionRecord {
        sequence: wire.sequence,
        event: record,
    })
}

pub fn wire_record_value(record: &SequencedSessionRecord) -> Result<Value, SessionProtocolError> {
    if record.sequence == 0 {
        return Err(SessionProtocolError::corrupted(
            "record sequence must be positive",
        ));
    }
    validate_event_shape(&record.event).map_err(SessionProtocolError::corrupted)?;
    serde_json::to_value(SessionRecordWireV1 {
        schema_version: record.event.schema_version.clone(),
        event_version: record.event.event_version,
        sequence: record.sequence,
        event_type: record.event.event_type,
        event_id: record.event.event_id.clone(),
        session_id: record.event.session_id.clone(),
        turn_id: record.event.turn_id.clone(),
        agent_run_id: record.event.agent_run_id.clone(),
        created_at_ms: record.event.created_at_ms,
        payload: record.event.payload.clone(),
    })
    .map_err(|error| SessionProtocolError::corrupted(format!("encode record failed: {error}")))
}

fn validate_manifest(manifest: &SessionManifestV1) -> Result<(), SessionProtocolError> {
    if manifest.schema_version != SESSION_MANIFEST_SCHEMA_VERSION {
        return Err(SessionProtocolError::unsupported(format!(
            "unsupported manifest schemaVersion: {}",
            manifest.schema_version
        )));
    }
    if manifest.protocol_major != SESSION_PROTOCOL_MAJOR {
        return Err(SessionProtocolError::unsupported(format!(
            "unsupported protocolMajor: {}",
            manifest.protocol_major
        )));
    }
    if manifest.integrity_mode != SESSION_INTEGRITY_MODE {
        return Err(SessionProtocolError::unsupported(format!(
            "unsupported integrityMode: {}",
            manifest.integrity_mode
        )));
    }
    if manifest.session_id.trim().is_empty()
        || manifest.writer_version.trim().is_empty()
        || manifest.created_at_ms < 0
    {
        return Err(SessionProtocolError::corrupted(
            "manifest identity, writerVersion, or createdAtMs is invalid",
        ));
    }
    if let Some(feature) = manifest.required_features.first() {
        return Err(SessionProtocolError::unsupported(format!(
            "unsupported required feature: {feature}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::canonical_json;
    use crate::session::SESSION_EVENT_VERSION;
    use serde_json::json;

    #[test]
    fn wire_decoder_distinguishes_unsupported_from_corrupted() {
        let unsupported = parse_wire_record(&json!({
            "schemaVersion": "session.event.banana"
        }))
        .expect_err("unsupported");
        assert_eq!(unsupported.code(), "unsupported_session_protocol");

        let corrupted = parse_wire_record(&json!({
            "schemaVersion": SESSION_EVENT_SCHEMA_VERSION,
            "eventVersion": SESSION_EVENT_VERSION,
            "type": "user_message"
        }))
        .expect_err("corrupted");
        assert_eq!(corrupted.code(), "corrupted_session");
    }

    #[test]
    fn minimal_v1_fixture_has_a_stable_projection_digest() {
        let lines = include_str!("../../tests/fixtures/session_event_v1/minimal-session.jsonl")
            .lines()
            .collect::<Vec<_>>();
        let manifest_value = serde_json::from_str::<Value>(lines[0]).expect("manifest JSON");
        let manifest = parse_manifest(&manifest_value).expect("manifest");
        assert_eq!(manifest.session_id, "fixture-minimal");

        let records = lines[1..]
            .iter()
            .map(|line| {
                let value = serde_json::from_str::<Value>(line).expect("record JSON");
                let record = parse_wire_record(&value).expect("wire record");
                assert_eq!(wire_record_value(&record).expect("wire value"), value);
                record
            })
            .collect::<Vec<_>>();
        super::super::validate_sequenced_session_records(records.as_slice())
            .expect("sequenced fixture");
        let projection = super::super::reduce_events(
            manifest.session_id.as_str(),
            records.iter().map(|item| &item.event),
        )
        .expect("fixture projection");
        let digest_input = json!({
            "messages": projection.messages.iter().map(|(id, message)| {
                json!([id, message.text, message.status])
            }).collect::<Vec<_>>(),
            "agentRuns": projection.agent_runs.iter().map(|(id, agent_run)| {
                json!([id, format!("{:?}", agent_run.state)])
            }).collect::<Vec<_>>()
        });
        assert_eq!(
            canonical_json::sha256("centaeris.session_projection_fixture.v1", &digest_input)
                .expect("projection digest"),
            "sha256:8569527625e2077f8c16c377cc319dc850da59ced65f96cff658705d09a70d90"
        );
    }

    #[test]
    fn wire_decoder_fails_closed_but_keeps_tool_extensions_open() {
        let unsupported_version = parse_wire_record(&json!({
            "schemaVersion": SESSION_EVENT_SCHEMA_VERSION,
            "eventVersion": 2,
            "type": "banana"
        }))
        .expect_err("unknown event version");
        assert_eq!(unsupported_version.code(), "unsupported_session_protocol");

        let unsupported_feature = parse_manifest(&json!({
            "schemaVersion": SESSION_MANIFEST_SCHEMA_VERSION,
            "sessionId": "fixture",
            "protocolMajor": SESSION_PROTOCOL_MAJOR,
            "createdAtMs": 1,
            "writerVersion": "fixture.v1",
            "requiredFeatures": ["banana"],
            "integrityMode": SESSION_INTEGRITY_MODE
        }))
        .expect_err("unknown required feature");
        assert_eq!(unsupported_feature.code(), "unsupported_session_protocol");

        let mut extra_envelope = serde_json::from_str::<Value>(
            include_str!("../../tests/fixtures/session_event_v1/minimal-session.jsonl")
                .lines()
                .nth(1)
                .expect("fixture record"),
        )
        .expect("fixture JSON");
        extra_envelope["banana"] = json!(true);
        assert_eq!(
            parse_wire_record(&extra_envelope)
                .expect_err("known wire version rejects extra fields")
                .code(),
            "corrupted_session"
        );

        let call = parse_wire_record(&json!({
            "schemaVersion": SESSION_EVENT_SCHEMA_VERSION,
            "eventVersion": SESSION_EVENT_VERSION,
            "sequence": 1,
            "type": "tool_call",
            "eventId": "event-call",
            "sessionId": "fixture",
            "turnId": "turn-1",
            "agentRunId": "agent-run-1",
            "createdAtMs": 1,
            "payload": {
                "callId": "call-1",
                "toolName": "banana",
                "toolContractDigest": format!("sha256:{}", "a".repeat(64)),
                "providerId": "example.plugin",
                "normalizedInput": {"banana": true},
                "displayTarget": "banana"
            }
        }))
        .expect("unknown toolName is an extension point");
        let result = parse_wire_record(&json!({
            "schemaVersion": SESSION_EVENT_SCHEMA_VERSION,
            "eventVersion": SESSION_EVENT_VERSION,
            "sequence": 2,
            "type": "tool_result",
            "eventId": "event-result",
            "sessionId": "fixture",
            "turnId": "turn-1",
            "agentRunId": "agent-run-1",
            "createdAtMs": 2,
            "payload": {
                "callId": "call-1",
                "toolName": "banana",
                "resultState": "successWithOutput",
                "modelContent": "ok",
                "fullOutputPath": null,
                "outputStartByte": null,
                "outputByteLength": 2,
                "outputComplete": true,
                "summary": "ok",
                "operations": [{"type": "banana", "futureField": true}],
                "modelInputImages": [],
                "latencyMs": 1
            }
        }))
        .expect("unknown operation is display-only");
        super::super::reduce_events("fixture", [&call.event, &result.event])
            .expect("unknown tool history remains readable");
    }
}
