import json
from pathlib import Path
import tempfile
import unittest

from perf.harness import control


class DockerCreateLimitTests(unittest.TestCase):
    def test_experiment_override_only_changes_runtime_create_limit(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / '.env.example').write_text((control.ROOT / '.env.example').read_text(encoding='utf-8'), encoding='utf-8')
            env_file = control.initialize(root)
            command, env = control.compose_command(control.ROOT, env_file, ['config', '--format', 'json'])
            baseline = json.loads(control.run(command, env=env))
            self.assertEqual(baseline['services']['runtime']['environment']['DOCKER_CREATE_CONCURRENCY'], '2')
            with env_file.open('a', encoding='utf-8') as handle:
                handle.write('PERF_DOCKER_CREATE_CONCURRENCY=4\n')
            experiment = json.loads(control.run(command, env=env))
            baseline['services']['runtime']['environment']['DOCKER_CREATE_CONCURRENCY'] = '4'
            self.assertEqual(baseline, experiment)
