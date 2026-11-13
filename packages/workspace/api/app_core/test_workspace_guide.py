"""Guide receipts and completion use current recipient authority and real records."""
import hashlib
import json
import tempfile
from pathlib import Path
from unittest.mock import patch

from django.contrib.auth import get_user_model
from django.db import DatabaseError, IntegrityError, transaction
from django.test import Client, TestCase, override_settings

from .models import (
    AgentRun, Issue, McpBearerCredential, ModelConfig, ModelProvider, ProviderCredential,
    Source, SourceGrant, SourceObject, UserLibraryObject, UserWorkspaceGuideState,
    Workspace, WorkspaceGroup, WorkspaceMembership, WorkspacePluginEnablement,
    WorkspacePluginInstallation,
)
from .testing import create_session


class WorkspaceGuideTests(TestCase):
    def setUp(self):
        self.user = get_user_model().objects.create_user(username="guide-owner")
        self.other = get_user_model().objects.create_user(username="guide-other")
        self.workspace = Workspace.objects.create(name="Guide", createdBy=self.user)
        self.member = WorkspaceMembership.objects.create(workspace=self.workspace, user=self.user, role="owner")
        WorkspaceMembership.objects.create(workspace=self.workspace, user=self.other, role="member")
        self.other_workspace = Workspace.objects.create(name="Other", createdBy=self.user)
        WorkspaceMembership.objects.create(workspace=self.other_workspace, user=self.user, role="owner")
        temporary = tempfile.TemporaryDirectory(prefix="centaeris-guide-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.enterContext(override_settings(PLUGIN_CATALOG_ROOT=self.root))
        self.url = f"/api/workspaces/{self.workspace.pk}/guide"
        self.client.force_login(self.user)

    def guide(self):
        response = self.client.get(self.url)
        self.assertEqual(response.status_code, 200, response.content)
        return response.json()["guide"]

    def steps(self):
        return {step["id"]: step["completed"] for step in self.guide()["steps"]}

    def update(self, payload):
        return self.client.patch(self.url, json.dumps(payload), content_type="application/json")

    def test_state_persistence_idempotence_and_user_workspace_isolation(self):
        self.assertFalse(self.guide()["dismissed"])
        self.assertEqual(UserWorkspaceGuideState.objects.count(), 0)
        self.assertEqual(self.update({"dismissed": True}).status_code, 200)
        state = UserWorkspaceGuideState.objects.get(user=self.user, workspace=self.workspace)
        self.assertIsNotNone(state.dismissedAt)
        self.assertEqual(self.update({"dismissed": True}).status_code, 200)
        self.assertEqual(UserWorkspaceGuideState.objects.get(pk=state.pk).dismissedAt, state.dismissedAt)
        fresh = Client()
        fresh.force_login(self.user)
        self.assertTrue(fresh.get(self.url).json()["guide"]["dismissed"])
        self.client.force_login(self.other)
        self.assertFalse(self.guide()["dismissed"])
        self.assertEqual(self.update({"dismissed": False}).status_code, 200)
        self.client.force_login(self.user)
        other_url = f"/api/workspaces/{self.other_workspace.pk}/guide"
        self.assertFalse(self.client.get(other_url).json()["guide"]["dismissed"])
        self.assertTrue(self.guide()["dismissed"])
        self.assertFalse(self.update({"dismissed": False}).json()["guide"]["dismissed"])
        self.assertIsNone(UserWorkspaceGuideState.objects.get(pk=state.pk).dismissedAt)
        with self.assertRaises(IntegrityError), transaction.atomic():
            UserWorkspaceGuideState.objects.create(user=self.user, workspace=self.workspace)

    def test_roles_and_active_membership_required_even_for_superuser(self):
        self.assertEqual(list(self.steps()), ["models", "materials", "plugins", "connections", "conversation", "issue"])
        self.member.role = "admin"
        self.member.save()
        self.assertEqual(len(self.steps()), 6)
        self.member.role = "member"
        self.member.save()
        self.assertEqual(list(self.steps()), ["materials", "conversation", "issue"])
        # QuerySet update isolates the membership lookup from workspace lifecycle writers.
        Workspace.objects.filter(pk=self.workspace.pk).update(status="suspended")
        self.assertEqual(self.client.get(self.url).status_code, 404)
        self.assertEqual(self.update({"dismissed": True}).status_code, 404)
        Workspace.objects.filter(pk=self.workspace.pk).update(status="active")
        self.member.delete()
        self.user.is_superuser = True
        self.user.save()
        self.assertEqual(self.client.get(self.url).status_code, 404)
        self.assertEqual(self.update({"dismissed": True}).status_code, 404)
        self.assertFalse(UserWorkspaceGuideState.objects.exists())

    def test_strict_schema_csrf_authentication_and_openapi(self):
        for payload in ({}, {"dismissed": 1}, {"dismissed": "true"}, {"dismissed": None},
                        {"dismissed": True, "userId": str(self.other.pk)}, {"dismissed_at": None}):
            self.assertEqual(self.update(payload).status_code, 400, payload)
        self.assertEqual(self.client.get(self.url + "?userId=x").status_code, 400)
        self.assertEqual(self.client.patch(self.url + "?userId=x", json.dumps({"dismissed": True}), content_type="application/json").status_code, 400)
        csrf = Client(enforce_csrf_checks=True)
        csrf.force_login(self.user)
        self.assertEqual(csrf.patch(self.url, json.dumps({"dismissed": True}), content_type="application/json").status_code, 403)
        token = csrf.get("/api/csrf").json()["csrfToken"]
        self.assertEqual(csrf.patch(self.url, json.dumps({"dismissed": True}), content_type="application/json", HTTP_X_CSRFTOKEN=token).status_code, 200)
        self.client.logout()
        self.assertEqual(self.client.get(self.url).status_code, 401)
        self.assertEqual(self.update({"dismissed": True}).status_code, 401)
        from api.ninja_api import api
        schema = api.get_openapi_schema()
        path = schema["paths"]["/api/workspaces/{workspace_id}/guide"]
        for method in ("get", "patch"):
            for status in (200, 400, 401, 403, 404, 503):
                self.assertIn(status, path[method]["responses"])
        self.assertFalse(schema["components"]["schemas"]["GuidePatch"]["additionalProperties"])
        self.assertNotIn("required", schema["components"]["schemas"]["GuidePatch"])
        self.assertIn("manualSteps", schema["components"]["schemas"]["GuidePatch"]["properties"])

    def test_manual_steps_override_real_data_and_survive_restore_and_new_client(self):
        Issue.objects.create(workspace=self.workspace, creator=self.user, title="Existing")
        self.assertTrue(self.steps()["issue"])
        response = self.update({"manualSteps": {"issue": False, "materials": True}})
        self.assertEqual(response.status_code, 200, response.content)
        self.assertFalse(self.steps()["issue"])
        self.assertTrue(self.steps()["materials"])
        Issue.objects.create(workspace=self.workspace, creator=self.user, title="Another")
        self.update({"dismissed": True})
        self.update({"manualSteps": {"conversation": True}})
        self.assertTrue(self.guide()["dismissed"])
        self.update({"dismissed": False})
        fresh = Client()
        fresh.force_login(self.user)
        steps = {step["id"]: step["completed"] for step in fresh.get(self.url).json()["guide"]["steps"]}
        self.assertFalse(steps["issue"])
        self.assertTrue(steps["materials"])
        self.assertTrue(steps["conversation"])
        self.assertEqual(self.update({"manualSteps": {"issue": True}}).status_code, 200)
        self.assertTrue(self.steps()["issue"])
        self.assertEqual(Issue.objects.filter(workspace=self.workspace).count(), 2)
        self.assertFalse(AgentRun.objects.exists())
        self.assertFalse(UserLibraryObject.objects.exists())
        self.client.force_login(self.other)
        self.assertFalse(self.steps()["materials"])
        self.client.force_login(self.user)
        other = self.client.get(f"/api/workspaces/{self.other_workspace.pk}/guide").json()["guide"]
        self.assertFalse(any(step["completed"] for step in other["steps"]))

    def test_manual_step_schema_and_admin_authority(self):
        for payload in ({"manualSteps": {}}, {"manualSteps": None}, {"manualSteps": []},
                        {"manualSteps": {"unknown": True}}, {"manualSteps": {"issue": 1}},
                        {"manualSteps": {"issue": "false"}}, {"manualSteps": {"issue": None}},
                        {"manual_steps": {"issue": True}}):
            self.assertEqual(self.update(payload).status_code, 400, payload)
        self.assertFalse(UserWorkspaceGuideState.objects.exists())
        self.assertEqual(self.update({"manualSteps": {"models": True}}).status_code, 200)
        self.member.role = "member"
        self.member.save()
        self.assertNotIn("models", self.steps())
        for step in ("models", "plugins", "connections"):
            self.assertEqual(self.update({"dismissed": True, "manualSteps": {step: False, "issue": True}}).status_code, 403)
        state = UserWorkspaceGuideState.objects.get(user=self.user, workspace=self.workspace)
        self.assertEqual(state.manualSteps, {"models": True})
        self.assertIsNone(state.dismissedAt)
        self.assertEqual(self.update({"manualSteps": {"issue": True}}).status_code, 200)

    def test_models_require_real_enabled_current_workspace_provider_and_credential(self):
        ModelConfig.objects.create()
        self.assertFalse(self.steps()["models"])
        provider = ModelProvider.objects.create(workspace=self.other_workspace, displayName="Provider", api="openai-responses", apiBase="https://example.invalid/v1")
        model = ModelConfig.objects.create(provider=provider, modelName="real-model")
        ProviderCredential.objects.create(provider=provider, displayName="Saved", encryptedSecret="saved", createdBy=self.user, updatedBy=self.user)
        self.assertFalse(self.steps()["models"])
        provider.workspace = self.workspace
        provider.save()
        self.assertTrue(self.steps()["models"])
        for field in ("enabled", "isCurrent"):
            setattr(model, field, False)
            model.save()
            self.assertFalse(self.steps()["models"])
            setattr(model, field, True)
            model.save()
        provider.enabled = False
        provider.save()
        self.assertFalse(self.steps()["models"])

    def test_materials_require_personal_ownership_or_source_grants(self):
        personal = UserLibraryObject.objects.create(owner=self.other, objectKind="file", displayName="Private", status="ready")
        source = Source.objects.create(workspace=self.workspace, sourceType="fileTree", name="Source", status="ready", createdBy=self.user)
        obj = SourceObject.objects.create(workspace=self.workspace, source=source, objectType="directory", displayPath="folder", displayName="folder", sourceVersion="1", status="ready")
        self.assertFalse(self.steps()["materials"])
        obj.objectType = "file"
        obj.save()
        self.assertTrue(self.steps()["materials"])
        self.client.force_login(self.other)
        personal.delete()
        UserLibraryObject.objects.create(owner=self.user, objectKind="file", displayName="Private", status="ready")
        self.assertFalse(self.steps()["materials"])
        group = WorkspaceGroup.objects.create(workspace=self.workspace, name="Readers", kind="custom", createdBy=self.user)
        other_membership = WorkspaceMembership.objects.get(workspace=self.workspace, user=self.other)
        group.members.add(other_membership)
        grant = SourceGrant.objects.create(workspace=self.workspace, source=source, workspaceGroup=group, accessLevel="read", createdBy=self.user)
        self.assertTrue(self.steps()["materials"])
        source.status = "processing"
        source.save()
        self.assertFalse(self.steps()["materials"])
        source.status = "ready"
        source.save()
        grant.delete()
        self.assertFalse(self.steps()["materials"])
        own = UserLibraryObject.objects.create(owner=self.other, objectKind="image", displayName="Own", status="ready")
        self.assertTrue(self.steps()["materials"])
        own.delete()
        self.assertFalse(self.steps()["materials"])

    def test_conversation_requires_user_interactive_run_and_issue_requires_creator(self):
        session = create_session(workspace=self.workspace, owner=self.user)
        self.assertFalse(self.steps()["conversation"])
        model = ModelConfig.objects.create()
        AgentRun.objects.create(workspace=self.workspace, session=session, user=self.user, modelConfig=model, prompt="Hello")
        Issue.objects.create(workspace=self.workspace, creator=self.user, title="Actual issue")
        self.assertTrue(self.steps()["conversation"])
        self.assertTrue(self.steps()["issue"])
        self.client.force_login(self.other)
        self.assertFalse(self.steps()["conversation"])
        self.assertFalse(self.steps()["issue"])
        other_session = create_session(workspace=self.workspace, owner=self.other, purpose="agentRunApi")
        AgentRun.objects.create(workspace=self.workspace, session=other_session, user=self.other, modelConfig=model, prompt="API")
        self.assertFalse(self.steps()["conversation"])

    def install(self, *, credential=True):
        name = "guide-service"
        directory = self.root / name
        (directory / ".centaeris-plugin").mkdir(parents=True)
        (directory / ".centaeris-plugin" / "plugin.json").write_text(json.dumps({"name": name, "version": "1.0.0", "paths": {}}), encoding="utf-8")
        transport = {"type": "streamableHttp", "url": "https://example.invalid/mcp"}
        if credential:
            transport["bearerCredentialRef"] = "service-token"
        content = json.dumps({"schema": "mcp_servers_v1", "servers": [{"id": "service", "transport": transport}]}).encode()
        (directory / "mcp.json").write_bytes(content)
        digest = hashlib.sha256(b"centaeris.plugin.tree.v1\0")
        for data in (b"mcp.json", content):
            digest.update(len(data).to_bytes(8, "big"))
            digest.update(data)
        package = {"name": name, "version": "1.0.0", "packageDigest": "sha256:" + "a" * 64,
            "skills": [], "cli": [], "hooks": [], "mcpServers": [{"path": "mcp.json", "digest": "sha256:" + digest.hexdigest()}]}
        WorkspacePluginInstallation.objects.create(workspace=self.workspace, plugin_name=name, package=package, updated_by=self.user)
        return name

    def test_plugins_and_connections_use_installed_metadata_and_workspace_credentials(self):
        self.assertFalse(self.steps()["plugins"])
        name = self.install()
        self.assertFalse(self.steps()["plugins"])
        self.assertFalse(self.steps()["connections"])
        WorkspacePluginEnablement.objects.create(workspace=self.workspace, pluginName=name)
        self.assertTrue(self.steps()["plugins"])
        saved = McpBearerCredential.objects.create(workspace=self.other_workspace, plugin_name=name, credential_ref="service-token", display_name="Saved", encrypted_secret="saved", created_by=self.user, updated_by=self.user)
        self.assertFalse(self.steps()["connections"])
        saved.workspace = self.workspace
        saved.save()
        self.assertTrue(self.steps()["connections"])
        (self.root / name / "mcp.json").write_text("{}", encoding="utf-8")
        self.assertEqual(self.update({"dismissed": True}).status_code, 503)
        self.assertFalse(UserWorkspaceGuideState.objects.exists())
        self.client.force_login(self.other)
        self.assertEqual(list(self.steps()), ["materials", "conversation", "issue"])

    def test_no_auth_service_is_configured_without_credentials(self):
        self.install(credential=False)
        self.assertTrue(self.steps()["connections"])
        self.assertFalse(self.steps()["plugins"])

    def test_catalog_and_persistence_errors_do_not_report_success(self):
        WorkspacePluginEnablement.objects.create(workspace=self.workspace, pluginName="missing")
        self.assertEqual(self.update({"dismissed": True}).status_code, 409)
        self.assertFalse(UserWorkspaceGuideState.objects.exists())
        WorkspacePluginEnablement.objects.all().delete()
        client = Client(raise_request_exception=False)
        client.force_login(self.user)
        with patch("app_core.http.workspace_guide.UserWorkspaceGuideState.objects.get_or_create", side_effect=DatabaseError("unavailable")):
            self.assertEqual(client.patch(self.url, json.dumps({"dismissed": True}), content_type="application/json").status_code, 500)
        self.assertFalse(UserWorkspaceGuideState.objects.exists())
