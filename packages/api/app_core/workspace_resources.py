"""Workspace resource scope. Platform records remain shared and immutable to tenant admins."""

from django.db import connection
from django.db.models import Q
from .models import Workspace, McpBearerCredential, ModelConfig
from .workspace_access import WORKSPACE_ADMIN_ROLES, locked_workspace_membership_for


def workspace_resource_admin(user, workspace_id):
    """Pin the tenant and actor for the complete mutation transaction.

    Platform operators are admitted explicitly here, not through a generic
    membership bypass and never by manufacturing a workspace membership.
    """
    if not connection.in_atomic_block:
        raise RuntimeError("workspace_resource_transaction_required")
    if not user.is_authenticated:
        raise PermissionError("workspace_resource_forbidden")
    if user.is_superuser:
        workspace = (
            Workspace.objects.select_for_update()
            .filter(pk=workspace_id, status="active")
            .first()
        )
    else:
        member = locked_workspace_membership_for(
            user, workspace_id, allowed_roles=WORKSPACE_ADMIN_ROLES
        )
        workspace = member.workspace if member else None
    if workspace is None:
        raise PermissionError("workspace_resource_forbidden")
    return workspace


def models_for_workspace(workspace_id):
    # The nullable provider is the existing fake model, also platform shared.
    return ModelConfig.objects.filter(
        Q(provider__workspace_id=workspace_id) | Q(provider__workspace__isnull=True)
    )


def credential_for_workspace(workspace_id, plugin_name, credential_ref, *, lock=False):
    query = McpBearerCredential.objects
    if lock:
        query = query.select_for_update()
    own = query.filter(
        workspace_id=workspace_id,
        plugin_name=plugin_name,
        credential_ref=credential_ref,
    ).first()
    if own is not None:
        return own
    return query.get(
        workspace__isnull=True, plugin_name=plugin_name, credential_ref=credential_ref
    )


def resource_scope(workspace_id):
    return {
        "scope": "workspace" if workspace_id is not None else "platform",
        "writable": workspace_id is not None,
    }
