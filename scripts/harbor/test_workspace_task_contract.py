"""Harbor task requests and receipts against the generated, typed Core corpus."""
import copy
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
import socket
import threading
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from workspace_client import WorkspaceError
from workspace_environment import OwnedEnvironment
from workspace_provisioner import WorkspaceHTTPProvisioner


CORPUS = Path(__file__).resolve().parents[2] / "tests/workspace/fixtures/environment_tasks/v1/maintenance.json"


def canonical(value):
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"))


class CoreTaskContractTests(unittest.TestCase):
    def setUp(self):
        self.contract = json.loads(CORPUS.read_text(encoding="utf-8"))
        self.assertEqual(self.contract["schema"], "centaeris.environmentTaskContract.v1")
        self.requests, self.events = [], []
        self.start_case = "start"
        self.transform = lambda operation, response: response
        self.lose_start = False
        fixture = self.contract
        self.fence = fixture["takeover"]["response"]["result"]["fence"]
        self.operation = self.fence["controllerOwner"]["operationId"]
        state = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_POST(self):
                raw = self.rfile.read(int(self.headers["Content-Length"]))
                value = json.loads(raw)
                state.requests.append(value)
                request = value["task"]["request"]
                operation = request.get("operation")
                state.events.append(operation)
                if operation == "query":
                    case = next((page for page in fixture["queryPages"]
                        if canonical(page["request"]["request"]["cursor"]) == canonical(request.get("cursor"))), None)
                else:
                    case = fixture.get(state.start_case if operation == "start" else operation)
                if (self.path != "/internal/environments/maintenance/task" or case is None
                        or canonical(value["task"]) != canonical(case["request"])
                        or canonical(value["request"]) != canonical(fixture["command"])):
                    body = json.dumps({"error": "fixture_core_request_invalid"}).encode()
                    self.send_response(400)
                else:
                    if operation == "start" and state.lose_start:
                        self.connection.shutdown(socket.SHUT_RDWR)
                        self.connection.close()
                        return
                    body = json.dumps(state.transform(operation, copy.deepcopy(case["response"]))).encode()
                    self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

        server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        origin = f"http://127.0.0.1:{server.server_port}"
        self.provisioner = WorkspaceHTTPProvisioner(api_origin="https://unused.invalid", runtime_origin=origin,
            maintenance_token="fixture-maintenance-" + "m" * 32, internal_token="fixture-internal-" + "i" * 32)
        self.provisioner._profiles["environment"] = {"workspaceRoot": "/app", "user": "0:0"}
        self.owned = OwnedEnvironment("session", "environment", None, "instance", "container", "host", 0,
            "sha256:" + "a" * 64, "sha256:" + "b" * 64, "resident", 1)
        grant = {"payload": {
            "controllerEpoch": self.fence["controllerEpoch"],
            "controllerOwner": self.fence["controllerOwner"],
            "controllerPurpose": self.fence["controllerPurpose"],
        }, "signature": "fixture"}
        issue = patch.object(self.provisioner, "_issue", return_value=grant)
        release = patch.object(self.provisioner, "_release",
            side_effect=lambda value: self.events.append("release"))
        identity = patch("workspace_provisioner.uuid.uuid4",
            return_value=SimpleNamespace(hex=self.operation.removeprefix("maintenance-")))
        for replacement in (issue, release, identity):
            self.addCleanup(replacement.stop)
        self.issue, self.release = issue.start(), release.start()
        identity.start()

    def execute(self):
        command = self.contract["command"]
        return self.provisioner.exec(self.owned, command=command["args"][1], cwd=command["cwd"],
            env={}, user="0:0", timeout_sec=command["timeoutMs"] / 1000)

    def test_exec_matches_complete_core_envelopes_and_advances_each_output_cursor(self):
        self.assertEqual(self.execute(), {"returnCode": 0, "stdout": "ok", "stderr": ""})
        expected = [self.contract["takeover"]["request"], self.contract["start"]["request"],
            *[page["request"] for page in self.contract["queryPages"]], self.contract["endOperation"]["request"]]
        self.assertEqual([canonical(value["task"]) for value in self.requests], list(map(canonical, expected)))
        self.assertEqual(self.events, ["takeover", "start", "query", "query", "endOperation", "release"])
        self.issue.assert_called_once_with(self.owned, self.contract["command"], self.operation)
        self.release.assert_called_once()

    def test_takeover_receipt_must_match_the_current_signed_fence_before_start(self):
        def transform(operation, response):
            if operation == "takeover":
                response["result"]["fence"]["controllerEpoch"] += 1
            return response
        self.transform = transform
        with self.assertRaises(WorkspaceError):
            self.execute()
        self.assertEqual(self.events, ["takeover", "endOperation", "release"])

    def test_replayed_core_start_reads_the_same_task_without_duplicate_start_or_output(self):
        self.start_case = "replayedStart"
        self.assertEqual(self.execute(), {"returnCode": 0, "stdout": "ok", "stderr": ""})
        starts = [value["task"] for value in self.requests
            if value["task"]["request"]["operation"] == "start"]
        self.assertEqual(starts, [self.contract["replayedStart"]["request"]])
        self.assertEqual([value["task"]["request"]["cursor"] for value in self.requests
            if value["task"]["request"]["operation"] == "query"], [0, 1])
        self.release.assert_called_once()

    def test_lost_start_reply_closes_original_operation_and_never_resends_start(self):
        self.lose_start = True
        with self.assertRaises(WorkspaceError):
            self.execute()
        self.assertEqual([value["task"] for value in self.requests],
            [self.contract["takeover"]["request"], self.contract["start"]["request"],
             self.contract["endOperation"]["request"]])
        self.assertEqual(self.events, ["takeover", "start", "endOperation", "release"])

    def test_unconfirmed_core_cleanup_does_not_release_maintenance_authority(self):
        def transform(operation, response):
            if operation == "endOperation":
                response["result"]["cleanupComplete"] = False
            return response
        self.transform = transform
        with self.assertRaisesRegex(WorkspaceError, "maintenance_cleanup_unconfirmed"):
            self.execute()
        self.release.assert_not_called()

    def test_malformed_or_foreign_start_receipts_fail_before_query_but_still_close_original_operation(self):
        for changed in ({"taskId": ""}, {"taskId": None}, {"state": []}, {"operationId": "1:foreign"},
                {"scope": {"kind": "operation", "operationId": "foreign-operation"}}):
            with self.subTest(changed=changed):
                self.requests.clear()
                self.events.clear()
                def transform(operation, response):
                    if operation == "start":
                        response["result"]["task"].update(changed)
                    return response
                self.transform = transform
                with self.assertRaises(WorkspaceError):
                    self.execute()
                self.assertEqual(self.events, ["takeover", "start", "endOperation", "release"])

    def test_invalid_query_cursor_types_or_foreign_receipts_are_never_consumed(self):
        mutations = [
            lambda result: result["task"].update(state=[]),
            lambda result: result["output"][0].update(stream=[]),
            lambda result: result.update(unrecognizedField=True),
            lambda result: result.pop("gap"),
            lambda result: result["task"].update(unrecognizedField=True),
            lambda result: result["output"][0].update(unrecognizedField=True),
            lambda result: result.update(nextCursor=True),
            lambda result: result.update(nextCursor=-1),
            lambda result: result.update(nextCursor="1"),
            lambda result: result.update(nextCursor=2**64),
            lambda result: result.update(earliestCursor="0"),
            lambda result: result.update(hasMore="false"),
            lambda result: result["output"][0].update(cursor=True),
            lambda result: result["task"].update(taskId="foreign-task"),
            lambda result: result["task"].update(operationId="1:foreign"),
            lambda result: result["task"].update(scope={"kind": "operation", "operationId": "foreign"}),
        ]
        for index, mutate in enumerate(mutations):
            with self.subTest(index=index):
                self.requests.clear()
                self.events.clear()
                def transform(operation, response):
                    if operation == "query":
                        mutate(response["result"])
                    return response
                self.transform = transform
                with self.assertRaises(WorkspaceError):
                    self.execute()
                self.assertEqual(self.events, ["takeover", "start", "query", "endOperation", "release"])
