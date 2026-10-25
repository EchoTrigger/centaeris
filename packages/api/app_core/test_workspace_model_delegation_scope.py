"""The model catalog binds explicit workspace queries to real delegation scope."""

from django.test import Client, TestCase

from app_core.models import ModelConfig, ModelProvider, Workspace, WorkspaceMembership
from app_core.test_app_delegations import AppDelegationFixture


class WorkspaceModelDelegationScopeTests(AppDelegationFixture, TestCase):
    def setUp(self):
        super().setUp()
        self.foreign = Workspace.objects.create(
            name="Another member workspace", createdBy=self.member
        )
        WorkspaceMembership.objects.create(
            workspace=self.foreign, user=self.member, role="owner"
        )
        own_provider = ModelProvider.objects.create(
            workspace=self.workspace,
            displayName="Own",
            api="openai-completions",
            apiBase="https://example.invalid/v1",
        )
        foreign_provider = ModelProvider.objects.create(
            workspace=self.foreign,
            displayName="Foreign",
            api="openai-completions",
            apiBase="https://example.invalid/v1",
        )
        self.own_model = ModelConfig.objects.create(
            provider=own_provider, modelName="own-model"
        )
        self.foreign_model = ModelConfig.objects.create(
            provider=foreign_provider, modelName="foreign-model"
        )
        self.shared_model = ModelConfig.objects.create(displayName="Shared fixture")
        _definition, _app, issued = self.delegation()
        self.token = issued.json()["accessToken"]
        self.bearer = Client()

    def catalog(self, query=""):
        return self.bearer.get(
            "/api/models" + query, HTTP_AUTHORIZATION="Bearer " + self.token
        )

    def test_foreign_query_is_denied_even_when_the_delegating_user_is_an_owner_there(
        self,
    ):
        browser = self.member_client.get(f"/api/models?workspaceId={self.foreign.id}")
        self.assertEqual(browser.status_code, 200, browser.content)
        self.assertIn(
            self.foreign_model.id, [item["id"] for item in browser.json()["models"]]
        )
        denied = self.catalog(f"?workspaceId={self.foreign.id}")
        self.assertIn(denied.status_code, (401, 403, 404), denied.content)
        self.assertNotIn(self.foreign_model.id.encode(), denied.content)

    def test_exact_delegated_workspace_query_returns_only_own_and_shared_models(self):
        response = self.catalog(f"?workspaceId={self.workspace.id}")
        self.assertEqual(response.status_code, 200, response.content)
        self.assertEqual(
            {item["id"] for item in response.json()["models"]},
            {self.own_model.id, self.shared_model.id},
        )

    def test_no_query_preserves_the_existing_shared_only_catalog_contract(self):
        response = self.catalog()
        self.assertEqual(response.status_code, 200, response.content)
        self.assertEqual(
            [item["id"] for item in response.json()["models"]], [self.shared_model.id]
        )
