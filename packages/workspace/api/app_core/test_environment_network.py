"""Workspace defaults become immutable, signed execution environment policy."""
import json
from copy import deepcopy
from pathlib import Path
from contextlib import nullcontext
from unittest.mock import patch

from django.test import Client, SimpleTestCase, TestCase, override_settings

from .models import AgentRun, Session, SessionEnvironment, WorkspaceMembership
from .test_hosted_operations import OperationFixture
from .runtime_contract import authorization_digest, authorization_signature, verify_agent_run_authorization_signature


class NetworkAuthorizationTests(SimpleTestCase):
    def test_cross_language_fixtures_sign_network_and_reject_missing_or_mismatched_policy(self):
        root = Path(__file__).resolve().parents[4] / "tests/workspace/fixtures/agent_run_authorization/v2"
        for name in ("valid", "custom"):
            payload = json.loads((root / f"{name}.json").read_text(encoding="utf-8"))
            expected_name = "expected" if name == "valid" else "custom-expected"
            expected = json.loads((root / f"{expected_name}.json").read_text(encoding="utf-8"))
            self.assertEqual(authorization_digest(payload), expected["digest"])
            self.assertEqual(authorization_signature(payload, "test-key"), expected["signature"])
            missing = deepcopy(payload)
            del missing["environmentProfile"]["network"]
            with self.assertRaises(ValueError):
                authorization_digest(missing)
            for value in (None, "host", "publicInternet" if payload["environmentProfile"]["network"] == "disabled" else "disabled"):
                changed = deepcopy(payload)
                changed["environmentProfile"]["network"] = value
                with self.assertRaises(ValueError):
                    verify_agent_run_authorization_signature(changed, "test-key", expected["signature"])


@override_settings(SANDBOX_CPU_MILLI=1000, SANDBOX_MEMORY_BYTES=2 * 1024**3,
    SANDBOX_WORKSPACE_BYTES=4 * 1024**3, SANDBOX_PIDS_LIMIT=512)
class EnvironmentNetworkTests(OperationFixture, TestCase):
    def setUp(self):
        super().setUp()
        for target in ("app_core.http.workspaces.plugin_lifecycle_lock",
                       "app_core.plugin_catalog.plugin_lifecycle_lock",
                       "app_core.agent_run_authorization_factory.plugin_lifecycle_lock"):
            context = patch(target, side_effect=lambda *a, **k: nullcontext())
            context.start()
            self.addCleanup(context.stop)
        self.url = f"/api/workspaces/{self.workspace.pk}/environment-settings"
        WorkspaceMembership.objects.filter(workspace=self.workspace, user=self.user).update(role="owner")

    def configure(self, network):
        return self.client.patch(self.url, data=json.dumps({"network": network}), content_type="application/json")

    def test_settings_responses_and_partial_updates_match_openapi(self):
        from api.ninja_api import api
        from jsonschema import Draft202012Validator

        schema = api.get_openapi_schema()
        operations = schema["paths"]["/api/workspaces/{workspace_id}/environment-settings"]
        request_schema = operations["patch"]["requestBody"]["content"]["application/json"]["schema"]
        request_validator = Draft202012Validator({**request_schema, "components": schema["components"]})
        self.assertTrue(request_validator.is_valid({"cpuMilli": 2000}))
        for invalid in ({}, {"cpuMilli": None}, {"cpuMilli": True}, {"workspaceBytes": 2**53}, {"revision": 2}):
            with self.subTest(payload=invalid):
                self.assertFalse(request_validator.is_valid(invalid))

        for method, response, expected in (
            ("get", self.client.get(self.url), 200),
            ("patch", self.client.patch(self.url, data=json.dumps({"cpuMilli": 2000}), content_type="application/json"), 200),
            ("patch", self.client.patch(self.url, data=json.dumps({"cpuMilli": None}), content_type="application/json"), 400),
        ):
            self.assertEqual(response.status_code, expected, response.content)
            responses = {str(code): value for code, value in operations[method]["responses"].items()}
            response_schema = responses[str(expected)]["content"]["application/json"]["schema"]
            Draft202012Validator({**response_schema, "components": schema["components"]}).validate(response.json())

    @staticmethod
    def settings_value(*, network="disabled", cpu=1000, memory=2 * 1024**3, workspace=4 * 1024**3, revision=1):
        return {"network": network, "cpuMilli": cpu, "memoryBytes": memory,
            "workspaceBytes": workspace, "revision": revision}

    def test_default_and_admin_update_are_strict_and_persisted(self):
        self.assertEqual(self.client.get(self.url).json(), self.settings_value())
        response = self.configure("publicInternet")
        self.assertEqual(response.status_code, 200, response.content)
        expected = self.settings_value(network="publicInternet", revision=2)
        self.assertEqual(response.json(), expected)
        for invalid in ({}, {"network": "host"}, {"network": None}, {"network": True},
                {"network": "disabled", "unknown": True}, {"revision": 3}, {"cpu_milli": 2000},
                *({field: value} for field in ("cpuMilli", "memoryBytes", "workspaceBytes")
                    for value in (None, True, 0, -1, "2000", 1.5)),
                {"cpuMilli": 2**32}, {"memoryBytes": 2**53}, {"workspaceBytes": 2**53}):
            with self.subTest(payload=invalid):
                rejected = self.client.patch(self.url, data=json.dumps(invalid), content_type="application/json")
                self.assertEqual(rejected.status_code, 400, rejected.content)
        self.assertEqual(self.client.get(self.url).json(), expected)
        self.assertEqual(self.configure("publicInternet").json(), expected)

    def test_resource_updates_are_partial_and_increment_one_revision_for_actual_changes(self):
        desired = {"network": "publicInternet", "cpuMilli": 2000,
            "memoryBytes": 4 * 1024**3, "workspaceBytes": 10 * 1024**3}
        response = self.client.patch(self.url, data=json.dumps(desired), content_type="application/json")
        self.assertEqual(response.status_code, 200, response.content)
        self.assertEqual(response.json(), desired | {"revision": 2})
        self.workspace.refresh_from_db()
        self.assertEqual(self.workspace.execution_resources, {"cpuMilli": 2000, "memoryBytes": 4 * 1024**3,
            "workspaceBytes": 10 * 1024**3, "storageBytes": 10 * 1024**3, "pidsLimit": 512})
        for update, revision in (({"cpuMilli": 2000}, 2), ({"cpuMilli": 3000}, 3)):
            response = self.client.patch(self.url, data=json.dumps(update), content_type="application/json")
            self.assertEqual(response.status_code, 200, response.content)
            desired.update(update)
            self.assertEqual(response.json(), desired | {"revision": revision})
            self.assertEqual(self.client.get(self.url).json(), response.json())

    def test_member_cannot_read_or_change_workspace_policy(self):
        WorkspaceMembership.objects.filter(workspace=self.workspace, user=self.user).update(role="member")
        self.assertEqual(self.client.get(self.url).status_code, 404)
        self.assertEqual(self.configure("publicInternet").status_code, 404)
        self.assertEqual(self.client.patch(self.url, data=json.dumps({"cpuMilli": 2000}),
            content_type="application/json").status_code, 404)
        self.workspace.refresh_from_db()
        self.assertEqual(self.workspace.execution_network, "disabled")

    def test_settings_require_authentication_and_csrf_for_mutation(self):
        client = Client(enforce_csrf_checks=True)
        self.assertEqual(client.get(self.url).status_code, 401)
        client.force_login(self.user)
        self.assertEqual(client.get(self.url).status_code, 200)
        response = client.patch(self.url, data=json.dumps({"network": "publicInternet"}), content_type="application/json")
        self.assertEqual(response.status_code, 403, response.content)
        self.assertEqual(self.client.get(self.url).json(), self.settings_value())

    def test_administrator_can_change_default(self):
        WorkspaceMembership.objects.filter(workspace=self.workspace, user=self.user).update(role="admin")
        self.assertEqual(self.configure("publicInternet").status_code, 200)

    def test_resource_defaults_belong_to_new_environments_and_first_grant_uses_that_snapshot(self):
        old = Session.objects.get(pk=self.create().json()["sessionId"])
        old_resources = {"cpuMilli": 1000, "memoryBytes": 2 * 1024**3, "pidsLimit": 512,
            "workspaceBytes": 4 * 1024**3, "storageBytes": 4 * 1024**3}
        new_resources = old_resources | {"cpuMilli": 2000, "memoryBytes": 4 * 1024**3,
            "workspaceBytes": 10 * 1024**3, "storageBytes": 10 * 1024**3}
        response = self.client.patch(self.url, data=json.dumps({"cpuMilli": 2000,
            "memoryBytes": 4 * 1024**3, "workspaceBytes": 10 * 1024**3}), content_type="application/json")
        self.assertEqual(response.status_code, 200, response.content)
        responses = [self.post(self.create_url + f"/{old.pk}/messages", operationId="old-run"),
            self.post(operationId="new-run")]
        runs = []
        for accepted, resources, revision in zip(responses, (old_resources, new_resources), (1, 2)):
            self.assertEqual(accepted.status_code, 202, accepted.content)
            run = AgentRun.objects.get(pk=accepted.json()["agentRunId"])
            self.assertEqual(run.environment.resource_defaults, resources)
            self.assertEqual(run.environment.resource_defaults_revision, revision)
            self.assertEqual(run.authorization.payload["environmentProfile"]["resources"], resources)
            runs.append(run)
        original_grants = [(deepcopy(run.authorization.payload), run.authorization.digest, run.authorization.signature)
            for run in runs]
        self.assertEqual(self.configure("publicInternet").status_code, 200)
        for run, original in zip(runs, original_grants):
            run.refresh_from_db()
            self.assertEqual((run.authorization.payload, run.authorization.digest, run.authorization.signature), original)
            self.assertEqual(run.environment.environment_profile, original[0]["environmentProfile"])
        later = SessionEnvironment.objects.create(session=old)
        self.assertEqual(later.resource_defaults, new_resources)
        self.assertEqual(later.resource_defaults_revision, 3)
        self.assertEqual(later.network, "publicInternet")

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
        self.assertEqual(run.authorization.payload["environmentProfile"]["network"], "publicInternet")
        old.current_environment.refresh_from_db()
        self.assertEqual(old.current_environment.network, "disabled")
        run.environment.network = "disabled"
        with self.assertRaisesMessage(ValueError, "environment_network_immutable"):
            run.environment.save(update_fields=["network"])
