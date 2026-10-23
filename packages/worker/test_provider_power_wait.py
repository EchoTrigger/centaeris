import unittest
from contextlib import ExitStack, contextmanager
from unittest.mock import Mock, patch

from test_worker import lifecycle_fixture, worker


def power_wait():
    return {
        "schema": "runtime.agent_run_lifecycle.resolved.v1",
        "disposition": "waiting",
        "waitReason": "environment_power_pending",
    }


class ProviderPowerWaitTests(unittest.TestCase):
    def test_resolve_accepts_exact_power_wait_without_execution_authority(self):
        job, start = lifecycle_fixture()
        response = power_wait()
        with patch.object(worker, "api_request", return_value=response):
            self.assertEqual(worker.resolve_agent_run_lifecycle(
                job, "worker:power-wait", start["agentRunId"],
                start["authorizationDigest"]), response)

    def test_wait_response_rejects_unknown_reason_missing_fields_and_authority(self):
        job, start = lifecycle_fixture()
        invalid = [
            {key: value for key, value in power_wait().items() if key != missing}
            for missing in power_wait()
        ]
        invalid.extend([
            {**power_wait(), "waitReason": "environment_busy"},
            {**power_wait(), "waitReason": None},
            {**power_wait(), "waitReason": True},
            {**power_wait(), "schema": "runtime.agent_run_lifecycle.resolved.v2"},
        ])
        invalid.extend({**power_wait(), name: value} for name, value in (
            ("agentRunStart", start), ("controllerEpoch", 1),
            ("agentRunCleanup", {}), ("terminalState", None),
            ("retryAtMs", 1000), ("unknown", True),
        ))
        for response in invalid:
            with self.subTest(response=response), \
                    patch.object(worker, "api_request", return_value=response):
                with self.assertRaisesRegex(
                        RuntimeError, "agent_run_lifecycle_resolve_response_invalid"):
                    worker.resolve_agent_run_lifecycle(
                        job, "worker:power-wait", start["agentRunId"],
                        start["authorizationDigest"])

    def test_wait_yields_same_job_after_lease_check_without_changing_run(self):
        job, _ = lifecycle_fixture()
        order = []
        healthy = Mock(side_effect=lambda: order.append("healthy"))
        with ExitStack() as stack:
            stack.enter_context(patch.object(worker, "api_request", return_value=power_wait()))
            stack.enter_context(patch.object(worker, "now_ms", return_value=1200))
            stack.enter_context(patch.object(worker, "AGENT_RUN_LIFECYCLE_RECHECK_MS", 30_000))
            yielded = stack.enter_context(patch.object(
                worker, "yield_job", side_effect=lambda *args: order.append("yield")))
            forbidden = [stack.enter_context(patch.object(worker, name)) for name in (
                "transition_agent_run", "agent_run_step_request",
                "runtime_teardown_request", "finish_environment_agent_run",
                "complete_job", "fail_job", "fail_claimed_job",
            )]
            self.assertFalse(worker.execute_agent_run_lifecycle_job(
                job, "worker:power-wait", healthy))
            yielded.assert_called_once_with(
                job["jobId"], "worker:power-wait", 31_200,
                "environment_power_pending")
            self.assertEqual(order, ["healthy", "yield"])
            for call in forbidden:
                call.assert_not_called()

    def test_repeated_waits_release_claims_without_using_failure_retries(self):
        job, _ = lifecycle_fixture()
        job.update(retryCount=10, maxRetries=10)
        healthy = Mock()

        @contextmanager
        def heartbeat(*args):
            yield healthy

        with ExitStack() as stack:
            stack.enter_context(patch.object(worker, "api_request", return_value=power_wait()))
            stack.enter_context(patch.object(worker, "start_job"))
            stack.enter_context(patch.object(worker, "lease_heartbeats", heartbeat))
            stack.enter_context(patch.object(worker, "now_ms", return_value=1200))
            stack.enter_context(patch.object(worker, "AGENT_RUN_LIFECYCLE_RECHECK_MS", 30_000))
            yielded = stack.enter_context(patch.object(worker, "yield_job"))
            forbidden = [stack.enter_context(patch.object(worker, name)) for name in (
                "fail_job", "fail_claimed_job", "complete_job",
                "transition_agent_run", "agent_run_step_request",
                "runtime_teardown_request", "finish_environment_agent_run",
            )]
            for owner in ("worker:first-claim", "worker:second-claim"):
                worker.execute_claimed_job(job, owner)
            self.assertEqual([call.args for call in yielded.call_args_list], [
                (job["jobId"], owner, 31_200, "environment_power_pending")
                for owner in ("worker:first-claim", "worker:second-claim")
            ])
            self.assertEqual(healthy.call_count, 2)
            self.assertEqual(job["retryCount"], 10)
            for call in forbidden:
                call.assert_not_called()

    def test_lease_loss_after_wait_response_never_yields_with_stale_owner(self):
        job, _ = lifecycle_fixture()
        with patch.object(worker, "api_request", return_value=power_wait()), \
                patch.object(worker, "yield_job") as yielded:
            with self.assertRaises(worker.RuntimeLeaseLost):
                worker.execute_agent_run_lifecycle_job(
                    job, "worker:expired", Mock(side_effect=worker.RuntimeLeaseLost))
            yielded.assert_not_called()

    def test_cancelled_waiting_job_does_not_become_an_execution_failure(self):
        job, _ = lifecycle_fixture()

        @contextmanager
        def heartbeat(*args):
            yield lambda: None

        with patch.object(worker, "api_request", return_value=power_wait()), \
                patch.object(worker, "start_job"), \
                patch.object(worker, "lease_heartbeats", heartbeat), \
                patch.object(worker, "yield_job", side_effect=worker.RuntimeJobCancelled("cancelled")), \
                patch.object(worker, "fail_claimed_job") as failed, \
                patch.object(worker, "transition_agent_run") as transition, \
                patch.object(worker, "agent_run_step_request") as execute:
            worker.execute_claimed_job(job, "worker:cancelled")
            failed.assert_not_called()
            transition.assert_not_called()
            execute.assert_not_called()

    def test_next_ready_claim_uses_new_authority_after_wait(self):
        job, start = lifecycle_fixture()
        ready = {"schema": "runtime.agent_run_lifecycle.resolved.v1",
                 "disposition": "ready", "agentRunStart": start}
        step = {"schema": "runtime.agent_run.step.result.v1",
                "agentRunId": start["agentRunId"], "disposition": "waiting",
                "terminalState": None, "transitionReason": "runtime_job_wait"}
        with patch.object(worker, "api_request", side_effect=[power_wait(), ready]), \
                patch.object(worker, "yield_job") as yielded, \
                patch.object(worker, "transition_agent_run") as transition, \
                patch.object(worker, "agent_run_step_request", return_value=step) as execute:
            worker.execute_agent_run_lifecycle_job(job, "worker:waiting", lambda: None)
            execute.assert_not_called()
            transition.assert_not_called()
            worker.execute_agent_run_lifecycle_job(job, "worker:ready", lambda: None)
            self.assertEqual(execute.call_args.args[0]["leaseOwner"], "worker:ready")
            self.assertEqual(execute.call_args.args[0]["agentRunStart"], start)
            self.assertEqual(yielded.call_count, 2)


if __name__ == "__main__":
    unittest.main()
