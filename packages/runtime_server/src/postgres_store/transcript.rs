use centaeris_core::session::transcript::{
    assemble_transcript_page_from_newest, parse_transcript_projection_source_envelope,
    parse_transcript_rebuild_ledger_fact, rebuild_transcript_generation_v1,
    transcript_page_before_order, transcript_patch_read_result_serialized_bytes,
    validate_transcript_resume_cursor_read, TranscriptBlockV1, TranscriptCheckpointRefsV1,
    TranscriptGenerationRebuildRequestV1, TranscriptGenerationRebuildSourcePortV1,
    TranscriptPageReadRequestV1, TranscriptPageReadResultV1, TranscriptPatchReadRequestV1,
    TranscriptPatchReadResultV1, TranscriptPatchV1, TranscriptPersistentPageQueryWorkV1,
    TranscriptPersistentPatchQueryWorkV1, TranscriptProjectionCheckpointV1,
    TranscriptProjectionCommitDispositionV1, TranscriptProjectionCommitV1,
    TranscriptProjectionCurrentGenerationV1, TranscriptProjectionFrontierV1,
    TranscriptProjectionGenerationRotationDispositionV1, TranscriptProjectionGenerationRotationV1,
    TranscriptProjectionGenerationStorePortV1, TranscriptProjectionHeadV1,
    TranscriptProjectionPayloadRequirementV1, TranscriptProjectionRecoveryV1,
    TranscriptProjectionSourceEnvelopeV1, TranscriptProjectionStorePort, TranscriptProjectorV1,
    TranscriptRebuildLedgerFactV1, TranscriptRebuildProjectionFactV1, TranscriptResumeCursorV1,
    TRANSCRIPT_PAGE_RESUME_CURSOR_MAX_COUNT, TRANSCRIPT_PATCH_READ_MAX_PATCHES,
    TRANSCRIPT_PATCH_READ_SERIALIZED_MAX_BYTES, TRANSCRIPT_PATCH_SCHEMA_V1,
    TRANSCRIPT_PROJECTION_SLICE_MAX_BYTES, TRANSCRIPT_PROJECTION_SLICE_MAX_EVENTS,
    TRANSCRIPT_PROJECTION_VERSION_V1,
};
use centaeris_core::session::{parse_wire_record, SequencedSessionRecord};
use postgres::{GenericClient, Row};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

use super::PostgresRuntimeStore;

/// Bump whenever the stored `TranscriptBlockV1` / `TranscriptTextContentV1`
/// encoding changes. A different value yields a distinct projection generation,
/// so projections written by an older encoding are invalidated and rebuilt
/// instead of being decoded under the new field names.
const WORKSPACE_TRANSCRIPT_BLOCK_SCHEMA_VERSION: u32 = 2;

fn workspace_transcript_generation(name: &str) -> String {
    format!("{name}.blocks{WORKSPACE_TRANSCRIPT_BLOCK_SCHEMA_VERSION}")
}

fn workspace_transcript_initial_generation() -> String {
    workspace_transcript_generation("generation-1")
}

fn workspace_transcript_generation_has_current_encoding(generation: &str) -> bool {
    generation.ends_with(&format!(
        ".blocks{WORKSPACE_TRANSCRIPT_BLOCK_SCHEMA_VERSION}"
    ))
}
const WORKSPACE_TRANSCRIPT_STREAM_ID_V1: &str = "workspace-transcript.v1";
static TRANSCRIPT_REBUILDS: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

#[cfg(test)]
thread_local! {
    static CATCH_UP_AFTER_INITIAL_READ: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranscriptCatchUpProgress {
    pub source_high_water: u64,
    pub projected_high_water: u64,
    pub events_projected: usize,
    pub source_bytes_projected: usize,
    pub oversized_source_events: usize,
    pub caught_up: bool,
}

type TranscriptCatchUpSnapshot = (Option<TranscriptProjectionHeadV1>, Option<(String, String)>);

impl PostgresRuntimeStore {
    pub fn read_transcript_event_content(
        &self,
        request: &centaeris_core::session::transcript::TranscriptContentRangeReadRequestV1,
    ) -> Result<centaeris_core::session::transcript::TranscriptContentRangeV1, String> {
        request.validate()?;
        let (event_id, _) = request.event_reference()?;
        self.with_client(|client| {
            // Bind the source lookup to the currently published generation in one query.
            let row = client.query_opt(
                "SELECT e.payload::text FROM public.app_core_sessionevent e JOIN runtime.transcript_projection_current_generations g ON g.session_id=e.session_id JOIN runtime.transcript_projection_heads h ON h.session_id=g.session_id AND h.projection_version=g.projection_version AND h.projection_generation=g.projection_generation WHERE e.session_id=$1 AND e.\"eventId\"=$2 AND g.projection_generation=$3 AND g.projection_version=$4 AND e.sequence<=h.source_high_water AND h.invalidation_reason IS NULL",
                &[&request.session_id, &event_id, &request.projection_generation, &request.projection_version],
            ).map_err(|error| format!("read transcript source failed: {error}"))?
                .ok_or_else(|| "transcript content source or generation is unavailable".to_string())?;
            let mut value = serde_json::from_str::<serde_json::Value>(&row.get::<_, String>(0))
                .map_err(|error| format!("decode transcript source failed: {error}"))?;
            super::session_payload::decode(&mut value)?;
            let record = parse_wire_record(&value).map_err(|error| error.to_string())?;
            centaeris_core::session::transcript::transcript_event_content_range(request, &record.event)
        })
    }

    pub fn load_or_initialize_current_transcript_generation(
        &self,
        session_id: &str,
    ) -> Result<TranscriptProjectionCurrentGenerationV1, String> {
        require_nonempty(session_id, "sessionId")?;
        let initial_generation = workspace_transcript_initial_generation();
        if let Some(current) = self.load_current_transcript_projection_generation(session_id)? {
            if workspace_transcript_generation_has_current_encoding(&current.projection_generation)
            {
                return Ok(current);
            }
            // The stored projection was written by a different block encoding.
            // Drop only the derived transcript state; source session events and
            // AgentRun control-plane data are untouched.
            self.invalidate_transcript_projection(session_id)?;
        }
        self.with_client(|client| {
            let mut tx = client.transaction().map_err(|error| {
                format!("begin initial transcript generation publication failed: {error}")
            })?;
            tx.execute(
                "INSERT INTO runtime.transcript_projection_heads(session_id,projection_version,projection_generation,source_high_water,invalidation_reason) VALUES($1,$2,$3,0,NULL) ON CONFLICT DO NOTHING",
                &[&session_id, &TRANSCRIPT_PROJECTION_VERSION_V1, &initial_generation],
            )
            .map_err(|error| format!("create initial transcript projection head failed: {error}"))?;
            tx.execute(
                "INSERT INTO runtime.transcript_projection_current_generations(session_id,projection_version,projection_generation,source_high_water) VALUES($1,$2,$3,0) ON CONFLICT DO NOTHING",
                &[&session_id, &TRANSCRIPT_PROJECTION_VERSION_V1, &initial_generation],
            )
            .map_err(|error| format!("publish initial transcript generation failed: {error}"))?;
            let row = tx
                .query_one(
                    "SELECT projection_generation,source_high_water FROM runtime.transcript_projection_current_generations WHERE session_id=$1 AND projection_version=$2",
                    &[&session_id, &TRANSCRIPT_PROJECTION_VERSION_V1],
                )
                .map_err(|error| format!("load initialized transcript generation failed: {error}"))?;
            let current = TranscriptProjectionCurrentGenerationV1 {
                session_id: session_id.to_string(),
                projection_version: TRANSCRIPT_PROJECTION_VERSION_V1.to_string(),
                projection_generation: row.get(0),
                source_high_water: row.get::<_, i64>(1).to_string(),
            };
            current.validate()?;
            tx.commit().map_err(|error| {
                format!("commit initial transcript generation publication failed: {error}")
            })?;
            Ok(current)
        })
    }

    /// Deletes only the derived transcript projection for a session. It never
    /// touches source session events, jobs, or snapshots.
    fn invalidate_transcript_projection(&self, session_id: &str) -> Result<(), String> {
        self.with_client(|client| {
            let mut tx = client
                .transaction()
                .map_err(|error| format!("begin transcript invalidation failed: {error}"))?;
            for table in [
                "transcript_projection_current_recoveries",
                "transcript_resume_cursors",
                "transcript_block_versions",
                "transcript_block_identities",
                "transcript_projection_commits",
                "transcript_projection_current_generations",
                "transcript_projection_heads",
            ] {
                tx.execute(
                    &format!("DELETE FROM runtime.{table} WHERE session_id=$1"),
                    &[&session_id],
                )
                .map_err(|error| format!("invalidate transcript projection failed: {error}"))?;
            }
            tx.commit()
                .map_err(|error| format!("commit transcript invalidation failed: {error}"))
        })
    }

    pub fn schedule_transcript_generation_rebuild(
        &self,
        session_id: &str,
        source_high_water: u64,
        expected_current_generation: &str,
    ) -> Result<(), String> {
        require_nonempty(session_id, "sessionId")?;
        require_nonempty(expected_current_generation, "expectedCurrentGeneration")?;
        if source_high_water == 0 {
            return Err(
                "transcript generation rebuild requires a positive sourceHighWater".to_string(),
            );
        }
        let active = TRANSCRIPT_REBUILDS.get_or_init(|| Mutex::new(HashSet::new()));
        {
            let mut guard = active
                .lock()
                .map_err(|_| "transcript rebuild registry lock poisoned".to_string())?;
            if !guard.insert(session_id.to_string()) {
                return Ok(());
            }
        }
        let store = self.clone();
        let session_id = session_id.to_string();
        let failed_session_id = session_id.clone();
        let expected_current_generation = expected_current_generation.to_string();
        std::thread::Builder::new()
            .name(format!("transcript-rebuild-{session_id}"))
            .spawn(move || {
                let next_generation = workspace_transcript_generation(&format!(
                    "generation-rebuild-{source_high_water}"
                ));
                let request = TranscriptGenerationRebuildRequestV1 {
                    session_id: session_id.clone(),
                    projection_generation: next_generation,
                    target_source_high_water: source_high_water.to_string(),
                    expected_current_generation: Some(expected_current_generation),
                };
                let source = PostgresTranscriptRebuildSource { store: &store };
                if let Err(error) = rebuild_transcript_generation_v1(&request, &source, &store) {
                    eprintln!(
                        "transcript generation rebuild failed: sessionId={session_id} error={error}"
                    );
                }
                if let Some(active) = TRANSCRIPT_REBUILDS.get() {
                    if let Ok(mut guard) = active.lock() {
                        guard.remove(session_id.as_str());
                    }
                }
            })
            .map(|_| ())
            .map_err(|error| {
                if let Ok(mut guard) = active.lock() {
                    guard.remove(failed_session_id.as_str());
                }
                format!("start transcript generation rebuild failed: {error}")
            })
    }

    pub fn catch_up_transcript_projection(
        &self,
        session_id: &str,
        projection_generation: &str,
        source_high_water: u64,
    ) -> Result<TranscriptCatchUpProgress, String> {
        require_nonempty(session_id, "sessionId")?;
        require_nonempty(projection_generation, "projectionGeneration")?;
        let (head, recovery_json) =
            self.load_transcript_catch_up_snapshot(session_id, projection_generation)?;
        #[cfg(test)]
        CATCH_UP_AFTER_INITIAL_READ.with(|observer| {
            if let Some(observe) = observer.borrow_mut().take() {
                observe();
            }
        });
        if let Some(head) = &head {
            if let Some(reason) = &head.invalidation_reason {
                return Err(format!("transcript projection is invalidated: {reason}"));
            }
        }
        let projected_high_water = head.as_ref().map_or(Ok(0), |head| {
            head.source_high_water
                .parse::<u64>()
                .map_err(|_| "stored transcript sourceHighWater exceeds u64".to_string())
        })?;
        if projected_high_water >= source_high_water {
            return Ok(TranscriptCatchUpProgress {
                source_high_water,
                projected_high_water,
                events_projected: 0,
                source_bytes_projected: 0,
                oversized_source_events: 0,
                caught_up: true,
            });
        }
        let mut projector = if projected_high_water == 0 {
            TranscriptProjectorV1::new(session_id.to_string(), projection_generation.to_string())?
        } else {
            let (checkpoint_json, frontier_json) = recovery_json.ok_or_else(|| {
                "transcript projection recovery checkpoint is missing".to_string()
            })?;
            let recovery = decode_recovery_json(&checkpoint_json, &frontier_json)?;
            if recovery.checkpoint.source_high_water != projected_high_water.to_string() {
                return Err("transcript projection recovery is behind projection head".to_string());
            }
            TranscriptProjectorV1::from_checkpoint_recovery(recovery)?
        };
        let (events_projected, source_bytes_projected, oversized_source_events,
            next_projected_high_water) = self.with_client(|client| {
            let mut tx = client.transaction()
                .map_err(|error| format!("begin transcript catch-up batch failed: {error}"))?;
            let event_limit = i64::try_from(TRANSCRIPT_PROJECTION_SLICE_MAX_EVENTS)
                .map_err(|_| "transcript projection event limit overflow".to_string())?;
            let byte_limit = i64::try_from(TRANSCRIPT_PROJECTION_SLICE_MAX_BYTES)
                .map_err(|_| "transcript projection byte limit overflow".to_string())?;
            let after = u64_to_i64(projected_high_water, "projected sourceHighWater")?;
            let through = u64_to_i64(source_high_water, "sourceHighWater")?;
            let rows = tx.query(
                TRANSCRIPT_SOURCE_SLICE_SQL,
                &[&session_id, &after, &through, &event_limit, &byte_limit],
            ).map_err(|error| format!("query transcript source slice failed: {error}"))?;
            if rows.is_empty() {
                return Err("transcript projection source range is incomplete".to_string());
            }
            let mut events_projected = 0usize;
            let mut source_bytes_projected = 0usize;
            let mut oversized_source_events = 0usize;
            let mut next_projected_high_water = projected_high_water;
            {
                let batch_store = PostgresTranscriptStore::buffered(&mut tx);
                // Decode only the records this bounded transaction consumes. A
                // slow read or commit never discards an already decoded lookahead.
                for row in rows {
                    super::remaining_request_time()?;
                    let wire = serde_json::from_str::<serde_json::Value>(
                        row.get::<_, String>(2).as_str(),
                    ).map_err(|error| format!("decode transcript source event failed: {error}"))?;
                    let mut source = decode_source_record(
                        u64::try_from(row.get::<_, i64>(0))
                            .map_err(|_| "negative transcript source sequence".to_string())?,
                        row.get(1),
                        &wire,
                    )?;
                    source.canonical_bytes =
                        usize::try_from(row.get::<_, i64>(3)).unwrap_or(usize::MAX);
                    let sequence = source.fact.sequence();
                    source_bytes_projected = source_bytes_projected.saturating_add(source.canonical_bytes);
                    if source.canonical_bytes > TRANSCRIPT_PROJECTION_SLICE_MAX_BYTES {
                        oversized_source_events = oversized_source_events.saturating_add(1);
                        eprintln!(
                            "transcript projection progress exception: sessionId={session_id} sourceSequence={sequence} sourceBytes={} sliceBytes={}",
                            source.canonical_bytes, TRANSCRIPT_PROJECTION_SLICE_MAX_BYTES
                        );
                    }
                    let checkpoint_refs = Some(TranscriptCheckpointRefsV1 {
                        frontier_ref: format!(
                            "transcript-frontier:{session_id}:{projection_generation}:{sequence}"
                        ),
                        block_index_ref: format!(
                            "postgres-transcript-index:{session_id}:{projection_generation}:{sequence}"
                        ),
                    });
                    let cursor = sequence.to_string();
                    let commit_id = format!(
                        "transcript-commit:{projection_generation}:{}", source.fact.event_id()
                    );
                    let update = match &source.fact {
                        SourceFact::Envelope(envelope) => projector.apply_envelope_and_commit(
                            envelope,
                            WORKSPACE_TRANSCRIPT_STREAM_ID_V1,
                            cursor.as_str(),
                            commit_id.as_str(),
                            checkpoint_refs,
                            &batch_store,
                        )?,
                        SourceFact::FullRecord(record) => projector.apply_and_commit(
                            record,
                            WORKSPACE_TRANSCRIPT_STREAM_ID_V1,
                            cursor.as_str(),
                            commit_id.as_str(),
                            checkpoint_refs,
                            &batch_store,
                        )?,
                    };
                    events_projected = events_projected.saturating_add(1);
                    next_projected_high_water = sequence;
                    if matches!(update, centaeris_core::session::transcript::TranscriptProjectionUpdateV1::ViewInvalidated { .. }) {
                        // The invalidation itself is authoritative derived state;
                        // publish it before asking the caller to rebuild this view.
                        break;
                    }
                }
                batch_store.flush()?;

            }
            super::remaining_request_time()?;
            tx.commit().map_err(|error| format!("commit transcript catch-up batch failed: {error}"))?;
            Ok((events_projected, source_bytes_projected, oversized_source_events,
                next_projected_high_water))
        })?;
        if let Some(reason) = self
            .load_transcript_projection_head(session_id, projection_generation)?
            .and_then(|head| head.invalidation_reason)
        {
            return Err(format!("transcript projection is invalidated: {reason}"));
        }
        Ok(TranscriptCatchUpProgress {
            source_high_water,
            projected_high_water: next_projected_high_water,
            events_projected,
            source_bytes_projected,
            oversized_source_events,
            caught_up: next_projected_high_water == source_high_water,
        })
    }

    fn load_transcript_catch_up_snapshot(
        &self,
        session_id: &str,
        projection_generation: &str,
    ) -> Result<TranscriptCatchUpSnapshot, String> {
        // READ COMMITTED transactions alone do not pin two SELECT snapshots.
        // The current recovery slot and head must come from one statement.
        self.with_client(|client| {
            let row = client.query_opt(
                "SELECT h.source_high_water,h.invalidation_reason,r.checkpoint_json,r.frontier_json FROM runtime.transcript_projection_heads h LEFT JOIN runtime.transcript_projection_current_recoveries r ON r.session_id=h.session_id AND r.projection_version=h.projection_version AND r.projection_generation=h.projection_generation AND r.source_high_water<=h.source_high_water WHERE h.session_id=$1 AND h.projection_version=$2 AND h.projection_generation=$3",
                &[&session_id, &TRANSCRIPT_PROJECTION_VERSION_V1, &projection_generation],
            ).map_err(|error| format!("load transcript catch-up snapshot failed: {error}"))?;
            let Some(row) = row else { return Ok((None, None)); };
            let head = TranscriptProjectionHeadV1 {
                session_id: session_id.to_string(),
                projection_version: TRANSCRIPT_PROJECTION_VERSION_V1.to_string(),
                projection_generation: projection_generation.to_string(),
                source_high_water: row.get::<_, i64>(0).to_string(),
                invalidation_reason: row.get(1),
            };
            head.validate()?;
            let recovery = row.get::<_, Option<String>>(2).zip(row.get::<_, Option<String>>(3));
            Ok((Some(head), recovery))
        })
    }
}

struct SourceRecord {
    fact: SourceFact,
    canonical_bytes: usize,
}

enum SourceFact {
    Envelope(TranscriptProjectionSourceEnvelopeV1),
    FullRecord(SequencedSessionRecord),
}

impl SourceFact {
    fn sequence(&self) -> u64 {
        match self {
            Self::Envelope(envelope) => envelope.sequence,
            Self::FullRecord(record) => record.sequence,
        }
    }

    fn event_id(&self) -> &str {
        match self {
            Self::Envelope(envelope) => envelope.event_id.as_str(),
            Self::FullRecord(record) => record.event.event_id.as_str(),
        }
    }
}

fn decode_source_record(
    stored_sequence: u64,
    _agent_run_id: String,
    wire: &serde_json::Value,
) -> Result<SourceRecord, String> {
    let mut canonical = wire.clone();
    super::session_payload::decode(&mut canonical)?;
    let wire = &canonical;
    let envelope = parse_transcript_projection_source_envelope(wire)
        .map_err(|error| format!("parse transcript source envelope failed: {error}"))?;
    if envelope.sequence != stored_sequence {
        return Err("transcript source sequence binding mismatch".to_string());
    }
    let fact = match envelope.transcript_payload_requirement()? {
        TranscriptProjectionPayloadRequirementV1::EnvelopeOnly => SourceFact::Envelope(envelope),
        TranscriptProjectionPayloadRequirementV1::FullRecord
        | TranscriptProjectionPayloadRequirementV1::TombstoneRebuild => {
            let record = parse_wire_record(wire)
                .map_err(|error| format!("parse transcript source event failed: {error}"))?;
            SourceFact::FullRecord(record)
        }
    };
    Ok(SourceRecord {
        fact,
        canonical_bytes: 0,
    })
}

struct PostgresTranscriptRebuildSource<'a> {
    store: &'a PostgresRuntimeStore,
}

impl TranscriptGenerationRebuildSourcePortV1 for PostgresTranscriptRebuildSource<'_> {
    fn scan_transcript_rebuild_ledger(
        &self,
        request: &TranscriptGenerationRebuildRequestV1,
        visitor: &mut dyn FnMut(TranscriptRebuildLedgerFactV1) -> Result<(), String>,
    ) -> Result<(), String> {
        self.scan_raw_source(request, &mut |_, wire| {
            visitor(parse_transcript_rebuild_ledger_fact(&wire)?)
        })
    }

    fn scan_transcript_rebuild_projection(
        &self,
        request: &TranscriptGenerationRebuildRequestV1,
        visitor: &mut dyn FnMut(TranscriptRebuildProjectionFactV1) -> Result<(), String>,
    ) -> Result<(), String> {
        self.scan_raw_source(request, &mut |_agent_run_id, wire| {
            let envelope = parse_transcript_projection_source_envelope(&wire)?;
            let cursor = envelope.sequence.to_string();
            let fact = match envelope.transcript_payload_requirement()? {
                TranscriptProjectionPayloadRequirementV1::FullRecord => {
                    TranscriptRebuildProjectionFactV1::FullRecord {
                        record: parse_wire_record(&wire).map_err(|error| error.to_string())?,
                        stream_id: WORKSPACE_TRANSCRIPT_STREAM_ID_V1.to_string(),
                        applied_cursor: cursor,
                    }
                }
                TranscriptProjectionPayloadRequirementV1::EnvelopeOnly
                | TranscriptProjectionPayloadRequirementV1::TombstoneRebuild => {
                    TranscriptRebuildProjectionFactV1::Envelope {
                        envelope,
                        stream_id: WORKSPACE_TRANSCRIPT_STREAM_ID_V1.to_string(),
                        applied_cursor: cursor,
                    }
                }
            };
            visitor(fact)
        })
    }
}

impl PostgresTranscriptRebuildSource<'_> {
    fn scan_raw_source(
        &self,
        request: &TranscriptGenerationRebuildRequestV1,
        visitor: &mut dyn FnMut(String, serde_json::Value) -> Result<(), String>,
    ) -> Result<(), String> {
        request.validate()?;
        let target = decimal_to_i64(
            request.target_source_high_water.as_str(),
            "rebuild targetSourceHighWater",
        )?;
        let mut after = 0_i64;
        while after < target {
            let rows = self.store.with_client(|client| {
                client
                    .query(
                        "SELECT sequence::bigint,agent_run_id,payload::text FROM public.app_core_sessionevent WHERE session_id=$1 AND sequence>$2::bigint AND sequence<=$3::bigint ORDER BY sequence LIMIT $4",
                        &[&request.session_id, &after, &target, &(TRANSCRIPT_PROJECTION_SLICE_MAX_EVENTS as i64)],
                    )
                    .map_err(|error| format!("scan transcript rebuild source failed: {error}"))?
                    .into_iter()
                    .map(|row| {
                        let sequence = row.get::<_, i64>(0);
                        let mut wire = serde_json::from_str::<serde_json::Value>(
                            row.get::<_, String>(2).as_str(),
                        )
                        .map_err(|error| format!("decode transcript rebuild source failed: {error}"))?;
                        super::session_payload::decode(&mut wire)?;
                        Ok((sequence, row.get::<_, String>(1), wire))
                    })
                    .collect::<Result<Vec<_>, String>>()
            })?;
            if rows.is_empty() {
                break;
            }
            for (sequence, agent_run_id, wire) in rows {
                visitor(agent_run_id, wire)?;
                after = sequence;
            }
        }
        Ok(())
    }
}

// Core owns projection semantics; this adapter borrows the current PostgreSQL
// client or transaction so a bounded catch-up batch shares one durable commit.
struct PostgresTranscriptStore<'client, C> {
    client: RefCell<&'client mut C>,
    pending: RefCell<Option<Vec<TranscriptProjectionCommitV1>>>,
}

impl<'client, C: GenericClient> PostgresTranscriptStore<'client, C> {
    fn new(client: &'client mut C) -> Self {
        Self {
            client: RefCell::new(client),
            pending: RefCell::new(None),
        }
    }

    fn buffered(client: &'client mut C) -> Self {
        Self {
            client: RefCell::new(client),
            pending: RefCell::new(Some(Vec::new())),
        }
    }

    fn flush(self) -> Result<(), String> {
        let commits = self
            .pending
            .into_inner()
            .expect("buffered transcript batch");
        save_transcript_commits(self.client.into_inner(), &commits).map(|_| ())
    }

    fn with_client<T>(
        &self,
        operation: impl FnOnce(&mut C) -> Result<T, String>,
    ) -> Result<T, String> {
        operation(&mut **self.client.borrow_mut())
    }
}

impl TranscriptProjectionStorePort for PostgresRuntimeStore {
    fn commit_transcript_projection(
        &self,
        commit: TranscriptProjectionCommitV1,
    ) -> Result<TranscriptProjectionCommitDispositionV1, String> {
        self.with_client(|client| {
            let mut tx = client
                .transaction()
                .map_err(|error| format!("begin transcript projection commit failed: {error}"))?;
            let disposition =
                PostgresTranscriptStore::new(&mut tx).commit_transcript_projection(commit)?;
            tx.commit()
                .map_err(|error| format!("commit transcript projection failed: {error}"))?;
            Ok(disposition)
        })
    }

    fn load_transcript_projection_head(
        &self,
        session_id: &str,
        projection_generation: &str,
    ) -> Result<Option<TranscriptProjectionHeadV1>, String> {
        self.with_client(|client| {
            PostgresTranscriptStore::new(client)
                .load_transcript_projection_head(session_id, projection_generation)
        })
    }

    fn load_transcript_page(
        &self,
        request: TranscriptPageReadRequestV1,
    ) -> Result<TranscriptPageReadResultV1, String> {
        self.with_client(|client| {
            PostgresTranscriptStore::new(client).load_transcript_page(request)
        })
    }

    fn load_latest_transcript_checkpoint(
        &self,
        session_id: &str,
        projection_generation: &str,
        at_or_before_source_high_water: u64,
    ) -> Result<Option<TranscriptProjectionCheckpointV1>, String> {
        self.with_client(|client| {
            PostgresTranscriptStore::new(client).load_latest_transcript_checkpoint(
                session_id,
                projection_generation,
                at_or_before_source_high_water,
            )
        })
    }

    fn load_latest_transcript_recovery(
        &self,
        session_id: &str,
        projection_generation: &str,
        at_or_before_source_high_water: u64,
    ) -> Result<Option<TranscriptProjectionRecoveryV1>, String> {
        self.with_client(|client| {
            PostgresTranscriptStore::new(client).load_latest_transcript_recovery(
                session_id,
                projection_generation,
                at_or_before_source_high_water,
            )
        })
    }

    fn load_transcript_patches(
        &self,
        request: TranscriptPatchReadRequestV1,
    ) -> Result<TranscriptPatchReadResultV1, String> {
        self.with_client(|client| {
            PostgresTranscriptStore::new(client).load_transcript_patches(request)
        })
    }

    fn load_transcript_resume_cursors(
        &self,
        session_id: &str,
        projection_generation: &str,
        at_or_before_source_high_water: u64,
    ) -> Result<Vec<TranscriptResumeCursorV1>, String> {
        self.with_client(|client| {
            PostgresTranscriptStore::new(client).load_transcript_resume_cursors(
                session_id,
                projection_generation,
                at_or_before_source_high_water,
            )
        })
    }
}

impl<C: GenericClient> TranscriptProjectionStorePort for PostgresTranscriptStore<'_, C> {
    fn commit_transcript_projection(
        &self,
        commit: TranscriptProjectionCommitV1,
    ) -> Result<TranscriptProjectionCommitDispositionV1, String> {
        commit.validate()?;
        {
            let mut pending = self.pending.borrow_mut();
            if let Some(commits) = pending.as_mut() {
                // The transaction publishes one final recovery slot. Earlier
                // validated checkpoints are never observable outside this batch.
                if commit.checkpoint.is_some() || commit.invalidation_reason.is_some() {
                    if let Some(previous) = commits.last_mut() {
                        previous.checkpoint = None;
                        previous.frontier = None;
                    }
                }
                commits.push(commit);
                return Ok(TranscriptProjectionCommitDispositionV1::Applied);
            }
        }
        self.with_client(|tx| save_transcript_commits(tx, &[commit]))
    }

    fn load_transcript_projection_head(
        &self,
        session_id: &str,
        projection_generation: &str,
    ) -> Result<Option<TranscriptProjectionHeadV1>, String> {
        require_nonempty(session_id, "sessionId")?;
        require_nonempty(projection_generation, "projectionGeneration")?;
        self.with_client(|client| {
            let head = client
                .query_opt(
                    "SELECT source_high_water,invalidation_reason FROM runtime.transcript_projection_heads WHERE session_id=$1 AND projection_version=$2 AND projection_generation=$3",
                    &[&session_id, &TRANSCRIPT_PROJECTION_VERSION_V1, &projection_generation],
                )
                .map_err(|error| format!("load transcript projection head failed: {error}"))?
                .map(|row| TranscriptProjectionHeadV1 {
                    session_id: session_id.to_string(),
                    projection_version: TRANSCRIPT_PROJECTION_VERSION_V1.to_string(),
                    projection_generation: projection_generation.to_string(),
                    source_high_water: row.get::<_, i64>(0).to_string(),
                    invalidation_reason: row.get(1),
                });
            if let Some(head) = &head {
                head.validate()?;
            }
            Ok(head)
        })
    }

    fn load_transcript_page(
        &self,
        request: TranscriptPageReadRequestV1,
    ) -> Result<TranscriptPageReadResultV1, String> {
        request.validate()?;
        let high_water = decimal_to_i64(request.source_high_water.as_str(), "sourceHighWater")?;
        let before = transcript_page_before_order(&request)?;
        self.with_client(|client| {
            require_projected_head(
                client,
                &request.session_id,
                &request.projection_generation,
                high_water,
            )?;
            let resume_cursors = load_resume_cursors(
                client,
                request.session_id.as_str(),
                request.projection_generation.as_str(),
                high_water,
            )?;
            let resume_rows = resume_cursors.len();
            let limit = i64::try_from(request.policy.max_blocks.saturating_add(1))
                .map_err(|_| "transcript page limit overflow".to_string())?;
            let rows = match before {
                Some((sequence, ordinal)) => client.query(
                    TRANSCRIPT_PAGE_BEFORE_SQL,
                    &[
                        &request.session_id,
                        &request.projection_version,
                        &request.projection_generation,
                        &high_water,
                        &u64_to_i64(sequence, "olderCursor sequence")?,
                        &i64::from(ordinal),
                        &limit,
                    ],
                ),
                None => client.query(
                    TRANSCRIPT_TAIL_PAGE_SQL,
                    &[
                        &request.session_id,
                        &request.projection_version,
                        &request.projection_generation,
                        &high_water,
                        &limit,
                    ],
                ),
            }
            .map_err(|error| format!("query transcript page failed: {error}"))?;
            let blocks = rows.iter().map(decode_block_row).collect::<Vec<_>>();
            let (page, candidates) =
                assemble_transcript_page_from_newest(&request, blocks, resume_cursors)?;
            Ok(TranscriptPageReadResultV1 {
                page,
                work: TranscriptPersistentPageQueryWorkV1 {
                    candidate_rows_read: candidates,
                    resume_cursor_rows_read: resume_rows,
                    raw_event_visits: 0,
                },
            })
        })
    }

    fn load_latest_transcript_checkpoint(
        &self,
        session_id: &str,
        projection_generation: &str,
        at_or_before_source_high_water: u64,
    ) -> Result<Option<TranscriptProjectionCheckpointV1>, String> {
        Ok(self
            .load_latest_transcript_recovery(
                session_id,
                projection_generation,
                at_or_before_source_high_water,
            )?
            .map(|recovery| recovery.checkpoint))
    }

    fn load_latest_transcript_recovery(
        &self,
        session_id: &str,
        projection_generation: &str,
        at_or_before_source_high_water: u64,
    ) -> Result<Option<TranscriptProjectionRecoveryV1>, String> {
        require_nonempty(session_id, "sessionId")?;
        require_nonempty(projection_generation, "projectionGeneration")?;
        let high_water = u64_to_i64(at_or_before_source_high_water, "checkpoint sourceHighWater")?;
        self.with_client(|client| {
            client
                .query_opt(
                    "SELECT checkpoint_json,frontier_json FROM runtime.transcript_projection_current_recoveries WHERE session_id=$1 AND projection_version=$2 AND projection_generation=$3 AND source_high_water<=$4",
                    &[&session_id, &TRANSCRIPT_PROJECTION_VERSION_V1, &projection_generation, &high_water],
                )
                .map_err(|error| format!("load transcript projection recovery failed: {error}"))?
                .map(|row| decode_recovery(&row))
                .transpose()
        })
    }

    fn load_transcript_patches(
        &self,
        request: TranscriptPatchReadRequestV1,
    ) -> Result<TranscriptPatchReadResultV1, String> {
        request.validate()?;
        let after = decimal_to_i64(
            request.after_source_high_water.as_str(),
            "afterSourceHighWater",
        )?;
        let through = decimal_to_i64(
            request.through_source_high_water.as_str(),
            "throughSourceHighWater",
        )?;
        self.with_client(|client| {
            require_projected_head(client, &request.session_id, &request.projection_generation, through)?;
            if after == through {
                let result = TranscriptPatchReadResultV1 {
                    patches: Vec::new(),
                    next_source_high_water: request.through_source_high_water.clone(),
                    has_more: false,
                    work: TranscriptPersistentPatchQueryWorkV1::default(),
                };
                result.validate(&request)?;
                return Ok(result);
            }
            let limit = i64::try_from(TRANSCRIPT_PATCH_READ_MAX_PATCHES + 1)
                .map_err(|_| "transcript patch read limit overflow".to_string())?;
            let rows = client
                .query(
                    "SELECT commit_json FROM runtime.transcript_projection_commits WHERE session_id=$1 AND projection_version=$2 AND projection_generation=$3 AND source_high_water>$4 AND source_high_water<=$5 ORDER BY source_high_water LIMIT $6",
                    &[&request.session_id, &request.projection_version, &request.projection_generation, &after, &through, &limit],
                )
                .map_err(|error| format!("query transcript projection patches failed: {error}"))?;
            let mut patches = Vec::new();
            let mut rows_read = 0usize;
            let mut has_more = false;
            let row_count = rows.len();
            for (index, row) in rows.into_iter().enumerate() {
                rows_read = rows_read.saturating_add(1);
                let patch = commit_row_to_patch(&row)?;
                let candidate_next = patch.source_high_water.clone();
                let candidate_has_more = index + 1 < row_count
                    || candidate_next != request.through_source_high_water;
                let mut candidate_patches = patches.clone();
                candidate_patches.push(patch.clone());
                let candidate = TranscriptPatchReadResultV1 {
                    patches: candidate_patches,
                    next_source_high_water: candidate_next,
                    has_more: candidate_has_more,
                    work: TranscriptPersistentPatchQueryWorkV1 {
                        commit_rows_read: rows_read,
                        raw_event_visits: 0,
                    },
                };
                if patches.len() >= TRANSCRIPT_PATCH_READ_MAX_PATCHES
                    || transcript_patch_read_result_serialized_bytes(&candidate)?
                        > TRANSCRIPT_PATCH_READ_SERIALIZED_MAX_BYTES
                {
                    has_more = true;
                    break;
                }
                patches.push(patch);
                has_more = candidate_has_more;
            }
            let next = patches.last().map_or_else(
                || request.after_source_high_water.clone(),
                |patch| patch.source_high_water.clone(),
            );
            if !has_more && next != request.through_source_high_water {
                return Err("transcript patch history is incomplete within frozen waterline".to_string());
            }
            let result = TranscriptPatchReadResultV1 {
                patches,
                next_source_high_water: next,
                has_more,
                work: TranscriptPersistentPatchQueryWorkV1 {
                    commit_rows_read: rows_read,
                    raw_event_visits: 0,
                },
            };
            result.validate(&request)?;
            Ok(result)
        })
    }

    fn load_transcript_resume_cursors(
        &self,
        session_id: &str,
        projection_generation: &str,
        at_or_before_source_high_water: u64,
    ) -> Result<Vec<TranscriptResumeCursorV1>, String> {
        require_nonempty(session_id, "sessionId")?;
        require_nonempty(projection_generation, "projectionGeneration")?;
        let high_water = u64_to_i64(at_or_before_source_high_water, "resume sourceHighWater")?;
        self.with_client(|client| {
            let cursors =
                load_resume_cursors(client, session_id, projection_generation, high_water)?;
            validate_transcript_resume_cursor_read(&cursors)?;
            Ok(cursors)
        })
    }
}

impl TranscriptProjectionGenerationStorePortV1 for PostgresRuntimeStore {
    fn load_current_transcript_projection_generation(
        &self,
        session_id: &str,
    ) -> Result<Option<TranscriptProjectionCurrentGenerationV1>, String> {
        require_nonempty(session_id, "sessionId")?;
        self.with_client(|client| {
            let current = client
                .query_opt(
                    "SELECT projection_generation,source_high_water FROM runtime.transcript_projection_current_generations WHERE session_id=$1 AND projection_version=$2",
                    &[&session_id, &TRANSCRIPT_PROJECTION_VERSION_V1],
                )
                .map_err(|error| {
                    format!("load current transcript projection generation failed: {error}")
                })?
                .map(|row| TranscriptProjectionCurrentGenerationV1 {
                    session_id: session_id.to_string(),
                    projection_version: TRANSCRIPT_PROJECTION_VERSION_V1.to_string(),
                    projection_generation: row.get(0),
                    source_high_water: row.get::<_, i64>(1).to_string(),
                });
            if let Some(current) = &current {
                current.validate()?;
            }
            Ok(current)
        })
    }

    fn rotate_current_transcript_projection_generation(
        &self,
        rotation: TranscriptProjectionGenerationRotationV1,
    ) -> Result<TranscriptProjectionGenerationRotationDispositionV1, String> {
        rotation.validate()?;
        let target = decimal_to_i64(
            rotation.target_source_high_water.as_str(),
            "generation targetSourceHighWater",
        )?;
        self.with_client(|client| {
            let mut tx = client
                .transaction()
                .map_err(|error| format!("begin transcript generation rotation failed: {error}"))?;
            let next_head = tx
                .query_opt(
                    "SELECT source_high_water,invalidation_reason FROM runtime.transcript_projection_heads WHERE session_id=$1 AND projection_version=$2 AND projection_generation=$3",
                    &[&rotation.session_id, &rotation.projection_version, &rotation.next_generation],
                )
                .map_err(|error| format!("load next transcript generation head failed: {error}"))?
                .ok_or_else(|| "next transcript projection generation head is missing".to_string())?;
            if next_head.get::<_, i64>(0) != target
                || next_head.get::<_, Option<String>>(1).is_some()
            {
                return Err(
                    "next transcript projection generation is not complete and readable"
                        .to_string(),
                );
            }
            let current = tx
                .query_opt(
                    "SELECT projection_generation,source_high_water FROM runtime.transcript_projection_current_generations WHERE session_id=$1 AND projection_version=$2 FOR UPDATE",
                    &[&rotation.session_id, &rotation.projection_version],
                )
                .map_err(|error| format!("load current transcript generation failed: {error}"))?
                .map(|row| (row.get::<_, String>(0), row.get::<_, i64>(1)));
            if current.as_ref() == Some(&(rotation.next_generation.clone(), target)) {
                return Ok(TranscriptProjectionGenerationRotationDispositionV1::AlreadyApplied);
            }
            match (&rotation.expected_current_generation, current) {
                (None, None) => {
                    tx.execute(
                        "INSERT INTO runtime.transcript_projection_current_generations(session_id,projection_version,projection_generation,source_high_water) VALUES($1,$2,$3,$4)",
                        &[&rotation.session_id, &rotation.projection_version, &rotation.next_generation, &target],
                    )
                    .map_err(|error| format!("publish initial transcript generation failed: {error}"))?;
                }
                (Some(expected), Some((current_generation, _)))
                    if expected == &current_generation =>
                {
                    let changed = tx
                        .execute(
                            "UPDATE runtime.transcript_projection_current_generations SET projection_generation=$1,source_high_water=$2 WHERE session_id=$3 AND projection_version=$4 AND projection_generation=$5",
                            &[&rotation.next_generation, &target, &rotation.session_id, &rotation.projection_version, &expected],
                        )
                        .map_err(|error| format!("rotate transcript generation failed: {error}"))?;
                    if changed != 1 {
                        return Err(
                            "transcript projection generation rotation conflict".to_string(),
                        );
                    }
                }
                _ => {
                    return Err("transcript projection generation rotation conflict".to_string())
                }
            }
            tx.commit()
                .map_err(|error| format!("commit transcript generation rotation failed: {error}"))?;
            Ok(TranscriptProjectionGenerationRotationDispositionV1::Applied)
        })
    }
}

// Both ordinary commits and bounded catch-up batches use the same persistence
// checks. The Core projector still produces every event's ordered patch/cursor;
// only their database round trips are combined inside the existing transaction.
fn save_transcript_commits<C: GenericClient>(
    tx: &mut C,
    commits: &[TranscriptProjectionCommitV1],
) -> Result<TranscriptProjectionCommitDispositionV1, String> {
    let Some(first) = commits.first() else {
        return Ok(TranscriptProjectionCommitDispositionV1::AlreadyApplied);
    };
    let session = &first.session_id;
    let version = &first.projection_version;
    let generation = &first.projection_generation;
    if first.expected_source_high_water == "0" {
        tx.execute(
            "INSERT INTO runtime.transcript_projection_heads(session_id,projection_version,projection_generation,source_high_water,invalidation_reason) VALUES($1,$2,$3,0,NULL) ON CONFLICT DO NOTHING",
            &[session, version, generation],
        ).map_err(|error| format!("create transcript projection head failed: {error}"))?;
    }
    // Serialize writers before deduplication. A competing batch may have become
    // durable while this projector was working from an older coherent snapshot.
    let current = tx.query_opt(
        "SELECT source_high_water,invalidation_reason FROM runtime.transcript_projection_heads WHERE session_id=$1 AND projection_version=$2 AND projection_generation=$3 FOR UPDATE",
        &[session, version, generation],
    ).map_err(|error| format!("load transcript projection head failed: {error}"))?
        .ok_or_else(|| "transcript projection sourceHighWater conflict: projection head is missing".to_string())?;
    let initial_high_water: i64 = current.get(0);
    let mut high_water = initial_high_water;
    let mut invalidation: Option<String> = current.get(1);
    let commit_ids = commits
        .iter()
        .map(|commit| commit.commit_id.as_str())
        .collect::<Vec<_>>();
    let existing = tx.query(
        "SELECT commit_id,commit_json FROM runtime.transcript_projection_commits WHERE commit_id=ANY($1::text[])",
        &[&commit_ids],
    ).map_err(|error| format!("load transcript projection commits failed: {error}"))?
        .into_iter().map(|row| (row.get::<_, String>(0), row.get::<_, String>(1)))
        .collect::<HashMap<_, _>>();
    let mut applied = Vec::new();
    let mut expected_waters = Vec::new();
    let mut source_waters = Vec::new();
    let mut patch_json = Vec::new();
    for commit in commits {
        // Historical patches omit the mutable recovery slot. Preserve the
        // existing semantic comparison for commitId replay, including scope.
        let mut patch = commit.clone();
        patch.checkpoint = None;
        patch.frontier = None;
        if let Some(json) = existing.get(&commit.commit_id) {
            let stored =
                serde_json::from_str::<TranscriptProjectionCommitV1>(json).map_err(|error| {
                    format!("decode stored transcript projection commit failed: {error}")
                })?;
            if stored != patch {
                return Err(format!(
                    "transcript projection commitId conflict: {}",
                    commit.commit_id
                ));
            }
            continue;
        }
        if let Some(reason) = &invalidation {
            return Err(format!("transcript projection is invalidated: {reason}"));
        }
        let expected = decimal_to_i64(
            &commit.expected_source_high_water,
            "expectedSourceHighWater",
        )?;
        if high_water != expected {
            return Err(format!("transcript projection sourceHighWater conflict: expected {expected}, actual {high_water}"));
        }
        high_water = decimal_to_i64(&commit.source_high_water, "sourceHighWater")?;
        invalidation = commit.invalidation_reason.clone();
        expected_waters.push(expected);
        source_waters.push(high_water);
        patch_json.push(
            serde_json::to_string(&patch).map_err(|error| {
                format!("serialize transcript projection commit failed: {error}")
            })?,
        );
        applied.push(commit);
    }
    if applied.is_empty() {
        return Ok(TranscriptProjectionCommitDispositionV1::AlreadyApplied);
    }

    let block_ids = applied
        .iter()
        .flat_map(|commit| &commit.upserts)
        .map(|item| item.block.block_id.as_str())
        .collect::<Vec<_>>();
    // Load identity and latest revision once for the blocks touched by this
    // batch, then validate subsequent in-batch revisions against that state.
    let mut blocks = if block_ids.is_empty() {
        HashMap::new()
    } else {
        tx.query(
            "SELECT identity.block_id,identity.order_source_sequence,identity.order_ordinal,previous.applied_source_sequence,previous.block_revision FROM runtime.transcript_block_identities identity LEFT JOIN LATERAL (SELECT applied_source_sequence,block_revision FROM runtime.transcript_block_versions WHERE session_id=identity.session_id AND projection_version=identity.projection_version AND projection_generation=identity.projection_generation AND block_id=identity.block_id ORDER BY applied_source_sequence DESC LIMIT 1) previous ON TRUE WHERE identity.session_id=$1 AND identity.projection_version=$2 AND identity.projection_generation=$3 AND identity.block_id=ANY($4::text[])",
            &[session, version, generation, &block_ids],
        ).map_err(|error| format!("load transcript block versions failed: {error}"))?
            .into_iter().map(|row| {
                (row.get::<_, String>(0), (
                    row.get::<_, i64>(1), row.get::<_, i64>(2),
                    row.get::<_, Option<i64>>(3).zip(row.get::<_, Option<i64>>(4)),
                ))
            }).collect::<HashMap<_, _>>()
    };
    let mut identity_ids = Vec::new();
    let mut identity_orders = Vec::new();
    let mut identity_ordinals = Vec::new();
    let mut version_ids = Vec::new();
    let mut version_sequences = Vec::new();
    let mut version_revisions = Vec::new();
    let mut version_json = Vec::new();
    let mut cursor_streams = Vec::new();
    let mut cursor_waters = Vec::new();
    let mut cursor_values = Vec::new();
    for commit in &applied {
        for item in &commit.upserts {
            let id = &item.block.block_id;
            let sequence = decimal_to_i64(&item.applied_source_sequence, "appliedSourceSequence")?;
            let order = decimal_to_i64(
                &item.block.order_key.source_sequence,
                "order sourceSequence",
            )?;
            let ordinal = i64::from(item.block.order_key.ordinal);
            let revision = decimal_to_i64(&item.block.block_revision, "blockRevision")?;
            if let Some((stored_order, stored_ordinal, previous)) = blocks.get(id) {
                if (*stored_order, *stored_ordinal) != (order, ordinal) {
                    return Err("transcript block orderKey changed across revisions".to_string());
                }
                if previous.is_some_and(|(previous_sequence, previous_revision)| {
                    sequence <= previous_sequence || revision <= previous_revision
                }) {
                    return Err("transcript block version is not increasing".to_string());
                }
            } else {
                if sequence != order {
                    return Err("transcript block first revision must be applied at its order sourceSequence".to_string());
                }
                identity_ids.push(id.as_str());
                identity_orders.push(order);
                identity_ordinals.push(ordinal);
            }
            blocks.insert(id.clone(), (order, ordinal, Some((sequence, revision))));
            version_ids.push(id.as_str());
            version_sequences.push(sequence);
            version_revisions.push(revision);
            version_json.push(
                serde_json::to_string(&item.block)
                    .map_err(|error| format!("serialize transcript block failed: {error}"))?,
            );
        }
        let water = decimal_to_i64(&commit.source_high_water, "sourceHighWater")?;
        for cursor in &commit.resume_cursors {
            cursor_streams.push(cursor.stream_id.as_str());
            cursor_waters.push(water);
            cursor_values.push(cursor.cursor.as_str());
        }
    }
    if !identity_ids.is_empty() {
        tx.execute(
            "INSERT INTO runtime.transcript_block_identities(session_id,projection_version,projection_generation,block_id,order_source_sequence,order_ordinal) SELECT $1,$2,$3,b.* FROM UNNEST($4::text[],$5::bigint[],$6::bigint[]) AS b",
            &[session, version, generation, &identity_ids, &identity_orders, &identity_ordinals],
        ).map_err(|error| format!("save transcript block identities failed: {error}"))?;
    }
    if !version_ids.is_empty() {
        tx.execute(
            "INSERT INTO runtime.transcript_block_versions(session_id,projection_version,projection_generation,block_id,applied_source_sequence,block_revision,block_json) SELECT $1,$2,$3,b.* FROM UNNEST($4::text[],$5::bigint[],$6::bigint[],$7::text[]) AS b",
            &[session, version, generation, &version_ids, &version_sequences, &version_revisions, &version_json],
        ).map_err(|error| format!("save transcript block versions failed: {error}"))?;
    }
    if !cursor_streams.is_empty() {
        tx.execute(
            "INSERT INTO runtime.transcript_resume_cursors(session_id,projection_version,projection_generation,stream_id,source_high_water,cursor) SELECT $1,$2,$3,c.* FROM UNNEST($4::text[],$5::bigint[],$6::text[]) AS c",
            &[session, version, generation, &cursor_streams, &cursor_waters, &cursor_values],
        ).map_err(|error| format!("save transcript resume cursors failed: {error}"))?;
    }
    if let Some(recovery_commit) = applied
        .iter()
        .rev()
        .find(|commit| commit.checkpoint.is_some() || commit.invalidation_reason.is_some())
    {
        if let Some(checkpoint) = &recovery_commit.checkpoint {
            let frontier = recovery_commit
                .frontier
                .as_ref()
                .expect("validated checkpoint frontier pair");
            let frontier_json = serde_json::to_string(frontier).map_err(|error| {
                format!("serialize transcript projection frontier failed: {error}")
            })?;
            let checkpoint_json = serde_json::to_string(checkpoint).map_err(|error| {
                format!("serialize transcript projection checkpoint failed: {error}")
            })?;
            let recovery_water =
                decimal_to_i64(&recovery_commit.source_high_water, "sourceHighWater")?;
            let changed = tx.execute(
                "INSERT INTO runtime.transcript_projection_current_recoveries(session_id,projection_version,projection_generation,source_high_water,checkpoint_json,frontier_json) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT(session_id,projection_version,projection_generation) DO UPDATE SET source_high_water=EXCLUDED.source_high_water,checkpoint_json=EXCLUDED.checkpoint_json,frontier_json=EXCLUDED.frontier_json WHERE runtime.transcript_projection_current_recoveries.source_high_water<$4",
                &[session, version, generation, &recovery_water, &checkpoint_json, &frontier_json],
            ).map_err(|error| format!("save current transcript projection recovery failed: {error}"))?;
            if changed != 1 {
                return Err("current transcript projection recovery did not advance".to_string());
            }
        } else {
            tx.execute(
                "DELETE FROM runtime.transcript_projection_current_recoveries WHERE session_id=$1 AND projection_version=$2 AND projection_generation=$3",
                &[session, version, generation],
            ).map_err(|error| format!("delete invalid transcript projection recovery failed: {error}"))?;
        }
    }
    let changed = tx.execute(
        "UPDATE runtime.transcript_projection_heads SET source_high_water=$1,invalidation_reason=$2 WHERE session_id=$3 AND projection_version=$4 AND projection_generation=$5 AND source_high_water=$6 AND invalidation_reason IS NULL",
        &[&high_water, &invalidation, session, version, generation, &initial_high_water],
    ).map_err(|error| format!("advance transcript projection head failed: {error}"))?;
    if changed != 1 {
        return Err("transcript projection sourceHighWater conflict while committing".to_string());
    }
    let applied_ids = applied
        .iter()
        .map(|commit| commit.commit_id.as_str())
        .collect::<Vec<_>>();
    tx.execute(
        "INSERT INTO runtime.transcript_projection_commits(commit_id,session_id,projection_version,projection_generation,expected_source_high_water,source_high_water,commit_json) SELECT c.commit_id,$1,$2,$3,c.expected_water,c.source_water,c.commit_json FROM UNNEST($4::text[],$5::bigint[],$6::bigint[],$7::text[]) AS c(commit_id,expected_water,source_water,commit_json)",
        &[session, version, generation, &applied_ids, &expected_waters, &source_waters, &patch_json],
    ).map_err(|error| format!("record transcript projection commits failed: {error}"))?;
    Ok(TranscriptProjectionCommitDispositionV1::Applied)
}

fn require_projected_head<C: GenericClient>(
    client: &mut C,
    session_id: &str,
    generation: &str,
    requested_high_water: i64,
) -> Result<(), String> {
    let row = client
        .query_opt(
            "SELECT source_high_water,invalidation_reason FROM runtime.transcript_projection_heads WHERE session_id=$1 AND projection_version=$2 AND projection_generation=$3",
            &[&session_id, &TRANSCRIPT_PROJECTION_VERSION_V1, &generation],
        )
        .map_err(|error| format!("load transcript projection head failed: {error}"))?
        .ok_or_else(|| "transcript projection head is missing".to_string())?;
    if let Some(reason) = row.get::<_, Option<String>>(1) {
        return Err(format!("transcript projection is invalidated: {reason}"));
    }
    if row.get::<_, i64>(0) < requested_high_water {
        return Err("transcript page sourceHighWater is not fully projected".to_string());
    }
    Ok(())
}

fn load_resume_cursors<C: GenericClient>(
    client: &mut C,
    session_id: &str,
    generation: &str,
    high_water: i64,
) -> Result<Vec<TranscriptResumeCursorV1>, String> {
    let cursors = client
        .query(
            "SELECT DISTINCT ON (stream_id) stream_id,cursor FROM runtime.transcript_resume_cursors WHERE session_id=$1 AND projection_version=$2 AND projection_generation=$3 AND source_high_water<=$4 ORDER BY stream_id,source_high_water DESC",
            &[&session_id, &TRANSCRIPT_PROJECTION_VERSION_V1, &generation, &high_water],
        )
        .map_err(|error| format!("query transcript resume cursors failed: {error}"))?
        .into_iter()
        .map(|row| TranscriptResumeCursorV1 {
            stream_id: row.get(0),
            cursor: row.get(1),
        })
        .collect::<Vec<_>>();
    if cursors.len() > TRANSCRIPT_PAGE_RESUME_CURSOR_MAX_COUNT {
        return Err("transcript projection exceeds maximum resume stream count".to_string());
    }
    for cursor in &cursors {
        cursor.validate()?;
    }
    Ok(cursors)
}

fn decode_block_row(row: &Row) -> Result<TranscriptBlockV1, String> {
    let block = serde_json::from_str::<TranscriptBlockV1>(row.get::<_, String>(0).as_str())
        .map_err(|error| format!("decode stored transcript block failed: {error}"))?;
    block.validate()?;
    Ok(block)
}

fn decode_recovery(row: &Row) -> Result<TranscriptProjectionRecoveryV1, String> {
    decode_recovery_json(&row.get::<_, String>(0), &row.get::<_, String>(1))
}

fn decode_recovery_json(
    checkpoint: &str,
    frontier: &str,
) -> Result<TranscriptProjectionRecoveryV1, String> {
    let recovery = TranscriptProjectionRecoveryV1 {
        checkpoint: serde_json::from_str::<TranscriptProjectionCheckpointV1>(checkpoint)
            .map_err(|error| format!("decode transcript projection checkpoint failed: {error}"))?,
        frontier: serde_json::from_str::<TranscriptProjectionFrontierV1>(frontier)
            .map_err(|error| format!("decode transcript projection frontier failed: {error}"))?,
    };
    recovery.validate()?;
    Ok(recovery)
}

fn commit_row_to_patch(row: &Row) -> Result<TranscriptPatchV1, String> {
    let commit =
        serde_json::from_str::<TranscriptProjectionCommitV1>(row.get::<_, String>(0).as_str())
            .map_err(|error| {
                format!("decode stored transcript projection commit failed: {error}")
            })?;
    commit.validate()?;
    if let Some(reason) = commit.invalidation_reason {
        return Err(format!("transcript projection is invalidated: {reason}"));
    }
    if commit.resume_cursors.len() != 1 {
        return Err("transcript projection commit must contain one patch cursor".to_string());
    }
    let cursor = &commit.resume_cursors[0];
    let patch = TranscriptPatchV1 {
        schema: TRANSCRIPT_PATCH_SCHEMA_V1.to_string(),
        session_id: commit.session_id,
        projection_version: commit.projection_version,
        projection_generation: commit.projection_generation,
        source_high_water: commit.source_high_water,
        stream_id: cursor.stream_id.clone(),
        applied_cursor: cursor.cursor.clone(),
        upserts: commit.upserts.into_iter().map(|item| item.block).collect(),
        removals: Vec::new(),
    };
    patch.validate()?;
    Ok(patch)
}

fn decimal_to_i64(value: &str, name: &str) -> Result<i64, String> {
    let value = value
        .parse::<u64>()
        .map_err(|_| format!("transcript {name} is not a valid u64"))?;
    u64_to_i64(value, name)
}

fn u64_to_i64(value: u64, name: &str) -> Result<i64, String> {
    i64::try_from(value).map_err(|_| format!("transcript {name} exceeds PostgreSQL bigint range"))
}

fn require_nonempty(value: &str, name: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err(format!("transcript {name} must not be empty"));
    }
    Ok(())
}

// Bound raw rows in native index order before casts, JSONB expansion or
// window functions. Materialize encoded text once for both output and byte size.
const TRANSCRIPT_SOURCE_SLICE_SQL: &str = r#"
WITH candidates AS MATERIALIZED (
  SELECT sequence,agent_run_id,payload
  FROM public.app_core_sessionevent
  WHERE session_id=$1 AND sequence>$2::bigint AND sequence<=$3::bigint
  ORDER BY sequence
  LIMIT $4
), encoded AS MATERIALIZED (
  SELECT sequence::bigint AS sequence,agent_run_id,payload::text AS payload
  FROM candidates
), bounded AS (
  SELECT *,octet_length(payload)::bigint AS payload_bytes,
         row_number() OVER (ORDER BY sequence) AS row_number,
         sum(octet_length(payload)::bigint) OVER (ORDER BY sequence) AS cumulative_bytes
  FROM encoded
)
SELECT sequence,agent_run_id,payload,payload_bytes
FROM bounded
WHERE cumulative_bytes<=$5::bigint OR row_number=1
ORDER BY sequence
"#;

const TRANSCRIPT_TAIL_PAGE_SQL: &str = r#"
SELECT version.block_json
FROM runtime.transcript_block_identities identity
JOIN LATERAL (
  SELECT candidate.block_json
  FROM runtime.transcript_block_versions candidate
  WHERE candidate.session_id=identity.session_id
    AND candidate.projection_version=identity.projection_version
    AND candidate.projection_generation=identity.projection_generation
    AND candidate.block_id=identity.block_id
    AND candidate.applied_source_sequence<=$4
  ORDER BY candidate.applied_source_sequence DESC
  LIMIT 1
) version ON TRUE
WHERE identity.session_id=$1 AND identity.projection_version=$2
  AND identity.projection_generation=$3 AND identity.order_source_sequence<=$4
ORDER BY identity.order_source_sequence DESC,identity.order_ordinal DESC
LIMIT $5
"#;

const TRANSCRIPT_PAGE_BEFORE_SQL: &str = r#"
SELECT version.block_json
FROM runtime.transcript_block_identities identity
JOIN LATERAL (
  SELECT candidate.block_json
  FROM runtime.transcript_block_versions candidate
  WHERE candidate.session_id=identity.session_id
    AND candidate.projection_version=identity.projection_version
    AND candidate.projection_generation=identity.projection_generation
    AND candidate.block_id=identity.block_id
    AND candidate.applied_source_sequence<=$4
  ORDER BY candidate.applied_source_sequence DESC
  LIMIT 1
) version ON TRUE
WHERE identity.session_id=$1 AND identity.projection_version=$2
  AND identity.projection_generation=$3 AND identity.order_source_sequence<=$4
  AND (identity.order_source_sequence<$5 OR (identity.order_source_sequence=$5 AND identity.order_ordinal<$6))
ORDER BY identity.order_source_sequence DESC,identity.order_ordinal DESC
LIMIT $7
"#;

#[cfg(test)]
mod tests {
    use super::{decode_source_record, SourceFact, TRANSCRIPT_SOURCE_SLICE_SQL};

    include!("transcript_boundary_tests.rs");

    #[test]
    fn current_encoding_accepts_published_rebuilds_and_rejects_old_derived_state() {
        assert!(super::workspace_transcript_generation_has_current_encoding(
            &super::workspace_transcript_initial_generation()
        ));
        assert!(super::workspace_transcript_generation_has_current_encoding(
            &super::workspace_transcript_generation("generation-rebuild-97")
        ));
        assert!(!super::workspace_transcript_generation_has_current_encoding("generation-1"));
        assert!(
            !super::workspace_transcript_generation_has_current_encoding("generation-rebuild-97")
        );
        assert!(
            !super::workspace_transcript_generation_has_current_encoding("generation-1.blocks1")
        );
    }

    #[test]
    fn workspace_resume_cursor_uses_one_session_display_stream() {
        assert_eq!(
            super::WORKSPACE_TRANSCRIPT_STREAM_ID_V1,
            "workspace-transcript.v1"
        );
        assert_eq!(42_u64.to_string(), "42");
    }

    #[test]
    fn compact_model_request_advances_through_core_envelope_without_hydration() {
        let compact = serde_json::json!({
            "schemaVersion": "session.event.v1",
            "eventVersion": 1,
            "sequence": 7,
            "type": "model_request_started",
            "eventId": "event_model_request_7",
            "sessionId": "session_compact",
            "turnId": "turn_1",
            "agentRunId": "agent_run_1",
            "createdAtMs": 7,
            "payload": {
                "requestId": "request_1",
                "observations": {"manifestDigest": format!("sha256:{}", "a".repeat(64))}
            }
        });
        let source = decode_source_record(7, "agent_run_1".to_string(), &compact)
            .expect("Core classifies model_request_started as envelope-only");
        assert!(matches!(source.fact, SourceFact::Envelope(_)));
    }
}
