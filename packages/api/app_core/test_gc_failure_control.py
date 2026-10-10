"""A failed collector must not starve independent reclamation work."""

import io
import json
from datetime import timedelta
from pathlib import Path
import shutil
import subprocess
import sys
from unittest.mock import patch

from django.core.management import call_command
from django.core.management.base import CommandError
from django.contrib.auth import get_user_model
from django.core.files.base import ContentFile
from django.core.files.storage import default_storage
from django.conf import settings
from django.test import SimpleTestCase, TestCase, TransactionTestCase, override_settings
from django.utils import timezone
import tempfile

from .deleted_resource_gc import (
    DeletedResourceGcReport,
    OrphanedLibraryGcReport,
    TrashExpirationReport,
)
from .workspace_snapshot_gc import WorkspaceSnapshotGcError, WorkspaceSnapshotGcReport
from .assets import tombstone_stored_object
from .models import DerivedResource, StorageCleanupRetry, UserLibraryObject
from .storage_gc_retry import claim_storage_key, clean_storage_key, next_cleanup_time
from . import deleted_resource_gc
from .migration_testing import isolated_migration_database


COMMAND = "app_core.management.commands.gc_deleted_resources"


class GcBackoffRangeTests(SimpleTestCase):
    @override_settings(GC_RETRY_BASE_SECONDS=1, GC_RETRY_MAX_SECONDS=2678400)
    def test_small_base_reaches_configured_maximum_and_bounds_legacy_counters(self):
        now = timezone.now()
        self.assertEqual(next_cleanup_time(22, now), now + timedelta(seconds=2097152))
        self.assertEqual(next_cleanup_time(2**31, now), now + timedelta(days=31))


class GcRetryProcessPersistenceTests(TransactionTestCase):
    serialized_rollback = True

    def test_restart_preserves_committed_backoff_and_quarantine_without_deleting_bytes(self):
        child_code = '''
import json
import sys
from datetime import datetime
from unittest.mock import patch
from django.conf import settings

fixture = json.load(sys.stdin)
settings.configure(
    DATABASES={"default": fixture["database"]}, INSTALLED_APPS=fixture["apps"],
    AUTH_USER_MODEL=fixture["userModel"], DEFAULT_AUTO_FIELD="django.db.models.BigAutoField",
    USE_TZ=True, MEDIA_ROOT=fixture["mediaRoot"],
    GC_MAX_CLEANUP_ATTEMPTS=2, GC_RETRY_BASE_SECONDS=60, GC_RETRY_MAX_SECONDS=120,
)
import django
django.setup()
from django.db import connections
from app_core.models import StorageCleanupRetry
from app_core.storage_gc_retry import clean_storage_key

calls = []
def denied(key):
    calls.append(key)
    raise PermissionError("synthetic deletion denied")

results = []
for instant in fixture["instants"]:
    with patch("django.utils.timezone.now", return_value=datetime.fromisoformat(instant)):
        results.append(clean_storage_key(fixture["key"], "workspaceSnapshot", denied).state)
retry = StorageCleanupRetry.objects.get(storageKey=fixture["key"])
print(json.dumps({"results": results, "calls": calls, "attempts": retry.cleanupAttempts,
                  "state": retry.state, "next": retry.nextCleanupAt.isoformat() if retry.nextCleanupAt else None}))
connections.close_all()
'''
        with isolated_migration_database([("app_core", "0009_storage_gc_backoff")]) as database:
            self.assertEqual(database.vendor, "postgresql")
            with tempfile.TemporaryDirectory(prefix="gc-process-retry-") as directory:
                with override_settings(MEDIA_ROOT=directory):
                    key = "snapshot/process-restart.snapshot"
                    default_storage.save(key, ContentFile(b"retained after denied deletion"))
                    now = timezone.now()
                    config = dict(database.settings_dict)
                    config["OPTIONS"] = dict(config.get("OPTIONS", {}))
                    config["OPTIONS"].pop("pool", None)
                    fixture = {
                        "database": config, "apps": settings.INSTALLED_APPS,
                        "userModel": settings.AUTH_USER_MODEL, "mediaRoot": directory, "key": key,
                    }

                    def run_process(instants):
                        result = subprocess.run(
                            [sys.executable, "-c", child_code],
                            input=json.dumps({**fixture, "instants": [instant.isoformat() for instant in instants]}),
                            cwd=Path(__file__).resolve().parents[1],
                            capture_output=True, text=True, timeout=10,
                        )
                        self.assertEqual(result.returncode, 0, result.stderr)
                        return json.loads(result.stdout)

                    first = run_process([now])
                    self.assertEqual((first["results"], first["calls"], first["attempts"]), (["failed"], [key], 1))
                    retry = StorageCleanupRetry.objects.using(database.alias).get(storageKey=key)
                    self.assertEqual(retry.state, "failed")
                    self.assertEqual(retry.nextCleanupAt, now + timedelta(seconds=60))

                    before_deadline = run_process([now + timedelta(seconds=59)])
                    self.assertEqual((before_deadline["results"], before_deadline["calls"], before_deadline["attempts"]),
                                     (["blocked"], [], 1))
                    self.assertEqual(before_deadline["next"], first["next"])

                    exhausted = run_process([now + timedelta(seconds=60), now + timedelta(hours=1)])
                    self.assertEqual((exhausted["results"], exhausted["calls"], exhausted["attempts"], exhausted["state"]),
                                     (["failed", "blocked"], [key], 2, "quarantined"))
                    retry.refresh_from_db()
                    self.assertEqual(retry.state, "quarantined")
                    self.assertEqual(retry.cleanupAttempts, 2)
                    self.assertEqual(retry.quarantinedAt, now + timedelta(seconds=60))
                    self.assertIsNone(retry.nextCleanupAt)
                    self.assertTrue(default_storage.exists(key))


class IndependentGcCollectorTests(SimpleTestCase):
    def run_collectors(self, *, resource_failure=False, snapshot_failure=False):
        with (
            patch(f"{COMMAND}.expire_trash", return_value=TrashExpirationReport(0, 0, 0, 0)),
            patch(
                f"{COMMAND}.collect_deleted_resource_gc",
                return_value=DeletedResourceGcReport([], [], [], [object()] if resource_failure else []),
            ),
            patch(
                f"{COMMAND}.collect_workspace_snapshot_gc",
                side_effect=WorkspaceSnapshotGcError("storage unavailable") if snapshot_failure else None,
                return_value=WorkspaceSnapshotGcReport([], ["snapshot"], [], []),
            ) as snapshots,
            patch(
                f"{COMMAND}.collect_orphaned_library_gc",
                return_value=OrphanedLibraryGcReport([], ["orphan"], []),
            ) as orphans,
        ):
            output = io.StringIO()
            with self.assertRaises(CommandError):
                call_command("gc_deleted_resources", older_than_seconds=0, orphaned_library=True, stdout=output)
            snapshots.assert_called_once()
            orphans.assert_called_once()
            self.assertIn("Cleaned orphaned library key orphan", output.getvalue())
            if not snapshot_failure:
                self.assertIn("Cleaned workspace snapshot key snapshot", output.getvalue())

    def test_resource_failure_does_not_prevent_snapshot_and_orphan_cleanup(self):
        self.run_collectors(resource_failure=True)

    def test_snapshot_backend_failure_does_not_prevent_orphan_cleanup(self):
        self.run_collectors(snapshot_failure=True)


class GcEntrypointTests(SimpleTestCase):
    def test_failed_command_reaches_wait_before_another_attempt(self):
        shell = shutil.which("sh")
        git_shell = Path("C:/Program Files/Git/bin/bash.exe")
        if shell is None and git_shell.is_file():
            shell = str(git_shell)
        if shell is None:
            self.skipTest("A POSIX shell is required to execute the container entrypoint")
        script = Path(__file__).resolve().parents[1] / "gc-entrypoint.sh"
        result = subprocess.run(
            [
                shell, "-c",
                'python() { printf "attempt\\n"; return 1; }; '
                'sleep() { printf "waiting:%s\\n" "$1"; exit 23; }; . "$1"',
                "gc-fixture", script.as_posix(),
            ],
            capture_output=True, text=True, timeout=10,
        )
        self.assertEqual(result.returncode, 23, result.stderr)
        self.assertEqual(result.stdout.splitlines(), ["attempt", "waiting:86400"])


@override_settings(GC_MAX_CLEANUP_ATTEMPTS=3, GC_RETRY_BASE_SECONDS=60, GC_RETRY_MAX_SECONDS=120)
class PersistentGcRetryTests(TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory(prefix="gc-retry-")
        self.addCleanup(directory.cleanup)
        isolated = override_settings(MEDIA_ROOT=directory.name)
        isolated.enable()
        self.addCleanup(isolated.disable)
        self.now = timezone.now()
        self.owner = get_user_model().objects.create_user(username="gc-retry-owner")

    def deleted_object(self, key):
        default_storage.save(key, ContentFile(b"fixture"))
        item = UserLibraryObject.objects.create(
            owner=self.owner, displayName=key.rsplit("/", 1)[-1], objectKind="file",
            contentType="text/plain", sizeBytes=7, sha256="sha256:" + "a" * 64,
            storageKey=key, status="ready",
        )
        tombstone_stored_object(item, self.now - timedelta(days=2))
        item.purgedAt = self.now - timedelta(days=1)
        item.save(update_fields=["purgedAt", "updatedAt"])
        return item

    def test_permanent_failure_backoff_survives_reloaded_rows_and_isolates_only_failed_object(self):
        bad = self.deleted_object("gc/denied.txt")
        good = self.deleted_object("gc/allowed.txt")
        calls = []
        actual_delete = deleted_resource_gc.delete_stored_object_for_gc

        def delete(key):
            calls.append(key)
            if key == bad.storageKey:
                raise PermissionError("denied")
            actual_delete(key)

        with patch.object(deleted_resource_gc, "delete_stored_object_for_gc", side_effect=delete):
            for delta, expected in [(0, 1), (0, 1), (59, 1), (60, 2), (179, 2), (180, 3), (10000, 3)]:
                with patch("django.utils.timezone.now", return_value=self.now + timedelta(seconds=delta)):
                    deleted_resource_gc.collect_deleted_resource_gc(self.now, False)
                resource = DerivedResource.objects.get(ownerId=bad.pk)
                self.assertEqual(resource.cleanupAttempts, expected)
        self.assertEqual(calls.count(bad.storageKey), 3)
        self.assertEqual(calls.count(good.storageKey), 1)
        self.assertEqual(resource.state, "quarantined")
        self.assertIsNotNone(resource.quarantinedAt)
        self.assertTrue(default_storage.exists(bad.storageKey))
        self.assertFalse(default_storage.exists(good.storageKey))

    def test_expired_cleanup_claim_also_consumes_the_persistent_ceiling(self):
        item = self.deleted_object("gc/crash.txt")
        resource = DerivedResource.objects.get(ownerId=item.pk)
        for attempt in range(1, 4):
            now = self.now + timedelta(minutes=6 * attempt)
            with patch("django.utils.timezone.now", return_value=now):
                claimed = deleted_resource_gc._claim_resource(resource.pk, f"worker-{attempt}")
            self.assertEqual(claimed.cleanupAttempts, attempt)
        with patch("django.utils.timezone.now", return_value=self.now + timedelta(hours=1)):
            self.assertIsNone(deleted_resource_gc._claim_resource(resource.pk, "replacement"))
        resource.refresh_from_db()
        self.assertEqual(resource.state, "quarantined")
        self.assertEqual(resource.cleanupAttempts, 3)
        self.assertTrue(default_storage.exists(item.storageKey))

    def test_exact_key_backoff_is_shared_by_restarts_and_preserves_other_keys(self):
        failing = "snapshot/permanent.snapshot"
        calls = []

        def delete(key):
            calls.append(key)
            if key == failing:
                raise OSError("storage offline")

        for delta, expected in [(0, 1), (0, 1), (60, 2), (180, 3), (10000, 3)]:
            with patch("django.utils.timezone.now", return_value=self.now + timedelta(seconds=delta)):
                clean_storage_key(failing, "workspaceSnapshot", delete)
            retry = StorageCleanupRetry.objects.get(storageKey=failing)
            self.assertEqual(retry.cleanupAttempts, expected)
        self.assertEqual(retry.state, "quarantined")
        self.assertEqual(calls, [failing] * 3)
        result = clean_storage_key("snapshot/healthy.snapshot", "workspaceSnapshot", delete)
        self.assertEqual(result.state, "cleaned")

    def test_failure_inside_owner_transaction_commits_retry_and_delayed_schedule(self):
        from django.db import transaction

        with transaction.atomic():
            result = clean_storage_key(
                "snapshot/owned.snapshot", "workspaceSnapshot",
                lambda _: (_ for _ in ()).throw(PermissionError("denied")),
            )
            self.assertEqual(result.state, "failed")
        retry = StorageCleanupRetry.objects.get(storageKey="snapshot/owned.snapshot")
        self.assertEqual(retry.cleanupAttempts, 1)
        self.assertIsNotNone(retry.nextCleanupAt)

    def test_exact_key_expired_claims_consume_ceiling_without_physical_deletion(self):
        key = "snapshot/crashed.snapshot"
        default_storage.save(key, ContentFile(b"retained bytes"))
        now = self.now
        for attempt in range(1, 4):
            with patch("django.utils.timezone.now", return_value=now):
                claim = claim_storage_key(key, "workspaceSnapshot")
                self.assertEqual(claim.cleanupAttempts, attempt)
                self.assertIsNone(claim_storage_key(key, "workspaceSnapshot"))
            now = claim.leaseExpiresAt
        with patch("django.utils.timezone.now", return_value=now):
            self.assertIsNone(claim_storage_key(key, "workspaceSnapshot"))
        retry = StorageCleanupRetry.objects.get(storageKey=key)
        self.assertEqual(retry.state, "quarantined")
        self.assertEqual(retry.cleanupAttempts, 3)
        self.assertEqual(retry.quarantinedAt, now)
        self.assertEqual(retry.leaseOwner, "")
        self.assertIsNone(retry.leaseExpiresAt)
        self.assertTrue(default_storage.exists(key))

    def test_stale_exact_key_owner_cannot_overwrite_replacement_claim(self):
        key = "snapshot/replaced.snapshot"
        with patch("django.utils.timezone.now", return_value=self.now):
            stale = claim_storage_key(key, "workspaceSnapshot")
        with patch("django.utils.timezone.now", return_value=stale.leaseExpiresAt):
            replacement = claim_storage_key(key, "workspaceSnapshot")
            with self.assertRaisesRegex(RuntimeError, "storage_cleanup_lease_lost"):
                clean_storage_key(key, "workspaceSnapshot", lambda _: None, claim=stale)
            retry = StorageCleanupRetry.objects.get(storageKey=key)
            self.assertEqual(retry.state, "cleaning")
            self.assertEqual(retry.leaseOwner, replacement.leaseOwner)
            self.assertEqual(retry.cleanupAttempts, 2)
            self.assertIsNone(retry.cleanedAt)
            self.assertEqual(
                clean_storage_key(key, "workspaceSnapshot", lambda _: None, claim=replacement).state,
                "cleaned",
            )
