"""Shared content survives snapshot GC; abandoned content uses bounded root queries."""
from datetime import timedelta
from math import ceil
import os
from unittest.mock import patch

from django.db import connection
from django.test import TransactionTestCase
from django.test.utils import CaptureQueriesContext
from django.utils import timezone

from .models import (AgentRun, SessionWorkspacePreservation, WorkspaceFileReservation,
                     WorkspaceSnapshotObject, WorkspaceSnapshotUpload)
from .test_workspace_file_http import WorkspaceFileHttpFixture, digest, document
from .workspace_file_objects import file_key
from .workspace_snapshot_gc import collect_workspace_snapshot_gc
from . import workspace_snapshot_reachability_gc as reachability


class WorkspaceFileGcTests(WorkspaceFileHttpFixture, TransactionTestCase):
    serialized_rollback = True

    def setUp(self):
        super().setUp()
        self.now = timezone.now()
        self.old = self.now - timedelta(days=35)
        self.roots = {"schema": "runtime.workspace_snapshot.roots.v1", "sessionId": self.session.pk,
            "sessionSequence": 0, "authorityFingerprint": "sha256:" + "a" * 64,
            "roots": [], "protectedAgentRunIds": []}
        self.enterContext(patch("app_core.workspace_snapshot_roots.current_witness",
                               return_value=(0, "sha256:" + "a" * 64)))
        def roots(session):
            self.assertFalse(connection.in_atomic_block)
            return self.roots | {"sessionId": session.pk}
        self.root_request = self.enterContext(patch(
            "app_core.workspace_snapshot_reachability_gc.request_workspace_snapshot_roots", side_effect=roots))

    def age_objects(self):
        WorkspaceSnapshotObject.objects.update(createdAt=self.old, updatedAt=self.old)
        WorkspaceSnapshotUpload.objects.update(createdAt=self.old, updatedAt=self.old)
        for path in self.storage_root.rglob("*"):
            if path.is_file():
                os.utime(path, (self.old.timestamp(), self.old.timestamp()))

    def collect(self):
        report = collect_workspace_snapshot_gc(self.now - timedelta(days=30), False)
        self.assertEqual(report.failures, [])
        return report

    def advance_run(self):
        previous_job = self.job_id
        self._lease(self.owner, expired=True)
        reader = self.new_reader()
        self.run = AgentRun.objects.select_related("authorization").get(pk=reader["agentRunId"])
        self.authorization = self.run.authorization
        self.job_id, self.owner = reader["jobId"], reader["leaseOwner"]
        def delete_previous_job():
            with connection.cursor() as cursor:
                cursor.execute("DELETE FROM runtime.runtime_jobs WHERE job_id=%s", [previous_job])
        self.addCleanup(delete_previous_job)

    def test_collecting_old_manifest_keeps_content_shared_with_current_head(self):
        stable, previous, current = b"shared stable body", b"old body", b"new body"
        self.store_files([stable, previous])
        first, _, _ = self.snapshot_upload(document([("a", stable, False), ("b", previous, True)]))
        self.assertEqual(first.status_code, 201, first.content)
        self.session.refresh_from_db()
        old_key = self.session.workspaceStorageKey
        self.advance_run()
        missing = self.store_files([stable, current])
        self.assertEqual(missing.json()["missing"], [digest(current)])
        second, _, _ = self.snapshot_upload(document([("a", stable, False), ("b", current, True)]))
        self.assertEqual(second.status_code, 201, second.content)
        self.session.refresh_from_db()
        head_key = self.session.workspaceStorageKey
        self._lease(self.owner, expired=True)
        self.age_objects()
        report = self.collect()
        old_content = file_key(self.session, digest(previous))
        self.assertEqual(set(report.cleaned), {old_key, old_content})
        for key in [head_key, file_key(self.session, digest(stable)), file_key(self.session, digest(current))]:
            self.assertTrue((self.storage_root / key).exists(), key)
        self.assertFalse((self.storage_root / old_key).exists())
        self.assertFalse((self.storage_root / old_content).exists())

    def test_current_reservation_keeps_content_until_lease_expires(self):
        content = b"not-yet-published"
        self.store_files([content])
        key = file_key(self.session, digest(content))
        self.age_objects()
        self.assertEqual(self.collect().cleaned, [])
        self.assertTrue((self.storage_root / key).exists())
        self._lease(self.owner, expired=True)
        self.assertEqual(self.collect().cleaned, [key])
        self.assertFalse((self.storage_root / key).exists())
        self.assertEqual(WorkspaceFileReservation.objects.count(), 0)

    def test_root_metadata_queries_are_bounded_per_batch_not_per_file(self):
        contents = [f"orphan-{index}".encode() for index in range(8)]
        self.store_files(contents)
        self._lease(self.owner, expired=True)
        self.age_objects()
        with CaptureQueriesContext(connection) as queries:
            report = self.collect()
        self.assertEqual(len(report.cleaned), len(contents))
        table = SessionWorkspacePreservation._meta.db_table.lower()
        root_queries = [query for query in queries.captured_queries
                        if query["sql"].lstrip().upper().startswith("SELECT") and table in query["sql"].lower()]
        self.assertLessEqual(len(root_queries), 2 * ceil(len(contents) / reachability.BATCH_SIZE))
        self.assertEqual(self.root_request.call_count, 1)
