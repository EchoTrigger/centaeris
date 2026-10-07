"""Real Rust connector clients revalidate cached capabilities against Django."""
import json
import os
import subprocess
from pathlib import Path

from django.db import connection
from django.test import LiveServerTestCase

from app_core.models import AgentConnectorCredentialApproval
from app_core.test_assistant_connectors import AssistantConnectorFixture


class AssistantConnectorRuntimeTests(AssistantConnectorFixture, LiveServerTestCase):
    serialized_rollback = True

    def test_rust_client_rejects_revoked_cached_connector(self):
        approval = self.ready_binding()
        run = self.submit_run()
        database = connection.settings_dict
        self.assertEqual(database["ENGINE"], "django.db.backends.postgresql")
        self.assertTrue(database["NAME"].startswith("test_"), "interop requires a Django test database")
        fixture = {
            "liveServerUrl": self.live_server_url,
            "agentRunId": run.id,
            "authorizationRef": run.authorization.id,
            "authorizationDigest": run.authorization.digest,
            "resourcePath": "mcp.json",
            "resourceDigest": self.packages[0]["mcpServers"][0]["digest"],
            "approvalId": approval["id"],
            "database": {key: database[key] for key in ("NAME", "USER", "PASSWORD", "HOST", "PORT")},
        }
        environment = os.environ.copy()
        environment["ASSISTANT_CONNECTOR_FIXTURE"] = json.dumps(fixture)
        root = Path(__file__).resolve().parents[3]
        completed = subprocess.run([
            "cargo", "test", "-p", "runtime_server", "mcp::tests::python_connector_interoperability",
            "--", "--ignored", "--exact", "--nocapture",
        ], cwd=root, env=environment, capture_output=True, text=True, timeout=300)
        output = completed.stdout + completed.stderr
        self.assertEqual(completed.returncode, 0, output)
        self.assertIn("running 1 test", output)
        self.assertIn("test mcp::tests::python_connector_interoperability ...", output)
        self.assertIn("assistant_connector_interoperability: cached_calls=2 revoked_calls=0", output)
        self.assertIn("1 passed; 0 failed; 0 ignored", output)
        self.assertIsNotNone(AgentConnectorCredentialApproval.objects.get(pk=approval["id"]).revoked_at)
        print("assistant_connector_interoperability: cached_calls=2 revoked_calls=0")
