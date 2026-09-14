use super::{TRANSCRIPT_CHECKPOINT_REQUIRED_EVENTS, TRANSCRIPT_CHECKPOINT_REQUIRED_EVENT_BYTES};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TranscriptCheckpointDecisionV1 {
    NotDue,
    AwaitingSafePoint,
    WriteCheckpoint,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TranscriptCheckpointWorkV1 {
    events_since_checkpoint: usize,
    event_bytes_since_checkpoint: usize,
    page_sealed_since_checkpoint: bool,
}

impl TranscriptCheckpointWorkV1 {
    pub fn observe_source_event(&mut self, canonical_bytes: usize) -> Result<(), String> {
        self.events_since_checkpoint = self
            .events_since_checkpoint
            .checked_add(1)
            .ok_or_else(|| "transcript checkpoint event count overflow".to_string())?;
        self.event_bytes_since_checkpoint = self
            .event_bytes_since_checkpoint
            .checked_add(canonical_bytes)
            .ok_or_else(|| "transcript checkpoint event bytes overflow".to_string())?;
        Ok(())
    }

    pub fn observe_page_sealed(&mut self) {
        self.page_sealed_since_checkpoint = true;
    }

    pub fn decision(&self, semantic_safe_point: bool) -> TranscriptCheckpointDecisionV1 {
        let due = self.page_sealed_since_checkpoint
            || self.events_since_checkpoint >= TRANSCRIPT_CHECKPOINT_REQUIRED_EVENTS
            || self.event_bytes_since_checkpoint >= TRANSCRIPT_CHECKPOINT_REQUIRED_EVENT_BYTES;
        match (due, semantic_safe_point) {
            (false, _) => TranscriptCheckpointDecisionV1::NotDue,
            (true, false) => TranscriptCheckpointDecisionV1::AwaitingSafePoint,
            (true, true) => TranscriptCheckpointDecisionV1::WriteCheckpoint,
        }
    }

    pub fn checkpoint_committed(&mut self) {
        self.events_since_checkpoint = 0;
        self.event_bytes_since_checkpoint = 0;
        self.page_sealed_since_checkpoint = false;
    }

    pub fn events_since_checkpoint(&self) -> usize {
        self.events_since_checkpoint
    }

    pub fn event_bytes_since_checkpoint(&self) -> usize {
        self.event_bytes_since_checkpoint
    }
}
