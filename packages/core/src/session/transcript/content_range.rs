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
        if !self.ref_id.starts_with("tool-output:") {
            return Err("transcript content range refId is unsupported".to_string());
        }
        if self.revision != "2" {
            return Err("transcript content range revision is unsupported".to_string());
        }
        Ok(())
    }
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
    fn content_range_request_is_strict_and_tool_output_scoped() {
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
        unsupported.ref_id = "session-event:event-1:summary".to_string();
        assert!(unsupported.validate().is_err());
        let mut oversized = request;
        oversized.max_bytes = TRANSCRIPT_CONTENT_RANGE_MAX_BYTES as u32 + 1;
        assert!(oversized.validate().is_err());
    }
}
