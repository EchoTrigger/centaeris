from django.contrib.auth import get_user_model, logout
from django.db import transaction
from ninja import Router, Status
from pydantic import Field

from app_core.models import ExternalIdentity
from app_core.sso import SsoRejected, begin_authorization, complete_authorization, providers
from app_core.sso import provider_for
from .schema import ErrorResponse, StrictSchema
from .security import require_public_csrf, session_auth


router = Router(tags=["auth"], by_alias=True)


class AuthorizationRequest(StrictSchema):
    return_to: str = Field(default="/", min_length=1, max_length=2048)


class LinkRequest(StrictSchema):
    current_password: str = Field(min_length=1, max_length=4096)
    return_to: str = Field(default="/settings/security", min_length=1, max_length=2048)


class UnlinkRequest(StrictSchema):
    current_password: str = Field(min_length=1, max_length=4096)


ERROR_RESPONSES = {400: ErrorResponse, 401: ErrorResponse, 403: ErrorResponse, 404: ErrorResponse,
                   409: ErrorResponse, 429: ErrorResponse}


@router.get("/sso/providers", auth=None, response={200: dict})
def list_sso_providers(request):
    return {"providers": [{"id": p.provider_id, "displayName": p.display_name} for p in providers()]}


@router.get("/account/external-identities", auth=session_auth, response={200: dict, 401: ErrorResponse})
def list_external_identities(request):
    identities = {item.provider_id: item for item in request.user.external_identities.all()}
    configured = {p.provider_id: p for p in providers()}
    result = []
    for provider_id in dict.fromkeys([*configured, *identities]):
        provider = configured.get(provider_id)
        identity = identities.get(provider_id)
        result.append({"id": provider_id, "displayName": provider.display_name if provider else identity.provider_name,
            "enabled": bool(provider), "identity": None if not identity else {
                "id": identity.pk, "displayName": identity.display_name,
                "subjectHint": "••••" + identity.subject[-4:],
                "createdAt": identity.created_at.isoformat(),
                "lastLoginAt": identity.last_login_at.isoformat() if identity.last_login_at else None,
                "isCurrentSession": request.session.get("externalIdentityId") == identity.pk}})
    return {"providers": result, "canVerifyPassword": request.user.has_usable_password()}


@router.post("/sso/{provider_id}/login", auth=require_public_csrf, response={200: dict} | ERROR_RESPONSES)
def start_sso_login(request, provider_id: str, payload: AuthorizationRequest):
    try:
        return begin_authorization(request, provider_for(provider_id), purpose="login", return_to=payload.return_to)
    except SsoRejected as error:
        return Status(error.status, {"error": error.code})


@router.post("/account/external-identities/{provider_id}/link", auth=session_auth, response={200: dict} | ERROR_RESPONSES)
def start_sso_link(request, provider_id: str, payload: LinkRequest):
    try:
        return begin_authorization(request, provider_for(provider_id), purpose="link",
            return_to=payload.return_to, current_password=payload.current_password)
    except SsoRejected as error:
        return Status(error.status, {"error": error.code})


@router.get(
    "/sso/{provider_id}/callback",
    auth=None,
    response={302: None},
    openapi_extra={
        "responses": {302: {
            "headers": {"Location": {"schema": {"type": "string", "format": "uri"}}},
        }},
    },
)
def sso_callback(request, provider_id: str):
    return complete_authorization(request, provider_id)


@router.post("/account/external-identities/{identity_id}/unlink", auth=session_auth, response={200: dict} | ERROR_RESPONSES)
@transaction.atomic
def unlink_external_identity(request, identity_id: str, payload: UnlinkRequest):
    user = get_user_model().objects.select_for_update().get(pk=request.user.pk, is_active=True)
    if not user.has_usable_password():
        return Status(409, {"error": "sso_local_login_required"})
    if not user.check_password(payload.current_password):
        return Status(403, {"error": "account_current_password_invalid"})
    identity = ExternalIdentity.objects.filter(pk=identity_id, user=user).first()
    if not identity:
        return Status(404, {"error": "sso_identity_not_found"})
    signed_out = request.session.get("externalIdentityId") == identity.pk
    identity.delete()
    if signed_out:
        logout(request)
    return {"ok": True, "signedOut": signed_out}
