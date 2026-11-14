from datetime import timedelta
import hashlib
import json
import tempfile
import time
from unittest.mock import patch
import uuid

from django.contrib.auth.models import User
from django.core.files.base import ContentFile
from django.core.files.storage import default_storage
from django.db import connection, IntegrityError, transaction
from django.test import RequestFactory, TransactionTestCase, override_settings
from django.utils import timezone

from . import models
from .artifact_publish import publish_artifact
from .http.agent_run_api import CreateRunRequest
from .agent_run_api_cleanup import collect_agent_run_api_cleanup
from .agent_run_api_credentials import create_credential
from .agent_run_api_runs import accept_run


class AgentRunApiCleanupTests(TransactionTestCase):
    serialized_rollback = True
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="agent-run-api-cleanup-")
        self.addCleanup(self.directory.cleanup)
        settings = override_settings(MEDIA_ROOT=self.directory.name)
        settings.enable()
        self.addCleanup(settings.disable)
        user = User.objects.create_user(username="agent-run-api-cleanup-owner")
        workspace = models.Workspace.objects.create(name="API", createdBy=user)
        models.WorkspaceMembership.objects.create(workspace=workspace, user=user, role="owner")
        model = models.ModelConfig.objects.create(displayName="Model")
        credential, _ = create_credential(user, workspace.pk, model.pk, name="Backend")
        with patch("app_core.agent_run_api_runs.request_execution_profile", return_value={
            "schema": "runtime.execution_profile.v1", "imageDigest": "sha256:" + "a" * 64,
        }), patch("app_core.agent_run_api_runs.schedule_agent_run_lifecycle", return_value="inserted"):
            self.operation, self.run = accept_run(credential, f"{int(time.time())}.{uuid.uuid4()}",
                CreateRunRequest(prompt="private input"))
        self.runtime = patch("app_core.agent_run_api_cleanup._purge_runtime", side_effect=self.purge_runtime).start()
        self.addCleanup(patch.stopall)
        self.redis = patch("app_core.agent_run_api_cleanup.redis.Redis.from_url").start()

    def purge_runtime(self, run):
        self.assertFalse(connection.in_atomic_block, "Runtime must acquire the same Session on its own connection")

    def publish(self, name, content, call="publish-1"):
        digest = "sha256:" + hashlib.sha256(content).hexdigest()
        identity = "pub_" + hashlib.sha256(json.dumps([self.run.pk, call], separators=(",", ":")).encode()).hexdigest()
        body = {"schema": "artifact.publication.v1", "publicationId": identity,
            "agentRunId": self.run.pk, "authorizationDigest": self.run.authorization.digest,
            "toolCallId": call, "filename": name, "sizeBytes": len(content), "sha256": digest}
        metadata = json.dumps(body, separators=(",", ":")).encode()
        request = RequestFactory().post("/internal/artifacts/publish",
            len(metadata).to_bytes(4, "big") + metadata + content, content_type="application/octet-stream")
        return publish_artifact(request)[1]

    def settle(self, *, no_allocation=False):
        now = timezone.now()
        models.AgentRun.objects.filter(pk=self.run.pk).update(
            status="cancelled" if no_allocation else "completed", completedAt=now,
            transitionReason="agent_run_cancelled" if no_allocation else "agent_run_completed",
            preAdmissionCancelledAt=now if no_allocation else None,
        )
        self.operation.deliveryClosedAt = now
        self.operation.save(update_fields=["deliveryClosedAt"])
        if not no_allocation:
            models.SessionEnvironment.objects.filter(pk=self.run.environment_id).update(
                controller_epoch=1, controller_run=self.run, controller_lease_owner="fixture-cleanup",
                controller_released=True, controller_purpose="cleanup",
            )
            models.AgentRunEnvironmentRelease.objects.create(agent_run=self.run, environment=self.run.environment,
                authorization_digest=self.run.authorization.digest, controller_epoch=1,
                controller_lease_digest="sha256:" + hashlib.sha256(
                    b"workspace:environment-controller-lease:v1\0fixture-cleanup").hexdigest())

    def test_payload_and_unregistered_temp_cleanup_preserves_only_receipt_and_shared_bytes(self):
        private = self.publish("private.docx", b"private bytes")
        shared = self.publish("shared.docx", b"shared bytes", "publish-2")
        self.assertFalse(models.UserLibraryObject.objects.exists())
        library = models.UserLibraryObject.objects.create(owner=self.run.user, displayName="Saved",
            objectKind="file", contentType=shared.contentType, sizeBytes=shared.sizeBytes,
            sha256=shared.sha256, storageKey=shared.storageKey, status="ready", contentGeneration=1)
        prefix = f"workspaces/{self.run.workspace_id}/sessions/{self.run.session_id}"
        orphan = default_storage.save(f"{prefix}/snapshots/1/.centaeris-immutable-abcd1234.tmp", ContentFile(b"partial"))
        self.settle()
        report = collect_agent_run_api_cleanup()
        self.assertEqual(report.failures, [])
        self.assertEqual(report.cleaned, [self.run.pk])
        self.assertFalse(models.Session.objects.filter(pk=self.run.session_id).exists())
        self.assertFalse(models.AgentRun.objects.filter(pk=self.run.pk).exists())
        self.assertFalse(models.AgentRunAuthorization.objects.filter(agent_run_id=self.run.pk).exists())
        self.assertFalse(default_storage.exists(private.storageKey))
        self.assertFalse(default_storage.exists(orphan))
        with default_storage.open(shared.storageKey, "rb") as source:
            self.assertEqual(source.read(), b"shared bytes")
        self.assertTrue(models.UserLibraryObject.objects.filter(pk=library.pk).exists())
        self.operation.refresh_from_db()
        self.assertEqual(self.operation.cleanupStatus, "completed")
        self.assertEqual(collect_agent_run_api_cleanup().cleaned, [])
        self.runtime.assert_called_once()
        expected = f"workspace-agent:agent-run:{{{self.run.pk}}}"
        self.redis.return_value.delete.assert_called_once_with(f"{expected}:signals", f"{expected}:live:text", f"{expected}:live:meta", f"{expected}:live:reasoning")

    def test_deleted_credential_leaves_no_receipt_after_cleanup(self):
        self.operation.credential.delete()
        self.operation.refresh_from_db()
        self.settle(no_allocation=True)
        self.assertEqual(collect_agent_run_api_cleanup().cleaned, [self.run.pk])
        self.assertFalse(models.AgentRunApiOperation.objects.filter(pk=self.operation.pk).exists())

    def test_open_delivery_blocks_and_expired_delivery_reclaims_cancelled_no_allocation(self):
        self.settle(no_allocation=True)
        self.operation.deliveryClosedAt = None
        self.operation.save(update_fields=["deliveryClosedAt"])
        self.assertEqual(collect_agent_run_api_cleanup().cleaned, [])
        self.runtime.assert_not_called()
        models.AgentRunApiOperation.objects.filter(pk=self.operation.pk).update(deliveryLeaseUntil=timezone.now() - timedelta(seconds=1))
        report = collect_agent_run_api_cleanup()
        self.assertEqual(report.failures, [])
        self.assertEqual(report.cleaned, [self.run.pk])
        self.operation.refresh_from_db()
        self.assertEqual(self.operation.reasonCode, "agent_run_cancelled")
        self.assertFalse(models.AgentRunEnvironmentRelease.objects.exists())

    def test_failed_storage_delete_retains_owner_for_retry(self):
        artifact = self.publish("result.docx", b"bytes")
        self.settle()
        with patch("app_core.agent_run_api_cleanup.delete_stored_object_for_gc", side_effect=OSError("unavailable")):
            self.assertEqual(collect_agent_run_api_cleanup().failures, [self.run.pk])
        self.assertTrue(models.Session.objects.filter(pk=self.run.session_id).exists())
        self.assertTrue(default_storage.exists(artifact.storageKey))
        self.assertEqual(collect_agent_run_api_cleanup().cleaned, [self.run.pk])

    def test_interactive_environment_and_release_remain_immutable(self):
        self.settle()
        models.Session.objects.filter(pk=self.run.session_id).update(purpose="interactive")
        models.AgentRunApiOperation.objects.filter(pk=self.operation.pk).update(cleanupStatus="cleaning")
        for model, key, reason in (
            (models.SessionEnvironment, self.run.environment_id, "environment_profile_binding_immutable"),
            (models.AgentRunEnvironmentRelease, models.AgentRunEnvironmentRelease.objects.get(agent_run=self.run).pk,
             "environment_release_immutable"),
        ):
            with self.subTest(model=model.__name__), self.assertRaisesRegex(IntegrityError, reason), transaction.atomic():
                model.objects.filter(pk=key)._raw_delete(connection.alias)

    def test_receipt_expiry_after_cleanup_does_not_restore_execution_identity(self):
        self.settle(no_allocation=True)
        self.assertEqual(collect_agent_run_api_cleanup().cleaned, [self.run.pk])
        models.AgentRunApiOperation.objects.filter(pk=self.operation.pk).update(expiresAt=timezone.now() - timedelta(seconds=1))
        collect_agent_run_api_cleanup()
        self.assertFalse(models.AgentRunApiOperation.objects.filter(pk=self.operation.pk).exists())
