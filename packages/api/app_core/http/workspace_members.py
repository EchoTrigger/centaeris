import hashlib
import secrets
import unicodedata
from datetime import timedelta
from typing import Literal
from urllib.parse import quote

from django.conf import settings
from django.contrib.auth import get_user_model, login
from django.contrib.auth.password_validation import validate_password
from django.core.exceptions import ValidationError
from django.db import IntegrityError, transaction
from django.utils import timezone
from ninja import Router, Status
from pydantic import Field, field_validator

from app_core.models import (
    SourceGrant,
    Workspace,
    WorkspaceGroup,
    WorkspaceInvitation,
    WorkspaceMembership,
    normalize_workspace_invitation_email,
)
from app_core.agent_defaults import ensure_default_agent
from app_core.workspace_access import (
    WORKSPACE_ADMIN_ROLES,
    locked_workspace_membership_for,
    workspace_membership_for,
)

from .response_schema import (
    COMMON_ERROR_RESPONSES,
    WorkspaceInvitationAcceptedResponse,
    WorkspaceInvitationEnvelope,
    WorkspaceInvitationPreviewResponse,
    WorkspaceInvitationsEnvelope,
    WorkspaceGroupEnvelope,
    WorkspaceGroupMembersEnvelope,
    WorkspaceGroupsEnvelope,
    WorkspaceMemberEnvelope,
    WorkspaceMembersEnvelope,
    WorkspaceOwnershipTransferredResponse,
)
from .schema import ErrorResponse, OkResponse, StrictSchema
from .security import require_public_csrf, session_auth


router = Router(tags=["workspace-members"], by_alias=True)
INVITATION_EXPIRY = timedelta(hours=72)


class CreateWorkspaceInvitationRequest(StrictSchema):
    email: str
    role: Literal["admin", "member"]

    @field_validator("email")
    @classmethod
    def normalize_email(cls, value: str) -> str:
        return normalize_workspace_invitation_email(value)


class PreviewWorkspaceInvitationRequest(StrictSchema):
    token: str


class AcceptWorkspaceInvitationRequest(StrictSchema):
    token: str
    name: str | None = None
    password: str | None = None


class UpdateWorkspaceMemberRoleRequest(StrictSchema):
    role: Literal["admin", "member"]


class WorkspaceGroupNameRequest(StrictSchema):
    name: str = Field(min_length=1, max_length=160)

    @field_validator("name")
    @classmethod
    def normalize_name(cls, value: str) -> str:
        normalized = unicodedata.normalize("NFC", value.strip())
        if not normalized:
            raise ValueError("workspace_group_name_required")
        return normalized


class TransferWorkspaceOwnershipRequest(StrictSchema):
    target_membership_id: str = Field(
        alias="targetMembershipId",
        min_length=1,
        max_length=64,
    )
    current_password: str = Field(
        alias="currentPassword",
        min_length=1,
        max_length=4096,
    )


def _token_digest(token: str) -> str | None:
    if not token or token != token.strip() or len(token) > 128:
        return None
    return f"sha256:{hashlib.sha256(token.encode('utf-8')).hexdigest()}"


def _serialize_invitation(invitation: WorkspaceInvitation) -> dict:
    return {
        "id": invitation.id,
        "email": invitation.email,
        "role": invitation.role,
        "status": invitation.status,
        "expiresAt": invitation.expires_at.isoformat(),
        "createdAt": invitation.created_at.isoformat(),
    }


def _serialize_member(membership: WorkspaceMembership) -> dict:
    return {
        "membershipId": membership.id,
        "userId": str(membership.user_id),
        "email": membership.user.email or membership.user.username,
        "role": membership.role,
        "createdAt": membership.created_at.isoformat(),
    }


def _serialize_group(group: WorkspaceGroup) -> dict:
    return {
        "id": group.id,
        "name": group.name,
        "kind": group.kind,
        "createdAt": group.createdAt.isoformat(),
    }


def _locked_workspace_and_actor_membership(
    user,
    workspace_id: str,
    *,
    allowed_roles=WORKSPACE_ADMIN_ROLES,
):
    actor_membership = locked_workspace_membership_for(
        user,
        workspace_id,
        allowed_roles=allowed_roles,
    )
    if actor_membership is None:
        return None
    return actor_membership.workspace, actor_membership


@router.get(
    "/workspaces/{workspace_id}/members",
    auth=session_auth,
    response={200: WorkspaceMembersEnvelope} | COMMON_ERROR_RESPONSES,
)
def list_workspace_members(request, workspace_id: str):
    membership = workspace_membership_for(
        request.user,
        workspace_id,
        allowed_roles=WORKSPACE_ADMIN_ROLES,
    )
    if membership is None:
        return Status(404, {"error": "workspace_not_found"})
    members = membership.workspace.memberships.select_related("user").order_by(
        "created_at", "id"
    )
    return {"members": [_serialize_member(item) for item in members]}


@router.get(
    "/workspaces/{workspace_id}/groups",
    auth=session_auth,
    response={200: WorkspaceGroupsEnvelope} | COMMON_ERROR_RESPONSES,
)
def list_workspace_groups(request, workspace_id: str):
    membership = workspace_membership_for(request.user, workspace_id)
    if membership is None:
        return Status(404, {"error": "workspace_not_found"})
    groups = membership.workspace.groups.order_by("kind", "createdAt", "id")
    return {"groups": [_serialize_group(group) for group in groups]}


@router.post(
    "/workspaces/{workspace_id}/groups",
    auth=session_auth,
    response={201: WorkspaceGroupEnvelope} | COMMON_ERROR_RESPONSES,
)
def create_workspace_group(
    request,
    workspace_id: str,
    payload: WorkspaceGroupNameRequest,
):
    with transaction.atomic():
        locked = _locked_workspace_and_actor_membership(request.user, workspace_id)
        if locked is None:
            return Status(404, {"error": "workspace_not_found"})
        workspace, _actor_membership = locked
        if WorkspaceGroup.objects.filter(
            workspace=workspace,
            name=payload.name,
        ).exists():
            return Status(409, {"error": "workspace_group_name_exists"})
        group = WorkspaceGroup.objects.create(
            workspace=workspace,
            name=payload.name,
            kind="custom",
            createdBy=request.user,
        )
    return Status(201, {"group": _serialize_group(group)})


@router.patch(
    "/workspaces/{workspace_id}/groups/{group_id}",
    auth=session_auth,
    response={200: WorkspaceGroupEnvelope} | COMMON_ERROR_RESPONSES,
)
def update_workspace_group(
    request,
    workspace_id: str,
    group_id: str,
    payload: WorkspaceGroupNameRequest,
):
    with transaction.atomic():
        locked = _locked_workspace_and_actor_membership(request.user, workspace_id)
        if locked is None:
            return Status(404, {"error": "workspace_not_found"})
        workspace, _actor_membership = locked
        group = WorkspaceGroup.objects.select_for_update().filter(
            id=group_id,
            workspace=workspace,
        ).first()
        if group is None:
            return Status(404, {"error": "workspace_group_not_found"})
        if group.kind == "all_members":
            return Status(409, {"error": "workspace_system_group_immutable"})
        if group.name == payload.name:
            return Status(409, {"error": "workspace_group_name_unchanged"})
        if WorkspaceGroup.objects.filter(
            workspace=workspace,
            name=payload.name,
        ).exclude(id=group.id).exists():
            return Status(409, {"error": "workspace_group_name_exists"})
        group.name = payload.name
        group.save(update_fields=["name"])
    return {"group": _serialize_group(group)}


@router.delete(
    "/workspaces/{workspace_id}/groups/{group_id}",
    auth=session_auth,
    response={200: OkResponse} | COMMON_ERROR_RESPONSES,
)
def delete_workspace_group(request, workspace_id: str, group_id: str):
    with transaction.atomic():
        locked = _locked_workspace_and_actor_membership(request.user, workspace_id)
        if locked is None:
            return Status(404, {"error": "workspace_not_found"})
        workspace, _actor_membership = locked
        group = WorkspaceGroup.objects.select_for_update().filter(
            id=group_id,
            workspace=workspace,
        ).first()
        if group is None:
            return Status(404, {"error": "workspace_group_not_found"})
        if group.kind == "all_members":
            return Status(409, {"error": "workspace_system_group_immutable"})
        SourceGrant.objects.filter(workspaceGroup=group).delete()
        group.delete()
    return {"ok": True}


@router.get(
    "/workspaces/{workspace_id}/groups/{group_id}/members",
    auth=session_auth,
    response={200: WorkspaceGroupMembersEnvelope} | COMMON_ERROR_RESPONSES,
)
def list_workspace_group_members(request, workspace_id: str, group_id: str):
    membership = workspace_membership_for(
        request.user,
        workspace_id,
        allowed_roles=WORKSPACE_ADMIN_ROLES,
    )
    if membership is None:
        return Status(404, {"error": "workspace_not_found"})
    group = WorkspaceGroup.objects.filter(
        id=group_id,
        workspace=membership.workspace,
    ).first()
    if group is None:
        return Status(404, {"error": "workspace_group_not_found"})
    members = (
        membership.workspace.memberships
        if group.kind == "all_members"
        else group.members
    ).select_related("user").order_by("created_at", "id")
    return {"members": [_serialize_member(item) for item in members]}


def _change_workspace_group_member(
    request,
    workspace_id: str,
    group_id: str,
    membership_id: str,
    *,
    add: bool,
):
    with transaction.atomic():
        locked = _locked_workspace_and_actor_membership(request.user, workspace_id)
        if locked is None:
            return Status(404, {"error": "workspace_not_found"})
        workspace, _actor_membership = locked
        group = WorkspaceGroup.objects.select_for_update().filter(
            id=group_id,
            workspace=workspace,
        ).first()
        if group is None:
            return Status(404, {"error": "workspace_group_not_found"})
        if group.kind == "all_members":
            return Status(409, {"error": "workspace_system_group_immutable"})
        target = WorkspaceMembership.objects.select_for_update().filter(
            id=membership_id,
            workspace=workspace,
        ).first()
        if target is None:
            return Status(404, {"error": "workspace_member_not_found"})
        if add:
            group.members.add(target)
        else:
            group.members.remove(target)
    return {"ok": True}


@router.put(
    "/workspaces/{workspace_id}/groups/{group_id}/members/{membership_id}",
    auth=session_auth,
    response={200: OkResponse} | COMMON_ERROR_RESPONSES,
)
def add_workspace_group_member(
    request,
    workspace_id: str,
    group_id: str,
    membership_id: str,
):
    return _change_workspace_group_member(
        request,
        workspace_id,
        group_id,
        membership_id,
        add=True,
    )


@router.delete(
    "/workspaces/{workspace_id}/groups/{group_id}/members/{membership_id}",
    auth=session_auth,
    response={200: OkResponse} | COMMON_ERROR_RESPONSES,
)
def remove_workspace_group_member(
    request,
    workspace_id: str,
    group_id: str,
    membership_id: str,
):
    return _change_workspace_group_member(
        request,
        workspace_id,
        group_id,
        membership_id,
        add=False,
    )


@router.patch(
    "/workspaces/{workspace_id}/members/{membership_id}",
    auth=session_auth,
    response={200: WorkspaceMemberEnvelope} | COMMON_ERROR_RESPONSES,
)
def update_workspace_member_role(
    request,
    workspace_id: str,
    membership_id: str,
    payload: UpdateWorkspaceMemberRoleRequest,
):
    with transaction.atomic():
        locked = _locked_workspace_and_actor_membership(request.user, workspace_id)
        if locked is None:
            return Status(404, {"error": "workspace_not_found"})
        workspace, actor_membership = locked
        target = (
            WorkspaceMembership.objects.select_for_update()
            .select_related("user")
            .filter(id=membership_id, workspace=workspace)
            .first()
        )
        if target is None:
            return Status(404, {"error": "workspace_member_not_found"})
        if target.id == actor_membership.id:
            return Status(409, {"error": "workspace_member_self_operation_forbidden"})
        if target.role == "owner":
            return Status(409, {"error": "workspace_owner_transfer_required"})
        if target.role == payload.role:
            return Status(409, {"error": "workspace_member_role_unchanged"})

        target.role = payload.role
        target.save(update_fields=["role", "updated_at"])
    return {"member": _serialize_member(target)}


@router.delete(
    "/workspaces/{workspace_id}/members/{membership_id}",
    auth=session_auth,
    response={200: OkResponse} | COMMON_ERROR_RESPONSES,
)
def remove_workspace_member(request, workspace_id: str, membership_id: str):
    with transaction.atomic():
        locked = _locked_workspace_and_actor_membership(request.user, workspace_id)
        if locked is None:
            return Status(404, {"error": "workspace_not_found"})
        workspace, actor_membership = locked
        target = (
            WorkspaceMembership.objects.select_for_update()
            .select_related("user")
            .filter(id=membership_id, workspace=workspace)
            .first()
        )
        if target is None:
            return Status(404, {"error": "workspace_member_not_found"})
        if target.id == actor_membership.id:
            return Status(409, {"error": "workspace_member_self_operation_forbidden"})
        if target.role == "owner":
            return Status(409, {"error": "workspace_owner_transfer_required"})

        target.delete()
    return {"ok": True}


@router.post(
    "/workspaces/{workspace_id}/owner-transfer",
    auth=session_auth,
    response={200: WorkspaceOwnershipTransferredResponse} | COMMON_ERROR_RESPONSES,
)
def transfer_workspace_ownership(
    request,
    workspace_id: str,
    payload: TransferWorkspaceOwnershipRequest,
):
    with transaction.atomic():
        locked = _locked_workspace_and_actor_membership(
            request.user,
            workspace_id,
            allowed_roles={"owner"},
        )
        if locked is None:
            return Status(404, {"error": "workspace_not_found"})
        workspace, owner_membership = locked
        if not request.user.check_password(payload.current_password):
            return Status(403, {"error": "workspace_owner_reauthentication_failed"})
        target = (
            WorkspaceMembership.objects.select_for_update()
            .select_related("user")
            .filter(id=payload.target_membership_id, workspace=workspace)
            .first()
        )
        if target is None:
            return Status(404, {"error": "workspace_member_not_found"})
        if target.id == owner_membership.id:
            return Status(409, {"error": "workspace_owner_transfer_to_self"})
        if target.role not in {"admin", "member"}:
            return Status(409, {"error": "workspace_owner_transfer_target_invalid"})
        if not target.user.is_active:
            return Status(409, {"error": "workspace_owner_transfer_target_inactive"})

        owner_membership.role = "admin"
        owner_membership.save(update_fields=["role", "updated_at"])
        target.role = "owner"
        target.save(update_fields=["role", "updated_at"])
    return {
        "owner": _serialize_member(target),
        "previousOwner": _serialize_member(owner_membership),
    }


@router.get(
    "/workspaces/{workspace_id}/invitations",
    auth=session_auth,
    response={200: WorkspaceInvitationsEnvelope} | COMMON_ERROR_RESPONSES,
)
def list_workspace_invitations(request, workspace_id: str):
    membership = workspace_membership_for(
        request.user,
        workspace_id,
        allowed_roles=WORKSPACE_ADMIN_ROLES,
    )
    if membership is None:
        return Status(404, {"error": "workspace_not_found"})
    invitations = membership.workspace.invitations.filter(
        status="pending",
        expires_at__gt=timezone.now(),
    ).order_by("created_at", "id")
    return {
        "invitations": [_serialize_invitation(invitation) for invitation in invitations]
    }


@router.post(
    "/workspaces/{workspace_id}/invitations",
    auth=session_auth,
    response={201: WorkspaceInvitationEnvelope} | COMMON_ERROR_RESPONSES,
)
def create_workspace_invitation(
    request,
    workspace_id: str,
    payload: CreateWorkspaceInvitationRequest,
):
    now = timezone.now()
    with transaction.atomic():
        locked = _locked_workspace_and_actor_membership(request.user, workspace_id)
        if locked is None:
            return Status(404, {"error": "workspace_not_found"})
        workspace, _actor_membership = locked
        account = get_user_model().objects.filter(
            username__iexact=payload.email
        ).first()
        if account is not None and WorkspaceMembership.objects.filter(
            workspace=workspace,
            user=account,
        ).exists():
            return Status(409, {"error": "workspace_member_exists"})

        pending = WorkspaceInvitation.objects.filter(
            workspace=workspace,
            email=payload.email,
            status="pending",
        )
        pending.filter(expires_at__lte=now).update(
            status="expired",
            updated_at=now,
        )
        pending.filter(expires_at__gt=now).update(
            status="revoked",
            revoked_by=request.user,
            updated_at=now,
        )

        token = secrets.token_urlsafe(32)
        invitation = WorkspaceInvitation.objects.create(
            workspace=workspace,
            email=payload.email,
            role=payload.role,
            token_digest=_token_digest(token),
            invited_by=request.user,
            expires_at=now + INVITATION_EXPIRY,
        )

    base_url = settings.WEB_ORIGIN.rstrip("/")
    return Status(
        201,
        {
            "invitation": _serialize_invitation(invitation),
            "inviteUrl": f"{base_url}/activate#token={quote(token, safe='')}",
        },
    )


@router.delete(
    "/workspaces/{workspace_id}/invitations/{invitation_id}",
    auth=session_auth,
    response={200: OkResponse} | COMMON_ERROR_RESPONSES,
)
def revoke_workspace_invitation(
    request,
    workspace_id: str,
    invitation_id: str,
):
    now = timezone.now()
    with transaction.atomic():
        locked = _locked_workspace_and_actor_membership(request.user, workspace_id)
        if locked is None:
            return Status(404, {"error": "workspace_not_found"})
        workspace, _actor_membership = locked
        invitation = WorkspaceInvitation.objects.select_for_update().filter(
            id=invitation_id,
            workspace=workspace,
        ).first()
        if invitation is None:
            return Status(404, {"error": "invitation_not_found"})
        if invitation.status != "pending":
            return Status(409, {"error": "invitation_not_pending"})
        if invitation.expires_at <= now:
            invitation.status = "expired"
            invitation.save(update_fields=["status", "updated_at"])
            return Status(410, {"error": "invitation_expired"})
        invitation.status = "revoked"
        invitation.revoked_by = request.user
        invitation.save(update_fields=["status", "revoked_by", "updated_at"])
    return {"ok": True}


@router.post(
    "/invitations/preview",
    auth=None,
    response={
        200: WorkspaceInvitationPreviewResponse,
        404: ErrorResponse,
        409: ErrorResponse,
        410: ErrorResponse,
    },
)
def preview_workspace_invitation(
    request,
    payload: PreviewWorkspaceInvitationRequest,
):
    digest = _token_digest(payload.token)
    if digest is None:
        return Status(404, {"error": "invitation_not_found"})
    invitation = WorkspaceInvitation.objects.select_related("workspace").filter(
        token_digest=digest
    ).first()
    if invitation is None or invitation.status != "pending":
        return Status(404, {"error": "invitation_not_found"})
    if invitation.expires_at <= timezone.now():
        return Status(410, {"error": "invitation_expired"})
    if invitation.workspace.status != "active":
        return Status(409, {"error": "workspace_unavailable"})
    account_exists = get_user_model().objects.filter(
        username__iexact=invitation.email
    ).exists()
    return {
        "workspaceId": invitation.workspace_id,
        "workspaceName": invitation.workspace.name,
        "email": invitation.email,
        "role": invitation.role,
        "accountExists": account_exists,
        "expiresAt": invitation.expires_at.isoformat(),
    }


@router.post(
    "/invitations/accept",
    auth=require_public_csrf,
    response={200: WorkspaceInvitationAcceptedResponse} | COMMON_ERROR_RESPONSES,
)
def accept_workspace_invitation(
    request,
    payload: AcceptWorkspaceInvitationRequest,
):
    digest = _token_digest(payload.token)
    if digest is None:
        return Status(404, {"error": "invitation_not_found"})

    accepted_user = None
    user_created = False
    with transaction.atomic():
        invitation = WorkspaceInvitation.objects.select_for_update().select_related(
            "workspace"
        ).filter(token_digest=digest).first()
        if invitation is None:
            return Status(404, {"error": "invitation_not_found"})
        if invitation.status != "pending":
            return Status(409, {"error": "invitation_not_pending"})
        if invitation.expires_at <= timezone.now():
            invitation.status = "expired"
            invitation.save(update_fields=["status", "updated_at"])
            return Status(410, {"error": "invitation_expired"})
        if invitation.workspace.status != "active":
            return Status(409, {"error": "workspace_unavailable"})

        User = get_user_model()
        accepted_user = User.objects.select_for_update().filter(
            username__iexact=invitation.email
        ).first()
        if accepted_user is not None:
            if not request.user.is_authenticated:
                return Status(401, {"error": "invitation_login_required"})
            if request.user.pk != accepted_user.pk:
                return Status(403, {"error": "invitation_account_mismatch"})
            if not accepted_user.is_active:
                return Status(403, {"error": "invitation_account_inactive"})
            if payload.name is not None or payload.password is not None:
                return Status(400, {"error": "invitation_account_exists"})
        else:
            if request.user.is_authenticated:
                return Status(403, {"error": "invitation_account_mismatch"})
            name = unicodedata.normalize("NFC", (payload.name or "").strip())
            password = payload.password or ""
            if not name or len(name) > 150 or not password:
                return Status(400, {"error": "invitation_account_setup_required"})
            accepted_user = User(
                username=invitation.email,
                email=invitation.email,
                first_name=name,
            )
            try:
                validate_password(password, user=accepted_user)
            except ValidationError:
                return Status(400, {"error": "password_invalid"})
            accepted_user.set_password(password)
            try:
                with transaction.atomic():
                    accepted_user.save()
            except IntegrityError:
                return Status(409, {"error": "invitation_account_created_concurrently"})
            user_created = True

        if WorkspaceMembership.objects.filter(
            workspace=invitation.workspace,
            user=accepted_user,
        ).exists():
            return Status(409, {"error": "workspace_member_exists"})
        membership = WorkspaceMembership.objects.create(
            workspace=invitation.workspace,
            user=accepted_user,
            role=invitation.role,
            invited_by=invitation.invited_by,
        )
        ensure_default_agent(invitation.workspace, accepted_user)
        invitation.status = "accepted"
        invitation.accepted_by = accepted_user
        invitation.accepted_membership_ref = membership.id
        invitation.save(
            update_fields=[
                "status",
                "accepted_by",
                "accepted_membership_ref",
                "updated_at",
            ]
        )

    if user_created:
        login(
            request,
            accepted_user,
            backend=settings.AUTHENTICATION_BACKENDS[0],
        )
    return {
        "workspaceId": invitation.workspace_id,
        "membershipId": membership.id,
        "role": membership.role,
        "userCreated": user_created,
    }
