"""Independent maintenance grants bind one operation to one physical instance."""
import copy
import json
from pathlib import Path
import unittest

from .environment_maintenance_authorization import sign_maintenance_authorization


class MaintenanceAuthorizationTests(unittest.TestCase):
    def test_shared_runtime_vector_matches_real_signer(self):
        path = Path(__file__).resolve().parents[3] / "tests/workspace/fixtures/environment_maintenance_authorization/v1/valid.json"
        vector = json.loads(path.read_text(encoding="utf-8"))
        self.assertEqual(sign_maintenance_authorization(vector["payload"], "test-key"), vector["signature"])

    def payload(self):
        return {
            "schema": "workspace.environment_maintenance_authorization.v1",
            "workspaceId": "workspace_test", "sessionId": "session_test",
            "environmentId": "environment_test", "instanceId": "instance_" + "a" * 64,
            "containerId": "b" * 64, "profileDigest": "sha256:" + "c" * 64,
            "controllerOwner": {"kind": "maintenanceOperation", "operationId": "prepare_test"},
            "controllerEpoch": 2, "controllerLeaseDigest": "sha256:" + "d" * 64,
            "controllerPurpose": "execute", "requestDigest": "sha256:" + "e" * 64,
            "expiresAtMs": 1800000000000,
        }

    def test_all_physical_authority_and_request_fields_are_signed(self):
        original = self.payload()
        signature = sign_maintenance_authorization(original, "test-key")
        self.assertEqual(original, self.payload())
        self.assertEqual(signature, sign_maintenance_authorization(dict(reversed(list(original.items()))), "test-key"))
        for name, value in {
            "workspaceId": "another_workspace", "sessionId": "another_session",
            "environmentId": "another_environment", "instanceId": "instance_" + "f" * 64,
            "containerId": "f" * 64, "profileDigest": "sha256:" + "f" * 64,
            "controllerOwner": {"kind": "maintenanceOperation", "operationId": "another_operation"},
            "controllerEpoch": 3, "controllerLeaseDigest": "sha256:" + "f" * 64,
            "controllerPurpose": "cleanup", "requestDigest": "sha256:" + "f" * 64,
            "expiresAtMs": 1800000000001,
        }.items():
            with self.subTest(field=name):
                self.assertNotEqual(signature, sign_maintenance_authorization(original | {name: value}, "test-key"))

    def test_rejects_fake_run_owner_unknown_fields_and_unbounded_grants(self):
        for changes in (
            {"controllerOwner": {"kind": "agentRun", "runId": "prepare_test"}},
            {"controllerOwner": {"kind": "maintenanceOperation", "operationId": ""}},
            {"controllerOwner": {"kind": "maintenanceOperation", "operationId": "op", "runId": "fake"}},
            {"agentRunId": "fake"}, {"controllerId": "opaque"},
            {"schema": "workspace.environment_authorization.v2"},
            {"controllerEpoch": True}, {"controllerEpoch": 0}, {"controllerEpoch": 2**63},
            {"expiresAtMs": True}, {"expiresAtMs": 0}, {"expiresAtMs": 2**63},
            {"containerId": "mutable-name"}, {"instanceId": "arbitrary"},
            {"profileDigest": "sha256:" + "A" * 64}, {"requestDigest": ""},
            {"controllerPurpose": "maintenance"}, {"workspaceId": "../foreign"},
        ):
            with self.subTest(changes=changes), self.assertRaises(ValueError):
                sign_maintenance_authorization(self.payload() | changes, "test-key")
        for name in self.payload():
            missing = copy.deepcopy(self.payload())
            del missing[name]
            with self.subTest(missing=name), self.assertRaises(ValueError):
                sign_maintenance_authorization(missing, "test-key")
        with self.assertRaises(ValueError):
            sign_maintenance_authorization(self.payload(), "")


if __name__ == "__main__":
    unittest.main()
