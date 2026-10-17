import importlib.util
import json
import hashlib
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('prepare', Path(__file__).with_name('prepare.py'))
prepare = importlib.util.module_from_spec(spec)
spec.loader.exec_module(prepare)


class PreparationTests(unittest.TestCase):
    def fixture(self, root):
        source, bins = root / 'source', root / 'bins'
        source.mkdir()
        (source / '.git').write_text('fixture placeholder; git calls are isolated by mocks')
        bins.mkdir()
        (source / '.env.example').write_text('POSTGRES_DB=old\n')
        for name in ('runtime_server','execution_agent','environment_task_service'):
            (bins / name).write_bytes(name.encode())
        model = root / 'model.py'
        model.write_text('pass\n')
        manifest=root / 'build-manifest.json'
        manifest.write_text(json.dumps({'sourceRevision':'a'*40,'sourceTree':'c'*40,'binarySha256':{
            name:hashlib.sha256(name.encode()).hexdigest() for name in
            ('runtime_server','execution_agent','environment_task_service')}}))
        return ['prepare','--source',str(source),'--bins',str(bins),'--model-script',str(model),
                '--output',str(root / 'result'),'--revision','a' * 40,'--build-manifest',str(manifest)] + [
                    item for name in ('api','worker','runtime','general','postgres','redis')
                    for item in ('--'+name+'-image','fixture-image')]

    def benchmark_fixture(self, root):
        argv = self.fixture(root)
        index = argv.index('--model-script')
        del argv[index:index+2]
        seed_script = root / 'source/scripts/workspace/retained_acceptance/benchmark_seed.py'
        seed_script.parent.mkdir(parents=True)
        seed_script.write_text('# synthetic seed fixture\n')
        model_validator = root / 'source/scripts/harbor/workspace_plan.py'
        model_validator.parent.mkdir(parents=True)
        model_validator.write_text('# synthetic validator fixture\n')
        helper = root / 'musl-helper'
        helper.write_bytes(b'synthetic-musl-helper')
        digest = hashlib.sha256(helper.read_bytes()).hexdigest()
        proof = root / 'helper-build.json'
        proof.write_text(json.dumps({'sourceRevision': 'a'*40, 'sourceTree': 'c'*40,
            'target': 'x86_64-unknown-linux-musl', 'binarySha256': {'execution_agent': digest}}))
        return argv + ['--benchmark-mode', '--docker-socket', '/run/owned-benchmark/docker.sock',
                       '--benchmark-helper', str(helper), '--benchmark-helper-sha256', digest,
                       '--benchmark-helper-manifest', str(proof)]

    def benchmark_commands(self):
        return [subprocess.CompletedProcess([], 0, '', '')] + [
            subprocess.CompletedProcess([], 0, 'sha256:'+'b'*64+'\n', '')]*6 + [
            subprocess.CompletedProcess([], 0, 'a'*40, ''),
            subprocess.CompletedProcess([], 0, '', ''),
            subprocess.CompletedProcess([], 0, 'c'*40, ''),
            subprocess.CompletedProcess([], 0, 'owned-daemon-identity', '')]

    def test_benchmark_is_distinct_real_provider_fixture_without_starting_any_service(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            argv = self.benchmark_fixture(root)
            with patch.object(sys, 'argv', argv), patch.object(prepare.subprocess, 'run',
                    side_effect=self.benchmark_commands()) as run, patch('builtins.print'):
                prepare.main()
            config = json.loads((root / 'result/compose.json').read_text())
            manifest = json.loads((root / 'result/manifest.json').read_text())
            self.assertEqual(set(config['services']), {'api', 'worker', 'runtime', 'postgres', 'redis', 'catalog-init'})
            self.assertTrue(config['name'].startswith('tbworkspace'))
            self.assertTrue(manifest['benchmarkMode'])
            self.assertIn(str((root / 'source/scripts/workspace/retained_acceptance/benchmark_seed.py').resolve()) +
                          ':/harness/benchmark_seed.py:ro', config['services']['api']['volumes'])
            self.assertEqual(manifest['dockerSocket'], '/run/owned-benchmark/docker.sock')
            self.assertEqual(manifest['dockerHostId'], 'owned-daemon-identity')
            self.assertEqual(manifest['benchmarkHelper']['target'], 'x86_64-unknown-linux-musl')
            self.assertEqual(config['services']['runtime']['mem_limit'], '512m')
            env = dict(line.split('=', 1) for line in (root / 'result/synthetic.env').read_text().splitlines())
            self.assertEqual(env['POSTGRES_DB'], config['name'])
            self.assertEqual(env['POSTGRES_USER'], config['name'])
            self.assertEqual(env['CENTAERIS_BENCHMARK_FIXTURE_ID'], config['name'])
            self.assertGreaterEqual(int(env['RESIDENT_TENANT_PIDS']), 8192)
            self.assertGreaterEqual(int(env['RESIDENT_GLOBAL_PIDS']), 8192)
            self.assertEqual(int(env['SANDBOX_PIDS_LIMIT']), 4096)
            self.assertNotIn('BENCHMARK_MODEL_KEY', env)
            self.assertIn(str((root / 'source/scripts/harbor/workspace_plan.py').resolve()) +
                          ':/harness/workspace_plan.py:ro', config['services']['api']['volumes'])
            helper = str((root / 'musl-helper').resolve())
            self.assertEqual(env['BENCHMARK_EXECUTION_HELPER'], helper)
            self.assertIn(helper+':'+helper+':ro', config['services']['runtime']['volumes'])
            self.assertIn('/run/owned-benchmark/docker.sock:/var/run/docker.sock', config['services']['runtime']['volumes'])
            for call in run.call_args_list:
                command = call.args[0]
                if command[0] == 'docker':
                    self.assertEqual(command[1:3], ['--host', 'unix:///run/owned-benchmark/docker.sock'])
            for service, gets_maintenance in (('api', True), ('runtime', True), ('worker', False), ('catalog-init', False)):
                effective = {}
                for env_path in config['services'][service]['env_file']:
                    effective.update(line.split('=', 1) for line in Path(env_path).read_text().splitlines())
                self.assertEqual('ENVIRONMENT_MAINTENANCE_TOKEN' in effective, gets_maintenance, service)

    def test_benchmark_foreign_inventory_refuses_without_stopping_existing_work(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            argv = self.benchmark_fixture(root)
            with patch.object(sys, 'argv', argv), patch.object(prepare.subprocess, 'run',
                    return_value=subprocess.CompletedProcess([], 0, 'foreign-managed-task\n', '')) as run:
                with self.assertRaisesRegex(SystemExit, 'managed sandboxes already exist'):
                    prepare.main()
            self.assertEqual(run.call_count, 1)
            self.assertFalse((root / 'result').exists())
            self.assertEqual(run.call_args.args[0][:5],
                ['docker', '--host', 'unix:///run/owned-benchmark/docker.sock', 'ps', '-a'])

    def test_benchmark_requires_explicit_socket_and_matching_static_helper_provenance(self):
        for change in ('missingSocket', 'badHelperHash', 'badHelperSource'):
            with self.subTest(change=change), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                argv = self.benchmark_fixture(root)
                if change == 'missingSocket':
                    index = argv.index('--docker-socket')
                    del argv[index:index+2]
                elif change == 'badHelperHash':
                    argv[argv.index('--benchmark-helper-sha256')+1] = 'f'*64
                else:
                    proof = root / 'helper-build.json'
                    value = json.loads(proof.read_text())
                    value['sourceRevision'] = 'd'*40
                    proof.write_text(json.dumps(value))
                with patch.object(sys, 'argv', argv), patch.object(prepare.subprocess, 'run',
                        side_effect=self.benchmark_commands()), patch('builtins.print'):
                    with self.assertRaises(SystemExit):
                        prepare.main()
                self.assertFalse((root / 'result').exists())

    def test_foreign_inventory_refuses_before_writing(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            with patch.object(sys, 'argv', self.fixture(root)), \
                    patch.object(prepare.subprocess,'run',return_value=subprocess.CompletedProcess([],0,'foreign\n','')):
                with self.assertRaisesRegex(SystemExit, 'managed sandboxes already exist'):
                    prepare.main()
            self.assertFalse((root / 'result').exists())

    def test_prepared_fixture_uses_independent_maintenance_authority_and_no_start(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            with patch.object(sys, 'argv', self.fixture(root)), \
                    patch.object(prepare.subprocess,'run',side_effect=[subprocess.CompletedProcess([],0,'','')]
                        + [subprocess.CompletedProcess([],0,'sha256:'+'b'*64+'\n','')]*6
                        + [subprocess.CompletedProcess([],0,'a'*40,'')]
                        + [subprocess.CompletedProcess([],0,'','')]
                        + [subprocess.CompletedProcess([],0,'c'*40,'')]) as run, \
                    patch('builtins.print'):
                prepare.main()
            self.assertEqual(run.call_count, 10)
            env = dict(line.split('=',1) for line in (root / 'result/synthetic.env').read_text().splitlines())
            self.assertGreaterEqual(len(env['ENVIRONMENT_MAINTENANCE_TOKEN'].encode()),32)
            self.assertNotEqual(env['ENVIRONMENT_MAINTENANCE_TOKEN'],env['INTERNAL_API_TOKEN'])
            compose = json.loads((root / 'result/compose.json').read_text())
            self.assertEqual(compose['services']['runtime']['ports'],['127.0.0.1:19001:9000'])
            self.assertTrue(compose['name'].startswith('retainedaccept'))
            self.assertIn(str(Path(prepare.__file__).with_name('boundary.py').resolve())+':/harness/boundary.py:ro',compose['services']['model']['volumes'])

    def test_catalog_initialization_is_separate_from_read_only_runtime_services(self):
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp)
            with patch.object(sys,'argv',self.fixture(root)), \
                    patch.object(prepare.subprocess,'run',side_effect=[subprocess.CompletedProcess([],0,'','')]
                        + [subprocess.CompletedProcess([],0,'sha256:'+'b'*64+'\n','')]*6
                        + [subprocess.CompletedProcess([],0,'a'*40,'')]
                        + [subprocess.CompletedProcess([],0,'','')]
                        + [subprocess.CompletedProcess([],0,'c'*40,'')]), patch('builtins.print'):
                prepare.main()
            config=json.loads((root / 'result/compose.json').read_text())
            self.assertTrue('catalog-init' in config['services'],
                'fixture needs an explicit setup service for the installed Plugin catalog')
            setup=config['services']['catalog-init']
            self.assertEqual(setup['command'],['/app/.venv/bin/python','manage.py','initialize_plugin_catalog'])
            self.assertEqual(setup['profiles'],['setup'])
            self.assertNotIn('ports',setup)
            self.assertEqual(setup['env_file'],config['services']['api']['env_file'])
            plugin_target='/opt/centaeris/plugins'
            def plugin_mount(service):
                return next(mount.split(':') for mount in service['volumes']
                    if len(mount.split(':'))>=2 and mount.split(':')[1]==plugin_target)
            self.assertEqual(plugin_mount(setup),['plugins',plugin_target,'rw'])
            for name in ('api','runtime'):
                self.assertEqual(plugin_mount(config['services'][name]),['plugins',plugin_target,'ro'])
            for mount in config['services']['api']['volumes']:
                if mount.split(':')[0]!='plugins':
                    self.assertIn(mount,setup['volumes'])
            for name in ('catalog-init','api','worker'):
                effective={}
                service=config['services'][name]
                for env_file in service['env_file']:
                    effective.update(line.split('=',1) for line in Path(env_file).read_text().splitlines())
                effective.update(service.get('environment',{}))
                self.assertTrue('ENVIRONMENT_MAINTENANCE_TOKEN' not in effective,
                    name+' must not receive maintenance authority')

    def test_only_runtime_inherits_maintenance_authority(self):
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp)
            with patch.object(sys,'argv',self.fixture(root)), \
                    patch.object(prepare.subprocess,'run',side_effect=[subprocess.CompletedProcess([],0,'','')]
                        + [subprocess.CompletedProcess([],0,'sha256:'+'b'*64+'\n','')]*6
                        + [subprocess.CompletedProcess([],0,'a'*40,'')]
                        + [subprocess.CompletedProcess([],0,'','')]
                        + [subprocess.CompletedProcess([],0,'c'*40,'')]), patch('builtins.print'):
                prepare.main()
            config=json.loads((root / 'result/compose.json').read_text())
            def effective_environment(service):
                result={}
                for env_file in service.get('env_file',[]):
                    result.update(line.split('=',1) for line in Path(env_file).read_text().splitlines())
                result.update(service.get('environment',{}))
                return result
            for name in ('api','worker'):
                effective=effective_environment(config['services'][name])
                with self.subTest(service=name):
                    self.assertTrue('ENVIRONMENT_MAINTENANCE_TOKEN' not in effective,
                        name+' must not inherit independent maintenance authority')
                    self.assertTrue(bool(effective.get('INTERNAL_API_TOKEN')),
                        name+' still needs its ordinary internal authority')
            runtime=effective_environment(config['services']['runtime'])
            full=dict(line.split('=',1) for line in (root / 'result/synthetic.env').read_text().splitlines())
            self.assertTrue(len(runtime.get('ENVIRONMENT_MAINTENANCE_TOKEN','').encode())>=32,
                'Runtime must retain valid independent maintenance authority')
            self.assertTrue(runtime['ENVIRONMENT_MAINTENANCE_TOKEN']==full['ENVIRONMENT_MAINTENANCE_TOKEN'],
                'Runtime and private maintainer driver must use the same maintenance authority')
            self.assertTrue(runtime['ENVIRONMENT_MAINTENANCE_TOKEN']!=runtime['INTERNAL_API_TOKEN'],
                'maintenance and ordinary internal authority must remain distinct')


if __name__ == '__main__':
    unittest.main()
