"""Owner policy is durable; each existing Run retains its original model snapshot."""
import json
from django.test import TestCase

from . import test_agent_messages
from .agent_model_settings import AgentModelSettingsError, configured_agent_model
from .models import AgentRun, ModelConfig


class AgentModelSettingsTests(TestCase):
    setUp = test_agent_messages.AgentMessageTests.setUp
    bind = test_agent_messages.AgentMessageTests.bind

    @property
    def settings_url(self):
        return f"/api/agents/{self.agent.pk}/model-settings"

    def update(self, model_id, mode=None, **changes):
        return self.client.patch(self.settings_url, content_type="application/json",
            data=json.dumps({"schema": "agent.model_settings.update.v1",
                "modelConfigRef": model_id, "thinkingMode": mode, **changes}))

    def model(self, name, mode="high"):
        return ModelConfig.objects.create(displayName=name,
            thinkingMode=mode, thinkingModes=["low", "high"])

    def test_unconfigured_agent_has_no_catalog_or_recent_session_fallback(self):
        self.model("First catalog model")
        session = self.bind()
        AgentRun.objects.create(workspace=self.workspace, user=self.user, session=session,
            modelConfig=self.model("Recent Run model"), prompt="Old", status="completed")
        response = self.client.get(self.settings_url)
        self.assertEqual(response.status_code, 200, response.content)
        self.assertEqual(response.json(), {"schema": "agent.model_settings.v1", "agentId": self.agent.pk,
            "modelConfigRef": None, "thinkingMode": None, "status": "unconfigured"})
        self.agent.refresh_from_db()
        with self.assertRaisesMessage(AgentModelSettingsError, "agent_model_not_configured"):
            configured_agent_model(self.agent)

    def test_owner_selection_persists_exact_revision_and_effective_effort(self):
        model = self.model("Selected model")
        response = self.update(model.pk)
        self.assertEqual(response.status_code, 200, response.content)
        self.assertEqual(response.json()["thinkingMode"], "high")
        self.agent.refresh_from_db()
        self.assertEqual((self.agent.model_config_id, self.agent.thinking_mode), (model.pk, "high"))
        selected, mode = configured_agent_model(self.agent)
        self.assertEqual((selected.pk, mode), (model.pk, "high"))
        self.assertEqual(self.client.get(self.settings_url).json(), response.json())

    def test_settings_change_and_clear_leave_active_run_authorization_snapshot_intact(self):
        from .agent_run_authorization_factory import create_agent_run_authorization
        session = self.bind()
        first, second = self.model("First model"), self.model("Second model", "low")
        self.update(first.pk)
        self.agent.refresh_from_db()
        model, mode = configured_agent_model(self.agent)
        run = AgentRun.objects.create(workspace=self.workspace, user=self.user, session=session,
            modelConfig=model, thinkingMode=mode, prompt="Active", status="running")
        authorization = create_agent_run_authorization(run, image_digest="sha256:" + "a" * 64)
        before = (authorization.payload, authorization.digest, authorization.signature)
        self.assertEqual(self.update(second.pk, "low").status_code, 200)
        self.agent.refresh_from_db()
        next_model, next_mode = configured_agent_model(self.agent)
        self.assertEqual((next_model.pk, next_mode), (second.pk, "low"))
        self.assertEqual(self.update(None).json()["status"], "unconfigured")
        run.refresh_from_db()
        authorization.refresh_from_db()
        self.assertEqual((run.modelConfig_id, run.thinkingMode, run.status), (first.pk, "high", "running"))
        self.assertEqual((authorization.payload, authorization.digest, authorization.signature), before)

    def test_unavailable_selection_is_retained_without_rebinding_to_current_catalog(self):
        model = self.model("Selected")
        self.update(model.pk)
        model.isCurrent = False
        model.enabled = False
        model.save(update_fields=["isCurrent", "enabled"])
        self.model("Replacement")
        response = self.client.get(self.settings_url)
        self.assertEqual(response.json()["status"], "unavailable")
        self.assertEqual(response.json()["modelConfigRef"], model.pk)
        self.agent.refresh_from_db()
        with self.assertRaisesMessage(AgentModelSettingsError, "agent_model_not_available"):
            configured_agent_model(self.agent)

    def test_invalid_mode_aliases_and_incomplete_clear_do_not_change_policy(self):
        model = self.model("Selected")
        self.assertEqual(self.update(model.pk, "unsupported").status_code, 400)
        self.assertEqual(self.update(None, "high").status_code, 400)
        self.assertEqual(self.update(model.pk, model_config_ref=model.pk).status_code, 400)
        self.assertEqual(self.update("bad\0identity").status_code, 400)
        self.assertEqual(self.client.patch(self.settings_url, data=json.dumps({
            "schema": "agent.model_settings.update.v1", "modelConfigRef": model.pk}),
            content_type="application/json").status_code, 400)
        self.agent.refresh_from_db()
        self.assertIsNone(self.agent.model_config_id)

    def test_only_current_private_owner_can_read_or_change_model_policy(self):
        model = self.model("Selected")
        self.client.force_login(self.other)
        self.assertEqual(self.client.get(self.settings_url).status_code, 404)
        self.assertEqual(self.update(model.pk).status_code, 404)
        self.client.force_login(self.user)
        self.membership.delete()
        self.assertEqual(self.client.get(self.settings_url).status_code, 404)
        self.assertEqual(self.update(model.pk).status_code, 404)
