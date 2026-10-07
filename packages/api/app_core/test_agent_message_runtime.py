"""Pinned Core, production message provider, real Django and PostgreSQL; no model API."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
from concurrent.futures import ThreadPoolExecutor
from threading import Event
from unittest.mock import patch

from django.conf import settings
from django.contrib.auth import get_user_model
from django.core.files.base import ContentFile
from django.core.files.storage import default_storage
from django.db import connection, transaction
from django.test import LiveServerTestCase, override_settings

from .agent_messages import message_contract_digest, validate_agent_message
from .agent_run_authorization_factory import create_agent_run_authorization
from .models import Agent, AgentRun, ModelConfig, Session, SessionAssetLink, SessionEvent, UserLibraryObject, Workspace, WorkspaceMembership
from .runtime_client import build_agent_run_start


class AgentMessageRuntimeTests(LiveServerTestCase):
    serialized_rollback = True

    def _runtime_fixture(self):
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
        # Django flush does not manage Core's native runtime schema. Remove only
        # tables this fixture creates, so later API lease fixtures keep their
        # own schema instead of inheriting Core's additional required columns.
        runtime_tables_query = """SELECT c.relname FROM pg_class c
            JOIN pg_namespace n ON n.oid = c.relnamespace
            WHERE n.nspname = 'runtime' AND c.relkind IN ('r', 'p')"""
        with connection.cursor() as cursor:
            cursor.execute("SELECT EXISTS (SELECT 1 FROM pg_namespace WHERE nspname = 'runtime')")
            previous_schema_exists = cursor.fetchone()[0]
            cursor.execute(runtime_tables_query)
            previous_tables = {row[0] for row in cursor.fetchall()}

        def remove_created_runtime_tables():
            with connection.cursor() as cursor:
                cursor.execute(runtime_tables_query)
                created = {row[0] for row in cursor.fetchall()} - previous_tables
                if created:
                    names = ", ".join("runtime." + connection.ops.quote_name(name) for name in sorted(created))
                    cursor.execute("DROP TABLE " + names)
                cursor.execute(runtime_tables_query)
                self.assertEqual({row[0] for row in cursor.fetchall()}, previous_tables)
                if not previous_schema_exists:
                    # A second production-store fixture must see the original
                    # absent schema, rather than an empty migrated namespace.
                    cursor.execute("DROP FUNCTION IF EXISTS runtime.notify_runtime_job_ready_v1()")
                    cursor.execute("DROP SCHEMA IF EXISTS runtime")

        self.addCleanup(remove_created_runtime_tables)
        fixture = {"liveServerUrl": self.live_server_url, "agentRunStart": build_agent_run_start(run),
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
        result = subprocess.run(["cargo", "test", "--locked", "-p", "runtime_server",
            "agent_messages::tests::runtime::" + test_name, "--", "--ignored", "--exact", "--nocapture"],
            cwd=root, env={**os.environ, "CARGO_BUILD_JOBS": "1", "CENTAERIS_AGENT_MESSAGE_FIXTURE": json.dumps(fixture),
                           "NO_PROXY": "127.0.0.1,localhost," + os.environ.get("NO_PROXY", "")},
            capture_output=True, text=True, encoding="utf-8", timeout=300)
        output = result.stdout + result.stderr
        self.assertEqual(result.returncode, 0, output)
        self.assertIn("running 1 test", output)
        self.assertIn("1 passed; 0 failed; 0 ignored", output)
        self.assertIn(receipt, output)

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
