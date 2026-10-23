"""Acceptance wrappers preserve the actual Worker calls and their arguments."""
import io
import json
import os
from pathlib import Path
import tempfile
from contextlib import redirect_stdout
from types import SimpleNamespace
from unittest.mock import Mock, patch
from django.test import SimpleTestCase
from .test_e2b_consumption_production import emit_lifecycle_call_report, observe_lifecycle_worker


class LifecycleCallReportDeliveryTests(SimpleTestCase):
    environment_key = 'CENTAERIS_E2B_LIFECYCLE_CALL_REPORT_PATH'
    prefix = 'e2b-lifecycle-call-measurements:'
    incomplete = {'coverageStatus': 'incomplete', 'observationUnavailable': True}

    def collector(self, report):
        return SimpleNamespace(marker=Mock(), summary=Mock(return_value=report))

    def test_default_stdout_preserves_the_report_and_one_summary_call(self):
        report = {'coverageStatus': 'unknown', 'records': [{'sequence': 9, 'value': None}]}
        collector = self.collector(report)
        with patch.dict(os.environ), redirect_stdout(io.StringIO()) as output:
            os.environ.pop(self.environment_key, None)
            emit_lifecycle_call_report(collector)
        self.assertEqual(json.loads(output.getvalue().removeprefix(self.prefix)), report)
        collector.marker.assert_called_once_with('sampleEnd')
        collector.summary.assert_called_once_with()

    def test_deferred_file_keeps_long_json_whole_while_progress_is_printed(self):
        report = {'coverageStatus': 'unknown', 'records': [
            {'sequence': index, 'value': None, 'extra': 'preserved' * 30} for index in range(50)]}
        collector = self.collector(report)
        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory) / 'calls.json'
            with patch.dict(os.environ, {self.environment_key: str(target)}), \
                    patch('os.fsync', wraps=os.fsync) as sync, redirect_stdout(io.StringIO()) as output:
                print('.', end='')
                emit_lifecycle_call_report(collector)
                print('.', end='')
            self.assertEqual(output.getvalue(), '..')
            self.assertEqual(json.loads(target.read_text(encoding='utf-8')), report)
            self.assertEqual(sync.call_count, 1)
        collector.marker.assert_called_once_with('sampleEnd')
        collector.summary.assert_called_once_with()

    def test_second_delivery_never_overwrites_an_existing_report(self):
        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory) / 'calls.json'
            first = self.collector({'value': 'first'})
            second = self.collector({'value': 'second'})
            with patch.dict(os.environ, {self.environment_key: str(target)}), redirect_stdout(io.StringIO()) as output:
                emit_lifecycle_call_report(first)
                original = target.read_bytes()
                emit_lifecycle_call_report(second)
            self.assertEqual(target.read_bytes(), original)
            self.assertEqual(output.getvalue(), '')
        first.summary.assert_called_once_with()
        second.summary.assert_called_once_with()

    def test_relative_outside_temp_and_traversal_paths_are_silent_observation_failures(self):
        temporary_root = Path(tempfile.gettempdir())
        outside = temporary_root.parent / 'private-report-must-not-be-created.json'
        values = ('', 'relative-report.json', str(outside), str(temporary_root / '..' / outside.name))
        for value in values:
            with self.subTest(value=value), patch.dict(os.environ, {self.environment_key: value}), \
                    patch('pathlib.Path.open') as opened, redirect_stdout(io.StringIO()) as output:
                emit_lifecycle_call_report(self.collector({'value': 'unchanged'}))
                opened.assert_not_called()
                self.assertEqual(output.getvalue(), '')

    def test_symlink_target_is_rejected_without_opening_or_replacing_it(self):
        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory) / 'private-link.json'
            with patch.dict(os.environ, {self.environment_key: str(target)}), \
                    patch('pathlib.Path.is_symlink', return_value=True), \
                    patch('pathlib.Path.open') as opened, redirect_stdout(io.StringIO()) as output:
                emit_lifecycle_call_report(self.collector({'value': 'unchanged'}))
            opened.assert_not_called()
            self.assertEqual(output.getvalue(), '')

    def test_symlink_parent_is_rejected_before_file_creation(self):
        with tempfile.TemporaryDirectory() as directory:
            parent = Path(directory)
            target = parent / 'calls.json'
            with patch.dict(os.environ, {self.environment_key: str(target)}), \
                    patch('pathlib.Path.is_symlink', autospec=True, side_effect=lambda path: path == parent), \
                    patch('pathlib.Path.open') as opened, redirect_stdout(io.StringIO()) as output:
                emit_lifecycle_call_report(self.collector({'value': 'unchanged'}))
            opened.assert_not_called()
            self.assertEqual(output.getvalue(), '')

    def test_summary_failure_delivers_only_static_incomplete_without_exception_details(self):
        collector = self.collector(None)
        collector.summary.side_effect = RuntimeError('private-exception-detail')
        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory) / 'calls.json'
            with patch.dict(os.environ, {self.environment_key: str(target)}), redirect_stdout(io.StringIO()) as output:
                emit_lifecycle_call_report(collector)
            self.assertEqual(json.loads(target.read_text(encoding='utf-8')), self.incomplete)
            self.assertEqual(output.getvalue(), '')
            self.assertNotIn('private-exception-detail', target.read_text(encoding='utf-8'))
        collector.summary.assert_called_once_with()

    def test_failed_durable_write_leaves_no_complete_report_and_no_private_error(self):
        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory) / 'private-calls.json'
            with patch.dict(os.environ, {self.environment_key: str(target)}), \
                    patch('os.fsync', side_effect=OSError('private-exception-detail')), \
                    redirect_stdout(io.StringIO()) as output:
                emit_lifecycle_call_report(self.collector({'coverageStatus': 'complete'}))
            self.assertFalse(target.exists())
            self.assertEqual(output.getvalue(), '')

    def test_default_summary_failure_keeps_the_static_stdout_fallback(self):
        collector = self.collector(None)
        collector.summary.side_effect = RuntimeError('private-exception-detail')
        with patch.dict(os.environ), redirect_stdout(io.StringIO()) as output:
            os.environ.pop(self.environment_key, None)
            emit_lifecycle_call_report(collector)
        self.assertEqual(json.loads(output.getvalue().removeprefix(self.prefix)), self.incomplete)

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
