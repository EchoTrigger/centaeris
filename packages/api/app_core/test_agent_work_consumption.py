"""Explicit first consumption accepts one durable native HostEvent admission."""
from contextlib import contextmanager
from concurrent.futures import ThreadPoolExecutor
from copy import deepcopy
import io
import json
import threading
from unittest.mock import patch

from django.apps import apps
from django.conf import settings
from django.db import IntegrityError, close_old_connections, connection, transaction
from django.test import Client, TransactionTestCase

from . import test_agent_work as work_fixture
from . import test_agent_work_returns as return_fixture
from .models import AgentRun, AgentRunAuthorization, HostedOperationReceipt, WorkspaceMembership
from .runtime_client import build_agent_run_start


class AgentWorkConsumptionTests(TransactionTestCase):
    serialized_rollback = True
    def setUp(self):
        work_fixture.AgentWorkTests.setUp(self)
        self.agent.model_config, self.agent.thinking_mode = self.model, self.run.thinkingMode
        self.agent.save(update_fields=["model_config", "thinking_mode"])
    request_fact = work_fixture.AgentWorkTests.request_fact
    dependencies = work_fixture.AgentWorkTests.dependencies
    post = work_fixture.AgentWorkTests.post
    child = return_fixture.AgentWorkReturnTests.child
    terminal = return_fixture.AgentWorkReturnTests.terminal
    publish = return_fixture.AgentWorkReturnTests.publish

    def notice(self, call_id="dispatch-one", *, idle=True):
        child = self.child(call_id)
        self.terminal(child)
        AgentRun.objects.filter(pk=child.pk).update(status="completed")
        response = self.publish(child.pk)
        self.assertEqual(response.status_code, 201, response.content)
        if idle:
            AgentRun.objects.filter(pk=self.run.pk).update(status="completed")
        return response.json()["notice"]

    def consume(self, notice, operation_key="consume-one", *, client=None, **changes):
        return (client or self.client).post(
            "/internal/agent-work/returns/consume", content_type="application/json",
            data=json.dumps({"schema": "workspace.agent_work.consume.v1",
                             "noticeId": notice["noticeId"], "operationId": operation_key,
                             **changes}),
            HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN,
        )

    def attempts(self):
        return apps.get_model("app_core", "AgentWorkConsumeAttempt").objects

    def counts(self):
        return (AgentRun.objects.count(), AgentRunAuthorization.objects.count(),
                HostedOperationReceipt.objects.count())

    @contextmanager
    def runtime(self, *, before_profile=None, before_schedule=None, fail_schedule=False):
        inputs, schedules = [], []

        def respond(request, **_kwargs):
            path = request.full_url
            self.assertFalse(connection.in_atomic_block,
                             "Runtime RPC must not hold the admission transaction")
            if path.endswith("/internal/execution-profile"):
                if before_profile:
                    before_profile()
                value = work_fixture.PROFILE
            elif path.endswith("/internal/host-event-input"):
                body = json.loads(request.data)
                self.assertEqual(set(body), {"schema", "sessionId", "inputId", "source", "content"})
                self.assertEqual(body["schema"], "runtime.host_event_input.create.v1")
                self.assertEqual(body["sessionId"], self.source.pk)
                # This opaque fake only checks ownership and forwarding at the
                # HTTP seam. Real Core identity/parser acceptance is exercised
                # by the Rust contract and hosted Runtime integration gates.
                supplied = {"inputId": body["inputId"], "messageId": "opaque-core-result:" + body["inputId"],
                            "source": body["source"], "content": body["content"]}
                inputs.append(supplied)
                value = {"schema": "runtime.host_event_input.created.v1", "input": supplied}
            elif path.endswith("/internal/jobs/schedule"):
                body = json.loads(request.data)
                if before_schedule:
                    before_schedule(body)
                if fail_schedule:
                    raise OSError("synthetic lost schedule response")
                schedules.append(body)
                value = {"disposition": "inserted", "job": {**body, "status": "queued"}}
            else:
                raise AssertionError("unexpected Runtime RPC: " + path)
            return io.BytesIO(json.dumps(value).encode())

        with patch("urllib.request.urlopen", side_effect=respond):
            yield inputs, schedules

    def test_first_consume_atomically_binds_native_input_run_authorization_and_receipt(self):
        notice = self.notice()
        baseline = self.counts()
        self.agent.instructions = "Keep the accepted objective."
        self.agent.save(update_fields=["instructions"])

        def committed_before_schedule(body):
            attempt = self.attempts().select_related("coordinator_run", "operation").get()
            self.assertEqual(attempt.coordinator_run_id, body["payloadRef"].removeprefix("record:agent_run:"))
            self.assertEqual(attempt.operation.agentRunId, attempt.coordinator_run_id)
            self.assertTrue(AgentRunAuthorization.objects.filter(agent_run_id=attempt.coordinator_run_id).exists())

        with self.runtime(before_schedule=committed_before_schedule) as (inputs, schedules):
            response = self.consume(notice)
        self.assertEqual(response.status_code, 201, response.content)
        self.assertEqual(self.counts(), tuple(count + 1 for count in baseline))
        attempt = self.attempts().select_related("coordinator_run__authorization", "operation").get()
        run = attempt.coordinator_run
        self.assertEqual(attempt.notice_id, notice["noticeId"])
        self.assertEqual((run.workspace_id, run.session_id, run.user_id, run.membership_ref),
                         (self.workspace.pk, self.source.pk, self.user.pk, self.membership.pk))
        self.assertEqual(run.prompt, self.run.prompt)
        self.assertNotEqual(run.prompt, self.args["objective"])
        self.assertEqual(run.agent_instructions, self.agent.instructions)
        self.assertIsNone(run.acting_app_id)
        self.assertIsNone(run.app_delegation_id)
        self.assertEqual(attempt.operation.command, "consumeWorkReturn")
        self.assertEqual(response.json()["operation"]["agentRunId"], run.pk)
        self.assertEqual(response.json()["attemptId"], attempt.pk)
        self.assertEqual(len(inputs), 1)
        self.assertEqual(inputs[0]["inputId"], attempt.pk)
        self.assertEqual(json.loads(inputs[0]["content"]), notice)
        start = build_agent_run_start(run)
        self.assertEqual(start["schema"], "workspace.agent_run.start.v2")
        self.assertEqual(start["initialInput"], {"type": "hostEvent", "attemptId": attempt.pk,
                         "noticeId": notice["noticeId"], "input": inputs[0]})
        self.assertEqual(attempt.input_binding["initialInput"], start["initialInput"])
        self.assertEqual(attempt.input_binding["authorizationDigest"], run.authorization.digest)
        self.assertEqual(len(schedules), 1)
        self.assertEqual(schedules[0]["jobId"], "agent_run.lifecycle:" + run.pk)
        self.assertFalse(run.events.exists(), "admission does not fabricate a Runtime input record")

    def test_active_admission_reuses_the_coordinator_without_another_lifecycle(self):
        notice = self.notice(idle=False)
        before = self.counts()
        with self.runtime() as (inputs, schedules):
            accepted = self.consume(notice)
        self.assertEqual(accepted.status_code, 201, accepted.content)
        self.assertEqual(self.counts(), (before[0], before[1], before[2] + 1))
        self.assertEqual(self.attempts().get().coordinator_run_id, self.run.pk)
        self.assertEqual(len(inputs), 1)
        self.assertEqual(schedules, [])

    def test_lost_response_replay_and_other_operation_reuse_the_first_root(self):
        notice = self.notice()
        with self.runtime(fail_schedule=True):
            first = self.consume(notice)
        self.assertEqual(first.status_code, 201, first.content)
        baseline = self.counts()
        with patch("urllib.request.urlopen", side_effect=AssertionError("replay requires no RPC")):
            same = self.consume(notice, client=Client())
            root = self.consume(notice, "another-root-operation")
        self.assertEqual((same.status_code, root.status_code), (200, 200))
        self.assertEqual(same.json(), first.json())
        self.assertEqual(root.json(), first.json())
        self.assertEqual(self.counts(), baseline)
        self.assertEqual(self.attempts().count(), 1)

    def test_same_operation_cannot_target_another_notice_and_new_notice_is_not_a_retry(self):
        first_notice = self.notice("first")
        with self.runtime():
            first = self.consume(first_notice)
        self.assertEqual(first.status_code, 201, first.content)
        first_run = first.json()["operation"]["agentRunId"]
        AgentRun.objects.filter(pk=first_run).update(status="failed")
        next_notice = self.notice("second")
        with self.runtime() as (inputs, _schedules):
            conflict = self.consume(next_notice)
            accepted = self.consume(next_notice, "consume-second")
        self.assertEqual(conflict.status_code, 409, conflict.content)
        self.assertEqual(accepted.status_code, 201, accepted.content)
        self.assertEqual(self.attempts().count(), 2)
        self.assertEqual(len(inputs), 1)
        self.assertEqual(json.loads(inputs[0]["content"]), next_notice)
        self.assertNotIn(first_notice["noticeId"], inputs[0]["content"])

    def test_original_membership_revocation_and_rejoin_never_revive_consumption(self):
        notice = self.notice()
        self.membership.delete()
        WorkspaceMembership.objects.create(workspace=self.workspace, user=self.user, role="owner")
        baseline = self.counts()
        with patch("urllib.request.urlopen", side_effect=AssertionError("reject authority before RPC")):
            denied = self.consume(notice)
        self.assertEqual(denied.status_code, 403, denied.content)
        self.assertEqual(self.counts(), baseline)
        self.assertFalse(self.attempts().exists())

    def test_replay_still_rechecks_current_membership_and_child_read_authority(self):
        notice = self.notice()
        with self.runtime():
            first = self.consume(notice)
        self.assertEqual(first.status_code, 201, first.content)
        from . import app_delegations
        current = app_delegations.session_authority_is_current
        child_session = notice["identity"]["workSessionId"]
        def denied_child(user_id, session_id, **kwargs):
            return session_id != child_session and current(user_id, session_id, **kwargs)
        with patch("app_core.agent_work_consumption.session_authority_is_current", side_effect=denied_child):
            denied = self.consume(notice)
        self.assertEqual(denied.status_code, 403, denied.content)
        self.assertEqual(self.attempts().count(), 1)

    def test_authority_rejection_does_not_evaluate_malformed_notice_content(self):
        from .models import AgentWorkReturn
        notice = self.notice()
        AgentWorkReturn.objects.filter(pk=notice["noticeId"]).update(payload={"terminal": None})
        checked = []
        def authority(user_id, session_id, *, scope):
            checked.append((user_id, session_id, scope))
            return scope != "events:read"
        before = self.counts()
        with patch("app_core.agent_work_consumption.session_authority_is_current", side_effect=authority), \
             patch("app_core.agent_work_consumption._payload", side_effect=AssertionError("authority rejects first")), \
             patch("urllib.request.urlopen", side_effect=AssertionError("no Runtime RPC on rejection")):
            denied = self.consume(notice)
        self.assertEqual(denied.status_code, 403, denied.content)
        self.assertEqual(denied.json(), {"error": "agent_work_consume_authority_rejected"})
        self.assertEqual(checked, [(self.user.pk, self.source.pk, "messages:submit"),
                                  (self.user.pk, notice["identity"]["workSessionId"], "events:read")])
        self.assertEqual(self.counts(), before)
        self.assertFalse(self.attempts().exists())

    def test_corrupt_native_binding_is_rejected_by_resolve_and_replay_without_new_facts(self):
        notice = self.notice()
        with self.runtime():
            accepted = self.consume(notice)
        self.assertEqual(accepted.status_code, 201, accepted.content)
        attempt = self.attempts().get()
        run = attempt.coordinator_run
        binding = deepcopy(attempt.input_binding)
        initial = binding["initialInput"]
        native = initial["input"]
        before = self.counts()
        def resolve():
            return self.client.post("/internal/agent-run-lifecycle/resolve", content_type="application/json",
                data=json.dumps({"schema": "runtime.agent_run_lifecycle.resolve.v1",
                    "jobId": "agent_run.lifecycle:" + run.pk, "agentRunId": run.pk,
                    "authorizationDigest": run.authorization.digest}),
                HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN)
        cases = [
            ("missing binding fields", {}),
            ("run identity precedes malformed initial input", {**binding, "agentRunId": "another-run", "initialInput": None}),
            ("missing initial input fields", {**binding, "initialInput": {}}),
            ("another notice", {**binding, "initialInput": {**initial, "noticeId": "another-notice"}}),
            ("missing native input fields", {**binding, "initialInput": {**initial, "input": {}}}),
            ("null native input", {**binding, "initialInput": {**initial, "input": None}}),
            ("another source", {**binding, "initialInput": {**initial, "input": {**native, "source": "another-source"}}}),
            ("non-string content", {**binding, "initialInput": {**initial, "input": {**native, "content": 7}}}),
            ("malformed JSON", {**binding, "initialInput": {**initial, "input": {**native, "content": "{"}}}),
            ("another payload", {**binding, "initialInput": {**initial, "input": {**native, "content": "{}"}}}),
        ]
        with patch("urllib.request.urlopen", side_effect=AssertionError("rejection requires no Runtime RPC")):
            for label, changed in cases:
                with self.subTest(binding=label):
                    self.attempts().filter(pk=attempt.pk).update(input_binding=changed)
                    rejected = resolve()
                    replay = self.consume(notice)
                    self.assertEqual(rejected.status_code, 409, rejected.content)
                    self.assertEqual(rejected.json(), {"error": "agent_run_start_invalid"})
                    self.assertEqual(replay.status_code, 400, replay.content)
                    self.assertEqual(replay.json(), {"error": "agent_work_consume_invalid"})
                    self.assertEqual(self.counts(), before)
            self.attempts().filter(pk=attempt.pk).update(input_binding=binding)
            for target, change, restore in (
                    (HostedOperationReceipt.objects.filter(pk=attempt.operation_id), {"command": "submitMessage"}, {"command": "consumeWorkReturn"}),
                    (AgentRun.objects.filter(pk=run.pk), {"prompt": "Changed accepted objective"}, {"prompt": run.prompt})):
                with self.subTest(change=change):
                    target.update(**change)
                    self.assertEqual(resolve().status_code, 409)
                    self.assertEqual(self.consume(notice).status_code, 400)
                    self.assertEqual(self.counts(), before)
                    target.update(**restore)
            self.assertEqual(resolve().status_code, 200)
            self.assertEqual(self.consume(notice).json(), accepted.json())
        self.assertFalse(run.events.exists())
        self.assertEqual(self.attempts().count(), 1)

    def test_authorization_or_receipt_insert_failure_leaves_no_half_admission(self):
        notice = self.notice()
        baseline = self.counts()
        for target in (AgentRunAuthorization, HostedOperationReceipt):
            def reject(instance, *args, **kwargs):
                raise IntegrityError("synthetic acceptance write failure")
            with self.subTest(model=target.__name__), self.runtime(), patch.object(target, "save", reject):
                response = self.consume(notice)
            self.assertGreaterEqual(response.status_code, 400, response.content)
            self.assertEqual(self.counts(), baseline)
            self.assertFalse(self.attempts().exists())

    def test_two_explicit_consumers_commit_one_root_and_the_same_receipt(self):
        notice = self.notice()
        barrier = threading.Barrier(2)
        with self.runtime(before_profile=lambda: barrier.wait(10)):
            def call(operation_id):
                close_old_connections()
                try:
                    return self.consume(notice, operation_id, client=Client())
                finally:
                    close_old_connections()
            with ThreadPoolExecutor(max_workers=2) as pool:
                responses = [future.result(timeout=20) for future in
                             [pool.submit(call, "racing-one"), pool.submit(call, "racing-two")]]
        self.assertEqual(sorted(response.status_code for response in responses), [200, 201])
        self.assertEqual(responses[0].json(), responses[1].json())
        self.assertEqual(self.attempts().count(), 1)

    def test_internal_transport_rejects_unknown_fields_and_missing_internal_token(self):
        notice = self.notice()
        for changes in ({"schema": "workspace.agent_work.consume.v0"}, {"predecessorAttemptId": "old"},
                        {"noticeIds": [notice["noticeId"]]}, {"operationId": ""}, {"operation_id": "alias"}):
            with self.subTest(changes=changes):
                self.assertEqual(self.consume(notice, **changes).status_code, 400)
        denied = self.client.post("/internal/agent-work/returns/consume", content_type="application/json",
            data=json.dumps({"schema": "workspace.agent_work.consume.v1", "noticeId": notice["noticeId"],
                             "operationId": "unauthorized"}))
        self.assertEqual(denied.status_code, 401)

    def test_existing_record_job_rebuilds_typed_user_start_without_changing_authorization(self):
        self.run.refresh_from_db()
        before = (self.run.id, self.run.turn_id, self.run.authorization.digest,
                  self.run.authorization.signature, self.run.membership_ref)
        response = self.client.post("/internal/agent-run-lifecycle/resolve", content_type="application/json",
            data=json.dumps({"schema": "runtime.agent_run_lifecycle.resolve.v1",
                "jobId": "agent_run.lifecycle:" + self.run.pk, "agentRunId": self.run.pk,
                "authorizationDigest": self.run.authorization.digest}),
            HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN)
        self.assertEqual(response.status_code, 200, response.content)
        start = response.json()["agentRunStart"]
        self.assertEqual(start["schema"], "workspace.agent_run.start.v2")
        self.assertEqual(start["initialInput"], {"type": "userMessage"})
        self.assertEqual((start["agentRunId"], start["turnId"], start["authorizationDigest"],
                          start["authorizationSignature"], self.run.membership_ref), before)
