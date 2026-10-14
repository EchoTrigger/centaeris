import unittest
from contextlib import contextmanager
from unittest.mock import patch

from test_worker import lifecycle_fixture
import worker


class RetainedLifecycleTests(unittest.TestCase):
    def test_retained_reconciler_pages_terminal_cleanup_without_requeue(self):
        next_cursor = {'createdAt': '2026-10-14T00:00:00+00:00', 'id': 'run-1'}
        def api(path, body, reason):
            if body['schema'] == 'runtime.agent_run_lifecycle.reconcile.v1':
                return {'activeNext': None, 'deadLetterNext': None}
            self.assertEqual(body['schema'], 'runtime.agent_run_lifecycle.reconcile.v2')
            self.assertIsNone(body['after'])
            return {'schema': 'runtime.agent_run_lifecycle.reconciled.v2', 'checked': 1,
                'pending': [{'agentRunId': 'run-1', 'authorizationDigest': 'sha256:' + 'a' * 64,
                    'terminalState': 'completed', 'jobStatus': 'dead_lettered'}], 'next': next_cursor}
        with patch.object(worker, 'AGENT_RUN_EXECUTION_MODE', 'retained', create=True), \
                patch.object(worker, 'api_request', side_effect=api) as requests, \
                patch.object(worker, 'runtime_request', side_effect=[{}, {'disposition': 'reconciled', 'checked': 0, 'waiters': [], 'next': None}]):
            reconciler = worker.LifecycleReconciler()
            response = reconciler()
            self.assertEqual(requests.call_count, 2)
            self.assertEqual(reconciler.cleanup_after, next_cursor)
            self.assertEqual(response['cleanupPending']['pending'][0]['jobStatus'], 'dead_lettered')

    def test_cleanup_failure_retries_same_job_without_projecting_another_terminal(self):
        job, _ = lifecycle_fixture()
        job.update(retryCount=0, maxRetries=10)
        @contextmanager
        def heartbeat(*args):
            yield lambda: None
        with patch.object(worker, 'AGENT_RUN_EXECUTION_MODE', 'retained', create=True), \
                patch.object(worker, 'start_job'), patch.object(worker, 'lease_heartbeats', heartbeat), \
                patch.object(worker, 'api_request', return_value=self.terminal()), \
                patch.object(worker, 'runtime_teardown_request', return_value={}), \
                patch.object(worker, 'fail_job', return_value=False) as failed, \
                patch.object(worker, 'transition_agent_run') as transition, \
                patch.object(worker, 'complete_job') as complete:
            worker.execute_claimed_job(job, 'worker:test-owner')
            self.assertTrue(failed.call_args.args[-1], 'cleanup failure must remain retryable')
            transition.assert_not_called()
            complete.assert_not_called()

    def execute(self, resolved, *, step=None, teardown=None, release=None):
        job, start = lifecycle_fixture()
        calls = []
        def api(path, body, reason):
            calls.append((path, body))
            if path.endswith('/resolve'):
                self.assertEqual(body['schema'], 'runtime.agent_run_lifecycle.resolve.v2')
                self.assertEqual(body['leaseOwner'], 'worker:test-owner')
                return resolved.pop(0)
            if path.endswith('/release'):
                return release
            raise AssertionError(path)
        with patch.object(worker, 'AGENT_RUN_EXECUTION_MODE', 'retained', create=True), \
                patch.object(worker, 'api_request', side_effect=api), \
                patch.object(worker, 'transition_agent_run') as transition, \
                patch.object(worker, 'agent_run_step_request', return_value=step) as execute, \
                patch.object(worker, 'runtime_teardown_request', return_value=teardown) as cleanup, \
                patch.object(worker, 'complete_job') as complete:
            worker.execute_agent_run_lifecycle_job(job, 'worker:test-owner', lambda: None)
            return calls, transition, execute, cleanup, complete

    def terminal(self):
        _, start = lifecycle_fixture()
        cleanup = {'schema': 'workspace.agent_run.cleanup.v1', **start,
            'authorizationSignature': 'signature', 'authorization': {},
            'environmentAuthorization': {'payload': {'controllerPurpose': 'cleanup',
                'controllerEpoch': 2}, 'signature': 'signature'}}
        cleanup.pop('turnId')
        return {'schema': 'runtime.agent_run_lifecycle.resolved.v2',
            'disposition': 'terminal', 'terminalState': 'completed', 'agentRunCleanup': cleanup}

    def test_released_replay_completes_without_acquiring_or_touching_runtime(self):
        calls, transition, execute, cleanup, complete = self.execute([{
            'schema': 'runtime.agent_run_lifecycle.resolved.v2', 'disposition': 'released',
            'terminalState': 'completed', 'releaseReceiptId': 'release-1'}])
        self.assertEqual(len(calls), 1)
        transition.assert_not_called()
        execute.assert_not_called()
        cleanup.assert_not_called()
        complete.assert_called_once()

    def test_terminal_cleanup_receipt_is_released_before_job_completion(self):
        calls, _, execute, cleanup, complete = self.execute([self.terminal()],
            teardown={'schema': 'runtime.agent_run.teardown.result.v2', 'agentRunId': 'agent_run_1',
                'status': 'retained', 'cleanupReceiptId': 'cleanup-1'},
            release={'schema': 'runtime.agent_run_lifecycle.released.v1',
                'agentRunId': 'agent_run_1', 'releaseReceiptId': 'release-1'})
        execute.assert_not_called()
        self.assertEqual(cleanup.call_args.args[0]['schema'], 'runtime.agent_run.teardown.v2')
        self.assertNotIn('agentRunStart', cleanup.call_args.args[0])
        self.assertEqual(calls[-1][1]['cleanupReceiptId'], 'cleanup-1')
        self.assertEqual(calls[-1][1]['controllerEpoch'], 2)
        complete.assert_called_once()

    def test_invalid_cleanup_receipt_never_releases_or_completes(self):
        with self.assertRaisesRegex(RuntimeError, 'runtime_teardown_response_invalid'):
            self.execute([self.terminal()], teardown={'schema': 'runtime.agent_run.teardown.result.v2',
                'agentRunId': 'agent_run_1', 'status': 'retained', 'cleanupReceiptId': ''})

    def test_step_terminal_resolves_new_cleanup_authority(self):
        _, start = lifecycle_fixture()
        start['schema'] = 'workspace.agent_run.start.v4'
        start['environmentAuthorization'] = {'payload': {'controllerPurpose': 'execute'}}
        calls, _, execute, cleanup, complete = self.execute([
            {'schema': 'runtime.agent_run_lifecycle.resolved.v2', 'disposition': 'ready', 'agentRunStart': start},
            self.terminal()], step={'schema': 'runtime.agent_run.step.result.v2', 'agentRunId': 'agent_run_1',
                'disposition': 'terminal', 'terminalState': 'completed', 'transitionReason': 'runtime_session_terminal_committed'},
            teardown={'schema': 'runtime.agent_run.teardown.result.v2', 'agentRunId': 'agent_run_1',
                'status': 'retained', 'cleanupReceiptId': 'cleanup-1'},
            release={'schema': 'runtime.agent_run_lifecycle.released.v1',
                'agentRunId': 'agent_run_1', 'releaseReceiptId': 'release-1'})
        self.assertEqual(len([call for call in calls if call[0].endswith('/resolve')]), 2)
        self.assertEqual(execute.call_args.args[0]['schema'], 'runtime.agent_run.step.v2')
        self.assertEqual(cleanup.call_args.args[0]['agentRunCleanup']['environmentAuthorization']['payload']['controllerPurpose'], 'cleanup')
        complete.assert_called_once()
