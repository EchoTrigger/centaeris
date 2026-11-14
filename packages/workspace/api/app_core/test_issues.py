import json
from unittest.mock import patch

from django.contrib.auth import get_user_model
from django.test import Client, TestCase
from django.db import DatabaseError

from app_core.models import Issue, IssueEvent, Workspace, WorkspaceMembership


class IssueTests(TestCase):
    def setUp(self):
        users = get_user_model()
        self.owner = users.objects.create_user(username="owner", email="owner@example.test")
        self.member = users.objects.create_user(username="member", first_name="Member")
        self.outsider = users.objects.create_user(username="outsider")
        self.workspace = Workspace.objects.create(name="Issues", createdBy=self.owner)
        WorkspaceMembership.objects.create(workspace=self.workspace, user=self.owner, role="owner")
        self.membership = WorkspaceMembership.objects.create(workspace=self.workspace, user=self.member)
        self.url = f"/api/workspaces/{self.workspace.id}/issues"
        self.client.force_login(self.member)

    def post(self, payload):
        return self.client.post(self.url, json.dumps(payload), content_type="application/json")

    def update(self, issue, payload):
        return self.client.patch(f"{self.url}/{issue['id']}", json.dumps(payload), content_type="application/json")

    def create(self, **fields):
        response = self.post({"title": "  Investigate  ", **fields})
        self.assertEqual(response.status_code, 201, response.content)
        return response.json()["issue"]

    def test_persist_revision_history_noop_and_conflict(self):
        issue = self.create(assigneeId=str(self.member.pk))
        self.assertEqual(issue["title"], "Investigate")
        self.assertEqual(Issue.objects.get(pk=issue["id"]).creator_id, self.member.pk)
        response = self.update(issue, {"version": 1, "status": "inProgress", "title": "Investigate"})
        self.assertEqual(response.status_code, 200, response.content)
        current = response.json()["issue"]
        self.assertEqual(current["version"], 2)
        noop = self.update(issue, {"version": 2, "status": "inProgress"})
        self.assertEqual(noop.json()["issue"], current)
        conflict = self.update(issue, {"version": 1, "title": "Overwrite"})
        self.assertEqual(conflict.status_code, 409)
        self.assertEqual(conflict.json(), {"error": "issue_version_conflict"})
        events = self.client.get(f"{self.url}/{issue['id']}/events").json()
        self.assertEqual(events["total"], 2)
        self.assertEqual([event["type"] for event in events["events"]], ["created", "updated"])
        self.assertEqual(events["events"][1]["changes"], {"status": {"before": "todo", "after": "inProgress"}})
        self.assertEqual(events["events"][0]["actor"], {"id": str(self.member.pk), "name": "Member"})

    def test_member_assignment_filters_archive_and_restore(self):
        for invalid_id in ("not-a-user", "0" + str(self.member.pk), " " + str(self.member.pk)):
            self.assertEqual(self.post({"title": "Bad assignment", "assigneeId": invalid_id}).status_code, 400)
        assigned = self.create(assigneeId=str(self.member.pk))
        self.create(assigneeId=str(self.owner.pk))
        mine = self.client.get(self.url + "?mine=true").json()
        self.assertEqual([item["id"] for item in mine["issues"]], [assigned["id"]])
        archived = self.update(assigned, {"version": 1, "archived": True, "description": "Reason"})
        self.assertIsNotNone(archived.json()["issue"]["archivedAt"])
        self.assertEqual(self.client.get(self.url).json()["total"], 1)
        self.assertEqual(self.client.get(self.url + "?archived=true").json()["total"], 1)
        self.assertEqual(self.update(assigned, {"version": 2, "archived": False}).status_code, 200)
        events = self.client.get(f"{self.url}/{assigned['id']}/events").json()["events"]
        self.assertEqual([event["type"] for event in events], ["created", "archived", "restored"])
        self.assertEqual(events[1]["changes"]["description"]["after"], "Reason")
        assignees = self.client.get(self.url + "/assignees")
        self.assertEqual(assignees.status_code, 200)
        self.assertEqual({item["id"] for item in assignees.json()["assignees"]}, {str(self.owner.pk), str(self.member.pk)})

    def test_authorization_and_removed_members_keep_actor_history(self):
        issue = self.create()
        self.assertEqual(self.post({"title": "Bad", "assigneeId": str(self.outsider.pk)}).status_code, 400)
        other = Workspace.objects.create(name="Other", createdBy=self.outsider)
        WorkspaceMembership.objects.create(workspace=other, user=self.outsider, role="owner")
        self.assertEqual(self.update(issue, {"version": 1, "assigneeId": str(self.outsider.pk)}).status_code, 400)
        self.client.force_login(self.outsider)
        for suffix in ("", f"/{issue['id']}", f"/{issue['id']}/events", "/assignees"):
            self.assertEqual(self.client.get(self.url + suffix).status_code, 404)
        self.assertEqual(self.post({"title": "No"}).status_code, 404)
        self.assertEqual(self.update(issue, {"version": 1, "title": "No"}).status_code, 404)
        self.membership.delete()
        self.client.force_login(self.member)
        self.assertEqual(self.client.get(self.url).status_code, 404)
        self.assertEqual(self.update(issue, {"version": 1, "title": "No"}).status_code, 404)
        self.client.force_login(self.owner)
        events = self.client.get(f"{self.url}/{issue['id']}/events").json()["events"]
        self.assertEqual(events[0]["actor"]["id"], str(self.member.pk))
        self.workspace.status = "archived"
        self.workspace.save()
        self.assertEqual(self.update(issue, {"version": 1, "title": "No"}).status_code, 404)

    def test_atomic_history_failure_rolls_back_create_and_patch(self):
        with patch("app_core.http.issues.IssueEvent.objects.create", side_effect=DatabaseError("history unavailable")):
            self.assertEqual(self.post({"title": "Rollback"}).status_code, 500)
        self.assertEqual(Issue.objects.count(), 0)
        issue = self.create()
        with patch("app_core.http.issues.IssueEvent.objects.create", side_effect=DatabaseError("history unavailable")):
            self.assertEqual(self.update(issue, {"version": 1, "title": "Rollback"}).status_code, 500)
        persisted = Issue.objects.get(pk=issue["id"])
        self.assertEqual(persisted.title, "Investigate")
        self.assertEqual(persisted.version, 1)
        self.assertEqual(IssueEvent.objects.count(), 1)

    def test_strict_payload_query_csrf_and_schema_contract(self):
        for payload in ({"title": " "}, {"title": "x", "status": "in_progress"},
                {"title": "x", "assignee_id": None}, {"title": "x", "extra": True},
                {"title": "x", "description": None}, {"title": "x" * 241}):
            self.assertEqual(self.post(payload).status_code, 400, payload)
        issue = self.create()
        for payload in ({"version": True}, {"version": 0}, {"version": "1"},
                {"version": 1, "title": None}, {"version": 1, "archived": None}, {"version": 1, "title": " "}):
            self.assertEqual(self.update(issue, payload).status_code, 400, payload)
        for query in ("?mine=1", "?limit=0", "?offset=-1", "?mine=true&mine=false", "?unknown=x"):
            self.assertEqual(self.client.get(self.url + query).status_code, 400)
        csrf = Client(enforce_csrf_checks=True)
        csrf.force_login(self.member)
        self.assertEqual(csrf.post(self.url, json.dumps({"title": "Blocked"}), content_type="application/json").status_code, 403)
        self.assertEqual(csrf.patch(f"{self.url}/{issue['id']}", json.dumps({"version": 1}), content_type="application/json").status_code, 403)
        from api.ninja_api import api
        schema = api.get_openapi_schema()
        path = schema["paths"]["/api/workspaces/{workspace_id}/issues/{issue_id}"]
        self.assertIn(200, path["patch"]["responses"])
        self.assertIn(409, path["patch"]["responses"])
        self.assertEqual(schema["components"]["schemas"]["IssuePatch"]["additionalProperties"], False)
