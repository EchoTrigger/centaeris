import hashlib
import json

from django.contrib.auth import get_user_model
from django.core.exceptions import ValidationError
from django.db.models import Q
from django.db.models.functions import Collate
from django.utils import timezone
from django.utils.dateparse import parse_datetime
from ninja import Router, Status

from app_core.models import UserLibraryObject
from app_core.trash_retention import TRASH_RETENTION, trash_cutoff
from app_core.workspace_access import workspace_membership_for

from .response_schema import COMMON_ERROR_RESPONSES, TrashEnvelope
from .security import session_auth
from .trash_pagination import TRASH_PAGE_SIZE, read_trash_cursor, trash_page


router = Router(tags=["trash"], by_alias=True)
LIBRARY_FILE_KINDS = ("file", "image", "savedArtifact")
LOCATION_KINDS = frozenset({"libraryRoot", "libraryFolder"})
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
    if kind and kind != "library":
        raise ValueError("trash_filter_invalid")
    if scope and scope != "privateLibrary":
        raise ValueError("trash_filter_invalid")
    if location_kind and location_kind not in LOCATION_KINDS:
        raise ValueError("trash_filter_invalid")
    location_requires_id = location_kind in {"libraryFolder"}
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


def _after_cursor(queryset, cursor: dict | None):
    # Use the same collation for page order and cursor comparisons.
    queryset = queryset.alias(trash_sort_id=Collate("id", "C")).order_by(
        "-deletedAt", "trash_sort_id"
    )
    if cursor is None:
        return queryset
    deleted_at = parse_datetime(cursor["deletedAt"])
    return queryset.filter(
        Q(deletedAt__lt=deleted_at)
        | Q(deletedAt=deleted_at, trash_sort_id__gt=cursor["id"])
    )


def _actor(user) -> dict | None:
    if user is None:
        return None
    return {"userId": str(user.id), "email": user.email or user.username}


def _location(kind: str, identity: str | None, label: str) -> dict:
    return {"kind": kind, "id": identity, "label": label, "scope": "privateLibrary"}


def _item(item) -> dict:
    return {
        "id": item.id,
        "kind": "library",
        "title": item.displayName,
        "deletedAt": item.deletedAt.isoformat(),
        "expiresAt": (item.deletedAt + TRASH_RETENTION).isoformat(),
        "scope": "privateLibrary",
        "location": _location(
            "libraryFolder" if item.parentFolder_id else "libraryRoot",
            item.parentFolder_id,
            item.parentFolder.displayName if item.parentFolder_id else "私人",
        ),
        "deletedBy": _actor(item.deletedBy),
    }


def _filter_library(filters, library):
    if filters["query"]:
        library = library.filter(displayName__icontains=filters["query"])
    if filters["deletedByUserId"]:
        library = library.filter(deletedBy_id=filters["deletedByUserId"])
    if filters["locationKind"] == "libraryRoot":
        library = library.filter(parentFolder__isnull=True)
    elif filters["locationKind"] == "libraryFolder":
        library = library.filter(parentFolder_id=filters["locationId"])
    return library


def _filter_options(library) -> dict:
    actor_ids = library.exclude(deletedBy__isnull=True).values_list("deletedBy_id", flat=True)
    users = get_user_model().objects.filter(id__in=actor_ids).order_by("username", "id")
    locations = []
    if library.filter(parentFolder__isnull=True).exists():
        locations.append(_location("libraryRoot", None, "私人"))
    locations.extend(
        _location("libraryFolder", folder_id, name)
        for folder_id, name in library.exclude(parentFolder__isnull=True)
        .values_list("parentFolder_id", "parentFolder__displayName")
        .distinct()
    )
    locations.sort(key=lambda item: (item["label"], item["kind"], item["id"] or ""))
    return {"deletedBy": [_actor(user) for user in users], "locations": locations}


@router.get(
    "/workspaces/{workspace_id}/trash",
    auth=session_auth,
    response={200: TrashEnvelope} | COMMON_ERROR_RESPONSES,
)
def list_trash(request, workspace_id: str):
    if workspace_membership_for(request.user, workspace_id) is None:
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
            or cursor["itemKind"] != "library"
            or not isinstance(cursor["id"], str)
            or not cursor["id"]
            or cursor["filterHash"] != filter_hash
        ):
            raise ValueError("trash_cursor_invalid")
    except (TypeError, ValueError) as error:
        return Status(400, {"error": str(error)})

    library = UserLibraryObject.objects.filter(
        owner=request.user,
        objectKind__in=LIBRARY_FILE_KINDS,
        status="deleted",
        purgedAt__isnull=True,
        deletedAt__gt=trash_cutoff(),
    )
    filter_options = _filter_options(library)
    library = _after_cursor(_filter_library(filters, library), cursor).select_related(
        "parentFolder", "deletedBy"
    )
    items = [_item(item) for item in library[: TRASH_PAGE_SIZE + 1]]
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
    return {
        "items": page,
        "filterOptions": filter_options,
        "nextCursor": next_cursor,
        "hasMore": has_more,
    }
