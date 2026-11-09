"""Published first-party material tool contracts and their wire identity."""

import hashlib
import json

from jsonschema import Draft202012Validator
from mcp import types


def _schema(properties, required=()):
    return {"type": "object", "properties": properties, "required": list(required), "additionalProperties": False}


TOOLS = (
    types.Tool(name="list_materials", description="Discover authorized material metadata in library, attached, or all scope. query filters names, not body text. Use nextCursor to continue.", inputSchema=_schema({
        "scope": {"enum": ["library", "attached", "all"]},
        "query": {"type": "string", "minLength": 1, "maxLength": 2048},
        "folder": {"type": "string", "maxLength": 64},
        "cursor": {"type": "string", "minLength": 1, "maxLength": 4096},
        "limit": {"type": "integer", "minimum": 1, "maximum": 100},
    })),
    types.Tool(name="read_material", description="Read an authorized material. Missing representations create durable processing operations; use get_operation for their status, then read again.", inputSchema=_schema({
        "input_ref": {"type": "string", "minLength": 1},
        "delivery": {"enum": ["content", "execution_file"]},
        "offset": {"type": "integer", "minimum": 0},
        "limit": {"type": "integer", "minimum": 1, "maximum": 2000},
    }, ("input_ref", "delivery"))),
    types.Tool(name="search_materials", description="Search authorized current material content; default all covers personal Library plus attachments. coverage states what is indexed. cursor pages ranked hits; processing_cursor independently advances bounded missing-document preparation. Failed and unsupported documents do not block indexed hits.", inputSchema=_schema({
        "query": {"type": "string", "minLength": 1},
        "scope": {"enum": ["all", "library", "attached"]},
        "input_refs": {"type": "array", "items": {"type": "string", "minLength": 1}, "uniqueItems": True, "maxItems": 128},
        "ranking": {"enum": ["relevance", "recent"]},
        "limit": {"type": "integer", "minimum": 1, "maximum": 20},
        "cursor": {"type": "string", "minLength": 1, "maxLength": 4096},
        "processing_cursor": {"type": "string", "minLength": 1, "maxLength": 4096},
    }, ("query",))),
    types.Tool(name="read_material_result", description="Continue an immutable saved material result using its result_ref and cursor. Returns exact ranges and the next call; only returned evidence has been read.", inputSchema=_schema({
        "result_ref": {"type": "string", "minLength": 1, "maxLength": 96},
        "cursor": {"type": "string", "pattern": "^[0-9]{1,10}:[0-9]{1,12}$"},
    }, ("result_ref", "cursor"))),
    types.Tool(name="get_operation", description="Get the durable status of a material operation owned by this run. Completed means read/search can be retried.", inputSchema=_schema({"operation_id": {"type": "string", "minLength": 1, "maxLength": 96}}, ("operation_id",))),
    types.Tool(name="cancel_operation", description="Cancel this run's material operation. This detaches its wait and does not stop shared background processing or delete its result.", inputSchema=_schema({"operation_id": {"type": "string", "minLength": 1, "maxLength": 96}}, ("operation_id",))),
)
VALIDATORS = {tool.name: Draft202012Validator(tool.input_schema) for tool in TOOLS}

MATERIAL_TOOL_CATALOG_HEADER = "X-Workspace-Material-Catalog"
MATERIAL_TOOL_CATALOG_DIGEST = "sha256:" + hashlib.sha256(json.dumps(
    [tool.model_dump(mode="json", by_alias=True, exclude_none=True) for tool in TOOLS],
    ensure_ascii=False, sort_keys=True, separators=(",", ":"),
).encode("utf-8")).hexdigest()
