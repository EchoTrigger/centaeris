"""A new deletion intent leaves existing Session identities and states unchanged."""
from django.db import transaction
from django.db.migrations.executor import MigrationExecutor
from django.test import TransactionTestCase
from django.utils import timezone

from .migration_testing import isolated_migration_database


class SessionPurgeIntentMigrationTests(TransactionTestCase):
    serialized_rollback = True
    previous = [("app_core", "0054_session_body_search")]
    target = [("app_core", "0055_session_purge_intent")]

    def test_forward_adds_null_intent_without_rewriting_historical_sessions(self):
        with isolated_migration_database(self.previous) as database:
            alias = database.alias
            historical = MigrationExecutor(database).loader.project_state(self.previous).apps
            user = historical.get_model("auth", "User").objects.using(alias).create(username="purge-intent-upgrade")
            workspaces = historical.get_model("app_core", "Workspace").objects.using(alias)
            sessions = historical.get_model("app_core", "Session").objects.using(alias)
            environments = historical.get_model("app_core", "SessionEnvironment").objects.using(alias)
            workspace = workspaces.create(name="Purge intent upgrade", createdBy_id=user.pk)
            historical.get_model("app_core", "WorkspaceMembership").objects.using(alias).create(
                workspace_id=workspace.pk, user_id=user.pk, role="owner",
            )
            for status in ("active", "deleted"):
                at = timezone.now() if status == "deleted" else None
                with transaction.atomic(using=alias):
                    session = sessions.create(workspace_id=workspace.pk, owner_id=user.pk,
                        title="Historical " + status, status=status, deletedAt=at, purgedAt=at)
                    environment = environments.create(session_id=session.pk)
                    sessions.filter(pk=session.pk).update(current_environment_id=environment.pk)
            previous = list(sessions.order_by("pk").values())
            original_environments = list(environments.order_by("pk").values())

            MigrationExecutor(database).migrate(self.target)
            upgraded = MigrationExecutor(database).loader.project_state(self.target).apps
            current = upgraded.get_model("app_core", "Session").objects.using(alias)
            for row, original in zip(current.order_by("pk").values(), previous, strict=True):
                self.assertIsNone(row.pop("purgeRequestedAt"))
                self.assertEqual(row, original)
            self.assertEqual(list(upgraded.get_model("app_core", "SessionEnvironment").objects.using(alias)
                .order_by("pk").values()), original_environments)
