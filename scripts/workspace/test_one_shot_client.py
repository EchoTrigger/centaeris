import base64
import hashlib
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from one_shot_client import receive


class OneShotClientTests(unittest.TestCase):
    def response(self, content=b"verified bytes", *, expected=None, terminal=True):
        digest = "sha256:" + hashlib.sha256(content if expected is None else expected).hexdigest()
        metadata = {"artifactId": "artifact-1", "filename": "result.docx", "contentType": "application/octet-stream",
                    "sizeBytes": len(content), "sha256": digest}
        events = [("artifactStarted", metadata), ("artifactChunk", {"artifactId": "artifact-1", "index": 0,
                  "data": base64.b64encode(content).decode()}), ("artifactCompleted", {**metadata, "chunkCount": 1})]
        if terminal:
            events.append(("runFinished", {"runStatus": "completed", "deliveryStatus": "completed"}))
        return io.BytesIO("".join(f"event: {event}\ndata: {json.dumps(data)}\n\n" for event, data in events).encode())

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
