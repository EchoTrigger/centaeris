import contextlib
import importlib.util
import io
import json
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import Mock, patch

SOURCE = Path(__file__).resolve().parents[1] / 'mock-model/fixed_workload.py'
class FixedWorkloadTests(unittest.TestCase):
    def load(self):
        spec = importlib.util.spec_from_file_location('fixed_workload_subject', SOURCE)
        module = importlib.util.module_from_spec(spec); spec.loader.exec_module(module); return module

    def test_actual_two_process_fixed_work_produces_the_precomputed_sample_specific_artifact(self):
        module = self.load()
        sample = {'sampleId': 'a' * 32, 'operationId': 'b' * 32, 'workUnits': 2}
        with tempfile.TemporaryDirectory() as directory:
            command = module.command_for(sample, output_directory=directory)
            argv = shlex.split(command); argv[0] = sys.executable
            subprocess.run(argv, stdin=subprocess.DEVNULL, check=True, capture_output=True, timeout=15)
            data = (Path(directory) / 'perf-result.bin').read_bytes()
            expected = module.expected_artifact(sample)
            import hashlib
            self.assertEqual(len(data), expected['sizeBytes'])
            self.assertEqual('sha256:' + hashlib.sha256(data).hexdigest(), expected['sha256'])
            value = json.loads(data)
            self.assertEqual(value['sampleId'], sample['sampleId'])
            self.assertEqual(value['cpuWorkers'], 2)
            self.assertEqual(value['completedWorkUnits'], [2, 2])

    def test_memory_profile_is_optional_paired_bounded_and_strict(self):
        module = self.load()
        base = {'sampleId': 'a' * 32, 'operationId': 'b' * 32, 'workUnits': 2}
        for memory, hold in ((1, 1), (1536, 120000)):
            sample = dict(base, memoryMiB=memory, minimumHoldMs=hold)
            self.assertIs(module.validate_sample(sample), sample)
            self.assertEqual(module.sample_from_text(module.MARKER + json.dumps(sample)), sample)
        invalid = [dict(base, memoryMiB=1), dict(base, minimumHoldMs=1), dict(base, memory_mib=1, minimumHoldMs=1), dict(base, extra=1)]
        for key in ('memoryMiB', 'minimumHoldMs'):
            for value in (0, -1, True, False, 1.0, '1', None, 1537 if key == 'memoryMiB' else 120001):
                invalid.append({**base, 'memoryMiB': 1, 'minimumHoldMs': 1, key: value})
        for sample in invalid:
            with self.subTest(sample=sample), self.assertRaises(ValueError):
                module.validate_sample(sample)

    def test_memory_profile_preserves_cpu_work_and_artifact_bytes(self):
        module = self.load()
        base = {'sampleId': 'a' * 32, 'operationId': 'b' * 32, 'workUnits': 2}
        profile = dict(base, memoryMiB=1, minimumHoldMs=100)
        self.assertEqual(module.artifact_bytes(profile), module.artifact_bytes(base))
        with tempfile.TemporaryDirectory() as directory:
            argv = shlex.split(module.command_for(profile, output_directory=directory)); argv[0] = sys.executable
            started = time.monotonic()
            result = subprocess.run(argv, stdin=subprocess.DEVNULL, check=True, capture_output=True, timeout=15)
            self.assertGreaterEqual(time.monotonic() - started, 0.1)
            self.assertEqual((Path(directory) / module.FILENAME).read_bytes(), module.artifact_bytes(base))
            self.assertEqual(result.stdout.decode().splitlines()[-1], 'PERF_FIXED_WORK_COMPLETE ' + base['sampleId'])

    def run_observed_profile(self, *, failure=None, spawn_failure=False, cpu_elapsed=0.02):
        module = self.load()
        sample = {'sampleId': 'a' * 32, 'operationId': 'b' * 32, 'workUnits': 2, 'memoryMiB': 1, 'minimumHoldMs': 100}
        events = []
        class Memory:
            def __init__(self, size):
                events.append(('allocate', size))
            def __setitem__(self, offset, value):
                events.append(('touch', offset, value))
            def __del__(self):
                events.append(('release',))
        children = [Mock(), Mock()]
        for index, child in enumerate(children):
            child.returncode = 1 if failure == 'exit' else 0
            child.poll.return_value = None
            def communicate(timeout, index=index):
                self.assertNotIn(('release',), events)
                events.append(('cpu_done', index))
                if failure == 'timeout':
                    raise subprocess.TimeoutExpired('worker', timeout)
                return (b'truncated' if failure == 'truncated' else b'a' * 64), b''
            child.communicate.side_effect = communicate
        def sleep(seconds):
            self.assertNotIn(('release',), events)
            self.assertEqual([event for event in events if event[0] == 'cpu_done'], [('cpu_done', 0), ('cpu_done', 1)])
            self.assertAlmostEqual(seconds, 0.1 - cpu_elapsed)
            events.append(('hold', seconds))
        with tempfile.TemporaryDirectory() as directory:
            argv = shlex.split(module.command_for(sample, output_directory=directory))
            namespace = {'bytearray': Memory}
            output = io.StringIO()
            with contextlib.redirect_stdout(output), patch.object(sys, 'argv', ['-c'] + argv[3:]), patch('subprocess.Popen', side_effect=[children[0], RuntimeError('spawn failed')] if spawn_failure else children), patch('time.monotonic', side_effect=[10.0, 10.0 + cpu_elapsed]), patch('time.sleep', side_effect=sleep):
                if failure or spawn_failure:
                    with self.assertRaises((RuntimeError, subprocess.TimeoutExpired)):
                        exec(argv[2], namespace)
                    self.assertFalse((Path(directory) / module.FILENAME).exists())
                    self.assertNotIn('PERF_FIXED_WORK_COMPLETE ', output.getvalue())
                else:
                    exec(argv[2], namespace)
            self.assertIn(('release',), events)
            for child in children[:1] if spawn_failure else children:
                child.kill.assert_called_once()
                child.wait.assert_called_once()
                child.stdout.close.assert_called_once()
                child.stderr.close.assert_called_once()
        return events

    def test_parent_touches_each_memory_page_and_holds_it_until_both_workers_and_deadline(self):
        import mmap
        events = self.run_observed_profile()
        touched_pages = {event[1] // mmap.PAGESIZE for event in events if event[0] == 'touch'}
        self.assertEqual(touched_pages, set(range(1024 * 1024 // mmap.PAGESIZE)))
        self.assertEqual(events[0], ('allocate', 1024 * 1024))
        self.assertEqual(events[-1], ('release',))
        self.assertLess(events.index(('cpu_done', 1)), next(i for i, event in enumerate(events) if event[0] == 'hold'))

    def test_cpu_work_beyond_minimum_hold_does_not_add_another_delay(self):
        events = self.run_observed_profile(cpu_elapsed=0.2)
        self.assertFalse(any(event[0] == 'hold' for event in events))
        self.assertLess(events.index(('cpu_done', 1)), events.index(('release',)))

    def test_failed_or_truncated_workers_and_partial_spawn_release_memory_and_reap_children(self):
        for failure in ('timeout', 'truncated', 'exit'):
            with self.subTest(failure=failure):
                self.run_observed_profile(failure=failure)
        self.run_observed_profile(spawn_failure=True)

    @unittest.skipUnless(sys.platform == 'linux', 'live RSS comes from Linux procfs')
    def test_memory_profile_commits_real_resident_pages_in_parent_process(self):
        import select
        module = self.load()
        sample = {'sampleId': 'a' * 32, 'operationId': 'b' * 32, 'workUnits': 2, 'memoryMiB': 32, 'minimumHoldMs': 500}
        with tempfile.TemporaryDirectory() as directory:
            argv = shlex.split(module.command_for(sample, output_directory=directory)); argv[0] = sys.executable
            child = subprocess.Popen(argv, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            try:
                self.assertTrue(select.select([child.stdout], [], [], 5)[0], 'memory readiness was not emitted')
                self.assertEqual(child.stdout.readline().decode().strip(), 'PERF_FIXED_WORK_READY ' + sample['sampleId'])
                rss_line = next(line for line in Path('/proc/' + str(child.pid) + '/status').read_text().splitlines() if line.startswith('VmRSS:'))
                self.assertGreaterEqual(int(rss_line.split()[1]), 32 * 1024)
                self.assertIsNone(child.poll())
                stdout, stderr = child.communicate(timeout=15)
                self.assertEqual(child.returncode, 0, stderr.decode())
                self.assertIn(('PERF_FIXED_WORK_COMPLETE ' + sample['sampleId']).encode(), stdout)
                self.assertEqual((Path(directory) / module.FILENAME).read_bytes(), module.artifact_bytes(sample))
            finally:
                if child.poll() is None:
                    child.kill(); child.wait(timeout=5)
                child.stdout.close(); child.stderr.close()

    def test_old_sample_bytes_cannot_satisfy_a_new_sample_expectation(self):
        module = self.load()
        first = module.expected_artifact({'sampleId': 'a' * 32, 'operationId': 'b' * 32, 'workUnits': 2})
        second = module.expected_artifact({'sampleId': 'c' * 32, 'operationId': 'd' * 32, 'workUnits': 2})
        self.assertNotEqual(first['sha256'], second['sha256'])

class FixedWorkloadHandlerTests(unittest.TestCase):
    def request(self, rounds, *, model_wait_ms=None, waits=None, sample_id=None, sample_profile=None):
        import io
        import json
        source=Path(__file__).resolve().parents[1]/'mock-model/server.py'
        spec=importlib.util.spec_from_file_location('fixed_handler',source)
        module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)
        sample={'sampleId':sample_id or 'a'*32,'operationId':'b'*32,'workUnits':2}
        if sample_profile is not None:sample.update(sample_profile)
        previous=[{'role':'user','content':'previous sample'}, {'type':'function_call_output','call_id':'old-bash','output':'old'}, {'type':'function_call_output','call_id':'old-publish','output':'old'}]
        current=[{'role':'user','content':'PERF_FIXED_WORKLOAD '+json.dumps(sample)}]+[{'type':'function_call_output','call_id':f'current-{i}','output':'ok'} for i in range(rounds)]
        body=json.dumps({'model':'perf-fixed-workload','stream':True,'input':previous+current}).encode()
        handler=object.__new__(module.Handler);handler.path='/v1/responses';handler.headers={'content-length':str(len(body))}
        handler.rfile=io.BytesIO(body);handler.wfile=io.BytesIO()
        handler.send_response=lambda *args:None;handler.send_header=lambda *args:None;handler.end_headers=lambda:None
        from unittest.mock import patch
        def observe_model_wait(seconds):
            self.assertNotIn(b'response.output_item.added',handler.wfile.getvalue(), 'model wait must precede tool intent')
            if waits is not None:waits.append(seconds)
        with patch.dict(module.os.environ, {'MOCK_FIXED_MODEL_WAIT_MS':str(0 if model_wait_ms is None else model_wait_ms)}), patch.object(module.time, 'sleep', side_effect=observe_model_wait):
            handler.do_POST()
        events=[json.loads(line[6:]) for line in handler.wfile.getvalue().decode().splitlines() if line.startswith('data: ')]
        return module,sample,events[-1]['response']['output'][0]

    def test_rolling_session_ignores_previous_samples_tool_history(self):
        import json
        module,sample,item=self.request(0)
        self.assertEqual(item.get('name'),'bash')
        self.assertEqual(json.loads(item['arguments'])['command'],module._fixed_workload.command_for(sample))
        self.assertEqual(self.request(1)[2].get('name'),'publish_artifact')
        self.assertEqual(self.request(2)[2]['type'],'message')

    def test_memory_profile_reaches_the_model_command_without_changing_the_sample(self):
        profile = {'memoryMiB': 1, 'minimumHoldMs': 100}
        module, sample, item = self.request(0, sample_profile=profile)
        self.assertEqual({key: sample[key] for key in profile}, profile)
        self.assertEqual(json.loads(item['arguments'])['command'], module._fixed_workload.command_for(sample))
        with self.assertRaises(ValueError):
            self.request(0, sample_profile={'memoryMiB': 1})

    def test_model_wait_occurs_once_before_each_rolling_runs_first_bash(self):
        waits=[]
        for sample_id in ('a'*32,'c'*32):
            self.assertEqual(self.request(0,model_wait_ms=18000,waits=waits,sample_id=sample_id)[2].get('name'),'bash')
            self.assertEqual(self.request(1,model_wait_ms=18000,waits=waits,sample_id=sample_id)[2].get('name'),'publish_artifact')
            self.assertEqual(self.request(2,model_wait_ms=18000,waits=waits,sample_id=sample_id)[2]['type'],'message')
        self.assertEqual(waits,[18.0,18.0])
        self.request(0,waits=waits)
        self.assertEqual(waits,[18.0,18.0],'default zero must not delay model or tool')

    def test_model_wait_has_an_explicit_upper_bound(self):
        for value in (-1,120001,'invalid'):
            with self.assertRaises(ValueError):self.request(0,model_wait_ms=value,waits=[])

if __name__ == '__main__': unittest.main()
