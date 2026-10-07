import secrets
from datetime import timedelta
from typing import Literal

from django.db import transaction
from django.http import HttpResponse
from django.utils import timezone
from ninja import Router, Status
from pydantic import Field, field_validator, model_validator

from app_core.agent_definitions import available_agent_definitions
from app_core.app_delegation_contract import NATIVE_SCOPES, validate_scopes
from app_core.app_delegations import require_current_delegation, token_digest
from app_core.credentials import validate_display_name
from app_core.models import Agent, AppDelegationAuditEvent, BusinessApplication, UserAppDelegation, Workspace
from app_core.workspace_access import locked_workspace_membership_for
from .response_schema import COMMON_ERROR_RESPONSES
from .schema import StrictSchema
from .security import session_auth, superuser_auth


router = Router(tags=["app-delegations"], by_alias=True)


class BusinessAppResponse(StrictSchema):
    id: str
    name: str
    status: Literal["pending", "active", "revoked"]


class BusinessAppEnvelope(StrictSchema):
    app: BusinessAppResponse


class BusinessAppsEnvelope(StrictSchema):
    apps: list[BusinessAppResponse]


class RegisterAppRequest(StrictSchema):
    name: str = Field(min_length=1, max_length=160)

    @field_validator("name")
    @classmethod
    def valid_name(cls, value):
        validate_display_name(value)
        return value


class AppStatusRequest(StrictSchema):
    status: Literal["active", "revoked"]


class DelegationRequest(StrictSchema):
    app_id: str = Field(alias="appId", min_length=1, max_length=64)
    workspace_id: str = Field(alias="workspaceId", min_length=1, max_length=64)
    definition_id: str | None = Field(None, alias="definitionId", min_length=1, max_length=64)
    agent_id: str | None = Field(None, alias="agentId", min_length=1, max_length=64)
    scopes: list[str]
    expires_in_seconds: int | None = Field(3600, alias="expiresInSeconds", ge=300, le=86400)

    @field_validator("scopes")
    @classmethod
    def valid_scopes(cls, value):
        return validate_scopes(value)

    @model_validator(mode="after")
    def valid_target(self):
        if (self.definition_id is None) == (self.agent_id is None):
            raise ValueError("delegation_request_invalid")
        if self.agent_id is not None and not set(self.scopes) <= NATIVE_SCOPES:
            raise ValueError("delegation_request_invalid")
        return self


class RotateDelegationRequest(StrictSchema):
    expected_credential_version: int = Field(alias="expectedCredentialVersion", ge=1, strict=True)


class DelegationResponse(StrictSchema):
    id: str
    app_id: str = Field(alias="appId")
    app_name: str = Field(alias="appName")
    workspace_id: str = Field(alias="workspaceId")
    workspace_name: str = Field(alias="workspaceName")
    definition_id: str | None = Field(alias="definitionId")
    definition_name: str | None = Field(alias="definitionName")
    agent_id: str | None = Field(alias="agentId")
    agent_name: str | None = Field(alias="agentName")
    credential_version: int = Field(alias="credentialVersion")
    scopes: list[str]
    issuer: str
    audience: str
    created_at: str = Field(alias="createdAt")
    expires_at: str | None = Field(alias="expiresAt")
    revoked_at: str | None = Field(alias="revokedAt")


class DelegationsEnvelope(StrictSchema):
    delegations: list[DelegationResponse]


class IssuedDelegationResponse(StrictSchema):
    delegation: DelegationResponse
    access_token: str = Field(alias="accessToken")
    token_type: Literal["Bearer"] = Field(alias="tokenType")


def serialize_app(app):
    return {"id": app.id, "name": app.name, "status": app.status}


def serialize_delegation(grant):
    return {"id": grant.id, "appId": grant.app_id, "appName": grant.app.name,
            "workspaceId": grant.workspace_id, "workspaceName": grant.workspace.name,
            "definitionId": grant.definition_id, "definitionName": grant.definition.name if grant.definition_id else None,
            "agentId": grant.agent_id, "agentName": grant.agent.name if grant.agent_id else None,
            "credentialVersion": grant.credential_version,
            "scopes": grant.scopes, "issuer": grant.issuer, "audience": grant.audience,
            "createdAt": grant.created_at.isoformat(), "expiresAt": grant.expires_at.isoformat() if grant.expires_at else None,
            "revokedAt": grant.revoked_at.isoformat() if grant.revoked_at else None}


@router.get("/business-apps", auth=session_auth, response={200: BusinessAppsEnvelope} | COMMON_ERROR_RESPONSES)
def list_business_apps(request):
    return {"apps": [serialize_app(app) for app in BusinessApplication.objects.filter(status="active").order_by("name", "id")]}


@router.get("/admin/business-apps", auth=superuser_auth, response={200: BusinessAppsEnvelope} | COMMON_ERROR_RESPONSES)
def list_registered_business_apps(request):
    return {"apps": [serialize_app(app) for app in BusinessApplication.objects.all().order_by("created_at", "id")]}


@router.post("/admin/business-apps", auth=superuser_auth, response={201: BusinessAppEnvelope} | COMMON_ERROR_RESPONSES)
def register_business_app(request, payload: RegisterAppRequest):
    app = BusinessApplication.objects.create(name=payload.name, created_by=request.user)
    return Status(201, {"app": serialize_app(app)})


@router.patch("/admin/business-apps/{app_id}", auth=superuser_auth, response={200: BusinessAppEnvelope} | COMMON_ERROR_RESPONSES)
def update_business_app(request, app_id: str, payload: AppStatusRequest):
    with transaction.atomic():
        app = BusinessApplication.objects.select_for_update().filter(id=app_id).first()
        if app is None:
            return Status(404, {"error": "business_app_not_found"})
        if app.status == "revoked":
            return Status(409, {"error": "business_app_revoked"})
        app.status = payload.status
        app.save(update_fields=["status", "updated_at"])
    return {"app": serialize_app(app)}


@router.get("/account/app-delegations", auth=session_auth, response={200: DelegationsEnvelope} | COMMON_ERROR_RESPONSES)
def list_app_delegations(request, response: HttpResponse):
    response["Cache-Control"] = "no-store"
    grants = UserAppDelegation.objects.select_related("app", "workspace", "definition", "agent").filter(user=request.user).order_by("created_at", "id")
    return {"delegations": [serialize_delegation(grant) for grant in grants]}


@router.post("/account/app-delegations", auth=session_auth, response={201: IssuedDelegationResponse} | COMMON_ERROR_RESPONSES)
def issue_app_delegation(request, response: HttpResponse, payload: DelegationRequest):
    response["Cache-Control"] = "no-store"
    with transaction.atomic():
        membership = locked_workspace_membership_for(request.user, payload.workspace_id)
        if membership is None:
            return Status(404, {"error": "delegation_not_available"})
        app = BusinessApplication.objects.select_for_update().filter(id=payload.app_id, status="active").first()
        definition = None
        agent = None
        if payload.definition_id is not None:
            definition = available_agent_definitions(membership).filter(id=payload.definition_id).first()
        else:
            agent = Agent.objects.select_for_update(of=("self",)).filter(id=payload.agent_id, workspace=membership.workspace,
                owner=request.user, definition__isnull=True, status="active", business_branch__isnull=True).first()
        if app is None or (definition is None and agent is None):
            return Status(404, {"error": "delegation_not_available"})
        token = "cwa_" + secrets.token_urlsafe(32)
        grant = UserAppDelegation.objects.create(user=request.user, app=app, workspace=membership.workspace,
            definition=definition, agent=agent, membership_ref=membership.id, scopes=payload.scopes, token_digest=token_digest(token),
            expires_at=(timezone.now() + timedelta(seconds=payload.expires_in_seconds)
                if payload.expires_in_seconds is not None else None))
        AppDelegationAuditEvent.objects.create(delegation=grant, actor=request.user, action="issued", credential_version=1)
    return Status(201, {"delegation": serialize_delegation(grant), "accessToken": token, "tokenType": "Bearer"})


@router.post("/account/app-delegations/{delegation_id}/rotate", auth=session_auth,
    response={200: IssuedDelegationResponse} | COMMON_ERROR_RESPONSES)
def rotate_app_delegation(request, response: HttpResponse, delegation_id: str, payload: RotateDelegationRequest):
    response["Cache-Control"] = "no-store"
    workspace_id = UserAppDelegation.objects.filter(id=delegation_id, user=request.user).values_list("workspace_id", flat=True).first()
    if workspace_id is None:
        return Status(404, {"error": "delegation_not_found"})
    with transaction.atomic():
        membership = locked_workspace_membership_for(request.user, workspace_id)
        if membership is None:
            return Status(403, {"error": "delegation_not_available"})
        grant = require_current_delegation(delegation_id, lock=True)
        if grant.user_id != request.user.pk:
            return Status(404, {"error": "delegation_not_found"})
        if grant.credential_version != payload.expected_credential_version:
            return Status(409, {"error": "delegation_credential_conflict"})
        token = "cwa_" + secrets.token_urlsafe(32)
        digest = token_digest(token)
        version = grant.credential_version + 1
        # Ordinary model saves still reject all authority and credential edits.
        # Only this locked, audited mutation replaces the current credential.
        changed = UserAppDelegation.objects.filter(pk=grant.pk, credential_version=grant.credential_version,
            revoked_at__isnull=True).update(token_digest=digest, credential_version=version)
        if changed != 1:
            return Status(409, {"error": "delegation_credential_conflict"})
        grant.token_digest, grant.credential_version = digest, version
        AppDelegationAuditEvent.objects.create(delegation=grant, actor=request.user, action="rotated", credential_version=version)
    return {"delegation": serialize_delegation(grant), "accessToken": token, "tokenType": "Bearer"}


@router.delete("/account/app-delegations/{delegation_id}", auth=session_auth, response={204: None} | COMMON_ERROR_RESPONSES)
def revoke_app_delegation(request, delegation_id: str):
    workspace_id = UserAppDelegation.objects.filter(id=delegation_id, user=request.user).values_list("workspace_id", flat=True).first()
    if workspace_id is None:
        return Status(404, {"error": "delegation_not_found"})
    with transaction.atomic():
        Workspace.objects.select_for_update().get(id=workspace_id)
        grant = UserAppDelegation.objects.select_for_update().filter(id=delegation_id, user=request.user).first()
        if grant is None:
            return Status(404, {"error": "delegation_not_found"})
        if grant.revoked_at is None:
            grant.revoked_at = timezone.now()
            grant.save(update_fields=["revoked_at"])
            AppDelegationAuditEvent.objects.create(delegation=grant, actor=request.user, action="revoked",
                credential_version=grant.credential_version)
    return Status(204, None)
