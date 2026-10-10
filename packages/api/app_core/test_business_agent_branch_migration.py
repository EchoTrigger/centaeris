"""Business identity is forward-compatible and cannot be discarded on downgrade."""
from unittest.mock import patch

from django.db import connection
from django.db.migrations.executor import MigrationExecutor
from django.db.migrations.recorder import MigrationRecorder
from django.test import TransactionTestCase
from .migration_testing import isolated_migration_database


class BusinessAgentBranchMigrationTests(TransactionTestCase):
    serialized_rollback = True
    migrate_from = [("app_core", "0003_persistent_browser_login")]
    migrate_to = [("app_core", "0004_business_agent_branches")]

    def test_forward_upgrade_preserves_root_resources_without_inventing_subjects(self):
        with isolated_migration_database(self.migrate_from) as isolated:
            alias = isolated.alias
            old = MigrationExecutor(isolated).loader.project_state(self.migrate_from).apps
            user = old.get_model("auth", "User").objects.using(alias).create(username="branch-upgrade-owner")
            workspace = old.get_model("app_core", "Workspace").objects.using(alias).create(name="Upgrade", createdBy=user)
            agent = old.get_model("app_core", "Agent").objects.using(alias).create(workspace=workspace, owner=user,
                name="Business root", instructions="Original policy\n中文")
            session = old.get_model("app_core", "Session").objects.using(alias).create(workspace=workspace, owner=user, agent=agent)
            original_agent = old.get_model("app_core", "Agent").objects.using(alias).values().get(pk=agent.pk)
            original_session = old.get_model("app_core", "Session").objects.using(alias).values().get(pk=session.pk)
            latest = MigrationExecutor(isolated)
            latest.migrate(self.migrate_to)
            current = latest.loader.project_state(self.migrate_to).apps
            self.assertEqual(current.get_model("app_core", "Agent").objects.using(alias).values().get(pk=agent.pk), original_agent)
            self.assertEqual(current.get_model("app_core", "Session").objects.using(alias).values().get(pk=session.pk), original_session)
            self.assertFalse(current.get_model("app_core", "BusinessAgentBranch").objects.using(alias).exists())

    def test_downgrade_refuses_to_discard_existing_subject_and_agent_identity(self):
        with isolated_migration_database(self.migrate_to) as isolated:
            alias = isolated.alias
            apps = MigrationExecutor(isolated).loader.project_state(self.migrate_to).apps
            user = apps.get_model("auth", "User").objects.using(alias).create(username="branch-downgrade-owner")
            workspace = apps.get_model("app_core", "Workspace").objects.using(alias).create(name="Upgrade", createdBy=user)
            app = apps.get_model("app_core", "BusinessApplication").objects.using(alias).create(
                name="Business app", status="active", created_by=user)
            agents = apps.get_model("app_core", "Agent").objects.using(alias)
            root = agents.create(workspace=workspace, owner=user, name="Business root")
            agent = agents.create(workspace=workspace, owner=user, name="Business branch")
            session = apps.get_model("app_core", "Session").objects.using(alias).create(workspace=workspace, owner=user, agent=agent)
            apps.get_model("app_core", "AgentCoordinationSession").objects.using(alias).create(agent=agent, session=session)
            branches = apps.get_model("app_core", "BusinessAgentBranch").objects.using(alias)
            branch = branches.create(app=app, root_agent=root, business_user_id="ExactUser", agent=agent, session=session)
            with self.assertRaisesRegex(RuntimeError, "business agent branches"):
                MigrationExecutor(isolated).migrate(self.migrate_from)
            preserved = branches.get(pk=branch.pk)
            self.assertEqual((preserved.business_user_id, preserved.agent_id, preserved.session_id),
                             ("ExactUser", agent.pk, session.pk))

    def test_fixture_errors_leave_shared_schema_unchanged_and_remove_owned_database(self):
        recorded = MigrationRecorder(connection).applied_migrations()
        tables = connection.introspection.table_names()
        names = []

        def failed_initial_migration(executor, targets):
            names.append(executor.connection.settings_dict["NAME"])
            raise RuntimeError("migration_fixture_initial_failure")

        with patch("app_core.migration_testing.MigrationExecutor.migrate", autospec=True,
                   side_effect=failed_initial_migration):
            with self.assertRaisesMessage(RuntimeError, "migration_fixture_initial_failure"):
                with isolated_migration_database(self.migrate_from):
                    self.fail("failed initialization must not enter the fixture body")
        with self.assertRaisesMessage(RuntimeError, "migration_fixture_body_failure"):
            with isolated_migration_database(self.migrate_from) as isolated:
                names.append(isolated.settings_dict["NAME"])
                raise RuntimeError("migration_fixture_body_failure")

        self.assertEqual(MigrationRecorder(connection).applied_migrations(), recorded)
        self.assertEqual(connection.introspection.table_names(), tables)
        if connection.vendor == "postgresql":
            with connection.cursor() as cursor:
                cursor.execute("SELECT datname FROM pg_database WHERE datname IN (%s, %s)", names)
                self.assertEqual(cursor.fetchall(), [])
