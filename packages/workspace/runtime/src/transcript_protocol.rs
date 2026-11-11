use centaeris_core::session::transcript::{
    TranscriptPagePolicyV1, TranscriptPageReadRequestV1, TranscriptPageV1,
    TranscriptPatchReadRequestV1, TranscriptProjectionGenerationStorePortV1,
    TranscriptProjectionStorePort, TRANSCRIPT_PAGE_SCHEMA_V1,
    TRANSCRIPT_PROJECTION_GENERATION_INVALIDATED, TRANSCRIPT_PROJECTION_VERSION_V1,
};

use crate::postgres_store::PostgresRuntimeStore;
use bytes::Bytes;
use std::sync::Arc;
use std::time::{Duration, Instant};

// Return durable progress within a read, while every database batch retains its
// event/byte bounds. This is response scheduling, not a per-event time slice.
const CATCH_UP_RESPONSE_BUDGET: Duration = Duration::from_secs(1);
pub(crate) const REQUEST_DEADLINE_EXCEEDED: &str = "request deadline exceeded";

#[derive(Clone, Copy)]
pub(crate) enum ReadOperation {
    Page,
    Patches,
}

impl ReadOperation {
    pub(crate) async fn read(
        self,
        body: Bytes,
        store: Arc<PostgresRuntimeStore>,
        deadline: Option<Instant>,
    ) -> Result<(u16, Vec<u8>), String> {
        let read = TranscriptRead {
            body,
            store,
            deadline,
            catch_up_until: Instant::now() + CATCH_UP_RESPONSE_BUDGET,
        };
        match self {
            Self::Page => page(&read).await,
            Self::Patches => patches(&read).await,
        }
    }
}

struct TranscriptRead {
    body: Bytes,
    store: Arc<PostgresRuntimeStore>,
    deadline: Option<Instant>,
    catch_up_until: Instant,
}

impl TranscriptRead {
    async fn blocking<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&PostgresRuntimeStore) -> Result<T, String> + Send + 'static,
    ) -> Result<T, String> {
        let deadline = self.deadline;
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err(REQUEST_DEADLINE_EXCEEDED.to_string());
        }
        let body = self.body.clone();
        let store = self.store.clone();
        let task = tokio::task::spawn_blocking(move || {
            let _context = crate::observations::Context::enter(&body);
            let _deadline = crate::postgres_store::RequestDeadline::enter(deadline);
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return Err(REQUEST_DEADLINE_EXCEEDED.to_string());
            }
            operation(&store)
        });
        let result = if let Some(deadline) = deadline {
            tokio::select! {
                result = task => result,
                _ = tokio::time::sleep_until(deadline.into()) => {
                    // Dropping the handle cannot stop SQL already in flight.
                    // Its connection retains the same PostgreSQL request deadline.
                    return Err(REQUEST_DEADLINE_EXCEEDED.to_string());
                }
            }
        } else {
            task.await
        };
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err(REQUEST_DEADLINE_EXCEEDED.to_string());
        }
        result.map_err(|error| format!("transcript read task failed: {error}"))?
    }
}

const PAGE_REQUEST_SCHEMA: &str = "runtime.transcript.page.read.v1";
const PATCH_REQUEST_SCHEMA: &str = "runtime.transcript.patch.read.v1";
const PATCH_PAGE_SCHEMA: &str = "transcript.patch.page.v1";

pub(crate) mod content_stream;
mod wire;
use wire::*;

pub(crate) fn content(body: &[u8], store: &PostgresRuntimeStore) -> Result<(u16, Vec<u8>), String> {
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

async fn page(read: &TranscriptRead) -> Result<(u16, Vec<u8>), String> {
    let wire = match serde_json::from_slice::<PageRequest>(&read.body) {
        Ok(wire)
            if wire.schema == PAGE_REQUEST_SCHEMA
                && wire.projection_version == TRANSCRIPT_PROJECTION_VERSION_V1 =>
        {
            wire
        }
        Ok(_) | Err(_) => return json_error(400, ErrorCode::PageRequestInvalid),
    };
    let anchor_sequence = wire.anchor_sequence;
    if anchor_sequence.as_deref().is_some_and(|value| {
        wire.older_cursor.is_some()
            || value
                .parse::<u64>()
                .map_or(true, |number| number == 0 || number.to_string() != value)
    }) {
        return json_error(400, ErrorCode::PageRequestInvalid);
    }
    let opens_current_view = wire.projection_generation.is_none();
    let session_id = wire.session_id.clone();
    let current = read
        .blocking(move |store| store.load_or_initialize_current_transcript_generation(&session_id))
        .await?;
    if wire
        .projection_generation
        .as_deref()
        .is_some_and(|generation| generation != current.projection_generation)
    {
        return json_error(409, ErrorCode::ViewInvalidated);
    }
    let mut request = TranscriptPageReadRequestV1 {
        session_id: wire.session_id,
        projection_version: wire.projection_version,
        projection_generation: current.projection_generation,
        source_high_water: wire.source_high_water,
        older_cursor: wire.older_cursor,
        policy: TranscriptPagePolicyV1::default(),
    };
    if request.validate().is_err()
        || anchor_sequence.as_ref().is_some_and(|value| {
            value.parse::<u64>().unwrap() > request.source_high_water.parse::<u64>().unwrap()
        })
    {
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
        read,
        request.session_id.as_str(),
        request.projection_generation.as_str(),
        request.source_high_water.as_str(),
        opens_current_view,
    )
    .await?
    {
        return Ok(response);
    }
    if let Some(sequence) = anchor_sequence {
        let anchor_request = request.clone();
        let cursor = match read
            .blocking(move |store| store.transcript_anchor_cursor(&anchor_request, &sequence))
            .await
        {
            Ok(cursor) => cursor,
            Err(error) if is_invalidated_store_read(&error) => {
                return json_error(409, ErrorCode::ViewInvalidated)
            }
            Err(error) => return Err(error),
        };
        request.older_cursor = Some(cursor.ok_or("transcript anchor block is unavailable")?);
    }
    let result = match read
        .blocking(move |store| store.load_current_transcript_page(request))
        .await
    {
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

async fn patches(read: &TranscriptRead) -> Result<(u16, Vec<u8>), String> {
    let wire = match serde_json::from_slice::<PatchRequest>(&read.body) {
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
    let session_id = request.session_id.clone();
    let current = read
        .blocking(move |store| store.load_or_initialize_current_transcript_generation(&session_id))
        .await?;
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
        read,
        request.session_id.as_str(),
        request.projection_generation.as_str(),
        request.through_source_high_water.as_str(),
        false,
    )
    .await?
    {
        return Ok(response);
    }
    let stored_request = request.clone();
    let result = match read
        .blocking(move |store| store.load_current_transcript_patches(stored_request))
        .await
    {
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

async fn projection_gate(
    read: &TranscriptRead,
    session_id: &str,
    generation: &str,
    requested_high_water: &str,
    schedule_rebuild: bool,
) -> Result<Option<(u16, Vec<u8>)>, String> {
    let requested = requested_high_water
        .parse::<u64>()
        .map_err(|_| "validated transcript waterline failed to parse".to_string())?;
    let session = session_id.to_string();
    let view = generation.to_string();
    let head = read
        .blocking(move |store| store.load_transcript_projection_head(&session, &view))
        .await?;
    if head
        .as_ref()
        .is_some_and(|head| head.invalidation_reason.is_some())
    {
        return invalidated_view(read, session_id, generation, requested, schedule_rebuild).await;
    }
    let mut projected = head.map_or_else(|| "0".to_string(), |head| head.source_high_water);
    loop {
        let projected_value = projected
            .parse::<u64>()
            .map_err(|_| "stored transcript waterline failed to parse".to_string())?;
        if projected_value >= requested {
            return Ok(None);
        }
        if Instant::now() >= read.catch_up_until {
            return projection_progress(generation, requested_high_water, projected).map(Some);
        }
        let session = session_id.to_string();
        let view = generation.to_string();
        let progress = read
            .blocking(move |store| store.catch_up_transcript_projection(&session, &view, requested))
            .await;
        match progress {
            Ok(progress) => projected = progress.projected_high_water.to_string(),
            // Only report the last completed batch. The timed-out batch may
            // still commit; a later request will observe that durable head.
            Err(error) if error == REQUEST_DEADLINE_EXCEEDED => {
                return projection_progress(generation, requested_high_water, projected).map(Some);
            }
            Err(error) => {
                let session = session_id.to_string();
                let view = generation.to_string();
                let invalidated = read
                    .blocking(move |store| {
                        store
                            .load_transcript_projection_head(&session, &view)
                            .map(|head| head.is_some_and(|head| head.invalidation_reason.is_some()))
                    })
                    .await?;
                if invalidated {
                    return invalidated_view(
                        read,
                        session_id,
                        generation,
                        requested,
                        schedule_rebuild,
                    )
                    .await;
                }
                return Err(error);
            }
        }
        // Cancellation drops this future; it never schedules the following
        // batch. There is no detached catch-up loop or per-event task.
        tokio::task::yield_now().await;
    }
}

async fn invalidated_view(
    read: &TranscriptRead,
    session_id: &str,
    generation: &str,
    requested: u64,
    schedule_rebuild: bool,
) -> Result<Option<(u16, Vec<u8>)>, String> {
    if schedule_rebuild {
        let session = session_id.to_string();
        let view = generation.to_string();
        read.blocking(move |store| {
            store.schedule_transcript_generation_rebuild(&session, requested, &view)
        })
        .await?;
    }
    json_error(409, ErrorCode::ViewInvalidated).map(Some)
}

fn projection_progress(
    generation: &str,
    requested_high_water: &str,
    projected: String,
) -> Result<(u16, Vec<u8>), String> {
    let response = ProjectionNotReady {
        error: "transcript_projection_not_ready",
        projection_generation: generation,
        source_high_water: requested_high_water,
        projected_high_water: projected,
    };
    serde_json::to_vec(&response)
        .map(|body| (409, body))
        .map_err(|error| format!("encode transcript projection progress failed: {error}"))
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
    use super::*;

    #[test]
    fn transcript_read_cancel_and_deadline_do_not_start_another_batch() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            for cancel in [true, false] {
                let deadline = Instant::now()
                    + if cancel {
                        Duration::from_secs(10)
                    } else {
                        Duration::from_millis(100)
                    };
                let read = TranscriptRead {
                    body: Bytes::from_static(b"{}"),
                    store: Arc::new(PostgresRuntimeStore::rejecting_http_test_store()),
                    deadline: Some(deadline),
                    catch_up_until: deadline,
                };
                let (entered, entered_rx) = tokio::sync::oneshot::channel();
                let (finished, mut finished_rx) = tokio::sync::oneshot::channel();
                let (release, release_rx) = std::sync::mpsc::channel();
                let next_batch = Arc::new(std::sync::atomic::AtomicUsize::new(0));
                let started = next_batch.clone();
                let task = tokio::spawn(async move {
                    read.blocking(move |_| {
                        entered.send(()).unwrap();
                        release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                        finished.send(()).unwrap();
                        Ok(())
                    })
                    .await?;
                    read.blocking(move |_| {
                        started.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        Ok(())
                    })
                    .await
                });
                tokio::time::timeout(Duration::from_secs(3), entered_rx)
                    .await
                    .unwrap()
                    .unwrap();
                if cancel {
                    task.abort();
                    assert!(task.await.unwrap_err().is_cancelled());
                } else {
                    assert_eq!(task.await.unwrap().unwrap_err(), REQUEST_DEADLINE_EXCEEDED);
                }
                // A started blocking batch still owns its work after the async
                // caller leaves. Releasing it must never start the next batch.
                assert!(matches!(
                    finished_rx.try_recv(),
                    Err(tokio::sync::oneshot::error::TryRecvError::Empty)
                ));
                release.send(()).unwrap();
                tokio::time::timeout(Duration::from_secs(3), finished_rx)
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(next_batch.load(std::sync::atomic::Ordering::SeqCst), 0);
            }
        });
    }

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
