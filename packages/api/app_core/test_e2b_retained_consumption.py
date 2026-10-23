"""Explicit retained E2B acceptance; ordinary discovery runs only offline guards."""
import os
import queue
import threading
import json
import math
import secrets
import time
from collections import Counter
from types import SimpleNamespace
from unittest.mock import Mock, patch
from urllib.error import HTTPError
from urllib.request import ProxyHandler, Request, build_opener
from uuid import uuid4

from django.conf import settings
from django.db import connection
from django.test import LiveServerTestCase, SimpleTestCase, override_settings

from . import test_e2b_consumption_production as product_fixture
from .e2b_workspace_test_support import WorkspaceObservationCollector


def retirement_http(runtime_url, body, internal_token, maintenance_token):
    headers = {'Content-Type': 'application/json', 'X-Internal-Token': internal_token}
    if maintenance_token is not None:
        headers['X-Environment-Maintenance-Token'] = maintenance_token
    request = Request(runtime_url + '/internal/environments/retire',
        data=json.dumps(body).encode(), headers=headers, method='POST')
    try:
        try:
            response = build_opener(ProxyHandler({})).open(request, timeout=30)
        except HTTPError as error:
            response = error
        with response:
            encoded = response.read(65537)
            if len(encoded) > 65536:
                raise ValueError()
            return response.status, json.loads(encoded)
    except Exception:
        raise AssertionError('retained_retirement_http_unavailable') from None


def retained_acceptance_enabled(environment):
    return (environment.get('CENTAERIS_E2B_PRODUCT_ACCEPTANCE') == '1'
            and environment.get('CENTAERIS_E2B_RETAINED_ACCEPTANCE') == '1')


def require_retained_workspace_observations(summary, run_a, run_b):
    """Require complete positive capture evidence before interpreting no restore."""
    incomplete = 'retained_workspace_evidence_incomplete'
    counters = ('rejectedRecords', 'foreignRecords', 'malformedRecords', 'oversizedLines',
                'droppedRecords', 'droppedBindings', 'conflictingOperations')
    if (not isinstance(summary, dict) or summary.get('schema') != 'e2b.workspaceMeasurements.v1'
            or summary.get('lifecycleStatus') != 'passed' or summary.get('streamComplete') is not True
            or summary.get('lossDetected') is not False or summary.get('aggregation') != 'recordsOnly'
            or summary.get('observationStatus') != 'observed'
            or any(type(summary.get(key)) is not int or summary[key] != 0 for key in counters)):
        raise AssertionError(incomplete)
    expected = {run: 'agent_run.lifecycle:' + run for run in (run_a, run_b)}
    coverage, operations = summary.get('runCoverage'), summary.get('operations')
    if (len(expected) != 2 or not isinstance(coverage, list) or len(coverage) != 2
            or not isinstance(operations, list) or not operations
            or not all(isinstance(operation, dict) for operation in operations)):
        raise AssertionError(incomplete)
    if any(not isinstance(row, dict) or row.get('runId') not in expected
           or row.get('jobId') != expected[row['runId']]
           or type(row.get('operationCount')) is not int or row['operationCount'] < 1
           or row['operationCount'] != sum(op.get('runId') == row['runId'] for op in operations)
           for row in coverage) or {row['runId'] for row in coverage} != set(expected):
        raise AssertionError(incomplete)
    required = {'sourceReadBytes': 'sandbox', 'scannedEntries': 'sandbox',
        'captureElapsedMs': 'runtime', 'sdkDownloadFrameBytes': 'runtime',
        'rawCaptureSpoolWrittenBytes': 'runtime', 'runtimeSpoolWrittenBytes': 'runtime',
        'snapshotLogicalBytes': 'runtime', 'uploadAttempts': 'runtime', 'uploadedBytes': 'runtime',
        'workspaceCommitElapsedMs': 'runtime', 'workspaceCommitDisposition': 'runtime'}
    complete_captures = set()
    for operation in operations:
        run, records = operation.get('runId'), operation.get('records')
        if (run not in expected or operation.get('jobId') != expected[run]
                or not isinstance(records, list) or not all(isinstance(row, dict) for row in records)):
            raise AssertionError(incomplete)
        if run == run_b and (operation.get('trigger') in ('sessionRestore', 'recoveryRestore')
                or any(row.get('metric') in ('restoreDownloadElapsedMs', 'restoreApplyElapsedMs') for row in records)):
            raise AssertionError('retained_workspace_restore_observed')
        if operation.get('trigger') != 'terminalCommit':
            continue
        complete = True
        metric_coverage = operation.get('metricCoverage', {})
        for metric, source in required.items():
            rows = [row for row in records if row.get('metric') == metric]
            if (not rows or metric_coverage.get(metric) != 'observed'
                    or any(row.get('source') != source or row.get('outcome') != 'ok' for row in rows)):
                complete = False
                break
            values = [row.get('value') for row in rows]
            if metric == 'workspaceCommitDisposition':
                valid = all(value == 'full' for value in values)
            else:
                valid = all(type(value) in (int, float) and math.isfinite(value) and value >= 0 for value in values)
                if not metric.endswith('ElapsedMs'):
                    valid = valid and any(value > 0 for value in values)
            if not valid:
                complete = False
                break
        if complete:
            complete_captures.add(run)
    if complete_captures != set(expected):
        raise AssertionError(incomplete)


def stop_runtime_once(fixture, delegate):
    process = fixture.runtime_process
    if process is None:
        return
    try:
        delegate(fixture)
    finally:
        # An unresolved stop retains its owner so later cleanup can finish it.
        if process.poll() is not None:
            fixture.runtime_process = None


def restart_retained_runtime(fixture):
    fixture.stop_runtime()
    fixture.runtime_markers = queue.Queue()
    fixture.runtime_output_drained = threading.Event()
    fixture.runtime_url = fixture.start_runtime()
    fixture.worker = fixture.load_worker(fixture.runtime_url)


def retained_teardown_is_safe(fixture, database, owner):
    if not getattr(fixture, '_owner_verified', False):
        return False
    try:
        with database.cursor() as cursor:
            cursor.execute("SELECT current_database(),shobj_description(oid,'pg_database') FROM pg_database WHERE datname=current_database()")
            product_fixture.require_owned_database(*cursor.fetchone(), owner)
            cursor.execute("SELECT to_regclass('runtime.provider_creation_operations')")
            if cursor.fetchone()[0] is None:
                return True
            cursor.execute('SELECT count(*) FROM runtime.provider_creation_operations WHERE settlement_id IS NULL')
            pending = cursor.fetchone()[0]
            if type(pending) is not int or pending != 0:
                return False
            cursor.execute("SELECT to_regclass('runtime.provider_environment_run_uses')")
            if cursor.fetchone()[0] is None:
                return False
            cursor.execute('SELECT count(*) FROM runtime.provider_environment_run_uses WHERE cleanup_receipt_id IS NULL')
            pending = cursor.fetchone()[0]
            return type(pending) is int and pending == 0
    except Exception:
        return False


class RetainedAcceptanceGuardTests(SimpleTestCase):
    def workspace_report(self, *, restore_trigger=None, restore_metric=None, omit_metric=None,
                         missing_run=None, stream_complete=True):
        collector = WorkspaceObservationCollector()
        for run in ('run-a', 'run-b'):
            job = 'agent_run.lifecycle:' + run
            collector.bind(run, job)
            if run == missing_run:
                continue
            for metric, value, source in (
                ('sourceReadBytes', 128, 'sandbox'), ('scannedEntries', 2, 'sandbox'),
                ('captureElapsedMs', 1.0, 'runtime'), ('sdkDownloadFrameBytes', 128, 'runtime'),
                ('rawCaptureSpoolWrittenBytes', 128, 'runtime'),
                ('runtimeSpoolWrittenBytes', 128, 'runtime'), ('snapshotLogicalBytes', 64, 'runtime'),
                ('uploadAttempts', 1, 'runtime'), ('uploadedBytes', 128, 'runtime'),
                ('workspaceCommitElapsedMs', 2.0, 'runtime'),
                ('workspaceCommitDisposition', 'full', 'runtime'),
            ):
                if metric == omit_metric:
                    continue
                collector.feed(json.dumps({'schema': 'workspace.perf.v1', 'source': source,
                    'phase': 'workspaceProtection', 'operationId': 'capture-' + run,
                    'runId': run, 'jobId': job, 'trigger': 'terminalCommit', 'metric': metric,
                    'value': value, 'outcome': 'ok', 'atUnixMs': 1}))
        if restore_trigger is not None or restore_metric is not None:
            collector.feed(json.dumps({'schema': 'workspace.perf.v1', 'source': 'runtime',
                'phase': 'workspaceProtection', 'operationId': 'restore-run-b', 'runId': 'run-b',
                'jobId': 'agent_run.lifecycle:run-b', 'trigger': restore_trigger or 'terminalCommit',
                'metric': restore_metric or 'restoreApplyElapsedMs', 'value': 0,
                'outcome': 'ok', 'atUnixMs': 2}))
        return collector.summary(lifecycle_passed=True, stream_drained=stream_complete)

    def test_retained_no_restore_requires_complete_positive_capture_for_both_runs(self):
        self.assertIsNone(require_retained_workspace_observations(self.workspace_report(), 'run-a', 'run-b'))

    def test_retained_no_restore_rejects_any_b_restore_even_measured_zero(self):
        for fields in ({'restore_trigger': 'sessionRestore'}, {'restore_trigger': 'recoveryRestore'},
                       {'restore_metric': 'restoreDownloadElapsedMs'}, {'restore_metric': 'restoreApplyElapsedMs'}):
            with self.subTest(fields=fields), self.assertRaisesRegex(AssertionError, 'retained_workspace_restore_observed'):
                require_retained_workspace_observations(self.workspace_report(**fields), 'run-a', 'run-b')

    def test_retained_no_restore_never_interprets_missing_or_lost_records_as_zero(self):
        for fields in ({'missing_run': 'run-a'}, {'missing_run': 'run-b'}, {'stream_complete': False},
                       {'omit_metric': 'sourceReadBytes'}, {'omit_metric': 'sdkDownloadFrameBytes'},
                       {'omit_metric': 'workspaceCommitDisposition'}, {'omit_metric': 'captureElapsedMs'}):
            with self.subTest(fields=fields), self.assertRaisesRegex(AssertionError, 'retained_workspace_evidence_incomplete'):
                require_retained_workspace_observations(self.workspace_report(**fields), 'run-a', 'run-b')

    def test_retained_no_restore_rejects_missing_collector_and_nonempty_loss_counters(self):
        for change in (None, {'lossDetected': True}, {'droppedRecords': 1}, {'foreignRecords': 1},
                       {'lifecycleStatus': 'failed'}, {'observationStatus': 'notObserved'}):
            summary = None if change is None else self.workspace_report() | change
            with self.subTest(change=change), self.assertRaisesRegex(AssertionError, 'retained_workspace_evidence_incomplete'):
                require_retained_workspace_observations(summary, 'run-a', 'run-b')

    def database(self, *, count=0, pending_uses=0, name='test_retained_owned', comment='centaeris:e2b-product:owner', table=True):
        cursor = Mock()
        cursor.fetchone.side_effect = [(name, comment), ('runtime.provider_creation_operations' if table else None,),
            (count,), ('runtime.provider_environment_run_uses',), (pending_uses,)]
        database = Mock()
        database.cursor.return_value.__enter__ = Mock(return_value=cursor)
        database.cursor.return_value.__exit__ = Mock(return_value=False)
        return database, cursor

    def test_live_discovery_requires_both_explicit_opt_ins(self):
        product = 'CENTAERIS_E2B_PRODUCT_ACCEPTANCE'
        retained = 'CENTAERIS_E2B_RETAINED_ACCEPTANCE'
        for environment in ({}, {product: '1'}, {retained: '1'}, {product: '1', retained: 'true'}):
            with self.subTest(environment=environment):
                self.assertFalse(retained_acceptance_enabled(environment))
        self.assertTrue(retained_acceptance_enabled({product: '1', retained: '1'}))

    def test_stop_is_idempotent_after_a_runtime_has_exited(self):
        process = SimpleNamespace(exit_code=None)
        process.poll = lambda: process.exit_code
        fixture = SimpleNamespace(runtime_process=process)

        def stopped(value):
            self.assertIs(value.runtime_process, process)
            self.assertIsNone(process.exit_code, 'already stopped Runtime was stopped again')
            process.exit_code = 0

        delegate = Mock(side_effect=stopped)
        stop_runtime_once(fixture, delegate)
        stop_runtime_once(fixture, delegate)
        self.assertEqual(delegate.call_count, 1)
        self.assertIsNone(fixture.runtime_process)

    def test_stop_failure_keeps_a_still_live_runtime_owned(self):
        process = SimpleNamespace(poll=lambda: None)
        fixture = SimpleNamespace(runtime_process=process)
        with self.assertRaisesRegex(RuntimeError, 'stop incomplete'):
            stop_runtime_once(fixture, Mock(side_effect=RuntimeError('stop incomplete')))
        self.assertIs(fixture.runtime_process, process)

    def test_failed_exit_is_not_stopped_and_closed_twice(self):
        process = SimpleNamespace(poll=lambda: 1)
        fixture = SimpleNamespace(runtime_process=process)
        delegate = Mock(side_effect=AssertionError('failed Runtime exit'))
        with self.assertRaisesRegex(AssertionError, 'failed Runtime exit'):
            stop_runtime_once(fixture, delegate)
        stop_runtime_once(fixture, delegate)
        self.assertEqual(delegate.call_count, 1)
        self.assertIsNone(fixture.runtime_process)

    def test_restart_discards_only_the_old_readiness_and_drain_markers(self):
        old_markers = queue.Queue()
        old_markers.put('process-exited')
        old_drained = threading.Event()
        old_drained.set()
        fixture = SimpleNamespace(runtime_markers=old_markers, runtime_output_drained=old_drained,
            runtime_url='old-runtime', worker='old-worker', stop_runtime=Mock())

        def started():
            self.assertIsNot(fixture.runtime_markers, old_markers)
            self.assertTrue(fixture.runtime_markers.empty())
            self.assertIsNot(fixture.runtime_output_drained, old_drained)
            self.assertFalse(fixture.runtime_output_drained.is_set())
            return 'new-runtime'

        fixture.start_runtime = Mock(side_effect=started)
        fixture.load_worker = Mock(return_value='new-worker')
        restart_retained_runtime(fixture)
        fixture.stop_runtime.assert_called_once_with()
        fixture.load_worker.assert_called_once_with('new-runtime')
        self.assertEqual((fixture.runtime_url, fixture.worker), ('new-runtime', 'new-worker'))
        self.assertEqual(old_markers.get_nowait(), 'process-exited')

    def test_restart_cannot_start_a_second_runtime_after_failed_stop(self):
        fixture = SimpleNamespace(stop_runtime=Mock(side_effect=RuntimeError('still running')),
            start_runtime=Mock(), load_worker=Mock())
        with self.assertRaisesRegex(RuntimeError, 'still running'):
            restart_retained_runtime(fixture)
        fixture.start_runtime.assert_not_called()
        fixture.load_worker.assert_not_called()

    def test_teardown_requires_owned_database_and_no_pending_creation_duty(self):
        fixture = SimpleNamespace(_owner_verified=True)
        database, _ = self.database(count=0)
        self.assertTrue(retained_teardown_is_safe(fixture, database, 'owner'))
        for count in (1, None, '0', False, -1):
            with self.subTest(count=count):
                database, _ = self.database(count=count)
                self.assertFalse(retained_teardown_is_safe(fixture, database, 'owner'))
        for fields in ({'name': 'production'}, {'comment': None}, {'comment': 'centaeris:e2b-product:other'}):
            with self.subTest(fields=fields):
                database, _ = self.database(**fields)
                self.assertFalse(retained_teardown_is_safe(fixture, database, 'owner'))
        database, _ = self.database(pending_uses=1)
        self.assertFalse(retained_teardown_is_safe(fixture, database, 'owner'))

    def test_teardown_unknown_or_unverified_state_preserves_business_grants(self):
        database = Mock()
        database.cursor.side_effect = RuntimeError('database unavailable')
        self.assertFalse(retained_teardown_is_safe(SimpleNamespace(_owner_verified=True), database, 'owner'))
        database.reset_mock()
        self.assertFalse(retained_teardown_is_safe(SimpleNamespace(_owner_verified=False), database, 'owner'))
        database.cursor.assert_not_called()


if retained_acceptance_enabled(os.environ):
    class E2BRetainedConsumptionProductionTests(LiveServerTestCase):
        """Same VM and immutable origin across separately authorized A/B Runs."""
        serialized_rollback = False
        admit = product_fixture.E2BConsumptionProductionTests.admit
        claim_due_job = product_fixture.E2BConsumptionProductionTests.claim_due_job
        assert_artifact = product_fixture.E2BConsumptionProductionTests.assert_artifact
        assert_provider_absent = product_fixture.E2BConsumptionProductionTests.assert_provider_absent

        def setUp(self):
            if os.environ.get('WORKSPACE_PERF_OBSERVATIONS') != '1':
                raise AssertionError('retained_workspace_observations_required')
            try:
                encoded = product_fixture.Path(os.environ['RUNTIME_E2B_PROFILE_PATH']).read_bytes()
                if len(encoded) > 65536:
                    raise ValueError()
                selected = json.loads(encoded)
                from .environment_profile import validate_environment_profile
                validate_environment_profile(selected)
                if selected['schema'] != 'runtime.e2b_environment_profile.v1' or selected['lifetime'] != 'retained':
                    raise ValueError()
            except Exception:
                raise AssertionError('explicit_retained_provider_profile_required') from None
            self.maintenance_token = secrets.token_urlsafe(32)
            self.assertNotEqual(self.maintenance_token, settings.INTERNAL_API_TOKEN)
            self.retained_marker = '/tmp/centaeris-retained-proof-' + uuid4().hex
            self.timings = {}
            self.retained_uses = {}
            self.retained_stream_completions = []
            product_fixture.E2BConsumptionProductionTests.setUp(self)

        def load_worker(self, runtime_url):
            with patch.dict(os.environ, {'AGENT_RUN_EXECUTION_MODE': 'retained'}):
                worker = product_fixture.E2BConsumptionProductionTests.load_worker(self, runtime_url)
            self.assertEqual(worker.AGENT_RUN_EXECUTION_MODE, 'retained')
            return worker

        def start_runtime(self):
            with patch.dict(os.environ, {'ENVIRONMENT_MAINTENANCE_TOKEN': self.maintenance_token}):
                return product_fixture.E2BConsumptionProductionTests.start_runtime(self)

        def stop_runtime(self):
            process = self.runtime_process
            try:
                stop_runtime_once(self, product_fixture.E2BConsumptionProductionTests.stop_runtime)
            finally:
                if process is not None and process.poll() is not None:
                    self.retained_stream_completions.append(process.returncode == 0
                        and self.runtime_output_drained.is_set() and not self.runtime_reader.is_alive())

        def _fixture_teardown(self):
            if not retained_teardown_is_safe(self, connection, os.environ.get('CENTAERIS_E2B_PRODUCT_DB_OWNER')):
                print('e2b-retained-responsibility:pending', flush=True)
                self.fail('Unresolved or unverified retained responsibility remains; owned database must remain')
            super()._fixture_teardown()

        def model_response(self, model, request):
            response = product_fixture.E2BConsumptionProductionTests.model_response(self, model, request)
            # The workspace archive excludes /tmp. This marker therefore checks
            # same-instance guest continuity independently of archived files.
            for call in response['toolCalls']:
                if call['name'] == 'bash':
                    arguments = json.loads(call['argsJson'])
                    if self.runs.index(request['agentRunId']) == 0:
                        arguments['command'] += f"; printf 'same-instance-A\\n' > {self.retained_marker}"
                    else:
                        arguments['command'] = (f"test \"$(cat {self.retained_marker})\" = same-instance-A && "
                                                + arguments['command'])
                    call['argsJson'] = json.dumps(arguments)
            return response

        def creation_target(self, environment_id):
            with connection.cursor() as cursor:
                cursor.execute('SELECT attempt_id,execution_id,instance_id,environment_id,session_id,workspace_id,'
                    'agent_run_id,authorization_digest,profile_digest,processing_job_id,provider_config_digest,'
                    'reported_sandbox_id,verified_sandbox_id,settlement_id FROM runtime.provider_creation_operations '
                    'WHERE environment_id=%s', [environment_id])
                rows = cursor.fetchall()
            self.assertEqual(len(rows), 1, 'retained A/B requires one original creation attempt')
            row = rows[0]
            self.assertIsNotNone(row[11])
            self.assertEqual(row[11], row[12])
            self.assertIsNone(row[13], 'Run cleanup must preserve the original provider responsibility')
            metadata = dict(zip(('centaerisAttemptId', 'executionId', 'instanceId', 'environmentId', 'sessionId',
                'workspaceId', 'agentRunId', 'authorizationDigest', 'profileDigest', 'processingJobId'), row[:10]))
            return {'creationAttemptId': row[0], 'instanceId': row[2], 'environmentId': row[3],
                'providerConfigDigest': row[10], 'sandboxId': row[11], 'metadata': metadata}

        def assert_provider_present(self, sdk, target):
            try:
                info = sdk.Sandbox.get_info(target['sandboxId'], retries=0)
            except Exception:
                raise AssertionError('retained_provider_identity_unavailable') from None
            self.assertEqual(info.sandbox_id, target['sandboxId'])
            self.assertEqual(info.template_id, self.profile['templateRef']['templateId'])
            self.assertEqual(info.metadata, target['metadata'])
            self.assertEqual(getattr(info.state, 'value', info.state), 'running')

        def execute_retained(self, run, role):
            started = time.monotonic()
            job_id = 'agent_run.lifecycle:' + run.pk

            def observe():
                run.refresh_from_db()
                return run.status, self.worker.runtime_request('/internal/jobs/' + job_id)['job']['status']

            product_fixture.drive_lifecycle_job(self.worker, job_id,
                lambda: self.claim_due_job(self.worker, job_id, 'agent_run.lifecycle', 'worker:e2b-retained'),
                observe, diagnostics=lambda: tuple(self.runtime_lines))
            self.timings['runLifecycle' + role + 'Ms'] = round((time.monotonic() - started) * 1000, 3)
            self.assertEqual(run.status, 'completed')
            self.assertEqual(self.worker.runtime_request('/internal/jobs/' + job_id)['job']['status'], 'succeeded')
            self.assertEqual(run.events.filter(payload__type='agent_run_completed').count(), 1)
            results = list(run.events.filter(payload__type='tool_result').values_list('payload', flat=True))
            self.assertEqual(Counter(row['payload']['callId'] for row in results),
                Counter(f'e2b-{len(self.runs)-1}-{index}' for index in range(4)))
            self.assertTrue(all(row['payload'].get('resultState') in ('successWithOutput', 'successNoOutput')
                                for row in results), 'retained Core tool failed')
            self.profile = run.authorization.payload['environmentProfile']
            self.assertEqual(self.profile['lifetime'], 'retained')
            release = product_fixture.AgentRunEnvironmentRelease.objects.get(
                agent_run=run, authorization_digest=run.authorization.digest)
            run.environment.refresh_from_db()
            self.assertTrue(run.environment.controller_released)
            with connection.cursor() as cursor:
                cursor.execute('SELECT provider_use_id,provider_use_digest,authorization_digest,execution_id,'
                    'cleanup_receipt_id,cleanup_controller_epoch FROM runtime.provider_environment_run_uses '
                    'WHERE agent_run_id=%s AND environment_id=%s', [run.pk, run.environment_id])
                rows = cursor.fetchall()
            self.assertEqual(len(rows), 1, 'each retained Run needs its own immutable use record')
            use = rows[0]
            self.assertEqual(use[2], run.authorization.digest)
            self.assertIsNotNone(use[4])
            self.assertEqual(use[5], release.controller_epoch)
            from .runtime_client import get_agent_run_cleanup_receipt
            receipt = get_agent_run_cleanup_receipt(run.pk, run.authorization.digest, use[4],
                frozen_profile=self.profile, frozen_profile_digest=run.authorization.payload['environmentProfileDigest'])
            self.assertEqual(receipt['schema'], 'runtime.agent_run.cleanup_receipt.retained_provider.v1')
            self.assertEqual(receipt['scope'], 'runEffectsOnly')
            self.assertEqual((receipt['providerUseId'], receipt['providerUseDigest']), (use[0], use[1]))
            self.retained_uses[run.pk] = {'providerUseId': use[0], 'executionId': use[3], 'receipt': receipt}
            return self.creation_target(run.environment_id)

        def test_real_e2b_retained_ab_restart_and_independent_retirement(self):
            from e2b import E2B
            sdk = E2B(api_key=os.environ['E2B_API_KEY'], domain=os.environ.get('E2B_DOMAIN'),
                api_url=os.environ['E2B_API_URL'], sandbox_url=os.environ['E2B_SANDBOX_URL'],
                retries=0, debug=False, request_timeout=5)
            with patch('app_core.model_adapter.fake_model_response', side_effect=self.model_response):
                with override_settings(RUNTIME_URL=self.runtime_url):
                    first = self.admit('new', 'Write A proof, preserve the same environment, and publish delivery A.')
                    target = self.execute_retained(first, 'A')
                    self.assert_artifact(first, product_fixture.ARTIFACT_A)
                    self.assertEqual(target['metadata']['agentRunId'], first.pk)
                    self.assert_provider_present(sdk, target)
                    session = product_fixture.Session.objects.get(pk=first.session_id)
                    first_generation, first_hash = session.workspaceGeneration, session.workspaceSnapshotSha256
                    self.assertGreater(first_generation, 0)
                    self.assertTrue(first_hash)

                started = time.monotonic()
                restart_retained_runtime(self)
                self.timings['runtimeRestartMs'] = round((time.monotonic() - started) * 1000, 3)
                self.assert_provider_present(sdk, target)
                with override_settings(RUNTIME_URL=self.runtime_url):
                    second = self.admit(session.pk, 'Verify retained A, append B, and publish delivery B.')
                    second_target = self.execute_retained(second, 'B')
                    self.assertEqual(second.environment_id, first.environment_id)
                    self.assertEqual(second_target, target)
                    self.assertNotEqual(second.pk, target['metadata']['agentRunId'])
                    self.assertNotEqual(self.retained_uses[first.pk]['providerUseId'], self.retained_uses[second.pk]['providerUseId'])
                    self.assertNotEqual(self.retained_uses[first.pk]['executionId'], self.retained_uses[second.pk]['executionId'])
                    self.assert_provider_present(sdk, target)
                    self.assert_artifact(second, product_fixture.FILE_B)
                    session.refresh_from_db()
                    self.assertEqual(session.workspaceGeneration, first_generation + 1)
                    self.assertNotEqual(session.workspaceSnapshotSha256, first_hash)
                    self.assertEqual(len(self.requests), 10)

                    second.environment.refresh_from_db()
                    body = {'schema': 'runtime.provider_environment.retire.v1',
                        'operationId': 'e2b-retained-retire-' + uuid4().hex,
                        'environmentId': target['environmentId'], 'creationAttemptId': target['creationAttemptId'],
                        'sandboxId': target['sandboxId'], 'providerConfigDigest': target['providerConfigDigest'],
                        'expectedControllerEpoch': second.environment.controller_epoch, 'acknowledgeDataLoss': True}
                    status, _ = retirement_http(self.runtime_url, body, settings.INTERNAL_API_TOKEN, None)
                    self.assertEqual(status, 403, 'ordinary internal authority cannot retire a retained environment')
                    self.assert_provider_present(sdk, target)
                    started = time.monotonic()
                    status, retired = retirement_http(self.runtime_url, body, settings.INTERNAL_API_TOKEN, self.maintenance_token)
                    self.timings['environmentRetirementMs'] = round((time.monotonic() - started) * 1000, 3)
                    self.assertIn(status, (200, 202))
                    self.assertEqual(retired, {key: body[key] for key in
                        ('operationId', 'environmentId', 'creationAttemptId', 'sandboxId')} |
                        {'schema': 'runtime.provider_environment.retire.result.v1', 'status': 'removed'})
                    self.assert_provider_absent(target['sandboxId'])
                    with connection.cursor() as cursor:
                        cursor.execute('SELECT settlement_kind,settlement_sandbox_id FROM runtime.provider_creation_operations '
                            'WHERE attempt_id=%s', [target['creationAttemptId']])
                        self.assertEqual(cursor.fetchone(), ('removed', target['sandboxId']))
                    from .runtime_client import get_agent_run_cleanup_receipt
                    for run in (first, second):
                        recorded = self.retained_uses[run.pk]['receipt']
                        self.assertEqual(get_agent_run_cleanup_receipt(run.pk, run.authorization.digest, recorded['receiptId'],
                            frozen_profile=self.profile, frozen_profile_digest=run.authorization.payload['environmentProfileDigest']), recorded)
                    self.assertTrue(retained_teardown_is_safe(self, connection, os.environ.get('CENTAERIS_E2B_PRODUCT_DB_OWNER')))
            self.lifecycle_passed = True
            # Absence of a B restore is meaningful only after both Runtime
            # streams have exited cleanly and the bounded collector has drained.
            self.stop_runtime()
            self.assertEqual(self.retained_stream_completions, [True, True])
            self.assertIsNotNone(self.workspace_observations)
            observations = self.workspace_observations.summary(lifecycle_passed=True,
                stream_drained=all(self.retained_stream_completions))
            require_retained_workspace_observations(observations, first.pk, second.pk)
            print('e2b-retained-product-measurements:' + json.dumps({'schema': 'e2b.retained.acceptance.v1',
                'sandboxId': target['sandboxId'], 'instanceId': target['instanceId'],
                'runAId': first.pk, 'runBId': second.pk, 'providerUseAId': self.retained_uses[first.pk]['providerUseId'],
                'providerUseBId': self.retained_uses[second.pk]['providerUseId'], 'providerAbsent': True,
                'pendingDuties': 0, 'workspaceNoRestoreBVerified': True,
                'workspaceCaptureOperationIds': {run.pk: [operation['operationId'] for operation in observations['operations']
                    if operation['runId'] == run.pk and operation['trigger'] == 'terminalCommit'] for run in (first, second)},
                'timings': self.timings}, sort_keys=True), flush=True)
