"""Fresh control attachment and fixed helper scope through the real SDK."""

import base64
from contextlib import contextmanager
import json
import os
from pathlib import Path
import queue
import subprocess
import sys
import threading
import unittest

from centaeris_e2b_adapter.bridge import Adapter, AdapterConfig
from test_adapter import request
from test_bulk_transport import direct_fixture_network
from test_sdk_transport_acceptance import create_request, lifecycle_request, provider_fixture

EXPECTED_METADATA = {key: "frozen-" + key for key in (
    "centaerisAttemptId", "environmentId", "instanceId", "executionId", "agentRunId",
    "sessionId", "workspaceId", "profileDigest", "authorizationDigest", "processingJobId")}


def helper_request(helper="agent-process-state", **extra):
    return request("invokeControlHelper", {"sandboxId": "sandbox-fixture", "helper": helper,
        "expectedTemplateId": "template-fixture", "expectedMetadata": EXPECTED_METADATA,
        "workspaceRoot": "/mnt/data", "workspaceUser": "agent", "timeoutSeconds": 5,
        "maxOutputBytes": 65536, **extra})


def setup_detail(fixture, **extra):
    original = fixture.detail
    detail = {**original(), "metadata": dict(EXPECTED_METADATA),
              "lifecycle": {"onTimeout": "kill", "autoResume": False}, **extra}
    fixture.detail = lambda: dict(detail)
    return detail


def control_adapter(fixture, lane="control"):
    return Adapter(AdapterConfig(api_key="e2b_" + "a" * 64, api_url=fixture.api_url,
        sandbox_url=fixture.api_url, lane=lane))


class AdapterProcess:
    def __init__(self, fixture, lane):
        env = dict(os.environ)
        for key in ("HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "http_proxy", "https_proxy", "all_proxy",
                    "E2B_DOMAIN", "E2B_ADAPTER_SPOOL_ROOT"):
            env.pop(key, None)
        env.update({"NO_PROXY": "*", "no_proxy": "*", "E2B_API_KEY": "e2b_" + "a" * 64,
                    "E2B_API_URL": fixture.api_url, "E2B_SANDBOX_URL": fixture.api_url})
        env["PYTHONPATH"] = str(Path(__file__).resolve().parents[1])
        self.process = subprocess.Popen([sys.executable, "-B", "-m", "centaeris_e2b_adapter", "--lane", lane],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True, env=env)
        self.responses = queue.Queue()
        def read():
            for line in self.process.stdout:
                self.responses.put(json.loads(line))
        threading.Thread(target=read, daemon=True).start()

    def send(self, value):
        self.process.stdin.write(json.dumps(value) + "\n")
        self.process.stdin.flush()

    def receive(self):
        return self.responses.get(timeout=5)

    def close(self):
        if self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=5)
        self.process.stdin.close()
        self.process.stdout.close()


class ControlHelperTests(unittest.TestCase):
    def test_fresh_control_uses_exact_fixed_process_and_scoped_token_without_resume(self):
        for helper, user in (("prepare-workspace", "root"), ("agent-process-state", "root"), ("quiesce-agent-processes", "agent")):
            with self.subTest(helper=helper), direct_fixture_network(), provider_fixture() as fixture:
                detail = setup_detail(fixture)
                end_at = detail["endAt"]
                response = control_adapter(fixture).handle(helper_request(helper))
                self.assertEqual(response["outcome"], "succeeded", response)
                self.assertEqual(response["result"]["helper"], helper)
                self.assertEqual(response["result"]["exitCode"], 0)
                self.assertEqual(base64.b64decode(response["result"]["stdoutBytesBase64"]), b"centaeris-adapter-ok\n")
                self.assertEqual([(entry["method"], entry["path"]) for entry in fixture.requests],
                                 [("GET", "/sandboxes/sandbox-fixture"), ("POST", "/process.Process/Start")])
                process = fixture.requests[1]
                self.assertEqual(process["body"]["process"], {"cmd": "/opt/centaeris/bin/execution_agent",
                    "args": [helper], "cwd": "/", "envs": {"CENTAERIS_WORKSPACE_ROOT": "/mnt/data", "CENTAERIS_WORKSPACE_USER": "10001:10001"}})
                self.assertEqual(process["headers"]["authorization"], "Basic " + base64.b64encode((user + ":").encode()).decode())
                self.assertEqual(process["headers"]["x-access-token"], "fixture-envd-access-token")
                self.assertNotIn("x-api-key", process["headers"])
                self.assertNotIn("fixture-envd-access-token", json.dumps(response))
                self.assertEqual(fixture.detail()["endAt"], end_at)

    def test_malformed_or_generic_execution_payload_never_reaches_provider(self):
        with direct_fixture_network(), provider_fixture() as fixture:
            setup_detail(fixture)
            adapter = control_adapter(fixture)
            values = [helper_request(helper="bash"), helper_request(helper="snapshot-capture"),
                helper_request(workspaceRoot="/tmp"), helper_request(workspaceUser="root"),
                helper_request(timeoutSeconds=0), helper_request(timeoutSeconds=61),
                helper_request(maxOutputBytes=0), helper_request(maxOutputBytes=65537),
                helper_request(expectedTemplateId="template:latest"), helper_request(expectedMetadata={}),
                helper_request(expectedMetadata={key: value for key, value in EXPECTED_METADATA.items() if key != "profileDigest"}),
                helper_request(expectedMetadata={**EXPECTED_METADATA, "unexpected": "value"}),
                helper_request(command="echo unsafe"), helper_request(envs={"UNSAFE": "yes"})]
            for value in values:
                with self.subTest(payload=value["payload"]):
                    self.assertEqual(adapter.handle(value)["outcome"], "notDispatched")
            self.assertEqual(fixture.requests, [])

    def test_fixed_helper_is_rejected_in_ordinary_and_cleanup_lanes(self):
        for lane in ("ordinary", "cleanup"):
            with self.subTest(lane=lane), direct_fixture_network(), provider_fixture() as fixture:
                setup_detail(fixture)
                response = control_adapter(fixture, lane).handle(helper_request())
                self.assertEqual(response["outcome"], "notDispatched", response)
                self.assertEqual(fixture.requests, [])

    def test_unsafe_or_missing_provider_policy_never_starts_a_helper(self):
        policies = ({}, {"lifecycle": None}, {"lifecycle": {"onTimeout": "kill"}},
            *({"lifecycle": {"onTimeout": "kill", "autoResume": value}} for value in (None, True, 0, "false")),
            {"lifecycle": {"onTimeout": "pause", "autoResume": False}}, {"state": "paused"},
            {"sandboxID": "another-sandbox"}, {"templateID": "another-template"},
            {"metadata": {**EXPECTED_METADATA, "instanceId": "another-instance"}},
            {"metadata": {**EXPECTED_METADATA, "providerExtra": "unexpected"}},
            {"metadata": {key: value for key, value in EXPECTED_METADATA.items() if key != "profileDigest"}},
            {"envdAccessToken": None}, {"envdAccessToken": ""}, {"envdAccessToken": "invalid\ntoken"})
        for index, extra in enumerate(policies):
            with self.subTest(case=index), direct_fixture_network(), provider_fixture() as fixture:
                detail = setup_detail(fixture, **extra)
                if index == 0:
                    detail.pop("lifecycle")
                response = control_adapter(fixture).handle(helper_request())
                self.assertEqual(response["outcome"], "unknown", response)
                self.assertEqual([(entry["method"], entry["path"]) for entry in fixture.requests],
                                 [("GET", "/sandboxes/sandbox-fixture")])

    def test_missing_secure_token_never_starts_a_helper(self):
        with direct_fixture_network(), provider_fixture() as fixture:
            detail = setup_detail(fixture)
            detail.pop("envdAccessToken")
            response = control_adapter(fixture).handle(helper_request())
            self.assertEqual(response["outcome"], "unknown", response)
            self.assertEqual([(entry["method"], entry["path"]) for entry in fixture.requests],
                             [("GET", "/sandboxes/sandbox-fixture")])

    def test_nonzero_helper_exit_keeps_bytes_and_redacts_provider_diagnostics(self):
        with direct_fixture_network(), provider_fixture("commandExit") as fixture:
            setup_detail(fixture)
            response = control_adapter(fixture).handle(helper_request())
            self.assertEqual(response["outcome"], "succeeded", response)
            self.assertEqual(response["result"]["exitCode"], 7)
            self.assertEqual(base64.b64decode(response["result"]["stdoutBytesBase64"]), b"centaeris-adapter-ok\n")
            self.assertNotIn("fixture-private-diagnostic", json.dumps(response))

    def test_control_output_over_limit_remains_unknown_with_known_pid(self):
        with direct_fixture_network(), provider_fixture() as fixture:
            setup_detail(fixture)
            response = control_adapter(fixture).handle(helper_request(maxOutputBytes=2))
            self.assertEqual(response["outcome"], "unknown", response)
            self.assertEqual(response["result"]["pid"], 71)
            self.assertEqual(response["error"]["code"], "outputLimitExceeded")
            self.assertNotIn("stdoutBytesBase64", response["result"])

    def test_fresh_control_process_finishes_while_ordinary_original_stream_waits(self):
        with direct_fixture_network(), provider_fixture("blockedOrdinary") as fixture:
            setup_detail(fixture)
            ordinary, control = AdapterProcess(fixture, "ordinary"), AdapterProcess(fixture, "control")
            try:
                ordinary.send(create_request())
                self.assertEqual(ordinary.receive()["outcome"], "succeeded")
                # Restore authoritative identity metadata after the fixture records create.
                setup_detail(fixture)
                ordinary.send(lifecycle_request("runCommand", command="blocked", timeoutSeconds=5))
                self.assertEqual(ordinary.receive()["outcome"], "succeeded")
                self.assertTrue(fixture.command_started.wait(2))
                ordinary.send(lifecycle_request("waitCommand", pid=71, timeoutSeconds=5))
                control.send(helper_request("quiesce-agent-processes"))
                advanced = control.receive()
                self.assertEqual(advanced["outcome"], "succeeded", advanced)
                self.assertEqual(advanced["result"]["helper"], "quiesce-agent-processes")
                self.assertTrue(ordinary.responses.empty())
                fixture.command_release.set()
                self.assertEqual(ordinary.receive()["outcome"], "succeeded")
            finally:
                fixture.command_release.set()
                ordinary.close()
                control.close()
