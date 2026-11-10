"""Workspace citation projections and explicit reconstruction of persisted facts."""
import hashlib
import re
import unicodedata

from django.core.exceptions import ObjectDoesNotExist

from .models import RunMaterialAdmission, RunMaterialScope, SessionCitationProjection, SessionMaterialInput


_SHA256 = re.compile(r"sha256:[0-9a-f]{64}\Z")
_ASSOCIATION_FIELDS = {"associationId", "inputRef", "sourceIdentity", "sourceToolName", "sourceToolCallId"}


def _identifier(value):
    return isinstance(value, str) and bool(value) and value.strip() == value and unicodedata.normalize("NFC", value) == value


def _line_range(value):
    return (isinstance(value, dict) and set(value) == {"startLine", "endLine"}
        and type(value["startLine"]) is int and type(value["endLine"]) is int
        and 0 < value["startLine"] <= value["endLine"] <= 2**64 - 1)


def validate_source_association(payload):
    if (not isinstance(payload, dict) or set(payload) - {"range"} != _ASSOCIATION_FIELDS
        or any(not _identifier(payload[field]) for field in _ASSOCIATION_FIELDS - {"sourceIdentity"})):
        raise ValueError("source_association_invalid")
    identity = payload["sourceIdentity"]
    if (not isinstance(identity, dict) or set(identity) != {"sourceRef", "revision", "sha256"}
        or not _identifier(identity["sourceRef"]) or "/" in identity["sourceRef"] or "\\" in identity["sourceRef"]
        or not _identifier(identity["revision"]) or not isinstance(identity["sha256"], str)
        or _SHA256.fullmatch(identity["sha256"]) is None
        or (payload.get("range") is not None and not _line_range(payload["range"]))):
        raise ValueError("source_association_invalid")


def _uint(value, bits=64):
    return type(value) is int and 0 <= value <= 2**bits - 1


def _digest(value, prefix):
    return isinstance(value, str) and re.fullmatch(re.escape(prefix) + r"[0-9a-f]{64}", value) is not None


def _historical_locator(locator):
    if not isinstance(locator, dict):
        raise ValueError("stored_citation_locator_invalid")
    fields = set(locator)
    if fields == {"startLine", "endLine"} and _line_range(locator):
        return locator.copy()
    if (fields == {"pageStart", "pageEnd"} and _uint(locator["pageStart"])
        and _uint(locator["pageEnd"]) and 0 < locator["pageStart"] <= locator["pageEnd"]):
        return None
    kind = locator.get("kind")
    if kind == "textSpan":
        required = {"kind", "startByte", "endByte", "startLine", "endLine"}
        lines = {key: locator.get(key) for key in ("startLine", "endLine")}
        start_page, end_page = locator.get("pageStart"), locator.get("pageEnd")
        pages_valid = ((start_page is None and end_page is None)
            or (_uint(start_page, 32) and _uint(end_page, 32) and 0 < start_page <= end_page))
        if (fields - {"pageStart", "pageEnd"} == required and pages_valid and _line_range(lines)
            and _uint(lines["startLine"], 32) and _uint(lines["endLine"], 32)
            and _uint(locator["startByte"]) and _uint(locator["endByte"])
            and locator["startByte"] < locator["endByte"]):
            return lines
    elif kind == "pageRegion":
        bbox = locator.get("bbox")
        if (fields == {"kind", "page", "bbox"} and _uint(locator["page"], 32) and locator["page"] > 0
            and isinstance(bbox, list) and len(bbox) == 4 and all(_uint(value, 16) for value in bbox)
            and 0 <= bbox[0] < bbox[2] <= 10_000 and 0 <= bbox[1] < bbox[3] <= 10_000):
            return None
    elif kind == "tableCell":
        if (fields == {"kind", "page", "tableId", "startRow", "endRow", "startColumn", "endColumn"}
            and _uint(locator["page"], 32) and locator["page"] > 0
            and isinstance(locator["tableId"], str) and bool(locator["tableId"].strip())
            and all(_uint(locator[key], 32) for key in ("startRow", "endRow", "startColumn", "endColumn"))
            and locator["startRow"] <= locator["endRow"] and locator["startColumn"] <= locator["endColumn"]):
            return None
    raise ValueError("stored_citation_locator_invalid")


def _historical_citation(payload):
    required = {"citationId", "inputRef", "ownerRef", "ownerKind", "displayName", "evidenceKind",
        "ownerSha256", "sourceToolName", "sourceToolCallId", "locator"}
    derived = {"ownerGeneration", "representationId", "specDigest", "evidenceSha256"}
    if (not isinstance(payload, dict) or not required.issubset(payload) or set(payload) - required - derived
        or any(not isinstance(payload[field], str) or not payload[field].strip() for field in required - {"locator"})
        or payload["ownerKind"] not in {"sourceObject", "userLibraryObject", "artifact"}
        or payload["evidenceKind"] not in {"workspaceSource", "userProvided", "generatedArtifact"}
        or not _digest(payload["citationId"], "citation:") or not _digest(payload["ownerSha256"], "sha256:")):
        raise ValueError("stored_citation_identity_invalid")
    lines = _historical_locator(payload["locator"])
    present = {field for field in derived if payload.get(field) is not None}
    if "kind" in payload["locator"] or set(payload) & derived:
        if (present != derived or not _uint(payload["ownerGeneration"]) or payload["ownerGeneration"] == 0
            or not _digest(payload["representationId"], "representation:sha256:")
            or not _digest(payload["specDigest"], "sha256:") or not _digest(payload["evidenceSha256"], "sha256:")):
            raise ValueError("stored_citation_derived_identity_invalid")
    return lines


def reconstruct_source_event(event):
    """Read a historical citation as a generic event while retaining its stored row."""
    if event.get("type") != "citation_recorded":
        return event
    old = event["payload"]
    lines = _historical_citation(old)
    payload = {"associationId": old["citationId"], "inputRef": old["inputRef"],
        "sourceIdentity": {"sourceRef": old["ownerKind"] + ":" + old["ownerRef"],
            "revision": str(old["ownerGeneration"]) if old.get("ownerGeneration") is not None else old["ownerSha256"], "sha256": old["ownerSha256"]},
        "sourceToolName": old["sourceToolName"], "sourceToolCallId": old["sourceToolCallId"]}
    if lines is not None:
        payload["range"] = lines
    validate_source_association(payload)
    return {**event, "type": "source_associated", "payload": payload}


def _declared_source(input):
    identity = input["inputIdentity"]
    owner_kind = identity["ownerKind"]
    return {"inputRef": input["inputRef"], "ownerRef": identity["ownerId"], "ownerKind": owner_kind,
        "ownerSha256": identity["sha256"], "ownerGeneration": identity["generation"],
        "displayName": input["displayName"], "citationAllowed": owner_kind in {"sourceObject", "userLibraryObject"},
        "evidenceKind": {"sourceObject": "workspaceSource", "userLibraryObject": "userProvided", "artifact": "generatedArtifact"}[owner_kind]}


def _authorized_sources(agent_run):
    # These records capture the identity admitted to execution. Presentation never
    # resolves a current resource version or grants execution authority.
    try:
        declared = agent_run.authorization.payload["assetRefs"]
    except ObjectDoesNotExist:
        declared = []
    sources = {input["inputRef"]: _declared_source(input) for input in declared}
    personal_library = RunMaterialScope.objects.filter(agent_run=agent_run, personal_library=True).exists()
    admitted = set(RunMaterialAdmission.objects.filter(agent_run=agent_run,
        confirmed=True, settled_absent=False).values_list("material_input_id", flat=True))
    records = SessionMaterialInput.objects.filter(session_id=agent_run.session_id, confirmed=True,
        admissions__confirmed=True, admissions__settled_absent=False).distinct()
    if not personal_library:
        records = records.filter(pk__in=admitted)
    for record in records:
        if record.input_ref in sources:
            continue
        resolved = record.resolved
        if record.pk not in admitted and resolved["ownerKind"] != "userLibraryObject":
            continue
        generation = int(resolved["sourceVersion"])
        if generation <= 0 or str(generation) != resolved["sourceVersion"]:
            raise ValueError("source_admission_revision_invalid")
        sources[record.input_ref] = {"inputRef": record.input_ref, "ownerRef": resolved["objectRef"],
            "ownerKind": resolved["ownerKind"], "ownerSha256": resolved["sha256"], "ownerGeneration": generation,
            "displayName": resolved["displayName"], "citationAllowed": resolved["citationAllowed"],
            "evidenceKind": resolved["evidenceKind"]}
    return sources


def _read_binding(agent_run, record, calls, results):
    event = record.payload
    payload = event["payload"]
    call_id = payload["sourceToolCallId"]
    bound_calls, bound_results = calls.get(call_id, []), results.get(call_id, [])
    if len(bound_calls) != 1 or len(bound_results) != 1:
        return False
    call, result = bound_calls[0], bound_results[0]
    invocation, output = call.payload["payload"], result.payload["payload"]
    return (invocation.get("providerId") == "centaeris.builtin" and invocation.get("toolName") == "read"
        and output.get("toolName") == "read" and output.get("resultState") == "successWithOutput"
        and payload["sourceToolName"] == "read" and output.get("outputComplete") is True
        and call.sequence < result.sequence < record.sequence
        and event.get("agentRunId") == agent_run.pk and event.get("sessionId") == agent_run.session_id
        and call.payload.get("turnId") == result.payload.get("turnId") == event.get("turnId")
        and _identifier(event.get("turnId")))


def read_citation_projections(agent_run, records):
    """Derive file citations from admitted identities and matching durable reads."""
    associations = [record for record in records if record.payload["type"] == "source_associated"]
    if not associations:
        return []
    sources = _authorized_sources(agent_run)
    calls, results = {}, {}
    for record in records:
        kind, payload = record.payload["type"], record.payload["payload"]
        if kind == "tool_call":
            calls.setdefault(payload["callId"], []).append(record)
        elif kind == "tool_result":
            results.setdefault(payload["callId"], []).append(record)
    projected = []
    for record in associations:
        payload = record.payload["payload"]
        validate_source_association(payload)
        source = sources.get(payload["inputRef"])
        if (source is None or source["ownerKind"] not in {"sourceObject", "userLibraryObject"}
            or source["citationAllowed"] is not True or not _line_range(payload.get("range"))
            or not _read_binding(agent_run, record, calls, results)):
            continue
        expected = {"sourceRef": source["ownerKind"] + ":" + source["ownerRef"],
            "revision": str(source["ownerGeneration"]), "sha256": source["ownerSha256"]}
        if payload["sourceIdentity"] != expected:
            continue
        lines = payload["range"]
        values = (agent_run.pk, payload["inputRef"], source["ownerRef"], source["ownerSha256"],
            f"lines:{lines['startLine']}-{lines['endLine']}")
        citation_id = "citation:" + hashlib.sha256(b"".join(value.encode("utf-8") + b"\0" for value in values)).hexdigest()
        evidence = {key: value for key, value in source.items() if key != "citationAllowed"}
        projected.append(SessionCitationProjection(citationId=citation_id, workspace=agent_run.workspace,
            session=agent_run.session, agent_run=agent_run, sequence=record.sequence,
            sourceToolName="read", sourceToolCallId=payload["sourceToolCallId"], locator=lines, **evidence))
    return projected


def merge_read_citation_projection(citations, historical_payloads, citation):
    """Keep the original direct-read evidence when an upgraded Run rereads it."""
    previous = citations.get(citation.citationId)
    if previous is None:
        citations[citation.citationId] = citation
        return
    original = historical_payloads.get(citation.citationId)
    derived_fields = ("representationId", "specDigest", "evidenceSha256")
    same_fields = ("workspace_id", "session_id", "agent_run_id", "inputRef", "ownerKind", "ownerRef",
        "ownerSha256", "displayName", "evidenceKind", "sourceToolName", "locator")
    # Direct historical reads captured the digest without a generation. Their
    # projection default is not an authoritative source revision.
    if (original is None or original.get("sourceToolName") != "read"
        or any(field in original for field in derived_fields) or not _line_range(original.get("locator"))
        or any(getattr(citation, field) for field in derived_fields)
        or any(getattr(previous, field) != getattr(citation, field) for field in same_fields)
        or ("ownerGeneration" in original and original["ownerGeneration"] != citation.ownerGeneration)):
        raise ValueError("material citation identity conflict")
