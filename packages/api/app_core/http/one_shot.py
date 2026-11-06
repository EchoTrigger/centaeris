"""Workspace-managed credentials and the external v1 single-response API."""
from django.conf import settings
from django.db import transaction
from django.http import JsonResponse
from django.utils import timezone
from ninja import Router
from ninja.security import HttpBearer
from pydantic import Field

from app_core.app_delegations import DelegationRejected
from app_core.hosted_operations import HostedOperationError
from app_core.material_contract import KnowledgeError
from app_core.models import AgentRun, OneShotCredential, OneShotOperation
from app_core.one_shot_credentials import (
    CredentialRejected,
    authenticate_credential,
    authority_is_current,
    create_credential,
)
from app_core.one_shot_protocol import (
    CANCEL_RUN_PATH,
    CREATE_RUN_PATH,
    GET_OPERATION_PATH,
    IDEMPOTENCY_HEADER,
    CancellationRequested,
    CreateRunRequest,
    DateTimeText,
    OperationReceipt,
)
from app_core.one_shot_runs import accept_run, operation_expiry, refresh_operation
from app_core.one_shot_stream import operation_receipt, stream_one_shot
from app_core.runtime_client import request_agent_run_cancellation
from app_core.session_model_settings import SessionModelSettingsError
from app_core.workspace_access import WORKSPACE_ADMIN_ROLES, locked_workspace_membership_for

from .response_schema import COMMON_ERROR_RESPONSES
from .schema import StrictSchema
from .security import session_auth
from .stream_response import OwnedAsyncStreamingHttpResponse, stream_database_call


router = Router(tags=["one-shot-runs"], by_alias=True)


class CreateCredentialRequest(StrictSchema):
    name: str = Field(min_length=1, max_length=120)
    model_config_ref: str
    plugin_names: list[str] = Field(default_factory=list, max_length=64)
    source_object_refs: list[str] = Field(default_factory=list, max_length=64)


class CredentialResponse(StrictSchema):
    id: str
    name: str
    workspace_id: str
    model_config_ref: str
    plugin_names: list[str]
    source_object_refs: list[str]
    created_at: DateTimeText
    revoked_at: DateTimeText | None


class CredentialListResponse(StrictSchema):
    credentials: list[CredentialResponse]


class CredentialEnvelope(StrictSchema):
    credential: CredentialResponse


class CreatedCredentialResponse(CredentialEnvelope):
    secret: str


class OneShotAuth(HttpBearer):
    openapi_description = (
        "Use the complete one-shot credential as Authorization: Bearer <credential>. "
        "The credential fixes the workspace and allowed execution scope. Use the same credential "
        "for submission, status lookup and cancellation. Browser session cookies or an authenticated "
        "product session cannot be combined with this credential. Revoked credentials cannot read "
        "their old receipts."
    )

    def __call__(self, request):
        if settings.SESSION_COOKIE_NAME in request.COOKIES or request.user.is_authenticated:
            raise DelegationRejected("authentication_mixed", 400)
        return super().__call__(request)

    def authenticate(self, request, token):
        try:
            return authenticate_credential(token)
        except CredentialRejected as error:
            raise DelegationRejected(error.code, error.status) from error


external_auth = OneShotAuth()


def _json(value, status=200):
    response = JsonResponse(value, status=status)
    response["Cache-Control"] = "no-store"
    return response


def _credential_value(credential):
    return {
        "id": credential.pk,
        "name": credential.name,
        "workspaceId": credential.workspace_id,
        "modelConfigRef": credential.modelConfig_id,
        "pluginNames": credential.pluginNames,
        "sourceObjectRefs": credential.sourceObjectRefs,
        "createdAt": credential.createdAt.isoformat(),
        "revokedAt": credential.revokedAt.isoformat() if credential.revokedAt else None,
    }


def _admin(user, workspace_id):
    member = locked_workspace_membership_for(user, workspace_id, allowed_roles=WORKSPACE_ADMIN_ROLES)
    if member is None:
        raise DelegationRejected("workspace_forbidden", 403)
    return member


@router.get(
    "/workspaces/{workspace_id}/api-credentials", auth=session_auth,
    response={200: CredentialListResponse} | COMMON_ERROR_RESPONSES,
)
def list_api_credentials(request, workspace_id: str):
    with transaction.atomic():
        _admin(request.user, workspace_id)
        credentials = OneShotCredential.objects.filter(workspace_id=workspace_id).order_by("createdAt", "id")
        return _json({"credentials": [_credential_value(item) for item in credentials]})


@router.post(
    "/workspaces/{workspace_id}/api-credentials", auth=session_auth,
    response={201: CreatedCredentialResponse} | COMMON_ERROR_RESPONSES,
)
def create_api_credential(request, workspace_id: str, payload: CreateCredentialRequest):
    try:
        credential, token = create_credential(
            request.user, workspace_id, payload.model_config_ref,
            plugin_names=payload.plugin_names,
            source_object_refs=payload.source_object_refs,
            name=payload.name,
        )
        return _json({"credential": _credential_value(credential), "secret": token}, 201)
    except CredentialRejected as error:
        return _json({"error": error.code}, error.status)
    except ValueError as error:
        return _json({"error": str(error)}, 400)


@router.delete(
    "/workspaces/{workspace_id}/api-credentials/{credential_id}", auth=session_auth,
    response={200: CredentialEnvelope} | COMMON_ERROR_RESPONSES,
)
def revoke_api_credential(request, workspace_id: str, credential_id: str):
    with transaction.atomic():
        _admin(request.user, workspace_id)
        credential = OneShotCredential.objects.select_for_update().filter(
            pk=credential_id, workspace_id=workspace_id,
        ).first()
        if credential is None:
            return _json({"error": "credential_not_found"}, 404)
        if credential.revokedAt is None:
            credential.revokedAt = timezone.now()
            credential.save(update_fields=["revokedAt"])
    # Reconciliation also observes revocation when a live control request is unavailable.
    return _json({"credential": _credential_value(credential)})


@router.post(
    CREATE_RUN_PATH, auth=external_auth, response={200: OperationReceipt} | COMMON_ERROR_RESPONSES,
    description=(
        "Submit one execution scoped to this credential and Idempotency-Key. Inspect the response "
        "Content-Type to choose the parser; the Accept header does not choose the response format. "
        "Only a newly accepted submission can deliver text and artifacts over SSE. Disconnecting does "
        "not cancel execution, and the stream and results cannot be replayed. Save the original "
        "operation ID before sending, and use it to check status after a timeout. Query parameters "
        "are rejected."
    ),
    openapi_extra={
        "parameters": [{
            "in": "header", "name": IDEMPOTENCY_HEADER, "required": True,
            "schema": {"type": "string"},
            "description": (
                "Client-generated operation ID: Unix seconds, a dot, and a lowercase UUIDv4. "
                "Keep the same credential, key and request body for retries. A previously accepted "
                "operation with different input returns 409. The key expires 24 hours after its "
                "timestamp; timestamps more than five minutes in the future are rejected."
            ),
        }],
        "responses": {200: {
            "description": (
                "A newly accepted operation returns text/event-stream. An already accepted "
                "operation with the same credential, key and request returns an application/json "
                "receipt only. A retry after an earlier submission was not accepted can still "
                "be the first acceptance and return SSE."
            ),
            "content": {"text/event-stream": {"schema": {"type": "string"}}},
        }},
    },
)
async def create_one_shot_run(request, payload: CreateRunRequest):
    if request.GET:
        return _json({"error": "request_invalid"}, 400)
    operation_id = request.headers.get(IDEMPOTENCY_HEADER, "")
    try:
        operation, run = await stream_database_call(accept_run)(request.auth, operation_id, payload)
    except (HostedOperationError, KnowledgeError) as error:
        return _json({"error": error.code}, error.status)
    except CredentialRejected as error:
        return _json({"error": error.code}, error.status)
    except SessionModelSettingsError as error:
        return _json({"error": str(error)}, 400)
    except RuntimeError:
        return _json({"error": "runtime_unavailable"}, 503)
    if run is None:
        operation = await stream_database_call(refresh_operation)(operation)
        return _json(operation_receipt(operation))

    async def authority_check():
        return await stream_database_call(authority_is_current)(request.auth)

    response = OwnedAsyncStreamingHttpResponse(
        stream_one_shot(operation, run, authority_check), content_type="text/event-stream",
    )
    response["Cache-Control"] = "no-store"
    response["X-Accel-Buffering"] = "no"
    return response


@router.get(
    GET_OPERATION_PATH, auth=external_auth, response={200: OperationReceipt} | COMMON_ERROR_RESPONSES,
    description=(
        "Read the status receipt using the original operation ID and the same credential that "
        "submitted it. This endpoint never returns answer text, artifact bytes or a replay stream. "
        "A valid, unexpired ID with no operation under this credential returns 404; an expired "
        "operation ID returns 410. Query parameters are rejected."
    ),
    openapi_extra={"responses": {200: {
        "description": "Operation status receipt only; no results or replay.",
    }}},
)
def get_one_shot_operation(request, operation_id: str):
    if request.GET:
        return _json({"error": "request_invalid"}, 400)
    try:
        operation_expiry(operation_id)
    except HostedOperationError as error:
        return _json({"error": error.code}, error.status)
    operation = OneShotOperation.objects.filter(credential=request.auth, operationId=operation_id).first()
    if operation is None:
        return _json({"error": "operation_not_found"}, 404)
    return _json(operation_receipt(refresh_operation(operation)))


@router.post(
    CANCEL_RUN_PATH, auth=external_auth,
    response={200: OperationReceipt, 202: CancellationRequested} | COMMON_ERROR_RESPONSES,
    description=(
        "Request cancellation using the runId from the original operation and the same credential. "
        "Send an empty body (zero bytes, not a JSON object) and no query parameters. A 202 response "
        "only acknowledges the cancellation request; use the original operation ID to confirm the "
        "terminal run status. Cancellation does not undo effects already performed."
    ),
    openapi_extra={"responses": {
        200: {
            "description": "The run is already terminal; return its operation status receipt.",
        },
        202: {
            "description": "Cancellation requested; this does not confirm that execution has stopped.",
        },
    }},
)
def cancel_one_shot_run(request, run_id: str):
    if request.META.get("CONTENT_LENGTH", "0") not in ("", "0") or request.GET:
        return _json({"error": "request_invalid"}, 400)
    operation = OneShotOperation.objects.filter(credential=request.auth, agentRunId=run_id).first()
    if operation is None:
        return _json({"error": "agent_run_not_found"}, 404)
    if operation.expiresAt <= timezone.now():
        return _json({"error": "operation_expired"}, 410)
    run = AgentRun.objects.filter(pk=run_id).first()
    if run is None or run.status in {"completed", "failed", "cancelled"}:
        return _json(operation_receipt(refresh_operation(operation)))
    try:
        result = request_agent_run_cancellation(run)
    except RuntimeError:
        return _json({"error": "agent_run_cancel_unavailable"}, 503)
    if result["disposition"] == "terminal":
        from app_core.session_event import project_committed_agent_run
        project_committed_agent_run(run, result["terminalState"])
        return _json(operation_receipt(refresh_operation(operation)))
    return _json(CancellationRequested(runId=run_id, cancelStatus="requested"), 202)
