"""Controller gates, private input handling, and readonly service observations."""
import asyncio
import copy
import hashlib
import io
import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch, AsyncMock
from contextlib import ExitStack

from workspace_launch import LaunchError, verify_physical_gate, private_mode_ok, read_credentials, service_checks, main, check_previous_batches, CapacityCheckedImages, DiskCapacityInsufficient, metric_factory, validate_adapter_sources, _launch_config, _metric_container
from workspace_plan import canonical_digest, CandidateBinding


class LaunchTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.candidate = CandidateBinding('a'*40, 'b'*64, 'c'*64, 'd'*64)
        self.artifacts = {}
        for name in ('runtimeServer', 'benchmarkHelper', 'serviceConfig'):
            path = self.root / name
            path.write_bytes(name.encode())
            self.artifacts[name] = {'path': str(path), 'sha256': hashlib.sha256(path.read_bytes()).hexdigest()}
        gates = []
        for name in ('coreQueryLoop', 'runtimeOutboxPg', 'linuxTaskDomains', 'benchmarkProfileApi', 'runtimeBenchmarkControl', 'benchmarkAdapterPython'):
            path = self.root / (name + '.log')
            path.write_text('python -B check.py\ntest_' + name + ' ... ok\n1 passed\n')
            gates.append({'id': name, 'sourceCommit': 'a'*40, 'exitCode': 0, 'executedTests': 1,
                'testIdentities': ['test_' + name], 'command': ['python', '-B', 'check.py'],
                'evidencePath': str(path), 'evidenceSha256': hashlib.sha256(path.read_bytes()).hexdigest()})
        self.proof = {'schema': 'centaeris.workspaceBenchmarkPhysicalGate.v1', 'candidate': self.candidate.to_dict(),
            'sourceCommit': 'a'*40, 'sourceTree': 'e'*40, 'artifacts': self.artifacts, 'gates': gates}

    def verify(self, proof=None):
        return verify_physical_gate(proof or self.proof, self.candidate, 'e'*40,
            {key: Path(value['path']) for key, value in self.artifacts.items()})

    def test_real_logs_hashes_identity_and_candidate_are_required(self):
        self.assertTrue(self.verify())
        for mutate in (
            lambda p: p['gates'].clear(),
            lambda p: p['gates'][0].update(executedTests=0),
            lambda p: p['gates'][0].update(testIdentities=['invented']),
            lambda p: p['gates'][0].update(exitCode=1),
            lambda p: p.update(sourceTree='f'*40),
            lambda p: p['candidate'].update(adapterSha256='f'*64),
            lambda p: p['gates'][0].update(passed=True),
        ):
            proof = copy.deepcopy(self.proof)
            mutate(proof)
            with self.assertRaises(LaunchError): self.verify(proof)

    def test_selected_focused_evidence_preserves_its_original_commit(self):
        proof = copy.deepcopy(self.proof)
        proof['gates'] = [proof['gates'][-1]]
        proof['gates'][0]['id'] = 'focusedAdapterCheck'
        proof['gates'][0]['sourceCommit'] = 'f' * 40
        self.assertTrue(self.verify(proof))
        self.assertEqual(proof['gates'][0]['sourceCommit'], 'f' * 40)

    def test_changed_binary_and_log_even_with_same_name_are_rejected(self):
        Path(self.artifacts['runtimeServer']['path']).write_text('changed')
        with self.assertRaises(LaunchError): self.verify()

    def test_private_modes_and_only_selected_credentials(self):
        self.assertTrue(private_mode_ok(0o600))
        self.assertFalse(private_mode_ok(0o640))
        path = self.root / 'private.env'
        path.write_text('UNRELATED_PASSWORD=not-returned\nINTERNAL_API_TOKEN=internal-test\nENVIRONMENT_MAINTENANCE_TOKEN=maintenance-test\n')
        with patch('workspace_launch.require_private_file'):
            self.assertEqual(read_credentials(path), ('internal-test', 'maintenance-test'))
        path.write_text('INTERNAL_API_TOKEN=one\nINTERNAL_API_TOKEN=two\nENVIRONMENT_MAINTENANCE_TOKEN=three\n')
        with patch('workspace_launch.require_private_file'), self.assertRaises(LaunchError): read_credentials(path)

    def test_caller_cannot_bind_only_an_unrelated_file_as_the_adapter(self):
        with self.assertRaises(LaunchError): validate_adapter_sources({'unrelated': str(self.root/'unrelated.py')})

    def test_cli_errors_never_echo_private_arguments_or_exception_values(self):
        output = io.StringIO()
        with patch('sys.stderr', output):
            self.assertEqual(main(['--unknown-password', 'never-print-this-secret']), 2)
        self.assertNotIn('never-print-this-secret', output.getvalue())
        def fail(coroutine):
            coroutine.close()
            raise RuntimeError('private-secret')
        with patch('workspace_launch.asyncio.run', side_effect=fail), patch('sys.stderr', output):
            self.assertEqual(main(['--config', 'private.json', '--stage', 'preflight']), 1)
        self.assertNotIn('private-secret', output.getvalue())

    def test_previous_unknown_pending_or_same_stage_never_starts_fresh_trial(self):
        batch = self.root / 'preflight-first'
        batch.mkdir()
        with self.assertRaises(LaunchError): check_previous_batches(self.root, 'pilot')
        report = {'stage': 'preflight', 'status': 'finished', 'results': [{'cleanupState': 'absent', 'resourceDebit': 0,
            'infrastructureError': None, 'severeIssue': None}]}
        (batch/'batch-result.json').write_text(json.dumps(report))
        check_previous_batches(self.root, 'pilot')
        with self.assertRaises(LaunchError): check_previous_batches(self.root, 'preflight')
        report['results'][0]['cleanupState'] = 'pending'
        (batch/'batch-result.json').write_text(json.dumps(report))
        with self.assertRaises(LaunchError): check_previous_batches(self.root, 'pilot')


class ServiceTests(unittest.IsolatedAsyncioTestCase):
    async def test_metric_discovery_uses_actual_bytes_and_exact_project_service_image(self):
        identity = {'dockerSocket': '/var/run/docker.sock', 'fixtureId': 'tbworkspace0123456789'}
        manifest = {'project': identity['fixtureId'], 'images': {'runtime': {'id': 'sha256:expected'}}}
        container = 'a'*64
        inspected = {'id': container, 'image': 'sha256:expected', 'running': True, 'project': manifest['project'], 'service': 'runtime'}
        with patch('workspace_launch._docker_capture', new=AsyncMock(side_effect=[(container+'\n').encode(), json.dumps(inspected).encode()])):
            self.assertEqual(await _metric_container(identity, manifest, 'runtime'), container)
        inspected['project'] = 'unrelated'
        with patch('workspace_launch._docker_capture', new=AsyncMock(side_effect=[(container+'\n').encode(), json.dumps(inspected).encode()])):
            with self.assertRaises(LaunchError): await _metric_container(identity, manifest, 'runtime')

    async def test_gate_precedes_login_then_valid_bootstrap_constructs_actual_private_ports(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            identity = {'schema': 'centaeris.workspaceBenchmarkFixture.v1', 'fixtureId': 'tbworkspace0123456789',
                'databaseName': 'tbworkspace0123456789', 'databaseUser': 'tbworkspace0123456789',
                'sourceRevision': 'a'*40, 'sourceTree': 'e'*40, 'dockerHostId': 'host', 'dockerSocket': '/var/run/docker.sock'}
            seed = {'schema': 'centaeris.workspaceBenchmarkSeed.v1', 'fixtureId': identity['fixtureId'],
                'sourceRevision': 'a'*40, 'email': 'fixture@example.test', 'password': 'not-a-real-password',
                'workspaceId': 'workspace', 'agentId': 'agent', 'modelConfigRef': 'model',
                'model': {'id': 'example/reasoner'}, 'modelSha256': canonical_digest({'id': 'example/reasoner'})}
            manifest = {'project': identity['fixtureId'], 'dockerSocket': identity['dockerSocket'], 'dockerHostId': 'host',
                'benchmarkMode': True, 'sourceRevision': 'a'*40, 'source': str(root), 'binaryDirectory': str(root),
                'binarySha256': {'runtime_server': 'hash'}, 'buildManifest': {'sourceTree': 'e'*40},
                'benchmarkHelper': {'path': str(root/'helper'), 'sha256': 'hash', 'sourceRevision': 'a'*40, 'sourceTree': 'e'*40},
                'apiPort': 12345, 'runtimePort': 12346}
            for name, value in [('benchmark-fixture.json', identity), ('manifest.json', manifest), ('seed.json', seed)]:
                (root/name).write_text(json.dumps(value))
            candidate = CandidateBinding('a'*40, 'b'*64, 'c'*64, 'd'*64)
            config = {'fixtureRoot': str(root), 'seedPath': str(root/'seed.json'), 'planPath': str(root/'plan'),
                'datasetRoot': str(root), 'candidate': candidate.to_dict(), 'adapterSources': {}, 'dockerSocket': identity['dockerSocket'],
                'physicalGatePath': str(root/'gate'), 'outputRoot': str(root/'jobs'), 'imageLockPath': str(root/'images.json'),
                'apiOrigin': 'http://127.0.0.1:12345', 'runtimeOrigin': 'http://127.0.0.1:12346',
                'resources': {'total': {'memoryMiB': 4, 'cpus': 1, 'storageMiB': 4}, 'reserve': {'memoryMiB': 1, 'cpus': 0.25, 'storageMiB': 1}}}
            (root/'gate').write_text('{}')
            with ExitStack() as stack:
                stack.enter_context(patch('workspace_launch.require_private_file'))
                stack.enter_context(patch('workspace_launch.load_plan', return_value={'taskIds': ['task'], 'model': {'id': 'example/reasoner'}}))
                stack.enter_context(patch('workspace_launch.load_task_resources', return_value={}))
                stack.enter_context(patch('workspace_launch.validate_adapter_sources', return_value={}))
                stack.enter_context(patch('workspace_launch.make_binding', return_value=candidate))
                stack.enter_context(patch('workspace_launch._git', side_effect=lambda source, revision: 'e'*40 if revision.endswith('{tree}') else 'a'*40))
                stack.enter_context(patch('workspace_launch.subprocess.run', return_value=SimpleNamespace(returncode=0)))
                stack.enter_context(patch('workspace_launch._sha', return_value='hash'))
                gate = stack.enter_context(patch('workspace_launch.verify_physical_gate', side_effect=LaunchError('physical_gate_log_changed')))
                client = stack.enter_context(patch('workspace_launch.WorkspaceClient'))
                runner = stack.enter_context(patch('workspace_launch.BenchmarkRunner'))
                with self.assertRaises(LaunchError): await _launch_config(config, 'preflight')
                client.assert_not_called()
                runner.assert_not_called()
                gate.side_effect = None
                gate.return_value = True
                stack.enter_context(patch('workspace_launch.os.sysconf', return_value=1024**3, create=True))
                stack.enter_context(patch('workspace_launch.read_credentials', return_value=('a'*64, 'b'*64)))
                stack.enter_context(patch('workspace_launch.metric_factory', new=AsyncMock(return_value=None)))
                runner.return_value.run_preflight = AsyncMock(return_value={'status': 'passed'})
                result = await _launch_config(config, 'preflight')
                self.assertEqual(result['status'], 'passed')
                self.assertEqual(runner.call_args.kwargs['model_ref'], 'model')
                client.assert_not_called()
                approval_path = root / 'direct-pilot-approval.json'
                approval_path.write_text(json.dumps({'schema': 'centaeris.workspaceBenchmarkPilotApproval.v1'}))
                runner.return_value.run_pilot = AsyncMock(return_value={'status': 'finished'})
                result = await _launch_config(config, 'pilot', approval=approval_path)
                self.assertEqual(result['status'], 'finished')
                runner.return_value.run_pilot.assert_awaited_once_with(approval=json.loads(approval_path.read_text()))
                full_approval_path = root / 'direct-full-approval.json'
                full_approval_path.write_text(json.dumps({'schema': 'centaeris.workspaceBenchmarkFullApproval.v1'}))
                runner.return_value.run_full = AsyncMock(return_value={'status': 'finished'})
                result = await _launch_config(config, 'full', approval=full_approval_path)
                self.assertEqual(result['status'], 'finished')
                runner.return_value.run_full.assert_awaited_once_with(approval=json.loads(full_approval_path.read_text()))
                for changed in ({'modelSha256': '0'*64},
                                {'model': {'id': 'another/reasoner'},
                                 'modelSha256': canonical_digest({'id': 'another/reasoner'})}):
                    mismatched = dict(seed, **changed)
                    (root/'seed.json').write_text(json.dumps(mismatched))
                    calls = runner.call_count
                    with self.assertRaisesRegex(LaunchError, 'seed_model_mismatch'):
                        await _launch_config(config, 'full', approval=full_approval_path)
                    self.assertEqual(runner.call_count, calls)
                    client.assert_not_called()

    async def test_capacity_rechecked_each_prepare_and_does_not_delete_or_repull(self):
        calls = []
        class Images:
            async def prepare(self, *args): calls.append(args); return {'imageDigest': 'owned'}
            def snapshot_locks(self): return {'tasks': {}}
        wrapper = CapacityCheckedImages(Images(), Path('.'), 1024, lambda: 2048)
        with patch('workspace_launch.shutil.disk_usage', return_value=SimpleNamespace(free=4096*1024**2)):
            self.assertEqual(await wrapper.prepare('task', 'log'), {'imageDigest': 'owned'})
        with patch('workspace_launch.shutil.disk_usage', return_value=SimpleNamespace(free=3000*1024**2)):
            with self.assertRaises(DiskCapacityInsufficient) as caught: await wrapper.prepare('task', 'log')
        self.assertEqual(caught.exception.diagnostic(), {'freeBytes': 3000*1024**2, 'requiredBytes': 3072*1024**2})
        self.assertEqual(len(calls), 1)

    async def test_missing_optional_metric_backend_does_not_fail_task(self):
        with patch('workspace_launch._metric_container', side_effect=RuntimeError('unavailable')):
            self.assertIsNone(await metric_factory({'dockerSocket': '/var/run/docker.sock'}, {}, Path('.')))

    async def test_checks_observe_exact_instance_download_real_bytes_and_background(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            body = b'actual server bytes'
            digest = hashlib.sha256(body).hexdigest()
            owned = SimpleNamespace(session_id='session', environment_id='env', agent_run_id='run',
                instance_id='instance', container_id='container', image_digest='image', profile_digest='profile')
            class Env:
                trial_paths = SimpleNamespace(artifacts_dir=root)
                provisioner = SimpleNamespace(observe_setup=lambda *args: owned)
                def _require_owned(self): return owned
                async def exec(self, command, **kwargs):
                    self.command = command
                    return SimpleNamespace(return_code=0, stdout=json.dumps({'files': {'/app/server.py': digest,
                        '/app/kv-store.proto': digest, '/app/kv_store_pb2.py': digest, '/app/kv_store_pb2_grpc.py': digest},
                        'packages': {'grpcio': '1.73.0', 'grpcio-tools': '1.73.0'}, 'port5328': True}))
                async def download_file(self, source, target):
                    self.downloaded = source
                    Path(target).write_bytes(body)
            env = Env()
            checks = await service_checks(None, env)
            self.assertEqual(checks, {'actualToolEffects': True, 'artifactDownload': True, 'backgroundContinuity': True})
            self.assertEqual(env.downloaded, '/app/server.py')
            self.assertTrue((root/'service-check.json').is_file())
            self.assertIn('socket.create_connection', env.command)

    async def test_changed_instance_is_rejected_before_command(self):
        owned = SimpleNamespace(session_id='s', environment_id='e', agent_run_id='r', instance_id='one',
            container_id='c', image_digest='i', profile_digest='p')
        env = SimpleNamespace(_require_owned=lambda: owned, provisioner=SimpleNamespace(observe_setup=lambda *a: None))
        with self.assertRaises(LaunchError): await service_checks(None, env)


if __name__ == '__main__': unittest.main()
