import os
import io
import json
import urllib.error
from contextlib import contextmanager
import unittest
from unittest.mock import patch

os.environ.update(RUNTIME_INTERNAL_URL="http://runtime.invalid", API_INTERNAL_URL="http://api.invalid",
                  INTERNAL_API_TOKEN="synthetic-internal-token")
import worker


@contextmanager
def healthy_lease(*args):
    yield lambda: None


class WorkReturnJobTests(unittest.TestCase):
    def job(self):
        return {"jobId": "agent_work.return:run_1", "jobKind": "agent_work.return",
                "sessionId": "session_1", "payloadRef": "record:agent_work:run_1",
                "idempotencyKey": "agent_work.return:run_1", "retryCount": 0, "maxRetries": 5}

    def invoke(self, response):
        with patch.object(worker, "start_job"), patch.object(worker, "lease_heartbeats", healthy_lease), \
                patch.object(worker, "api_request", return_value=response) as api, \
                patch.object(worker, "complete_job") as complete, patch.object(worker, "yield_job") as yielded:
            worker.execute_claimed_job(self.job(), "synthetic-lease-owner")
        return api, complete, yielded

    def test_completed_return_leaves_the_high_frequency_queue(self):
        api, complete, yielded = self.invoke({"schema": "workspace.agent_work.return_materialized.v1",
            "disposition": "duplicate", "notice": {"noticeId": "retained-notice"}})
        self.assertEqual(api.call_count, 1)
        self.assertEqual(api.call_args.args[0], "/internal/agent-work/returns/materialize")
        complete.assert_called_once_with("agent_work.return:run_1", "synthetic-lease-owner", [])
        yielded.assert_not_called()

    def test_unfinished_child_yields_without_failure_budget_or_worker_sleep(self):
        with patch.object(worker, "now_ms", return_value=1000), patch.object(worker, "fail_claimed_job") as failed, \
                patch.object(worker.time, "sleep", side_effect=AssertionError("worker must release its slot")):
            _, complete, yielded = self.invoke({"schema": "workspace.agent_work.return_materialized.v1",
                "disposition": "pending", "notice": None})
        complete.assert_not_called()
        failed.assert_not_called()
        self.assertEqual(yielded.call_count, 1)
        self.assertGreater(yielded.call_args.args[2], 1000)

    def test_dependency_failure_uses_the_existing_persistent_job_retry(self):
        with patch.object(worker, "start_job"), patch.object(worker, "lease_heartbeats", healthy_lease), \
                patch.object(worker, "api_request", side_effect=worker.DependencyUnavailable("lost response")), \
                patch.object(worker, "fail_claimed_job") as failed:
            worker.execute_claimed_job(self.job(), "synthetic-lease-owner")
        failed.assert_called_once_with(self.job(), "synthetic-lease-owner", "dependency_unavailable", True)

    def test_unstructured_server_failure_is_retryable_at_the_http_boundary(self):
        for payload in (b'{"error":"temporary_materialization_failure"}', b"Internal Server Error"):
            with self.subTest(payload=payload), patch.object(worker.urllib.request, "urlopen",
                    side_effect=urllib.error.HTTPError("http://api.invalid", 500, "Internal Server Error",
                                                      {}, io.BytesIO(payload))):
                with self.assertRaises(worker.DependencyUnavailable) as failure:
                    worker.api_request("/internal/agent-work/returns/materialize", {}, "return_unavailable")
                self.assertEqual(failure.exception.http_status, 500)

    def test_client_rejections_and_structured_permanent_failures_do_not_retry(self):
        structured = {"schema": "runtime.agent_run.step.failure.v1", "agentRunId": "run_1",
                      "failureClass": "permanent", "retryable": False,
                      "transitionReason": "runtime_step_failed", "error": "runtime_step_failed"}
        for status, payload, expected in ((400, {"error": "invalid_request"}, RuntimeError),
                (401, {"error": "unauthorized"}, RuntimeError),
                (409, {"error": "binding_invalid"}, RuntimeError),
                (500, structured, worker.RuntimeStepFailed),
                (500, {**structured, "unknown": True}, RuntimeError)):
            with self.subTest(status=status, payload=payload), patch.object(worker.urllib.request, "urlopen",
                    side_effect=urllib.error.HTTPError("http://api.invalid", status, "Rejected", {},
                                                      io.BytesIO(json.dumps(payload).encode()))):
                with self.assertRaises(expected) as failure:
                    worker.api_request("/internal/agent-work/returns/materialize", {}, "return_unavailable")
                self.assertNotIsInstance(failure.exception, worker.DependencyUnavailable)
                if expected is worker.RuntimeStepFailed:
                    self.assertFalse(failure.exception.retryable)

    def test_structured_server_failure_still_requires_the_requested_run_identity(self):
        payload = {"schema": "runtime.agent_run.step.failure.v1", "agentRunId": "other_run",
                   "failureClass": "permanent", "retryable": False,
                   "transitionReason": "runtime_step_failed", "error": "runtime_step_failed"}
        with patch.object(worker.urllib.request, "urlopen", side_effect=urllib.error.HTTPError(
                "http://runtime.invalid/agent-runs/step", 500, "Internal Server Error", {},
                io.BytesIO(json.dumps(payload).encode()))):
            with self.assertRaisesRegex(RuntimeError, "^runtime_step_failure_response_invalid$") as failure:
                worker.agent_run_step_request({"agentRunStart": {"agentRunId": "run_1"}})
            self.assertNotIsInstance(failure.exception, worker.DependencyUnavailable)

    def test_currently_unbound_source_is_not_completed_or_discarded(self):
        with patch.object(worker, "fail_claimed_job") as failed:
            _, complete, yielded = self.invoke({"schema": "workspace.agent_work.return_materialized.v1",
                "disposition": "notWork", "notice": None})
        complete.assert_not_called()
        failed.assert_not_called()
        self.assertEqual(yielded.call_count, 1)

    def test_generic_core_replay_is_rejected_before_publication(self):
        # Default runtime_ops replay artifact for deadLetterId=dead_return_run_1,
        # replayKey=manual-1000. Core retains kind/session/payload and changes both
        # the job identity and idempotency key; neither encodes a child suffix.
        replay = {**self.job(), "jobId": "runtime_job_replay:dead_return_run_1:manual-1000",
                  "idempotencyKey": "dlq_replay:dead_return_run_1:manual-1000", "maxRetries": 3}
        with patch.object(worker, "start_job"), patch.object(worker, "lease_heartbeats", healthy_lease), \
                patch.object(worker, "api_request", return_value={
                    "schema": "workspace.agent_work.return_materialized.v1", "disposition": "duplicate",
                    "notice": {"noticeId": "retained"}}) as api, \
                patch.object(worker, "complete_job") as complete, patch.object(worker, "fail_claimed_job") as failed:
            worker.execute_claimed_job(replay, "synthetic-replay-owner")
        failed.assert_called_once_with(replay, "synthetic-replay-owner", "work_return_job_binding_invalid", False)
        api.assert_not_called()
        complete.assert_not_called()

class WorkReturnAuditTests(unittest.TestCase):
    def page(self):
        return {"schema": "workspace.agent_work.return_audit.claimed.v1", "disposition": "claimed",
            "leaseOwner": "synthetic-audit-lease", "after": None, "through": "002", "next": None,
            "entries": [{"cursor": "001", "workAgentRunId": "run_1", "materialized": True},
                        {"cursor": "002", "workAgentRunId": "run_2", "materialized": False}]}

    def test_failed_schedule_retains_unattempted_cursor_for_next_audit(self):
        finished = []
        page = self.page()
        page["entries"][0]["materialized"] = False

        def request(path, body, *args, **kwargs):
            if path.endswith("claim"):
                return page
            if path.endswith("schedule"):
                raise worker.DependencyUnavailable("lost schedule response")
            finished.append(body)
            return {"schema": "workspace.agent_work.return_audit.finished.v1", "disposition": "recorded"}

        with patch.object(worker, "api_request", side_effect=request), patch.object(worker.time, "monotonic", return_value=1000):
            worker.WorkReturnAudit()()
        self.assertEqual(len(finished), 1)
        self.assertIsNone(finished[0]["after"])
        self.assertFalse(finished[0]["complete"])

    def test_known_rejected_binding_does_not_block_the_rest_of_the_audit(self):
        page = self.page()
        page["entries"][0]["materialized"] = False
        scheduled, finished = [], []

        def request(path, body, *args, **kwargs):
            if path.endswith("claim"):
                return page
            if path.endswith("schedule"):
                scheduled.append(body["workAgentRunId"])
                if body["workAgentRunId"] == "run_1":
                    error = RuntimeError("agent_work_return_binding_rejected")
                    error.http_status = 409
                    error.http_error_code = "agent_work_return_binding_rejected"
                    raise error
                return {"schema": "workspace.agent_work.return_scheduled.v1", "disposition": "inserted"}
            finished.append(body)
            return {"schema": "workspace.agent_work.return_audit.finished.v1", "disposition": "recorded"}

        with patch.object(worker, "api_request", side_effect=request), \
                patch.object(worker.time, "monotonic", return_value=1000), patch.object(worker, "print") as diagnostic:
            worker.WorkReturnAudit()()
        self.assertEqual(scheduled, ["run_1", "run_2"])
        self.assertEqual(finished[0]["after"], "002")
        self.assertTrue(finished[0]["complete"])
        self.assertIn("run_1", diagnostic.call_args.args[0])
        self.assertIn("409", diagnostic.call_args.args[0])

    def test_authentication_failure_keeps_cursor_before_unscheduled_work(self):
        page = self.page()
        page["entries"][0]["materialized"] = False
        finished = []

        def request(path, body, *args, **kwargs):
            if path.endswith("claim"):
                return page
            if path.endswith("schedule"):
                error = RuntimeError("internal_authentication_rejected")
                error.http_status = 401
                raise error
            finished.append(body)
            return {"schema": "workspace.agent_work.return_audit.finished.v1", "disposition": "recorded"}

        with patch.object(worker, "api_request", side_effect=request), patch.object(worker.time, "monotonic", return_value=1000):
            worker.WorkReturnAudit()()
        self.assertIsNone(finished[0]["after"])
        self.assertFalse(finished[0]["complete"])

    def test_real_http_error_advances_only_the_exact_known_binding_rejection(self):
        payloads = [{"error": "agent_work_return_binding_rejected"},
                    {"error": "agent_work_return_binding_rejected", "unknown": True},
                    {"error": "unknown_binding_failure"}]
        real_api = worker.api_request
        for payload in payloads:
            page = self.page()
            page["entries"][0]["materialized"] = False
            scheduled, finished = [], []
            def transport(request, **kwargs):
                run = json.loads(request.data)["workAgentRunId"]
                scheduled.append(run)
                if run == "run_1":
                    raise urllib.error.HTTPError(request.full_url, 409, "Conflict", {},
                                                 io.BytesIO(json.dumps(payload).encode()))
                return io.BytesIO(json.dumps({"schema": "workspace.agent_work.return_scheduled.v1",
                                              "disposition": "inserted"}).encode())
            def api(path, body, *args, **kwargs):
                if path.endswith("claim"):
                    return page
                if path.endswith("finish"):
                    finished.append(body)
                    return {"schema": "workspace.agent_work.return_audit.finished.v1", "disposition": "recorded"}
                return real_api(path, body, *args, **kwargs)
            with self.subTest(payload=payload), patch.object(worker, "api_request", side_effect=api), \
                    patch.object(worker.urllib.request, "urlopen", side_effect=transport), \
                    patch.object(worker.time, "monotonic", return_value=0):
                worker.WorkReturnAudit()()
            if payload == payloads[0]:
                self.assertEqual(scheduled, ["run_1", "run_2"])
                self.assertEqual(finished[0]["after"], "002")
                self.assertTrue(finished[0]["complete"])
            else:
                self.assertEqual(scheduled, ["run_1"])
                self.assertIsNone(finished[0]["after"])
                self.assertFalse(finished[0]["complete"])
