"""Explicit, recipient-scoped result sharing from private Issue executions."""
from django.db.models import Max, Q
from django.utils import timezone

from .hosted_operations import HostedOperationError
from .issue_access import issue_visible_to
from .models import (Artifact, Issue, IssueDeliverable, IssueDeliverableResponse, IssueEvent,
                     IssueExecution, IssueNotification, WorkspaceMembership, WORKSPACE_ROLES)
from .workspace_access import locked_workspace_membership_for, workspace_membership_for


def visible_deliverables(user, workspace_id):
    return IssueDeliverable.objects.filter(issue__workspace_id=workspace_id).filter(Q(author=user) | Q(recipient=user))


def get_issue(user, workspace_id, issue_id, *, version=None):
    membership = (locked_workspace_membership_for if version is not None else workspace_membership_for)(user, workspace_id)
    if membership is None:
        raise HostedOperationError(404, "workspace_not_found")
    query = Issue.objects.select_for_update() if version is not None else Issue.objects
    issue = query.filter(pk=issue_id, workspace_id=workspace_id).first()
    if issue is None or not issue_visible_to(user, issue):
        raise HostedOperationError(404, "issue_not_found")
    if version is not None:
        if issue.archivedAt is not None:
            raise HostedOperationError(409, "issue_archived")
        if issue.version != version:
            raise HostedOperationError(409, "issue_version_conflict")
    return issue


def _person(user):
    return {"id": str(user.pk), "name": user.get_full_name() or user.email or user.username}


def serialize_deliverable(item):
    root = f"/api/workspaces/{item.issue.workspace_id}/issues/{item.issue_id}/deliverables/{item.pk}"
    return {"id": str(item.pk), "issueId": item.issue_id, "executionId": str(item.execution_id),
            "agentRunId": item.agent_run_id, "author": _person(item.author), "recipient": _person(item.recipient),
            "revision": item.revision, "replacesId": str(item.replaces_id) if item.replaces_id else None,
            "summary": item.summary, "artifacts": [{"id": value["artifactId"], "displayName": value["displayName"],
                "downloadUrl": f"{root}/artifacts/{value['artifactId']}"} for value in item.artifacts],
            "responses": [{"id": str(response.pk), "actor": _person(response.actor), "action": response.action,
                "text": response.text, "createdAt": response.createdAt.isoformat()}
                for response in item.responses.select_related("actor").order_by("createdAt", "id")],
            "createdAt": item.createdAt.isoformat(), "isCurrent": not item.replacements.exists()}


def _record(issue, actor, item, event_type):
    issue.version += 1
    issue.updatedAt = timezone.now()
    issue.save(update_fields=["version", "updatedAt"])
    event = IssueEvent.objects.create(issue=issue, actor=actor, type=event_type,
        changes={"deliverableId": {"before": None, "after": str(item.pk)}})
    other_id = item.recipient_id if actor.pk == item.author_id else item.author_id
    if (event_type == "deliverableSubmitted" or other_id != actor.pk) and WorkspaceMembership.objects.filter(workspace_id=issue.workspace_id,
            user_id=other_id, user__is_active=True, role__in=WORKSPACE_ROLES).exists():
        IssueNotification.objects.create(event=event, recipient_id=other_id,
            issueTitle=issue.title, kind="deliverable")


def submit_deliverable(user, issue, payload):
    recipient = WorkspaceMembership.objects.select_related("user").filter(
        workspace_id=issue.workspace_id, user_id=payload.recipient_id,
        user__is_active=True, role__in=WORKSPACE_ROLES).first()
    if recipient is None or (issue.visibility == "participants" and not issue_visible_to(recipient.user, issue)):
        raise HostedOperationError(400, "deliverable_recipient_invalid")
    execution = IssueExecution.objects.filter(pk=payload.execution_id, issue=issue, issuer=user,
        session__workspace_id=issue.workspace_id, session__owner=user, session__purpose="interactive",
        session__status="active", session__archivedAt__isnull=True, session__purgeRequestedAt__isnull=True,
        session__purgedAt__isnull=True).first()
    if execution is None:
        raise HostedOperationError(404, "issue_execution_not_found")
    run = execution.session.agent_runs.filter(pk=payload.agent_run_id, workspace_id=issue.workspace_id, user=user).first()
    if run is None:
        raise HostedOperationError(404, "deliverable_run_not_found")
    if not payload.summary.strip() and not payload.artifact_ids:
        raise HostedOperationError(400, "deliverable_content_required")
    if len(set(payload.artifact_ids)) != len(payload.artifact_ids):
        raise HostedOperationError(400, "deliverable_artifacts_invalid")
    artifacts = list(Artifact.objects.select_for_update().filter(pk__in=payload.artifact_ids,
        workspace_id=issue.workspace_id, session_id=execution.session_id, agent_run=run,
        createdBy=user, status="published", publishedAt__isnull=False, deletedAt__isnull=True).order_by("id"))
    if len(artifacts) != len(payload.artifact_ids):
        raise HostedOperationError(403, "deliverable_artifact_unavailable")
    replaces = None
    if payload.replaces_id is not None:
        replaces = IssueDeliverable.objects.filter(pk=payload.replaces_id, issue=issue,
            author=user, recipient=recipient.user).first()
        if replaces is None:
            raise HostedOperationError(404, "deliverable_not_found")
        if replaces.replacements.exists():
            raise HostedOperationError(409, "deliverable_superseded")
    revision = (issue.deliverables.aggregate(value=Max("revision"))["value"] or 0) + 1
    frozen = [{"artifactId": value.pk, "displayName": value.displayName, "contentType": value.contentType,
        "contentGeneration": value.contentGeneration, "sizeBytes": value.sizeBytes, "sha256": value.sha256}
        for value in artifacts]
    item = IssueDeliverable.objects.create(issue=issue, execution=execution, agent_run=run, author=user,
        recipient=recipient.user, revision=revision, replaces=replaces, summary=payload.summary.strip(), artifacts=frozen)
    _record(issue, user, item, "deliverableSubmitted")
    return item


def respond_to_deliverable(user, issue, item, payload):
    if item.replacements.exists():
        raise HostedOperationError(409, "deliverable_superseded")
    if payload.action == "accept":
        if user.pk != item.recipient_id:
            raise HostedOperationError(403, "deliverable_recipient_required")
        if item.responses.filter(action="accept").exists():
            raise HostedOperationError(409, "deliverable_already_accepted")
    elif not payload.text.strip():
        raise HostedOperationError(400, "deliverable_feedback_required")
    IssueDeliverableResponse.objects.create(deliverable=item, actor=user, action=payload.action, text=payload.text.strip())
    _record(issue, user, item, "deliverableAccepted" if payload.action == "accept" else "deliverableFeedback")


def select_deliverable_artifact(user, workspace_id, issue_id, deliverable_id, artifact_id):
    issue = get_issue(user, workspace_id, issue_id)
    item = visible_deliverables(user, workspace_id).filter(pk=deliverable_id, issue=issue).first()
    if item is None:
        raise HostedOperationError(404, "deliverable_not_found")
    frozen = next((value for value in item.artifacts if value["artifactId"] == artifact_id), None)
    if frozen is None:
        raise HostedOperationError(404, "deliverable_artifact_not_found")
    artifact = Artifact.objects.filter(pk=artifact_id, workspace_id=workspace_id,
        session_id=item.execution.session_id, agent_run_id=item.agent_run_id, createdBy_id=item.author_id,
        status="published", publishedAt__isnull=False, deletedAt__isnull=True,
        contentGeneration=frozen["contentGeneration"], sizeBytes=frozen["sizeBytes"], sha256=frozen["sha256"]).first()
    if artifact is None:
        raise HostedOperationError(404, "deliverable_artifact_unavailable")
    return artifact.storageKey, frozen["contentType"], artifact.safeFilename, artifact.sizeBytes
