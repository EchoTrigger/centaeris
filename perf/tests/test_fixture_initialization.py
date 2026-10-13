"""Hosted fixtures initialize against the API's registered management commands."""
from contextlib import redirect_stdout
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "perf" / "harness"))
import bootstrap
import pool_workload
import transcript_capture_validation


class FixtureReady(Exception):
    """The transcript scenario has finished initialization and submitted its Run."""


class FixtureClient:
    def __init__(self):
        self.calls = []
        self.csrf = None

    def call(self, method, path, payload=None, **_kwargs):
        self.calls.append((method, path, payload))
        responses = {
            ("GET", "/api/csrf"): {"csrfToken": "synthetic-csrf"},
            ("POST", "/api/login"): {"user": {"id": "synthetic-owner"}},
            ("POST", "/api/admin/model-providers"): {"provider": {"id": "synthetic-provider"}},
            ("POST", "/api/admin/models"): {"model": {"id": "synthetic-model"}},
            ("GET", "/api/workspaces"): {"workspaces": [{"id": "synthetic-workspace"}]},
            ("GET", "/api/workspaces/synthetic-workspace/agents"): {"agents": [{"id": "synthetic-agent"}]},
            ("POST", "/api/workspaces/synthetic-workspace/sessions/new/messages"): {
                "sessionId": "synthetic-session", "agentRunId": "synthetic-run"},
        }
        return responses[(method, path)]

    def observe(self, accepted, *_args):
        if accepted != {"sessionId": "synthetic-session", "agentRunId": "synthetic-run"}:
            raise AssertionError("scenario did not retain the accepted Run identity")
        raise FixtureReady()


class FixtureInitializationTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        # Use real Django registration without connecting to PostgreSQL, Docker
        # or HTTP. The harness gate can run from a stdlib-only Python interpreter.
        python = ([sys.executable] if importlib.util.find_spec("django") else
                  ["uv", "run", "--frozen", "--package", "api", "python"])
        result = subprocess.run([*python, "-c",
            "import django,json; django.setup(); "
            "from django.core.management import get_commands; "
            "print(json.dumps(sorted(get_commands())))"],
            cwd=ROOT / "packages" / "api", check=True, capture_output=True, text=True,
            env={**os.environ, "DJANGO_SETTINGS_MODULE": "api.migration_test_settings"}, timeout=60)
        cls.commands = set(json.loads(result.stdout))

    def registered_command(self, args, **_kwargs):
        if "manage.py" not in args:
            raise AssertionError("unexpected external command while initializing fixture")
        command = args[args.index("manage.py") + 1]
        self.assertIn(command, self.commands, "fixture dispatched an unavailable Django management command")
        return subprocess.CompletedProcess(args, 0, stdout="", stderr="")

    def stack(self):
        stack = unittest.mock.Mock()
        stack.values = {"BOOTSTRAP_SUPERADMIN_EMAIL": "synthetic@localhost.invalid",
                        "BOOTSTRAP_SUPERADMIN_PASSWORD": "synthetic-fixture-password"}
        stack.compose.side_effect = self.registered_command
        stack.quiet.return_value = True
        stack.manifest.return_value = {"project": "synthetic-fixture"}
        return stack

    def assert_fixture(self, client, model_name, *, submitted):
        providers = [payload for method, path, payload in client.calls
                     if method == "POST" and path == "/api/admin/model-providers"]
        models = [payload for method, path, payload in client.calls
                  if method == "POST" and path == "/api/admin/models"]
        self.assertEqual(len(providers), 1)
        self.assertEqual(providers[0]["apiBase"], "https://mock-model:9999/v1")
        self.assertEqual(len(models), 1)
        self.assertEqual(models[0]["providerId"], "synthetic-provider")
        self.assertEqual(models[0]["modelName"], model_name)
        if submitted:
            messages = [payload for method, path, payload in client.calls
                        if method == "POST" and path.endswith("/sessions/new/messages")]
            self.assertEqual(len(messages), 1)
            self.assertEqual(messages[0]["agentId"], "synthetic-agent")
            self.assertEqual(messages[0]["modelConfigRef"], "synthetic-model")
            self.assertEqual(messages[0]["attachmentRefs"], [])
            self.assertTrue(messages[0]["operationId"])

    def test_smoke_initializes_model_and_completes_synthetic_run(self):
        client = FixtureClient()
        def call(method, path, payload=None, *_args, **_kwargs):
            result = client.call(method, path, payload)
            return (202 if path.endswith("/messages") else 200), result
        stream = io.BytesIO(b'data: {"event":{"type":"agent_run_completed"}}\n')
        with patch.object(bootstrap, "PASSWORD", "synthetic-fixture-password"), \
                patch.object(bootstrap, "call", side_effect=call), \
                patch.object(bootstrap.subprocess, "run", side_effect=self.registered_command), \
                patch.object(bootstrap.urllib.request, "urlopen", return_value=stream), \
                redirect_stdout(io.StringIO()):
            bootstrap.main()
        self.assert_fixture(client, "perf-instant", submitted=True)

    def test_pool_initialization_returns_bound_workspace_agent_and_model(self):
        client = FixtureClient()
        self.assertEqual(pool_workload.setup(self.stack(), client),
                         ("synthetic-workspace", "synthetic-agent", "synthetic-model"))
        self.assert_fixture(client, "perf-paced", submitted=False)

    def test_transcript_initialization_reaches_accepted_run(self):
        client = FixtureClient()
        with tempfile.TemporaryDirectory() as temporary, \
                patch.object(transcript_capture_validation.control, "Stack", return_value=self.stack()), \
                patch.object(transcript_capture_validation, "Client", return_value=client):
            output = Path(temporary) / "evidence"
            with self.assertRaises(FixtureReady):
                transcript_capture_validation.main(output)
            report = json.loads((output / "report.json").read_text())
        self.assertEqual(report["accepted"], {"sessionId": "synthetic-session", "agentRunId": "synthetic-run"})
        self.assert_fixture(client, "perf-transcript-capture", submitted=True)


if __name__ == "__main__":
    unittest.main()
