"""An exact-key GC decision cannot be undone by a late snapshot publisher."""
import hashlib
from unittest.mock import patch

from django.db import transaction
from django.test import TransactionTestCase
from django.utils import timezone

from .http import internal
from .models import Agent, Session, StorageCleanupRetry, WorkspaceSnapshotObject, WorkspaceSnapshotUpload
from .test_execution_publication_fencing import ExecutionPublicationFixture
from .storage_gc_retry import claim_storage_key,clean_storage_key


class WorkspaceSnapshotPublicationTests(ExecutionPublicationFixture, TransactionTestCase):
    serialized_rollback = True

    def key(self, snapshot):
        return (f"workspaces/{self.session.workspace_id}/sessions/{self.session.pk}/snapshots/1/"
                f"{hashlib.sha256(snapshot).hexdigest()}.snapshot")

    def tombstone(self, key):
        with transaction.atomic():
            Agent.objects.select_for_update().get(pk=self.session.agent_id)
            Session.objects.select_for_update().get(pk=self.session.pk)
            object, _ = WorkspaceSnapshotObject.objects.get_or_create(
                keyDigest=hashlib.sha256(key.encode()).hexdigest(),
                defaults={"storageKey": key, "session": self.session, "kind": "snapshot"})
            object.state, object.collectedAt = "collected", timezone.now()
            object.save(update_fields=["state", "collectedAt", "updatedAt"])

    def test_fresh_authorized_upload_can_reintroduce_a_collected_exact_key(self):
        body, snapshot = self._body()
        self.tombstone(self.key(snapshot))
        response = self._post(body)
        self.assertEqual(response.status_code, 201, response.content)
        self.assertTrue((self.storage_root / self.key(snapshot)).exists())
        self.assertEqual(WorkspaceSnapshotObject.objects.get(storageKey=self.key(snapshot)).state, "live")

    def test_streamed_late_writer_cannot_recreate_a_tombstoned_final_key(self):
        body, snapshot = self._body()
        store = internal._store_workspace_snapshot
        def collected_after_stream(*args, **kwargs):
            store(*args, **kwargs)
            self._lease(self.owner, expired=True)
            self.tombstone(self.key(snapshot))
            self._lease(self.owner)
        with patch.object(internal, "_store_workspace_snapshot", collected_after_stream):
            response = self._post(body)
        self.assertEqual(response.status_code, 409, response.content)
        self.assertFalse((self.storage_root / self.key(snapshot)).exists())

    def test_fresh_authorized_incarnation_rearms_legacy_cleaned_retry(self):
        body, snapshot = self._body()
        key = self.key(snapshot)
        StorageCleanupRetry.objects.create(keyDigest=hashlib.sha256(key.encode()).hexdigest(),
            storageKey=key, collector="workspaceSnapshot", state="cleaned", cleanedAt=timezone.now())
        response = self._post(body)
        self.assertEqual(response.status_code, 201, response.content)
        self.assertTrue((self.storage_root / key).exists())
        self.assertEqual(StorageCleanupRetry.objects.get(storageKey=key).state,"pending")

    def test_reopening_for_a_fresh_upload_does_not_revive_the_old_upload_token(self):
        body,snapshot=self._body()
        store=internal._store_workspace_snapshot
        fresh=[]
        def reopen_after_stream(*args,**kwargs):
            store(*args,**kwargs)
            self.tombstone(self.key(snapshot))
            with patch.object(internal,"_store_workspace_snapshot",store):
                fresh.append(self._post(body))
            self.assertEqual(fresh[0].status_code,201,fresh[0].content)
        with patch.object(internal,"_store_workspace_snapshot",reopen_after_stream):
            old=self._post(body)
        self.assertEqual(old.status_code,409,old.content)
        self.assertEqual(old.json(),{"error":"session_workspace_publication_collected"})
        self.assertTrue((self.storage_root/self.key(snapshot)).exists())

    def test_old_gc_claim_cannot_unlink_a_fresh_object_incarnation(self):
        body,snapshot=self._body()
        key=self.key(snapshot)
        self.tombstone(key)
        token=WorkspaceSnapshotObject.objects.get(storageKey=key).publicationToken
        claim=claim_storage_key(key,"workspaceSnapshot",publication_token=token)
        fresh=self._post(body)
        self.assertEqual(fresh.status_code,201,fresh.content)
        def unlink(value):
            (self.storage_root/value).unlink()
        with self.assertRaisesMessage(RuntimeError,"storage_cleanup_lease_lost"):
            clean_storage_key(key,"workspaceSnapshot",unlink,claim=claim,publication_token=token)
        self.assertTrue((self.storage_root/key).exists())
        retry=StorageCleanupRetry.objects.get(storageKey=key)
        self.assertEqual(retry.state,"pending")
        self.assertNotEqual(retry.publicationToken,token)

    def test_same_run_replacement_execution_lease_can_reintroduce_content(self):
        _,snapshot=self._body()
        key=self.key(snapshot)
        self.tombstone(key)
        self._lease(self.owner,expired=True)
        self._lease(self.replacement)
        body,_=self._body(owner=self.replacement)
        fresh=self._post(body)
        self.assertEqual(fresh.status_code,201,fresh.content)
        self.assertTrue((self.storage_root/key).exists())
        self.session.refresh_from_db()
        self.assertEqual(self.session.workspaceLastAdvancedAgentRun_id,self.run.pk)

    def test_successful_upload_records_durable_final_and_temporary_ownership(self):
        body, snapshot = self._body()
        response = self._post(body)
        self.assertEqual(response.status_code, 201, response.content)
        upload = WorkspaceSnapshotUpload.objects.get(agent_run=self.run, finalObject__kind="snapshot")
        self.assertEqual(upload.finalObject.storageKey, self.key(snapshot))
        self.assertEqual((upload.jobId, upload.leaseOwner, upload.authorizationDigest, upload.state),
            (self.job_id, self.owner, self.authorization.digest, "committed"))
        self.assertIsNotNone(upload.finishedAt)
        self.assertFalse((self.storage_root / upload.temporaryObject.storageKey).exists())
