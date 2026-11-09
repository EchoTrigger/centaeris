"""Every lifecycle policy acquires a controller and completes after a release receipt."""
import io
import json
import unittest
import urllib.error
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

    def test_agent_run_api_wait_wakes_at_its_signed_execution_deadline(self):
        job, start = self.start()
        start['authorization']['executionPolicy'] = {'purpose': 'agentRunApi', 'deadlineAtMs': 1001}
        resolved = {'schema': 'runtime.agent_run_lifecycle.resolved.v1', 'disposition': 'ready', 'agentRunStart': start}
        step = {'schema': 'runtime.agent_run.step.result.v1', 'agentRunId': start['agentRunId'],
            'disposition': 'waiting', 'terminalState': None, 'transitionReason': 'runtime_job_wait'}
        with patch.object(worker, 'api_request', return_value=resolved), \
                patch.object(worker, 'transition_agent_run'), patch.object(worker, 'yield_job') as yielded, \
                patch.object(worker, 'now_ms', return_value=1000), \
                patch.object(worker, 'agent_run_step_request', return_value=step):
            worker.execute_agent_run_lifecycle_job(job, 'worker:fixture-owner', lambda: None)
        self.assertEqual(yielded.call_args.args[2], 1001)

    def test_terminal_replay_uses_the_existing_cleanup_grant_and_waits_for_release(self):
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
                finalized = {'schema': 'runtime.agent_run.finalize.result.v1', 'agentRunId': start['agentRunId'],
                    'releaseReceiptId': 'release-1'}
                with patch.object(worker, 'AGENT_RUN_EXECUTION_MODE', mode), \
                        patch.object(worker, 'api_request', side_effect=[terminal, release]) as api, \
                        patch.object(worker, 'agent_run_step_request') as execute, \
                        patch.object(worker, 'runtime_finalize_request', return_value=finalized) as finalize, \
                        patch.object(worker, 'runtime_teardown_request', return_value=teardown) as teardown_request, \
                        patch.object(worker, 'transition_agent_run') as transition, \
                        patch.object(worker, 'complete_job') as complete:
                    worker.execute_agent_run_lifecycle_job(job, 'worker:fixture-owner', lambda: None)
                execute.assert_not_called()
                transition.assert_not_called()
                if mode == 'perRun':
                    finalize.assert_called_once_with({'schema': 'runtime.agent_run.finalize.v1',
                        'jobId': job['jobId'], 'leaseOwner': 'worker:fixture-owner', 'agentRunCleanup': cleanup,
                        'agentRunId': start['agentRunId'], 'authorizationDigest': start['authorizationDigest'],
                        'terminalState': 'completed'})
                    api.assert_called_once()
                    teardown_request.assert_not_called()
                else:
                    finalize.assert_not_called()
                    teardown_request.assert_called_once_with({'schema': 'runtime.agent_run.teardown.v1',
                        'jobId': job['jobId'], 'leaseOwner': 'worker:fixture-owner', 'agentRunCleanup': cleanup})
                    self.assertEqual(api.call_args.args[1]['cleanupReceiptId'], 'cleanup-1')
                complete.assert_called_once_with(job['jobId'], 'worker:fixture-owner', [])

    def test_per_run_finalization_response_or_lease_loss_keeps_the_job_unacknowledged(self):
        job, start = self.start()
        owner = 'worker:fixture-owner'
        terminal = {'schema': 'runtime.agent_run_lifecycle.resolved.v1', 'disposition': 'terminal',
            'terminalState': 'completed', 'agentRunCleanup': {'schema': 'workspace.agent_run.cleanup.v1',
                'agentRunId': start['agentRunId'], 'authorizationDigest': start['authorizationDigest'],
                'authorizationSignature': 'signed', 'authorization': {},
                'environmentAuthorization': {'payload': {'controllerPurpose': 'cleanup', 'controllerEpoch': 2}}}}
        path = '/agent-runs/finalize'
        url = worker.RUNTIME_INTERNAL_URL + path
        loss = {'error': 'agent_run_lifecycle_lease_lost'}
        for status in (None, 409, 503):
            with self.subTest(status=status):
                error = (OSError('lost ACK') if status is None else urllib.error.HTTPError(
                    url, status, 'fixture', {}, io.BytesIO(json.dumps(loss).encode())))
                with patch.object(worker, 'AGENT_RUN_EXECUTION_MODE', 'perRun'), \
                        patch.object(worker, 'api_request', return_value=terminal) as api, \
                        patch('urllib.request.urlopen', side_effect=error) as runtime, \
                        patch.object(worker, 'now_ms', return_value=1000), \
                        patch.object(worker, 'complete_job') as complete, patch.object(worker, 'yield_job') as yielded:
                    if status == 409:
                        with self.assertRaises(worker.RuntimeLeaseLost):
                            worker.execute_agent_run_lifecycle_job(job, owner, lambda: None)
                        yielded.assert_not_called()
                    else:
                        self.assertFalse(worker.execute_agent_run_lifecycle_job(job, owner, lambda: None))
                        yielded.assert_called_once_with(job['jobId'], owner, 2000, 'retained_cleanup_pending')
                self.assertEqual(runtime.call_args.args[0].full_url, url)
                self.assertEqual(runtime.call_args.args[0].get_method(), 'POST')
                api.assert_called_once()
                complete.assert_not_called()

    def test_per_run_unbound_finalization_receipt_does_not_complete_job(self):
        job, start = self.start()
        ready = {'schema': 'runtime.agent_run_lifecycle.resolved.v1', 'disposition': 'ready', 'agentRunStart': start}
        step = {'schema': 'runtime.agent_run.step.result.v1', 'agentRunId': start['agentRunId'], 'disposition': 'terminal',
            'terminalState': 'completed', 'transitionReason': 'runtime_session_terminal_committed'}
        receipt = {'schema': 'runtime.agent_run.finalize.result.v1', 'agentRunId': 'different-run',
            'releaseReceiptId': 'release-1'}
        with patch.object(worker, 'AGENT_RUN_EXECUTION_MODE', 'perRun'), \
                patch.object(worker, 'api_request', return_value=ready), patch.object(worker, 'transition_agent_run'), \
                patch.object(worker, 'agent_run_step_request', return_value=step), \
                patch.object(worker, 'runtime_finalize_request', return_value=receipt), \
                patch.object(worker, 'complete_job') as complete, patch.object(worker, 'yield_job') as yielded:
            self.assertFalse(worker.execute_agent_run_lifecycle_job(job, 'worker:fixture-owner', lambda: None))
        complete.assert_not_called()
        self.assertEqual(yielded.call_args.args[-1], 'retained_cleanup_pending')

    def test_released_per_run_job_completes_without_sending_finalize(self):
        job, _start = self.start()
        released = {'schema': 'runtime.agent_run_lifecycle.resolved.v1', 'disposition': 'released',
            'terminalState': 'completed', 'releaseReceiptId': 'release-1'}
        with patch.object(worker, 'AGENT_RUN_EXECUTION_MODE', 'perRun'), \
                patch.object(worker, 'api_request', return_value=released) as api, \
                patch.object(worker, 'agent_run_step_request') as step, \
                patch.object(worker, 'runtime_finalize_request') as finalize, \
                patch.object(worker, 'complete_job') as complete:
            self.assertFalse(worker.execute_agent_run_lifecycle_job(job, 'worker:fixture-owner', lambda: None))
        api.assert_called_once()
        step.assert_not_called()
        finalize.assert_not_called()
        complete.assert_called_once_with(job['jobId'], 'worker:fixture-owner', [])

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
