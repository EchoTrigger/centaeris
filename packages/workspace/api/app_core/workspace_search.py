"""Browser search over current authorized materials and explicitly shared results."""
from urllib.parse import quote, urlencode

from django.core import signing
from django.db.models import F, OuterRef, Q, Subquery, Value
from django.db.models.functions import Greatest, Lower, StrIndex, Substr

from .assets import source_object_is_granted
from .material_contract import KnowledgeError, MAX_READ_BYTES
from .material_storage import read_stored
from .models import DerivedRepresentation, KnowledgeSegment, SourceObject, UserLibraryObject
from .workspace_access import source_access_map_for_workspace_member, workspace_membership_for

CURSOR_SALT = "workspace.browser-search.v1"
SNIPPET_LENGTH = 240
READ_LINES = 200


def _scope(user, workspace_id):
    if workspace_membership_for(user, workspace_id) is None:
        raise KnowledgeError("workspace_not_found", 404)
    library = UserLibraryObject.objects.filter(owner=user, status="ready", deletedAt__isnull=True).exclude(objectKind="folder")
    granted = source_access_map_for_workspace_member(user, workspace_id)
    if granted is None:
        raise KnowledgeError("workspace_not_found", 404)
    sources = SourceObject.objects.filter(workspace_id=workspace_id, source_id__in=granted,
        source__status="ready", status="ready", deletedAt__isnull=True, objectType="file")
    return library, sources


def _with_content(queryset, kind, query):
    # Only a representation of the owner's current bytes can contribute a hit.
    # Different processing specifications remain distinct immutable revisions.
    representations = DerivedRepresentation.objects.filter(ownerKind=kind, ownerId=OuterRef("pk"),
        ownerContentGeneration=OuterRef("contentGeneration"), ownerSha256=OuterRef("sha256"))
    queryset = queryset.annotate(_representation=Subquery(representations.order_by("-createdAt", "representationId").values("pk")[:1]))
    segments = KnowledgeSegment.objects.filter(representation_id=OuterRef("_representation")).annotate(
        _folded=Lower("boundedText")).filter(_folded__contains=query.lower()).annotate(
        _start=Greatest(StrIndex("_folded", Value(query.lower())) - Value(70), Value(1)),
    ).annotate(_snippet=Substr("boundedText", F("_start"), max(SNIPPET_LENGTH, len(query) + 140))).order_by("ordinal", "segmentId")
    return queryset.annotate(_snippet=Subquery(segments.values("_snippet")[:1]),
        _segment_line=Subquery(segments.values("locator__startLine")[:1]), _name=Lower("displayName")).filter(
        Q(_name__contains=query.lower()) | Q(_snippet__isnull=False))


def _decode_cursor(cursor, selection):
    if cursor is None:
        return ""
    try:
        value = signing.loads(cursor, salt=CURSOR_SALT, fallback_keys=[])
        if (not isinstance(value, dict) or set(value) != {"selection", "after"} or value["selection"] != selection
                or not isinstance(value["after"], str) or not value["after"]):
            raise ValueError
        return value["after"]
    except (signing.BadSignature, ValueError, TypeError):
        raise KnowledgeError("workspace_search_cursor_invalid", 400) from None


def _after(queryset, prefix, after):
    # Each source yields one bounded page. Merging these pages preserves the same
    # global keyset order without loading every authorized object into Python.
    if after.startswith(prefix):
        return queryset.filter(pk__gt=after[len(prefix):])
    return queryset.none() if after and prefix < after.split(":", 1)[0] + ":" else queryset


def search_workspace(user, workspace_id, *, query, limit=20, cursor=None):
    if not isinstance(query, str) or not query.strip() or len(query) > 256 or type(limit) is not int or not 1 <= limit <= 50:
        raise KnowledgeError("workspace_search_query_invalid", 400)
    query = query.strip()
    library, sources = _scope(user, workspace_id)
    selection = {"userId": str(user.pk), "workspaceId": workspace_id, "query": query, "limit": limit}
    after = _decode_cursor(cursor, selection)
    base = f"/w/{quote(workspace_id, safe='')}"
    matches = []
    for kind, objects in (("userLibraryObject", library), ("sourceObject", sources)):
        found = _after(_with_content(objects, kind, query), kind + ":", after).order_by("pk")
        fields = ["id", "displayName", "_snippet", "_representation", "_segment_line"] + (["source_id"] if kind == "sourceObject" else [])
        for row in found.values(*fields)[:limit + 1]:
            location = {"highlight": query}
            if type(row["_segment_line"]) is int and row["_segment_line"] > 0:
                location["offset"] = row["_segment_line"] - 1
            if kind == "userLibraryObject" and row["_snippet"] is not None:
                location["representationId"] = row["_representation"]
            href = f"{base}/library/{quote(row['id'], safe='')}?{urlencode(location)}"
            if kind == "sourceObject":
                params = {"view": "knowledge", "source": row["source_id"], "object": row["id"], **location}
                if row["_representation"]:
                    params["representationId"] = row["_representation"]
                href = f"{base}/library?{urlencode(params)}"
            matches.append({"id": kind + ":" + row["id"], "kind": "material", "title": row["displayName"],
                            "snippet": row["_snippet"] or "", "href": href})
    from .issue_access import visible_issues
    from .models import IssueDeliverable
    deliveries = IssueDeliverable.objects.filter(issue_id__in=visible_issues(user, workspace_id).filter(
        archivedAt__isnull=True).values("pk")).filter(Q(author=user) | Q(recipient=user)).annotate(
        _folded=Lower("summary")).filter(_folded__contains=query.lower()).annotate(
        _start=Greatest(StrIndex("_folded", Value(query.lower())) - Value(70), Value(1)),
    ).annotate(_snippet=Substr("summary", F("_start"), max(SNIPPET_LENGTH, len(query) + 140)))
    for row in _after(deliveries, "deliverable:", after).order_by("pk").values(
        "id", "issue_id", "issue__title", "revision", "_snippet")[:limit + 1]:
        matches.append({"id": "deliverable:" + str(row["id"]), "kind": "deliverable", "title": row["issue__title"],
                        "snippet": row["_snippet"], "href": f"{base}/work/tasks/{quote(row['issue_id'], safe='')}?{urlencode({'deliverable': str(row['id'])})}"})
    matches.sort(key=lambda item: item["id"])
    has_more = len(matches) > limit
    rows = matches[:limit]
    next_cursor = signing.dumps({"selection": selection, "after": rows[-1]["id"]}, salt=CURSOR_SALT, compress=True) if has_more else None
    return {"matches": rows, "nextCursor": next_cursor}


def read_search_material(user, workspace_id, kind, object_id, *, representation_id=None, offset=0):
    library, sources = _scope(user, workspace_id)
    if kind not in {"sourceObject", "userLibraryObject"} or type(offset) is not int or offset < 0:
        raise KnowledgeError("workspace_search_query_invalid", 400)
    owner = (sources if kind == "sourceObject" else library).filter(pk=object_id).first()
    if owner is None or (kind == "sourceObject" and not source_object_is_granted(user, owner)):
        raise KnowledgeError("material_not_found", 404)
    query = DerivedRepresentation.objects.filter(ownerKind=kind, ownerId=owner.pk,
        ownerContentGeneration=owner.contentGeneration, ownerSha256=owner.sha256)
    if representation_id is not None:
        query = query.filter(pk=representation_id)
    representation = query.order_by("-createdAt", "representationId").first()
    if representation is None:
        raise KnowledgeError("material_representation_unavailable", 404)
    content = read_stored(representation.canonicalTextKey, representation.canonicalTextSizeBytes,
                          representation.canonicalTextSha256).decode("utf-8", errors="strict")
    lines = content.splitlines(keepends=True)
    if offset > len(lines):
        raise KnowledgeError("workspace_search_query_invalid", 400)
    selected = []
    size = 0
    for line in lines[offset:offset + READ_LINES]:
        line_size = len(line.encode("utf-8"))
        if size + line_size > MAX_READ_BYTES:
            if not selected:
                raise KnowledgeError("material_read_line_too_large", 422)
            break
        selected.append(line)
        size += line_size
    end = offset + len(selected)
    return {"material": {"id": owner.pk, "kind": kind, "displayName": owner.displayName,
                         "sourceId": owner.source_id if kind == "sourceObject" else None},
            "representationId": representation.pk, "content": "".join(selected), "startLine": offset + 1,
            "endLine": end, "nextOffset": end if end < len(lines) else None, "totalLines": len(lines)}
