from importlib import import_module
from types import SimpleNamespace

from django.contrib.auth import get_user_model
from django.db.migrations.exceptions import IrreversibleError
from django.db.migrations.state import ProjectState
from django.test import SimpleTestCase, TestCase

from .models import Workspace, WorkspaceMembership


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
