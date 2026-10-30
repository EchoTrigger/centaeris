import json
from unittest.mock import patch
from contextlib import nullcontext
from django.contrib.auth.models import User
from django.test import TestCase
from app_core.models import (
    Workspace,
    WorkspaceMembership,
    ModelProvider,
    McpBearerCredential,
)
from app_core.credentials import encrypt_credential_secret, decrypt_credential_secret


class WorkspaceResourceAdminTests(TestCase):
    def setUp(self):
        self.owner = User.objects.create_user("owner", password="password")
        self.admin = User.objects.create_user("admin", password="password")
        self.member = User.objects.create_user("member", password="password")
        self.other = User.objects.create_user("other", password="password")
        self.platform = User.objects.create_superuser(
            "platform", "platform@example.invalid", "password"
        )
        self.workspace = Workspace.objects.create(name="A", createdBy=self.owner)
        self.foreign = Workspace.objects.create(name="B", createdBy=self.other)
        for user, workspace, role in [
            (self.owner, self.workspace, "owner"),
            (self.admin, self.workspace, "admin"),
            (self.member, self.workspace, "member"),
            (self.other, self.foreign, "admin"),
        ]:
            WorkspaceMembership.objects.create(
                workspace=workspace, user=user, role=role
            )
        self.base = f"/api/workspaces/{self.workspace.id}"
        self.enterContext(
            patch(
                "app_core.plugin_catalog.plugin_lifecycle_lock", side_effect=nullcontext
            )
        )

    def call(self, user, method, path, body):
        self.client.force_login(user)
        return getattr(self.client, method)(
            path, data=json.dumps(body), content_type="application/json"
        )

    def provider(self, user=None):
        response = self.call(
            user or self.admin,
            "post",
            self.base + "/model-providers",
            {
                "displayName": "Local API",
                "api": "openai-completions",
                "apiBase": "https://api.example.invalid/v1",
                "secret": "new-local-secret",
            },
        )
        self.assertEqual(response.status_code, 201, response.content)
        self.assertNotIn(b"new-local-secret", response.content)
        return response.json()["provider"]

    def test_owner_admin_and_platform_can_manage_local_provider_and_rotate_without_reading_secret(
        self,
    ):
        for actor in (self.owner, self.admin, self.platform):
            provider = self.provider(actor)
            self.assertEqual(
                ModelProvider.objects.get(id=provider["id"]).workspace_id,
                self.workspace.id,
            )
            response = self.call(
                actor,
                "post",
                self.base + f"/model-providers/{provider['id']}/credential/rotate",
                {"secret": "replacement-only"},
            )
            self.assertEqual(response.status_code, 200, response.content)
            self.assertNotIn(b"replacement-only", response.content)
            self.assertNotIn(b"new-local-secret", response.content)
            saved = ModelProvider.objects.get(id=provider["id"])
            self.assertEqual(
                decrypt_credential_secret(saved.credential.encryptedSecret),
                "replacement-only",
            )

    def test_member_foreign_admin_and_id_substitution_are_rejected(self):
        provider = self.provider()
        for actor in (self.member, self.other):
            response = self.call(
                actor,
                "post",
                self.base + f"/model-providers/{provider['id']}/credential/rotate",
                {"secret": "stolen"},
            )
            self.assertIn(response.status_code, (403, 404), response.content)
        foreign_base = f"/api/workspaces/{self.foreign.id}"
        response = self.call(
            self.other,
            "post",
            foreign_base + f"/model-providers/{provider['id']}/credential/rotate",
            {"secret": "stolen"},
        )
        self.assertEqual(response.status_code, 404, response.content)

    def test_workspace_mcp_key_override_does_not_change_shared_key_or_other_workspace(
        self,
    ):
        shared = McpBearerCredential.objects.create(
            plugin_name="banana",
            credential_ref="api-key",
            display_name="Shared",
            encrypted_secret=encrypt_credential_secret("shared-secret"),
            created_by=self.platform,
            updated_by=self.platform,
        )
        with patch(
            "app_core.http.workspace_resources.credential_ref_exists", return_value=True
        ):
            response = self.call(
                self.admin,
                "post",
                self.base + "/mcp-bearer-credentials",
                {
                    "pluginName": "banana",
                    "credentialRef": "api-key",
                    "displayName": "Local",
                    "secret": "local-secret",
                },
            )
        self.assertEqual(response.status_code, 201, response.content)
        local = McpBearerCredential.objects.get(pk=response.json()["credential"]["id"])
        self.assertEqual(local.workspace_id, self.workspace.id)
        shared.refresh_from_db()
        self.assertEqual(
            decrypt_credential_secret(shared.encrypted_secret), "shared-secret"
        )
        from app_core.workspace_resources import credential_for_workspace

        self.assertEqual(
            credential_for_workspace(self.workspace.id, "banana", "api-key").id,
            local.id,
        )
        self.assertEqual(
            credential_for_workspace(self.foreign.id, "banana", "api-key").id, shared.id
        )
        self.assertNotIn(b"local-secret", response.content)
        self.assertNotIn(b"shared-secret", response.content)

    def test_shared_provider_cannot_be_changed_through_workspace_route(self):
        shared = ModelProvider.objects.create(
            displayName="Shared",
            api="openai-completions",
            apiBase="https://example.invalid/v1",
        )
        response = self.call(
            self.admin,
            "patch",
            self.base + f"/model-providers/{shared.id}",
            {"displayName": "Changed"},
        )
        self.assertEqual(response.status_code, 404, response.content)
        shared.refresh_from_db()
        self.assertEqual(shared.displayName, "Shared")

    def test_service_layer_rejects_foreign_admin_without_http(self):
        from app_core.workspace_resources import workspace_resource_admin
        from django.db import transaction

        with transaction.atomic(), self.assertRaises(PermissionError):
            workspace_resource_admin(self.other, self.workspace.id)


class WorkspacePluginScopeTests(TestCase):
    def setUp(self):
        import tempfile
        from pathlib import Path
        from django.test import override_settings

        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.root.joinpath("catalog.snapshot.json").write_text(
            json.dumps(
                {
                    "schema": "plugin_activation_snapshot_v1",
                    "packages": [],
                    "digest": "sha256:aa",
                }
            )
        )
        from app_core.plugin_catalog import activation_digest

        self.root.joinpath("catalog.snapshot.json").write_text(
            json.dumps(
                {
                    "schema": "plugin_activation_snapshot_v1",
                    "packages": [],
                    "digest": activation_digest([]),
                }
            )
        )
        self.enterContext(override_settings(PLUGIN_CATALOG_ROOT=self.root))
        self.owner = User.objects.create_user("owner")
        self.admin = User.objects.create_user("admin")
        self.other = User.objects.create_user("other")
        self.workspace = Workspace.objects.create(name="A", createdBy=self.owner)
        self.foreign = Workspace.objects.create(name="B", createdBy=self.other)
        WorkspaceMembership.objects.create(
            workspace=self.workspace, user=self.admin, role="admin"
        )
        WorkspaceMembership.objects.create(
            workspace=self.foreign, user=self.other, role="admin"
        )
        self.enterContext(
            patch(
                "app_core.workspace_plugins.plugin_lifecycle_lock",
                side_effect=nullcontext,
            )
        )
        self.enterContext(
            patch(
                "app_core.plugin_catalog.plugin_lifecycle_lock", side_effect=nullcontext
            )
        )

    def test_workspace_install_pins_package_without_replacing_shared_catalog(self):
        from django.core.files.uploadedfile import SimpleUploadedFile
        from app_core.plugin_install_source import UploadedZip
        from app_core.test_plugin_upload import _plugin_zip
        from app_core.workspace_plugins import (
            install_workspace_plugin,
            workspace_plugin_catalog,
            remove_workspace_plugin,
        )
        from app_core.plugin_catalog import (
            load_plugin_catalog,
            plugin_activation_for_workspace,
        )

        package = {
            "name": "banana",
            "version": "1.0.0",
            "packageDigest": "sha256:" + "a" * 64,
            "skills": [],
            "cli": [],
            "hooks": [],
            "mcpServers": [],
        }
        with patch(
            "app_core.workspace_plugins.request_plugin_inspection", return_value=package
        ):
            install_workspace_plugin(
                self.admin,
                self.workspace.id,
                UploadedZip(SimpleUploadedFile("banana.zip", _plugin_zip())),
            )
        self.assertEqual(load_plugin_catalog()["packages"], [])
        self.assertEqual(
            workspace_plugin_catalog(self.workspace)["packages"], [package]
        )
        self.assertEqual(workspace_plugin_catalog(self.foreign)["packages"], [])
        self.assertEqual(
            plugin_activation_for_workspace(self.workspace)["packages"], [package]
        )
        self.assertTrue(
            (
                self.root
                / ".packages"
                / ("a" * 64)
                / "banana"
                / ".centaeris-plugin/plugin.json"
            ).is_file()
        )
        with self.assertRaises(PermissionError):
            remove_workspace_plugin(self.other, self.workspace.id, "banana")
        remove_workspace_plugin(self.admin, self.workspace.id, "banana")
        self.assertEqual(workspace_plugin_catalog(self.workspace)["packages"], [])
        self.assertEqual(load_plugin_catalog()["packages"], [])

    def test_model_selection_rejects_other_workspace_configuration(self):
        from app_core.models import ModelConfig
        from app_core.agent_model_settings import (
            selected_model,
            AgentModelSettingsError,
        )

        own = ModelProvider.objects.create(
            workspace=self.workspace,
            displayName="A",
            api="openai-completions",
            apiBase="https://example.invalid/v1",
        )
        model = ModelConfig.objects.create(provider=own, modelName="demo")
        self.assertEqual(
            selected_model(model.id, None, workspace_id=self.workspace.id)[0].id,
            model.id,
        )
        with self.assertRaises(AgentModelSettingsError):
            selected_model(model.id, None, workspace_id=self.foreign.id)


from django.test import SimpleTestCase


class WorkspaceSkillUploadTests(SimpleTestCase):
    def test_standalone_skill_zip_uses_existing_safe_extraction_and_becomes_a_plugin(
        self,
    ):
        import io, zipfile, tempfile
        from pathlib import Path
        from django.core.files.uploadedfile import SimpleUploadedFile
        from app_core.plugin_install_source import (
            UploadedWorkspaceZip,
            PluginInstallSourceError,
        )

        data = io.BytesIO()
        with zipfile.ZipFile(data, "w") as archive:
            archive.writestr(
                "banana/SKILL.md",
                "---\nname: banana\ndescription: Test skill.\n---\nBody",
            )
            archive.writestr("banana/references/example.txt", "Example")
        with tempfile.TemporaryDirectory() as temp:
            package = UploadedWorkspaceZip(
                SimpleUploadedFile("banana.zip", data.getvalue())
            ).normalize_into(Path(temp) / "staging")
            self.assertEqual(
                json.loads((package / ".centaeris-plugin/plugin.json").read_text())[
                    "name"
                ],
                "banana",
            )
            self.assertTrue((package / "skills/banana/SKILL.md").is_file())
            self.assertEqual(
                (package / "skills/banana/references/example.txt").read_text(),
                "Example",
            )
        bad = io.BytesIO()
        with zipfile.ZipFile(bad, "w") as archive:
            archive.writestr("../escape", "bad")
        with (
            tempfile.TemporaryDirectory() as temp,
            self.assertRaises(PluginInstallSourceError),
        ):
            UploadedWorkspaceZip(
                SimpleUploadedFile("banana.zip", bad.getvalue())
            ).normalize_into(Path(temp) / "staging")


class PlatformWorkspaceResourceTests(TestCase):
    def test_operator_browses_only_workspace_resource_metadata_without_membership(self):
        owner = User.objects.create_user("operator-metadata-owner")
        admin = User.objects.create_user("operator-metadata-admin")
        operator = User.objects.create_superuser(
            "operator-metadata-platform", "operator@example.invalid", "password"
        )
        first = Workspace.objects.create(name="First", createdBy=owner)
        other = Workspace.objects.create(name="Other", createdBy=owner)
        WorkspaceMembership.objects.create(workspace=first, user=admin, role="admin")
        self.client.force_login(operator)
        response = self.client.get("/api/admin/workspace-resources")
        self.assertEqual(response.status_code, 200, response.content)
        self.assertEqual(
            {x["id"] for x in response.json()["workspaces"]}, {first.id, other.id}
        )
        for item in response.json()["workspaces"]:
            self.assertEqual(set(item), {"id", "name", "status"})
        self.assertFalse(WorkspaceMembership.objects.filter(user=operator).exists())
        self.assertEqual(self.client.get("/api/workspaces").json()["workspaces"], [])
        self.assertEqual(
            self.client.get(f"/api/workspaces/{other.id}/model-providers").status_code,
            200,
        )
        self.assertEqual(
            self.client.get(f"/api/workspaces/{other.id}/sessions").status_code, 404
        )
        for actor in (owner, admin):
            self.client.force_login(actor)
            self.assertEqual(
                self.client.get("/api/admin/workspace-resources").status_code, 404
            )
