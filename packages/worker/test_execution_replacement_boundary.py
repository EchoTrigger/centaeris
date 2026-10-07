"""Protect the worker dispatch boundary that makes recovery change job leases."""

import http.client
import io
import json
import os
import unittest
from contextlib import contextmanager
from unittest.mock import patch
from urllib.parse import urlsplit

os.environ.update(
    RUNTIME_INTERNAL_URL="http://runtime.invalid",
    API_INTERNAL_URL="http://api.invalid",
    INTERNAL_API_TOKEN="test-internal-token",
)

import worker


class ExecutionReplacementBoundaryTests(unittest.TestCase):
    def setUp(self):
        self.owner = "claim:execution-replacement-test"
        self.start = {
            "agentRunId": "agent_run_replacement",
            "turnId": "turn_replacement",
            "authorizationDigest": "sha256:" + "a" * 64,
        }
        self.job = {
            "jobId": "agent_run.lifecycle:agent_run_replacement",
            "jobKind": "agent_run.lifecycle",
            "sessionId": "session_replacement",
            "payloadRef": "record:agent_run:agent_run_replacement",
            "idempotencyKey": "agent_run.lifecycle:agent_run_replacement:" + self.start["authorizationDigest"],
            "retryCount": 0,
            "maxRetries": 2,
        }
        self.calls = []

    @contextmanager
    def heartbeats(self, job_id, owner):
        self.assertEqual((job_id, owner), (self.job["jobId"], self.owner))
        try:
            yield lambda: None
        finally:
            self.calls.append(("heartbeat_stopped", None))

    def run_attempt(self, step_result):
        def request(request, *, timeout):
            path = urlsplit(request.full_url).path
            body = json.loads(request.data)
            self.calls.append((path, body))
            if path == "/internal/agent-run-lifecycle/resolve":
                result = {
                    "schema": "runtime.agent_run_lifecycle.resolved.v1",
                    "disposition": "ready",
                    "agentRunStart": self.start,
                }
            elif path == "/internal/agent-runs/transition":
                self.assertEqual(body["state"], "running")
                result = {"agentRunId": self.start["agentRunId"], "state": "running"}
            elif path == "/agent-runs/step":
                self.assertIsNone(timeout)
                self.assertEqual(body["leaseOwner"], self.owner)
                if isinstance(step_result, Exception):
                    raise step_result
                result = step_result
            elif path == "/internal/jobs/fail":
                result = {"disposition": "retry_scheduled"}
            elif path in {"/internal/jobs/start", "/internal/jobs/yield"}:
                result = {}
            else:
                self.fail(f"Unexpected call during a single claimed attempt: {path}")
            return io.BytesIO(json.dumps(result).encode())

        with (
            patch.object(worker.urllib.request, "urlopen", side_effect=request),
            patch.object(worker, "lease_heartbeats", side_effect=self.heartbeats),
            patch.object(worker, "now_ms", return_value=1_000_000),
        ):
            worker.execute_claimed_job(self.job, self.owner)

    def test_recovery_yields_once_after_step_and_exits_the_claimed_attempt(self):
        reason = "execution_recovery_checkpoint_committed"
        self.run_attempt({
            "schema": "runtime.agent_run.step.result.v1",
            "agentRunId": self.start["agentRunId"],
            "disposition": "waiting",
            "terminalState": None,
            "transitionReason": reason,
            "retryAtMs": 1_001_200,
        })
        self.assertEqual([path for path, _ in self.calls], [
            "/internal/jobs/start",
            "/internal/agent-run-lifecycle/resolve",
            "/internal/agent-runs/transition",
            "/agent-runs/step",
            "/internal/agent-runs/transition",
            "/internal/jobs/yield",
            "heartbeat_stopped",
        ])
        self.assertEqual(self.calls[-2][1], {
            "schema": "runtime.job.yield.v1",
            "jobId": self.job["jobId"],
            "leaseOwner": self.owner,
            "yieldedAtMs": 1_000_000,
            "runAtMs": 1_001_200,
            "transitionReason": reason,
        })

    def test_unknown_step_outcome_schedules_job_retry_without_resending_step(self):
        for failure in (TimeoutError("response lost"), http.client.RemoteDisconnected("response lost")):
            with self.subTest(failure=type(failure).__name__):
                self.calls.clear()
                self.run_attempt(failure)
                self.assertEqual([path for path, _ in self.calls], [
                    "/internal/jobs/start",
                    "/internal/agent-run-lifecycle/resolve",
                    "/internal/agent-runs/transition",
                    "/agent-runs/step",
                    "heartbeat_stopped",
                    "/internal/jobs/fail",
                ])
                self.assertEqual(self.calls[-1][1], {
                    "schema": "runtime.job.fail.v1",
                    "jobId": self.job["jobId"],
                    "leaseOwner": self.owner,
                    "failedAtMs": 1_000_000,
                    "error": "dependency_unavailable",
                    "retryable": True,
                })


if __name__ == "__main__":
    unittest.main()
