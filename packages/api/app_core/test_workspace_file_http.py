"""Exercise incremental workspace storage through the authenticated HTTP contract."""
from contextlib import redirect_stderr
import hashlib
import io
import os
import json
from unittest.mock import patch

from django.conf import settings
from django.db import connection
from django.test import TransactionTestCase

from .agent_run_authorization_factory import create_agent_run_authorization
from .models import AgentRun, Session, WorkspaceFileObject, WorkspaceSnapshotFileReference, WorkspaceSnapshotObject
from .test_execution_publication_fencing import ExecutionPublicationFixture
from .tests import streaming_response_bytes


PREFIX = "/internal/agent-runs/"


def digest(content):
    return "sha256:" + hashlib.sha256(content).hexdigest()


def file_descriptor(content):
    return {"sha256": digest(content), "sizeBytes": len(content)}


def document(entries):
    return {"schema": "workspace.snapshot.v1", "files": [
        {"path": path, "sizeBytes": len(content), "sha256": digest(content), "executable": executable}
        for path, content, executable in entries]}


def canonical(value):
    return json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode("utf-8")


class WorkspaceFileHttpFixture(ExecutionPublicationFixture):
    serialized_rollback = True

    def identity(self, schema, **values):
        return {"schema": schema, "jobId": self.job_id, "leaseOwner": self.owner,
                "agentRunId": self.run.pk, "authorizationDigest": self.authorization.digest, **values}

    def post_json(self, endpoint, body, *, token=None):
        return self.client.post(PREFIX + endpoint, data=canonical(body), content_type="application/json",
            HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN if token is None else token)

    def missing(self, contents=None, *, files=None, **identity):
        body = self.identity("runtime.workspace_files.missing.v1",
            files=[file_descriptor(content) for content in contents] if files is None else files)
        return self.post_json("workspace-files/missing", body | identity)

    def upload_batch(self, contents, *, received=None, **identity):
        metadata = canonical(self.identity("runtime.workspace_files.upload.v1",
            files=[file_descriptor(content) for content in contents]) | identity)
        payload = len(metadata).to_bytes(4, "big") + metadata + (b"".join(contents) if received is None else received)
        return self.client.post(PREFIX + "workspace-files/upload", data=payload,
            content_type="application/octet-stream", HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN)

    def upload(self, content, *, received=None, **identity):
        return self.upload_batch([content], received=received, **identity)

    def store_files(self, contents):
        missing = self.missing(contents)
        self.assertEqual(missing.status_code, 200, missing.content)
        requested = set(missing.json()["missing"])
        unique = list({digest(content): content for content in contents if digest(content) in requested}.values())
        for offset in range(0, len(unique), 32):
            batch = unique[offset:offset + 32]
            uploaded = self.upload_batch(batch)
            self.assertEqual(uploaded.status_code, 201, uploaded.content)
            self.assertEqual(uploaded.json()["files"], [file_descriptor(content) for content in batch])
        return missing

    def snapshot_upload(self, value, *, checkpoint_id=None, metadata_changes=None, received=None):
        encoded = canonical(value)
        snapshot = {"snapshotSha256": digest(encoded), "snapshotSizeBytes": len(encoded),
                    "expandedSizeBytes": sum(file["sizeBytes"] for file in value["files"]),
                    "fileCount": len(value["files"])}
        body = self.identity("runtime.session_workspace.commit.v2" if checkpoint_id is None
                             else "runtime.execution_workspace.stage.v2",
                             snapshotEncoding="fileManifestV1", **snapshot)
        if checkpoint_id is not None:
            body["checkpointId"] = checkpoint_id
        body.update(metadata_changes or {})
        metadata = canonical(body)
        response = self.client.post(PREFIX + ("session-workspace/commit" if checkpoint_id is None
                                               else "execution-workspace/stage"),
            data=len(metadata).to_bytes(4, "big") + metadata + (encoded if received is None else received),
            content_type="application/octet-stream", HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN)
        return response, snapshot, encoded

    def new_reader(self):
        AgentRun.objects.filter(pk=self.run.pk).update(status="completed")
        self.session.refresh_from_db()
        reader = AgentRun.objects.create(workspace=self.run.workspace, session=self.session,
            user=self.run.user, modelConfig=self.run.modelConfig, prompt="read persisted workspace", status="running")
        grant = create_agent_run_authorization(reader, image_digest="sha256:" + "a" * 64)
        job = "agent_run.lifecycle:" + reader.pk
        owner = "worker:workspace-file-reader"
        with connection.cursor() as cursor:
            cursor.execute("INSERT INTO runtime.runtime_jobs(job_id,job_kind,status,lease_owner,lease_expires_at_ms,"
                "idempotency_key,session_id,payload_ref) VALUES(%s,'agent_run.lifecycle','running',%s,"
                "(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint+60000,%s,%s,%s)",
                [job, owner, job + ":" + grant.digest, self.session.pk, "record:agent_run:" + reader.pk])
        def delete_job():
            with connection.cursor() as cursor:
                cursor.execute("DELETE FROM runtime.runtime_jobs WHERE job_id=%s", [job])
        self.addCleanup(delete_job)
        return {"jobId": job, "leaseOwner": owner, "agentRunId": reader.pk, "authorizationDigest": grant.digest}

    def download_batch(self, contents, snapshot, *, identity=None, checkpoint_id=None, **changes):
        body = self.identity("runtime.workspace_files.download.v1", checkpointId=checkpoint_id,
                             snapshot=snapshot, files=[file_descriptor(content) for content in contents])
        body.update(identity or {})
        body.update(changes)
        response = self.post_json("workspace-files/download", body)
        return response, streaming_response_bytes(response) if response.streaming else response.content

    def download(self, content, snapshot, **kwargs):
        return self.download_batch([content], snapshot, **kwargs)

    def checkpoint(self, checkpoint_id, staged):
        snapshot = {key: staged[key] for key in
                    ("objectRef", "snapshotSha256", "snapshotSizeBytes", "expandedSizeBytes", "fileCount")}
        payload = {"schema": "runtime.recovery_checkpoint.v1", "checkpointId": checkpoint_id,
            "sessionId": self.session.pk, "agentRunId": self.run.pk, "executionId": "execution:files-http",
            "authorizationDigest": self.authorization.digest, "sessionSequence": 1,
            "modelRequestId": "request:files-http", "workspaceSnapshot": snapshot, "createdAtMs": 1}
        with connection.cursor() as cursor:
            cursor.execute("CREATE TABLE IF NOT EXISTS runtime.checkpoints(checkpoint_id text PRIMARY KEY,"
                "kind text NOT NULL,session_id text NOT NULL,turn_id text NOT NULL,status text NOT NULL,"
                "done_reason text,updated_at_ms bigint NOT NULL,payload_json text NOT NULL)")
            cursor.execute("INSERT INTO runtime.checkpoints VALUES(%s,'recovery',%s,%s,'committed',NULL,1,%s)",
                           [checkpoint_id, self.session.pk, self.run.turn_id, canonical(payload).decode()])
        def delete_checkpoint():
            with connection.cursor() as cursor:
                cursor.execute("DELETE FROM runtime.checkpoints WHERE checkpoint_id=%s", [checkpoint_id])
        self.addCleanup(delete_checkpoint)
        roots = {"schema": "runtime.workspace_snapshot.roots.v1", "sessionId": self.session.pk,
            "sessionSequence": 1, "authorityFingerprint": "sha256:" + "a" * 64,
            "protectedAgentRunIds": [self.run.pk], "roots": [{"agentRunId": self.run.pk,
                "authorizationDigest": self.authorization.digest, "checkpointId": checkpoint_id, **snapshot}]}
        self.enterContext(patch("app_core.workspace_snapshot_roots.request_workspace_snapshot_roots", return_value=roots))
        self.enterContext(patch("app_core.http.workspace_files.request_workspace_snapshot_roots", return_value=roots))
        self.enterContext(patch("app_core.workspace_snapshot_roots.current_witness",
                               return_value=(1, "sha256:" + "a" * 64)))
        return roots

class WorkspaceFileHttpTests(WorkspaceFileHttpFixture, TransactionTestCase):
    serialized_rollback = True

    def test_missing_upload_commit_and_next_run_download_preserve_exact_content(self):
        contents = [b"stable body", b"executable body"]
        missing = self.store_files(contents)
        self.assertEqual(set(missing.json()["missing"]), {digest(content) for content in contents})
        self.assertEqual(self.missing(contents).json()["missing"], [])
        value = document([("a.txt", contents[0], False), ("script", contents[1], True)])
        committed, snapshot, encoded = self.snapshot_upload(value)
        self.assertEqual(committed.status_code, 201, committed.content)
        self.session.refresh_from_db()
        self.assertEqual(self.session.workspaceSnapshotEncoding, "fileManifestV1")
        self.assertEqual((self.session.workspaceSnapshotSizeBytes, self.session.workspaceExpandedSizeBytes),
                         (len(encoded), sum(map(len, contents))))
        self.assertEqual((self.storage_root / self.session.workspaceStorageKey).read_bytes(), encoded)
        self.assertEqual(WorkspaceSnapshotFileReference.objects.count(), 2)
        reader = self.new_reader()
        for content in contents:
            response, received = self.download(content, snapshot, identity=reader)
            self.assertEqual(response.status_code, 200, received)
            self.assertEqual(response["Content-Length"], str(len(content)))
            self.assertEqual(received, content)
        manifest_response = self.post_json("session-workspace/download", {
            "schema": "runtime.session_workspace.download.v1", **reader})
        self.assertEqual(streaming_response_bytes(manifest_response), encoded)
        self.assertEqual(manifest_response["X-Workspace-Snapshot-Encoding"], "fileManifestV1")

    def test_stage_download_requires_a_core_selected_checkpoint_reference(self):
        content = b"checkpoint file"
        self.store_files([content])
        checkpoint_id = "checkpoint:files-http"
        staged, snapshot, encoded = self.snapshot_upload(document([("file", content, False)]), checkpoint_id=checkpoint_id)
        self.assertEqual(staged.status_code, 201, staged.content)
        roots = self.checkpoint(checkpoint_id, staged.json())
        registry = WorkspaceSnapshotObject.objects.get(storageKey=staged.json()["objectRef"])
        self.assertEqual(registry.snapshotEncoding, "fileManifestV1")
        response, received = self.download(content, snapshot, checkpoint_id=checkpoint_id)
        self.assertEqual(response.status_code, 200, received)
        self.assertEqual(received, content)
        manifest_response = self.post_json("execution-workspace/download",
            self.identity("runtime.execution_workspace.download.v1", checkpointId=checkpoint_id))
        self.assertEqual(manifest_response.status_code, 200, manifest_response.content if not manifest_response.streaming else b"")
        self.assertEqual(streaming_response_bytes(manifest_response), encoded)
        self.assertEqual(manifest_response["X-Workspace-Snapshot-Encoding"], "fileManifestV1")
        roots["roots"] = []
        response, _ = self.download(content, snapshot, checkpoint_id=checkpoint_id)
        self.assertEqual(response.status_code, 409)
        self.assertEqual(response.json()["error"], "execution_workspace_checkpoint_unreachable")

    def test_unuploaded_or_tampered_content_cannot_publish_a_manifest(self):
        content = b"expected"
        value = document([("file", content, False)])
        self.assertEqual(self.missing([content]).status_code, 200)
        invalid = self.upload(content, received=b"tampered")
        self.assertEqual(invalid.status_code, 400, invalid.content)
        committed, _, _ = self.snapshot_upload(value)
        self.assertEqual(committed.status_code, 409, committed.content)
        self.assertEqual(committed.json()["error"], "workspace_manifest_file_unavailable")
        self.session.refresh_from_db()
        self.assertEqual(self.session.workspaceGeneration, 0)
        self.assertEqual(WorkspaceSnapshotFileReference.objects.count(), 0)

    def test_manifest_digest_and_encoding_are_checked_before_head_advance(self):
        content = b"valid body"
        self.store_files([content])
        value = document([("file", content, False)])
        encoded = canonical(value)
        altered = encoded.replace(b'"executable":false', b'"executable": true')
        response, _, _ = self.snapshot_upload(value, received=altered)
        self.assertEqual(response.status_code, 400, response.content)
        response, _, _ = self.snapshot_upload(value, metadata_changes={"snapshotEncoding": "frameV1"})
        self.assertEqual(response.status_code, 400, response.content)
        self.session.refresh_from_db()
        self.assertEqual(self.session.workspaceGeneration, 0)

    def test_wrong_lease_and_unknown_protocol_fields_are_rejected(self):
        content = b"reserved"
        response = self.missing([content], leaseOwner="worker:not-the-owner")
        self.assertEqual(response.status_code, 409, response.content)
        self.assertEqual(response.json()["error"], "session_workspace_lease_lost")
        self.assertEqual(WorkspaceFileObject.objects.count(), 0)
        response = self.missing([content], unexpected=True)
        self.assertEqual(response.status_code, 400, response.content)
        response = self.post_json("workspace-files/missing",
            self.identity("runtime.workspace_files.missing.v1", files=[file_descriptor(content)]), token="wrong-token")
        self.assertEqual(response.status_code, 401, response.content)
        self.assertEqual(WorkspaceFileObject.objects.count(), 0)

    def test_download_does_not_expose_a_reserved_file_absent_from_selected_manifest(self):
        included, excluded = b"included", b"excluded"
        self.store_files([included, excluded])
        committed, snapshot, _ = self.snapshot_upload(document([("file", included, False)]))
        self.assertEqual(committed.status_code, 201, committed.content)
        response, _ = self.download(excluded, snapshot, identity=self.new_reader())
        self.assertEqual(response.status_code, 404)
        self.assertEqual(response.json()["error"], "workspace_file_reference_not_found")

    def test_file_upload_observations_count_request_and_body_in_their_own_span(self):
        content = b"measured workspace file body"
        reserved = self.missing([content])
        self.assertEqual(reserved.status_code, 200, reserved.content)
        metadata = canonical(self.identity("runtime.workspace_files.upload.v1", files=[file_descriptor(content)]))
        output = io.StringIO()
        with patch.dict(os.environ, {"WORKSPACE_PERF_OBSERVATIONS": "1"}), redirect_stderr(output):
            uploaded = self.upload(content)
        self.assertEqual(uploaded.status_code, 201, uploaded.content)
        records = [json.loads(line) for line in output.getvalue().splitlines() if line.startswith('{"schema":')]
        self.assertEqual(sum(row["value"] for row in records if row["metric"] == "apiReceivedBytes"),
                         4 + len(metadata) + len(content))
        self.assertEqual(sum(row["value"] for row in records if row["metric"] == "apiWrittenBytes"), len(content))
        self.assertTrue(records)
        self.assertTrue(all(row["trigger"] == "workspaceFileUpload" and row["source"] == "api"
                            and row["sessionId"] == self.session.pk and row["runId"] == self.run.pk
                            and row["jobId"] == self.job_id and row["outcome"] == "ok" for row in records))
        self.assertEqual(len({row["operationId"] for row in records}), 1)

    def test_batch_download_preserves_order_repeated_content_and_empty_files(self):
        first, last = b"shared object", b"different body"
        self.store_files([first, last, b""])
        committed, snapshot, _ = self.snapshot_upload(document([
            ("a", first, False), ("copy", first, True), ("empty", b"", False), ("z", last, False)]))
        self.assertEqual(committed.status_code, 201, committed.content)
        requested = [last, first, b"", first]
        response, received = self.download_batch(requested, snapshot, identity=self.new_reader())
        self.assertEqual(response.status_code, 200, received)
        self.assertEqual(response["Content-Length"], str(sum(map(len, requested))))
        self.assertEqual(received, b"".join(requested))

    def test_missing_request_respects_the_current_workspace_budget(self):
        budget = self.authorization.payload["environmentProfile"]["resources"]["workspaceBytes"]
        response = self.missing(files=[{"sha256": "sha256:" + "e" * 64, "sizeBytes": budget + 1}])
        self.assertEqual(response.status_code, 400, response.content)
        self.assertEqual(response.json()["error"], "workspace_file_budget_exceeded")
        self.assertEqual(WorkspaceFileObject.objects.count(), 0)
