"""Historical Environment profiles retain their own resource authority during upgrade."""
import copy

from django.db import IntegrityError, transaction
from django.db.migrations.executor import MigrationExecutor
from django.test import TransactionTestCase, override_settings
from django.utils import timezone

from .environment_profile import environment_profile_digest
from .migration_testing import isolated_migration_database
from .runtime_contract import authorization_digest, authorization_signature
from .test_agent_run_authorization import fixture


@override_settings(SANDBOX_CPU_MILLI=1000, SANDBOX_MEMORY_BYTES=2 * 1024**3,
    SANDBOX_WORKSPACE_BYTES=4 * 1024**3, SANDBOX_PIDS_LIMIT=512)
class EnvironmentResourcesMigrationTests(TransactionTestCase):
    serialized_rollback = True
    old = [("app_core", "0055_session_purge_intent")]
    current = [("app_core", "0056_workspace_environment_resources")]

    def test_upgrade_copies_authoritative_resources_without_rewriting_profiles_or_signed_history(self):
        with isolated_migration_database(self.old) as database:
            apps = MigrationExecutor(database).loader.project_state(self.old).apps
            manager = lambda name: apps.get_model("app_core", name).objects.using(database.alias)
            user = apps.get_model("auth", "User").objects.using(database.alias).create(username="resource-history")
            workspace = manager("Workspace").create(name="Retained resources", createdBy_id=user.pk)
            membership = manager("WorkspaceMembership").create(workspace_id=workspace.pk, user_id=user.pk, role="owner")
            config = manager("ModelConfig").create(displayName="Retained resources")

            def session_with_environment():
                with transaction.atomic(using=database.alias):
                    session = manager("Session").create(workspace_id=workspace.pk, owner_id=user.pk)
                    environment = manager("SessionEnvironment").create(session_id=session.pk)
                    manager("Session").filter(pk=session.pk).update(current_environment_id=environment.pk)
                return session, environment

            session, bound = session_with_environment()
            _, unbound = session_with_environment()
            payload = fixture()
            profile = copy.deepcopy(payload["environmentProfile"])
            profile["resources"].update(cpuMilli=3000, memoryBytes=2**53 + 1, storageBytes=2**64 - 1)
            profile_digest = environment_profile_digest(profile)
            manager("SessionEnvironment").filter(pk=bound.pk).update(environment_profile=profile,
                environment_profile_digest=profile_digest, environment_profile_operation_id="historical-bind",
                environment_profile_bound_at=timezone.now(), network=profile["network"])
            run = manager("AgentRun").create(workspace_id=workspace.pk, session_id=session.pk,
                environment_id=bound.pk, user_id=user.pk, membership_ref=membership.pk,
                modelConfig_id=config.pk, thinkingMode=payload["thinkingMode"], prompt="Retained", status="completed")
            payload.update(id="authorization-" + run.pk, workspaceId=workspace.pk, sessionId=session.pk,
                agentRunId=run.pk, userId=str(user.pk), modelConfigRef=config.pk,
                environmentProfile=profile, environmentProfileDigest=profile_digest)
            grant = manager("AgentRunAuthorization").create(id=payload["id"], agent_run_id=run.pk,
                payload=payload, digest=authorization_digest(payload), signature=authorization_signature(payload, "test-key"))
            original = {name: list(manager(name).order_by("pk").values())
                for name in ("Workspace", "Session", "SessionEnvironment", "AgentRun", "AgentRunAuthorization")}

            MigrationExecutor(database).migrate(self.current)
            current = MigrationExecutor(database).loader.project_state(self.current).apps
            Workspace = current.get_model("app_core", "Workspace")
            Environment = current.get_model("app_core", "SessionEnvironment")
            defaults = {"cpuMilli": 1000, "memoryBytes": 2 * 1024**3, "pidsLimit": 512,
                "workspaceBytes": 4 * 1024**3, "storageBytes": 4 * 1024**3}
            upgraded_workspace = Workspace.objects.using(database.alias).get(pk=workspace.pk)
            self.assertEqual(upgraded_workspace.execution_resources, defaults)
            self.assertEqual(upgraded_workspace.execution_settings_revision, 1)
            configured = Environment.objects.using(database.alias).get(pk=bound.pk)
            empty = Environment.objects.using(database.alias).get(pk=unbound.pk)
            self.assertEqual(configured.resource_defaults, profile["resources"])
            self.assertEqual(empty.resource_defaults, defaults)
            self.assertIsNone(configured.resource_defaults_revision)
            self.assertIsNone(empty.resource_defaults_revision)
            self.assertIsNone(empty.environment_profile)
            self.assertEqual(configured.environment_profile, profile)
            self.assertEqual(configured.environment_profile_digest, profile_digest)
            for name, rows in original.items():
                self.assertEqual(list(manager(name).order_by("pk").values()), rows)
            retained = manager("AgentRunAuthorization").get(pk=grant.pk)
            self.assertEqual(authorization_digest(retained.payload), retained.digest)
            self.assertEqual(authorization_signature(retained.payload, "test-key"), retained.signature)
            with self.assertRaisesRegex(IntegrityError, "environment_resource_defaults_immutable"), transaction.atomic(using=database.alias):
                Environment.objects.using(database.alias).filter(pk=bound.pk).update(resource_defaults_revision=1)
            later = Environment.objects.using(database.alias).create(session_id=session.pk,
                resource_defaults={"forged": True}, resource_defaults_revision=999)
            later.refresh_from_db(using=database.alias)
            self.assertEqual(later.resource_defaults, defaults)
            self.assertEqual(later.resource_defaults_revision, 1)
