import io
import threading
import unittest
import urllib.error
from unittest.mock import Mock,patch
import test_worker
from test_worker import worker

class RetryTests(unittest.TestCase):
    def test_transport_failure_preserves_unclaimed_jobs_and_resumes_dispatch(self):
        stopped=Mock();stopped.is_set.side_effect=[False,False,True]
        job={'jobId':'next-job','leaseOwner':'opaque-owner-123'}
        error=worker.DependencyUnavailable('disconnected')
        with patch.object(worker,'claim_next_job',side_effect=[error,job]), \
                patch.object(worker.ClaimedJobDispatcher,'start') as dispatch, \
                patch.object(worker,'wait_for_jobs') as wait, patch.object(worker,'fail_claimed_job') as fail,patch('sys.stderr'):
            worker.run_job_dispatcher(stopped)
        stopped.wait.assert_called_once_with(worker.JOB_WAIT_FAILURE_BACKOFF_SECONDS)
        dispatch.assert_called_once_with(job,worker.run_claimed_job)
        wait.assert_not_called();fail.assert_not_called()

    def test_stop_interrupts_dispatch_without_another_claim(self):
        stopped=threading.Event()
        def fail(_sequence): stopped.set();raise worker.DependencyUnavailable('disconnected')
        with patch.object(worker,'claim_next_job',side_effect=fail) as claim,patch('sys.stderr'):
            worker.run_job_dispatcher(stopped)
        claim.assert_called_once()

    def test_rpc_attaches_route_without_accepting_unknown_error_fields(self):
        error=urllib.error.HTTPError('http://runtime.invalid/internal/jobs/claim',503,'unavailable',{},
            io.BytesIO(b'{"error":"unavailable","private":"not-a-category"}'))
        with patch.object(worker.urllib.request,'urlopen',side_effect=error):
            with self.assertRaises(worker.DependencyUnavailable) as caught: worker.runtime_request('/internal/jobs/claim',{})
        self.assertEqual(caught.exception.route,'/internal/jobs/claim')
        self.assertIsNone(caught.exception.http_error_code)

    def test_unconfirmed_job_failure_remains_owned_for_durable_reconciliation(self):
        job={'jobId':'claimed-job','leaseOwner':'opaque-claim-owner'}
        with patch.object(worker,'execute_claimed_job',side_effect=RuntimeError('private failure')), \
                patch.object(worker,'fail_claimed_job') as fail,patch('sys.stderr'):
            worker.run_claimed_job(job)
        fail.assert_not_called()

if __name__=='__main__':unittest.main()
