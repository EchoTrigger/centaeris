from tempfile import TemporaryDirectory
from types import SimpleNamespace
from unittest.mock import patch

from django.contrib.auth.models import User
from django.core.files.base import ContentFile
from django.core.files.storage import FileSystemStorage
from django.test import TestCase
from django.utils import timezone

from . import material_task_source as sources
from .material_contract import BoundInput, KnowledgeError, sha256_bytes
from .material_identity import processing_spec_digest, representation_id
from .models import MaterialProcessingTask, UserLibraryObject
from .test_platform_mcp import processing_specification


class PlatformTaskSourceTests(TestCase):
    def setUp(self):
        temporary = TemporaryDirectory(prefix="material-task-source-")
        self.addCleanup(temporary.cleanup)
        self.storage = FileSystemStorage(location=temporary.name)
        storage_patch = patch.object(sources, "default_storage", self.storage)
        storage_patch.start()
        self.addCleanup(storage_patch.stop)
        self.owner = User.objects.create(username="material-owner")
        self.content = b"platform-owned material"
        self.storage.save("original.txt", ContentFile(self.content))
        self.material = UserLibraryObject.objects.create(
            owner=self.owner, objectKind="file", displayName="original.txt",
            contentType="text/plain", sizeBytes=len(self.content),
            storageKey="original.txt", sha256=sha256_bytes(self.content),
            contentGeneration=1, status="ready",
        )
        self.identity = {"ownerKind": "userLibraryObject", "ownerId": self.material.pk,
                         "generation": 1, "sha256": self.material.sha256}
        self.spec = processing_specification()
        self.digest = processing_spec_digest(self.spec)
        self.rep = representation_id(self.identity, self.digest)
        self.bound = BoundInput({"inputRef": "input-a", "displayName": "original.txt",
                                 "contentType": "text/plain", "sizeBytes": len(self.content)},
                                "original.txt", self.material, self.identity, self.rep)

    def admit(self):
        access = SimpleNamespace(agent_run=object(), authorization_digest="private-run-digest",
                                 processing_specification=self.spec, spec_digest=self.digest)
        with patch.object(sources, "bind_inputs", return_value=[self.bound]) as bind:
            task = sources.admit_task(access, {"inputRef": "input-a", "representationId": self.rep})
        bind.assert_called_once()
        return task

    def test_durable_task_has_no_run_credential_or_waiter_dependency(self):
        task = self.admit()
        self.assertEqual(set(task.payload), {"schema", "inputIdentity", "specDigest", "sizeBytes"})
        self.assertEqual(task.executionBackend, "platform")
        self.assertFalse(task.operations.exists())
        # Load only persisted state: no originating access object is needed.
        source = sources.resolve_task_source(MaterialProcessingTask.objects.get(pk=task.pk))
        self.assertEqual(source.storage_key, "original.txt")
        self.assertEqual(source.owner.pk, self.material.pk)

    def test_admission_failure_does_not_create_task(self):
        with patch.object(sources, "bind_inputs", side_effect=KnowledgeError("access_revoked")):
            with self.assertRaises(KnowledgeError):
                sources.admit_task(SimpleNamespace(agent_run=object(), authorization_digest="digest", spec_digest=self.digest), {})
        self.assertFalse(MaterialProcessingTask.objects.exists())

    def test_repeated_admission_reuses_task_without_resetting_lease(self):
        task = self.admit()
        MaterialProcessingTask.objects.filter(pk=task.pk).update(leaseEpoch=4, attemptCount=2)
        again = self.admit()
        self.assertEqual(again.pk, task.pk)
        self.assertEqual(again.leaseEpoch, 4)
        self.assertEqual(again.attemptCount, 2)

    def test_existing_runtime_task_is_not_silently_transferred(self):
        MaterialProcessingTask.objects.create(representationId=self.rep, processingSpecification=self.spec, payload={}, executionBackend="runtime")
        with self.assertRaisesRegex(KnowledgeError, "material_task_backend_conflict"):
            self.admit()

    def test_deleted_material_cannot_be_resolved(self):
        task = self.admit()
        UserLibraryObject.objects.filter(pk=self.material.pk).update(
            status="deleted", deletedAt=timezone.now(), deletedFromStatus="ready")
        with self.assertRaisesRegex(KnowledgeError, "material_source_unavailable"):
            sources.resolve_task_source(task)

    def test_changed_generation_hash_or_size_cannot_be_resolved(self):
        task = self.admit()
        for changed in [{"contentGeneration": 2}, {"sha256": sha256_bytes(b"other")}, {"sizeBytes": 123}]:
            with self.subTest(changed=changed):
                UserLibraryObject.objects.filter(pk=self.material.pk).update(**changed)
                with self.assertRaisesRegex(KnowledgeError, "material_source_identity_changed"):
                    sources.resolve_task_source(task)
                UserLibraryObject.objects.filter(pk=self.material.pk).update(
                    contentGeneration=1, sha256=self.material.sha256, sizeBytes=len(self.content))

    def test_missing_bytes_cannot_be_resolved(self):
        task = self.admit()
        self.storage.delete("original.txt")
        with self.assertRaisesRegex(KnowledgeError, "material_source_unavailable"):
            sources.resolve_task_source(task)

    def test_unknown_payload_fields_and_identity_changes_fail_closed(self):
        task = self.admit()
        task.payload = {**task.payload, "agentRunId": "borrowed-authority"}
        with self.assertRaisesRegex(KnowledgeError, "material_task_payload_invalid"):
            sources.resolve_task_source(task)
        task.refresh_from_db()
        task.payload["inputIdentity"]["ownerId"] = "another-object"
        with self.assertRaisesRegex(KnowledgeError, "material_task_identity_mismatch"):
            sources.resolve_task_source(task)
