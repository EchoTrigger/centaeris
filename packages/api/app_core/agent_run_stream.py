import asyncio
import base64
import binascii
import json
import logging
import re
import time
from collections.abc import Awaitable, Callable

import redis
import redis.asyncio as async_redis
from asgiref.sync import sync_to_async
from django.conf import settings

from .models import AgentRun, SessionEvent


STREAM_ITEM_SCHEMA = "session.stream.item.v1"
CURSOR_SCHEMA = "session.stream.cursor.v1"
CURSOR_PREFIX = "v1."
CURSOR_MAX_LENGTH = 512
SIGNAL_SCHEMA = "agent_run.transient.signal.v1"
SIGNAL_FIELD = "signal"
SIGNAL_FIELDS = {
    "commit_wake": {"schema", "kind", "agentRunId", "highWaterSequence"},
    "live": {
        "schema",
        "kind",
        "agentRunId",
        "afterSequence",
        "revision",
        "turnId",
        "messageId",
        "text",
    },
}
TERMINAL_EVENT_TYPES = {"agent_run_completed", "agent_run_failed", "agent_run_interrupted"}
OVERLAY_BARRIER_EVENT_TYPES = {"phase_event", "assistant_message"}
CURSOR_PAYLOAD = re.compile(r"^[A-Za-z0-9_-]+$")
POSTGRES_BATCH_SIZE = 100
POSTGRES_RECONCILE_SECONDS = 5.0
REDIS_RETRY_SECONDS = 1.0
AUTHORITY_RECHECK_SECONDS = 5.0
AUTHORITY_CHECK_TIMEOUT_SECONDS = 5.0

logger = logging.getLogger(__name__)

_redis_sync_pool = redis.ConnectionPool.from_url(
    settings.REDIS_URL,
    decode_responses=True,
    max_connections=settings.REDIS_BROWSER_SYNC_MAX_CONNECTIONS,
)
_redis_sync_client = redis.Redis(connection_pool=_redis_sync_pool)
_redis_stream_pool = async_redis.ConnectionPool.from_url(
    settings.REDIS_URL,
    decode_responses=True,
    max_connections=settings.REDIS_BROWSER_LIVE_MAX_CONNECTIONS,
)
_redis_stream_client = async_redis.Redis(connection_pool=_redis_stream_pool)


class AgentRunStreamUnavailable(RuntimeError):
    pass


def agent_run_signal_key(agent_run_id: str) -> str:
    return f"workspace-agent:agent-run:{{{agent_run_id}}}:signals"


def encode_stream_cursor(agent_run_id: str, source_sequence: int) -> str:
    if (
        not isinstance(source_sequence, int)
        or isinstance(source_sequence, bool)
        or source_sequence < 0
    ):
        raise ValueError("source_sequence_invalid")
    if source_sequence == 0:
        return "0-0"
    payload = json.dumps(
        {
            "schema": CURSOR_SCHEMA,
            "agentRunId": agent_run_id,
            "sourceSequence": source_sequence,
        },
        ensure_ascii=False,
        separators=(",", ":"),
        sort_keys=True,
    ).encode("utf-8")
    return CURSOR_PREFIX + base64.urlsafe_b64encode(payload).decode("ascii").rstrip("=")


def parse_last_event_cursor(value: str, agent_run_id: str) -> int:
    if not value or value == "0-0":
        return 0
    if (
        len(value) > CURSOR_MAX_LENGTH
        or not value.startswith(CURSOR_PREFIX)
        or CURSOR_PAYLOAD.fullmatch(value[len(CURSOR_PREFIX) :]) is None
    ):
        raise ValueError("last_event_id_invalid")
    encoded = value[len(CURSOR_PREFIX) :]
    try:
        raw = base64.urlsafe_b64decode(encoded + "=" * (-len(encoded) % 4))
        payload = json.loads(raw.decode("utf-8"))
    except (binascii.Error, UnicodeDecodeError, ValueError, json.JSONDecodeError) as error:
        raise ValueError("last_event_id_invalid") from error
    if not isinstance(payload, dict) or set(payload) != {
        "schema",
        "agentRunId",
        "sourceSequence",
    }:
        raise ValueError("last_event_id_invalid")
    source_sequence = payload["sourceSequence"]
    if (
        payload["schema"] != CURSOR_SCHEMA
        or payload["agentRunId"] != agent_run_id
        or not isinstance(source_sequence, int)
        or isinstance(source_sequence, bool)
        or source_sequence <= 0
        or encode_stream_cursor(agent_run_id, source_sequence) != value
    ):
        raise ValueError("last_event_id_invalid")
    return source_sequence


def load_session_high_water(session_id: str) -> int:
    return (
        SessionEvent.objects.filter(session_id=session_id)
        .order_by("-sequence")
        .values_list("sequence", flat=True)
        .first()
        or 0
    )


def require_cursor_not_future(agent_run: AgentRun, source_sequence: int) -> None:
    if source_sequence > load_session_high_water(agent_run.session_id):
        raise ValueError("last_event_id_invalid")


def _live_state(meta: dict, text: str | None) -> dict | None:
    if not meta:
        return None
    if set(meta) - {"reasoning"} != {"messageId", "turnId", "afterSequence", "revision"}:
        raise ValueError("agent_run_live_state_invalid")
    try:
        after_sequence = int(meta["afterSequence"])
        revision = int(meta["revision"])
    except (KeyError, TypeError, ValueError) as error:
        raise ValueError("agent_run_live_state_invalid") from error
    if (
        after_sequence < 0
        or revision < 0
        or not meta["messageId"]
        or not meta["turnId"]
    ):
        raise ValueError("agent_run_live_state_invalid")
    if revision == 0:
        return None
    return {
        "messageId": meta["messageId"],
        "turnId": meta["turnId"],
        "afterSequence": after_sequence,
        "revision": revision,
        "text": text if isinstance(text, str) else "",
        **({"reasoning": _validate_reasoning(json.loads(meta["reasoning"]))} if "reasoning" in meta else {}),
    }


def _validate_reasoning(value):
    if value is None:
        return None
    if not isinstance(value, dict) or set(value) != {"blockId", "requestId", "text"}:
        raise ValueError("agent_run_live_reasoning_invalid")
    if not isinstance(value["requestId"], str) or not value["requestId"].strip() or value["blockId"] != "reasoning:" + value["requestId"] or not isinstance(value["text"], str):
        raise ValueError("agent_run_live_reasoning_invalid")
    return value


def load_live_text_state(agent_run_id: str) -> dict | None:
    live_meta_key = f"workspace-agent:agent-run:{{{agent_run_id}}}:live:meta"
    live_text_key = f"workspace-agent:agent-run:{{{agent_run_id}}}:live:text"
    try:
        with _redis_sync_client.pipeline(transaction=True) as pipeline:
            meta, text = pipeline.hgetall(live_meta_key).get(live_text_key).execute()
        return _live_state(meta, text)
    except ValueError as error:
        raise AgentRunStreamUnavailable(str(error)) from error
    except redis.RedisError as error:
        raise AgentRunStreamUnavailable("agent_run_live_state_unavailable") from error


async def _load_live_text_state_async(agent_run_id: str) -> dict | None:
    live_meta_key = f"workspace-agent:agent-run:{{{agent_run_id}}}:live:meta"
    live_text_key = f"workspace-agent:agent-run:{{{agent_run_id}}}:live:text"
    async with _redis_stream_client.pipeline(transaction=True) as pipeline:
        meta, text = await pipeline.hgetall(live_meta_key).get(live_text_key).execute()
    return _live_state(meta, text)


async def _capture_signal_tail(agent_run_id: str) -> str:
    entries = await _redis_stream_client.xrevrange(
        agent_run_signal_key(agent_run_id), max="+", min="-", count=1
    )
    return entries[0][0] if entries else "0-0"


def _decode_signal(agent_run: AgentRun, fields: dict) -> dict:
    if set(fields) != {SIGNAL_FIELD}:
        raise RuntimeError("Redis AgentRun signal fields mismatch")
    raw = fields.get(SIGNAL_FIELD)
    if not isinstance(raw, str):
        raise RuntimeError("Redis AgentRun signal is missing")
    try:
        signal = json.loads(raw)
    except json.JSONDecodeError as error:
        raise RuntimeError("Redis AgentRun signal is invalid JSON") from error
    if not isinstance(signal, dict) or signal.get("kind") not in SIGNAL_FIELDS:
        raise RuntimeError("Redis AgentRun signal kind is unsupported")
    if set(signal) - ({"reasoning"} if signal["kind"] == "live" else set()) != SIGNAL_FIELDS[signal["kind"]]:
        raise RuntimeError("Redis AgentRun signal fields mismatch")
    if signal["schema"] != SIGNAL_SCHEMA or signal["agentRunId"] != agent_run.id:
        raise RuntimeError("Redis AgentRun signal binding mismatch")
    if signal["kind"] == "commit_wake":
        value = signal["highWaterSequence"]
        if not isinstance(value, int) or isinstance(value, bool) or value <= 0:
            raise RuntimeError("Redis commit wake highWaterSequence is invalid")
        return signal
    for field, minimum in (("afterSequence", 0), ("revision", 1)):
        value = signal[field]
        if not isinstance(value, int) or isinstance(value, bool) or value < minimum:
            raise RuntimeError(f"Redis live signal {field} is invalid")
    for field in ("turnId", "messageId", "text"):
        if not isinstance(signal[field], str) or (field != "text" and not signal[field]):
            raise RuntimeError(f"Redis live signal {field} is invalid")
    if "reasoning" in signal:
        _validate_reasoning(signal["reasoning"])
    return signal


@sync_to_async(thread_sensitive=True)
def _load_postgres_page(
    agent_run: AgentRun, after_sequence: int
) -> tuple[list[dict], int]:
    high_water = load_session_high_water(agent_run.session_id)
    records = list(
        SessionEvent.objects.filter(
            session_id=agent_run.session_id,
            agent_run_id=agent_run.id,
            projects_to_agent_run_stream=True,
            sequence__gt=after_sequence,
        )
        .order_by("sequence")
        .values("sequence", "eventId", "payload")[:POSTGRES_BATCH_SIZE]
    )
    if records:
        high_water = max(high_water, records[-1]["sequence"])
    return records, high_water


@sync_to_async(thread_sensitive=True)
def _load_terminal_sequence(agent_run_id: str) -> int | None:
    sequence = (
        SessionEvent.objects.filter(
            agent_run_id=agent_run_id,
            projects_to_agent_run_stream=True,
            payload__type__in=TERMINAL_EVENT_TYPES,
        )
        .order_by("sequence")
        .values_list("sequence", flat=True)
        .first()
    )
    if sequence is None and AgentRun.objects.filter(id=agent_run_id, status__in={"failed", "cancelled"}).exists():
        # Admission can fail before any semantic event exists. Close the transport;
        # its next HTTP request reports the admission failure without fabricating events.
        if not SessionEvent.objects.filter(agent_run_id=agent_run_id, payload__type="user_message").exists():
            return 0
    return sequence



def advance_overlay_barrier(barriers: dict[str, int], event: dict, sequence: int) -> None:
    if event.get("type") not in OVERLAY_BARRIER_EVENT_TYPES:
        return
    turn_id = event.get("turnId")
    if not isinstance(turn_id, str) or not turn_id:
        raise RuntimeError("committed overlay barrier identity is invalid")
    barriers[turn_id] = max(barriers.get(turn_id, 0), sequence)


def live_overlay_is_superseded(live_state: dict, barriers: dict[str, int]) -> bool:
    return live_state["afterSequence"] < barriers.get(live_state["turnId"], 0)


@sync_to_async(thread_sensitive=True)
def _load_committed_overlay_projection(
    agent_run: AgentRun, turn_id: str, message_id: str
) -> tuple[int, bool]:
    barrier = (
        SessionEvent.objects.filter(
            session_id=agent_run.session_id,
            agent_run_id=agent_run.id,
            projects_to_agent_run_stream=True,
            payload__turnId=turn_id,
            payload__type__in=OVERLAY_BARRIER_EVENT_TYPES,
        )
        .order_by("-sequence")
        .values_list("sequence", flat=True)
        .first()
        or 0
    )
    sealed_rows = list(
        SessionEvent.objects.filter(
            session_id=agent_run.session_id,
            agent_run_id=agent_run.id,
            projects_to_agent_run_stream=True,
            payload__type="assistant_message",
            payload__payload__messageId=message_id,
        )
        .order_by("sequence")
        .values_list("payload", flat=True)[:2]
    )
    if len(sealed_rows) > 1 or (
        sealed_rows and sealed_rows[0].get("turnId") != turn_id
    ):
        raise RuntimeError("live assistant message identity conflicts with committed history")
    return barrier, bool(sealed_rows)


def _committed_item(agent_run: AgentRun, record: dict) -> dict:
    sequence = record["sequence"]
    event = record["payload"]
    if (
        not isinstance(sequence, int)
        or isinstance(sequence, bool)
        or sequence <= 0
        or not isinstance(event, dict)
        or event.get("sequence") != sequence
        or event.get("eventId") != record["eventId"]
        or event.get("sessionId") != agent_run.session_id
        or event.get("agentRunId") != agent_run.id
    ):
        raise RuntimeError("PostgreSQL session stream event binding mismatch")
    return {
        "schema": STREAM_ITEM_SCHEMA,
        "kind": "committed",
        "agentRunId": agent_run.id,
        "sourceSequence": sequence,
        "event": event,
    }


async def _drain_postgres(agent_run: AgentRun, state: dict):
    while True:
        records, high_water = await _load_postgres_page(
            agent_run, state["source_sequence"]
        )
        state["session_high_water"] = high_water
        for record in records:
            item = _committed_item(agent_run, record)
            sequence = item["sourceSequence"]
            if sequence <= state["source_sequence"]:
                raise RuntimeError("PostgreSQL session stream sequence did not advance")
            state["source_sequence"] = sequence
            event = item["event"]
            advance_overlay_barrier(state["overlay_barriers"], event, sequence)
            if event.get("type") == "assistant_message":
                payload = event.get("payload")
                if not isinstance(payload, dict):
                    raise RuntimeError("committed assistant payload is invalid")
                identity = (event.get("turnId"), payload.get("messageId"))
                if not all(isinstance(value, str) and value for value in identity):
                    raise RuntimeError("committed assistant identity is invalid")
                state["sealed_live_identities"].add(identity)
            yield _encode_sse(
                item,
                encode_stream_cursor(agent_run.id, sequence),
            )
            if event.get("type") in TERMINAL_EVENT_TYPES:
                state["terminal"] = True
                return
        if len(records) < POSTGRES_BATCH_SIZE:
            terminal_sequence = await _load_terminal_sequence(agent_run.id)
            state["terminal"] = (
                terminal_sequence is not None
                and terminal_sequence <= state["source_sequence"]
            )
            return


async def _stream_live_candidate(agent_run: AgentRun, state: dict, live_state: dict):
    if live_state["afterSequence"] > state["session_high_water"]:
        async for item in _drain_postgres(agent_run, state):
            yield item
        if state["terminal"]:
            return
        if live_state["afterSequence"] > state["session_high_water"]:
            raise RuntimeError("live afterSequence exceeds PostgreSQL session high-water")

    if live_overlay_is_superseded(live_state, state["overlay_barriers"]):
        return

    identity = (live_state["turnId"], live_state["messageId"])
    if state["terminal"] or identity in state["sealed_live_identities"]:
        return
    if identity not in state["checked_live_identities"]:
        barrier, sealed = await _load_committed_overlay_projection(agent_run, *identity)
        state["overlay_barriers"][identity[0]] = max(
            state["overlay_barriers"].get(identity[0], 0), barrier
        )
        if live_overlay_is_superseded(live_state, state["overlay_barriers"]):
            return
        if sealed:
            state["sealed_live_identities"].add(identity)
            return
        state["checked_live_identities"].add(identity)
    if (
        identity == state["live_identity"]
        and live_state["revision"] <= state["live_revision"]
    ):
        return
    state["live_identity"] = identity
    state["live_revision"] = live_state["revision"]
    yield _encode_sse(
        {
            "schema": STREAM_ITEM_SCHEMA,
            "kind": "live",
            "agentRunId": agent_run.id,
            **live_state,
        }
    )


def _new_tail_state(source_sequence: int) -> dict:
    return {
        "source_sequence": source_sequence,
        "session_high_water": 0,
        "terminal": False,
        "sealed_live_identities": set(),
        "checked_live_identities": set(),
        "overlay_barriers": {},
        "live_identity": None,
        "live_revision": 0,
    }


def _consume_stream_task_result(task: asyncio.Task) -> None:
    try:
        task.result()
    except (asyncio.CancelledError, Exception):
        pass


async def _stream_authority_is_current(check: Callable[[], Awaitable[bool]]) -> bool:
    task = None
    try:
        task = asyncio.create_task(check())
        done, _ = await asyncio.wait({task}, timeout=AUTHORITY_CHECK_TIMEOUT_SECONDS)
        if not done:
            return False
        return task.result() is True
    except asyncio.CancelledError:
        raise
    except Exception as error:
        logger.warning(
            "Session stream authority check failed",
            extra={"exceptionType": type(error).__name__},
        )
        return False
    finally:
        # Do not let a failed or blocked authority service delay transport closure.
        if task is not None:
            if not task.done():
                task.cancel()
            task.add_done_callback(_consume_stream_task_result)


async def stream_agent_run_session_items_async(
    agent_run: AgentRun,
    source_sequence: int,
    *,
    authority_check: Callable[[], Awaitable[bool]] | None = None,
):
    source = _stream_agent_run_session_items_async(agent_run, source_sequence)
    if authority_check is None:
        try:
            async for item in source:
                yield item
        finally:
            await source.aclose()
        return

    next_item = None
    try:
        if not await _stream_authority_is_current(authority_check):
            return
        while True:
            if next_item is None:
                next_item = asyncio.create_task(anext(source))
            done, _ = await asyncio.wait({next_item}, timeout=AUTHORITY_RECHECK_SECONDS)
            if not done:
                if not await _stream_authority_is_current(authority_check):
                    return
                continue
            try:
                item = next_item.result()
            except StopAsyncIteration:
                return
            next_item = None
            # Recheck after loading each item, including terminal historical
            # backlog and busy live overlays, immediately before exposing it.
            if not await _stream_authority_is_current(authority_check):
                return
            yield item
    finally:
        if next_item is not None and not next_item.done():
            # Cancellation unwinds the producer even if Redis/PostgreSQL is
            # blocked. Its completion must not hold the response open.
            next_item.cancel()
            next_item.add_done_callback(_consume_stream_task_result)
        else:
            if next_item is not None:
                _consume_stream_task_result(next_item)
            await source.aclose()


async def _stream_agent_run_session_items_async(agent_run: AgentRun, source_sequence: int):
    state = _new_tail_state(source_sequence)
    if agent_run.status in {"completed", "failed", "cancelled"}:
        async for item in _drain_postgres(agent_run, state):
            yield item
        return

    redis_available = True
    try:
        redis_cursor = await _capture_signal_tail(agent_run.id)
    except redis.RedisError as error:
        redis_available = False
        redis_cursor = "0-0"
        logger.warning(
            "Redis AgentRun signals unavailable; using PostgreSQL polling",
            extra={"agentRunId": agent_run.id, "error": str(error)},
        )

    async for item in _drain_postgres(agent_run, state):
        yield item
    if state["terminal"]:
        return
    next_pg_reconcile_at = time.monotonic() + POSTGRES_RECONCILE_SECONDS

    if redis_available:
        try:
            live_state = await _load_live_text_state_async(agent_run.id)
        except redis.RedisError as error:
            redis_available = False
            logger.warning(
                "Redis AgentRun overlay unavailable; using PostgreSQL polling",
                extra={"agentRunId": agent_run.id, "error": str(error)},
            )
        else:
            if live_state is not None:
                async for item in _stream_live_candidate(agent_run, state, live_state):
                    yield item
                if state["terminal"]:
                    return

    while True:
        if not redis_available:
            await asyncio.sleep(REDIS_RETRY_SECONDS)
            async for item in _drain_postgres(agent_run, state):
                yield item
            if state["terminal"]:
                return
            next_pg_reconcile_at = time.monotonic() + POSTGRES_RECONCILE_SECONDS
            try:
                redis_cursor = await _capture_signal_tail(agent_run.id)
            except redis.RedisError:
                continue
            async for item in _drain_postgres(agent_run, state):
                yield item
            if state["terminal"]:
                return
            next_pg_reconcile_at = time.monotonic() + POSTGRES_RECONCILE_SECONDS
            try:
                live_state = await _load_live_text_state_async(agent_run.id)
            except redis.RedisError:
                continue
            redis_available = True
            logger.info(
                "Redis AgentRun signals reattached",
                extra={"agentRunId": agent_run.id},
            )
            if live_state is not None:
                async for item in _stream_live_candidate(agent_run, state, live_state):
                    yield item
                if state["terminal"]:
                    return
            continue

        remaining_seconds = next_pg_reconcile_at - time.monotonic()
        if remaining_seconds <= 0:
            async for item in _drain_postgres(agent_run, state):
                yield item
            if state["terminal"]:
                return
            next_pg_reconcile_at = time.monotonic() + POSTGRES_RECONCILE_SECONDS
            continue

        try:
            batches = await _redis_stream_client.xread(
                {agent_run_signal_key(agent_run.id): redis_cursor},
                count=100,
                block=max(1, min(1000, int(remaining_seconds * 1000))),
            )
        except asyncio.CancelledError:
            raise
        except redis.RedisError as error:
            redis_available = False
            logger.warning(
                "Redis AgentRun signals disconnected; using PostgreSQL polling",
                extra={"agentRunId": agent_run.id, "error": str(error)},
            )
            continue
        if not batches:
            continue

        saw_live_signal = False
        for _, entries in batches:
            for redis_id, fields in entries:
                signal = _decode_signal(agent_run, fields)
                redis_cursor = redis_id
                saw_live_signal = saw_live_signal or signal["kind"] == "live"

        async for item in _drain_postgres(agent_run, state):
            yield item
        if state["terminal"]:
            return
        next_pg_reconcile_at = time.monotonic() + POSTGRES_RECONCILE_SECONDS

        if saw_live_signal:
            try:
                live_state = await _load_live_text_state_async(agent_run.id)
            except redis.RedisError as error:
                redis_available = False
                logger.warning(
                    "Redis AgentRun overlay disconnected; using PostgreSQL polling",
                    extra={"agentRunId": agent_run.id, "error": str(error)},
                )
                continue
            if live_state is not None:
                async for item in _stream_live_candidate(agent_run, state, live_state):
                    yield item
                if state["terminal"]:
                    return


def _encode_sse(item: dict, cursor: str | None = None) -> str:
    data = json.dumps(item, ensure_ascii=False, separators=(",", ":"))
    return f"id: {cursor}\ndata: {data}\n\n" if cursor else f"data: {data}\n\n"
