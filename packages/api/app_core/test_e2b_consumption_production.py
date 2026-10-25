"""Opt-in real E2B lifecycle through public admission, Worker and existing Core.

The ordinary source gate checks only explicit launcher/loopback selection. Set
CENTAERIS_E2B_PRODUCT_ACCEPTANCE=1 to discover the real LiveServer test. Its
prebuilt Runtime argv is explicit JSON; both processes run in the same Linux
network context. Only synthetic model responses are simulated.
"""
from collections import Counter, deque
from copy import deepcopy
import hashlib
import ipaddress
import json
import os
from pathlib import Path
import queue
import re
import subprocess
import threading
import time
from types import SimpleNamespace
from unittest import skipUnless
from unittest.mock import Mock, patch
from urllib.parse import quote, urlsplit

from django.conf import settings
from django.contrib.auth import get_user_model
from django.core.files.storage import default_storage
from django.db import connection
from django.test import LiveServerTestCase, SimpleTestCase, override_settings

from . import test_native_consumption_production as native_fixture
from .e2b_lifecycle_timing_test_support import (LifecycleCallCollector, feed_runtime_line,
    observe_first_successor_tool_followup, observe_lifecycle_marker, per_run_acceptance_report)
from .e2b_workspace_test_support import collect_runtime_output, workspace_observation_collector
from .e2b_workspace_trace_test_support import expected_manifest, fixed_trace_case, trace_steps, trace_workload_report
from .model_adapter.common import fake_tool_call, zero_usage
from .models import Agent, AgentRun, AgentRunEnvironmentRelease, Artifact, ModelConfig, Session, Workspace, WorkspaceMembership

RUNTIME_TEST = 'e2b_consumption_tests::e2b_consumption_production_server'
READY = 'e2b-product-ready:'
FILE_A = b'workspace-A\nbash-A\n'
FILE_B = FILE_A + b'workspace-B\n'
ARTIFACT_A = b'artifact-A\n'


def runtime_argv(value):
    try:
        result = json.loads(value) if isinstance(value, str) else None
    except (ValueError, TypeError):
        result = None
    if (not isinstance(result, list) or not 5 <= len(result) <= 32
            or any(not isinstance(item, str) or not item or '\x00' in item for item in result)
            or RUNTIME_TEST not in result
            or not {'--ignored', '--exact', '--nocapture'}.issubset(result)):
        raise ValueError('Explicit prebuilt E2B Runtime test argv is required')
    return result


def runtime_shutdown_wait_seconds(environment):
    # Runtime drains at most two adapter requests plus ten seconds. Allow a
    # further ten seconds for process exit and stdout drain before forcing it.
    try:
        request_timeout = int(environment.get('RUNTIME_E2B_REQUEST_TIMEOUT_SECONDS', '20'))
    except (TypeError, ValueError):
        raise ValueError('E2B shutdown request timeout invalid') from None
    if not 1 <= request_timeout <= 60:
        raise ValueError('E2B shutdown request timeout invalid')
    return request_timeout * 2 + 20


def require_local_provider(environment):
    for name in ('E2B_API_URL', 'E2B_SANDBOX_URL'):
        try:
            url = urlsplit(environment.get(name, ''))
            host = url.hostname
            loopback = host == 'localhost' or ipaddress.ip_address(host or '').is_loopback
        except ValueError:
            loopback = False
        if not loopback or url.scheme not in ('http', 'https') or url.username or url.password:
            raise ValueError('E2B acceptance requires explicit loopback provider endpoints')
    if (environment.get('RUNTIME_EXECUTION_PROVIDER') != 'e2b'
            or environment.get('CENTAERIS_NATIVE_CONSUME_PRODUCTION_FIXTURE')):
        raise ValueError('E2B acceptance requires the real E2B backend without a native fixture')


def require_owned_database(name, comment, owner):
    if (not isinstance(name, str) or not name.startswith('test_')
            or not isinstance(owner, str) or not 1 <= len(owner) <= 128
            or not owner.isascii() or any(not (char.isalnum() or char in '-_') for char in owner)
            or comment != 'centaeris:e2b-product:' + owner):
        raise ValueError('Explicit owned disposable E2B database is required')


WORKSPACE_DIAGNOSTIC = re.compile(
    r'e2b_workspace_protection: stage=(resolve|exclusive|quiesce|capture); code=([a-z0-9_]{1,80})')
WORKSPACE_DIAGNOSTIC_CODES = frozenset({
    'e2b_workspace_activity_unavailable', 'e2b_prior_helper_effects_unknown',
    'e2b_dispatch_authority_rejected', 'e2b_dispatch_authority_binding_rejected',
    'e2b_dispatch_authority_target_rejected', 'e2b_dispatch_outcome_unconfirmed',
    'e2b_dispatch_callback_replayed', 'e2b_activity_unavailable',
    'e2b_adapter_not_dispatched', 'e2b_adapter_result_unknown', 'e2b_adapter_result_missing',
    'e2b_control_receipt_invalid', 'e2b_control_helper_failed', 'e2b_control_helper_unsupported',
    'e2b_control_helper_input_invalid', 'e2b_process_state_invalid',
    'e2b_workspace_not_quiescent', 'e2b_quiescence_output_invalid', 'e2b_quiescence_unconfirmed',
    'e2b_helper_process_failed', 'e2b_helper_health_failed', 'e2b_helper_health_invalid',
    'e2b_helper_input_staging_unconfirmed', 'e2b_helper_output_staging_unconfirmed',
    'e2b_capture_transfer_limit', 'e2b_capture_request_invalid', 'e2b_capture_spool_unavailable',
})


def safe_workspace_diagnostics(lines):
    accepted = []
    for line in lines:
        match = WORKSPACE_DIAGNOSTIC.fullmatch(line.rstrip('\r\n'))
        if match and ((match[1], match[2]) in {('resolve', 'api_unavailable'), ('exclusive', 'activity_busy')}
                      or (match[1] != 'resolve' and match[2] in WORKSPACE_DIAGNOSTIC_CODES)):
            accepted.append(match[0])
    return accepted[-8:]


def fixed_trace_model_response(case, run_index, ordinal):
    script = trace_steps(case, run_index)
    if type(ordinal) is not int or not 0 <= ordinal <= len(script):
        raise AssertionError('fixed E2B trace exceeded its bounded scripted model calls')
    if ordinal == len(script):
        return {'text': 'Fixed E2B workspace trace complete.', 'toolCalls': [], 'usage': zero_usage()}
    name, arguments = script[ordinal]
    return fake_tool_call(f'e2b-{run_index}-{ordinal}', name, arguments)


def observe_lifecycle_worker(worker, collector):
    """Measure real transport boundaries without acquiring authority or retries."""
    if collector is None:
        return worker
    worker.runtime_request = collector.wrap('runtime', worker.runtime_request)
    worker.api_request = collector.wrap('api', worker.api_request)
    for name, path in (('agent_run_step_request', '/agent-runs/step'),
                       ('runtime_teardown_request', '/agent-runs/teardown'),
                       ('wait_for_jobs', '/internal/jobs/wait')):
        original = getattr(worker, name)
        def transport(_path, *args, _original=original, **kwargs):
            return _original(*args, **kwargs)
        measured = collector.wrap('runtime', transport)
        def bound(*args, _path=path, _measured=measured, **kwargs):
            return _measured(_path, *args, **kwargs)
        setattr(worker, name, bound)
    return worker


def emit_lifecycle_call_report(collector):
    import tempfile
    from pathlib import Path
    prefix = 'e2b-lifecycle-call-measurements:'
    incomplete = '{"coverageStatus":"incomplete","observationUnavailable":true}'
    deferred_path = os.environ.get('CENTAERIS_E2B_LIFECYCLE_CALL_REPORT_PATH')
    try:
        collector.marker('sampleEnd')
        report = json.dumps(collector.summary(), sort_keys=True)
    except Exception:
        report = incomplete
    if deferred_path is None:
        print(prefix + report, flush=True)
        return
    target = None
    created = None
    try:
        target = Path(deferred_path)
        if not target.is_absolute() or '..' in target.parts or target.is_symlink():
            return
        if any(parent.is_symlink() for parent in target.parents):
            return
        temporary_root = Path(tempfile.gettempdir()).resolve(strict=True)
        parent = target.parent.resolve(strict=True)
        parent.relative_to(temporary_root)
        target = parent / target.name
        with target.open('x', encoding='utf-8', newline='') as output:
            created = os.fstat(output.fileno())
            output.write(report)
            output.flush()
            os.fsync(output.fileno())
    except Exception:
        # Only remove our own incomplete file. The wrapper emits a static
        # unavailable record after Django's progress output has finished.
        if created is not None:
            try:
                if not target.is_symlink() and os.path.samestat(created, target.lstat()):
                    target.unlink()
            except Exception:
                pass


def drive_lifecycle_job(worker, job_id, claim, observe, *, clock=time.monotonic, diagnostics=lambda: ()):
    """Reclaim only this test Job, respecting its existing durable due time.

    Limits are checked between rounds; an in-flight RPC retains its ordinary
    timeout and cannot be interrupted by this fixture's elapsed-time check.
    Failure diagnostics refresh state without retrying lifecycle work.
    """
    started = clock()
    state = ('unobserved', 'unobserved')

    def error_code(error):
        for name, code in (('RuntimeLeaseLost', 'lease_lost'), ('RuntimeJobCancelled', 'job_cancelled'),
                           ('DependencyUnavailable', 'dependency_unavailable'), ('RuntimeStepFailed', 'runtime_step_failed'),
                           ('RetainedCleanupPending', 'retained_cleanup_pending')):
            error_type = getattr(worker, name, None)
            if isinstance(error_type, type) and isinstance(error, error_type):
                return code
        for error_type, code in ((TimeoutError, 'timeout'), (ConnectionError, 'connection'),
                                 (OSError, 'transport_error'), (KeyError, 'invalid_response'),
                                 (TypeError, 'invalid_response'), (ValueError, 'invalid_response'),
                                 (AssertionError, 'assertion_failed'), (RuntimeError, 'runtime_error')):
            if isinstance(error, error_type):
                return code
        return 'unexpected_error'

    def read_state():
        observed = observe()
        if (not isinstance(observed, (tuple, list)) or len(observed) != 2
                or any(not isinstance(value, str) for value in observed)):
            raise ValueError
        return tuple(observed)

    def fail(reason, claims, *, stage='claim', code=None, observation_code=None):
        run_status = state[0] if state[0] in {'queued', 'running', 'completed', 'failed', 'cancelled'} else 'unobserved'
        job_status = state[1] if state[1] in {'queued', 'leased', 'running', 'succeeded', 'failed', 'dead'} else 'unobserved'
        try:
            workspace_diagnostics = safe_workspace_diagnostics(diagnostics())
        except Exception:
            workspace_diagnostics = []
        detail = {'reason': reason, 'claims': claims, 'stage': stage, 'errorCode': code,
            'observationErrorCode': observation_code, 'runStatus': run_status, 'jobStatus': job_status,
            'workspaceDiagnostics': workspace_diagnostics}
        if isinstance(job_id, str) and re.fullmatch(r'agent_run\.lifecycle:agent_run_[0-9a-f]{32}', job_id):
            detail['jobId'] = job_id
        raise AssertionError(json.dumps(detail)) from None

    for index in range(8):
        if clock() - started >= 30:
            fail('e2b_product_lifecycle_deadline', index)
        stage = 'claim'
        failure_code = None
        mismatch = False
        try:
            job = claim()
            mismatch = job['jobId'] != job_id
            if not mismatch:
                owner = job['leaseOwner']
                stage = 'start'
                worker.start_job(job_id, owner)
                stage = 'execute'
                worker.execute_agent_run_lifecycle_job(job, owner, lambda: None)
                stage = 'observe'
                state = read_state()
        except Exception as error:
            failure_code = error_code(error)
        if mismatch or failure_code is not None:
            observation_code = failure_code if stage == 'observe' else None
            if stage != 'observe':
                try:
                    state = read_state()
                except Exception as error:
                    observation_code = error_code(error)
            # Raise outside exception handlers so raw causes and context cannot
            # escape through a launcher traceback or another error renderer.
            fail('e2b_product_lifecycle_job_mismatch' if mismatch else 'e2b_product_lifecycle_advance_unavailable',
                 index + 1, stage=stage, code='job_mismatch' if mismatch else failure_code,
                 observation_code=observation_code)
        if state == ('completed', 'succeeded'):
            return
        if state[0] in {'completed', 'failed', 'cancelled'} or state[1] != 'queued':
            fail('e2b_product_lifecycle_incomplete', index + 1, stage='observe')
    fail('e2b_product_lifecycle_claim_limit', 8)


class E2BProductLauncherContractTests(SimpleTestCase):
    def test_fixed_trace_model_requests_use_real_bash_and_bounded_completion(self):
        for case in ('unchanged', 'reverted', 'smallChange', 'manyFiles'):
            for run_index in (0, 1):
                for ordinal, (name, arguments) in enumerate(trace_steps(case, run_index)):
                    response = fixed_trace_model_response(case, run_index, ordinal)
                    self.assertEqual(response['toolCalls'], [{'id': f'e2b-{run_index}-{ordinal}',
                        'name': name, 'argsJson': json.dumps(arguments)}])
                    self.assertEqual(response['usage'], zero_usage())
                final = fixed_trace_model_response(case, run_index, 2)
                self.assertEqual(final['toolCalls'], [])
                self.assertEqual(final['usage'], zero_usage())
                for ordinal in (-1, 3, True, None):
                    with self.subTest(case=case, run=run_index, ordinal=ordinal), self.assertRaises(AssertionError):
                        fixed_trace_model_response(case, run_index, ordinal)


    def test_lifecycle_driver_reclaims_waiting_job_until_terminal(self):
        job_id = 'agent_run.lifecycle:owned-run'
        jobs = [dict(jobId=job_id, leaseOwner=owner) for owner in ('owner-one', 'owner-two')]
        claim = Mock(side_effect=jobs)
        observe = Mock(side_effect=[('running', 'queued'), ('completed', 'succeeded')])
        worker = SimpleNamespace(start_job=Mock(), execute_agent_run_lifecycle_job=Mock())

        drive_lifecycle_job(worker, job_id, claim, observe)

        self.assertEqual(claim.call_count, 2)
        self.assertEqual([call.args for call in worker.start_job.call_args_list],
                         [(job_id, 'owner-one'), (job_id, 'owner-two')])
        self.assertEqual([call.args[:2] for call in worker.execute_agent_run_lifecycle_job.call_args_list],
                         [(jobs[0], 'owner-one'), (jobs[1], 'owner-two')])

    def test_lifecycle_driver_stops_after_bounded_waiting_claims(self):
        job_id = 'agent_run.lifecycle:owned-run'
        claim = Mock(return_value={'jobId': job_id, 'leaseOwner': 'owner'})
        worker = SimpleNamespace(start_job=Mock(), execute_agent_run_lifecycle_job=Mock())
        with self.assertRaisesRegex(AssertionError, 'e2b_product_lifecycle_claim_limit'):
            drive_lifecycle_job(worker, job_id, claim, lambda: ('running', 'queued'))
        self.assertEqual(claim.call_count, 8)
        self.assertEqual(worker.execute_agent_run_lifecycle_job.call_count, 8)

    def test_lifecycle_driver_checks_elapsed_deadline_between_claims(self):
        job_id = 'agent_run.lifecycle:owned-run'
        claim = Mock(return_value={'jobId': job_id, 'leaseOwner': 'owner'})
        worker = SimpleNamespace(start_job=Mock(), execute_agent_run_lifecycle_job=Mock())
        with self.assertRaisesRegex(AssertionError, 'e2b_product_lifecycle_deadline'):
            drive_lifecycle_job(worker, job_id, claim, lambda: ('running', 'queued'),
                                clock=Mock(side_effect=[0, 0, 31]))
        self.assertEqual(claim.call_count, 1)

    def test_lifecycle_driver_rejects_terminal_run_without_completed_job(self):
        job_id = 'agent_run.lifecycle:owned-run'
        claim = Mock(return_value={'jobId': job_id, 'leaseOwner': 'owner'})
        worker = SimpleNamespace(start_job=Mock(), execute_agent_run_lifecycle_job=Mock())
        for state in (('completed', 'queued'), ('failed', 'succeeded'), ('cancelled', 'succeeded')):
            with self.subTest(state=state), self.assertRaisesRegex(AssertionError, 'e2b_product_lifecycle_incomplete'):
                drive_lifecycle_job(worker, job_id, claim, lambda: state)
        self.assertEqual(claim.call_count, 3)

    def test_lifecycle_failure_only_includes_known_static_workspace_diagnostics(self):
        job_id = 'agent_run.lifecycle:owned-run'
        worker = SimpleNamespace(start_job=Mock(), execute_agent_run_lifecycle_job=Mock())
        safe = 'e2b_workspace_protection: stage=quiesce; code=e2b_adapter_result_unknown'
        resolve = 'e2b_workspace_protection: stage=resolve; code=api_unavailable'
        busy = 'e2b_workspace_protection: stage=exclusive; code=activity_busy'
        secret = 'secret-fixture-value'
        lines = [secret, safe + '\n', resolve, busy, safe + '; authorization=' + secret,
                 'e2b_workspace_protection: stage=capture; code=api_unavailable',
                 'e2b_workspace_protection: stage=quiesce; code=activity_busy',
                 'e2b_workspace_protection: stage=capture; code=secret_fixture_value']
        with self.assertRaises(AssertionError) as failed:
            drive_lifecycle_job(worker, job_id,
                lambda: {'jobId': job_id, 'leaseOwner': 'owner'}, lambda: ('completed', 'queued'),
                diagnostics=lambda: lines)
        detail = json.loads(str(failed.exception))
        self.assertEqual(detail['workspaceDiagnostics'], [safe, resolve, busy])
        self.assertNotIn(secret, str(failed.exception))
        self.assertNotIn('secret_fixture_value', str(failed.exception))

    def test_prebuilt_argv_is_explicit_and_preserves_boundaries(self):
        argv = ['/out/runtime_server-owned', RUNTIME_TEST, '--ignored', '--exact', '--nocapture']
        self.assertEqual(runtime_argv(json.dumps(argv)), argv)
        for value in (None, '{}', '[]', json.dumps(['cargo', 'test']), json.dumps(argv + ['bad\x00arg'])):
            with self.subTest(value=value), self.assertRaises(ValueError):
                runtime_argv(value)

    def test_provider_selection_cannot_silently_use_cloud_or_empty_physical_host(self):
        environment = {'E2B_API_URL': 'http://127.0.0.1:18080', 'E2B_SANDBOX_URL': 'http://localhost:18081',
                       'RUNTIME_EXECUTION_PROVIDER': 'e2b'}
        require_local_provider(environment)
        for change in ({'E2B_API_URL': 'https://api.e2b.dev'}, {'E2B_SANDBOX_URL': ''},
                       {'RUNTIME_EXECUTION_PROVIDER': 'docker'}, {'CENTAERIS_NATIVE_CONSUME_PRODUCTION_FIXTURE': '{}'}):
            with self.subTest(fields=sorted(change)), self.assertRaises(ValueError):
                require_local_provider(environment | change)

    def test_database_owner_is_explicit_and_matches_the_shared_comment(self):
        owner = 'owned-fixture-token'
        comment = 'centaeris:e2b-product:' + owner
        require_owned_database('test_centaeris_owned', comment, owner)
        for name, value, token in (('centaeris', comment, owner), ('test_owned', None, owner),
                                   ('test_owned', comment, ''), ('test_owned', comment, 'other-owner'),
                                   ('test_owned', comment, 'invalid owner')):
            with self.subTest(database=name), self.assertRaises(ValueError):
                require_owned_database(name, value, token)


if os.environ.get('CENTAERIS_E2B_PRODUCT_ACCEPTANCE') == '1':
    class E2BConsumptionProductionTests(LiveServerTestCase):
        serialized_rollback = False
        def load_worker(self, runtime_url):
            worker = native_fixture.NativeConsumptionProductionTests.load_worker(self, runtime_url)
            return observe_lifecycle_worker(worker, getattr(self, 'lifecycle_calls', None))
        def claim_due_job(self, worker, job_id, job_kind, worker_id):
            return native_fixture.NativeConsumptionProductionTests.claim_due_job(self,
                worker, job_id, job_kind, worker_id, observations=getattr(self, 'lifecycle_calls', None))

        def setUp(self):
            require_local_provider(os.environ)
            self.argv = runtime_argv(os.environ.get('CENTAERIS_E2B_RUNTIME_TEST_ARGV'))
            self._owner_verified = False
            with connection.cursor() as cursor:
                cursor.execute("SELECT current_database(),shobj_description(oid,'pg_database') FROM pg_database WHERE datname=current_database()")
                name, comment = cursor.fetchone()
                require_owned_database(name, comment, os.environ.get('CENTAERIS_E2B_PRODUCT_DB_OWNER'))
                self.assertEqual(name, connection.settings_dict['NAME'])
                cursor.execute("SELECT EXISTS(SELECT 1 FROM pg_namespace WHERE nspname='runtime')")
                self.assertFalse(cursor.fetchone()[0], 'real E2B acceptance requires a fresh runtime namespace')
            self._owner_verified = True
            self.user = get_user_model().objects.create_user(username='e2b-product-owner')
            self.workspace = Workspace.objects.create(name='E2B product acceptance', createdBy=self.user)
            WorkspaceMembership.objects.create(workspace=self.workspace, user=self.user, role='owner')
            self.model = ModelConfig.objects.create(displayName='Scripted E2B acceptance')
            self.agent = Agent.objects.create(workspace=self.workspace, owner=self.user, name='E2B product agent',
                instructions='Execute the requested proof steps exactly once.', model_config=self.model)
            self.client.force_login(self.user)
            self.requests, self.steps, self.runs = [], [], []
            self.workspace_observations = workspace_observation_collector(os.environ)
            self.lifecycle_calls = LifecycleCallCollector() if os.environ.get('WORKSPACE_PERF_OBSERVATIONS') == '1' else None
            if self.lifecycle_calls is not None:
                self.lifecycle_calls.marker('sampleStart')
                self.addCleanup(emit_lifecycle_call_report, self.lifecycle_calls)
            self.lifecycle_passed = False
            self.runtime_output_drained = threading.Event()
            self.runtime_lines = deque(maxlen=128)
            self.runtime_markers = queue.Queue()
            self.runtime_process = None
            self.runtime_url = self.start_runtime()
            with patch.dict(os.environ, {'AGENT_RUN_EXECUTION_MODE': 'perRun'}):
                self.worker = self.load_worker(self.runtime_url)

        def _fixture_teardown(self):
            # Django's ordinary flush must not erase the only business grants
            # backing an unresolved provider operation. The external owner also
            # runs with keepdb and checks this ledger before dropping the DB.
            safe = False
            try:
                if getattr(self, '_owner_verified', False):
                    with connection.cursor() as cursor:
                        cursor.execute("SELECT current_database(),shobj_description(oid,'pg_database') FROM pg_database WHERE datname=current_database()")
                        name, comment = cursor.fetchone()
                        require_owned_database(name, comment, os.environ.get('CENTAERIS_E2B_PRODUCT_DB_OWNER'))
                        cursor.execute("SELECT to_regclass('runtime.provider_creation_operations')")
                        if cursor.fetchone()[0] is None:
                            safe = True
                        else:
                            cursor.execute('SELECT count(*) FROM runtime.provider_creation_operations WHERE settlement_id IS NULL')
                            safe = cursor.fetchone()[0] == 0
            except Exception:
                safe = False
            if not safe:
                print('e2b-product-responsibility:pending', flush=True)
                self.fail('Unresolved or unverified E2B responsibility retained; owned database must remain')
            super()._fixture_teardown()

        def start_runtime(self):
            database = connection.settings_dict
            self.assertTrue(database['NAME'].startswith('test_'), 'owned disposable database required')
            database_url = 'postgresql://{}:{}@{}:{}/{}'.format(quote(database['USER']), quote(database['PASSWORD']),
                database['HOST'], database['PORT'], quote(database['NAME']))
            environment = {**os.environ, 'CENTAERIS_E2B_PRODUCT_DATABASE_URL': database_url,
                'API_INTERNAL_URL': self.live_server_url, 'INTERNAL_API_TOKEN': settings.INTERNAL_API_TOKEN,
                'AGENT_RUN_AUTHORIZATION_SIGNING_KEY': settings.AGENT_RUN_AUTHORIZATION_SIGNING_KEY,
                'REDIS_URL': 'redis://127.0.0.1:1/15', 'RUNTIME_STREAM_TTL_SECONDS': '3600',
                'RUNTIME_LIVE_STATE_TTL_SECONDS': '3600', 'NO_PROXY': '127.0.0.1,localhost,' + os.environ.get('NO_PROXY', '')}
            self.runtime_shutdown_wait_seconds = runtime_shutdown_wait_seconds(environment)
            self.runtime_process = subprocess.Popen(self.argv, cwd=Path(__file__).resolve().parents[3], env=environment,
                stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, encoding='utf-8')
            self.addCleanup(self.stop_runtime)
            def collect():
                def on_line(line):
                    feed_runtime_line(getattr(self, "lifecycle_calls", None), line)
                    self.runtime_lines.append(line)
                    if line.startswith(READY):
                        self.runtime_markers.put(line.rstrip())
                try:
                    collect_runtime_output(self.runtime_process.stdout, on_line, self.workspace_observations)
                except (OSError, ValueError, UnicodeError):
                    # Preserve lifecycle assertions; the optional report records
                    # that this stream did not drain and cannot prove coverage.
                    pass
                else:
                    self.runtime_output_drained.set()
                finally:
                    self.runtime_markers.put('process-exited')
            self.runtime_reader = threading.Thread(target=collect, daemon=True)
            self.runtime_reader.start()
            try:
                marker = self.runtime_markers.get(timeout=30)
            except queue.Empty:
                self.fail('E2B product Runtime did not become ready within its startup deadline')
            self.assertTrue(marker.startswith(READY), 'E2B product Runtime exited before readiness')
            return marker.removeprefix(READY)

        def stop_runtime(self):
            process = self.runtime_process
            if process is None:
                return
            if process.poll() is None:
                process.stdin.write('stop\n')
                process.stdin.flush()
            try:
                process.wait(timeout=getattr(self, 'runtime_shutdown_wait_seconds',
                    runtime_shutdown_wait_seconds({})))
            finally:
                if process.poll() is None:
                    process.terminate()
                    process.wait(timeout=5)
                self.runtime_reader.join(timeout=5)
                process.stdin.close()
                process.stdout.close()
                if self.workspace_observations is not None:
                    summary = self.workspace_observations.summary(lifecycle_passed=self.lifecycle_passed,
                        stream_drained=self.runtime_output_drained.is_set() and not self.runtime_reader.is_alive())
                    summary['runtimeExitStatus'] = 'clean' if process.returncode == 0 else 'failed'
                    if getattr(self, 'fixed_trace', None) is not None:
                        summary['workload'] = trace_workload_report(self.fixed_trace)
                        roles = dict(zip(self.runs, ('A', 'B')))
                        for record in summary['runCoverage'] + summary['operations']:
                            record['role'] = roles.get(record['runId'], 'unassigned')
                    print('e2b-product-workspace-measurements:' + json.dumps(summary, sort_keys=True), flush=True)
            self.assertEqual(process.returncode, 0, 'E2B product Runtime exited unsuccessfully')

        def model_response(self, model, request):
            self.assertEqual(model.pk, self.model.pk)
            self.requests.append(deepcopy(request))
            run_id = request['agentRunId']
            ordinal = sum(item['agentRunId'] == run_id for item in self.requests) - 1
            index = self.runs.index(run_id)
            if getattr(self, 'fixed_trace', None) is not None:
                return fixed_trace_model_response(self.fixed_trace, index, ordinal)
            observe_first_successor_tool_followup(getattr(self, 'lifecycle_calls', None), index, ordinal)
            if index == 0:
                script = [
                    ('write', {'path': '/mnt/data/product-proof.txt', 'content': 'workspace-A\n'}),
                    ('bash', {'command': "set -e; printf 'bash-A\\n' >> product-proof.txt; printf 'artifact-A\\n' > delivery-A.bin; cat product-proof.txt"}),
                    ('read', {'path': '/mnt/data/product-proof.txt'}),
                    ('publish_artifact', {'path': '/mnt/data/delivery-A.bin'}),
                ]
            else:
                script = [
                    ('read', {'path': '/mnt/data/product-proof.txt'}),
                    ('bash', {'command': "test \"$(cat product-proof.txt)\" = \"$(printf 'workspace-A\\nbash-A\\n')\" && printf 'workspace-B\\n' >> product-proof.txt && cp product-proof.txt delivery-B.bin && cat product-proof.txt"}),
                    ('read', {'path': '/mnt/data/product-proof.txt'}),
                    ('publish_artifact', {'path': '/mnt/data/delivery-B.bin'}),
                ]
            self.assertLessEqual(ordinal, len(script), 'scripted model exceeded its bounded calls')
            if ordinal == len(script):
                return {'text': 'E2B product proof complete.', 'toolCalls': [], 'usage': zero_usage()}
            name, arguments = script[ordinal]
            return fake_tool_call(f'e2b-{index}-{ordinal}', name, arguments)

        def admit(self, session_id, text):
            if len(self.runs) == 1:
                observe_lifecycle_marker(self.lifecycle_calls, 'successorAdmissionStarted')
            response = self.client.post(f'/api/workspaces/{self.workspace.pk}/sessions/{session_id}/messages',
                data=json.dumps({'operationId': 'e2b-product-' + str(len(self.runs)), 'text': text,
                    'agentId': self.agent.pk, 'modelConfigRef': self.model.pk, 'attachmentRefs': []}), content_type='application/json')
            self.assertEqual(response.status_code, 202, 'E2B public message admission failed')
            body = response.json()
            self.assertEqual(body.get('status'), 'accepted')
            if len(self.runs) == 1:
                observe_lifecycle_marker(self.lifecycle_calls, 'successorAdmitted')
            self.runs.append(body['agentRunId'])
            if self.lifecycle_calls is not None:
                self.lifecycle_calls.bind_run(body['agentRunId'])
            if self.workspace_observations is not None:
                self.workspace_observations.bind(body['agentRunId'], 'agent_run.lifecycle:' + body['agentRunId'])
            return AgentRun.objects.get(pk=body['agentRunId'])

        def execute(self, run, *, tool_count=4):
            job_id = 'agent_run.lifecycle:' + run.pk
            def observe():
                run.refresh_from_db()
                return run.status, self.worker.runtime_request('/internal/jobs/' + job_id)['job']['status']
            drive_lifecycle_job(self.worker, job_id,
                lambda: self.claim_due_job(self.worker, job_id, 'agent_run.lifecycle', 'worker:e2b-product'), observe,
                diagnostics=lambda: tuple(self.runtime_lines))
            self.assertEqual(run.status, 'completed', 'real Core Run did not complete')
            self.assertEqual(self.worker.runtime_request('/internal/jobs/' + job_id)['job']['status'], 'succeeded')
            self.assertEqual(run.events.filter(payload__type='agent_run_completed').count(), 1)
            results = list(run.events.filter(payload__type='tool_result').values_list('payload', flat=True))
            self.assertEqual(len(results), tool_count)
            self.assertEqual(Counter(row['payload']['callId'] for row in results), Counter(f'e2b-{len(self.runs)-1}-{index}' for index in range(tool_count)))
            self.assertTrue(all(row['payload'].get('resultState') in ('successWithOutput', 'successNoOutput') for row in results), 'one of the real tools failed')
            self.assertTrue(AgentRunEnvironmentRelease.objects.filter(agent_run=run, authorization_digest=run.authorization.digest).exists())
            with connection.cursor() as cursor:
                cursor.execute('SELECT reported_sandbox_id,verified_sandbox_id,settlement_sandbox_id,dispatch_state,settlement_kind FROM runtime.provider_creation_operations WHERE agent_run_id=%s', [run.pk])
                rows = cursor.fetchall()
                self.assertEqual(len(rows), 1)
                sandbox_id, verified, settled, dispatched, settlement = rows[0]
                self.assertTrue(sandbox_id)
                self.assertEqual((verified, settled, dispatched, settlement), (sandbox_id, sandbox_id, 'dispatched', 'removed'))
                cursor.execute('SELECT operation_count,authorization_digest,profile_digest FROM runtime.provider_run_cleanup_receipts WHERE agent_run_id=%s', [run.pk])
                self.assertEqual(cursor.fetchall(), [(1, run.authorization.digest, run.authorization.payload['environmentProfileDigest'])])
            self.assert_provider_absent(sandbox_id)
            return sandbox_id

        def assert_provider_absent(self, sandbox_id):
            from e2b import E2B, NotFoundException
            sdk = E2B(api_key=os.environ['E2B_API_KEY'], domain=os.environ.get('E2B_DOMAIN'),
                api_url=os.environ['E2B_API_URL'], sandbox_url=os.environ['E2B_SANDBOX_URL'], retries=0, debug=False, request_timeout=5)
            try:
                sdk.Sandbox.get_info(sandbox_id)
            except NotFoundException:
                return
            except Exception:
                self.fail('Exact provider absence could not be verified')
            self.fail('Provider still reports the settled sandbox present')

        def assert_artifact(self, run, expected):
            artifact = Artifact.objects.get(agent_run=run, status='published')
            self.assertEqual(artifact.sizeBytes, len(expected))
            self.assertEqual(artifact.sha256, 'sha256:' + hashlib.sha256(expected).hexdigest())
            response = self.client.get(f'/api/artifacts/{artifact.pk}/download')
            self.assertEqual(response.status_code, 200)
            if response.streaming:
                from .tests import streaming_response_bytes
                actual = streaming_response_bytes(response)
            else:
                actual = response.content
            self.assertEqual(actual, expected)

        def test_real_e2b_core_tools_snapshot_ab_and_full_lifecycle(self):
            with override_settings(RUNTIME_URL=self.runtime_url), patch('app_core.model_adapter.fake_model_response', side_effect=self.model_response):
                first = self.admit('new', 'Create the deterministic A proof files and publish delivery A.')
                first_sandbox = self.execute(first)
                observe_lifecycle_marker(self.lifecycle_calls, 'predecessorLifecycleCompleted')
                self.assert_artifact(first, ARTIFACT_A)
                session = Session.objects.get(pk=first.session_id)
                self.assertGreater(session.workspaceGeneration, 0)
                first_hash = session.workspaceSnapshotSha256
                self.assertTrue(first_hash)
                second = self.admit(session.pk, 'Read restored A, append B, and publish delivery B.')
                second_sandbox = self.execute(second)
                self.assert_artifact(second, FILE_B)
                self.assertNotEqual(first_sandbox, second_sandbox, 'perRun must use distinct physical instances')
                session.refresh_from_db()
                self.assertGreater(session.workspaceGeneration, 1)
                self.assertNotEqual(session.workspaceSnapshotSha256, first_hash)
                self.assertEqual(len(self.requests), 10)
                self.assertEqual(first.session_id, second.session_id)
                first_tool = second.events.filter(payload__type='tool_result').order_by('agent_run_sequence').first()
                self.assertIsNotNone(first_tool)
                self.assertEqual(first_tool.payload['payload']['callId'], 'e2b-1-0')
                self.assertEqual(first_tool.payload['payload']['toolName'], 'read')
                self.assertIn(first_tool.payload['payload']['resultState'], ('successWithOutput', 'successNoOutput'))
                with connection.cursor() as cursor:
                    cursor.execute('SELECT count(*) FROM runtime.provider_creation_operations WHERE settlement_id IS NULL')
                    pending_duties = cursor.fetchone()[0]
                self.assertEqual(pending_duties, 0)
                report = per_run_acceptance_report(run_a_id=first.pk, run_b_id=second.pk,
                    sandbox_a_id=first_sandbox, sandbox_b_id=second_sandbox,
                    same_session=first.session_id == second.session_id, provider_absent=True, pending_duties=pending_duties,
                    first_tool_event_id=first_tool.eventId, first_tool_created_at_ms=first_tool.createdAtMs,
                    first_tool_payload=first_tool.payload)
            self.lifecycle_passed = True
            print('e2b-per-run-product-measurements:' + json.dumps(report, sort_keys=True), flush=True)


        def assert_trace_workspace_archive(self, session, case, *, final):
            from .workspace_snapshot import validate_snapshot_stream
            expected = expected_manifest(case, final=final)
            self.assertEqual(session.workspaceExpandedSizeBytes,
                             sum(file['sizeBytes'] for file in expected['files']))
            self.assertEqual(session.workspaceFileCount, len(expected['files']))
            candidate = {'generation': session.workspaceGeneration,
                'encoding': session.workspaceSnapshotEncoding,
                'snapshotSha256': session.workspaceSnapshotSha256,
                'snapshotSizeBytes': session.workspaceSnapshotSizeBytes,
                'expandedSizeBytes': session.workspaceExpandedSizeBytes,
                'fileCount': session.workspaceFileCount}
            # Independent of Core tool output: validate every persisted file
            # body and the stored object's exact outer digest/encoding.
            with default_storage.open(session.workspaceStorageKey, 'rb') as source:
                manifest = validate_snapshot_stream(source, candidate)
            self.assertEqual(manifest, expected)

        @skipUnless(os.environ.get('CENTAERIS_E2B_FIXED_TRACE') is not None,
                    'Select an explicit fixed E2B workspace trace case')
        def test_real_e2b_fixed_workspace_trace(self):
            self.fixed_trace = fixed_trace_case(os.environ.get('CENTAERIS_E2B_FIXED_TRACE'))
            with override_settings(RUNTIME_URL=self.runtime_url), patch(
                    'app_core.model_adapter.fake_model_response', side_effect=self.model_response):
                first = self.admit('new', 'Prepare and verify the fixed workspace corpus.')
                first_sandbox = self.execute(first, tool_count=2)
                session = Session.objects.get(pk=first.session_id)
                self.assertEqual(session.workspaceGeneration, 1)
                first_generation = session.workspaceGeneration
                first_hash = session.workspaceSnapshotSha256
                self.assertTrue(first_hash)
                self.assert_trace_workspace_archive(session, self.fixed_trace, final=False)
                second = self.admit(session.pk, 'Verify restored corpus, apply the fixed trace, and verify exact final files.')
                second_sandbox = self.execute(second, tool_count=2)
                self.assertNotEqual(first_sandbox, second_sandbox,
                                    'perRun fixed trace requires exactly two distinct physical instances')
                session.refresh_from_db()
                self.assert_trace_workspace_archive(session, self.fixed_trace, final=True)
                if self.fixed_trace in ('unchanged', 'reverted'):
                    self.assertEqual(session.workspaceGeneration, first_generation)
                    self.assertEqual(session.workspaceSnapshotSha256, first_hash)
                else:
                    self.assertEqual(session.workspaceGeneration, first_generation + 1)
                    self.assertNotEqual(session.workspaceSnapshotSha256, first_hash)
                self.assertFalse(Artifact.objects.filter(agent_run__in=[first, second]).exists())
                self.assertEqual(len(self.requests), 6)
            self.lifecycle_passed = True
