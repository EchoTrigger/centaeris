"""Bounded async transport for Runtime's presentation-only content batches."""
import json
from contextlib import aclosing, nullcontext

import httpx
from django.conf import settings

from .runtime_client import (
    TranscriptRuntimeError,
    _validate_transcript_conflict,
    validate_transcript_content_range,
)


MAX_CONTENT_REFERENCES = 128
MAX_CONTENT_RANGE_BYTES = 65_536
MAX_LINE_BYTES = 512 * 1024
REQUEST_SCHEMA = "runtime.transcript.content_stream.read.v1"
COMPLETE = {"schema": "runtime.transcript.content_stream.complete.v1"}
MEDIA_TYPE = "application/x-ndjson"


def _invalid():
    return RuntimeError("transcript_content_stream_invalid")


def _json_object(pairs):
    result = dict(pairs)
    if len(result) != len(pairs):
        raise _invalid()
    return result


async def _lines(response):
    line = bytearray()
    async for chunk in response.aiter_raw():
        start = 0
        while start < len(chunk):
            newline = chunk.find(b"\n", start)
            end = len(chunk) if newline == -1 else newline + 1
            if len(line) + end - start > MAX_LINE_BYTES:
                raise _invalid()
            line.extend(chunk[start:end])
            start = end
            if newline != -1:
                try:
                    item = json.loads(line.decode("utf-8"), object_pairs_hook=_json_object)
                except (ValueError, RecursionError) as error:
                    raise _invalid() from error
                line.clear()
                yield item
    if line:
        raise _invalid()


async def _require_stream(response, body):
    if response.headers.get("Content-Encoding", "identity").lower() != "identity":
        raise _invalid()
    if response.status_code == 409:
        raw = bytearray()
        async for chunk in response.aiter_raw():
            if len(raw) + len(chunk) > MAX_LINE_BYTES:
                raise _invalid()
            raw.extend(chunk)
        try:
            payload = json.loads(raw.decode("utf-8"), object_pairs_hook=_json_object)
        except (ValueError, RecursionError) as error:
            raise _invalid() from error
        if isinstance(payload, dict):
            conflict = _validate_transcript_conflict(payload, body)
            if conflict is not None:
                raise TranscriptRuntimeError(conflict)
    if response.status_code != 200:
        raise RuntimeError("transcript_runtime_request_failed")
    if response.headers.get("Content-Type", "").split(";", 1)[0].strip().lower() != MEDIA_TYPE:
        raise _invalid()


def _complete_prefix(reference, offset, cap):
    length = int(reference["byteLength"])
    return offset == length if length <= cap else cap - 3 <= offset <= cap


async def _ranges(response, body):
    references = body["references"]
    cap = body["maxDisplayBytes"]
    position = offset = 0
    complete = False
    binding = {key: body[key] for key in ("sessionId", "projectionVersion", "projectionGeneration")}
    async with aclosing(_lines(response)) as lines:
        async for item in lines:
            if complete or not isinstance(item, dict):
                raise _invalid()
            reference = references[position]
            if item == COMPLETE:
                if position != len(references) - 1 or not _complete_prefix(reference, offset, cap):
                    raise _invalid()
                complete = True
                continue
            if item.get("refId") != reference["refId"]:
                if position + 1 == len(references) or not _complete_prefix(reference, offset, cap):
                    raise _invalid()
                position += 1
                reference = references[position]
                offset = 0
            try:
                validate_transcript_content_range(item, {
                    **binding, **reference, "offset": str(offset), "maxBytes": MAX_CONTENT_RANGE_BYTES,
                })
            except UnicodeEncodeError as error:
                raise _invalid() from error
            end = int(item["endOffset"])
            if not offset < end <= cap:
                raise _invalid()
            offset = end
            yield item
    if not complete:
        raise _invalid()


def create_transcript_content_client():
    return httpx.AsyncClient(
        timeout=settings.RUNTIME_START_TIMEOUT_SECONDS, follow_redirects=False,
        # Presentation admission bounds concurrent streams. Bound idle sockets
        # separately so the pool adds no queue in front of the admitted streams.
        limits=httpx.Limits(max_connections=None, max_keepalive_connections=20),
    )


async def stream_transcript_content(session_id, projection_version, generation, references, max_display_bytes, *, client=None):
    body = {
        "schema": REQUEST_SCHEMA, "sessionId": session_id, "projectionVersion": projection_version,
        "projectionGeneration": generation, "references": references, "maxDisplayBytes": max_display_bytes,
    }
    headers = {"X-Internal-Token": settings.INTERNAL_API_TOKEN, "Accept": MEDIA_TYPE, "Accept-Encoding": "identity"}
    try:
        async with (nullcontext(client) if client is not None else create_transcript_content_client()) as client:
            async with client.stream(
                "POST", f"{settings.RUNTIME_URL.rstrip('/')}/internal/transcript/content-stream", json=body, headers=headers,
            ) as response:
                await _require_stream(response, body)
                async with aclosing(_ranges(response, body)) as ranges:
                    async for item in ranges:
                        yield item
    except httpx.HTTPError:
        raise RuntimeError("transcript_runtime_request_failed") from None
