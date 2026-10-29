"""Publication tokens preserve released cleanup retry facts on forward upgrade."""
from datetime import timedelta

from django.db.migrations.executor import MigrationExecutor
from django.test import TransactionTestCase
from django.utils import timezone

from .migration_testing import isolated_migration_database


class WorkspaceSnapshotPublicationMigrationTests(TransactionTestCase):
    serialized_rollback=True

    def test_forward_preserves_retry_state_and_versioned_history_blocks_reversal(self):
        old=[("app_core","0033_session_workspace_authority")]
        current=[("app_core","0034_workspace_snapshot_publication")]
        with isolated_migration_database(old) as database:
            apps=MigrationExecutor(database).loader.project_state(old).apps
            retries=apps.get_model("app_core","StorageCleanupRetry").objects.using(database.alias)
            retries.create(keyDigest="a"*64,storageKey="original.snapshot",collector="workspaceSnapshot",
                state="failed",cleanupAttempts=2,nextCleanupAt=timezone.now()+timedelta(days=2),lastFailure="retained original failure")
            before=retries.values().get()
            MigrationExecutor(database).migrate(current)
            upgraded=MigrationExecutor(database).loader.project_state(current).apps
            rows=upgraded.get_model("app_core","StorageCleanupRetry").objects.using(database.alias)
            after=rows.values().get()
            self.assertEqual(after.pop("publicationToken"),"")
            self.assertEqual(after,before)
            self.assertFalse(upgraded.get_model("app_core","WorkspaceSnapshotObject").objects.using(database.alias).exists())
            rows.update(publicationToken="b"*32)
            with self.assertRaisesRegex(RuntimeError,"publication_requires_forward_migration"):
                MigrationExecutor(database).migrate(old)
            self.assertEqual(rows.get().publicationToken,"b"*32)
