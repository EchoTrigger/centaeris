"""Record visibility composes with the caller's current Workspace membership."""
from django.db.models import Q

from .models import Issue


def visible_issues(user, workspace_id, queryset=None):
    query = Issue.objects.all() if queryset is None else queryset
    return query.filter(workspace_id=workspace_id).filter(
        Q(visibility="workspace") | Q(creator=user) | Q(assignee=user))


def issue_visible_to(user, issue):
    return issue.visibility == "workspace" or user.pk in {issue.creator_id, issue.assignee_id}
