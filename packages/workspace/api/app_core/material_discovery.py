"""Bounded metadata discovery; does not process or search document bodies."""
from django.core import signing

from .assets import MAX_DIRECT_INPUT_BYTES, DeferredInputResolutionError, resolved_input_for_link, allocated_virtual_paths
from .library_materials import library_descriptor, personal_library_allowed, resolve_library_input, input_identity
from .material_access import declared_inputs_for_run
from .material_contract import KnowledgeError
from .models import UserLibraryObject, SessionAssetLink


def _attached_metadata(run, ref, declared):
    try:
        library = resolve_library_input(run, ref, include_content=False)
        if library is not None:
            resolved, _source, owner = library
        else:
            link = SessionAssetLink.objects.select_related("sourceObject__source", "userLibraryObject", "artifact").get(
                id=ref, session_id=run.session_id, workspace_id=run.workspace_id, attachedBy_id=run.user_id)
            resolved = resolved_input_for_link(run, link, allocated_virtual_paths(run)[ref])
            owner = link.sourceObject or link.userLibraryObject or link.artifact
        if input_identity(resolved) != declared["inputIdentity"] or resolved["sizeBytes"] != declared["sizeBytes"]:
            raise KnowledgeError("knowledge_input_binding_invalid", 409)
        return resolved, owner
    except SessionAssetLink.DoesNotExist:
        raise KnowledgeError("knowledge_input_not_authorized", 403) from None
    except DeferredInputResolutionError as error:
        raise KnowledgeError(error.errorCode, 409) from error


def list_materials(access, *, scope="all", query=None, folder=None, cursor=None, limit=50):
    if scope not in {"library", "attached", "all"} or type(limit) is not int or not 1 <= limit <= 100:
        raise KnowledgeError("material_list_arguments_invalid", 400)
    run = access.agent_run
    personal = personal_library_allowed(run)
    if scope == "library" and not personal:
        raise KnowledgeError("knowledge_input_not_authorized", 403)
    selection = {"run": run.pk, "scope": scope, "query": query, "folder": folder}
    after = ""
    if cursor is not None:
        try:
            decoded = signing.loads(cursor, salt="workspace.material.list.v1", fallback_keys=[])
            if set(decoded) != {"selection", "after"} or decoded["selection"] != selection or not isinstance(decoded["after"], str):
                raise ValueError
            after = decoded["after"]
        except (signing.BadSignature, ValueError, TypeError):
            raise KnowledgeError("material_list_cursor_invalid", 400) from None
    items = {}
    declared = declared_inputs_for_run(run)
    if scope in {"attached", "all"}:
        for ref, item in declared.items():
            if query and query.casefold() not in item["displayName"].casefold():
                continue
            _resolved, owner = _attached_metadata(run, ref, item)
            if folder is not None and getattr(owner, "parentFolder_id", None) != (folder or None):
                continue
            sources = ["attached", "library"] if scope == "all" and personal and item["inputIdentity"]["ownerKind"] == "userLibraryObject" else ["attached"]
            items[ref] = {"inputRef": ref, "displayName": item["displayName"], "contentType": item["contentType"],
                          "sizeBytes": item["sizeBytes"], "objectRef": item["inputIdentity"]["ownerId"],
                          "ownerKind": item["inputIdentity"]["ownerKind"], "sources": sources,
                          "canFetchOriginal": item["sizeBytes"] <= MAX_DIRECT_INPUT_BYTES, "contentState": "unknown"}
    if scope in {"library", "all"} and personal:
        library = UserLibraryObject.objects.filter(owner_id=run.user_id, status="ready")
        if query:
            library = library.filter(displayName__icontains=query)
        if folder is not None:
            if folder and not UserLibraryObject.objects.filter(pk=folder, owner_id=run.user_id, status="ready", objectKind="folder").exists():
                raise KnowledgeError("library_folder_not_found", 404)
            library = library.filter(parentFolder_id=folder or None)
        # The stable cursor is an object/ref key, not an offset into mutable names.
        # Query only one bounded page past the key; attached entries are at most 64.
        library = library.order_by("id")
        library_after = after.split("|", 1)[1] if after.startswith("library|") else ""
        if library_after:
            library = library.filter(id__gt=library_after)
        for owner in library[:limit + 1]:
            if owner.objectKind == "folder":
                ref = "folder:" + owner.pk
                resolved = {"inputRef": ref, "displayName": owner.displayName,
                            "contentType": owner.contentType, "sizeBytes": 0}
            else:
                resolved = library_descriptor(run, owner)
                ref = resolved["inputRef"]
            sources = list(dict.fromkeys(items.get(ref, {}).get("sources", []) + ["library"]))
            items[ref] = {**{key: resolved[key] for key in ("inputRef", "displayName", "contentType", "sizeBytes")},
                "objectRef": owner.pk, "ownerKind": "userLibraryObject", "objectKind": owner.objectKind,
                "parentFolderId": owner.parentFolder_id, "sources": sources,
                "canFetchOriginal": owner.objectKind != "folder" and (owner.sizeBytes or 0) <= MAX_DIRECT_INPUT_BYTES,
                "contentState": "not_applicable" if owner.objectKind == "folder" else "unknown"}
    ordered = sorted(items.values(), key=lambda item: ("library|" + item["objectRef"] if "library" in item["sources"] else "attached|" + item["inputRef"]))
    remaining = [("library|" + item["objectRef"] if "library" in item["sources"] else "attached|" + item["inputRef"], item) for item in ordered]
    remaining = [(key, item) for key, item in remaining if key > after]
    page = remaining[:limit]
    next_cursor = signing.dumps({"selection": selection, "after": page[-1][0]}, salt="workspace.material.list.v1") if len(remaining) > limit else None
    return {"scope": scope, "items": [item for _key, item in page], "nextCursor": next_cursor}
