"""Project a single accepted Run into its original, bounded response."""

import asyncio
import base64
import hashlib
import time
from datetime import timedelta

from django.utils import timezone

from .agent_run_stream import _committed_item, _load_postgres_page, _load_terminal_sequence, POSTGRES_BATCH_SIZE
from .http.storage_stream import open_storage_stream
from .http.stream_response import stream_database_call
from .models import AgentRun, Artifact, AgentRunApiOperation
from .agent_run_api_protocol import (
    ARTIFACT_CHUNK,
    ARTIFACT_COMPLETED,
    ARTIFACT_FAILED,
    ARTIFACT_STARTED,
    OUTPUT_TEXT,
    RUN_ACCEPTED,
    RUN_FINISHED,
    RUN_PROGRESS,
    ArtifactStarted,
    OperationReceipt,
    RunFinished,
)


TERMINAL_STATUSES = {"completed", "failed", "cancelled"}
DELIVERY_SECONDS = 60
POLL_SECONDS = 1
AUTHORITY_SECONDS = 5
READ_TIMEOUT_SECONDS = 5
TEXT_CHUNK_CHARACTERS = 16 * 1024


def operation_receipt(operation) -> OperationReceipt:
    receipt: OperationReceipt = {
        "operationId": operation.operationId,
        "runId": operation.agentRunId,
        "acceptedAt": operation.acceptedAt.isoformat(),
        "deadlineAt": operation.deadlineAt.isoformat(),
        "expiresAt": operation.expiresAt.isoformat(),
        "runStatus": operation.runStatus,
        "cleanupStatus": operation.cleanupStatus,
    }
    if operation.reasonCode:
        receipt["reasonCode"] = operation.reasonCode
    return receipt


@stream_database_call
def _run_state(operation_id):
    operation = AgentRunApiOperation.objects.get(pk=operation_id)
    run = AgentRun.objects.get(pk=operation.agentRunId)
    reason = run.transitionReason if run.status in {"failed", "cancelled"} else ""
    if run.status != operation.runStatus or reason != operation.reasonCode:
        operation.runStatus = run.status
        operation.reasonCode = reason
        operation.save(update_fields=["runStatus", "reasonCode"])
    if run.status in TERMINAL_STATUSES:
        expires = (run.completedAt or timezone.now()) + timedelta(seconds=DELIVERY_SECONDS)
        if operation.deliveryLeaseUntil is None or expires < operation.deliveryLeaseUntil:
            operation.deliveryLeaseUntil = expires
            operation.save(update_fields=["deliveryLeaseUntil"])
    return operation


@stream_database_call
def _artifacts(agent_run_id):
    return list(Artifact.objects.filter(
        agent_run_id=agent_run_id, status="published",
    ).order_by("publishedAt", "id"))


@stream_database_call
def _close_delivery(operation_id):
    AgentRunApiOperation.objects.filter(pk=operation_id, deliveryClosedAt__isnull=True).update(
        deliveryClosedAt=timezone.now(), deliveryLeaseUntil=None,
    )


async def _deliver_artifact(artifact):
    metadata: ArtifactStarted = {
        "artifactId": artifact.id,
        "filename": artifact.safeFilename,
        "contentType": artifact.contentType,
        "sizeBytes": artifact.sizeBytes,
        "sha256": artifact.sha256,
    }
    yield ARTIFACT_STARTED.frame(metadata)
    source = None
    size = 0
    digest = hashlib.sha256()
    index = 0
    try:
        source = await open_storage_stream(artifact.storageKey)
        async for chunk in source:
            size += len(chunk)
            if size > artifact.sizeBytes:
                raise ValueError("artifact_integrity_mismatch")
            digest.update(chunk)
            yield ARTIFACT_CHUNK.frame({
                "artifactId": artifact.id, "index": index,
                "data": base64.b64encode(chunk).decode("ascii"),
            })
            index += 1
        if size != artifact.sizeBytes or f"sha256:{digest.hexdigest()}" != artifact.sha256:
            raise ValueError("artifact_integrity_mismatch")
    except asyncio.CancelledError:
        raise
    except Exception:
        yield ARTIFACT_FAILED.frame({"artifactId": artifact.id, "error": "artifact_delivery_failed"})
        return
    finally:
        if source is not None:
            await source.aclose()
    yield ARTIFACT_COMPLETED.frame({**metadata, "chunkCount": index})


async def _produce(operation, agent_run):
    yield RUN_ACCEPTED.frame(operation_receipt(operation))
    yield RUN_PROGRESS.frame({"runId": agent_run.id, "stage": "queued"})
    sequence = 0
    final_text = ""
    running_reported = False
    terminal_seen = False
    while True:
        records, _ = await _load_postgres_page(agent_run, sequence)
        for record in records:
            item = _committed_item(agent_run, record)
            sequence = item["sourceSequence"]
            event = item["event"]
            terminal_seen = terminal_seen or event["type"] in {
                "agent_run_completed", "agent_run_failed", "agent_run_interrupted",
            }
            if event["type"] == "agent_run_started" and not running_reported:
                running_reported = True
                yield RUN_PROGRESS.frame({"runId": agent_run.id, "stage": "running"})
            if event["type"] == "assistant_message":
                # Committed, model-visible text is the only transcript projection.
                # Tool output, reasoning, source references and internal IDs stay private.
                text = event["payload"]["modelMarkdown"]
                if not isinstance(text, str):
                    raise RuntimeError("agent_run_api_assistant_text_invalid")
                final_text = text
        operation = await _run_state(operation.pk)
        if operation.runStatus in TERMINAL_STATUSES and len(records) < POSTGRES_BATCH_SIZE:
            if terminal_seen or await _load_terminal_sequence(agent_run.id) == 0:
                break
        if not records:
            yield ": keepalive\n\n"
            await asyncio.sleep(POLL_SECONDS)

    for offset in range(0, len(final_text), TEXT_CHUNK_CHARACTERS):
        yield OUTPUT_TEXT.frame({"runId": agent_run.id, "text": final_text[offset:offset + TEXT_CHUNK_CHARACTERS]})
    artifacts = await _artifacts(agent_run.id)
    if artifacts:
        yield RUN_PROGRESS.frame({"runId": agent_run.id, "stage": "delivering"})
    failed = False
    for artifact in artifacts:
        source = _deliver_artifact(artifact)
        try:
            async for frame in source:
                failed = failed or frame.startswith(f"event: {ARTIFACT_FAILED.name}\n")
                yield frame
        finally:
            await source.aclose()
    finished: RunFinished = {
        "runId": agent_run.id,
        "runStatus": operation.runStatus,
        "deliveryStatus": "failed" if failed else "completed",
    }
    if operation.reasonCode:
        finished["reasonCode"] = operation.reasonCode
    yield RUN_FINISHED.frame(finished)


async def stream_agent_run_api(operation, agent_run, authority_check):
    """Disconnect closes delivery only. The lifecycle Job retains execution ownership."""
    producer = _produce(operation, agent_run)
    pending = None
    next_authority = 0
    lease_until = operation.deliveryLeaseUntil
    try:
        while True:
            now = time.monotonic()
            if lease_until is None or lease_until <= timezone.now():
                return
            if now >= next_authority:
                async with asyncio.timeout(READ_TIMEOUT_SECONDS):
                    if await authority_check() is not True:
                        return
                    current = await _run_state(operation.pk)
                lease_until = current.deliveryLeaseUntil
                next_authority = now + AUTHORITY_SECONDS
            if pending is None:
                pending = asyncio.create_task(anext(producer))
            done, _ = await asyncio.wait({pending}, timeout=min(
                POLL_SECONDS, max(0.001, next_authority - now),
                max(0.001, (lease_until - timezone.now()).total_seconds()),
            ))
            if not done:
                yield ": keepalive\n\n"
                continue
            try:
                frame = pending.result()
            except StopAsyncIteration:
                return
            pending = None
            async with asyncio.timeout(READ_TIMEOUT_SECONDS):
                if await authority_check() is not True:
                    return
            yield frame
    finally:
        if pending is not None:
            pending.cancel()
            await asyncio.gather(pending, return_exceptions=True)
        await producer.aclose()
        await _close_delivery(operation.pk)
