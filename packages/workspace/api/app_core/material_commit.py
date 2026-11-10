"""Document representation commits independent of the HTTP binary envelope.

Callers provide fresh platform-authorized access and a bounded binary stream.
This service owns processing identity, manifest validation and persistence; it does
not authenticate transport credentials or implement MCP task scheduling.
"""

from dataclasses import dataclass
import json
import re

from django.db import transaction

from .assets import delete_stored_object, register_derived_resource
from .material_contract import KnowledgeError, sha256_bytes
from .material_identity import MAX_OUTPUT_BYTES, canonical_json, require_representation_input
from .material_storage import read_stored, store_stream
from .models import DerivedRepresentation, KnowledgeSegment, ProcessingSpecification
from .runtime_contract import require_sha256


MAX_SEGMENT_BYTES = 32 * 1024


@dataclass(frozen=True)
class ProcessingOutput:
    representation_id: str
    canonical_size_bytes: int
    canonical_sha256: str
    preview_size_bytes: int
    preview_sha256: str | None
    manifest: dict
    workbook_size_bytes: int = 0
    workbook_sha256: str | None = None


def validate_commit(commit: ProcessingOutput) -> None:
    for name in ["canonical_size_bytes", "preview_size_bytes", "workbook_size_bytes"]:
        value = getattr(commit, name)
        if isinstance(value, bool) or not isinstance(value, int) or value < 0:
            raise KnowledgeError("knowledge_commit_size_invalid", 400)
    if (
        commit.canonical_size_bytes <= 0
        or commit.canonical_size_bytes + commit.preview_size_bytes + commit.workbook_size_bytes > MAX_OUTPUT_BYTES
    ):
        raise KnowledgeError("knowledge_commit_size_invalid", 400)
    require_sha256("canonicalSha256", commit.canonical_sha256)
    if commit.preview_size_bytes:
        require_sha256("previewSha256", commit.preview_sha256)
    elif commit.preview_sha256 is not None:
        raise KnowledgeError("knowledge_commit_preview_identity_invalid", 400)
    if commit.workbook_size_bytes:
        require_sha256("workbookSha256", commit.workbook_sha256)
    elif commit.workbook_sha256 is not None:
        raise KnowledgeError("knowledge_commit_workbook_identity_invalid", 400)


def commit_bound_material(bound, specification, spec_digest, commit, stream, *, published=None, storage_keys=None):
    """Shared persistence after caller-owned binding/locking; not an auth API.

    The optional publication callback participates in the representation DB
    transaction. Storage cleanup on failure runs before caller locks are released.
    """
    validate_commit(commit)
    if commit.representation_id != bound.representation_id:
        raise KnowledgeError("material_task_identity_mismatch")
    existing = DerivedRepresentation.objects.filter(
        representationId=bound.representation_id
    ).first()
    canonical_key = _canonical_key(bound.representation_id)
    preview_key = _preview_key(bound.representation_id) if commit.preview_size_bytes else ""
    workbook_key = _workbook_key(bound.representation_id) if commit.workbook_size_bytes else ""
    if storage_keys is not None:
        canonical_key, preview_key, workbook_key = storage_keys
    created = []
    try:
        canonical_created = store_stream(
            stream,
            canonical_key,
            commit.canonical_size_bytes,
            commit.canonical_sha256,
        )
        if canonical_created:
            created.append(canonical_key)
        if preview_key:
            preview_created = store_stream(
                stream,
                preview_key,
                commit.preview_size_bytes,
                commit.preview_sha256,
            )
            if preview_created:
                created.append(preview_key)
        if workbook_key:
            workbook_created = store_stream(
                stream,
                workbook_key,
                commit.workbook_size_bytes,
                commit.workbook_sha256,
            )
            if workbook_created:
                created.append(workbook_key)
        if stream.read(1):
            raise KnowledgeError("knowledge_commit_body_has_trailing_bytes", 400)
        canonical = read_stored(
            canonical_key,
            commit.canonical_size_bytes,
            commit.canonical_sha256,
        )
        if workbook_key:
            workbook = read_stored(
                workbook_key,
                commit.workbook_size_bytes,
                commit.workbook_sha256,
            )
            _validate_workbook_preview(workbook)
        manifest = commit.manifest
        _validate_manifest(manifest, canonical)
        if existing is not None:
            _require_existing_representation(existing, bound, commit, spec_digest, canonical_key, preview_key, workbook_key)
            if published is not None:
                with transaction.atomic():
                    published()
            return _commit_response(existing)
        segments = _segments(bound.representation_id, manifest)
        with transaction.atomic():
            stored_spec, inserted = ProcessingSpecification.objects.get_or_create(
                specDigest=spec_digest, defaults={"payload": specification}
            )
            if not inserted and stored_spec.payload != specification:
                raise KnowledgeError("knowledge_specification_identity_conflict")
            representation = DerivedRepresentation.objects.create(
                representationId=bound.representation_id,
                ownerKind=bound.input_identity["ownerKind"],
                ownerId=bound.input_identity["ownerId"],
                ownerContentGeneration=bound.input_identity["generation"],
                ownerSha256=bound.input_identity["sha256"],
                processingSpecification=stored_spec,
                pageCount=manifest["pageCount"],
                canonicalTextKey=canonical_key,
                canonicalTextSizeBytes=commit.canonical_size_bytes,
                canonicalTextSha256=commit.canonical_sha256,
                previewPdfKey=preview_key,
                previewPdfSizeBytes=commit.preview_size_bytes,
                previewPdfSha256=commit.preview_sha256 or "",
                workbookPreviewKey=workbook_key,
                workbookPreviewSizeBytes=commit.workbook_size_bytes,
                workbookPreviewSha256=commit.workbook_sha256 or "",
                manifest=manifest,
            )
            KnowledgeSegment.objects.bulk_create(
                [KnowledgeSegment(representation=representation, **segment) for segment in segments]
            )
            register_derived_resource(bound.owner, "storageObject", canonical_key)
            if preview_key:
                register_derived_resource(bound.owner, "storageObject", preview_key)
            if workbook_key:
                register_derived_resource(bound.owner, "storageObject", workbook_key)
            if published is not None:
                published()
        return _commit_response(representation)
    except Exception:
        for storage_key in created:
            delete_stored_object(storage_key)
        raise


def _segments(representation_id: str, manifest: dict) -> list[dict]:
    segments = []
    ordinal = 0
    for page in manifest["pages"]:
        text = page["pageText"]["text"]
        encoded = text.encode("utf-8")
        start = 0
        line = page["canonicalStartLine"]
        while start < len(encoded):
            end = min(len(encoded), start + MAX_SEGMENT_BYTES)
            while end > start:
                try:
                    bounded = encoded[start:end].decode("utf-8")
                    break
                except UnicodeDecodeError:
                    end -= 1
            if end == start:
                raise KnowledgeError("knowledge_segment_utf8_boundary_invalid")
            if bounded.strip():
                text_hash = sha256_bytes(bounded.encode("utf-8"))
                identity = sha256_bytes(
                    canonical_json([representation_id, ordinal, text_hash])
                ).removeprefix("sha256:")
                segments.append(
                    {
                        "segmentId": f"segment:sha256:{identity}",
                        "ordinal": ordinal,
                        "boundedText": bounded,
                        "textSha256": text_hash,
                        "locator": {
                            "kind": "textSpan",
                            "pageStart": page["pageText"]["page"],
                            "pageEnd": page["pageText"]["page"],
                            "startByte": page["canonicalStartByte"] + start,
                            "endByte": page["canonicalStartByte"] + end,
                            "startLine": line,
                            "endLine": line + bounded.count("\n"),
                        },
                    }
                )
                ordinal += 1
            line += bounded.count("\n")
            start = end
    return segments


def _validate_manifest(manifest: dict, canonical: bytes):
    try:
        canonical.decode("utf-8", errors="strict")
    except UnicodeDecodeError as error:
        raise KnowledgeError("knowledge_canonical_utf8_invalid", 400) from error
    if (
        not isinstance(manifest, dict)
        or set(manifest) != {"schema", "pageCount", "pages"}
        or manifest.get("schema") != "knowledge.derived_manifest.v1"
    ):
        raise KnowledgeError("knowledge_manifest_invalid", 400)
    pages = manifest["pages"]
    if (
        not isinstance(pages, list)
        or isinstance(manifest["pageCount"], bool)
        or not isinstance(manifest["pageCount"], int)
        or manifest["pageCount"] != len(pages)
        or not pages
    ):
        raise KnowledgeError("knowledge_manifest_page_count_invalid", 400)
    previous_end = 0
    previous_end_line = 1
    for expected_page, page in enumerate(pages, 1):
        if not isinstance(page, dict) or set(page) != {
            "pageText", "canonicalStartByte", "canonicalEndByte",
            "canonicalStartLine", "canonicalEndLine",
        }:
            raise KnowledgeError("knowledge_manifest_page_invalid", 400)
        text = page["pageText"]
        if not isinstance(text, dict) or set(text) != {
            "schema", "page", "route", "widthMillipoints", "heightMillipoints",
            "text", "textSha256", "spans",
        }:
            raise KnowledgeError("knowledge_page_text_invalid", 400)
        encoded = text["text"].encode("utf-8") if isinstance(text["text"], str) else b""
        positions = [
            page["canonicalStartByte"], page["canonicalEndByte"],
            page["canonicalStartLine"], page["canonicalEndLine"],
        ]
        if (
            any(isinstance(value, bool) or not isinstance(value, int) for value in positions)
            or text["schema"] != "knowledge.page_text.v1"
            or isinstance(text["page"], bool)
            or not isinstance(text["page"], int)
            or text["page"] != expected_page
            or text["route"] not in {"nativeText", "pdfNative", "ppOcrV6Small", "empty"}
            or not _positive_int(text["widthMillipoints"], 4_294_967_295)
            or not _positive_int(text["heightMillipoints"], 4_294_967_295)
            or len(encoded) > 4 * 1024 * 1024
            or sha256_bytes(encoded) != text["textSha256"]
            or page["canonicalStartByte"] < previous_end
            or page["canonicalStartByte"] < 0
            or page["canonicalEndByte"] != page["canonicalStartByte"] + len(encoded)
            or page["canonicalEndByte"] > len(canonical)
            or canonical[page["canonicalStartByte"] : page["canonicalEndByte"]] != encoded
            or page["canonicalStartLine"] != previous_end_line + canonical.count(b"\n", previous_end, page["canonicalStartByte"])
            or page["canonicalEndLine"] != page["canonicalStartLine"] + encoded.count(b"\n")
        ):
            raise KnowledgeError("knowledge_page_text_identity_invalid", 400)
        spans = text["spans"]
        if (
            not isinstance(spans, list)
            or len(spans) > 200_000
            or any(not _valid_page_text_span(span) for span in spans)
            or (text["route"] == "empty" and (encoded or spans))
            or (spans and "\n".join(span["text"] for span in spans) != text["text"])
            or (encoded and not spans)
        ):
            raise KnowledgeError("knowledge_page_text_spans_invalid", 400)
        previous_end = page["canonicalEndByte"]
        previous_end_line = page["canonicalEndLine"]


def _validate_workbook_preview(payload: bytes) -> None:
    try:
        workbook = json.loads(payload.decode("utf-8", errors="strict"))
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise KnowledgeError("knowledge_workbook_preview_invalid", 400) from error
    if (
        not isinstance(workbook, dict)
        or set(workbook) != {"schema", "styles", "sheets"}
        or workbook.get("schema") != "knowledge.workbook_preview.v1"
        or not isinstance(workbook["styles"], list)
        or not isinstance(workbook["sheets"], list)
        or not workbook["sheets"]
        or len(workbook["sheets"]) > 100
    ):
        raise KnowledgeError("knowledge_workbook_preview_invalid", 400)
    if any(not _valid_workbook_style(style) for style in workbook["styles"]):
        raise KnowledgeError("knowledge_workbook_preview_invalid", 400)
    for sheet in workbook["sheets"]:
        if (
            not isinstance(sheet, dict)
            or set(sheet) != {"name", "state", "maxRow", "maxColumn", "frozenPane", "columns", "rows", "merges"}
            or not isinstance(sheet["name"], str)
            or not sheet["name"]
            or sheet["state"] not in {"visible", "hidden", "veryHidden"}
            or not _positive_int(sheet["maxRow"], 1_048_576)
            or not _positive_int(sheet["maxColumn"], 16_384)
            or sheet["maxRow"] * sheet["maxColumn"] > 100_000
            or sheet["frozenPane"] is not None and (
                not isinstance(sheet["frozenPane"], str)
                or re.fullmatch(r"[A-Z]{1,3}[1-9][0-9]{0,6}", sheet["frozenPane"]) is None
            )
            or not isinstance(sheet["columns"], list)
            or not isinstance(sheet["rows"], list)
            or not isinstance(sheet["merges"], list)
            or any(not _valid_workbook_column(column, sheet["maxColumn"]) for column in sheet["columns"])
            or any(not _valid_workbook_row(row, sheet["maxRow"], sheet["maxColumn"], len(workbook["styles"])) for row in sheet["rows"])
            or any(not _valid_workbook_merge(merge, sheet["maxRow"], sheet["maxColumn"]) for merge in sheet["merges"])
        ):
            raise KnowledgeError("knowledge_workbook_preview_invalid", 400)


def _valid_color(value) -> bool:
    return value is None or isinstance(value, str) and re.fullmatch(r"#[0-9A-F]{6}", value) is not None


def _valid_workbook_style(style) -> bool:
    if not isinstance(style, dict) or set(style) != {"font", "fill", "horizontal", "vertical", "wrapText", "borders"}:
        return False
    font, borders = style["font"], style["borders"]
    return (
        isinstance(font, dict)
        and set(font) == {"bold", "italic", "color"}
        and isinstance(font["bold"], bool)
        and isinstance(font["italic"], bool)
        and _valid_color(font["color"])
        and _valid_color(style["fill"])
        and (style["horizontal"] is None or style["horizontal"] in {"left", "center", "right", "general", "fill", "justify", "centerContinuous", "distributed"})
        and (style["vertical"] is None or style["vertical"] in {"top", "center", "bottom", "justify", "distributed"})
        and isinstance(style["wrapText"], bool)
        and isinstance(borders, dict)
        and set(borders) <= {"top", "right", "bottom", "left"}
        and all(isinstance(border, dict) and set(border) == {"style", "color"} and isinstance(border["style"], str) and _valid_color(border["color"]) for border in borders.values())
    )


def _valid_workbook_column(column, maximum) -> bool:
    return (
        isinstance(column, dict)
        and set(column) == {"index", "widthPx", "hidden"}
        and _positive_int(column["index"], maximum)
        and (column["widthPx"] is None or _positive_int(column["widthPx"], 10_000))
        and isinstance(column["hidden"], bool)
    )


def _valid_workbook_row(row, maximum_row, maximum_column, style_count) -> bool:
    if (
        not isinstance(row, dict)
        or set(row) != {"index", "heightPx", "hidden", "cells"}
        or not _positive_int(row["index"], maximum_row)
        or row["heightPx"] is not None and not _positive_int(row["heightPx"], 10_000)
        or not isinstance(row["hidden"], bool)
        or not isinstance(row["cells"], list)
    ):
        return False
    return all(
        isinstance(cell, dict)
        and set(cell) == {"column", "displayValue", "kind", "formula", "numberFormat", "styleId"}
        and _positive_int(cell["column"], maximum_column)
        and isinstance(cell["displayValue"], str)
        and len(cell["displayValue"].encode("utf-8")) <= 4 * 1024 * 1024
        and cell["kind"] in {"text", "number", "date", "boolean", "formula", "blank"}
        and (cell["formula"] is None or isinstance(cell["formula"], str))
        and isinstance(cell["numberFormat"], str)
        and isinstance(cell["styleId"], int) and not isinstance(cell["styleId"], bool)
        and 0 <= cell["styleId"] < style_count
        for cell in row["cells"]
    )


def _valid_workbook_merge(merge, maximum_row, maximum_column) -> bool:
    return (
        isinstance(merge, dict)
        and set(merge) == {"startRow", "startColumn", "endRow", "endColumn"}
        and _positive_int(merge["startRow"], maximum_row)
        and _positive_int(merge["endRow"], maximum_row)
        and _positive_int(merge["startColumn"], maximum_column)
        and _positive_int(merge["endColumn"], maximum_column)
        and merge["startRow"] <= merge["endRow"]
        and merge["startColumn"] <= merge["endColumn"]
    )


def _positive_int(value, maximum: int) -> bool:
    return (
        isinstance(value, int)
        and not isinstance(value, bool)
        and 0 < value <= maximum
    )


def _valid_page_text_span(span) -> bool:
    if not isinstance(span, dict) or set(span) not in [
        {"text", "bbox"},
        {"text", "bbox", "confidenceMilli"},
    ]:
        return False
    bbox = span["bbox"]
    confidence = span.get("confidenceMilli")
    return (
        isinstance(span["text"], str)
        and bool(span["text"])
        and isinstance(bbox, list)
        and len(bbox) == 4
        and all(isinstance(value, int) and not isinstance(value, bool) for value in bbox)
        and 0 <= bbox[0] < bbox[2] <= 10_000
        and 0 <= bbox[1] < bbox[3] <= 10_000
        and (
            confidence is None
            or isinstance(confidence, int)
            and not isinstance(confidence, bool)
            and 0 <= confidence <= 1_000
        )
    )


def _require_existing_representation(existing, bound, commit, spec_digest, canonical_key, preview_key, workbook_key):
    require_representation_input(existing, bound)
    if (
        existing.processingSpecification_id != spec_digest
        or existing.canonicalTextKey != canonical_key
        or existing.canonicalTextSizeBytes != commit.canonical_size_bytes
        or existing.canonicalTextSha256 != commit.canonical_sha256
        or existing.previewPdfKey != preview_key
        or existing.previewPdfSizeBytes != commit.preview_size_bytes
        or existing.previewPdfSha256 != (commit.preview_sha256 or "")
        or existing.workbookPreviewKey != workbook_key
        or existing.workbookPreviewSizeBytes != commit.workbook_size_bytes
        or existing.workbookPreviewSha256 != (commit.workbook_sha256 or "")
        or existing.manifest != commit.manifest
    ):
        raise KnowledgeError("knowledge_representation_identity_conflict")


def _commit_response(representation):
    return {
        "representationId": representation.representationId,
        "specDigest": representation.processingSpecification_id,
        "pageCount": representation.pageCount,
    }


def _canonical_key(representation_id: str) -> str:
    return f"knowledge/{representation_id.removeprefix('representation:sha256:')}/canonical.md"


def _preview_key(representation_id: str) -> str:
    return f"knowledge/{representation_id.removeprefix('representation:sha256:')}/preview.pdf"


def _workbook_key(representation_id: str) -> str:
    return f"knowledge/{representation_id.removeprefix('representation:sha256:')}/workbook.json"
