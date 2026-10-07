use centaeris_core::session::transcript::{
    TranscriptPagePolicyV1, TranscriptPageReadRequestV1, TranscriptPageV1,
    TranscriptPatchReadRequestV1, TranscriptProjectionGenerationStorePortV1,
    TranscriptProjectionStorePort, TRANSCRIPT_PAGE_SCHEMA_V1,
    TRANSCRIPT_PROJECTION_GENERATION_INVALIDATED, TRANSCRIPT_PROJECTION_VERSION_V1,
};

use crate::postgres_store::PostgresRuntimeStore;

const PAGE_REQUEST_SCHEMA: &str = "runtime.transcript.page.read.v1";
const PATCH_REQUEST_SCHEMA: &str = "runtime.transcript.patch.read.v1";
const PATCH_PAGE_SCHEMA: &str = "transcript.patch.page.v1";

mod wire;
use wire::*;

pub(crate) fn handle(
    path: &str,
    body: &[u8],
    store: &PostgresRuntimeStore,
) -> Option<Result<(u16, Vec<u8>), String>> {
    match path {
        "/internal/transcript/page" => Some(page(body, store)),
        "/internal/transcript/patches" => Some(patches(body, store)),
        "/internal/transcript/content" => Some(content(body, store)),
        _ => None,
    }
}

fn content(body: &[u8], store: &PostgresRuntimeStore) -> Result<(u16, Vec<u8>), String> {
    let request = match serde_json::from_slice::<
        centaeris_core::session::transcript::TranscriptContentRangeReadRequestV1,
    >(body)
    {
        Ok(request) if request.validate().is_ok() && request.event_reference().is_ok() => request,
        _ => return json_error(400, ErrorCode::ContentRequestInvalid),
    };
    let response = match store.read_transcript_event_content(&request) {
        Ok(response) => response,
        Err(_) => return json_error(409, ErrorCode::ContentUnavailable),
    };
    serde_json::to_vec(&response)
        .map(|body| (200, body))
        .map_err(|error| format!("encode transcript content failed: {error}"))
}

fn page(body: &[u8], store: &PostgresRuntimeStore) -> Result<(u16, Vec<u8>), String> {
    let wire = match serde_json::from_slice::<PageRequest>(body) {
        Ok(wire)
            if wire.schema == PAGE_REQUEST_SCHEMA
                && wire.projection_version == TRANSCRIPT_PROJECTION_VERSION_V1 =>
        {
            wire
        }
        Ok(_) | Err(_) => return json_error(400, ErrorCode::PageRequestInvalid),
    };
    let opens_current_view = wire.projection_generation.is_none();
    let current =
        store.load_or_initialize_current_transcript_generation(wire.session_id.as_str())?;
    if wire
        .projection_generation
        .as_deref()
        .is_some_and(|generation| generation != current.projection_generation)
    {
        return json_error(409, ErrorCode::ViewInvalidated);
    }
    let request = TranscriptPageReadRequestV1 {
        session_id: wire.session_id,
        projection_version: wire.projection_version,
        projection_generation: current.projection_generation,
        source_high_water: wire.source_high_water,
        older_cursor: wire.older_cursor,
        policy: TranscriptPagePolicyV1::default(),
    };
    if request.validate().is_err() {
        return json_error(400, ErrorCode::PageRequestInvalid);
    }
    if request.source_high_water == "0" {
        let page = TranscriptPageV1 {
            schema: TRANSCRIPT_PAGE_SCHEMA_V1.to_string(),
            session_id: request.session_id,
            projection_version: request.projection_version,
            projection_generation: request.projection_generation,
            source_high_water: "0".to_string(),
            blocks: Vec::new(),
            older_cursor: None,
            has_older: false,
            resume_cursors: Vec::new(),
        };
        page.validate(TranscriptPagePolicyV1::default())?;
        return serde_json::to_vec(&page)
            .map(|body| (200, body))
            .map_err(|error| format!("encode empty transcript page failed: {error}"));
    }
    if let Some(response) = projection_gate(
        store,
        request.session_id.as_str(),
        request.projection_generation.as_str(),
        request.source_high_water.as_str(),
        opens_current_view,
    )? {
        return Ok(response);
    }
    let result = match store.load_current_transcript_page(request) {
        Ok(result) => result,
        Err(error) if is_invalidated_store_read(&error) => {
            return json_error(409, ErrorCode::ViewInvalidated)
        }
        Err(error) => return Err(error),
    };
    serde_json::to_vec(&result.page)
        .map(|body| (200, body))
        .map_err(|error| format!("encode transcript page failed: {error}"))
}

fn patches(body: &[u8], store: &PostgresRuntimeStore) -> Result<(u16, Vec<u8>), String> {
    let wire = match serde_json::from_slice::<PatchRequest>(body) {
        Ok(wire)
            if wire.schema == PATCH_REQUEST_SCHEMA
                && wire.projection_version == TRANSCRIPT_PROJECTION_VERSION_V1 =>
        {
            wire
        }
        Ok(_) | Err(_) => return json_error(400, ErrorCode::PatchRequestInvalid),
    };
    let request = TranscriptPatchReadRequestV1 {
        session_id: wire.session_id,
        projection_version: wire.projection_version,
        projection_generation: wire.projection_generation,
        after_source_high_water: wire.after_source_high_water,
        through_source_high_water: wire.through_source_high_water,
    };
    if request.validate().is_err() {
        return json_error(400, ErrorCode::PatchRequestInvalid);
    }
    let current =
        store.load_or_initialize_current_transcript_generation(request.session_id.as_str())?;
    if current.projection_generation != request.projection_generation {
        return json_error(409, ErrorCode::ViewInvalidated);
    }
    if request.after_source_high_water == "0" && request.through_source_high_water == "0" {
        let page = PatchPage {
            schema: PATCH_PAGE_SCHEMA,
            session_id: request.session_id,
            projection_version: request.projection_version,
            projection_generation: request.projection_generation,
            through_source_high_water: "0".to_string(),
            patches: Vec::new(),
            next_source_high_water: "0".to_string(),
            has_more: false,
        };
        return serde_json::to_vec(&page)
            .map(|body| (200, body))
            .map_err(|error| format!("encode empty transcript patch page failed: {error}"));
    }
    if let Some(response) = projection_gate(
        store,
        request.session_id.as_str(),
        request.projection_generation.as_str(),
        request.through_source_high_water.as_str(),
        false,
    )? {
        return Ok(response);
    }
    let result = match store.load_current_transcript_patches(request.clone()) {
        Ok(result) => result,
        Err(error) if is_invalidated_store_read(&error) => {
            return json_error(409, ErrorCode::ViewInvalidated)
        }
        Err(error) => return Err(error),
    };
    let page = PatchPage {
        schema: PATCH_PAGE_SCHEMA,
        session_id: request.session_id,
        projection_version: request.projection_version,
        projection_generation: request.projection_generation,
        through_source_high_water: request.through_source_high_water,
        patches: result.patches,
        next_source_high_water: result.next_source_high_water,
        has_more: result.has_more,
    };
    serde_json::to_vec(&page)
        .map(|body| (200, body))
        .map_err(|error| format!("encode transcript patch page failed: {error}"))
}

fn projection_gate(
    store: &PostgresRuntimeStore,
    session_id: &str,
    generation: &str,
    requested_high_water: &str,
    schedule_rebuild: bool,
) -> Result<Option<(u16, Vec<u8>)>, String> {
    let requested = requested_high_water
        .parse::<u64>()
        .map_err(|_| "validated transcript waterline failed to parse".to_string())?;
    let mut head = store.load_transcript_projection_head(session_id, generation)?;
    if let Some(head) = &head {
        if head.invalidation_reason.is_some() {
            if schedule_rebuild {
                store.schedule_transcript_generation_rebuild(session_id, requested, generation)?;
            }
            return json_error(409, ErrorCode::ViewInvalidated).map(Some);
        }
    }
    let mut projected = head.map_or_else(|| "0".to_string(), |head| head.source_high_water);
    let projected_value = projected
        .parse::<u64>()
        .map_err(|_| "stored transcript waterline failed to parse".to_string())?;
    if projected_value < requested {
        if let Err(error) = store.catch_up_transcript_projection(session_id, generation, requested)
        {
            let invalidated = store
                .load_transcript_projection_head(session_id, generation)?
                .is_some_and(|head| head.invalidation_reason.is_some());
            if invalidated {
                if schedule_rebuild {
                    store.schedule_transcript_generation_rebuild(
                        session_id, requested, generation,
                    )?;
                }
                return json_error(409, ErrorCode::ViewInvalidated).map(Some);
            }
            return Err(error);
        }
        head = store.load_transcript_projection_head(session_id, generation)?;
        if let Some(head) = &head {
            if head.invalidation_reason.is_some() {
                return json_error(409, ErrorCode::ViewInvalidated).map(Some);
            }
        }
        projected = head.map_or_else(|| "0".to_string(), |head| head.source_high_water);
    }
    let projected_value = projected
        .parse::<u64>()
        .map_err(|_| "stored transcript waterline failed to parse".to_string())?;
    if projected_value < requested {
        let response = ProjectionNotReady {
            error: "transcript_projection_not_ready",
            projection_generation: generation,
            source_high_water: requested_high_water,
            projected_high_water: projected,
        };
        return serde_json::to_vec(&response)
            .map(|body| Some((409, body)))
            .map_err(|error| format!("encode transcript projection progress failed: {error}"));
    }
    Ok(None)
}

fn json_error(status: u16, error: ErrorCode) -> Result<(u16, Vec<u8>), String> {
    serde_json::to_vec(&ErrorResponse { error })
        .map(|body| (status, body))
        .map_err(|failure| format!("encode transcript error failed: {failure}"))
}

fn is_invalidated_store_read(error: &str) -> bool {
    error == TRANSCRIPT_PROJECTION_GENERATION_INVALIDATED
        || error.starts_with("transcript projection is invalidated:")
}

#[cfg(test)]
mod tests {
    use super::is_invalidated_store_read;

    #[test]
    fn store_generation_races_map_to_the_public_invalidation_contract() {
        assert!(is_invalidated_store_read(
            "transcript_projection_generation_invalidated"
        ));
        assert!(is_invalidated_store_read(
            "transcript projection is invalidated: tombstone_requires_rebuild"
        ));
        assert!(!is_invalidated_store_read(
            "transcript page sourceHighWater is not fully projected"
        ));
    }
}
