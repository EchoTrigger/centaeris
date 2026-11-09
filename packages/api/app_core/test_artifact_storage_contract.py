"""Artifact bytes are staged privately before publishing an immutable local key."""
import hashlib
import json
from concurrent.futures import ThreadPoolExecutor
from io import BytesIO
from pathlib import Path
import tempfile
from threading import Barrier, Event
from types import SimpleNamespace

from django.contrib.auth import get_user_model
from django.core.files.base import ContentFile
from django.core.files.storage import default_storage
from django.test import SimpleTestCase, TransactionTestCase, override_settings

from .agent_run_authorization_factory import create_agent_run_authorization
from .artifact_publish import (
    ArtifactPublishError, _HashingReader, _install_staged_artifact, _stage_artifact,
    publish_artifact, verify_stored_object,
)
from .assets import store_immutable_bytes_at_key, delete_stored_object_for_gc
from .models import AgentRun, Artifact, ArtifactPublication, ModelConfig, UserLibraryObject, Workspace, WorkspaceMembership
from .testing import create_session


class ArtifactLocalStorageContractTests(SimpleTestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory(prefix="artifact-local-contract-")
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        settings = override_settings(MEDIA_ROOT=directory.name)
        settings.enable()
        self.addCleanup(settings.disable)

    def test_same_content_converges_without_overwrite_or_temporary_files(self):
        data = bytes(range(256)) * 300
        def upload(_):
            return store_immutable_bytes_at_key(data, "artifacts/immutable.bin")
        with ThreadPoolExecutor(max_workers=4) as pool:
            results = list(pool.map(upload, range(4)))
        self.assertEqual(sum(item["created"] for item in results), 1)
        self.assertEqual({item["storageKey"] for item in results}, {"artifacts/immutable.bin"})
        self.assertEqual(list(self.root.rglob("*.tmp")), [])
        verify_stored_object("artifacts/immutable.bin", len(data), results[0]["sha256"])

    def test_conflicting_content_does_not_delete_the_winner(self):
        store_immutable_bytes_at_key(b"AAA", "artifacts/fixed.bin")
        with self.assertRaisesRegex(RuntimeError, "identity_conflict"):
            store_immutable_bytes_at_key(b"BBB", "artifacts/fixed.bin")
        self.assertEqual((self.root / "artifacts/fixed.bin").read_bytes(), b"AAA")

    def test_django_save_is_not_an_immutable_put(self):
        # The independent Library copy retains Django's ordinary storage API.
        first = default_storage.save("artifacts/result.bin", ContentFile(b"AAA"))
        second = default_storage.save("artifacts/result.bin", ContentFile(b"BBB"))
        self.assertNotEqual(first, second)
        self.assertEqual((self.root / first).read_bytes(), b"AAA")
        self.assertEqual((self.root / second).read_bytes(), b"BBB")

    def test_missing_corrupt_and_deleted_bytes_have_explicit_results(self):
        key = "artifacts/result.bin"
        sha = "sha256:" + hashlib.sha256(b"AAA").hexdigest()
        with self.assertRaisesRegex(ArtifactPublishError, "artifact_storage_missing"):
            verify_stored_object(key, 3, sha)
        default_storage.save(key, ContentFile(b"BBB"))
        with self.assertRaisesRegex(ArtifactPublishError, "artifact_storage_integrity_mismatch"):
            verify_stored_object(key, 3, sha)
        delete_stored_object_for_gc(key)
        delete_stored_object_for_gc(key)
        self.assertFalse(default_storage.exists(key))

    def test_truncated_upload_and_wrong_digest_cannot_verify(self):
        reader = _HashingReader(BytesIO(b"ab"), 3)
        self.assertEqual(reader.read(3), b"ab")
        with self.assertRaisesRegex(ArtifactPublishError, "body_truncated"):
            reader.read(1)
        reader = _HashingReader(BytesIO(b"abc"), 3)
        self.assertEqual(reader.read(3), b"abc")
        with self.assertRaisesRegex(ArtifactPublishError, "integrity_mismatch"):
            reader.require_complete("sha256:" + "0" * 64)

    def artifact(self, data):
        return SimpleNamespace(storageKey="artifacts/result.bin", status="staging",
                               sizeBytes=len(data), sha256="sha256:" + hashlib.sha256(data).hexdigest())

    def test_partial_staging_is_private_and_rejected_upload_cannot_remove_winner(self):
        data = b"a" * (2 * 64 * 1024)
        artifact = self.artifact(data)
        paused = Event()
        resume = Event()

        class PausedUpload(BytesIO):
            def read(self, size=-1):
                if self.tell() == 64 * 1024:
                    paused.set()
                    if not resume.wait(5):
                        raise RuntimeError("test upload did not resume")
                return super().read(size)

        def upload():
            with _stage_artifact(PausedUpload(data), artifact) as (key, temporary):
                _install_staged_artifact(artifact, key, temporary)

        with ThreadPoolExecutor(max_workers=1) as pool:
            pending = pool.submit(upload)
            try:
                self.assertTrue(paused.wait(5))
                self.assertFalse(default_storage.exists(artifact.storageKey))
                with self.assertRaisesMessage(ArtifactPublishError, "artifact_integrity_mismatch"):
                    with _stage_artifact(BytesIO(b"b" * len(data)), artifact):
                        self.fail("corrupt bytes were staged")
            finally:
                resume.set()
            pending.result(timeout=5)
        self.assertEqual((self.root / artifact.storageKey).read_bytes(), data)
        self.assertEqual(list(self.root.rglob("*.tmp")), [])

    def test_two_complete_stages_converge_on_one_exact_key(self):
        data = b"content" * 12000
        artifact = self.artifact(data)
        ready = Barrier(2)

        def upload():
            with _stage_artifact(BytesIO(data), artifact) as (key, temporary):
                ready.wait(timeout=5)
                _install_staged_artifact(artifact, key, temporary)

        with ThreadPoolExecutor(max_workers=2) as pool:
            futures = [pool.submit(upload) for _ in range(2)]
            for future in futures:
                future.result(timeout=5)
        self.assertEqual((self.root / artifact.storageKey).read_bytes(), data)
        self.assertEqual(list(self.root.rglob("*.tmp")), [])

    def test_stage_rejects_truncated_or_trailing_bytes_and_keeps_an_existing_conflict(self):
        artifact = self.artifact(b"abc")
        for data, error in ((b"ab", "body_truncated"), (b"abcd", "content_length_mismatch")):
            with self.subTest(error=error):
                with self.assertRaisesRegex(ArtifactPublishError, error):
                    with _stage_artifact(BytesIO(data), artifact):
                        self.fail("malformed bytes were staged")
                self.assertFalse(default_storage.exists(artifact.storageKey))
        path = self.root / artifact.storageKey
        path.write_bytes(b"bad")
        with self.assertRaisesMessage(ArtifactPublishError, "artifact_storage_integrity_mismatch"):
            with _stage_artifact(BytesIO(b"abc"), artifact):
                self.fail("conflicting winner was accepted")
        self.assertEqual(path.read_bytes(), b"bad")
        self.assertEqual(list(self.root.rglob("*.tmp")), [])


class ArtifactPublicationAuthorityTests(TransactionTestCase):
    def test_membership_revocation_during_upload_leaves_only_staging_reservation(self):
        directory = self.enterContext(tempfile.TemporaryDirectory(prefix="artifact-publication-authority-"))
        self.enterContext(override_settings(MEDIA_ROOT=directory))
        user = get_user_model().objects.create_user(username="artifact-publisher")
        workspace = Workspace.objects.create(name="Publication", createdBy=user)
        membership = WorkspaceMembership.objects.create(workspace=workspace, user=user, role="owner")
        session = create_session(workspace=workspace, owner=user)
        run = AgentRun.objects.create(workspace=workspace, session=session, user=user,
                                      modelConfig=ModelConfig.objects.create(), prompt="Publish")
        grant = create_agent_run_authorization(run, image_digest="sha256:" + "a" * 64)
        data = b"verified bytes"
        call = "publish-call"
        identity = "pub_" + hashlib.sha256(json.dumps([run.pk, call], separators=(",", ":")).encode()).hexdigest()
        metadata = json.dumps({
            "schema": "artifact.publication.v1", "publicationId": identity, "agentRunId": run.pk,
            "authorizationDigest": grant.digest, "toolCallId": call, "filename": "file.bin",
            "sizeBytes": len(data), "sha256": "sha256:" + hashlib.sha256(data).hexdigest(),
        }).encode()
        wire = len(metadata).to_bytes(4, "big") + metadata + data

        class RevokingUpload(BytesIO):
            META = {"CONTENT_LENGTH": str(len(wire))}

            def read(self, size=-1):
                at_body = self.tell() == len(wire) - len(data)
                chunk = super().read(size)
                if at_body:
                    membership.delete()
                return chunk

        with self.assertRaisesMessage(ArtifactPublishError, "artifact_scope_mismatch"):
            publish_artifact(RevokingUpload(wire))
        artifact = Artifact.objects.get(agent_run=run)
        self.assertEqual(artifact.status, "staging")
        self.assertEqual(ArtifactPublication.objects.get(artifact=artifact).status, "staging")
        self.assertFalse(default_storage.exists(artifact.storageKey))
        self.assertFalse(UserLibraryObject.objects.filter(owner=user).exists())
        self.assertEqual(list(Path(directory).rglob("*.tmp")), [])
