"""Committed Session workspace authority survives idle time and later admission."""
from copy import deepcopy
from datetime import timedelta
import hashlib
import json
import os
from pathlib import Path
import tempfile

from django.conf import settings
from django.core.files.storage import default_storage
from django.core.files.uploadedfile import SimpleUploadedFile
from django.test import TestCase, override_settings
from django.utils import timezone

from .models import AgentRun, Session, SessionAssetLink, SessionEvent, WorkspaceMembership
from . import test_agent_messages
from .test_hosted_operations import OperationFixture
from .testing import leased_lifecycle_job_fixture
from .workspace_snapshot_gc import collect_workspace_snapshot_gc


class SessionWorkspaceRetentionTests(OperationFixture, TestCase):
    def test_month_old_committed_workspace_and_session_facts_survive_gc_and_continuation(self):
        storage = self.enterContext(tempfile.TemporaryDirectory(prefix="session-workspace-retention-"))
        self.enterContext(override_settings(MEDIA_ROOT=storage))
        session = Session.objects.get(pk=self.create().json()["sessionId"])
        uploaded = self.client.post(f"/api/sessions/{session.pk}/uploads", {
            "files": [SimpleUploadedFile("evidence.txt", b"original attachment", "text/plain")],
        })
        self.assertEqual(uploaded.status_code, 201, uploaded.content)
        first = self.post(self.create_url + f"/{session.pk}/messages")
        self.assertEqual(first.status_code, 202, first.content)
        run = AgentRun.objects.get(pk=first.json()["agentRunId"])
        original_grant = deepcopy((run.authorization.payload, run.authorization.digest, run.authorization.signature))
        test_agent_messages.AgentMessageTests.commit(self, run, "user_message", {
            "messageId": "original-message", "text": "original history", "attachments": [],
        })
        lease_owner = leased_lifecycle_job_fixture(self, run)
        content = b"authoritative committed workspace"
        manifest = json.dumps({"schema": "workspace.snapshot.v1", "files": [{
            "path": "notes.txt", "sizeBytes": len(content),
            "sha256": "sha256:" + hashlib.sha256(content).hexdigest(), "executable": False,
        }]}, separators=(",", ":")).encode()
        snapshot = len(manifest).to_bytes(4, "big") + manifest + content
        metadata = json.dumps({
            "schema": "runtime.session_workspace.commit.v1",
            "jobId": f"agent_run.lifecycle:{run.pk}", "leaseOwner": lease_owner,
            "agentRunId": run.pk, "authorizationDigest": run.authorization.digest,
            "snapshotSha256": "sha256:" + hashlib.sha256(snapshot).hexdigest(),
            "snapshotSizeBytes": len(snapshot), "expandedSizeBytes": len(content), "fileCount": 1,
        }, separators=(",", ":")).encode()
        committed = self.client.post("/internal/agent-runs/session-workspace/commit",
            data=len(metadata).to_bytes(4, "big") + metadata + snapshot,
            content_type="application/octet-stream", HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN)
        self.assertEqual(committed.status_code, 201, committed.content)
        session.refresh_from_db()
        baseline = {name: getattr(session, name) for name in (
            "workspaceGeneration", "workspaceStorageKey", "workspaceSnapshotSha256",
            "workspaceSnapshotSizeBytes", "workspaceExpandedSizeBytes", "workspaceFileCount",
            "workspaceLastAdvancedAgentRun_id", "current_environment_id",
        )}
        old = timezone.now() - timedelta(days=35)
        Session.objects.filter(pk=session.pk).update(createdAt=old, updatedAt=old)
        AgentRun.objects.filter(pk=run.pk).update(status="completed", completedAt=old)
        os.utime(Path(storage) / session.workspaceStorageKey, (old.timestamp(), old.timestamp()))
        facts = (list(SessionEvent.objects.filter(session=session).values()),
                 list(SessionAssetLink.objects.filter(session=session).values()),
                 list(WorkspaceMembership.objects.filter(workspace=self.workspace).values()))

        report = collect_workspace_snapshot_gc(timezone.now() - timedelta(days=1), False)
        self.assertEqual((report.planned, report.cleaned, report.blocked, report.failures), ([], [], [], []))
        with default_storage.open(session.workspaceStorageKey, "rb") as stored:
            self.assertEqual(stored.read(), snapshot)
        self.profile.return_value = {"schema": "runtime.execution_profile.v1", "imageDigest": "sha256:" + "b" * 64}
        with override_settings(SANDBOX_CPU_MILLI=9999):
            second = self.post(self.create_url + f"/{session.pk}/messages", operationId="month-later")
        self.assertEqual(second.status_code, 202, second.content)
        continued = AgentRun.objects.get(pk=second.json()["agentRunId"])
        self.assertEqual((continued.session_id, continued.environment_id), (session.pk, baseline["current_environment_id"]))
        self.assertEqual(continued.authorization.payload["sessionWorkspace"], {
            "generation": 1, "snapshotSha256": baseline["workspaceSnapshotSha256"],
            "snapshotSizeBytes": len(snapshot), "expandedSizeBytes": len(content), "fileCount": 1,
        })
        self.assertEqual(continued.authorization.payload["environmentProfile"], original_grant[0]["environmentProfile"])
        self.assertEqual(continued.authorization.payload["assetRefs"], original_grant[0]["assetRefs"])
        run.authorization.refresh_from_db()
        self.assertEqual((run.authorization.payload, run.authorization.digest, run.authorization.signature), original_grant)
        session.refresh_from_db()
        self.assertEqual({name: getattr(session, name) for name in baseline}, baseline)
        self.assertEqual((list(SessionEvent.objects.filter(session=session).values()),
                          list(SessionAssetLink.objects.filter(session=session).values()),
                          list(WorkspaceMembership.objects.filter(workspace=self.workspace).values())), facts)
