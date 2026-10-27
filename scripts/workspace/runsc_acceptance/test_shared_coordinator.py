import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import types
import sys
import contextlib
import datetime
import io
import subprocess
import time

spec = importlib.util.spec_from_file_location('shared_coordinator', Path(__file__).with_name('shared_workload.py'))
module = importlib.util.module_from_spec(spec); spec.loader.exec_module(module)


class SharedCoordinatorTests(unittest.TestCase):
    def invoke(self, root, *, overlap='verified', bad_run=False, bad_retirement=False, resumed=None, environments=4, working=None, model_wait_ms=0, initial_stagger_ms=0, expected_working_environments=None, run_terminal_timeout_seconds=120, runtime_logs=None, runtime_log_error=None):
        resumed = environments if resumed is None else resumed
        working = environments if working is None else working
        fixture = root / 'fixture'; fixture.mkdir()
        (fixture / 'manifest.json').write_text('{}')
        calls = []
        class Driver:
            def __init__(self, fixture, result, *, expected_running_limit):
                self.result = result; result.mkdir(); self.base = 'http://127.0.0.1:18083'
                self.seed = {'email': 'fixture@example.invalid', 'password': 'private-fixture-password', 'workspaceId': 'workspace-a', 'agentId': 'agent-a', 'modelConfigRef': 'model-a'}
                self.manifest = {'sharedWorkload': {'environmentCount': environments, 'modelWaitMs': model_wait_ms}, 'sourceRevision': 'abc'}
                self.docker = ['docker', '--host', 'unix:///owned/docker.sock']
                self.compose = fixture / 'compose.json'
                self.environment = {'COMPOSE_DISABLE_ENV_FILE': 'true'}
                calls.append(('driver', expected_running_limit))
            def completed(self, record):
                calls.append(('completed', record['agentRunId']))
                env = record['sessionId'].split('-')[-1]
                return {**record, 'environmentId': 'environment-' + env, 'instanceId': 'instance-' + env,
                        'runStatus': 'failed' if bad_run else 'completed', 'coreTerminalType': 'agent_run_completed', 'cleanupComplete': True, 'releaseReceiptCount': 1}
            def observe(self, label, record):
                env = record['sessionId'].split('-')[-1]
                return {'environmentId': 'environment-' + env, 'instanceId': 'instance-' + env, 'containerId': 'container-' + env,
                        'hostId': 'host', 'controllerEpoch': 3, 'runtime': 'runsc', 'paused': True, 'running': True, 'powerState': 'paused'}
            def wait(self, observe, predicate, description):
                value = observe(); assert predicate(value); return value
            def retire(self, row):
                calls.append(('retire', row['environmentId']))
                return {'environmentId': row['environmentId'], 'status': 'destroyed', 'containerAbsent': not bad_retirement, 'residentHoldingCount': 0, 'historyPreserved': True}
        class Observer:
            def __init__(self, manifest, output, expected_environments, *, expected_working_environments=None): calls.append(('observer', expected_environments, expected_working_environments))
            def start(self): calls.append(('start',))
            def stop(self):
                calls.append(('stop',))
                return {'physicalOverlap': overlap, 'peakWorkingEnvironments': working,
                        'pausedEnvironmentInstances': environments, 'resumedEnvironmentInstances': resumed}
        def workload(path, output, binary, **options):
            private = json.loads(path.read_text())
            self.assertEqual(len(private['samples']), environments * 3)
            self.assertEqual({sample['workUnits'] for sample in private['samples']}, {2000})
            self.assertEqual(options['between_run_ms'], 6000)
            self.assertEqual(options['timeout'],module.workload_budget(environments,3,run_terminal_timeout_seconds,6000,initial_stagger_ms)['k6ProcessSeconds'])
            self.assertEqual(options['run_terminal_timeout_seconds'],run_terminal_timeout_seconds)
            if initial_stagger_ms: self.assertEqual(options['initial_stagger_ms'], initial_stagger_ms)
            self.assertEqual(set(private['samples'][0]), {'sampleId', 'operationId', 'workUnits'})
            if os_name_is_posix(): self.assertEqual(path.stat().st_mode & 0o777, 0o600)
            output.mkdir()
            records = [{'phase': 'accepted', 'agentRunId': f'run-{e}-{i}', 'sessionId': f'session-{e}'} for e in range(environments) for i in range(3)]
            (output / 'run-records.jsonl').write_text(''.join(json.dumps(record) + '\n' for record in records))
            calls.append(('k6',))
            return {'status': 'clientVerified', 'verifiedSamples': environments * 3, 'wallTimeMs': 1000, 'betweenRunMs': 6000,
                    'firstSuccessfulToolResultLatency': {'source': 'k6SseReceive', 'valuesMs': [125] * (environments * 3)}}
        dependencies = {'driver': Driver, 'observer': Observer, 'workload': workload}
        options = {'environments': environments,'run_terminal_timeout_seconds':run_terminal_timeout_seconds}
        if model_wait_ms: options['model_wait_ms'] = model_wait_ms
        if initial_stagger_ms: options['initial_stagger_ms'] = initial_stagger_ms
        if expected_working_environments is not None: options['expected_working_environments'] = expected_working_environments
        def runtime_process(argv, **kwargs):
            from unittest.mock import Mock
            calls.append(('runtimeLogs', tuple(argv)))
            if runtime_log_error is not None:
                raise runtime_log_error
            return types.SimpleNamespace(stdout=io.BytesIO((runtime_logs or '').encode()),
                poll=Mock(return_value=0), wait=Mock(return_value=0), terminate=Mock(), kill=Mock())
        def execute():
            with patch.object(module.subprocess, 'Popen', side_effect=runtime_process):
                return module.run(fixture, root / 'result', root / 'k6', dependencies=dependencies, **options)
        return calls, execute

    def test_k6_is_only_admitter_and_retirement_follows_all_real_completion_facts(self):
        with tempfile.TemporaryDirectory() as name:
            calls, execute = self.invoke(Path(name)); report = execute()
            self.assertEqual(report['status'], 'passed')
            self.assertEqual(report['completedRuns'], 12)
            self.assertEqual(sum(call[0] == 'retire' for call in calls), 4)
            self.assertLess(calls.index(('stop',)), next(i for i, call in enumerate(calls) if call[0] == 'retire'))
            self.assertEqual(report['memoryContinuity'], 'notObserved')
            self.assertEqual(report['clientFirstToolLatency'], {'source': 'k6SseReceive', 'valuesMs': [125] * 12})
    def test_unobserved_overlap_preserves_responsibility_and_stops_observer(self):
        with tempfile.TemporaryDirectory() as name:
            calls, execute = self.invoke(Path(name), overlap='notObserved')
            with self.assertRaises(RuntimeError): execute()
            self.assertIn(('stop',), calls); self.assertFalse(any(call[0] == 'retire' for call in calls))
            self.assertEqual(json.loads((Path(name) / 'result/shared-report.json').read_text())['status'], 'failedPreserved')
    def test_failed_run_never_retires_and_still_stops_observer(self):
        with tempfile.TemporaryDirectory() as name:
            calls, execute = self.invoke(Path(name), bad_run=True)
            with self.assertRaises((ValueError, RuntimeError)): execute()
            self.assertIn(('stop',), calls); self.assertFalse(any(call[0] == 'retire' for call in calls))
    def test_destroyed_label_without_positive_absence_is_rejected(self):
        with tempfile.TemporaryDirectory() as name:
            calls, execute = self.invoke(Path(name), bad_retirement=True)
            with self.assertRaises(RuntimeError): execute()
    def test_shared_gate_requires_each_environment_to_pause_and_resume(self):
        with tempfile.TemporaryDirectory() as name:
            calls, execute = self.invoke(Path(name), resumed=3)
            with self.assertRaises(RuntimeError): execute()
            self.assertFalse(any(call[0] == 'retire' for call in calls))


    def test_sparse_eight_reports_two_working_threshold_but_verifies_all_eight_lifecycles(self):
        with tempfile.TemporaryDirectory() as name:
            calls,execute=self.invoke(Path(name),environments=8,working=2,model_wait_ms=400,initial_stagger_ms=125,expected_working_environments=2)
            report=execute()
            self.assertEqual(report['status'],'passed')
            self.assertEqual(report['environments'],8)
            self.assertEqual(report['expectedWorkingEnvironments'],2)
            self.assertEqual(report['native']['peakWorkingEnvironments'],2)
            self.assertEqual(report['modelWaitMs'],400)
            self.assertEqual(report['initialStaggerMs'],125)
            self.assertEqual(report['completedRuns'],24)
            self.assertEqual(report['sameInstanceEnvironments'],8)
            self.assertEqual(report['cleanupCompleteRuns'],24)
            self.assertEqual(sum(call[0]=='retire' for call in calls),8)
            self.assertIn(('observer',8,2),calls)

    def test_sparse_threshold_does_not_reduce_pause_resume_or_durable_requirements(self):
        for options in ({'resumed':7},{'bad_run':True},{'working':1}):
            with self.subTest(options=options),tempfile.TemporaryDirectory() as name:
                calls,execute=self.invoke(Path(name),environments=8,expected_working_environments=2,**options)
                with self.assertRaises((ValueError,RuntimeError)):execute()
                self.assertFalse(any(call[0]=='retire' for call in calls))

    def test_failed_physical_gate_still_collects_only_safe_runtime_timings(self):
        event = {'schema': 'workspace.perf.v1', 'source': 'runtime', 'jobId': 'agent_run.lifecycle:run-0-0',
            'phase': 'physicalConfirm', 'elapsedMs': 125.5, 'outcome': 'error', 'atUnixMs': 1000,
            'error': 'private runtime response', 'body': {'token': 'private token'}}
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            calls, execute = self.invoke(root, overlap='notObserved', runtime_logs=json.dumps(event))
            with self.assertRaisesRegex(RuntimeError, 'overlap threshold'):
                execute()
            report = json.loads((root / 'result/shared-report.json').read_text())
            self.assertEqual(report['status'], 'failedPreserved')
            self.assertEqual(report['preparationTiming']['records'], [
                {key: event[key] for key in ('jobId', 'phase', 'elapsedMs', 'outcome', 'atUnixMs')}])
            self.assertEqual(report['native']['physicalOverlap'], 'notObserved')
            self.assertFalse(any(call[0] == 'retire' for call in calls))
            self.assertEqual(len([call for call in calls if call[0] == 'runtimeLogs']), 1)
            self.assertNotIn('private runtime response', json.dumps(report))
            self.assertNotIn('private token', json.dumps(report))

    def test_timing_capture_failure_does_not_change_successful_physical_gate(self):
        with tempfile.TemporaryDirectory() as name:
            calls, execute = self.invoke(Path(name), runtime_log_error=RuntimeError('private command output'))
            report = execute()
            self.assertEqual(report['status'], 'passed')
            self.assertEqual(report['preparationTiming'], 'notObserved')
            self.assertEqual(sum(call[0] == 'retire' for call in calls), 4)
            self.assertNotIn('private command output', json.dumps(report))

    def test_shared_workload_rejects_unbounded_wait_stagger_and_invalid_overlap_before_loading(self):
        for options in ({'model_wait_ms':120001},{'model_wait_ms':-1},{'initial_stagger_ms':20000,'environments':8},{'expected_working_environments':9,'environments':8},{'expected_working_environments':0}):
            with self.subTest(options=options),self.assertRaises(ValueError):
                module.run(Path('absent'),Path('absent-result'),Path('absent-k6'),**options)

    def test_excessive_total_budget_rejects_before_loading_any_fixture_dependency(self):
        from unittest.mock import patch
        with patch.object(module,'_dependencies',side_effect=AssertionError('fixture must not load')):
            with self.assertRaisesRegex(ValueError,'four-hour'):
                module.run(Path('absent'),Path('absent-result'),Path('absent-k6'),environments=64,iterations=20,
                    run_terminal_timeout_seconds=600,between_run_ms=60000)

    def test_long_stream_deadline_is_explicit_without_reducing_three_rounds_or_four_way_overlap(self):
        with tempfile.TemporaryDirectory() as directory:
            calls,execute=self.invoke(Path(directory),run_terminal_timeout_seconds=300)
            report=execute()
            self.assertEqual(report['runTerminalTimeoutSeconds'],300)
            self.assertEqual(report['budgets'],module.workload_budget(4,3,300,6000,0))
            self.assertEqual(report['iterations'],3)
            self.assertEqual(report['expectedWorkingEnvironments'],4)
            self.assertEqual(report['completedRuns'],12)


def os_name_is_posix():
    import os
    return os.name == 'posix'


class FixtureRunningLimitTests(unittest.TestCase):
    def driver(self, root, *, limit=4, expected=4, shared=True):
        source = Path(__file__).resolve().parents[3]
        spec = importlib.util.spec_from_file_location('shared_limit_driver', source / 'scripts/workspace/runsc_rotation_acceptance.py')
        rotation = importlib.util.module_from_spec(spec); spec.loader.exec_module(rotation)
        fixture = root / 'fixture'; fixture.mkdir()
        manifest = {'project': 'retainedaccept' + 'a' * 10, 'isolationOwner': str(root / 'owner.json'),
                    'source': str(source), 'sourceRevision': 'abc', 'binaryDirectory': str(root), 'binarySha256': {}, 'apiPort': 18083,
                    'sharedWorkload': {'environmentCount': 4} if shared else None}
        (fixture / 'manifest.json').write_text(json.dumps(manifest))
        config = {'name': manifest['project'], 'services': {'runtime': {'environment': {'OCI_RUNTIME': 'runsc',
                  'RUNTIME_RUNSC_MAX_RUNNING_ENVIRONMENTS': str(limit), 'RUNTIME_RUNSC_IDLE_PAUSE_MS': '3000',
                  'RUNTIME_NATIVE_SHARED_ADMISSION': '1' if shared else '0'}, 'volumes': ['/owned/docker.sock:/var/run/docker.sock']}}}
        (fixture / 'compose.json').write_text(json.dumps(config)); (fixture / 'synthetic.env').write_text('')
        (fixture / 'seed.json').write_text(json.dumps({'email': 'test@fixture.invalid', 'password': 'synthetic'}))
        def command(self, arguments, **options): return 'abc' if arguments[-1] == 'HEAD' else ''
        modules = {'runsc_acceptance.isolation': types.SimpleNamespace(verify_owner=lambda path: {})}
        with patch.dict(sys.modules, modules), patch.object(rotation.FixtureDriver, 'command', command), patch.object(rotation.FixtureDriver, 'http', lambda *args: {'csrfToken': 'synthetic'}):
            return rotation.FixtureDriver(fixture, root / 'result', expected_running_limit=expected)

    def test_shared_driver_checks_explicit_four_slot_admission(self):
        with tempfile.TemporaryDirectory() as name:
            self.assertEqual(self.driver(Path(name)).expected_running_limit, 4)
    def test_original_default_does_not_silently_accept_four_slots(self):
        with tempfile.TemporaryDirectory() as name:
            with self.assertRaises(RuntimeError): self.driver(Path(name), expected=1)
    def test_four_slot_mode_rejects_missing_shared_admission(self):
        with tempfile.TemporaryDirectory() as name:
            with self.assertRaises(RuntimeError): self.driver(Path(name), shared=False)
    def test_original_single_slot_fixture_still_initializes(self):
        with tempfile.TemporaryDirectory() as name:
            self.assertEqual(self.driver(Path(name), limit=1, expected=1, shared=False).expected_running_limit, 1)


class DurableTimingTests(unittest.TestCase):
    def report(self, events=None, *, availability=None, claim=None):
        utc = datetime.timezone.utc
        at = lambda millis: datetime.datetime.fromtimestamp(millis / 1000, utc)
        run = types.SimpleNamespace(id='run-a', createdAt=at(1000), startedAt=at(1100), completedAt=at(1800))
        class Query:
            def __init__(self, value): self.value = value
            def filter(self, **values): return self
            def order_by(self, *values): return self
            def values_list(self, *values, **options): return self
            def first(self): return self.value
            def __iter__(self): return iter(self.value)
        def field(row, name):
            value=row
            for key in name.split('__'):value=value.get(key) if isinstance(value,dict) else getattr(value,key,None)
            return value
        class Events:
            def __init__(self, rows):self.rows=rows
            def filter(self, **values):
                def matches(row):
                    for key,value in values.items():
                        if key.endswith('__in'):
                            if field(row,key[:-4]) not in value:return False
                        elif field(row,key)!=value:return False
                    return True
                return Events([row for row in self.rows if matches(row)])
            def order_by(self, *values):return Events(sorted(self.rows,key=lambda row:tuple(field(row,key) for key in values)))
            def values_list(self, *values, flat=False):
                rows=[tuple(field(row,key) for key in values) for row in self.rows]
                return Values([row[0] for row in rows] if flat else rows)
        class Values(Query):
            def first(self):return self.value[0] if self.value else None
        if events is None:events=[(1,1150,'agent_run_execution_started',{}),(2,1200,'tool_call',{'callId':'call-a','toolName':'bash'}),(3,1400,'tool_result',{'callId':'call-a','toolName':'bash','resultState':'successWithOutput','modelContent':'private-command-content'})]
        rows=[{'agent_run':run,'session_level':False,'agent_run_sequence':seq,'createdAtMs':at_ms,'payload':{'type':kind,'payload':payload}} for seq,at_ms,kind,payload in events]
        claim = claim if claim is not None else {'schema':'runtime.resident_capacity.claimed.v1','jobId':'agent_run.lifecycle:run-a','generation':2,'releaseRecordedAtMs':1050,'claimedAtMs':1080,'leaseOwner':'private-authority-not-exported'}
        availability = availability or []
        class Cursor:
            def __enter__(self): return self
            def __exit__(self, *args): return False
            def execute(self, sql, parameters=None):
                if parameters is None:self.kind='setup'
                elif parameters == ['run-a']:self.kind='cleanup'
                elif len(parameters)>1 and parameters[1]=='runtime.resident_capacity.claimed.v1':self.kind='claims'
                elif parameters[0]=='runtime.host_preparation.available.v1':self.kind='availability'
                else:raise AssertionError('unexpected timing query')
            def fetchone(self):
                assert self.kind=='cleanup';return (1900,)
            def fetchall(self):
                if self.kind=='claims':return [('agent_run.lifecycle:run-a', json.dumps(claim))]
                if self.kind=='availability':return availability
                raise AssertionError('unexpected timing result query')
        modules = {'django.conf': types.SimpleNamespace(settings=types.SimpleNamespace(DATABASES={'default': {'NAME': 'retained_acceptance'}})),
                   'django.db': types.SimpleNamespace(transaction=types.SimpleNamespace(atomic=contextlib.nullcontext), connection=types.SimpleNamespace(cursor=Cursor)),
                   'app_core.models': types.SimpleNamespace(AgentRun=types.SimpleNamespace(objects=Query([run])),
                        AgentRunEnvironmentRelease=types.SimpleNamespace(objects=Query(at(1850))),
                        SessionEvent=types.SimpleNamespace(objects=Events(rows)))}
        def sql(code):
            output = io.StringIO()
            with patch.dict(sys.modules, modules), contextlib.redirect_stdout(output): exec(code, {})
            return json.loads(output.getvalue())
        return module._durable_timing(types.SimpleNamespace(sql=sql), [{'agentRunId': 'run-a'}])

    def test_durable_times_keep_cleanup_release_and_first_client_tool_distinct(self):
        report=self.report();row=report['rows'][0]
        self.assertEqual(row['runWallTimeMs'], 700)
        self.assertEqual(row['durableFirstBashCallAtMs'], 1200)
        self.assertEqual(row['businessReleaseAtMs'], 1850)
        self.assertEqual(row['cleanupReceiptAtMs'], 1900)
        self.assertEqual(report['clientFirstToolLatency'], 'notObserved')
        self.assertEqual(report['resumeToFirstToolLatency'], 'notObserved')
        self.assertEqual(report['releaseToClaimMs'], [30])
        self.assertEqual(report['capacityClaims'][0]['agentRunId'], 'run-a')
        self.assertNotIn('leaseOwner', report['capacityClaims'][0])

    def test_committed_execution_fact_to_successful_result_uses_matching_call_id(self):
        events=[(1,1150,'agent_run_execution_started',{}),(2,1200,'tool_call',{'callId':'call-a','toolName':'bash'}),
                (3,1250,'tool_result',{'callId':'unrelated','toolName':'bash','resultState':'successWithOutput'}),
                (4,1300,'tool_result',{'callId':'call-a','toolName':'bash','resultState':'error'}),
                (5,1400,'tool_result',{'callId':'call-a','toolName':'bash','resultState':'successNoOutput','modelContent':'private-command-content'})]
        report=self.report(events);row=report['rows'][0]
        self.assertEqual(row['durableExecutionFactCreatedAtMs'],1150)
        self.assertEqual(row['durableFirstSuccessfulToolResultCreatedAtMs'],1400)
        self.assertEqual(row['executionFactToFirstToolResultMs'],250)
        self.assertNotIn('private-command-content',json.dumps(report))
        self.assertEqual(report['resumeToFirstToolLatency'],'notObserved')

    def test_unconfirmed_event_shape_never_invents_result_time(self):
        for result in ({'toolName':'bash','resultState':'successWithOutput'},
                       {'callId':'call-a','toolName':'read','resultState':'successWithOutput'},
                       {'callId':'call-a','toolName':'bash','resultState':'error'}):
            with self.subTest(result=result):
                row=self.report([(1,1150,'agent_run_execution_started',{}),(2,1200,'tool_call',{'callId':'call-a','toolName':'bash'}),(3,1400,'tool_result',result)])['rows'][0]
                self.assertIsNone(row['durableFirstSuccessfulToolResultCreatedAtMs'])
                self.assertIsNone(row['executionFactToFirstToolResultMs'])


    def preparation_fact(self, **changed):
        fact={'schema':'runtime.host_preparation.available.v1','scope':'globalPreparation','hostId':'unlocking-host','generation':2,'availableAtMs':1050}
        fact.update(changed)
        return ('host_preparation_available:2','unlocking-host',1050,json.dumps(fact))

    def test_host_preparation_unlock_is_not_reported_as_physical_resource_release(self):
        report=self.report(availability=[self.preparation_fact()])
        self.assertEqual(report['hostPreparationToClaimMs'],[30])
        self.assertEqual(report['releaseToClaimMs'],'notObserved')
        row=report['capacityClaims'][0]
        self.assertEqual(row['wakeSource'],'hostPreparation')
        self.assertEqual(row['hostId'],'unlocking-host')
        self.assertEqual(row['hostPreparationToClaimMs'],30)
        self.assertNotIn('releaseToClaimMs',row)

    def test_physical_release_without_preparation_fact_keeps_release_timing(self):
        report=self.report()
        self.assertEqual(report['hostPreparationToClaimMs'],'notObserved')
        self.assertEqual(report['releaseToClaimMs'],[30])
        self.assertEqual(report['capacityClaims'][0]['wakeSource'],'physicalRelease')

    def test_preparation_availability_requires_exact_generation_identity_and_time(self):
        invalid=[self.preparation_fact(availableAtMs=1051),self.preparation_fact(hostId=''),
                 self.preparation_fact(generation=3),self.preparation_fact(scope='hostLocal'),
                 self.preparation_fact(schema='unsupported')]
        correct=self.preparation_fact()
        invalid.extend([('wrong-event',*correct[1:]),(correct[0],'other-host',*correct[2:]),(correct[0],correct[1],1049,correct[3])])
        for fact in invalid:
            with self.subTest(fact=fact),self.assertRaisesRegex(RuntimeError,'preparation availability'):
                self.report(availability=[fact])
        with self.assertRaisesRegex(RuntimeError,'preparation availability'):
            self.report(availability=[correct,correct])

class PreparationTimingCaptureTests(unittest.TestCase):
    def event(self, **changes):
        return {'schema': 'workspace.perf.v1', 'source': 'runtime', 'jobId': 'agent_run.lifecycle:run-a',
            'phase': 'preparationAttempt', 'elapsedMs': 27.5, 'outcome': 'ok', 'atUnixMs': 2000} | changes

    def collect(self, output, *, error=None, accepted=None, exit_code=0):
        from unittest.mock import Mock
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            records = root / 'run-records.jsonl'
            accepted = accepted if accepted is not None else [{'phase': 'accepted', 'agentRunId': 'run-a'}]
            records.write_text(''.join(json.dumps(row) + '\n' for row in accepted))
            process = types.SimpleNamespace(stdout=io.BytesIO(output.encode()), poll=Mock(return_value=exit_code),
                wait=Mock(return_value=exit_code), terminate=Mock(), kill=Mock())
            command = Mock(return_value=process, side_effect=error)
            driver = types.SimpleNamespace(docker=['docker', '--host', 'unix:///owned/docker.sock'], compose=root / 'compose.json',
                environment={'COMPOSE_DISABLE_ENV_FILE': 'true'})
            with contextlib.redirect_stdout(io.StringIO()) as stdout, contextlib.redirect_stderr(io.StringIO()) as stderr, \
                    patch.object(module.subprocess, 'Popen', command):
                result = module._preparation_timing(driver, records)
            self.assertEqual(stdout.getvalue(), '')
            self.assertEqual(stderr.getvalue(), '')
            return result, command

    def test_phase_and_identity_whitelist_projects_safe_fields_and_preserves_failed_samples(self):
        phases = ('preparationAttempt', 'environmentAcquire', 'globalAcquire', 'globalHold', 'preparationInventory',
            'preparationReconcileReserve', 'physicalConfirm', 'controllerTakeover', 'workspacePreparation')
        events = [self.event(phase=phase, outcome=outcome, elapsedMs=index + 10,
            error='private response detail', body={'token': 'private token'}, leaseOwner='private owner')
            for index, (phase, outcome) in enumerate(zip(phases, ('ok', 'error', 'interrupted') * 3))]
        rejected = [self.event(schema='old.schema'), self.event(source='worker'), self.event(jobId='agent_run.lifecycle:other'),
            self.event(phase='unknownPhase'), self.event(outcome='private outcome'), self.event(elapsedMs=-1),
            self.event(elapsedMs=True), self.event(elapsedMs='27'), self.event(elapsedMs=float('nan')),
            self.event(elapsedMs=float('inf')), self.event(atUnixMs=True), self.event(atUnixMs=-1),
            self.event(atUnixMs='2000'), {'schema': 'workspace.perf.v1'}, ['private data']]
        logs = '\n'.join(['private arbitrary log', '{invalid json', *map(json.dumps, events + rejected)])
        report, command = self.collect(logs)
        self.assertEqual(report['schema'], 'runsc.shared.preparation_timing.v1')
        self.assertEqual(report['records'], [
            {key: event[key] for key in ('jobId', 'phase', 'elapsedMs', 'outcome', 'atUnixMs')} for event in events])
        self.assertEqual({record['outcome'] for record in report['records']}, {'ok', 'error', 'interrupted'})
        self.assertNotIn('private', json.dumps(report))
        args = command.call_args.args[0]
        self.assertEqual(args[:5], ['docker', '--host', 'unix:///owned/docker.sock', 'compose', '-f'])
        self.assertEqual(args[6:], ['logs', '--no-color', '--no-log-prefix', 'runtime'])
        expected = {'stdin': subprocess.DEVNULL, 'stdout': subprocess.PIPE, 'stderr': subprocess.DEVNULL,
            'env': {'COMPOSE_DISABLE_ENV_FILE': 'true'}}
        if os_name_is_posix(): expected['start_new_session'] = True
        self.assertEqual(command.call_args.kwargs, expected)

    def test_unreached_phases_are_absent_rather_than_zero_duration_successes(self):
        event = self.event(phase='globalAcquire', elapsedMs=18, outcome='error')
        report, _ = self.collect(json.dumps(event))
        self.assertEqual(report['records'], [{key: event[key] for key in ('jobId', 'phase', 'elapsedMs', 'outcome', 'atUnixMs')}])
        self.assertFalse(any(record['phase'] == 'physicalConfirm' for record in report['records']))

    def test_absent_matching_data_and_capture_failures_remain_not_observed(self):
        for output, error in [('', None), ('private arbitrary log', None), ('{}', None),
                              (json.dumps(self.event(jobId='agent_run.lifecycle:not-accepted')), None),
                              ('private command stdout', RuntimeError('private stderr')),
                              ('private command stdout', TimeoutError('private timeout detail')),
                              ('x' * (64 * 1024 + 1), None)]:
            with self.subTest(error=type(error).__name__ if error else None, length=len(output)):
                result, _ = self.collect(output, error=error)
                self.assertEqual(result, 'notObserved')

    def test_full_stream_keeps_first_and_last_records_amid_many_unrelated_lines(self):
        first, last = self.event(), self.event(phase='workspacePreparation', outcome='error', atUnixMs=3000)
        unrelated = json.dumps(self.event(jobId='agent_run.lifecycle:unrelated', error='private body')) + '\n'
        output = unrelated * 2500 + json.dumps(first) + '\n' + unrelated * 2500 + json.dumps(last) + '\n' + unrelated * 2500
        report, _ = self.collect(output)
        self.assertEqual(report['records'], [
            {key: event[key] for key in ('jobId', 'phase', 'elapsedMs', 'outcome', 'atUnixMs')} for event in (first, last)])
        self.assertNotIn('private body', json.dumps(report))

    def test_exceeding_scan_or_record_bounds_discards_partial_timing_evidence(self):
        event = json.dumps(self.event()) + '\n'
        for setting, limit, output in [('_PREPARATION_LOG_MAX_BYTES', len(event.encode()) + 10, event + 'unrelated\n' * 10),
                                       ('_PREPARATION_LOG_MAX_RECORDS', 1, event * 2)]:
            with self.subTest(setting=setting), patch.object(module, setting, limit):
                report, _ = self.collect(output)
                self.assertEqual(report, 'notObserved')

    def test_timeout_terminates_and_reaps_the_observation_child(self):
        real_popen = subprocess.Popen
        children = []
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            descendant_pid = root / 'descendant.pid'
            descendant_ready = root / 'descendant.ready'
            # POSIX production uses a private process group: emulate a compose
            # plugin retaining stdout after its parent exits and ignoring TERM.
            script = 'import time; time.sleep(30)'
            if os_name_is_posix():
                plugin = ('import signal,time,pathlib; signal.signal(signal.SIGTERM,signal.SIG_IGN); '
                    + 'pathlib.Path(' + repr(str(descendant_ready)) + ').write_text("ready"); time.sleep(30)')
                script = ('import subprocess,sys,time,pathlib; '
                    + 'child=subprocess.Popen([sys.executable,"-B","-c",' + repr(plugin) + ']); '
                    + 'pathlib.Path(' + repr(str(descendant_pid)) + ').write_text(str(child.pid)); time.sleep(30)')
            def spawn(argv, **kwargs):
                child = real_popen([sys.executable, '-B', '-c', script], **kwargs)
                children.append(child)
                if os_name_is_posix():
                    deadline = time.monotonic() + 3
                    while not descendant_ready.exists() and time.monotonic() < deadline:
                        time.sleep(.005)
                    self.assertTrue(descendant_ready.exists(), 'plugin must ignore TERM before collector timeout starts')
                return child
            records = root / 'run-records.jsonl'
            records.write_text(json.dumps({'phase': 'accepted', 'agentRunId': 'run-a'}) + '\n')
            driver = types.SimpleNamespace(docker=['docker'], compose=root / 'compose.json', environment=None)
            with patch.object(module.subprocess, 'Popen', side_effect=spawn), \
                    patch.object(module, '_PREPARATION_LOG_TIMEOUT_SECONDS', .2):
                result = module._preparation_timing(driver, records)
            self.assertEqual(result, 'notObserved')
            self.assertEqual(len(children), 1)
            self.assertIsNotNone(children[0].poll(), 'timed-out observation child must be reaped')
            self.assertTrue(children[0].stdout.closed)
            if os_name_is_posix():
                self.assertTrue(descendant_pid.exists(), 'plugin must have started before timeout')
                state = Path('/proc') / descendant_pid.read_text() / 'stat'
                self.assertTrue(not state.exists() or state.read_text().split(') ', 1)[1].startswith('Z '),
                    'collector plugin retaining the pipe must also stop')

    def test_nonzero_log_process_exit_discards_partial_records(self):
        result, _ = self.collect(json.dumps(self.event()), exit_code=1)
        self.assertEqual(result, 'notObserved')

    def test_nonaccepted_record_does_not_authorize_log_collection(self):
        result, command = self.collect(json.dumps(self.event()), accepted=[{'phase': 'verified', 'agentRunId': 'run-a'}])
        self.assertEqual(result, 'notObserved')
        command.assert_not_called()


if __name__ == '__main__': unittest.main()
