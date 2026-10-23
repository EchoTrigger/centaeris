"""Acceptance wrappers preserve the actual Worker calls and their arguments."""
from types import SimpleNamespace
from unittest.mock import Mock
from django.test import SimpleTestCase
from .test_e2b_consumption_production import observe_lifecycle_worker

class LifecycleObservationBindingTests(SimpleTestCase):
    def test_all_transport_lanes_are_measured_without_extra_calls_or_body_inspection(self):
        calls=[]
        class Collector:
            def wrap(self,lane,callback):
                def measured(path,*args,**kwargs):
                    calls.append((lane,path))
                    return callback(path,*args,**kwargs)
                return measured
        original={name:Mock(return_value=object()) for name in (
            'runtime_request','api_request','agent_run_step_request','runtime_teardown_request','wait_for_jobs')}
        worker=SimpleNamespace(**original)
        self.assertIs(observe_lifecycle_worker(worker,Collector()),worker)
        private_body=object()
        self.assertIs(worker.runtime_request('/internal/jobs/claim',private_body,timeout=2),original['runtime_request'].return_value)
        original['runtime_request'].assert_called_once_with('/internal/jobs/claim',private_body,timeout=2)
        self.assertIs(worker.api_request('/internal/agent-run-lifecycle/resolve',private_body,'failure'),original['api_request'].return_value)
        original['api_request'].assert_called_once_with('/internal/agent-run-lifecycle/resolve',private_body,'failure')
        for name in ('agent_run_step_request','runtime_teardown_request'):
            self.assertIs(getattr(worker,name)(private_body),original[name].return_value)
            original[name].assert_called_once_with(private_body)
        self.assertIs(worker.wait_for_jobs(),original['wait_for_jobs'].return_value)
        original['wait_for_jobs'].assert_called_once_with()
        self.assertEqual(calls,[('runtime','/internal/jobs/claim'),('api','/internal/agent-run-lifecycle/resolve'),
            ('runtime','/agent-runs/step'),('runtime','/agent-runs/teardown'),('runtime','/internal/jobs/wait')])

    def test_transport_exception_is_preserved_without_retry(self):
        error=RuntimeError('private-error')
        worker=SimpleNamespace(**{name:Mock(side_effect=error) for name in (
            'runtime_request','api_request','agent_run_step_request','runtime_teardown_request','wait_for_jobs')})
        collector=SimpleNamespace(wrap=lambda lane,callback:callback)
        observe_lifecycle_worker(worker,collector)
        with self.assertRaises(RuntimeError) as raised:worker.agent_run_step_request(object())
        self.assertIs(raised.exception,error)

    def test_disabled_observation_preserves_the_existing_worker(self):
        worker=object()
        self.assertIs(observe_lifecycle_worker(worker,None),worker)

    def test_claim_and_lease_observation_are_both_measured_once(self):
        from .test_native_consumption_production import NativeConsumptionProductionTests
        from .e2b_lifecycle_timing_test_support import LifecycleCallCollector
        collector=LifecycleCallCollector()
        job={'jobId':'job','jobKind':'agent_run.lifecycle','status':'leased','leaseOwner':'owner'}
        request=Mock(side_effect=[{'jobs':[job]},{'job':job}])
        worker=SimpleNamespace(json_request=request,RUNTIME_INTERNAL_URL='http://runtime',INTERNAL_API_TOKEN='fixture',now_ms=lambda:0)
        actual=NativeConsumptionProductionTests.claim_due_job(self,worker,'job','agent_run.lifecycle','worker',observations=collector)
        self.assertIs(actual,job)
        self.assertEqual(request.call_count,2)
        self.assertEqual([r['endpoint'] for r in collector.summary()['records']],['claim','other'])
