import importlib.util
import json
from io import BytesIO
from pathlib import Path
import unittest
import shlex
import subprocess
import sys
import tempfile
from unittest.mock import patch
from unittest.mock import Mock
from urllib.error import HTTPError
from urllib.request import Request

SOURCE = Path(__file__).resolve().parents[1] / "mock-model/server.py"
spec = importlib.util.spec_from_file_location("capture_mock", SOURCE)
mock = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mock)


class CaptureWorkloadTests(unittest.TestCase):
    def test_content_wait_accepts_only_explicit_unavailable_and_closes_errors(self):
        harness = Path(__file__).resolve().parents[1] / "harness"
        sys.path.insert(0, str(harness))
        self.addCleanup(sys.path.remove, str(harness))
        import transcript_capture_validation as validation
        for status, code in ((409, "transcript_content_unavailable"),
                             (409, "unrelated_conflict"), (500, "transcript_content_unavailable")):
            with self.subTest(status=status, code=code):
                body = BytesIO(json.dumps({"error": code}).encode())
                client = Mock()
                client.records = []
                client.request.return_value = Request("http://127.0.0.1/api/content")
                client.opener.open.side_effect = HTTPError("http://127.0.0.1/api/content", status,
                                                         "synthetic", {}, body)
                client.read.side_effect = lambda response, _deadline: response.read()
                expected = validation.CaptureNotReady if status == 409 and code == (
                    "transcript_content_unavailable") else HTTPError
                with self.assertRaises(expected):
                    validation.query_content(client, "session", {"offset": "0"})
                self.assertTrue(body.closed)
                self.assertEqual(client.records[0]["status"], status)

    def test_invalid_content_fails_the_experiment_without_retrying(self):
        harness = Path(__file__).resolve().parents[1] / "harness"
        sys.path.insert(0, str(harness))
        self.addCleanup(sys.path.remove, str(harness))
        import transcript_capture_validation as validation
        stack, client = Mock(), Mock()
        stack.quiet.return_value = True
        stack.manifest.return_value = {}
        stack.values = {"BOOTSTRAP_SUPERADMIN_EMAIL": "synthetic@example.invalid",
                        "BOOTSTRAP_SUPERADMIN_PASSWORD": "synthetic-test-only"}
        tools = [{"type": "tool_result", "payload": {
            "callId": str(index), "toolName": "bash", "resultState": "successWithOutput",
            "modelContent": text, "fullOutputPath": "result.log", "outputByteLength": 110056,
        }} for index, text in enumerate(("preview", "capture-mutated:1", "capture-deleted:1"))]
        stack.sql.return_value = json.dumps(tools)
        accepted = {"sessionId": "session", "agentRunId": "run"}
        client.call.side_effect = [{"csrfToken": "synthetic"}, {}, {"csrfToken": "synthetic"},
            {"provider": {"id": "provider"}}, {"id": "model"},
            {"workspaces": [{"id": "workspace"}]}, {"agents": [{"id": "agent"}]}, accepted]
        client.observe.side_effect = lambda _accepted, _index, observations, _stop: observations.append(
            {"terminal": "agent_run_completed"})
        data = b"Command completed successfully with exit code 0.\nstdout:\n" + (
            "归档😀\n" * 10000).encode().rstrip(b"\n")
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(validation.control, "Stack", return_value=stack), \
                patch.object(validation, "Client", return_value=client), \
                patch.object(validation, "durable_runs", return_value=[{"status": "completed"}]), \
                patch.object(validation, "ready_page", return_value={"projectionGeneration": "generation"}), \
                patch.object(validation.time, "sleep"), \
                patch.object(validation, "read_all", side_effect=[
                    RuntimeError("invalid bounded UTF-8 continuation"), (data, 2), (data, 2),
                ]) as read:
            output = Path(directory) / "evidence"
            with self.assertRaisesRegex(RuntimeError, "continuation"):
                validation.main(output)
            self.assertEqual(read.call_count, 1)
            self.assertFalse(json.loads((output / "report.json").read_text())["passed"])

    def test_projection_read_waits_only_for_explicit_not_ready(self):
        sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "harness"))
        self.addCleanup(sys.path.pop, 0)
        import transcript_capture_validation as validation
        client = Mock()
        ready = {"projectionGeneration": "generation-1", "blocks": []}
        client.call.side_effect = [{"error": "transcript_projection_not_ready"}, ready]
        with patch.object(validation.time, "sleep"):
            self.assertEqual(validation.ready_page(client, "session"), ready)
        client.call.side_effect = [{"error": "unrelated_conflict"}]
        with self.assertRaises(RuntimeError):
            validation.ready_page(client, "session")

    def test_capture_profile_produces_mutates_and_removes_before_finishing(self):
        commands = [mock.capture_command(index) for index in range(4)]
        self.assertIsNone(commands[3])
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            spill = root / ".agent-tool-results/session/result.log"
            spill.parent.mkdir(parents=True)
            spill.write_bytes(b"AAA")
            other = root / "keep.txt"
            other.write_bytes(b"keep")
            def run(command):
                return subprocess.check_output([sys.executable, *shlex.split(command)[1:]], cwd=root)
            self.assertEqual(run(commands[0]), ("归档😀\n" * 10000).encode())
            self.assertIn(b"capture-mutated:1", run(commands[1]))
            self.assertEqual(spill.read_bytes(), b"BBB")
            self.assertIn(b"capture-deleted:1", run(commands[2]))
            self.assertFalse(spill.exists())
            self.assertEqual(other.read_bytes(), b"keep")

    def test_history_reader_rejects_partial_and_misaligned_ranges(self):
        harness = Path(__file__).resolve().parents[1] / "harness"
        sys.path.insert(0, str(harness))
        self.addCleanup(sys.path.remove, str(harness))
        import transcript_capture_validation as validation
        with patch.object(validation, "query_content", return_value={
            "content": "界", "endOffset": "1", "hasMore": False
        }), self.assertRaisesRegex(RuntimeError, "continuation"):
            validation.read_all(None, "session", {"byteLength": "3"})
        with patch.object(validation, "query_content", return_value={
            "content": "界", "endOffset": "3", "hasMore": False
        }), self.assertRaisesRegex(RuntimeError, "incomplete"):
            validation.read_all(None, "session", {"byteLength": "6"})
        with patch.object(validation, "query_content", side_effect=[
            {"content": "界", "endOffset": "3", "hasMore": True},
            {"content": "文", "endOffset": "6", "hasMore": False},
        ]):
            data, pages = validation.read_all(None, "session", {"byteLength": "6"})
        self.assertEqual(data, "界文".encode())
        self.assertEqual(pages, 2)
