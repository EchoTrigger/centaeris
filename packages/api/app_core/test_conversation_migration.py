"""Existing branches retain their identities through the published-instance migration."""
from django.db import DEFAULT_DB_ALIAS, connection, router
from django.db.migrations.executor import MigrationExecutor
from django.db.migrations.recorder import MigrationRecorder
from django.test import TransactionTestCase

from .migration_testing import isolated_migration_database


class ConversationMigrationTests(TransactionTestCase):
    serialized_rollback = True

    def test_upgrade_marks_existing_branch_without_recreating_messages_or_sessions(self):
        before = [("app_core", "0006_session_event_payload_storage")]
        recorded = MigrationRecorder(connection).applied_migrations()
        with isolated_migration_database(before) as isolated:
            alias = isolated.alias
            old = MigrationExecutor(isolated).loader.project_state(before).apps
            user = old.get_model("auth", "User").objects.using(alias).create(username="conversation-upgrade")
            workspace = old.get_model("app_core", "Workspace").objects.using(alias).create(name="Upgrade", createdBy=user)
            Agent = old.get_model("app_core", "Agent")
            root = Agent.objects.using(alias).create(workspace=workspace, owner=user, name="Root")
            agent = Agent.objects.using(alias).create(workspace=workspace, owner=user, name="Branch")
            self.assertEqual(Agent.objects.get(pk=agent.pk)._state.db, alias)
            session = old.get_model("app_core", "Session").objects.using(alias).create(workspace=workspace, owner=user, agent=agent)
            old.get_model("app_core", "AgentCoordinationSession").objects.using(alias).create(agent=agent, session=session)
            app = old.get_model("app_core", "BusinessApplication").objects.using(alias).create(name="App", status="active", created_by=user)
            branch = old.get_model("app_core", "BusinessAgentBranch").objects.using(alias).create(app=app, root_agent=root,
                agent=agent, session=session, business_user_id="user")
            original = old.get_model("app_core", "BusinessAgentBranch").objects.using(alias).values().get(pk=branch.pk)
            latest = MigrationExecutor(isolated)
            latest.migrate(latest.loader.graph.leaf_nodes("app_core"))
            from .models import Agent as CurrentAgent, BusinessAgentBranch
            self.assertEqual(BusinessAgentBranch.objects.using(alias).values().get(pk=branch.pk), original)
            self.assertTrue(CurrentAgent.objects.using(alias).get(pk=agent.pk).is_business_instance)
            self.assertFalse(CurrentAgent.objects.using(alias).get(pk=root.pk).is_business_instance)
        self.assertEqual(MigrationRecorder(connection).applied_migrations(), recorded)
        self.assertEqual(router.db_for_read(Agent), DEFAULT_DB_ALIAS)
        self.assertFalse(CurrentAgent.objects.using(DEFAULT_DB_ALIAS).filter(pk__in=[root.pk, agent.pk]).exists())
        if connection.vendor == "postgresql":
            with connection.cursor() as cursor:
                cursor.execute("SELECT datname FROM pg_database WHERE datname=%s", [alias])
                self.assertEqual(cursor.fetchall(), [])
