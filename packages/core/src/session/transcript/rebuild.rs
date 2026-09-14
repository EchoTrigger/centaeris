use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::session::{
    parse_wire_record, validate_event_shape, ActiveTombstoneLedger, SequencedSessionRecord,
    SessionRecordType,
};

use super::{
    parse_decimal_u64, require_identifier, TranscriptCheckpointRefsV1,
    TranscriptProjectionCommitV1, TranscriptProjectionGenerationRotationV1,
    TranscriptProjectionGenerationStorePortV1, TranscriptProjectionPayloadRequirementV1,
    TranscriptProjectionSourceEnvelopeV1, TranscriptProjectorV1, TranscriptResumeCursorV1,
    TRANSCRIPT_PROJECTION_VERSION_V1,
};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TranscriptRebuildLedgerFactV1 {
    pub envelope: TranscriptProjectionSourceEnvelopeV1,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tombstone_target_event_ids: Vec<String>,
}

/// Parses a strict flat source wire record into the payload-minimal first-pass fact. Only a
/// tombstone payload is decoded and validated because its targets define Core's active-set rules.
pub fn parse_transcript_rebuild_ledger_fact(
    raw_wire: &serde_json::Value,
) -> Result<TranscriptRebuildLedgerFactV1, String> {
    let envelope = super::parse_transcript_projection_source_envelope(raw_wire)?;
    let tombstone_target_event_ids = if envelope.event_type == SessionRecordType::Tombstone {
        let record = parse_wire_record(raw_wire).map_err(|error| error.to_string())?;
        record
            .event
            .payload
            .get("targetEventIds")
            .and_then(serde_json::Value::as_array)
            .expect("validated tombstone targetEventIds")
            .iter()
            .map(|target| {
                target
                    .as_str()
                    .expect("validated tombstone target string")
                    .to_string()
            })
            .collect()
    } else {
        Vec::new()
    };
    Ok(TranscriptRebuildLedgerFactV1 {
        envelope,
        tombstone_target_event_ids,
    })
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum TranscriptRebuildProjectionFactV1 {
    Envelope {
        envelope: TranscriptProjectionSourceEnvelopeV1,
        stream_id: String,
        applied_cursor: String,
    },
    FullRecord {
        record: SequencedSessionRecord,
        stream_id: String,
        applied_cursor: String,
    },
}

impl TranscriptRebuildProjectionFactV1 {
    fn envelope(&self) -> TranscriptProjectionSourceEnvelopeV1 {
        match self {
            Self::Envelope { envelope, .. } => envelope.clone(),
            Self::FullRecord { record, .. } => TranscriptProjectionSourceEnvelopeV1 {
                sequence: record.sequence,
                schema_version: record.event.schema_version.clone(),
                event_version: record.event.event_version,
                event_type: record.event.event_type,
                event_id: record.event.event_id.clone(),
                session_id: record.event.session_id.clone(),
            },
        }
    }

    fn stream_and_cursor(&self) -> (&str, &str) {
        match self {
            Self::Envelope {
                stream_id,
                applied_cursor,
                ..
            }
            | Self::FullRecord {
                stream_id,
                applied_cursor,
                ..
            } => (stream_id, applied_cursor),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TranscriptGenerationRebuildRequestV1 {
    pub session_id: String,
    pub projection_generation: String,
    pub target_source_high_water: String,
    pub expected_current_generation: Option<String>,
}

impl TranscriptGenerationRebuildRequestV1 {
    pub fn validate(&self) -> Result<(), String> {
        require_identifier(
            self.session_id.as_str(),
            "transcript generation rebuild sessionId",
        )?;
        require_identifier(
            self.projection_generation.as_str(),
            "transcript generation rebuild projectionGeneration",
        )?;
        let target = parse_decimal_u64(
            self.target_source_high_water.as_str(),
            "transcript generation rebuild targetSourceHighWater",
        )?;
        if target == 0 {
            return Err(
                "transcript generation rebuild targetSourceHighWater must be positive".to_string(),
            );
        }
        if let Some(expected) = self.expected_current_generation.as_deref() {
            require_identifier(
                expected,
                "transcript generation rebuild expectedCurrentGeneration",
            )?;
            if expected == self.projection_generation {
                return Err("transcript generation rebuild must change generation".to_string());
            }
        }
        Ok(())
    }
}

/// Supplies one immutable `(0, H]` snapshot through two streaming passes.
/// Pass one contains only envelopes and tombstone targets. In pass two the host follows Core's
/// event-type payload requirement: `EnvelopeOnly` and `TombstoneRebuild` stay payload-free, while
/// every `FullRecord` type is hydrated. Core may then discard an inactive full record using its
/// private tombstone ledger; the host never receives or reproduces that active-set decision.
pub trait TranscriptGenerationRebuildSourcePortV1 {
    fn scan_transcript_rebuild_ledger(
        &self,
        request: &TranscriptGenerationRebuildRequestV1,
        visitor: &mut dyn FnMut(TranscriptRebuildLedgerFactV1) -> Result<(), String>,
    ) -> Result<(), String>;

    fn scan_transcript_rebuild_projection(
        &self,
        request: &TranscriptGenerationRebuildRequestV1,
        visitor: &mut dyn FnMut(TranscriptRebuildProjectionFactV1) -> Result<(), String>,
    ) -> Result<(), String>;
}

/// Explicit/background rebuild operation. Normal page/patch paths must never invoke this function.
/// The new generation remains undiscoverable until every fact through H is committed, then Core
/// requests one compare-and-swap rotation of the per-session current-generation pointer.
pub fn rebuild_transcript_generation_v1<Source, Store>(
    request: &TranscriptGenerationRebuildRequestV1,
    source: &Source,
    store: &Store,
) -> Result<TranscriptProjectorV1, String>
where
    Source: TranscriptGenerationRebuildSourcePortV1 + ?Sized,
    Store: TranscriptProjectionGenerationStorePortV1 + ?Sized,
{
    request.validate()?;
    let target = parse_decimal_u64(
        request.target_source_high_water.as_str(),
        "transcript generation rebuild targetSourceHighWater",
    )?;
    let mut ledger = ActiveTombstoneLedger::default();
    let mut first_scan = RebuildScanIdentity::default();
    source.scan_transcript_rebuild_ledger(request, &mut |fact| {
        validate_envelope_sequence(request, target, &fact.envelope, &mut first_scan.sequence)?;
        if fact.envelope.event_type != SessionRecordType::Tombstone
            && !fact.tombstone_target_event_ids.is_empty()
        {
            return Err("non-tombstone rebuild ledger fact cannot carry targets".to_string());
        }
        ledger.apply_fact(
            request.session_id.as_str(),
            fact.envelope.event_id.as_str(),
            fact.envelope.session_id.as_str(),
            fact.envelope.event_type,
            fact.tombstone_target_event_ids.as_slice(),
        )?;
        first_scan.observe(&fact.envelope);
        Ok(())
    })?;
    require_complete_scan(first_scan.sequence, target)?;

    let mut projector = TranscriptProjectorV1::new(
        request.session_id.clone(),
        request.projection_generation.clone(),
    )?;
    let mut second_scan = RebuildScanIdentity::default();
    source.scan_transcript_rebuild_projection(request, &mut |fact| {
        let envelope = fact.envelope();
        validate_envelope_sequence(request, target, &envelope, &mut second_scan.sequence)?;
        second_scan.observe(&envelope);
        let inactive = ledger.is_inactive(envelope.event_id.as_str());
        let payload_requirement = envelope.transcript_payload_requirement()?;
        let neutral =
            inactive || payload_requirement != TranscriptProjectionPayloadRequirementV1::FullRecord;
        match (&fact, payload_requirement) {
            (
                TranscriptRebuildProjectionFactV1::FullRecord { .. },
                TranscriptProjectionPayloadRequirementV1::EnvelopeOnly
                | TranscriptProjectionPayloadRequirementV1::TombstoneRebuild,
            ) => {
                return Err(
                    "transcript generation rebuild neutral fact must use a payload-free envelope"
                        .to_string(),
                )
            }
            (
                TranscriptRebuildProjectionFactV1::Envelope { .. },
                TranscriptProjectionPayloadRequirementV1::FullRecord,
            ) => {
                return Err(format!(
                    "transcript source type {} requires its full payload",
                    envelope.event_type.as_str()
                ))
            }
            _ => {}
        }
        let checkpoint_refs =
            (envelope.sequence == target).then(|| checkpoint_refs(request, target));
        let commit_id = format!(
            "transcript-rebuild:{}:{}:{}",
            request.session_id, request.projection_generation, envelope.sequence
        );
        let (stream_id, applied_cursor) = fact.stream_and_cursor();
        require_identifier(stream_id, "transcript generation rebuild streamId")?;
        require_identifier(
            applied_cursor,
            "transcript generation rebuild appliedCursor",
        )?;
        if neutral {
            apply_no_display_commit(
                &mut projector,
                &envelope,
                stream_id,
                applied_cursor,
                commit_id.as_str(),
                request.projection_generation.as_str(),
                checkpoint_refs,
                store,
            )?;
        } else {
            let TranscriptRebuildProjectionFactV1::FullRecord { record, .. } = &fact else {
                unreachable!("active display fact shape checked above")
            };
            validate_event_shape(&record.event)?;
            projector.apply_and_commit(
                record,
                stream_id,
                applied_cursor,
                commit_id.as_str(),
                checkpoint_refs,
                store,
            )?;
        }
        Ok(())
    })?;
    require_complete_scan(second_scan.sequence, target)?;
    if first_scan.finish() != second_scan.finish() {
        return Err("transcript generation rebuild source changed between scans".to_string());
    }

    store.rotate_current_transcript_projection_generation(
        TranscriptProjectionGenerationRotationV1 {
            session_id: request.session_id.clone(),
            projection_version: TRANSCRIPT_PROJECTION_VERSION_V1.to_string(),
            expected_current_generation: request.expected_current_generation.clone(),
            next_generation: request.projection_generation.clone(),
            target_source_high_water: request.target_source_high_water.clone(),
        },
    )?;
    Ok(projector)
}

#[derive(Default)]
struct RebuildScanIdentity {
    sequence: u64,
    hasher: Sha256,
}

impl RebuildScanIdentity {
    fn observe(&mut self, envelope: &TranscriptProjectionSourceEnvelopeV1) {
        self.hasher.update(envelope.sequence.to_le_bytes());
        self.hasher.update(envelope.event_id.as_bytes());
        self.hasher.update([0]);
        self.hasher.update(envelope.event_type.as_str().as_bytes());
        self.hasher.update([0]);
    }

    fn finish(self) -> [u8; 32] {
        self.hasher.finalize().into()
    }
}

fn validate_envelope_sequence(
    request: &TranscriptGenerationRebuildRequestV1,
    target: u64,
    envelope: &TranscriptProjectionSourceEnvelopeV1,
    previous_sequence: &mut u64,
) -> Result<(), String> {
    envelope.validate()?;
    if envelope.session_id != request.session_id {
        return Err("transcript generation rebuild received a cross-session event".to_string());
    }
    if envelope.sequence > target || envelope.sequence != previous_sequence.saturating_add(1) {
        return Err(
            "transcript generation rebuild source must contain every sequence through targetSourceHighWater"
                .to_string(),
        );
    }
    *previous_sequence = envelope.sequence;
    Ok(())
}

fn require_complete_scan(last_sequence: u64, target: u64) -> Result<(), String> {
    if last_sequence != target {
        return Err(
            "transcript generation rebuild source did not reach targetSourceHighWater".to_string(),
        );
    }
    Ok(())
}

fn checkpoint_refs(
    request: &TranscriptGenerationRebuildRequestV1,
    target: u64,
) -> TranscriptCheckpointRefsV1 {
    TranscriptCheckpointRefsV1 {
        frontier_ref: format!(
            "transcript-frontier:{}:{}:{}",
            request.session_id, request.projection_generation, target
        ),
        block_index_ref: format!(
            "transcript-block-index:{}:{}:{}",
            request.session_id, request.projection_generation, target
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_no_display_commit<Store>(
    projector: &mut TranscriptProjectorV1,
    envelope: &TranscriptProjectionSourceEnvelopeV1,
    stream_id: &str,
    applied_cursor: &str,
    commit_id: &str,
    projection_generation: &str,
    checkpoint_refs: Option<TranscriptCheckpointRefsV1>,
    store: &Store,
) -> Result<(), String>
where
    Store: TranscriptProjectionGenerationStorePortV1 + ?Sized,
{
    let expected_source_high_water = envelope.sequence - 1;
    let mut candidate = projector.clone();
    candidate.advance_without_display(envelope.sequence, stream_id, applied_cursor)?;
    let recovery = checkpoint_refs
        .map(|refs| {
            refs.validate()?;
            candidate.checkpoint_recovery(refs.frontier_ref.as_str(), refs.block_index_ref.as_str())
        })
        .transpose()?;
    store.commit_transcript_projection(TranscriptProjectionCommitV1 {
        commit_id: commit_id.to_string(),
        session_id: envelope.session_id.clone(),
        projection_version: TRANSCRIPT_PROJECTION_VERSION_V1.to_string(),
        projection_generation: projection_generation.to_string(),
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
    *projector = candidate;
    Ok(())
}
