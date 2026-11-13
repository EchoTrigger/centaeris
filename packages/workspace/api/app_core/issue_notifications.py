"""Project each new issue event into current members' private inboxes."""
from .models import IssueNotification, WorkspaceMembership, WORKSPACE_ROLES


def project_issue_notifications(event):
    issue = event.issue
    recipients = {}
    if event.type != "created" and "status" in event.changes:
        recipients[str(issue.creator_id)] = "statusChanged"
        if issue.assignee_id is not None:
            recipients[str(issue.assignee_id)] = "statusChanged"
    if "assigneeId" in event.changes and issue.assignee_id is not None:
        recipients[str(issue.assignee_id)] = "assigned"
    recipients.pop(str(event.actor_id), None)
    current = WorkspaceMembership.objects.filter(
        workspace_id=issue.workspace_id, workspace__status="active",
        role__in=WORKSPACE_ROLES, user_id__in=recipients,
    ).values_list("user_id", flat=True)
    IssueNotification.objects.bulk_create([
        IssueNotification(event=event, recipient_id=user_id, issueTitle=issue.title, kind=recipients[str(user_id)])
        for user_id in current
    ])
