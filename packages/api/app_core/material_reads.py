"""Shared Workspace read orchestration over a freshly authorized scope.

HTTP and MCP adapters must obtain MaterialAccess for each request before calling
these operations. They cannot grant access by supplying model-controlled IDs.
Transport envelopes are constructed by the caller; evidence semantics live here
and in material_evidence. Processing jobs and citation persistence are separate.
"""

from . import material_access, material_evidence
from .material_access import MaterialAccess
from .material_contract import BoundInput, KnowledgeError, MAX_READ_LINES
from .material_identity import require_representation_input
from .material_storage import read_stored
from .models import DerivedRepresentation


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
