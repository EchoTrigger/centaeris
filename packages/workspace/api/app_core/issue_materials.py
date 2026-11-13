"""Issue references reuse the existing Session asset authorization boundary."""
from .assets import captured_input_fields, source_object_is_granted
from .hosted_operations import HostedOperationError
from .models import SessionAssetLink, SourceObject, UserLibraryObject


def resolve_material(user, workspace_id, kind, asset_id, *, lock=False):
    if kind == "sourceObject":
        query = SourceObject.objects.select_related("source")
        if lock:
            query = query.select_for_update(of=("self",))
        asset = query.filter(id=asset_id, workspace_id=workspace_id, objectType="file", status="ready").first()
        return asset if asset and source_object_is_granted(user, asset) else None
    query = UserLibraryObject.objects
    if lock:
        query = query.select_for_update()
    return query.filter(id=asset_id, owner=user, status="ready",
                        objectKind__in=["file", "image", "note", "savedArtifact"]).first()


def material_matches(material, asset):
    return asset is not None and (material.contentGeneration, material.sizeBytes, material.sha256) == (
        asset.contentGeneration, asset.sizeBytes, asset.sha256)


def serialize_material(material, user, workspace_id):
    asset = resolve_material(user, workspace_id, material.assetKind, material.assetId)
    available = material_matches(material, asset)
    return {"id": str(material.pk), "assetKind": material.assetKind,
            "assetId": material.assetId if available else None,
            "displayName": material.displayName if available else None,
            "contentType": material.contentType if available else None,
            "sizeBytes": material.sizeBytes if available else None, "available": available}


def material_envelope(issue, user):
    return {"version": issue.version, "materials": [serialize_material(item, user, issue.workspace_id)
            for item in issue.materials.order_by("createdAt", "id")]}


def bind_issue_materials(issue, session, user):
    """Freeze accepted inputs as ordinary SessionAssetLinks in the caller transaction."""
    refs = []
    for material in issue.materials.order_by("assetKind", "assetId"):
        asset = resolve_material(user, issue.workspace_id, material.assetKind, material.assetId, lock=True)
        if asset is None:
            raise HostedOperationError(403, "issue_material_unavailable")
        if not material_matches(material, asset):
            raise HostedOperationError(409, "issue_material_version_changed")
        captured = captured_input_fields(asset)
        link = SessionAssetLink.objects.create(
            workspace=issue.workspace, session=session, attachedBy=user,
            capturedDisplayName=material.displayName, capturedContentType=material.contentType,
            **captured, **{material.assetKind: asset},
        )
        refs.append(link.pk)
    return sorted(refs)
