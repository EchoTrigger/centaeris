"""Pinned Core, production message provider, real Django and PostgreSQL; no model API."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
from contextlib import contextmanager
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from threading import Thread
from concurrent.futures import ThreadPoolExecutor
from threading import Event
from unittest.mock import patch

from django.conf import settings
from django.contrib.auth import get_user_model
from django.core.files.base import ContentFile
from django.core.files.storage import default_storage
from django.db import connection, transaction
from django.test import LiveServerTestCase, SimpleTestCase, override_settings

from .agent_messages import message_contract_digest, validate_agent_message
from .agent_run_authorization_factory import create_agent_run_authorization
from .models import Agent, AgentRun, ModelConfig, Session, SessionAssetLink, SessionEvent, UserLibraryObject, Workspace, WorkspaceMembership
from .testing import fenced_start_fixture
from .runtime_schema_test_helpers import isolate_runtime_schema


def _unallocated_receipt_query(raw, *, path, token, allowed_run_ids):
    """Closed request contract for the fixture's read-only receipt transport."""
    from .runtime_contract import require_opaque_ref, require_sha256
    if path != "/internal/agent-runs/cleanup-receipt":
        raise LookupError("fixture_cleanup_receipt_not_found")
    if token != settings.INTERNAL_API_TOKEN:
        raise PermissionError("fixture_cleanup_receipt_unauthorized")

    def unique_object(pairs):
        result = {}
        for name, value in pairs:
            if name in result:
                raise ValueError("fixture_cleanup_receipt_duplicate_field")
            result[name] = value
        return result

    body = json.loads(raw, object_pairs_hook=unique_object)
    if (not isinstance(body, dict)
            or set(body) != {"schema", "agentRunId", "authorizationDigest", "cleanupReceiptId"}
            or body["schema"] != "runtime.agent_run.cleanup_receipt.get.v1"):
        raise ValueError("fixture_cleanup_receipt_query_invalid")
    require_opaque_ref("agentRunId", body["agentRunId"])
    require_opaque_ref("cleanupReceiptId", body["cleanupReceiptId"])
    require_sha256("authorizationDigest", body["authorizationDigest"])
    if body["agentRunId"] not in allowed_run_ids:
        raise LookupError("fixture_cleanup_receipt_not_found")
    return body


def _read_unallocated_store_receipt(body):
    """Read the fact written by Rust Store; never create or repair any receipt."""
    if (connection.settings_dict["ENGINE"] != "django.db.backends.postgresql"
            or not connection.settings_dict["NAME"].startswith("test_")):
        raise RuntimeError("fixture_cleanup_receipt_database_invalid")
    with connection.cursor() as cursor:
        cursor.execute(
            "SELECT receipt_id,agent_run_id,environment_id,authorization_digest,"
            "controller_epoch,allocation_kind,instance_id,cleanup_complete,"
            "physical_removed,removed_container_id "
            "FROM runtime.agent_run_cleanup_receipts "
            "WHERE agent_run_id=%s AND authorization_digest=%s AND receipt_id=%s",
            [body["agentRunId"], body["authorizationDigest"], body["cleanupReceiptId"]],
        )
        row = cursor.fetchone()
    if row is None:
        raise LookupError("fixture_cleanup_receipt_not_found")
    if (row[5] != "neverAllocated" or row[6] is not None or row[7] is not True
            or row[8] is not False or row[9] is not None):
        raise RuntimeError("fixture_cleanup_receipt_allocation_invalid")
    return {
        "schema": "runtime.agent_run.cleanup_receipt.v1",
        "receiptId": row[0], "agentRunId": row[1], "environmentId": row[2],
        "authorizationDigest": row[3], "controllerEpoch": row[4],
        "allocation": {"kind": "neverAllocated"}, "cleanupComplete": row[7],
    }


@contextmanager
def unallocated_receipt_bridge(allowed_run_ids):
    """Replace only transport, preserving real Store writes and API release."""
    allowed_run_ids = frozenset(allowed_run_ids)
    requests = []

    class Handler(BaseHTTPRequestHandler):
        def do_POST(self):
            try:
                length = int(self.headers.get("Content-Length", "0"))
                if not 0 < length <= 4096:
                    raise ValueError("fixture_cleanup_receipt_size_invalid")
                query = _unallocated_receipt_query(
                    self.rfile.read(length), path=self.path,
                    token=self.headers.get("X-Internal-Token"),
                    allowed_run_ids=allowed_run_ids,
                )
                receipt = _read_unallocated_store_receipt(query)
                requests.append(query)
                status, payload = 200, receipt
            except PermissionError:
                status, payload = 403, {"error": "fixture_cleanup_receipt_unauthorized"}
            except LookupError:
                status, payload = 404, {"error": "fixture_cleanup_receipt_not_found"}
            except (ValueError, TypeError, UnicodeError):
                status, payload = 400, {"error": "fixture_cleanup_receipt_query_invalid"}
            except Exception:
                status, payload = 503, {"error": "fixture_cleanup_receipt_unavailable"}
            finally:
                connection.close()
            encoded = json.dumps(payload).encode("utf-8")
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(encoded)))
            self.send_header("Connection", "close")
            self.end_headers()
            self.wfile.write(encoded)

        def log_message(self, *args):
            pass

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    server.daemon_threads = True
    thread = Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        with override_settings(RUNTIME_URL=f"http://127.0.0.1:{server.server_port}"), \
                patch.dict(os.environ, {"NO_PROXY": "127.0.0.1,localhost,::1",
                                        "no_proxy": "127.0.0.1,localhost,::1"}):
            yield requests
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=2)
        assert not thread.is_alive(), "fixture receipt transport did not stop"


class HostedLifecycleReceiptQueryTests(SimpleTestCase):
    def query(self, body, **changes):
        options = {"path": "/internal/agent-runs/cleanup-receipt",
                   "token": "fixture-internal-token", "allowed_run_ids": {"run-1"}} | changes
        with override_settings(INTERNAL_API_TOKEN="fixture-internal-token"):
            return _unallocated_receipt_query(body, **options)

    def test_closed_receipt_query_preserves_exact_requested_identities(self):
        body = {"schema": "runtime.agent_run.cleanup_receipt.get.v1", "agentRunId": "run-1",
                "authorizationDigest": "sha256:" + "a" * 64, "cleanupReceiptId": "receipt-1"}
        self.assertEqual(self.query(json.dumps(body)), body)
        for change in ({"extra": True}, {"schema": "unsupported"}, {"agentRunId": True},
                       {"authorizationDigest": "bad"}, {"cleanupReceiptId": ""}):
            with self.subTest(change=change), self.assertRaises((ValueError, TypeError)):
                self.query(json.dumps(body | change))

    def test_foreign_identity_token_path_and_duplicate_fields_are_rejected(self):
        body = json.dumps({"schema": "runtime.agent_run.cleanup_receipt.get.v1",
                           "agentRunId": "run-1", "authorizationDigest": "sha256:" + "a" * 64,
                           "cleanupReceiptId": "receipt-1"})
        with self.assertRaises(PermissionError):
            self.query(body, token="wrong")
        for change in ({"path": "/another"}, {"allowed_run_ids": {"other-run"}}):
            with self.subTest(change=change), self.assertRaises(LookupError):
                self.query(body, **change)
        duplicate = body[:-1] + ', "agentRunId": "run-1"}'
        with self.assertRaises(ValueError):
            self.query(duplicate)
        for raw in ("[]", "null", "invalid"):
            with self.subTest(raw=raw), self.assertRaises(ValueError):
                self.query(raw)


class AgentMessageRuntimeTests(LiveServerTestCase):
    serialized_rollback = True

    def _runtime_fixture(self):
        isolate_runtime_schema(self)
        storage = tempfile.TemporaryDirectory(prefix="agent-message-contract-")
        self.addCleanup(storage.cleanup)
        self.enterContext(override_settings(MEDIA_ROOT=storage.name))
        user = get_user_model().objects.create_user(username="runtime-message-owner")
        workspace = Workspace.objects.create(name="Runtime messages", createdBy=user)
        membership = WorkspaceMembership.objects.create(workspace=workspace, user=user, role="owner")
        agent = Agent.objects.create(workspace=workspace, owner=user, name="Private coordinator")
        work = Session.objects.create(workspace=workspace, owner=user, agent=agent)
        self.client.force_login(user)
        binding = self.client.post(f"/api/agents/{agent.id}/coordination-session", data="{}", content_type="application/json")
        self.assertEqual(binding.status_code, 201, binding.content)
        source = Session.objects.get(pk=binding.json()["sessionId"])
        content = b"Synthetic report bytes."
        key = default_storage.save("message-contract/report.txt", ContentFile(content))
        library = UserLibraryObject.objects.create(owner=user, displayName="report.txt", objectKind="file",
            contentType="text/plain", sizeBytes=len(content), sha256="sha256:" + hashlib.sha256(content).hexdigest(),
            contentGeneration=1, storageKey=key, status="ready")
        link = SessionAssetLink.objects.create(workspace=workspace, session=source, userLibraryObject=library,
            attachedBy=user, capturedDisplayName=library.displayName, capturedContentType=library.contentType,
            capturedOwnerKind="userLibraryObject", capturedOwnerId=library.id,
            capturedContentGeneration=1, capturedSizeBytes=library.sizeBytes, capturedSha256=library.sha256)
        model = ModelConfig.objects.create(displayName="Synthetic message model")
        run = AgentRun.objects.create(workspace=workspace, session=source, user=user, modelConfig=model,
            prompt="Send, inspect, send, then Final.", status="running")
        create_agent_run_authorization(run, image_digest="sha256:" + "a" * 64)
        database = connection.settings_dict
        self.assertEqual(database["ENGINE"], "django.db.backends.postgresql")
        self.assertTrue(database["NAME"].startswith("test_"), "requires an isolated Django test database")
        fixture = {"liveServerUrl": self.live_server_url, "agentRunStart": fenced_start_fixture(run),
            "internalToken": settings.INTERNAL_API_TOKEN, "cookieName": settings.SESSION_COOKIE_NAME,
            "cookieValue": self.client.cookies[settings.SESSION_COOKIE_NAME].value,
            "workSessionId": work.id, "fileRef": link.id, "libraryObjectId": library.id,
            "membershipId": membership.id, "messageContractDigest": message_contract_digest(),
            "firstBody": "  中文 🔎\n" + "complete evidence\n" * 3000 + "Tail  ",
            "secondBody": "Second complete message.\n",
            "database": {key: database[key] for key in ("NAME", "USER", "PASSWORD", "HOST", "PORT")}}
        return fixture, run, source, work, membership

    def _invoke_runtime(self, fixture, test_name, receipt):
        root = Path(__file__).resolve().parents[3]
        with unallocated_receipt_bridge({fixture["agentRunStart"]["agentRunId"]}) as receipt_reads:
            result = subprocess.run(["cargo", "test", "--locked", "-p", "runtime_server",
                "agent_messages::tests::runtime::" + test_name, "--", "--ignored", "--exact", "--nocapture"],
                cwd=root, env={**os.environ, "CARGO_BUILD_JOBS": "1", "CENTAERIS_AGENT_MESSAGE_FIXTURE": json.dumps(fixture),
                               "AGENT_RUN_AUTHORIZATION_SIGNING_KEY": settings.AGENT_RUN_AUTHORIZATION_SIGNING_KEY,
                               "NO_PROXY": "127.0.0.1,localhost,::1", "no_proxy": "127.0.0.1,localhost,::1"},
                capture_output=True, text=True, encoding="utf-8", timeout=300)
        output = result.stdout + result.stderr
        self.assertEqual(result.returncode, 0, output)
        self.assertIn("running 1 test", output)
        self.assertIn("1 passed; 0 failed; 0 ignored", output)
        self.assertIn(receipt, output)
        self.assertEqual([row["agentRunId"] for row in receipt_reads],
                         [fixture["agentRunStart"]["agentRunId"]])

    def test_real_message_provider_commits_continue_turn_and_rechecks_revoked_authority(self):
        fixture, run, source, work, membership = self._runtime_fixture()
        self._invoke_runtime(fixture, "django_message_runtime_contract", "agent-message-django-contract-ok")
        events = SessionEvent.objects.filter(session=source)
        self.assertEqual(events.filter(payload__type="tool_call").count(), 3)
        self.assertEqual(events.filter(payload__type="tool_result", payload__payload__resultState="successWithOutput").count(), 3)
        self.assertEqual(events.filter(payload__type="assistant_message").count(), 1)
        self.assertEqual(events.filter(payload__type="agent_run_completed").count(), 1)
        self.assertFalse(work.events.exists())
        self.assertEqual(self.client.get(f"/api/agents/{source.agent_id}/messages").status_code, 404,
                         "revoked current membership must deny the actual history route")
        print("agent-message-django-contract-ok: 2 committed bubbles; 3 calls; 1 Final; real authority revocation")

    def test_membership_revoked_after_validation_before_commit_characterizes_inflight_message(self):
        fixture, run, source, work, membership = self._runtime_fixture()
        validated, release_response = Event(), Event()
        validated_inputs = []

        def hold_successful_validation(body):
            record = validate_agent_message(body)
            if body["toolCallId"] == "message-one":
                # The production validator's atomic transaction has returned.
                # Holding HTTP 200 here prevents provider completion and its
                # fenced receipt append until the independent revocation commits.
                validated_inputs.append(body)
                validated.set()
                if not release_response.wait(timeout=30):
                    raise AssertionError("validation/revocation barrier was not released")
            return record

        messages_url = f"/api/agents/{source.agent_id}/messages"
        with patch("app_core.http.agent_messages.validate_agent_message", side_effect=hold_successful_validation):
            with ThreadPoolExecutor(max_workers=1) as executor:
                future = executor.submit(self._invoke_runtime, fixture,
                    "django_message_revocation_commit_window", "agent-message-revocation-window-ok")
                try:
                    self.assertTrue(validated.wait(timeout=180), "production API validation did not reach the barrier")
                    self.assertEqual(len(validated_inputs), 1)
                    self.assertEqual(validated_inputs[0]["body"], fixture["firstBody"])
                    self.assertEqual(source.events.filter(payload__type="tool_result").count(), 0)
                    self.assertEqual(self.client.get(messages_url).json()["messages"], [])
                    with transaction.atomic():
                        WorkspaceMembership.objects.filter(pk=membership.pk).delete()
                    self.assertFalse(WorkspaceMembership.objects.filter(pk=membership.pk).exists())
                    self.assertEqual(self.client.get(messages_url).status_code, 404)
                finally:
                    release_response.set()
                future.result(timeout=300)

        results = source.events.filter(payload__type="tool_result", payload__payload__toolName="send_message")
        accepted = results.get(payload__payload__resultState="successWithOutput")
        self.assertEqual(accepted.payload["payload"]["callId"], "message-one")
        self.assertEqual(results.count(), 2)
        self.assertEqual(results.exclude(pk=accepted.pk).count(), 1)
        self.assertEqual(source.events.filter(payload__type="assistant_message").count(), 1)
        self.assertEqual(source.events.filter(payload__type="agent_run_completed").count(), 1)
        self.assertFalse(work.events.exists())
        self.assertEqual(self.client.get(messages_url).status_code, 404)

        replacement = WorkspaceMembership.objects.create(workspace=source.workspace, user=run.user, role="owner")
        self.assertNotEqual(replacement.id, membership.id)
        visible = self.client.get(messages_url)
        self.assertEqual(visible.status_code, 200, visible.content)
        self.assertEqual(len(visible.json()["messages"]), 1)
        message = visible.json()["messages"][0]
        self.assertEqual(message["id"], accepted.eventId)
        self.assertEqual(message["body"], fixture["firstBody"])
        self.assertEqual(message["sessionRefs"], [work.id])
        self.assertEqual(message["fileRefs"], [fixture["fileRef"]])
        self.assertEqual(self.client.get(messages_url).json(), visible.json())
        followup = self.client.post("/internal/agent-messages/validate", content_type="application/json",
            data=json.dumps({**validated_inputs[0], "toolCallId": "send-after-rejoin"}),
            HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN)
        self.assertEqual(followup.status_code, 403, "rejoining must not revive the Run's original membership identity")
        replacement.delete()
        self.assertEqual(self.client.get(messages_url).status_code, 404)
        print("agent-message-revocation-window-ok: validation barrier; revoke before fenced commit; 1 stable committed message; current history/rejoin checks")
