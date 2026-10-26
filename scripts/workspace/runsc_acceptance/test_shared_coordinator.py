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

spec = importlib.util.spec_from_file_location('shared_coordinator', Path(__file__).with_name('shared_workload.py'))
module = importlib.util.module_from_spec(spec); spec.loader.exec_module(module)


class SharedCoordinatorTests(unittest.TestCase):
    def invoke(self, root, *, overlap='verified', bad_run=False, bad_retirement=False, resumed=None, environments=4, working=None, model_wait_ms=0, initial_stagger_ms=0, expected_working_environments=None):
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
        options = {'environments': environments}
        if model_wait_ms: options['model_wait_ms'] = model_wait_ms
        if initial_stagger_ms: options['initial_stagger_ms'] = initial_stagger_ms
        if expected_working_environments is not None: options['expected_working_environments'] = expected_working_environments
        return calls, lambda: module.run(fixture, root / 'result', root / 'k6', dependencies=dependencies, **options)

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

    def test_shared_workload_rejects_unbounded_wait_stagger_and_invalid_overlap_before_loading(self):
        for options in ({'model_wait_ms':120001},{'model_wait_ms':-1},{'initial_stagger_ms':20000,'environments':8},{'expected_working_environments':9,'environments':8},{'expected_working_environments':0}):
            with self.subTest(options=options),self.assertRaises(ValueError):
                module.run(Path('absent'),Path('absent-result'),Path('absent-k6'),**options)


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

if __name__ == '__main__': unittest.main()
