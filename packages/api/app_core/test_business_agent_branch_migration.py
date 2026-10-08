"""Business identity is forward-compatible and cannot be discarded on downgrade."""
from django.db import connection
from django.db.migrations.executor import MigrationExecutor
from django.test import TransactionTestCase


class BusinessAgentBranchMigrationTests(TransactionTestCase):
    serialized_rollback = True
    migrate_from = [("app_core", "0003_persistent_browser_login")]
    migrate_to = [("app_core", "0004_business_agent_branches")]

    def test_forward_upgrade_preserves_root_resources_without_inventing_subjects(self):
        executor = MigrationExecutor(connection)
        executor.migrate(self.migrate_from)
        old = executor.loader.project_state(self.migrate_from).apps
        try:
            user = old.get_model("auth", "User").objects.create(username="branch-upgrade-owner")
            workspace = old.get_model("app_core", "Workspace").objects.create(name="Upgrade", createdBy=user)
            agent = old.get_model("app_core", "Agent").objects.create(workspace=workspace, owner=user,
                name="Business root", instructions="Original policy\n中文")
            session = old.get_model("app_core", "Session").objects.create(workspace=workspace, owner=user, agent=agent)
            original_agent = old.get_model("app_core", "Agent").objects.values().get(pk=agent.pk)
            original_session = old.get_model("app_core", "Session").objects.values().get(pk=session.pk)
            latest = MigrationExecutor(connection)
            latest.migrate(self.migrate_to)
            current = latest.loader.project_state(self.migrate_to).apps
            self.assertEqual(current.get_model("app_core", "Agent").objects.values().get(pk=agent.pk), original_agent)
            self.assertEqual(current.get_model("app_core", "Session").objects.values().get(pk=session.pk), original_session)
            self.assertFalse(current.get_model("app_core", "BusinessAgentBranch").objects.exists())
        finally:
            latest = MigrationExecutor(connection)
            latest.migrate(latest.loader.graph.leaf_nodes("app_core"))

    def test_downgrade_refuses_to_discard_existing_subject_and_agent_identity(self):
        from .models import Agent, AgentCoordinationSession, BusinessAgentBranch, BusinessApplication, Session, Workspace
        from django.contrib.auth import get_user_model
        user = get_user_model().objects.create_user(username="branch-downgrade-owner")
        workspace = Workspace.objects.create(name="Upgrade", createdBy=user)
        app = BusinessApplication.objects.create(name="Business app", status="active", created_by=user)
        root = Agent.objects.create(workspace=workspace, owner=user, name="Business root")
        agent = Agent.objects.create(workspace=workspace, owner=user, name="Business branch", is_business_instance=True)
        session = Session.objects.create(workspace=workspace, owner=user, agent=agent)
        AgentCoordinationSession.objects.create(agent=agent, session=session)
        branch = BusinessAgentBranch.objects.create(app=app, root_agent=root, business_user_id="ExactUser",
            agent=agent, session=session)
        try:
            with self.assertRaisesRegex(RuntimeError, "business agent branches"):
                MigrationExecutor(connection).migrate(self.migrate_from)
            preserved = BusinessAgentBranch.objects.get(pk=branch.pk)
            self.assertEqual((preserved.business_user_id, preserved.agent_id, preserved.session_id),
                             ("ExactUser", agent.pk, session.pk))
        finally:
            latest = MigrationExecutor(connection)
            latest.migrate(latest.loader.graph.leaf_nodes("app_core"))
