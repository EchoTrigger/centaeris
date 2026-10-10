"""Old cleanup liabilities survive the forward upgrade and refused downgrade."""

from contextlib import contextmanager
from datetime import timedelta
from unittest.mock import patch

from django.db import connections
from django.db.migrations.exceptions import IrreversibleError
from django.db.migrations.executor import MigrationExecutor
from django.test import TransactionTestCase, override_settings
from django.utils import timezone

from .migration_testing import isolated_migration_database


@contextmanager
def migration_alias(database, alias):
    """Exercise default routing against the disposable database, never the runner DB."""
    if alias != "default":
        yield
        return
    previous = connections["default"]
    previous_alias = database.alias
    connections["default"] = database
    database.alias = "default"
    try:
        yield
    finally:
        database.alias = previous_alias
        connections["default"] = previous


@override_settings(GC_MAX_CLEANUP_ATTEMPTS=10)
class StorageGcMigrationTests(TransactionTestCase):
    serialized_rollback = True
    migrate_from = [("app_core", "0007_business_definition_instances")]
    migrate_to = [("app_core", "0009_storage_gc_backoff")]

    def seed_resources(self, database):
        old = MigrationExecutor(database).loader.project_state(self.migrate_from).apps
        resources = old.get_model("app_core", "DerivedResource").objects.using(database.alias)
        now = timezone.now()
        fixtures = [("failed", 2), ("failed", 5), ("pending", 1), ("cleaning", 4), ("cleaned", 7)]
        for state, attempts in fixtures:
            resources.create(
                id=f"legacy-{state}-{attempts}", ownerKind="userLibraryObject",
                ownerId=f"owner-{state}-{attempts}", ownerContentGeneration=1,
                deletionGeneration=1, resourceKind="storageObject",
                resourceKey=f"legacy/{state}-{attempts}", state=state,
                tombstonedAt=now - timedelta(days=40), cleanupAttempts=attempts,
                lastFailure="legacy diagnostic" if state == "failed" else "",
                leaseOwner="legacy-worker" if state == "cleaning" else "",
                leaseExpiresAt=now + timedelta(minutes=5) if state == "cleaning" else None,
                cleanedAt=now if state == "cleaned" else None,
            )
        return list(resources.order_by("id").values()), now

    def test_upgrade_preserves_attempts_and_current_policy_for_default_and_alternate_alias(self):
        for requested_alias in ("default", "alternate"):
            with self.subTest(alias=requested_alias):
                with isolated_migration_database(self.migrate_from) as database:
                    self.assertEqual(database.vendor, "postgresql")
                    with migration_alias(database, requested_alias):
                        before, now = self.seed_resources(database)
                        with patch("django.utils.timezone.now", return_value=now):
                            MigrationExecutor(database).migrate(self.migrate_to)
                        current = MigrationExecutor(database).loader.project_state(self.migrate_to).apps
                        resources = current.get_model("app_core", "DerivedResource").objects.using(database.alias)
                        for legacy in before:
                            upgraded = resources.get(pk=legacy["id"])
                            for field, expected in legacy.items():
                                self.assertEqual(getattr(upgraded, field), expected, (requested_alias, field))
                            self.assertIsNone(upgraded.quarantinedAt)
                            self.assertEqual(
                                upgraded.nextCleanupAt,
                                now + timedelta(days=1) if legacy["state"] == "failed" else None,
                            )

    def test_refused_downgrade_keeps_data_tables_columns_and_migration_record(self):
        with isolated_migration_database(self.migrate_from) as database:
            self.assertEqual(database.vendor, "postgresql")
            self.seed_resources(database)
            MigrationExecutor(database).migrate(self.migrate_to)
            current = MigrationExecutor(database).loader.project_state(self.migrate_to).apps
            resources = current.get_model("app_core", "DerivedResource").objects.using(database.alias)
            retries = current.get_model("app_core", "StorageCleanupRetry").objects.using(database.alias)
            retries.create(
                keyDigest="a" * 64, storageKey="snapshot/retained.snapshot", collector="workspaceSnapshot",
                state="failed", cleanupAttempts=3, nextCleanupAt=timezone.now() + timedelta(days=1),
                lastFailure="retained cleanup failure",
            )
            rows = list(resources.order_by("id").values())
            retry_rows = list(retries.values())
            with database.cursor() as cursor:
                tables = set(database.introspection.table_names(cursor))
                columns = {item.name for item in database.introspection.get_table_description(cursor, resources.model._meta.db_table)}
            applied = set(MigrationExecutor(database).loader.applied_migrations)
            with self.assertRaises(IrreversibleError):
                MigrationExecutor(database).migrate(self.migrate_from)
            self.assertEqual(list(resources.order_by("id").values()), rows)
            self.assertEqual(list(retries.values()), retry_rows)
            with database.cursor() as cursor:
                self.assertEqual(set(database.introspection.table_names(cursor)), tables)
                self.assertEqual(
                    {item.name for item in database.introspection.get_table_description(cursor, resources.model._meta.db_table)},
                    columns,
                )
            self.assertEqual(set(MigrationExecutor(database).loader.applied_migrations), applied)
