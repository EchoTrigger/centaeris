use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use super::{
    parse_decimal_u64, require_identifier, TranscriptBlockV1, TranscriptPagePolicyV1,
    TranscriptPageQueryWorkV1, TranscriptPageV1, TranscriptPatchV1,
    TranscriptProjectionCheckpointV1, TranscriptProjectionFrontierV1,
    TranscriptProjectionRecoveryV1, TranscriptResumeCursorV1, TRANSCRIPT_CHECKPOINT_MAX_BYTES,
    TRANSCRIPT_PATCH_READ_MAX_PATCHES, TRANSCRIPT_PATCH_READ_SERIALIZED_MAX_BYTES,
    TRANSCRIPT_PROJECTION_SLICE_MAX_BYTES, TRANSCRIPT_PROJECTION_SLICE_MAX_EVENTS,
    TRANSCRIPT_PROJECTION_VERSION_V1,
};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TranscriptCommittedBlockV1 {
    pub applied_source_sequence: String,
    pub block: TranscriptBlockV1,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TranscriptProjectionCommitV1 {
    pub commit_id: String,
    pub session_id: String,
    pub projection_version: String,
    pub projection_generation: String,
    pub expected_source_high_water: String,
    pub source_high_water: String,
    pub upserts: Vec<TranscriptCommittedBlockV1>,
    pub resume_cursors: Vec<TranscriptResumeCursorV1>,
    pub checkpoint: Option<TranscriptProjectionCheckpointV1>,
    pub frontier: Option<TranscriptProjectionFrontierV1>,
    pub invalidation_reason: Option<String>,
}

impl TranscriptProjectionCommitV1 {
    pub fn validate(&self) -> Result<(), String> {
        require_identifier(self.commit_id.as_str(), "transcript projection commitId")?;
        require_identifier(self.session_id.as_str(), "transcript projection sessionId")?;
        if self.projection_version != TRANSCRIPT_PROJECTION_VERSION_V1 {
            return Err("transcript projectionVersion is unsupported".to_string());
        }
        require_identifier(
            self.projection_generation.as_str(),
            "transcript projectionGeneration",
        )?;
        let expected = parse_decimal_u64(
            self.expected_source_high_water.as_str(),
            "transcript projection expectedSourceHighWater",
        )?;
        let source_high_water = parse_decimal_u64(
            self.source_high_water.as_str(),
            "transcript projection sourceHighWater",
        )?;
        if source_high_water == 0 || source_high_water <= expected {
            return Err("transcript projection sourceHighWater must advance".to_string());
        }
        if self.upserts.len() > TRANSCRIPT_PROJECTION_SLICE_MAX_EVENTS {
            return Err("transcript projection commit exceeds maximum block changes".to_string());
        }
        if self.resume_cursors.len() > TRANSCRIPT_PROJECTION_SLICE_MAX_EVENTS {
            return Err("transcript projection commit exceeds maximum resume cursors".to_string());
        }
        if self.invalidation_reason.is_some()
            && (!self.upserts.is_empty() || self.checkpoint.is_some() || self.frontier.is_some())
        {
            return Err(
                "invalidated transcript projection cannot commit blocks or a checkpoint"
                    .to_string(),
            );
        }
        if let Some(reason) = self.invalidation_reason.as_deref() {
            require_identifier(reason, "transcript projection invalidationReason")?;
        }

        let mut committed = HashSet::new();
        let mut block_orders = HashMap::new();
        let mut previous_applied = 0;
        for item in &self.upserts {
            item.block.validate()?;
            let applied = parse_decimal_u64(
                item.applied_source_sequence.as_str(),
                "transcript projection appliedSourceSequence",
            )?;
            if applied == 0 || applied > source_high_water || applied < previous_applied {
                return Err(
                    "transcript projection appliedSourceSequence is not ordered within waterline"
                        .to_string(),
                );
            }
            if item.block.order_key.source_sequence_value()? > applied {
                return Err(
                    "transcript projection block orderKey is after its applied sequence"
                        .to_string(),
                );
            }
            if !committed.insert((item.block.block_id.as_str(), applied)) {
                return Err("transcript projection commit duplicates a block version".to_string());
            }
            let order = (
                item.block.order_key.source_sequence_value()?,
                item.block.order_key.ordinal,
            );
            if block_orders
                .insert(item.block.block_id.as_str(), order)
                .is_some_and(|existing| existing != order)
            {
                return Err(
                    "transcript projection block orderKey changed within commit".to_string()
                );
            }
            previous_applied = applied;
        }

        let mut stream_ids = HashSet::new();
        for cursor in &self.resume_cursors {
            cursor.validate()?;
            if !stream_ids.insert(cursor.stream_id.as_str()) {
                return Err("transcript projection commit duplicates a resume streamId".to_string());
            }
        }
        match (&self.checkpoint, &self.frontier) {
            (Some(checkpoint), Some(frontier)) => {
                let recovery = TranscriptProjectionRecoveryV1 {
                    checkpoint: checkpoint.clone(),
                    frontier: frontier.clone(),
                };
                recovery.validate()?;
                if checkpoint.session_id != self.session_id
                    || checkpoint.projection_version != self.projection_version
                    || checkpoint.projection_generation != self.projection_generation
                    || checkpoint.source_high_water != self.source_high_water
                {
                    return Err("transcript projection checkpoint binding mismatch".to_string());
                }
                let checkpoint_bytes = serde_json::to_vec(&recovery)
                    .map_err(|error| format!("serialize transcript recovery failed: {error}"))?
                    .len();
                if checkpoint_bytes > TRANSCRIPT_CHECKPOINT_MAX_BYTES {
                    return Err("transcript checkpoint exceeds maximum bytes".to_string());
                }
            }
            (None, None) => {}
            _ => {
                return Err(
                    "transcript projection checkpoint and frontier must be committed together"
                        .to_string(),
                )
            }
        }
        let serialized_bytes = serde_json::to_vec(self)
            .map_err(|error| format!("serialize transcript projection commit failed: {error}"))?
            .len();
        if serialized_bytes > TRANSCRIPT_PROJECTION_SLICE_MAX_BYTES {
            return Err(
                "transcript projection commit exceeds maximum serialized bytes".to_string(),
            );
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TranscriptProjectionCommitDispositionV1 {
    Applied,
    AlreadyApplied,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TranscriptProjectionHeadV1 {
    pub session_id: String,
    pub projection_version: String,
    pub projection_generation: String,
    pub source_high_water: String,
    pub invalidation_reason: Option<String>,
}

impl TranscriptProjectionHeadV1 {
    pub fn validate(&self) -> Result<(), String> {
        require_identifier(
            self.session_id.as_str(),
            "transcript projection head sessionId",
        )?;
        if self.projection_version != TRANSCRIPT_PROJECTION_VERSION_V1 {
            return Err("transcript projectionVersion is unsupported".to_string());
        }
        require_identifier(
            self.projection_generation.as_str(),
            "transcript projection head projectionGeneration",
        )?;
        parse_decimal_u64(
            self.source_high_water.as_str(),
            "transcript projection head sourceHighWater",
        )?;
        if let Some(reason) = self.invalidation_reason.as_deref() {
            require_identifier(reason, "transcript projection head invalidationReason")?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranscriptPageReadRequestV1 {
    pub session_id: String,
    pub projection_version: String,
    pub projection_generation: String,
    pub source_high_water: String,
    pub older_cursor: Option<String>,
    pub policy: TranscriptPagePolicyV1,
}

impl TranscriptPageReadRequestV1 {
    pub fn validate(&self) -> Result<(), String> {
        require_identifier(
            self.session_id.as_str(),
            "transcript page request sessionId",
        )?;
        if self.projection_version != TRANSCRIPT_PROJECTION_VERSION_V1 {
            return Err("transcript projectionVersion is unsupported".to_string());
        }
        require_identifier(
            self.projection_generation.as_str(),
            "transcript page request projectionGeneration",
        )?;
        parse_decimal_u64(
            self.source_high_water.as_str(),
            "transcript page request sourceHighWater",
        )?;
        if self
            .older_cursor
            .as_deref()
            .is_some_and(|cursor| cursor.trim().is_empty())
        {
            return Err("transcript page request olderCursor must not be empty".to_string());
        }
        self.policy.validate()?;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TranscriptPersistentPageQueryWorkV1 {
    pub candidate_rows_read: usize,
    pub resume_cursor_rows_read: usize,
    pub raw_event_visits: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranscriptPageReadResultV1 {
    pub page: TranscriptPageV1,
    pub work: TranscriptPersistentPageQueryWorkV1,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TranscriptPatchReadRequestV1 {
    pub session_id: String,
    pub projection_version: String,
    pub projection_generation: String,
    pub after_source_high_water: String,
    pub through_source_high_water: String,
}

impl TranscriptPatchReadRequestV1 {
    pub fn validate(&self) -> Result<(), String> {
        require_identifier(
            self.session_id.as_str(),
            "transcript patch request sessionId",
        )?;
        if self.projection_version != TRANSCRIPT_PROJECTION_VERSION_V1 {
            return Err("transcript projectionVersion is unsupported".to_string());
        }
        require_identifier(
            self.projection_generation.as_str(),
            "transcript patch request projectionGeneration",
        )?;
        let after = parse_decimal_u64(
            self.after_source_high_water.as_str(),
            "transcript patch request afterSourceHighWater",
        )?;
        let through = parse_decimal_u64(
            self.through_source_high_water.as_str(),
            "transcript patch request throughSourceHighWater",
        )?;
        if after > through {
            return Err(
                "transcript patch request afterSourceHighWater exceeds throughSourceHighWater"
                    .to_string(),
            );
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TranscriptPersistentPatchQueryWorkV1 {
    pub commit_rows_read: usize,
    pub raw_event_visits: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TranscriptPatchReadResultV1 {
    pub patches: Vec<TranscriptPatchV1>,
    pub next_source_high_water: String,
    pub has_more: bool,
    pub work: TranscriptPersistentPatchQueryWorkV1,
}

impl TranscriptPatchReadResultV1 {
    pub fn validate(&self, request: &TranscriptPatchReadRequestV1) -> Result<(), String> {
        request.validate()?;
        if self.patches.len() > TRANSCRIPT_PATCH_READ_MAX_PATCHES {
            return Err("transcript patch read exceeds maximum patch count".to_string());
        }
        let after = parse_decimal_u64(
            request.after_source_high_water.as_str(),
            "transcript patch request afterSourceHighWater",
        )?;
        let through = parse_decimal_u64(
            request.through_source_high_water.as_str(),
            "transcript patch request throughSourceHighWater",
        )?;
        let next = parse_decimal_u64(
            self.next_source_high_water.as_str(),
            "transcript patch result nextSourceHighWater",
        )?;
        let mut previous = after;
        for patch in &self.patches {
            patch.validate()?;
            if patch.session_id != request.session_id
                || patch.projection_version != request.projection_version
                || patch.projection_generation != request.projection_generation
            {
                return Err("transcript patch result projection identity mismatch".to_string());
            }
            let source_high_water = parse_decimal_u64(
                patch.source_high_water.as_str(),
                "transcript patch sourceHighWater",
            )?;
            if source_high_water <= previous || source_high_water > through {
                return Err(
                    "transcript patch result is not strictly ascending within the frozen waterline"
                        .to_string(),
                );
            }
            previous = source_high_water;
        }
        let serialized_bytes = transcript_patch_read_result_serialized_bytes(self)?;
        if serialized_bytes > TRANSCRIPT_PATCH_READ_SERIALIZED_MAX_BYTES {
            return Err("transcript patch read exceeds maximum serialized bytes".to_string());
        }
        if next != previous {
            return Err("transcript patch result nextSourceHighWater is inconsistent".to_string());
        }
        if self.has_more {
            if next >= through {
                return Err("transcript patch result hasMore is inconsistent".to_string());
            }
        } else if next != through {
            return Err("transcript patch result ended before the frozen waterline".to_string());
        }
        if self.work.commit_rows_read < self.patches.len() {
            return Err("transcript patch read commit row count is inconsistent".to_string());
        }
        if self.work.raw_event_visits != 0 {
            return Err("transcript patch read must not visit raw events".to_string());
        }
        if self.work.commit_rows_read > self.patches.len().saturating_add(1) {
            return Err("transcript patch read exceeded bounded commit lookahead".to_string());
        }
        Ok(())
    }
}

pub fn transcript_patch_read_result_serialized_bytes(
    result: &TranscriptPatchReadResultV1,
) -> Result<usize, String> {
    serde_json::to_vec(result)
        .map(|bytes| bytes.len())
        .map_err(|error| format!("serialize transcript patch read failed: {error}"))
}

pub const TRANSCRIPT_PROJECTION_GENERATION_INVALIDATED: &str =
    "transcript_projection_generation_invalidated";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TranscriptProjectionCurrentGenerationV1 {
    pub session_id: String,
    pub projection_version: String,
    pub projection_generation: String,
    pub source_high_water: String,
}

impl TranscriptProjectionCurrentGenerationV1 {
    pub fn validate(&self) -> Result<(), String> {
        require_identifier(
            self.session_id.as_str(),
            "transcript current generation sessionId",
        )?;
        if self.projection_version != TRANSCRIPT_PROJECTION_VERSION_V1 {
            return Err("transcript projectionVersion is unsupported".to_string());
        }
        require_identifier(
            self.projection_generation.as_str(),
            "transcript current projectionGeneration",
        )?;
        parse_decimal_u64(
            self.source_high_water.as_str(),
            "transcript current generation sourceHighWater",
        )?;
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TranscriptProjectionGenerationRotationV1 {
    pub session_id: String,
    pub projection_version: String,
    pub expected_current_generation: Option<String>,
    pub next_generation: String,
    pub target_source_high_water: String,
}

impl TranscriptProjectionGenerationRotationV1 {
    pub fn validate(&self) -> Result<(), String> {
        require_identifier(
            self.session_id.as_str(),
            "transcript generation rotation sessionId",
        )?;
        if self.projection_version != TRANSCRIPT_PROJECTION_VERSION_V1 {
            return Err("transcript projectionVersion is unsupported".to_string());
        }
        if let Some(expected) = self.expected_current_generation.as_deref() {
            require_identifier(
                expected,
                "transcript generation rotation expectedCurrentGeneration",
            )?;
            if expected == self.next_generation {
                return Err("transcript generation rotation must change generation".to_string());
            }
        }
        require_identifier(
            self.next_generation.as_str(),
            "transcript generation rotation nextGeneration",
        )?;
        parse_decimal_u64(
            self.target_source_high_water.as_str(),
            "transcript generation rotation targetSourceHighWater",
        )?;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TranscriptProjectionGenerationRotationDispositionV1 {
    Applied,
    AlreadyApplied,
}

pub trait TranscriptProjectionStorePort {
    fn commit_transcript_projection(
        &self,
        commit: TranscriptProjectionCommitV1,
    ) -> Result<TranscriptProjectionCommitDispositionV1, String>;

    fn load_transcript_projection_head(
        &self,
        session_id: &str,
        projection_generation: &str,
    ) -> Result<Option<TranscriptProjectionHeadV1>, String>;

    fn load_transcript_page(
        &self,
        request: TranscriptPageReadRequestV1,
    ) -> Result<TranscriptPageReadResultV1, String>;

    fn load_latest_transcript_checkpoint(
        &self,
        session_id: &str,
        projection_generation: &str,
        at_or_before_source_high_water: u64,
    ) -> Result<Option<TranscriptProjectionCheckpointV1>, String>;

    fn load_latest_transcript_recovery(
        &self,
        session_id: &str,
        projection_generation: &str,
        at_or_before_source_high_water: u64,
    ) -> Result<Option<TranscriptProjectionRecoveryV1>, String>;

    /// Reads committed patches strictly after the requested waterline and no
    /// later than the frozen target. An invalidated generation must fail
    /// explicitly rather than returning a partial or mixed-generation result.
    fn load_transcript_patches(
        &self,
        request: TranscriptPatchReadRequestV1,
    ) -> Result<TranscriptPatchReadResultV1, String>;

    /// Returns the latest cursor for every stream at or before the requested
    /// source waterline, uniquely and strictly sorted by `streamId`.
    fn load_transcript_resume_cursors(
        &self,
        session_id: &str,
        projection_generation: &str,
        at_or_before_source_high_water: u64,
    ) -> Result<Vec<TranscriptResumeCursorV1>, String>;
}

/// Owns the per-session discoverable generation pointer.
///
/// Implementations must rotate with compare-and-swap semantics and must only publish
/// `nextGeneration` when its non-invalidated head exists exactly at `targetSourceHighWater`.
/// A failed or partial rebuild remains undiscoverable. Callers serving page/patch traffic use the
/// current-only helpers below, so a cursor from an old generation fails explicitly.
pub trait TranscriptProjectionGenerationStorePortV1: TranscriptProjectionStorePort {
    fn load_current_transcript_projection_generation(
        &self,
        session_id: &str,
    ) -> Result<Option<TranscriptProjectionCurrentGenerationV1>, String>;

    fn rotate_current_transcript_projection_generation(
        &self,
        rotation: TranscriptProjectionGenerationRotationV1,
    ) -> Result<TranscriptProjectionGenerationRotationDispositionV1, String>;

    fn load_current_transcript_page(
        &self,
        request: TranscriptPageReadRequestV1,
    ) -> Result<TranscriptPageReadResultV1, String> {
        request.validate()?;
        let current = self
            .load_current_transcript_projection_generation(request.session_id.as_str())?
            .ok_or_else(|| TRANSCRIPT_PROJECTION_GENERATION_INVALIDATED.to_string())?;
        current.validate()?;
        if current.projection_generation != request.projection_generation {
            return Err(TRANSCRIPT_PROJECTION_GENERATION_INVALIDATED.to_string());
        }
        self.load_transcript_page(request)
    }

    fn load_current_transcript_patches(
        &self,
        request: TranscriptPatchReadRequestV1,
    ) -> Result<TranscriptPatchReadResultV1, String> {
        request.validate()?;
        let current = self
            .load_current_transcript_projection_generation(request.session_id.as_str())?
            .ok_or_else(|| TRANSCRIPT_PROJECTION_GENERATION_INVALIDATED.to_string())?;
        current.validate()?;
        if current.projection_generation != request.projection_generation {
            return Err(TRANSCRIPT_PROJECTION_GENERATION_INVALIDATED.to_string());
        }
        self.load_transcript_patches(request)
    }
}

impl From<TranscriptPageQueryWorkV1> for TranscriptPersistentPageQueryWorkV1 {
    fn from(work: TranscriptPageQueryWorkV1) -> Self {
        Self {
            candidate_rows_read: work.order_keys_visited,
            resume_cursor_rows_read: 0,
            raw_event_visits: work.raw_event_visits,
        }
    }
}
