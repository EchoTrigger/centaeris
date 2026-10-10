"""An API refusal before business execution yields without spending job retries."""

from contextlib import contextmanager
import io
import json
import os
import unittest
import urllib.error
from unittest.mock import Mock, patch

os.environ.update(RUNTIME_INTERNAL_URL="http://runtime.invalid", API_INTERNAL_URL="http://api.invalid",
                  INTERNAL_API_TOKEN="synthetic-upload-capacity-token")
import worker


@contextmanager
def healthy_lease(*_args):
    yield lambda: None


class UploadCapacityWorkerTests(unittest.TestCase):
    def setUp(self):
        digest = "sha256:" + "a" * 64
        self.job = {
            "jobId": "agent_run.lifecycle:upload-wait-run", "jobKind": "agent_run.lifecycle",
            "sessionId": "upload-wait-session", "payloadRef": "record:agent_run:upload-wait-run",
            "idempotencyKey": "agent_run.lifecycle:upload-wait-run:" + digest,
        }
        self.start = {"agentRunId": "upload-wait-run", "authorizationDigest": digest}

    def execute(self, status, payload, *, error_url=None, runtime_step=False):
        raw = payload if isinstance(payload, bytes) else json.dumps(payload).encode()
        stream = io.BytesIO(raw)
        url = error_url or worker.API_INTERNAL_URL + "/internal/agent-run-lifecycle/resolve"
        error = urllib.error.HTTPError(url, status, "synthetic refusal", {}, stream)
        successful = Mock()
        successful.__enter__ = Mock(return_value=successful)
        successful.__exit__ = Mock(return_value=False)
        successful.read.return_value = json.dumps({
            "schema": "runtime.agent_run_lifecycle.resolved.v1", "disposition": "ready",
            "agentRunStart": self.start,
        }).encode()
        outcomes = [successful, error] if runtime_step else error
        with (patch.object(worker, "start_job"), patch.object(worker, "lease_heartbeats", healthy_lease),
              patch.object(worker, "transition_agent_run") as transition,
              patch.object(worker.urllib.request, "urlopen", side_effect=outcomes) as transport,
              patch.object(worker, "now_ms", return_value=1000),
              patch.object(worker, "yield_job") as yielded,
              patch.object(worker, "complete_job") as completed,
              patch.object(worker, "fail_claimed_job") as failed):
            worker.execute_claimed_job(self.job, "synthetic-lease-owner")
        self.assertTrue(stream.closed, "HTTP error response was not closed")
        completed.assert_not_called()
        return yielded, failed, transition, transport

    def test_exact_api_upload_capacity_refusal_yields_without_failure_or_terminal_transition(self):
        yielded, failed, transition, transport = self.execute(429, {"error": "upload_capacity_exhausted"})
        failed.assert_not_called()
        transition.assert_not_called()
        yielded.assert_called_once_with(
            self.job["jobId"], "synthetic-lease-owner",
            1000 + worker.AGENT_RUN_LIFECYCLE_RECHECK_MS, "upload_capacity_wait",
        )
        self.assertEqual(transport.call_args.args[0].full_url,
                         worker.API_INTERNAL_URL + "/internal/agent-run-lifecycle/resolve")

    def test_other_or_malformed_api_429_responses_keep_existing_failure_behavior(self):
        for payload in ({"error": "other_capacity_error"},
                        {"error": "upload_capacity_exhausted", "unknown": True},
                        b"not json", ["upload_capacity_exhausted"]):
            with self.subTest(payload=payload):
                yielded, failed, _transition, _transport = self.execute(429, payload)
                yielded.assert_not_called()
                failed.assert_called_once_with(self.job, "synthetic-lease-owner", "agent_run_lifecycle_failed", False)

    def test_runtime_429_is_not_classified_as_api_ingress_capacity(self):
        yielded, failed, transition, transport = self.execute(
            429, {"error": "upload_capacity_exhausted"}, runtime_step=True,
            error_url=worker.RUNTIME_INTERNAL_URL + "/agent-runs/step",
        )
        yielded.assert_not_called()
        failed.assert_called_once_with(self.job, "synthetic-lease-owner", "agent_run_lifecycle_failed", False)
        self.assertEqual(transition.call_args.args[1], "running")
        self.assertEqual(transport.call_args.args[0].full_url, worker.RUNTIME_INTERNAL_URL + "/agent-runs/step")

    def test_an_api_request_redirected_to_runtime_does_not_gain_capacity_yield(self):
        yielded, failed, _transition, _transport = self.execute(
            429, {"error": "upload_capacity_exhausted"},
            error_url=worker.RUNTIME_INTERNAL_URL + "/internal/agent-run-lifecycle/resolve",
        )
        yielded.assert_not_called()
        failed.assert_called_once_with(self.job, "synthetic-lease-owner", "agent_run_lifecycle_failed", False)

    def test_api_502_keeps_existing_retryable_dependency_failure(self):
        yielded, failed, _transition, _transport = self.execute(502, {"error": "upload_capacity_exhausted"})
        yielded.assert_not_called()
        failed.assert_called_once_with(self.job, "synthetic-lease-owner", "dependency_unavailable", True)
