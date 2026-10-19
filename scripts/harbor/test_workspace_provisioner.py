"""Controller HTTP and bounded transfer regressions; no external services."""
import io
import json
from pathlib import Path
import socket
import tarfile
import tempfile
import threading
import unittest
from unittest.mock import patch
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from workspace_provisioner import WorkspaceHTTPProvisioner, pack_upload, unpack_download
from workspace_client import WorkspaceError
from workspace_environment import OwnedEnvironment


class ArchiveTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)

    def archive(self, members):
        path = self.root / "input.tar"
        with tarfile.open(path, "w") as archive:
            for name, kind, data in members:
                info = tarfile.TarInfo(name)
                info.type = kind
                if kind == tarfile.REGTYPE:
                    info.size = len(data)
                    archive.addfile(info, io.BytesIO(data))
                else:
                    info.linkname = data.decode()
                    archive.addfile(info)
        return path

    def test_directory_roundtrip_preserves_file_bytes_and_nested_paths(self):
        source = self.root / "source"
        (source / "nested").mkdir(parents=True)
        (source / "nested" / "file").write_bytes(b"original\x00bytes")
        archive = self.root / "payload.tar"
        pack_upload(source, archive, directory=True)
        target = self.root / "destination"
        unpack_download(archive, target, directory=True)
        self.assertEqual((target / "nested" / "file").read_bytes(), b"original\x00bytes")

    def test_file_roundtrip_does_not_take_remote_basename_as_host_path(self):
        source = self.root / "file"
        source.write_bytes(b"data")
        archive = self.root / "payload.tar"
        pack_upload(source, archive, directory=False)
        target = self.root / "different-name"
        unpack_download(archive, target, directory=False)
        self.assertEqual(target.read_bytes(), b"data")

    def test_traversal_absolute_symlink_and_hardlink_are_rejected_before_writes(self):
        for name, kind, body in [("../escape", tarfile.REGTYPE, b"x"),
                                 ("/absolute", tarfile.REGTYPE, b"x"),
                                 ("link", tarfile.SYMTYPE, b"../escape"),
                                 ("link", tarfile.LNKTYPE, b"file"),
                                 ("dir\\escape", tarfile.REGTYPE, b"x"),
                                 ("dir/file:stream", tarfile.REGTYPE, b"x"),
                                 ("dir/NUL", tarfile.REGTYPE, b"x")]:
            with self.subTest(name=name, kind=kind):
                archive = self.archive([("safe", tarfile.REGTYPE, b"safe"), (name, kind, body)])
                with self.assertRaises(WorkspaceError):
                    unpack_download(archive, self.root / "destination", directory=True)
                self.assertFalse((self.root / "destination").exists())

    def test_member_count_expanded_bytes_and_encoded_bytes_are_bounded(self):
        archive = self.archive([("a", tarfile.REGTYPE, b"1234"), ("b", tarfile.REGTYPE, b"5678")])
        for options in ({"max_members": 1}, {"max_expanded_bytes": 7}, {"max_archive_bytes": 1024}):
            with self.subTest(options=options), self.assertRaises(WorkspaceError):
                unpack_download(archive, self.root / "destination", directory=True, **options)
        self.assertFalse((self.root / "destination").exists())

    def test_duplicate_member_and_file_parent_collision_rejected(self):
        for members in ([('a', tarfile.REGTYPE, b'1'), ('a', tarfile.REGTYPE, b'2')],
                        [('a', tarfile.REGTYPE, b'1'), ('a/b', tarfile.REGTYPE, b'2')]):
            with self.subTest(members=members), self.assertRaises(WorkspaceError):
                unpack_download(self.archive(members), self.root / "destination", directory=True)

    def test_existing_destination_symlink_cannot_redirect_extraction(self):
        source = self.root / "source"
        source.mkdir()
        (source / "file").write_bytes(b"content")
        archive = self.root / "payload.tar"
        pack_upload(source, archive, directory=True)
        destination = self.root / "destination"
        destination.mkdir()
        outside = self.root / "outside"
        outside.mkdir()
        try:
            (destination / "file").symlink_to(outside / "written")
        except OSError:
            self.skipTest("host does not allow symlink creation")
        with self.assertRaises(WorkspaceError):
            unpack_download(archive, destination, directory=True)
        self.assertFalse((outside / "written").exists())


class HTTPProvisionerTests(unittest.TestCase):
    def setUp(self):
        import hashlib
        self.profile = {'schema': 'runtime.environment_profile.v1', 'imageDigest': 'sha256:' + 'a' * 64,
            'workspaceRoot': '/app', 'user': '0:0', 'network': 'disabled', 'resources': {
                'cpuMilli': 1000, 'memoryBytes': 2147483648, 'pidsLimit': 256,
                'dataTmpfsBytes': 1073741824, 'storageBytes': 10737418240}}
        self.profile_digest = 'sha256:' + hashlib.sha256(json.dumps(self.profile, sort_keys=True,
            separators=(',', ':')).encode()).hexdigest()
        self.requests, self.request_bodies = [], []
        self.observation_epochs, self.observation_errors, self.retirement_errors = [], [], []
        self.lose_retirement_ack = False
        self.state, self.debit, self.response_transform = 'resident', 1, None
        owner = self
        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass
            def do_POST(self):
                raw = self.rfile.read(int(self.headers['Content-Length']))
                value = json.loads(raw)
                owner.requests.append((self.path, dict(self.headers), value))
                owner.request_bodies.append((self.path, raw))
                errors = owner.observation_errors if self.path.endswith('/observe') else owner.retirement_errors
                if errors:
                    status, code = errors.pop(0)
                    body = json.dumps({'error': code}).encode()
                    self.send_response(status)
                elif self.path.endswith('/observe'):
                    result = {'schema': 'runtime.environment.observe.result.v1', 'sessionId': 'session',
                        'environmentId': 'environment', 'profile': owner.profile, 'profileDigest': owner.profile_digest,
                        'state': owner.state, 'controllerOwner': None,
                        'controllerEpoch': owner.observation_epochs.pop(0) if owner.observation_epochs else 2,
                        'instanceId': 'instance', 'containerId': 'container', 'hostId': 'host', 'resourceHolding': owner.debit}
                    if owner.response_transform:
                        result = owner.response_transform(result)
                    body = json.dumps(result).encode()
                    self.send_response(200)
                elif self.path.endswith('/retire'):
                    if owner.lose_retirement_ack:
                        self.connection.shutdown(socket.SHUT_RDWR)
                        self.connection.close()
                        return
                    body = json.dumps({'schema': 'runtime.environment.retire.result.v1',
                        'operationId': value['operationId'], 'environmentId': 'environment',
                        'instanceId': 'instance', 'status': 'pending'}).encode()
                    self.send_response(200)
                else:
                    self.send_error(404)
                    return
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
            maintenance_token='fake-maintenance-' + 'm' * 32, internal_token='fake-internal-' + 'i' * 32)
        self.owned = OwnedEnvironment('session', 'environment', 'run', 'instance', 'container', 'host', 2,
            self.profile['imageDigest'], self.profile_digest, 'resident', 1)

    def test_observation_is_run_independent_and_unknown_never_claims_absence(self):
        self.assertEqual(self.provisioner.observe_setup('session', 'environment', 'run'), self.owned)
        self.assertEqual(self.requests[0][2], {'schema': 'runtime.environment.observe.v1',
            'sessionId': 'session', 'environmentId': 'environment'})
        self.state, self.debit = 'unknown', None
        self.assertIsNone(self.provisioner.observe_setup('session', 'environment'))
        observed = self.provisioner.observe_cleanup(self.owned)
        self.assertEqual((observed.physical_state, observed.resource_holding), ('unknown', None))

    def test_unavailable_observation_preserves_error_and_never_claims_absence(self):
        self.observation_errors = [(503, 'environment_observation_unavailable')]
        with self.assertRaisesRegex(WorkspaceError, 'environment_observation_unavailable'):
            self.provisioner.observe_cleanup(self.owned)
        self.assertEqual(len(self.requests), 1)

    def test_destroyed_requires_confirmed_debit_and_is_not_reposted(self):
        self.state, self.debit = 'destroyed', 0
        observed = self.provisioner.observe_cleanup(self.owned)
        self.assertEqual((observed.physical_state, observed.resource_holding), ('absent', 0))
        self.assertEqual(self.provisioner.retire(self.owned, 'already-destroyed'), 'destroyed')
        self.assertTrue(all(path.endswith('/observe') for path, _, _ in self.requests))

    def test_resident_partial_or_unknown_schema_never_claims_ownership(self):
        for mutation in ({'instanceId': None}, {'containerId': None}, {'hostId': None},
                {'resourceHolding': 0}, {'schema': 'unsupported'}, {'extra': True}):
            with self.subTest(mutation=mutation):
                self.response_transform = lambda value: value | mutation
                with self.assertRaises(WorkspaceError):
                    self.provisioner.observe_setup('session', 'environment')

    def test_retire_refreshes_cleanup_epoch_once_then_reuses_exact_operation_request(self):
        self.response_transform = lambda value: {**value, "controllerEpoch": 4} if value["schema"] == "runtime.environment.observe.result.v1" else value
        self.provisioner.retire(self.owned, "retire-op")
        first = next(value for path, _, value in self.requests if path == "/internal/environments/retire")
        self.assertEqual(first["expectedControllerEpoch"], 4)
        self.response_transform = lambda value: {**value, "controllerEpoch": 5} if value["schema"] == "runtime.environment.observe.result.v1" else value
        self.provisioner.retire(self.owned, "retire-op")
        self.assertEqual([value for path, _, value in self.requests if path == "/internal/environments/retire"], [first, first])

    def test_retire_reobserves_only_an_explicit_unaccepted_epoch_conflict(self):
        for conflicts in (1, 2):
            with self.subTest(conflicts=conflicts):
                self.requests.clear()
                self.observation_epochs = list(range(4, 5 + conflicts))
                self.retirement_errors = [(409, "environment_retirement_epoch_conflict")] * conflicts
                operation = "retire-op-" + str(conflicts)
                self.assertEqual(self.provisioner.retire(self.owned, operation), "pending")
                requests = [value for path, _, value in self.requests if path == "/internal/environments/retire"]
                self.assertEqual([value["expectedControllerEpoch"] for value in requests], list(range(4, 5 + conflicts)))
                self.assertEqual({value["operationId"] for value in requests}, {operation})
                unchanged = [{key: value for key, value in request.items() if key != "expectedControllerEpoch"}
                             for request in requests]
                self.assertTrue(all(value == unchanged[0] for value in unchanged))
                self.assertEqual([path for path, _, _ in self.requests],
                                 ["/internal/environments/observe", "/internal/environments/retire"] * (conflicts + 1))

    def test_retire_stops_after_three_epoch_conflicts_and_preserves_the_error(self):
        self.observation_epochs = [4, 5, 6, 7]
        self.retirement_errors = [(409, "environment_retirement_epoch_conflict")] * 3
        with self.assertRaises(WorkspaceError) as raised:
            self.provisioner.retire(self.owned, "retire-op")
        self.assertEqual((raised.exception.code, raised.exception.status),
                         ("environment_retirement_epoch_conflict", 409))
        requests = [value for path, _, value in self.requests if path == "/internal/environments/retire"]
        self.assertEqual([value["expectedControllerEpoch"] for value in requests], [4, 5, 6])
        self.assertEqual({value["operationId"] for value in requests}, {"retire-op"})
        self.assertEqual(self.provisioner.retire(self.owned, "retire-op"), "pending")
        self.assertEqual(self.requests[-1][2]["expectedControllerEpoch"], 7)

    def test_retire_lost_ack_and_pending_keep_exact_request_bytes_without_reobservation(self):
        self.observation_epochs = [4, 5]
        self.lose_retirement_ack = True
        with self.assertRaisesRegex(WorkspaceError, "transport_unavailable"):
            self.provisioner.retire(self.owned, "retire-op")
        self.assertEqual([path for path, _, _ in self.requests],
                         ["/internal/environments/observe", "/internal/environments/retire"])
        self.lose_retirement_ack = False
        self.assertEqual(self.provisioner.retire(self.owned, "retire-op"), "pending")
        self.assertEqual(self.provisioner.retire(self.owned, "retire-op"), "pending")
        bodies = [raw for path, raw in self.request_bodies if path == "/internal/environments/retire"]
        self.assertEqual(bodies, [bodies[0]] * 3)
        self.assertEqual(sum(path == "/internal/environments/observe" for path, _, _ in self.requests), 1)

    def test_retire_does_not_refresh_on_generic_conflict_or_wrong_status(self):
        for status, code in [(409, "environment_retirement_rejected"),
                             (503, "environment_retirement_epoch_conflict"),
                             (409, "environment_retirement_epoch_conflict_unknown")]:
            with self.subTest(status=status, code=code):
                self.requests.clear()
                self.request_bodies.clear()
                self.observation_epochs = [4, 5]
                self.retirement_errors = [(status, code)]
                operation = "retire-op-" + str(status) + "-" + code
                with self.assertRaises(WorkspaceError) as raised:
                    self.provisioner.retire(self.owned, operation)
                self.assertEqual((raised.exception.status, raised.exception.code), (status, code))
                self.assertEqual(self.provisioner.retire(self.owned, operation), "pending")
                self.assertEqual([path for path, _, _ in self.requests],
                                 ["/internal/environments/observe", "/internal/environments/retire", "/internal/environments/retire"])
                bodies = [raw for path, raw in self.request_bodies if path == "/internal/environments/retire"]
                self.assertEqual(bodies, [bodies[0]] * 2)

    def test_retire_pending_acceptance_never_refreshes_its_immutable_body(self):
        self.observation_epochs = [4, 5]
        self.assertEqual(self.provisioner.retire(self.owned, "retire-op"), "pending")
        self.retirement_errors = [(409, "environment_retirement_epoch_conflict")]
        with self.assertRaises(WorkspaceError):
            self.provisioner.retire(self.owned, "retire-op")
        self.assertEqual(sum(path == "/internal/environments/observe" for path, _, _ in self.requests), 1)
        bodies = [raw for path, raw in self.request_bodies if path == "/internal/environments/retire"]
        self.assertEqual(bodies, [bodies[0]] * 2)

    def test_retire_reobservation_rejects_changed_identity_before_second_post(self):
        for field, changed in [("sessionId", "another"), ("environmentId", "another"),
                               ("instanceId", "another"),
                               ("containerId", "another"), ("hostId", "another"),
                               ("profileDigest", "sha256:" + "c" * 64)]:
            with self.subTest(field=field):
                self.requests.clear()
                self.observation_epochs = [4, 5]
                self.retirement_errors = [(409, "environment_retirement_epoch_conflict")]
                def transform(value):
                    if (value["schema"] == "runtime.environment.observe.result.v1"
                            and sum(path == "/internal/environments/observe" for path, _, _ in self.requests) == 2):
                        return {**value, field: changed}
                    return value
                self.response_transform = transform
                with self.assertRaisesRegex(WorkspaceError, "retirement_binding_mismatch|environment_observation_invalid|control_response_invalid|environment_profile_binding_mismatch"):
                    self.provisioner.retire(self.owned, "retire-op-" + field)
                self.assertEqual(sum(path == "/internal/environments/retire" for path, _, _ in self.requests), 1)
