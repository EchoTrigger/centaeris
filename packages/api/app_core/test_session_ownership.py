"""Focused Session authority and forward migration behavior, using a fresh test DB."""
import json
from contextlib import nullcontext
from importlib import import_module
from unittest.mock import patch
from django.contrib.auth import get_user_model
from django.db import DatabaseError, connection, transaction
from django.db.migrations.executor import MigrationExecutor
from django.test import TestCase, TransactionTestCase, SimpleTestCase
from .models import ModelConfig, Session, SessionProject, Workspace, WorkspaceMembership
from .testing import create_session
from .session_model_settings import selected_session_model, SessionModelSettingsError


class SessionOwnershipTests(TestCase):
    def setUp(self):
        self.user = get_user_model().objects.create_user(username="session-owner", password="unused")
        self.other = get_user_model().objects.create_user(username="other-owner", password="unused")
        self.workspace = Workspace.objects.create(name="Workspace", createdBy=self.user)
        WorkspaceMembership.objects.create(workspace=self.workspace, user=self.user, role="owner")
        self.client.force_login(self.user)

    def test_creation_projects_and_instructions_have_only_session_ownership(self):
        project = self.client.post(f"/api/workspaces/{self.workspace.pk}/session-projects",
            json.dumps({"name":"Project"}), content_type="application/json")
        self.assertEqual(project.status_code, 201, project.content)
        project_id = project.json()["project"]["id"]
        created = self.client.post(f"/api/workspaces/{self.workspace.pk}/sessions",
            json.dumps({"operationId":"create-session", "projectId":project_id}), content_type="application/json")
        self.assertEqual(created.status_code, 201, created.content)
        sid = created.json()["sessionId"]
        result = self.client.patch(f"/api/sessions/{sid}", json.dumps({"instructions":"Use this Session's instructions."}), content_type="application/json")
        self.assertEqual(result.status_code, 200, result.content)
        self.assertEqual(result.json()["session"]["instructions"], "Use this Session's instructions.")
        self.assertNotIn("agentId", result.json()["session"])
        self.assertEqual(Session.objects.get(pk=sid).project_id, project_id)
        self.assertEqual(self.client.get(f"/api/workspaces/{self.workspace.pk}/sessions?agentId=old").status_code, 400)
        self.assertEqual(self.client.get("/api/agents").status_code, 404)
        self.assertEqual(self.client.get("/api/v1/chats").status_code, 404)
        self.client.force_login(self.other)
        self.assertEqual(self.client.get(f"/api/sessions/{sid}").status_code, 404)

    def test_legacy_agent_request_and_implicit_model_selection_are_rejected(self):
        result = self.client.post(f"/api/workspaces/{self.workspace.pk}/sessions",
            json.dumps({"operationId":"old", "agentId":"old"}), content_type="application/json")
        self.assertEqual(result.status_code, 400)
        ModelConfig.objects.create(id="explicit-model")
        with self.assertRaises(SessionModelSettingsError):
            selected_session_model(self.workspace.pk, "", None)
        model, mode = selected_session_model(self.workspace.pk, "explicit-model", None)
        self.assertEqual(model.pk, "explicit-model")
        with self.assertRaisesRegex(SessionModelSettingsError, "model_thinking_mode_unsupported"):
            selected_session_model(self.workspace.pk, "explicit-model", "")

    def test_retired_storage_queue_protects_shared_library_and_reclaims_unreferenced_key(self):
        import hashlib
        from .models import StorageCleanupRetry, UserLibraryObject
        from .deleted_resource_gc import collect_retired_session_gc
        key = "retired-coordination/exact.bin"
        StorageCleanupRetry.objects.create(keyDigest=hashlib.sha256(key.encode()).hexdigest(), storageKey=key, collector="retiredSession")
        owner = UserLibraryObject.objects.create(owner=self.user, displayName="Shared", objectKind="file",
            contentType="text/plain", sizeBytes=1, sha256="sha256:"+"a"*64, storageKey=key, status="ready")
        with patch("app_core.deleted_resource_gc.delete_stored_object_for_gc") as remove:
            report = collect_retired_session_gc(False)
            self.assertEqual(report.blocked, [key])
            remove.assert_not_called()
            UserLibraryObject.objects.filter(pk=owner.pk).update(storageKey="other-owner-key")
            report = collect_retired_session_gc(False)
            self.assertEqual(report.cleaned, [key])
            remove.assert_called_once_with(key)

    def test_mcp_credentials_follow_current_workspace_enablement_and_rotation(self):
        from .models import AgentRun, McpBearerCredential, WorkspacePluginEnablement
        from .agent_run_authorization_factory import create_agent_run_authorization
        from .assistant_connectors import authorize_connector, ConnectorRejected
        from .credentials import encrypt_credential_secret
        session = create_session(workspace=self.workspace, owner=self.user)
        model = ModelConfig.objects.create()
        run = AgentRun.objects.create(workspace=self.workspace, session=session, user=self.user, modelConfig=model, prompt="connector")
        with patch("app_core.agent_run_authorization_factory.plugin_lifecycle_lock", return_value=nullcontext()), \
                patch("app_core.plugin_catalog.plugin_lifecycle_lock", return_value=nullcontext()):
            grant = create_agent_run_authorization(run, image_digest="sha256:"+"a"*64)
        WorkspacePluginEnablement.objects.create(workspace=self.workspace, pluginName="connector-test")
        credential = McpBearerCredential.objects.create(workspace=self.workspace, plugin_name="connector-test",
            credential_ref="token", display_name="Test", encrypted_secret=encrypt_credential_secret("test-bearer"), created_by=self.user, updated_by=self.user)
        resource = {"resourcePath":"mcp.json", "resourceDigest":"sha256:"+"b"*64, "credentialRef":"token"}
        body = {"pluginName":"connector-test", "serverId":"server", "resourcePath":resource["resourcePath"],
            "resourceDigest":resource["resourceDigest"], "operation":"connect", "bindingDigest":None}
        with patch("app_core.assistant_connectors.connector_resource", return_value=resource):
            first = authorize_connector(grant, body)
            self.assertEqual((first["scope"], first["token"]), ("workspace", "test-bearer"))
            body.update(operation="dispatch", bindingDigest=first["bindingDigest"])
            self.assertIsNone(authorize_connector(grant, body)["token"])
            McpBearerCredential.objects.filter(pk=credential.pk).update(version=2)
            with self.assertRaisesRegex(ConnectorRejected, "mcp_connector_binding_changed"):
                authorize_connector(grant, body)
            WorkspacePluginEnablement.objects.filter(workspace=self.workspace).delete()
            with self.assertRaisesRegex(ConnectorRejected, "mcp_connector_capability_revoked"):
                authorize_connector(grant, body)


class SessionOwnershipMigrationTests(TransactionTestCase):
    migrate_from = ("app_core", "0037_merge_workspace_resources_and_agents")
    migrate_to = ("app_core", "0038_session_ownership")

    def _restore_current_test_schema(self):
        if self._schema_is_current or connection.vendor != "postgresql":
            return
        self.assertTrue(connection.settings_dict["NAME"].startswith("test_"))
        connection.rollback()
        with connection.cursor() as cursor:
            cursor.execute("DROP SCHEMA public CASCADE")
            cursor.execute("CREATE SCHEMA public")
        executor = MigrationExecutor(connection)
        executor.migrate(executor.loader.graph.leaf_nodes())

    def test_ordinary_history_and_original_signature_survive_coordinator_retirement(self):
        self._schema_is_current = False
        self.addCleanup(self._restore_current_test_schema)
        executor = MigrationExecutor(connection)
        # The migration is deliberately forward-only; construct the old schema
        # in this fresh test DB instead of reversing an applied retirement.
        from django.db.migrations.recorder import MigrationRecorder
        if connection.vendor == "postgresql":
            self.assertTrue(connection.settings_dict["NAME"].startswith("test_"),
                "Schema recreation is restricted to a Django-owned test_ database")
            with connection.cursor() as cursor:
                cursor.execute("DROP SCHEMA public CASCADE")
                cursor.execute("CREATE SCHEMA public")
        else:
            with connection.schema_editor() as editor:
                state = executor.loader.project_state([self.migrate_to])
                for model in reversed(list(state.apps.get_app_config("app_core").get_models())):
                    editor.delete_model(model)
            MigrationRecorder(connection).migration_qs.filter(app="app_core").delete()
        executor = MigrationExecutor(connection)
        dependencies = [node for node in executor.loader.graph.leaf_nodes() if node[0] != "app_core"]
        executor.migrate([self.migrate_from, *dependencies])
        old = executor.loader.project_state([self.migrate_from]).apps
        get = lambda name: old.get_model("app_core", name)
        user = get_user_model().objects.create_user(username="migration-owner")
        workspace = get("Workspace").objects.create(name="Before", createdBy_id=user.pk)
        member = get("WorkspaceMembership").objects.create(workspace=workspace, user_id=user.pk, role="owner")
        agent = get("Agent").objects.create(workspace=workspace, owner_id=user.pk, name="Retired", instructions="Do not copy")
        with transaction.atomic():
            ordinary = get("Session").objects.create(workspace=workspace, owner_id=user.pk, agent=agent)
            env = get("SessionEnvironment").objects.create(session=ordinary)
            get("Session").objects.filter(pk=ordinary.pk).update(current_environment=env)
            coordinator = get("Session").objects.create(workspace=workspace, owner_id=user.pk, agent=agent)
            coord_env = get("SessionEnvironment").objects.create(session=coordinator)
            get("Session").objects.filter(pk=coordinator.pk).update(current_environment=coord_env)
            get("AgentCoordinationSession").objects.create(agent=agent, session=coordinator)
        from pathlib import Path
        from django.utils import timezone
        vector = json.loads((Path(__file__).resolve().parents[3] / "tests/workspace/fixtures/agent_run_authorization/v1/valid.json").read_text())
        for environment in (env, coord_env):
            get("SessionEnvironment").objects.filter(pk=environment.pk).update(
                environment_profile=vector["environmentProfile"], environment_profile_digest=vector["environmentProfileDigest"],
                environment_profile_operation_id="historical-bound-profile", environment_profile_bound_at=timezone.now())
        model = get("ModelConfig").objects.create()
        run = get("AgentRun").objects.create(workspace=workspace, session=ordinary, environment=env,
            user_id=user.pk, modelConfig=model, membership_ref=member.pk, prompt="history", status="completed")
        payload = {"schema":"workspace.agent_run_authorization.v1", "agentId":agent.pk, "opaque":"historical"}
        get("AgentRunAuthorization").objects.create(agent_run=run, payload=payload, digest="original-digest", signature="original-signature")
        get("SessionEvent").objects.create(eventId="history-event", workspace=workspace, session=ordinary,
            agent_run=run, sequence=1, agent_run_sequence=1, projects_to_agent_run_stream=True, payload={"type":"user_message"}, createdAtMs=1)
        coord_run = get("AgentRun").objects.create(workspace=workspace, session=coordinator, environment=coord_env,
            user_id=user.pk, modelConfig=model, membership_ref=member.pk, prompt="discard", status="completed")
        release = get("AgentRunEnvironmentRelease").objects.create(agent_run=coord_run, environment=coord_env,
            authorization_digest="original-coordinator-digest", controller_epoch=1,
            controller_lease_digest="sha256:"+"f"*64)
        artifact = get("Artifact").objects.create(workspace=workspace, session=coordinator, agent_run=coord_run,
            createdBy_id=user.pk, displayName="Retired", contentType="text/plain", sizeBytes=1,
            sha256="sha256:"+"c"*64, storageKey="exact-coordinator-artifact", status="published")
        ordinary_artifact = get("Artifact").objects.create(workspace=workspace, session=ordinary, agent_run=run,
            createdBy_id=user.pk, displayName="Keep", contentType="text/plain", sizeBytes=1,
            sha256="sha256:"+"d"*64, storageKey="ordinary-artifact", status="published")
        app = get("BusinessApplication").objects.create(name="Old app", created_by_id=user.pk, status="active")
        delegation = get("UserAppDelegation").objects.create(user_id=user.pk, app=app, workspace=workspace,
            agent=agent, membership_ref=member.pk, scopes=["sessions:read"], token_digest="sha256:"+"e"*64)
        migration = import_module("app_core.migrations.0038_session_ownership")
        get("AgentRun").objects.filter(pk=run.pk).update(status="queued")
        with connection.schema_editor() as editor:
            with self.assertRaisesRegex(RuntimeError, "settled_runs"):
                migration.retire_agent_data(old, editor)
        get("AgentRun").objects.filter(pk=run.pk).update(status="completed")
        executor = MigrationExecutor(connection)
        executor.migrate([self.migrate_to])
        self._schema_is_current = True
        current = executor.loader.project_state([self.migrate_to]).apps
        self.assertFalse(current.get_model("app_core", "Session").objects.filter(pk=coordinator.pk).exists())
        session = current.get_model("app_core", "Session").objects.get(pk=ordinary.pk)
        self.assertEqual(session.instructions, "")
        grant = current.get_model("app_core", "AgentRunAuthorization").objects.get(agent_run_id=run.pk)
        self.assertEqual((grant.payload, grant.digest, grant.signature), (payload, "original-digest", "original-signature"))
        self.assertTrue(current.get_model("app_core", "SessionEvent").objects.filter(pk="history-event").exists())
        self.assertTrue(current.get_model("app_core", "Artifact").objects.filter(pk=ordinary_artifact.pk).exists())
        self.assertFalse(current.get_model("app_core", "Artifact").objects.filter(pk=artifact.pk).exists())
        retry = current.get_model("app_core", "StorageCleanupRetry").objects.get(storageKey="exact-coordinator-artifact")
        self.assertEqual((retry.collector, retry.state), ("retiredSession", "pending"))
        self.assertIsNotNone(current.get_model("app_core", "UserAppDelegation").objects.get(pk=delegation.pk).revoked_at)
        if connection.vendor == "postgresql":
            # The migration's narrow coordinator deletion permission is gone;
            # retained ordinary profile and release history remain immutable.
            with self.assertRaisesMessage(DatabaseError, "environment_profile_binding_immutable"):
                with transaction.atomic():
                    current.get_model("app_core", "SessionEnvironment").objects.filter(pk=env.pk).update(environment_profile_digest="changed")
            kept_release = current.get_model("app_core", "AgentRunEnvironmentRelease").objects.create(
                agent_run_id=run.pk, environment_id=env.pk, authorization_digest="original-digest",
                controller_epoch=1, controller_lease_digest="sha256:"+"a"*64)
            with self.assertRaisesMessage(DatabaseError, "environment_release_immutable"):
                with transaction.atomic():
                    current.get_model("app_core", "AgentRunEnvironmentRelease").objects.filter(pk=kept_release.pk).delete()
