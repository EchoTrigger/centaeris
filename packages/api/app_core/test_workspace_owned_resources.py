from importlib import import_module
from types import SimpleNamespace
from unittest.mock import patch

from django.apps import apps
from django.contrib.auth import get_user_model
from django.db import connection
from django.db.migrations.exceptions import IrreversibleError
from django.db.migrations.state import ProjectState
from django.test import SimpleTestCase, TestCase

from .models import (
    McpBearerCredential,
    ModelConfig,
    ModelProvider,
    ProviderCredential,
    Workspace,
    WorkspaceMembership,
    WorkspacePluginEnablement,
    WorkspacePluginInstallation,
)
from .workspace_resources import models_for_workspace


class WorkspaceResourceRetirementMigrationTests(SimpleTestCase):
    def test_retirement_refuses_rollback_before_database_access(self):
        migration = import_module(
            "app_core.migrations.0046_workspace_owned_resources"
        ).Migration("0046_workspace_owned_resources", "app_core")
        schema_editor = SimpleNamespace(
            atomic_migration=True,
            connection=SimpleNamespace(alias="default"),
        )
        with self.assertRaises(IrreversibleError):
            migration.unapply(ProjectState(), schema_editor)


class WorkspaceOwnedResourceTests(TestCase):
    def setUp(self):
        self.user = get_user_model().objects.create_superuser("operator")
        self.workspace = Workspace.objects.create(name="Workspace", createdBy=self.user)
        WorkspaceMembership.objects.create(workspace=self.workspace, user=self.user, role="owner")
        self.client.force_login(self.user)

    def test_platform_management_routes_are_unavailable(self):
        for path in (
            "workspace-resources", "model-providers", "model-provider-templates",
            "model-catalog-reconciliation", "models", "plugins", "mcp-bearer-credentials",
        ):
            with self.subTest(path=path):
                self.assertEqual(self.client.get(f"/api/admin/{path}").status_code, 404)

    def test_retirement_removes_platform_secrets_and_keeps_workspace_bindings(self):
        def provider(workspace):
            value = ModelProvider.objects.create(
                workspace=workspace, displayName="Provider",
                api="openai-completions", apiBase="https://api.example.invalid",
            )
            ProviderCredential.objects.create(
                provider=value, displayName="Key", encryptedSecret="test ciphertext",
                createdBy=self.user, updatedBy=self.user,
            )
            return value

        shared = provider(None)
        local = provider(self.workspace)
        shared_model = ModelConfig.objects.create(provider=shared, modelName="shared")
        local_model = ModelConfig.objects.create(provider=local, modelName="local")
        for workspace in (None, self.workspace):
            McpBearerCredential.objects.create(
                workspace=workspace, plugin_name="sample", credential_ref="api-key",
                display_name="Key", encrypted_secret="test ciphertext",
                created_by=self.user, updated_by=self.user,
            )
        WorkspacePluginEnablement.objects.create(workspace=self.workspace, pluginName="sample")
        package = {"name": "sample", "packageDigest": "sha256:" + "a" * 64}
        retire = import_module(
            "app_core.migrations.0046_workspace_owned_resources"
        ).retire_platform_configuration
        with patch("app_core.plugin_catalog.load_plugin_catalog_at", return_value={"packages": [package]}):
            retire(apps, SimpleNamespace(connection=connection))

        shared.refresh_from_db()
        shared_model.refresh_from_db()
        self.assertIsNotNone(shared.archivedAt)
        self.assertFalse(shared_model.isCurrent)
        self.assertFalse(ProviderCredential.objects.filter(provider=shared).exists())
        self.assertTrue(ProviderCredential.objects.filter(provider=local).exists())
        self.assertEqual(list(models_for_workspace(self.workspace.pk)), [local_model])
        self.assertEqual(
            list(McpBearerCredential.objects.values_list("workspace_id", flat=True)),
            [self.workspace.pk],
        )
        self.assertEqual(
            WorkspacePluginInstallation.objects.get(workspace=self.workspace, plugin_name="sample").package,
            package,
        )
