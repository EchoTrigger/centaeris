"""Sequential product gate orchestration with isolated boundary doubles."""
import importlib.util
import json
from pathlib import Path
import tempfile
import types
import unittest
from unittest.mock import patch


class RecoveryCoordinatorTests(unittest.TestCase):
    def invoke(self, root, fail=None):
        spec = importlib.util.spec_from_file_location('workspace_recovery_subject', Path(__file__).with_name('workspace_recovery.py'))
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        calls = []

        class Driver:
            def __init__(self, fixture, result, **kwargs):
                self.result = result
                result.mkdir()
                self.tag = 'a' * 32
                self.manifest = {'sourceRevision': 'b' * 40, 'executionMode': 'perRun',
                                 'sharedWorkload': {'environmentCount': 2}, 'workspaceRecovery': True}
                self.phase = 0
            def admit(self, label, prompt):
                self.phase += 1
                calls.append(('admit', self.phase))
                return {'sessionId': 'session-other' if fail == 'foreignSession' and self.phase == 2 else 'session-a',
                        'agentRunId': 'run-' + str(self.phase)}
            def completed(self, admitted):
                calls.append(('completed', self.phase))
                return {**admitted, 'environmentId': 'env-a', 'instanceId': 'instance-' + str(self.phase),
                        'runStatus': 'completed', 'coreTerminalType': 'agent_run_completed',
                        'cleanupComplete': True, 'releaseReceiptCount': 1}
            def per_run_cleanup(self, admitted):
                calls.append(('cleanup', self.phase))
                if fail == 'cleanup' and self.phase == 1:
                    raise RuntimeError('exact container remains present')
                return {**admitted, 'environmentId': 'env-a', 'instanceId': 'instance-' + str(self.phase),
                        'containerId': 'c' * 63 + str(1 if fail == 'sameContainer' else self.phase),
                        'cleanupComplete': True, 'physicalRemoved': True, 'containerAbsent': True,
                        'residentHoldingCount': 0, 'historyPreserved': True, 'cleanupReceiptAtMs': 1000}
            def per_run_inventory_absent(self):
                return True
            def settle(self, admitted):
                calls.append(('settle', self.phase))
                return {'jobId': 'agent_run.lifecycle:' + admitted['agentRunId'], 'status': 'succeeded', 'retryCount': 0}
            def head(self, session):
                generation = 1 if self.phase == 1 or fail == 'unchangedHead' else 2
                if fail == 'restartHead' and ('restart',) in calls:
                    generation = 99
                return {'baselineKnown': fail != 'unknownHead', 'generation': generation,
                        'storageKey': 'snapshot-' + str(generation), 'snapshotSha256': 'sha256:' + str(generation) * 64,
                        'snapshotSizeBytes': 100, 'expandedSizeBytes': 64, 'fileCount': 2,
                        'snapshotEncoding': 'frameV1', 'payloadSha256': 'sha256:' + str(generation) * 64}
            def restart(self):
                calls.append(('restart',))
                return {'performed': True, 'beforeRuntimeStartedAt': 'before', 'afterRuntimeStartedAt': 'after'}
            def collect(self, old, current):
                calls.append(('gc',))
                if fail == 'gc':
                    raise RuntimeError('old head not collected')
                return {'cleaned': [old['storageKey']], 'preserved': current['storageKey']}

        def verify(driver, admitted, sample, binary):
            calls.append(('k6', driver.phase))
            if fail == 'artifact' and driver.phase == 2:
                raise RuntimeError('download hash mismatch')
            return {'sessionId': admitted['sessionId'], 'agentRunId': admitted['agentRunId'],
                    'artifact': module.workload.recovery_expectation(driver.tag)}

        try:
            report = module.run(root / 'fixture', root / 'result', root / 'k6',
                                dependencies={'driver': Driver, 'verify': verify})
            return report, calls
        except RuntimeError as error:
            error.calls = calls
            raise

    def test_three_runs_keep_one_session_with_cleanup_barriers_restart_and_actual_gc(self):
        with tempfile.TemporaryDirectory() as folder:
            report, calls = self.invoke(Path(folder))
        self.assertEqual(report['status'], 'passed')
        self.assertEqual([call for call in calls if call[0] == 'admit'], [('admit', 1), ('admit', 2), ('admit', 3)])
        self.assertLess(calls.index(('cleanup', 1)), calls.index(('admit', 2)))
        self.assertLess(calls.index(('settle', 2)), calls.index(('restart',)))
        self.assertLess(calls.index(('restart',)), calls.index(('gc',)))
        self.assertLess(calls.index(('gc',)), calls.index(('admit', 3)))
        self.assertEqual(len(report['runs']), 3)

    def test_failed_boundary_stops_before_later_admission(self):
        for failure, last in [('cleanup', 1), ('unknownHead', 1), ('artifact', 2),
                              ('foreignSession', 2), ('sameContainer', 2), ('unchangedHead', 2),
                              ('restartHead', 2), ('gc', 2)]:
            with self.subTest(failure=failure), tempfile.TemporaryDirectory() as folder:
                with self.assertRaises(RuntimeError) as raised:
                    self.invoke(Path(folder), failure)
                admissions = [call[1] for call in raised.exception.calls if call[0] == 'admit']
                self.assertEqual(admissions[-1], last)


class RecoveryVerifierTests(unittest.TestCase):
    def test_k6_phase_uses_private_environment_and_requires_exact_success_record(self):
        spec = importlib.util.spec_from_file_location('recovery_verifier_subject', Path(__file__).with_name('workspace_recovery.py'))
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        sample = {'schema': 'workspace.recovery.fixture.v1', 'caseId': 'a' * 32, 'phase': 'read'}
        admitted = {'sessionId': 'session-a', 'agentRunId': 'run-a'}
        expected = module.workload.recovery_expectation(sample['caseId'])
        record = {'schema': 'workspace.perf.run.verified.v1', **admitted, 'artifact': expected}
        for change in (None, 'hash', 'binding', 'missing', 'nonzero'):
            with self.subTest(change=change), tempfile.TemporaryDirectory() as folder:
                driver = types.SimpleNamespace(result=Path(folder), base='http://127.0.0.1:18083',
                    environment={'PATH': 'local', 'INTERNAL_API_TOKEN': 'must-not-cross', 'REAL_MODEL_SOCKET': '/private'},
                    seed={'workspaceId': 'workspace-a', 'agentId': 'agent-a', 'modelConfigRef': 'model-a',
                          'email': 'synthetic@fixture.invalid', 'password': 'synthetic-private'})
                value = {**record, 'artifact': dict(expected)}
                if change == 'hash':value['artifact']['sha256'] = 'sha256:' + 'f' * 64
                if change == 'binding':value['agentRunId'] = 'foreign'
                output = '' if change == 'missing' else json.dumps({'msg': 'PERF_RECOVERY_RUN ' + json.dumps(value)})
                process = types.SimpleNamespace(returncode=1 if change == 'nonzero' else 0, stdout='', stderr=output)
                with patch('subprocess.run', return_value=process) as run:
                    if change is None:
                        self.assertEqual(module.verify_run(driver, admitted, sample, '/owned/k6'), record)
                    else:
                        with self.assertRaises(RuntimeError):module.verify_run(driver, admitted, sample, '/owned/k6')
                    environment = run.call_args.kwargs['env']
                    self.assertNotIn('INTERNAL_API_TOKEN', environment)
                    self.assertNotIn('REAL_MODEL_SOCKET', environment)
                    self.assertEqual(environment['PERF_ADMIN_PASSWORD'], 'synthetic-private')


if __name__ == '__main__':
    unittest.main()
