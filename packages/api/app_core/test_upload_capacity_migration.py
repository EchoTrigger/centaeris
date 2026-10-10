"""Forward schema and refusal to erase a live temporary liability."""
import tempfile
from pathlib import Path

from django.core.management import call_command
from django.db import connection
from django.db.migrations.executor import MigrationExecutor
from django.test import TransactionTestCase, override_settings


class UploadCapacityMigrationTests(TransactionTestCase):
    serialized_rollback = True

    def test_forward_capacity_survives_rejected_reverse_until_offline_cleanup(self):
        from .models import UploadCapacity, UploadLease
        from .upload_capacity import mark_cleanup_unknown, reserve_ingress
        from .upload_temp import UploadTempTracker, initialize_upload_temp_root
        temporary = tempfile.TemporaryDirectory(prefix="upload-migration-")
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        target = [("app_core", "0011_upload_capacity")]
        previous = [("app_core", "0010_active_run_admission")]
        latest = MigrationExecutor(connection).loader.graph.leaf_nodes("app_core")
        with override_settings(MEDIA_ROOT=str(root / "stored"), FILE_UPLOAD_TEMP_DIR=str(root / "pool"),
                UPLOAD_BODY_MAX_BYTES=8, UPLOAD_TEMP_MAX_BYTES=128, UPLOAD_MAX_CONCURRENT=1):
            # Exercise the new forward migration, then an actual persisted spool.
            MigrationExecutor(connection).migrate(previous)
            try:
                MigrationExecutor(connection).migrate(target)
                lease = reserve_ingress(4)
                pool, pool_ref = initialize_upload_temp_root()
                tracker = UploadTempTracker(pool, pool_ref, lease.pk)
                handle = tracker.open_file(body=True)
                handle.write(b"held")
                handle.close()
                mark_cleanup_unknown(lease.pk)
                with self.assertRaisesRegex(RuntimeError, "upload_capacity_rollback_requires_confirmed_cleanup"):
                    MigrationExecutor(connection).migrate(previous)
                self.assertEqual(UploadLease.objects.get(pk=lease.pk).state, "unknown")
                self.assertEqual(UploadCapacity.objects.get(pk=1).reservedBytes, 8)
                self.assertEqual(Path(handle.name).read_bytes(), b"held")
                call_command("reconcile_upload_capacity", api_workers_stopped=True)
                self.assertFalse(Path(handle.name).exists())
                MigrationExecutor(connection).migrate(previous)
                self.assertNotIn("app_core_uploadlease", connection.introspection.table_names())
            finally:
                MigrationExecutor(connection).migrate(latest)
