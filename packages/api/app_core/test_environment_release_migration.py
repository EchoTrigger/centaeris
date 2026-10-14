"""Reconstruct only the last authoritative business release, preserving history."""
from django.db import transaction
from django.db.migrations.executor import MigrationExecutor
from django.test import TransactionTestCase

from .migration_testing import isolated_migration_database
from .runtime_contract import authorization_digest, authorization_signature
from .test_agent_run_authorization import fixture, KEY


class EnvironmentReleaseMigrationTests(TransactionTestCase):
    serialized_rollback = True
    old = [("app_core", "0017_environment_controllers")]
    current = [("app_core", "0018_environment_release_receipts")]

    def seed(self, database):
        alias = database.alias
        state = MigrationExecutor(database).loader.project_state(self.old).apps
        model = lambda name: state.get_model("app_core", name).objects.using(alias)
        user = state.get_model("auth", "User").objects.using(alias).create(username="release-upgrade")
        workspace = model("Workspace").create(name="Release history", createdBy_id=user.pk)
        member = model("WorkspaceMembership").create(workspace_id=workspace.pk, user_id=user.pk, role="owner")
        agent = model("Agent").create(workspace_id=workspace.pk, owner_id=user.pk, name="Retained")
        config = model("ModelConfig").create(displayName="Retained")
        with transaction.atomic(using=alias):
            session = model("Session").create(workspace_id=workspace.pk, owner_id=user.pk, agent_id=agent.pk)
            environment = model("SessionEnvironment").create(session_id=session.pk)
            model("Session").filter(pk=session.pk).update(current_environment_id=environment.pk)
        runs = []
        for _ in range(2):
            run = model("AgentRun").create(workspace_id=workspace.pk, user_id=user.pk,
                session_id=session.pk, environment_id=environment.pk, membership_ref=member.pk,
                modelConfig_id=config.pk, prompt="historical", status="completed")
            payload = fixture() | {"id": "authorization-" + run.pk, "workspaceId": workspace.pk,
                "userId": str(user.pk), "sessionId": session.pk, "agentId": agent.pk,
                "agentRunId": run.pk, "modelConfigRef": config.pk, "thinkingMode": run.thinkingMode or None}
            model("AgentRunAuthorization").create(id=payload["id"], agent_run_id=run.pk,
                payload=payload, digest=authorization_digest(payload), signature=authorization_signature(payload, KEY))
            runs.append(run)
        model("SessionEnvironment").filter(pk=environment.pk).update(controller_run_id=runs[0].pk,
            controller_epoch=7, controller_lease_owner="recorded-owner", controller_purpose="cleanup", controller_released=True)
        return model, environment, runs

    def test_forward_preserves_source_records_and_only_reconstructs_last_released_controller(self):
        with isolated_migration_database(self.old) as database:
            model, environment, runs = self.seed(database)
            original = {name: list(model(name).order_by("pk").values())
                for name in ("SessionEnvironment", "AgentRun", "AgentRunAuthorization")}
            MigrationExecutor(database).migrate(self.current)
            state = MigrationExecutor(database).loader.project_state(self.current).apps
            Receipt = state.get_model("app_core", "AgentRunEnvironmentRelease").objects.using(database.alias)
            self.assertEqual(Receipt.count(), 1)
            receipt = Receipt.get(agent_run_id=runs[0].pk)
            self.assertTrue(receipt.reconstructed)
            self.assertEqual(receipt.controller_epoch, 7)
            self.assertEqual(receipt.environment_id, environment.pk)
            self.assertFalse(Receipt.filter(agent_run_id=runs[1].pk).exists())
            for name, values in original.items():
                self.assertEqual(list(model(name).order_by("pk").values()), values)
            with self.assertRaisesRegex(RuntimeError, "release_history_requires_forward_migration"):
                MigrationExecutor(database).migrate(self.old)
            self.assertEqual(Receipt.count(), 1)

    def test_inconsistent_candidate_grant_explicitly_refuses_forward_migration(self):
        with isolated_migration_database(self.old) as database:
            model, _, runs = self.seed(database)
            model("AgentRunAuthorization").filter(agent_run_id=runs[0].pk).update(digest="sha256:" + "0" * 64)
            with self.assertRaisesRegex(RuntimeError, "release_reconstruction_binding_invalid"):
                MigrationExecutor(database).migrate(self.current)
