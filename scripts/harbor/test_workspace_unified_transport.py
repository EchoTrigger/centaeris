"""Real loopback HTTP exercises the common task and binary filesystem wire."""
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
import socket
import struct
import tempfile
import threading
import unittest
from unittest.mock import patch

from workspace_client import WorkspaceError
from workspace_environment import OwnedEnvironment
from workspace_provisioner import WorkspaceHTTPProvisioner


def digest(value):
    return 'sha256:' + hashlib.sha256(json.dumps(value, ensure_ascii=False, sort_keys=True,
        separators=(',', ':')).encode()).hexdigest()


class UnifiedTransportTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.content = self.root / 'remote-file'
        self.profile = {'schema': 'runtime.environment_profile.v1', 'imageDigest': 'sha256:' + 'a' * 64,
            'workspaceRoot': '/app', 'user': '0:0', 'network': 'publicInternet', 'resources': {
                'cpuMilli': 1000, 'memoryBytes': 2147483648, 'pidsLimit': 256,
                'workspaceBytes': 1073741824, 'storageBytes': 10737418240}}
        self.requests, self.epoch = [], 0
        self.corrupt_digest = self.truncate = self.lose_start = self.invalid_result = False
        self.headers = []
        state = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_POST(self):
                state.headers.append((self.path, dict(self.headers)))
                framed = self.path.endswith('/filesystem')
                if framed:
                    size = struct.unpack('!I', self.rfile.read(4))[0]
                    value = json.loads(self.rfile.read(size))
                    metadata = value['request']
                    remaining = metadata['contentLength']
                    if metadata['request']['operation']['type'] == 'writeFile':
                        hasher = hashlib.sha256()
                        with state.content.open('wb') as output:
                            while remaining:
                                block = self.rfile.read(min(65536, remaining))
                                if not block:
                                    raise AssertionError('incomplete upload')
                                output.write(block)
                                hasher.update(block)
                                remaining -= len(block)
                        assert metadata['contentDigest'] == 'sha256:' + hasher.hexdigest()
                    state.requests.append((self.path, value))
                    operation = metadata['request']['operation']['type']
                    identity = {'key': 'file', 'displayPath': metadata['request']['modelPath']}
                    raw_length = state.content.stat().st_size if operation == 'readFile' else 0
                    raw_hash = hashlib.sha256()
                    if raw_length:
                        with state.content.open('rb') as source:
                            for block in iter(lambda: source.read(65536), b''):
                                raw_hash.update(block)
                    output = ({'read_file': {'identity': identity, 'bytes': [], 'fileHash': 'sha256:' + raw_hash.hexdigest()}}
                        if operation == 'readFile' else {'write_file': {'identity': identity, 'created': True}})
                    response = {'result': {'output': output, 'error': None}, 'contentLength': raw_length,
                        'contentDigest': 'sha256:' + ('f' * 64 if state.corrupt_digest else raw_hash.hexdigest())}
                    if state.invalid_result:
                        response['result']['error'] = {'code': 'io', 'message': 'failed'}
                    header = json.dumps(response).encode()
                    self.send_response(200)
                    self.send_header('Content-Type', 'application/octet-stream')
                    self.send_header('Content-Length', str(4 + len(header) + raw_length))
                    self.end_headers()
                    self.wfile.write(struct.pack('!I', len(header)) + header)
                    if raw_length:
                        remaining = raw_length - (1 if state.truncate else 0)
                        with state.content.open('rb') as source:
                            while remaining:
                                block = source.read(min(65536, remaining))
                                self.wfile.write(block)
                                remaining -= len(block)
                    self.close_connection = True
                    return
                value = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
                state.requests.append((self.path, value))
                if self.path.endswith('/maintenance-authorizations'):
                    state.epoch += 1
                    result = {'payload': {'schema': 'workspace.environment_maintenance_authorization.v1',
                        'operationKind': 'task', 'environmentId': 'environment', 'sessionId': 'session',
                        'instanceId': 'instance', 'containerId': 'container', 'profileDigest': digest(state.profile),
                        'controllerOwner': {'kind': 'maintenanceOperation', 'operationId': value['operationId']},
                        'controllerEpoch': state.epoch, 'controllerLeaseDigest': 'sha256:' + 'd' * 64,
                        'controllerPurpose': 'execute', 'requestDigest': digest(value['request'])}, 'signature': 'signed'}
                elif self.path.endswith('/release'):
                    result = {'environmentId': 'environment', 'controllerOwner': {'kind': 'maintenanceOperation',
                        'operationId': self.path.split('/')[-2]}, 'controllerEpoch': value['controllerEpoch'],
                        'controllerLeaseDigest': value['controllerLeaseDigest'], 'released': True}
                elif self.path.endswith('/observe'):
                    result = {'schema': 'runtime.environment.observe.result.v1', 'environmentId': 'environment',
                        'sessionId': 'session', 'profile': state.profile, 'profileDigest': digest(state.profile),
                        'state': 'resident', 'controllerEpoch': state.epoch, 'controllerOwner': None,
                        'instanceId': 'instance', 'containerId': 'container', 'hostId': 'host', 'resourceHolding': 1}
                elif self.path.endswith('/task'):
                    request = value['task']['request']
                    kind = request['kind']
                    task = {'taskId': 'task-one', 'operationId': str(value['authorization']['payload']['controllerEpoch']) + ':execute', 'scope': {'kind': 'operation',
                        'operationId': value['authorization']['payload']['controllerOwner']['operationId']},
                        'state': 'exited', 'exitCode': 0, 'cleanupComplete': True, 'outputComplete': True}
                    if kind == 'start' and state.lose_start:
                        self.connection.shutdown(socket.SHUT_RDWR)
                        self.connection.close()
                        return
                    result = {'schema': 'environment.task.v1', 'status': 'success', 'result':
                        {'kind': 'takenOver', 'fence': {key: request[key] for key in ('environmentInstanceId',
                            'controllerOwner', 'controllerEpoch', 'controllerPurpose')}} if kind == 'takeover' else
                        {'kind': 'mutated', 'task': task} if kind == 'start' else
                        {'kind': 'task', 'task': task, 'output': [{'cursor': 0, 'stream': 'stdout', 'data': [111, 107]}],
                            'earliestCursor': 0, 'nextCursor': 1, 'gap': False, 'hasMore': False} if kind == 'query' else
                        {'kind': 'operationEnded', 'taskIds': [], 'cleanupComplete': True}}
                else:
                    self.send_error(404)
                    return
                body = json.dumps(result).encode()
                self.send_response(200)
                self.send_header('Content-Type', 'application/json')
                self.send_header('Content-Length', str(len(body)))
                self.end_headers()
                self.wfile.write(body)

        server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        origin = f'http://127.0.0.1:{server.server_port}'
        self.provisioner = WorkspaceHTTPProvisioner(api_origin=origin, runtime_origin=origin,
            maintenance_token='fixture-maintenance-' + 'm' * 32, internal_token='fixture-internal-' + 'i' * 32)
        self.owned = OwnedEnvironment('session', 'environment', None, 'instance', 'container', 'host', 0,
            self.profile['imageDigest'], digest(self.profile), 'resident', 1)

    def test_large_binary_roundtrip_uses_bound_metadata_and_streams_without_whole_file_reads(self):
        source = self.root / 'verifier-package'
        source.write_bytes(b'x' * 11_506_423)
        target = self.root / 'download'
        with patch.object(Path, 'read_bytes', side_effect=AssertionError('whole file read')):
            self.provisioner.upload_file(self.owned, source, '/tests/private.tar.gz')
            self.provisioner.download_file(self.owned, '/tests/private.tar.gz', target)
        self.assertEqual(target.stat().st_size, source.stat().st_size)
        with source.open('rb') as left, target.open('rb') as right:
            self.assertEqual(hashlib.file_digest(left, 'sha256').digest(), hashlib.file_digest(right, 'sha256').digest())
        frame = next(value for path, value in self.requests if path.endswith('/filesystem'))
        self.assertEqual(frame['request']['request']['operation']['content'], [])
        grant_request = next(value['request'] for path, value in self.requests if path.endswith('/maintenance-authorizations'))
        self.assertEqual(frame['request'], grant_request)
        self.assertEqual(frame['request']['contentLength'], 11_506_423)

    def test_bad_response_digest_or_truncated_body_never_publishes_an_artifact(self):
        self.content.write_bytes(b'content')
        for attribute in ('corrupt_digest', 'truncate', 'invalid_result'):
            with self.subTest(attribute=attribute):
                setattr(self, attribute, True)
                target = self.root / attribute
                count = sum(path.endswith('/filesystem') for path, _ in self.requests)
                with self.assertRaises(WorkspaceError):
                    self.provisioner.download_file(self.owned, '/logs/verifier/reward.txt', target)
                self.assertFalse(target.exists())
                self.assertEqual(sum(path.endswith('/filesystem') for path, _ in self.requests), count + 1)
                setattr(self, attribute, False)

    def test_command_uses_one_owned_task_and_closes_before_releasing_authority(self):
        self.assertEqual(self.provisioner.exec(self.owned, command='printf ok', cwd='/app', env={}, user='0:0',
            timeout_sec=5), {'returnCode': 0, 'stdout': 'ok', 'stderr': ''})
        commands = [value['task']['request'] for path, value in self.requests if path.endswith('/task')]
        self.assertEqual([value['kind'] for value in commands], ['takeover', 'start', 'query', 'endOperation'])
        self.assertEqual(commands[1]['scope']['kind'], 'operation')
        self.assertTrue(self.requests[-1][0].endswith('/release'))

    def test_lost_start_ack_never_dispatches_a_second_start(self):
        self.lose_start = True
        with self.assertRaises(WorkspaceError):
            self.provisioner.exec(self.owned, command='external-effect', timeout_sec=5)
        starts = [value for path, value in self.requests if path.endswith('/task')
            and value['task']['request']['kind'] == 'start']
        self.assertEqual(len(starts), 1)

    def test_upload_cap_is_checked_before_any_remote_mutation(self):
        source = self.root / 'oversized'
        with source.open('wb') as stream:
            stream.truncate(32 * 1024 * 1024 + 1)
        with self.assertRaisesRegex(WorkspaceError, 'transfer_expanded_limit_exceeded'):
            self.provisioner.upload_file(self.owned, source, '/tests/large')
        self.assertEqual(self.requests, [])

    def test_api_never_receives_runtime_internal_credentials(self):
        self.provisioner.exec(self.owned, command='true', timeout_sec=1)
        for path, headers in self.headers:
            self.assertEqual(headers.get('X-Environment-Maintenance-Token'), 'fixture-maintenance-' + 'm' * 32)
            if path.endswith('/maintenance-authorizations') or path.endswith('/release'):
                self.assertNotIn('X-Internal-Token', headers)
            else:
                self.assertEqual(headers.get('X-Internal-Token'), 'fixture-internal-' + 'i' * 32)


class DirectoryTransferTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.provisioner = WorkspaceHTTPProvisioner(api_origin='https://unused.invalid', runtime_origin='https://unused.invalid',
            maintenance_token='maintenance-' + 'm' * 32, internal_token='internal-' + 'i' * 32)
        self.owned = OwnedEnvironment('session', 'environment', None, 'instance', 'container', 'host', 0,
            'sha256:' + 'a' * 64, 'sha256:' + 'b' * 64, 'resident', 1)
        self.provisioner._profiles['environment'] = {'workspaceRoot': '/app'}

    def test_core_display_paths_are_resolved_from_cwd_then_stripped_against_requested_root(self):
        for source, path in (('/app/logs', 'logs/nested/reward.txt'), ('/logs', '/logs/nested/reward.txt')):
            with self.subTest(source=source):
                target = self.root / source.strip('/').replace('/', '-')
                output = {'list_directory': {'entries': [{'path': path, 'kind': 'file', 'sizeBytes': 1}]}}
                def download(owned, remote, local):
                    self.assertEqual(remote, source + '/nested/reward.txt')
                    local.parent.mkdir(parents=True, exist_ok=True)
                    local.write_bytes(b'1')
                with patch.object(self.provisioner, '_filesystem', return_value=output), \
                        patch.object(self.provisioner, 'download_file', side_effect=download):
                    self.provisioner.download_dir(self.owned, source, target)
                self.assertEqual((target / 'nested/reward.txt').read_bytes(), b'1')
                self.assertFalse((target / 'logs').exists())

    def test_bad_directory_paths_and_collisions_are_rejected_before_artifact_writes(self):
        for paths in (['logs/safe', '../escape'], ['logs/safe', 'outside/file'], ['logs/a', 'logs/a'],
                ['logs/a', 'logs/a/b'], ['logs/safe', 'logs/NUL']):
            with self.subTest(paths=paths):
                target = self.root / 'destination'
                output = {'list_directory': {'entries': [{'path': path, 'kind': 'file', 'sizeBytes': 1} for path in paths]}}
                with patch.object(self.provisioner, '_filesystem', return_value=output), \
                        patch.object(self.provisioner, 'download_file') as download:
                    with self.assertRaises(WorkspaceError):
                        self.provisioner.download_dir(self.owned, '/app/logs', target)
                download.assert_not_called()
                self.assertFalse(target.exists())
