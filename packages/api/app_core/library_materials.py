"""Personal Library identity bindings. Session identity never grants Run authority."""
import hashlib
import json

from .assets import DeferredInputResolutionError, allocated_virtual_paths, resolved_input, _virtual_filename
from .models import RunMaterialScope, Session, SessionMaterialInput, UserLibraryObject
from .note_content import content_source
from .workspace_access import agent_run_membership_is_current


def personal_library_allowed(run):
    return RunMaterialScope.objects.filter(agent_run=run, personal_library=True).exists()


def identity_key(identity):
    return hashlib.sha256(json.dumps(identity, sort_keys=True, separators=(",", ":")).encode()).hexdigest()


def input_identity(resolved):
    return {"ownerKind": resolved["ownerKind"], "ownerId": resolved["objectRef"],
            "generation": int(resolved["sourceVersion"]), "sha256": resolved["sha256"]}


def remember_input(run, resolved):
    """Caller holds the Session lock; immutable identity/path are first-writer facts."""
    key = identity_key(input_identity(resolved))
    record, _ = SessionMaterialInput.objects.get_or_create(session_id=run.session_id, identity_key=key,
        defaults={"id": "session_input:" + hashlib.sha256(f"{run.session_id}:{key}".encode()).hexdigest(),
                  "input_ref": resolved["inputRef"], "resolved": resolved})
    return record


def remap_library_attachments(run, declared, message_refs):
    mapping = {}
    result = {}
    for item in declared:
        original_ref = item["inputRef"]
        identity = item["inputIdentity"]
        if identity["ownerKind"] == "userLibraryObject":
            record = SessionMaterialInput.objects.filter(session_id=run.session_id, identity_key=identity_key(identity)).first()
            if record is not None:
                item = {**item, "inputRef": record.input_ref, "displayName": record.resolved["displayName"],
                        "contentType": record.resolved["contentType"]}
        mapping[original_ref] = item["inputRef"]
        result[item["inputRef"]] = item
    return sorted(result.values(), key=lambda item: item["inputRef"]), sorted({mapping[ref] for ref in message_refs})


def library_descriptor(run, owner):
    identity = {"ownerKind": "userLibraryObject", "ownerId": owner.pk,
                "generation": owner.contentGeneration, "sha256": owner.sha256}
    existing = SessionMaterialInput.objects.filter(session_id=run.session_id, identity_key=identity_key(identity)).first()
    if existing is not None:
        return existing.resolved
    for item in run.authorization.payload["assetRefs"]:
        if item["inputIdentity"] == identity:
            return resolved_input(item["inputRef"], item["displayName"], item["contentType"], owner.pk,
                "userLibraryObject", allocated_virtual_paths(run)[item["inputRef"]], item["sizeBytes"],
                owner.sha256, str(owner.contentGeneration), "userProvided")
    ref = f"library:{owner.pk}:{owner.contentGeneration}:{owner.sha256.removeprefix('sha256:')}"
    path = f"inputs/{hashlib.sha256(ref.encode()).hexdigest()[:32]}/{_virtual_filename(owner.displayName)}"
    return resolved_input(ref, owner.displayName, owner.contentType, owner.pk, "userLibraryObject",
                          path, owner.sizeBytes, owner.sha256, str(owner.contentGeneration), "userProvided")


def resolve_library_input(run, ref, *, require_admission=False, include_content=True, include_note_body=True):
    declared = next((item for item in run.authorization.payload["assetRefs"] if item["inputRef"] == ref), None)
    if declared is not None and declared["inputIdentity"]["ownerKind"] != "userLibraryObject":
        return None
    if declared is None and not personal_library_allowed(run):
        return None
    record = SessionMaterialInput.objects.filter(session_id=run.session_id, input_ref=ref).first()
    if record is None and not ref.startswith("library:"):
        return None
    if not agent_run_membership_is_current(run) or not Session.objects.filter(
            pk=run.session_id, owner_id=run.user_id, owner__is_active=True, status="active").exists():
        raise DeferredInputResolutionError("access_revoked")
    if record is not None:
        resolved = record.resolved
        if resolved["ownerKind"] != "userLibraryObject":
            return None
        owner_id = resolved["objectRef"]
    elif ref.startswith("library:"):
        pieces = ref.split(":")
        if len(pieces) != 4:
            raise DeferredInputResolutionError("asset_unavailable")
        owner_id = pieces[1]
        resolved = None
    else:
        return None
    try:
        query = UserLibraryObject.objects.select_related("noteHead") if include_content else UserLibraryObject.objects
        if include_content and not include_note_body:
            query = query.defer("noteHead__markdown")
        owner = query.get(pk=owner_id, owner_id=run.user_id)
    except UserLibraryObject.DoesNotExist:
        raise DeferredInputResolutionError("asset_unavailable") from None
    if owner.status == "deleted":
        raise DeferredInputResolutionError("asset_removed")
    if owner.status != "ready" or owner.objectKind == "folder":
        raise DeferredInputResolutionError("access_revoked")
    if resolved is None:
        resolved = library_descriptor(run, owner)
        if resolved["inputRef"] != ref:
            raise DeferredInputResolutionError("stale_generation")
    if (str(owner.contentGeneration) != resolved["sourceVersion"] or owner.sha256 != resolved["sha256"]
            or owner.sizeBytes != resolved["sizeBytes"]):
        raise DeferredInputResolutionError("stale_generation")
    if declared is not None and declared["inputIdentity"] != input_identity(resolved):
        raise DeferredInputResolutionError("stale_generation")
    if require_admission and (record is None or not (record.confirmed or record.admissions.filter(agent_run=run, settled_absent=False).exists())):
        raise DeferredInputResolutionError("asset_unavailable")
    source = None
    if include_content:
        try:
            source = content_source(owner, include_note_body=include_note_body)
        except FileNotFoundError:
            raise DeferredInputResolutionError("asset_unavailable") from None
    return resolved, source, owner
