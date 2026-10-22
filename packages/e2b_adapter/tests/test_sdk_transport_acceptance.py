"""Exercise the pinned E2B SDK over real loopback HTTP, without provider access.

These tests establish adapter/SDK transport behavior. They do not run an E2B
control plane, Firecracker, the Core harness, or a physical cleanup acceptance.
"""

from contextlib import contextmanager
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import importlib.metadata
import json
from pathlib import Path
import socket
import sys
import threading
import unittest


sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from centaeris_e2b_adapter.bridge import Adapter, AdapterConfig


class ProviderFixture:
    def __init__(self, mode="success"):
        self.mode = mode
        self.requests = []
        self.metadata = {}
        self.lock = threading.Lock()

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
            body = json.loads(self.rfile.read(size)) if size else None
            fixture.record("POST", self.path, self.headers, body)
            if self.path == "/v2/sandboxes/sandbox-fixture/connect":
                self.reply(200, fixture.sandbox())
                return
            if self.path == "/sandboxes/sandbox-fixture/pause":
                self.reply(204, {})
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


def adapter_for(fixture, key="e2b_" + "a" * 64, lane="ordinary"):
    return Adapter(AdapterConfig(
        api_key=key,
        domain="fixture.invalid",
        api_url=fixture.api_url,
        idempotency_supported=True,
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
        self.assertEqual(creation["body"]["metadata"], {
            "environmentId": "environment-fixture", "centaerisAttemptId": "attempt-fixture",
        })
        self.assertEqual(creation["headers"]["idempotency-key"], "request-key-fixture")
        self.assertEqual(creation["headers"]["x-api-key"], "e2b_" + "a" * 64)

    def test_disconnected_creation_response_is_unknown_and_never_retried(self):
        with provider_fixture("lostCreateReply") as fixture:
            response = adapter_for(fixture).handle(create_request())
        self.assertEqual(response["outcome"], "unknown", response)
        self.assertEqual(response["error"]["code"], "providerRequestFailed")
        self.assertEqual(
            [(item["method"], item["path"]) for item in fixture.requests],
            [("POST", "/v2/sandboxes")],
        )

    def test_provider_refusal_does_not_retry_or_become_non_dispatch_evidence(self):
        for mode in ("refused503", "refused429"):
            with self.subTest(mode=mode), provider_fixture(mode) as fixture:
                response = adapter_for(fixture).handle(create_request())
            self.assertEqual(response["outcome"], "unknown", response)
            self.assertEqual(len(fixture.requests), 1)
            self.assertEqual(fixture.requests[0]["method"], "POST")

    def test_lost_inspection_response_preserves_the_known_sandbox_identity(self):
        with provider_fixture("lostInspectionReply") as fixture:
            response = adapter_for(fixture).handle(create_request())
        self.assertEqual(response["outcome"], "unknown", response)
        self.assertEqual(response["result"]["sandboxId"], "sandbox-fixture")
        self.assertEqual(response["result"]["attemptId"], "attempt-fixture")
        self.assertEqual(len(fixture.requests), 2)

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
            response = adapter_for(fixture).handle(lifecycle_request(
                "connect", timeoutSeconds=120, onResume="restore",
            ))
        self.assertEqual(response["outcome"], "succeeded", response)
        self.assertEqual(response["result"]["sandboxId"], "sandbox-fixture")
        self.assertEqual(
            [(item["method"], item["path"]) for item in fixture.requests],
            [("POST", "/v2/sandboxes/sandbox-fixture/connect"),
             ("GET", "/sandboxes/sandbox-fixture")],
        )
        # SDK restore uses the provider default; memory=False requests a reboot.
        self.assertEqual(fixture.requests[0]["body"], {"timeout": 120})

    def test_pause_sends_the_explicit_memory_policy_without_claiming_a_snapshot(self):
        for mode, memory in (("full", True), ("filesystem", False)):
            with self.subTest(mode=mode), provider_fixture() as fixture:
                response = adapter_for(fixture, lane="cleanup").handle(
                    lifecycle_request("pause", mode=mode)
                )
            self.assertEqual(response["outcome"], "succeeded", response)
            self.assertEqual(response["result"], {
                "sandboxId": "sandbox-fixture", "requestedMode": mode, "changed": True,
            })
            self.assertEqual(len(fixture.requests), 1)
            self.assertEqual(fixture.requests[0]["path"], "/sandboxes/sandbox-fixture/pause")
            self.assertEqual(fixture.requests[0]["body"], {"memory": memory})

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
