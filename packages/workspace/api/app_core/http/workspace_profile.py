import base64
import binascii
import hashlib
from io import BytesIO

from django.db import transaction
from django.http import HttpResponse, JsonResponse
from ninja import Router, Status
from PIL import Image, ImageOps, UnidentifiedImageError
from pydantic import field_validator

from app_core.workspace_access import (
    WORKSPACE_ADMIN_ROLES,
    locked_workspace_membership_for,
    workspace_membership_for,
)

from .response_schema import COMMON_ERROR_RESPONSES, WorkspaceEnvelope
from .schema import StrictSchema
from .security import session_auth
from .serialization import serialize_workspace


router = Router(tags=["workspace-profile"], by_alias=True)
MAX_AVATAR_BYTES = 1024 * 1024
AVATAR_FORMATS = {"image/png": "PNG", "image/jpeg": "JPEG", "image/webp": "WEBP"}


class UpdateWorkspaceProfileRequest(StrictSchema):
    name: str
    avatar_data_url: str | None = None

    @field_validator("name")
    @classmethod
    def normalize_name(cls, value):
        value = value.strip()
        if not value or len(value) > 160:
            raise ValueError("workspace_name_invalid")
        return value


def normalize_avatar(value):
    try:
        header, encoded = value.split(",", 1)
        content_type = header.removeprefix("data:").removesuffix(";base64")
        if header != f"data:{content_type};base64" or content_type not in AVATAR_FORMATS:
            raise ValueError
        if len(encoded) > 4 * ((MAX_AVATAR_BYTES + 2) // 3):
            raise ValueError
        raw = base64.b64decode(encoded, validate=True)
        if not raw or len(raw) > MAX_AVATAR_BYTES:
            raise ValueError
        with Image.open(BytesIO(raw)) as image:
            if image.format != AVATAR_FORMATS[content_type]:
                raise ValueError
            image.verify()
        with Image.open(BytesIO(raw)) as image:
            image = ImageOps.exif_transpose(image).convert("RGBA")
            cropped = ImageOps.fit(image, (256, 256), method=Image.Resampling.LANCZOS)
            # Copy pixels into a fresh image so source metadata is not retained.
            normalized = Image.new("RGBA", cropped.size)
            normalized.paste(cropped)
            output = BytesIO()
            normalized.save(output, format="PNG")
        return output.getvalue()
    except (ValueError, binascii.Error, OSError, SyntaxError, UnidentifiedImageError, Image.DecompressionBombError):
        raise ValueError("workspace_avatar_invalid") from None


@router.patch("/workspaces/{workspace_id}/profile", auth=session_auth,
              response={200: WorkspaceEnvelope} | COMMON_ERROR_RESPONSES)
def update_workspace_profile(request, workspace_id: str, payload: UpdateWorkspaceProfileRequest):
    with transaction.atomic():
        membership = locked_workspace_membership_for(request.user, workspace_id,
                                                      allowed_roles=WORKSPACE_ADMIN_ROLES)
        if membership is None:
            return Status(403, {"error": "workspace_profile_forbidden"})
        workspace = membership.workspace
        if "avatar_data_url" in payload.model_fields_set:
            try:
                image = normalize_avatar(payload.avatar_data_url) if payload.avatar_data_url is not None else None
            except ValueError as error:
                return Status(400, {"error": str(error)})
            workspace.avatar_image = image
            workspace.avatar_digest = hashlib.sha256(image).hexdigest() if image is not None else ""
        workspace.name = payload.name
        workspace.save(update_fields=["name", "avatar_image", "avatar_digest", "updatedAt"])
        return {"workspace": serialize_workspace(workspace, membership.role)}


@router.get("/workspaces/{workspace_id}/avatar/{digest}", auth=session_auth,
            response={200: None} | COMMON_ERROR_RESPONSES,
            openapi_extra={"responses": {200: {
                "description": "Normalized Workspace avatar",
                "content": {"image/png": {"schema": {"type": "string", "format": "binary"}}},
            }}})
def workspace_avatar(request, workspace_id: str, digest: str):
    membership = workspace_membership_for(request.user, workspace_id)
    if membership is None or not digest or membership.workspace.avatar_digest != digest:
        return JsonResponse({"error": "workspace_avatar_not_found"}, status=404,
                            headers={"Cache-Control": "private, no-store"})
    response = HttpResponse(bytes(membership.workspace.avatar_image), content_type="image/png")
    response["X-Content-Type-Options"] = "nosniff"
    response["Cache-Control"] = "private, no-store"
    return response
