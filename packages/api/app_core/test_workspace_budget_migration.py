"""Forward resource rebinding preserves released authorization and workspace facts."""
import copy

from django.db import connection, transaction
from django.db.migrations.executor import MigrationExecutor
from django.test import TransactionTestCase
from django.utils import timezone

from .migration_authorization_fixtures import historical_digest, historical_signature
from .migration_testing import isolated_migration_database
from .test_agent_run_authorization import fixture
from .test_unified_environment_profile import profile


class WorkspaceResourceMigrationTests(TransactionTestCase):
    serialized_rollback = True
    old = [("app_core", "0034_workspace_snapshot_publication")]
    current = [("app_core", "0035_workspace_resource_contract")]

    def seed(self, database):
        apps = MigrationExecutor(database).loader.project_state(self.old).apps
        alias = database.alias
        manager = lambda name: apps.get_model("app_core", name).objects.using(alias)
        user = apps.get_model("auth", "User").objects.using(alias).create(username="workspace-budget")
        workspace = manager("Workspace").create(name="Workspace budget", createdBy_id=user.pk)
        member = manager("WorkspaceMembership").create(workspace_id=workspace.pk, user_id=user.pk, role="owner")
        agent = manager("Agent").create(workspace_id=workspace.pk, owner_id=user.pk, name="Budget")
        config = manager("ModelConfig").create(displayName="Budget")
        with transaction.atomic(using=alias):
            owner = manager("Session").create(workspace_id=workspace.pk, owner_id=user.pk, agent_id=agent.pk)
            environment = manager("SessionEnvironment").create(session_id=owner.pk)
            manager("Session").filter(pk=owner.pk).update(current_environment_id=environment.pk)
        original_profile = profile()
        original_profile["resources"]["dataTmpfsBytes"] = original_profile["resources"].pop("workspaceBytes")
        original_profile["resources"]["storageBytes"] *= 2
        manager("SessionEnvironment").filter(pk=environment.pk).update(environment_profile=original_profile,
            network=original_profile["network"], environment_profile_digest=historical_digest(original_profile),
            environment_profile_operation_id="original-binding", environment_profile_bound_at=timezone.now())
        run = manager("AgentRun").create(workspace_id=workspace.pk, user_id=user.pk,
            session_id=owner.pk, environment_id=environment.pk, membership_ref=member.pk,
            modelConfig_id=config.pk, prompt="original history", status="completed")
        value = fixture()
        value.update(id="authorization-budget-history", workspaceId=workspace.pk, userId=str(user.pk),
            sessionId=owner.pk, agentId=agent.pk, agentRunId=run.pk, modelConfigRef=config.pk,
            environmentProfile=copy.deepcopy(original_profile),
            environmentProfileDigest=historical_digest(original_profile))
        grant = manager("AgentRunAuthorization").create(id=value["id"], agent_run_id=run.pk,
            payload=value, digest=historical_digest(value), signature=historical_signature(value, "test-key"))
        manager("Session").filter(pk=owner.pk).update(workspaceGeneration=4, workspaceBaselineKnown=True,
            workspaceStorageKey="retained.snapshot", workspaceSnapshotSha256="sha256:" + "a" * 64,
            workspaceSnapshotSizeBytes=100, workspaceExpandedSizeBytes=3, workspaceFileCount=1,
            workspaceLastAdvancedAgentRun_id=run.pk)
        if database.vendor == "postgresql":
            with database.cursor() as cursor:
                cursor.execute("CREATE SCHEMA runtime")
                cursor.execute("CREATE TABLE runtime.runtime_jobs(job_id text PRIMARY KEY,status text)")
                cursor.execute("CREATE TABLE runtime.resident_sandboxes(environment_id text)")
                cursor.execute("CREATE TABLE runtime.execution_instances(execution_id text PRIMARY KEY,environment_id text,instance_id text)")
                cursor.execute("CREATE TABLE runtime.environment_creation_attempts(allocation_execution_id text,instance_id text,state text)")
                cursor.execute("CREATE TABLE runtime.agent_run_cleanup_receipts(instance_id text,environment_id text,cleanup_complete bool,physical_removed bool)")
                cursor.execute("CREATE TABLE runtime.environment_retirements(instance_id text,environment_id text,state text)")
        return apps, manager, owner, environment, run, grant

    def test_rebind_preserves_history_and_workspace_and_accepts_a_new_run(self):
        with isolated_migration_database(self.old) as database:
            apps, manager, owner, environment, run, grant = self.seed(database)
            original_environment = manager("SessionEnvironment").values().get(pk=environment.pk)
            original_grant = manager("AgentRunAuthorization").values().get(pk=grant.pk)
            original_session = manager("Session").values().get(pk=owner.pk)
            MigrationExecutor(database).migrate(self.current)
            current = MigrationExecutor(database).loader.project_state(self.current).apps
            sessions = current.get_model("app_core", "Session").objects.using(database.alias)
            upgraded = sessions.values().get(pk=owner.pk)
            new_environment_id = upgraded.pop("current_environment_id")
            original_session.pop("current_environment_id")
            self.assertEqual(upgraded, original_session)
            self.assertNotEqual(new_environment_id, environment.pk)
            self.assertEqual(manager("SessionEnvironment").values().get(pk=environment.pk), original_environment)
            self.assertEqual(manager("AgentRunAuthorization").values().get(pk=grant.pk), original_grant)
            self.assertEqual(manager("AgentRun").get(pk=run.pk).environment_id, environment.pk)
            rebuilt = current.get_model("app_core", "SessionEnvironment").objects.using(database.alias).get(pk=new_environment_id)
            expected = copy.deepcopy(original_environment["environment_profile"])
            expected["resources"]["workspaceBytes"] = expected["resources"].pop("dataTmpfsBytes")
            self.assertEqual(rebuilt.environment_profile, expected)
            self.assertEqual(rebuilt.environment_profile_digest, historical_digest(expected))
            from .environment_profile import validate_environment_profile
            validate_environment_profile(rebuilt.environment_profile)
            replacement_run = current.get_model("app_core", "AgentRun").objects.using(database.alias).create(
                workspace_id=run.workspace_id, user_id=run.user_id, session_id=owner.pk,
                environment_id=rebuilt.pk, membership_ref=run.membership_ref,
                modelConfig_id=run.modelConfig_id, prompt="continue the same Session")
            self.assertEqual(replacement_run.environment_id, rebuilt.pk)
            with self.assertRaisesRegex(RuntimeError, "requires_forward_migration"):
                MigrationExecutor(database).migrate(self.old)

    def test_unfinished_run_and_controller_must_settle_before_rebinding(self):
        with isolated_migration_database(self.old) as database:
            apps, manager, owner, environment, run, grant = self.seed(database)
            manager("AgentRun").filter(pk=run.pk).update(status="queued")
            with self.assertRaisesRegex(RuntimeError, "requires_drained_environment"):
                MigrationExecutor(database).migrate(self.current)
            self.assertEqual(manager("Session").get(pk=owner.pk).current_environment_id, environment.pk)
            manager("AgentRun").filter(pk=run.pk).update(status="completed")
            manager("SessionEnvironment").filter(pk=environment.pk).update(controller_run_id=run.pk,
                controller_epoch=1, controller_lease_owner="original-lease", controller_purpose="cleanup",
                controller_released=False)
            with self.assertRaisesRegex(RuntimeError, "requires_drained_environment"):
                MigrationExecutor(database).migrate(self.current)
            manager("SessionEnvironment").filter(pk=environment.pk).update(controller_released=True)
            MigrationExecutor(database).migrate(self.current)
            self.assertNotEqual(manager("Session").get(pk=owner.pk).current_environment_id, environment.pk)

    def test_postgres_job_reservation_and_unknown_dispatch_keep_original_binding(self):
        if connection.vendor != "postgresql":
            self.skipTest("requires PostgreSQL Runtime responsibility queries")
        with isolated_migration_database(self.old) as database:
            apps, manager, owner, environment, run, grant = self.seed(database)
            with database.cursor() as cursor:
                cursor.execute("INSERT INTO runtime.runtime_jobs VALUES(%s,'queued')", ["agent_run.lifecycle:" + run.pk])
            with self.assertRaisesRegex(RuntimeError, "requires_drained_environment"):
                MigrationExecutor(database).migrate(self.current)
            with database.cursor() as cursor:
                cursor.execute("UPDATE runtime.runtime_jobs SET status='succeeded'")
                cursor.execute("INSERT INTO runtime.resident_sandboxes VALUES(%s)", [environment.pk])
            with self.assertRaisesRegex(RuntimeError, "requires_drained_environment"):
                MigrationExecutor(database).migrate(self.current)
            with database.cursor() as cursor:
                cursor.execute("DELETE FROM runtime.resident_sandboxes")
                cursor.execute("INSERT INTO runtime.execution_instances VALUES('original-allocation',%s,'original-instance')", [environment.pk])
                cursor.execute("INSERT INTO runtime.environment_creation_attempts VALUES('original-allocation','original-instance','dispatched')")
            with self.assertRaisesRegex(RuntimeError, "requires_drained_environment"):
                MigrationExecutor(database).migrate(self.current)
            self.assertEqual(manager("Session").get(pk=owner.pk).current_environment_id, environment.pk)
            with database.cursor() as cursor:
                cursor.execute("INSERT INTO runtime.agent_run_cleanup_receipts VALUES('original-instance',%s,true,true)", [environment.pk])
            MigrationExecutor(database).migrate(self.current)
            self.assertNotEqual(manager("Session").get(pk=owner.pk).current_environment_id, environment.pk)

    def test_postgres_orphan_dispatch_remains_an_unsettled_migration_dependency(self):
        if connection.vendor != "postgresql":
            self.skipTest("requires PostgreSQL Runtime responsibility queries")
        with isolated_migration_database(self.old) as database:
            apps, manager, owner, environment, run, grant = self.seed(database)
            with database.cursor() as cursor:
                cursor.execute("INSERT INTO runtime.environment_creation_attempts VALUES('missing-anchor','unknown-instance','prepared')")
            for state in ("prepared", "dispatched"):
                with database.cursor() as cursor:
                    cursor.execute("UPDATE runtime.environment_creation_attempts SET state=%s", [state])
                with self.subTest(state=state), self.assertRaisesRegex(RuntimeError, "requires_drained_environment"):
                    MigrationExecutor(database).migrate(self.current)
                self.assertEqual(manager("Session").get(pk=owner.pk).current_environment_id, environment.pk)

    def test_both_released_branches_upgrade_without_replacing_shared_agents_or_history(self):
        target = [("app_core", "0037_merge_workspace_resources_and_agents")]
        branches = ("0036_workspace_navigation_changes", "0036_workspace_file_objects")
        for branch in branches:
            with self.subTest(branch=branch), isolated_migration_database(self.old) as database:
                _, manager, owner, environment, run, grant = self.seed(database)
                original_grant = manager("AgentRunAuthorization").values().get(pk=grant.pk)
                original_run = manager("AgentRun").values().get(pk=run.pk)
                MigrationExecutor(database).migrate([("app_core", branch)])
                definition_id = None
                if branch == "0036_workspace_navigation_changes":
                    apps = MigrationExecutor(database).loader.project_state([("app_core", branch)]).apps
                    definitions = apps.get_model("app_core", "AgentDefinition").objects.using(database.alias)
                    definition = definitions.create(workspace_id=run.workspace_id, created_by_id=run.user_id,
                        name="Published workspace Agent", instructions="Shared instructions", model_config_id=run.modelConfig_id)
                    definition_id = definition.pk
                    apps.get_model("app_core", "Workspace").objects.using(database.alias).filter(
                        pk=run.workspace_id).update(default_agent_definition_id=definition_id)
                    apps.get_model("app_core", "Agent").objects.using(database.alias).filter(
                        pk=owner.agent_id).update(definition_id=definition_id)
                    original_definition = definitions.values().get(pk=definition_id)
                MigrationExecutor(database).migrate(target)
                executor = MigrationExecutor(database)
                self.assertEqual(executor.loader.detect_conflicts(), {})
                apps = executor.loader.project_state(target).apps
                sessions = apps.get_model("app_core", "Session").objects.using(database.alias)
                upgraded = sessions.get(pk=owner.pk)
                rebuilt = apps.get_model("app_core", "SessionEnvironment").objects.using(database.alias).get(
                    pk=upgraded.current_environment_id)
                self.assertNotEqual(rebuilt.pk, environment.pk)
                self.assertIn("workspaceBytes", rebuilt.environment_profile["resources"])
                self.assertNotIn("dataTmpfsBytes", rebuilt.environment_profile["resources"])
                self.assertEqual(upgraded.workspaceGeneration, 4)
                self.assertEqual(upgraded.workspaceStorageKey, "retained.snapshot")
                self.assertEqual(upgraded.agent_id, owner.agent_id)
                self.assertEqual(manager("AgentRunAuthorization").values().get(pk=grant.pk), original_grant)
                self.assertEqual(manager("AgentRun").values().get(pk=run.pk), original_run)
                if definition_id is not None:
                    self.assertEqual(apps.get_model("app_core", "AgentDefinition").objects.using(database.alias).values().get(
                        pk=definition_id), original_definition)
                    self.assertEqual(apps.get_model("app_core", "Workspace").objects.using(database.alias).get(
                        pk=run.workspace_id).default_agent_definition_id, definition_id)
                self.assertIn("WorkspaceFileObject", [model.__name__ for model in apps.get_app_config("app_core").get_models()])
