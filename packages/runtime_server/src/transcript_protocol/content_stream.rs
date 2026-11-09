//! Ordered display ranges pulled by the consumer, one source at a time.
use std::collections::HashSet;
use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::Response;
use bytes::Bytes;
use centaeris_core::session::transcript::{
    transcript_utf8_range, TranscriptContentRangeReadRequestV1, TranscriptContentRangeV1,
    TRANSCRIPT_CONTENT_RANGE_MAX_BYTES, TRANSCRIPT_CONTENT_RANGE_REQUEST_SCHEMA_V1,
    TRANSCRIPT_CONTENT_RANGE_SCHEMA_V1,
};
use serde::Deserialize;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::postgres_store::{PostgresRuntimeStore, RequestDeadline};

const MAX_REFERENCES: usize = 128;
const MAX_DISPLAY_BYTES: usize = 16 * 1024 * 1024;
const MAX_LINE_BYTES: usize = 512 * 1024;
pub(crate) const CONTENT_BUFFER_UNITS: usize = (256 * 1024 * 1024) / MAX_LINE_BYTES;
pub(crate) const MAX_READ_TIMEOUT: Duration = Duration::from_secs(4);
const REQUEST_SCHEMA: &str = "runtime.transcript.content_stream.read.v1";
const COMPLETE: &[u8] = b"{\"schema\":\"runtime.transcript.content_stream.complete.v1\"}\n";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ContentStreamRequest {
    schema: String,
    session_id: String,
    projection_version: String,
    projection_generation: String,
    references: Vec<Reference>,
    max_display_bytes: usize,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Reference {
    ref_id: String,
    revision: String,
    byte_length: String,
}

impl ContentStreamRequest {
    fn ranges(self) -> Result<Vec<TranscriptContentRangeReadRequestV1>, String> {
        let invalid = || "transcript_content_request_invalid".to_string();
        if self.schema != REQUEST_SCHEMA
            || !(4..=MAX_DISPLAY_BYTES).contains(&self.max_display_bytes)
            || !(1..=MAX_REFERENCES).contains(&self.references.len())
        {
            return Err(invalid());
        }
        let mut seen = HashSet::with_capacity(self.references.len());
        self.references
            .into_iter()
            .map(|reference| {
                if reference.byte_length == "0" || !seen.insert(reference.ref_id.clone()) {
                    return Err(invalid());
                }
                let range = TranscriptContentRangeReadRequestV1 {
                    schema: TRANSCRIPT_CONTENT_RANGE_REQUEST_SCHEMA_V1.into(),
                    session_id: self.session_id.clone(),
                    projection_version: self.projection_version.clone(),
                    projection_generation: self.projection_generation.clone(),
                    ref_id: reference.ref_id,
                    revision: reference.revision,
                    byte_length: reference.byte_length,
                    offset: "0".into(),
                    max_bytes: TRANSCRIPT_CONTENT_RANGE_MAX_BYTES as u32,
                };
                range.validate()?;
                range.event_reference()?;
                Ok(range)
            })
            .collect()
    }
}

pub(crate) fn response(
    body: &[u8],
    store: Arc<PostgresRuntimeStore>,
    slots: Arc<Semaphore>,
    read_timeout: Duration,
    observed_request: Option<crate::observations::Request>,
) -> Response<Body> {
    let Ok(request) = serde_json::from_slice::<ContentStreamRequest>(body) else {
        return crate::bounded_json_error_response(400, "transcript_content_request_invalid");
    };
    let max_display_bytes = request.max_display_bytes;
    let Ok(ranges) = request.ranges() else {
        return crate::bounded_json_error_response(400, "transcript_content_request_invalid");
    };
    // Keep a single source prefix and a serialized line per stream. A short
    // response must not reserve the same capacity as a 16 MiB display prefix.
    let largest_prefix = ranges
        .iter()
        .map(|range| {
            range
                .byte_length
                .parse::<u64>()
                .expect("validated byteLength")
        })
        .max()
        .unwrap_or(0)
        .min(max_display_bytes as u64) as usize;
    let units = largest_prefix.div_ceil(MAX_LINE_BYTES) + 1;
    let Ok(slot) = slots.try_acquire_many_owned(units as u32) else {
        return crate::bounded_json_error_response(503, "transcript_content_stream_busy");
    };
    let stream = ContentStream {
        store,
        slot: Arc::new(slot),
        ranges: ranges.into_iter(),
        max_display_bytes,
        read_timeout,
        active: None,
        complete: Some(Bytes::from_static(COMPLETE)),
        _observed_request: observed_request,
    };
    Response::builder()
        .header("content-type", "application/x-ndjson")
        .header("cache-control", "no-store")
        .body(Body::from_stream(futures::stream::try_unfold(
            stream,
            ContentStream::next,
        )))
        .expect("static transcript stream headers")
}

struct ActiveSource {
    range: TranscriptContentRangeReadRequestV1,
    prefix: String,
    witness: String,
    offset: usize,
    length: u64,
}

struct ContentStream {
    store: Arc<PostgresRuntimeStore>,
    slot: Arc<OwnedSemaphorePermit>,
    ranges: std::vec::IntoIter<TranscriptContentRangeReadRequestV1>,
    max_display_bytes: usize,
    read_timeout: Duration,
    active: Option<ActiveSource>,
    complete: Option<Bytes>,
    _observed_request: Option<crate::observations::Request>,
}

impl ContentStream {
    async fn read<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&PostgresRuntimeStore) -> Result<T, String> + Send + 'static,
    ) -> io::Result<T> {
        let deadline = Instant::now() + self.read_timeout;
        let store = self.store.clone();
        // A disconnected observer drops the stream. Any SQL already in flight
        // keeps its slot and deadline until the blocking read exits.
        let slot = self.slot.clone();
        let task = tokio::task::spawn_blocking(move || {
            let _slot = slot;
            let _deadline = RequestDeadline::enter(Some(deadline));
            if Instant::now() >= deadline {
                return Err("transcript content read deadline exceeded".into());
            }
            operation(&store)
        });
        tokio::time::timeout_at(deadline.into(), task)
            .await
            .map_err(|_| io::Error::other("transcript content read deadline exceeded"))?
            .map_err(|_| io::Error::other("transcript content read task failed"))?
            .map_err(|_| io::Error::other("transcript content unavailable"))
    }

    async fn next(mut self) -> io::Result<Option<(Bytes, Self)>> {
        if let Some(active) = &self.active {
            let range = active.range.clone();
            let witness = active.witness.clone();
            if !self
                .read(move |store| store.transcript_display_source_is_current(&range, &witness))
                .await?
            {
                return Err(io::Error::other("transcript content source changed"));
            }
        } else {
            let Some(range) = self.ranges.next() else {
                return Ok(self.complete.take().map(|line| (line, self)));
            };
            let request = range.clone();
            let max = self.max_display_bytes;
            let (prefix, witness) = self
                .read(move |store| store.open_transcript_display_content(&request, max))
                .await?;
            let length = range.byte_length.parse::<u64>().map_err(io::Error::other)?;
            self.active = Some(ActiveSource {
                range,
                prefix,
                witness,
                length,
                offset: 0,
            });
        }
        let active = self.active.as_mut().expect("active transcript source");
        let (end, content) = transcript_utf8_range(
            &active.prefix,
            active.offset,
            TRANSCRIPT_CONTENT_RANGE_MAX_BYTES,
        )
        .map_err(io::Error::other)?;
        if end <= active.offset {
            return Err(io::Error::other("transcript content range did not advance"));
        }
        let range = &active.range;
        let item = TranscriptContentRangeV1 {
            schema: TRANSCRIPT_CONTENT_RANGE_SCHEMA_V1.into(),
            session_id: range.session_id.clone(),
            projection_version: range.projection_version.clone(),
            projection_generation: range.projection_generation.clone(),
            ref_id: range.ref_id.clone(),
            revision: range.revision.clone(),
            byte_length: range.byte_length.clone(),
            start_offset: active.offset.to_string(),
            end_offset: end.to_string(),
            content: content.to_string(),
            has_more: (end as u64) < active.length,
        };
        let mut line = serde_json::to_vec(&item).map_err(io::Error::other)?;
        if line.len() >= MAX_LINE_BYTES {
            return Err(io::Error::other("transcript content line exceeds budget"));
        }
        line.push(b'\n');
        active.offset = end;
        if end == active.prefix.len() {
            self.active = None;
        }
        Ok(Some((Bytes::from(line), self)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn stream_admission_is_released_when_the_response_is_dropped() {
        let store = Arc::new(PostgresRuntimeStore::rejecting_http_test_store());
        let slots = Arc::new(Semaphore::new(2));
        let request = json!({
            "schema": REQUEST_SCHEMA, "sessionId": "session", "projectionVersion": "transcript.projection.v1",
            "projectionGeneration": "generation", "maxDisplayBytes": 65536,
            "references": [{"refId":"session-event:event:text", "revision":"1", "byteLength":"5"}]
        });
        let open = || {
            response(
                &serde_json::to_vec(&request).unwrap(),
                store.clone(),
                slots.clone(),
                MAX_READ_TIMEOUT,
                None,
            )
        };
        let stream = open();
        assert_eq!(stream.status(), 200);
        assert_eq!(open().status(), 503);
        drop(stream);
        assert_eq!(open().status(), 200);
    }

    #[test]
    fn stream_rejects_duplicate_references_and_private_output_fields() {
        let mut request = json!({
            "schema": REQUEST_SCHEMA, "sessionId": "session", "projectionVersion": "transcript.projection.v1",
            "projectionGeneration": "generation", "maxDisplayBytes": 65536,
            "references": [{"refId":"session-event:event:text", "revision":"1", "byteLength":"5"}]
        });
        let parse = |value| {
            serde_json::from_value::<ContentStreamRequest>(value)
                .unwrap()
                .ranges()
        };
        assert!(parse(request.clone()).is_ok());
        let reference = request["references"][0].clone();
        request["references"]
            .as_array_mut()
            .unwrap()
            .push(reference);
        assert!(parse(request.clone()).is_err());
        request["references"] =
            json!([{"refId":"tool-output:call", "revision":"2", "byteLength":"5"}]);
        assert!(parse(request).is_err());
    }
}
