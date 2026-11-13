import json
from unittest.mock import patch

from django.contrib.auth import get_user_model
from django.test import Client, TestCase

from .models import Issue, ModelConfig, WorkspaceIssueTemplate, WorkspaceMembership
from .session_record_test_fixtures import SessionRecordFixture
from .test_hosted_operations import PROFILE


DEFINITION = {"visibility": "participants", "instructions": "Check the provided supporting evidence.",
    "fields": [{"key": "start", "label": "Start", "type": "date", "required": True},
               {"key": "end", "label": "End", "type": "date", "required": True},
               {"key": "items", "label": "Items", "type": "items", "required": True,
                "fields": [{"key": "amount", "label": "Amount", "type": "money", "minValue": "0.01", "required": True}]}],
    "dateOrder": [["start", "end"]]}
VALUES = {"start": "2026-11-01", "end": "2026-11-02", "items": [{"amount": "12.5"}]}


class IssueTemplateTests(SessionRecordFixture, TestCase):
    def setUp(self):
        super().setUp()
        self.url = f"/api/workspaces/{self.workspace.pk}/issue-templates"
        self.issues = f"/api/workspaces/{self.workspace.pk}/issues"
        self.template = WorkspaceIssueTemplate.objects.create(workspace=self.workspace,
            key="request", name="Request", definition=DEFINITION)

    def create(self, **changes):
        return self.client.post(self.issues, json.dumps({"title": "A private request", "templateKey": "request",
            "properties": VALUES, "assigneeId": str(self.other.pk), **changes}), content_type="application/json")

    def test_installed_forms_capture_definition_and_normalize_money(self):
        self.assertEqual(self.client.get(self.url).json()["templates"][0]["key"], "request")
        response = self.create()
        self.assertEqual(response.status_code, 201, response.content)
        data = response.json()["issue"]
        self.assertEqual(data["properties"]["items"][0]["amount"], "12.50")
        self.assertEqual(data["visibility"], "participants")
        self.template.definition = {**DEFINITION, "instructions": "New definition"}
        self.template.save()
        row = Issue.objects.get(pk=data["id"])
        self.assertEqual(row.templateSnapshot["definition"]["instructions"], DEFINITION["instructions"])
        changed = {**VALUES, "items": [{"amount": "25"}]}
        result = self.client.patch(f"{self.issues}/{row.pk}", json.dumps({"version": 1, "properties": changed}), content_type="application/json")
        self.assertEqual(result.status_code, 200, result.content)
        self.assertEqual(result.json()["issue"]["properties"]["items"][0]["amount"], "25.00")

    def test_required_fields_unknown_fields_date_order_and_decimal_are_enforced(self):
        for values in ({}, {**VALUES, "unknown": "x"}, {**VALUES, "start": "2026-12-01"},
                       {**VALUES, "items": [{"amount": "1.234"}]}, {**VALUES, "items": [{"amount": "0"}]}):
            with self.subTest(values=values):
                self.assertEqual(self.create(properties=values).status_code, 400)
        self.assertEqual(self.create(templateKey="", properties=VALUES).status_code, 400)
        self.assertFalse(Issue.objects.filter(templateKey="request").exists())

    def test_record_visibility_covers_every_issue_entry_point_and_stale_notifications(self):
        row = self.create().json()["issue"]
        third = get_user_model().objects.create_user(username="outside-request")
        WorkspaceMembership.objects.create(workspace=self.workspace, user=third, role="member")
        self.client.force_login(third)
        self.assertEqual(self.client.get(self.issues).json()["total"], 0)
        for suffix in ("", "/events", "/materials", "/executions", "/deliverables"):
            with self.subTest(suffix=suffix):
                self.assertEqual(self.client.get(f"{self.issues}/{row['id']}{suffix}").status_code, 404)
        self.client.force_login(self.other)
        self.assertEqual(self.client.get(f"{self.issues}/{row['id']}").status_code, 200)
        self.client.force_login(self.user)
        changed = self.client.patch(f"{self.issues}/{row['id']}", json.dumps({"version": 1, "assigneeId": None}), content_type="application/json")
        self.assertEqual(changed.status_code, 200)
        self.client.force_login(self.other)
        self.assertEqual(self.client.get(f"/api/workspaces/{self.workspace.pk}/notifications").json()["total"], 0)

    def test_installation_requires_admin_and_strict_schema_and_csrf(self):
        payload = {"key": "new-form", "name": "New form", "definition": DEFINITION}
        self.client.force_login(self.other)
        self.assertEqual(self.client.post(self.url, json.dumps(payload), content_type="application/json").status_code, 404)
        self.client.force_login(self.user)
        self.assertEqual(self.client.post(self.url, json.dumps(payload), content_type="application/json").status_code, 201)
        self.assertEqual(self.client.post(self.url, json.dumps(payload), content_type="application/json").status_code, 409)
        self.assertEqual(self.client.post(self.url, json.dumps({**payload, "extra": 1}), content_type="application/json").status_code, 400)
        client = Client(enforce_csrf_checks=True)
        client.force_login(self.user)
        self.assertEqual(client.post(self.url, json.dumps(payload), content_type="application/json").status_code, 403)

    def test_execution_uses_captured_form_and_instructions(self):
        row = self.create().json()["issue"]
        model = ModelConfig.objects.create(displayName="Form model")
        with patch("app_core.http.workspaces.request_execution_profile", return_value=PROFILE), patch(
            "app_core.http.workspaces.schedule_agent_run_lifecycle", return_value="inserted"):
            result = self.client.post(f"{self.issues}/{row['id']}/executions", json.dumps({"version": 1,
                "operationId": "form-execution", "modelConfigRef": model.pk}), content_type="application/json")
        self.assertEqual(result.status_code, 202, result.content)
        from .models import AgentRun
        run = AgentRun.objects.get(pk=result.json()["agentRunId"])
        self.assertIn("Amount: 12.50", run.prompt)
        self.assertIn(DEFINITION["instructions"], run.prompt)
