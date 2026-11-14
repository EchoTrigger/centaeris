import json
from unittest.mock import patch

from django.contrib.auth import get_user_model
from django.db import DatabaseError, IntegrityError, transaction
from django.test import Client, TestCase

from app_core.models import Issue, IssueEvent, IssueNotification, Workspace, WorkspaceMembership


class NotificationTests(TestCase):
    def setUp(self):
        self.a = get_user_model().objects.create_user(username="alice")
        self.b = get_user_model().objects.create_user(username="bob")
        self.c = get_user_model().objects.create_user(username="charlie")
        self.workspace = Workspace.objects.create(name="Work", createdBy=self.a)
        for user in (self.a, self.b, self.c):
            WorkspaceMembership.objects.create(workspace=self.workspace, user=user)
        self.base = f"/api/workspaces/{self.workspace.id}"
        self.client.force_login(self.a)

    def create(self, **fields):
        response = self.client.post(self.base + "/issues", json.dumps({"title": "Original", **fields}), content_type="application/json")
        self.assertEqual(response.status_code, 201, response.content)
        return response.json()["issue"]

    def update_issue(self, issue, **fields):
        return self.client.patch(self.base + "/issues/" + issue["id"], json.dumps({"version": issue["version"], **fields}), content_type="application/json")

    def receipt(self, notification, **fields):
        return self.client.patch(self.base + "/notifications/" + notification["id"], json.dumps(fields), content_type="application/json")

    def inbox(self, query=""):
        response = self.client.get(self.base + "/notifications" + query)
        self.assertEqual(response.status_code, 200, response.content)
        return response.json()

    def test_assignment_status_flow_snapshot_and_receipts(self):
        issue = self.create(assigneeId=str(self.b.pk))
        self.assertEqual(self.inbox()["total"], 0)
        self.client.force_login(self.b)
        notification = self.inbox()["notifications"][0]
        self.assertEqual(notification["kind"], "assigned")
        self.assertEqual(notification["event"]["actor"]["id"], str(self.a.pk))
        response = self.update_issue(issue, status="inProgress", title="Renamed")
        self.assertEqual(response.status_code, 200)
        notification_url = self.base + "/notifications/" + notification["id"]
        self.assertEqual(self.client.get(notification_url).json()["notification"]["issueTitle"], "Original")
        read = self.receipt(notification, read=True).json()["notification"]
        self.assertIsNotNone(read["readAt"])
        self.assertEqual(self.receipt(notification, read=True).json()["notification"]["readAt"], read["readAt"])
        self.assertEqual(self.inbox("?view=unread")["total"], 0)
        archived = self.receipt(notification, archived=True).json()["notification"]
        self.assertEqual(archived["readAt"], read["readAt"])
        self.assertEqual(self.receipt(notification, archived=True).json()["notification"], archived)
        self.assertEqual(self.inbox()["total"], 0)
        self.assertEqual(self.inbox("?view=archived")["total"], 1)
        restored = self.receipt(notification, archived=False, read=False).json()["notification"]
        self.assertIsNone(restored["readAt"])
        self.assertIsNone(restored["archivedAt"])
        self.assertEqual(self.inbox()["unreadCount"], 1)
        self.client.force_login(self.a)
        status = self.inbox()["notifications"][0]
        self.assertEqual(status["kind"], "statusChanged")
        self.assertEqual(status["event"]["changes"]["status"]["after"], "inProgress")

    def test_projection_dedup_exclusions_and_unique_constraint(self):
        issue = self.create(assigneeId=str(self.a.pk))
        self.assertEqual(IssueNotification.objects.count(), 0)
        changed = self.update_issue(issue, assigneeId=str(self.b.pk), status="done").json()["issue"]
        notification = IssueNotification.objects.get()
        self.assertEqual(notification.kind, "assigned")
        with self.assertRaises(IntegrityError), transaction.atomic():
            IssueNotification.objects.create(event=notification.event, recipient=self.b, issueTitle="Duplicate", kind="assigned")
        for fields in ({"status": "done"}, {"title": "New"}, {"archived": True}, {"archived": False}):
            changed = self.update_issue(changed, **fields).json()["issue"]
        self.assertEqual(IssueNotification.objects.count(), 1)
        WorkspaceMembership.objects.filter(workspace=self.workspace, user=self.b).delete()
        self.update_issue(changed, status="todo")
        self.assertEqual(IssueNotification.objects.count(), 1)

    def test_status_notifies_creator_and_assignee_and_assignment_wins(self):
        issue = self.create(assigneeId=str(self.b.pk))
        self.client.force_login(self.c)
        changed = self.update_issue(issue, status="inProgress").json()["issue"]
        event = IssueEvent.objects.get(issue_id=issue["id"], type="updated")
        self.assertEqual(set(event.notifications.values_list("recipient_id", "kind")), {
            (self.a.pk, "statusChanged"), (self.b.pk, "statusChanged")})
        changed = self.update_issue(changed, assigneeId=str(self.a.pk), status="done").json()["issue"]
        event = IssueEvent.objects.get(issue_id=issue["id"], changes__status__after="done")
        self.assertEqual(list(event.notifications.values_list("recipient_id", "kind")), [(self.a.pk, "assigned")])

    def test_authorization_scope_and_current_membership(self):
        self.create(assigneeId=str(self.b.pk))
        notification = IssueNotification.objects.get()
        path = self.base + "/notifications/" + notification.id
        outsider = get_user_model().objects.create_user(username="outsider")
        for user in (self.a, self.c, outsider):
            self.client.force_login(user)
            self.assertEqual(self.client.get(path).status_code, 404)
            self.assertEqual(self.receipt({"id": notification.id}, read=True).status_code, 404)
        self.client.force_login(self.b)
        other = Workspace.objects.create(name="Other", createdBy=self.b)
        WorkspaceMembership.objects.create(workspace=other, user=self.b)
        self.assertEqual(self.client.get(f"/api/workspaces/{other.id}/notifications/{notification.id}").status_code, 404)
        WorkspaceMembership.objects.filter(workspace=self.workspace, user=self.b).delete()
        self.assertEqual(self.client.get(path).status_code, 404)
        self.assertEqual(self.client.get(self.base + "/notifications").status_code, 404)
        self.assertEqual(self.receipt({"id": notification.id}, read=True).status_code, 404)
        self.client.force_login(self.c)
        self.workspace.status = "archived"
        self.workspace.save()
        self.assertEqual(self.client.get(self.base + "/notifications").status_code, 404)

    def test_notification_failure_rolls_back_issue_and_event(self):
        with patch("app_core.issue_notifications.IssueNotification.objects.bulk_create", side_effect=DatabaseError("unavailable")):
            response = self.client.post(self.base + "/issues", json.dumps({"title": "Failed", "assigneeId": str(self.b.pk)}), content_type="application/json")
        self.assertEqual(response.status_code, 500)
        self.assertEqual(Issue.objects.count(), 0)
        self.assertEqual(IssueEvent.objects.count(), 0)
        issue = self.create()
        with patch("app_core.issue_notifications.IssueNotification.objects.bulk_create", side_effect=DatabaseError("unavailable")):
            self.assertEqual(self.update_issue(issue, assigneeId=str(self.b.pk)).status_code, 500)
        persisted = Issue.objects.get(pk=issue["id"])
        self.assertIsNone(persisted.assignee_id)
        self.assertEqual(persisted.version, 1)
        self.assertEqual(IssueEvent.objects.count(), 1)

    def test_pagination_order_and_unread_count_ignore_view_and_page(self):
        self.create(assigneeId=str(self.b.pk))
        self.create(assigneeId=str(self.b.pk))
        receipts = list(IssueNotification.objects.all())
        timestamp = receipts[0].createdAt
        IssueNotification.objects.update(createdAt=timestamp)
        self.client.force_login(self.b)
        expected = sorted((item.id for item in receipts), reverse=True)
        page = self.inbox("?limit=1&offset=1")
        self.assertEqual([item["id"] for item in page["notifications"]], expected[1:])
        self.assertEqual(page["total"], 2)
        self.assertEqual(page["unreadCount"], 2)
        self.receipt({"id": expected[0]}, archived=True)
        archived = self.inbox("?view=archived")
        self.assertEqual(archived["total"], 1)
        self.assertEqual(archived["unreadCount"], 1)

    def test_strict_queries_payload_csrf_and_openapi(self):
        self.create(assigneeId=str(self.b.pk))
        self.client.force_login(self.b)
        notification = self.inbox()["notifications"][0]
        for payload in ({}, {"read": None}, {"read": 1}, {"readAt": None}, {"archived": "true"}, {"unknown": True}):
            self.assertEqual(self.receipt(notification, **payload).status_code, 400, payload)
        for query in ("?view=all", "?view=inbox&view=unread", "?limit=101", "?offset=-1", "?unknown=x"):
            self.assertEqual(self.client.get(self.base + "/notifications" + query).status_code, 400)
        csrf = Client(enforce_csrf_checks=True)
        csrf.force_login(self.b)
        self.assertEqual(csrf.patch(self.base + "/notifications/" + notification["id"], json.dumps({"read": True}), content_type="application/json").status_code, 403)
        from api.ninja_api import api
        schema = api.get_openapi_schema()
        path = schema["paths"]["/api/workspaces/{workspace_id}/notifications/{notification_id}"]
        self.assertIn(200, path["patch"]["responses"])
        self.assertIn(404, path["patch"]["responses"])
        self.assertFalse(schema["components"]["schemas"]["NotificationPatch"]["additionalProperties"])
