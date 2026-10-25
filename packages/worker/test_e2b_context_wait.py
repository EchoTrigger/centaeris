import unittest
from contextlib import ExitStack, contextmanager
from unittest.mock import Mock, patch

from test_worker import lifecycle_fixture, worker


E2B_CONTEXT_WAIT_REASONS = (
    "e2b_environment_capacity_wait",
    "e2b_live_context_capacity_wait",
    "e2b_environment_already_active",
    "e2b_step_already_active",
)


def step_wait(start, reason):
    return {
        "schema": "runtime.agent_run.step.result.v1",
        "agentRunId": start["agentRunId"],
        "disposition": "waiting",
        "terminalState": None,
        "transitionReason": reason,
    }


class E2bContextWaitTests(unittest.TestCase):
    def test_busy_context_yields_same_job_without_consuming_failure_retries(self):
        for reason in E2B_CONTEXT_WAIT_REASONS:
            with self.subTest(reason=reason):
                job, start = lifecycle_fixture()
                job.update(retryCount=10, maxRetries=10)
                healthy = Mock()

                @contextmanager
                def heartbeat(*args):
                    yield healthy

                with ExitStack() as stack:
                    stack.enter_context(patch.object(worker, "api_request", return_value={
                        "schema": "runtime.agent_run_lifecycle.resolved.v1",
                        "disposition": "ready", "agentRunStart": start,
                    }))
                    stack.enter_context(patch.object(worker, "start_job"))
                    stack.enter_context(patch.object(worker, "lease_heartbeats", heartbeat))
                    stack.enter_context(patch.object(worker, "agent_run_step_request",
                        return_value=step_wait(start, reason)))
                    stack.enter_context(patch.object(worker, "transition_agent_run"))
                    stack.enter_context(patch.object(worker, "now_ms", return_value=1200))
                    stack.enter_context(patch.object(worker,
                        "AGENT_RUN_LIFECYCLE_RECHECK_MS", 30_000))
                    yielded = stack.enter_context(patch.object(worker, "yield_job"))
                    forbidden = [stack.enter_context(patch.object(worker, name)) for name in (
                        "fail_job", "fail_claimed_job", "complete_job",
                        "runtime_teardown_request", "finish_environment_agent_run",
                    )]
                    owners = ("worker:first-claim", "worker:second-claim")
                    for owner in owners:
                        worker.execute_claimed_job(job, owner)
                    self.assertEqual([call.args for call in yielded.call_args_list], [
                        (job["jobId"], owner, 31_200, reason) for owner in owners
                    ])
                    self.assertEqual(job["retryCount"], 10)
                    self.assertGreaterEqual(healthy.call_count, len(owners))
                    for call in forbidden:
                        call.assert_not_called()

    def test_unknown_context_wait_remains_a_protocol_rejection(self):
        job, start = lifecycle_fixture()
        with patch.object(worker, "api_request", return_value={
                "schema": "runtime.agent_run_lifecycle.resolved.v1",
                "disposition": "ready", "agentRunStart": start,
            }), patch.object(worker, "agent_run_step_request",
                return_value=step_wait(start, "e2b_environment_missing")), \
                patch.object(worker, "transition_agent_run"), \
                patch.object(worker, "yield_job") as yielded:
            with self.assertRaisesRegex(RuntimeError, "agent_run_step_response_invalid"):
                worker.execute_agent_run_lifecycle_job(job, "worker:current", lambda: None)
            yielded.assert_not_called()


if __name__ == "__main__":
    unittest.main()
