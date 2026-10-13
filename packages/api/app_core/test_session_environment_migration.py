"""Upgrade retained Sessions and grants without rewriting execution history."""
import json

from django.db import IntegrityError, transaction
from django.db.migrations.executor import MigrationExecutor
from django.test import TransactionTestCase
from django.utils import timezone

from .migration_testing import isolated_migration_database
from .runtime_contract import authorization_digest, authorization_signature
from .test_agent_run_authorization import fixture, KEY


class SessionEnvironmentMigrationTests(TransactionTestCase):
    serialized_rollback = True
    old = [("app_core", "0015_note_revision_immutability")]
    current = [("app_core", "0016_session_environments")]

    def test_forward_preserves_all_signed_history_and_reverse_refuses_identity_loss(self):
        with isolated_migration_database(self.old) as database:
            alias = database.alias
            historical = MigrationExecutor(database).loader.project_state(self.old).apps
            model = lambda name: historical.get_model("app_core", name).objects.using(alias)
            user = historical.get_model("auth", "User").objects.using(alias).create(username="environment-migration")
            workspace = model("Workspace").create(name="Retained", createdBy_id=user.pk)
            membership = model("WorkspaceMembership").create(workspace_id=workspace.pk, user_id=user.pk, role="owner")
            agent = model("Agent").create(workspace_id=workspace.pk, owner_id=user.pk, name="Retained")
            config = model("ModelConfig").create(displayName="Retained")
            sessions, grants = [], {}
            now = timezone.now()
            for status in ("active", "deleted", "purged"):
                session = model("Session").create(workspace_id=workspace.pk, owner_id=user.pk, agent_id=agent.pk,
                    status="active" if status == "active" else "deleted",
                    deletedAt=None if status == "active" else now, purgedAt=now if status == "purged" else None)
                sessions.append(session.pk)
                for run_status in ("queued", "running", "completed", "failed", "cancelled"):
                    run = model("AgentRun").create(workspace_id=workspace.pk, user_id=user.pk, session_id=session.pk,
                        membership_ref=membership.pk, modelConfig_id=config.pk, prompt="retained", status=run_status)
                    payload = fixture() | {"id": "authorization-" + run.pk, "workspaceId": workspace.pk,
                        "userId": str(user.pk), "sessionId": session.pk, "agentId": agent.pk,
                        "agentRunId": run.pk, "modelConfigRef": config.pk}
                    digest, signature = authorization_digest(payload), authorization_signature(payload, KEY)
                    model("AgentRunAuthorization").create(id=payload["id"], agent_run_id=run.pk,
                        payload=payload, digest=digest, signature=signature)
                    grants[run.pk] = (json.dumps(payload, sort_keys=True), digest, signature)
            # Include a conversation which has never created a Run.
            empty = model("Session").create(workspace_id=workspace.pk, owner_id=user.pk, agent_id=agent.pk)
            sessions.append(empty.pk)
            MigrationExecutor(database).migrate(self.current)
            upgraded = MigrationExecutor(database).loader.project_state(self.current).apps
            Environment = upgraded.get_model("app_core", "SessionEnvironment")
            Session = upgraded.get_model("app_core", "Session")
            Run = upgraded.get_model("app_core", "AgentRun")
            bindings = dict(Environment.objects.using(alias).values_list("session_id", "id"))
            self.assertEqual(set(bindings), set(sessions))
            self.assertEqual(len(set(bindings.values())), len(sessions))
            for session in Session.objects.using(alias).all():
                self.assertEqual(session.current_environment_id, bindings[session.pk])
            for run in Run.objects.using(alias).all():
                self.assertEqual(run.environment_id, bindings[run.session_id])
                authorization = model("AgentRunAuthorization").get(agent_run_id=run.pk)
                self.assertEqual((json.dumps(authorization.payload, sort_keys=True), authorization.digest,
                                  authorization.signature), grants[run.pk])
            MigrationExecutor(database).migrate(self.current)
            self.assertEqual(dict(Environment.objects.using(alias).values_list("session_id", "id")), bindings)
            with self.assertRaisesRegex(RuntimeError, "reversal_requires_empty_environments"):
                MigrationExecutor(database).migrate(self.old)
            # Refusing reversal must leave the original protection installed.
            run = Run.objects.using(alias).first()
            foreign = next(value for session, value in bindings.items() if session != run.session_id)
            with self.assertRaises(IntegrityError), transaction.atomic(using=alias):
                Run.objects.using(alias).filter(pk=run.pk).update(environment_id=foreign)

    def test_empty_schema_reverses_and_can_be_upgraded_again(self):
        with isolated_migration_database(self.old) as database:
            MigrationExecutor(database).migrate(self.current)
            MigrationExecutor(database).migrate(self.old)
            MigrationExecutor(database).migrate(self.current)
            state = MigrationExecutor(database).loader.project_state(self.current).apps
            self.assertFalse(state.get_model("app_core", "SessionEnvironment").objects.using(database.alias).exists())
