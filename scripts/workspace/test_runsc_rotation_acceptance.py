import copy
import importlib.util
from pathlib import Path
import unittest
import json
import tempfile
from unittest.mock import Mock, patch
import subprocess
import sys

SCRIPT = Path(__file__).with_name('runsc_rotation_acceptance.py')
spec = importlib.util.spec_from_file_location('rotation_acceptance', SCRIPT)
rotation = importlib.util.module_from_spec(spec)
spec.loader.exec_module(rotation)


def oracle(label, index):
    nonce = ('a' if label == 'A' else 'b') * 64
    challenge = f'fixture-{label}-{index}'
    return {'schema': 'runsc.rotation.oracle.v1', 'nonce': nonce, 'pid': 41 if label == 'A' else 42,
            'startTicks': 100 if label == 'A' else 200, 'counter': index + 1,
            'fileSha256': ('c' if label == 'A' else 'd') * 64,
            'challenge': challenge, 'proof': rotation.challenge_proof(nonce, challenge)}


def sample(label, index):
    return {'label': label, 'agentRunId': f'run-{index}', 'sessionId': f'session-{label}',
            'environmentId': f'env-{label}', 'instanceId': f'instance-{label}',
            'containerId': ('1' if label == 'A' else '2') * 64, 'runStatus': 'completed',
            'coreTerminalType': 'agent_run_completed', 'releaseReceiptCount': 1,
            'cleanupComplete': True, 'runtime': 'runsc', 'running': True, 'paused': True, 'powerState': 'paused',
            'runningReservations': 0, 'residentCount': 1 if index == 0 else 2,
            'residentMemoryBytes': 256 if index == 0 else 512,
            'residentPids':512,'pids':{'current':67,'max':512,'eventsMax':0},
            'oracle': oracle(label, index)}


class RotationEvidenceTests(unittest.TestCase):
    def evidence(self):
        runs = [sample('A' if i % 2 == 0 else 'B', i) for i in range(12)]
        return {'schema': 'runsc.rotation.acceptance.v1', 'sourceRevision': 'a' * 40,
                'runningLimit': 1, 'runs': runs,
                'wakeup': {'waitingRunId': 'run-5', 'waitObserved': True, 'waitingPaused': True,
                           'runningReservations': 1, 'completedWithoutReadmission': True},
                'restart': {'performed': True, 'beforeRuntimeStartedAt': 'old', 'afterRuntimeStartedAt': 'new',
                            'preservedContainers': [runs[0]['containerId'], runs[1]['containerId']]},
                'cancellation': {'queuedRunStatus': 'cancelled', 'queuedSuccessfulCommands': 0,
                                 'holderRunStatus': 'cancelled', 'runningReservationsAfter': 0},
                'retirements': [{'environmentId': f'env-{label}', 'status': 'destroyed',
                                 'containerAbsent': True, 'residentHoldingCount': 0, 'historyPreserved': True}
                                for label in ('A', 'B')]}

    def test_twelve_alternating_runs_require_memory_and_exact_identity(self):
        self.assertEqual(rotation.validate_evidence(self.evidence())['completedRuns'], 12)

    def test_new_process_or_new_nonce_cannot_pass_as_resume(self):
        for field, value in [('pid', 77), ('startTicks', 999), ('nonce', 'e' * 64)]:
            data = self.evidence()
            data['runs'][4]['oracle'][field] = value
            data['runs'][4]['oracle']['proof'] = rotation.challenge_proof(
                data['runs'][4]['oracle']['nonce'], data['runs'][4]['oracle']['challenge'])
            with self.subTest(field=field), self.assertRaises(RuntimeError):
                rotation.validate_evidence(data)

    def test_file_only_or_stale_oracle_never_proves_memory(self):
        for mutation in ('missing', 'stale', 'proof'):
            data = self.evidence()
            if mutation == 'missing':
                del data['runs'][2]['oracle']
            elif mutation == 'stale':
                data['runs'][2]['oracle'] = copy.deepcopy(data['runs'][0]['oracle'])
            else:
                data['runs'][2]['oracle']['proof'] = 'f' * 64
            with self.subTest(mutation=mutation), self.assertRaises(RuntimeError):
                rotation.validate_evidence(data)

    def test_pause_must_be_physical_and_durable_without_refunding_residency(self):
        for field, value in [('paused', False), ('powerState', 'pauseDispatched'),
                             ('runningReservations', 1), ('residentCount', 1), ('residentMemoryBytes', 256)]:
            data = self.evidence()
            data['runs'][4][field] = value
            with self.subTest(field=field), self.assertRaises(RuntimeError):
                rotation.validate_evidence(data)

    def test_pid_exhaustion_or_changed_charged_limit_rejects(self):
        for field,value in [('eventsMax',1),('max',128),('current',513)]:
            data=self.evidence();data['runs'][4]['pids'][field]=value
            with self.subTest(field=field),self.assertRaises(RuntimeError):rotation.validate_evidence(data)

    def test_missing_restart_cancellation_or_retirement_is_not_complete(self):
        for field in ('restart', 'cancellation', 'retirements', 'wakeup'):
            data = self.evidence()
            del data[field]
            with self.subTest(field=field), self.assertRaises(RuntimeError):
                rotation.validate_evidence(data)

    def test_model_initialization_uses_background_task_and_subsequent_run_only_queries(self):
        initial = {'messages': [{'role': 'user', 'content': 'RUNSC_ROTATION ' + 'a' * 32 + ' A 0'}]}
        self.assertEqual(rotation.next_rotation_call(initial)[0], 'background_task_start')
        subsequent = {'messages': [{'role': 'user', 'content': 'RUNSC_ROTATION ' + 'a' * 32 + ' A 2'}]}
        call = rotation.next_rotation_call(subsequent)
        self.assertEqual(call[0], 'bash')

    def test_product_driver_admits_exactly_twelve_runs_and_only_retires_after_lifecycle_checks(self):
        with tempfile.TemporaryDirectory() as folder:
            driver = object.__new__(rotation.FixtureDriver)
            driver.result = Path(folder)
            driver.tag = 'a' * 32
            driver.manifest = {'sourceRevision': 'a' * 40}
            driver.last = {}
            sequence = []
            def admit(label, marker):
                index = int(marker.rsplit(' ', 1)[1])
                sequence.append(('admit', index))
                return {'agentRunId': f'run-{index}', 'sessionId': 'session-' + label}
            driver.admit = Mock(side_effect=admit)
            driver.completed = Mock(side_effect=lambda run: sample('A' if int(run['agentRunId'][4:]) % 2 == 0 else 'B', int(run['agentRunId'][4:])))
            driver.paused = Mock(side_effect=lambda label, run: sample(label, int(run['agentRunId'][4:])))
            def observe(label, run):
                row = sample(label, int(run['agentRunId'][4:]))
                row['runningReservations'] = 1
                row['paused'] = label == 'B'
                row.pop('oracle')
                return row
            driver.observe = Mock(side_effect=observe)
            def wait(observe_fn, predicate, description, timeout=120):
                value = observe_fn()
                self.assertTrue(predicate(value), description)
                return value
            driver.wait = wait
            valid = self.evidence()
            driver.cancellation = Mock(side_effect=lambda: sequence.append(('cancellation',)) or valid['cancellation'])
            driver.restart = Mock(side_effect=lambda: sequence.append(('restart',)) or valid['restart'])
            def retire(row):
                sequence.append(('retire', row['label']))
                return next(item for item in valid['retirements'] if item['environmentId'] == row['environmentId'])
            driver.retire = Mock(side_effect=retire)
            self.assertEqual(driver.run()['status'], 'passed')
            self.assertEqual(driver.admit.call_count, 12)
            self.assertLess(sequence.index(('admit', 3)), sequence.index(('cancellation',)))
            self.assertLess(sequence.index(('restart',)), sequence.index(('admit', 6)))
            self.assertGreater(sequence.index(('retire', 'A')), sequence.index(('admit', 11)))
            self.assertTrue((Path(folder) / 'summary.json').exists())

    def test_driver_failure_preserves_partial_observations_without_retirement(self):
        with tempfile.TemporaryDirectory() as folder:
            driver = object.__new__(rotation.FixtureDriver)
            driver.result = Path(folder)
            driver.tag = 'a' * 32
            driver.manifest = {'sourceRevision': 'a' * 40}
            driver.last = {}
            def admit(label, marker):
                return {'agentRunId': 'run-' + marker.rsplit(' ', 1)[1], 'sessionId': 'session-' + label}
            driver.admit = Mock(side_effect=admit)
            driver.completed = Mock(side_effect=[{'runStatus': 'completed', 'coreTerminalType': 'agent_run_completed',
                                                  'releaseReceiptCount': 1, 'cleanupComplete': True},
                                                 RuntimeError('observed failure')])
            driver.paused = Mock(side_effect=lambda label, run: sample(label, int(run['agentRunId'].split('-')[1])))
            driver.retire = Mock()
            with self.assertRaisesRegex(RuntimeError, 'observed failure'):
                driver.run()
            driver.retire.assert_not_called()
            self.assertFalse((Path(folder) / 'summary.json').exists())
            retained = json.loads((Path(folder) / 'observations.json').read_text())
            self.assertEqual(len(retained['runs']), 1)

    def test_fresh_challenge_and_counter_are_both_required(self):
        for changed in ('challenge', 'counter'):
            data = self.evidence()
            if changed == 'challenge':
                data['runs'][4]['oracle']['challenge'] = data['runs'][0]['oracle']['challenge']
                value = data['runs'][4]['oracle']
                value['proof'] = rotation.challenge_proof(value['nonce'], value['challenge'])
            else:
                data['runs'][4]['oracle']['counter'] = data['runs'][0]['oracle']['counter']
            with self.subTest(changed=changed), self.assertRaises(RuntimeError):
                rotation.validate_evidence(data)

    @unittest.skipUnless(sys.platform == 'linux', 'memory oracle uses Linux process identity')
    def test_oracle_hash_reads_actual_file_after_resume_observation(self):
        import shlex
        with tempfile.TemporaryDirectory(prefix='runsc-file-oracle-') as folder:
            root=str(Path(folder)/'oracle')
            argv=rotation.memory_oracle_server('a'*32,'A')
            argv[0],argv[-1]=sys.executable,root
            process=subprocess.Popen(argv,stdout=subprocess.DEVNULL,stderr=subprocess.PIPE)
            try:
                replies=[]
                for index in (0,2):
                    if replies:Path(root+'.txt').write_text('changed actual file after first observation')
                    client=shlex.split(rotation.memory_oracle_client('a'*32,'A',index))
                    client[0],client[-2]=sys.executable,root+'.sock'
                    output=subprocess.run(client,check=True,capture_output=True,text=True,timeout=20)
                    replies.append(rotation.extract_oracle(output.stdout))
                self.assertEqual(replies[0]['nonce'],replies[1]['nonce'])
                self.assertEqual(replies[0]['pid'],replies[1]['pid'])
                self.assertNotEqual(replies[0]['fileSha256'],replies[1]['fileSha256'])
                self.assertEqual(replies[1]['fileSha256'],rotation.hashlib.sha256(Path(root+'.txt').read_bytes()).hexdigest())
            finally:
                process.terminate();process.wait(timeout=5);process.stderr.close()

    @unittest.skipUnless(sys.platform == 'linux', 'memory oracle uses Linux /proc and Unix sockets')
    def test_actual_oracle_keeps_nonce_only_in_live_process_and_answers_fresh_challenges(self):
        import shlex
        with tempfile.TemporaryDirectory(prefix='runsc-oracle-') as folder:
            root = str(Path(folder) / 'oracle')
            argv = rotation.memory_oracle_server('a' * 32, 'A')
            argv[0], argv[-1] = sys.executable, root
            process = subprocess.Popen(argv, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
            try:
                samples = []
                for index in (0, 2):
                    client = shlex.split(rotation.memory_oracle_client('a' * 32, 'A', index))
                    client[0], client[-2] = sys.executable, root + '.sock'
                    result = subprocess.run(client, check=True, capture_output=True, text=True, timeout=20)
                    samples.append(rotation.extract_oracle(result.stdout))
                self.assertEqual(samples[0]['nonce'], samples[1]['nonce'])
                self.assertEqual(samples[0]['pid'], samples[1]['pid'])
                self.assertEqual(samples[0]['startTicks'], samples[1]['startTicks'])
                self.assertEqual(samples[1]['counter'], samples[0]['counter'] + 1)
                self.assertNotEqual(samples[0]['challenge'], samples[1]['challenge'])
                self.assertNotIn(samples[0]['nonce'], Path(root + '.txt').read_text())
            finally:
                process.terminate()
                process.wait(timeout=5)
                process.stderr.close()

    def test_marker_rejects_shell_injection_and_wrong_alternation(self):
        for marker in ('RUNSC_ROTATION ../../escape A 0', 'RUNSC_ROTATION ' + 'a' * 32 + ' B 0',
                       'RUNSC_ROTATION ' + 'a' * 32 + ' A 12'):
            with self.subTest(marker=marker), self.assertRaises(ValueError):
                rotation.next_rotation_call({'messages': [{'role': 'user', 'content': marker}]})


if __name__ == '__main__':
    unittest.main()
