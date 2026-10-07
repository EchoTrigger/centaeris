import importlib.util
import json
from pathlib import Path
import sys
import unittest

HARNESS = Path(__file__).resolve().parents[1] / 'harness'
sys.path.insert(0, str(HARNESS))
import pool_validation as validation


class PoolValidationTests(unittest.TestCase):
    def test_only_api_may_change_during_rollback(self):
        before = {'api': 'a', 'worker': 'w', 'postgres': 'p'}
        validation.verify_api_replacement(before, dict(before, api='b'))
        for after in (before, {'api': 'b', 'worker': 'changed', 'postgres': 'p'},
                      {'api': 'b', 'postgres': 'p'}):
            with self.assertRaises(RuntimeError):
                validation.verify_api_replacement(before, after)

    def test_empty_missing_or_overbudget_metrics_fail(self):
        good = {'maxConnections': 100, 'connections': 20}
        validation.check_database_budget(good)
        for value in ({}, dict(good, connections=80), dict(good, maxConnections=0)):
            with self.assertRaises(RuntimeError):
                validation.check_database_budget(value)

    def test_probe_evidence_requires_serving_pool_and_no_error(self):
        sample = {'kind': 'sample', 'pid': 1, 'pool': {'state': 'active',
                  'stats': {'pool_size': 2, 'pool_max': 8, 'requests_errors': 0}}}
        validation.validate_probe([sample], 8)
        for rows in ([], [dict(sample, kind='httpError')],
                     [dict(sample, pool={'state': 'uncreated', 'stats': {}})],
                     [dict(sample, pool={'state': 'active', 'stats': {'pool_size': 9}})]):
            with self.assertRaises(RuntimeError):
                validation.validate_probe(rows, 8)
        validation.validate_probe([dict(sample, pool={'state': 'disabled', 'stats': {}})], 0)

    def test_isolated_overlay_keeps_existing_ownership_guards(self):
        import tempfile
        import control
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / '.env.example').write_text((control.ROOT / '.env.example').read_text())
            env_file = control.initialize(root)
            command, env = validation.pool_compose_command(env_file, ['config', '--format', 'json'])
            config = json.loads(control.run(command, env=env))
            control.validate_config(config, control.ROOT)
            api = config['services']['api']
            self.assertEqual(api['image'], 'centaeris-perf-api-pool:local')
            self.assertIn('pool_probe:application', api['entrypoint'])
            self.assertEqual(api['environment']['API_WORKERS'], '1')
            self.assertNotIn('pool_probe', str(config['services']['api-init']['entrypoint']))


if __name__ == '__main__':
    unittest.main()
