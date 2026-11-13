import json

from django.contrib.auth import get_user_model
from django.test import Client, TestCase

from .models import ModelConfig, ModelProvider, Workspace, WorkspaceMembership


class ModelDefaultsTests(TestCase):
    def setUp(self):
        self.user = get_user_model().objects.create_user(username="defaults-owner")
        self.workspace = Workspace.objects.create(name="Defaults", createdBy=self.user)
        self.member = WorkspaceMembership.objects.create(workspace=self.workspace, user=self.user, role="owner")
        self.path = f"/api/workspaces/{self.workspace.pk}/model-defaults"
        self.provider = ModelProvider.objects.create(workspace=self.workspace, displayName="Purpose",
            api="openai-responses", apiBase="https://example.invalid/v1")
        self.model = ModelConfig.objects.create(provider=self.provider, modelName="purpose-model",
            thinkingMode="high", thinkingModes=["low", "high"])
        self.client.force_login(self.user)

    def update(self, ref, mode="low", **extra):
        return self.client.patch(self.path, data=json.dumps({"titleModelConfigRef": ref,
            "titleThinkingMode": mode, **extra}), content_type="application/json")

    def test_default_and_strict_patch(self):
        self.assertEqual(self.client.get(self.path).json(), {"defaults": {
            "titleModelConfigRef": None, "titleThinkingMode": "low"}})
        self.assertEqual(self.update(self.model.pk).status_code, 200)
        self.assertEqual(self.update(self.model.pk, invented=True).status_code, 400)
        self.assertEqual(self.update(self.model.pk, "unsupported").status_code, 400)
        self.assertEqual(self.client.patch(self.path, data='{}', content_type="application/json").status_code, 400)

    def test_disabled_foreign_models_and_no_thinking(self):
        ModelConfig.objects.filter(pk=self.model.pk).update(enabled=False)
        self.assertEqual(self.update(self.model.pk).status_code, 400)
        ModelConfig.objects.filter(pk=self.model.pk).update(enabled=True, thinkingModes=[], thinkingMode="")
        self.assertEqual(self.update(self.model.pk).status_code, 400)
        self.assertEqual(self.update(self.model.pk, "").status_code, 200)
        foreign = Workspace.objects.create(name="Foreign", createdBy=self.user)
        ModelProvider.objects.filter(pk=self.provider.pk).update(workspace=foreign)
        self.assertEqual(self.update(self.model.pk, "").status_code, 400)

    def test_family_resolves_current_revision_and_unavailable_stays_explicit(self):
        self.assertEqual(self.update(self.model.pk).status_code, 200)
        ModelConfig.objects.filter(pk=self.model.pk).update(isCurrent=False)
        current = ModelConfig.objects.create(provider=self.provider, modelName=self.model.modelName,
            familyId=self.model.familyId, revision=2, thinkingModes=["low"], thinkingMode="low")
        self.assertEqual(self.client.get(self.path).json()["defaults"]["titleModelConfigRef"], current.pk)
        ModelConfig.objects.filter(pk=current.pk).update(enabled=False)
        self.assertEqual(self.client.get(self.path).json()["defaults"]["titleModelConfigRef"], current.pk)

    def test_member_revocation_and_csrf(self):
        csrf = Client(enforce_csrf_checks=True)
        csrf.force_login(self.user)
        response = csrf.patch(self.path, data=json.dumps({"titleModelConfigRef": None,
            "titleThinkingMode": "low"}), content_type="application/json")
        self.assertEqual(response.status_code, 403)
        for role in ("member",):
            WorkspaceMembership.objects.filter(pk=self.member.pk).update(role=role)
            self.assertEqual(self.client.get(self.path).status_code, 404)
            self.assertEqual(self.update(None).status_code, 404)
        self.member.delete()
        self.assertEqual(self.client.get(self.path).status_code, 404)
