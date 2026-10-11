import importlib.util
import json
import os
import sys
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

SOURCE = Path(__file__).resolve().parents[1] / 'harness' / 'control.py'
spec = importlib.util.spec_from_file_location('perf_control', SOURCE)
control = importlib.util.module_from_spec(spec)
spec.loader.exec_module(control)


class IsolationTests(unittest.TestCase):
    def test_isolated_fixture_contains_finite_upload_capacity_without_inheriting_host_inputs(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / '.env.example').write_bytes((control.ROOT / '.env.example').read_bytes())
            with patch.dict(os.environ, {
                    'UPLOAD_BODY_MAX_BYTES': 'untrusted-host-value',
                    'UPLOAD_TEMP_MAX_BYTES': 'untrusted-host-value',
                    'UPLOAD_MAX_CONCURRENT': 'untrusted-host-value'}):
                env_file = control.initialize(root)
                values = control.read_env(env_file)
                _, process_env = control.compose_command(control.ROOT, env_file, ['config', '--format', 'json'])
            self.assertEqual(values['UPLOAD_BODY_MAX_BYTES'], '1048576')
            self.assertEqual(values['UPLOAD_TEMP_MAX_BYTES'], '4194304')
            self.assertEqual(values['UPLOAD_MAX_CONCURRENT'], '2')
            for key in ('UPLOAD_BODY_MAX_BYTES', 'UPLOAD_TEMP_MAX_BYTES', 'UPLOAD_MAX_CONCURRENT'):
                self.assertNotIn(key, process_env)

    def test_manifest_records_the_shared_checkout_without_external_core_tools(self):
        stack = object.__new__(control.Stack)
        revision = control.run(['git', 'rev-parse', 'HEAD'], cwd=control.ROOT).strip()
        dirty = control.run(['git', 'status', '--porcelain'], cwd=control.ROOT).splitlines()
        container = {'Id': 'test-container', 'Image': 'test-image',
                     'State': {'StartedAt': '2026-10-08T00:00:00Z'}}
        # Keep Git and source reads real; only the external Docker stack is isolated.
        with patch.object(stack, 'compose', return_value='2\n'), \
                patch.object(stack, 'container', return_value=container):
            manifest = stack.manifest()
        self.assertEqual(manifest['workspaceSha'], revision)
        self.assertEqual(manifest['coreSha'], revision)
        self.assertEqual(manifest['dirtyFiles'], dirty)
        self.assertEqual(manifest['workerSlots'], 2)
        self.assertEqual(set(manifest['containers']), set(control.SERVICES))

    def test_command_utf8_io_does_not_depend_on_windows_locale(self):
        # Reproduce a non-UTF-8 Windows default even on UTF-8 test hosts.
        with patch.object(control.subprocess, '_text_encoding', return_value='gbk'):
            output = control.run(
                [sys.executable, '-c',
                 'import sys; value = sys.stdin.buffer.read().decode("utf-8"); '
                 'sys.stdout.buffer.write(value.encode("utf-8"))'],
                input='阶段报告：中文输入与输出\n')
        self.assertEqual(output, '阶段报告：中文输入与输出\n')

    def test_terminal_accounting_is_exact_and_cancellation_is_not_completion(self):
        self.assertEqual(control.account_runs(['a', 'b'], [{'id': 'a', 'status': 'completed'}, {'id': 'b', 'status': 'completed'}]), {'completed': 2})
        for rows in ([{'id': 'a', 'status': 'completed'}],
                     [{'id': 'a', 'status': 'completed'}, {'id': 'b', 'status': 'cancelled'}]):
            with self.assertRaises(RuntimeError):
                control.account_runs(['a', 'b'], rows)
        with self.assertRaises(RuntimeError):
            control.account_runs(['a'], [{'id': 'a', 'status': 'completed'}], accepted_count=2)

    def test_rendered_compose_is_isolated(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / '.env.example').write_text((control.ROOT / '.env.example').read_text())
            env_file = control.initialize(root)
            command, env = control.compose_command(control.ROOT, env_file, ['config', '--format', 'json'])
            config = json.loads(control.run(command, env=env))
            control.validate_config(config, control.ROOT)
            self.assertEqual(config['services']['api']['image'], 'centaeris-perf-api:local')
            self.assertEqual(config['services']['material-worker']['image'], 'centaeris-perf-api:local')
            self.assertEqual(config['services']['material-worker']['environment']['MATERIAL_PROCESSOR_IMAGE'],
                             config['services']['document-processor']['image'])
            self.assertEqual(config['services']['runtime']['environment']['PLUGIN_VOLUME_NAME'], 'centaeris-perf_plugin-data')

    def test_smoke_reads_replayed_terminal_and_has_a_process_deadline(self):
        spec = importlib.util.spec_from_file_location('perf_bootstrap', SOURCE.parent / 'bootstrap.py')
        bootstrap = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(bootstrap)
        for terminal in ('completed', 'failed', 'interrupted'):
            lines = [b': heartbeat\n', ('data: ' + json.dumps({'event': {'type': 'agent_run_' + terminal}})).encode()]
            self.assertEqual(bootstrap.read_terminal(lines), 'agent_run_' + terminal)
        with self.assertRaises(ValueError):
            bootstrap.read_terminal([b'data: invalid-json'])
        with self.assertRaises(RuntimeError):
            bootstrap.read_terminal([b': heartbeat'])
        with patch.object(bootstrap.subprocess, 'run') as launch:
            bootstrap.run_bounded_smoke()
            self.assertEqual(launch.call_args.kwargs['timeout'], 180)

    def test_large_sql_is_sent_over_stdin(self):
        stack = object.__new__(control.Stack)
        stack.values = {'POSTGRES_USER': 'test', 'POSTGRES_DB': 'test'}
        statement = 'SELECT ' + '1,' * 20000 + '1;'
        with patch.object(stack, 'compose', return_value='ok') as compose:
            self.assertEqual(stack.sql(statement), 'ok')
        args, kwargs = compose.call_args
        self.assertNotIn(statement, args[0])
        self.assertEqual(kwargs['input'], statement)

    def test_completion_buckets_use_completion_time_and_exclude_failures_from_latency(self):
        rows = [
            {'id': 'a', 'status': 'completed', 'createdAt': '2026-01-01T00:00:00+00:00', 'completedAt': '2026-01-01T00:02:00+00:00'},
            {'id': 'b', 'status': 'failed', 'createdAt': '2026-01-01T00:00:00+00:00', 'completedAt': '2026-01-01T00:05:00+00:00'},
        ]
        summary = control.summarize_runs(rows)
        self.assertEqual(summary['completionBuckets'], {'2026-01-01T00:02': 1})
        self.assertEqual(summary['completedLatencyMs']['p50'], 120000)
        self.assertEqual(summary['failed'], 1)

    def test_sampling_failure_does_not_kill_workload_or_get_hidden_by_accounting(self):
        stack = unittest.mock.Mock()
        stack.env_file = Path('/test.env')
        stack.values = {'BOOTSTRAP_SUPERADMIN_EMAIL': 'test', 'BOOTSTRAP_SUPERADMIN_PASSWORD': 'secret'}
        stack.quiet.return_value = True
        stack.sample.side_effect = [RuntimeError('sampling failed'), []]
        def sql(statement):
            if 'json_agg' in statement:
                raise OSError('accounting failed')
            return '{}'
        stack.sql.side_effect = sql
        process = unittest.mock.Mock(returncode=0)
        process.poll.side_effect = [None, 0, 0]
        with tempfile.TemporaryDirectory() as tmp:
            output = Path(tmp)
            def launch(*args, **kwargs):
                kwargs['stdout'].write('accepted run a session s\n')
                kwargs['stdout'].flush()
                return process
            with patch.object(control.subprocess, 'check_output', return_value='a' * 40), patch.object(control.subprocess, 'Popen', side_effect=launch), patch.object(control, 'run', return_value='k6'), patch.object(control.time, 'sleep'), patch.object(control.time, 'monotonic', return_value=0):
                with self.assertRaises(RuntimeError):
                    control.load(stack, output, 60, 10)
            errors = json.loads((output / 'failure.json').read_text())['errors']
            self.assertEqual([e['phase'] for e in errors], ['sampling', 'accounting'])
            self.assertFalse(json.loads((output / 'outcome.json').read_text())['valid'])
        process.terminate.assert_not_called()

    def test_environment_is_generated_without_reading_private_env(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / '.env.example').write_text('COMPOSE_PROJECT_NAME=centaeris-workspace\nPOSTGRES_PASSWORD=\nAPI_HOST_PORT=8000\n')
            (root / '.env').write_text('POSTGRES_PASSWORD=production-secret')
            result = control.initialize(root)
            values = control.read_env(result)
            self.assertEqual(values['COMPOSE_PROJECT_NAME'], 'centaeris-perf')
            self.assertNotEqual(values['POSTGRES_PASSWORD'], 'production-secret')
            self.assertEqual((root / '.env').read_text(), 'POSTGRES_PASSWORD=production-secret')
            with self.assertRaises(FileExistsError):
                control.initialize(root)

    def test_init_uses_runsc_and_requires_an_explicit_runc_override(self):
        for override, expected in ((None, 'runsc'), ('runsc', 'runsc'), ('runc', 'runc')):
            with self.subTest(override=override), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                (root / '.env.example').write_text((control.ROOT / '.env.example').read_text())
                path = control.initialize(root, oci_runtime=override)
                self.assertEqual(control.read_env(path)['OCI_RUNTIME'], expected)
                command, env = control.compose_command(control.ROOT, path, ['config', '--format', 'json'])
                services = json.loads(control.run(command, env=env))['services']
                for name in ('runtime', 'material-worker'):
                    self.assertEqual(services[name]['environment']['OCI_RUNTIME'], expected)

    def test_init_rejects_invalid_runtime_without_creating_an_environment(self):
        for value in ('', 'RUNSC', ' runsc ', 'unknown'):
            with self.subTest(value=value), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                (root / '.env.example').write_text((control.ROOT / '.env.example').read_text())
                with self.assertRaisesRegex(ValueError, 'oci_runtime must be exactly runc or runsc'):
                    control.initialize(root, oci_runtime=value)
                self.assertFalse((root / 'perf/.state/test.env').exists())

    def test_init_cli_passes_the_explicit_runtime(self):
        with patch.object(sys, 'argv', ['control.py', 'init', '--oci-runtime', 'runc']), \
                patch.object(control, 'initialize') as initialize:
            control.main()
        initialize.assert_called_once_with(control.ROOT, oci_runtime='runc')

    def test_runtime_option_is_rejected_outside_initialization(self):
        with patch.object(sys, 'argv', ['control.py', 'check', '--oci-runtime', 'runc']), \
                patch.object(control, 'Stack') as stack, self.assertRaises(SystemExit):
            control.main()
        stack.assert_not_called()

    def test_compose_command_pins_every_input_and_clears_ambient_overrides(self):
        with patch.dict('os.environ', {'COMPOSE_FILE': 'production.yml', 'POSTGRES_PASSWORD': 'secret', 'WORKER_SLOT_COUNT': '16'}):
            command, env = control.compose_command(Path('/repo'), Path('/repo/perf/.state/test.env'), ['config'])
        self.assertIn('centaeris-perf', command)
        self.assertIn('/repo/perf/compose.perf.yml', command)
        self.assertIn('/repo/perf/.state/test.env', command)
        self.assertNotIn('COMPOSE_FILE', env)
        self.assertNotIn('POSTGRES_PASSWORD', env)
        self.assertNotIn('WORKER_SLOT_COUNT', env)

    def test_validation_rejects_production_resources_and_missing_tls(self):
        valid = {
            'name': 'centaeris-perf',
            'volumes': {'data': {'name': 'centaeris-perf_data'}},
            'networks': {'default': {'name': 'centaeris-perf_default'}},
            'services': {name: {'environment': {'SSL_CERT_FILE': '/perf-certs/mock-ca.crt'},
                'volumes': [{'type': 'bind', 'source': '/repo/perf/certs/out/isolated/mock-ca.crt', 'target': '/perf-certs/mock-ca.crt', 'read_only': True}]}
                for name in ('api', 'runtime')},
        }
        control.validate_config(valid, Path('/repo'))
        for mutate in (
            lambda c: c.update(name='centaeris-workspace'),
            lambda c: c['volumes']['data'].update(name='centaeris-workspace_data'),
            lambda c: c['volumes']['data'].update(external=True),
            lambda c: c['services']['api']['environment'].clear(),
            lambda c: c['services']['api']['volumes'].clear(),
            lambda c: c['services']['api'].update(container_name='production-api'),
        ):
            config = json.loads(json.dumps(valid))
            mutate(config)
            with self.assertRaises(ValueError):
                control.validate_config(config, Path('/repo'))

    def test_switch_slots_preserves_nonworker_identity_and_refuses_busy_stack(self):
        fake = unittest.mock.Mock()
        fake.quiet.return_value = True
        fake.identities.return_value = {'api': 'a', 'runtime': 'r'}
        control.switch_slots(fake, 6)
        fake.compose.assert_called_once_with(['up', '-d', '--no-deps', '--force-recreate', 'worker'], slots=6)
        fake.verify_slots.assert_called_once_with(6)
        fake.quiet.return_value = False
        fake.compose.reset_mock()
        with self.assertRaises(RuntimeError):
            control.switch_slots(fake, 2)
        fake.compose.assert_not_called()
        fake.quiet.return_value = True
        fake.identities.side_effect = [{'api': 'a'}, {'api': 'b'}]
        with self.assertRaises(RuntimeError):
            control.switch_slots(fake, 2)

    def test_resource_parser_rejects_missing_or_invalid_samples(self):
        row = {'Name': 'centaeris-perf-api-1', 'CPUPerc': '12.5%', 'MemUsage': '20MiB / 1GiB'}
        self.assertEqual(control.parse_stats(json.dumps(row), {'centaeris-perf-api-1'})[0]['cpuPercent'], 12.5)
        for text in ('', '{}', json.dumps({**row, 'CPUPerc': '--'})):
            with self.assertRaises((ValueError, KeyError)):
                control.parse_stats(text, {'centaeris-perf-api-1'})


if __name__ == '__main__':
    unittest.main()
