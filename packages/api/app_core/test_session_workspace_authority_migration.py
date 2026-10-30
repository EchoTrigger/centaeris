"""Released empty-looking descriptors retain their missing-evidence state."""
from django.db import transaction
from django.db.migrations.executor import MigrationExecutor
from django.test import TransactionTestCase

from .migration_testing import isolated_migration_database


class SessionWorkspaceAuthorityMigrationTests(TransactionTestCase):
    serialized_rollback = True
    old = [("app_core", "0032_agent_avatar_image")]
    current = [("app_core", "0033_session_workspace_authority")]

    def test_only_committed_generations_reconstruct_authority_and_new_sessions_start_known(self):
        with isolated_migration_database(self.old) as database:
            alias = database.alias
            apps = MigrationExecutor(database).loader.project_state(self.old).apps
            manager = lambda name: apps.get_model("app_core", name).objects.using(alias)
            user = apps.get_model("auth", "User").objects.using(alias).create(username="workspace-authority")
            workspace = manager("Workspace").create(name="Workspace authority", createdBy_id=user.pk)
            member = manager("WorkspaceMembership").create(workspace_id=workspace.pk, user_id=user.pk, role="owner")
            agent = manager("Agent").create(workspace_id=workspace.pk, owner_id=user.pk, name="Authority")
            config = manager("ModelConfig").create(displayName="Authority")

            def session():
                with transaction.atomic(using=alias):
                    owner = manager("Session").create(workspace_id=workspace.pk, owner_id=user.pk, agent_id=agent.pk)
                    environment = manager("SessionEnvironment").create(session_id=owner.pk)
                    manager("Session").filter(pk=owner.pk).update(current_environment_id=environment.pk)
                return owner, environment

            no_history, _ = session()
            history, environment = session()
            manager("AgentRun").create(workspace_id=workspace.pk, user_id=user.pk, session_id=history.pk,
                environment_id=environment.pk, membership_ref=member.pk, modelConfig_id=config.pk,
                prompt="history without a workspace commit", status="completed")
            committed = []
            for nonempty in (False, True):
                owner, environment = session()
                run = manager("AgentRun").create(workspace_id=workspace.pk, user_id=user.pk,
                    session_id=owner.pk, environment_id=environment.pk, membership_ref=member.pk,
                    modelConfig_id=config.pk, prompt="committed baseline", status="completed")
                values = {"workspaceGeneration": 1, "workspaceLastAdvancedAgentRun_id": run.pk}
                if nonempty:
                    values |= {"workspaceStorageKey": "original.snapshot", "workspaceSnapshotSha256": "sha256:" + "a" * 64,
                        "workspaceSnapshotSizeBytes": 100, "workspaceExpandedSizeBytes": 3, "workspaceFileCount": 1}
                manager("Session").filter(pk=owner.pk).update(**values)
                committed.append(owner.pk)
            original_sessions = list(manager("Session").order_by("pk").values())
            original_runs = list(manager("AgentRun").order_by("pk").values())
            MigrationExecutor(database).migrate(self.current)
            upgraded = MigrationExecutor(database).loader.project_state(self.current).apps
            sessions = upgraded.get_model("app_core", "Session").objects.using(alias)
            for row in original_sessions:
                current = sessions.values().get(pk=row["id"])
                self.assertEqual(current.pop("workspaceBaselineKnown"), row["id"] in committed)
                self.assertEqual(current, row)
            self.assertFalse(sessions.get(pk=no_history.pk).workspaceBaselineKnown)
            self.assertFalse(sessions.get(pk=history.pk).workspaceBaselineKnown)
            self.assertEqual(list(manager("AgentRun").order_by("pk").values()), original_runs)
            with transaction.atomic(using=alias):
                fresh = sessions.create(workspace_id=workspace.pk, owner_id=user.pk, agent_id=agent.pk)
                env = upgraded.get_model("app_core", "SessionEnvironment").objects.using(alias).create(session_id=fresh.pk)
                sessions.filter(pk=fresh.pk).update(current_environment_id=env.pk)
            self.assertTrue(sessions.get(pk=fresh.pk).workspaceBaselineKnown)
            with self.assertRaisesRegex(RuntimeError, "authority_requires_forward_migration"):
                MigrationExecutor(database).migrate(self.old)
