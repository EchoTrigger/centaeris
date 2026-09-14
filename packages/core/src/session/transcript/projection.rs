use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::session::{
    validate_event_shape, SequencedSessionRecord, SessionLogRecord, SessionRecordType,
    SESSION_EVENT_ID_MAX_BYTES, SESSION_EVENT_SCHEMA_VERSION,
};
use crate::tool::layer::ToolResultState;

use super::{
    require_identifier, TranscriptBlockBodyV1, TranscriptBlockIndexV1, TranscriptBlockStatusV1,
    TranscriptBlockV1, TranscriptCheckpointRefsV1, TranscriptCommittedBlockV1,
    TranscriptContentRefV1, TranscriptOrderKeyV1, TranscriptPagePolicyV1, TranscriptPageV1,
    TranscriptPatchV1, TranscriptProjectionCheckpointV1, TranscriptProjectionCommitV1,
    TranscriptProjectionFrontierV1, TranscriptProjectionOpenToolV1, TranscriptProjectionRecoveryV1,
    TranscriptProjectionStorePort, TranscriptResumeCursorV1, TranscriptTextContentV1,
    TRANSCRIPT_CHECKPOINT_SCHEMA_V1, TRANSCRIPT_FRONTIER_SCHEMA_V1,
    TRANSCRIPT_PAGE_INLINE_CONTENT_MAX_BYTES, TRANSCRIPT_PATCH_SCHEMA_V1,
    TRANSCRIPT_PROJECTION_VERSION_V1,
};

/// The payload-free subset of a `session.event.v1` source fact.
///
/// Validation of this type deliberately covers only the fields represented here. It never means
/// that the omitted source payload has been decoded or validated.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TranscriptProjectionSourceEnvelopeV1 {
    pub sequence: u64,
    pub schema_version: String,
    pub event_version: u32,
    #[serde(rename = "type")]
    pub event_type: SessionRecordType,
    pub event_id: String,
    pub session_id: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum TranscriptProjectionPayloadRequirementV1 {
    EnvelopeOnly,
    FullRecord,
    TombstoneRebuild,
}

impl TranscriptProjectionSourceEnvelopeV1 {
    pub fn validate(&self) -> Result<(), String> {
        if self.sequence == 0 {
            return Err("transcript source envelope sequence must be positive".to_string());
        }
        if self.schema_version != SESSION_EVENT_SCHEMA_VERSION {
            return Err(format!(
                "transcript source envelope schemaVersion mismatch: expected {SESSION_EVENT_SCHEMA_VERSION}, got {}",
                self.schema_version
            ));
        }
        if self.event_version != self.event_type.event_version() {
            return Err(format!(
                "transcript source envelope eventVersion is unsupported for {}",
                self.event_type.as_str()
            ));
        }
        require_identifier(self.event_id.as_str(), "transcript source envelope eventId")?;
        if self.event_id.len() > SESSION_EVENT_ID_MAX_BYTES {
            return Err(format!(
                "transcript source envelope eventId exceeds {SESSION_EVENT_ID_MAX_BYTES} bytes"
            ));
        }
        require_identifier(
            self.session_id.as_str(),
            "transcript source envelope sessionId",
        )?;
        Ok(())
    }

    pub fn transcript_payload_requirement(
        &self,
    ) -> Result<TranscriptProjectionPayloadRequirementV1, String> {
        self.validate()?;
        Ok(if self.event_type == SessionRecordType::Tombstone {
            TranscriptProjectionPayloadRequirementV1::TombstoneRebuild
        } else if transcript_event_type_is_payload_free(self.event_type) {
            TranscriptProjectionPayloadRequirementV1::EnvelopeOnly
        } else {
            TranscriptProjectionPayloadRequirementV1::FullRecord
        })
    }
}

/// Parses only the strict flat `SessionRecordWireV1` envelope.
/// The payload must be an object, but its contents are intentionally neither cloned nor decoded.
pub fn parse_transcript_projection_source_envelope(
    value: &serde_json::Value,
) -> Result<TranscriptProjectionSourceEnvelopeV1, String> {
    let event = value
        .as_object()
        .ok_or_else(|| "session record wire must be an object".to_string())?;
    reject_unknown_keys(
        event,
        &[
            "schemaVersion",
            "eventVersion",
            "sequence",
            "type",
            "eventId",
            "sessionId",
            "turnId",
            "agentRunId",
            "createdAtMs",
            "payload",
        ],
        "session record wire",
    )?;
    let sequence = event
        .get("sequence")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| "session record wire sequence must be a u64".to_string())?;
    let required_string = |field: &str| {
        event
            .get(field)
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| format!("session event envelope {field} must be a string"))
    };
    for optional in ["turnId", "agentRunId"] {
        if let Some(value) = event.get(optional) {
            if value.is_null() {
                continue;
            }
            let value = value
                .as_str()
                .ok_or_else(|| format!("session event envelope {optional} must be a string"))?;
            if value.trim().is_empty() {
                return Err(format!(
                    "session event envelope {optional} must not be empty"
                ));
            }
        }
    }
    let created_at_ms = event
        .get("createdAtMs")
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| "session event envelope createdAtMs must be an i64".to_string())?;
    if created_at_ms < 0 {
        return Err("session event envelope createdAtMs must not be negative".to_string());
    }
    if !event
        .get("payload")
        .is_some_and(serde_json::Value::is_object)
    {
        return Err("session event envelope payload must be an object".to_string());
    }
    let event_type = serde_json::from_value::<SessionRecordType>(
        event
            .get("type")
            .cloned()
            .ok_or_else(|| "session event envelope type is required".to_string())?,
    )
    .map_err(|error| format!("session event envelope type is unsupported: {error}"))?;
    let envelope = TranscriptProjectionSourceEnvelopeV1 {
        sequence,
        schema_version: required_string("schemaVersion")?,
        event_version: event
            .get("eventVersion")
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| "session event envelope eventVersion must be a u32".to_string())?,
        event_type,
        event_id: required_string("eventId")?,
        session_id: required_string("sessionId")?,
    };
    envelope.validate()?;
    Ok(envelope)
}

fn reject_unknown_keys(
    object: &serde_json::Map<String, serde_json::Value>,
    allowed: &[&str],
    context: &str,
) -> Result<(), String> {
    if let Some(key) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(format!("{context} contains unknown field: {key}"));
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TranscriptProjectionUpdateV1 {
    Patch(TranscriptPatchV1),
    NoDisplayChange {
        source_high_water: String,
        stream_id: String,
        applied_cursor: String,
    },
    ViewInvalidated {
        source_high_water: String,
        reason: String,
    },
}

#[derive(Clone, Debug)]
struct ProjectedTool {
    order_key: TranscriptOrderKeyV1,
    tool_name: String,
    revision: u64,
    block: TranscriptBlockV1,
}

#[derive(Clone, Debug)]
pub struct TranscriptProjectorV1 {
    session_id: String,
    index: TranscriptBlockIndexV1,
    tools: HashMap<String, ProjectedTool>,
    last_sequence: u64,
    invalidated: bool,
}

impl TranscriptProjectorV1 {
    pub fn new(session_id: String, projection_generation: String) -> Result<Self, String> {
        Ok(Self {
            index: TranscriptBlockIndexV1::new(session_id.clone(), projection_generation)?,
            session_id,
            tools: HashMap::new(),
            last_sequence: 0,
            invalidated: false,
        })
    }

    pub fn checkpoint_recovery(
        &self,
        frontier_ref: &str,
        block_index_ref: &str,
    ) -> Result<TranscriptProjectionRecoveryV1, String> {
        if self.invalidated {
            return Err("transcript projection view is invalidated".to_string());
        }
        let mut open_tools = self
            .tools
            .iter()
            .map(|(call_id, tool)| TranscriptProjectionOpenToolV1 {
                call_id: call_id.clone(),
                block: tool.block.clone(),
            })
            .collect::<Vec<_>>();
        open_tools.sort_by(|left, right| left.block.order_key.cmp(&right.block.order_key));
        let frontier = TranscriptProjectionFrontierV1 {
            schema: TRANSCRIPT_FRONTIER_SCHEMA_V1.to_string(),
            frontier_ref: frontier_ref.to_string(),
            session_id: self.session_id.clone(),
            projection_version: TRANSCRIPT_PROJECTION_VERSION_V1.to_string(),
            projection_generation: self.index.projection_generation().to_string(),
            source_high_water: self.last_sequence.to_string(),
            open_tools,
        };
        let checkpoint = TranscriptProjectionCheckpointV1 {
            schema: TRANSCRIPT_CHECKPOINT_SCHEMA_V1.to_string(),
            session_id: self.session_id.clone(),
            projection_version: TRANSCRIPT_PROJECTION_VERSION_V1.to_string(),
            projection_generation: self.index.projection_generation().to_string(),
            source_high_water: self.last_sequence.to_string(),
            frontier_ref: frontier_ref.to_string(),
            block_index_ref: block_index_ref.to_string(),
        };
        let recovery = TranscriptProjectionRecoveryV1 {
            checkpoint,
            frontier,
        };
        recovery.validate()?;
        Ok(recovery)
    }

    pub fn from_checkpoint_recovery(
        recovery: TranscriptProjectionRecoveryV1,
    ) -> Result<Self, String> {
        recovery.validate()?;
        let source_high_water = recovery
            .frontier
            .source_high_water
            .parse::<u64>()
            .map_err(|_| "transcript frontier sourceHighWater exceeds u64".to_string())?;
        let mut index = TranscriptBlockIndexV1::new(
            recovery.frontier.session_id.clone(),
            recovery.frontier.projection_generation.clone(),
        )?;
        let mut tools = HashMap::new();
        for open in recovery.frontier.open_tools {
            let sequence = open.block.order_key.source_sequence_value()?;
            index.apply_committed(sequence, open.block.clone())?;
            let (tool_name, revision) = match &open.block.body {
                TranscriptBlockBodyV1::Tool { tool_name, .. } => (
                    tool_name.clone(),
                    open.block
                        .block_revision
                        .parse::<u64>()
                        .map_err(|_| "transcript frontier blockRevision exceeds u64".to_string())?,
                ),
                _ => unreachable!("validated open tool body"),
            };
            tools.insert(
                open.call_id,
                ProjectedTool {
                    order_key: open.block.order_key.clone(),
                    tool_name,
                    revision,
                    block: open.block,
                },
            );
        }
        index.advance_source_high_water(source_high_water)?;
        Ok(Self {
            session_id: recovery.frontier.session_id,
            index,
            tools,
            last_sequence: source_high_water,
            invalidated: false,
        })
    }

    pub fn apply(
        &mut self,
        record: &SequencedSessionRecord,
        stream_id: &str,
        applied_cursor: &str,
    ) -> Result<TranscriptProjectionUpdateV1, String> {
        if self.invalidated {
            return Err("transcript projection view is invalidated".to_string());
        }
        validate_event_shape(&record.event)?;
        if record.event.session_id != self.session_id {
            return Err("transcript projection received a cross-session event".to_string());
        }
        if record.sequence == 0 || record.sequence <= self.last_sequence {
            return Err("transcript projection sequence is not increasing".to_string());
        }

        if record.event.event_type == SessionRecordType::Tombstone {
            self.index.advance_source_high_water(record.sequence)?;
            self.index.record_resume_cursor(
                record.sequence,
                stream_id.to_string(),
                applied_cursor.to_string(),
            )?;
            self.last_sequence = record.sequence;
            self.invalidated = true;
            return Ok(TranscriptProjectionUpdateV1::ViewInvalidated {
                source_high_water: record.sequence.to_string(),
                reason: "tombstone".to_string(),
            });
        }

        let block = self.project_block(record)?;
        if let Some(block) = block.as_ref() {
            self.index.apply_committed(record.sequence, block.clone())?;
        } else {
            self.index.advance_source_high_water(record.sequence)?;
        }
        self.index.record_resume_cursor(
            record.sequence,
            stream_id.to_string(),
            applied_cursor.to_string(),
        )?;
        self.last_sequence = record.sequence;

        let Some(block) = block else {
            return Ok(TranscriptProjectionUpdateV1::NoDisplayChange {
                source_high_water: record.sequence.to_string(),
                stream_id: stream_id.to_string(),
                applied_cursor: applied_cursor.to_string(),
            });
        };
        let patch = TranscriptPatchV1 {
            schema: TRANSCRIPT_PATCH_SCHEMA_V1.to_string(),
            session_id: self.session_id.clone(),
            projection_version: TRANSCRIPT_PROJECTION_VERSION_V1.to_string(),
            projection_generation: self.index.projection_generation().to_string(),
            source_high_water: record.sequence.to_string(),
            stream_id: stream_id.to_string(),
            applied_cursor: applied_cursor.to_string(),
            upserts: vec![block],
            removals: Vec::new(),
        };
        patch.validate()?;
        Ok(TranscriptProjectionUpdateV1::Patch(patch))
    }

    /// Applies a source fact without decoding its payload, but only when Core has proven that its
    /// event type cannot change transcript display state in projection v1.
    pub fn apply_envelope(
        &mut self,
        envelope: &TranscriptProjectionSourceEnvelopeV1,
        stream_id: &str,
        applied_cursor: &str,
    ) -> Result<TranscriptProjectionUpdateV1, String> {
        if self.invalidated {
            return Err("transcript projection view is invalidated".to_string());
        }
        envelope.validate()?;
        if envelope.session_id != self.session_id {
            return Err(
                "transcript projection received a cross-session event envelope".to_string(),
            );
        }
        if envelope.sequence <= self.last_sequence {
            return Err("transcript projection sequence is not increasing".to_string());
        }
        if envelope.transcript_payload_requirement()?
            != TranscriptProjectionPayloadRequirementV1::EnvelopeOnly
        {
            return Err(format!(
                "transcript source type {} requires its full payload",
                envelope.event_type.as_str()
            ));
        }
        self.advance_without_display(envelope.sequence, stream_id, applied_cursor)?;
        Ok(TranscriptProjectionUpdateV1::NoDisplayChange {
            source_high_water: envelope.sequence.to_string(),
            stream_id: stream_id.to_string(),
            applied_cursor: applied_cursor.to_string(),
        })
    }

    pub fn apply_envelope_and_commit<S: TranscriptProjectionStorePort + ?Sized>(
        &mut self,
        envelope: &TranscriptProjectionSourceEnvelopeV1,
        stream_id: &str,
        applied_cursor: &str,
        commit_id: &str,
        checkpoint_refs: Option<TranscriptCheckpointRefsV1>,
        store: &S,
    ) -> Result<TranscriptProjectionUpdateV1, String> {
        let expected_source_high_water = self.last_sequence;
        let mut candidate = self.clone();
        let update = candidate.apply_envelope(envelope, stream_id, applied_cursor)?;
        let recovery = match (&update, checkpoint_refs) {
            (TranscriptProjectionUpdateV1::ViewInvalidated { .. }, Some(refs)) => {
                refs.validate()?;
                None
            }
            (TranscriptProjectionUpdateV1::ViewInvalidated { .. }, None) => None,
            (_, refs) => refs
                .map(|refs| {
                    refs.validate()?;
                    candidate.checkpoint_recovery(
                        refs.frontier_ref.as_str(),
                        refs.block_index_ref.as_str(),
                    )
                })
                .transpose()?,
        };
        store.commit_transcript_projection(TranscriptProjectionCommitV1 {
            commit_id: commit_id.to_string(),
            session_id: self.session_id.clone(),
            projection_version: TRANSCRIPT_PROJECTION_VERSION_V1.to_string(),
            projection_generation: self.index.projection_generation().to_string(),
            expected_source_high_water: expected_source_high_water.to_string(),
            source_high_water: envelope.sequence.to_string(),
            upserts: Vec::new(),
            resume_cursors: vec![TranscriptResumeCursorV1 {
                stream_id: stream_id.to_string(),
                cursor: applied_cursor.to_string(),
            }],
            checkpoint: recovery.as_ref().map(|value| value.checkpoint.clone()),
            frontier: recovery.map(|value| value.frontier),
            invalidation_reason: None,
        })?;
        *self = candidate;
        Ok(update)
    }

    pub fn apply_and_commit<S: TranscriptProjectionStorePort + ?Sized>(
        &mut self,
        record: &SequencedSessionRecord,
        stream_id: &str,
        applied_cursor: &str,
        commit_id: &str,
        checkpoint_refs: Option<TranscriptCheckpointRefsV1>,
        store: &S,
    ) -> Result<TranscriptProjectionUpdateV1, String> {
        let expected_source_high_water = self.last_sequence;
        let mut candidate = self.clone();
        let update = candidate.apply(record, stream_id, applied_cursor)?;
        let (upserts, invalidation_reason) = match &update {
            TranscriptProjectionUpdateV1::Patch(patch) => (
                patch
                    .upserts
                    .iter()
                    .cloned()
                    .map(|block| TranscriptCommittedBlockV1 {
                        applied_source_sequence: record.sequence.to_string(),
                        block,
                    })
                    .collect(),
                None,
            ),
            TranscriptProjectionUpdateV1::NoDisplayChange { .. } => (Vec::new(), None),
            TranscriptProjectionUpdateV1::ViewInvalidated { reason, .. } => {
                (Vec::new(), Some(reason.clone()))
            }
        };
        let recovery = match (&update, checkpoint_refs) {
            (TranscriptProjectionUpdateV1::ViewInvalidated { .. }, Some(refs)) => {
                refs.validate()?;
                None
            }
            (TranscriptProjectionUpdateV1::ViewInvalidated { .. }, None) => None,
            (_, refs) => refs
                .map(|refs| {
                    refs.validate()?;
                    candidate.checkpoint_recovery(
                        refs.frontier_ref.as_str(),
                        refs.block_index_ref.as_str(),
                    )
                })
                .transpose()?,
        };
        let commit = TranscriptProjectionCommitV1 {
            commit_id: commit_id.to_string(),
            session_id: self.session_id.clone(),
            projection_version: TRANSCRIPT_PROJECTION_VERSION_V1.to_string(),
            projection_generation: self.index.projection_generation().to_string(),
            expected_source_high_water: expected_source_high_water.to_string(),
            source_high_water: record.sequence.to_string(),
            upserts,
            resume_cursors: vec![TranscriptResumeCursorV1 {
                stream_id: stream_id.to_string(),
                cursor: applied_cursor.to_string(),
            }],
            checkpoint: recovery.as_ref().map(|value| value.checkpoint.clone()),
            frontier: recovery.map(|value| value.frontier),
            invalidation_reason,
        };
        store.commit_transcript_projection(commit)?;
        *self = candidate;
        Ok(update)
    }

    pub fn page_at(
        &self,
        source_high_water: u64,
        older_cursor: Option<&str>,
        policy: TranscriptPagePolicyV1,
    ) -> Result<TranscriptPageV1, String> {
        if self.invalidated {
            return Err("transcript projection view is invalidated".to_string());
        }
        self.index.page_at(source_high_water, older_cursor, policy)
    }

    fn project_block(
        &mut self,
        record: &SequencedSessionRecord,
    ) -> Result<Option<TranscriptBlockV1>, String> {
        let event = &record.event;
        let payload = event
            .payload
            .as_object()
            .expect("session event shape validation requires an object payload");
        let order_key = TranscriptOrderKeyV1 {
            source_sequence: record.sequence.to_string(),
            ordinal: 0,
        };
        let block = match event.event_type {
            SessionRecordType::UserMessage => {
                let block_id = payload_string(payload, "messageId")?;
                let text = payload_string_allow_empty(payload, "text")?;
                Some(TranscriptBlockV1 {
                    block_id,
                    block_revision: "1".to_string(),
                    order_key,
                    body: TranscriptBlockBodyV1::UserText {
                        content: text_content(event, "text", text, 1),
                    },
                })
            }
            SessionRecordType::AssistantMessage => {
                let block_id = payload_string(payload, "messageId")?;
                let text = payload_string_allow_empty(payload, "modelMarkdown")?;
                let status = match payload_string(payload, "status")?.as_str() {
                    "done" => TranscriptBlockStatusV1::Completed,
                    "error" => TranscriptBlockStatusV1::Failed,
                    _ => unreachable!("validated assistant status"),
                };
                Some(TranscriptBlockV1 {
                    block_id,
                    block_revision: "1".to_string(),
                    order_key,
                    body: TranscriptBlockBodyV1::AssistantText {
                        content: text_content(event, "modelMarkdown", text, 1),
                        status,
                    },
                })
            }
            SessionRecordType::ReasoningBlock => {
                let block_id = payload_string(payload, "blockId")?;
                let request_id = payload_string(payload, "requestId")?;
                let text = payload_string(payload, "text")?;
                let status = match payload_string(payload, "status")?.as_str() {
                    "done" => TranscriptBlockStatusV1::Completed,
                    "interrupted" => TranscriptBlockStatusV1::Interrupted,
                    _ => unreachable!("validated reasoning status"),
                };
                Some(TranscriptBlockV1 {
                    block_id,
                    block_revision: "1".to_string(),
                    order_key,
                    body: TranscriptBlockBodyV1::Reasoning {
                        request_id,
                        content: text_content(event, "text", text, 1),
                        status,
                    },
                })
            }
            SessionRecordType::ToolCall => {
                let call_id = payload_string(payload, "callId")?;
                let tool_name = payload_string(payload, "toolName")?;
                let (summary, summary_ref) = bounded_text(
                    event,
                    "displayTarget",
                    payload_string(payload, "displayTarget")?,
                    1,
                );
                let block_id = format!("tool:{call_id}");
                if self.tools.contains_key(call_id.as_str()) {
                    return Err("transcript projection duplicates tool callId".to_string());
                }
                let block = TranscriptBlockV1 {
                    block_id,
                    block_revision: "1".to_string(),
                    order_key: order_key.clone(),
                    body: TranscriptBlockBodyV1::Tool {
                        call_id: call_id.clone(),
                        tool_name: tool_name.clone(),
                        status: TranscriptBlockStatusV1::Running,
                        summary,
                        summary_ref,
                        output_ref: None,
                    },
                };
                self.tools.insert(
                    call_id,
                    ProjectedTool {
                        order_key,
                        tool_name,
                        revision: 1,
                        block: block.clone(),
                    },
                );
                Some(block)
            }
            SessionRecordType::ToolResult => {
                let call_id = payload_string(payload, "callId")?;
                let tool_name = payload_string(payload, "toolName")?;
                let tool = self.tools.get(call_id.as_str()).cloned().ok_or_else(|| {
                    "transcript tool result has no projected tool call".to_string()
                })?;
                if tool.tool_name != tool_name {
                    return Err("transcript tool result toolName mismatch".to_string());
                }
                let revision = tool
                    .revision
                    .checked_add(1)
                    .ok_or_else(|| "transcript tool block revision overflow".to_string())?;
                let result_state: ToolResultState = serde_json::from_value(
                    payload
                        .get("resultState")
                        .cloned()
                        .expect("validated tool resultState"),
                )
                .expect("validated tool resultState enum");
                let status = match result_state {
                    ToolResultState::SuccessWithOutput
                    | ToolResultState::SuccessNoOutput
                    | ToolResultState::SuccessNoMatches => TranscriptBlockStatusV1::Completed,
                    ToolResultState::Failed | ToolResultState::Denied => {
                        TranscriptBlockStatusV1::Failed
                    }
                    ToolResultState::Aborted => TranscriptBlockStatusV1::Interrupted,
                };
                let output_byte_length = payload
                    .get("outputByteLength")
                    .and_then(|value| value.as_u64())
                    .expect("validated outputByteLength");
                let output_ref = (output_byte_length > 0).then(|| TranscriptContentRefV1 {
                    ref_id: format!("tool-output:{call_id}"),
                    revision: revision.to_string(),
                    byte_length: output_byte_length.to_string(),
                });
                self.tools.remove(call_id.as_str());
                let (summary, summary_ref) = bounded_text(
                    event,
                    "summary",
                    payload_string(payload, "summary")?,
                    revision,
                );
                Some(TranscriptBlockV1 {
                    block_id: format!("tool:{call_id}"),
                    block_revision: revision.to_string(),
                    order_key: tool.order_key.clone(),
                    body: TranscriptBlockBodyV1::Tool {
                        call_id,
                        tool_name,
                        status,
                        summary,
                        summary_ref,
                        output_ref,
                    },
                })
            }
            SessionRecordType::PhaseEvent => Some(TranscriptBlockV1 {
                block_id: format!("notice:{}", event.event_id),
                block_revision: "1".to_string(),
                order_key,
                body: TranscriptBlockBodyV1::Notice {
                    notice_type: payload_string(payload, "stage")?,
                    content: text_content(event, "message", payload_string(payload, "message")?, 1),
                    status: TranscriptBlockStatusV1::Running,
                },
            }),
            SessionRecordType::SessionMeta
            | SessionRecordType::AgentRunStarted
            | SessionRecordType::AgentRunRecoveryAttempted
            | SessionRecordType::AgentRunExecutionStarted
            | SessionRecordType::AgentRunExecutionEnded
            | SessionRecordType::TurnSupplement
            | SessionRecordType::ModelRequestStarted
            | SessionRecordType::ProviderUsage
            | SessionRecordType::ExternalEvidenceRef
            | SessionRecordType::CitationRecorded
            | SessionRecordType::ArtifactPublished
            | SessionRecordType::Compaction
            | SessionRecordType::CheckpointRef
            | SessionRecordType::FileFact
            | SessionRecordType::AgentRunCompleted
            | SessionRecordType::AgentRunFailed
            | SessionRecordType::AgentRunInterrupted => None,
            SessionRecordType::Tombstone => unreachable!("tombstone handled before projection"),
        };
        Ok(block)
    }

    pub(super) fn advance_without_display(
        &mut self,
        sequence: u64,
        stream_id: &str,
        applied_cursor: &str,
    ) -> Result<(), String> {
        if self.invalidated {
            return Err("transcript projection view is invalidated".to_string());
        }
        if sequence == 0 || sequence <= self.last_sequence {
            return Err("transcript projection sequence is not increasing".to_string());
        }
        require_identifier(stream_id, "transcript source streamId")?;
        require_identifier(applied_cursor, "transcript source appliedCursor")?;
        self.index.advance_source_high_water(sequence)?;
        self.index.record_resume_cursor(
            sequence,
            stream_id.to_string(),
            applied_cursor.to_string(),
        )?;
        self.last_sequence = sequence;
        Ok(())
    }
}

pub(super) fn transcript_event_type_is_payload_free(event_type: SessionRecordType) -> bool {
    matches!(
        event_type,
        SessionRecordType::SessionMeta
            | SessionRecordType::AgentRunStarted
            | SessionRecordType::AgentRunRecoveryAttempted
            | SessionRecordType::AgentRunExecutionStarted
            | SessionRecordType::AgentRunExecutionEnded
            | SessionRecordType::TurnSupplement
            | SessionRecordType::ModelRequestStarted
            | SessionRecordType::ProviderUsage
            | SessionRecordType::ExternalEvidenceRef
            | SessionRecordType::CitationRecorded
            | SessionRecordType::ArtifactPublished
            | SessionRecordType::Compaction
            | SessionRecordType::CheckpointRef
            | SessionRecordType::FileFact
            | SessionRecordType::AgentRunCompleted
            | SessionRecordType::AgentRunFailed
            | SessionRecordType::AgentRunInterrupted
    )
}

fn text_content(
    event: &SessionLogRecord,
    field: &str,
    text: String,
    revision: u64,
) -> TranscriptTextContentV1 {
    let (inline, reference) = bounded_text(event, field, text, revision);
    match (inline, reference) {
        (Some(text), None) => TranscriptTextContentV1::inline(text),
        (None, Some(reference)) => TranscriptTextContentV1::referenced(reference),
        _ => unreachable!("bounded text has exactly one representation"),
    }
}

fn bounded_text(
    event: &SessionLogRecord,
    field: &str,
    text: String,
    revision: u64,
) -> (Option<String>, Option<TranscriptContentRefV1>) {
    if text.len() <= TRANSCRIPT_PAGE_INLINE_CONTENT_MAX_BYTES {
        (Some(text), None)
    } else {
        let byte_length = text.len().to_string();
        (
            None,
            Some(TranscriptContentRefV1 {
                ref_id: format!("session-event:{}:{field}", event.event_id),
                revision: revision.to_string(),
                byte_length,
            }),
        )
    }
}

fn payload_string(
    payload: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<String, String> {
    payload
        .get(field)
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("transcript source payload {field} is required"))
}

fn payload_string_allow_empty(
    payload: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<String, String> {
    payload
        .get(field)
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .ok_or_else(|| format!("transcript source payload {field} must be a string"))
}
