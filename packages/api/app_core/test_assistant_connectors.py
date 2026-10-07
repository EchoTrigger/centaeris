"""Scoped business capabilities and synthetic secure-store references."""
import hashlib
import json
import tempfile
from pathlib import Path
from unittest.mock import patch

from django.test import Client, TestCase, override_settings
from django.utils import timezone

from app_core.credentials import encrypt_credential_secret
from app_core.models import AgentRun, McpBearerCredential, WorkspacePluginEnablement
from app_core.plugin_catalog import activation_digest
from app_core.test_agent_definitions import DefinitionFixture, PROFILE


class AssistantConnectorFixture(DefinitionFixture):
    def setUp(self):
        super().setUp()
        temporary = tempfile.TemporaryDirectory(prefix="centaeris-assistant-connectors-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.packages = []
        for name in ("banana", "kiwi"):
            directory = self.root / name
            (directory / ".centaeris-plugin").mkdir(parents=True)
            (directory / ".centaeris-plugin/plugin.json").write_text(json.dumps({
                "name": name, "version": "1.0.0", "paths": {"mcpServers": ["mcp.json"]},
            }), encoding="utf-8")
            content = json.dumps({"schema": "mcp_servers_v1", "servers": [
                {"id": "ledger", "transport": {"type": "streamableHttp",
                 "url": "https://business.invalid/mcp", "bearerCredentialRef": "business-token"}, "tools": []},
                {"id": "public", "transport": {"type": "stdio", "program": "fixture", "args": []}, "tools": []},
            ]}).encode("utf-8")
            (directory / "mcp.json").write_bytes(content)
            digest = hashlib.sha256(b"centaeris.plugin.tree.v1\0")
            for data in (b"mcp.json", content):
                digest.update(len(data).to_bytes(8, "big"))
                digest.update(data)
            self.packages.append({"name": name, "version": "1.0.0", "packageDigest": "sha256:" + "a" * 64,
                "skills": [], "cli": [], "hooks": [],
                "mcpServers": [{"path": "mcp.json", "digest": "sha256:" + digest.hexdigest()}]})
        self.write_catalog()
        self.enterContext(override_settings(PLUGIN_CATALOG_ROOT=self.root))
        self.enablement = WorkspacePluginEnablement.objects.create(workspace=self.workspace, pluginName="banana")
        self.definition, self.initial_version = self.ready_definition()
        from django.contrib.auth.models import User
        self.custodian = User.objects.create_superuser(username="credential-custodian", email="fixture@example.invalid")
        self.custodian_client = Client()
        self.custodian_client.force_login(self.custodian)
        self.source = McpBearerCredential.objects.create(plugin_name="banana", credential_ref="source-one",
            display_name="Fixture account", encrypted_secret=encrypt_credential_secret("fixture-account-one"),
            created_by=self.custodian, updated_by=self.custodian)
        from app_core.models import ModelConfig
        self.model = ModelConfig.objects.create(displayName="Fixture")
        for target, value in (("request_execution_profile", PROFILE), ("schedule_agent_run_lifecycle", "inserted")):
            self.enterContext(patch("app_core.http.workspaces." + target, return_value=value))

    def write_catalog(self):
        (self.root / "catalog.snapshot.json").write_text(json.dumps({"schema": "plugin_activation_snapshot_v1",
            "digest": activation_digest(self.packages), "packages": self.packages}), encoding="utf-8")

    def select_plugins(self, names, definition=None):
        return self.send("patch", self.base + f"/agent-definitions/{(definition or self.definition)['id']}", {"pluginNames": names})

    def selected_publication(self, definition=None):
        definition = definition or self.definition
        self.assertEqual(self.select_plugins(["banana"], definition).status_code, 200)
        return self.publish(definition)

    def submit_run(self, definition=None, operation="connector-run"):
        definition = definition or self.definition
        self.client.force_login(self.member)
        agent = self.instance(definition).json()["agent"]
        response = self.send("post", self.base + "/sessions/new/messages", {
            "operationId": operation, "agentId": agent["id"], "text": "fixture", "modelConfigRef": self.model.id,
        })
        self.assertEqual(response.status_code, 202, response.content)
        return AgentRun.objects.get(id=response.json()["agentRunId"])

    def approve(self, definition=None, source=None, client=None, server="ledger"):
        return self.send("post", f"/api/admin/mcp-bearer-credentials/{(source or self.source).id}/assistant-approvals", {
            "workspaceId": self.workspace.id, "definitionId": (definition or self.definition)["id"], "serverId": server,
        }, client or self.custodian_client)

    def bind(self, approval, definition=None):
        return self.send("put", self.base + f"/agent-definitions/{(definition or self.definition)['id']}/connector-bindings/banana/ledger",
                         {"approvalId": approval["id"]})

    def ready_binding(self, definition=None, source=None):
        self.client.force_login(self.admin)
        self.selected_publication(definition)
        approved = self.approve(definition, source)
        self.assertEqual(approved.status_code, 201, approved.content)
        self.assertEqual(self.bind(approved.json()["approval"], definition).status_code, 200)
        return approved.json()["approval"]

    def authorize(self, run, **changes):
        body = {"schema": "runtime.mcp_connector.authorization.v1", "agentRunId": run.id,
            "authorizationRef": run.authorization.id, "authorizationDigest": run.authorization.digest,
            "pluginName": "banana", "serverId": "ledger", "resourcePath": "mcp.json",
            "resourceDigest": self.packages[0]["mcpServers"][0]["digest"], "operation": "connect", "bindingDigest": None}
        body.update(changes)
        return self.client.post("/internal/mcp-connectors/authorize", json.dumps(body), content_type="application/json",
                                HTTP_X_INTERNAL_TOKEN="test-internal-token")

class AssistantConnectorTests(AssistantConnectorFixture, TestCase):
    def test_selected_capability_excludes_another_workspace_enabled_plugin(self):
        WorkspacePluginEnablement.objects.create(workspace=self.workspace, pluginName="kiwi")
        self.selected_publication()
        run = self.submit_run()
        self.assertEqual([package["name"] for package in run.authorization.payload["pluginActivation"]["packages"]], ["banana"])
        denied = self.authorize(run, pluginName="kiwi", serverId="public",
            resourceDigest=self.packages[1]["mcpServers"][0]["digest"])
        self.assertEqual(denied.status_code, 409, denied.content)

    def test_changed_package_requires_a_new_publication_before_accepting_new_runs(self):
        self.selected_publication()
        self.packages[0]["packageDigest"] = "sha256:" + "b" * 64
        self.write_catalog()
        self.client.force_login(self.member)
        agent = self.instance(self.definition).json()["agent"]
        response = self.send("post", self.base + "/sessions/new/messages", {
            "operationId": "changed-package", "agentId": agent["id"], "text": "fixture", "modelConfigRef": self.model.id})
        self.assertEqual(response.status_code, 403, response.content)
        self.assertFalse(AgentRun.objects.exists())

    def test_selection_is_workspace_enabled_strict_and_publication_frozen(self):
        for names in (["unknown"], ["kiwi"], ["banana", "banana"], ["Banana"]):
            response = self.select_plugins(names)
            self.assertEqual(response.status_code, 400, response.content)
        version = self.selected_publication()
        self.assertEqual(version["pluginNames"], ["banana"])
        self.assertEqual(self.select_plugins([]).status_code, 200)
        run = self.submit_run()
        self.assertEqual(run.authorization.payload["pluginActivation"]["packages"], [self.packages[0]])
        self.assertEqual(run.definition_version_id, version["id"])
        self.client.force_login(self.admin)
        empty = self.publish(self.definition)
        run_two = self.submit_run(operation="empty-publication")
        self.assertEqual(run_two.authorization.payload["pluginActivation"]["packages"], [])
        self.assertEqual(run_two.definition_version_id, empty["id"])

    def test_workspace_plugin_disable_blocks_publication_and_new_run(self):
        self.selected_publication()
        self.enablement.delete()
        response = self.send("post", self.base + f"/agent-definitions/{self.definition['id']}/versions")
        self.assertEqual(response.status_code, 409, response.content)
        self.client.force_login(self.member)
        agent = self.instance(self.definition).json()["agent"]
        response = self.send("post", self.base + "/sessions/new/messages", {
            "operationId": "disabled-plugin", "agentId": agent["id"], "text": "fixture", "modelConfigRef": self.model.id})
        self.assertEqual(response.status_code, 403, response.content)
        self.assertFalse(AgentRun.objects.exists())

    def test_managed_connector_has_no_global_fallback_without_binding(self):
        self.selected_publication()
        run = self.submit_run()
        response = self.authorize(run)
        self.assertEqual(response.status_code, 409, response.content)
        self.assertNotIn("fixture-account-one", response.content.decode())
        old = self.client.post("/internal/mcp-bearer-credentials/resolve", json.dumps({
            "schema": "runtime.mcp_bearer_credential.resolve.v1", "agentRunId": run.id,
            "authorizationRef": run.authorization.id, "authorizationDigest": run.authorization.digest,
            "pluginName": "banana", "credentialRef": "source-one"}), content_type="application/json",
            HTTP_X_INTERNAL_TOKEN="test-internal-token")
        self.assertEqual(old.status_code, 409, old.content)

    def test_credential_custodian_approves_exact_assistant_scope_before_admin_binding(self):
        self.selected_publication()
        self.assertEqual(self.approve(client=self.client).status_code, 403)
        self.assertEqual(self.client.get("/api/admin/mcp-bearer-credentials").status_code, 403)
        self.assertEqual(self.send("put", self.base + f"/agent-definitions/{self.definition['id']}/connector-bindings/banana/ledger",
            {"secretRef": self.source.id}).status_code, 400)
        approved = self.approve().json()["approval"]
        listed = self.client.get(self.base + f"/agent-definitions/{self.definition['id']}/credential-approvals")
        self.assertEqual(listed.status_code, 200, listed.content)
        self.assertNotIn(self.source.id, listed.content.decode())
        self.assertNotIn("fixture-account-one", listed.content.decode())
        self.assertEqual(self.bind(approved).status_code, 200)
        other = self.create_definition()
        self.publish(other)
        self.selected_publication(other)
        self.assertEqual(self.bind(approved, other).status_code, 400)
        self.client.force_login(self.member)
        self.assertEqual(self.bind(approved).status_code, 404)

    def test_other_superuser_cannot_approve_another_custodians_credential(self):
        from django.contrib.auth.models import User
        other = User.objects.create_superuser(username="other-custodian", email="other@example.invalid")
        client = Client()
        client.force_login(other)
        self.selected_publication()
        self.assertEqual(self.approve(client=client).status_code, 404)

    def test_same_plugin_different_assistants_resolve_different_approved_accounts(self):
        self.ready_binding()
        other = self.create_definition()
        self.publish(other)
        self.availability(other)
        source = McpBearerCredential.objects.create(plugin_name="banana", credential_ref="source-two",
            display_name="Other account", encrypted_secret=encrypt_credential_secret("fixture-account-two"),
            created_by=self.custodian, updated_by=self.custodian)
        self.ready_binding(other, source)
        one = self.authorize(self.submit_run()).json()
        two = self.authorize(self.submit_run(other, "other-assistant")).json()
        self.assertEqual((one["token"], two["token"]), ("fixture-account-one", "fixture-account-two"))
        self.assertEqual(one["scope"], "managed")
        self.assertNotEqual(one["bindingDigest"], two["bindingDigest"])

    def test_current_binding_and_approval_revocation_invalidate_cached_connection(self):
        approval = self.ready_binding()
        run = self.submit_run()
        connected = self.authorize(run)
        self.assertEqual(connected.status_code, 200, connected.content)
        digest = connected.json()["bindingDigest"]
        dispatched = self.authorize(run, operation="dispatch", bindingDigest=digest)
        self.assertEqual(dispatched.status_code, 200, dispatched.content)
        self.assertIsNone(dispatched.json()["token"])
        revoked = self.custodian_client.delete(f"/api/admin/mcp-assistant-credential-approvals/{approval['id']}")
        self.assertEqual(revoked.status_code, 204, revoked.content)
        self.assertEqual(self.authorize(run, operation="dispatch", bindingDigest=digest).status_code, 409)

    def test_binding_removal_and_rotation_do_not_select_another_secret(self):
        self.ready_binding()
        run = self.submit_run()
        digest = self.authorize(run).json()["bindingDigest"]
        self.source.version += 1
        self.source.encrypted_secret = encrypt_credential_secret("fixture-rotated")
        self.source.save(update_fields=["version", "encrypted_secret"])
        self.assertEqual(self.authorize(run, operation="dispatch", bindingDigest=digest).status_code, 409)
        self.client.force_login(self.admin)
        deleted = self.client.delete(self.base + f"/agent-definitions/{self.definition['id']}/connector-bindings/banana/ledger")
        self.assertEqual(deleted.status_code, 204, deleted.content)
        self.assertEqual(self.authorize(run).status_code, 409)

    def test_current_membership_definition_and_workspace_capability_are_dispatch_authority(self):
        self.ready_binding()
        run = self.submit_run()
        digest = self.authorize(run).json()["bindingDigest"]
        self.client.force_login(self.admin)
        self.availability(self.definition, "none")
        self.assertEqual(self.authorize(run, operation="dispatch", bindingDigest=digest).status_code, 403)
        self.availability(self.definition)
        self.enablement.delete()
        self.assertEqual(self.authorize(run, operation="dispatch", bindingDigest=digest).status_code, 403)
        WorkspacePluginEnablement.objects.create(workspace=self.workspace, pluginName="banana")
        self.membership.delete()
        self.assertEqual(self.authorize(run, operation="dispatch", bindingDigest=digest).status_code, 403)

    def test_uncredentialed_connector_still_requires_current_capability_authority(self):
        self.selected_publication()
        run = self.submit_run()
        response = self.authorize(run, serverId="public")
        self.assertEqual(response.status_code, 200, response.content)
        self.assertIsNone(response.json()["token"])
        self.enablement.delete()
        self.assertEqual(self.authorize(run, serverId="public", operation="dispatch",
            bindingDigest=response.json()["bindingDigest"]).status_code, 403)

    def test_frozen_resource_mismatch_unknown_server_and_secret_override_fail_closed(self):
        self.ready_binding()
        run = self.submit_run()
        for change in ({"serverId": "other"}, {"resourcePath": "different.json"},
                       {"resourceDigest": "sha256:" + "b" * 64}, {"secretRef": self.source.id}):
            response = self.authorize(run, **change)
            self.assertIn(response.status_code, (400, 409), response.content)
            self.assertNotIn("fixture-account-one", response.content.decode())

    def test_missing_managed_run_version_cannot_downgrade_to_private_global_credentials(self):
        self.ready_binding()
        run = self.submit_run()
        AgentRun.objects.filter(id=run.id).update(definition_version=None)
        run.refresh_from_db()
        self.assertEqual(self.authorize(run).status_code, 409)

    def test_approved_secure_store_reference_prevents_uncontrolled_source_deletion(self):
        self.ready_binding()
        deleted = self.custodian_client.delete(f"/api/admin/mcp-bearer-credentials/{self.source.id}")
        self.assertEqual(deleted.status_code, 409, deleted.content)
        self.assertEqual(deleted.json(), {"error": "mcp_bearer_credential_in_use"})
        self.assertTrue(McpBearerCredential.objects.filter(id=self.source.id).exists())

    def test_private_connector_uses_explicit_private_state_and_declared_legacy_reference(self):
        from app_core.agent_run_authorization_factory import create_agent_run_authorization
        from app_core.models import Agent, Session
        source = McpBearerCredential.objects.create(plugin_name="banana", credential_ref="business-token",
            display_name="Private account", encrypted_secret=encrypt_credential_secret("fixture-private"),
            created_by=self.custodian, updated_by=self.custodian)
        agent = Agent.objects.create(workspace=self.workspace, owner=self.member, name="Private")
        session = Session.objects.create(workspace=self.workspace, owner=self.member, agent=agent)
        run = AgentRun.objects.create(workspace=self.workspace, session=session, user=self.member,
            membership_ref=self.membership.id, modelConfig=self.model, prompt="private")
        create_agent_run_authorization(run, image_digest=PROFILE["imageDigest"])
        response = self.authorize(run)
        self.assertEqual(response.status_code, 200, response.content)
        self.assertEqual((response.json()["scope"], response.json()["token"]), ("private", "fixture-private"))
        source.delete()
        self.assertEqual(self.authorize(run, operation="dispatch", bindingDigest=response.json()["bindingDigest"]).status_code, 404)

    def test_replacing_binding_rejects_cached_digest_and_requires_new_explicit_approval(self):
        self.ready_binding()
        run = self.submit_run()
        original = self.authorize(run).json()
        source = McpBearerCredential.objects.create(plugin_name="banana", credential_ref="source-two",
            display_name="Other account", encrypted_secret=encrypt_credential_secret("fixture-account-two"),
            created_by=self.custodian, updated_by=self.custodian)
        self.client.force_login(self.admin)
        approval = self.approve(source=source).json()["approval"]
        self.assertEqual(self.bind(approval).status_code, 200)
        self.assertEqual(self.authorize(run, operation="dispatch", bindingDigest=original["bindingDigest"]).status_code, 409)
        self.assertEqual(self.authorize(run).json()["token"], "fixture-account-two")

    def test_new_publication_does_not_reinterpret_an_accepted_runs_configuration(self):
        self.ready_binding()
        run = self.submit_run()
        original = self.authorize(run).json()
        self.client.force_login(self.admin)
        self.assertEqual(self.select_plugins([]).status_code, 200)
        self.publish(self.definition)
        self.assertEqual(self.authorize(run, operation="dispatch", bindingDigest=original["bindingDigest"]).status_code, 200)

    def test_current_session_agent_and_run_state_are_checked_before_dispatch(self):
        self.ready_binding()
        run = self.submit_run()
        original = self.authorize(run).json()
        for object_, field, value in ((run.session, "status", "deleted"),
                                      (run.session.agent, "status", "deleted"), (run, "status", "completed")):
            with self.subTest(field=field, object=type(object_).__name__):
                old = getattr(object_, field)
                deleted_fields = {"deletedAt": timezone.now()} if value == "deleted" else {}
                type(object_).objects.filter(pk=object_.pk).update(**{field: value}, **deleted_fields)
                self.assertEqual(self.authorize(run, operation="dispatch", bindingDigest=original["bindingDigest"]).status_code, 409)
                restored_fields = {"deletedAt": None} if value == "deleted" else {}
                type(object_).objects.filter(pk=object_.pk).update(**{field: old}, **restored_fields)

    def test_connector_request_requires_internal_auth_and_every_identity_field(self):
        self.ready_binding()
        run = self.submit_run()
        body = {"schema": "runtime.mcp_connector.authorization.v1", "agentRunId": run.id,
            "authorizationRef": run.authorization.id, "authorizationDigest": run.authorization.digest,
            "pluginName": "banana", "serverId": "ledger", "resourcePath": "mcp.json",
            "resourceDigest": self.packages[0]["mcpServers"][0]["digest"], "operation": "connect", "bindingDigest": None}
        self.assertEqual(self.client.post("/internal/mcp-connectors/authorize", json.dumps(body),
            content_type="application/json").status_code, 401)
        for field in body:
            missing = {key: value for key, value in body.items() if key != field}
            self.assertEqual(self.client.post("/internal/mcp-connectors/authorize", json.dumps(missing),
                content_type="application/json", HTTP_X_INTERNAL_TOKEN="test-internal-token").status_code, 400)

    def test_binding_and_custodian_approval_browser_mutations_keep_csrf(self):
        self.selected_publication()
        approved = self.approve().json()["approval"]
        browser = Client(enforce_csrf_checks=True)
        browser.force_login(self.admin)
        bound = self.send("put", self.base + f"/agent-definitions/{self.definition['id']}/connector-bindings/banana/ledger",
                          {"approvalId": approved["id"]}, browser)
        self.assertEqual(bound.status_code, 403)
        browser.force_login(self.custodian)
        self.assertEqual(self.approve(client=browser).status_code, 403)
        self.assertEqual(browser.delete(f"/api/admin/mcp-assistant-credential-approvals/{approved['id']}").status_code, 403)
