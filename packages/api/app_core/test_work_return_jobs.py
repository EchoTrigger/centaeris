"""Recover return publication through real HTTP and persistent Core jobs."""
from copy import deepcopy
from unittest.mock import patch
import queue

from django.test import LiveServerTestCase, override_settings

from . import test_agent_work as work_fixture
from . import test_agent_work_returns as return_fixture
from . import test_native_consumption_production as runtime_fixture
from .agent_work_return_jobs import schedule_work_return_job
from .models import AgentWorkReturn
from .test_native_consumption_runtime import isolate_runtime_schema


class WorkReturnJobRecoveryTests(LiveServerTestCase):
    serialized_rollback = True
    dependencies = work_fixture.AgentWorkTests.dependencies
    request_fact = work_fixture.AgentWorkTests.request_fact
    post = work_fixture.AgentWorkTests.post
    child = return_fixture.AgentWorkReturnTests.child
    terminal = return_fixture.AgentWorkReturnTests.terminal
    start_runtime = runtime_fixture.NativeConsumptionProductionTests.start_runtime
    stop_runtime = runtime_fixture.NativeConsumptionProductionTests.stop_runtime
    load_worker = runtime_fixture.NativeConsumptionProductionTests.load_worker

    def setUp(self):
        work_fixture.AgentWorkTests.setUp(self)
        isolate_runtime_schema(self)
        self.runtime_lines = []
        self.runtime_markers = queue.Queue()
        self.requests, self.steps, self.automatic_admissions = [], [], []

    def test_one_http_500_recovers_the_same_persisted_job_and_one_notice(self):
        runtime_url = self.start_runtime()
        worker = self.load_worker(runtime_url)
        with override_settings(RUNTIME_URL=runtime_url):
            child = self.child()
            job_id = "agent_work.return:" + child.pk
            self.assertEqual(schedule_work_return_job(child), "existing")

            def current():
                return worker.runtime_request("/internal/jobs/" + job_id)["job"]

            self.assertEqual((current()["status"], current()["retryCount"]), ("queued", 0))

            def claim(at):
                return worker.runtime_request("/internal/jobs/claim", {
                    "schema": "runtime.job.claim.v1", "workerId": "worker:return-recovery-test",
                    "jobId": job_id, "jobKind": "agent_work.return", "nowMs": at,
                    "leaseMs": 300000, "limit": 1})["jobs"]

            def execute_at(at):
                with patch.object(worker, "now_ms", return_value=at):
                    jobs = claim(at)
                    self.assertEqual(len(jobs), 1)
                    job = jobs[0]
                    self.assertEqual((job["jobId"], job["sessionId"], job["payloadRef"], job["idempotencyKey"]),
                        (job_id, child.session_id, "record:agent_work:" + child.pk, job_id))
                    worker.execute_claimed_job(job, job["leaseOwner"])
                    return job

            # A legal pending response releases the slot without spending retries.
            first_at = max(worker.now_ms(), current()["runAtMs"])
            execute_at(first_at)
            pending = current()
            self.assertEqual((pending["status"], pending["retryCount"]), ("queued", 0))
            self.assertGreater(pending["runAtMs"], first_at)
            terminal = self.terminal(child)

            # This unexpected storage exception becomes an actual LiveServer HTTP
            # 500. The worker transport and Core /jobs/fail route remain real.
            with patch("app_core.agent_work_returns.materialize_work_return",
                       side_effect=OSError("synthetic one-request storage outage")) as failed_service:
                execute_at(pending["runAtMs"])
            self.assertEqual(failed_service.call_count, 1)
            retry = current()
            self.assertEqual((retry["status"], retry["retryCount"], retry["lastError"]),
                             ("queued", 1, "dependency_unavailable"))
            self.assertGreater(retry["runAtMs"], pending["runAtMs"])
            self.assertGreater(retry["maxRetries"], retry["retryCount"])
            self.assertEqual(claim(retry["runAtMs"] - 1), [])
            self.assertFalse(AgentWorkReturn.objects.filter(child_run=child).exists())
            self.assertEqual(schedule_work_return_job(child), "existing")
            self.assertEqual(current(), retry)

            # Advance only the public request clock to the persisted due time;
            # no queue rows, retry counters, or failure functions are mocked.
            reclaimed = execute_at(retry["runAtMs"])
            self.assertEqual(reclaimed["retryCount"], 1)
            self.assertEqual(reclaimed["jobId"], job_id)
            self.assertEqual((current()["status"], current()["retryCount"]), ("succeeded", 1))
            notice = AgentWorkReturn.objects.get(child_run=child)
            payload = deepcopy(notice.payload)
            self.assertEqual(payload["terminal"]["factRef"], terminal.eventId)
            self.assertEqual(schedule_work_return_job(child), "delivered")
            self.assertEqual(claim(retry["runAtMs"] + 300000), [])
            duplicate = worker.api_request("/internal/agent-work/returns/materialize", {
                "schema": "workspace.agent_work.return_materialize.v1", "workAgentRunId": child.pk},
                "return_unavailable")
            self.assertEqual((duplicate["disposition"], duplicate["notice"]), ("duplicate", payload))
            self.assertEqual(AgentWorkReturn.objects.filter(child_run=child).count(), 1)
