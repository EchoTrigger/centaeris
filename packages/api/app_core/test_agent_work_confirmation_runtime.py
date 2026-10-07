"""Actual Core confirmation provider and fenced PostgreSQL Session commits."""
import json
from contextlib import nullcontext
import os
from pathlib import Path
import subprocess
from unittest.mock import patch
from urllib.parse import quote

from django.conf import settings
from django.db import connection
from django.test import LiveServerTestCase

from . import test_agent_work_consumption as fixture
from . import test_native_consumption_runtime as native
from .agent_work_confirmation import committed_confirmation
from .runtime_client import build_agent_run_start
from .models import WorkspaceMembership


class WorkReturnConfirmationRuntimeTests(LiveServerTestCase):
    serialized_rollback = True
    setUp = fixture.AgentWorkConsumptionTests.setUp
    request_fact = fixture.AgentWorkConsumptionTests.request_fact
    dependencies = fixture.AgentWorkConsumptionTests.dependencies
    post = fixture.AgentWorkConsumptionTests.post
    child = fixture.AgentWorkConsumptionTests.child
    terminal = fixture.AgentWorkConsumptionTests.terminal
    publish = fixture.AgentWorkConsumptionTests.publish
    notice = fixture.AgentWorkConsumptionTests.notice
    consume = fixture.AgentWorkConsumptionTests.consume
    attempts = fixture.AgentWorkConsumptionTests.attempts
    runtime = fixture.AgentWorkConsumptionTests.runtime
    invoke_native = native.NativeConsumptionRuntimeTests.invoke

    def run_confirmation(self, case):
        native.isolate_runtime_schema(self)
        notice = self.notice(idle=case == "success")
        def create_input(session_id, input_id, source, content):
            output = self.invoke_native("create_native_input", "CENTAERIS_NATIVE_INPUT_REQUEST", {
                "schema": "runtime.host_event_input.create.v1", "sessionId": session_id,
                "inputId": input_id, "source": source, "content": content})
            return json.loads(next(line.removeprefix("core-native-input:") for line in output.splitlines()
                if line.startswith("core-native-input:")))["input"]
        with self.runtime(), patch("app_core.agent_work_consumption.request_host_event_input", side_effect=create_input):
            response = self.consume(notice)
        self.assertEqual(response.status_code, 201, response.content)
        attempt = self.attempts().get(pk=response.json()["attemptId"])
        run = attempt.coordinator_run
        # Dispatch helpers emit partial source wire fixtures. Acceptance and
        # notice identity are retained; only the confirmation call/result below
        # is generated, parsed and committed by the actual Core runtime.
        self.run.events.all().delete()
        config = connection.settings_dict
        url = "postgresql://{}:{}@{}:{}/{}".format(quote(config["USER"]), quote(config["PASSWORD"]),
            config["HOST"], config["PORT"], quote(config["NAME"]))
        payload = {"agentRunStart": build_agent_run_start(run), "databaseUrl": url,
            "signingKey": settings.AGENT_RUN_AUTHORIZATION_SIGNING_KEY,
            "liveServerUrl": self.live_server_url, "internalToken": settings.INTERNAL_API_TOKEN,
            "notice": notice, "attemptId": attempt.pk, "case": case}
        from .agent_work_confirmation import validate_work_return_confirmation
        def validated_then_rejoined(body):
            record = validate_work_return_confirmation(body)
            WorkspaceMembership.objects.filter(pk=self.membership.pk).delete()
            WorkspaceMembership.objects.create(workspace=self.workspace, user=self.user, role="owner")
            return record
        mutator = patch("app_core.agent_work_confirmation.validate_work_return_confirmation", side_effect=validated_then_rejoined) if case == "membership_rejoined" else nullcontext()
        with mutator:
            result = subprocess.run(["cargo", "test", "--locked", "--offline", "-p", "runtime_server",
                "agent_work::tests::runtime::confirm_native_return_fenced_commit", "--", "--ignored", "--exact", "--nocapture"],
                cwd=Path(__file__).resolve().parents[3], capture_output=True, text=True, encoding="utf-8",
                timeout=300, env={**os.environ, "CENTAERIS_WORK_CONFIRM_FIXTURE": json.dumps(payload)})
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("confirmation-core-pg-ok:" + case, result.stdout)
        projection = committed_confirmation(attempt)
        self.assertEqual(projection is not None, case in {"success", "active_success"})
        if projection is not None:
            self.assertEqual(committed_confirmation(self.attempts().get(pk=attempt.pk)), projection)
            self.assertEqual(run.events.filter(payload__type="tool_call", payload__payload__toolName="confirm_work_return").count(), 1)
            self.assertEqual(run.events.filter(payload__type="tool_result", payload__payload__toolName="confirm_work_return").count(), 1)

    def test_real_core_native_confirmation_split_commit_and_query(self):
        self.run_confirmation("success")

    def test_real_core_active_user_initial_confirmation_split_commit_and_query(self):
        self.run_confirmation("active_success")

    def test_real_core_forged_success_result_is_rejected_before_commit(self):
        self.run_confirmation("forged_result")

    def test_real_core_send_message_keeps_its_commit_behavior_and_is_unhandled(self):
        self.run_confirmation("other_tool")

    def test_real_core_success_followed_by_pg_rollback_is_unhandled(self):
        self.run_confirmation("commit_failure")

    def test_real_core_success_with_stale_lease_is_unhandled(self):
        self.run_confirmation("lease_failure")

    def test_real_core_success_then_role_revoked_before_commit_is_unhandled(self):
        self.run_confirmation("role_revoked")

    def test_real_core_success_then_child_read_revoked_before_commit_is_unhandled(self):
        self.run_confirmation("child_archived")

    def test_real_core_success_then_user_inactive_before_commit_is_unhandled(self):
        self.run_confirmation("user_inactive")

    def test_real_core_validated_membership_revocation_and_rejoin_do_not_revive_commit(self):
        self.run_confirmation("membership_rejoined")
