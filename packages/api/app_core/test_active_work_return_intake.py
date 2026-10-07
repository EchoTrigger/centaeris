"""Native returns share durable coordinator intake with user carriers."""
import json
import os
from pathlib import Path
import subprocess
from urllib.parse import quote
from unittest.mock import patch

from django.conf import settings
from django.contrib.auth import get_user_model
from django.db import IntegrityError, connection, transaction
from django.test import TransactionTestCase

from . import test_agent_work_consumption as fixture
from . import test_native_consumption_runtime as native
from .models import AgentInput, AgentInputDelivery, AgentInputQueue, AgentRun, AgentWorkConsumeAttempt, ModelConfig
from .runtime_client import build_agent_run_start


class ActiveWorkReturnIntakeTests(TransactionTestCase):
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
    runtime = fixture.AgentWorkConsumptionTests.runtime
    attempts = fixture.AgentWorkConsumptionTests.attempts
    invoke = native.NativeConsumptionRuntimeTests.invoke

    def test_native_acceptance_requires_sequence_and_keeps_explicit_initial_host(self):
        notice = self.notice()
        with self.runtime():
            self.assertEqual(self.consume(notice).status_code, 201)
        attempt = self.attempts().get()
        queue = AgentInputQueue.objects.get(agent_run=attempt.coordinator_run)
        self.assertEqual(attempt.delivery_sequence, 1)
        self.assertEqual(queue.initial_host_id, attempt.pk)
        self.assertIsNone(queue.initial_input_id)
        with self.assertRaises(IntegrityError), transaction.atomic():
            self.attempts().filter(pk=attempt.pk).update(delivery_sequence=None)
        attempt.refresh_from_db()
        self.assertEqual(attempt.delivery_sequence, 1)
        self.assertEqual(AgentInputQueue.objects.get(pk=queue.pk).initial_host_id, attempt.pk)


    def test_active_run_accepts_multiple_notices_without_changing_initial_or_authorization(self):
        notices = [self.notice(call, idle=False) for call in ("one", "two")]
        before = build_agent_run_start(self.run)
        self.agent.model_config = None
        self.agent.save(update_fields=["model_config"])
        with self.runtime() as (_, schedules):
            responses = [self.consume(notice, "consume-" + str(i)) for i, notice in enumerate(notices)]
        self.assertEqual([r.status_code for r in responses], [201, 201])
        self.assertEqual(set(self.attempts().values_list("coordinator_run_id", flat=True)), {self.run.pk})
        self.assertEqual(build_agent_run_start(self.run), before)
        self.assertEqual(schedules, [], "active intake does not schedule another lifecycle")

    def test_closed_active_queue_retains_acceptance_and_scanner_binds_one_successor(self):
        notice = self.notice(idle=False)
        AgentInputQueue.objects.create(agent_run=self.run, authorization_digest=self.run.authorization.digest,
                                       accepting=False, closed_reason="safe_point_closed", closed_at_ms=1)
        with self.runtime() as (_, schedules):
            accepted = self.consume(notice)
        self.assertEqual(accepted.status_code, 201, accepted.content)
        attempt = self.attempts().get()
        self.assertIsNone(attempt.coordinator_run_id)
        self.assertEqual(schedules, [])
        AgentRun.objects.filter(pk=self.run.pk).update(status="completed")
        with self.runtime() as (_, schedules):
            for _ in range(2):
                page = self.client.post("/internal/agent-work/returns/consume/discover", content_type="application/json",
                    data=json.dumps({"schema": "workspace.agent_work.consume_discover.v1", "limit": 100,
                                     "after": None, "through": None}),
                    HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN).json()
                self.assertIn("entries", page)
                for entry in page["entries"]:
                    self.consume(notice, entry["operationId"])
        attempt.refresh_from_db()
        self.assertIsNotNone(attempt.coordinator_run_id)
        self.assertEqual(len(schedules), 1)
        self.assertEqual(self.attempts().count(), 1)
        self.assertEqual(AgentInputQueue.objects.get(agent_run=attempt.coordinator_run).initial_host_id, attempt.pk)

    def test_real_postgres_queue_claims_native_return_before_empty_close(self):
        self.run_typed_store(active=False)

    def test_pending_replay_rejects_corrupt_receipt_owner_without_runtime_rpc(self):
        notice = self.notice(idle=False)
        AgentInputQueue.objects.create(agent_run=self.run, authorization_digest=self.run.authorization.digest,
            accepting=False, closed_reason="safe_point_closed", closed_at_ms=1)
        with self.runtime():
            self.assertEqual(self.consume(notice).status_code, 201)
        attempt = self.attempts().get()
        foreign = get_user_model().objects.create(username="foreign-receipt-owner")
        type(attempt.operation).objects.filter(pk=attempt.operation_id).update(user=foreign)
        with patch("urllib.request.urlopen", side_effect=AssertionError("reject before RPC")):
            rejected = self.consume(notice)
        self.assertEqual(rejected.status_code, 400, rejected.content)
        self.assertEqual(self.attempts().count(), 1)
        self.assertIsNone(self.attempts().get().coordinator_run_id)

    def test_closed_batch_binds_one_successor_in_order_under_current_idle_policy(self):
        notices = [self.notice("batch-" + str(i), idle=False) for i in range(2)]
        AgentInputQueue.objects.create(agent_run=self.run, authorization_digest=self.run.authorization.digest,
            accepting=False, closed_reason="safe_point_closed", closed_at_ms=1)
        with self.runtime():
            for i, notice in enumerate(notices):
                self.assertEqual(self.consume(notice, "batch-op-" + str(i)).status_code, 201)
        attempts = list(self.attempts().order_by("delivery_sequence"))
        self.assertEqual([a.notice_id for a in attempts], [n["noticeId"] for n in notices])
        selected = ModelConfig.objects.create(displayName="Next idle policy", thinkingMode="low", thinkingModes=["low"])
        self.agent.model_config, self.agent.thinking_mode = selected, "low"
        self.agent.save(update_fields=["model_config", "thinking_mode"])
        AgentRun.objects.filter(pk=self.run.pk).update(status="completed")
        with self.runtime() as (_, schedules):
            self.assertEqual(self.consume(notices[1], "batch-op-1").status_code, 200)
        attempts = list(self.attempts().order_by("delivery_sequence"))
        self.assertEqual(len({a.coordinator_run_id for a in attempts}), 1)
        run = attempts[0].coordinator_run
        self.assertEqual((run.modelConfig_id, run.thinkingMode), (selected.pk, "low"))
        self.assertEqual(run.authorization.payload["thinkingMode"], "low")
        self.assertEqual(AgentInputQueue.objects.get(agent_run=run).initial_host_id, attempts[0].pk)
        self.assertEqual(len(schedules), 1)

    def test_idle_handoff_prefers_pending_user_initial_and_retains_host_delivery(self):
        notice = self.notice()
        user = AgentInput.objects.create(agent=self.agent, session=self.source, membership_ref=self.membership.pk,
            input_id="idle-user-priority", sequence=1, accepted_source_sequence=0,
            body="  pending user objective  ", created_at_ms=1)
        with self.runtime():
            self.assertEqual(self.consume(notice).status_code, 201)
        attempt = self.attempts().get()
        queue = AgentInputQueue.objects.get(agent_run=attempt.coordinator_run)
        self.assertEqual(queue.initial_input_id, user.pk)
        self.assertIsNone(queue.initial_host_id)
        self.assertEqual(build_agent_run_start(attempt.coordinator_run)["initialInput"],
            {"type": "userInput", "inputId": user.input_id, "message": user.body, "attachmentRefs": []})
        self.assertEqual(attempt.delivery_sequence, 1)

    def test_real_mixed_queue_prioritizes_user_and_commits_active_host_with_lease(self):
        self.run_typed_store(active=True)

    def test_real_native_intake_reaches_committed_main_requests(self):
        self.run_typed_store(active=True, main=True)

    def run_typed_store(self, *, active, main=False):
        native.isolate_runtime_schema(self)
        notices = [self.notice("native-" + str(i), idle=not active) for i in range(2 if active else 1)]
        def create_input(session_id, input_id, source, content):
            output = self.invoke("create_native_input", "CENTAERIS_NATIVE_INPUT_REQUEST", {
                "schema": "runtime.host_event_input.create.v1", "sessionId": session_id,
                "inputId": input_id, "source": source, "content": content})
            return json.loads(next(line.removeprefix("core-native-input:") for line in output.splitlines()
                if line.startswith("core-native-input:")))["input"]
        with self.runtime(), patch("app_core.agent_work_consumption.request_host_event_input", side_effect=create_input):
            for i, notice in enumerate(notices):
                accepted = self.consume(notice, "native-op-" + str(i))
                self.assertEqual(accepted.status_code, 201, accepted.content)
        attempts = list(self.attempts().order_by("delivery_sequence"))
        run = attempts[0].coordinator_run
        expected = [{"inputId": attempt.pk, "native": True} for attempt in attempts]
        if active:
            fact = AgentInput.objects.create(agent=self.agent, session=self.source, membership_ref=self.membership.pk,
                input_id="user-priority", sequence=1, accepted_source_sequence=0, body="  exact priority body  ", created_at_ms=1)
            AgentInputDelivery.objects.create(input=fact, queue=AgentInputQueue.objects.get(agent_run=run))
            expected.insert(0, {"inputId": fact.input_id, "body": fact.body})
            # The dispatch helpers emit partial wire fixtures. The admission
            # and notice are already committed; omit those synthetic rows
            # before asking the real Core parser to validate fenced records.
            self.run.events.all().delete()
        database = connection.settings_dict
        url = "postgresql://{}:{}@{}:{}/{}".format(quote(database["USER"]), quote(database["PASSWORD"]),
            database["HOST"], database["PORT"], quote(database["NAME"]))
        payload = {"agentRunStart": build_agent_run_start(run), "databaseUrl": url,
                   "signingKey": settings.AGENT_RUN_AUTHORIZATION_SIGNING_KEY,
                   "expected": expected, "commitNative": active, "modelMain": main,
                   "nativeInputs": [a.input_binding["initialInput"]["input"] for a in attempts]}
        result = subprocess.run(["cargo", "test", "--locked", "--offline", "-p", "runtime_server",
            "agent_inputs_tests::hosted_typed_input_store", "--", "--ignored", "--exact", "--nocapture"],
            cwd=Path(__file__).resolve().parents[3], capture_output=True, text=True, encoding="utf-8",
            timeout=300, env={**os.environ, "CENTAERIS_AGENT_INPUT_FIXTURE": json.dumps(payload)})
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
