from dataclasses import replace
from datetime import timedelta
from io import BytesIO
from unittest.mock import patch
from concurrent.futures import ThreadPoolExecutor, TimeoutError as FutureTimeout
from threading import Event

from django.db import connections, transaction
from django.test import TestCase, TransactionTestCase
from django.db.models.functions import Now
from django.utils import timezone

from . import material_commit, material_leases, material_storage, platform_material_commit
from .assets import tombstone_stored_object
from .material_contract import KnowledgeError, sha256_bytes
from .models import DerivedRepresentation, DerivedResource, KnowledgeSegment, MaterialProcessingTask, UserLibraryObject
from . import test_material_task_source as source_fixtures


class PlatformMaterialCommitTests(TestCase):
    def setUp(self):
        source_fixtures.PlatformTaskSourceTests.setUp(self)
        self.task = source_fixtures.PlatformTaskSourceTests.admit(self)
        self.lease = material_leases.claim(self.task.pk, "processor-a", 60)
        for target, name, value in [
            (material_storage, "default_storage", self.storage),
            (material_storage, "delete_stored_object", self.storage.delete),
            (material_commit, "delete_stored_object", self.storage.delete),
        ]:
            patcher = patch.object(target, name, value)
            patcher.start()
            self.addCleanup(patcher.stop)
        self.output = b"processed evidence"
        digest = sha256_bytes(self.output)
        self.commit = platform_material_commit.ProcessingOutput(
            representation_id=self.rep, canonical_size_bytes=len(self.output), canonical_sha256=digest,
            preview_size_bytes=0, preview_sha256=None,
            manifest={"schema": "knowledge.derived_manifest.v1", "pageCount": 1, "pages": [{
                "pageText": {"schema": "knowledge.page_text.v1", "page": 1, "route": "nativeText",
                    "widthMillipoints": 1000, "heightMillipoints": 1000, "text": self.output.decode(),
                    "textSha256": digest, "spans": [{"text": self.output.decode(), "bbox": [0, 0, 10000, 10000]}]},
                "canonicalStartByte": 0, "canonicalEndByte": len(self.output),
                "canonicalStartLine": 1, "canonicalEndLine": 1,
            }]},
        )

    def publish(self, lease=None, output=None, stream=None):
        return platform_material_commit.commit_processing_output(
            lease or self.lease, output or self.commit, stream or BytesIO(self.output))

    def assert_no_publication(self):
        self.assertFalse(DerivedRepresentation.objects.exists())
        self.assertFalse(KnowledgeSegment.objects.exists())
        self.task.refresh_from_db()
        self.assertNotEqual(self.task.status, "completed")

    def test_real_commit_publishes_representation_segments_resources_and_completion(self):
        result = self.publish()
        representation = DerivedRepresentation.objects.get(pk=self.rep)
        self.assertEqual(result["representationId"], self.rep)
        self.assertEqual(representation.ownerId, self.material.pk)
        self.assertEqual(KnowledgeSegment.objects.get().boundedText, self.output.decode())
        self.assertEqual(DerivedResource.objects.get().resourceKey, representation.canonicalTextKey)
        with self.storage.open(representation.canonicalTextKey) as stored:
            self.assertEqual(stored.read(), self.output)
        self.task.refresh_from_db()
        self.assertEqual(self.task.status, "completed")

    def test_exact_completed_replay_verifies_bytes_and_does_not_duplicate(self):
        first = self.publish()
        self.assertEqual(self.publish(), first)
        self.assertEqual(DerivedRepresentation.objects.count(), 1)
        self.assertEqual(KnowledgeSegment.objects.count(), 1)
        with self.assertRaises(KnowledgeError):
            self.publish(stream=BytesIO(b"x" * len(self.output)))
        self.assertTrue(self.storage.exists(DerivedRepresentation.objects.get().canonicalTextKey))

    def test_reassigned_worker_rejects_old_fence_before_reading_output(self):
        MaterialProcessingTask.objects.filter(pk=self.rep).update(leaseExpiresAt=Now() - timedelta(seconds=1))
        fresh = material_leases.claim(self.rep, "processor-b", 60)
        stream = BytesIO(self.output)
        with self.assertRaises(material_leases.LeaseLost):
            self.publish(stream=stream)
        self.assertEqual(stream.tell(), 0)
        self.assert_no_publication()
        self.publish(lease=fresh)

    def test_deleted_or_superseded_material_cannot_publish(self):
        UserLibraryObject.objects.filter(pk=self.material.pk).update(contentGeneration=2)
        with self.assertRaisesRegex(KnowledgeError, "material_source_identity_changed"):
            self.publish()
        self.assert_no_publication()
        UserLibraryObject.objects.filter(pk=self.material.pk).update(contentGeneration=1)
        tombstone_stored_object(self.material)
        with self.assertRaisesRegex(KnowledgeError, "material_source_unavailable"):
            self.publish()
        self.assert_no_publication()

    def test_output_cannot_target_another_task(self):
        with self.assertRaisesRegex(KnowledgeError, "material_task_identity_mismatch"):
            self.publish(output=replace(self.commit, representation_id="representation:sha256:" + "f" * 64))
        self.assert_no_publication()

    def test_completion_write_failure_rolls_back_representation_and_new_storage(self):
        original = MaterialProcessingTask.save
        def save(task, *args, **kwargs):
            if task.status == "completed":
                raise RuntimeError("completion_write_failed")
            return original(task, *args, **kwargs)
        with patch.object(MaterialProcessingTask, "save", save):
            with self.assertRaisesRegex(RuntimeError, "completion_write_failed"):
                self.publish()
        self.assert_no_publication()
        self.assertFalse(DerivedResource.objects.exists())
        self.assertEqual(self.storage.listdir("")[1], ["original.txt"])

    def test_expiry_during_stream_verification_cannot_publish(self):
        original = material_commit.store_stream
        def store(*args):
            result = original(*args)
            MaterialProcessingTask.objects.filter(pk=self.rep).update(leaseExpiresAt=Now() - timedelta(seconds=1))
            return result
        with patch.object(material_commit, "store_stream", side_effect=store):
            with self.assertRaises(material_leases.LeaseLost):
                self.publish()
        self.assert_no_publication()

    def test_completed_replay_does_not_bypass_source_lifecycle(self):
        self.publish()
        tombstone_stored_object(self.material)
        with self.assertRaisesRegex(KnowledgeError, "material_source_unavailable"):
            self.publish()

    def test_completed_replay_requires_existing_representation(self):
        MaterialProcessingTask.objects.filter(pk=self.rep).update(status="completed")
        with self.assertRaisesRegex(KnowledgeError, "material_processing_result_missing"):
            self.publish()
        self.assertFalse(DerivedRepresentation.objects.exists())

    def test_completed_replay_rejects_a_different_worker(self):
        self.publish()
        with self.assertRaises(material_leases.LeaseLost):
            self.publish(lease=replace(self.lease, owner="another-worker"))

    def test_lost_commit_acknowledgement_cleanup_preserves_published_bytes(self):
        from .material_staging import prepare_staging, recover_staging
        from .models import MaterialStagedObject
        self.publish()
        # A surviving/replayed durable intent is not proof that the DB commit failed.
        keys = prepare_staging(self.lease, self.commit)
        with transaction.atomic():
            task = MaterialProcessingTask.objects.select_for_update().get(pk=self.rep)
            recover_staging(task, discard=True)
        self.assertFalse(MaterialStagedObject.objects.exists())
        with self.storage.open(keys[0]) as stored:
            self.assertEqual(stored.read(), self.output)
        self.assertEqual(DerivedRepresentation.objects.count(), 1)

    def test_live_lease_prevents_worker_gc_of_partial_output(self):
        from unittest.mock import Mock
        from django.core.files.base import ContentFile
        from .material_staging import prepare_staging
        from .material_processor import MaterialWorker
        from .models import MaterialStagedObject
        key = prepare_staging(self.lease, self.commit)[0]
        self.storage.save(key, ContentFile(b"partial"))
        worker = object.__new__(MaterialWorker)
        worker.client, worker.namespace = Mock(), "fixture"
        worker.client.containers.list.return_value = []
        worker.cleanup_expired()
        self.assertTrue(self.storage.exists(key))
        self.assertTrue(MaterialStagedObject.objects.exists())
        MaterialProcessingTask.objects.filter(pk=self.rep).update(
            leaseExpiresAt=Now() - timedelta(seconds=1))
        worker.cleanup_expired()
        self.assertFalse(self.storage.exists(key))
        self.assertFalse(MaterialStagedObject.objects.exists())

    def test_crashed_partial_storage_is_repaired_without_deleting_published_bytes(self):
        from .material_staging import prepare_staging
        keys = prepare_staging(self.lease, self.commit)
        from django.core.files.base import ContentFile
        self.storage.save(keys[0], ContentFile(b"interrupted write"))
        self.publish()
        representation = DerivedRepresentation.objects.get(pk=self.rep)
        with self.storage.open(representation.canonicalTextKey) as stored:
            self.assertEqual(stored.read(), self.output)
        self.publish()


class PlatformMaterialCommitConcurrencyTests(TransactionTestCase):
    serialized_rollback = True

    def setUp(self):
        PlatformMaterialCommitTests.setUp(self)

    def test_deletion_waits_for_publication_and_tombstones_its_resources(self):
        reading, release, deleting = Event(), Event(), Event()
        output_bytes = self.output

        class PausedStream(BytesIO):
            def read(self, size=-1):
                reading.set()
                if not release.wait(10):
                    raise RuntimeError("test_publication_release_timeout")
                return super().read(size)

        def publish():
            try:
                return platform_material_commit.commit_processing_output(self.lease, self.commit, PausedStream(output_bytes))
            finally:
                connections.close_all()

        def delete():
            try:
                with transaction.atomic():
                    deleting.set()
                    material = UserLibraryObject.objects.select_for_update().get(pk=self.material.pk)
                    tombstone_stored_object(material)
            finally:
                connections.close_all()

        with ThreadPoolExecutor(max_workers=2) as workers:
            publisher = workers.submit(publish)
            try:
                self.assertTrue(reading.wait(10))
                deletion = workers.submit(delete)
                self.assertTrue(deleting.wait(10))
                with self.assertRaises(FutureTimeout):
                    deletion.result(timeout=0.2)
            finally:
                release.set()
            publisher.result(timeout=10)
            deletion.result(timeout=10)
        representation = DerivedRepresentation.objects.get(pk=self.rep)
        resource = DerivedResource.objects.get(resourceKey=representation.canonicalTextKey)
        self.assertEqual(resource.state, "pending")
        self.material.refresh_from_db()
        self.assertEqual(self.material.status, "deleted")
