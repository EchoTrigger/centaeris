"""Authorized browser search; no Agent Run identity or model permission is involved."""
from typing import Literal
from ninja import Router, Status

from app_core.material_contract import KnowledgeError
from app_core.workspace_search import read_search_material, search_workspace
from .query import valid_query_fields
from .response_schema import COMMON_ERROR_RESPONSES
from .schema import StrictSchema
from .security import session_auth

router = Router(tags=["workspace-search"], by_alias=True)


class SearchMatch(StrictSchema):
    id: str
    kind: Literal["material", "deliverable"]
    title: str
    snippet: str
    href: str


class SearchResponse(StrictSchema):
    matches: list[SearchMatch]
    next_cursor: str | None


class SearchMaterial(StrictSchema):
    id: str
    kind: Literal["sourceObject", "userLibraryObject"]
    display_name: str
    source_id: str | None


class SearchMaterialResponse(StrictSchema):
    material: SearchMaterial
    representation_id: str
    content: str
    start_line: int
    end_line: int
    next_offset: int | None
    total_lines: int


def _integer(value, default):
    if value is None:
        return default
    if len(value) > 12 or not value.isascii() or not value.isdecimal() or (value != "0" and value.startswith("0")):
        raise KnowledgeError("workspace_search_query_invalid", 400)
    return int(value)


@router.get("/workspaces/{workspace_id}/search", auth=session_auth,
            response={200: SearchResponse} | COMMON_ERROR_RESPONSES)
def workspace_search(request, workspace_id: str):
    if not valid_query_fields(request.GET, {"query", "limit", "cursor"}, required={"query"}):
        return Status(400, {"error": "workspace_search_query_invalid"})
    try:
        return search_workspace(request.user, workspace_id, query=request.GET["query"],
            limit=_integer(request.GET.get("limit"), 20), cursor=request.GET.get("cursor"))
    except KnowledgeError as error:
        return Status(error.status, {"error": error.code})


@router.get("/workspaces/{workspace_id}/search/materials/{kind}/{object_id}", auth=session_auth,
            response={200: SearchMaterialResponse} | COMMON_ERROR_RESPONSES)
def search_material_content(request, workspace_id: str,
                            kind: Literal["sourceObject", "userLibraryObject"], object_id: str):
    if not valid_query_fields(request.GET, {"representationId", "offset"}):
        return Status(400, {"error": "workspace_search_query_invalid"})
    try:
        return read_search_material(request.user, workspace_id, kind, object_id,
            representation_id=request.GET.get("representationId"), offset=_integer(request.GET.get("offset"), 0))
    except KnowledgeError as error:
        return Status(error.status, {"error": error.code})
