import importlib.util
import json
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile
import unittest

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

    def test_old_sample_bytes_cannot_satisfy_a_new_sample_expectation(self):
        module = self.load()
        first = module.expected_artifact({'sampleId': 'a' * 32, 'operationId': 'b' * 32, 'workUnits': 2})
        second = module.expected_artifact({'sampleId': 'c' * 32, 'operationId': 'd' * 32, 'workUnits': 2})
        self.assertNotEqual(first['sha256'], second['sha256'])

class FixedWorkloadHandlerTests(unittest.TestCase):
    def request(self, rounds, *, model_wait_ms=None, waits=None, sample_id=None):
        import io
        import json
        source=Path(__file__).resolve().parents[1]/'mock-model/server.py'
        spec=importlib.util.spec_from_file_location('fixed_handler',source)
        module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)
        sample={'sampleId':sample_id or 'a'*32,'operationId':'b'*32,'workUnits':2}
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
