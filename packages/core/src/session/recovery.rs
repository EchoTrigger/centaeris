//! Streaming recovery at a fixed Session high-water mark.
//!
//! The caller first supplies ordered envelopes to the filter, then hydrates and
//! feeds validated active records and inactive provenance separately. Historical request bodies are validated and dropped;
//! their small input/delivery facts and compaction source digests remain authoritative.
use super::*;
use crate::runtime::prompt_compaction_commit::{
    reconstructed_source_request, CompactionSourceRequest,
};

/// First-pass tombstone decisions. This intentionally does not validate external
/// CAS bodies: all records must still pass full validation in the second pass.
pub struct SessionRecoveryFilter {
    session_id: String,
    ledger: ActiveTombstoneLedger,
}
impl SessionRecoveryFilter {
    pub fn new(session_id: &str) -> Self {
        Self {
            session_id: session_id.into(),
            ledger: ActiveTombstoneLedger::default(),
        }
    }
    pub fn push_wire_envelope(&mut self, wire: &Value) -> Result<(), String> {
        let root = wire
            .as_object()
            .ok_or("Session recovery envelope must be an object")?;
        const FIELDS: &[&str] = &[
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
        ];
        if root.keys().any(|key| !FIELDS.contains(&key.as_str())) {
            return Err("Session recovery envelope has unknown fields".into());
        }
        if wire["schemaVersion"] != SESSION_EVENT_SCHEMA_VERSION
            || wire["eventVersion"] != SESSION_EVENT_VERSION
        {
            return Err("unsupported Session recovery envelope schema/version".into());
        }
        if wire["sequence"]
            .as_u64()
            .is_none_or(|sequence| sequence == 0)
            || wire["createdAtMs"].as_i64().is_none()
            || !wire["payload"].is_object()
        {
            return Err("invalid Session recovery envelope fields".into());
        }
        let event_id = wire["eventId"]
            .as_str()
            .filter(|id| !id.trim().is_empty() && id.len() <= SESSION_EVENT_ID_MAX_BYTES)
            .ok_or("invalid Session recovery eventId")?;
        let session_id = wire["sessionId"]
            .as_str()
            .filter(|id| !id.trim().is_empty())
            .ok_or("invalid Session recovery sessionId")?;
        for field in ["agentRunId", "turnId"] {
            if let Some(value) = root.get(field) {
                if !value.is_null() && value.as_str().is_none_or(|id| id.trim().is_empty()) {
                    return Err(format!("invalid Session recovery {field}"));
                }
            }
        }
        let event_type: SessionRecordType = serde_json::from_value(wire["type"].clone())
            .map_err(|error| format!("unsupported Session recovery type: {error}"))?;
        let targets = if event_type == SessionRecordType::Tombstone {
            let record = parse_wire_record(wire)
                .map_err(|error| error.to_string())?
                .event;
            record.payload["targetEventIds"]
                .as_array()
                .ok_or("invalid tombstone targets")?
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| "invalid tombstone target".to_string())
                })
                .collect::<Result<Vec<_>, _>>()?
        } else {
            Vec::new()
        };
        self.ledger
            .apply_fact(&self.session_id, event_id, session_id, event_type, &targets)
    }
    pub fn is_inactive(&self, event_id: &str) -> bool {
        self.ledger.is_inactive(event_id)
    }
}

/// Minimal original ToolCall identity retained even when its record is inactive.
/// Admission historically checks every origin, not only the active transcript.
pub(crate) struct ToolCallRecoveryOrigin {
    pub(crate) call_id: String,
    pub(crate) agent_run_id: String,
    pub(crate) turn_id: String,
    pub(crate) event_id: String,
}
impl ToolCallRecoveryOrigin {
    pub(crate) fn from_record(record: &SessionLogRecord) -> Result<Option<Self>, String> {
        if record.event_type != SessionRecordType::ToolCall {
            return Ok(None);
        }
        Ok(Some(Self {
            call_id: required_payload_string(payload_object(record)?, "callId", record)?,
            agent_run_id: required_event_agent_run_id(record)?,
            turn_id: required_event_turn_id(record)?,
            event_id: record.event_id.clone(),
        }))
    }
}

pub struct SessionRecoveryResult {
    pub snapshot: SessionStateSnapshot,
    /// Real active source facts for admission closure identity, never synthetic
    /// request records with missing observations.
    pub source_records: Vec<SessionLogRecord>,
    pub(crate) tool_call_origins: Vec<ToolCallRecoveryOrigin>,
}

#[derive(Default)]
pub(super) struct ModelControlFacts {
    committed_by_run: BTreeMap<String, std::collections::BTreeSet<String>>,
    by_turn: HashMap<(String, String), Vec<String>>,
    delivery: Option<(crate::runtime::RequiredCompletionDeliveryV1, String, String)>,
}
impl ModelControlFacts {
    fn apply(&mut self, record: &SessionLogRecord) -> Result<(), String> {
        if record.event_type != SessionRecordType::ModelRequestStarted
            || record.payload["purpose"] != "main"
        {
            return Ok(());
        }
        let run = required_event_agent_run_id(record)?;
        let turn = required_event_turn_id(record)?;
        let observations: Vec<ModelObservationV1> =
            serde_json::from_value(record.payload["observations"].clone())
                .map_err(|error| format!("decode Session model observations failed: {error}"))?;
        for observation in observations {
            match observation {
                ModelObservationV1::InputUptake { input_ids } => {
                    self.committed_by_run
                        .entry(run.clone())
                        .or_default()
                        .extend(input_ids.iter().cloned());
                    let ids = self.by_turn.entry((run.clone(), turn.clone())).or_default();
                    for id in input_ids {
                        if !ids.contains(&id) {
                            ids.push(id);
                        }
                    }
                }
                ModelObservationV1::RequiredCompletionDelivery { state } => {
                    // Invoke the existing owner/state contract before retiring an
                    // older observation; invalid intermediate states still fail.
                    let mut scratch = SessionStateSnapshot::new(String::new(), 0);
                    crate::runtime::completion_delivery::restore_observation(
                        &mut scratch,
                        &state,
                        &run,
                        &turn,
                    )?;
                    self.delivery = Some((state, run.clone(), turn.clone()));
                }
                _ => {}
            }
        }
        Ok(())
    }
    pub(super) fn restore_into(
        &self,
        snapshot: &mut SessionStateSnapshot,
        current: Option<&SessionLogRecord>,
    ) -> Result<(), String> {
        if let Some((state, run, turn)) = &self.delivery {
            crate::runtime::completion_delivery::restore_observation(snapshot, state, run, turn)?;
        }
        for (run, ids) in &self.committed_by_run {
            turn_input::recovery::record_committed(
                snapshot,
                run,
                &ids.iter().cloned().collect::<Vec<_>>(),
            )?;
        }
        if let Some(current) = current {
            let run = required_event_agent_run_id(current)?;
            let turn = required_event_turn_id(current)?;
            let ids = self
                .by_turn
                .get(&(run.clone(), turn.clone()))
                .map_or(&[][..], Vec::as_slice);
            turn_input::recovery::freeze_prepared(snapshot, &run, &turn, ids)?;
        }
        Ok(())
    }
}
#[cfg(test)]
pub(super) fn control_facts(records: &[SessionLogRecord]) -> Result<ModelControlFacts, String> {
    let mut facts = ModelControlFacts::default();
    for record in records {
        facts.apply(record)?;
    }
    Ok(facts)
}
#[cfg(test)]
pub(super) fn compaction_sources(
    records: &[SessionLogRecord],
) -> Result<HashMap<String, CompactionSourceRequest>, String> {
    let mut latest = HashMap::new();
    let mut sources = HashMap::new();
    for record in records {
        capture_source(record, &mut latest, &mut sources)?;
    }
    Ok(sources)
}
fn capture_source(
    record: &SessionLogRecord,
    latest: &mut HashMap<String, CompactionSourceRequest>,
    sources: &mut HashMap<String, CompactionSourceRequest>,
) -> Result<(), String> {
    if record.event_type == SessionRecordType::ModelRequestStarted
        && record.payload["purpose"] == "compaction"
    {
        latest.insert(
            required_event_agent_run_id(record)?,
            reconstructed_source_request(record)?,
        );
    } else if record.event_type == SessionRecordType::Compaction {
        if let Some(source) = record.agent_run_id.as_ref().and_then(|run| latest.get(run)) {
            sources.insert(record.event_id.clone(), source.clone());
        }
    }
    Ok(())
}

/// Consumes active records in canonical order. Full model observations are owned
/// only for the latest main request; other retained source facts grow linearly
/// with actual transcript/control history, not with repeated context snapshots.
pub struct SessionRecoveryAccumulator {
    session_id: String,
    records: Vec<SessionLogRecord>,
    tool_call_origins: Vec<ToolCallRecoveryOrigin>,
    latest_main: Option<(usize, SessionLogRecord)>,
    controls: ModelControlFacts,
    latest_sources: HashMap<String, CompactionSourceRequest>,
    sources: HashMap<String, CompactionSourceRequest>,
    projection: SessionProjection,
    seen: HashSet<String>,
    open_calls: HashMap<String, (String, String, String)>,
    updated_at_ms: i64,
}
impl SessionRecoveryAccumulator {
    pub fn new(session_id: &str) -> Self {
        Self {
            session_id: session_id.into(),
            records: Vec::new(),
            tool_call_origins: Vec::new(),
            latest_main: None,
            controls: ModelControlFacts::default(),
            latest_sources: HashMap::new(),
            sources: HashMap::new(),
            projection: SessionProjection::default(),
            seen: HashSet::new(),
            open_calls: HashMap::new(),
            updated_at_ms: 0,
        }
    }
    pub fn push(&mut self, record: SessionLogRecord) -> Result<(), String> {
        if record.event_type == SessionRecordType::Tombstone {
            return Err("Session recovery requires prefiltered active records".into());
        }
        reduce_event(
            &self.session_id,
            &mut self.projection,
            &mut self.seen,
            &mut self.open_calls,
            &record,
        )?;
        if let Some(origin) = ToolCallRecoveryOrigin::from_record(&record)? {
            self.tool_call_origins.push(origin);
        }
        capture_source(&record, &mut self.latest_sources, &mut self.sources)?;
        self.controls.apply(&record)?;
        self.updated_at_ms = record.created_at_ms;
        if record.event_type == SessionRecordType::ModelRequestStarted {
            if record.payload["purpose"] == "main" {
                self.latest_main = Some((self.records.len(), record));
            }
        } else {
            self.records.push(record);
        }
        Ok(())
    }
    /// Validate and retire an inactive record without adding it to the active projection.
    pub fn push_inactive_record(&mut self, record: SessionLogRecord) -> Result<(), String> {
        validate_event_shape(&record)?;
        if record.session_id != self.session_id {
            return Err("cross-session inactive recovery record".into());
        }
        if let Some(origin) = ToolCallRecoveryOrigin::from_record(&record)? {
            self.tool_call_origins.push(origin);
        }
        Ok(())
    }
    /// Serialized bytes of the actually owned model observations, excluding
    /// allocation/container overhead; this is not a heap or disk-I/O estimate.
    #[cfg(test)]
    pub(super) fn retained_model_observation_bytes(&self) -> usize {
        self.records
            .iter()
            .filter(|record| record.event_type == SessionRecordType::ModelRequestStarted)
            .map(|record| record.payload["observations"].to_string().len())
            .sum::<usize>()
            + self.latest_main.as_ref().map_or(0, |(_, record)| {
                record.payload["observations"].to_string().len()
            })
    }
    pub fn finish(mut self) -> Result<SessionRecoveryResult, String> {
        if self.session_id.trim().is_empty() {
            return Err("expected sessionId is required".into());
        }
        if let Some((index, main)) = self.latest_main {
            self.records.insert(index, main);
        }
        let snapshot = restore_active_runtime_snapshot(
            &self.session_id,
            &self.records,
            &self.projection,
            &self.controls,
            &self.sources,
            self.updated_at_ms,
        )?;
        self.records
            .retain(|record| record.event_type != SessionRecordType::ModelRequestStarted);
        Ok(SessionRecoveryResult {
            snapshot,
            source_records: self.records,
            tool_call_origins: self.tool_call_origins,
        })
    }
}
