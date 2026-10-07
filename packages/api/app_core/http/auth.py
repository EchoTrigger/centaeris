from django.conf import settings
from django.contrib.auth import authenticate, get_user_model, login, logout, update_session_auth_hash
from django.contrib.auth.password_validation import validate_password
from django.core.exceptions import ValidationError as DjangoValidationError
from django.db.models import Q
from django.db import transaction
from django.middleware.csrf import get_token
from ninja import Router, Status

from app_core.models import ModelConfig
from app_core.password_reset import queue_password_reset, reset_password

from .schema import (
    CsrfTokenResponse,
    ErrorResponse,
    LoginRequest,
    ModelsEnvelope,
    OkResponse,
    PasswordChangeRequest,
    PasswordResetConfirmRequest,
    PasswordResetRequest,
    UserEnvelope,
)
from .security import require_public_csrf, session_auth, usage_auth
from .serialization import serialize_model, serialize_user


router = Router(tags=["auth"], by_alias=True)


@router.get("/csrf", auth=None, response=CsrfTokenResponse)
def csrf_token(request):
    return {"csrfToken": get_token(request)}


@router.post(
    "/login",
    auth=require_public_csrf,
    response={200: UserEnvelope, 400: ErrorResponse, 401: ErrorResponse, 403: ErrorResponse},
)
def log_in(request, payload: LoginRequest):
    user = authenticate(
        request,
        username=payload.email,
        password=payload.password,
    )
    if user is None:
        return Status(401, {"error": "invalid_credentials"})
    login(request, user)
    return {"user": serialize_user(user)}


@router.post(
    "/logout",
    auth=require_public_csrf,
    response={200: OkResponse, 403: ErrorResponse},
)
def log_out(request):
    logout(request)
    return {"ok": True}


@router.patch(
    "/account/password",
    auth=session_auth,
    response={
        200: OkResponse,
        400: ErrorResponse,
        401: ErrorResponse,
        403: ErrorResponse,
        409: ErrorResponse,
    },
)
@transaction.atomic
def change_password(request, payload: PasswordChangeRequest):
    try:
        user = get_user_model().objects.select_for_update().get(pk=request.user.pk, is_active=True)
    except get_user_model().DoesNotExist:
        return Status(403, {"error": "account_current_password_invalid"})
    if not user.check_password(payload.current_password):
        return Status(403, {"error": "account_current_password_invalid"})
    if user.check_password(payload.new_password):
        return Status(409, {"error": "account_password_unchanged"})
    try:
        validate_password(payload.new_password, user=user)
    except DjangoValidationError:
        return Status(400, {"error": "account_password_invalid"})
    user.set_password(payload.new_password)
    user.save(update_fields=["password"])
    update_session_auth_hash(request, user)
    return {"ok": True}


@router.post(
    "/account/password-reset-requests",
    auth=require_public_csrf,
    response={202: OkResponse, 400: ErrorResponse, 403: ErrorResponse, 503: ErrorResponse},
)
def request_password_reset(request, payload: PasswordResetRequest):
    if not settings.PASSWORD_RESET_ENABLED:
        return Status(503, {"error": "account_password_reset_unavailable"})
    queue_password_reset(payload.email, request.META.get("REMOTE_ADDR", ""))
    return Status(202, {"ok": True})


@router.post(
    "/account/password-resets",
    auth=require_public_csrf,
    response={
        200: OkResponse,
        400: ErrorResponse,
        403: ErrorResponse,
        409: ErrorResponse,
        503: ErrorResponse,
    },
)
def confirm_password_reset(request, payload: PasswordResetConfirmRequest):
    if not settings.PASSWORD_RESET_ENABLED:
        return Status(503, {"error": "account_password_reset_unavailable"})
    error_code = reset_password(payload.uid, payload.token, payload.new_password)
    if error_code == "account_password_unchanged":
        return Status(409, {"error": error_code})
    if error_code:
        return Status(400, {"error": error_code})
    return {"ok": True}


@router.get(
    "/me",
    auth=session_auth,
    response={200: UserEnvelope, 401: ErrorResponse},
)
def current_user(request):
    return {"user": serialize_user(request.user)}


@router.get(
    "/models",
    auth=usage_auth("messages:submit"),
    response={200: ModelsEnvelope, 401: ErrorResponse},
)
def available_models(request):
    models = [
        serialize_model(model)
        for model in ModelConfig.objects.select_related("provider").filter(
            enabled=True,
            isCurrent=True,
        ).filter(
            Q(provider__isnull=True)
            | Q(provider__enabled=True, provider__archivedAt__isnull=True)
        )
    ]
    return {"models": models}
