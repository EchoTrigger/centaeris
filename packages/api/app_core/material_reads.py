"""Shared Workspace read/search orchestration over a freshly authorized scope.

HTTP and MCP adapters must obtain MaterialAccess for each request before calling
these operations. They cannot grant access by supplying model-controlled IDs.
Transport envelopes are constructed by the caller; evidence semantics live here
and in material_evidence. Processing jobs and citation persistence are separate.
"""

import re
from datetime import UTC

from django.utils.dateparse import parse_datetime

from . import material_access, material_evidence
from .material_access import MaterialAccess
from .material_contract import BoundInput, KnowledgeError, MAX_READ_LINES
from .material_identity import require_representation_input
from .material_storage import read_stored
from .models import DerivedRepresentation, KnowledgeSegment


MAX_SEARCH_CANDIDATES = 10_000


def read_materials(access: MaterialAccess, input_bindings: list, *, offset=None, limit=None, full_window=False) -> dict:
    inputs = material_access.bind_inputs(access.agent_run, access.authorization_digest, input_bindings, access.spec_digest)
    if not 1 <= len(inputs) <= 4:
        raise KnowledgeError("knowledge_read_inputs_invalid", 400)
    if len(inputs) > 1 and (offset is not None or limit is not None):
        raise KnowledgeError("knowledge_read_batch_pagination_invalid", 400)
    if offset is not None and (isinstance(offset, bool) or not isinstance(offset, int) or offset < 0):
        raise KnowledgeError("knowledge_read_offset_invalid", 400)
    if limit is not None and (
        isinstance(limit, bool) or not isinstance(limit, int) or not 1 <= limit <= MAX_READ_LINES
    ):
        raise KnowledgeError("knowledge_read_limit_invalid", 400)
    representations, missing = _representations(inputs)
    if missing:
        return {"disposition": "pending", "missing": missing}
    items = [
        _read_representation(item, representation, offset or 0, limit or MAX_READ_LINES, full_window=full_window)
        for item, representation in zip(inputs, representations, strict=True)
    ]
    return {
        "disposition": "ready",
        "items": items,
        "processingSpecification": access.processing_specification,
    }


def search_materials(access: MaterialAccess, input_bindings: list, *, query, ranking, date_range, limit, full_window=False) -> dict:
    if (
        not isinstance(query, str)
        or not query.strip()
        or len(query.encode("utf-8")) > 2_048
        or ranking not in {"relevance", "recent"}
        or isinstance(limit, bool)
        or not isinstance(limit, int)
        or not 1 <= limit <= 20
    ):
        raise KnowledgeError("knowledge_search_arguments_invalid", 400)
    inputs = material_access.bind_inputs(access.agent_run, access.authorization_digest, input_bindings, access.spec_digest)
    if len(inputs) > 128:
        raise KnowledgeError("knowledge_search_inputs_invalid", 400)
    date_range = _date_range(date_range)
    inputs = [
        item
        for item in inputs
        if _within_date_range(material_evidence.owner_updated_at(item.owner), date_range)
    ]
    representations, missing = _representations(inputs)
    if missing:
        return {"disposition": "pending", "missing": missing}
    representation_ids = [item.representation_id for item in inputs]
    candidates = list(
        KnowledgeSegment.objects.filter(representation_id__in=representation_ids)
        .select_related("representation")
        .order_by("representation_id", "ordinal")[: MAX_SEARCH_CANDIDATES + 1]
    )
    if len(candidates) > MAX_SEARCH_CANDIDATES:
        raise KnowledgeError("knowledge_search_candidate_limit_exceeded")
    inputs_by_representation = {item.representation_id: item for item in inputs}
    terms = list(dict.fromkeys(re.findall(r"\w+", query.casefold()))) or [query.casefold()]
    matches = []
    for segment in candidates:
        folded = segment.boundedText.casefold()
        score = sum(folded.count(term) for term in terms)
        if not score:
            continue
        item = inputs_by_representation[segment.representation_id]
        if query.casefold() in folded:
            score += 2
        matches.append((score, material_evidence.owner_updated_at(item.owner), segment, item))
    if ranking == "recent":
        matches.sort(key=lambda value: (-value[1].timestamp(), -value[0], value[2].segmentId))
    else:
        matches.sort(key=lambda value: (-value[0], -value[1].timestamp(), value[2].segmentId))
    hits = [
        material_evidence.search_evidence(segment, item, score, full_window=full_window)
        for score, _updated, segment, item in matches[:limit]
    ]
    return {
        "disposition": "ready",
        "query": query,
        "ranking": ranking,
        "hits": hits,
        "processingSpecification": access.processing_specification,
    }


def _representations(inputs: list[BoundInput]):
    by_id = DerivedRepresentation.objects.in_bulk(
        [item.representation_id for item in inputs], field_name="representationId"
    )
    representations = []
    missing = []
    for item in inputs:
        representation = by_id.get(item.representation_id)
        if representation is None:
            missing.append(
                {"inputRef": item.resolved["inputRef"], "representationId": item.representation_id}
            )
        else:
            require_representation_input(representation, item)
        representations.append(representation)
    return representations, missing


def _read_representation(bound: BoundInput, representation, offset: int, limit: int, *, full_window=False) -> dict:
    content = read_stored(
        representation.canonicalTextKey,
        representation.canonicalTextSizeBytes,
        representation.canonicalTextSha256,
    ).decode("utf-8", errors="strict")
    return material_evidence.read_evidence(content, bound, representation, offset, limit, full_window=full_window)


def _date_range(value):
    if value is None:
        return None, None
    if not isinstance(value, dict) or set(value) != {"updatedFrom", "updatedTo"}:
        raise KnowledgeError("knowledge_search_date_range_invalid", 400)
    parsed = []
    for name in ("updatedFrom", "updatedTo"):
        raw = value[name]
        date = parse_datetime(raw) if isinstance(raw, str) else None
        if raw is not None and (date is None or date.tzinfo is None):
            raise KnowledgeError("knowledge_search_date_range_invalid", 400)
        parsed.append(date.astimezone(UTC) if date else None)
    if parsed[0] and parsed[1] and parsed[0] > parsed[1]:
        raise KnowledgeError("knowledge_search_date_range_invalid", 400)
    return tuple(parsed)


def _within_date_range(value, date_range):
    lower, upper = date_range
    return (lower is None or value >= lower) and (upper is None or value <= upper)
