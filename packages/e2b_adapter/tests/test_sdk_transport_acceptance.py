"""Exercise the pinned E2B SDK over real loopback HTTP, without provider access.

These tests establish adapter/SDK transport behavior. They do not run an E2B
control plane, Firecracker, the Core harness, or a physical cleanup acceptance.
"""

from contextlib import contextmanager
import base64
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import importlib.metadata
import json
import os
from pathlib import Path
import socket
import sys
import threading
import unittest
from unittest.mock import patch


sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from centaeris_e2b_adapter.bridge import Adapter, AdapterConfig
from centaeris_e2b_adapter.control import EXPECTED_METADATA_KEYS


def expected_metadata():
    return {key: "original-" + key for key in EXPECTED_METADATA_KEYS}


class ProviderFixture:
    def __init__(self, mode="success"):
        self.mode = mode
        self.requests = []
        self.metadata = {}
        self.lock = threading.Lock()
        self.command_started = threading.Event()
        self.command_release = threading.Event()

    def record(self, method, path, headers, body):
        with self.lock:
            self.requests.append({
                "method": method,
                "path": path,
                "headers": {key.lower(): value for key, value in headers.items()},
                "body": body,
            })

    def sandbox(self):
        return {
            "sandboxID": "sandbox-fixture",
            "templateID": "template-fixture",
            "clientID": "client-fixture",
            "envdVersion": "0.3.0",
            "domain": "fixture.invalid",
            "envdAccessToken": "fixture-envd-access-token",
        }

    def detail(self):
        return {
            **self.sandbox(),
            "startedAt": "2026-10-22T00:00:00Z",
            "endAt": "2026-10-22T00:05:00Z",
            "cpuCount": 1,
            "memoryMB": 512,
            "diskSizeMB": 1024,
            "state": "running",
            "metadata": self.metadata,
        }


@contextmanager
def provider_fixture(mode="success"):
    fixture = ProviderFixture(mode)

    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def log_message(self, *_args):
            pass

        def reply(self, status, body):
            wire = json.dumps(body).encode("utf-8")
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(wire)))
            self.send_header("Connection", "close")
            self.end_headers()
            self.wfile.write(wire)
            self.close_connection = True

        def do_POST(self):
            size = int(self.headers.get("Content-Length", "0"))
            if self.headers.get("Transfer-Encoding", "").lower() == "chunked":
                raw = bytearray()
                while True:
                    chunk_size = int(self.rfile.readline().split(b";", 1)[0], 16)
                    if chunk_size == 0:
                        self.rfile.readline()
                        break
                    raw.extend(self.rfile.read(chunk_size))
                    self.rfile.read(2)
                raw = bytes(raw)
            else:
                raw = self.rfile.read(size)
            if self.path in ("/process.Process/Start", "/process.Process/Connect"):
                body = json.loads(raw[5:5 + int.from_bytes(raw[1:5], "big")])
                fixture.record("POST", self.path, self.headers, body)
                events = [{"event": {"start": {"pid": 71}}}]
                if self.path.endswith("/Start"):
                    if fixture.mode.startswith("commandRaw"):
                        for stream, content in (("stdout", b"\x00\xffA\xc3"), ("stderr", b"\xfe\x00B"), ("stdout", b"\xa9\x80")):
                            events.append({"event": {"data": {stream: base64.b64encode(content).decode()}}})
                    else:
                        events.append({"event": {"data": {"stdout": base64.b64encode(b"centaeris-adapter-ok\n").decode()}}})
                # A new connection observes completion without replaying output.
                if fixture.mode not in ("commandRawMissingEnd", "commandRawDisconnect"):
                    exit_code = 7 if fixture.mode in ("commandExit", "commandRawExit") else 0
                    completion = {"exitCode": exit_code, "exited": fixture.mode != "commandRawNotExited", "status": "exited"}
                    if exit_code:
                        completion["error"] = "fixture-private-diagnostic: exit status 7"
                    events.append({"event": {"end": completion}})
                chunks = [json.dumps(event).encode() for event in events]
                wire = b"".join(b"\x00" + len(chunk).to_bytes(4, "big") + chunk for chunk in chunks)
                wire += b"\x02\x00\x00\x00\x02{}"
                self.send_response(200)
                self.send_header("Content-Type", "application/connect+json")
                self.send_header("Content-Length", str(len(wire) + (100 if fixture.mode == "commandRawDisconnect" else 0)))
                self.send_header("Connection", "close")
                self.end_headers()
                if fixture.mode == "blockedOrdinary" and body["process"]["cmd"] == "/bin/bash":
                    first_size = 5 + len(chunks[0])
                    self.wfile.write(wire[:first_size])
                    self.wfile.flush()
                    fixture.command_started.set()
                    fixture.command_release.wait(10)
                    self.wfile.write(wire[first_size:])
                else:
                    self.wfile.write(wire)
                self.close_connection = True
                return
            body = json.loads(raw) if size else None
            fixture.record("POST", self.path, self.headers, body)
            if self.path in ("/v2/sandboxes/sandbox-fixture/connect", "/sandboxes/sandbox-fixture/pause"):
                action = "Connect" if self.path.endswith("/connect") else "Pause"
                if fixture.mode == "lost" + action + "Reply":
                    self.connection.shutdown(socket.SHUT_RDWR)
                    self.connection.close()
                    self.close_connection = True
                    return
                if fixture.mode in ("refused" + action + "503", "refused" + action + "429"):
                    status = 503 if fixture.mode.endswith("503") else 429
                    self.reply(status, {"code": status, "message": "fixture-private-provider-diagnostic"})
                    return
                if action == "Pause":
                    if fixture.mode == "alreadyPaused":
                        self.reply(409, {"code": 409, "message": "already paused"})
                    else:
                        self.reply(204, {})
                else:
                    self.reply(200, fixture.sandbox())
                return
            if self.path != "/v2/sandboxes":
                self.reply(404, {"code": 404, "message": "fixture_route_missing"})
                return
            fixture.metadata = body.get("metadata", {})
            if fixture.mode == "lostCreateReply":
                # The fixture accepted the request and discards the response.
                # This does not prove a physical sandbox was ever created.
                self.connection.shutdown(socket.SHUT_RDWR)
                self.connection.close()
                self.close_connection = True
                return
            if fixture.mode in ("refused503", "refused429", "refusedSecret"):
                status = 503 if fixture.mode == "refused503" else 429
                message = ("fixture-private-token " + self.headers.get("X-API-Key", "")
                           if fixture.mode == "refusedSecret" else "fixture_refusal")
                self.reply(status, {"code": status, "message": message})
                return
            self.reply(201, fixture.sandbox())

        def do_GET(self):
            fixture.record("GET", self.path, self.headers, None)
            if fixture.mode == "fileData" and self.path.startswith("/files?"):
                content = b"\x00\xffself-hosted-data"
                self.send_response(200)
                self.send_header("Content-Type", "application/octet-stream")
                self.send_header("Content-Length", str(len(content)))
                self.send_header("Connection", "close")
                self.end_headers()
                self.wfile.write(content)
                self.close_connection = True
                return
            if self.path != "/sandboxes/sandbox-fixture":
                self.reply(404, {"code": 404, "message": "fixture_route_missing"})
                return
            if fixture.mode == "lostInspectionReply":
                self.connection.shutdown(socket.SHUT_RDWR)
                self.connection.close()
                self.close_connection = True
                return
            self.reply(200, fixture.detail())

        def do_DELETE(self):
            fixture.record("DELETE", self.path, self.headers, None)
            if self.path != "/sandboxes/sandbox-fixture":
                self.reply(404, {"code": 404, "message": "fixture_route_missing"})
                return
            if fixture.mode == "alreadyAbsent":
                self.reply(404, {"code": 404, "message": "fixture_already_absent"})
                return
            self.reply(204, {})

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    server.daemon_threads = True
    thread = threading.Thread(
        target=lambda: server.serve_forever(poll_interval=0.01), daemon=True
    )
    thread.start()
    fixture.api_url = "http://127.0.0.1:" + str(server.server_address[1])
    try:
        yield fixture
    finally:
        fixture.command_release.set()
        server.shutdown()
        server.server_close()
        thread.join(timeout=2)


def create_request(attempt="attempt-fixture", key="request-key-fixture"):
    return {
        "schema": "runtime.e2b.adapter.request.v1",
        "requestId": "rpc-" + attempt,
        "operation": "create",
        "payload": {
            "attemptId": attempt,
            "idempotencyKey": key,
            "templateRef": {"templateId": "template-fixture", "buildId": "123e4567-e89b-12d3-a456-426614174000"},
            "metadata": {"environmentId": "environment-fixture"},
            "timeoutSeconds": 120,
            "allowInternetAccess": False,
            "onTimeout": "kill",
        },
    }


def adapter_for(fixture, key="e2b_" + "a" * 64, lane="ordinary", idempotency_supported=True):
    return Adapter(AdapterConfig(
        api_key=key,
        domain="fixture.invalid",
        api_url=fixture.api_url,
        sandbox_url=fixture.api_url,
        idempotency_supported=idempotency_supported,
        lane=lane,
    ))


def lifecycle_request(operation, **extra):
    return {
        "schema": "runtime.e2b.adapter.request.v1",
        "requestId": "rpc-" + operation,
        "operation": operation,
        "payload": {"sandboxId": "sandbox-fixture", **extra},
    }


class SdkTransportAcceptanceTests(unittest.TestCase):
    def test_started_command_keeps_its_output_when_a_new_stream_does_not_replay(self):
        for mode, exit_code in (("success", 0), ("commandExit", 7)):
            with self.subTest(exit_code=exit_code), provider_fixture(mode) as fixture:
                adapter = adapter_for(fixture)
                self.assertEqual(adapter.handle(create_request())["outcome"], "succeeded")
                started = adapter.handle({"schema": "runtime.e2b.adapter.request.v1", "requestId": "start",
                    "operation": "runCommand", "payload": {"sandboxId": "sandbox-fixture", "command": "echo centaeris-adapter-ok", "timeoutSeconds": 10}})
                self.assertEqual(started["outcome"], "succeeded", started)
                completed = adapter.handle({"schema": "runtime.e2b.adapter.request.v1", "requestId": "wait",
                    "operation": "waitCommand", "payload": {"sandboxId": "sandbox-fixture", "pid": started["result"]["pid"], "timeoutSeconds": 10}})
                self.assertEqual(completed["outcome"], "succeeded", completed)
                self.assertEqual(completed["result"]["stdout"], "centaeris-adapter-ok\n")
                self.assertEqual(completed["result"]["exitCode"], exit_code)
                self.assertEqual(sum(entry["path"] == "/process.Process/Start" for entry in fixture.requests), 1)
                self.assertEqual(sum(entry["path"] == "/process.Process/Connect" for entry in fixture.requests), 0)

    def test_command_output_preserves_raw_bytes_across_chunks_and_streams(self):
        for mode, exit_code in (("commandRaw", 0), ("commandRawExit", 7)):
            with self.subTest(mode=mode), provider_fixture(mode) as fixture:
                adapter = adapter_for(fixture)
                self.assertEqual(adapter.handle(create_request())["outcome"], "succeeded")
                started = adapter.handle(lifecycle_request("runCommand", command="printf raw", timeoutSeconds=10, user="user", cwd="/workspace", envs={"LANG": "C"}))
                self.assertEqual(started["outcome"], "succeeded", started)
                completed = adapter.handle(lifecycle_request("waitCommand", pid=started["result"]["pid"], timeoutSeconds=10, maxOutputBytes=9))
                self.assertEqual(completed["outcome"], "succeeded", completed)
                result = completed["result"]
                self.assertEqual(base64.b64decode(result["stdoutBytesBase64"]), b"\x00\xffA\xc3\xa9\x80")
                self.assertEqual(base64.b64decode(result["stderrBytesBase64"]), b"\xfe\x00B")
                self.assertEqual((result["stdoutByteLength"], result["stderrByteLength"]), (6, 3))
                self.assertEqual(result["stdout"], "\x00\ufffdA\xe9\ufffd")
                self.assertEqual(result["stderr"], "\ufffd\x00B")
                self.assertEqual(result["exitCode"], exit_code)
                self.assertNotIn("fixture-private-diagnostic", json.dumps(completed))
                replay = adapter.handle(lifecycle_request("recoverCommand", pid=started["result"]["pid"], timeoutSeconds=10, maxOutputBytes=9))
                self.assertEqual(replay["result"], result)
                self.assertEqual([(entry["method"], entry["path"]) for entry in fixture.requests],
                                 [("POST", "/v2/sandboxes"), ("GET", "/sandboxes/sandbox-fixture"),
                                  ("POST", "/process.Process/Start")])
                process_calls = [entry for entry in fixture.requests if entry["path"].startswith("/process.Process/")]
                self.assertEqual([entry["path"] for entry in process_calls], ["/process.Process/Start"])
                self.assertEqual(process_calls[0]["body"]["process"], {"cmd": "/bin/bash", "args": ["-l", "-c", "printf raw"], "envs": {"LANG": "C"}, "cwd": "/workspace"})
                self.assertFalse(process_calls[0]["body"].get("stdin", False))
                self.assertNotIn("x-api-key", process_calls[0]["headers"])
                self.assertEqual(process_calls[0]["headers"]["x-access-token"], "fixture-envd-access-token")
                self.assertEqual(process_calls[0]["headers"]["e2b-sandbox-id"], "sandbox-fixture")
                self.assertEqual(process_calls[0]["headers"]["authorization"], "Basic dXNlcjo=")
                self.assertNotIn("fixture-envd-access-token", json.dumps(completed))

    def test_command_output_without_a_complete_end_event_is_unknown(self):
        for mode in ("commandRawMissingEnd", "commandRawDisconnect", "commandRawNotExited"):
            with self.subTest(mode=mode), provider_fixture(mode) as fixture:
                adapter = adapter_for(fixture)
                self.assertEqual(adapter.handle(create_request())["outcome"], "succeeded")
                started = adapter.handle(lifecycle_request("runCommand", command="printf raw", timeoutSeconds=10))
                self.assertEqual(started["outcome"], "succeeded", started)
                completed = adapter.handle(lifecycle_request("waitCommand", pid=71, timeoutSeconds=10))
                self.assertEqual(completed["outcome"], "unknown", completed)
                self.assertEqual(completed["result"]["pid"], 71)
                self.assertNotIn("exitCode", completed["result"])
                retry = adapter.handle(lifecycle_request("recoverCommand", pid=71, timeoutSeconds=10))
                self.assertEqual(retry["error"]["code"], "commandOutputUnavailable")
                self.assertEqual(sum(entry["path"] == "/process.Process/Start" for entry in fixture.requests), 1)
                self.assertEqual(sum(entry["path"] == "/process.Process/Connect" for entry in fixture.requests), 0)

    def test_raw_stdout_and_stderr_share_the_original_byte_budget(self):
        with provider_fixture("commandRaw") as fixture:
            adapter = adapter_for(fixture)
            self.assertEqual(adapter.handle(create_request())["outcome"], "succeeded")
            started = adapter.handle(lifecycle_request("runCommand", command="printf raw", timeoutSeconds=10))
            self.assertEqual(started["outcome"], "succeeded", started)
            completed = adapter.handle(lifecycle_request("waitCommand", pid=71, timeoutSeconds=10, maxOutputBytes=8))
            self.assertEqual(completed["outcome"], "unknown", completed)
            self.assertEqual(completed["error"]["code"], "outputLimitExceeded")
            self.assertEqual(completed["result"]["pid"], 71)
            retry = adapter.handle(lifecycle_request("recoverCommand", pid=71, timeoutSeconds=10))
            self.assertEqual(retry["error"]["code"], "commandOutputUnavailable")
            self.assertEqual(sum(entry["path"] == "/process.Process/Start" for entry in fixture.requests), 1)

    def test_self_hosted_create_and_connect_keep_binary_traffic_on_explicit_sandbox_url(self):
        with provider_fixture() as control, provider_fixture("fileData") as data, provider_fixture() as ambient:
            with patch.dict(os.environ, {"E2B_API_URL": ambient.api_url, "E2B_SANDBOX_URL": ambient.api_url, "E2B_DOMAIN": ""}):
                adapter = Adapter(AdapterConfig(api_key="e2b_" + "a" * 64, api_url=control.api_url,
                    sandbox_url=data.api_url, idempotency_supported=True))
                create = create_request()
                create["payload"]["metadata"] = expected_metadata() | {"centaerisAttemptId": "attempt-fixture"}
                created = adapter.handle(create)
                self.assertEqual(created["outcome"], "succeeded", created)
                read = {"schema": "runtime.e2b.adapter.request.v1", "requestId": "read-created",
                    "operation": "readFile", "payload": {"sandboxId": "sandbox-fixture", "path": "/workspace/binary"}}
                first = adapter.handle(read)
                self.assertEqual(first["outcome"], "succeeded", first)
                self.assertEqual(base64.b64decode(first["result"]["dataBase64"]), b"\x00\xffself-hosted-data")
                reconnected = adapter.handle(lifecycle_request("connect", timeoutSeconds=120, onResume="restore",
                    expectedTemplateId="template-fixture", expectedMetadata=control.metadata))
                self.assertEqual(reconnected["outcome"], "succeeded", reconnected)
                second = adapter.handle(read | {"requestId": "read-connected"})
                self.assertEqual(second["outcome"], "succeeded", second)
                self.assertEqual(base64.b64decode(second["result"]["dataBase64"]), b"\x00\xffself-hosted-data")
        self.assertEqual(ambient.requests, [])
        self.assertEqual([entry["method"] for entry in control.requests], ["POST", "GET", "GET", "POST", "GET"])
        self.assertEqual(len(data.requests), 2)
        self.assertTrue(all(entry["path"].startswith("/files?") for entry in data.requests))
        self.assertTrue(all("x-api-key" not in entry["headers"] for entry in data.requests))

    def test_pinned_sdk_creates_and_independently_inspects_the_bound_sandbox(self):
        self.assertEqual(importlib.metadata.version("e2b"), "2.58.0")
        with provider_fixture() as fixture:
            response = adapter_for(fixture).handle(create_request())
        self.assertEqual(response["outcome"], "succeeded", response)
        self.assertEqual(response["result"]["sandboxId"], "sandbox-fixture")
        self.assertEqual(response["result"]["attemptId"], "attempt-fixture")
        self.assertEqual(
            [(item["method"], item["path"]) for item in fixture.requests],
            [("POST", "/v2/sandboxes"), ("GET", "/sandboxes/sandbox-fixture")],
        )
        creation = fixture.requests[0]
        self.assertEqual(creation["body"]["templateID"], "template-fixture:123e4567-e89b-12d3-a456-426614174000")
        self.assertEqual(creation["body"]["timeout"], 120)
        # The provider's generated API retains this spelling; our adapter
        # request above uses the public camelCase contract.
        self.assertIs(creation["body"]["allow_internet_access"], False)
        self.assertIs(creation["body"]["autoPause"], False)
        self.assertEqual(creation["body"]["autoResume"], {"enabled": False})
        self.assertEqual(creation["body"]["metadata"], {
            "environmentId": "environment-fixture", "centaerisAttemptId": "attempt-fixture",
        })
        self.assertEqual(creation["headers"]["idempotency-key"], "request-key-fixture")
        self.assertEqual(creation["headers"]["x-api-key"], "e2b_" + "a" * 64)

    def test_invalid_provider_idempotency_keys_fail_before_any_http_request(self):
        keys = ("a" * 256, "Uppercase", "under_score", "dot.key", "colon:key", "has space", "slash/key", "line\nbreak", "\u00e9", "")
        for configured in (False, True):
            for key in keys:
                with self.subTest(configured=configured, key=key), provider_fixture() as fixture:
                    response = adapter_for(fixture, idempotency_supported=configured).handle(create_request(key=key))
                    self.assertEqual(response["outcome"], "notDispatched", response)
                    self.assertEqual(response["error"]["code"], "invalidRequest")
                    self.assertEqual(fixture.requests, [])

    def test_provider_idempotency_key_boundaries_keep_the_exact_header(self):
        for key in ("-", "a" * 255):
            with self.subTest(length=len(key)), provider_fixture() as fixture:
                response = adapter_for(fixture, idempotency_supported=False).handle(create_request(key=key))
                self.assertEqual(response["outcome"], "succeeded", response)
                self.assertEqual([(item["method"], item["path"]) for item in fixture.requests],
                                 [("POST", "/v2/sandboxes"), ("GET", "/sandboxes/sandbox-fixture")])
                self.assertEqual(fixture.requests[0]["headers"]["idempotency-key"], key)

    def test_unverified_idempotency_create_sends_one_request_with_bound_identity(self):
        with provider_fixture() as fixture:
            response = adapter_for(fixture, idempotency_supported=False).handle(create_request())
        self.assertEqual(response["outcome"], "succeeded", response)
        self.assertEqual(response["result"]["sandboxId"], "sandbox-fixture")
        self.assertEqual(response["result"]["attemptId"], "attempt-fixture")
        self.assertEqual([(item["method"], item["path"]) for item in fixture.requests],
                         [("POST", "/v2/sandboxes"), ("GET", "/sandboxes/sandbox-fixture")])
        creation = fixture.requests[0]
        self.assertEqual(creation["body"]["templateID"], "template-fixture:123e4567-e89b-12d3-a456-426614174000")
        self.assertEqual(creation["body"]["metadata"]["centaerisAttemptId"], "attempt-fixture")
        self.assertEqual(creation["headers"]["idempotency-key"], "request-key-fixture")

    def test_disconnected_creation_response_is_unknown_and_never_retried(self):
        for configured in (False, True):
            with self.subTest(configured=configured), provider_fixture("lostCreateReply") as fixture:
                response = adapter_for(fixture, idempotency_supported=configured).handle(create_request())
            self.assertEqual(response["outcome"], "unknown", response)
            self.assertEqual(response["error"]["code"], "providerRequestFailed")
            self.assertEqual(response["result"]["attemptId"], "attempt-fixture")
            self.assertEqual([(item["method"], item["path"]) for item in fixture.requests],
                             [("POST", "/v2/sandboxes")])

    def test_provider_refusal_does_not_retry_or_become_non_dispatch_evidence(self):
        for configured in (False, True):
            for mode in ("refused503", "refused429"):
                with self.subTest(mode=mode, configured=configured), provider_fixture(mode) as fixture:
                    response = adapter_for(fixture, idempotency_supported=configured).handle(create_request())
                self.assertEqual(response["outcome"], "unknown", response)
                self.assertEqual(response["result"]["attemptId"], "attempt-fixture")
                self.assertEqual([(item["method"], item["path"]) for item in fixture.requests],
                                 [("POST", "/v2/sandboxes")])

    def test_control_and_cleanup_lanes_cannot_dispatch_create(self):
        for configured in (False, True):
            for lane in ("control", "cleanup"):
                with self.subTest(configured=configured, lane=lane), provider_fixture() as fixture:
                    response = adapter_for(fixture, lane=lane, idempotency_supported=configured).handle(create_request())
                self.assertEqual(response["outcome"], "notDispatched", response)
                self.assertEqual(response["error"]["code"], "laneForbidden")
                self.assertEqual(fixture.requests, [])

    def test_lost_inspection_response_preserves_the_known_sandbox_identity(self):
        for configured in (False, True):
            with self.subTest(configured=configured), provider_fixture("lostInspectionReply") as fixture:
                response = adapter_for(fixture, idempotency_supported=configured).handle(create_request())
            self.assertEqual(response["outcome"], "unknown", response)
            self.assertEqual(response["result"]["sandboxId"], "sandbox-fixture")
            self.assertEqual(response["result"]["attemptId"], "attempt-fixture")
            self.assertEqual([(item["method"], item["path"]) for item in fixture.requests],
                             [("POST", "/v2/sandboxes"), ("GET", "/sandboxes/sandbox-fixture")])

    def test_explicit_clients_keep_account_headers_and_attempts_isolated(self):
        with provider_fixture() as first, provider_fixture() as second:
            first_adapter = adapter_for(first, "e2b_" + "a" * 64)
            second_adapter = adapter_for(second, "e2b_" + "b" * 64)
            first_response = first_adapter.handle(create_request("attempt-a", "key-a"))
            second_response = second_adapter.handle(create_request("attempt-b", "key-b"))
        self.assertEqual(first_response["outcome"], "succeeded", first_response)
        self.assertEqual(second_response["outcome"], "succeeded", second_response)
        for fixture, letter in ((first, "a"), (second, "b")):
            self.assertEqual(len(fixture.requests), 2)
            self.assertTrue(all(
                item["headers"]["x-api-key"] == "e2b_" + letter * 64
                for item in fixture.requests
            ))
            self.assertEqual(fixture.requests[0]["headers"]["idempotency-key"], "key-" + letter)
            self.assertEqual(fixture.metadata["centaerisAttemptId"], "attempt-" + letter)

    def test_connect_preserves_requested_identity_and_sdk_default_restore(self):
        with provider_fixture() as fixture:
            fixture.metadata = expected_metadata()
            response = adapter_for(fixture).handle(lifecycle_request(
                "connect", timeoutSeconds=120, onResume="restore",
                expectedTemplateId="template-fixture", expectedMetadata=fixture.metadata,
            ))
        self.assertEqual(response["outcome"], "succeeded", response)
        self.assertEqual(response["result"]["sandboxId"], "sandbox-fixture")
        self.assertEqual(
            [(item["method"], item["path"]) for item in fixture.requests],
            [("GET", "/sandboxes/sandbox-fixture"),
             ("POST", "/v2/sandboxes/sandbox-fixture/connect"),
             ("GET", "/sandboxes/sandbox-fixture")],
        )
        # SDK restore uses the provider default; memory=False requests a reboot.
        self.assertEqual(fixture.requests[1]["body"], {"timeout": 120})

    def test_pause_sends_the_explicit_memory_policy_without_claiming_a_snapshot(self):
        for mode, memory in (("full", True), ("filesystem", False)):
            with self.subTest(mode=mode), provider_fixture() as fixture:
                fixture.metadata = expected_metadata()
                response = adapter_for(fixture, lane="cleanup").handle(
                    lifecycle_request("pause", mode=mode, expectedTemplateId="template-fixture", expectedMetadata=fixture.metadata)
                )
            self.assertEqual(response["outcome"], "succeeded", response)
            self.assertEqual(response["result"], {
                "sandboxId": "sandbox-fixture", "requestedMode": mode, "changed": True,
            })
            self.assertEqual(len(fixture.requests), 2)
            self.assertEqual(fixture.requests[1]["path"], "/sandboxes/sandbox-fixture/pause")
            self.assertEqual(fixture.requests[1]["body"], {"memory": memory})

    def test_cleanup_targets_only_the_exact_id_and_handles_provider_absence(self):
        for mode, killed in (("success", True), ("alreadyAbsent", False)):
            with self.subTest(mode=mode), provider_fixture(mode) as fixture:
                response = adapter_for(fixture, lane="cleanup").handle(lifecycle_request("kill"))
            self.assertEqual(response["outcome"], "succeeded", response)
            self.assertEqual(response["result"], {
                "sandboxId": "sandbox-fixture", "deleteAcknowledged": True, "providerReportedKilled": killed,
            })
            self.assertEqual(
                [(item["method"], item["path"]) for item in fixture.requests],
                [("DELETE", "/sandboxes/sandbox-fixture")],
            )

    def test_provider_error_body_and_credentials_do_not_escape_the_adapter_response(self):
        with provider_fixture("refusedSecret") as fixture:
            response = adapter_for(fixture).handle(create_request())
        self.assertEqual(response["outcome"], "unknown", response)
        wire = json.dumps(response)
        self.assertNotIn("fixture-private-token", wire)
        self.assertNotIn("e2b_" + "a" * 64, wire)
        self.assertNotIn(fixture.api_url, wire)


if __name__ == "__main__":
    unittest.main()
