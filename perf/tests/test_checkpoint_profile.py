import importlib.util
import json
from pathlib import Path
import unittest

HARNESS = Path(__file__).resolve().parents[1] / 'harness'


def load(name):
    spec = importlib.util.spec_from_file_location(name, HARNESS / f'{name}.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


checkpoint = load('checkpoint_profile')
recovery = load('recovery_checkpoint_profile')


class CheckpointProfileTests(unittest.TestCase):
    def test_extracts_exactly_one_result(self):
        value = {'waiting': 1, 'measurements': []}
        output = 'noise\n' + checkpoint.MARKER + json.dumps(value) + '\nmore noise\n'
        self.assertEqual(checkpoint.result_from_output(output), value)
        for invalid in ('', output + checkpoint.MARKER + '{}'):
            with self.assertRaises(RuntimeError):
                checkpoint.result_from_output(invalid)

    def test_database_url_quotes_credentials_and_ipv6(self):
        config = {'host': '::1', 'port': '5432', 'user': 'user name', 'password': 'p@ss'}
        self.assertEqual(checkpoint.database_url(config, 'profile'),
                         'postgresql://user%20name:p%40ss@[::1]:5432/profile')

    def test_statement_delta_keeps_only_changed_queries(self):
        before = {'a': {'query': 'q', 'calls': 2, 'rows': 4, 'totalExecMs': 1.5,
                        'sharedBlocksHit': 7, 'sharedBlocksRead': 1}}
        after = {'a': {'query': 'q', 'calls': 5, 'rows': 10, 'totalExecMs': 2.0,
                       'sharedBlocksHit': 9, 'sharedBlocksRead': 1}}
        self.assertEqual(recovery.delta(before, after), [{
            'queryId': 'a', 'query': 'q', 'calls': 3, 'rows': 6, 'totalExecMs': 0.5,
            'sharedBlocksHit': 2, 'sharedBlocksRead': 0}])


if __name__ == '__main__':
    unittest.main()
