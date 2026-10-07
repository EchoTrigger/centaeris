"""The real PostgreSQL typed store retains bodies across claims, ACK and recovery."""
import json
import os
from pathlib import Path
import subprocess
from urllib.parse import quote

from django.conf import settings
from django.db import connection
from django.http import HttpRequest
from django.middleware.csrf import get_token
from django.test import TransactionTestCase, LiveServerTestCase

from . import test_agent_input_delivery as fixture
from . import test_native_consumption_runtime as native_fixture
from . import test_agent_input_attachments as attachment_fixture
from .runtime_client import build_agent_run_start


def runtime_fixture_env(payload):
    env = {**os.environ, "NO_PROXY": "127.0.0.1,localhost,::1",
           "no_proxy": "127.0.0.1,localhost,::1", "CENTAERIS_AGENT_INPUT_FIXTURE": json.dumps(payload)}
    for key in ("HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "http_proxy", "https_proxy", "all_proxy"):
        env.pop(key, None)
    return env


class AgentInputTypedStoreTests(TransactionTestCase):
    serialized_rollback = True
    bind = fixture.AgentInputDeliveryTests.bind
    submit = fixture.AgentInputDeliveryTests.submit
    input_url = fixture.AgentInputDeliveryTests.input_url
    runtime = fixture.AgentInputDeliveryTests.runtime
    delivery = fixture.AgentInputDeliveryTests.delivery
    read = fixture.AgentInputDeliveryTests.read

    def setUp(self):
        fixture.AgentInputDeliveryTests.setUp(self)
        native_fixture.isolate_runtime_schema(self)

    def test_real_typed_store_fences_claims_reclaims_exact_ids_and_ack_never_creates_read(self):
        bodies = ["  first\n中文  ", "  second exact body  "]
        with self.runtime():
            self.submit("initial-one", bodies[0])
            self.submit("active-two", bodies[1])
        run = self.delivery("initial-one").queue.agent_run
        database = connection.settings_dict
        url = "postgresql://{}:{}@{}:{}/{}".format(quote(database["USER"]), quote(database["PASSWORD"]),
            database["HOST"], database["PORT"], quote(database["NAME"]))
        payload = {"agentRunStart": build_agent_run_start(run), "databaseUrl": url,
            "signingKey": settings.AGENT_RUN_AUTHORIZATION_SIGNING_KEY,
            "expected": [{"inputId": identity, "body": body} for identity, body in zip(
                ["initial-one", "active-two"], bodies)]}
        result = subprocess.run(["cargo", "test", "--locked", "--offline", "-p", "runtime_server",
            "agent_inputs_tests::hosted_typed_input_store", "--", "--ignored", "--exact", "--nocapture"],
            cwd=Path(__file__).resolve().parents[3], capture_output=True, text=True, encoding="utf-8",
            timeout=300, env=runtime_fixture_env(payload))
        diagnostic = result.stdout + result.stderr
        self.assertEqual(result.returncode, 0, diagnostic)
        self.assertIn("hosted-agent-input-store-ok", diagnostic)
        self.assertEqual([self.delivery(identity).input.body for identity in ("initial-one", "active-two")], bodies)
        self.assertIsNone(self.read("initial-one"))
        self.assertIsNone(self.read("active-two"))


class AgentInputAttachmentStoreTests(LiveServerTestCase):
    serialized_rollback = True
    bind = fixture.AgentInputDeliveryTests.bind
    runtime = fixture.AgentInputDeliveryTests.runtime
    input_url = fixture.AgentInputDeliveryTests.input_url
    delivery = fixture.AgentInputDeliveryTests.delivery
    read = fixture.AgentInputDeliveryTests.read
    upload = attachment_fixture.AgentInputAttachmentTests.upload
    submit = attachment_fixture.AgentInputAttachmentTests.submit

    def setUp(self):
        fixture.AgentInputDeliveryTests.setUp(self)
        native_fixture.isolate_runtime_schema(self)

    def test_hosted_initial_and_active_attachment_claim_ack_restart_retains_original_grants(self):
        import base64
        image = base64.b64decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jRZkAAAAASUVORK5CYII=")
        initial, active = self.upload("initial.png", image), self.upload("active.png", image + b"\n")
        with self.runtime():
            first = self.submit("initial-image", "", [initial])
            later = self.submit("active-image", "  late body\n", [active])
        self.assertEqual((first.status_code, later.status_code), (201, 201))
        run = self.delivery("initial-image").queue.agent_run
        original = (run.authorization.payload, run.authorization.digest, run.authorization.signature)
        database = connection.settings_dict
        url = "postgresql://{}:{}@{}:{}/{}".format(quote(database["USER"]), quote(database["PASSWORD"]),
            database["HOST"], database["PORT"], quote(database["NAME"]))
        payload = {"agentRunStart": build_agent_run_start(run), "databaseUrl": url,
            "signingKey": settings.AGENT_RUN_AUTHORIZATION_SIGNING_KEY, "attachmentRecovery": True,
            "apiUrl": self.live_server_url, "internalToken": settings.INTERNAL_API_TOKEN,
            "expectedImageData": {initial: base64.b64encode(image).decode(),
                active: base64.b64encode(image + b"\n").decode()},
            "expectedAttachmentRefs": sorted([initial, active]),
            "expected": [{key: response.json()["input"][key] for key in ("inputId", "body", "attachments")}
                for response in (first, later)]}
        result = subprocess.run(["cargo", "test", "--locked", "--offline", "-p", "runtime_server",
            "agent_inputs_tests::hosted_typed_input_store", "--", "--ignored", "--exact", "--nocapture"],
            cwd=Path(__file__).resolve().parents[3], capture_output=True, text=True, encoding="utf-8",
            timeout=300, env=runtime_fixture_env(payload))
        diagnostic = result.stdout + result.stderr
        self.assertEqual(result.returncode, 0, diagnostic)
        self.assertIn("hosted-agent-attachment-recovery-ok", diagnostic)
        run.authorization.refresh_from_db()
        self.assertEqual((run.authorization.payload, run.authorization.digest, run.authorization.signature), original)
        for identity in ("initial-image", "active-image"):
            self.assertIsNotNone(self.delivery(identity).acknowledged_at_ms)
            self.assertIsNone(self.read(identity))


class AgentInputMainRequestTests(LiveServerTestCase):
    serialized_rollback = True
    bind = fixture.AgentInputDeliveryTests.bind
    submit = fixture.AgentInputDeliveryTests.submit
    input_url = fixture.AgentInputDeliveryTests.input_url
    runtime = fixture.AgentInputDeliveryTests.runtime
    delivery = fixture.AgentInputDeliveryTests.delivery
    read = fixture.AgentInputDeliveryTests.read

    def setUp(self):
        fixture.AgentInputDeliveryTests.setUp(self)
        native_fixture.isolate_runtime_schema(self)

    def test_real_api_input_main_commit_and_recovery_retain_one_read_and_exact_context(self):
        self.run_recovery("hosted_input_main_request_recovery")

    def test_real_api_committed_main_failed_ack_recovers_one_read_and_exact_context(self):
        self.run_recovery("hosted_input_ack_recovery")

    def test_real_api_checkpoint_replacement_retains_exact_context_and_read_identity(self):
        self.run_recovery("hosted_input_checkpoint_recovery")

    def run_recovery(self, rust_test):
        bodies = ["  original main body\n", "  active main body\n", "  recovered main body\n"]
        database = connection.settings_dict
        url = "postgresql://{}:{}@{}:{}/{}".format(quote(database["USER"]), quote(database["PASSWORD"]),
            database["HOST"], database["PORT"], quote(database["NAME"]))
        with self.runtime():
            self.assertEqual(self.submit("initial-one", bodies[0]).status_code, 201)
            self.assertEqual(self.submit("active-two", bodies[1]).status_code, 201)
            run = self.delivery("initial-one").queue.agent_run
            csrf_request = HttpRequest()
            csrf_token = get_token(csrf_request)
            payload = {"agentRunStart": build_agent_run_start(run), "databaseUrl": url,
                "signingKey": settings.AGENT_RUN_AUTHORIZATION_SIGNING_KEY,
                "apiUrl": self.live_server_url + self.input_url,
                "cookie": settings.SESSION_COOKIE_NAME + "=" + self.client.cookies[settings.SESSION_COOKIE_NAME].value
                    + "; " + settings.CSRF_COOKIE_NAME + "=" + csrf_request.META["CSRF_COOKIE"],
                "csrfToken": csrf_token,
                "expected": [{"inputId": identity, "body": body} for identity, body in zip(
                    ["initial-one", "active-two", "recovered-three"], bodies)]}
            result = subprocess.run(["cargo", "test", "--locked", "--offline", "-p", "runtime_server",
                "agent_inputs_tests::" + rust_test, "--", "--ignored", "--exact", "--nocapture"],
                cwd=Path(__file__).resolve().parents[3], capture_output=True, text=True, encoding="utf-8",
                timeout=300, env=runtime_fixture_env(payload))
        diagnostic = result.stdout + result.stderr
        self.assertEqual(result.returncode, 0, diagnostic)
        self.assertIn("hosted-input-main-request-recovery-ok", diagnostic)
        self.assertEqual(fixture.models.AgentRun.objects.count(), 1)
        references = [self.read(identity) for identity in ("initial-one", "active-two", "recovered-three")]
        self.assertIsNotNone(references[0])
        self.assertEqual(references[0], references[1])
        if rust_test == "hosted_input_ack_recovery":
            self.assertEqual(references, [references[0]] * 3)
        else:
            self.assertIsNotNone(references[2])
            self.assertNotEqual(references[0], references[2])
        events = list(fixture.models.SessionEvent.objects.filter(agent_run=run).order_by("sequence"))
        # Admission and the first supplemental input share the authoritative initial turn.
        self.assertEqual(events[0].payload["turnId"], run.turn_id)
        self.assertEqual(events[1].payload["turnId"], run.turn_id)
        supplement = next(event for event in events if event.payload["type"] == "turn_supplement"
                          and event.payload["payload"]["supplementId"] == "active-two")
        self.assertEqual(supplement.payload["turnId"], run.turn_id)
        self.assertEqual(supplement.payload["payload"]["message"], bodies[1])
        for reference in references:
            event = next(event for event in events if event.eventId == reference["eventId"])
            self.assertEqual((event.agent_run_id, event.session_id), (run.id, run.session_id))
            self.assertEqual(reference["agentRunId"], run.id)
            self.assertEqual(event.payload["payload"]["requestId"], reference["requestId"])
        first_read = next(event for event in events if event.eventId == references[0]["eventId"])
        self.assertEqual(first_read.payload["turnId"], run.turn_id)
        if rust_test != "hosted_input_ack_recovery":
            deferred_read = next(event for event in events if event.eventId == references[2]["eventId"])
            self.assertNotEqual(deferred_read.payload["turnId"], run.turn_id)
        self.assertEqual([self.delivery(identity).input.body for identity in
            ("initial-one", "active-two", "recovered-three")], bodies)
