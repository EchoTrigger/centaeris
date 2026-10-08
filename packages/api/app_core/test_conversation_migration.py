"""Existing branches retain their identities through the published-instance migration."""
from django.db import connection
from django.db.migrations.executor import MigrationExecutor
from django.test import TransactionTestCase


class ConversationMigrationTests(TransactionTestCase):
    serialized_rollback = True

    def test_upgrade_marks_existing_branch_without_recreating_messages_or_sessions(self):
        before = [("app_core", "0006_session_event_payload_storage")]
        executor = MigrationExecutor(connection)
        executor.migrate(before)
        old = executor.loader.project_state(before).apps
        try:
            user = old.get_model("auth", "User").objects.create(username="conversation-upgrade")
            workspace = old.get_model("app_core", "Workspace").objects.create(name="Upgrade", createdBy=user)
            Agent = old.get_model("app_core", "Agent")
            root = Agent.objects.create(workspace=workspace, owner=user, name="Root")
            agent = Agent.objects.create(workspace=workspace, owner=user, name="Branch")
            session = old.get_model("app_core", "Session").objects.create(workspace=workspace, owner=user, agent=agent)
            old.get_model("app_core", "AgentCoordinationSession").objects.create(agent=agent, session=session)
            app = old.get_model("app_core", "BusinessApplication").objects.create(name="App", status="active", created_by=user)
            branch = old.get_model("app_core", "BusinessAgentBranch").objects.create(app=app, root_agent=root,
                agent=agent, session=session, business_user_id="user")
            original = old.get_model("app_core", "BusinessAgentBranch").objects.values().get(pk=branch.pk)
            latest = MigrationExecutor(connection)
            latest.migrate(latest.loader.graph.leaf_nodes("app_core"))
            from .models import Agent as CurrentAgent, BusinessAgentBranch
            self.assertEqual(BusinessAgentBranch.objects.values().get(pk=branch.pk), original)
            self.assertTrue(CurrentAgent.objects.get(pk=agent.pk).is_business_instance)
            self.assertFalse(CurrentAgent.objects.get(pk=root.pk).is_business_instance)
        finally:
            latest = MigrationExecutor(connection)
            latest.migrate(latest.loader.graph.leaf_nodes("app_core"))
