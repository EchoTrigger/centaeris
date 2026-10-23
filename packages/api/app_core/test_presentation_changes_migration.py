"""Forward installation preserves facts and requires retained dirtiness to survive."""
from django.db.migrations.executor import MigrationExecutor
from django.test import TransactionTestCase

from .migration_testing import isolated_migration_database


class PresentationChangesMigrationTests(TransactionTestCase):
    serialized_rollback = True

    old = [("app_core", "0027_session_attention_watermarks")]
    current = [("app_core", "0028_presentation_changes")]

    def test_forward_install_preserves_sources_and_observes_direct_sql_changes(self):
        with isolated_migration_database(self.old) as database:
            alias = database.alias
            apps = MigrationExecutor(database).loader.project_state(self.old).apps
            user = apps.get_model("auth", "User").objects.using(alias).create(username="presentation-upgrade")
            workspace = apps.get_model("app_core", "Workspace").objects.using(alias).create(name="Upgrade", createdBy_id=user.pk)
            agent = apps.get_model("app_core", "Agent").objects.using(alias).create(workspace_id=workspace.pk, owner_id=user.pk, name="Upgrade")
            original = list(apps.get_model("app_core", "Agent").objects.using(alias).values())
            MigrationExecutor(database).migrate(self.current)
            upgraded = MigrationExecutor(database).loader.project_state(self.current).apps
            self.assertEqual(list(upgraded.get_model("app_core", "Agent").objects.using(alias).values()), original)
            with database.cursor() as cursor:
                cursor.execute('UPDATE app_core_agent SET name=%s WHERE id=%s', ["Renamed after upgrade", agent.pk])
            pending = upgraded.get_model("app_core", "PresentationChangeOutbox").objects.using(alias)
            self.assertTrue(pending.filter(key=f"agent:{agent.pk}:navigation").exists())
            with self.assertRaisesRegex(RuntimeError, "presentation_change_history_requires_forward_migration"):
                MigrationExecutor(database).migrate(self.old)

    def test_unused_install_can_reverse_and_reinstall_without_stale_triggers(self):
        with isolated_migration_database(self.current) as database:
            MigrationExecutor(database).migrate(self.old)
            MigrationExecutor(database).migrate(self.current)
            apps = MigrationExecutor(database).loader.project_state(self.current).apps
            apps.get_model("auth", "User").objects.using(database.alias).create(username="fresh-presentation-upgrade")
            self.assertTrue(apps.get_model("app_core", "PresentationChangeOutbox").objects.using(database.alias).filter(key="authority").exists())
