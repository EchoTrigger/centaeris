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
        self.requests = []
        self.lose_ack = False
        self.lose_activation_ack = False
        self.state = "waitingSetup"
        self.physical = "present"
        self.debit = 1
        self.uploaded = None
        self.downloaded_archive = None
        self.response_transform = None
        self.observation_errors = []
        self.download_error = None
        self.download_content_type = "application/x-tar"
        self.download_declared_bytes = None
        owner = self
        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass
            def do_POST(self):
                raw = self.rfile.read(int(self.headers["Content-Length"]))
                value = (json.loads(self.headers["X-Benchmark-Transfer"]) if self.path == "/internal/benchmark/upload"
                         and self.headers.get("Content-Type") == "application/x-tar" else json.loads(raw))
                owner.requests.append((self.path, dict(self.headers), value))
                if self.path == "/internal/benchmark/download" and owner.download_error:
                    status, code = owner.download_error
                    body = json.dumps({"error": code}).encode()
                    self.send_response(status)
                    self.send_header("Content-Length", str(len(body)))
                    self.end_headers()
                    self.wfile.write(body)
                    return
                if self.path == "/internal/benchmark/observe":
                    if owner.observation_errors:
                        status, code = owner.observation_errors.pop(0)
                        body = json.dumps({"error": code}).encode()
                        self.send_response(status)
                        self.send_header("Content-Type", "application/json")
                        self.send_header("Content-Length", str(len(body)))
                        self.end_headers()
                        self.wfile.write(body)
                        return
                    result = {"schema": "runtime.benchmark.observe.result.v1", "state": owner.state,
                        "sessionId": "session", "environmentId": "environment", "agentRunId": "run",
                        "instanceId": "instance", "containerId": "container", "hostId": "host", "controllerEpoch": 2,
                        "imageDigest": "sha256:" + "a" * 64, "profileDigest": "sha256:" + "b" * 64,
                        "physicalState": owner.physical, "resourceHolding": owner.debit}
                elif self.path == "/internal/benchmark/exec":
                    if owner.lose_ack:
                        self.connection.shutdown(socket.SHUT_RDWR)
                        self.connection.close()
                        return
                    result = {"schema": "runtime.benchmark.exec.result.v1", "exitCode": 0, "stdout": "output",
                              "stderr": "", "timedOut": False, "outcome": "completed"}
                elif self.path == "/internal/environments/retire":
                    result = {"schema": "runtime.environment.retire.result.v1", "operationId": value["operationId"],
                              "environmentId": "environment", "instanceId": "instance", "status": "pending"}
                elif self.path.endswith("/benchmark-setup-complete"):
                    if owner.lose_activation_ack:
                        owner.state = "ready"
                        self.connection.shutdown(socket.SHUT_RDWR)
                        self.connection.close()
                        return
                    result = {**value, "schema": "workspace.benchmark_setup.completed.v1", "environmentId": "environment"}
                elif self.path == "/internal/benchmark/upload":
                    owner.uploaded = raw
                    result = {"schema": "runtime.benchmark.upload.result.v1", "uploadedBytes": len(owner.uploaded)}
                elif self.path == "/internal/benchmark/download":
                    body = owner.downloaded_archive or owner.uploaded
                    self.send_response(200)
                    self.send_header("Content-Type", owner.download_content_type)
                    self.send_header("Content-Length", str(owner.download_declared_bytes or len(body)))
                    self.end_headers()
                    self.wfile.write(body)
                    return
                else:
                    import hashlib
                    digest = "sha256:" + hashlib.sha256(json.dumps(value["profile"], sort_keys=True,
                        ensure_ascii=False, separators=(",", ":")).encode()).hexdigest()
                    result = {"schema": "workspace.benchmark_profile.bound.v1", "operationId": value["operationId"],
                        "sessionId": value["sessionId"], "environmentId": "environment", "profileDigest": digest,
                        "profile": value["profile"]}
                if owner.response_transform:
                    result = owner.response_transform(result)
                body = json.dumps(result).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)
        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.addCleanup(self.server.server_close)
        self.addCleanup(self.server.shutdown)
        origin = f"http://127.0.0.1:{self.server.server_port}"
        self.provisioner = WorkspaceHTTPProvisioner(api_origin=origin, runtime_origin=origin,
            maintenance_token="fake-maintenance-" + "m" * 32, internal_token="fake-internal-" + "i" * 32)
        self.owned = OwnedEnvironment("session", "environment", "run", "instance", "container", "host", 2,
                                      "sha256:" + "a" * 64, "sha256:" + "b" * 64, "waitingSetup", 1)

    def test_large_verifier_payload_roundtrips_as_bounded_raw_tar(self):
        import hashlib
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "private_test.tar.gz"
            source.write_bytes(b"x" * 11_506_423)
            expected = hashlib.sha256(source.read_bytes()).hexdigest()
            # Archive transfer must stream its file; reading the entire tar in
            # Python would add a second archive-sized allocation.
            with patch.object(Path, "read_bytes", side_effect=AssertionError("whole-file read")):
                self.provisioner.upload_file(self.owned, source, "/tests/private_test.tar.gz")
                self.provisioner.download_file(self.owned, "/tests/private_test.tar.gz", root / "download")
            self.assertEqual(hashlib.sha256((root / "download").read_bytes()).hexdigest(), expected)
            upload = next(item for item in self.requests if item[0] == "/internal/benchmark/upload")
            self.assertEqual(upload[1]["Content-Type"], "application/x-tar")
            self.assertEqual(set(upload[2]), {"schema", "sessionId", "environmentId", "agentRunId",
                "instanceId", "profileDigest", "path"})
            self.assertLess(len(upload[1]["X-Benchmark-Transfer"]), 8192)

    def test_upload_over_cap_fails_before_remote_mutations(self):
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary) / "large"
            with source.open("wb") as output:
                output.truncate(32 * 1024 * 1024 + 1)
            with self.assertRaisesRegex(WorkspaceError, "transfer_expanded_limit_exceeded"):
                self.provisioner.upload_file(self.owned, source, "/tests/large")
        self.assertEqual(self.requests, [])

    def test_download_categories_propagate_without_retries_or_artifact_writes(self):
        for status, code in [(404, 'benchmark_download_path_missing'), (413, 'benchmark_archive_limit_exceeded'),
                (409, 'benchmark_transfer_identity_rejected'), (409, 'benchmark_transfer_outcome_unknown')]:
            with self.subTest(code=code), tempfile.TemporaryDirectory() as temporary:
                self.download_error = (status, code)
                target = Path(temporary) / 'download'
                before = len(self.requests)
                with self.assertRaises(WorkspaceError) as caught:
                    self.provisioner.download_file(self.owned, '/logs/verifier/reward.txt', target)
                self.assertEqual((caught.exception.status, caught.exception.code), (status, code))
                self.assertEqual(len(self.requests) - before, 1)
                self.assertFalse(target.exists())

    def test_download_rejects_incomplete_or_over_cap_body_before_extraction(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / 'input'
            source.write_text('data')
            archive = root / 'input.tar'
            pack_upload(source, archive, directory=False)
            self.downloaded_archive = archive.read_bytes()
            for length, code in [(len(self.downloaded_archive) + 1, 'transfer_response_incomplete'),
                    (32 * 1024 * 1024 + 1, 'transfer_archive_limit_exceeded')]:
                self.download_declared_bytes = length
                with self.subTest(length=length), self.assertRaisesRegex(WorkspaceError, code):
                    self.provisioner.download_file(self.owned, '/input', root / 'download')
                self.assertFalse((root / 'download').exists())

    def test_observation_returns_owned_instance_and_unknown_is_not_ready(self):
        self.assertEqual(self.provisioner.observe_setup("session", "environment", "run"), self.owned)
        self.state, self.physical, self.debit = "unknown", "unknown", None
        self.assertIsNone(self.provisioner.observe_setup("session", "environment", "run"))
        observation = self.provisioner.observe_cleanup(self.owned)
        self.assertEqual((observation.physical_state, observation.resource_holding), ("unknown", None))

    def test_temporary_setup_observation_unavailable_waits_then_returns_same_owned_instance(self):
        self.observation_errors = [(503, "benchmark_observation_unavailable")]
        self.assertIsNone(self.provisioner.observe_setup("session", "environment", "run"))
        self.assertEqual(self.provisioner.observe_setup("session", "environment", "run"), self.owned)
        self.assertEqual([path for path, _, _ in self.requests], ["/internal/benchmark/observe"] * 2)
        self.assertEqual(self.requests[0][2], self.requests[1][2])

    def test_setup_observation_does_not_retry_conflicts_or_other_unavailable_responses(self):
        for status, code in [(409, "benchmark_observation_rejected"),
                             (409, "benchmark_observation_unavailable"),
                             (503, "other_dependency_unavailable"),
                             (403, "environment_maintenance_authority_required")]:
            with self.subTest(status=status, code=code):
                self.observation_errors = [(status, code)]
                before = len(self.requests)
                with self.assertRaises(WorkspaceError) as caught:
                    self.provisioner.observe_setup("session", "environment", "run")
                self.assertEqual((caught.exception.status, caught.exception.code), (status, code))
                self.assertEqual(len(self.requests) - before, 1)

    def test_cleanup_observation_unavailable_never_claims_absence_or_releases_resources(self):
        self.observation_errors = [(503, "benchmark_observation_unavailable")]
        with self.assertRaisesRegex(WorkspaceError, "benchmark_observation_unavailable"):
            self.provisioner.observe_cleanup(self.owned)
        self.assertEqual([path for path, _, _ in self.requests], ["/internal/benchmark/observe"])

    def test_waiting_setup_before_allocation_waits_without_claiming_ownership(self):
        self.state, self.physical, self.debit = "waitingSetup", "unknown", 0
        self.response_transform = lambda value: {**value, "instanceId": None, "containerId": None, "hostId": None}
        self.assertIsNone(self.provisioner.observe_setup("session", "environment", "run"))
        self.response_transform = None
        self.physical, self.debit = "present", 1
        self.assertEqual(self.provisioner.observe_setup("session", "environment", "run"), self.owned)
        self.assertTrue(all(path == "/internal/benchmark/observe" for path, _, _ in self.requests))

    def test_waiting_setup_partial_or_contradictory_allocation_still_rejected(self):
        self.physical, self.debit = "unknown", 0
        for present in ("instanceId", "containerId", "hostId"):
            self.response_transform = lambda value: {**value, **{key: None for key in ("instanceId", "containerId", "hostId") if key != present}}
            with self.assertRaisesRegex(WorkspaceError, "benchmark_observation_invalid"):
                self.provisioner.observe_setup("session", "environment", "run")
        self.physical, self.debit = "present", 1
        self.response_transform = lambda value: {**value, "instanceId": None, "containerId": None, "hostId": None}
        with self.assertRaisesRegex(WorkspaceError, "benchmark_observation_invalid"):
            self.provisioner.observe_setup("session", "environment", "run")

    def test_exec_uses_exact_shell_argv_and_unknown_transport_is_not_retried(self):
        result = self.provisioner.exec(self.owned, command="echo 'original'", cwd="/app", env={"TASK": "value"},
                                       timeout_sec=120, user="1000")
        self.assertEqual(result["stdout"], "output")
        self.assertEqual(self.requests[-1][2]["command"], ["/bin/bash", "-lc", "echo 'original'"])
        self.assertEqual(self.requests[-1][2]["timeoutMs"], 120000)
        self.lose_ack = True
        before = len(self.requests)
        with self.assertRaisesRegex(WorkspaceError, "transport_unavailable"):
            self.provisioner.exec(self.owned, command="write effect", cwd=None, env=None, timeout_sec=1, user=None)
        self.assertEqual(len(self.requests) - before, 1)

    def test_maintenance_and_internal_authority_are_separate_and_not_in_payload(self):
        self.provisioner.activate_setup(self.owned, "activate-op")
        self.provisioner.retire(self.owned, "retire-op")
        api_headers, runtime_headers = self.requests[0][1], self.requests[1][1]
        self.assertIn("X-Environment-Maintenance-Token", api_headers)
        self.assertNotIn("X-Internal-Token", api_headers)
        self.assertIn("X-Internal-Token", runtime_headers)
        self.assertNotIn("Cookie", runtime_headers)
        self.assertNotIn("fake-maintenance", json.dumps([request[2] for request in self.requests]))

    def test_file_upload_download_roundtrip_serializes_bounded_tar(self):
        with tempfile.TemporaryDirectory() as temporary:
            source, target = Path(temporary) / "input", Path(temporary) / "output"
            source.write_bytes(b"payload")
            self.provisioner.upload_file(self.owned, source, "/tests/renamed")
            self.assertEqual(self.requests[-1][2]["path"], "/tests")
            with tarfile.open(fileobj=io.BytesIO(self.uploaded)) as archive:
                self.assertEqual(archive.getnames(), ["renamed"])
            self.provisioner.download_file(self.owned, "/tests/renamed", target)
            self.assertEqual(target.read_bytes(), b"payload")

    def test_unknown_destination_creation_does_not_continue_or_replay_upload(self):
        self.lose_ack = True
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary) / "input"
            source.write_bytes(b"payload")
            with self.assertRaisesRegex(WorkspaceError, "transport_unavailable"):
                self.provisioner.upload_file(self.owned, source, "/tests/renamed")
        self.assertEqual([request[0] for request in self.requests], ["/internal/benchmark/exec"])
        self.assertIsNone(self.uploaded)

    def test_profile_bind_transmits_exact_immutable_body_and_checks_digest(self):
        profile = {"schema": "runtime.benchmark_execution_profile.v1", "imageDigest": "sha256:" + "a" * 64,
                   "workspaceRoot": "/app", "user": "0:0", "network": "disabled", "resources": {
                       "cpuMilli": 1000, "memoryBytes": 2048, "pidsLimit": 32, "dataTmpfsBytes": 4096, "storageBytes": 8192}}
        result = self.provisioner.bind_profile("session", "environment", profile, "bind-op")
        self.assertEqual(result["profile"], profile)
        self.assertEqual(self.requests[-1][2], {"schema": "workspace.benchmark_profile.bind.v1",
            "operationId": "bind-op", "sessionId": "session", "profile": profile})
        self.response_transform = lambda value: {**value, "profileDigest": "sha256:" + "f" * 64}
        with self.assertRaisesRegex(WorkspaceError, "benchmark_profile_binding_mismatch"):
            self.provisioner.bind_profile("session", "environment", profile, "bind-op")

    def test_directory_download_strips_only_requested_docker_archive_root(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "bundle"
            source.mkdir()
            (source / "file").write_bytes(b"data")
            archive = root / "docker.tar"
            with tarfile.open(archive, "w") as output:
                output.add(source, arcname="bundle")
            self.downloaded_archive = archive.read_bytes()
            self.provisioner.download_dir(self.owned, "/tmp/bundle", root / "target")
            self.assertEqual((root / "target" / "file").read_bytes(), b"data")
            with self.assertRaises(WorkspaceError):
                self.provisioner.download_dir(self.owned, "/tmp/different", root / "wrong")
            self.assertFalse((root / "wrong").exists())

    def test_cleanup_requires_ledger_destroyed_and_separate_absence_and_zero_debit(self):
        self.state, self.physical, self.debit = "ready", "absent", 0
        self.assertEqual(self.provisioner.observe_cleanup(self.owned).physical_state, "unknown")
        self.state = "destroyed"
        observation = self.provisioner.observe_cleanup(self.owned)
        self.assertEqual((observation.physical_state, observation.resource_holding), ("absent", 0))
        self.response_transform = lambda value: {**value, "instanceId": "another"}
        with self.assertRaisesRegex(WorkspaceError, "cleanup_instance_mismatch"):
            self.provisioner.observe_cleanup(self.owned)

    def test_unknown_schema_fields_and_exec_outcome_are_rejected(self):
        self.response_transform = lambda value: {**value, "unexpected": True}
        with self.assertRaisesRegex(WorkspaceError, "control_response_invalid"):
            self.provisioner.observe_setup("session", "environment", "run")
        self.response_transform = lambda value: {**value, "exitCode": None, "outcome": "unknown"}
        with self.assertRaisesRegex(WorkspaceError, "benchmark_exec_outcome_unknown"):
            self.provisioner.exec(self.owned, command="write once", timeout_sec=1)

    def test_retire_refreshes_cleanup_epoch_once_then_reuses_exact_operation_request(self):
        self.response_transform = lambda value: {**value, "controllerEpoch": 4} if value["schema"] == "runtime.benchmark.observe.result.v1" else value
        self.provisioner.retire(self.owned, "retire-op")
        first = next(value for path, _, value in self.requests if path == "/internal/environments/retire")
        self.assertEqual(first["expectedControllerEpoch"], 4)
        self.response_transform = lambda value: {**value, "controllerEpoch": 5} if value["schema"] == "runtime.benchmark.observe.result.v1" else value
        self.provisioner.retire(self.owned, "retire-op")
        self.assertEqual([value for path, _, value in self.requests if path == "/internal/environments/retire"], [first, first])

    def test_lost_activation_ack_queries_exact_owned_readiness_without_second_mutation(self):
        self.lose_activation_ack = True
        self.assertEqual(self.provisioner.activate_setup(self.owned, "activate-op"), "ready")
        self.assertEqual([path for path, _, _ in self.requests], [
            "/internal/environments/environment/benchmark-setup-complete", "/internal/benchmark/observe"])


if __name__ == "__main__":
    unittest.main()
