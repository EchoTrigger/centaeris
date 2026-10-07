"""Construct bounded evidence from authorized, integrity-verified materials.

This module performs no authorization, database lookup, storage read or citation
publication. Callers supply a current authorized binding and verified canonical
text and validated pagination arguments. Existing response field names remain
unchanged during the MCP migration.
"""

from datetime import UTC, datetime

from .material_contract import (
    BoundInput,
    KnowledgeError,
    MAX_READ_BYTES,
    MAX_SEARCH_SNIPPET_BYTES,
    sha256_bytes,
)


def read_evidence(content: str, bound: BoundInput, representation, offset: int, limit: int, *, full_window=False) -> dict:
    lines = content.splitlines(keepends=full_window)
    if offset > len(lines):
        raise KnowledgeError("knowledge_read_offset_exceeds_content", 400)
    selected = []
    output_bytes = 0
    truncated_by = None
    first_line_exceeds_limit = False
    for line in lines[offset:]:
        if len(selected) == limit:
            truncated_by = "lines"
            break
        encoded = line.encode("utf-8")
        next_bytes = output_bytes + (1 if selected else 0) + len(encoded)
        if not full_window and next_bytes > MAX_READ_BYTES:
            truncated_by = "bytes"
            first_line_exceeds_limit = not selected
            break
        selected.append(line)
        output_bytes = next_bytes
    end_offset = offset + len(selected)
    truncated = end_offset < len(lines)
    if not truncated:
        truncated_by = None
        first_line_exceeds_limit = False
    selected_text = ("" if full_window else "\n").join(selected)
    output_bytes = len(selected_text.encode("utf-8"))
    locator = _line_locator(content, offset + 1, end_offset) if selected_text else None
    page_start, page_end = _locator_pages(representation.manifest, locator) if locator else (None, None)
    if locator is not None:
        locator["pageStart"] = page_start
        locator["pageEnd"] = page_end
    return {
        "schema": "file_read_result.v1",
        "path": bound.resolved["virtualPath"],
        "startLine": offset + 1,
        "endLine": end_offset,
        "totalLines": len(lines),
        "totalBytes": len(content.encode("utf-8")),
        "outputBytes": output_bytes,
        "maxLines": limit,
        "maxBytes": MAX_READ_BYTES,
        "truncated": truncated,
        "truncatedBy": truncated_by,
        "firstLineExceedsLimit": first_line_exceeds_limit,
        "nextOffset": end_offset if truncated and not first_line_exceeds_limit else None,
        "fileHash": representation.canonicalTextSha256,
        "content": selected_text,
        "inputRef": bound.resolved["inputRef"],
        "displayName": bound.resolved["displayName"],
        "ownerRef": bound.resolved["objectRef"],
        "ownerKind": bound.resolved["ownerKind"],
        "evidenceKind": bound.resolved["evidenceKind"],
        "ownerSha256": bound.resolved["sha256"],
        "ownerGeneration": bound.input_identity["generation"],
        "representationId": representation.representationId,
        "specDigest": representation.processingSpecification_id,
        "locator": locator,
        "evidenceSha256": sha256_bytes(selected_text.encode("utf-8")),
        "citationAllowed": bound.resolved["citationAllowed"] and bool(selected_text),
        "pageStart": page_start,
        "pageEnd": page_end,
        "documentRoute": "knowledgeDerived",
        "documentUsedOcr": any(
            page["pageText"]["route"] == "ppOcrV6Small"
            for page in representation.manifest["pages"]
        ),
    }


def search_evidence(segment, bound: BoundInput, score: int, *, full_window=False) -> dict:
    content = segment.boundedText if full_window else _bounded_utf8_prefix(segment.boundedText, MAX_SEARCH_SNIPPET_BYTES)
    locator = dict(segment.locator)
    locator["endByte"] = locator["startByte"] + len(content.encode("utf-8"))
    locator["endLine"] = locator["startLine"] + content.count("\n")
    return {
        "segmentId": segment.segmentId,
        "inputRef": bound.resolved["inputRef"],
        "displayName": bound.resolved["displayName"],
        "ownerRef": bound.resolved["objectRef"],
        "ownerKind": bound.resolved["ownerKind"],
        "evidenceKind": bound.resolved["evidenceKind"],
        "ownerSha256": bound.resolved["sha256"],
        "ownerGeneration": bound.input_identity["generation"],
        "representationId": segment.representation_id,
        "specDigest": segment.representation.processingSpecification_id,
        "locator": locator,
        "evidenceSha256": sha256_bytes(content.encode("utf-8")),
        "citationAllowed": bound.resolved["citationAllowed"],
        "score": score,
        "content": content,
        "updatedAt": owner_updated_at(bound.owner).isoformat(),
    }


def _line_locator(content: str, start_line: int, end_line: int) -> dict:
    if end_line < start_line:
        raise KnowledgeError("knowledge_read_locator_invalid")
    lines = content.splitlines(keepends=True)
    start_byte = len("".join(lines[: start_line - 1]).encode("utf-8"))
    end_byte = len("".join(lines[:end_line]).encode("utf-8"))
    return {
        "kind": "textSpan", "pageStart": None, "pageEnd": None, "startByte": start_byte,
        "endByte": max(start_byte + 1, end_byte), "startLine": start_line, "endLine": end_line,
    }


def _bounded_utf8_prefix(value: str, maximum_bytes: int) -> str:
    encoded = value.encode("utf-8")
    if len(encoded) <= maximum_bytes:
        return value
    end = maximum_bytes
    while end and encoded[end : end + 1] and encoded[end] & 0b1100_0000 == 0b1000_0000:
        end -= 1
    return encoded[:end].decode("utf-8")


def _locator_pages(manifest: dict, locator: dict):
    pages = [
        page["pageText"]["page"]
        for page in manifest["pages"]
        if page["canonicalStartByte"] < locator["endByte"]
        and page["canonicalEndByte"] > locator["startByte"]
    ]
    return (min(pages), max(pages)) if pages else (None, None)


def owner_updated_at(owner) -> datetime:
    value = getattr(owner, "updatedAt", None) or getattr(owner, "publishedAt", None) or owner.createdAt
    return value.astimezone(UTC)
