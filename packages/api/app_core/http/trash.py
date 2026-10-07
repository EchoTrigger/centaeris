import hashlib
import json

from django.contrib.auth import get_user_model
from django.core.exceptions import ValidationError
from django.db.models import Q
from django.db.models.functions import Collate
from django.utils import timezone
from django.utils.dateparse import parse_datetime
from ninja import Router, Status

from app_core.models import Agent, Session, Source, UserLibraryObject
from app_core.trash_retention import TRASH_RETENTION, trash_cutoff
from app_core.workspace_access import (
    source_access_map_for_workspace_member,
    workspace_membership_for,
)

from .response_schema import COMMON_ERROR_RESPONSES, TrashEnvelope
from .security import session_auth
from .trash_pagination import TRASH_PAGE_SIZE, read_trash_cursor, trash_page


router = Router(tags=["trash"], by_alias=True)
TRASH_KINDS = frozenset({"agent", "session", "source", "library"})
TRASH_SCOPES = frozenset({"workspace", "privateLibrary"})
LOCATION_KINDS = frozenset({"workspace", "agent", "libraryRoot", "libraryFolder"})
QUERY_FIELDS = {
    "cursor",
    "query",
    "kind",
    "scope",
    "locationKind",
    "locationId",
    "deletedByUserId",
}


def _query_value(request, name: str) -> str:
    return request.GET.get(name, "").strip()


def _read_filters(request) -> dict:
    query = _query_value(request, "query")
    kind = _query_value(request, "kind")
    scope = _query_value(request, "scope")
    location_kind = _query_value(request, "locationKind")
    location_id = _query_value(request, "locationId")
    deleted_by_user_id = _query_value(request, "deletedByUserId")
    if "query" in request.GET and (not query or len(query) > 200):
        raise ValueError("trash_query_invalid")
    if kind and kind not in TRASH_KINDS:
        raise ValueError("trash_filter_invalid")
    if scope and scope not in TRASH_SCOPES:
        raise ValueError("trash_filter_invalid")
    if location_kind and location_kind not in LOCATION_KINDS:
        raise ValueError("trash_filter_invalid")
    location_requires_id = location_kind in {"agent", "libraryFolder"}
    if location_requires_id != bool(location_id):
        raise ValueError("trash_filter_invalid")
    if deleted_by_user_id:
        try:
            parsed_user_id = get_user_model()._meta.pk.to_python(deleted_by_user_id)
        except (TypeError, ValueError, ValidationError):
            raise ValueError("trash_filter_invalid") from None
        if str(parsed_user_id) != deleted_by_user_id:
            raise ValueError("trash_filter_invalid")
    return {
        "query": query,
        "kind": kind,
        "scope": scope,
        "locationKind": location_kind,
        "locationId": location_id,
        "deletedByUserId": deleted_by_user_id,
    }


def _filter_hash(filters: dict) -> str:
    canonical = json.dumps(filters, ensure_ascii=False, separators=(",", ":"), sort_keys=True)
    return hashlib.sha256(canonical.encode("utf-8")).hexdigest()


def _after_cursor(queryset, kind: str, cursor: dict | None):
    # Match Python's ID order in both selection and cursor comparison, regardless
    # of the database default locale. ID is only a tie-breaker after deletion time.
    queryset = queryset.alias(trash_sort_id=Collate("id", "C")).order_by(
        "-deletedAt", "trash_sort_id"
    )
    if cursor is None:
        return queryset
    deleted_at = parse_datetime(cursor["deletedAt"])
    after = Q(deletedAt__lt=deleted_at)
    if kind > cursor["itemKind"]:
        after |= Q(deletedAt=deleted_at)
    elif kind == cursor["itemKind"]:
        after |= Q(deletedAt=deleted_at, trash_sort_id__gt=cursor["id"])
    return queryset.filter(after)


def _actor(user) -> dict | None:
    if user is None:
        return None
    return {"userId": str(user.id), "email": user.email or user.username}


def _location(kind: str, identity: str | None, label: str, scope: str) -> dict:
    return {"kind": kind, "id": identity, "label": label, "scope": scope}


def _item(kind: str, item, title: str, scope: str, location: dict) -> dict:
    return {
        "id": item.id,
        "kind": kind,
        "title": title,
        "deletedAt": item.deletedAt.isoformat(),
        "expiresAt": (item.deletedAt + TRASH_RETENTION).isoformat(),
        "scope": scope,
        "location": location,
        "deletedBy": _actor(item.deletedBy),
        "_deletedAt": item.deletedAt,
    }


def _filter_querysets(filters, agents, sessions, sources, library):
    if filters["query"]:
        agents = agents.filter(name__icontains=filters["query"])
        sessions = sessions.filter(title__icontains=filters["query"])
        sources = sources.filter(name__icontains=filters["query"])
        library = library.filter(displayName__icontains=filters["query"])
    if filters["deletedByUserId"]:
        deleted_by = filters["deletedByUserId"]
        agents = agents.filter(deletedBy_id=deleted_by)
        sessions = sessions.filter(deletedBy_id=deleted_by)
        sources = sources.filter(deletedBy_id=deleted_by)
        library = library.filter(deletedBy_id=deleted_by)
    if filters["kind"]:
        agents = agents if filters["kind"] == "agent" else agents.none()
        sessions = sessions if filters["kind"] == "session" else sessions.none()
        sources = sources if filters["kind"] == "source" else sources.none()
        library = library if filters["kind"] == "library" else library.none()
    if filters["scope"] == "workspace":
        library = library.none()
    elif filters["scope"] == "privateLibrary":
        agents, sessions, sources = agents.none(), sessions.none(), sources.none()
    location_kind = filters["locationKind"]
    location_id = filters["locationId"]
    if location_kind:
        agents = agents if location_kind == "workspace" else agents.none()
        sources = sources if location_kind == "workspace" else sources.none()
        sessions = (
            sessions.filter(agent_id=location_id)
            if location_kind == "agent"
            else sessions.none()
        )
        if location_kind == "libraryRoot":
            library = library.filter(parentFolder__isnull=True)
        elif location_kind == "libraryFolder":
            library = library.filter(parentFolder_id=location_id)
        else:
            library = library.none()
    return agents, sessions, sources, library


def _filter_options(workspace, querysets) -> dict:
    agents, sessions, sources, library = querysets
    actor_ids = set()
    for queryset in querysets:
        actor_ids.update(
            queryset.exclude(deletedBy__isnull=True).values_list(
                "deletedBy_id", flat=True
            )
        )
    users = get_user_model().objects.filter(id__in=actor_ids).order_by("username", "id")
    locations = []
    if agents.exists() or sources.exists():
        locations.append(_location("workspace", None, workspace.name, "workspace"))
    locations.extend(
        _location("agent", agent_id, name, "workspace")
        for agent_id, name in sessions.values_list("agent_id", "agent__name").distinct()
    )
    if library.filter(parentFolder__isnull=True).exists():
        locations.append(_location("libraryRoot", None, "私人", "privateLibrary"))
    locations.extend(
        _location("libraryFolder", folder_id, name, "privateLibrary")
        for folder_id, name in library.exclude(parentFolder__isnull=True)
        .values_list("parentFolder_id", "parentFolder__displayName")
        .distinct()
    )
    locations.sort(key=lambda item: (item["scope"], item["label"], item["kind"], item["id"] or ""))
    return {"deletedBy": [_actor(user) for user in users], "locations": locations}


@router.get(
    "/workspaces/{workspace_id}/trash",
    auth=session_auth,
    response={200: TrashEnvelope} | COMMON_ERROR_RESPONSES,
)
def list_trash(request, workspace_id: str):
    membership = workspace_membership_for(request.user, workspace_id)
    access_by_source = source_access_map_for_workspace_member(request.user, workspace_id)
    if membership is None or access_by_source is None:
        return Status(404, {"error": "workspace_not_found"})
    try:
        filters = _read_filters(request)
        filter_hash = _filter_hash(filters)
        cursor = read_trash_cursor(
            request,
            "trash",
            {"deletedAt", "itemKind", "id", "filterHash"},
            QUERY_FIELDS,
        )
        deleted_at = parse_datetime(cursor["deletedAt"]) if cursor else None
        if cursor and (
            deleted_at is None
            or not timezone.is_aware(deleted_at)
            or cursor["itemKind"] not in TRASH_KINDS
            or not isinstance(cursor["id"], str)
            or not cursor["id"]
            or cursor["filterHash"] != filter_hash
        ):
            raise ValueError("trash_cursor_invalid")
    except (TypeError, ValueError) as error:
        return Status(400, {"error": str(error)})

    base = {
        "status": "deleted",
        "purgedAt__isnull": True,
        "deletedAt__gt": trash_cutoff(),
    }
    controlled_source_ids = [
        source_id
        for source_id, access_level in access_by_source.items()
        if access_level == "control"
    ]
    querysets = (
        Agent.objects.filter(workspace_id=workspace_id, owner=request.user, **base),
        Session.objects.filter(
            workspace_id=workspace_id,
            owner=request.user,
            agent__status="active",
            **base,
        ),
        Source.objects.filter(
            workspace_id=workspace_id,
            id__in=controlled_source_ids,
            **base,
        ),
        UserLibraryObject.objects.filter(owner=request.user, **base),
    )
    filter_options = _filter_options(membership.workspace, querysets)
    agents, sessions, sources, library = _filter_querysets(filters, *querysets)
    agents = _after_cursor(agents, "agent", cursor).select_related("deletedBy")
    sessions = _after_cursor(sessions, "session", cursor).select_related(
        "agent", "deletedBy"
    )
    sources = _after_cursor(sources, "source", cursor).select_related("deletedBy")
    library = _after_cursor(library, "library", cursor).select_related(
        "parentFolder", "deletedBy"
    )
    workspace_location = _location("workspace", None, membership.workspace.name, "workspace")
    items = [
        *(
            _item("agent", item, item.name, "workspace", workspace_location)
            for item in agents[: TRASH_PAGE_SIZE + 1]
        ),
        *(
            _item(
                "session",
                item,
                item.title,
                "workspace",
                _location("agent", item.agent_id, item.agent.name, "workspace"),
            )
            for item in sessions[: TRASH_PAGE_SIZE + 1]
        ),
        *(
            _item("source", item, item.name, "workspace", workspace_location)
            for item in sources[: TRASH_PAGE_SIZE + 1]
        ),
        *(
            _item(
                "library",
                item,
                item.displayName,
                "privateLibrary",
                _location(
                    "libraryFolder" if item.parentFolder_id else "libraryRoot",
                    item.parentFolder_id,
                    item.parentFolder.displayName if item.parentFolder_id else "私人",
                    "privateLibrary",
                ),
            )
            for item in library[: TRASH_PAGE_SIZE + 1]
        ),
    ]
    # Stable sorts implement deletedAt DESC, kind ASC, ID ASC (matching SQL C).
    items.sort(key=lambda item: (item["kind"], item["id"]))
    items.sort(key=lambda item: item["_deletedAt"], reverse=True)
    page, next_cursor, has_more = trash_page(
        items,
        "trash",
        lambda item: {
            "deletedAt": item["deletedAt"],
            "itemKind": item["kind"],
            "id": item["id"],
            "filterHash": filter_hash,
        },
    )
    for item in page:
        del item["_deletedAt"]
    return {
        "items": page,
        "filterOptions": filter_options,
        "nextCursor": next_cursor,
        "hasMore": has_more,
    }
