"""Deleted workspace bytes are reclaimed without broadening resource ownership."""

from datetime import timedelta
import hashlib
import io
import os
from pathlib import Path
import subprocess
import tempfile
from unittest.mock import patch

from django.contrib.auth import get_user_model
from django.core.files.base import ContentFile
from django.core.files.storage import default_storage
from django.core.management import call_command
from django.core.management.base import CommandError
from django.test import TestCase, override_settings
from django.utils import timezone

from .models import (
    Agent,
    AgentRun,
    Artifact,
    ModelConfig,
    Session,
    SessionEvent,
    TranscriptOutputCapture,
    TranscriptOutputChunk,
    UserLibraryObject,
    Workspace,
    WorkspaceMembership,
)
from .testing import create_session


class WorkspaceSnapshotGcTests(TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory(prefix="workspace-snapshot-gc-")
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        storage = override_settings(MEDIA_ROOT=directory.name)
        storage.enable()
        self.addCleanup(storage.disable)
        self.now = timezone.now()
        self.old = self.now - timedelta(days=2)
        self.owner = get_user_model().objects.create_user(username="snapshot-gc-owner")
        self.workspace = Workspace.objects.create(name="Snapshot GC", createdBy=self.owner)
        WorkspaceMembership.objects.create(
            workspace=self.workspace, user=self.owner, role="owner",
        )
        self.model = ModelConfig.objects.create(displayName="Snapshot GC")
        self.session, self.run = self.create_owner(purged=True)

    def create_owner(self, *, workspace=None, purged=False, trash=False):
        workspace = workspace or self.workspace
        session = create_session(workspace=workspace, owner=self.owner)
        run = AgentRun.objects.create(
            workspace=workspace, session=session, user=self.owner,
            modelConfig=self.model, prompt="snapshot GC", status="completed",
        )
        if purged or trash:
            session.status = "deleted"
            session.deletedAt = self.old
            session.purgedAt = self.old if purged else None
            session.save(update_fields=["status", "deletedAt", "purgedAt", "updatedAt"])
        return session, run

    def store(self, key, content=b"snapshot bytes", *, modified=None):
        saved = default_storage.save(key, ContentFile(content))
        self.assertEqual(saved, key)
        timestamp = (modified or self.old).timestamp()
        os.utime(self.root / key, (timestamp, timestamp))
        return key

    def snapshot(self, *, session=None, run=None, generation=1,
                 content=b"snapshot bytes", current=False, modified=None):
        session = session or self.session
        run = run or self.run
        digest = hashlib.sha256(content).hexdigest()
        key = self.store(
            f"workspaces/{session.workspace_id}/sessions/{session.id}/"
            f"snapshots/{generation}/{digest}.snapshot",
            content, modified=modified,
        )
        if current:
            session.workspaceGeneration = generation
            session.workspaceStorageKey = key
            session.workspaceSnapshotSha256 = f"sha256:{digest}"
            session.workspaceSnapshotSizeBytes = len(content)
            session.workspaceExpandedSizeBytes = len(content)
            session.workspaceFileCount = 1
            session.workspaceLastAdvancedAgentRun = run
            session.save(update_fields=[
                "workspaceGeneration", "workspaceStorageKey", "workspaceSnapshotSha256",
                "workspaceSnapshotSizeBytes", "workspaceExpandedSizeBytes",
                "workspaceFileCount", "workspaceLastAdvancedAgentRun", "updatedAt",
            ])
        return key

    def checkpoint(self, *, session=None, run=None, content=b"snapshot bytes",
                   checkpoint_id="checkpoint:first", modified=None):
        session = session or self.session
        run = run or self.run
        checkpoint_digest = hashlib.sha256(checkpoint_id.encode()).hexdigest()
        digest = hashlib.sha256(content).hexdigest()
        return self.store(
            f"workspaces/{session.workspace_id}/sessions/{session.id}/"
            f"agent-runs/{run.id}/execution-checkpoints/{checkpoint_digest}/{digest}.snapshot",
            content, modified=modified,
        )

    def collect(self, *, dry_run=False, older_than_seconds=0):
        output = io.StringIO()
        call_command(
            "gc_deleted_resources", older_than_seconds=older_than_seconds,
            dry_run=dry_run, stdout=output,
        )
        return output.getvalue()

    def agent_children(self, *, trash=False):
        agent = Agent.objects.create(
            workspace=self.workspace, owner=self.owner, name="Parent GC",
        )
        children = []
        for deleted in (False, True):
            session = create_session(workspace=self.workspace, owner=self.owner, agent=agent)
            run = AgentRun.objects.create(
                workspace=self.workspace, session=session, user=self.owner,
                modelConfig=self.model, prompt="parent GC", status="completed",
            )
            if deleted:
                session.status = "deleted"
                session.deletedAt = self.old
                session.save(update_fields=["status", "deletedAt", "updatedAt"])
            children.append((session, run))
        if trash:
            agent.status = "deleted"
            agent.deletedAt = self.old
            agent.save(update_fields=["status", "deletedAt", "updatedAt"])
        return agent, children

    def child_keys(self, children):
        return [key for session, run in children for key in (
            self.snapshot(session=session, run=run, current=True),
            self.snapshot(session=session, run=run, generation=2, content=b"older child"),
            self.checkpoint(session=session, run=run),
        )]

    def test_parent_purge_reclaims_children_without_cascading_session_state(self):
        agent, children = self.agent_children()
        keys = self.child_keys(children)
        self.client.force_login(self.owner)
        with patch("app_core.http.agents.timezone.now", return_value=self.old):
            self.assertEqual(self.client.delete(f"/api/agents/{agent.id}").status_code, 200)
            response = self.client.delete(f"/api/agents/{agent.id}/trash")
        self.assertEqual(response.status_code, 200, response.content)
        before = list(Session.objects.filter(agent=agent).values())
        self.assertTrue(all(child["purgedAt"] is None for child in before))

        preview = self.collect(dry_run=True, older_than_seconds=24 * 60 * 60)

        for key in keys:
            self.assertIn(key, preview)
            self.assertTrue(default_storage.exists(key))
        self.collect(older_than_seconds=24 * 60 * 60)
        self.assertTrue(all(not default_storage.exists(key) for key in keys))
        self.assertEqual(list(Session.objects.filter(agent=agent).values()), before)
        self.assertIn("Cleaned 0 workspace snapshot keys", self.collect())

    def test_parent_purge_honors_both_purge_and_file_retention(self):
        agent, children = self.agent_children(trash=True)
        keys = self.child_keys(children)
        agent.purgedAt = self.now
        agent.save(update_fields=["purgedAt", "updatedAt"])
        self.collect(older_than_seconds=24 * 60 * 60)
        self.assertTrue(all(default_storage.exists(key) for key in keys))
        agent.purgedAt = self.old
        agent.save(update_fields=["purgedAt", "updatedAt"])
        recent = self.snapshot(
            session=children[0][0], run=children[0][1], generation=3,
            content=b"recent child", modified=self.now,
        )

        output = self.collect(older_than_seconds=24 * 60 * 60)

        self.assertTrue(all(not default_storage.exists(key) for key in keys))
        self.assertTrue(default_storage.exists(recent))
        self.assertIn(f"Blocked workspace snapshot key {recent}", output)
        self.collect()
        self.assertFalse(default_storage.exists(recent))

    def test_retained_agent_children_are_preserved(self):
        retained = []
        for trash in (False, True):
            agent, children = self.agent_children(trash=trash)
            retained.extend(self.child_keys(children))

        output = self.collect()

        for key in retained:
            self.assertTrue(default_storage.exists(key))
            self.assertNotIn(key, output)

    def test_parent_purge_keeps_queued_or_running_children_until_terminal(self):
        agent, children = self.agent_children(trash=True)
        agent.purgedAt = self.old
        agent.save(update_fields=["purgedAt", "updatedAt"])
        keys = self.child_keys(children)
        for (session, run), status in zip(children, ("queued", "running")):
            run.status = status
            run.save(update_fields=["status", "updatedAt"])

        output = self.collect()

        for key in keys:
            self.assertTrue(default_storage.exists(key))
            self.assertIn(f"Blocked workspace snapshot key {key}", output)
        AgentRun.objects.filter(session__agent=agent).update(status="completed")
        self.collect()
        self.assertTrue(all(not default_storage.exists(key) for key in keys))

    def test_parent_tombstone_is_rechecked_after_enumeration(self):
        from . import workspace_snapshot_gc as gc

        agent, children = self.agent_children(trash=True)
        agent.purgedAt = self.old
        agent.save(update_fields=["purgedAt", "updatedAt"])
        keys = self.child_keys(children)
        enumerate_keys = gc._snapshot_keys

        def changed_parent(*args):
            for key in enumerate_keys(*args):
                Agent.objects.filter(id=agent.id).update(purgedAt=self.now)
                yield key

        with patch.object(gc, "_snapshot_keys", changed_parent):
            output = self.collect(older_than_seconds=24 * 60 * 60)

        for key in keys:
            self.assertTrue(default_storage.exists(key))
        self.assertIn(f"Blocked workspace snapshot key {keys[0]}", output)

    def test_expired_agent_children_use_expiration_purge_retention(self):
        agent, children = self.agent_children(trash=True)
        agent.deletedAt = self.now - timedelta(days=31)
        agent.save(update_fields=["deletedAt", "updatedAt"])
        keys = self.child_keys(children)
        before = list(Session.objects.filter(agent=agent).values())
        self.collect(dry_run=True, older_than_seconds=24 * 60 * 60)
        agent.refresh_from_db()
        self.assertIsNone(agent.purgedAt)
        with patch("app_core.deleted_resource_gc.timezone.now", return_value=self.now):
            output = self.collect(older_than_seconds=24 * 60 * 60)
        self.assertIn("Expired 1 agents", output)
        agent.refresh_from_db()
        self.assertEqual(agent.purgedAt, self.now)
        self.assertTrue(all(default_storage.exists(key) for key in keys))

        with patch("app_core.deleted_resource_gc.timezone.now", return_value=self.now + timedelta(days=2)):
            self.collect(older_than_seconds=24 * 60 * 60)

        self.assertTrue(all(not default_storage.exists(key) for key in keys))
        self.assertEqual(list(Session.objects.filter(agent=agent).values()), before)

    def test_reclaims_all_snapshot_generations_and_execution_checkpoint_bytes(self):
        keys = [
            self.snapshot(content=b"first generation"),
            self.snapshot(generation=2, content=b"latest generation", current=True),
            self.checkpoint(content=b"latest generation"),
            self.checkpoint(content=b"old checkpoint", checkpoint_id="checkpoint:older"),
        ]
        original_reference = self.session.workspaceStorageKey

        output = self.collect()

        for key in keys:
            with self.subTest(key=key):
                self.assertFalse(default_storage.exists(key))
                self.assertIn(key, output)
        self.session.refresh_from_db()
        self.assertEqual(self.session.status, "deleted")
        self.assertEqual(self.session.purgedAt, self.old)
        self.assertEqual(self.session.workspaceStorageKey, original_reference)
        self.assertTrue(AgentRun.objects.filter(id=self.run.id).exists())
        self.assertIn("Cleaned 0 workspace snapshot keys", self.collect())

    def test_preserves_active_and_restorable_trash_sessions_with_the_same_digest(self):
        purged_key = self.snapshot(current=True)
        retained = []
        for trash in (False, True):
            session, run = self.create_owner(trash=trash)
            retained.append(self.snapshot(session=session, run=run, current=True))
            retained.append(self.snapshot(session=session, run=run, generation=2))
            retained.append(self.checkpoint(session=session, run=run))

        output = self.collect()

        self.assertFalse(default_storage.exists(purged_key))
        for key in retained:
            with self.subTest(key=key):
                self.assertTrue(default_storage.exists(key))
                self.assertNotIn(key, output)

    def test_dry_run_reports_exact_keys_without_storage_or_owner_writes(self):
        keys = [self.snapshot(current=True), self.checkpoint()]
        before = list(Session.objects.values())

        output = self.collect(dry_run=True)

        for key in keys:
            self.assertIn(key, output)
            self.assertTrue(default_storage.exists(key))
        self.assertIn("Would clean 2 workspace snapshot keys", output)
        self.assertEqual(list(Session.objects.values()), before)
        self.collect()
        self.assertTrue(all(not default_storage.exists(key) for key in keys))

    def test_never_deletes_paths_outside_the_two_exact_snapshot_key_shapes(self):
        valid = self.snapshot()
        prefix = f"workspaces/{self.workspace.id}/sessions/{self.session.id}"
        digest = hashlib.sha256(b"snapshot bytes").hexdigest()
        checkpoint_digest = hashlib.sha256(b"checkpoint:first").hexdigest()
        invalid_paths = [
            f"{prefix}/snapshots/0/{digest}.snapshot",
            f"{prefix}/snapshots/01/{digest}.snapshot",
            f"{prefix}/snapshots/not-a-generation/{digest}.snapshot",
            f"{prefix}/snapshots/3/not-a-digest.snapshot",
            f"{prefix}/snapshots/3/{digest.upper()}.snapshot",
            f"{prefix}/snapshots/3/{digest}.snapshot.tmp",
            f"{prefix}/snapshots/3/nested/{digest}.snapshot",
            f"{prefix}/snapshots/3/.centaeris-snapshot-pending.tmp",
            f"{prefix}/agent-runs/{self.run.id}/execution-checkpoints/invalid/{digest}.snapshot",
            f"{prefix}/agent-runs/{self.run.id}/execution-checkpoints/"
            f"{checkpoint_digest}/nested/{digest}.snapshot",
            f"{prefix}/agent-runs/not-a-run/execution-checkpoints/"
            f"{checkpoint_digest}/{digest}.snapshot",
            f"{prefix}/artifacts/{digest}.snapshot",
            f"{prefix}/snapshots-backup/1/{digest}.snapshot",
            f"workspaces/{self.workspace.id}/sessions/session_unknown/snapshots/1/{digest}.snapshot",
        ]
        for key in invalid_paths:
            self.store(key)

        output = self.collect()

        self.assertFalse(default_storage.exists(valid))
        for key in invalid_paths:
            with self.subTest(key=key):
                self.assertTrue(default_storage.exists(key))
                if "/agent-runs/not-a-run/" not in key:
                    self.assertNotIn(key, output)

    def test_honors_recent_object_and_recent_purge_retention(self):
        old_key = self.snapshot()
        recent_key = self.snapshot(generation=2, content=b"fresh", modified=self.now)
        recent_session, recent_run = self.create_owner(purged=True)
        recent_session.purgedAt = self.now
        recent_session.save(update_fields=["purgedAt", "updatedAt"])
        recent_purge_key = self.snapshot(session=recent_session, run=recent_run)

        output = self.collect(older_than_seconds=24 * 60 * 60)

        self.assertFalse(default_storage.exists(old_key))
        for key in (recent_key, recent_purge_key):
            self.assertTrue(default_storage.exists(key))
        self.assertIn(f"Blocked workspace snapshot key {recent_key}", output)
        self.assertNotIn(recent_purge_key, output)
        self.collect()
        self.assertFalse(default_storage.exists(recent_key))
        self.assertFalse(default_storage.exists(recent_purge_key))

    def test_default_retention_keeps_newly_purged_snapshots(self):
        key = self.snapshot()

        output = io.StringIO()
        call_command("gc_deleted_resources", stdout=output)

        self.assertTrue(default_storage.exists(key))
        self.assertNotIn(key, output.getvalue())

    def test_reclaims_only_valid_temporary_upload_names_in_snapshot_directories(self):
        final_key = self.snapshot()
        checkpoint_key = self.checkpoint()
        temporary_keys = []
        for key in (final_key, checkpoint_key):
            descriptor, name = tempfile.mkstemp(
                dir=self.root / key.rsplit("/", 1)[0],
                prefix=".centaeris-immutable-", suffix=".tmp",
            )
            with os.fdopen(descriptor, "wb") as temporary:
                temporary.write(b"interrupted snapshot upload")
            os.utime(name, (self.old.timestamp(), self.old.timestamp()))
            temporary_keys.append(Path(name).relative_to(self.root).as_posix())
        unknown_key = self.store(
            final_key.rsplit("/", 1)[0] + "/.centaeris-immutable-not-valid!.tmp",
        )

        output = self.collect()

        for key in [final_key, checkpoint_key, *temporary_keys]:
            self.assertFalse(default_storage.exists(key))
            self.assertIn(key, output)
        self.assertTrue(default_storage.exists(unknown_key))
        self.assertNotIn(unknown_key, output)

    def test_pathless_backend_fails_without_mutating_snapshot_bytes(self):
        key = self.snapshot()

        with patch.object(default_storage, "path", side_effect=NotImplementedError("remote backend")):
            with self.assertRaises(CommandError):
                self.collect()

        self.assertTrue(default_storage.exists(key))

    def test_missing_local_storage_root_is_empty_and_is_not_created(self):
        missing_root = self.root / "storage-not-created"

        with override_settings(MEDIA_ROOT=str(missing_root)):
            dry_run = self.collect(dry_run=True)
            output = self.collect()

        self.assertIn("Would clean 0 workspace snapshot keys", dry_run)
        self.assertIn("Cleaned 0 workspace snapshot keys", output)
        self.assertFalse(missing_root.exists())

    def test_uploader_cleanup_after_path_check_is_an_idempotent_missing_key(self):
        snapshot_key = self.snapshot()
        temporary_key = self.store(
            snapshot_key.rsplit("/", 1)[0] + "/.centaeris-immutable-deadbeef.tmp",
        )
        temporary_path = self.root / temporary_key
        real_stat = Path.stat
        removed = []

        def remove_before_final_stat(path, *args, **kwargs):
            # lstat completes, then uploader's finally removes its temporary
            # file before the collector reads the retention timestamp.
            if path == temporary_path and kwargs.get("follow_symlinks", True):
                path.unlink()
                removed.append(temporary_key)
            return real_stat(path, *args, **kwargs)

        with patch.object(Path, "stat", new=remove_before_final_stat):
            output = self.collect()

        self.assertEqual(removed, [temporary_key])
        self.assertFalse(default_storage.exists(temporary_key))
        self.assertFalse(default_storage.exists(snapshot_key))
        self.assertIn("failed 0", output)

    def test_symlinked_snapshot_directory_fails_closed_without_deleting_target(self):
        self.snapshot()
        outside = tempfile.TemporaryDirectory(prefix="workspace-snapshot-gc-outside-")
        self.addCleanup(outside.cleanup)
        target = Path(outside.name)
        digest = hashlib.sha256(b"outside bytes").hexdigest()
        victim = target / f"{digest}.snapshot"
        victim.write_bytes(b"outside bytes")
        os.utime(victim, (self.old.timestamp(), self.old.timestamp()))
        parent = self.root / f"workspaces/{self.workspace.id}/sessions/{self.session.id}/snapshots/3"
        if os.name == "nt":
            # A junction is a directory reparse point and needs no administrator
            # symlink privilege on the Windows test host.
            subprocess.run(
                ["cmd", "/c", "mklink", "/J", str(parent), str(target)],
                check=True, capture_output=True,
            )
            self.addCleanup(parent.rmdir)
        else:
            parent.symlink_to(target, target_is_directory=True)
            self.addCleanup(parent.unlink)
        output = io.StringIO()

        with self.assertRaises(CommandError):
            call_command("gc_deleted_resources", older_than_seconds=0, stdout=output)

        self.assertEqual(victim.read_bytes(), b"outside bytes")
        self.assertIn("snapshots/3", output.getvalue())

    def test_unrecognized_run_directory_is_blocked_and_preserved(self):
        self.snapshot()
        digest = hashlib.sha256(b"unrecognized checkpoint").hexdigest()
        checkpoint_digest = hashlib.sha256(b"checkpoint:unknown-run").hexdigest()
        unknown_run_key = self.store(
            f"workspaces/{self.workspace.id}/sessions/{self.session.id}/"
            f"agent-runs/agent_run_{'f' * 32}/execution-checkpoints/"
            f"{checkpoint_digest}/{digest}.snapshot", b"unrecognized checkpoint",
        )

        output = self.collect()

        self.assertTrue(default_storage.exists(unknown_run_key))
        self.assertIn("Blocked", output)
        self.assertIn(f"agent_run_{'f' * 32}", output)

    def test_storage_failure_is_reported_and_a_later_pass_retries_remaining_bytes(self):
        failed_key = self.snapshot()
        other_key = self.checkpoint()
        real_delete = default_storage.delete

        def fail_one(key):
            if key == failed_key:
                raise OSError("injected snapshot deletion failure")
            return real_delete(key)

        output = io.StringIO()
        with patch.object(default_storage, "delete", side_effect=fail_one):
            with self.assertRaises(CommandError):
                call_command("gc_deleted_resources", older_than_seconds=0, stdout=output)
        self.assertIn(failed_key, output.getvalue())
        self.assertIn("injected snapshot deletion failure", output.getvalue())
        self.assertTrue(default_storage.exists(failed_key))
        self.assertFalse(default_storage.exists(other_key))

        self.collect()

        self.assertFalse(default_storage.exists(failed_key))
        self.assertIn("Cleaned 0 workspace snapshot keys", self.collect())

    def test_interrupted_pass_can_resume_after_partial_deletion(self):
        keys = [self.snapshot(), self.checkpoint()]
        real_delete = default_storage.delete
        deleted = []

        def interrupt_after_first(key):
            if deleted:
                raise KeyboardInterrupt("injected GC interruption")
            real_delete(key)
            deleted.append(key)

        with patch.object(default_storage, "delete", side_effect=interrupt_after_first):
            with self.assertRaises(KeyboardInterrupt):
                self.collect()
        self.assertEqual(len(deleted), 1)
        self.assertFalse(default_storage.exists(deleted[0]))
        self.assertEqual(sum(default_storage.exists(key) for key in keys), 1)

        self.collect()

        self.assertTrue(all(not default_storage.exists(key) for key in keys))

    def test_unknown_session_and_wrong_workspace_prefix_are_not_collected(self):
        valid = self.snapshot()
        digest = hashlib.sha256(b"snapshot bytes").hexdigest()
        wrong_workspace_key = self.store(
            f"workspaces/ws_unowned/sessions/{self.session.id}/snapshots/1/{digest}.snapshot",
        )
        unknown_session_key = self.store(
            f"workspaces/{self.workspace.id}/sessions/session_unowned/snapshots/1/{digest}.snapshot",
        )

        self.collect()

        self.assertFalse(default_storage.exists(valid))
        self.assertTrue(default_storage.exists(wrong_workspace_key))
        self.assertTrue(default_storage.exists(unknown_session_key))

    def test_snapshot_gc_preserves_artifacts_library_and_durable_history(self):
        snapshot_key = self.snapshot(current=True)
        artifact_content = b"published artifact"
        artifact_key = self.store(
            f"artifacts/{self.workspace.id}/{self.session.id}/result.bin", artifact_content,
        )
        artifact = Artifact.objects.create(
            workspace=self.workspace, session=self.session, agent_run=self.run,
            createdBy=self.owner, displayName="result.bin", safeFilename="result.bin",
            contentType="application/octet-stream", sizeBytes=len(artifact_content),
            sha256="sha256:" + hashlib.sha256(artifact_content).hexdigest(),
            storageKey=artifact_key, status="published", publishedAt=self.old,
        )
        library_key = self.store(f"users/{self.owner.id}/library/saved/result.bin", artifact_content)
        library = UserLibraryObject.objects.create(
            owner=self.owner, displayName="result.bin", objectKind="savedArtifact",
            contentType="application/octet-stream", sizeBytes=len(artifact_content),
            sha256=artifact.sha256, storageKey=library_key, status="ready",
        )
        history_session, history_run = self.create_owner()
        event = SessionEvent.objects.create(
            eventId="event:snapshot-gc-history", workspace=self.workspace,
            session=history_session, agent_run=history_run, sequence=1,
            agent_run_sequence=1, projects_to_agent_run_stream=True,
            payload={"type": "tool_result", "payload": {"modelContent": "committed output"}},
            createdAtMs=1,
        )
        history_bytes = b"committed output"
        capture = TranscriptOutputCapture.objects.create(
            event=event, executionId="execution:snapshot-gc-history",
            sha256="sha256:" + hashlib.sha256(history_bytes).hexdigest(),
            byteLength=len(history_bytes), chunkSize=65536,
        )
        TranscriptOutputChunk.objects.create(
            capture=capture, index=0, data=history_bytes, sha256=capture.sha256,
        )
        before_artifact = Artifact.objects.values().get(id=artifact.id)
        before_library = UserLibraryObject.objects.values().get(id=library.id)
        before_event = SessionEvent.objects.values().get(eventId=event.eventId)
        before_capture = TranscriptOutputCapture.objects.values().get(event_id=event.eventId)

        self.collect()

        self.assertFalse(default_storage.exists(snapshot_key))
        self.assertEqual((self.root / artifact_key).read_bytes(), artifact_content)
        self.assertEqual((self.root / library_key).read_bytes(), artifact_content)
        self.assertEqual(Artifact.objects.values().get(id=artifact.id), before_artifact)
        self.assertEqual(UserLibraryObject.objects.values().get(id=library.id), before_library)
        self.assertEqual(SessionEvent.objects.values().get(eventId=event.eventId), before_event)
        self.assertEqual(TranscriptOutputCapture.objects.values().get(event_id=event.eventId), before_capture)
        self.assertEqual(bytes(capture.chunks.get(index=0).data), history_bytes)
