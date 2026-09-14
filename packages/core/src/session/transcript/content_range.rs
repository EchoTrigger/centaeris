use serde::{Deserialize, Serialize};

use super::{parse_decimal_u64, require_identifier, TRANSCRIPT_PROJECTION_VERSION_V1};

pub const TRANSCRIPT_CONTENT_RANGE_REQUEST_SCHEMA_V1: &str = "transcript.content.range.read.v1";
pub const TRANSCRIPT_CONTENT_RANGE_SCHEMA_V1: &str = "transcript.content.range.v1";
pub const TRANSCRIPT_CONTENT_RANGE_MAX_BYTES: usize = 64 * 1024;
pub const TRANSCRIPT_CONTENT_RANGE_SERIALIZED_MAX_BYTES: usize =
    2 * TRANSCRIPT_CONTENT_RANGE_MAX_BYTES;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TranscriptContentRangeReadRequestV1 {
    pub schema: String,
    pub session_id: String,
    pub projection_version: String,
    pub projection_generation: String,
    pub ref_id: String,
    pub revision: String,
    pub byte_length: String,
    pub offset: String,
    pub max_bytes: u32,
}

impl TranscriptContentRangeReadRequestV1 {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != TRANSCRIPT_CONTENT_RANGE_REQUEST_SCHEMA_V1 {
            return Err("transcript content range request schema is unsupported".to_string());
        }
        require_identifier(
            self.session_id.as_str(),
            "transcript content range sessionId",
        )?;
        if self.projection_version != TRANSCRIPT_PROJECTION_VERSION_V1 {
            return Err("transcript content range projectionVersion is unsupported".to_string());
        }
        require_identifier(
            self.projection_generation.as_str(),
            "transcript content range projectionGeneration",
        )?;
        require_identifier(self.ref_id.as_str(), "transcript content range refId")?;
        parse_decimal_u64(self.revision.as_str(), "transcript content range revision")?;
        let byte_length = parse_decimal_u64(
            self.byte_length.as_str(),
            "transcript content range byteLength",
        )?;
        let offset = parse_decimal_u64(self.offset.as_str(), "transcript content range offset")?;
        if offset > byte_length {
            return Err("transcript content range offset exceeds byteLength".to_string());
        }
        if self.max_bytes == 0 || self.max_bytes as usize > TRANSCRIPT_CONTENT_RANGE_MAX_BYTES {
            return Err("transcript content range maxBytes is invalid".to_string());
        }
        if let Some(call_id) = self.ref_id.strip_prefix("tool-output:") {
            require_identifier(call_id, "transcript tool output callId")?;
            if self.revision != "2" {
                return Err("transcript content range revision is unsupported".to_string());
            }
        } else {
            let (_, field) = self.event_reference()?;
            let revision = if field == "summary" { "2" } else { "1" };
            if self.revision != revision {
                return Err("transcript content range revision is unsupported".to_string());
            }
        }
        Ok(())
    }

    pub fn event_reference(&self) -> Result<(&str, &str), String> {
        let (event_id, field) = self
            .ref_id
            .strip_prefix("session-event:")
            .and_then(|value| value.rsplit_once(':'))
            .ok_or_else(|| "transcript content range refId is unsupported".to_string())?;
        require_identifier(event_id, "transcript content eventId")?;
        if !matches!(
            field,
            "text" | "modelMarkdown" | "displayTarget" | "summary" | "message"
        ) {
            return Err("transcript content range field is unsupported".to_string());
        }
        Ok((event_id, field))
    }
}

/// Resolves only text actually exposed by the transcript projection. Hosts select the
/// authorized source record; Core owns the reference, revision and field semantics.
pub fn transcript_event_content_range(
    request: &TranscriptContentRangeReadRequestV1,
    event: &crate::session::SessionLogRecord,
) -> Result<TranscriptContentRangeV1, String> {
    use crate::session::SessionRecordType;
    request.validate()?;
    crate::session::validate_event_shape(event)?;
    let (event_id, field) = request.event_reference()?;
    let expected_field = match event.event_type {
        SessionRecordType::UserMessage | SessionRecordType::ReasoningBlock => "text",
        SessionRecordType::AssistantMessage => "modelMarkdown",
        SessionRecordType::ToolCall => "displayTarget",
        SessionRecordType::ToolResult => "summary",
        SessionRecordType::PhaseEvent => "message",
        _ => return Err("transcript source has no visible text".to_string()),
    };
    if event.session_id != request.session_id
        || event.event_id != event_id
        || field != expected_field
    {
        return Err("transcript content reference does not match its source".to_string());
    }
    let text = event
        .payload
        .get(field)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "transcript source text is missing".to_string())?;
    if text.len().to_string() != request.byte_length {
        return Err("transcript content reference is stale".to_string());
    }
    let offset = request
        .offset
        .parse::<usize>()
        .map_err(|_| "transcript offset exceeds usize")?;
    let (end, content) = transcript_utf8_range(text, offset, request.max_bytes as usize)?;
    let response = TranscriptContentRangeV1 {
        schema: TRANSCRIPT_CONTENT_RANGE_SCHEMA_V1.to_string(),
        session_id: request.session_id.clone(),
        projection_version: request.projection_version.clone(),
        projection_generation: request.projection_generation.clone(),
        ref_id: request.ref_id.clone(),
        revision: request.revision.clone(),
        byte_length: request.byte_length.clone(),
        start_offset: request.offset.clone(),
        end_offset: end.to_string(),
        content: content.to_string(),
        has_more: end < text.len(),
    };
    response.validate()?;
    Ok(response)
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TranscriptContentRangeV1 {
    pub schema: String,
    pub session_id: String,
    pub projection_version: String,
    pub projection_generation: String,
    pub ref_id: String,
    pub revision: String,
    pub byte_length: String,
    pub start_offset: String,
    pub end_offset: String,
    pub content: String,
    pub has_more: bool,
}

impl TranscriptContentRangeV1 {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != TRANSCRIPT_CONTENT_RANGE_SCHEMA_V1 {
            return Err("transcript content range schema is unsupported".to_string());
        }
        require_identifier(
            self.session_id.as_str(),
            "transcript content range sessionId",
        )?;
        if self.projection_version != TRANSCRIPT_PROJECTION_VERSION_V1 {
            return Err("transcript content range projectionVersion is unsupported".to_string());
        }
        require_identifier(
            self.projection_generation.as_str(),
            "transcript content range projectionGeneration",
        )?;
        require_identifier(self.ref_id.as_str(), "transcript content range refId")?;
        parse_decimal_u64(self.revision.as_str(), "transcript content range revision")?;
        let byte_length = parse_decimal_u64(
            self.byte_length.as_str(),
            "transcript content range byteLength",
        )?;
        let start = parse_decimal_u64(
            self.start_offset.as_str(),
            "transcript content range startOffset",
        )?;
        let end = parse_decimal_u64(
            self.end_offset.as_str(),
            "transcript content range endOffset",
        )?;
        if start > end || end > byte_length || end - start != self.content.len() as u64 {
            return Err("transcript content range bounds are invalid".to_string());
        }
        if self.content.len() > TRANSCRIPT_CONTENT_RANGE_MAX_BYTES
            || self.has_more != (end < byte_length)
        {
            return Err("transcript content range budget or continuation is invalid".to_string());
        }
        Ok(())
    }
}

pub fn transcript_utf8_range(
    content: &str,
    offset: usize,
    max_bytes: usize,
) -> Result<(usize, &str), String> {
    if max_bytes == 0 || max_bytes > TRANSCRIPT_CONTENT_RANGE_MAX_BYTES {
        return Err("transcript content range maxBytes is invalid".to_string());
    }
    if offset > content.len() || !content.is_char_boundary(offset) {
        return Err("transcript content range offset is not a UTF-8 boundary".to_string());
    }
    let mut end = offset.saturating_add(max_bytes).min(content.len());
    while end > offset && !content.is_char_boundary(end) {
        end -= 1;
    }
    Ok((end, &content[offset..end]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_ranges_are_bounded_and_continue_on_scalar_boundaries() {
        let text = format!(
            "{}界tail",
            "a".repeat(TRANSCRIPT_CONTENT_RANGE_MAX_BYTES - 1)
        );
        let (end, first) = transcript_utf8_range(&text, 0, TRANSCRIPT_CONTENT_RANGE_MAX_BYTES)
            .expect("first range");
        assert_eq!(first.len(), TRANSCRIPT_CONTENT_RANGE_MAX_BYTES - 1);
        assert!(text.is_char_boundary(end));
        let (final_end, second) =
            transcript_utf8_range(&text, end, TRANSCRIPT_CONTENT_RANGE_MAX_BYTES)
                .expect("second range");
        assert_eq!(second, "界tail");
        assert_eq!(final_end, text.len());
        assert!(transcript_utf8_range(&text, end + 1, 1).is_err());
    }

    #[test]
    fn content_range_request_rejects_unknown_fields_and_oversized_ranges() {
        let request = TranscriptContentRangeReadRequestV1 {
            schema: TRANSCRIPT_CONTENT_RANGE_REQUEST_SCHEMA_V1.to_string(),
            session_id: "session-1".to_string(),
            projection_version: TRANSCRIPT_PROJECTION_VERSION_V1.to_string(),
            projection_generation: "generation-1".to_string(),
            ref_id: "tool-output:call-1".to_string(),
            revision: "2".to_string(),
            byte_length: "12".to_string(),
            offset: "0".to_string(),
            max_bytes: TRANSCRIPT_CONTENT_RANGE_MAX_BYTES as u32,
        };
        request.validate().expect("valid request");
        let mut unsupported = request.clone();
        unsupported.ref_id = "session-event:event-1:privateField".to_string();
        assert!(unsupported.validate().is_err());
        let mut oversized = request;
        oversized.max_bytes = TRANSCRIPT_CONTENT_RANGE_MAX_BYTES as u32 + 1;
        assert!(oversized.validate().is_err());
    }

    #[test]
    fn long_answer_reference_is_a_readable_content_range() {
        let request = TranscriptContentRangeReadRequestV1 {
            schema: TRANSCRIPT_CONTENT_RANGE_REQUEST_SCHEMA_V1.to_string(),
            session_id: "session-1".to_string(),
            projection_version: TRANSCRIPT_PROJECTION_VERSION_V1.to_string(),
            projection_generation: "generation-1".to_string(),
            ref_id: "session-event:event:answer:modelMarkdown".to_string(),
            revision: "1".to_string(),
            byte_length: "70000".to_string(),
            offset: "0".to_string(),
            max_bytes: TRANSCRIPT_CONTENT_RANGE_MAX_BYTES as u32,
        };
        request
            .validate()
            .expect("projected answer reference must be readable");
    }
}
