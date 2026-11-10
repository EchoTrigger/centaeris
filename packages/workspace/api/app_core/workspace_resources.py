"""Scope model and credential access to a workspace."""

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
    # The provider-less diagnostic model is not workspace configuration.
    scope = Q(provider__isnull=True)
    if workspace_id is not None:
        scope |= Q(provider__workspace_id=workspace_id)
    return ModelConfig.objects.filter(scope)


def credential_for_workspace(workspace_id, plugin_name, credential_ref, *, lock=False):
    query = McpBearerCredential.objects
    if lock:
        query = query.select_for_update()
    return query.get(
        workspace_id=workspace_id,
        plugin_name=plugin_name,
        credential_ref=credential_ref,
    )


def resource_scope(workspace_id):
    return {
        "scope": "workspace" if workspace_id is not None else "platform",
        "writable": workspace_id is not None,
    }
