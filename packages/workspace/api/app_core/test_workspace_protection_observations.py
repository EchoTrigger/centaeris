"""Protection observations account for actual API work without changing outcomes."""

from contextlib import redirect_stderr
import io
import hashlib
import json
import os
from pathlib import Path
import tempfile
from unittest.mock import patch

from django.conf import settings
from django.db import DatabaseError, connection
from django.test import RequestFactory, SimpleTestCase, TransactionTestCase, override_settings

from .http import internal
from .test_execution_publication_fencing import ExecutionPublicationFixture, prepare_workspace_files
from . import workspace_observations, workspace_snapshot_gc


def protection_records(output):
    return [json.loads(line) for line in output.getvalue().splitlines() if line.startswith('{"schema":')]


def total(records, metric):
    values = [row["value"] for row in records if row["metric"] == metric]
    if not values:
        raise AssertionError(f"{metric} was not measured")
    return sum(values)


def snapshot(content):
    manifest = json.dumps({"schema": "workspace.snapshot.v1", "files": [{
        "path": "test.txt", "sizeBytes": len(content), "sha256": "sha256:" + hashlib.sha256(content).hexdigest(),
        "executable": False,
    }]}, separators=(",", ":")).encode()
    body = len(manifest).to_bytes(4, "big") + manifest + content
    return body, {"snapshotSizeBytes": len(body), "snapshotSha256": "sha256:" + hashlib.sha256(body).hexdigest(),
                  "expandedSizeBytes": len(content), "fileCount": 1}


class WorkspaceProtectionObservationTests(SimpleTestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory(prefix="api-protection-observations-")
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        self.enterContext(override_settings(MEDIA_ROOT=directory.name))

    def test_invalid_metadata_still_counts_received_bytes_without_logging_body(self):
        metadata = b'{"credential":"DO-NOT-LOG-THIS"}'
        body = len(metadata).to_bytes(4, "big") + metadata
        request = RequestFactory().post("/internal/agent-runs/session-workspace/commit", data=body,
                                        content_type="application/octet-stream")
        output = io.StringIO()
        with patch.dict(os.environ, {"WORKSPACE_PERF_OBSERVATIONS": "1"}), redirect_stderr(output):
            response = internal.commit_session_workspace(request)
        self.assertEqual(response.status_code, 400)
        records = protection_records(output)
        self.assertTrue(records, "enabled protection observations must record failed requests")
        received = [row for row in records if row["metric"] == "apiReceivedBytes"]
        self.assertEqual(len(received), 1)
        self.assertEqual(received[0]["value"], len(body))
        self.assertEqual(received[0]["outcome"], "error")
        self.assertNotIn("DO-NOT-LOG-THIS", output.getvalue())
        self.assertNotIn("credential", output.getvalue())

    def test_disabled_observations_do_not_emit_or_change_storage_result(self):
        body, candidate = snapshot(b"body")
        output = io.StringIO()
        with patch.dict(os.environ, {"WORKSPACE_PERF_OBSERVATIONS": "0"}), redirect_stderr(output):
            with workspace_observations.operation("test"):
                with internal._workspace_snapshot_upload("disabled.snapshot") as upload:
                    internal._store_workspace_snapshot(io.BytesIO(body), candidate, "disabled.snapshot", upload, {"snapshotEncoding": "frameV1"})
                    self.assertTrue(internal._link_workspace_snapshot("disabled.snapshot", upload))
        self.assertEqual(output.getvalue(), "")
        self.assertEqual((self.root / "disabled.snapshot").read_bytes(), body)

    def test_validation_failure_counts_consumed_bytes_and_partial_file_writes(self):
        body, candidate = snapshot(b"a" * (128 * 1024))
        damaged = body[:-1] + b"b"
        output = io.StringIO()
        with patch.dict(os.environ, {"WORKSPACE_PERF_OBSERVATIONS": "1"}), redirect_stderr(output):
            with self.assertRaises(internal.SessionWorkspaceError), workspace_observations.operation("test"):
                with internal._workspace_snapshot_upload("invalid.snapshot") as upload:
                    internal._store_workspace_snapshot(io.BytesIO(damaged), candidate, "invalid.snapshot", upload, {"snapshotEncoding": "frameV1"})
        records = protection_records(output)
        self.assertEqual(total(records, "apiReceivedBytes"), len(damaged))
        self.assertEqual(total(records, "apiWrittenBytes"), 128 * 1024)
        self.assertTrue(all(row["outcome"] == "error" for row in records))
        self.assertFalse((self.root / "invalid.snapshot").exists())
        self.assertEqual(list(self.root.iterdir()), [])

    def test_verification_failure_counts_all_bytes_it_read(self):
        body, candidate = snapshot(b"body")
        (self.root / "corrupt.snapshot").write_bytes(body + b"extra")
        output = io.StringIO()
        with patch.dict(os.environ, {"WORKSPACE_PERF_OBSERVATIONS": "1"}), redirect_stderr(output):
            with self.assertRaises(internal.SessionWorkspaceError), workspace_observations.operation("test"):
                internal._verify_workspace_snapshot(candidate, "corrupt.snapshot")
        self.assertEqual(total(protection_records(output), "apiVerificationReadBytes"), len(body) + 5)

    def test_partial_write_error_counts_bytes_that_reached_the_file(self):
        body, candidate = snapshot(b"body" * 100)
        real_fdopen = os.fdopen
        class PartialWriter:
            def __init__(self, descriptor, mode):
                self.handle = real_fdopen(descriptor, mode)
            def __enter__(self):
                return self
            def __exit__(self, *_args):
                self.handle.close()
            def write(self, chunk):
                self.handle.write(chunk[:37])
                self.handle.flush()
                raise OSError("injected partial write")
        output = io.StringIO()
        with patch.dict(os.environ, {"WORKSPACE_PERF_OBSERVATIONS": "1"}), redirect_stderr(output):
            with self.assertRaisesRegex(OSError, "injected partial write"), workspace_observations.operation("test"):
                with internal._workspace_snapshot_upload("partial.snapshot") as upload:
                    with patch.object(os, "fdopen", side_effect=PartialWriter):
                        internal._store_workspace_snapshot(io.BytesIO(body), candidate, "partial.snapshot", upload, {"snapshotEncoding": "frameV1"})
        records = protection_records(output)
        self.assertEqual(total(records, "apiReceivedBytes"), len(body))
        self.assertEqual(total(records, "apiWrittenBytes"), 37)
        self.assertEqual(list(self.root.iterdir()), [])

    def test_gc_counts_only_removed_last_link_bytes_and_keeps_failed_deletion_zero(self):
        content = b"retained bytes"
        (self.root / "first.snapshot").write_bytes(content)
        os.link(self.root / "first.snapshot", self.root / "second.snapshot")
        output = io.StringIO()
        with patch.dict(os.environ, {"WORKSPACE_PERF_OBSERVATIONS": "1"}), redirect_stderr(output):
            workspace_snapshot_gc._delete_observed_snapshot("first.snapshot", session_id="session:test")
            with patch.object(workspace_snapshot_gc, "delete_stored_object_for_gc", side_effect=OSError("failed unlink")):
                with self.assertRaises(OSError):
                    workspace_snapshot_gc._delete_observed_snapshot("second.snapshot", session_id="session:test")
            workspace_snapshot_gc._delete_observed_snapshot("second.snapshot", session_id="session:test")
        reclaimed = [row for row in protection_records(output) if row["metric"] == "gcReclaimedBytes"]
        self.assertEqual([row["value"] for row in reclaimed], [0, 0, len(content)])
        self.assertEqual([row["outcome"] for row in reclaimed], ["ok", "error", "ok"])
        self.assertTrue(all(row["sessionId"] == "session:test" for row in reclaimed))

    def test_observation_sink_failure_does_not_fail_request(self):
        metadata = b"invalid"
        body = len(metadata).to_bytes(4, "big") + metadata
        request = RequestFactory().post("/internal/agent-runs/session-workspace/commit", data=body,
                                        content_type="application/octet-stream")
        with patch.dict(os.environ, {"WORKSPACE_PERF_OBSERVATIONS": "1"}), \
                patch("app_core.workspace_observations.print", side_effect=OSError("sink unavailable")), \
                self.assertLogs("app_core.workspace_observations", level="WARNING") as logs:
            response = internal.commit_session_workspace(request)
        self.assertEqual(response.status_code, 400)
        self.assertIn("eventSink", logs.output[0])
        self.assertNotIn("sink unavailable", logs.output[0])

    def test_gc_error_after_unlink_still_counts_the_removed_bytes(self):
        path = self.root / "removed.snapshot"
        path.write_bytes(b"removed before failure")
        def delete_then_fail(_key):
            path.unlink()
            raise OSError("error after deletion")
        output = io.StringIO()
        with patch.dict(os.environ, {"WORKSPACE_PERF_OBSERVATIONS": "1"}), redirect_stderr(output), \
                patch.object(workspace_snapshot_gc, "delete_stored_object_for_gc", side_effect=delete_then_fail):
            with self.assertRaises(OSError):
                workspace_snapshot_gc._delete_observed_snapshot("removed.snapshot", session_id="session:test")
        records = protection_records(output)
        self.assertEqual(total(records, "gcReclaimedBytes"), len(b"removed before failure"))
        self.assertTrue(all(row["outcome"] == "error" for row in records))

    def test_gc_of_mutable_upload_temporary_keeps_reclaimed_bytes_unmeasured(self):
        key = ".centaeris-immutable-abcd1234.tmp"
        path = self.root / key
        path.write_bytes(b"still uploading")
        def growing_then_delete(_key):
            with path.open("ab") as handle:
                handle.write(b"more upload bytes")
            path.unlink()
        output = io.StringIO()
        with patch.dict(os.environ, {"WORKSPACE_PERF_OBSERVATIONS": "1"}), redirect_stderr(output), \
                patch.object(workspace_snapshot_gc, "delete_stored_object_for_gc", side_effect=growing_then_delete), \
                self.assertLogs("app_core.workspace_observations", level="WARNING") as logs:
            workspace_snapshot_gc._delete_observed_snapshot(key, session_id="session:test")
        records = protection_records(output)
        self.assertNotIn("gcReclaimedBytes", {row["metric"] for row in records})
        self.assertGreaterEqual(total(records, "gcElapsedMs"), 0)
        self.assertIn("gcReclaimedBytes", logs.output[0])
        self.assertNotIn(key, logs.output[0])
        self.assertFalse(path.exists())

    def test_missing_file_measurement_is_omitted_instead_of_reported_as_zero(self):
        body, candidate = snapshot(b"body")
        output = io.StringIO()
        real_stat = os.stat
        def failed_sample(path, *args, **kwargs):
            if ".centaeris-immutable-" in str(path):
                raise OSError("sample failure")
            return real_stat(path, *args, **kwargs)
        with patch.dict(os.environ, {"WORKSPACE_PERF_OBSERVATIONS": "1"}), redirect_stderr(output):
            with workspace_observations.operation("test"), self.assertLogs("app_core.workspace_observations", level="WARNING"):
                with internal._workspace_snapshot_upload("sample.snapshot") as upload:
                    with patch.object(os, "stat", side_effect=failed_sample):
                        internal._store_workspace_snapshot(io.BytesIO(body), candidate, "sample.snapshot", upload, {"snapshotEncoding": "frameV1"})
                    self.assertTrue(internal._link_workspace_snapshot("sample.snapshot", upload))
        records = protection_records(output)
        self.assertNotIn("apiWrittenBytes", {row["metric"] for row in records})
        self.assertEqual((self.root / "sample.snapshot").read_bytes(), body)


class ExecutionProtectionObservationTests(ExecutionPublicationFixture, TransactionTestCase):
    serialized_rollback = True

    def post_manifest(self, body):
        return self.client.post("/internal/agent-runs/session-workspace/commit", data=body,
            content_type="application/octet-stream", HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN)

    def test_stage_then_replay_measures_upload_without_advancing_session_head(self):
        body, snapshot_body = self._body(b"staged recovery contents")
        prepare_workspace_files(self, body)
        metadata_size = int.from_bytes(body[:4], "big")
        metadata = json.loads(body[4:4 + metadata_size])
        metadata.update(schema="runtime.execution_workspace.stage.v2", checkpointId="checkpoint:observations")
        encoded = json.dumps(metadata, separators=(",", ":")).encode()
        stage_body = len(encoded).to_bytes(4, "big") + encoded + snapshot_body
        outputs, receipts = [], []
        for expected_writes, expected_reads, expected_links in ((len(snapshot_body), 0, 1),
                                                               (0, len(snapshot_body), 0)):
            output = io.StringIO()
            with patch.dict(os.environ, {"WORKSPACE_PERF_OBSERVATIONS": "1"}), redirect_stderr(output):
                response = self.client.post(
                    "/internal/agent-runs/execution-workspace/stage", data=stage_body,
                    content_type="application/octet-stream",
                    HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN,
                )
            self.assertEqual(response.status_code, 201, response.content)
            receipt = response.json()
            self.assertEqual(receipt["schema"], "runtime.execution_workspace.stage.result.v1")
            for field in ("snapshotSha256", "snapshotSizeBytes", "expandedSizeBytes", "fileCount"):
                self.assertEqual(receipt[field], metadata[field])
            self.assertEqual((self.storage_root / receipt["objectRef"]).read_bytes(), snapshot_body)
            records = protection_records(output)
            self.assertEqual(total(records, "apiReceivedBytes"), len(stage_body))
            self.assertEqual(total(records, "apiWrittenBytes"), expected_writes)
            self.assertEqual(total(records, "apiVerificationReadBytes"), expected_reads)
            self.assertEqual(total(records, "publishedLinks"), expected_links)
            self.assertEqual(total(records, "committedSnapshots"), 0)
            self.assertTrue(all(row["source"] == "api" and row["trigger"] == "executionStage"
                                and row["outcome"] == "ok" and row["sessionId"] == self.session.id
                                and row["runId"] == self.run.id and row["jobId"] == self.job_id
                                for row in records))
            self.assertEqual(len({row["operationId"] for row in records}), 1)
            self.session.refresh_from_db()
            self.assertEqual(self.session.workspaceGeneration, 0)
            self.assertEqual(self.session.workspaceStorageKey, "")
            outputs.append(records)
            receipts.append(receipt)
        self.assertEqual(receipts[0], receipts[1])
        self.assertNotEqual(outputs[0][0]["operationId"], outputs[1][0]["operationId"])
        self.assertNotIn("staged recovery contents", json.dumps(outputs))

    def test_commit_then_replay_reports_actual_transfer_storage_and_link_cost(self):
        body, snapshot_body = self._body(b"protected contents")
        prepare_workspace_files(self, body)
        outputs = []
        with patch.dict(os.environ, {"WORKSPACE_PERF_OBSERVATIONS": "1"}):
            for expected_status in (201, 200):
                output = io.StringIO()
                with redirect_stderr(output):
                    response = self.post_manifest(body)
                self.assertEqual(response.status_code, expected_status, response.content)
                outputs.append(protection_records(output))
        first, replay = outputs
        self.assertEqual(total(first, "apiReceivedBytes"), len(body))
        self.assertEqual(total(first, "apiWrittenBytes"), len(snapshot_body))
        self.assertEqual(total(first, "apiVerificationReadBytes"), 0)
        self.assertEqual(total(first, "publishedLinks"), 1)
        self.assertEqual(total(first, "committedSnapshots"), 1)
        self.assertEqual(total(replay, "apiReceivedBytes"), len(body))
        self.assertEqual(total(replay, "apiWrittenBytes"), 0)
        self.assertEqual(total(replay, "apiVerificationReadBytes"), len(snapshot_body))
        self.assertEqual(total(replay, "publishedLinks"), 0)
        self.assertEqual(total(replay, "committedSnapshots"), 0)
        temporary = [row["value"] for row in first if row["metric"] == "temporaryBytes"]
        self.assertEqual(temporary, [len(snapshot_body), 0])
        self.assertNotEqual(first[0]["operationId"], replay[0]["operationId"])
        self.assertTrue(all(row["sessionId"] == self.session.id and row["runId"] == self.run.id for row in first))
        self.assertNotIn("protected contents", json.dumps(outputs))
        self._assert_snapshot(snapshot_body)

    def test_database_commit_failure_keeps_commit_result_unmeasured(self):
        body, snapshot_body = self._body(b"uploaded before commit failure")
        prepare_workspace_files(self, body)
        real_commit = connection.commit
        commits = 0
        def fail_publication_commit():
            nonlocal commits
            commits += 1
            if commits == 2:
                raise DatabaseError("injected publication commit failure")
            return real_commit()
        output = io.StringIO()
        with patch.dict(os.environ, {"WORKSPACE_PERF_OBSERVATIONS": "1"}), redirect_stderr(output), \
                patch.object(connection, "commit", side_effect=fail_publication_commit), \
                self.assertLogs("app_core.workspace_observations", level="WARNING") as logs:
            response = self.post_manifest(body)
        self.assertEqual(response.status_code, 500)
        records = protection_records(output)
        self.assertEqual(total(records, "apiWrittenBytes"), len(snapshot_body))
        self.assertEqual(total(records, "publishedLinks"), 1)
        self.assertNotIn("committedSnapshots", {row["metric"] for row in records})
        self.assertEqual(logs.output, ["WARNING:app_core.workspace_observations:"
                                     "Workspace protection observation unavailable: committedSnapshots"])
        self.session.refresh_from_db()
        self.assertEqual(self.session.workspaceGeneration, 0)

    def test_lost_commit_acknowledgement_does_not_report_zero_committed_snapshots(self):
        body, snapshot_body = self._body(b"DO-NOT-LOG-COMMITTED-BODY")
        prepare_workspace_files(self, body)
        real_commit = connection.commit
        commits = 0
        def lose_publication_acknowledgement():
            nonlocal commits
            commits += 1
            real_commit()
            if commits == 2:
                raise DatabaseError("DO-NOT-LOG-COMMIT-DETAILS")
        output = io.StringIO()
        with patch.dict(os.environ, {"WORKSPACE_PERF_OBSERVATIONS": "1"}), redirect_stderr(output), \
                patch.object(connection, "commit", side_effect=lose_publication_acknowledgement), \
                self.assertLogs("app_core.workspace_observations", level="WARNING") as logs:
            response = self.post_manifest(body)
        self.assertEqual(response.status_code, 500)
        self._assert_snapshot(snapshot_body)
        records = protection_records(output)
        self.assertEqual(total(records, "apiWrittenBytes"), len(snapshot_body))
        self.assertEqual(total(records, "publishedLinks"), 1)
        self.assertNotIn("committedSnapshots", {row["metric"] for row in records})
        self.assertTrue(all(row["outcome"] == "error" for row in records))
        self.assertNotIn("DO-NOT-LOG-COMMITTED-BODY", json.dumps(records))
        self.assertNotIn("DO-NOT-LOG-COMMIT-DETAILS", json.dumps(records))
        self.assertEqual(logs.output, ["WARNING:app_core.workspace_observations:"
                                     "Workspace protection observation unavailable: committedSnapshots"])
