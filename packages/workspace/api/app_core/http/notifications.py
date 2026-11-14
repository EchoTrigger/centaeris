from app_core.issue_access import visible_issues
"""Recipient-only issue inbox and receipt mutations."""
from typing import Literal

from django.db import transaction
from django.utils import timezone
from ninja import Router, Status
from pydantic import model_validator

from app_core.models import IssueNotification
from app_core.workspace_access import locked_workspace_membership_for, workspace_membership_for
from .issues import IssueEventResponse, _actor, _pagination
from .response_schema import COMMON_ERROR_RESPONSES
from .schema import StrictSchema
from .security import session_auth

router = Router(tags=["notifications"], by_alias=True)


class NotificationResponse(StrictSchema):
    id: str
    workspace_id: str
    issue_id: str
    issue_title: str
    kind: Literal["assigned", "statusChanged", "deliverable"]
    event: IssueEventResponse
    read_at: str | None
    archived_at: str | None
    created_at: str


class NotificationEnvelope(StrictSchema):
    notification: NotificationResponse


class NotificationsEnvelope(StrictSchema):
    notifications: list[NotificationResponse]
    total: int
    unread_count: int


class NotificationPatch(StrictSchema):
    read: bool = False
    archived: bool = False

    @model_validator(mode="after")
    def require_receipt_field(self):
        if not self.model_fields_set:
            raise ValueError("notification_receipt_required")
        return self


def _notifications(user, workspace_id):
    return IssueNotification.objects.filter(recipient=user, event__issue__in=visible_issues(user, workspace_id))


def _serialize(notification):
    event = notification.event
    return {"id": notification.id, "workspaceId": event.issue.workspace_id,
        "issueId": event.issue_id, "issueTitle": notification.issueTitle, "kind": notification.kind,
        "event": {"id": event.id, "issueId": event.issue_id, "actor": _actor(event.actor),
            "type": event.type, "changes": event.changes, "createdAt": event.createdAt.isoformat()},
        "readAt": notification.readAt.isoformat() if notification.readAt else None,
        "archivedAt": notification.archivedAt.isoformat() if notification.archivedAt else None,
        "createdAt": notification.createdAt.isoformat()}


@router.get("/workspaces/{workspace_id}/notifications", auth=session_auth,
    response={200: NotificationsEnvelope} | COMMON_ERROR_RESPONSES)
def list_notifications(request, workspace_id: str):
    if workspace_membership_for(request.user, workspace_id) is None:
        return Status(404, {"error": "workspace_not_found"})
    page = _pagination(request, ("view",))
    view = request.GET.get("view", "inbox")
    if page is None or view not in {"inbox", "unread", "archived"}:
        return Status(400, {"error": "notification_query_invalid"})
    receipts = _notifications(request.user, workspace_id)
    unread_count = receipts.filter(readAt__isnull=True, archivedAt__isnull=True).count()
    receipts = receipts.filter(archivedAt__isnull=view != "archived")
    if view == "unread":
        receipts = receipts.filter(readAt__isnull=True)
    limit, offset = page
    return {"notifications": [_serialize(item) for item in receipts.select_related(
        "event__issue", "event__actor").order_by("-createdAt", "-id")[offset:offset + limit]],
        "total": receipts.count(), "unreadCount": unread_count}


@router.get("/workspaces/{workspace_id}/notifications/{notification_id}", auth=session_auth,
    response={200: NotificationEnvelope} | COMMON_ERROR_RESPONSES)
def get_notification(request, workspace_id: str, notification_id: str):
    if workspace_membership_for(request.user, workspace_id) is None:
        return Status(404, {"error": "workspace_not_found"})
    notification = _notifications(request.user, workspace_id).select_related("event__issue", "event__actor").filter(id=notification_id).first()
    return {"notification": _serialize(notification)} if notification else Status(404, {"error": "notification_not_found"})


@router.patch("/workspaces/{workspace_id}/notifications/{notification_id}", auth=session_auth,
    response={200: NotificationEnvelope} | COMMON_ERROR_RESPONSES)
def update_notification(request, workspace_id: str, notification_id: str, payload: NotificationPatch):
    with transaction.atomic():
        if locked_workspace_membership_for(request.user, workspace_id) is None:
            return Status(404, {"error": "workspace_not_found"})
        notification = _notifications(request.user, workspace_id).select_for_update().filter(id=notification_id).first()
        if notification is None:
            return Status(404, {"error": "notification_not_found"})
        changed = []
        for field, timestamp in (("read", "readAt"), ("archived", "archivedAt")):
            if field in payload.model_fields_set and (getattr(notification, timestamp) is not None) != getattr(payload, field):
                setattr(notification, timestamp, timezone.now() if getattr(payload, field) else None)
                changed.append(timestamp)
        if changed:
            notification.save(update_fields=changed)
        return {"notification": _serialize(notification)}
