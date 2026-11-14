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
from contextlib import aclosing
from dataclasses import dataclass

from .runtime_client import TranscriptRuntimeError
from .transcript_content_relay import MAX_CONTENT_REFERENCES


SCHEMA = "workspace.presentation.v1"
CURSOR_SCHEMA = "workspace.presentation.cursor.v1"
PROJECTION_VERSION = "transcript.projection.v1"
CONTENT_DISPLAY_BYTES = 16 * 1024 * 1024
MAX_FRAME_BYTES = 4 * 1024 * 1024
NAVIGATION_CHUNK_BYTES = 65536
PROJECTION_STALL_TIMEOUT_SECONDS = 30.0


@dataclass(frozen=True)
class PresentationScope:
    workspace_id: str
    session_id: str | None = None

    def wire(self):
        return {"workspaceId": self.workspace_id, "sessionId": self.session_id}


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
        if (not isinstance(body, dict) or set(body) != {"schema", "workspaceId", "sessionId", "projectionGeneration", "sourceHighWater"}
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


def _content_batches(blocks, generation, sent_references):
    batch = {}
    for reference in _references(blocks):
        ref_id = reference["refId"]
        identity = (generation, ref_id, reference["revision"], reference["byteLength"])
        if identity in sent_references or reference["byteLength"] == "0":
            continue
        if ref_id in batch and batch[ref_id][0] == identity:
            continue
        if ref_id in batch or len(batch) == MAX_CONTENT_REFERENCES:
            yield list(batch.values())
            batch = {}
        batch[ref_id] = (identity, reference)
    if batch:
        yield list(batch.values())


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
    # A read has its own deadline. Catch-up may take longer while committed
    # projection water advances toward the originally frozen target.
    stalled_until = time.monotonic() + PROJECTION_STALL_TIMEOUT_SECONDS
    projected = -1
    while True:
        remaining = stalled_until - time.monotonic()
        if remaining <= 0:
            raise TimeoutError("presentation_projection_stalled")
        try:
            return await _bounded(operation(), min(timeout, remaining))
        except TranscriptRuntimeError as error:
            if error.payload.get("error") != "transcript_projection_not_ready":
                raise
            # Runtime conflict payloads have already been validated against the
            # request's generation and target by runtime_client.
            next_projected = int(error.payload["projectedHighWater"])
            if next_projected > projected:
                projected = next_projected
                stalled_until = time.monotonic() + PROJECTION_STALL_TIMEOUT_SECONDS
                await asyncio.sleep(0)
            else:
                await asyncio.sleep(min(.05, max(0, stalled_until - time.monotonic())))


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
    last_activity = None
    last_question = object()
    last_live = object()
    live_cursor = None
    live_run = None
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
        for batch in _content_batches(blocks, generation, sent_references):
            references = [reference for _, reference in batch]
            async with aclosing(loader.content_stream(generation, references, CONTENT_DISPLAY_BYTES)) as ranges:
                while True:
                    try:
                        item = await _bounded(anext(ranges), read_timeout)
                    except StopAsyncIteration:
                        break
                    yield _encode(scope, "content", range=item)
            # A batch can only end cleanly after its completion marker and EOF.
            for identity, _ in batch:
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
            activity = state["activity"]
            if activity not in {None, "thinking", "working", "waitingInput", "retrying", "preserving"}:
                raise RuntimeError("presentation_activity_invalid")
            activity_identity = None if state["agentRun"] is None else (state["agentRun"]["agentRunId"], target, activity)
            activity_changed = (activity_identity is not None and activity_identity != last_activity
                and (activity is not None or last_activity is not None))
            last_activity = activity_identity
            question = state["question"]
            question_identity = None if state["agentRun"] is None else (state["agentRun"]["agentRunId"], question)
            question_changed = question_identity != last_question
            last_question = question_identity
            if changed:
                # This authority observation is useful even if the Runtime read
                # then fails. It has no id and cannot acknowledge missing text.
                yield _encode(scope, "checkpoint", projectionGeneration=generation,
                              sourceHighWater=target, agentRun=state["agentRun"])
                if activity_changed:
                    yield _encode(scope, "activity", agentRunId=activity_identity[0], sourceHighWater=target, activity=activity)
                if question_changed and question_identity is not None and state["agentRun"]["status"] in {"queued", "running"}:
                    yield _encode(scope, "question", agentRunId=question_identity[0], sourceHighWater=target, question=question)
                if int(water) > int(target):
                    reset_reason, needs_snapshot = "cursorInvalid", True
                if not needs_snapshot:
                    try:
                        after = water
                        while True:
                            page = await _projection_read(lambda: loader.patches(generation, after, target), read_timeout)
                            yield _encode(scope, "patches", page=page)
                            blocks = [block for item in page["patches"] for block in item["upserts"]]
                            async with aclosing(content_frames(blocks)) as frames:
                                async for frame in frames:
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
                        live_cursor = None
                        last_live = object()
                    page = await _tail_read(loader, target, read_timeout)
                    generation = page["projectionGeneration"]
                    water = page["sourceHighWater"]
                    sent_references.clear()
                    sent_operations.clear()
                    yield _encode(scope, "snapshot", page=page)
                    async with aclosing(content_frames(page["blocks"])) as frames:
                        async for frame in frames:
                            yield frame
                    async for frame in operation_frames(page["blocks"], water):
                        yield frame
                    yield _encode(scope, "checkpoint", cursor=encode_cursor(scope, generation, water),
                                  projectionGeneration=generation, sourceHighWater=water, agentRun=state["agentRun"])
                    needs_snapshot = False
                last_state = state
        run = last_state["agentRun"] if last_state is not None else None
        if run is not None and run["status"] in {"queued", "running"}:
            if live_run != run["agentRunId"]:
                live_cursor = None
                last_live = object()
                live_run = run["agentRunId"]
            live = await _bounded(loader.live(live_run, live_cursor), read_timeout)
            if live.kind is not None:
                marker = ((live_run, *(live.value[key] for key in ("turnId", "messageId", "afterSequence", "revision")))
                          if live.value is not None else (live_run, None))
                if marker != last_live:
                    payload = {"delta" if live.kind == "liveDelta" else "live": live.value}
                    yield _encode(scope, live.kind, agentRunId=live_run, **payload)
                    last_live = marker
                live_cursor = live.cursor
        else:
            live_cursor = None
            live_run = None
        view_revision = {f"{kind}Revision": str(versions[kind]) for kind in ("context", "history", "work", "subagents")}
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
