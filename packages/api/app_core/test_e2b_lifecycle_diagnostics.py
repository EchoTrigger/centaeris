"""Offline failure-boundary contracts for the owned E2B lifecycle driver."""
import json
import traceback
from types import SimpleNamespace
from unittest.mock import Mock

from django.test import SimpleTestCase

from .test_e2b_consumption_production import drive_lifecycle_job


class E2BLifecycleDiagnosticsTests(SimpleTestCase):
    def fixture(self):
        job_id = 'agent_run.lifecycle:private-fixture-run'
        job = {'jobId': job_id, 'leaseOwner': 'private-fixture-owner'}
        worker = SimpleNamespace(start_job=Mock(), execute_agent_run_lifecycle_job=Mock())
        return job_id, job, worker, Mock(return_value=job)

    def failure(self, worker, job_id, claim, observe, **options):
        try:
            drive_lifecycle_job(worker, job_id, claim, observe, **options)
        except AssertionError as error:
            detail = json.loads(str(error))
            rendered = ''.join(traceback.format_exception(error))
            self.assertIsNone(error.__cause__)
            self.assertIsNone(error.__context__)
            for private in ('private-fixture-run', 'private-fixture-owner', 'private-exception-detail'):
                self.assertNotIn(private, rendered)
                self.assertNotIn(private, json.dumps(detail))
            return detail
        self.fail('The lifecycle driver unexpectedly completed')

    def test_each_failed_stage_is_distinct_and_reads_the_latest_available_state_once(self):
        for stage in ('claim', 'start', 'execute', 'observe'):
            with self.subTest(stage=stage):
                job_id, job, worker, claim = self.fixture()
                observe = Mock(return_value=('failed', 'running'))
                error = RuntimeError('private-exception-detail')
                {'claim': claim, 'start': worker.start_job,
                 'execute': worker.execute_agent_run_lifecycle_job, 'observe': observe}[stage].side_effect = error
                detail = self.failure(worker, job_id, claim, observe)
                self.assertEqual(detail['reason'], 'e2b_product_lifecycle_advance_unavailable')
                self.assertEqual(detail['stage'], stage)
                self.assertEqual(detail['errorCode'], 'runtime_error')
                self.assertEqual(detail['claims'], 1)
                self.assertEqual((detail['runStatus'], detail['jobStatus']),
                                 ('unobserved', 'unobserved') if stage == 'observe' else ('failed', 'running'))
                self.assertEqual(claim.call_count, 1)
                self.assertEqual(worker.start_job.call_count, int(stage != 'claim'))
                self.assertEqual(worker.execute_agent_run_lifecycle_job.call_count, int(stage in {'execute', 'observe'}))
                self.assertEqual(observe.call_count, 1)

    def test_execute_failure_refreshes_state_instead_of_reusing_the_previous_round(self):
        job_id, job, worker, claim = self.fixture()
        worker.execute_agent_run_lifecycle_job.side_effect = [None, TimeoutError('private-exception-detail')]
        observe = Mock(side_effect=[('running', 'queued'), ('failed', 'running')])
        detail = self.failure(worker, job_id, claim, observe)
        self.assertEqual((detail['stage'], detail['errorCode']), ('execute', 'timeout'))
        self.assertEqual((detail['runStatus'], detail['jobStatus']), ('failed', 'running'))
        self.assertEqual(detail['claims'], 2)
        self.assertEqual(claim.call_count, 2)
        self.assertEqual(worker.execute_agent_run_lifecycle_job.call_count, 2)
        self.assertEqual(observe.call_count, 2)

    def test_failed_observation_keeps_last_known_state_and_does_not_retry_it(self):
        job_id, job, worker, claim = self.fixture()
        observe = Mock(side_effect=[('running', 'queued'), ConnectionError('private-exception-detail')])
        detail = self.failure(worker, job_id, claim, observe)
        self.assertEqual((detail['stage'], detail['errorCode']), ('observe', 'connection'))
        self.assertEqual((detail['runStatus'], detail['jobStatus']), ('running', 'queued'))
        self.assertEqual(detail['observationErrorCode'], 'connection')
        self.assertEqual(observe.call_count, 2)
        self.assertEqual(claim.call_count, 2)

    def test_state_refresh_failure_cannot_replace_the_primary_failure(self):
        job_id, job, worker, claim = self.fixture()
        worker.execute_agent_run_lifecycle_job.side_effect = [None, RuntimeError('private-exception-detail')]
        observe = Mock(side_effect=[('running', 'queued'), TimeoutError('private-exception-detail')])
        detail = self.failure(worker, job_id, claim, observe)
        self.assertEqual((detail['stage'], detail['errorCode']), ('execute', 'runtime_error'))
        self.assertEqual(detail['observationErrorCode'], 'timeout')
        self.assertEqual((detail['runStatus'], detail['jobStatus']), ('running', 'queued'))
        self.assertEqual(observe.call_count, 2)
        self.assertEqual(worker.execute_agent_run_lifecycle_job.call_count, 2)

    def test_foreign_claim_keeps_its_specific_failure_and_never_starts_work(self):
        job_id, job, worker, claim = self.fixture()
        claim.return_value = job | {'jobId': 'private-foreign-job'}
        detail = self.failure(worker, job_id, claim, Mock(return_value=('running', 'queued')))
        self.assertEqual(detail['reason'], 'e2b_product_lifecycle_job_mismatch')
        self.assertEqual((detail['stage'], detail['errorCode']), ('claim', 'job_mismatch'))
        worker.start_job.assert_not_called()
        worker.execute_agent_run_lifecycle_job.assert_not_called()
        self.assertNotIn('private-foreign-job', json.dumps(detail))

    def test_worker_failure_types_have_static_categories_without_reading_error_values(self):
        for name, code in (('RuntimeLeaseLost', 'lease_lost'), ('RuntimeJobCancelled', 'job_cancelled'),
                           ('DependencyUnavailable', 'dependency_unavailable'), ('RuntimeStepFailed', 'runtime_step_failed'),
                           ('RetainedCleanupPending', 'retained_cleanup_pending')):
            with self.subTest(kind=name):
                job_id, job, worker, claim = self.fixture()
                error_type = type('PrivateDynamicErrorName', (RuntimeError,), {})
                setattr(worker, name, error_type)
                worker.execute_agent_run_lifecycle_job.side_effect = error_type('private-exception-detail')
                detail = self.failure(worker, job_id, claim, Mock(return_value=('failed', 'running')))
                self.assertEqual(detail['errorCode'], code)
                self.assertNotIn('PrivateDynamicErrorName', json.dumps(detail))

    def test_builtin_failure_types_are_mapped_to_static_categories(self):
        for error_type, code in ((TimeoutError, 'timeout'), (ConnectionError, 'connection'),
                                 (OSError, 'transport_error'), (KeyError, 'invalid_response'),
                                 (ValueError, 'invalid_response'), (TypeError, 'invalid_response'),
                                 (AssertionError, 'assertion_failed'), (RuntimeError, 'runtime_error'),
                                 (Exception, 'unexpected_error')):
            with self.subTest(kind=error_type):
                job_id, job, worker, claim = self.fixture()
                claim.side_effect = error_type('private-exception-detail')
                detail = self.failure(worker, job_id, claim, Mock(return_value=('failed', 'running')))
                self.assertEqual(detail['errorCode'], code)

    def test_diagnostic_reader_failure_is_ignored_without_exposing_its_exception(self):
        job_id, job, worker, claim = self.fixture()
        worker.execute_agent_run_lifecycle_job.side_effect = RuntimeError('private-exception-detail')
        detail = self.failure(worker, job_id, claim, Mock(return_value=('failed', 'running')),
                              diagnostics=Mock(side_effect=RuntimeError('private-exception-detail')))
        self.assertEqual((detail['stage'], detail['errorCode']), ('execute', 'runtime_error'))
        self.assertEqual(detail['workspaceDiagnostics'], [])

    def test_unknown_status_values_are_not_included_in_the_diagnostic(self):
        job_id, job, worker, claim = self.fixture()
        worker.execute_agent_run_lifecycle_job.side_effect = RuntimeError('private-exception-detail')
        detail = self.failure(worker, job_id, claim,
                              Mock(return_value=('private-fixture-run', 'private-fixture-owner')))
        self.assertEqual((detail['runStatus'], detail['jobStatus']), ('unobserved', 'unobserved'))

    def test_canonical_expected_job_identity_is_preserved_for_failure_correlation(self):
        job_id, job, worker, claim = self.fixture()
        job_id = 'agent_run.lifecycle:agent_run_' + 'a' * 32
        claim.return_value = job | {'jobId': job_id}
        worker.execute_agent_run_lifecycle_job.side_effect = RuntimeError('private-exception-detail')
        detail = self.failure(worker, job_id, claim, Mock(return_value=('failed', 'running')))
        self.assertEqual(detail['jobId'], job_id)
        self.assertEqual((detail['stage'], detail['errorCode']), ('execute', 'runtime_error'))

    def test_job_correlation_uses_only_expected_identity_not_claim_or_exception(self):
        job_id, job, worker, claim = self.fixture()
        expected = 'agent_run.lifecycle:agent_run_' + 'a' * 32
        foreign = 'agent_run.lifecycle:agent_run_' + 'b' * 32
        claim.return_value = job | {'jobId': foreign}
        detail = self.failure(worker, expected, claim, Mock(return_value=('running', 'queued')))
        self.assertEqual(detail['jobId'], expected)
        self.assertNotIn(foreign, json.dumps(detail))
        worker.start_job.assert_not_called()
        claim.side_effect = RuntimeError(foreign + ':private-exception-detail')
        detail = self.failure(worker, expected, claim, Mock(return_value=('failed', 'running')))
        self.assertEqual(detail['jobId'], expected)
        self.assertNotIn(foreign, json.dumps(detail))

    def test_noncanonical_expected_job_identity_is_omitted(self):
        prefix = 'agent_run.lifecycle:agent_run_'
        for job_id in ('private-fixture-run', prefix + 'a' * 31, prefix + 'a' * 33,
                       prefix + 'A' * 32, prefix + 'g' * 32, prefix + 'a' * 32 + '\n',
                       ' ' + prefix + 'a' * 32, 'agent_run.lifecycle:' + 'a' * 32):
            with self.subTest(job_id=job_id):
                _, job, worker, claim = self.fixture()
                claim.return_value = job | {'jobId': job_id}
                worker.execute_agent_run_lifecycle_job.side_effect = RuntimeError('private-exception-detail')
                detail = self.failure(worker, job_id, claim, Mock(return_value=('failed', 'running')))
                self.assertNotIn('jobId', detail)
