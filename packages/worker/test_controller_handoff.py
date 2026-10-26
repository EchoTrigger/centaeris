"""Controller contention and uncertain yield delivery preserve the original job."""
from contextlib import contextmanager
from copy import deepcopy
import unittest
from unittest.mock import patch

from test_worker import lifecycle_fixture
from test_controller_capacity_wait import rejection
import test_retained_lifecycle
import worker


class ControllerHandoffTests(unittest.TestCase):
    def test_terminal_resolve_and_release_contention_yield_twenty_times_without_failure(self):
        for stage in ('post_terminal_resolve', 'terminal_release'):
            for code in ('environment_controller_owned', 'environment_controller_busy'):
                with self.subTest(stage=stage, code=code):
                    job, start = lifecycle_fixture()
                    job.update(retryCount=3, maxRetries=10)
                    terminal = test_retained_lifecycle.RetainedLifecycleTests().terminal()
                    step_result = {'schema': 'runtime.agent_run.step.result.v1', 'agentRunId': start['agentRunId'],
                        'disposition': 'terminal', 'terminalState': 'completed', 'transitionReason': 'runtime_session_terminal_committed'}
                    resolves = 0
                    def api(path, body, *_args):
                        nonlocal resolves
                        if path.endswith('/resolve'):
                            resolves += 1
                            if stage == 'post_terminal_resolve' and resolves == 1:
                                return {'schema': 'runtime.agent_run_lifecycle.resolved.v1', 'disposition': 'ready', 'agentRunStart': start}
                            if stage == 'post_terminal_resolve':
                                raise rejection(code)
                            return terminal
                        if path.endswith('/release'):
                            raise rejection(code)
                        raise AssertionError(path)
                    @contextmanager
                    def heartbeats(*_args):
                        yield lambda: None
                    with patch.object(worker, 'start_job'), patch.object(worker, 'lease_heartbeats', heartbeats), \
                            patch.object(worker, 'api_request', side_effect=api), \
                            patch.object(worker, 'transition_agent_run') as transition, \
                            patch.object(worker, 'agent_run_step_request', return_value=step_result) as step, \
                            patch.object(worker, 'runtime_teardown_request', return_value={
                                'schema': 'runtime.agent_run.teardown.result.v1', 'agentRunId': start['agentRunId'],
                                'status': 'cleanupConfirmed', 'cleanupReceiptId': 'cleanup-1'}), \
                            patch.object(worker, 'yield_job') as yielded, \
                            patch.object(worker, 'fail_job') as failed, patch.object(worker, 'complete_job') as complete:
                        for _ in range(20):
                            worker.execute_claimed_job(job, 'worker:fixture-owner')
                        self.assertEqual(yielded.call_count, 20)
                        self.assertEqual(step.call_count, 1 if stage == 'post_terminal_resolve' else 0)
                        self.assertTrue(all(call.args[1] != 'failed' for call in transition.call_args_list))
                        failed.assert_not_called()
                        complete.assert_not_called()
                        self.assertEqual(job['retryCount'], 3)

    def test_control_contention_keeps_terminal_and_same_job_in_every_phase(self):
        for stage in ('control_resolve', 'cleanup_resolve', 'cleanup_teardown', 'cleanup_release'):
            for error in (rejection('environment_controller_owned'), rejection('environment_controller_busy'),
                          RuntimeError('environment_controller_owned'), rejection('environment_controller_owned', 403)):
                with self.subTest(stage=stage, error=str(error), status=getattr(error, 'http_status', None)):
                    job, start = lifecycle_fixture()
                    job.update(leaseOwner='worker:fixture-owner', controlStage='cleanup', retryCount=3)
                    terminal = test_retained_lifecycle.RetainedLifecycleTests().terminal()
                    def api(path, body, *_args):
                        if path.endswith('/control'):
                            if stage == 'control_resolve':
                                raise error
                            return {'schema': 'runtime.agent_run_lifecycle.control.resolved.v1',
                                'agentRunControl': {'agentRunId': start['agentRunId']}}
                        if path.endswith('/resolve'):
                            if stage == 'cleanup_resolve':
                                raise error
                            return terminal
                        if path.endswith('/release'):
                            raise error
                        raise AssertionError(path)
                    def teardown(body):
                        if stage == 'cleanup_teardown':
                            raise error
                        return {'schema': 'runtime.agent_run.teardown.result.v1', 'agentRunId': start['agentRunId'],
                            'status': 'cleanupConfirmed', 'cleanupReceiptId': 'cleanup-1'}
                    @contextmanager
                    def heartbeats(*_args):
                        yield lambda: None
                    result = {'schema': 'runtime.agent_run.control.result.v1', 'agentRunId': start['agentRunId'],
                        'disposition': 'terminal', 'terminalState': 'completed'}
                    with patch.object(worker, 'lease_heartbeats', heartbeats), patch.object(worker, 'api_request', side_effect=api), \
                            patch.object(worker, 'runtime_request', return_value=result), \
                            patch.object(worker, 'runtime_teardown_request', side_effect=teardown), \
                            patch.object(worker, 'yield_job') as yielded, patch.object(worker, 'fail_job') as failed, \
                            patch.object(worker, 'transition_agent_run') as transition, patch.object(worker, 'agent_run_step_request') as step:
                        for _ in range(20):
                            worker.execute_control_job(job)
                        self.assertEqual(yielded.call_count, 20)
                        expected = 'environment_controller_wait' if getattr(error, 'http_status', None) == 409 else 'control_retry_pending'
                        self.assertTrue(all(call.args[0:2] == (job['jobId'], job['leaseOwner']) and call.args[3] == expected
                            for call in yielded.call_args_list))
                        self.assertTrue(all(call.args[1] == 'completed' for call in transition.call_args_list))
                        failed.assert_not_called()
                        step.assert_not_called()
                        self.assertEqual(job['retryCount'], 3)

    def test_queued_query_does_not_confirm_yield_and_replay_is_bounded(self):
        requests = []
        def runtime(path, body=None, **kwargs):
            requests.append((path, deepcopy(body)))
            if path.endswith('/heartbeat'):
                return {'ok': True}
            if path.endswith('/yield'):
                raise worker.DependencyUnavailable('ack_unknown')
            return {'job': {'jobId': 'job-1', 'status': 'queued', 'leaseOwner': None}}
        with patch.object(worker, 'runtime_request', side_effect=runtime):
            with worker.lease_heartbeats('job-1', 'worker:fixture-owner'):
                with self.assertRaisesRegex(worker.YieldHandoffUnknown, 'job_yield_handoff_unknown'):
                    worker.yield_job('job-1', 'worker:fixture-owner', 1000, 'environment_controller_wait')
        yields = [body for path, body in requests if path.endswith('/yield')]
        self.assertEqual(len(yields), 3)
        self.assertTrue(all(body == yields[0] for body in yields))
        self.assertEqual(sum(path.endswith('/heartbeat') for path, _ in requests), 1)

    def test_guardless_unknown_yield_never_retries_or_invents_confirmed_lease(self):
        requests = []
        def runtime(path, body=None, **kwargs):
            requests.append(path)
            self.assertGreater(kwargs['timeout'], 0)
            self.assertLessEqual(kwargs['timeout'], 5)
            if path.endswith('/yield'):
                raise worker.DependencyUnavailable('ack_unknown')
            return {'job': {'jobId': 'job-1', 'status': 'running', 'leaseOwner': 'worker:fixture-owner', 'leaseExpiresAtMs': 9999999999999}}
        with patch.object(worker, 'runtime_request', side_effect=runtime):
            with self.assertRaises(worker.YieldHandoffUnknown):
                worker.yield_job('job-1', 'worker:fixture-owner', 1000, 'environment_controller_wait')
        self.assertEqual(requests, ['/internal/jobs/yield', '/internal/jobs/job-1'])

    def test_recovery_rpcs_each_respect_remaining_confirmed_deadline(self):
        clock = [100.0]
        requests = []
        def runtime(path, body=None, **kwargs):
            requests.append((path, kwargs.get('timeout')))
            if path.endswith('/heartbeat'):
                return {'ok': True}
            if path.endswith('/yield'):
                self.assertLessEqual(kwargs['timeout'], 159.0 - clock[0])
                if sum(route.endswith('/yield') for route, _ in requests) == 1:
                    clock[0] = 158.75
                    raise worker.DependencyUnavailable('ack_unknown')
                return {'ok': True, 'runAtMs': body['runAtMs'], 'transitionReason': body['transitionReason']}
            self.assertLessEqual(kwargs['timeout'], 0.25)
            return {'job': {'jobId': 'job-1', 'status': 'running', 'leaseOwner': 'worker:fixture-owner', 'leaseExpiresAtMs': 2000000}}
        with patch.object(worker, 'runtime_request', side_effect=runtime), \
                patch.object(worker.time, 'monotonic', side_effect=lambda: clock[0]), patch.object(worker, 'now_ms', return_value=1000000):
            with worker.lease_heartbeats('job-1', 'worker:fixture-owner'):
                worker.yield_job('job-1', 'worker:fixture-owner', 1001000, 'environment_controller_wait')
        self.assertEqual([path for path, _ in requests], ['/internal/jobs/heartbeat', '/internal/jobs/yield', '/internal/jobs/job-1', '/internal/jobs/yield'])

    def run_uncertain_yield(self, *, committed, state='current', unavailable=False, expire=False, control=False, followup_error=None, query_error=None):
        job, start = lifecycle_fixture()
        owner = 'worker:fixture-owner'
        job.update(leaseOwner=owner, retryCount=3, maxRetries=10, controlStage='advance')
        clock = [100.0]
        events = []
        yielded = []
        stored = {'jobId': job['jobId'], 'status': 'running', 'leaseOwner': owner, 'leaseExpiresAtMs': 2000000}
        def runtime(path, body=None, **kwargs):
            if path.endswith('/heartbeat'):
                events.append('heartbeat')
                return {'ok': True}
            if path.endswith('/start'):
                return {'ok': True}
            if path == '/agent-runs/control':
                return {'schema': 'runtime.agent_run.control.result.v1', 'agentRunId': start['agentRunId'],
                    'terminalState': 'completed', 'disposition': 'terminal'}
            if path.endswith('/yield'):
                events.append('yield')
                yielded.append(deepcopy(body))
                if len(yielded) == 1:
                    if committed:
                        stored.update(status='queued', leaseOwner=None)
                    if state == 'new_owner':
                        stored.update(status='running', leaseOwner='worker:new-owner')
                    if state == 'cancelled':
                        stored.update(status='cancelled', leaseOwner=None)
                    if expire:
                        clock[0] = 200.0
                    raise worker.DependencyUnavailable('runtime_job_request_failed')
                if followup_error is not None:
                    raise followup_error
                if unavailable:
                    raise worker.DependencyUnavailable('runtime_job_request_failed')
                self.assertEqual(body, yielded[0])
                stored.update(status='queued', leaseOwner=None)
                return {'ok': True, 'runAtMs': body['runAtMs'], 'transitionReason': body['transitionReason']}
            if path == '/internal/jobs/' + job['jobId']:
                events.append('query')
                if query_error is not None:
                    raise query_error
                if unavailable:
                    raise worker.DependencyUnavailable('runtime_job_request_failed')
                return {'job': deepcopy(stored)}
            raise AssertionError(path)
        def api(path, body, *_args):
            if path.endswith('/control'):
                return {'schema': 'runtime.agent_run_lifecycle.control.resolved.v1', 'agentRunControl': {'agentRunId': start['agentRunId']}}
            raise rejection('environment_controller_busy')
        with patch.object(worker, 'runtime_request', side_effect=runtime), patch.object(worker, 'api_request', side_effect=api), \
                patch.object(worker.time, 'monotonic', side_effect=lambda: clock[0]), \
                patch.object(worker, 'now_ms', side_effect=range(1000000, 1001000)), \
                patch.object(worker, 'transition_agent_run') as transition, \
                patch.object(worker, 'agent_run_step_request') as step, \
                patch.object(worker, 'runtime_teardown_request') as teardown, \
                patch.object(worker, 'fail_job') as fail, patch.object(worker, 'fail_claimed_job') as fail_claimed:
            if control:
                worker.execute_control_job(job)
            else:
                worker.execute_claimed_job(job, owner)
            fail.assert_not_called()
            fail_claimed.assert_not_called()
            step.assert_not_called()
            teardown.assert_not_called()
            self.assertTrue(all(call.args[1] != 'failed' for call in transition.call_args_list))
            if not control:
                transition.assert_not_called()
        self.assertEqual(events.count('heartbeat'), 1)
        self.assertEqual(job['retryCount'], 3)
        self.assertTrue(all(body == yielded[0] for body in yielded))
        return events, yielded

    def test_followup_yield_errors_never_turn_unknown_delivery_into_job_failure(self):
        for committed in (False, True):
            for error in (rejection('invalid_yield', 400), RuntimeError('private response detail')):
                with self.subTest(committed=committed, error_type=type(error).__name__, status=getattr(error, 'http_status', None)):
                    _, yielded = self.run_uncertain_yield(committed=committed, followup_error=error)
                    self.assertEqual(len(yielded), 2)

    def test_followup_yield_error_after_unavailable_query_keeps_unknown_delivery(self):
        _, yielded = self.run_uncertain_yield(committed=False,
            query_error=worker.DependencyUnavailable('query_unavailable'), followup_error=RuntimeError('private response detail'))
        self.assertEqual(len(yielded), 2)

    def test_non_transport_query_error_stops_unknown_delivery_without_more_mutations(self):
        for error in (rejection('query_forbidden', 403), RuntimeError('private query detail')):
            with self.subTest(error_type=type(error).__name__):
                _, yielded = self.run_uncertain_yield(committed=False, query_error=error)
                self.assertEqual(len(yielded), 1)

    def test_unknown_diagnostics_only_report_safe_error_type_and_status(self):
        with patch.object(worker, 'print', create=True) as logged:
            self.run_uncertain_yield(committed=False, followup_error=RuntimeError('private response detail'))
        self.assertEqual(logged.call_count, 1)
        self.assertIn('errorType=RuntimeError', logged.call_args.args[0])
        self.assertNotIn('private response detail', logged.call_args.args[0])
        self.assertNotIn('worker:fixture-owner', logged.call_args.args[0])

    def test_yield_requires_exact_matching_acknowledgement(self):
        for invalid in ({}, {'ok': True}, None, {'ok': 1, 'runAtMs': 1001000, 'transitionReason': 'environment_controller_wait'},
                        {'ok': True, 'runAtMs': 1001000.0, 'transitionReason': 'environment_controller_wait'}, {'ok': False, 'runAtMs': 1001000, 'transitionReason': 'environment_controller_wait'},
                        {'ok': True, 'runAtMs': 1000999, 'transitionReason': 'environment_controller_wait'},
                        {'ok': True, 'runAtMs': 1001000, 'transitionReason': 'different_reason'},
                        {'ok': True, 'runAtMs': 1001000, 'transitionReason': 'environment_controller_wait', 'extra': True}):
            for recover in (False, True):
                with self.subTest(invalid=invalid, recover=recover):
                    yielded = []
                    def runtime(path, body=None, **kwargs):
                        if path.endswith('/heartbeat'):
                            return {'ok': True}
                        if path.endswith('/yield'):
                            yielded.append(deepcopy(body))
                            if recover and len(yielded) > 1:
                                return {'ok': True, 'runAtMs': body['runAtMs'], 'transitionReason': body['transitionReason']}
                            return invalid
                        return {'job': {'jobId': 'job-1', 'status': 'queued', 'leaseOwner': None}}
                    with patch.object(worker, 'runtime_request', side_effect=runtime), patch.object(worker, 'now_ms', return_value=1000000):
                        with worker.lease_heartbeats('job-1', 'worker:fixture-owner'):
                            if recover:
                                worker.yield_job('job-1', 'worker:fixture-owner', 1001000, 'environment_controller_wait')
                            else:
                                with self.assertRaises(worker.YieldHandoffUnknown):
                                    worker.yield_job('job-1', 'worker:fixture-owner', 1001000, 'environment_controller_wait')
                    self.assertEqual(len(yielded), 2 if recover else 3)
                    self.assertTrue(all(body == yielded[0] for body in yielded))

    def test_lost_yield_ack_recovers_identical_request_before_or_after_commit(self):
        for committed in (False, True):
            with self.subTest(committed=committed):
                events, yielded = self.run_uncertain_yield(committed=committed)
                self.assertEqual(len(yielded), 2)
                self.assertIn('query', events)

    def test_unknown_delivery_new_owner_and_expiry_stop_old_owner_without_failure(self):
        for kwargs in ({'state': 'new_owner'}, {'state': 'cancelled'}, {'unavailable': True}, {'expire': True}):
            with self.subTest(**kwargs):
                _, yielded = self.run_uncertain_yield(committed=False, **kwargs)
                self.assertLessEqual(len(yielded), 3)
                if 'state' in kwargs or 'expire' in kwargs:
                    self.assertEqual(len(yielded), 1)

    def test_control_yield_ack_loss_never_replaces_original_request(self):
        for committed in (False, True):
            with self.subTest(committed=committed):
                _, yielded = self.run_uncertain_yield(committed=committed, control=True)
                self.assertEqual(len(yielded), 2)
                self.assertEqual(yielded[0]['transitionReason'], 'control_cleanup_pending')

    def test_control_unknown_delivery_stops_without_fallback_yield(self):
        _, yielded = self.run_uncertain_yield(committed=True, unavailable=True, control=True)
        self.assertLessEqual(len(yielded), 3)
        self.assertEqual(yielded[0]['transitionReason'], 'control_cleanup_pending')

    def test_late_controller_errors_keep_exact_http_classification(self):
        job, _ = lifecycle_fixture()
        terminal = test_retained_lifecycle.RetainedLifecycleTests().terminal()
        for error in (rejection('environment_controller_owned', 403), RuntimeError('environment_controller_owned'),
                      rejection('environment_controller_lease_rejected')):
            with self.subTest(error=str(error)), patch.object(worker, 'resolve_agent_run_lifecycle', return_value=terminal), \
                    patch.object(worker, 'runtime_teardown_request', side_effect=error), patch.object(worker, 'yield_job') as yielded:
                with self.assertRaises(worker.RetainedCleanupPending):
                    worker.execute_agent_run_lifecycle_job(job, 'worker:fixture-owner', lambda: None)
                yielded.assert_not_called()


if __name__ == '__main__':
    unittest.main()

