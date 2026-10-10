"""Exercise snapshot publication across real, independently committed lease changes."""

import hashlib
import io
import json
import os
import tempfile
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from threading import Event, current_thread
from unittest.mock import patch

from asgiref.sync import async_to_sync
from django.conf import settings
from django.contrib.auth.models import User
from django.core.files.base import ContentFile
from django.core.files.storage import default_storage
from django.core.management import call_command
from django.core.management.base import CommandError
from django.db import connection, connections
from django.test import Client, TransactionTestCase, override_settings
from django.utils import timezone

from .agent_run_authorization_factory import create_agent_run_authorization
from .http import internal, workspaces
from .models import Agent, AgentRun, ModelConfig, StorageCleanupRetry, Workspace
from .testing import create_session


class ExecutionPublicationFixture:
    def setUp(self):
        super().setUp()
        storage = tempfile.TemporaryDirectory(prefix="execution-publication-")
        self.storage_root = Path(storage.name)
        self.addCleanup(storage.cleanup)
        self.enterContext(override_settings(MEDIA_ROOT=storage.name))
        user = User.objects.create_user(username="publication-owner")
        workspace = Workspace.objects.create(name="Publication fencing", createdBy=user)
        workspace.members.add(user)
        model = ModelConfig.objects.create(id="publication-model", displayName="Publication")
        self.session = create_session(workspace=workspace, owner=user)
        self.run = AgentRun.objects.create(
            workspace=workspace, session=self.session, user=user,
            modelConfig=model, prompt="persist files", status="running",
        )
        self.authorization = create_agent_run_authorization(
            self.run, image_digest="sha256:" + "a" * 64,
        )
        self.job_id = f"agent_run.lifecycle:{self.run.id}"
        self.owner = "worker:publication-first"
        self.replacement = "worker:publication-replacement"
        with connection.cursor() as cursor:
            # The API test database has no Runtime migrations. Match the lease
            # fields read by this API, as in the existing internal CAS fixture.
            cursor.execute("CREATE SCHEMA IF NOT EXISTS runtime")
            cursor.execute(
                "CREATE TABLE IF NOT EXISTS runtime.runtime_jobs("
                "job_id text PRIMARY KEY, job_kind text NOT NULL, status text NOT NULL, "
                "lease_owner text, lease_expires_at_ms bigint, idempotency_key text NOT NULL, "
                "session_id text, payload_ref text)"
            )
            cursor.execute(
                "INSERT INTO runtime.runtime_jobs(job_id,job_kind,status,lease_owner,"
                "lease_expires_at_ms,idempotency_key,session_id,payload_ref) "
                "VALUES(%s,'agent_run.lifecycle','running',%s,"
                "(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint+60000,%s,%s,%s)",
                [self.job_id, self.owner, f"{self.job_id}:{self.authorization.digest}",
                 self.session.id, f"record:agent_run:{self.run.id}"],
            )
            cursor.execute("SELECT pg_backend_pid()")
            self.main_connection_pid = cursor.fetchone()[0]
        self.addCleanup(self._delete_job)

    def _delete_job(self):
        with connection.cursor() as cursor:
            cursor.execute("DELETE FROM runtime.runtime_jobs WHERE job_id=%s", [self.job_id])

    def _lease(self, owner, *, expired=False):
        with connection.cursor() as cursor:
            cursor.execute(
                "UPDATE runtime.runtime_jobs SET lease_owner=%s, lease_expires_at_ms="
                "(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint+%s WHERE job_id=%s",
                [owner, -1 if expired else 60000, self.job_id],
            )

    def _body(self, content=b"AAA", *, owner=None):
        manifest = json.dumps({
            "schema": "workspace.snapshot.v1",
            "files": [{"path": "notes.txt", "sizeBytes": len(content),
                       "sha256": "sha256:" + hashlib.sha256(content).hexdigest(),
                       "executable": False}],
        }, separators=(",", ":")).encode()
        snapshot = len(manifest).to_bytes(4, "big") + manifest + content
        metadata = json.dumps({
            "schema": "runtime.session_workspace.commit.v1",
            "jobId": self.job_id, "leaseOwner": owner or self.owner,
            "agentRunId": self.run.id, "authorizationDigest": self.authorization.digest,
            "snapshotSha256": "sha256:" + hashlib.sha256(snapshot).hexdigest(),
            "snapshotSizeBytes": len(snapshot), "expandedSizeBytes": len(content),
            "fileCount": 1,
        }, separators=(",", ":")).encode()
        return len(metadata).to_bytes(4, "big") + metadata + snapshot, snapshot

    def _post(self, body):
        return Client().post(
            "/internal/agent-runs/session-workspace/commit", data=body,
            content_type="application/octet-stream",
            HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN,
        )

    def _assert_snapshot(self, expected):
        self.session.refresh_from_db()
        self.assertEqual(self.session.workspaceGeneration, 1)
        self.assertEqual(self.session.workspaceLastAdvancedAgentRun_id, self.run.id)
        self.assertTrue(default_storage.exists(self.session.workspaceStorageKey))
        with default_storage.open(self.session.workspaceStorageKey, "rb") as stored:
            self.assertEqual(stored.read(), expected)


class ExecutionPublicationFencingTests(ExecutionPublicationFixture, TransactionTestCase):
    serialized_rollback = True






    def _thread_post(self, body):
        try:
            with connection.cursor() as cursor:
                cursor.execute("SELECT pg_backend_pid()")
                self.assertNotEqual(cursor.fetchone()[0], self.main_connection_pid)
            return self._post(body)
        finally:
            connections.close_all()

    def _during_upload(self, change, *, body=None):
        """Let storage finish, change committed facts, then allow publication."""
        uploaded, release = Event(), Event()
        real_store = internal._store_workspace_snapshot
        caller = current_thread()

        def paused_store(*args):
            real_store(*args)
            if current_thread() is not caller:
                uploaded.set()
                if not release.wait(10):
                    raise TimeoutError("Snapshot publication test was not released")

        with patch.object(internal, "_store_workspace_snapshot", paused_store):
            with ThreadPoolExecutor(max_workers=1) as executor:
                pending = executor.submit(self._thread_post, body if body is not None else self._body()[0])
                try:
                    self.assertTrue(uploaded.wait(5), "Snapshot did not finish uploading")
                    change()
                finally:
                    release.set()
                return pending.result(timeout=10)

    def _assert_error(self, response, error):
        self.assertEqual(response.status_code, 409, response.content)
        self.assertEqual(response.json(), {"error": error})


    def _assert_no_upload_temporaries(self):
        self.assertEqual(list(self.storage_root.rglob(".centaeris-immutable-*.tmp")), [])

    def _snapshot_key(self, snapshot):
        return (
            f"workspaces/{self.run.workspace_id}/sessions/{self.session.id}/snapshots/1/"
            f"{hashlib.sha256(snapshot).hexdigest()}.snapshot"
        )

    def test_replaced_lease_rejects_late_upload_and_new_owner_can_publish_same_run(self):
        replacement_body, replacement_snapshot = self._body(b"BBB", owner=self.replacement)

        def replace_and_publish():
            self._lease(self.replacement)
            committed = self._post(replacement_body)
            self.assertEqual(committed.status_code, 201, committed.content)

        stale = self._during_upload(replace_and_publish)
        self._assert_error(stale, "session_workspace_lease_lost")
        self._assert_snapshot(replacement_snapshot)
        self.authorization.refresh_from_db()
        self.assertEqual(self.authorization.payload["sessionWorkspace"]["generation"], 0)

    def test_lease_expiry_after_upload_prevents_publication(self):
        stale = self._during_upload(lambda: self._lease(self.owner, expired=True))
        self._assert_error(stale, "session_workspace_lease_lost")
        self.session.refresh_from_db()
        self.assertEqual(self.session.workspaceGeneration, 0)
        self.assertEqual(self.session.workspaceStorageKey, "")

    def test_changed_baseline_after_upload_prevents_overwrite(self):
        replacement_body, replacement_snapshot = self._body(b"BBB")

        def publish_other_snapshot():
            committed = self._post(replacement_body)
            self.assertEqual(committed.status_code, 201, committed.content)

        stale = self._during_upload(publish_other_snapshot)
        self._assert_error(stale, "session_workspace_baseline_conflict")
        self._assert_snapshot(replacement_snapshot)

    def test_terminal_run_after_upload_prevents_publication(self):
        stale = self._during_upload(
            lambda: AgentRun.objects.filter(id=self.run.id).update(status="completed")
        )
        self._assert_error(stale, "session_workspace_session_unavailable")
        self.session.refresh_from_db()
        self.run.refresh_from_db()
        self.assertEqual(self.session.workspaceGeneration, 0)
        self.assertEqual(self.run.status, "completed")

    def _purge_session(self):
        now = timezone.now()
        type(self.session).objects.filter(id=self.session.id).update(
            status="deleted", deletedAt=now, purgedAt=now,
        )

    def _purge_agent(self):
        now = timezone.now()
        Agent.objects.filter(id=self.session.agent_id).update(
            status="deleted", deletedAt=now, purgedAt=now,
        )

    def test_parent_purge_during_snapshot_upload_fences_live_lease(self):
        self._parent_purge_during_upload(checkpoint=False)

    def test_parent_purge_during_checkpoint_upload_fences_live_lease(self):
        self._parent_purge_during_upload(checkpoint=True)

    def _parent_purge_during_upload(self, *, checkpoint):
        body, snapshot = self._body()
        checkpoint_id = "checkpoint:parent-purge"
        if checkpoint:
            metadata_size = int.from_bytes(body[:4], "big")
            metadata = json.loads(body[4:4 + metadata_size])
            metadata.update(schema="runtime.execution_workspace.stage.v1", checkpointId=checkpoint_id)
            encoded = json.dumps(metadata, separators=(",", ":")).encode()
            body = len(encoded).to_bytes(4, "big") + encoded + snapshot
        retained_key = (
            f"workspaces/{self.run.workspace_id}/sessions/{self.session.id}/snapshots/2/"
            f"{hashlib.sha256(b'previous snapshot').hexdigest()}.snapshot"
        )
        default_storage.save(retained_key, ContentFile(b"previous snapshot"))
        reports = []

        def purge_and_collect():
            self._purge_agent()
            output = io.StringIO()
            call_command("gc_deleted_resources", older_than_seconds=0, stdout=output)
            self.assertTrue(default_storage.exists(retained_key))
            reports.append(output.getvalue())

        original_post = self._post
        if checkpoint:
            self._post = lambda payload: Client().post(
                "/internal/agent-runs/execution-workspace/stage", data=payload,
                content_type="application/octet-stream", HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN,
            )
        try:
            stale = self._during_upload(purge_and_collect, body=body)
            self._assert_error(stale, "session_workspace_session_unavailable")
            self.assertIn(f"Blocked workspace snapshot key {retained_key}", reports[0])
            self._assert_error(self._post(body), "session_workspace_session_unavailable")
        finally:
            self._post = original_post
        self.session.refresh_from_db()
        self.assertEqual(self.session.status, "active")
        self.assertIsNone(self.session.purgedAt)
        self.assertEqual(self.session.workspaceGeneration, 0)
        self.assertEqual(list(self.storage_root.rglob("*.snapshot")), [self.storage_root / retained_key])
        self._assert_no_upload_temporaries()
        AgentRun.objects.filter(id=self.run.id).update(status="completed")
        call_command("gc_deleted_resources", older_than_seconds=0, stdout=io.StringIO())
        self.assertFalse(default_storage.exists(retained_key))

    def test_purged_session_during_upload_cannot_create_final_snapshot(self):
        stale = self._during_upload(self._purge_session)
        self._assert_error(stale, "session_workspace_session_unavailable")
        self.session.refresh_from_db()
        self.assertEqual(self.session.workspaceGeneration, 0)
        self.assertEqual(list(self.storage_root.rglob("*.snapshot")), [])
        self._assert_no_upload_temporaries()

    def test_checkpoint_stage_purged_during_upload_is_rejected_without_final_bytes(self):
        body, _snapshot = self._body()
        metadata_size = int.from_bytes(body[:4], "big")
        metadata = json.loads(body[4:4 + metadata_size])
        metadata.update(
            schema="runtime.execution_workspace.stage.v1", checkpointId="checkpoint:purge-race",
        )
        encoded = json.dumps(metadata, separators=(",", ":")).encode()
        stage_body = len(encoded).to_bytes(4, "big") + encoded + body[4 + metadata_size:]
        original_post = self._post

        def post_stage(_body):
            return Client().post(
                "/internal/agent-runs/execution-workspace/stage", data=stage_body,
                content_type="application/octet-stream",
                HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN,
            )

        self._post = post_stage
        try:
            stale = self._during_upload(self._purge_session)
        finally:
            self._post = original_post
        self._assert_error(stale, "session_workspace_session_unavailable")
        self.assertEqual(list(self.storage_root.rglob("*.snapshot")), [])
        self._assert_no_upload_temporaries()

    def test_purge_and_gc_during_snapshot_payload_reject_publication(self):
        self._purge_and_gc_during_payload(checkpoint=False)

    def test_purge_and_gc_during_checkpoint_payload_reject_publication(self):
        self._purge_and_gc_during_payload(checkpoint=True)

    def test_purge_and_gc_during_recent_snapshot_payload_blocks_temporary(self):
        self._purge_and_gc_during_payload(checkpoint=False, recent_temporary=True)

    def _purge_and_gc_during_payload(self, *, checkpoint, recent_temporary=False):
        from . import workspace_snapshot_gc
        from .management.commands import gc_deleted_resources

        body, snapshot = self._body(b"A" * (256 * 1024))
        if checkpoint:
            metadata_size = int.from_bytes(body[:4], "big")
            metadata = json.loads(body[4:4 + metadata_size])
            metadata.update(
                schema="runtime.execution_workspace.stage.v1",
                checkpointId="checkpoint:payload-purge-race",
            )
            encoded = json.dumps(metadata, separators=(",", ":")).encode()
            body = len(encoded).to_bytes(4, "big") + encoded + snapshot

        copying, release = Event(), Event()
        real_read = internal._WorkspaceSnapshotReader.read
        reads = 0

        def paused_read(reader, size=-1):
            nonlocal reads
            reads += 1
            if reads == 2:
                copying.set()
                if not release.wait(10):
                    raise TimeoutError("In-flight snapshot upload was not released")
            return real_read(reader, size)

        def upload():
            try:
                endpoint = (
                    "/internal/agent-runs/execution-workspace/stage" if checkpoint
                    else "/internal/agent-runs/session-workspace/commit"
                )
                return Client().post(
                    endpoint, data=body, content_type="application/octet-stream",
                    HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN,
                )
            finally:
                connections.close_all()

        with patch.object(internal._WorkspaceSnapshotReader, "read", paused_read):
            with ThreadPoolExecutor(max_workers=1) as executor:
                pending = executor.submit(upload)
                try:
                    self.assertTrue(copying.wait(5))
                    temporaries = list(self.storage_root.rglob(".centaeris-immutable-*.tmp"))
                    self.assertEqual(len(temporaries), 1)
                    self.assertGreater(temporaries[0].stat().st_size, 0)
                    self._purge_session()
                    gc_state = {"key": temporaries[0].relative_to(self.storage_root).as_posix()}
                    real_collect = gc_deleted_resources.collect_workspace_snapshot_gc

                    def collect(cutoff, dry_run):
                        # Establish retention eligibility against this command's
                        # actual cutoff, independently of host timestamp timing.
                        modified = cutoff.timestamp() + (60 if recent_temporary else -60)
                        os.utime(temporaries[0], (modified, modified))
                        self.session.refresh_from_db()
                        metadata = temporaries[0].stat()
                        gc_state.update(cutoff=cutoff.isoformat(), mtimeNs=metadata.st_mtime_ns,
                                        purgedAt=self.session.purgedAt, status=self.session.status,
                                        copyingSet=copying.is_set(), releaseSet=release.is_set(), uploadDone=pending.done())
                        self.assertEqual(self.session.status, "deleted", gc_state)
                        self.assertIsNotNone(self.session.purgedAt, gc_state)
                        self.assertLessEqual(self.session.purgedAt, cutoff, gc_state)
                        self.assertEqual(metadata.st_mtime > cutoff.timestamp(), recent_temporary, gc_state)
                        self.assertTrue(copying.is_set(), gc_state)
                        self.assertFalse(release.is_set(), gc_state)
                        self.assertFalse(pending.done(), gc_state)
                        report = real_collect(cutoff, dry_run)
                        gc_state["report"] = {name: getattr(report, name)
                                              for name in ("planned", "cleaned", "blocked", "failures")}
                        return report

                    output = io.StringIO()
                    with patch.object(workspace_snapshot_gc, "delete_stored_object_for_gc",
                                      wraps=workspace_snapshot_gc.delete_stored_object_for_gc) as delete:
                        try:
                            with patch.object(gc_deleted_resources, "collect_workspace_snapshot_gc", collect):
                                call_command("gc_deleted_resources", older_than_seconds=0, stdout=output)
                        except CommandError as error:
                            gc_state.update(output=output.getvalue(), deleteCalls=delete.call_args_list)
                            self.assertFalse(recent_temporary, gc_state)
                            self.assertEqual(os.name, "nt", gc_state)
                            self.assertIn("Workspace snapshot GC failed", str(error), gc_state)
                            self.assertEqual(delete.call_args_list, [((gc_state["key"],), {})], gc_state)
                            self.assertEqual(gc_state["report"]["blocked"], [], gc_state)
                            self.assertEqual(len(gc_state["report"]["failures"]), 1, gc_state)
                            self.assertTrue(gc_state["report"]["failures"][0].startswith(gc_state["key"] + ": "), gc_state)
                        else:
                            gc_state.update(output=output.getvalue(), deleteCalls=delete.call_args_list)
                            self.assertEqual(gc_state["report"]["failures"], [], gc_state)
                            if recent_temporary:
                                self.assertEqual(gc_state["report"]["blocked"], [gc_state["key"]], gc_state)
                                self.assertEqual(gc_state["report"]["cleaned"], [], gc_state)
                                self.assertEqual(delete.call_count, 0, gc_state)
                                self.assertTrue(temporaries[0].exists(), gc_state)
                            else:
                                self.assertEqual(gc_state["report"]["blocked"], [], gc_state)
                                self.assertEqual(gc_state["report"]["cleaned"], [gc_state["key"]], gc_state)
                                self.assertEqual(delete.call_args_list, [((gc_state["key"],), {})], gc_state)
                                self.assertFalse(temporaries[0].exists(), gc_state)
                finally:
                    release.set()
                rejected = pending.result(timeout=10)
        gc_state.update(releaseSet=release.is_set(), uploadDone=pending.done())
        self.assertEqual(rejected.status_code, 409, {**gc_state, "response": rejected.content})
        self.assertEqual(rejected.json(), {"error": "session_workspace_session_unavailable"}, gc_state)
        self.assertEqual(list(self.storage_root.rglob("*.snapshot")), [], gc_state)
        self.assertEqual(list(self.storage_root.rglob(".centaeris-immutable-*.tmp")), [], gc_state)
        call_command("gc_deleted_resources", older_than_seconds=0, stdout=io.StringIO())
        self.assertEqual(list(self.storage_root.rglob("*.snapshot")), [], gc_state)
        self.assertEqual(list(self.storage_root.rglob(".centaeris-immutable-*.tmp")), [], gc_state)

    def test_existing_snapshot_recheck_racing_purge_and_gc_is_controlled(self):
        body, _snapshot = self._body()
        self.assertEqual(self._post(body).status_code, 201)
        self.session.refresh_from_db()
        key = self.session.workspaceStorageKey
        opening, release = Event(), Event()
        real_open = default_storage.open
        caller = current_thread()

        def paused_open(storage_key, mode):
            if storage_key == key and current_thread() is not caller:
                opening.set()
                if not release.wait(10):
                    raise TimeoutError("Existing snapshot read was not released")
            return real_open(storage_key, mode)

        with patch.object(default_storage, "open", side_effect=paused_open):
            with ThreadPoolExecutor(max_workers=1) as executor:
                pending = executor.submit(self._thread_post, body)
                try:
                    self.assertTrue(opening.wait(5))
                    self._purge_session()
                    try:
                        call_command("gc_deleted_resources", older_than_seconds=0, stdout=io.StringIO())
                    except CommandError as error:
                        # Windows can retain the open upload temporary until
                        # the request exits; that failure remains retryable.
                        self.assertIn("Workspace snapshot GC failed", str(error))
                    self.assertFalse(default_storage.exists(key))
                finally:
                    release.set()
                stale = pending.result(timeout=10)
        self._assert_error(stale, "session_workspace_session_unavailable")
        call_command("gc_deleted_resources", older_than_seconds=0, stdout=io.StringIO())
        self._assert_no_upload_temporaries()

    def _download_body(self):
        body, snapshot = self._body()
        self.assertEqual(self._post(body).status_code, 201)
        self.session.refresh_from_db()
        self.authorization.delete()
        self.authorization = create_agent_run_authorization(
            self.run, image_digest="sha256:" + "a" * 64,
        )
        with connection.cursor() as cursor:
            cursor.execute(
                "UPDATE runtime.runtime_jobs SET idempotency_key=%s WHERE job_id=%s",
                [f"{self.job_id}:{self.authorization.digest}", self.job_id],
            )
        return {
            "schema": "runtime.session_workspace.download.v1",
            "jobId": self.job_id, "leaseOwner": self.owner,
            "agentRunId": self.run.id, "authorizationDigest": self.authorization.digest,
        }, snapshot

    def _checkpoint_download_body(self):
        body, snapshot = self._body()
        metadata_size = int.from_bytes(body[:4], "big")
        metadata = json.loads(body[4:4 + metadata_size])
        checkpoint_id = "checkpoint:download-purge-race"
        metadata.update(schema="runtime.execution_workspace.stage.v1", checkpointId=checkpoint_id)
        encoded = json.dumps(metadata, separators=(",", ":")).encode()
        staged = Client().post(
            "/internal/agent-runs/execution-workspace/stage",
            data=len(encoded).to_bytes(4, "big") + encoded + snapshot,
            content_type="application/octet-stream",
            HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN,
        )
        self.assertEqual(staged.status_code, 201, staged.content)
        stored_snapshot = {name: staged.json()[name] for name in (
            "objectRef", "snapshotSha256", "snapshotSizeBytes", "expandedSizeBytes", "fileCount",
        )}
        checkpoint = {
            "schema": "runtime.recovery_checkpoint.v1", "checkpointId": checkpoint_id,
            "sessionId": self.session.id, "agentRunId": self.run.id,
            "executionId": "execution:download-purge-race",
            "authorizationDigest": self.authorization.digest, "sessionSequence": 1,
            "modelRequestId": "request:download-purge-race",
            "workspaceSnapshot": stored_snapshot, "createdAtMs": 1,
        }
        with connection.cursor() as cursor:
            cursor.execute(
                "CREATE TABLE IF NOT EXISTS runtime.checkpoints("
                "checkpoint_id text PRIMARY KEY, kind text NOT NULL, "
                "session_id text NOT NULL, turn_id text NOT NULL, status text NOT NULL, "
                "done_reason text, updated_at_ms bigint NOT NULL, payload_json text NOT NULL)"
            )
            cursor.execute(
                "INSERT INTO runtime.checkpoints("
                "checkpoint_id,kind,session_id,turn_id,status,done_reason,updated_at_ms,payload_json) "
                "VALUES(%s,'recovery',%s,%s,'committed',NULL,1,%s)",
                [checkpoint_id, self.session.id, self.run.turn_id,
                 json.dumps(checkpoint, separators=(",", ":"))],
            )

        def delete_checkpoint():
            with connection.cursor() as cursor:
                cursor.execute("DELETE FROM runtime.checkpoints WHERE checkpoint_id=%s", [checkpoint_id])

        self.addCleanup(delete_checkpoint)
        return {
            "schema": "runtime.execution_workspace.download.v1", "checkpointId": checkpoint_id,
            "jobId": self.job_id, "leaseOwner": self.owner,
            "agentRunId": self.run.id, "authorizationDigest": self.authorization.digest,
        }, snapshot, stored_snapshot["objectRef"]

    def _thread_download(self, body, *, checkpoint=False):
        try:
            endpoint = (
                "/internal/agent-runs/execution-workspace/download" if checkpoint
                else "/internal/agent-runs/session-workspace/download"
            )
            return Client().post(
                endpoint, data=body,
                content_type="application/json", HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN,
            )
        finally:
            connections.close_all()

    def test_snapshot_download_opens_before_concurrent_purge_can_finish(self):
        body, snapshot = self._download_body()
        self._download_open_purge_race(body, snapshot, self.session.workspaceStorageKey)

    def test_checkpoint_download_opens_before_concurrent_purge_can_finish(self):
        body, snapshot, key = self._checkpoint_download_body()
        self._download_open_purge_race(body, snapshot, key, checkpoint=True)

    def test_snapshot_download_opens_before_parent_purge_can_finish(self):
        body, snapshot = self._download_body()
        self._download_open_purge_race(body, snapshot, self.session.workspaceStorageKey, parent=True)

    def test_checkpoint_download_opens_before_parent_purge_can_finish(self):
        body, snapshot, key = self._checkpoint_download_body()
        self._download_open_purge_race(body, snapshot, key, checkpoint=True, parent=True)

    def _download_open_purge_race(self, body, snapshot, key, *, checkpoint=False, parent=False):
        opening, release, purging = Event(), Event(), Event()
        real_open = default_storage.open
        response = None

        def paused_open(storage_key, mode):
            if storage_key == key:
                opening.set()
                if not release.wait(10):
                    raise TimeoutError("Snapshot download open was not released")
            return real_open(storage_key, mode)

        def purge():
            try:
                purging.set()
                self._purge_agent() if parent else self._purge_session()
            finally:
                connections.close_all()

        try:
            with patch.object(default_storage, "open", side_effect=paused_open):
                with ThreadPoolExecutor(max_workers=2) as executor:
                    download = executor.submit(self._thread_download, body, checkpoint=checkpoint)
                    try:
                        self.assertTrue(opening.wait(5))
                        deletion = executor.submit(purge)
                        self.assertTrue(purging.wait(5))
                        try:
                            deletion.result(timeout=0.2)
                            waited_for_open = False
                        except TimeoutError:
                            waited_for_open = True
                    finally:
                        release.set()
                    response = download.result(timeout=10)
                    deletion.result(timeout=10)
            self.assertTrue(waited_for_open, "Purge passed authorization before the file was opened")
            self.assertEqual(response.status_code, 200, getattr(response, "content", b""))
            try:
                call_command("gc_deleted_resources", older_than_seconds=0, stdout=io.StringIO())
            except CommandError as error:
                self.assertIn("Workspace snapshot GC failed", str(error))

            async def consume():
                return b"".join([chunk async for chunk in response.streaming_content])

            self.assertEqual(async_to_sync(consume)(), snapshot)
        finally:
            if response is not None:
                response.close()
        if parent:
            AgentRun.objects.filter(id=self.run.id).update(status="completed")
        retry = StorageCleanupRetry.objects.filter(storageKey=key, state="failed").first()
        if retry is not None:
            self.assertIsNotNone(retry.nextCleanupAt)
            self.assertGreater(retry.nextCleanupAt, timezone.now())
            with patch("django.utils.timezone.now", return_value=retry.nextCleanupAt):
                call_command("gc_deleted_resources", older_than_seconds=0, stdout=io.StringIO())
        else:
            call_command("gc_deleted_resources", older_than_seconds=0, stdout=io.StringIO())
        self.assertFalse(default_storage.exists(key))
        self._assert_error(
            self._thread_download(body, checkpoint=checkpoint), "session_workspace_session_unavailable",
        )

    def test_public_session_delete_and_snapshot_upload_do_not_deadlock(self):
        deleting, release_delete = Event(), Event()
        real_owned_session = workspaces._locked_owned_session
        user = self.run.user

        def paused_owned_session(*args):
            result = real_owned_session(*args)
            deleting.set()
            if not release_delete.wait(10):
                raise TimeoutError("Session deletion was not released")
            return result

        def delete():
            try:
                client = Client()
                client.force_login(user)
                return client.delete(f"/api/sessions/{self.session.id}")
            finally:
                connections.close_all()

        with (
            patch.object(workspaces, "_locked_owned_session", paused_owned_session),
            patch.object(workspaces, "request_agent_run_cancellation", return_value={"disposition": "requested"}),
        ):
            with ThreadPoolExecutor(max_workers=2) as executor:
                deletion = executor.submit(delete)
                try:
                    self.assertTrue(deleting.wait(5))
                    upload = executor.submit(self._thread_post, self._body()[0])
                    waiting = False
                    deadline = time.monotonic() + 5
                    while time.monotonic() < deadline:
                        with connection.cursor() as cursor:
                            cursor.execute(
                                "SELECT EXISTS(SELECT 1 FROM pg_stat_activity "
                                "WHERE datname=current_database() AND wait_event_type='Lock' "
                                "AND (query LIKE '%%app_core_session%%' "
                                "OR query LIKE '%%app_core_agent%%'))"
                            )
                            waiting = cursor.fetchone()[0]
                        if waiting:
                            break
                        time.sleep(0.02)
                    self.assertTrue(waiting, "Upload did not reach the concurrent owner lock")
                finally:
                    release_delete.set()
                deleted = deletion.result(timeout=10)
                rejected = upload.result(timeout=10)
        self.assertEqual(deleted.status_code, 200, deleted.content)
        self._assert_error(rejected, "session_workspace_session_unavailable")
        self.assertEqual(list(self.storage_root.rglob("*.snapshot")), [])
        self._assert_no_upload_temporaries()

    def test_replay_keeps_one_generation_and_terminal_run_cannot_republish(self):
        body, snapshot = self._body()
        committed = self._post(body)
        self.assertEqual(committed.status_code, 201, committed.content)
        replay = self._post(body)
        self.assertEqual(replay.status_code, 200, replay.content)
        self.assertEqual(replay.json()["disposition"], "idempotent")
        self._assert_snapshot(snapshot)
        AgentRun.objects.filter(id=self.run.id).update(status="completed")
        self._assert_error(self._post(body), "session_workspace_session_unavailable")
        self._assert_snapshot(snapshot)

    def test_stale_lease_cannot_start_upload(self):
        self._lease(self.replacement)
        with patch.object(internal, "_store_workspace_snapshot") as upload:
            self._assert_error(self._post(self._body()[0]), "session_workspace_lease_lost")
        upload.assert_not_called()

    def test_replaced_lease_identical_upload_cannot_delete_committed_snapshot(self):
        self._competing_identical_upload()

    def test_invalid_stale_upload_cannot_delete_replacement_snapshot(self):
        self._competing_identical_upload(invalid_content=True)

    def _competing_identical_upload(self, *, invalid_content=False):
        # Both requests have passed the absent-object check. The first request
        # pauses after observing absence; the second publishes that same
        # content-addressed key under a new lease. Stale cleanup must not delete
        # the winner even though the stale request began while its lease held.
        observed_absence, release = Event(), Event()
        real_exists = default_storage.exists
        caller = current_thread()

        def paused_exists(*args, **kwargs):
            exists = real_exists(*args, **kwargs)
            if current_thread() is not caller and not observed_absence.is_set():
                self.assertFalse(exists)
                observed_absence.set()
                if not release.wait(10):
                    raise TimeoutError("Competing snapshot upload was not released")
            return exists

        body, snapshot = self._body()
        competing_body = body[:-1] + b"X" if invalid_content else body
        with patch.object(default_storage, "exists", paused_exists):
            with ThreadPoolExecutor(max_workers=1) as executor:
                pending = executor.submit(self._thread_post, competing_body)
                try:
                    self.assertTrue(observed_absence.wait(5), "Upload did not check storage")
                    self._lease(self.owner, expired=True)
                    self._lease(self.replacement)
                    committed = self._post(self._body(owner=self.replacement)[0])
                    self.assertEqual(committed.status_code, 201, committed.content)
                    self._assert_snapshot(snapshot)
                finally:
                    release.set()
                competing = pending.result(timeout=10)
        if invalid_content:
            self.assertEqual(competing.status_code, 400, competing.content)
            self.assertEqual(competing.json(), {"error": "session_workspace_snapshot_invalid"})
        else:
            self._assert_error(competing, "session_workspace_lease_lost")
        self._assert_snapshot(snapshot)
        self._assert_no_upload_temporaries()

    def test_truncated_upload_leaves_no_published_object_and_valid_retry_succeeds(self):
        body, snapshot = self._body()
        self._invalid_upload_then_retry(body[:-1], body, snapshot)

    def test_trailing_bytes_leave_no_published_object_and_valid_retry_succeeds(self):
        body, snapshot = self._body()
        self._invalid_upload_then_retry(body + b"unexpected", body, snapshot)

    def test_corrupt_upload_leaves_no_published_object_and_same_candidate_retry_succeeds(self):
        body, snapshot = self._body()
        # Preserve both declared and actual length so validation reaches the
        # streamed snapshot rather than rejecting the metadata envelope.
        self._invalid_upload_then_retry(
            body[:-1] + b"X", body, snapshot,
            error="session_workspace_snapshot_invalid",
        )

    def _invalid_upload_then_retry(
        self, invalid_body, valid_body, snapshot, *, error="session_workspace_commit_invalid"
    ):
        invalid = self._post(invalid_body)
        self.assertEqual(invalid.status_code, 400, invalid.content)
        self.assertEqual(invalid.json(), {"error": error})
        self.session.refresh_from_db()
        self.assertEqual(self.session.workspaceGeneration, 0)
        storage_key = self._snapshot_key(snapshot)
        self.assertFalse(default_storage.exists(storage_key))
        self._assert_no_upload_temporaries()
        committed = self._post(valid_body)
        self.assertEqual(committed.status_code, 201, committed.content)
        self._assert_snapshot(snapshot)
        self._assert_no_upload_temporaries()

    def test_interrupted_upload_cleans_partial_bytes_and_valid_retry_succeeds(self):
        body, snapshot = self._body(b"A" * (128 * 1024))
        real_read = internal._WorkspaceSnapshotReader.read
        chunks_read = 0

        def interrupted_read(reader, size=-1):
            nonlocal chunks_read
            if chunks_read:
                raise OSError("Simulated upload disconnect after one chunk")
            chunk = real_read(reader, size)
            self.assertTrue(chunk)
            self.assertGreater(reader.remaining, 0)
            chunks_read += 1
            return chunk

        with patch.object(internal._WorkspaceSnapshotReader, "read", interrupted_read):
            with self.assertLogs(internal.logger, level="ERROR"):
                failed = self._post(body)
        self.assertEqual(failed.status_code, 500, failed.content)
        self.assertEqual(failed.json(), {"error": "session_workspace_commit_failed"})
        self.assertEqual(chunks_read, 1)
        self.session.refresh_from_db()
        self.assertEqual(self.session.workspaceGeneration, 0)
        self.assertFalse(default_storage.exists(self._snapshot_key(snapshot)))
        self._assert_no_upload_temporaries()
        committed = self._post(body)
        self.assertEqual(committed.status_code, 201, committed.content)
        self._assert_snapshot(snapshot)
        self._assert_no_upload_temporaries()

    def test_existing_corrupt_snapshot_is_rejected_without_deleting_or_overwriting_it(self):
        body, snapshot = self._body()
        key = self._snapshot_key(snapshot)
        corrupt = snapshot[:-1] + b"X"
        self.assertEqual(default_storage.save(key, ContentFile(corrupt)), key)
        self._assert_error(self._post(body), "session_workspace_snapshot_invalid")
        # Also cover another writer occupying the key after the absence check.
        with patch.object(default_storage, "exists", return_value=False):
            self._assert_error(self._post(body), "session_workspace_snapshot_invalid")
        with default_storage.open(key, "rb") as stored:
            self.assertEqual(stored.read(), corrupt)
        self.session.refresh_from_db()
        self.assertEqual(self.session.workspaceGeneration, 0)
        self._assert_no_upload_temporaries()

    def test_execution_checkpoint_stage_stores_immutable_bytes_without_publishing_session(self):
        body, snapshot = self._body()
        metadata_size = int.from_bytes(body[:4], "big")
        metadata = json.loads(body[4:4 + metadata_size])
        metadata.update(schema="runtime.execution_workspace.stage.v1", checkpointId="checkpoint:stage-test")
        encoded = json.dumps(metadata, separators=(",", ":")).encode()
        stage_body = len(encoded).to_bytes(4, "big") + encoded + snapshot
        expected_key = (
            f"workspaces/{self.run.workspace_id}/sessions/{self.session.id}/"
            f"agent-runs/{self.run.id}/execution-checkpoints/"
            f"{hashlib.sha256(metadata['checkpointId'].encode()).hexdigest()}/"
            f"{hashlib.sha256(snapshot).hexdigest()}.snapshot"
        )
        for _ in range(2):
            staged = self.client.post(
                "/internal/agent-runs/execution-workspace/stage", data=stage_body,
                content_type="application/octet-stream",
                HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN,
            )
            self.assertEqual(staged.status_code, 201, staged.content)
            self.assertEqual(staged.json()["objectRef"], expected_key)
            with default_storage.open(expected_key, "rb") as stored:
                self.assertEqual(stored.read(), snapshot)
        self.session.refresh_from_db()
        self.assertEqual(self.session.workspaceGeneration, 0)
        self.assertEqual(self.session.workspaceStorageKey, "")
        self._assert_no_upload_temporaries()
