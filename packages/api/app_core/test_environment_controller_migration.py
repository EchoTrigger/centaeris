"""Controller migration grants no authority to historical per-Run bindings."""
from django.db.migrations.executor import MigrationExecutor
from django.db import transaction
from django.test import TransactionTestCase

from .migration_testing import isolated_migration_database
from .runtime_contract import authorization_digest, authorization_signature
from .test_agent_run_authorization import fixture, KEY


class EnvironmentControllerMigrationTests(TransactionTestCase):
    serialized_rollback = True
    old = [("app_core", "0016_session_environments")]
    current = [("app_core", "0017_environment_controllers")]

    def test_history_preserved_no_controller_invented_and_reversal_blocks_consumed_epoch(self):
        with isolated_migration_database(self.old) as database:
            alias = database.alias
            historical = MigrationExecutor(database).loader.project_state(self.old).apps
            model = lambda name: historical.get_model("app_core", name).objects.using(alias)
            user = historical.get_model("auth", "User").objects.using(alias).create(username="controller-upgrade")
            workspace = model("Workspace").create(name="Retained", createdBy_id=user.pk)
            member = model("WorkspaceMembership").create(workspace_id=workspace.pk, user_id=user.pk, role="owner")
            agent = model("Agent").create(workspace_id=workspace.pk, owner_id=user.pk, name="Retained")
            config = model("ModelConfig").create(displayName="Retained")
            with transaction.atomic(using=alias):
                session = model("Session").create(workspace_id=workspace.pk, owner_id=user.pk, agent_id=agent.pk)
                environment = model("SessionEnvironment").create(session_id=session.pk)
                model("Session").filter(pk=session.pk).update(current_environment_id=environment.pk)
            run = model("AgentRun").create(workspace_id=workspace.pk, user_id=user.pk,
                session_id=session.pk, environment_id=environment.pk, membership_ref=member.pk,
                modelConfig_id=config.pk, prompt="retained", status="completed")
            payload = fixture() | {"id": "authorization-" + run.pk, "workspaceId": workspace.pk,
                "userId": str(user.pk), "sessionId": session.pk, "agentId": agent.pk,
                "agentRunId": run.pk, "modelConfigRef": config.pk}
            model("AgentRunAuthorization").create(id=payload["id"], agent_run_id=run.pk,
                payload=payload, digest=authorization_digest(payload), signature=authorization_signature(payload, KEY))
            original = list(model("AgentRunAuthorization").values())
            MigrationExecutor(database).migrate(self.current)
            state = MigrationExecutor(database).loader.project_state(self.current).apps
            Environment = state.get_model("app_core", "SessionEnvironment").objects.using(alias)
            upgraded = Environment.get(pk=environment.pk)
            self.assertEqual(upgraded.controller_epoch, 0)
            self.assertIsNone(upgraded.controller_run_id)
            self.assertEqual(upgraded.controller_lease_owner, "")
            self.assertTrue(upgraded.controller_released)
            self.assertEqual(upgraded.controller_purpose, "")
            self.assertEqual(list(model("AgentRunAuthorization").values()), original)
            Environment.filter(pk=environment.pk).update(controller_epoch=1,
                controller_run_id=run.pk, controller_lease_owner="former-controller", controller_released=True,
                controller_purpose="cleanup")
            with self.assertRaisesRegex(RuntimeError, "controller_history_requires_forward_migration"):
                MigrationExecutor(database).migrate(self.old)
            self.assertEqual(Environment.get(pk=environment.pk).controller_epoch, 1)
