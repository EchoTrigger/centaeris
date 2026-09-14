use std::collections::HashSet;

use centaeris_core::session::transcript::{
    TranscriptBlockBodyV1, TranscriptBlockStatusV1, TranscriptPagePolicyV1, TranscriptPageV1,
    TranscriptPatchV1, TranscriptTextContentV1, TranscriptViewStateV1,
    TRANSCRIPT_PROJECTION_VERSION_V1,
};
use serde::Deserialize;
use serde_json::{json, Value};

use super::TranscriptLine;
use crate::tool_projection::transcript_page_tool_line;

const TRANSCRIPT_PAGE_RPC_SCHEMA_V1: &str = "transcript.page.rpc.v1";
const TRANSCRIPT_PATCH_RPC_SCHEMA_V1: &str = "transcript.patch.rpc.v1";
const TRANSCRIPT_PROJECTION_STALL_MAX_POLLS: usize = 600;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TranscriptPageRpcResponseV1 {
    schema: String,
    projection_version: String,
    projection_generation: String,
    projected_source_high_water: String,
    target_source_high_water: String,
    target_reached: bool,
    page: Option<TranscriptPageV1>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TranscriptPatchRpcResponseV1 {
    schema: String,
    projection_version: String,
    projection_generation: String,
    projected_source_high_water: String,
    target_source_high_water: String,
    target_reached: bool,
    patches: Vec<TranscriptPatchV1>,
    next_source_high_water: String,
    has_more: bool,
}

pub(super) fn request_transcript_page_with<Request, Wait>(
    session_id: &str,
    projection_generation: Option<&str>,
    source_high_water: Option<u64>,
    older_cursor: Option<&str>,
    mut request: Request,
    mut wait: Wait,
) -> Result<TranscriptPageV1, String>
where
    Request: FnMut(Value) -> Result<Value, String>,
    Wait: FnMut(),
{
    let mut fixed_generation = projection_generation.map(str::to_string);
    let mut fixed_source_high_water = source_high_water;
    let mut last_projected_source_high_water = None;
    let mut stalled_polls = 0usize;
    loop {
        let response = request(json!({
            "request": {
                "sessionId": session_id,
                "projectionGeneration": fixed_generation,
                "sourceHighWater": fixed_source_high_water.map(|value| value.to_string()),
                "olderCursor": older_cursor,
            }
        }))?;
        let response = serde_json::from_value::<TranscriptPageRpcResponseV1>(response)
            .map_err(|error| format!("invalid transcript/page response: {error}"))?;
        validate_page_rpc_identity(&response, session_id)?;
        let projected_source_high_water = parse_waterline(
            response.projected_source_high_water.as_str(),
            "projectedSourceHighWater",
        )?;
        let target_source_high_water = parse_waterline(
            response.target_source_high_water.as_str(),
            "targetSourceHighWater",
        )?;
        if projected_source_high_water > target_source_high_water {
            return Err("transcript/page projected waterline exceeds target".to_string());
        }
        if fixed_generation
            .as_deref()
            .is_some_and(|value| value != response.projection_generation)
            || fixed_source_high_water.is_some_and(|value| value != target_source_high_water)
        {
            return Err("transcript/page view identity changed while polling".to_string());
        }
        fixed_generation.get_or_insert(response.projection_generation.clone());
        fixed_source_high_water.get_or_insert(target_source_high_water);

        if response.target_reached {
            let page = response
                .page
                .ok_or_else(|| "transcript/page reached target without a page".to_string())?;
            page.validate(TranscriptPagePolicyV1::default())?;
            if page.session_id != session_id
                || page.projection_generation != response.projection_generation
                || page.source_high_water != response.target_source_high_water
            {
                return Err("transcript/page payload identity mismatch".to_string());
            }
            return Ok(page);
        }
        if response.page.is_some() {
            return Err("transcript/page returned a page before reaching target".to_string());
        }
        if last_projected_source_high_water == Some(projected_source_high_water) {
            stalled_polls = stalled_polls.saturating_add(1);
            if stalled_polls >= TRANSCRIPT_PROJECTION_STALL_MAX_POLLS {
                return Err("transcript/page projection did not make progress".to_string());
            }
        } else {
            stalled_polls = 0;
            last_projected_source_high_water = Some(projected_source_high_water);
        }
        wait();
    }
}

pub(super) fn request_transcript_patches_with<Request, Wait>(
    state: &mut TranscriptPagingState,
    mut request: Request,
    mut wait: Wait,
) -> Result<bool, String>
where
    Request: FnMut(Value) -> Result<Value, String>,
    Wait: FnMut(),
{
    let mut target_source_high_water = None;
    let mut last_projection_progress = None;
    let mut stalled_polls = 0usize;
    let mut changed = false;
    loop {
        let after_source_high_water = state.current_source_high_water();
        let response = request(json!({
            "request": {
                "sessionId": state.session_id(),
                "projectionGeneration": state.projection_generation(),
                "afterSourceHighWater": after_source_high_water.to_string(),
                "throughSourceHighWater": target_source_high_water
                    .map(|value: u64| value.to_string()),
            }
        }))?;
        let response = serde_json::from_value::<TranscriptPatchRpcResponseV1>(response)
            .map_err(|error| format!("invalid transcript/patches response: {error}"))?;
        validate_patch_rpc_identity(&response, state)?;
        let projected_source_high_water = parse_patch_waterline(
            response.projected_source_high_water.as_str(),
            "projectedSourceHighWater",
        )?;
        let response_target_source_high_water = parse_patch_waterline(
            response.target_source_high_water.as_str(),
            "targetSourceHighWater",
        )?;
        let next_source_high_water = parse_patch_waterline(
            response.next_source_high_water.as_str(),
            "nextSourceHighWater",
        )?;
        if target_source_high_water
            .is_some_and(|target| target != response_target_source_high_water)
        {
            return Err("transcript/patches target changed while polling".to_string());
        }
        target_source_high_water.get_or_insert(response_target_source_high_water);
        if projected_source_high_water > response_target_source_high_water
            || next_source_high_water < after_source_high_water
            || next_source_high_water > projected_source_high_water
        {
            return Err("transcript/patches returned invalid waterline progress".to_string());
        }
        for patch in response.patches {
            let patch_source_high_water =
                parse_patch_waterline(patch.source_high_water.as_str(), "patch.sourceHighWater")?;
            if patch_source_high_water <= after_source_high_water
                || patch_source_high_water > next_source_high_water
            {
                return Err(
                    "transcript/patches returned a patch outside the response range".to_string(),
                );
            }
            state.apply_patch(patch)?;
            changed = true;
        }
        state.advance_current_source_high_water(next_source_high_water)?;

        if response.target_reached && !response.has_more {
            if next_source_high_water != response_target_source_high_water {
                return Err("transcript/patches reached target without consuming it".to_string());
            }
            return Ok(changed);
        }
        let progress = (projected_source_high_water, next_source_high_water);
        if last_projection_progress == Some(progress) {
            stalled_polls = stalled_polls.saturating_add(1);
            if stalled_polls >= TRANSCRIPT_PROJECTION_STALL_MAX_POLLS {
                return Err("transcript/patches projection did not make progress".to_string());
            }
        } else {
            stalled_polls = 0;
            last_projection_progress = Some(progress);
        }
        wait();
    }
}

fn validate_page_rpc_identity(
    response: &TranscriptPageRpcResponseV1,
    session_id: &str,
) -> Result<(), String> {
    if response.schema != TRANSCRIPT_PAGE_RPC_SCHEMA_V1
        || response.projection_version != TRANSCRIPT_PROJECTION_VERSION_V1
        || response.projection_generation.trim().is_empty()
        || session_id.trim().is_empty()
    {
        return Err("transcript/page response identity is invalid".to_string());
    }
    Ok(())
}

fn validate_patch_rpc_identity(
    response: &TranscriptPatchRpcResponseV1,
    state: &TranscriptPagingState,
) -> Result<(), String> {
    if response.schema != TRANSCRIPT_PATCH_RPC_SCHEMA_V1
        || response.projection_version != TRANSCRIPT_PROJECTION_VERSION_V1
        || response.projection_generation != state.projection_generation
    {
        return Err("transcript/patches response identity is invalid".to_string());
    }
    Ok(())
}

fn parse_waterline(value: &str, field: &str) -> Result<u64, String> {
    value
        .parse::<u64>()
        .map_err(|_| format!("transcript/page {field} is invalid"))
}

fn parse_patch_waterline(value: &str, field: &str) -> Result<u64, String> {
    value
        .parse::<u64>()
        .map_err(|_| format!("transcript/patches {field} is invalid"))
}

#[derive(Clone, Debug)]
pub(super) struct TranscriptPagingState {
    view: TranscriptViewStateV1,
    session_id: String,
    projection_generation: String,
    base_source_high_water: u64,
    current_source_high_water: u64,
    older_cursor: Option<String>,
    tail_block_ids: HashSet<String>,
    tail_older_cursor: Option<String>,
}

impl TranscriptPagingState {
    pub(super) fn open(page: TranscriptPageV1) -> Result<Self, String> {
        let session_id = page.session_id.clone();
        let projection_generation = page.projection_generation.clone();
        let base_source_high_water = page
            .source_high_water
            .parse::<u64>()
            .map_err(|_| "transcript page sourceHighWater is invalid".to_string())?;
        let older_cursor = page.older_cursor.clone();
        let tail_older_cursor = older_cursor.clone();
        let tail_block_ids = page
            .blocks
            .iter()
            .map(|block| block.block_id.clone())
            .collect();
        let view = TranscriptViewStateV1::open("tui-session-view".to_string(), page)?;
        Ok(Self {
            view,
            session_id,
            projection_generation,
            base_source_high_water,
            current_source_high_water: base_source_high_water,
            older_cursor,
            tail_block_ids,
            tail_older_cursor,
        })
    }

    pub(super) fn apply_older_page(&mut self, page: TranscriptPageV1) -> Result<(), String> {
        let next_older_cursor = page.older_cursor.clone();
        self.view.apply_page(page)?;
        self.older_cursor = next_older_cursor;
        Ok(())
    }

    pub(super) fn apply_patch(&mut self, patch: TranscriptPatchV1) -> Result<(), String> {
        let next_source_high_water = patch
            .source_high_water
            .parse::<u64>()
            .map_err(|_| "transcript patch sourceHighWater is invalid".to_string())?;
        self.view.apply_patch(patch)?;
        self.current_source_high_water = next_source_high_water;
        Ok(())
    }

    pub(super) fn projection_generation(&self) -> &str {
        self.projection_generation.as_str()
    }

    pub(super) fn session_id(&self) -> &str {
        self.session_id.as_str()
    }

    pub(super) fn source_high_water(&self) -> u64 {
        self.base_source_high_water
    }

    pub(super) fn current_source_high_water(&self) -> u64 {
        self.current_source_high_water
    }

    fn advance_current_source_high_water(&mut self, next: u64) -> Result<(), String> {
        if next < self.current_source_high_water {
            return Err("transcript patch waterline moved backward".to_string());
        }
        self.current_source_high_water = next;
        Ok(())
    }

    pub(super) fn older_cursor(&self) -> Option<&str> {
        self.older_cursor.as_deref()
    }

    pub(super) fn release_loaded_history(&mut self) -> usize {
        let removed = self.view.release_loaded_history(&self.tail_block_ids);
        self.older_cursor = self.tail_older_cursor.clone();
        removed
    }

    pub(super) fn materialize_history(&self, live_overlay_active: bool) -> Vec<TranscriptLine> {
        self.view
            .visible_blocks()
            .into_iter()
            .filter(|block| {
                !live_overlay_active
                    || block
                        .order_key
                        .source_sequence_value()
                        .is_ok_and(|sequence| sequence <= self.base_source_high_water)
            })
            .map(materialize_block)
            .collect()
    }
}

fn materialize_block(
    block: &centaeris_core::session::transcript::TranscriptBlockV1,
) -> TranscriptLine {
    match &block.body {
        TranscriptBlockBodyV1::UserText { content } => {
            TranscriptLine::User(materialize_text(content))
        }
        TranscriptBlockBodyV1::AssistantText { content, .. } => {
            TranscriptLine::Summary(materialize_text(content))
        }
        TranscriptBlockBodyV1::Reasoning { content, .. } => {
            TranscriptLine::Supplement(materialize_text(content))
        }
        TranscriptBlockBodyV1::Tool {
            call_id,
            tool_name,
            status,
            summary,
            summary_ref,
            output_ref,
        } => TranscriptLine::Tool(transcript_page_tool_line(
            call_id,
            tool_name,
            *status,
            summary.clone().unwrap_or_else(|| {
                summary_ref
                    .as_ref()
                    .map(|reference| referenced_content_label(reference.byte_length.as_str()))
                    .unwrap_or_else(|| tool_name.clone())
            }),
            output_ref
                .as_ref()
                .map(|reference| reference.byte_length.as_str()),
        )),
        TranscriptBlockBodyV1::Notice {
            notice_type,
            content,
            status,
        } => {
            let text = materialize_text(content);
            if *status == TranscriptBlockStatusV1::Failed {
                TranscriptLine::Error(text)
            } else {
                TranscriptLine::Supplement(format!("{notice_type}: {text}"))
            }
        }
    }
}

fn materialize_text(content: &TranscriptTextContentV1) -> String {
    content
        .inline_content
        .clone()
        .or_else(|| {
            content
                .source_ref
                .as_ref()
                .map(|reference| referenced_content_label(reference.byte_length.as_str()))
        })
        .unwrap_or_default()
}

fn referenced_content_label(byte_length: &str) -> String {
    format!("[referenced content: {byte_length} bytes]")
}
