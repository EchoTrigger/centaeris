use std::collections::HashSet;

use serde::{Deserialize, Serialize};

pub const TRANSCRIPT_PAGE_SCHEMA_V1: &str = "transcript.page.v1";
pub const TRANSCRIPT_PATCH_SCHEMA_V1: &str = "transcript.patch.v1";
pub const TRANSCRIPT_CHECKPOINT_SCHEMA_V1: &str = "transcript.checkpoint.v1";
pub const TRANSCRIPT_PROJECTION_VERSION_V1: &str = "transcript.projection.v1";

pub const TRANSCRIPT_PAGE_INLINE_CONTENT_MAX_BYTES: usize = 64 * 1024;
pub const TRANSCRIPT_PAGE_BLOCK_MAX_COUNT: usize = 128;
pub const TRANSCRIPT_PAGE_RESUME_CURSOR_MAX_COUNT: usize = TRANSCRIPT_PAGE_BLOCK_MAX_COUNT;
pub const TRANSCRIPT_PAGE_SERIALIZED_MAX_BYTES: usize =
    4 * TRANSCRIPT_PAGE_INLINE_CONTENT_MAX_BYTES;
pub const TRANSCRIPT_PATCH_MAX_CHANGES: usize = TRANSCRIPT_PAGE_BLOCK_MAX_COUNT;
pub const TRANSCRIPT_PATCH_SERIALIZED_MAX_BYTES: usize = TRANSCRIPT_PAGE_SERIALIZED_MAX_BYTES;
pub const TRANSCRIPT_PATCH_READ_MAX_PATCHES: usize = TRANSCRIPT_PAGE_BLOCK_MAX_COUNT;
pub const TRANSCRIPT_PATCH_READ_SERIALIZED_MAX_BYTES: usize =
    2 * TRANSCRIPT_PATCH_SERIALIZED_MAX_BYTES;
pub const TRANSCRIPT_PROJECTION_SLICE_MAX_EVENTS: usize = TRANSCRIPT_PAGE_BLOCK_MAX_COUNT;
pub const TRANSCRIPT_PROJECTION_SLICE_MAX_BYTES: usize = TRANSCRIPT_PAGE_SERIALIZED_MAX_BYTES;
pub const TRANSCRIPT_PROJECTION_SLICE_MAX_MICROS: u64 = 4_000;
pub const TRANSCRIPT_CHECKPOINT_REQUIRED_EVENTS: usize = 4 * TRANSCRIPT_PROJECTION_SLICE_MAX_EVENTS;
pub const TRANSCRIPT_CHECKPOINT_REQUIRED_EVENT_BYTES: usize =
    8 * TRANSCRIPT_PROJECTION_SLICE_MAX_BYTES;
pub const TRANSCRIPT_CHECKPOINT_MAX_BYTES: usize = TRANSCRIPT_PAGE_SERIALIZED_MAX_BYTES;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TranscriptPagePolicyV1 {
    pub max_blocks: usize,
    pub max_inline_content_bytes: usize,
    pub max_serialized_bytes: usize,
}

impl TranscriptPagePolicyV1 {
    pub fn validate(self) -> Result<Self, String> {
        if self.max_blocks == 0 || self.max_blocks > TRANSCRIPT_PAGE_BLOCK_MAX_COUNT {
            return Err("transcript page maxBlocks is invalid".to_string());
        }
        if self.max_inline_content_bytes == 0
            || self.max_inline_content_bytes > TRANSCRIPT_PAGE_INLINE_CONTENT_MAX_BYTES
        {
            return Err("transcript page maxInlineContentBytes is invalid".to_string());
        }
        if self.max_serialized_bytes == 0
            || self.max_serialized_bytes > TRANSCRIPT_PAGE_SERIALIZED_MAX_BYTES
        {
            return Err("transcript page maxSerializedBytes is invalid".to_string());
        }
        Ok(self)
    }
}

impl Default for TranscriptPagePolicyV1 {
    fn default() -> Self {
        Self {
            max_blocks: TRANSCRIPT_PAGE_BLOCK_MAX_COUNT,
            max_inline_content_bytes: TRANSCRIPT_PAGE_INLINE_CONTENT_MAX_BYTES,
            max_serialized_bytes: TRANSCRIPT_PAGE_SERIALIZED_MAX_BYTES,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TranscriptOrderKeyV1 {
    pub source_sequence: String,
    pub ordinal: u32,
}

impl TranscriptOrderKeyV1 {
    pub fn source_sequence_value(&self) -> Result<u64, String> {
        parse_decimal_u64(
            self.source_sequence.as_str(),
            "transcript block sourceSequence",
        )
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TranscriptContentRefV1 {
    pub ref_id: String,
    pub revision: String,
    pub byte_length: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TranscriptTextContentV1 {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inline_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_ref: Option<TranscriptContentRefV1>,
}

impl TranscriptTextContentV1 {
    pub fn inline(value: String) -> Self {
        Self {
            inline_content: Some(value),
            source_ref: None,
        }
    }

    pub fn referenced(reference: TranscriptContentRefV1) -> Self {
        Self {
            inline_content: None,
            source_ref: Some(reference),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        match (&self.inline_content, &self.source_ref) {
            (Some(text), None) if text.len() <= TRANSCRIPT_PAGE_INLINE_CONTENT_MAX_BYTES => Ok(()),
            (Some(_), None) => Err("transcript inline content exceeds page budget".to_string()),
            (None, Some(reference)) => reference.validate(),
            _ => Err(
                "transcript text content must contain exactly one of inlineContent or sourceRef"
                    .to_string(),
            ),
        }
    }

    pub fn inline_content_bytes(&self) -> usize {
        self.inline_content
            .as_deref()
            .map(str::len)
            .unwrap_or_default()
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum TranscriptBlockStatusV1 {
    Queued,
    Running,
    Completed,
    Failed,
    Interrupted,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum TranscriptBlockBodyV1 {
    UserText {
        content: TranscriptTextContentV1,
    },
    AssistantText {
        content: TranscriptTextContentV1,
        status: TranscriptBlockStatusV1,
    },
    Reasoning {
        request_id: String,
        content: TranscriptTextContentV1,
        status: TranscriptBlockStatusV1,
    },
    Tool {
        call_id: String,
        tool_name: String,
        status: TranscriptBlockStatusV1,
        summary: Option<String>,
        output_ref: Option<TranscriptContentRefV1>,
    },
    Notice {
        notice_type: String,
        content: TranscriptTextContentV1,
        status: TranscriptBlockStatusV1,
    },
}

impl TranscriptBlockBodyV1 {
    pub fn inline_content_bytes(&self) -> usize {
        match self {
            Self::UserText { content }
            | Self::AssistantText { content, .. }
            | Self::Reasoning { content, .. }
            | Self::Notice { content, .. } => content.inline_content_bytes(),
            Self::Tool { summary, .. } => summary.as_deref().map(str::len).unwrap_or_default(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TranscriptBlockV1 {
    pub block_id: String,
    pub block_revision: String,
    pub order_key: TranscriptOrderKeyV1,
    pub body: TranscriptBlockBodyV1,
}

impl TranscriptBlockV1 {
    pub fn order_key(&self) -> &TranscriptOrderKeyV1 {
        &self.order_key
    }

    pub fn validate(&self) -> Result<(), String> {
        require_identifier(self.block_id.as_str(), "transcript blockId")?;
        parse_decimal_u64(self.block_revision.as_str(), "transcript blockRevision")?;
        self.order_key.source_sequence_value()?;
        match &self.body {
            TranscriptBlockBodyV1::Reasoning {
                request_id,
                content,
                ..
            } => {
                require_identifier(request_id, "transcript reasoning requestId")?;
                content.validate()?;
            }
            TranscriptBlockBodyV1::Tool {
                call_id,
                tool_name,
                output_ref,
                ..
            } => {
                require_identifier(call_id, "transcript tool callId")?;
                require_identifier(tool_name, "transcript toolName")?;
                if let Some(reference) = output_ref {
                    reference.validate()?;
                }
            }
            TranscriptBlockBodyV1::Notice {
                notice_type,
                content,
                ..
            } => {
                require_identifier(notice_type, "transcript noticeType")?;
                content.validate()?;
            }
            TranscriptBlockBodyV1::UserText { content }
            | TranscriptBlockBodyV1::AssistantText { content, .. } => content.validate()?,
        }
        Ok(())
    }
}

impl TranscriptContentRefV1 {
    pub fn validate(&self) -> Result<(), String> {
        require_identifier(self.ref_id.as_str(), "transcript content refId")?;
        parse_decimal_u64(self.revision.as_str(), "transcript content revision")?;
        parse_decimal_u64(self.byte_length.as_str(), "transcript content byteLength")?;
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TranscriptResumeCursorV1 {
    pub stream_id: String,
    pub cursor: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TranscriptPageV1 {
    pub schema: String,
    pub session_id: String,
    pub projection_version: String,
    pub projection_generation: String,
    pub source_high_water: String,
    pub blocks: Vec<TranscriptBlockV1>,
    pub older_cursor: Option<String>,
    pub has_older: bool,
    pub resume_cursors: Vec<TranscriptResumeCursorV1>,
}

impl TranscriptPageV1 {
    pub fn validate(&self, policy: TranscriptPagePolicyV1) -> Result<(), String> {
        let policy = policy.validate()?;
        validate_projection_identity(
            self.schema.as_str(),
            TRANSCRIPT_PAGE_SCHEMA_V1,
            self.session_id.as_str(),
            self.projection_version.as_str(),
            self.projection_generation.as_str(),
        )?;
        let source_high_water = parse_decimal_u64(
            self.source_high_water.as_str(),
            "transcript page sourceHighWater",
        )?;
        if self.blocks.len() > policy.max_blocks {
            return Err("transcript page exceeds maxBlocks".to_string());
        }
        let mut previous_order = None;
        let mut block_ids = HashSet::new();
        let mut inline_bytes = 0usize;
        for block in &self.blocks {
            block.validate()?;
            if !block_ids.insert(block.block_id.as_str()) {
                return Err("transcript page contains duplicate blockId".to_string());
            }
            let order = (
                block.order_key.source_sequence_value()?,
                block.order_key.ordinal,
            );
            if order.0 > source_high_water {
                return Err("transcript page block is after sourceHighWater".to_string());
            }
            if previous_order.is_some_and(|previous| previous >= order) {
                return Err("transcript page blocks are not strictly ordered".to_string());
            }
            previous_order = Some(order);
            inline_bytes = inline_bytes.saturating_add(block.body.inline_content_bytes());
        }
        if inline_bytes > policy.max_inline_content_bytes {
            return Err("transcript page exceeds maxInlineContentBytes".to_string());
        }
        if self.has_older != self.older_cursor.is_some() {
            return Err("transcript page hasOlder and olderCursor disagree".to_string());
        }
        if self
            .older_cursor
            .as_deref()
            .is_some_and(|cursor| cursor.trim().is_empty())
        {
            return Err("transcript page olderCursor must not be empty".to_string());
        }
        let mut stream_ids = HashSet::new();
        if self.resume_cursors.len() > TRANSCRIPT_PAGE_RESUME_CURSOR_MAX_COUNT {
            return Err("transcript page exceeds maximum resume cursor count".to_string());
        }
        for cursor in &self.resume_cursors {
            cursor.validate()?;
            if !stream_ids.insert(cursor.stream_id.as_str()) {
                return Err("transcript page contains duplicate resume streamId".to_string());
            }
        }
        let serialized_bytes = serde_json::to_vec(self)
            .map_err(|error| format!("serialize transcript page failed: {error}"))?
            .len();
        if serialized_bytes > policy.max_serialized_bytes {
            return Err("transcript page exceeds maxSerializedBytes".to_string());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TranscriptBlockRemovalV1 {
    pub block_id: String,
    pub block_revision: String,
}

impl TranscriptBlockRemovalV1 {
    pub fn validate(&self) -> Result<(), String> {
        require_identifier(self.block_id.as_str(), "transcript removal blockId")?;
        parse_decimal_u64(
            self.block_revision.as_str(),
            "transcript removal blockRevision",
        )?;
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TranscriptPatchV1 {
    pub schema: String,
    pub session_id: String,
    pub projection_version: String,
    pub projection_generation: String,
    pub source_high_water: String,
    pub stream_id: String,
    pub applied_cursor: String,
    pub upserts: Vec<TranscriptBlockV1>,
    pub removals: Vec<TranscriptBlockRemovalV1>,
}

impl TranscriptPatchV1 {
    pub fn validate(&self) -> Result<(), String> {
        validate_projection_identity(
            self.schema.as_str(),
            TRANSCRIPT_PATCH_SCHEMA_V1,
            self.session_id.as_str(),
            self.projection_version.as_str(),
            self.projection_generation.as_str(),
        )?;
        let source_high_water = parse_decimal_u64(
            self.source_high_water.as_str(),
            "transcript patch sourceHighWater",
        )?;
        require_identifier(self.stream_id.as_str(), "transcript patch streamId")?;
        require_identifier(
            self.applied_cursor.as_str(),
            "transcript patch appliedCursor",
        )?;
        if self.upserts.len().saturating_add(self.removals.len()) > TRANSCRIPT_PATCH_MAX_CHANGES {
            return Err("transcript patch exceeds maximum change count".to_string());
        }
        let mut block_ids = HashSet::new();
        for block in &self.upserts {
            block.validate()?;
            if block.order_key.source_sequence_value()? > source_high_water {
                return Err("transcript patch block is after sourceHighWater".to_string());
            }
            if !block_ids.insert(block.block_id.as_str()) {
                return Err("transcript patch contains duplicate block change".to_string());
            }
        }
        for removal in &self.removals {
            removal.validate()?;
            if !block_ids.insert(removal.block_id.as_str()) {
                return Err("transcript patch contains duplicate block change".to_string());
            }
        }
        let serialized_bytes = serde_json::to_vec(self)
            .map_err(|error| format!("serialize transcript patch failed: {error}"))?
            .len();
        if serialized_bytes > TRANSCRIPT_PATCH_SERIALIZED_MAX_BYTES {
            return Err("transcript patch exceeds maximum serialized bytes".to_string());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TranscriptProjectionCheckpointV1 {
    pub schema: String,
    pub session_id: String,
    pub projection_version: String,
    pub projection_generation: String,
    pub source_high_water: String,
    pub frontier_ref: String,
    pub block_index_ref: String,
}

impl TranscriptResumeCursorV1 {
    pub fn validate(&self) -> Result<(), String> {
        require_identifier(self.stream_id.as_str(), "transcript resume streamId")?;
        require_identifier(self.cursor.as_str(), "transcript resume cursor")?;
        Ok(())
    }
}

pub fn validate_transcript_resume_cursor_read(
    cursors: &[TranscriptResumeCursorV1],
) -> Result<(), String> {
    if cursors.len() > TRANSCRIPT_PAGE_RESUME_CURSOR_MAX_COUNT {
        return Err("transcript resume cursor read exceeds maximum cursor count".to_string());
    }
    let mut previous_stream_id: Option<&str> = None;
    for cursor in cursors {
        cursor.validate()?;
        if previous_stream_id.is_some_and(|previous| previous >= cursor.stream_id.as_str()) {
            return Err(
                "transcript resume cursor read must be unique and sorted by streamId".to_string(),
            );
        }
        previous_stream_id = Some(cursor.stream_id.as_str());
    }
    Ok(())
}

impl TranscriptProjectionCheckpointV1 {
    pub fn validate(&self) -> Result<(), String> {
        validate_projection_identity(
            self.schema.as_str(),
            TRANSCRIPT_CHECKPOINT_SCHEMA_V1,
            self.session_id.as_str(),
            self.projection_version.as_str(),
            self.projection_generation.as_str(),
        )?;
        parse_decimal_u64(
            self.source_high_water.as_str(),
            "transcript checkpoint sourceHighWater",
        )?;
        require_identifier(
            self.frontier_ref.as_str(),
            "transcript checkpoint frontierRef",
        )?;
        require_identifier(
            self.block_index_ref.as_str(),
            "transcript checkpoint blockIndexRef",
        )?;
        let serialized_bytes = serde_json::to_vec(self)
            .map_err(|error| format!("serialize transcript checkpoint failed: {error}"))?
            .len();
        if serialized_bytes > TRANSCRIPT_CHECKPOINT_MAX_BYTES {
            return Err("transcript checkpoint exceeds maximum bytes".to_string());
        }
        Ok(())
    }
}

fn validate_projection_identity(
    schema: &str,
    expected_schema: &str,
    session_id: &str,
    projection_version: &str,
    projection_generation: &str,
) -> Result<(), String> {
    if schema != expected_schema {
        return Err(format!(
            "transcript schema mismatch: expected {expected_schema}, got {schema}"
        ));
    }
    require_identifier(session_id, "transcript sessionId")?;
    if projection_version != TRANSCRIPT_PROJECTION_VERSION_V1 {
        return Err("transcript projectionVersion is unsupported".to_string());
    }
    require_identifier(projection_generation, "transcript projectionGeneration")?;
    Ok(())
}

pub(crate) fn parse_decimal_u64(value: &str, name: &str) -> Result<u64, String> {
    if value.is_empty()
        || (value.len() > 1 && value.starts_with('0'))
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(format!("{name} must be a canonical decimal string"));
    }
    value
        .parse::<u64>()
        .map_err(|_| format!("{name} exceeds the u64 range"))
}

pub(crate) fn require_identifier(value: &str, name: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err(format!("{name} must not be empty"));
    }
    Ok(())
}
