use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use super::{
    parse_decimal_u64, require_identifier, TranscriptBlockBodyV1, TranscriptBlockStatusV1,
    TranscriptBlockV1, TranscriptProjectionCheckpointV1, TRANSCRIPT_CHECKPOINT_MAX_BYTES,
    TRANSCRIPT_CHECKPOINT_SCHEMA_V1, TRANSCRIPT_PROJECTION_SLICE_MAX_EVENTS,
    TRANSCRIPT_PROJECTION_VERSION_V1,
};

pub const TRANSCRIPT_FRONTIER_SCHEMA_V1: &str = "transcript.frontier.v1";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TranscriptCheckpointRefsV1 {
    pub frontier_ref: String,
    pub block_index_ref: String,
}

impl TranscriptCheckpointRefsV1 {
    pub fn validate(&self) -> Result<(), String> {
        require_identifier(
            self.frontier_ref.as_str(),
            "transcript checkpoint frontierRef",
        )?;
        require_identifier(
            self.block_index_ref.as_str(),
            "transcript checkpoint blockIndexRef",
        )
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TranscriptProjectionOpenToolV1 {
    pub call_id: String,
    pub block: TranscriptBlockV1,
}

impl TranscriptProjectionOpenToolV1 {
    pub fn validate(&self, source_high_water: u64) -> Result<(), String> {
        require_identifier(self.call_id.as_str(), "transcript frontier tool callId")?;
        self.block.validate()?;
        if self.block.block_id != format!("tool:{}", self.call_id) {
            return Err("transcript frontier tool blockId mismatch".to_string());
        }
        if self.block.order_key.source_sequence_value()? > source_high_water {
            return Err("transcript frontier tool is after sourceHighWater".to_string());
        }
        match &self.block.body {
            TranscriptBlockBodyV1::Tool {
                call_id,
                status: TranscriptBlockStatusV1::Running,
                output_ref: None,
                ..
            } if call_id == &self.call_id => Ok(()),
            TranscriptBlockBodyV1::Tool { .. } => {
                Err("transcript frontier contains a settled or mismatched tool".to_string())
            }
            _ => Err("transcript frontier open tool must contain a tool block".to_string()),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TranscriptProjectionFrontierV1 {
    pub schema: String,
    pub frontier_ref: String,
    pub session_id: String,
    pub projection_version: String,
    pub projection_generation: String,
    pub source_high_water: String,
    pub open_tools: Vec<TranscriptProjectionOpenToolV1>,
}

impl TranscriptProjectionFrontierV1 {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != TRANSCRIPT_FRONTIER_SCHEMA_V1 {
            return Err("transcript frontier schema is unsupported".to_string());
        }
        require_identifier(self.frontier_ref.as_str(), "transcript frontierRef")?;
        require_identifier(self.session_id.as_str(), "transcript frontier sessionId")?;
        if self.projection_version != TRANSCRIPT_PROJECTION_VERSION_V1 {
            return Err("transcript frontier projectionVersion is unsupported".to_string());
        }
        require_identifier(
            self.projection_generation.as_str(),
            "transcript frontier projectionGeneration",
        )?;
        let source_high_water = parse_decimal_u64(
            self.source_high_water.as_str(),
            "transcript frontier sourceHighWater",
        )?;
        if self.open_tools.len() > TRANSCRIPT_PROJECTION_SLICE_MAX_EVENTS {
            return Err("transcript frontier exceeds maximum open tools".to_string());
        }
        let mut call_ids = HashSet::new();
        let mut orders = HashSet::new();
        let mut previous_order = None;
        for tool in &self.open_tools {
            tool.validate(source_high_water)?;
            if !call_ids.insert(tool.call_id.as_str()) {
                return Err("transcript frontier duplicates tool callId".to_string());
            }
            let order = (
                tool.block.order_key.source_sequence_value()?,
                tool.block.order_key.ordinal,
            );
            if !orders.insert(order) {
                return Err("transcript frontier duplicates tool orderKey".to_string());
            }
            if previous_order.is_some_and(|previous| previous >= order) {
                return Err("transcript frontier open tools are not ordered".to_string());
            }
            previous_order = Some(order);
        }
        let bytes = serde_json::to_vec(self)
            .map_err(|error| format!("serialize transcript frontier failed: {error}"))?
            .len();
        if bytes > TRANSCRIPT_CHECKPOINT_MAX_BYTES {
            return Err("transcript frontier exceeds maximum bytes".to_string());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TranscriptProjectionRecoveryV1 {
    pub checkpoint: TranscriptProjectionCheckpointV1,
    pub frontier: TranscriptProjectionFrontierV1,
}

impl TranscriptProjectionRecoveryV1 {
    pub fn validate(&self) -> Result<(), String> {
        self.checkpoint.validate()?;
        self.frontier.validate()?;
        if self.checkpoint.schema != TRANSCRIPT_CHECKPOINT_SCHEMA_V1
            || self.checkpoint.frontier_ref != self.frontier.frontier_ref
            || self.checkpoint.session_id != self.frontier.session_id
            || self.checkpoint.projection_version != self.frontier.projection_version
            || self.checkpoint.projection_generation != self.frontier.projection_generation
            || self.checkpoint.source_high_water != self.frontier.source_high_water
        {
            return Err("transcript checkpoint frontier binding mismatch".to_string());
        }
        let bytes = serde_json::to_vec(self)
            .map_err(|error| format!("serialize transcript recovery failed: {error}"))?
            .len();
        if bytes > TRANSCRIPT_CHECKPOINT_MAX_BYTES {
            return Err("transcript recovery exceeds maximum bytes".to_string());
        }
        Ok(())
    }
}
