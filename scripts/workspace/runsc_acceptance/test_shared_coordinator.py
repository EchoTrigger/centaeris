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
    def invoke(self, root, *, overlap='verified', bad_run=False, bad_retirement=False, resumed=4):
        fixture = root / 'fixture'; fixture.mkdir()
        (fixture / 'manifest.json').write_text('{}')
        calls = []
        class Driver:
            def __init__(self, fixture, result, *, expected_running_limit):
                self.result = result; result.mkdir(); self.base = 'http://127.0.0.1:18083'
                self.seed = {'email': 'fixture@example.invalid', 'password': 'private-fixture-password', 'workspaceId': 'workspace-a', 'agentId': 'agent-a', 'modelConfigRef': 'model-a'}
                self.manifest = {'sharedWorkload': {'environmentCount': 4}, 'sourceRevision': 'abc'}
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
            def __init__(self, manifest, output, expected_environments): calls.append(('observer', expected_environments))
            def start(self): calls.append(('start',))
            def stop(self):
                calls.append(('stop',))
                return {'physicalOverlap': overlap, 'peakWorkingEnvironments': 4,
                        'pausedEnvironmentInstances': 4, 'resumedEnvironmentInstances': resumed}
        def workload(path, output, binary, **options):
            private = json.loads(path.read_text())
            self.assertEqual(len(private['samples']), 12)
            self.assertEqual({sample['workUnits'] for sample in private['samples']}, {2000})
            self.assertEqual(options['between_run_ms'], 6000)
            self.assertEqual(set(private['samples'][0]), {'sampleId', 'operationId', 'workUnits'})
            if os_name_is_posix(): self.assertEqual(path.stat().st_mode & 0o777, 0o600)
            output.mkdir()
            records = [{'phase': 'accepted', 'agentRunId': f'run-{e}-{i}', 'sessionId': f'session-{e}'} for e in range(4) for i in range(3)]
            (output / 'run-records.jsonl').write_text(''.join(json.dumps(record) + '\n' for record in records))
            calls.append(('k6',))
            return {'status': 'clientVerified', 'verifiedSamples': 12, 'wallTimeMs': 1000, 'betweenRunMs': 6000}
        dependencies = {'driver': Driver, 'observer': Observer, 'workload': workload}
        return calls, lambda: module.run(fixture, root / 'result', root / 'k6', dependencies=dependencies)

    def test_k6_is_only_admitter_and_retirement_follows_all_real_completion_facts(self):
        with tempfile.TemporaryDirectory() as name:
            calls, execute = self.invoke(Path(name)); report = execute()
            self.assertEqual(report['status'], 'passed')
            self.assertEqual(report['completedRuns'], 12)
            self.assertEqual(sum(call[0] == 'retire' for call in calls), 4)
            self.assertLess(calls.index(('stop',)), next(i for i, call in enumerate(calls) if call[0] == 'retire'))
            self.assertEqual(report['memoryContinuity'], 'notObserved')
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
    def test_durable_times_keep_cleanup_release_and_first_client_tool_distinct(self):
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
        class Cursor:
            def __enter__(self): return self
            def __exit__(self, *args): return False
            def execute(self, sql, parameters=None): pass
            def fetchone(self): return (1900,)
            def fetchall(self): return [('agent_run.lifecycle:run-a', json.dumps({'jobId': 'agent_run.lifecycle:run-a', 'generation': 2, 'releaseRecordedAtMs': 1050, 'claimedAtMs': 1080, 'leaseOwner': 'private-authority-not-exported'}))]
        modules = {'django.conf': types.SimpleNamespace(settings=types.SimpleNamespace(DATABASES={'default': {'NAME': 'retained_acceptance'}})),
                   'django.db': types.SimpleNamespace(transaction=types.SimpleNamespace(atomic=contextlib.nullcontext), connection=types.SimpleNamespace(cursor=Cursor)),
                   'app_core.models': types.SimpleNamespace(AgentRun=types.SimpleNamespace(objects=Query([run])),
                        AgentRunEnvironmentRelease=types.SimpleNamespace(objects=Query(at(1850))),
                        SessionEvent=types.SimpleNamespace(objects=Query(1200)))}
        def sql(code):
            output = io.StringIO()
            with patch.dict(sys.modules, modules), contextlib.redirect_stdout(output): exec(code, {})
            return json.loads(output.getvalue())
        report = module._durable_timing(types.SimpleNamespace(sql=sql), [{'agentRunId': 'run-a'}])
        row = report['rows'][0]
        self.assertEqual(row['runWallTimeMs'], 700)
        self.assertEqual(row['durableFirstBashCallAtMs'], 1200)
        self.assertEqual(row['businessReleaseAtMs'], 1850)
        self.assertEqual(row['cleanupReceiptAtMs'], 1900)
        self.assertEqual(report['clientFirstToolLatency'], 'notObserved')
        self.assertEqual(report['resumeToFirstToolLatency'], 'notObserved')
        self.assertEqual(report['releaseToClaimMs'], [30])
        self.assertEqual(report['capacityClaims'][0]['agentRunId'], 'run-a')
        self.assertNotIn('leaseOwner', report['capacityClaims'][0])


if __name__ == '__main__': unittest.main()
