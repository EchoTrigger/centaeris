"""Automatic first admission reads committed notices, never terminal status as handling."""
from concurrent.futures import ThreadPoolExecutor
from contextlib import contextmanager
import json
import threading
from unittest.mock import patch

from django.conf import settings
from django.db import close_old_connections, connection, transaction
from django.test import Client, LiveServerTestCase
from django.test.utils import CaptureQueriesContext

from . import test_agent_work_consumption as consume_fixture
from . import test_native_consumption_production as native_fixture
from .models import AgentRun, AgentWorkConsumeAttempt, AgentWorkReturn, HostedOperationReceipt, WorkspaceMembership
from .test_agent_work import PROFILE


class AutomaticWorkConsumptionTests(LiveServerTestCase):
    serialized_rollback = True
    setUp = consume_fixture.AgentWorkConsumptionTests.setUp
    dependencies = consume_fixture.AgentWorkConsumptionTests.dependencies
    request_fact = consume_fixture.AgentWorkConsumptionTests.request_fact
    post = consume_fixture.AgentWorkConsumptionTests.post
    child = consume_fixture.AgentWorkConsumptionTests.child
    terminal = consume_fixture.AgentWorkConsumptionTests.terminal
    publish = consume_fixture.AgentWorkConsumptionTests.publish
    notice = consume_fixture.AgentWorkConsumptionTests.notice
    consume = consume_fixture.AgentWorkConsumptionTests.consume
    load_worker = native_fixture.NativeConsumptionProductionTests.load_worker

    def discover(self, after=None, through=None, limit=100, **changes):
        return self.client.post("/internal/agent-work/returns/consume/discover", content_type="application/json",
            data=json.dumps({"schema": "workspace.agent_work.consume_discover.v1", "limit": limit,
                             "after": after, "through": through, **changes}),
            HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN)

    @contextmanager
    def runtime_boundary(self):
        # Only the Runtime boundary is synthetic here; worker -> API HTTP,
        # authority locks and all admissions/receipts are real PostgreSQL facts.
        def create(session_id, input_id, source, content):
            return {"inputId": input_id, "messageId": "fixture-native:" + input_id,
                    "source": source, "content": content}
        with patch("app_core.agent_work_consumption.request_execution_profile", return_value=PROFILE), \
             patch("app_core.agent_work_consumption.request_host_event_input", side_effect=create), \
             patch("app_core.agent_work_consumption.schedule_agent_run_lifecycle", return_value="inserted"):
            yield

    def test_discovery_reads_committed_notice_ledger_without_admitting_or_using_runtime(self):
        notice = self.notice()
        before = (AgentRun.objects.count(), HostedOperationReceipt.objects.count())
        with CaptureQueriesContext(connection) as queries, \
             patch("app_core.agent_work_consumption.request_execution_profile", side_effect=AssertionError("read only")):
            response = self.discover()
        discovery_sql = [query["sql"] for query in queries]
        self.assertEqual(response.status_code, 200, response.content)
        entry = response.json()["entries"][0]
        self.assertEqual((entry["cursor"], entry["noticeId"]), (notice["noticeId"], notice["noticeId"]))
        self.assertEqual(self.discover().json()["entries"][0]["operationId"], entry["operationId"])
        self.assertEqual((AgentRun.objects.count(), HostedOperationReceipt.objects.count()), before)
        self.assertFalse(AgentWorkConsumeAttempt.objects.exists())
        self.assertFalse(any(sql.lstrip().upper().startswith(("INSERT", "UPDATE", "DELETE")) for sql in discovery_sql))
        self.assertTrue(any("REPEATABLE READ, READ ONLY" in sql for sql in discovery_sql))

    def test_uncommitted_notice_is_invisible_then_restart_admits_it_once(self):
        child = self.child()
        self.terminal(child)
        AgentRun.objects.filter(pk__in=[child.pk, self.run.pk]).update(status="completed")
        notice = self.publish(child.pk).json()["notice"]
        retained = AgentWorkReturn.objects.get(pk=notice["noticeId"])
        retained.delete()
        def outside():
            close_old_connections()
            try:
                return Client().post("/internal/agent-work/returns/consume/discover", content_type="application/json",
                    data=json.dumps({"schema": "workspace.agent_work.consume_discover.v1", "limit": 100,
                                     "after": None, "through": None}),
                    HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN)
            finally:
                close_old_connections()
        with transaction.atomic():
            AgentWorkReturn.objects.create(id=notice["noticeId"], work=retained.work, child_run=child,
                fact_kind=retained.fact_kind, fact_ref=retained.fact_ref, payload=notice)
            with ThreadPoolExecutor(max_workers=1) as pool:
                invisible = pool.submit(outside).result(timeout=10)
            self.assertEqual(invisible.status_code, 200, invisible.content)
            self.assertEqual(invisible.json()["entries"], [])
        worker = self.load_worker("http://runtime.invalid")
        with self.runtime_boundary():
            worker.WorkReturnConsumer()()
            attempt = AgentWorkConsumeAttempt.objects.get()
            facts = (attempt.pk, attempt.coordinator_run_id, attempt.operation_id)
            worker.WorkReturnConsumer()()
        self.assertEqual((attempt.notice_id, json.loads(attempt.input_binding["initialInput"]["input"]["content"])),
                         (notice["noticeId"], notice))
        self.assertEqual(list(AgentWorkConsumeAttempt.objects.values_list("id", "coordinator_run_id", "operation_id")), [facts])

    def test_active_intake_preserves_parallel_child_sessions_and_one_coordinator(self):
        notices = [self.notice("parallel-one", idle=False), self.notice("parallel-two", idle=False)]
        worker = self.load_worker("http://runtime.invalid")
        with self.runtime_boundary():
            worker.WorkReturnConsumer()()
            worker.WorkReturnConsumer()()
        self.assertEqual(AgentWorkConsumeAttempt.objects.count(), 2)
        self.assertEqual(set(AgentWorkConsumeAttempt.objects.values_list("coordinator_run_id", flat=True)), {self.run.pk})
        self.assertNotEqual(notices[0]["identity"]["workSessionId"], notices[1]["identity"]["workSessionId"])
        self.assertEqual(self.source.agent_runs.filter(status__in=["queued", "running"]).count(), 1)

    def test_failed_cancelled_and_final_attempts_never_gain_a_second_automatic_root(self):
        worker = self.load_worker("http://runtime.invalid")
        with self.runtime_boundary():
            for index, state in enumerate(("failed", "cancelled", "completed")):
                notice = self.notice("terminal-" + str(index))
                worker.WorkReturnConsumer()()
                attempt = AgentWorkConsumeAttempt.objects.get(notice_id=notice["noticeId"])
                AgentRun.objects.filter(pk=attempt.coordinator_run_id).update(status=state)
                worker.WorkReturnConsumer()()
                self.assertEqual(AgentWorkConsumeAttempt.objects.filter(notice_id=notice["noticeId"]).count(), 1)
                self.assertEqual(self.discover().json()["entries"], [])
        self.assertEqual(AgentWorkConsumeAttempt.objects.count(), 3)
        for attempt in AgentWorkConsumeAttempt.objects.all():
            self.assertEqual(json.loads(attempt.input_binding["initialInput"]["input"]["content"])["noticeId"], attempt.notice_id)
        self.assertEqual(AgentWorkReturn.objects.count(), 3)

    def test_worker_restart_binds_closed_queue_acceptance_once_after_terminal(self):
        from .models import AgentInputQueue
        notice = self.notice(idle=False)
        AgentInputQueue.objects.create(agent_run=self.run, authorization_digest=self.run.authorization.digest,
            accepting=False, closed_reason="safe_point_closed", closed_at_ms=1)
        with self.runtime_boundary():
            self.load_worker("http://runtime.invalid").WorkReturnConsumer()()
            attempt = AgentWorkConsumeAttempt.objects.get()
            self.assertIsNone(attempt.coordinator_run_id)
            acceptance = (attempt.pk, attempt.operation_id, attempt.input_binding["initialInput"])
            AgentRun.objects.filter(pk=self.run.pk).update(status="completed")
            self.load_worker("http://runtime.invalid").WorkReturnConsumer()()
            self.load_worker("http://runtime.invalid").WorkReturnConsumer()()
        attempt.refresh_from_db()
        self.assertEqual((attempt.pk, attempt.operation_id, attempt.input_binding["initialInput"]), acceptance)
        self.assertIsNotNone(attempt.coordinator_run_id)
        self.assertEqual(AgentWorkConsumeAttempt.objects.count(), 1)
        self.assertEqual(self.source.agent_runs.filter(status__in=["queued", "running"]).count(), 1)
        self.assertEqual(self.discover().json()["entries"], [])

    def test_two_workers_and_lost_admission_response_preserve_one_committed_attempt(self):
        notice = self.notice()
        workers = [self.load_worker("http://runtime.invalid") for _ in range(2)]
        barrier = threading.Barrier(2)
        def race(worker):
            close_old_connections()
            try:
                original = worker.api_request
                def rpc(path, body, reason, **kwargs):
                    if path.endswith("/consume/discover"):
                        page = original(path, body, reason, **kwargs)
                        barrier.wait(10)
                        return page
                    result = original(path, body, reason, **kwargs)
                    if path.endswith("/consume"):
                        raise OSError("synthetic response loss after real admission")
                    return result
                with patch.object(worker, "api_request", side_effect=rpc):
                    worker.WorkReturnConsumer()()
            finally:
                close_old_connections()
        with self.runtime_boundary(), ThreadPoolExecutor(max_workers=2) as pool:
            futures = [pool.submit(race, worker) for worker in workers]
            for future in futures:
                future.result(timeout=30)
            attempt = AgentWorkConsumeAttempt.objects.get()
            identity = (attempt.pk, attempt.coordinator_run_id, attempt.operation_id)
            self.load_worker("http://runtime.invalid").WorkReturnConsumer()()
        self.assertEqual(attempt.notice_id, notice["noticeId"])
        self.assertEqual(list(AgentWorkConsumeAttempt.objects.values_list("id", "coordinator_run_id", "operation_id")), [identity])

    def test_frozen_page_advances_past_denied_source_and_revisits_only_unattempted_notices(self):
        notices = [self.notice("page-" + str(index)) for index in range(3)]
        ids = sorted(notice["noticeId"] for notice in notices)
        first = self.discover(limit=1).json()
        self.assertEqual((first["entries"][0]["noticeId"], first["next"], first["through"]), (ids[0], ids[0], ids[-1]))
        worker = self.load_worker("http://runtime.invalid")
        original = worker.api_request
        denied = ids[0]
        attempted = []
        def rpc(path, body, reason, **kwargs):
            if path.endswith("/consume/discover"):
                body = {**body, "limit": 1}
            if path.endswith("/consume"):
                attempted.append(body["noticeId"])
                if body["noticeId"] == denied:
                    raise worker.DependencyUnavailable("synthetic denied first notice")
            return original(path, body, reason, **kwargs)
        scan = worker.WorkReturnConsumer()
        with self.runtime_boundary(), patch.object(worker, "api_request", side_effect=rpc):
            scan()
            upper = scan.through
            self.assertEqual((scan.after, upper), (ids[0], ids[-1]))
            scan()
            accepted = AgentWorkConsumeAttempt.objects.get()
            self.assertEqual(accepted.notice_id, ids[1])
            AgentRun.objects.filter(pk=accepted.coordinator_run_id).update(status="failed")
            scan()
            self.assertIsNone(scan.through)
            scan()
        self.assertEqual(attempted, [ids[0], ids[1], ids[2], ids[0]])
        self.assertEqual(AgentWorkConsumeAttempt.objects.count(), 2)
        self.assertEqual(self.discover().json()["entries"][0]["noticeId"], denied)

    def test_revoked_original_membership_and_strict_transport_cannot_admit(self):
        notice = self.notice()
        self.membership.delete()
        WorkspaceMembership.objects.create(workspace=self.workspace, user=self.user, role="owner")
        with self.runtime_boundary():
            self.load_worker("http://runtime.invalid").WorkReturnConsumer()()
        self.assertFalse(AgentWorkConsumeAttempt.objects.exists())
        self.assertEqual(self.discover().json()["entries"][0]["noticeId"], notice["noticeId"])
        for changes in ({"limit": True}, {"limit": 0}, {"after": "x", "through": None},
                        {"after": "z", "through": "a"}, {"notice_id": notice["noticeId"]}):
            self.assertEqual(self.discover(**changes).status_code, 400)
        request = {"schema": "workspace.agent_work.consume_discover.v1", "limit": 1, "after": None, "through": None}
        self.assertEqual(self.client.post("/internal/agent-work/returns/consume/discover",
            data=json.dumps(request), content_type="application/json").status_code, 401)
