import json
import threading
from concurrent.futures import ThreadPoolExecutor
from unittest.mock import patch

from django.contrib.auth.models import User
from django.db import close_old_connections, transaction
from django.test import Client, TestCase, TransactionTestCase

from app_core.models import Agent, AgentRun, ModelConfig, Session, Workspace, WorkspaceMembership


PROFILE = {"schema": "runtime.execution_profile.v1", "imageCapability": "workspace_general_v1",
           "imageDigest": "sha256:" + "a" * 64}


class DefinitionFixture:
    def setUp(self):
        super().setUp()
        self.admin = User.objects.create_user(username="definition-admin")
        self.member = User.objects.create_user(username="definition-member")
        self.other = User.objects.create_user(username="definition-other")
        self.workspace = Workspace.objects.create(name="Business", createdBy=self.admin)
        self.admin_membership = WorkspaceMembership.objects.create(workspace=self.workspace, user=self.admin, role="owner")
        self.membership = WorkspaceMembership.objects.create(workspace=self.workspace, user=self.member)
        self.other_membership = WorkspaceMembership.objects.create(workspace=self.workspace, user=self.other)
        self.base = f"/api/workspaces/{self.workspace.id}"
        self.client.force_login(self.admin)

    def send(self, method, path, payload=None, client=None):
        return getattr(client or self.client, method)(path, json.dumps(payload or {}), content_type="application/json")

    def create_definition(self):
        response = self.send("post", self.base + "/agent-definitions", {
            "name": "Business assistant", "description": "Shared configuration",
            "instructions": "Use primary evidence.", "avatarKind": "centaeris",
        })
        self.assertEqual(response.status_code, 201, response.content)
        return response.json()["definition"]

    def publish(self, definition):
        response = self.send("post", self.base + f"/agent-definitions/{definition['id']}/versions")
        self.assertEqual(response.status_code, 201, response.content)
        return response.json()["version"]

    def availability(self, definition, scope="workspace", membership_ids=None):
        return self.send("put", self.base + f"/agent-definitions/{definition['id']}/availability",
                         {"scope": scope, "membershipIds": membership_ids or []})

    def instance(self, definition, client=None):
        return self.send("post", self.base + f"/available-agent-definitions/{definition['id']}/instance", client=client)

    def ready_definition(self):
        definition = self.create_definition()
        version = self.publish(definition)
        self.assertEqual(self.availability(definition).status_code, 200)
        return definition, version


class AgentDefinitionTests(DefinitionFixture, TestCase):
    def test_publishing_does_not_open_default_scope_or_create_instances(self):
        definition = self.create_definition()
        self.assertEqual(definition["availabilityScope"], "none")
        self.assertIsNone(definition["publishedVersionId"])
        self.publish(definition)
        self.client.force_login(self.member)
        self.assertEqual(self.client.get(self.base + "/available-agent-definitions").json(), {"definitions": []})
        self.assertEqual(self.instance(definition).status_code, 404)
        self.assertFalse(Agent.objects.exists())

    def test_members_cannot_manage_definitions_even_when_available(self):
        definition, _ = self.ready_definition()
        self.client.force_login(self.member)
        path = self.base + f"/agent-definitions/{definition['id']}"
        for response in (self.client.get(self.base + "/agent-definitions"), self.client.get(path),
                         self.send("post", self.base + "/agent-definitions", {"name": "Denied"}),
                         self.send("patch", path, {"instructions": "Override"}),
                         self.send("post", path + "/versions"), self.availability(definition, "none")):
            self.assertEqual(response.status_code, 404, response.content)

    def test_scope_member_identity_is_current_and_workspace_bound(self):
        definition = self.create_definition()
        self.publish(definition)
        self.assertEqual(self.availability(definition, "members", [self.membership.id]).status_code, 200)
        self.client.force_login(self.other)
        self.assertEqual(self.instance(definition).status_code, 404)
        self.client.force_login(self.member)
        first = self.instance(definition)
        self.assertEqual(first.status_code, 201, first.content)
        self.membership.delete()
        WorkspaceMembership.objects.create(workspace=self.workspace, user=self.member)
        self.assertEqual(self.instance(definition).status_code, 404)
        self.assertTrue(Agent.objects.filter(id=first.json()["agent"]["id"]).exists())
        self.client.force_login(self.admin)
        foreign_workspace = Workspace.objects.create(name="Foreign", createdBy=self.other)
        foreign = WorkspaceMembership.objects.create(workspace=foreign_workspace, user=self.other)
        for ids in ([foreign.id], [self.other_membership.id, "wsm_missing"], [self.other_membership.id] * 2):
            self.assertEqual(self.availability(definition, "members", ids).status_code, 400)
        self.assertEqual(self.client.get(self.base + f"/agent-definitions/{definition['id']}").json()["definition"]["membershipIds"], [])

    def test_instances_are_private_unique_and_managed_configuration_cannot_be_overridden(self):
        definition, version = self.ready_definition()
        self.client.force_login(self.member)
        first = self.instance(definition)
        self.assertEqual(first.status_code, 201, first.content)
        second = self.instance(definition)
        self.assertEqual(second.status_code, 200)
        self.assertEqual(first.json(), second.json())
        agent = first.json()["agent"]
        self.assertEqual(agent["definitionId"], definition["id"])
        self.assertEqual(agent["definitionVersionId"], version["id"])
        for field, value in (("name", "Override"), ("instructions", "Override"), ("description", "Override"), ("avatarKind", "banana")):
            response = self.send("patch", f"/api/agents/{agent['id']}", {field: value})
            self.assertEqual(response.status_code, 409, response.content)
            self.assertEqual(response.json(), {"error": "agent_configuration_managed"})
        session = Session.objects.create(workspace=self.workspace, owner=self.member, agent_id=agent["id"])
        self.client.force_login(self.admin)
        self.assertEqual(self.client.get(f"/api/agents/{agent['id']}").status_code, 404)
        self.assertEqual(self.client.get(f"/api/sessions/{session.id}").status_code, 404)
        self.assertEqual(self.client.get(self.base + "/agents").json(), {"agents": []})
        other = self.instance(definition)
        self.assertEqual(other.status_code, 201, other.content)
        self.assertNotEqual(other.json()["agent"]["id"], agent["id"])

    def test_draft_changes_require_publication_and_old_versions_are_immutable(self):
        definition, old = self.ready_definition()
        updated = self.send("patch", self.base + f"/agent-definitions/{definition['id']}", {"instructions": "New instructions."})
        self.assertEqual(updated.status_code, 200, updated.content)
        self.client.force_login(self.member)
        self.assertEqual(self.client.get(self.base + "/available-agent-definitions").json()["definitions"][0]["instructions"], old["instructions"])
        self.client.force_login(self.admin)
        new = self.publish(definition)
        self.assertEqual(new["version"], 2)
        from app_core.models import AgentDefinitionVersion
        stored = AgentDefinitionVersion.objects.get(id=old["id"])
        stored.instructions = "Mutation"
        with self.assertRaisesRegex(ValueError, "immutable"):
            stored.save()
        self.assertEqual(AgentDefinitionVersion.objects.get(id=old["id"]).instructions, old["instructions"])

    def test_cross_workspace_and_unknown_definition_identities_fail(self):
        definition, _ = self.ready_definition()
        foreign = Workspace.objects.create(name="Foreign", createdBy=self.admin)
        WorkspaceMembership.objects.create(workspace=foreign, user=self.admin, role="owner")
        for path in (f"/api/workspaces/{foreign.id}/agent-definitions/{definition['id']}",
                     self.base + "/agent-definitions/definition_missing"):
            self.assertEqual(self.client.get(path).status_code, 404)
            self.assertEqual(self.send("post", path + "/versions").status_code, 404)
        self.assertEqual(self.send("post", f"/api/workspaces/{foreign.id}/available-agent-definitions/{definition['id']}/instance").status_code, 404)

    def test_strict_camel_case_schema_rejects_unknown_configuration_and_scope_shapes(self):
        definition = self.create_definition()
        for payload in ({"name": "Test", "avatar_kind": "banana"}, {"name": "Test", "skillIds": []},
                        {"name": "Test", "credentialRef": "global"}, {"name": "Test", "nameExtra": "unknown"}):
            self.assertEqual(self.send("post", self.base + "/agent-definitions", payload).status_code, 400)
        for payload in ({"scope": "all"}, {"scope": "workspace", "membershipIds": [self.membership.id]},
                        {"scope": "members", "membership_ids": []}):
            self.assertEqual(self.send("put", self.base + f"/agent-definitions/{definition['id']}/availability", payload).status_code, 400)
        self.assertEqual(self.send("post", self.base + f"/agent-definitions/{definition['id']}/versions", {"version": 9}).status_code, 400)
        self.assertEqual(self.send("patch", self.base + f"/agent-definitions/{definition['id']}", {"publishedVersionId": "fake"}).status_code, 400)

    def test_workspace_admin_can_manage_but_management_does_not_imply_usage(self):
        self.membership.role = "admin"
        self.membership.save(update_fields=["role"])
        self.client.force_login(self.member)
        definition = self.create_definition()
        self.publish(definition)
        self.assertEqual(self.client.get(self.base + "/agent-definitions").status_code, 200)
        self.assertEqual(self.instance(definition).status_code, 404)

    def test_browser_mutations_keep_csrf_protection(self):
        definition, _ = self.ready_definition()
        browser = Client(enforce_csrf_checks=True)
        browser.force_login(self.admin)
        for method, path, payload in (
            ("post", self.base + "/agent-definitions", {"name": "Blocked"}),
            ("patch", self.base + f"/agent-definitions/{definition['id']}", {"status": "disabled"}),
            ("post", self.base + f"/agent-definitions/{definition['id']}/versions", {}),
            ("put", self.base + f"/agent-definitions/{definition['id']}/availability", {"scope": "none"}),
            ("post", self.base + f"/available-agent-definitions/{definition['id']}/instance", {}),
        ):
            response = self.send(method, path, payload, browser)
            self.assertEqual(response.status_code, 403, response.content)
            self.assertEqual(response.json(), {"error": "csrf_failed"})

    def test_publication_rolls_back_version_when_pointer_update_fails(self):
        from app_core.models import AgentDefinition, AgentDefinitionVersion
        definition, version = self.ready_definition()
        with patch.object(AgentDefinition, "save", side_effect=RuntimeError("publication failure")):
            response = self.send("post", self.base + f"/agent-definitions/{definition['id']}/versions")
        self.assertEqual(response.status_code, 500, response.content)
        self.assertEqual(AgentDefinition.objects.get(id=definition["id"]).published_version_id, version["id"])
        self.assertEqual(AgentDefinitionVersion.objects.filter(definition_id=definition["id"]).count(), 1)
        self.assertEqual(self.publish(definition)["version"], 2)


class AgentDefinitionRunTests(DefinitionFixture, TestCase):
    def setUp(self):
        super().setUp()
        self.definition, self.version = self.ready_definition()
        self.client.force_login(self.member)
        self.agent = self.instance(self.definition).json()["agent"]
        self.model = ModelConfig.objects.create(displayName="Fixture")
        for target, value in (("request_execution_profile", PROFILE), ("schedule_agent_run_lifecycle", "inserted")):
            patcher = patch("app_core.http.workspaces." + target, return_value=value)
            mock = patcher.start()
            setattr(self, target, mock)
            self.addCleanup(patcher.stop)

    def submit(self, operation, session_id="new"):
        return self.send("post", self.base + f"/sessions/{session_id}/messages", {
            "operationId": operation, "text": "hello", "agentId": self.agent["id"], "modelConfigRef": self.model.id,
        })

    def test_new_run_uses_latest_published_snapshot_in_existing_private_session(self):
        first = self.submit("original")
        self.assertEqual(first.status_code, 202, first.content)
        old_run = AgentRun.objects.get(id=first.json()["agentRunId"])
        self.assertEqual(old_run.definition_version_id, self.version["id"])
        AgentRun.objects.filter(id=old_run.id).update(status="completed")
        self.client.force_login(self.admin)
        self.send("patch", self.base + f"/agent-definitions/{self.definition['id']}", {"instructions": "New managed instructions."})
        latest = self.publish(self.definition)
        self.client.force_login(self.member)
        second = self.submit("continued", first.json()["sessionId"])
        self.assertEqual(second.status_code, 202, second.content)
        new_run = AgentRun.objects.get(id=second.json()["agentRunId"])
        self.assertEqual(new_run.definition_version_id, latest["id"])
        self.assertEqual(new_run.agent_instructions, latest["instructions"])
        self.assertEqual(new_run.session_id, old_run.session_id)
        old_run.refresh_from_db()
        self.assertEqual(old_run.definition_version_id, self.version["id"])
        self.assertEqual(old_run.agent_instructions, self.version["instructions"])
        from app_core.runtime_client import build_agent_run_start
        self.assertEqual(build_agent_run_start(old_run)["agentInstructions"], self.version["instructions"])
        self.assertEqual(build_agent_run_start(new_run)["agentInstructions"], latest["instructions"])

    def test_disabled_or_revoked_definition_blocks_new_runs_but_retains_private_data_and_receipts(self):
        first = self.submit("accepted")
        self.assertEqual(first.status_code, 202, first.content)
        AgentRun.objects.filter(id=first.json()["agentRunId"]).update(status="completed")
        self.client.force_login(self.admin)
        self.assertEqual(self.send("patch", self.base + f"/agent-definitions/{self.definition['id']}", {"status": "disabled"}).status_code, 200)
        self.client.force_login(self.member)
        self.assertEqual(self.submit("blocked", first.json()["sessionId"]).status_code, 403)
        self.assertEqual(self.submit("accepted").json(), first.json())
        self.assertEqual(self.client.get(f"/api/sessions/{first.json()['sessionId']}").status_code, 200)
        self.assertEqual(AgentRun.objects.count(), 1)
        self.client.force_login(self.admin)
        self.send("patch", self.base + f"/agent-definitions/{self.definition['id']}", {"status": "active"})
        self.availability(self.definition, "none")
        self.client.force_login(self.member)
        self.assertEqual(self.submit("revoked", first.json()["sessionId"]).status_code, 403)
        self.assertEqual(Session.objects.count(), 1)
        self.assertEqual(Agent.objects.get(id=self.agent["id"]).owner_id, self.member.id)

    def test_managed_runs_do_not_inherit_workspace_external_plugins(self):
        from app_core.models import WorkspacePluginEnablement
        WorkspacePluginEnablement.objects.create(workspace=self.workspace, pluginName="banana")
        first = self.submit("zero-extensions")
        self.assertEqual(first.status_code, 202, first.content)
        run = AgentRun.objects.get(id=first.json()["agentRunId"])
        self.assertEqual(run.authorization.payload["pluginActivation"]["packages"], [])

    def test_unavailable_definition_is_rejected_before_runtime_contact(self):
        self.client.force_login(self.admin)
        self.availability(self.definition, "none")
        self.client.force_login(self.member)
        self.request_execution_profile.side_effect = RuntimeError("offline")
        rejected = self.submit("unavailable-offline")
        self.assertEqual(rejected.status_code, 403, rejected.content)
        self.request_execution_profile.assert_not_called()
        self.assertFalse(Session.objects.exists())
        self.assertFalse(AgentRun.objects.exists())

    def test_failed_authorization_rolls_back_managed_run_session_and_receipt(self):
        from app_core.models import HostedOperationReceipt
        with patch("app_core.http.workspaces.create_agent_run_authorization", side_effect=RuntimeError("fixture failure")):
            response = self.submit("rollback")
        self.assertEqual(response.status_code, 500, response.content)
        self.assertFalse(Session.objects.exists())
        self.assertFalse(AgentRun.objects.exists())
        self.assertFalse(HostedOperationReceipt.objects.exists())
        self.assertEqual(Agent.objects.count(), 1)
        retry = self.submit("rollback")
        self.assertEqual(retry.status_code, 202, retry.content)

    def test_empty_activation_rejects_existing_global_bearer_credential(self):
        from app_core.credentials import encrypt_credential_secret
        from app_core.models import McpBearerCredential, McpCredentialAuditEvent
        McpBearerCredential.objects.create(plugin_name="banana", credential_ref="synthetic-source", display_name="Fixture",
            encrypted_secret=encrypt_credential_secret("synthetic-secret"), created_by=self.admin, updated_by=self.admin)
        response = self.submit("no-global-grant")
        self.assertEqual(response.status_code, 202, response.content)
        run = AgentRun.objects.get(id=response.json()["agentRunId"])
        resolved = self.client.post("/internal/mcp-bearer-credentials/resolve", json.dumps({
            "schema": "runtime.mcp_bearer_credential.resolve.v1", "agentRunId": run.id,
            "authorizationRef": run.authorization.id, "authorizationDigest": run.authorization.digest,
            "pluginName": "banana", "credentialRef": "synthetic-source",
        }), content_type="application/json", HTTP_X_INTERNAL_TOKEN="test-internal-token")
        self.assertEqual(resolved.status_code, 409, resolved.content)
        self.assertEqual(resolved.json(), {"error": "mcp_credential_authorization_invalid"})
        self.assertNotIn("synthetic-secret", resolved.content.decode())
        self.assertFalse(McpCredentialAuditEvent.objects.exists())

    def test_definition_admin_cannot_read_or_submit_another_users_run_or_transcript(self):
        response = self.submit("private-run")
        self.assertEqual(response.status_code, 202, response.content)
        session_id, run_id = response.json()["sessionId"], response.json()["agentRunId"]
        self.client.force_login(self.admin)
        for suffix in ("", "/transcript", "/transcript/patches", "/transcript/content",
                       f"/agent-runs/{run_id}/events", "/assets"):
            denied = self.client.get(f"/api/sessions/{session_id}" + suffix)
            self.assertEqual(denied.status_code, 404, denied.content)
        self.assertEqual(self.submit("foreign-submit", session_id).status_code, 404)

    def test_tail_rewrite_uses_current_definition_and_preserves_private_ownership(self):
        first = self.submit("before-rewrite")
        self.assertEqual(first.status_code, 202, first.content)
        original = AgentRun.objects.get(id=first.json()["agentRunId"])
        AgentRun.objects.filter(id=original.id).update(status="completed")
        self.client.force_login(self.admin)
        self.send("patch", self.base + f"/agent-definitions/{self.definition['id']}",
                  {"instructions": "Published rewrite instructions."})
        latest = self.publish(self.definition)
        payload = {"operationId": "rewrite-private", "text": "revised hello",
                   "agentId": self.agent["id"], "modelConfigRef": self.model.id,
                   "tailAction": {"type": "rewriteLastUser", "targetMessageId": "message-user",
                                  "expectedTailMessageId": "message-tail"}}
        path = self.base + f"/sessions/{original.session_id}/messages"
        self.request_execution_profile.reset_mock()
        denied = self.send("post", path, payload)
        self.assertEqual(denied.status_code, 404, denied.content)
        self.request_execution_profile.assert_not_called()
        self.assertEqual(AgentRun.objects.count(), 1)
        self.client.force_login(self.member)
        accepted = self.send("post", path, payload)
        self.assertEqual(accepted.status_code, 202, accepted.content)
        rewritten = AgentRun.objects.get(id=accepted.json()["agentRunId"])
        self.assertEqual((rewritten.session_id, rewritten.user_id), (original.session_id, self.member.id))
        self.assertEqual((rewritten.definition_version_id, rewritten.agent_instructions),
                         (latest["id"], latest["instructions"]))
        from app_core.runtime_client import build_agent_run_start
        start = build_agent_run_start(rewritten)
        self.assertEqual(start["tailAction"], payload["tailAction"])
        original.refresh_from_db()
        self.assertEqual((original.definition_version_id, original.agent_instructions),
                         (self.version["id"], self.version["instructions"]))
        AgentRun.objects.filter(id=rewritten.id).update(status="completed")
        self.client.force_login(self.admin)
        self.availability(self.definition, "none")
        self.client.force_login(self.member)
        payload["operationId"] = "rewrite-revoked"
        rejected = self.send("post", path, payload)
        self.assertEqual(rejected.status_code, 403, rejected.content)
        self.assertEqual(AgentRun.objects.count(), 2)


class AgentDefinitionConcurrencyTests(DefinitionFixture, TransactionTestCase):
    serialized_rollback = True

    def _submit_across_access_change(self, *, remove_membership):
        from app_core.models import HostedOperationReceipt
        definition, _ = self.ready_definition()
        self.assertEqual(self.availability(definition, "members",
                         [self.membership.id, self.other_membership.id]).status_code, 200)
        self.client.force_login(self.member)
        agent = self.instance(definition).json()["agent"]
        self.client.force_login(self.admin)
        submitter = Client()
        submitter.force_login(self.member)
        model = ModelConfig.objects.create(displayName="Fixture")
        preflight_complete, changed = threading.Event(), threading.Event()

        def profile():
            preflight_complete.set()
            if not changed.wait(timeout=10):
                raise AssertionError("access change did not commit")
            return PROFILE

        def submit():
            close_old_connections()
            try:
                return self.send("post", self.base + "/sessions/new/messages", {
                    "operationId": "access-change-race", "agentId": agent["id"],
                    "text": "hello", "modelConfigRef": model.id,
                }, submitter)
            finally:
                close_old_connections()

        with patch("app_core.http.workspaces.request_execution_profile", side_effect=profile), \
             patch("app_core.http.workspaces.schedule_agent_run_lifecycle") as schedule:
            with ThreadPoolExecutor(max_workers=1) as pool:
                submitted = pool.submit(submit)
                try:
                    self.assertTrue(preflight_complete.wait(timeout=10))
                    if remove_membership:
                        change = self.client.delete(self.base + f"/members/{self.membership.id}")
                    else:
                        change = self.availability(definition, "members", [self.other_membership.id])
                    self.assertEqual(change.status_code, 200, change.content)
                finally:
                    changed.set()
                response = submitted.result(timeout=20)
        self.assertEqual(response.status_code, 404 if remove_membership else 403, response.content)
        self.assertEqual(response.json(), {"error": "session_not_found" if remove_membership
                                          else "agent_definition_not_available"})
        self.assertFalse(Session.objects.exists())
        self.assertFalse(AgentRun.objects.exists())
        self.assertFalse(HostedOperationReceipt.objects.exists())
        self.assertTrue(Agent.objects.filter(id=agent["id"], owner=self.member).exists())
        self.assertEqual(self.client.get(self.base + f"/agent-definitions/{definition['id']}").json()
                         ["definition"]["membershipIds"], [self.other_membership.id])
        schedule.assert_not_called()

    def test_member_allowlist_revocation_during_submission_rejects_only_revoked_member(self):
        self._submit_across_access_change(remove_membership=False)

    def test_membership_deletion_during_submission_rolls_back_acceptance(self):
        self._submit_across_access_change(remove_membership=True)

    def test_publication_after_submission_lock_preserves_accepted_old_version(self):
        from app_core.workspace_access import locked_workspace_membership_for
        definition, previous = self.ready_definition()
        self.client.force_login(self.member)
        agent = self.instance(definition).json()["agent"]
        self.client.force_login(self.admin)
        self.send("patch", self.base + f"/agent-definitions/{definition['id']}",
                  {"instructions": "Later publication instructions."})
        publisher, submitter = Client(), Client()
        publisher.force_login(self.admin)
        submitter.force_login(self.member)
        model = ModelConfig.objects.create(displayName="Fixture")
        preflight_complete = threading.Event()
        submission_locked, publication_waiting = threading.Event(), threading.Event()

        def profile():
            preflight_complete.set()
            return PROFILE

        def pause_submission(user, *args, **kwargs):
            membership = locked_workspace_membership_for(user, *args, **kwargs)
            if user.id == self.member.id and preflight_complete.is_set():
                submission_locked.set()
                if not publication_waiting.wait(timeout=10):
                    raise AssertionError("publisher did not reach workspace lock")
            return membership

        def mark_publication(user, *args, **kwargs):
            publication_waiting.set()
            return locked_workspace_membership_for(user, *args, **kwargs)

        def publish():
            close_old_connections()
            try:
                if not submission_locked.wait(timeout=10):
                    raise AssertionError("submitter did not acquire workspace lock")
                return self.send("post", self.base + f"/agent-definitions/{definition['id']}/versions", client=publisher)
            finally:
                close_old_connections()

        def submit():
            close_old_connections()
            try:
                return self.send("post", self.base + "/sessions/new/messages", {
                    "operationId": "submission-before-publication", "agentId": agent["id"],
                    "text": "hello", "modelConfigRef": model.id,
                }, submitter)
            finally:
                close_old_connections()

        with patch("app_core.http.workspaces.locked_workspace_membership_for", side_effect=pause_submission), \
             patch("app_core.http.agent_definitions.locked_workspace_membership_for", side_effect=mark_publication), \
             patch("app_core.http.workspaces.request_execution_profile", side_effect=profile), \
             patch("app_core.http.workspaces.schedule_agent_run_lifecycle", return_value="inserted"):
            with ThreadPoolExecutor(max_workers=2) as pool:
                submitted, published = pool.submit(submit), pool.submit(publish)
                accepted, publication = submitted.result(timeout=20), published.result(timeout=20)
        self.assertEqual(accepted.status_code, 202, accepted.content)
        self.assertEqual(publication.status_code, 201, publication.content)
        run = AgentRun.objects.get(id=accepted.json()["agentRunId"])
        self.assertEqual((run.definition_version_id, run.agent_instructions),
                         (previous["id"], previous["instructions"]))
        self.assertNotEqual(publication.json()["version"]["id"], previous["id"])

    def test_concurrent_first_use_creates_one_private_instance(self):
        definition, _ = self.ready_definition()
        clients = [Client(), Client()]
        for client in clients:
            client.force_login(self.member)
        barrier = threading.Barrier(2)

        def use(client):
            close_old_connections()
            try:
                barrier.wait(timeout=10)
                result = self.instance(definition, client)
                return result.status_code, result.json()
            finally:
                close_old_connections()

        with ThreadPoolExecutor(max_workers=2) as pool:
            results = list(pool.map(use, clients))
        self.assertEqual(sorted(status for status, _ in results), [200, 201], results)
        self.assertEqual(results[0][1], results[1][1])
        self.assertEqual(Agent.objects.filter(workspace=self.workspace, owner=self.member).count(), 1)

    def test_publication_and_submission_use_one_complete_version(self):
        from app_core.models import AgentDefinitionVersion
        from app_core.workspace_access import locked_workspace_membership_for
        definition, previous = self.ready_definition()
        self.client.force_login(self.member)
        agent = self.instance(definition).json()["agent"]
        self.client.force_login(self.admin)
        self.send("patch", self.base + f"/agent-definitions/{definition['id']}",
                  {"name": "New publication", "instructions": "New complete instructions."})
        publisher, submitter = Client(), Client()
        publisher.force_login(self.admin)
        submitter.force_login(self.member)
        model = ModelConfig.objects.create(displayName="Fixture")
        version_created = threading.Event()
        submission_waiting = threading.Event()
        original_create = AgentDefinitionVersion.objects.create

        def pause_publication(**fields):
            version = original_create(**fields)
            version_created.set()
            if not submission_waiting.wait(timeout=10):
                raise AssertionError("submission did not reach acceptance lock")
            return version

        def mark_submission(user, *args, **kwargs):
            if user.id == self.member.id:
                submission_waiting.set()
            return locked_workspace_membership_for(user, *args, **kwargs)

        def publish():
            close_old_connections()
            try:
                return self.send("post", self.base + f"/agent-definitions/{definition['id']}/versions", client=publisher)
            finally:
                close_old_connections()

        def submit():
            close_old_connections()
            try:
                if not version_created.wait(timeout=10):
                    raise AssertionError("publication did not reach transaction")
                return self.send("post", self.base + "/sessions/new/messages", {
                    "operationId": "publication-race", "agentId": agent["id"], "text": "hello", "modelConfigRef": model.id,
                }, submitter)
            finally:
                close_old_connections()

        with patch.object(AgentDefinitionVersion.objects, "create", side_effect=pause_publication), \
             patch("app_core.http.workspaces.locked_workspace_membership_for", side_effect=mark_submission), \
             patch("app_core.http.workspaces.request_execution_profile", return_value=PROFILE), \
             patch("app_core.http.workspaces.schedule_agent_run_lifecycle", return_value="inserted"):
            with ThreadPoolExecutor(max_workers=2) as pool:
                published = pool.submit(publish)
                submitted = pool.submit(submit)
                published_response, submitted_response = published.result(timeout=20), submitted.result(timeout=20)
        self.assertEqual(published_response.status_code, 201, published_response.content)
        self.assertEqual(submitted_response.status_code, 202, submitted_response.content)
        version = published_response.json()["version"]
        run = AgentRun.objects.get(id=submitted_response.json()["agentRunId"])
        self.assertEqual((run.definition_version_id, run.agent_instructions), (version["id"], version["instructions"]))
        self.assertEqual(AgentDefinitionVersion.objects.get(id=previous["id"]).instructions, previous["instructions"])

    def test_revocation_between_preflight_and_acceptance_rolls_back_new_session(self):
        definition, _ = self.ready_definition()
        self.client.force_login(self.member)
        agent = self.instance(definition).json()["agent"]
        self.client.force_login(self.admin)
        submitter = Client()
        submitter.force_login(self.member)
        model = ModelConfig.objects.create(displayName="Fixture")
        preflight_complete, revoked = threading.Event(), threading.Event()

        def profile():
            preflight_complete.set()
            if not revoked.wait(timeout=10):
                raise AssertionError("revocation did not commit")
            return PROFILE

        def submit():
            close_old_connections()
            try:
                return self.send("post", self.base + "/sessions/new/messages", {
                    "operationId": "revocation-race", "agentId": agent["id"], "text": "hello", "modelConfigRef": model.id,
                }, submitter)
            finally:
                close_old_connections()

        with patch("app_core.http.workspaces.request_execution_profile", side_effect=profile), \
             patch("app_core.http.workspaces.schedule_agent_run_lifecycle") as schedule:
            with ThreadPoolExecutor(max_workers=1) as pool:
                submitted = pool.submit(submit)
                try:
                    self.assertTrue(preflight_complete.wait(timeout=10))
                    self.assertEqual(self.availability(definition, "none").status_code, 200)
                finally:
                    revoked.set()
                response = submitted.result(timeout=20)
        self.assertEqual(response.status_code, 403, response.content)
        self.assertEqual(response.json(), {"error": "agent_definition_not_available"})
        self.assertFalse(Session.objects.exists())
        self.assertFalse(AgentRun.objects.exists())
        schedule.assert_not_called()
