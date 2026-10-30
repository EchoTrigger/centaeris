import importlib.util
import io
import json
from pathlib import Path
import unittest
from unittest.mock import patch

SOURCE = Path(__file__).resolve().parents[1] / 'mock-model/server.py'
SPEC = importlib.util.spec_from_file_location('artifact_mock', SOURCE)

class FileWorkloadMockTests(unittest.TestCase):
    def request(self, rounds):
        module = importlib.util.module_from_spec(SPEC); SPEC.loader.exec_module(module)
        handler = object.__new__(module.Handler); handler.path = '/v1/responses'
        value = {'model': 'perf-file-workload', 'stream': True,
                 'input': [{'type': 'function_call_output', 'call_id': f'call-{i}', 'output': 'result'} for i in range(rounds)]}
        body = json.dumps(value).encode(); handler.headers = {'content-length': str(len(body))}
        handler.rfile = io.BytesIO(body); handler.wfile = io.BytesIO()
        handler.send_response = lambda *args: None; handler.send_header = lambda *args: None; handler.end_headers = lambda: None
        with patch.object(module, 'TOOL_ARGS', json.dumps({'command': 'python3 /fixed/perf_workload.py'})):
            handler.do_POST()
        events = [json.loads(line[6:]) for line in handler.wfile.getvalue().decode().splitlines() if line.startswith('data: ')]
        return events[-1]['response']['output']

    def test_file_workload_executes_real_command_then_publishes_the_file_then_finishes(self):
        call = self.request(0)[0]
        self.assertEqual(call['name'], 'bash')
        self.assertEqual(json.loads(call['arguments']), {'command': 'python3 /fixed/perf_workload.py'})
        published = self.request(1)[0]
        self.assertEqual(published['name'], 'publish_artifact')
        self.assertEqual(json.loads(published['arguments']), {'path': '/mnt/data/perf-result.bin'})
        self.assertEqual(self.request(2)[0]['type'], 'message')

if __name__ == '__main__':
    unittest.main()
