"""Model catalog selection is scoped by current Workspace membership."""
from django.contrib.auth import get_user_model
from django.test import Client, TestCase
from .models import ModelConfig, ModelProvider, Workspace, WorkspaceMembership

class WorkspaceModelCatalogScopeTests(TestCase):
    def setUp(self):
        self.user = get_user_model().objects.create_user(username="catalog-owner")
        self.workspace = Workspace.objects.create(name="Own", createdBy=self.user)
        WorkspaceMembership.objects.create(workspace=self.workspace, user=self.user, role="owner")
        self.foreign = Workspace.objects.create(name="Foreign", createdBy=self.user)
        self.own_model = self.model(self.workspace, "Own")
        self.foreign_model = self.model(self.foreign, "Foreign")
        self.shared_model = ModelConfig.objects.create(displayName="Shared fixture")
        self.client.force_login(self.user)

    def model(self, workspace, name):
        provider = ModelProvider.objects.create(workspace=workspace, displayName=name,
            api="openai-completions", apiBase="https://example.invalid/v1")
        return ModelConfig.objects.create(provider=provider, modelName=name.lower())

    def test_foreign_workspace_requires_current_membership(self):
        path = f"/api/models?workspaceId={self.foreign.pk}"
        denied = self.client.get(path)
        self.assertEqual(denied.status_code, 404, denied.content)
        WorkspaceMembership.objects.create(workspace=self.foreign, user=self.user, role="member")
        response = self.client.get(path)
        self.assertEqual(response.status_code, 200, response.content)
        self.assertEqual({item["id"] for item in response.json()["models"]}, {self.foreign_model.pk, self.shared_model.pk})

    def test_workspace_catalog_contains_only_local_and_shared_models(self):
        response = self.client.get(f"/api/models?workspaceId={self.workspace.pk}")
        self.assertEqual(response.status_code, 200, response.content)
        self.assertEqual({item["id"] for item in response.json()["models"]}, {self.own_model.pk, self.shared_model.pk})

    def test_no_query_returns_only_shared_models(self):
        response = self.client.get("/api/models")
        self.assertEqual(response.status_code, 200, response.content)
        self.assertEqual({item["id"] for item in response.json()["models"]}, {self.shared_model.pk})
