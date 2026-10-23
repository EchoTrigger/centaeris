"""Resumable browser delivery of the Runtime-owned transcript projection.

The feed owns delivery, never execution. Only checkpoints acknowledge committed
projection water; navigation and ephemeral live snapshots carry no replay cursor.
"""
import asyncio
import base64
import binascii
import json
import hashlib
import time
from dataclasses import dataclass

from .runtime_client import TranscriptRuntimeError


SCHEMA = "workspace.presentation.v1"
CURSOR_SCHEMA = "workspace.presentation.cursor.v1"
PROJECTION_VERSION = "transcript.projection.v1"
CONTENT_CHUNK_BYTES = 65536
CONTENT_DISPLAY_BYTES = 16 * 1024 * 1024
MAX_FRAME_BYTES = 4 * 1024 * 1024
NAVIGATION_CHUNK_BYTES = 65536


@dataclass(frozen=True)
class PresentationScope:
    workspace_id: str
    agent_id: str
    session_id: str | None = None

    def wire(self):
        return {"workspaceId": self.workspace_id, "agentId": self.agent_id, "sessionId": self.session_id}


def _water(value):
    return (isinstance(value, str) and value.isascii() and value.isdigit()
            and (value == "0" or not value.startswith("0"))
            and len(value) <= 20 and int(value) <= 18_446_744_073_709_551_615)


def encode_cursor(scope, generation, water):
    if not isinstance(generation, str) or not generation or len(generation) > 160 or not _water(water):
        raise ValueError("presentation_cursor_invalid")
    body = {"schema": CURSOR_SCHEMA, **scope.wire(), "projectionGeneration": generation, "sourceHighWater": water}
    return "p1." + base64.urlsafe_b64encode(json.dumps(body, separators=(",", ":"), sort_keys=True).encode()).decode().rstrip("=")


def parse_cursor(scope, cursor):
    if not cursor:
        return None
    try:
        if len(cursor) > 2048 or not cursor.startswith("p1."):
            raise ValueError()
        raw = cursor[3:]
        body = json.loads(base64.b64decode(raw + "=" * (-len(raw) % 4), altchars=b"-_", validate=True))
        if (not isinstance(body, dict) or set(body) != {"schema", "workspaceId", "agentId", "sessionId", "projectionGeneration", "sourceHighWater"}
                or body["schema"] != CURSOR_SCHEMA or any(body[key] != value for key, value in scope.wire().items())
                or encode_cursor(scope, body["projectionGeneration"], body["sourceHighWater"]) != cursor):
            raise ValueError()
        return body
    except (ValueError, TypeError, KeyError, UnicodeDecodeError, binascii.Error) as error:
        raise ValueError("presentation_cursor_invalid") from error


def _encode(scope, kind, *, cursor=None, **payload):
    encoded = json.dumps({"schema": SCHEMA, "kind": kind, **scope.wire(), **payload}, ensure_ascii=False, separators=(",", ":"))
    if len(encoded.encode()) > MAX_FRAME_BYTES:
        raise RuntimeError("presentation_frame_budget_exceeded")
    return (f"id: {cursor}\n" if cursor else "") + f"data: {encoded}\n\n"


def _references(blocks):
    for block in blocks:
        body = block["body"]
        reference = body.get("summaryRef") if body["kind"] == "tool" else body["content"].get("sourceRef")
        # Full tool output is an independently requested detail, never eagerly
        # expanded merely because the containing turn entered the tail window.
        if reference is not None:
            if not reference["refId"].startswith("session-event:"):
                raise RuntimeError("presentation_body_reference_invalid")
            yield reference


def _navigation_frames(scope, sessions):
    digest = hashlib.sha256()
    for piece in json.JSONEncoder(sort_keys=True, separators=(",", ":"), ensure_ascii=False).iterencode(sessions):
        digest.update(piece.encode())
    snapshot_id = digest.hexdigest()
    offset, rows, size = 0, [], 2
    for session in sessions:
        row_size = len(json.dumps(session, ensure_ascii=False, separators=(",", ":")).encode()) + 1
        if row_size > NAVIGATION_CHUNK_BYTES:
            raise RuntimeError("presentation_navigation_row_budget_exceeded")
        if rows and size + row_size > NAVIGATION_CHUNK_BYTES:
            yield _encode(scope, "navigation", snapshotId=snapshot_id, offset=offset, hasMore=True, sessions=rows)
            offset += len(rows)
            rows, size = [], 2
        rows.append(session)
        size += row_size
    yield _encode(scope, "navigation", snapshotId=snapshot_id, offset=offset, hasMore=False, sessions=rows)


async def _bounded(operation, timeout):
    async with asyncio.timeout(timeout):
        return await operation


async def _projection_read(operation, timeout):
    # Keep the originally frozen source water during projection catch-up.
    async with asyncio.timeout(timeout):
        while True:
            try:
                return await operation()
            except TranscriptRuntimeError as error:
                if error.payload.get("error") != "transcript_projection_not_ready":
                    raise
                await asyncio.sleep(.05)


async def _tail_read(loader, water, timeout):
    generation = None

    async def read():
        nonlocal generation
        try:
            return await loader.page(water, generation)
        except TranscriptRuntimeError as error:
            if error.payload.get("error") == "transcript_projection_not_ready":
                progress = error.payload
                if (progress.get("sourceHighWater") != water
                        or (generation is not None and generation != progress.get("projectionGeneration"))):
                    raise RuntimeError("presentation_frozen_projection_changed") from error
                generation = progress["projectionGeneration"]
            raise

    return await _projection_read(read, timeout)


async def _produce(scope, loader, cursor, read_timeout):
    generation = None
    water = "0"
    needs_snapshot = scope.session_id is not None
    reset_reason = None
    try:
        restored = parse_cursor(scope, cursor)
        if restored is not None:
            generation = restored["projectionGeneration"]
            water = restored["sourceHighWater"]
            needs_snapshot = False
    except ValueError:
        reset_reason = "cursorInvalid"
    navigation = None
    last_state = None
    last_live = object()
    last_versions = None
    last_view_revision = None
    sent_references = set()
    sent_operations = set()

    async def operation_frames(blocks, through):
        for block in blocks:
            body = block["body"]
            if body["kind"] != "tool":
                continue
            details = await _bounded(loader.operations(body["callId"], through), read_timeout)
            for detail in details:
                identity = detail["sourceEventId"]
                if identity in sent_operations:
                    continue
                yield _encode(scope, "operation", detail=detail)
                if len(sent_operations) >= 2048:
                    sent_operations.clear()
                sent_operations.add(identity)

    async def content_frames(blocks):
        for reference in _references(blocks):
            identity = (generation, reference["refId"], reference["revision"], reference["byteLength"])
            if identity in sent_references:
                continue
            length = int(reference["byteLength"])
            offset, limit = 0, min(length, CONTENT_DISPLAY_BYTES)
            while offset < limit:
                # A display prefix may finish at the preceding whole UTF-8
                # codepoint (at most three bytes below the byte budget).
                if length > limit and offset >= limit - 3:
                    break
                item = await _bounded(loader.content(generation, reference, str(offset)), read_timeout)
                end = int(item["endOffset"])
                if item["startOffset"] != str(offset) or not offset < end <= length or end - offset > CONTENT_CHUNK_BYTES:
                    raise RuntimeError("presentation_content_range_invalid")
                if end > limit:
                    prefix = item["content"].encode()[:limit - offset].decode("utf-8", errors="ignore")
                    end = offset + len(prefix.encode())
                    if end <= offset:
                        raise RuntimeError("presentation_content_utf8_boundary_invalid")
                    item = {**item, "content": prefix, "endOffset": str(end), "hasMore": end < length}
                yield _encode(scope, "content", range=item)
                offset = end
            # Bound bookkeeping as well as the individual streamed chunk.
            if len(sent_references) >= 2048:
                sent_references.clear()
            sent_references.add(identity)

    while True:
        versions = await _bounded(loader.revisions(), read_timeout)
        if last_versions is None or versions["navigation"] != last_versions["navigation"]:
            current = await _bounded(loader.navigation(), read_timeout)
            if current != navigation:
                for frame in _navigation_frames(scope, current):
                    yield frame
                navigation = current
        if scope.session_id is not None and (last_versions is None or versions["state"] != last_versions["state"]):
            state = await _bounded(loader.state(), read_timeout)
            target = state["sourceHighWater"]
            if not _water(target):
                raise RuntimeError("presentation_authority_water_invalid")
            changed = state != last_state or needs_snapshot
            if changed:
                # This authority observation is useful even if the Runtime read
                # then fails. It has no id and cannot acknowledge missing text.
                yield _encode(scope, "checkpoint", projectionGeneration=generation,
                              sourceHighWater=target, agentRun=state["agentRun"])
                if int(water) > int(target):
                    reset_reason, needs_snapshot = "cursorInvalid", True
                if not needs_snapshot:
                    try:
                        after = water
                        while True:
                            page = await _projection_read(lambda: loader.patches(generation, after, target), read_timeout)
                            yield _encode(scope, "patches", page=page)
                            blocks = [block for item in page["patches"] for block in item["upserts"]]
                            async for frame in content_frames(blocks):
                                yield frame
                            async for frame in operation_frames(blocks, page["nextSourceHighWater"]):
                                yield frame
                            water = page["nextSourceHighWater"]
                            yield _encode(scope, "checkpoint", cursor=encode_cursor(scope, generation, water),
                                          projectionGeneration=generation, sourceHighWater=water, agentRun=state["agentRun"])
                            if not page["hasMore"]:
                                break
                            if int(water) <= int(after):
                                raise RuntimeError("presentation_patch_water_did_not_advance")
                            after = water
                    except TranscriptRuntimeError as error:
                        if error.payload.get("error") != "transcript_view_invalidated":
                            raise
                        reset_reason, needs_snapshot = "projectionInvalidated", True
                if needs_snapshot:
                    if reset_reason is not None:
                        yield _encode(scope, "reset", reason=reset_reason)
                        reset_reason = None
                    page = await _tail_read(loader, target, read_timeout)
                    generation = page["projectionGeneration"]
                    water = page["sourceHighWater"]
                    sent_references.clear()
                    sent_operations.clear()
                    yield _encode(scope, "snapshot", page=page)
                    async for frame in content_frames(page["blocks"]):
                        yield frame
                    async for frame in operation_frames(page["blocks"], water):
                        yield frame
                    yield _encode(scope, "checkpoint", cursor=encode_cursor(scope, generation, water),
                                  projectionGeneration=generation, sourceHighWater=water, agentRun=state["agentRun"])
                    needs_snapshot = False
                last_state = state
        run = last_state["agentRun"] if last_state is not None else None
        if run is not None and run["status"] in {"queued", "running"}:
            live = await _bounded(loader.live(run["agentRunId"]), read_timeout)
            if live != last_live:
                yield _encode(scope, "live", agentRunId=run["agentRunId"], live=live)
                last_live = live
        view_revision = {f"{kind}Revision": str(versions[kind]) for kind in ("context", "history", "work")}
        if view_revision != last_view_revision:
            yield _encode(scope, "viewRevision", **view_revision)
            last_view_revision = view_revision
        last_versions = versions
        await loader.wait_for_change(versions)


async def stream_presentation(scope, loader, cursor="", *, heartbeat_seconds=2.0,
                              authority_seconds=5.0, read_timeout_seconds=5.0):
    """One pending frame gives natural backpressure; disconnect cancels only reads.

    The outer owner checks authority and sends heartbeat comments independently
    of producer waits. Every producer operation also has a finite deadline.
    """
    producer = _produce(scope, loader, cursor, read_timeout_seconds)
    pending = None
    next_authority = 0.0
    try:
        while True:
            now = time.monotonic()
            if now >= next_authority:
                if await _bounded(loader.authorized(), read_timeout_seconds) is not True:
                    return
                next_authority = now + authority_seconds
            if pending is None:
                pending = asyncio.create_task(anext(producer))
            done, _ = await asyncio.wait({pending}, timeout=min(heartbeat_seconds, max(0.001, next_authority - now)))
            if not done:
                yield ": keepalive\n\n"
                continue
            try:
                frame = pending.result()
            except StopAsyncIteration:
                return
            pending = None
            # A revoked grant cannot continue flushing an already prepared page.
            if await _bounded(loader.authorized(), read_timeout_seconds) is not True:
                return
            yield frame
    finally:
        if pending is not None:
            pending.cancel()
            await asyncio.gather(pending, return_exceptions=True)
        await producer.aclose()
