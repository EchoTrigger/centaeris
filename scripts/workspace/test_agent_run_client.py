import base64
from email.message import Message
import hashlib
import io
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from agent_run_client import main, receive


class AgentRunApiClientTests(unittest.TestCase):
    def artifact(self, content=b"verified bytes", *, expected=None, artifact_id="artifact-1", filename="result.docx"):
        digest = "sha256:" + hashlib.sha256(content if expected is None else expected).hexdigest()
        metadata = {"artifactId": artifact_id, "filename": filename, "contentType": "application/octet-stream",
                    "sizeBytes": len(content), "sha256": digest}
        return [("artifactStarted", metadata), ("artifactChunk", {"artifactId": artifact_id, "index": 0,
                  "data": base64.b64encode(content).decode()}), ("artifactCompleted", {**metadata, "chunkCount": 1})]

    def stream(self, events):
        return io.BytesIO("".join(f"event: {event}\ndata: {json.dumps(data)}\n\n" for event, data in events).encode())

    def response(self, content=b"verified bytes", *, expected=None, terminal=True):
        events = self.artifact(content, expected=expected)
        if terminal:
            events.append(("runFinished", {"runStatus": "completed", "deliveryStatus": "completed"}))
        return self.stream(events)

    @patch("sys.stderr", new_callable=io.StringIO)
    def test_verified_artifact_is_published(self, _stderr):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory)
            self.assertEqual(receive(self.response(), output), [output / "result.docx"])
            self.assertEqual((output / "result.docx").read_bytes(), b"verified bytes")
            self.assertEqual(list(output.iterdir()), [output / "result.docx"])

    @patch("sys.stderr", new_callable=io.StringIO)
    def test_bad_hash_removes_unverified_bytes(self, _stderr):
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaisesRegex(RuntimeError, "verification failed"):
                receive(self.response(expected=b"different bytes"), Path(directory))
            self.assertEqual(list(Path(directory).iterdir()), [])

    @patch("sys.stderr", new_callable=io.StringIO)
    def test_existing_caller_file_is_preserved(self, _stderr):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory)
            target = output / "result.docx"
            target.write_bytes(b"caller file")
            with self.assertRaises(FileExistsError):
                receive(self.response(), output)
            self.assertEqual(target.read_bytes(), b"caller file")
            self.assertEqual(list(output.iterdir()), [target])

    @patch("sys.stderr", new_callable=io.StringIO)
    def test_artifact_failure_keeps_later_verified_files_but_fails_delivery(self, _stderr):
        for delivery_status in ("failed", "completed"):
            with self.subTest(delivery_status=delivery_status), tempfile.TemporaryDirectory() as directory:
                output = Path(directory)
                events = self.artifact()[:2]
                events.append(("artifactFailed", {"artifactId": "artifact-1", "error": "artifact_delivery_failed"}))
                events.extend(self.artifact(b"later file", artifact_id="artifact-2", filename="later.txt"))
                events.append(("runFinished", {"runStatus": "completed", "deliveryStatus": delivery_status}))
                with self.assertRaises(RuntimeError):
                    receive(self.stream(events), output)
                self.assertEqual((output / "later.txt").read_bytes(), b"later file")
                self.assertEqual(list(output.iterdir()), [output / "later.txt"])

    @patch("sys.stderr", new_callable=io.StringIO)
    def test_completion_metadata_must_match_the_started_artifact(self, _stderr):
        changes = {"filename": "other.txt", "contentType": "text/plain", "sizeBytes": 1, "sha256": "sha256:bad"}
        for field, changed in changes.items():
            with self.subTest(field=field), tempfile.TemporaryDirectory() as directory:
                events = self.artifact()
                events[-1][1][field] = changed
                events.append(("runFinished", {"runStatus": "completed", "deliveryStatus": "completed"}))
                with self.assertRaisesRegex(RuntimeError, "verification failed"):
                    receive(self.stream(events), Path(directory))
                self.assertEqual(list(Path(directory).iterdir()), [])

    @patch("sys.stderr", new_callable=io.StringIO)
    def test_an_ended_artifact_cannot_complete_again(self, _stderr):
        for ending in ("artifactCompleted", "artifactFailed"):
            with self.subTest(ending=ending), tempfile.TemporaryDirectory() as directory:
                output = Path(directory)
                events = self.artifact()
                completion = events[-1]
                if ending == "artifactFailed":
                    events[-1] = (ending, {"artifactId": "artifact-1", "error": "artifact_delivery_failed"})
                events.append(completion)
                with self.assertRaisesRegex(RuntimeError, "active artifact"):
                    receive(self.stream(events), output)
                if ending == "artifactCompleted":
                    self.assertEqual((output / "result.docx").read_bytes(), b"verified bytes")
                    self.assertEqual(list(output.iterdir()), [output / "result.docx"])
                else:
                    self.assertEqual(list(output.iterdir()), [])


class AgentRunApiCommandTests(unittest.TestCase):
    operation_id = "1793950000.07baf8fa-3024-4992-a2dd-5dc3e12afdb6"

    def response(self, value, content_type="application/json"):
        content = json.dumps(value).encode() if content_type == "application/json" else value
        response = io.BytesIO(content)
        response.headers = Message()
        response.headers["Content-Type"] = content_type
        return response

    def receipt(self, status="queued"):
        return {"operationId": self.operation_id, "runId": "run-1", "runStatus": status}

    def invoke(self, args, response):
        with patch.dict(os.environ, {"CENTAERIS_RUN_KEY": "fixture-run-key"}), \
                patch("sys.argv", ["agent_run_client.py", *args]), \
                patch("sys.stdout", new_callable=io.StringIO) as stdout, \
                patch("sys.stderr", new_callable=io.StringIO) as stderr, \
                patch("urllib.request.urlopen", return_value=response) as urlopen:
            code = main()
            return code, stdout.getvalue(), stderr.getvalue(), urlopen

    def test_submit_receipt_is_not_result_success(self):
        for status in ("queued", "failed", "completed"):
            with self.subTest(status=status), tempfile.TemporaryDirectory() as directory:
                code, stdout, stderr, urlopen = self.invoke([
                    "--url", "https://workspace.example", "--prompt", "Reply OK", "--output", directory,
                    "--operation-id", self.operation_id,
                ], self.response(self.receipt(status)))
                self.assertEqual(code, 2)
                self.assertEqual(stdout, "")
                self.assertIn("receipt only", stderr.lower())
                self.assertIn("not replay", stderr.lower())
                self.assertEqual(urlopen.call_count, 1)

    def test_status_queries_the_original_operation_without_submitting(self):
        receipt = self.receipt("failed")
        code, stdout, _stderr, urlopen = self.invoke([
            "--url", "https://workspace.example", "--status", "--operation-id", self.operation_id,
        ], self.response(receipt))
        self.assertEqual(code, 0)
        self.assertEqual(json.loads(stdout), receipt)
        urlopen.assert_called_once()
        request = urlopen.call_args.args[0]
        self.assertEqual(request.method, "GET")
        self.assertEqual(request.full_url, f"https://workspace.example/api/v1/agent-runs/operations/{self.operation_id}")
        self.assertIsNone(request.data)
        self.assertEqual(request.get_header("Authorization"), "Bearer fixture-run-key")
        self.assertIsNone(request.get_header("Idempotency-key"))

    def test_uncertain_submission_reuses_its_identity_credential_and_body(self):
        with tempfile.TemporaryDirectory() as directory, \
                patch.dict(os.environ, {"CENTAERIS_RUN_KEY": "fixture-run-key"}), \
                patch("sys.stderr", new_callable=io.StringIO) as stderr:
            requests = []

            def request_response(request, *, timeout):
                operation_id = request.get_header("Idempotency-key")
                self.assertIn(f"operationId={operation_id}", stderr.getvalue())
                requests.append(request)
                if len(requests) == 1:
                    raise TimeoutError("response timed out")
                return self.response({**self.receipt(), "operationId": operation_id})

            args = ["--url", "https://workspace.example", "--prompt", "Reply OK", "--output", directory]
            with patch("urllib.request.urlopen", side_effect=request_response):
                with patch("sys.argv", ["agent_run_client.py", *args]):
                    self.assertEqual(main(), 1)
                self.assertEqual(len(requests), 1)
                operation_id = requests[0].get_header("Idempotency-key")
                self.assertIn(f"--operation-id {operation_id}", stderr.getvalue())
                self.assertIn("--status", stderr.getvalue())
                with patch("sys.argv", ["agent_run_client.py", *args, "--operation-id", operation_id]):
                    self.assertEqual(main(), 2)
            self.assertEqual(len(requests), 2)
            self.assertEqual(requests[0].get_header("Idempotency-key"), requests[1].get_header("Idempotency-key"))
            self.assertEqual(requests[0].get_header("Authorization"), requests[1].get_header("Authorization"))
            self.assertEqual(requests[0].data, requests[1].data)
            self.assertEqual(json.loads(requests[1].data), {"prompt": "Reply OK"})

    def test_successful_http_response_requires_the_expected_content_type(self):
        with tempfile.TemporaryDirectory() as directory:
            code, _stdout, stderr, _urlopen = self.invoke([
                "--url", "https://workspace.example", "--prompt", "Reply OK", "--output", directory,
                "--operation-id", self.operation_id,
            ], self.response(b"<html>proxy response</html>", "text/html"))
            self.assertEqual(code, 1)
            self.assertIn("Content-Type", stderr)
            self.assertIn(self.operation_id, stderr)
