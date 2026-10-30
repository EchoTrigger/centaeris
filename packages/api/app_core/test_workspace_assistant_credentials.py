"""Local credentials require local authority; existing global custodian grants remain valid."""

from django.test import TestCase
from .test_assistant_connectors import AssistantConnectorFixture
from .models import McpBearerCredential, WorkspaceMembership
from .credentials import encrypt_credential_secret


class WorkspaceAssistantCredentialTests(AssistantConnectorFixture, TestCase):
    def test_workspace_admin_approves_and_uses_local_key_without_platform_role(self):
        self.admin_membership.role = "admin"
        self.admin_membership.save(update_fields=["role"])
        self.selected_publication()
        local = McpBearerCredential.objects.create(
            workspace=self.workspace,
            plugin_name="banana",
            credential_ref="business-token",
            display_name="Local",
            encrypted_secret=encrypt_credential_secret("local-assistant-secret"),
            created_by=self.admin,
            updated_by=self.admin,
        )
        approved = self.send(
            "post",
            self.base + f"/mcp-bearer-credentials/{local.id}/assistant-approvals",
            {
                "workspaceId": self.workspace.id,
                "definitionId": self.definition["id"],
                "serverId": "ledger",
            },
        )
        self.assertEqual(approved.status_code, 201, approved.content)
        self.assertNotIn(b"local-assistant-secret", approved.content)
        self.assertFalse(self.admin.is_superuser)
        self.assertEqual(self.bind(approved.json()["approval"]).status_code, 200)
        run = self.submit_run(operation="local-assistant")
        response = self.authorize(run)
        self.assertEqual(response.status_code, 200, response.content)
        self.assertEqual(response.json()["token"], "local-assistant-secret")

    def test_local_approval_rejects_member_shared_source_and_cross_workspace(self):
        self.selected_publication()
        local = McpBearerCredential.objects.create(
            workspace=self.workspace,
            plugin_name="banana",
            credential_ref="business-token",
            display_name="Local",
            encrypted_secret=encrypt_credential_secret("local"),
            created_by=self.admin,
            updated_by=self.admin,
        )
        path = self.base + f"/mcp-bearer-credentials/{local.id}/assistant-approvals"
        payload = {
            "workspaceId": "another-workspace",
            "definitionId": self.definition["id"],
            "serverId": "ledger",
        }
        self.assertIn(self.send("post", path, payload).status_code, (400, 404))
        payload["workspaceId"] = self.workspace.id
        shared_path = (
            self.base + f"/mcp-bearer-credentials/{self.source.id}/assistant-approvals"
        )
        self.assertEqual(self.send("post", shared_path, payload).status_code, 404)
        self.client.force_login(self.member)
        self.assertEqual(self.send("post", path, payload).status_code, 404)
