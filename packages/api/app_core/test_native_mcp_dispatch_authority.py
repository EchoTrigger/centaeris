import hashlib
import json
from pathlib import Path
from tempfile import TemporaryDirectory

from django.conf import settings
from django.contrib.auth import get_user_model
from django.db import connection
from django.test import TransactionTestCase, override_settings

from .agent_run_authorization_factory import create_agent_run_authorization
from .credentials import encrypt_credential_secret
from .models import (
    AgentRun, McpBearerCredential, McpCredentialAuditEvent, ModelConfig, Workspace,
    WorkspaceMembership, WorkspacePluginEnablement, WorkspacePluginInstallation,
)
from .test_native_projected_input_authority import (
    native_authority_sql, native_run_authority_fixture, native_run_authority_params,
)
from .testing import create_session


AUTHORITY_SQL = native_authority_sql("native_mcp_dispatch_authority.sql")
PLUGIN = "fixture-connector"
SERVER = "fixture-server"
RESOURCE = "mcp/remote.json"
CREDENTIAL = "fixture-token"


class NativeMcpDispatchAuthorityTests(TransactionTestCase):
    def setUp(self):
        catalog = Path(self.enterContext(TemporaryDirectory(prefix="centaeris-mcp-authority-")))
        self.enterContext(override_settings(PLUGIN_CATALOG_ROOT=str(catalog)))
        self.user = get_user_model().objects.create_user(username="native-mcp-owner")
        self.workspace = Workspace.objects.create(name="MCP fixture", createdBy=self.user)
        self.membership = WorkspaceMembership.objects.create(workspace=self.workspace, user=self.user, role="owner")
        content = json.dumps({
            "schema": "mcp_servers_v1",
            "servers": [{"id": SERVER, "transport": {
                "type": "streamableHttp", "url": "https://mcp.example.invalid/tools",
                "bearerCredentialRef": CREDENTIAL,
            }}],
        }).encode()
        resource = RESOURCE.encode()
        digest = hashlib.sha256(b"centaeris.plugin.tree.v1\0")
        for part in (resource, content):
            digest.update(len(part).to_bytes(8, "big"))
            digest.update(part)
        self.resource_digest = f"sha256:{digest.hexdigest()}"
        package_digest = "a" * 64
        path = catalog / ".packages" / package_digest / PLUGIN / RESOURCE
        path.parent.mkdir(parents=True)
        path.write_bytes(content)
        self.package = {
            "name": PLUGIN, "version": "1.0.0", "packageDigest": "sha256:" + package_digest,
            "skills": [], "cli": [], "hooks": [],
            "mcpServers": [{"path": RESOURCE, "digest": self.resource_digest}],
        }
        self.installation = WorkspacePluginInstallation.objects.create(
            workspace=self.workspace, plugin_name=PLUGIN, package=self.package, updated_by=self.user,
        )
        self.enablement = WorkspacePluginEnablement.objects.create(workspace=self.workspace, pluginName=PLUGIN)
        self.credential = self.create_credential(self.workspace)
        session = create_session(workspace=self.workspace, owner=self.user)
        self.run = AgentRun.objects.create(
            workspace=self.workspace, session=session, user=self.user, modelConfig=ModelConfig.objects.create(),
            prompt="Use the connector",
        )
        self.grant = create_agent_run_authorization(self.run, image_digest="sha256:" + "b" * 64)
        self.lease_owner, self.controller = native_run_authority_fixture(self, self.run)

    def create_credential(self, workspace):
        return McpBearerCredential.objects.create(
            workspace=workspace, plugin_name=PLUGIN, credential_ref=CREDENTIAL, display_name="Fixture credential",
            encrypted_secret=encrypt_credential_secret("fixture-bearer"),
            created_by=self.user, updated_by=self.user,
        )

    def native(self, **changes):
        values = native_run_authority_params(self.run, self.lease_owner, self.controller) | {
            "plugin": PLUGIN, "resource": RESOURCE, "resource_digest": self.resource_digest,
            "credential_ref": CREDENTIAL,
        } | changes
        with connection.cursor() as cursor:
            cursor.execute(AUTHORITY_SQL, list(values.values()))
            return cursor.fetchall()

    def authorize(self, operation, binding_digest=None):
        return self.client.post(
            "/internal/mcp-connectors/authorize",
            {"schema": "runtime.mcp_connector.authorization.v1", "agentRunId": self.run.id,
             "authorizationRef": self.grant.id, "authorizationDigest": self.grant.digest, "pluginName": PLUGIN,
             "serverId": SERVER, "resourcePath": RESOURCE, "resourceDigest": self.resource_digest,
             "operation": operation, "bindingDigest": binding_digest},
            content_type="application/json", HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN,
        )

    def assert_bound(self, response, token, action):
        self.assertEqual(response.status_code, 200, response.content)
        [(credential_id, version)] = self.native()
        self.assertEqual((credential_id, version), (self.credential.id, self.credential.version))
        identity = {
            "scope": "workspace", "workspace": self.workspace.id, "plugin": PLUGIN, "server": SERVER,
            "resourcePath": RESOURCE, "resourceDigest": self.resource_digest,
            "credential": credential_id, "credentialVersion": version,
        }
        expected = hashlib.sha256(json.dumps(identity, sort_keys=True, separators=(",", ":")).encode()).hexdigest()
        self.assertEqual(response.json(), {
            "schema": "runtime.mcp_connector.authorized.v1", "scope": "workspace",
            "bindingDigest": "sha256:" + expected, "token": token,
        })
        self.assertEqual(McpCredentialAuditEvent.objects.filter(
            credential_id=credential_id, action="resolved",
        ).count(), action)
        return "sha256:" + expected

    def test_audited_connect_and_dispatch_match_sql_witness_across_rotation(self):
        digest = self.assert_bound(self.authorize("connect"), "fixture-bearer", 1)
        self.assert_bound(self.authorize("dispatch", digest), None, 1)

        self.client.force_login(self.user)
        rotation = self.client.post(
            f"/api/workspaces/{self.workspace.id}/mcp-bearer-credentials/{self.credential.id}/rotate",
            {"secret": "rotated-bearer"}, content_type="application/json",
        )
        self.assertEqual(rotation.status_code, 200, rotation.content)
        self.credential.refresh_from_db()
        self.assertEqual(self.credential.version, 2)
        stale = self.authorize("dispatch", digest)
        self.assertEqual(stale.status_code, 409, stale.content)
        self.assertEqual(stale.json(), {"error": "mcp_connector_binding_changed"})
        self.assertNotEqual(self.assert_bound(self.authorize("connect"), "rotated-bearer", 2), digest)

    def test_disable_and_membership_revoke_fresh_dispatch(self):
        digest = self.assert_bound(self.authorize("connect"), "fixture-bearer", 1)
        self.enablement.delete()
        self.assertEqual(self.authorize("dispatch", digest).status_code, 403)
        self.assertEqual(self.native(), [])
        WorkspacePluginEnablement.objects.create(workspace=self.workspace, pluginName=PLUGIN)
        self.assert_bound(self.authorize("dispatch", digest), None, 1)
        self.membership.delete()
        self.assertEqual(self.authorize("dispatch", digest).status_code, 403)
        self.assertEqual(self.native(), [])

    def test_fenced_dispatch_requires_current_environment_and_live_execute_lease(self):
        digest = self.assert_bound(self.authorize("connect"), "fixture-bearer", 1)
        self.assertEqual(self.native(controller_epoch=self.controller["controllerEpoch"] + 1), [])
        self.assertEqual(self.native(environment_id="other-environment"), [])
        with connection.cursor() as cursor:
            cursor.execute(
                "UPDATE runtime.runtime_jobs SET lease_expires_at_ms = 0 WHERE job_id = %s",
                ["agent_run.lifecycle:" + self.run.id],
            )
        self.assertEqual(self.native(), [])
        self.assertEqual(self.authorize("dispatch", digest).status_code, 200)

    def test_credential_and_resource_must_match_the_workspace_and_signed_package(self):
        digest = self.assert_bound(self.authorize("connect"), "fixture-bearer", 1)
        self.assertEqual(self.native(resource="mcp/other.json"), [])
        self.assertEqual(self.native(resource_digest="sha256:" + "0" * 64), [])
        self.assertEqual(self.native(credential_ref="different-token"), [])

        self.installation.delete()
        self.assert_bound(self.authorize("dispatch", digest), None, 1)

        self.create_credential(None)
        self.credential.delete()
        self.assertEqual(self.authorize("dispatch", digest).status_code, 404)
        self.assertEqual(self.native(), [])
