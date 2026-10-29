"""Every lifecycle policy acquires a controller and closes through the same wire."""
import unittest
from unittest.mock import patch

from test_worker import lifecycle_fixture
import worker


class UnifiedEnvironmentLifecycleTests(unittest.TestCase):
    def start(self):
        job, start = lifecycle_fixture()
        start.update(schema='workspace.agent_run.start.v1', authorization={},
            environmentAuthorization={'payload': {'controllerPurpose': 'execute'}})
        return job, start

    def test_both_modes_send_the_lease_and_mode_and_dispatch_the_same_start_and_step(self):
        for mode in ('perRun', 'retained'):
            with self.subTest(mode=mode):
                job, start = self.start()
                resolved = {'schema': 'runtime.agent_run_lifecycle.resolved.v1', 'disposition': 'ready',
                    'agentRunStart': start}
                step = {'schema': 'runtime.agent_run.step.result.v1', 'agentRunId': start['agentRunId'],
                    'disposition': 'waiting', 'terminalState': None, 'transitionReason': 'runtime_job_wait'}
                with patch.object(worker, 'AGENT_RUN_EXECUTION_MODE', mode), \
                        patch.object(worker, 'api_request', return_value=resolved) as api, \
                        patch.object(worker, 'transition_agent_run'), patch.object(worker, 'yield_job'), \
                        patch.object(worker, 'agent_run_step_request', return_value=step) as runtime:
                    worker.execute_agent_run_lifecycle_job(job, 'worker:fixture-owner', lambda: None)
                self.assertEqual(api.call_args.args[1], {'schema': 'runtime.agent_run_lifecycle.resolve.v1',
                    'jobId': job['jobId'], 'leaseOwner': 'worker:fixture-owner',
                    'executionMode': mode, 'agentRunId': start['agentRunId'],
                    'authorizationDigest': start['authorizationDigest']})
                self.assertEqual(runtime.call_args.args[0], {'schema': 'runtime.agent_run.step.v1',
                    'jobId': job['jobId'], 'leaseOwner': 'worker:fixture-owner', 'agentRunStart': start})

    def test_terminal_replay_only_tears_down_and_releases_after_a_durable_cleanup_receipt(self):
        for mode in ('perRun', 'retained'):
            with self.subTest(mode=mode):
                job, start = self.start()
                cleanup = {'schema': 'workspace.agent_run.cleanup.v1', 'agentRunId': start['agentRunId'],
                    'authorizationDigest': start['authorizationDigest'], 'authorizationSignature': 'signed',
                    'authorization': {}, 'environmentAuthorization': {'payload': {
                        'controllerPurpose': 'cleanup', 'controllerEpoch': 2}}}
                terminal = {'schema': 'runtime.agent_run_lifecycle.resolved.v1', 'disposition': 'terminal',
                    'terminalState': 'completed', 'agentRunCleanup': cleanup}
                release = {'schema': 'runtime.agent_run_lifecycle.released.v1',
                    'agentRunId': start['agentRunId'], 'releaseReceiptId': 'release-1'}
                teardown = {'schema': 'runtime.agent_run.teardown.result.v1', 'agentRunId': start['agentRunId'],
                    'status': 'cleanupConfirmed', 'cleanupReceiptId': 'cleanup-1'}
                with patch.object(worker, 'AGENT_RUN_EXECUTION_MODE', mode), \
                        patch.object(worker, 'api_request', side_effect=[terminal, release]) as api, \
                        patch.object(worker, 'agent_run_step_request') as execute, \
                        patch.object(worker, 'runtime_teardown_request', return_value=teardown) as runtime, \
                        patch.object(worker, 'transition_agent_run') as transition, \
                        patch.object(worker, 'complete_job') as complete:
                    worker.execute_agent_run_lifecycle_job(job, 'worker:fixture-owner', lambda: None)
                execute.assert_not_called()
                transition.assert_not_called()
                self.assertEqual(runtime.call_args.args[0], {'schema': 'runtime.agent_run.teardown.v1',
                    'jobId': job['jobId'], 'leaseOwner': 'worker:fixture-owner', 'agentRunCleanup': cleanup})
                self.assertEqual(api.call_args.args[1]['cleanupReceiptId'], 'cleanup-1')
                complete.assert_called_once()

    def test_cleanup_response_loss_keeps_the_job_unacknowledged(self):
        job, start = self.start()
        terminal = {'schema': 'runtime.agent_run_lifecycle.resolved.v1', 'disposition': 'terminal',
            'terminalState': 'completed', 'agentRunCleanup': {'schema': 'workspace.agent_run.cleanup.v1',
                'agentRunId': start['agentRunId'], 'authorizationDigest': start['authorizationDigest'],
                'authorizationSignature': 'signed', 'authorization': {},
                'environmentAuthorization': {'payload': {'controllerPurpose': 'cleanup', 'controllerEpoch': 2}}}}
        with patch.object(worker, 'AGENT_RUN_EXECUTION_MODE', 'perRun'), \
                patch.object(worker, 'api_request', return_value=terminal) as api, \
                patch.object(worker, 'runtime_teardown_request', side_effect=OSError('lost ACK')), \
                patch.object(worker, 'complete_job') as complete, patch.object(worker, 'yield_job') as yielded:
            self.assertFalse(worker.execute_agent_run_lifecycle_job(job, 'worker:fixture-owner', lambda: None))
        self.assertEqual(api.call_count, 1)
        complete.assert_not_called()
        self.assertEqual(yielded.call_args.args[-1], 'retained_cleanup_pending')

    def test_cleanup_scan_is_independent_and_identical_for_both_modes(self):
        for mode in ('perRun', 'retained'):
            with self.subTest(mode=mode):
                next_cursor = {'createdAt': '2026-10-14T00:00:00+00:00', 'id': 'run-1'}
                def api(path, body, reason):
                    if path.endswith('/cleanup-reconcile'):
                        self.assertEqual(body, {'schema': 'runtime.agent_run_cleanup.reconcile.v1',
                            'limit': worker.CONTROL_PAGE_LIMIT, 'after': None})
                        return {'schema': 'runtime.agent_run_cleanup.reconciled.v1', 'checked': 1,
                            'pending': [{'agentRunId': 'run-1', 'authorizationDigest': 'sha256:' + 'a' * 64,
                                'terminalState': 'completed', 'jobStatus': 'dead_lettered'}], 'next': next_cursor}
                    self.assertEqual(body['schema'], 'runtime.agent_run_lifecycle.reconcile.v1')
                    return {'activeNext': None, 'deadLetterNext': None}
                with patch.object(worker, 'AGENT_RUN_EXECUTION_MODE', mode), \
                        patch.object(worker, 'api_request', side_effect=api) as request, \
                        patch.object(worker, 'runtime_request', side_effect=[{}, {'disposition': 'reconciled',
                            'checked': 0, 'waiters': [], 'next': None}]):
                    reconciler = worker.LifecycleReconciler()
                    result = reconciler()
                self.assertEqual(request.call_count, 2)
                self.assertEqual(reconciler.cleanup_after, next_cursor)
                self.assertEqual(result['cleanupPending']['pending'][0]['jobStatus'], 'dead_lettered')
