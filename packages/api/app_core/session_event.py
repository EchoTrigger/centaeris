import json
from datetime import UTC, datetime

from django.db import connection, transaction

from .models import AgentRun, ArtifactPublication, SessionCitationProjection, SessionEvent


TERMINAL_STATES = {
    "agent_run_completed": "completed",
    "agent_run_failed": "failed",
    "agent_run_interrupted": "cancelled",
}


def _run_records(agent_run: AgentRun):
    """Run-level execution facts only.

    Session-level recovery facts (``session_level=True``, e.g. tool_call_closure)
    live in the same session log and model history, but they are not part of the
    AgentRun execution sequence: they carry no ``agent_run_sequence`` and must
    never drive run ordering, terminal state, or run-scoped citation coverage.
    """
    return SessionEvent.objects.filter(
        workspace_id=agent_run.workspace_id,
        session_id=agent_run.session_id,
        agent_run_id=agent_run.id,
        session_level=False,
    )


def _committed_events(agent_run: AgentRun) -> list[SessionEvent]:
    return list(_run_records(agent_run).order_by("agent_run_sequence"))


def pre_admission_terminal_fact(agent_run: AgentRun) -> dict | None:
    """Read a Runtime-committed control fact, never a cached business status.

    Runtime stores no model history for preparation failures. Its lease-fenced
    receipt binds the original lifecycle owner; later controller leases need
    not have that same owner. SQLite has no Runtime ledger.
    """
    failure = None
    if connection.vendor == "postgresql":
        event_id = "agent_run_pre_admission_failure:" + agent_run.id
        with connection.cursor() as cursor:
            cursor.execute("SELECT to_regclass('runtime.runtime_events')")
            if cursor.fetchone()[0] is not None:
                cursor.execute(
                    "SELECT session_id,task_id,event_type,visibility,at_ms,payload_json "
                    "FROM runtime.runtime_events WHERE event_id=%s", [event_id])
                row = cursor.fetchone()
                if row is not None:
                    session_id, task_id, event_type, visibility, at_ms, raw = row
                    try:
                        payload = json.loads(raw)
                    except (TypeError, ValueError) as error:
                        raise ValueError("pre-admission failure receipt invalid") from error
                    schema = "runtime.agent_run.pre_admission_failure.v1"
                    fields = {"schema", "workspaceId", "sessionId", "agentRunId",
                        "authorizationDigest", "jobId", "leaseOwner", "failedAtMs"}
                    if (not isinstance(payload, dict) or set(payload) != fields
                            or payload["schema"] != schema or event_type != schema
                            or session_id != agent_run.session_id or task_id != agent_run.id
                            or visibility != "internal" or type(at_ms) is not int
                            or type(payload["failedAtMs"]) is not int
                            or not 0 < payload["failedAtMs"] <= 253402300799999
                            or at_ms != payload["failedAtMs"]
                            or agent_run.session.workspace_id != agent_run.workspace_id
                            or payload["workspaceId"] != agent_run.workspace_id
                            or payload["sessionId"] != agent_run.session_id
                            or payload["agentRunId"] != agent_run.id
                            or payload["authorizationDigest"] != agent_run.authorization.digest
                            or payload["jobId"] != "agent_run.lifecycle:" + agent_run.id
                            or not isinstance(payload["leaseOwner"], str) or not payload["leaseOwner"].strip()):
                        raise ValueError("pre-admission failure receipt binding mismatch")
                    failure = {"state": "failed", "sourceType": "preAdmissionFailure",
                        "factRef": event_id, "atMs": at_ms}
    if failure is not None:
        if agent_run.preAdmissionCancelledAt is not None or SessionEvent.objects.filter(
                agent_run_id=agent_run.id, session_level=False).exists():
            raise ValueError("pre-admission failure conflicts with committed execution")
        return failure
    if agent_run.preAdmissionCancelledAt is not None and not SessionEvent.objects.filter(
            agent_run_id=agent_run.id, session_level=False).exists():
        return {"state": "cancelled", "sourceType": "preAdmissionCancellation",
            "factRef": agent_run.id, "atMs": int(agent_run.preAdmissionCancelledAt.timestamp() * 1000)}
    return None


def committed_session_terminal_state(agent_run: AgentRun) -> str | None:
    pre_admission = pre_admission_terminal_fact(agent_run)
    if pre_admission is not None:
        return pre_admission["state"]
    terminal = (
        _run_records(agent_run)
        .order_by("-agent_run_sequence")
        .values_list("payload__type", flat=True)
        .first()
    )
    return TERMINAL_STATES.get(terminal)


def project_committed_agent_run(
    agent_run: AgentRun, expected_status: str | None = None
) -> AgentRun:
    """Materialize database and UI projections from Core-validated Session facts."""
    with transaction.atomic():
        locked_agent_run = (
            AgentRun.objects.select_for_update(of=("self",), no_key=True)
            .select_related("workspace", "session")
            .get(id=agent_run.id)
        )
        fact = pre_admission_terminal_fact(locked_agent_run)
        events = _committed_events(locked_agent_run)
        if not events:
            if fact is not None:
                if expected_status not in (None, fact["state"]):
                    raise ValueError("runtime and pre-admission terminal states mismatch")
                locked_agent_run.status = fact["state"]
                locked_agent_run.startedAt = None
                locked_agent_run.completedAt = (locked_agent_run.preAdmissionCancelledAt
                    if fact["sourceType"] == "preAdmissionCancellation"
                    else datetime.fromtimestamp(fact["atMs"] / 1000, tz=UTC))
                locked_agent_run.save(update_fields=["status", "startedAt", "completedAt", "updatedAt"])
            return locked_agent_run
        status = TERMINAL_STATES.get(events[-1].payload["type"])
        if status is None:
            return locked_agent_run
        if expected_status is not None and status != expected_status:
            raise ValueError("runtime and Session terminal states mismatch")
        rebuild_agent_run_citation_projection(locked_agent_run)
        if any(event.payload["type"] == "assistant_message" for event in events):
            locked_agent_run.session.isUnread = True
            locked_agent_run.session.save(update_fields=["isUnread"])
        locked_agent_run.status = status
        locked_agent_run.startedAt = datetime.fromtimestamp(
            events[0].payload["createdAtMs"] / 1000, tz=UTC
        )
        locked_agent_run.completedAt = datetime.fromtimestamp(
            events[-1].payload["createdAtMs"] / 1000, tz=UTC
        )
        locked_agent_run.save(
            update_fields=["status", "startedAt", "completedAt", "updatedAt"]
        )
        return locked_agent_run


@transaction.atomic
def rebuild_agent_run_citation_projection(
    agent_run: AgentRun,
    through_sequence: int | None = None,
) -> list[SessionCitationProjection]:
    from .material_receipts import citation_projections
    # Serialize projection writers without locking joined Session/Workspace rows
    # or blocking the runtime's deferred AgentRun foreign-key checks.
    agent_run = AgentRun.objects.select_for_update(of=("self",), no_key=True).select_related("workspace", "session").get(pk=agent_run.pk)
    citations = {}
    for stored_event in _committed_events(agent_run):
        if through_sequence is not None and stored_event.sequence > through_sequence:
            continue
        event = stored_event.payload
        if event["type"] != "citation_recorded":
            continue
        payload = event["payload"]
        citations[payload["citationId"]] = SessionCitationProjection(
            citationId=payload["citationId"],
            workspace=agent_run.workspace,
            session=agent_run.session,
            agent_run=agent_run,
            sequence=stored_event.sequence,
            inputRef=payload["inputRef"],
            ownerRef=payload["ownerRef"],
            ownerKind=payload["ownerKind"],
            displayName=payload["displayName"],
            evidenceKind=payload["evidenceKind"],
            ownerSha256=payload["ownerSha256"],
            ownerGeneration=payload.get("ownerGeneration", 1),
            representationId=payload.get("representationId", ""),
            specDigest=payload.get("specDigest", ""),
            evidenceSha256=payload.get("evidenceSha256", ""),
            sourceToolName=payload.get("sourceToolName", "read"),
            sourceToolCallId=payload["sourceToolCallId"],
            locator=payload["locator"],
        )
    for citation in citation_projections(agent_run, through_sequence):
        if citation.citationId in citations:
            raise ValueError("material citation identity conflict")
        citations[citation.citationId] = citation
    SessionCitationProjection.objects.filter(agent_run=agent_run).delete()
    return SessionCitationProjection.objects.bulk_create(citations.values())


@transaction.atomic
def citation_snapshot(agent_run: AgentRun) -> dict:
    """Platform presentation snapshot; never creates Core Session events."""
    agent_run = AgentRun.objects.select_for_update(of=("self",), no_key=True).get(pk=agent_run.pk)
    through = _run_records(agent_run).order_by("-sequence").values_list("sequence", flat=True).first() or 0
    citations = rebuild_agent_run_citation_projection(agent_run, through)
    return {"schema": "workspace.citations.v1", "agentRunId": agent_run.id,
        "sessionId": agent_run.session_id, "throughSequence": through,
        "citations": [{"citationId": c.citationId, "inputRef": c.inputRef, "displayName": c.displayName,
            "sourceToolCallId": c.sourceToolCallId, "sourceUrl": f"/api/citations/{c.citationId}"}
            for c in sorted(citations, key=lambda c: (c.sequence, c.citationId))]}


def _published_artifact_for_event(agent_run: AgentRun, payload: dict):
    try:
        publication = ArtifactPublication.objects.select_related("artifact").get(
            publicationId=payload["publicationId"]
        )
    except ArtifactPublication.DoesNotExist as error:
        raise ValueError("Session artifact publication is missing") from error
    artifact = publication.artifact
    artifact_id = payload["artifactRef"].removeprefix("artifact:")
    if (
        publication.agent_run_id != agent_run.id
        or publication.toolCallId != payload["toolCallId"]
        or publication.filename != payload["filename"]
        or publication.sizeBytes != payload["sizeBytes"]
        or publication.sha256 != payload["sha256"]
        or publication.status != "published"
        or artifact is None
        or artifact.id != artifact_id
        or artifact.agent_run_id != agent_run.id
        or artifact.session_id != agent_run.session_id
        or artifact.workspace_id != agent_run.workspace_id
        or artifact.status != "published"
        or artifact.displayName != payload["filename"]
        or artifact.sizeBytes != payload["sizeBytes"]
        or artifact.sha256 != payload["sha256"]
    ):
        raise ValueError("Session artifact publication binding mismatch")
    return artifact


def published_artifact_links(agent_run: AgentRun) -> list[dict]:
    artifacts = {}
    artifact_refs = None
    for stored_event in _committed_events(agent_run):
        event = stored_event.payload
        if event["type"] == "artifact_published":
            payload = event["payload"]
            artifacts[payload["artifactRef"]] = _published_artifact_for_event(
                agent_run, payload
            )
        elif event["type"] == "assistant_message":
            artifact_refs = event["payload"]["artifactRefs"]
    links = []
    for reference in artifact_refs or []:
        artifact = artifacts.get(reference)
        if artifact is None:
            raise ValueError("assistant references an unpublished artifact")
        links.append(
            {
                "artifactRef": reference,
                "filename": artifact.displayName,
                "downloadUrl": f"/api/artifacts/{artifact.id}/download",
            }
        )
    return links
