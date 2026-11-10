use centaeris_core::session::transcript::TranscriptPatchV1;
use serde::{Deserialize, Serialize};

#[cfg_attr(feature = "contract-schema", derive(schemars::JsonSchema))]
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct PageRequest {
    pub(super) schema: String,
    pub(super) session_id: String,
    pub(super) projection_version: String,
    pub(super) projection_generation: Option<String>,
    pub(super) source_high_water: String,
    pub(super) older_cursor: Option<String>,
}

#[cfg_attr(feature = "contract-schema", derive(schemars::JsonSchema))]
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct PatchRequest {
    pub(super) schema: String,
    pub(super) session_id: String,
    pub(super) projection_version: String,
    pub(super) projection_generation: String,
    pub(super) after_source_high_water: String,
    pub(super) through_source_high_water: String,
}

#[cfg_attr(feature = "contract-schema", derive(schemars::JsonSchema))]
#[derive(Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct PatchPage {
    pub(super) schema: &'static str,
    pub(super) session_id: String,
    pub(super) projection_version: String,
    pub(super) projection_generation: String,
    pub(super) through_source_high_water: String,
    pub(super) patches: Vec<TranscriptPatchV1>,
    pub(super) next_source_high_water: String,
    pub(super) has_more: bool,
}

#[cfg_attr(feature = "contract-schema", derive(schemars::JsonSchema))]
#[derive(Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct ProjectionNotReady<'a> {
    pub(super) error: &'static str,
    pub(super) projection_generation: &'a str,
    pub(super) source_high_water: &'a str,
    pub(super) projected_high_water: String,
}

#[cfg_attr(feature = "contract-schema", derive(schemars::JsonSchema))]
#[derive(Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct ErrorResponse {
    pub(super) error: ErrorCode,
}

#[cfg_attr(feature = "contract-schema", derive(schemars::JsonSchema))]
#[derive(Serialize)]
pub(super) enum ErrorCode {
    #[serde(rename = "transcript_content_request_invalid")]
    ContentRequestInvalid,
    #[serde(rename = "transcript_content_unavailable")]
    ContentUnavailable,
    #[serde(rename = "transcript_page_request_invalid")]
    PageRequestInvalid,
    #[serde(rename = "transcript_patch_request_invalid")]
    PatchRequestInvalid,
    #[serde(rename = "transcript_view_invalidated")]
    ViewInvalidated,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn page_request_preserves_optional_fields_and_rejects_unknown_names() {
        let request = json!({"schema":"runtime.transcript.page.read.v1", "sessionId":"s",
            "projectionVersion":"v", "sourceHighWater":"0"});
        let decoded: PageRequest = serde_json::from_value(request.clone()).unwrap();
        assert!(decoded.projection_generation.is_none());
        assert!(decoded.older_cursor.is_none());
        for (key, value) in [("olderCursor", json!(false)), ("session_id", json!("s"))] {
            let mut invalid = request.clone();
            invalid[key] = value;
            assert!(serde_json::from_value::<PageRequest>(invalid).is_err());
        }
        let mut invalid = request;
        invalid.as_object_mut().unwrap().remove("sourceHighWater");
        assert!(serde_json::from_value::<PageRequest>(invalid).is_err());
    }

    #[test]
    fn patch_request_requires_generation_and_exact_names() {
        let request = json!({"schema":"runtime.transcript.patch.read.v1", "sessionId":"s",
            "projectionVersion":"v", "projectionGeneration":"g",
            "afterSourceHighWater":"0", "throughSourceHighWater":"1"});
        assert!(serde_json::from_value::<PatchRequest>(request.clone()).is_ok());
        for key in [
            "projectionGeneration",
            "afterSourceHighWater",
            "throughSourceHighWater",
        ] {
            let mut invalid = request.clone();
            invalid.as_object_mut().unwrap().remove(key);
            assert!(serde_json::from_value::<PatchRequest>(invalid).is_err());
        }
        let mut invalid = request;
        invalid["unknown"] = json!(true);
        assert!(serde_json::from_value::<PatchRequest>(invalid).is_err());
    }

    #[test]
    fn error_wire_values_stay_exact() {
        for (error, expected) in [
            (
                ErrorCode::ContentRequestInvalid,
                "transcript_content_request_invalid",
            ),
            (
                ErrorCode::ContentUnavailable,
                "transcript_content_unavailable",
            ),
            (
                ErrorCode::PageRequestInvalid,
                "transcript_page_request_invalid",
            ),
            (
                ErrorCode::PatchRequestInvalid,
                "transcript_patch_request_invalid",
            ),
            (ErrorCode::ViewInvalidated, "transcript_view_invalidated"),
        ] {
            assert_eq!(
                serde_json::to_value(ErrorResponse { error }).unwrap(),
                json!({"error":expected})
            );
        }
        assert_eq!(
            serde_json::to_value(ProjectionNotReady {
                error: "transcript_projection_not_ready",
                projection_generation: "g",
                source_high_water: "2",
                projected_high_water: "1".into(),
            })
            .unwrap(),
            json!({"error":"transcript_projection_not_ready",
            "projectionGeneration":"g", "sourceHighWater":"2", "projectedHighWater":"1"})
        );
    }
}
