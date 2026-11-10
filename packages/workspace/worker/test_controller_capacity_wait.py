"""Initial controller contention must queue the original Run, never terminalize it."""
import io
import json
from contextlib import contextmanager
from copy import deepcopy
from unittest.mock import Mock, patch
import unittest
import urllib.error

from test_worker import lifecycle_fixture
import worker


def rejection(code, status=409, *, payload=None):
    body = {'error': code} if payload is None else payload
    error = urllib.error.HTTPError('http://api.invalid/internal/agent-run-lifecycle/resolve', status,
                                  'fixture rejection', {}, io.BytesIO(json.dumps(body).encode()))
    with patch.object(worker.urllib.request, 'urlopen', side_effect=error):
        try:
            worker._json_request(error.url, {}, 'X-Internal-Token', 'synthetic', 'resolve_failed')
        except RuntimeError as rejected:
            return rejected
    raise AssertionError('HTTP rejection expected')


class ControllerCapacityWaitTests(unittest.TestCase):
    def test_repeated_owned_or_busy_yields_original_job_without_retry_or_terminal_facts(self):
        for code in ('environment_controller_owned', 'environment_controller_busy'):
            with self.subTest(code=code):
                job, _ = lifecycle_fixture(); job.update(retryCount=3, backoffPolicy={'maxRetries': 10})
                original = deepcopy(job); healthy = Mock()
                @contextmanager
                def heartbeats(*args): yield healthy
                with (patch.object(worker, 'start_job'), patch.object(worker, 'lease_heartbeats', heartbeats),
                      patch.object(worker, 'api_request', side_effect=[rejection(code) for _ in range(20)]),
                      patch.object(worker, 'yield_job') as yielded,
                      patch.object(worker, 'now_ms', return_value=1000000),
                      patch.object(worker, 'transition_agent_run') as transition,
                      patch.object(worker, 'agent_run_step_request') as step,
                      patch.object(worker, 'finish_environment_agent_run') as finish,
                      patch.object(worker, 'complete_job') as complete,
                      patch.object(worker, 'fail_claimed_job') as failed):
                    for index in range(20):
                        owner = 'worker:fixture-owner-' + str(index)
                        worker.execute_claimed_job(job, owner)
                        yielded.assert_called_with(job['jobId'], owner, 1001000, 'environment_controller_wait')
                    self.assertEqual(yielded.call_count, 20); self.assertEqual(healthy.call_count, 20)
                    for action in (transition, step, finish, complete, failed): action.assert_not_called()
                self.assertEqual(job, original)

    def test_release_then_reclaim_enters_same_run_once_without_a_replacement(self):
        job, start = lifecycle_fixture(); owner = 'worker:fixture-owner'
        waiting = {'schema': 'runtime.agent_run.step.result.v1', 'agentRunId': start['agentRunId'],
                   'disposition': 'waiting', 'terminalState': None, 'transitionReason': 'runtime_job_wait'}
        answers = [rejection('environment_controller_owned'),
                   {'schema': 'runtime.agent_run_lifecycle.resolved.v1', 'disposition': 'ready', 'agentRunStart': start}]
        with (patch.object(worker, 'api_request', side_effect=answers),
              patch.object(worker, 'yield_job') as yielded,
              patch.object(worker, 'transition_agent_run') as transition,
              patch.object(worker, 'agent_run_step_request', return_value=waiting) as step):
            self.assertFalse(worker.execute_agent_run_lifecycle_job(job, owner, lambda: None))
            self.assertFalse(worker.execute_agent_run_lifecycle_job(job, owner, lambda: None))
            step.assert_called_once(); self.assertEqual(step.call_args.args[0]['agentRunStart'], start)
            self.assertEqual(yielded.call_count, 2)
            self.assertEqual(transition.call_args_list[0].args, (start['agentRunId'], 'running', 'agent_run_lifecycle_step_started'))

    def test_other_status_codes_authority_errors_and_untyped_errors_do_not_wait(self):
        errors = [rejection('environment_controller_owned', 403), rejection('environment_controller_owned', 400),
                  rejection('environment_controller_lease_rejected'), rejection('prefix_environment_controller_owned'),
                  rejection('environment_controller_owned', payload={'error': 'environment_controller_owned', 'unexpected': True}),
                  RuntimeError('environment_controller_owned')]
        for error in errors:
            with self.subTest(reason=str(error)):
                job, _ = lifecycle_fixture()
                with patch.object(worker, 'api_request', side_effect=error), patch.object(worker, 'yield_job') as yielded:
                    with self.assertRaises(RuntimeError):
                        worker.execute_agent_run_lifecycle_job(job, 'worker:fixture-owner', lambda: None)
                    yielded.assert_not_called()

    def test_lease_loss_before_yield_preserves_owner_and_starts_nothing(self):
        job, _ = lifecycle_fixture(); healthy = Mock(side_effect=worker.RuntimeLeaseLost())
        @contextmanager
        def heartbeats(*args): yield healthy
        with (patch.object(worker, 'start_job'), patch.object(worker, 'lease_heartbeats', heartbeats),
              patch.object(worker, 'api_request', side_effect=rejection('environment_controller_owned')),
              patch.object(worker, 'yield_job') as yielded, patch.object(worker, 'agent_run_step_request') as step,
              patch.object(worker, 'fail_claimed_job') as failed, patch.object(worker, 'finish_environment_agent_run') as finish):
            worker.execute_claimed_job(job, 'worker:fixture-owner')
            healthy.assert_called_once()
            for action in (yielded, step, failed, finish): action.assert_not_called()


class HostPreparationWaitTests(unittest.TestCase):
    def test_preparation_contention_waits_without_retry_deadline_or_attempt_consumption(self):
        job, start = lifecycle_fixture(); job['retryCount'] = 3
        waiting = {'schema': 'runtime.agent_run.step.result.v1', 'agentRunId': start['agentRunId'],
                   'disposition': 'waiting', 'terminalState': None, 'transitionReason': 'execution_host_preparation_wait'}
        def api_request(path, body, *_args):
            if path.endswith('/resolve'):
                return {'schema': 'runtime.agent_run_lifecycle.resolved.v1', 'disposition': 'ready', 'agentRunStart': start}
            return {'agentRunId': start['agentRunId'], 'state': body['state']}
        with (patch.object(worker, 'api_request', side_effect=api_request),
              patch.object(worker, 'agent_run_step_request', return_value=waiting),
              patch.object(worker, 'now_ms', return_value=1000000),
              patch.object(worker, 'yield_job') as yielded,
              patch.object(worker, 'complete_job') as completed,
              patch.object(worker, 'finish_environment_agent_run') as finish):
            for _ in range(20):
                self.assertFalse(worker.execute_agent_run_lifecycle_job(job, 'worker:fixture-owner', lambda: None))
            yielded.assert_called_with(job['jobId'], 'worker:fixture-owner',
                                      1000000 + worker.AGENT_RUN_LIFECYCLE_RECHECK_MS, 'execution_host_preparation_wait')
            self.assertEqual(yielded.call_count, 20)
            completed.assert_not_called(); finish.assert_not_called()
            self.assertEqual(job['retryCount'], 3)

    def test_preparation_wait_does_not_accept_a_scheduled_retry_deadline(self):
        job, start = lifecycle_fixture()
        waiting = {'schema': 'runtime.agent_run.step.result.v1', 'agentRunId': start['agentRunId'],
                   'disposition': 'waiting', 'terminalState': None, 'transitionReason': 'execution_host_preparation_wait',
                   'retryAtMs': 1000200}
        resolved = {'schema': 'runtime.agent_run_lifecycle.resolved.v1', 'disposition': 'ready', 'agentRunStart': start}
        with (patch.object(worker, 'resolve_agent_run_lifecycle', return_value=resolved),
              patch.object(worker, 'transition_agent_run'), patch.object(worker, 'agent_run_step_request', return_value=waiting),
              patch.object(worker, 'yield_job') as yielded):
            with self.assertRaisesRegex(RuntimeError, 'agent_run_step_response_invalid'):
                worker.execute_agent_run_lifecycle_job(job, 'worker:fixture-owner', lambda: None)
            yielded.assert_not_called()


if __name__ == '__main__': unittest.main()
