use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use centaeris_core::session::transcript::{
    rebuild_transcript_generation_v1, TranscriptCheckpointRefsV1,
    TranscriptContentRangeReadRequestV1, TranscriptContentRangeV1,
    TranscriptGenerationRebuildRequestV1, TranscriptGenerationRebuildSourcePortV1,
    TranscriptPagePolicyV1, TranscriptPageReadRequestV1, TranscriptPageV1,
    TranscriptPatchReadRequestV1, TranscriptPatchV1, TranscriptProjectionGenerationRotationV1,
    TranscriptProjectionGenerationStorePortV1, TranscriptProjectionStorePort,
    TranscriptProjectorV1, TranscriptRebuildLedgerFactV1, TranscriptRebuildProjectionFactV1,
    TranscriptResumeCursorV1, TRANSCRIPT_CONTENT_RANGE_SCHEMA_V1, TRANSCRIPT_PAGE_SCHEMA_V1,
    TRANSCRIPT_PROJECTION_GENERATION_INVALIDATED, TRANSCRIPT_PROJECTION_SLICE_MAX_EVENTS,
    TRANSCRIPT_PROJECTION_SLICE_MAX_MICROS, TRANSCRIPT_PROJECTION_VERSION_V1,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{agent_runtime, message_log, user_data_layout};

const TRANSCRIPT_SOURCE_STREAM_ID: &str = "session-jsonl.v1";
const LOCAL_TRANSCRIPT_PROJECTION_GENERATION_V1: &str = "local-transcript-v1";
const TRANSCRIPT_PAGE_RPC_SCHEMA_V1: &str = "transcript.page.rpc.v1";
const TRANSCRIPT_PATCH_RPC_SCHEMA_V1: &str = "transcript.patch.rpc.v1";
static TRANSCRIPT_BACKFILLS: OnceLock<Mutex<HashMap<String, TranscriptBackgroundJobV1>>> =
    OnceLock::new();

#[derive(Clone, Debug, PartialEq, Eq)]
struct TranscriptBackgroundJobV1 {
    target_source_high_water: u64,
    projection_generation: String,
    expected_current_generation: Option<String>,
    rebuild: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TranscriptGenerationSelectionV1 {
    projection_generation: String,
    expected_current_generation: Option<String>,
    published: bool,
    rebuild: bool,
}

struct LocalTranscriptRebuildSourceV1<'a> {
    path: &'a Path,
}

impl TranscriptGenerationRebuildSourcePortV1 for LocalTranscriptRebuildSourceV1<'_> {
    fn scan_transcript_rebuild_ledger(
        &self,
        request: &TranscriptGenerationRebuildRequestV1,
        visitor: &mut dyn FnMut(TranscriptRebuildLedgerFactV1) -> Result<(), String>,
    ) -> Result<(), String> {
        message_log::scan_transcript_rebuild_ledger(self.path, request, visitor)
    }

    fn scan_transcript_rebuild_projection(
        &self,
        request: &TranscriptGenerationRebuildRequestV1,
        visitor: &mut dyn FnMut(TranscriptRebuildProjectionFactV1) -> Result<(), String>,
    ) -> Result<(), String> {
        message_log::scan_transcript_rebuild_projection(self.path, request, visitor)
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct TranscriptPageRpcRequestV1 {
    session_id: String,
    projection_generation: Option<String>,
    source_high_water: Option<String>,
    older_cursor: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TranscriptPageRpcResponseV1 {
    schema: &'static str,
    projection_version: &'static str,
    projection_generation: String,
    projected_source_high_water: String,
    target_source_high_water: String,
    target_reached: bool,
    page: Option<TranscriptPageV1>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct TranscriptPatchRpcRequestV1 {
    session_id: String,
    projection_generation: Option<String>,
    after_source_high_water: String,
    through_source_high_water: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TranscriptPatchRpcResponseV1 {
    schema: &'static str,
    projection_version: &'static str,
    projection_generation: String,
    projected_source_high_water: String,
    target_source_high_water: String,
    target_reached: bool,
    patches: Vec<TranscriptPatchV1>,
    next_source_high_water: String,
    has_more: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TranscriptBackfillSliceResultV1 {
    projected_source_high_water: u64,
    projected_events: usize,
    source_bytes: usize,
    target_reached: bool,
    oversized_record: bool,
}

pub(crate) fn page(
    request: TranscriptPageRpcRequestV1,
) -> Result<TranscriptPageRpcResponseV1, String> {
    let path = session_source_path(request.session_id.as_str())?;
    let store = agent_runtime::agent_runtime_store_actor()?;
    let session_id = request.session_id.clone();
    let response = page_with_store(&store, path.as_path(), request)?;
    if !response.target_reached {
        let selection = select_projection_generation(
            &store,
            session_id.as_str(),
            Some(response.projection_generation.as_str()),
        )?;
        schedule_background_backfill(session_id, path, background_job(&response, selection)?)?;
    }
    Ok(response)
}

pub(crate) fn patches(
    request: TranscriptPatchRpcRequestV1,
) -> Result<TranscriptPatchRpcResponseV1, String> {
    let path = session_source_path(request.session_id.as_str())?;
    let store = agent_runtime::agent_runtime_store_actor()?;
    let session_id = request.session_id.clone();
    let response = patches_with_store(&store, path.as_path(), request)?;
    if !response.target_reached {
        let selection = select_projection_generation(
            &store,
            session_id.as_str(),
            Some(response.projection_generation.as_str()),
        )?;
        schedule_background_backfill(
            session_id,
            path,
            TranscriptBackgroundJobV1 {
                target_source_high_water: parse_waterline(
                    response.target_source_high_water.as_str(),
                    "target",
                )?,
                projection_generation: selection.projection_generation,
                expected_current_generation: selection.expected_current_generation,
                rebuild: selection.rebuild,
            },
        )?;
    }
    Ok(response)
}

pub(crate) fn content_range(
    request: TranscriptContentRangeReadRequestV1,
) -> Result<TranscriptContentRangeV1, String> {
    request.validate()?;
    let store = agent_runtime::agent_runtime_store_actor()?;
    let current = store
        .load_current_transcript_projection_generation(request.session_id.as_str())?
        .ok_or_else(|| "transcript content range view is unavailable".to_string())?;
    if current.projection_generation != request.projection_generation {
        return Err("transcript content range view is invalidated".to_string());
    }
    if request.ref_id.starts_with("session-event:") {
        return message_log::read_transcript_event_content(&request);
    }
    let call_id = request
        .ref_id
        .strip_prefix("tool-output:")
        .expect("validated tool output ref");
    let offset = parse_waterline(request.offset.as_str(), "content range offset")?;
    let byte_length = parse_waterline(request.byte_length.as_str(), "content range byteLength")?;
    let (end, content) = message_log::read_transcript_tool_output_range(
        request.session_id.as_str(),
        call_id,
        byte_length,
        offset,
        request.max_bytes as usize,
    )?;
    let response = TranscriptContentRangeV1 {
        schema: TRANSCRIPT_CONTENT_RANGE_SCHEMA_V1.to_string(),
        session_id: request.session_id,
        projection_version: TRANSCRIPT_PROJECTION_VERSION_V1.to_string(),
        projection_generation: request.projection_generation,
        ref_id: request.ref_id,
        revision: request.revision,
        byte_length: byte_length.to_string(),
        start_offset: offset.to_string(),
        end_offset: end.to_string(),
        content,
        has_more: end < byte_length,
    };
    response.validate()?;
    Ok(response)
}

fn schedule_background_backfill(
    session_id: String,
    path: std::path::PathBuf,
    job: TranscriptBackgroundJobV1,
) -> Result<(), String> {
    let active = TRANSCRIPT_BACKFILLS.get_or_init(|| Mutex::new(HashMap::new()));
    {
        let mut guard = active
            .lock()
            .map_err(|_| "transcript backfill registry lock poisoned".to_string())?;
        if let Some(current) = guard.get_mut(session_id.as_str()) {
            merge_background_job(current, job)?;
            return Ok(());
        }
        guard.insert(session_id.clone(), job);
    }
    let failed_spawn_session_id = session_id.clone();
    std::thread::Builder::new()
        .name(format!("transcript-backfill-{session_id}"))
        .spawn(move || {
            let result = (|| -> Result<(), String> {
                let store = agent_runtime::agent_runtime_store_actor()?;
                loop {
                    let job = TRANSCRIPT_BACKFILLS
                        .get()
                        .ok_or_else(|| "transcript backfill registry is missing".to_string())?
                        .lock()
                        .map_err(|_| "transcript backfill registry lock poisoned".to_string())?
                        .get(session_id.as_str())
                        .cloned()
                        .ok_or_else(|| "transcript backfill registration is missing".to_string())?;
                    let progress = match run_background_job_slice(
                        &store,
                        path.as_path(),
                        session_id.as_str(),
                        &job,
                    ) {
                        Ok(progress) => progress,
                        Err(error) => {
                            let active = TRANSCRIPT_BACKFILLS.get().ok_or_else(|| {
                                "transcript backfill registry is missing".to_string()
                            })?;
                            let mut guard = active.lock().map_err(|_| {
                                "transcript backfill registry lock poisoned".to_string()
                            })?;
                            if guard.get(session_id.as_str()) != Some(&job) {
                                continue;
                            }
                            guard.remove(session_id.as_str());
                            return Err(error);
                        }
                    };
                    if progress.oversized_record {
                        eprintln!(
                            "transcript projection oversizedSlice sessionId={session_id} generation={} sourceHighWater={} sourceBytes={} projectedEvents={}",
                            job.projection_generation,
                            progress.projected_source_high_water,
                            progress.source_bytes,
                            progress.projected_events,
                        );
                    }
                    if progress.target_reached {
                        let active = TRANSCRIPT_BACKFILLS
                            .get()
                            .ok_or_else(|| "transcript backfill registry is missing".to_string())?;
                        let mut guard = active.lock().map_err(|_| {
                            "transcript backfill registry lock poisoned".to_string()
                        })?;
                        if guard.get(session_id.as_str()) == Some(&job) {
                            guard.remove(session_id.as_str());
                            return Ok(());
                        }
                        if job.rebuild {
                            if let Some(current) = guard.get_mut(session_id.as_str()) {
                                if current.projection_generation == job.projection_generation
                                    && current.rebuild
                                {
                                    current.rebuild = false;
                                    current.expected_current_generation = None;
                                }
                            }
                        }
                    }
                    std::thread::yield_now();
                }
            })();
            if let Err(error) = result {
                eprintln!("transcript background backfill failed for {session_id}: {error}");
            }
        })
        .map(|_| ())
        .map_err(|error| {
            if let Ok(mut guard) = active.lock() {
                guard.remove(failed_spawn_session_id.as_str());
            }
            format!("start transcript background backfill failed: {error}")
        })
}

fn merge_background_job(
    current: &mut TranscriptBackgroundJobV1,
    next: TranscriptBackgroundJobV1,
) -> Result<(), String> {
    if current.projection_generation == next.projection_generation {
        if current.rebuild && !next.rebuild {
            let target_source_high_water = current
                .target_source_high_water
                .max(next.target_source_high_water);
            *current = next;
            current.target_source_high_water = target_source_high_water;
            return Ok(());
        }
        if current.rebuild != next.rebuild
            || current.expected_current_generation != next.expected_current_generation
        {
            return Err(
                "transcript background generation parameters changed while work is active"
                    .to_string(),
            );
        }
        current.target_source_high_water = current
            .target_source_high_water
            .max(next.target_source_high_water);
        return Ok(());
    }
    if current.rebuild && next.rebuild {
        return Err(
            "transcript replacement generation changed while rebuild is active".to_string(),
        );
    }
    // A tombstone can invalidate the generation currently being projected. Replace that job
    // in-place; the one worker for this session observes the new generation after its slice.
    *current = next;
    Ok(())
}

fn background_job(
    response: &TranscriptPageRpcResponseV1,
    selection: TranscriptGenerationSelectionV1,
) -> Result<TranscriptBackgroundJobV1, String> {
    Ok(TranscriptBackgroundJobV1 {
        target_source_high_water: parse_waterline(
            response.target_source_high_water.as_str(),
            "target",
        )?,
        projection_generation: selection.projection_generation,
        expected_current_generation: selection.expected_current_generation,
        rebuild: selection.rebuild,
    })
}

fn run_background_job_slice<S: TranscriptProjectionGenerationStorePortV1 + ?Sized>(
    store: &S,
    path: &Path,
    session_id: &str,
    job: &TranscriptBackgroundJobV1,
) -> Result<TranscriptBackfillSliceResultV1, String> {
    if job.target_source_high_water == 0 {
        return Ok(TranscriptBackfillSliceResultV1 {
            projected_source_high_water: 0,
            projected_events: 0,
            source_bytes: 0,
            target_reached: true,
            oversized_record: false,
        });
    }
    if job.rebuild {
        let request = TranscriptGenerationRebuildRequestV1 {
            session_id: session_id.to_string(),
            projection_generation: job.projection_generation.clone(),
            target_source_high_water: job.target_source_high_water.to_string(),
            expected_current_generation: job.expected_current_generation.clone(),
        };
        rebuild_transcript_generation_v1(
            &request,
            &LocalTranscriptRebuildSourceV1 { path },
            store,
        )?;
        return Ok(TranscriptBackfillSliceResultV1 {
            projected_source_high_water: job.target_source_high_water,
            projected_events: usize::try_from(job.target_source_high_water).unwrap_or(usize::MAX),
            source_bytes: 0,
            target_reached: true,
            oversized_record: false,
        });
    }
    let progress = project_one_source_slice(
        store,
        path,
        session_id,
        job.projection_generation.as_str(),
        job.target_source_high_water,
    )?;
    if progress.target_reached {
        match store.load_current_transcript_projection_generation(session_id)? {
            Some(current) if current.projection_generation == job.projection_generation => {}
            Some(_) => {
                return Err(TRANSCRIPT_PROJECTION_GENERATION_INVALIDATED.to_string());
            }
            None => {
                store.rotate_current_transcript_projection_generation(
                    TranscriptProjectionGenerationRotationV1 {
                        session_id: session_id.to_string(),
                        projection_version: TRANSCRIPT_PROJECTION_VERSION_V1.to_string(),
                        expected_current_generation: None,
                        next_generation: job.projection_generation.clone(),
                        target_source_high_water: progress.projected_source_high_water.to_string(),
                    },
                )?;
            }
        }
    }
    Ok(progress)
}

fn select_projection_generation<S: TranscriptProjectionGenerationStorePortV1 + ?Sized>(
    store: &S,
    session_id: &str,
    requested: Option<&str>,
) -> Result<TranscriptGenerationSelectionV1, String> {
    let current = store.load_current_transcript_projection_generation(session_id)?;
    if let Some(current) = current {
        let head = store
            .load_transcript_projection_head(session_id, current.projection_generation.as_str())?
            .ok_or_else(|| {
                "current transcript projection generation head is missing".to_string()
            })?;
        if head.invalidation_reason.is_none() {
            if requested.is_some_and(|value| value != current.projection_generation) {
                return Err(TRANSCRIPT_PROJECTION_GENERATION_INVALIDATED.to_string());
            }
            return Ok(TranscriptGenerationSelectionV1 {
                projection_generation: current.projection_generation,
                expected_current_generation: None,
                published: true,
                rebuild: false,
            });
        }
        let next =
            rebuild_projection_generation(session_id, current.projection_generation.as_str());
        match requested {
            Some(value) if value == current.projection_generation => {
                return Err(TRANSCRIPT_PROJECTION_GENERATION_INVALIDATED.to_string())
            }
            Some(value) if value != next => {
                return Err(TRANSCRIPT_PROJECTION_GENERATION_INVALIDATED.to_string())
            }
            _ => {}
        }
        return Ok(TranscriptGenerationSelectionV1 {
            projection_generation: next,
            expected_current_generation: Some(current.projection_generation),
            published: false,
            rebuild: true,
        });
    }

    let boot_head = store
        .load_transcript_projection_head(session_id, LOCAL_TRANSCRIPT_PROJECTION_GENERATION_V1)?;
    if boot_head
        .as_ref()
        .is_some_and(|head| head.invalidation_reason.is_some())
    {
        let next =
            rebuild_projection_generation(session_id, LOCAL_TRANSCRIPT_PROJECTION_GENERATION_V1);
        match requested {
            Some(value) if value == LOCAL_TRANSCRIPT_PROJECTION_GENERATION_V1 => {
                return Err(TRANSCRIPT_PROJECTION_GENERATION_INVALIDATED.to_string())
            }
            Some(value) if value != next => {
                return Err(TRANSCRIPT_PROJECTION_GENERATION_INVALIDATED.to_string())
            }
            _ => {}
        }
        return Ok(TranscriptGenerationSelectionV1 {
            projection_generation: next,
            expected_current_generation: None,
            published: false,
            rebuild: true,
        });
    }
    validate_requested_generation(requested)?;
    Ok(TranscriptGenerationSelectionV1 {
        projection_generation: LOCAL_TRANSCRIPT_PROJECTION_GENERATION_V1.to_string(),
        expected_current_generation: None,
        published: false,
        rebuild: false,
    })
}

fn rebuild_projection_generation(session_id: &str, current_generation: &str) -> String {
    let digest =
        Sha256::digest(format!("{session_id}\0{current_generation}\0rebuild-v1").as_bytes());
    format!("local-transcript-rebuild-v1-{digest:x}")
}

fn session_source_path(session_id: &str) -> Result<std::path::PathBuf, String> {
    if session_id.trim().is_empty() {
        return Err("transcript sessionId is required".to_string());
    }
    user_data_layout::find_session_log_file_path(session_id)?
        .ok_or_else(|| format!("session not found: {session_id}"))
}

fn page_with_store<S: TranscriptProjectionGenerationStorePortV1 + ?Sized>(
    store: &S,
    path: &Path,
    request: TranscriptPageRpcRequestV1,
) -> Result<TranscriptPageRpcResponseV1, String> {
    let available_source_high_water =
        message_log::transcript_source_high_water(path, request.session_id.as_str())?;
    let target_source_high_water = request
        .source_high_water
        .as_deref()
        .map(|value| parse_waterline(value, "page target"))
        .transpose()?
        .unwrap_or(available_source_high_water);
    if target_source_high_water > available_source_high_water {
        return Err("transcript page sourceHighWater is after the available source".to_string());
    }
    let generation = select_projection_generation(
        store,
        request.session_id.as_str(),
        request.projection_generation.as_deref(),
    )?;
    let projected_source_high_water = load_projected_source_high_water(
        store,
        request.session_id.as_str(),
        generation.projection_generation.as_str(),
    )?;
    let target_reached = projected_source_high_water >= target_source_high_water
        && (target_source_high_water == 0 || generation.published);
    let requested_waterline = target_source_high_water.to_string();
    let page = if target_source_high_water == 0 && target_reached {
        if request.older_cursor.is_some() {
            return Err("empty transcript page cannot have an olderCursor".to_string());
        }
        let page = TranscriptPageV1 {
            schema: TRANSCRIPT_PAGE_SCHEMA_V1.to_string(),
            session_id: request.session_id,
            projection_version: TRANSCRIPT_PROJECTION_VERSION_V1.to_string(),
            projection_generation: LOCAL_TRANSCRIPT_PROJECTION_GENERATION_V1.to_string(),
            source_high_water: "0".to_string(),
            blocks: Vec::new(),
            older_cursor: None,
            has_older: false,
            resume_cursors: Vec::new(),
        };
        page.validate(TranscriptPagePolicyV1::default())?;
        Some(page)
    } else if target_reached && generation.published {
        let older_cursor = request.older_cursor.clone();
        let stored = store
            .load_current_transcript_page(TranscriptPageReadRequestV1 {
                session_id: request.session_id,
                projection_version: TRANSCRIPT_PROJECTION_VERSION_V1.into(),
                projection_generation: generation.projection_generation.clone(),
                source_high_water: requested_waterline,
                older_cursor: request.older_cursor,
                policy: TranscriptPagePolicyV1::default(),
            })?
            .page;
        Some(page_with_read_only_presentation(
            path,
            older_cursor.as_deref(),
            stored,
        )?)
    } else {
        None
    };
    Ok(TranscriptPageRpcResponseV1 {
        schema: TRANSCRIPT_PAGE_RPC_SCHEMA_V1,
        projection_version: TRANSCRIPT_PROJECTION_VERSION_V1,
        projection_generation: generation.projection_generation,
        projected_source_high_water: projected_source_high_water.to_string(),
        target_source_high_water: target_source_high_water.to_string(),
        target_reached,
        page,
    })
}

fn page_with_read_only_presentation(
    path: &Path,
    older_cursor: Option<&str>,
    page: TranscriptPageV1,
) -> Result<TranscriptPageV1, String> {
    if page.blocks.iter().all(|block| block.presentation.is_some()) {
        return Ok(page);
    }
    let target = parse_waterline(&page.source_high_water, "read-only presentation")?;
    let mut cursor = None;
    let mut records = Vec::new();
    while records.last().map_or(
        0,
        |record: &centaeris_core::session::SequencedSessionRecord| record.sequence,
    ) < target
    {
        let slice =
            message_log::read_transcript_source_slice(path, &page.session_id, cursor.as_ref())?;
        let before = records.len();
        for (record, next) in slice.records.into_iter().zip(slice.cursors_after_records) {
            if record.sequence > target {
                break;
            }
            records.push(record);
            cursor = Some(next);
        }
        if records.len() == before {
            return Err("read-only transcript source ended before the requested page".into());
        }
    }
    let mut refreshed =
        centaeris_core::session::transcript::read_only_transcript_page_from_records(
            &TranscriptPageReadRequestV1 {
                session_id: page.session_id,
                projection_version: page.projection_version,
                projection_generation: page.projection_generation,
                source_high_water: page.source_high_water,
                older_cursor: older_cursor.map(str::to_string),
                policy: TranscriptPagePolicyV1::default(),
            },
            &records,
        )?;
    refreshed.resume_cursors = page.resume_cursors;
    Ok(refreshed)
}

fn patches_with_store<S: TranscriptProjectionGenerationStorePortV1 + ?Sized>(
    store: &S,
    path: &Path,
    request: TranscriptPatchRpcRequestV1,
) -> Result<TranscriptPatchRpcResponseV1, String> {
    let available_source_high_water =
        message_log::transcript_source_high_water(path, request.session_id.as_str())?;
    let target_source_high_water = request
        .through_source_high_water
        .as_deref()
        .map(|value| parse_waterline(value, "patch target"))
        .transpose()?
        .unwrap_or(available_source_high_water);
    if target_source_high_water > available_source_high_water {
        return Err(
            "transcript patch throughSourceHighWater is after the available source".to_string(),
        );
    }
    let generation = select_projection_generation(
        store,
        request.session_id.as_str(),
        request.projection_generation.as_deref(),
    )?;
    let projected_source_high_water = load_projected_source_high_water(
        store,
        request.session_id.as_str(),
        generation.projection_generation.as_str(),
    )?;
    let through_source_high_water = target_source_high_water
        .min(projected_source_high_water)
        .to_string();
    let result = if projected_source_high_water == 0 || !generation.published {
        let after = parse_waterline(request.after_source_high_water.as_str(), "patch after")?;
        if after != 0 {
            return Err("transcript patch afterSourceHighWater is not projected".to_string());
        }
        centaeris_core::session::transcript::TranscriptPatchReadResultV1 {
            patches: Vec::new(),
            next_source_high_water: "0".to_string(),
            has_more: false,
            work: Default::default(),
        }
    } else {
        store.load_current_transcript_patches(TranscriptPatchReadRequestV1 {
            session_id: request.session_id,
            projection_version: TRANSCRIPT_PROJECTION_VERSION_V1.to_string(),
            projection_generation: generation.projection_generation.clone(),
            after_source_high_water: request.after_source_high_water,
            through_source_high_water,
        })?
    };
    Ok(TranscriptPatchRpcResponseV1 {
        schema: TRANSCRIPT_PATCH_RPC_SCHEMA_V1,
        projection_version: TRANSCRIPT_PROJECTION_VERSION_V1,
        projection_generation: generation.projection_generation,
        projected_source_high_water: projected_source_high_water.to_string(),
        target_source_high_water: target_source_high_water.to_string(),
        target_reached: projected_source_high_water >= target_source_high_water
            && (target_source_high_water == 0 || generation.published),
        patches: result.patches,
        next_source_high_water: result.next_source_high_water,
        has_more: result.has_more,
    })
}

fn load_projected_source_high_water<S: TranscriptProjectionStorePort + ?Sized>(
    store: &S,
    session_id: &str,
    projection_generation: &str,
) -> Result<u64, String> {
    let head = store.load_transcript_projection_head(session_id, projection_generation)?;
    if let Some(reason) = head
        .as_ref()
        .and_then(|value| value.invalidation_reason.as_deref())
    {
        return Err(format!("transcript projection is invalidated: {reason}"));
    }
    head.as_ref()
        .map(|value| parse_waterline(value.source_high_water.as_str(), "projection head"))
        .transpose()
        .map(Option::unwrap_or_default)
}

fn project_one_source_slice<S: TranscriptProjectionStorePort + ?Sized>(
    store: &S,
    path: &Path,
    session_id: &str,
    projection_generation: &str,
    target_source_high_water: u64,
) -> Result<TranscriptBackfillSliceResultV1, String> {
    let started = Instant::now();
    if session_id.trim().is_empty() || projection_generation.trim().is_empty() {
        return Err("transcript projection identity is required".to_string());
    }
    let head = store.load_transcript_projection_head(session_id, projection_generation)?;
    if let Some(reason) = head
        .as_ref()
        .and_then(|value| value.invalidation_reason.as_deref())
    {
        return Err(format!("transcript projection is invalidated: {reason}"));
    }
    let head_high_water = head
        .as_ref()
        .map(|value| parse_waterline(value.source_high_water.as_str(), "projection head"))
        .transpose()?
        .unwrap_or(0);
    if head_high_water >= target_source_high_water {
        return Ok(TranscriptBackfillSliceResultV1 {
            projected_source_high_water: head_high_water,
            projected_events: 0,
            source_bytes: 0,
            target_reached: true,
            oversized_record: false,
        });
    }
    let recovery = store.load_latest_transcript_recovery(
        session_id,
        projection_generation,
        head_high_water,
    )?;
    let recovery_high_water = recovery
        .as_ref()
        .map(|value| {
            parse_waterline(
                value.checkpoint.source_high_water.as_str(),
                "projection checkpoint",
            )
        })
        .transpose()?
        .unwrap_or(0);
    if head_high_water.saturating_sub(recovery_high_water)
        >= TRANSCRIPT_PROJECTION_SLICE_MAX_EVENTS as u64
    {
        return Err(
            "transcript projection recovery window exceeds one bounded checkpoint interval"
                .to_string(),
        );
    }

    let mut projector = match recovery {
        Some(value) => TranscriptProjectorV1::from_checkpoint_recovery(value)?,
        None => {
            TranscriptProjectorV1::new(session_id.to_string(), projection_generation.to_string())?
        }
    };
    if recovery_high_water < head_high_water {
        let mut cursor = load_source_cursor_at(
            store,
            session_id,
            projection_generation,
            recovery_high_water,
        )?;
        let mut recovered_events = 0usize;
        while projector_source_cursor_sequence(cursor.as_ref()) <= head_high_water {
            let recovery_slice =
                message_log::read_transcript_source_slice(path, session_id, cursor.as_ref())?;
            let mut advanced = false;
            for (record, next_cursor) in recovery_slice
                .records
                .iter()
                .zip(recovery_slice.cursors_after_records.iter())
            {
                if record.sequence > head_high_water {
                    break;
                }
                recovered_events = recovered_events.saturating_add(1);
                if recovered_events >= TRANSCRIPT_PROJECTION_SLICE_MAX_EVENTS {
                    return Err(
                        "transcript projection recovery exceeded one bounded checkpoint interval"
                            .to_string(),
                    );
                }
                projector.apply(
                    record,
                    TRANSCRIPT_SOURCE_STREAM_ID,
                    next_cursor.encode().as_str(),
                )?;
                cursor = Some(next_cursor.clone());
                advanced = true;
            }
            if !advanced {
                return Err(
                    "transcript projection recovery did not reach projection head".to_string(),
                );
            }
        }
    }

    let head_cursor =
        load_source_cursor_at(store, session_id, projection_generation, head_high_water)?;
    let slice = message_log::read_transcript_source_slice(path, session_id, head_cursor.as_ref())?;
    let mut projected_source_high_water = head_high_water;
    let mut projected_events = 0usize;
    let mut projected_source_bytes = 0usize;
    for ((record, next_cursor), cumulative_source_bytes) in slice
        .records
        .iter()
        .zip(slice.cursors_after_records.iter())
        .zip(slice.source_bytes_after_records.iter())
    {
        if record.sequence > target_source_high_water {
            break;
        }
        let checkpoint_refs = Some(TranscriptCheckpointRefsV1 {
            frontier_ref: projection_record_identity(
                "transcript-frontier-v1",
                session_id,
                projection_generation,
                record.sequence,
            ),
            block_index_ref: projection_record_identity(
                "transcript-block-index-v1",
                session_id,
                projection_generation,
                record.sequence,
            ),
        });
        projector.apply_and_commit(
            record,
            TRANSCRIPT_SOURCE_STREAM_ID,
            next_cursor.encode().as_str(),
            projection_record_identity(
                "transcript-commit-v1",
                session_id,
                projection_generation,
                record.sequence,
            )
            .as_str(),
            checkpoint_refs,
            store,
        )?;
        projected_source_high_water = record.sequence;
        projected_events = projected_events.saturating_add(1);
        projected_source_bytes = *cumulative_source_bytes;
        if started.elapsed().as_micros() >= u128::from(TRANSCRIPT_PROJECTION_SLICE_MAX_MICROS) {
            break;
        }
    }
    Ok(TranscriptBackfillSliceResultV1 {
        projected_source_high_water,
        projected_events,
        source_bytes: projected_source_bytes,
        target_reached: projected_source_high_water >= target_source_high_water,
        oversized_record: projected_events == 1 && slice.oversized_record,
    })
}

fn load_source_cursor_at<S: TranscriptProjectionStorePort + ?Sized>(
    store: &S,
    session_id: &str,
    projection_generation: &str,
    source_high_water: u64,
) -> Result<Option<message_log::TranscriptSourceSliceCursorV1>, String> {
    if source_high_water == 0 {
        return Ok(None);
    }
    let cursors = store.load_transcript_resume_cursors(
        session_id,
        projection_generation,
        source_high_water,
    )?;
    let cursor = required_source_resume_cursor(cursors.as_slice())?;
    message_log::TranscriptSourceSliceCursorV1::decode(cursor.cursor.as_str()).map(Some)
}

fn required_source_resume_cursor(
    cursors: &[TranscriptResumeCursorV1],
) -> Result<&TranscriptResumeCursorV1, String> {
    let mut matches = cursors
        .iter()
        .filter(|cursor| cursor.stream_id == TRANSCRIPT_SOURCE_STREAM_ID);
    let cursor = matches
        .next()
        .ok_or_else(|| "transcript projection source resume cursor is missing".to_string())?;
    if matches.next().is_some() {
        return Err("transcript projection source resume cursor is duplicated".to_string());
    }
    Ok(cursor)
}

fn projector_source_cursor_sequence(
    cursor: Option<&message_log::TranscriptSourceSliceCursorV1>,
) -> u64 {
    cursor.map_or(1, message_log::TranscriptSourceSliceCursorV1::next_sequence)
}

fn parse_waterline(value: &str, name: &str) -> Result<u64, String> {
    value
        .parse::<u64>()
        .map_err(|_| format!("transcript {name} sourceHighWater is invalid"))
}

fn validate_requested_generation(value: Option<&str>) -> Result<(), String> {
    if value.is_some_and(|value| value != LOCAL_TRANSCRIPT_PROJECTION_GENERATION_V1) {
        return Err(
            "transcript projectionGeneration does not match the local Runtime generation"
                .to_string(),
        );
    }
    Ok(())
}

fn projection_record_identity(
    kind: &str,
    session_id: &str,
    projection_generation: &str,
    source_sequence: u64,
) -> String {
    let digest = Sha256::digest(
        format!("{kind}\0{session_id}\0{projection_generation}\0{source_sequence}").as_bytes(),
    );
    format!("{kind}:sha256:{digest:x}")
}

#[cfg(test)]
mod tests {
    use std::fs::{self, OpenOptions};
    use std::io::Write;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use centaeris_core::session::transcript::{
        TranscriptBlockBodyV1, TranscriptProjectionStorePort,
    };
    use centaeris_core::session::{
        parse_event, wire_record_value, SequencedSessionRecord, SessionManifestV1,
    };
    use centaeris_runtime_sqlite::SqliteRuntimeStore;
    use serde_json::json;

    use super::*;

    #[test]
    fn tombstone_replacement_generation_supersedes_the_in_flight_incremental_job() {
        let mut current = TranscriptBackgroundJobV1 {
            target_source_high_water: 10,
            projection_generation: LOCAL_TRANSCRIPT_PROJECTION_GENERATION_V1.to_string(),
            expected_current_generation: None,
            rebuild: false,
        };
        let replacement = TranscriptBackgroundJobV1 {
            target_source_high_water: 12,
            projection_generation: "local-transcript-rebuild-v1-test".to_string(),
            expected_current_generation: Some(
                LOCAL_TRANSCRIPT_PROJECTION_GENERATION_V1.to_string(),
            ),
            rebuild: true,
        };
        merge_background_job(&mut current, replacement.clone())
            .expect("replacement generation takes ownership of the single worker");
        assert_eq!(current, replacement);
    }

    #[test]
    fn published_rebuild_job_transitions_to_incremental_catch_up() {
        let generation = "local-transcript-rebuild-v1-test".to_string();
        let mut current = TranscriptBackgroundJobV1 {
            target_source_high_water: 10,
            projection_generation: generation.clone(),
            expected_current_generation: Some(
                LOCAL_TRANSCRIPT_PROJECTION_GENERATION_V1.to_string(),
            ),
            rebuild: true,
        };
        merge_background_job(
            &mut current,
            TranscriptBackgroundJobV1 {
                target_source_high_water: 12,
                projection_generation: generation,
                expected_current_generation: None,
                rebuild: false,
            },
        )
        .expect("published generation continues incrementally");
        assert_eq!(current.target_source_high_water, 12);
        assert!(!current.rebuild);
        assert!(current.expected_current_generation.is_none());
    }

    #[test]
    fn old_session_backfill_is_slice_bounded_and_resumes_from_persisted_cursors() {
        let fixture = TranscriptFixture::new("bounded", 260);
        let store = SqliteRuntimeStore::new(&fixture.database_path).expect("create store");
        let mut projected = 0;
        let mut slices = 0;
        loop {
            let result = project_one_source_slice(
                &store,
                fixture.log_path.as_path(),
                fixture.session_id,
                LOCAL_TRANSCRIPT_PROJECTION_GENERATION_V1,
                260,
            )
            .expect("project bounded slice");
            assert!(result.projected_events <= TRANSCRIPT_PROJECTION_SLICE_MAX_EVENTS);
            assert!(result.projected_source_high_water >= projected);
            projected = result.projected_source_high_water;
            assert_eq!(
                store
                    .load_latest_transcript_recovery(
                        fixture.session_id,
                        LOCAL_TRANSCRIPT_PROJECTION_GENERATION_V1,
                        projected,
                    )
                    .expect("load yield recovery")
                    .expect("every yielded head has a recovery frontier")
                    .checkpoint
                    .source_high_water,
                projected.to_string()
            );
            slices += 1;
            if result.target_reached {
                break;
            }
            assert!(slices < 400, "backfill must make progress");
        }
        assert!(slices >= 3);
        assert_eq!(projected, 260);
        assert_eq!(
            store
                .load_transcript_projection_head(
                    fixture.session_id,
                    LOCAL_TRANSCRIPT_PROJECTION_GENERATION_V1,
                )
                .expect("load head")
                .expect("projection head")
                .source_high_water,
            "260"
        );
    }

    #[test]
    fn oversized_source_record_owns_one_slice_and_advances_to_a_pageable_ref() {
        let fixture = TranscriptFixture::new_with_text(
            "oversized",
            1,
            "z".repeat(
                centaeris_core::session::transcript::TRANSCRIPT_PROJECTION_SLICE_MAX_BYTES + 1,
            ),
        );
        let store = SqliteRuntimeStore::new(&fixture.database_path).expect("create store");
        let result = project_one_source_slice(
            &store,
            fixture.log_path.as_path(),
            fixture.session_id,
            LOCAL_TRANSCRIPT_PROJECTION_GENERATION_V1,
            1,
        )
        .expect("project oversized source record");
        assert_eq!(result.projected_events, 1);
        assert_eq!(result.projected_source_high_water, 1);
        assert!(result.target_reached);
        assert!(result.oversized_record);
        let cursor = load_source_cursor_at(
            &store,
            fixture.session_id,
            LOCAL_TRANSCRIPT_PROJECTION_GENERATION_V1,
            1,
        )
        .expect("load advanced cursor")
        .expect("stored cursor");
        assert_eq!(cursor.next_sequence(), 2);
        let page = store
            .load_transcript_page(TranscriptPageReadRequestV1 {
                session_id: fixture.session_id.to_string(),
                projection_version: TRANSCRIPT_PROJECTION_VERSION_V1.to_string(),
                projection_generation: LOCAL_TRANSCRIPT_PROJECTION_GENERATION_V1.to_string(),
                source_high_water: "1".to_string(),
                older_cursor: None,
                policy: TranscriptPagePolicyV1::default(),
            })
            .expect("load page after oversized record")
            .page;
        assert_eq!(page.blocks.len(), 1);
        let TranscriptBlockBodyV1::UserText { content } = &page.blocks[0].body else {
            panic!("oversized fixture projects user text")
        };
        assert!(content.inline_content.is_none());
        assert!(content.source_ref.is_some());
    }

    #[test]
    fn page_waterline_and_committed_patch_recovery_have_no_gap_or_duplicate() {
        let mut fixture = TranscriptFixture::new("page-patch", 2);
        let store = SqliteRuntimeStore::new(&fixture.database_path).expect("create store");
        let first = page_until_ready(
            &store,
            fixture.log_path.as_path(),
            TranscriptPageRpcRequestV1 {
                session_id: fixture.session_id.to_string(),
                projection_generation: None,
                source_high_water: None,
                older_cursor: None,
            },
        );
        let page = first.page.expect("ready page");
        assert_eq!(page.source_high_water, "2");
        assert_eq!(page.resume_cursors.len(), 1);

        fixture.append_user_record(3);
        let request = TranscriptPatchRpcRequestV1 {
            session_id: fixture.session_id.to_string(),
            projection_generation: Some(LOCAL_TRANSCRIPT_PROJECTION_GENERATION_V1.to_string()),
            after_source_high_water: page.source_high_water,
            through_source_high_water: None,
        };
        let first_patch = patches_until_ready(
            &store,
            fixture.log_path.as_path(),
            TranscriptPatchRpcRequestV1 {
                session_id: request.session_id.clone(),
                projection_generation: request.projection_generation.clone(),
                after_source_high_water: request.after_source_high_water.clone(),
                through_source_high_water: None,
            },
        );
        assert_eq!(first_patch.patches.len(), 1);
        assert_eq!(first_patch.patches[0].source_high_water, "3");
        assert_eq!(first_patch.next_source_high_water, "3");

        let retry = store
            .load_transcript_patches(TranscriptPatchReadRequestV1 {
                session_id: request.session_id.clone(),
                projection_version: TRANSCRIPT_PROJECTION_VERSION_V1.to_string(),
                projection_generation: LOCAL_TRANSCRIPT_PROJECTION_GENERATION_V1.to_string(),
                after_source_high_water: request.after_source_high_water,
                through_source_high_water: "3".to_string(),
            })
            .expect("retry frozen patch interval");
        assert_eq!(retry.patches, first_patch.patches);

        let caught_up = patches_with_store(
            &store,
            fixture.log_path.as_path(),
            TranscriptPatchRpcRequestV1 {
                session_id: request.session_id,
                projection_generation: request.projection_generation,
                after_source_high_water: first_patch.next_source_high_water,
                through_source_high_water: None,
            },
        )
        .expect("continue after applied cursor");
        assert!(caught_up.patches.is_empty());
        assert_eq!(caught_up.next_source_high_water, "3");
    }

    #[test]
    fn client_cannot_create_an_arbitrary_projection_generation() {
        let fixture = TranscriptFixture::new("generation", 1);
        let store = SqliteRuntimeStore::new(&fixture.database_path).expect("create store");
        let error = page_with_store(
            &store,
            fixture.log_path.as_path(),
            TranscriptPageRpcRequestV1 {
                session_id: fixture.session_id.to_string(),
                projection_generation: Some("client-controlled-generation".to_string()),
                source_high_water: None,
                older_cursor: None,
            },
        )
        .expect_err("client generation must not select storage identity");
        assert!(error.contains("does not match the local Runtime generation"));
        assert!(store
            .load_transcript_projection_head(fixture.session_id, "client-controlled-generation",)
            .expect("head lookup")
            .is_none());
    }

    #[test]
    fn projection_storage_identities_are_unique_across_sessions_with_the_same_sequence() {
        let first = TranscriptFixture::new("identity-a", 1);
        let second = TranscriptFixture::new("identity-b", 1);
        let store = SqliteRuntimeStore::new(&first.database_path).expect("create shared store");
        for fixture in [&first, &second] {
            let progress = project_one_source_slice(
                &store,
                fixture.log_path.as_path(),
                fixture.session_id,
                LOCAL_TRANSCRIPT_PROJECTION_GENERATION_V1,
                1,
            )
            .expect("project session sequence one");
            assert_eq!(progress.projected_source_high_water, 1);
        }
        for session_id in [first.session_id, second.session_id] {
            assert_eq!(
                store
                    .load_transcript_projection_head(
                        session_id,
                        LOCAL_TRANSCRIPT_PROJECTION_GENERATION_V1,
                    )
                    .expect("load session head")
                    .expect("session projection")
                    .source_high_water,
                "1"
            );
        }
    }

    #[test]
    fn empty_session_returns_a_valid_empty_tail_without_a_projection_head() {
        let fixture = TranscriptFixture::new("empty", 0);
        let store = SqliteRuntimeStore::new(&fixture.database_path).expect("create store");
        let response = page_with_store(
            &store,
            fixture.log_path.as_path(),
            TranscriptPageRpcRequestV1 {
                session_id: fixture.session_id.to_string(),
                projection_generation: None,
                source_high_water: None,
                older_cursor: None,
            },
        )
        .expect("empty tail page");
        let page = response.page.expect("ready empty page");
        assert_eq!(page.source_high_water, "0");
        assert!(page.blocks.is_empty());
        assert!(!page.has_older);
        assert!(page.resume_cursors.is_empty());
        assert!(store
            .load_transcript_projection_head(
                fixture.session_id,
                LOCAL_TRANSCRIPT_PROJECTION_GENERATION_V1,
            )
            .expect("head lookup")
            .is_none());
    }

    #[test]
    fn initial_page_freezes_source_target_while_new_events_become_patches() {
        let mut fixture = TranscriptFixture::new_with_text("active", 5, "x".repeat(60 * 1024));
        let store = SqliteRuntimeStore::new(&fixture.database_path).expect("create store");
        let first = page_with_store(
            &store,
            fixture.log_path.as_path(),
            TranscriptPageRpcRequestV1 {
                session_id: fixture.session_id.to_string(),
                projection_generation: None,
                source_high_water: None,
                older_cursor: None,
            },
        )
        .expect("start initial page");
        assert_eq!(first.target_source_high_water, "5");
        assert!(
            first.page.is_none(),
            "source byte budget must yield before H=5"
        );

        fixture.append_user_record(6);
        let frozen = page_until_ready(
            &store,
            fixture.log_path.as_path(),
            TranscriptPageRpcRequestV1 {
                session_id: fixture.session_id.to_string(),
                projection_generation: Some(LOCAL_TRANSCRIPT_PROJECTION_GENERATION_V1.to_string()),
                source_high_water: Some(first.target_source_high_water),
                older_cursor: None,
            },
        );
        let page = frozen.page.expect("frozen page");
        assert_eq!(page.source_high_water, "5");
        assert_eq!(frozen.target_source_high_water, "5");
        assert!(page
            .blocks
            .iter()
            .all(|block| block.block_id != "message-6"));

        let patch = patches_until_ready(
            &store,
            fixture.log_path.as_path(),
            TranscriptPatchRpcRequestV1 {
                session_id: fixture.session_id.to_string(),
                projection_generation: Some(LOCAL_TRANSCRIPT_PROJECTION_GENERATION_V1.to_string()),
                after_source_high_water: "5".to_string(),
                through_source_high_water: None,
            },
        );
        assert_eq!(patch.target_source_high_water, "6");
        assert_eq!(patch.patches.len(), 1);
        assert_eq!(patch.patches[0].source_high_water, "6");
    }

    #[test]
    fn malformed_session_projection_does_not_poison_another_session() {
        let mut malformed = TranscriptFixture::new("failure", 0);
        let healthy = TranscriptFixture::new("identity-b", 1);
        let store = SqliteRuntimeStore::new(&healthy.database_path).expect("create shared store");
        malformed.append_invalid_user_record(1);
        let error = project_one_source_slice(
            &store,
            malformed.log_path.as_path(),
            malformed.session_id,
            LOCAL_TRANSCRIPT_PROJECTION_GENERATION_V1,
            1,
        )
        .expect_err("malformed session projection must fail");
        assert!(error.contains("decode transcript source sequence 1 failed"));

        let healthy_result = project_one_source_slice(
            &store,
            healthy.log_path.as_path(),
            healthy.session_id,
            LOCAL_TRANSCRIPT_PROJECTION_GENERATION_V1,
            1,
        )
        .expect("healthy session still projects");
        assert_eq!(healthy_result.projected_source_high_water, 1);
    }

    #[test]
    fn tombstone_rotates_generation_and_old_cursors_fail_explicitly() {
        let mut fixture = TranscriptFixture::new("tombstone", 1);
        let store = SqliteRuntimeStore::new(&fixture.database_path).expect("create store");
        let initial = page_until_ready(
            &store,
            fixture.log_path.as_path(),
            TranscriptPageRpcRequestV1 {
                session_id: fixture.session_id.to_string(),
                projection_generation: None,
                source_high_water: None,
                older_cursor: None,
            },
        );
        assert_eq!(
            initial.projection_generation,
            LOCAL_TRANSCRIPT_PROJECTION_GENERATION_V1
        );
        assert_eq!(initial.page.expect("initial page").blocks.len(), 1);

        fixture.append_tombstone(2, "event-1");
        let incremental = TranscriptBackgroundJobV1 {
            target_source_high_water: 2,
            projection_generation: LOCAL_TRANSCRIPT_PROJECTION_GENERATION_V1.to_string(),
            expected_current_generation: None,
            rebuild: false,
        };
        run_background_job_slice(
            &store,
            fixture.log_path.as_path(),
            fixture.session_id,
            &incremental,
        )
        .expect("commit invalidating tombstone");

        let old_cursor_error = page_with_store(
            &store,
            fixture.log_path.as_path(),
            TranscriptPageRpcRequestV1 {
                session_id: fixture.session_id.to_string(),
                projection_generation: Some(LOCAL_TRANSCRIPT_PROJECTION_GENERATION_V1.to_string()),
                source_high_water: Some("1".to_string()),
                older_cursor: None,
            },
        )
        .expect_err("invalidated generation cursor must fail");
        assert_eq!(
            old_cursor_error,
            TRANSCRIPT_PROJECTION_GENERATION_INVALIDATED
        );

        let pending = page_with_store(
            &store,
            fixture.log_path.as_path(),
            TranscriptPageRpcRequestV1 {
                session_id: fixture.session_id.to_string(),
                projection_generation: None,
                source_high_water: None,
                older_cursor: None,
            },
        )
        .expect("open replacement generation");
        assert!(!pending.target_reached);
        assert!(pending.page.is_none());
        assert_ne!(
            pending.projection_generation,
            LOCAL_TRANSCRIPT_PROJECTION_GENERATION_V1
        );
        let selection = select_projection_generation(
            &store,
            fixture.session_id,
            Some(pending.projection_generation.as_str()),
        )
        .expect("select replacement generation");
        run_background_job_slice(
            &store,
            fixture.log_path.as_path(),
            fixture.session_id,
            &TranscriptBackgroundJobV1 {
                target_source_high_water: 2,
                projection_generation: selection.projection_generation.clone(),
                expected_current_generation: selection.expected_current_generation,
                rebuild: selection.rebuild,
            },
        )
        .expect("rebuild and rotate generation");
        let rebuilt = page_with_store(
            &store,
            fixture.log_path.as_path(),
            TranscriptPageRpcRequestV1 {
                session_id: fixture.session_id.to_string(),
                projection_generation: Some(selection.projection_generation),
                source_high_water: Some("2".to_string()),
                older_cursor: None,
            },
        )
        .expect("read rebuilt generation");
        assert!(rebuilt.target_reached);
        assert!(rebuilt.page.expect("rebuilt page").blocks.is_empty());
    }

    fn persisted_transcript_fixture() -> TranscriptFixture {
        let mut fixture = TranscriptFixture::new("generation", 0);
        fixture.session_id = "session-1";
        fixture.log_path = fixture.root.join("session-1.jsonl");
        fs::write(
            &fixture.log_path,
            include_bytes!("../tests/fixtures/transcript/session.jsonl"),
        )
        .unwrap();
        let observations = fixture.root.join("session-1.observations");
        fs::create_dir(&observations).unwrap();
        fs::write(observations.join("manifest-608038e10859b6eeaef30ed2702ab70bcaec9ea9b5d9cd044f5fa9313d0e854b.json"),
            include_bytes!("../tests/fixtures/transcript/session-1.observations/manifest-608038e10859b6eeaef30ed2702ab70bcaec9ea9b5d9cd044f5fa9313d0e854b.json")).unwrap();
        fixture
    }

    #[test]
    fn current_store_preserves_history_sources_and_continuation() {
        use centaeris_core::session::external_context::ExternalContextStorePort;
        use centaeris_core::session::store::AgentRuntimeSnapshotStorePort;
        let mut fixture = persisted_transcript_fixture();
        let source = include_bytes!("../tests/fixtures/transcript/session.jsonl");
        let store = SqliteRuntimeStore::new(&fixture.database_path).expect("create current store");
        store
            .upsert_external_context_object(
                centaeris_core::session::external_context::ExternalContextObject {
                    schema_version: "external_context.v1".into(),
                    object_id: "source-object-1".into(),
                    object_kind: "text".into(),
                    source_provider_id: "fixture".into(),
                    source_tool_name: "read".into(),
                    title: "notice.md".into(),
                    content: "notice contents".into(),
                    metadata: json!({"fixture":"transcript"}),
                    updated_at_ms: 1,
                },
            )
            .unwrap();
        store
            .save_agent_runtime_snapshot("session-1", r#"{"fixture":"transcript"}"#, 1)
            .unwrap();
        drop(store);
        let request = || TranscriptPageRpcRequestV1 {
            session_id: "session-1".into(),
            projection_generation: None,
            source_high_water: None,
            older_cursor: None,
        };
        let store = SqliteRuntimeStore::new(&fixture.database_path).expect("open current store");
        let page = page_until_ready(&store, &fixture.log_path, request())
            .page
            .unwrap();
        assert!(page.blocks.iter().any(|block| matches!(&block.body,
            TranscriptBlockBodyV1::Reasoning { request_id, content, .. }
            if request_id == "model-request-1" && content.inline_content.as_deref() == Some("Upgrade fixture reasoning."))));
        assert!(page.blocks.iter().any(|block| matches!(&block.body,
            TranscriptBlockBodyV1::AssistantText { content, .. } if content.inline_content.as_deref() == Some("done"))));
        let output = page
            .blocks
            .iter()
            .find_map(|block| match &block.body {
                TranscriptBlockBodyV1::Tool {
                    call_id,
                    output_ref,
                    ..
                } if call_id == "call-1" => output_ref.clone(),
                _ => None,
            })
            .expect("tool output source reference");
        assert_eq!(output.ref_id, "tool-output:call-1");
        assert_eq!(output.byte_length, "15");
        assert_eq!(
            store
                .load_external_context_object("source-object-1")
                .unwrap()
                .unwrap()
                .content,
            "notice contents"
        );
        assert_eq!(
            store
                .load_agent_runtime_snapshot("session-1")
                .unwrap()
                .as_deref(),
            Some("{\"fixture\":\"transcript\"}")
        );
        assert_eq!(fs::read(&fixture.log_path).unwrap(), source);
        let document = message_log::read_session_document(&fixture.log_path)
            .expect("hydrate persisted observation manifest and records");
        let citation = document
            .records
            .iter()
            .find(|record| record.event_id == "evt-citation")
            .expect("retained citation");
        assert_eq!(citation.payload["sourceToolCallId"], "call-1");
        assert_eq!(citation.payload["ownerRef"], "source-object-1");
        assert_eq!(
            citation.payload["locator"],
            json!({"startLine": 1, "endLine": 8})
        );
        centaeris_core::session::restore_runtime_snapshot_from_session_records(
            "session-1",
            &document.records,
        )
        .expect("persisted conversation remains restorable for continuation");
        drop(store);
        let store = SqliteRuntimeStore::new(&fixture.database_path).expect("restart current store");
        assert_eq!(
            page_until_ready(&store, &fixture.log_path, request())
                .page
                .unwrap(),
            page
        );
        fixture.append_user_record(11);
        let continued = page_until_ready(&store, &fixture.log_path, request())
            .page
            .unwrap();
        assert_eq!(continued.blocks.len(), page.blocks.len() + 1);
        drop(store);
        let store =
            SqliteRuntimeStore::new(&fixture.database_path).expect("restart after continuation");
        assert_eq!(
            page_until_ready(&store, &fixture.log_path, request())
                .page
                .unwrap(),
            continued
        );
    }

    #[test]
    fn invalidated_projection_rebuilds_without_rewriting_source() {
        let mut fixture = persisted_transcript_fixture();
        let store = SqliteRuntimeStore::new(&fixture.database_path).expect("old store");
        let request = || TranscriptPageRpcRequestV1 {
            session_id: fixture.session_id.into(),
            projection_generation: None,
            source_high_water: None,
            older_cursor: None,
        };
        let before = page_until_ready(&store, &fixture.log_path, request())
            .page
            .unwrap();
        drop(store);
        let conn = rusqlite::Connection::open(&fixture.database_path).unwrap();
        conn.execute_batch("UPDATE transcript_projection_heads SET invalidation_reason='fixture_rebuild';
            UPDATE transcript_block_versions SET block_json = json_remove(json_set(block_json,
                '$.body.request_id', json_extract(block_json, '$.body.requestId')), '$.body.requestId')
                WHERE json_extract(block_json, '$.body.kind')='reasoning';").unwrap();
        drop(conn);
        let original = fs::read(&fixture.log_path).unwrap();
        let store =
            SqliteRuntimeStore::new(&fixture.database_path).expect("open invalidated store");
        let pending = page_with_store(&store, &fixture.log_path, request())
            .expect("invalidated, not decoded");
        assert!(pending.page.is_none());
        drop(store); // Restart before the rebuild has run.
        let store =
            SqliteRuntimeStore::new(&fixture.database_path).expect("restart pending rebuild");
        let rebuilt = page_until_ready(&store, &fixture.log_path, request());
        assert_ne!(
            rebuilt.projection_generation,
            LOCAL_TRANSCRIPT_PROJECTION_GENERATION_V1
        );
        assert_eq!(rebuilt.page.as_ref().unwrap().blocks, before.blocks);
        assert_eq!(fs::read(&fixture.log_path).unwrap(), original);
        drop(store);
        let store =
            SqliteRuntimeStore::new(&fixture.database_path).expect("reopen completed rebuild");
        let reopened = page_until_ready(&store, &fixture.log_path, request());
        assert_eq!(
            reopened.projection_generation,
            rebuilt.projection_generation
        );
        fixture.append_user_record(11);
        let continued = page_until_ready(
            &store,
            &fixture.log_path,
            TranscriptPageRpcRequestV1 {
                session_id: fixture.session_id.into(),
                projection_generation: None,
                source_high_water: None,
                older_cursor: None,
            },
        );
        assert_eq!(
            continued.page.unwrap().blocks.len(),
            before.blocks.len() + 1
        );
    }

    fn page_until_ready(
        store: &SqliteRuntimeStore,
        path: &Path,
        request: TranscriptPageRpcRequestV1,
    ) -> TranscriptPageRpcResponseV1 {
        for _ in 0..100 {
            let response = page_with_store(
                store,
                path,
                TranscriptPageRpcRequestV1 {
                    session_id: request.session_id.clone(),
                    projection_generation: request.projection_generation.clone(),
                    source_high_water: request.source_high_water.clone(),
                    older_cursor: request.older_cursor.clone(),
                },
            )
            .expect("page request");
            if response.page.is_some() {
                return response;
            }
            let target = response
                .target_source_high_water
                .parse::<u64>()
                .expect("target waterline");
            let selection = select_projection_generation(
                store,
                request.session_id.as_str(),
                Some(response.projection_generation.as_str()),
            )
            .expect("select background generation");
            run_background_job_slice(
                store,
                path,
                request.session_id.as_str(),
                &TranscriptBackgroundJobV1 {
                    target_source_high_water: target,
                    projection_generation: selection.projection_generation,
                    expected_current_generation: selection.expected_current_generation,
                    rebuild: selection.rebuild,
                },
            )
            .expect("background projection slice");
        }
        panic!("page projection did not reach source EOF");
    }

    fn patches_until_ready(
        store: &SqliteRuntimeStore,
        path: &Path,
        request: TranscriptPatchRpcRequestV1,
    ) -> TranscriptPatchRpcResponseV1 {
        for _ in 0..100 {
            let response = patches_with_store(
                store,
                path,
                TranscriptPatchRpcRequestV1 {
                    session_id: request.session_id.clone(),
                    projection_generation: request.projection_generation.clone(),
                    after_source_high_water: request.after_source_high_water.clone(),
                    through_source_high_water: request.through_source_high_water.clone(),
                },
            )
            .expect("patch request");
            if response.target_reached {
                return response;
            }
            let target = response
                .target_source_high_water
                .parse::<u64>()
                .expect("target waterline");
            let selection = select_projection_generation(
                store,
                request.session_id.as_str(),
                Some(response.projection_generation.as_str()),
            )
            .expect("select background generation");
            run_background_job_slice(
                store,
                path,
                request.session_id.as_str(),
                &TranscriptBackgroundJobV1 {
                    target_source_high_water: target,
                    projection_generation: selection.projection_generation,
                    expected_current_generation: selection.expected_current_generation,
                    rebuild: selection.rebuild,
                },
            )
            .expect("background projection slice");
        }
        panic!("patch projection did not reach target");
    }

    #[test]
    fn missing_presentation_is_read_from_source_without_rewriting_history() {
        let fixture = TranscriptFixture::new("active", 1);
        let mut file = OpenOptions::new()
            .append(true)
            .open(&fixture.log_path)
            .unwrap();
        for (sequence, kind, payload) in [
            (2, "agent_run_started", json!({"userObjective":"inspect"})),
            (
                3,
                "phase_event",
                json!({"stage":"model_process_summary","message":"Inspecting source"}),
            ),
            (
                4,
                "assistant_message",
                json!({"messageId":"message:turn-1:assistant","modelMarkdown":"Result","artifactRefs":[],"status":"done"}),
            ),
            (5, "agent_run_completed", json!({"doneReason":"finalized"})),
        ] {
            let record = SequencedSessionRecord { sequence, event:parse_event(&json!({
                "schemaVersion":"session.event.v1","eventVersion":1,"type":kind,"eventId":format!("event-{sequence}"),
                "sessionId":fixture.session_id,"turnId":"turn-1","agentRunId":"run-1","createdAtMs":sequence,"payload":payload
            })).unwrap() };
            serde_json::to_writer(&mut file, &wire_record_value(&record).unwrap()).unwrap();
            file.write_all(b"\n").unwrap();
        }
        file.sync_all().unwrap();
        drop(file);
        let store = SqliteRuntimeStore::new(&fixture.database_path).unwrap();
        let current = page_until_ready(
            &store,
            &fixture.log_path,
            TranscriptPageRpcRequestV1 {
                session_id: fixture.session_id.into(),
                projection_generation: None,
                source_high_water: None,
                older_cursor: None,
            },
        )
        .page
        .unwrap();
        let mut old = current.clone();
        old.blocks.retain(|block| !matches!(&block.body,TranscriptBlockBodyV1::Notice{notice_type,..} if notice_type == "run_boundary"));
        for block in &mut old.blocks {
            block.presentation = None;
        }
        let before = fs::read(&fixture.log_path).unwrap();
        let hydrated = page_with_read_only_presentation(&fixture.log_path, None, old).unwrap();
        assert_eq!(hydrated, current);
        assert_eq!(fs::read(&fixture.log_path).unwrap(), before);
        assert_eq!(
            store
                .load_current_transcript_page(TranscriptPageReadRequestV1 {
                    session_id: fixture.session_id.into(),
                    projection_version: TRANSCRIPT_PROJECTION_VERSION_V1.into(),
                    projection_generation: current.projection_generation.clone(),
                    source_high_water: current.source_high_water.clone(),
                    older_cursor: None,
                    policy: TranscriptPagePolicyV1::default(),
                })
                .unwrap()
                .page,
            current
        );
    }

    struct TranscriptFixture {
        root: PathBuf,
        database_path: PathBuf,
        log_path: PathBuf,
        session_id: &'static str,
    }

    impl TranscriptFixture {
        fn new(suffix: &'static str, records: u64) -> Self {
            Self::new_with_text(suffix, records, "bounded".to_string())
        }

        fn new_with_text(suffix: &'static str, records: u64, text: String) -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos();
            let root = std::env::temp_dir().join(format!(
                "centaeris-transcript-runtime-{suffix}-{}-{nonce}",
                std::process::id()
            ));
            fs::create_dir_all(&root).expect("create fixture root");
            let session_id = match suffix {
                "bounded" => "session-bounded",
                "page-patch" => "session-page-patch",
                "generation" => "session-generation",
                "identity-a" => "session-identity-a",
                "identity-b" => "session-identity-b",
                "empty" => "session-empty",
                "active" => "session-active",
                "failure" => "session-failure",
                "tombstone" => "session-tombstone",
                "oversized" => "session-oversized",
                _ => panic!("unknown fixture suffix"),
            };
            let log_path = root.join(format!("{session_id}.jsonl"));
            let database_path = root.join("runtime.db");
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&log_path)
                .expect("create source log");
            serde_json::to_writer(
                &mut file,
                &SessionManifestV1::new(session_id, 1, "test").expect("manifest"),
            )
            .expect("write manifest");
            file.write_all(b"\n").expect("manifest newline");
            for sequence in 1..=records {
                write_user_record(&mut file, session_id, sequence, text.as_str());
            }
            file.sync_all().expect("sync source log");
            Self {
                root,
                database_path,
                log_path,
                session_id,
            }
        }

        fn append_user_record(&mut self, sequence: u64) {
            let mut file = OpenOptions::new()
                .append(true)
                .open(&self.log_path)
                .expect("open source append");
            write_user_record(&mut file, self.session_id, sequence, "bounded");
            file.sync_all().expect("sync appended source record");
        }

        fn append_invalid_user_record(&mut self, sequence: u64) {
            let mut file = OpenOptions::new()
                .append(true)
                .open(&self.log_path)
                .expect("open invalid source append");
            serde_json::to_writer(
                &mut file,
                &json!({
                    "schemaVersion": "session.event.v1",
                    "eventVersion": 1,
                    "sequence": sequence,
                    "type": "user_message",
                    "eventId": format!("invalid-event-{sequence}"),
                    "sessionId": self.session_id,
                    "turnId": "turn-invalid",
                    "agentRunId": "run-invalid",
                    "createdAtMs": sequence,
                    "payload": {"attachments": []}
                }),
            )
            .expect("write invalid source record");
            file.write_all(b"\n").expect("invalid source newline");
            file.sync_all().expect("sync invalid source record");
        }

        fn append_tombstone(&mut self, sequence: u64, target_event_id: &str) {
            let mut file = OpenOptions::new()
                .append(true)
                .open(&self.log_path)
                .expect("open tombstone append");
            let record = SequencedSessionRecord {
                sequence,
                event: parse_event(&json!({
                    "schemaVersion": "session.event.v1",
                    "eventVersion": 1,
                    "type": "tombstone",
                    "eventId": format!("event-tombstone-{sequence}"),
                    "sessionId": self.session_id,
                    "turnId": format!("turn-{sequence}"),
                    "agentRunId": format!("run-{sequence}"),
                    "createdAtMs": sequence,
                    "payload": {
                        "tombstoneId": format!("tombstone-{sequence}"),
                        "targetEventIds": [target_event_id],
                        "reasonType": "rewrite"
                    }
                }))
                .expect("valid tombstone"),
            };
            serde_json::to_writer(
                file.by_ref(),
                &wire_record_value(&record).expect("tombstone wire"),
            )
            .expect("write tombstone");
            file.write_all(b"\n").expect("tombstone newline");
            file.sync_all().expect("sync tombstone");
        }
    }

    impl Drop for TranscriptFixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn write_user_record(file: &mut std::fs::File, session_id: &str, sequence: u64, text: &str) {
        let record = SequencedSessionRecord {
            sequence,
            event: parse_event(&json!({
                "schemaVersion": "session.event.v1",
                "eventVersion": 1,
                "type": "user_message",
                "eventId": format!("event-{sequence}"),
                "sessionId": session_id,
                "turnId": format!("turn-{sequence}"),
                "agentRunId": format!("run-{sequence}"),
                "createdAtMs": sequence,
                "payload": {
                    "messageId": format!("message-{sequence}"),
                    "text": text,
                    "attachments": []
                }
            }))
            .expect("valid source event"),
        };
        serde_json::to_writer(
            file.by_ref(),
            &wire_record_value(&record).expect("wire record"),
        )
        .expect("write source record");
        file.write_all(b"\n").expect("source newline");
    }
}
