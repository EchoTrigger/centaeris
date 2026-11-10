import unittest
from contextlib import contextmanager
from unittest.mock import patch

from test_worker import lifecycle_fixture
import worker


class RetainedLifecycleTests(unittest.TestCase):
    def test_per_run_keeps_attachment_and_extension_dispatch(self):
        job, start = lifecycle_fixture()
        start['authorization'] = {'assetRefs': [{'inputRef': 'library:input'}],
            'pluginActivation': {'packages': [{'mcpServers': [{'path': 'mcp.json'}],
                'hooks': [{'path': 'hooks.json'}]}]}}
        resolved = {'schema': 'runtime.agent_run_lifecycle.resolved.v1',
            'disposition': 'ready', 'agentRunStart': start}
        result = {'schema': 'runtime.agent_run.step.result.v1', 'agentRunId': 'agent_run_1',
            'disposition': 'waiting', 'terminalState': None, 'transitionReason': 'runtime_job_wait'}
        with patch.object(worker, 'AGENT_RUN_EXECUTION_MODE', 'perRun'), \
                patch.object(worker, 'api_request', return_value=resolved), \
                patch.object(worker, 'transition_agent_run'), patch.object(worker, 'yield_job'), \
                patch.object(worker, 'agent_run_step_request', return_value=result) as execute:
            worker.execute_agent_run_lifecycle_job(job, 'worker:test-owner', lambda: None)
            self.assertEqual(execute.call_args.args[0]['agentRunStart'], start)

    def test_retained_keeps_attachment_and_extension_dispatch(self):
        job, start = lifecycle_fixture()
        start['authorization'] = {'assetRefs': [{'inputRef': 'library:input'}],
            'pluginActivation': {'packages': [{'mcpServers': [{'path': 'mcp.json'}],
                'hooks': [{'path': 'hooks.json'}]}]}}
        resolved = {'schema': 'runtime.agent_run_lifecycle.resolved.v1',
            'disposition': 'ready', 'agentRunStart': start}
        step = {'schema': 'runtime.agent_run.step.result.v1', 'agentRunId': 'agent_run_1',
            'disposition': 'waiting', 'terminalState': None, 'transitionReason': 'runtime_job_wait'}
        with patch.object(worker, 'AGENT_RUN_EXECUTION_MODE', 'retained'), \
                patch.object(worker, 'api_request', return_value=resolved), \
                patch.object(worker, 'transition_agent_run'), patch.object(worker, 'yield_job'), \
                patch.object(worker, 'agent_run_step_request', return_value=step) as execute:
            worker.execute_agent_run_lifecycle_job(job, 'worker:test-owner', lambda: None)
        self.assertEqual(execute.call_args.args[0]['agentRunStart'], start)

    def test_cleanup_failure_yields_same_job_without_projecting_another_terminal(self):
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
                patch.object(worker, 'complete_job') as complete, patch.object(worker, 'yield_job') as yielded:
            worker.execute_claimed_job(job, 'worker:test-owner')
            failed.assert_not_called()
            self.assertEqual(yielded.call_args.args[-1], 'retained_cleanup_pending')
            transition.assert_not_called()
            complete.assert_not_called()

    def execute(self, resolved, *, step=None, teardown=None, release=None):
        job, start = lifecycle_fixture()
        calls = []
        def api(path, body, reason):
            calls.append((path, body))
            if path.endswith('/resolve'):
                self.assertEqual(body['schema'], 'runtime.agent_run_lifecycle.resolve.v1')
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
        cleanup = {**start, 'schema': 'workspace.agent_run.cleanup.v1',
            'authorizationSignature': 'signature', 'authorization': {},
            'environmentAuthorization': {'payload': {'controllerPurpose': 'cleanup',
                'controllerEpoch': 2}, 'signature': 'signature'}}
        cleanup.pop('turnId')
        return {'schema': 'runtime.agent_run_lifecycle.resolved.v1',
            'disposition': 'terminal', 'terminalState': 'completed', 'agentRunCleanup': cleanup}

    def test_released_replay_completes_without_acquiring_or_touching_runtime(self):
        calls, transition, execute, cleanup, complete = self.execute([{
            'schema': 'runtime.agent_run_lifecycle.resolved.v1', 'disposition': 'released',
            'terminalState': 'completed', 'releaseReceiptId': 'release-1'}])
        self.assertEqual(len(calls), 1)
        transition.assert_not_called()
        execute.assert_not_called()
        cleanup.assert_not_called()
        complete.assert_called_once()

    def test_terminal_cleanup_receipt_is_released_before_job_completion(self):
        calls, _, execute, cleanup, complete = self.execute([self.terminal()],
            teardown={'schema': 'runtime.agent_run.teardown.result.v1', 'agentRunId': 'agent_run_1',
                'status': 'cleanupConfirmed', 'cleanupReceiptId': 'cleanup-1'},
            release={'schema': 'runtime.agent_run_lifecycle.released.v1',
                'agentRunId': 'agent_run_1', 'releaseReceiptId': 'release-1'})
        execute.assert_not_called()
        self.assertEqual(cleanup.call_args.args[0]['schema'], 'runtime.agent_run.teardown.v1')
        self.assertNotIn('agentRunStart', cleanup.call_args.args[0])
        self.assertEqual(calls[-1][1]['cleanupReceiptId'], 'cleanup-1')
        self.assertEqual(calls[-1][1]['controllerEpoch'], 2)
        complete.assert_called_once()

    def test_invalid_cleanup_receipt_never_releases_or_completes(self):
        with patch.object(worker, 'yield_job') as yielded:
            calls, transition, execute, _, complete = self.execute([self.terminal()], teardown={'schema': 'runtime.agent_run.teardown.result.v1',
                'agentRunId': 'agent_run_1', 'status': 'cleanupConfirmed', 'cleanupReceiptId': ''})
        self.assertEqual(len(calls), 1, 'an invalid receipt cannot authorize controller release')
        transition.assert_not_called()
        execute.assert_not_called()
        complete.assert_not_called()
        self.assertEqual(yielded.call_args.args[-1], 'retained_cleanup_pending')

    def test_step_terminal_resolves_new_cleanup_authority(self):
        _, start = lifecycle_fixture()
        start['schema'] = 'workspace.agent_run.start.v1'
        start['environmentAuthorization'] = {'payload': {'controllerPurpose': 'execute'}}
        calls, _, execute, cleanup, complete = self.execute([
            {'schema': 'runtime.agent_run_lifecycle.resolved.v1', 'disposition': 'ready', 'agentRunStart': start},
            self.terminal()], step={'schema': 'runtime.agent_run.step.result.v1', 'agentRunId': 'agent_run_1',
                'disposition': 'terminal', 'terminalState': 'completed', 'transitionReason': 'runtime_session_terminal_committed'},
            teardown={'schema': 'runtime.agent_run.teardown.result.v1', 'agentRunId': 'agent_run_1',
                'status': 'cleanupConfirmed', 'cleanupReceiptId': 'cleanup-1'},
            release={'schema': 'runtime.agent_run_lifecycle.released.v1',
                'agentRunId': 'agent_run_1', 'releaseReceiptId': 'release-1'})
        self.assertEqual(len([call for call in calls if call[0].endswith('/resolve')]), 2)
        self.assertEqual(execute.call_args.args[0]['schema'], 'runtime.agent_run.step.v1')
        self.assertEqual(cleanup.call_args.args[0]['agentRunCleanup']['environmentAuthorization']['payload']['controllerPurpose'], 'cleanup')
        complete.assert_called_once()
