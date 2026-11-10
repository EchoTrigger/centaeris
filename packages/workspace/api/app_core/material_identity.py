"""Workspace processing and representation identity rules shared by transports."""

import json

from .material_contract import BoundInput, KnowledgeError, sha256_bytes
from .runtime_contract import require_sha256


DET_MODEL = "PP-OCRv6_small_det"
REC_MODEL = "PP-OCRv6_small_rec"
MAX_OUTPUT_BYTES = 256 * 1024 * 1024


def canonical_json(value) -> bytes:
    return json.dumps(
        value, ensure_ascii=False, separators=(",", ":"), sort_keys=True
    ).encode("utf-8")


def processing_spec_digest(specification: dict) -> str:
    validate_processing_specification(specification)
    return sha256_bytes(canonical_json(specification))


def representation_id(input_identity: dict, spec_digest: str) -> str:
    _validate_input_identity(input_identity)
    require_sha256("specDigest", spec_digest)
    digest = sha256_bytes(
        canonical_json(
            {"inputIdentity": input_identity, "specDigest": spec_digest}
        )
    ).removeprefix("sha256:")
    return f"representation:sha256:{digest}"


def validate_processing_specification(value: dict):
    fields = {"schema", "processorId", "processorVersion", "executionImageDigest", "modelDigests", "options"}
    if not isinstance(value, dict) or set(value) != fields or value["schema"] != "knowledge.processing_specification.v1":
        raise KnowledgeError("knowledge_specification_invalid", 400)
    if value["processorId"] not in {"centaeris.document.cpu", "centaeris.document.cuda.gpu0"} or value["processorVersion"] != "1.0.0":
        raise KnowledgeError("knowledge_processor_identity_unsupported", 400)
    require_sha256("executionImageDigest", value["executionImageDigest"])
    if not isinstance(value["modelDigests"], dict) or set(value["modelDigests"]) != {DET_MODEL, REC_MODEL}:
        raise KnowledgeError("knowledge_model_identity_invalid", 400)
    for digest in value["modelDigests"].values():
        require_sha256("modelDigest", digest)
    expected_options = {
        "renderDpi": 220, "maxInputBytes": 64 * 1024 * 1024,
        "maxRenderedPixelsPerPage": 16_000_000, "maxOutputBytes": MAX_OUTPUT_BYTES,
    }
    if value["options"] != expected_options:
        raise KnowledgeError("knowledge_processing_options_unsupported", 400)


def _validate_input_identity(value: dict):
    if not isinstance(value, dict) or set(value) != {"ownerKind", "ownerId", "generation", "sha256"}:
        raise KnowledgeError("knowledge_input_identity_invalid", 400)
    if value["ownerKind"] not in {"sourceObject", "userLibraryObject", "artifact"}:
        raise KnowledgeError("knowledge_owner_kind_unsupported", 400)
    if not isinstance(value["ownerId"], str) or not value["ownerId"]:
        raise KnowledgeError("knowledge_input_identity_invalid", 400)
    if isinstance(value["generation"], bool) or not isinstance(value["generation"], int) or value["generation"] < 0:
        raise KnowledgeError("knowledge_input_identity_invalid", 400)
    require_sha256("inputIdentity.sha256", value["sha256"])


def require_representation_input(representation, bound: BoundInput):
    if (
        representation.ownerKind != bound.input_identity["ownerKind"]
        or representation.ownerId != bound.input_identity["ownerId"]
        or representation.ownerContentGeneration != bound.input_identity["generation"]
        or representation.ownerSha256 != bound.input_identity["sha256"]
    ):
        raise KnowledgeError("knowledge_representation_identity_conflict")
