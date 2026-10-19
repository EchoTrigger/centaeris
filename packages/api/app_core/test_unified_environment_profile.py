"""One signed environment contract, independently of lifecycle or caller kind."""
import copy
import hashlib
import json
from types import SimpleNamespace
from unittest.mock import patch

from django.test import SimpleTestCase

from .test_agent_run_authorization import fixture
from .runtime_contract import (authorization_digest, authorization_signature,
    build_agent_run_authorization_payload, verify_agent_run_authorization_signature)


def profile():
    return {"schema": "runtime.environment_profile.v1", "imageDigest": "sha256:" + "a" * 64,
        "workspaceRoot": "/workspace", "user": "10001:10001", "network": "disabled",
        "resources": {"cpuMilli": 2000, "memoryBytes": 2 * 1024**3, "pidsLimit": 512,
            "dataTmpfsBytes": 4 * 1024**3, "storageBytes": 4 * 1024**3}}


def payload():
    value = fixture()
    for field in ("imageCapability", "imageDigest", "resources", "network", "benchmarkProfile", "benchmarkProfileDigest"):
        value.pop(field, None)
    value.update(schema="workspace.agent_run_authorization.v1", environmentProfile=profile(),
        environmentProfileDigest="sha256:" + hashlib.sha256(json.dumps(profile(), ensure_ascii=False,
            separators=(",", ":"), sort_keys=True).encode("utf-8")).hexdigest())
    return value


class UnifiedEnvironmentAuthorizationTests(SimpleTestCase):
    def test_generic_profile_is_the_only_signed_execution_configuration(self):
        value = payload()
        signature = authorization_signature(value, "test-key")
        verify_agent_run_authorization_signature(value, "test-key", signature)
        for field, content in (("imageCapability", "workspace_general_v1"),
                ("benchmarkProfile", profile()), ("resources", profile()["resources"]),
                ("imageDigest", profile()["imageDigest"]), ("network", "disabled")):
            with self.subTest(field=field), self.assertRaises(ValueError):
                authorization_digest(value | {field: content})
        for schema in ("workspace.agent_run_authorization.v2", "workspace.agent_run_authorization.v3"):
            with self.subTest(schema=schema), self.assertRaises(ValueError):
                authorization_digest(value | {"schema": schema})
        for field in ("environmentProfile", "environmentProfileDigest"):
            for missing in (True, False):
                changed = copy.deepcopy(value)
                if missing:
                    del changed[field]
                else:
                    changed[field] = None
                with self.subTest(field=field, missing=missing), self.assertRaises(ValueError):
                    authorization_digest(changed)
        changed = copy.deepcopy(value)
        changed["environmentProfile"]["network"] = "publicInternet"
        with self.assertRaises(ValueError):
            authorization_digest(changed)

    def test_builder_reads_the_immutable_snapshot_instead_of_current_defaults(self):
        value = payload()
        run = SimpleNamespace(id=value["agentRunId"], workspace_id=value["workspaceId"],
            user_id=value["userId"], session_id=value["sessionId"], modelConfig_id=value["modelConfigRef"],
            thinkingMode=None, definition_version_id=None, workspace=object(),
            session=SimpleNamespace(agent_id=value["agentId"]), environment=SimpleNamespace(
                environment_profile=value["environmentProfile"], environment_profile_digest=value["environmentProfileDigest"],
                network="disabled", benchmark_profile=None))
        with patch("app_core.runtime_contract.session_workspace_for_session", return_value=value["sessionWorkspace"]), \
                patch("app_core.runtime_contract.plugin_activation_for_workspace", return_value=value["pluginActivation"]):
            built = build_agent_run_authorization_payload(run, value["id"], image_digest="sha256:" + "b" * 64,
                declared_inputs=value["assetRefs"], message_asset_refs=value["messageAssetRefs"])
        self.assertEqual(set(built), set(value))
        self.assertEqual(built['schema'], value['schema'])
        self.assertEqual(built['environmentProfile'], value['environmentProfile'])
        self.assertEqual(built['environmentProfileDigest'], value['environmentProfileDigest'])

    def test_both_lifecycle_modes_require_the_same_real_controller_shape(self):
        from .environment_authorization import sign_environment_authorization
        value = {"schema": "workspace.environment_authorization.v1", "environmentId": "environment_1",
            "sessionId": "session_1", "agentRunId": "run_1", "agentRunAuthorizationDigest": "sha256:" + "a" * 64,
            "executionMode": "perRun", "controllerId": "run_1", "controllerEpoch": 1,
            "controllerLeaseDigest": "sha256:" + "b" * 64, "controllerPurpose": "execute"}
        for mode in ("perRun", "retained"):
            current = value | {"executionMode": mode}
            self.assertTrue(sign_environment_authorization(current, "test-key").startswith("hmac-sha256:"))
            for name in ("controllerId", "controllerEpoch", "controllerLeaseDigest", "controllerPurpose"):
                missing = current.copy()
                del missing[name]
                with self.subTest(mode=mode, field=name), self.assertRaises(ValueError):
                    sign_environment_authorization(missing, "test-key")
        with self.assertRaises(ValueError):
            sign_environment_authorization(value | {"schema": "workspace.environment_authorization.v2"}, "test-key")
