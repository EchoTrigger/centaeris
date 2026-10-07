from collections.abc import Collection

from django.db.models import Q

from app_core.models import (
    SOURCE_ACCESS_LEVELS,
    Source,
    SourceGrant,
    WORKSPACE_ROLES,
    Workspace,
    WorkspaceMembership,
)


WORKSPACE_ADMIN_ROLES = frozenset({"owner", "admin"})
SOURCE_ACCESS_RANK = {"read": 1, "write": 2, "control": 3}


def workspace_membership_for(
    user,
    workspace_id: str,
    *,
    allowed_roles: Collection[str] = WORKSPACE_ROLES,
) -> WorkspaceMembership | None:
    roles = frozenset(allowed_roles)
    if not roles or not roles <= WORKSPACE_ROLES:
        raise ValueError("unsupported workspace role set")
    if not user.is_authenticated:
        return None
    return (
        WorkspaceMembership.objects.select_related("workspace")
        .filter(
            workspace_id=workspace_id,
            workspace__status="active",
            user=user,
            role__in=roles,
        )
        .first()
    )


def locked_workspace_membership_for(
    user,
    workspace_id: str,
    *,
    allowed_roles: Collection[str] = WORKSPACE_ROLES,
) -> WorkspaceMembership | None:
    roles = frozenset(allowed_roles)
    if not roles or not roles <= WORKSPACE_ROLES:
        raise ValueError("unsupported workspace role set")
    workspace = (
        Workspace.objects.select_for_update()
        .filter(id=workspace_id, status="active")
        .first()
    )
    if workspace is None:
        return None
    return WorkspaceMembership.objects.select_for_update().filter(
        workspace=workspace,
        user=user,
        role__in=roles,
    ).first()


def agent_run_membership_is_current(agent_run) -> bool:
    return WorkspaceMembership.objects.filter(
        id=agent_run.membership_ref,
        workspace_id=agent_run.workspace_id,
        workspace__status="active",
        user_id=agent_run.user_id,
        role__in=WORKSPACE_ROLES,
    ).exists()


def source_grants_for_membership(membership: WorkspaceMembership):
    return (
        SourceGrant.objects.filter(workspace_id=membership.workspace_id)
        .filter(
            Q(workspaceGroup__kind="all_members")
            | Q(workspaceGroup__members=membership)
        )
        .distinct()
    )


def source_access_map_for_workspace_member(user, workspace_id: str):
    membership = workspace_membership_for(user, workspace_id)
    if membership is None:
        return None
    if membership.role in WORKSPACE_ADMIN_ROLES:
        return {
            source_id: "control"
            for source_id in Source.objects.filter(
                workspace_id=workspace_id
            ).values_list("id", flat=True)
        }
    access_by_source = {}
    for source_id, access_level in source_grants_for_membership(membership).values_list(
        "source_id", "accessLevel"
    ):
        current = access_by_source.get(source_id)
        if current is None or SOURCE_ACCESS_RANK[access_level] > SOURCE_ACCESS_RANK[current]:
            access_by_source[source_id] = access_level
    return access_by_source


def source_access_level_for_membership(membership: WorkspaceMembership, source):
    if membership.workspace_id != source.workspace_id:
        raise ValueError("Source membership workspace mismatch")
    if membership.role in WORKSPACE_ADMIN_ROLES:
        return "control"
    grants = source_grants_for_membership(membership).filter(source=source)
    highest = None
    for access_level in grants.values_list("accessLevel", flat=True):
        if highest is None or SOURCE_ACCESS_RANK[access_level] > SOURCE_ACCESS_RANK[highest]:
            highest = access_level
    return highest


def source_access_is_at_least(
    membership: WorkspaceMembership,
    source,
    required_access: str,
) -> bool:
    if required_access not in SOURCE_ACCESS_LEVELS:
        raise ValueError(f"unsupported source access level: {required_access}")
    actual = source_access_level_for_membership(membership, source)
    return actual is not None and SOURCE_ACCESS_RANK[actual] >= SOURCE_ACCESS_RANK[required_access]
