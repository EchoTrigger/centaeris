"""Agent Run API credentials bound execution authority and exclude product sessions."""

import hashlib
import json
from datetime import timedelta
from types import SimpleNamespace
from unittest.mock import patch

from django.contrib.auth import get_user_model
from django.test import SimpleTestCase, TransactionTestCase
from django.utils import timezone

from .models import (
    AgentRun,
    Artifact,
    ModelConfig,
    ModelProvider,
    AgentRunApiCredential,
    AgentRunApiOperation,
    Session,
    Source,
    SourceObject,
    Workspace,
    WorkspaceMembership,
)
from .agent_run_api_credentials import (
    CredentialRejected,
    authenticate_credential,
    authority_is_current,
    create_credential,
    plugin_activation_for_run,
    source_object_allowed,
)
from .plugin_catalog import PLUGIN_ACTIVATION_SCHEMA, activation_digest
from .workspace_access import agent_run_membership_is_current


class AgentRunApiPluginScopeTests(SimpleTestCase):
    def setUp(self):
        self.run = SimpleNamespace(workspace=object(), session=SimpleNamespace(purpose="agentRunApi"))
        self.credential = self.enterContext(patch(
            "app_core.agent_run_api_credentials.credential_for_run",
            return_value=SimpleNamespace(pluginNames=[]),
        ))
        self.catalog = self.enterContext(patch(
            "app_core.agent_run_api_credentials.plugin_activation_for_workspace",
            side_effect=ValueError("plugin_package_missing:unrelated"),
        ))

    def test_empty_scope_does_not_load_unrelated_plugins(self):
        self.assertEqual(plugin_activation_for_run(self.run), {
            "schema": PLUGIN_ACTIVATION_SCHEMA,
            "digest": activation_digest([]),
            "packages": [],
        })
        self.catalog.assert_not_called()

    def test_invalid_binding_is_rejected_before_loading_plugins(self):
        self.credential.return_value = None
        with self.assertRaisesRegex(CredentialRejected, "api_credential_binding_invalid"):
            plugin_activation_for_run(self.run)
        self.catalog.assert_not_called()

    def test_nonempty_scope_keeps_filtering_available_plugins(self):
        self.credential.return_value.pluginNames = ["allowed", "unavailable"]
        self.catalog.side_effect = None
        self.catalog.return_value = {
            "schema": PLUGIN_ACTIVATION_SCHEMA,
            "packages": [{"name": "allowed"}, {"name": "private"}],
        }
        self.assertEqual(plugin_activation_for_run(self.run), {
            "schema": PLUGIN_ACTIVATION_SCHEMA,
            "digest": activation_digest([{"name": "allowed"}]),
            "packages": [{"name": "allowed"}],
        })
        self.catalog.assert_called_once_with(self.run.workspace)

    def test_interactive_run_still_uses_workspace_plugins(self):
        self.run.session.purpose = "interactive"
        with self.assertRaisesRegex(ValueError, "plugin_package_missing:unrelated"):
            plugin_activation_for_run(self.run)
        self.catalog.assert_called_once_with(self.run.workspace)
        self.credential.assert_not_called()


class AgentRunApiCredentialAuthorityTests(TransactionTestCase):
    serialized_rollback = True

    def setUp(self):
        self.user = get_user_model().objects.create_user(username="api-owner")
        self.member = get_user_model().objects.create_user(username="api-member")
        self.workspace = Workspace.objects.create(name="API", createdBy=self.user)
        self.membership = WorkspaceMembership.objects.create(workspace=self.workspace, user=self.user, role="owner")
        WorkspaceMembership.objects.create(workspace=self.workspace, user=self.member, role="member")
        self.model = ModelConfig.objects.create()

    def credential(self, **kwargs):
        return create_credential(self.user, self.workspace.pk, self.model.pk, name="Backend", **kwargs)

    def create_run(self, credential):
        session = Session.objects.create(workspace=self.workspace, owner=self.user, purpose="agentRunApi")
        run = AgentRun.objects.create(workspace=self.workspace, session=session, user=self.user,
            modelConfig=self.model, prompt="Create a document")
        now = timezone.now()
        AgentRunApiOperation.objects.create(credential=credential, operationId="agent-run-api-authority", requestDigest="a" * 64,
            agentRunId=run.pk, sessionId=session.pk, deadlineAt=now + timedelta(minutes=10),
            expiresAt=now + timedelta(hours=24))
        return run

    def test_list_excludes_secrets_and_deletion_invalidates_current_runs(self):
        credential, token = self.credential()
        self.assertRegex(token, r"\Aapp-[A-Za-z0-9]{24}\Z")
        self.assertEqual(credential.secretDigest, hashlib.sha256(token.encode()).hexdigest())
        self.assertEqual(authenticate_credential(token).pk, credential.pk)
        run = self.create_run(credential)
        self.assertTrue(agent_run_membership_is_current(run))
        self.client.force_login(self.user)
        listed = self.client.get(f"/api/workspaces/{self.workspace.pk}/api-credentials")
        self.assertEqual(listed.status_code, 200, listed.content)
        self.assertNotIn(token, listed.content.decode())
        self.assertNotIn("secretDigest", listed.content.decode())
        deleted = self.client.delete(f"/api/workspaces/{self.workspace.pk}/api-credentials/{credential.pk}")
        self.assertEqual(deleted.status_code, 204, deleted.content)
        self.assertFalse(authority_is_current(credential))
        self.assertFalse(agent_run_membership_is_current(run))
        with self.assertRaisesRegex(CredentialRejected, "api_credential_invalid"):
            authenticate_credential(token)
        with self.assertRaisesRegex(CredentialRejected, "api_credential_invalid"):
            authenticate_credential(token + "invalid")

    def test_previously_issued_long_key_still_authenticates(self):
        credential, _ = self.credential()
        token = f"{credential.pk}.{'a' * 43}"
        AgentRunApiCredential.objects.filter(pk=credential.pk).update(
            secretDigest=hashlib.sha256(token.encode()).hexdigest(),
            encryptedSecret="",
        )
        self.assertEqual(authenticate_credential(token).pk, credential.pk)

    def test_sponsor_role_and_model_access_are_rechecked(self):
        credential, _ = self.credential()
        self.assertTrue(authority_is_current(credential))
        WorkspaceMembership.objects.filter(pk=self.membership.pk).update(role="member")
        self.assertFalse(authority_is_current(credential))
        WorkspaceMembership.objects.filter(pk=self.membership.pk).update(role="admin")
        self.assertTrue(authority_is_current(credential))
        ModelConfig.objects.filter(pk=self.model.pk).update(enabled=False)
        self.assertFalse(authority_is_current(credential))
        ModelConfig.objects.filter(pk=self.model.pk).update(enabled=True)
        self.assertTrue(authority_is_current(credential))
        self.membership.delete()
        WorkspaceMembership.objects.create(workspace=self.workspace, user=self.user, role="owner")
        self.assertFalse(authority_is_current(credential))

    def test_only_current_admin_can_create_with_an_accessible_model(self):
        with self.assertRaisesRegex(CredentialRejected, "api_credential_forbidden"):
            create_credential(self.member, self.workspace.pk, self.model.pk, name="Denied")
        foreign = Workspace.objects.create(name="Foreign", createdBy=self.user)
        provider = ModelProvider.objects.create(workspace=foreign, displayName="Foreign provider",
            api="openai-completions", apiBase="https://example.invalid/v1")
        model = ModelConfig.objects.create(provider=provider)
        with self.assertRaisesRegex(CredentialRejected, "model_not_found"):
            create_credential(self.user, self.workspace.pk, model.pk, name="Denied")
        self.assertFalse(AgentRunApiCredential.objects.exists())

    def test_sources_and_plugin_activation_are_limited_to_credential_scope(self):
        source = Source.objects.create(workspace=self.workspace, sourceType="fileTree", name="Files",
            status="ready", createdBy=self.user)
        item = SourceObject.objects.create(workspace=self.workspace, source=source, objectType="file",
            displayPath="source.txt", displayName="source.txt", sourceVersion="one", status="ready")
        credential, _ = self.credential(source_object_refs=[item.pk])
        self.assertTrue(source_object_allowed(credential, item))
        other, _ = self.credential()
        self.assertFalse(source_object_allowed(other, item))
        source.status = "deleted"
        self.assertFalse(source_object_allowed(credential, item))
        run = self.create_run(credential)
        AgentRunApiCredential.objects.filter(pk=credential.pk).update(pluginNames=["allowed"])
        available = {"schema": "plugin.activation.v1", "packages": [{"name": "allowed"}, {"name": "private"}]}
        with patch("app_core.agent_run_api_credentials.plugin_activation_for_workspace", return_value=available):
            self.assertEqual(plugin_activation_for_run(run)["packages"], [{"name": "allowed"}])

    def test_temporary_session_and_artifact_are_hidden_from_product_entry_points(self):
        credential, _ = self.credential()
        run = self.create_run(credential)
        artifact = Artifact.objects.create(workspace=self.workspace, session=run.session, agent_run=run,
            createdBy=self.user, displayName="result.docx", safeFilename="result.docx", sizeBytes=1,
            sha256="sha256:" + "a" * 64, storageKey="temporary-result", status="published", publishedAt=timezone.now())
        interactive = Session.objects.create(workspace=self.workspace, owner=self.user)
        self.client.force_login(self.user)
        listing = self.client.get(f"/api/workspaces/{self.workspace.pk}/sessions")
        self.assertEqual(listing.status_code, 200, listing.content)
        self.assertEqual([item["id"] for item in listing.json()["sessions"]], [interactive.pk])
        for path in (
            f"/api/sessions/{run.session_id}",
            f"/api/sessions/{run.session_id}/transcript",
            f"/api/sessions/{run.session_id}/assets",
            f"/api/sessions/{run.session_id}/agent-runs/{run.pk}/events",
            f"/api/sessions/{run.session_id}/agent-runs/{run.pk}/citations",
            f"/api/artifacts/{artifact.pk}/download",
            f"/api/office-preview/artifact/{artifact.pk}",
            f"/api/workspaces/{self.workspace.pk}/presentation/events?sessionId={run.session_id}",
        ):
            with self.subTest(path=path):
                response = self.client.get(path)
                self.assertEqual(response.status_code, 404, response.content)
        metadata = self.client.patch(f"/api/sessions/{run.session_id}",
            json.dumps({"title": "Expose"}), content_type="application/json")
        self.assertEqual(metadata.status_code, 404, metadata.content)
        self.assertEqual(self.client.delete(f"/api/sessions/{run.session_id}").status_code, 404)
        with patch("app_core.http.jobs.get_runtime_job", return_value={"sessionId": run.session_id}):
            self.assertEqual(self.client.get("/api/jobs/job_private").status_code, 404)
