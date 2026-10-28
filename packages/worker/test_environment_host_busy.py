"""Exact Host admission rejection preserves the current lifecycle Job."""
from contextlib import contextmanager
import io
import json
import unittest
import urllib.error
from unittest.mock import patch
from test_worker import lifecycle_fixture
import test_retained_lifecycle
import worker


def rejection(status=409, payload=None):
    if payload is None: payload = {'error':'environment_host_busy'}
    return urllib.error.HTTPError('http://runtime.invalid/agent-runs/teardown', status, 'busy', None, io.BytesIO(json.dumps(payload).encode()))


@contextmanager
def heartbeats(*_args):
    yield lambda: None


class EnvironmentHostBusyTests(unittest.TestCase):
    def test_cleanup_busy_yields_same_job_twenty_times_in_both_execution_modes(self):
        for mode in ('perRun', 'retained'):
            with self.subTest(mode=mode):
                self.observe_cleanup_busy(mode)

    def observe_cleanup_busy(self, mode, *, control=False):
        job, start = lifecycle_fixture()
        job.update(retryCount=0, maxRetries=0, leaseOwner='worker:fixture-owner', controlStage='cleanup')
        terminal = test_retained_lifecycle.RetainedLifecycleTests().terminal()
        def busy(*_args, **_kwargs):
            raise rejection()
        def api_response(path, *_args):
            if path.endswith('/control'):
                return {'schema':'runtime.agent_run_lifecycle.control.resolved.v1',
                        'agentRunControl':{'agentRunId':start['agentRunId']}}
            self.assertTrue(path.endswith('/resolve'))
            return terminal
        control_result = {'schema':'runtime.agent_run.control.result.v1','agentRunId':start['agentRunId'],
                          'disposition':'terminal','terminalState':'completed'}
        with patch.object(worker, 'AGENT_RUN_EXECUTION_MODE', mode), \
                patch.object(worker, 'start_job'), patch.object(worker, 'lease_heartbeats', heartbeats), \
                patch.object(worker, 'api_request', side_effect=api_response) as api, \
                patch('urllib.request.urlopen', side_effect=busy), \
                patch.object(worker, 'runtime_request', return_value=control_result), \
                patch.object(worker, 'now_ms', return_value=1000), patch.object(worker, 'yield_job') as yielded, \
                patch.object(worker, 'fail_job') as failed, patch.object(worker, 'complete_job') as complete, \
                patch.object(worker, 'transition_agent_run') as transition, patch.object(worker, 'agent_run_step_request') as step:
            for _ in range(20):
                if control:
                    worker.execute_control_job(job)
                else:
                    worker.execute_claimed_job(job, job['leaseOwner'])
        self.assertEqual(yielded.call_count, 20)
        for call in yielded.call_args_list:
            self.assertEqual(call.args, (job['jobId'], job['leaseOwner'], 2000, 'execution_host_admission_wait'))
        self.assertEqual(api.call_count, 40 if control else 20)
        failed.assert_not_called(); complete.assert_not_called(); step.assert_not_called()
        if control:
            self.assertTrue(all(call.args[1] == 'completed' for call in transition.call_args_list))
        else:
            transition.assert_not_called()
        self.assertEqual(job['retryCount'], 0)

    def test_control_cleanup_busy_uses_the_same_wait_in_both_execution_modes(self):
        for mode in ('perRun', 'retained'):
            with self.subTest(mode=mode):
                self.observe_cleanup_busy(mode, control=True)

    def test_other_http_and_transport_errors_keep_cleanup_failure_path(self):
        for status,payload in [(503,{'error':'environment_host_busy'}),(409,{'error':'other'}),
                (409,{'error':'environment_host_busy','extra':True}),(409,{}),(500,{'error':'environment_host_busy'}),
                (403,{'error':'environment_host_busy'}),(None,None)]:
            with self.subTest(status=status,payload=payload):
                job,_=lifecycle_fixture();terminal=test_retained_lifecycle.RetainedLifecycleTests().terminal()
                error=OSError('transport unavailable') if status is None else rejection(status,payload)
                with patch.object(worker,'resolve_agent_run_lifecycle',return_value=terminal), \
                        patch('urllib.request.urlopen',side_effect=error),patch.object(worker,'yield_job') as yielded:
                    with self.assertRaises(worker.RetainedCleanupPending):
                        worker.execute_agent_run_lifecycle_job(job,'worker:fixture-owner',lambda:None)
                yielded.assert_not_called()

    def test_preparation_wait_result_uses_short_yield_without_cleanup(self):
        job,start=lifecycle_fixture()
        waiting={'schema':'runtime.agent_run.step.result.v1','agentRunId':start['agentRunId'],
            'disposition':'waiting','terminalState':None,'transitionReason':'execution_host_admission_wait'}
        with patch.object(worker,'resolve_agent_run_lifecycle',return_value={'schema':'runtime.agent_run_lifecycle.resolved.v1',
                    'disposition':'ready','agentRunStart':start}),patch.object(worker,'agent_run_step_request',return_value=waiting), \
                patch.object(worker,'transition_agent_run'),patch.object(worker,'now_ms',return_value=1000), \
                patch.object(worker,'yield_job') as yielded,patch.object(worker,'runtime_teardown_request') as teardown:
            self.assertFalse(worker.execute_agent_run_lifecycle_job(job,'worker:fixture-owner',lambda:None))
        yielded.assert_called_once_with(job['jobId'],'worker:fixture-owner',2000,'execution_host_admission_wait')
        teardown.assert_not_called()

    def test_cleanup_busy_does_not_yield_under_lost_lease(self):
        job,_=lifecycle_fixture();terminal=test_retained_lifecycle.RetainedLifecycleTests().terminal();checks=0
        def healthy():
            nonlocal checks
            checks+=1
            if checks>=3:raise worker.RuntimeLeaseLost()
        with patch.object(worker,'resolve_agent_run_lifecycle',return_value=terminal), \
                patch('urllib.request.urlopen',side_effect=rejection()),patch.object(worker,'yield_job') as yielded:
            with self.assertRaises(worker.RuntimeLeaseLost):
                worker.execute_agent_run_lifecycle_job(job,'worker:fixture-owner',healthy)
        yielded.assert_not_called()


if __name__=='__main__':unittest.main()
