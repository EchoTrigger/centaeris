"""Resource services authorize actors even outside an HTTP router."""

from types import SimpleNamespace
from unittest.mock import patch
from django.contrib.auth.models import User
from django.test import TestCase
from ninja import Status
from app_core.http import model_management as models
from app_core.models import ModelProvider, Workspace, WorkspaceMembership


class WorkspaceResourceServiceAuthorityTests(TestCase):
    def setUp(self):
        self.admin = User.objects.create_user("service-admin")
        self.member = User.objects.create_user("service-member")
        self.platform = User.objects.create_superuser(
            "service-platform", "platform@example.test", "unused-password"
        )
        self.workspace = Workspace.objects.create(name="Own", createdBy=self.admin)
        self.foreign = Workspace.objects.create(name="Foreign", createdBy=self.platform)
        WorkspaceMembership.objects.create(
            workspace=self.workspace, user=self.admin, role="admin"
        )
        WorkspaceMembership.objects.create(
            workspace=self.workspace, user=self.member, role="member"
        )

    @staticmethod
    def payload():
        return models.CreateModelProviderRequest(
            displayName="Direct service provider",
            api="openai-completions",
            apiBase="https://api.example.invalid/v1",
            secret="synthetic-new-write-only-key",
        )

    def assert_denied(self, request):
        before = ModelProvider.objects.count()
        try:
            result = models.create_model_provider(request, self.payload())
        except PermissionError:
            pass
        else:
            self.assertIsInstance(result, Status)
            self.assertIn(result.status_code, (403, 404), result.value)
        self.assertEqual(ModelProvider.objects.count(), before)

    def test_member_without_scope_cannot_create_platform_provider_directly(self):
        self.assert_denied(SimpleNamespace(user=self.member))

    def test_member_cannot_forge_own_workspace_management_scope(self):
        self.assert_denied(
            SimpleNamespace(user=self.member, resource_workspace=self.workspace)
        )

    def test_admin_cannot_forge_foreign_workspace_management_scope(self):
        self.assert_denied(
            SimpleNamespace(user=self.admin, resource_workspace=self.foreign)
        )

    def test_current_admin_can_create_in_own_workspace_directly(self):
        result = models.create_model_provider(
            SimpleNamespace(user=self.admin, resource_workspace=self.workspace),
            self.payload(),
        )
        self.assertEqual(result.status_code, 201)
        self.assertEqual(
            ModelProvider.objects.get(id=result.value["provider"]["id"]).workspace_id,
            self.workspace.id,
        )
        self.assertNotIn("synthetic-new-write-only-key", str(result.value))

    def test_platform_operator_can_create_platform_provider_directly(self):
        result = models.create_model_provider(
            SimpleNamespace(user=self.platform), self.payload()
        )
        self.assertEqual(result.status_code, 201)
        self.assertIsNone(
            ModelProvider.objects.get(id=result.value["provider"]["id"]).workspace_id
        )

    def test_forged_scope_cannot_rotate_update_or_delete_another_tenant_provider(self):
        request = SimpleNamespace(user=self.platform, resource_workspace=self.foreign)
        created = models.create_model_provider(request, self.payload())
        provider = ModelProvider.objects.select_related("credential").get(
            id=created.value["provider"]["id"]
        )
        original_key = provider.credential.encryptedSecret
        original_name = provider.displayName
        forged = SimpleNamespace(user=self.admin, resource_workspace=self.foreign)
        for action, kwargs in [
            (
                models.rotate_model_provider_credential,
                {
                    "payload": models.RotateProviderCredentialRequest(
                        secret="synthetic-unauthorized-replacement"
                    )
                },
            ),
            (
                models.update_model_provider,
                {
                    "payload": models.UpdateModelProviderRequest(
                        displayName="unauthorized-name"
                    )
                },
            ),
            (models.delete_model_provider, {}),
        ]:
            with self.subTest(action=action.__name__):
                try:
                    result = action(forged, provider_id=provider.id, **kwargs)
                except PermissionError:
                    pass
                else:
                    self.assertIsInstance(result, Status)
                    self.assertIn(result.status_code, (403, 404), result.value)
                provider.refresh_from_db()
                provider.credential.refresh_from_db()
                self.assertEqual(provider.credential.encryptedSecret, original_key)
                self.assertEqual(provider.displayName, original_name)
                self.assertIsNone(provider.archivedAt)

    def test_demoted_admin_cannot_reuse_a_previously_authorized_workspace_object(self):
        request = SimpleNamespace(user=self.admin, resource_workspace=self.workspace)
        WorkspaceMembership.objects.filter(
            user=self.admin, workspace=self.workspace
        ).update(role="member")
        self.assert_denied(request)

    def test_workspace_scope_never_authorizes_global_catalog_reconciliation(self):
        request = SimpleNamespace(user=self.admin, resource_workspace=self.workspace)
        with patch.object(models, "_catalog_templates", return_value=({}, {})):
            result = models.preview_model_catalog_reconciliation(request)
            self.assertIsInstance(result, Status)
            self.assertEqual(result.status_code, 403)
            result = models.apply_model_catalog_reconciliation(
                request,
                models.ApplyModelCatalogReconciliationRequest(expectedDigest="stale"),
            )
            self.assertIsInstance(result, Status)
            self.assertEqual(result.status_code, 403)
