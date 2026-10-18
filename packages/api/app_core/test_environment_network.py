"""Workspace defaults become immutable, signed execution environment policy."""
import json
from copy import deepcopy
from pathlib import Path
from contextlib import nullcontext
from unittest.mock import patch

from django.test import Client, SimpleTestCase, TestCase

from .models import AgentRun, Session, WorkspaceMembership
from .test_hosted_operations import OperationFixture
from .runtime_contract import authorization_digest, authorization_signature, verify_agent_run_authorization_signature


class NetworkAuthorizationTests(SimpleTestCase):
    def test_cross_language_fixtures_sign_network_and_reject_missing_or_mismatched_policy(self):
        root = Path(__file__).resolve().parents[3] / "tests/workspace/fixtures/agent_run_authorization/v3"
        for name in ("ordinary", "benchmark"):
            payload = json.loads((root / f"{name}.json").read_text(encoding="utf-8"))
            expected = json.loads((root / f"{name}-expected.json").read_text(encoding="utf-8"))
            self.assertEqual(authorization_digest(payload), expected["digest"])
            self.assertEqual(authorization_signature(payload, "test-key"), expected["signature"])
            missing = deepcopy(payload)
            del missing["network"]
            with self.assertRaises(ValueError):
                authorization_digest(missing)
            for value in (None, "host", "disabled"):
                changed = payload | {"network": value}
                with self.assertRaises(ValueError):
                    verify_agent_run_authorization_signature(changed, "test-key", expected["signature"])


class EnvironmentNetworkTests(OperationFixture, TestCase):
    def setUp(self):
        super().setUp()
        for target in ("app_core.http.workspaces.plugin_lifecycle_lock",
                       "app_core.plugin_catalog.plugin_lifecycle_lock",
                       "app_core.agent_run_authorization_factory.plugin_lifecycle_lock"):
            context = patch(target, side_effect=lambda *a, **k: nullcontext())
            context.start()
            self.addCleanup(context.stop)
        context = patch("app_core.http.workspaces.queued_admission_error", return_value=None)
        context.start()
        self.addCleanup(context.stop)
        self.url = f"/api/workspaces/{self.workspace.pk}/environment-settings"
        WorkspaceMembership.objects.filter(workspace=self.workspace, user=self.user).update(role="owner")

    def configure(self, network):
        return self.client.patch(self.url, data=json.dumps({"network": network}), content_type="application/json")

    def test_default_and_admin_update_are_strict_and_persisted(self):
        self.assertEqual(self.client.get(self.url).json(), {"network": "disabled"})
        response = self.configure("publicInternet")
        self.assertEqual(response.status_code, 200, response.content)
        self.assertEqual(self.client.get(self.url).json(), {"network": "publicInternet"})
        self.assertEqual(self.configure("host").status_code, 400)
        self.assertEqual(self.client.patch(self.url, data=json.dumps({"network": "disabled", "unknown": True}),
                                          content_type="application/json").status_code, 400)
        self.assertEqual(self.client.get(self.url).json(), {"network": "publicInternet"})

    def test_member_cannot_read_or_change_workspace_policy(self):
        WorkspaceMembership.objects.filter(workspace=self.workspace, user=self.user).update(role="member")
        self.assertEqual(self.client.get(self.url).status_code, 404)
        self.assertEqual(self.configure("publicInternet").status_code, 404)
        self.workspace.refresh_from_db()
        self.assertEqual(self.workspace.execution_network, "disabled")

    def test_settings_require_authentication_and_csrf_for_mutation(self):
        client = Client(enforce_csrf_checks=True)
        self.assertEqual(client.get(self.url).status_code, 401)
        client.force_login(self.user)
        self.assertEqual(client.get(self.url).status_code, 200)
        response = client.patch(self.url, data=json.dumps({"network": "publicInternet"}), content_type="application/json")
        self.assertEqual(response.status_code, 403, response.content)
        self.assertEqual(self.client.get(self.url).json(), {"network": "disabled"})

    def test_administrator_can_change_default(self):
        WorkspaceMembership.objects.filter(workspace=self.workspace, user=self.user).update(role="admin")
        self.assertEqual(self.configure("publicInternet").status_code, 200)

    def test_setting_changes_only_new_environments_and_signed_grants_match_snapshot(self):
        old_response = self.create()
        self.assertEqual(old_response.status_code, 201, old_response.content)
        old = Session.objects.get(pk=old_response.json()["sessionId"])
        self.assertEqual(old.current_environment.network, "disabled")
        self.assertEqual(self.configure("publicInternet").status_code, 200)
        accepted = self.post()
        self.assertEqual(accepted.status_code, 202, accepted.content)
        run = AgentRun.objects.get(pk=accepted.json()["agentRunId"])
        self.assertEqual(run.environment.network, "publicInternet")
        self.assertEqual(run.authorization.payload["network"], "publicInternet")
        old.current_environment.refresh_from_db()
        self.assertEqual(old.current_environment.network, "disabled")
        run.environment.network = "disabled"
        with self.assertRaisesMessage(ValueError, "environment_network_immutable"):
            run.environment.save(update_fields=["network"])
