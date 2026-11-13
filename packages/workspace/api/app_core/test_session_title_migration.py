from django.db import transaction
from django.db.migrations.executor import MigrationExecutor
from django.test import TransactionTestCase

from .migration_testing import isolated_migration_database


class SessionTitleMigrationTests(TransactionTestCase):
    serialized_rollback = True

    def test_existing_titles_preserved_without_enabling_generation(self):
        previous = [("app_core", "0059_workspace_guide_manual_steps")]
        target = [("app_core", "0060_session_titles")]
        with isolated_migration_database(previous) as database:
            alias = database.alias
            historical = MigrationExecutor(database).loader.project_state(previous).apps
            user = historical.get_model("auth", "User").objects.using(alias).create(username="title-upgrade")
            workspace = historical.get_model("app_core", "Workspace").objects.using(alias).create(name="Existing", createdBy_id=user.pk)
            sessions = historical.get_model("app_core", "Session").objects.using(alias)
            with transaction.atomic(using=alias):
                named = sessions.create(workspace_id=workspace.pk, owner_id=user.pk, title="已有的手动标题")
                unnamed = sessions.create(workspace_id=workspace.pk, owner_id=user.pk)
                for session in (named, unnamed):
                    environment = historical.get_model("app_core", "SessionEnvironment").objects.using(alias).create(session_id=session.pk)
                    sessions.filter(pk=session.pk).update(current_environment_id=environment.pk)
            MigrationExecutor(database).migrate(target)
            current = MigrationExecutor(database).loader.project_state(target).apps
            upgraded = current.get_model("app_core", "Session").objects.using(alias)
            self.assertEqual(upgraded.get(pk=named.pk).title, "已有的手动标题")
            self.assertEqual(upgraded.get(pk=named.pk).titleRevision, 1)
            self.assertEqual(upgraded.get(pk=unnamed.pk).titleRevision, 0)
            self.assertFalse(current.get_model("app_core", "WorkspaceModelDefaults").objects.using(alias).exists())
            self.assertFalse(current.get_model("app_core", "SessionTitleJob").objects.using(alias).exists())
