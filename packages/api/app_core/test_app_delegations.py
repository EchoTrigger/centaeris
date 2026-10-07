import hashlib
import json
from datetime import timedelta
from unittest.mock import patch

from django.contrib.auth.models import User
from django.test import Client, TestCase
from django.utils import timezone

from app_core.models import Agent, AgentRun, HostedOperationReceipt, ModelConfig, Session, UserAppDelegation, WorkspaceMembership
from app_core.test_agent_definitions import DefinitionFixture, PROFILE


ALL_SCOPES = ["assistant:use", "sessions:read", "sessions:create", "messages:submit",
              "attachments:write", "events:read", "artifacts:read", "runs:cancel"]


class AppDelegationFixture(DefinitionFixture):
    def setUp(self):
        super().setUp()
        self.platform_admin = User.objects.create_superuser(username="app-custodian", password="fixture-password")
        self.platform_client = Client()
        self.platform_client.force_login(self.platform_admin)
        self.member_client = Client()
        self.member_client.force_login(self.member)

    def app(self, *, active=True):
        response = self.send("post", "/api/admin/business-apps", {"name": "Business fixture"}, client=self.platform_client)
        self.assertEqual(response.status_code, 201, response.content)
        app = response.json()["app"]
        self.assertEqual(app["status"], "pending")
        if active:
            response = self.send("patch", f"/api/admin/business-apps/{app['id']}", {"status": "active"}, client=self.platform_client)
            self.assertEqual(response.status_code, 200, response.content)
            app = response.json()["app"]
        return app

    def delegation(self, *, scopes=None, client=None):
        definition, _ = self.ready_definition()
        app = self.app()
        payload = {"appId": app["id"], "workspaceId": self.workspace.id, "definitionId": definition["id"],
                   "scopes": scopes or ALL_SCOPES, "expiresInSeconds": 3600}
        response = self.send("post", "/api/account/app-delegations", payload, client=client or self.member_client)
        self.assertEqual(response.status_code, 201, response.content)
        return definition, app, response


class AppDelegationConsentTests(AppDelegationFixture, TestCase):
    def test_pending_app_is_not_available_and_revocation_is_permanent(self):
        app = self.app(active=False)
        self.assertEqual(self.member_client.get("/api/business-apps").json(), {"apps": []})
        path = f"/api/admin/business-apps/{app['id']}"
        self.assertEqual(self.send("patch", path, {"status": "active"}, client=self.platform_client).status_code, 200)
        self.assertEqual(self.member_client.get("/api/business-apps").json()["apps"][0]["id"], app["id"])
        self.assertEqual(self.send("patch", path, {"status": "revoked"}, client=self.platform_client).status_code, 200)
        response = self.send("patch", path, {"status": "active"}, client=self.platform_client)
        self.assertEqual(response.status_code, 409, response.content)
        self.assertEqual(response.json(), {"error": "business_app_revoked"})

    def test_token_is_one_time_and_only_digest_is_persisted_for_current_user(self):
        definition, app, response = self.delegation()
        from app_core.models import UserAppDelegation
        body = response.json()
        grant = UserAppDelegation.objects.get(id=body["delegation"]["id"])
        self.assertEqual(grant.user_id, self.member.id)
        self.assertEqual(grant.app_id, app["id"])
        self.assertEqual(grant.definition_id, definition["id"])
        self.assertEqual(grant.membership_ref, self.membership.id)
        self.assertEqual(grant.token_digest, "sha256:" + hashlib.sha256(body["accessToken"].encode()).hexdigest())
        self.assertEqual(body["tokenType"], "Bearer")
        self.assertEqual(response["Cache-Control"], "no-store")
        listing = self.member_client.get("/api/account/app-delegations")
        self.assertEqual(listing.status_code, 200, listing.content)
        self.assertNotIn(body["accessToken"], listing.content.decode())
        self.assertNotIn(grant.token_digest, listing.content.decode())
        self.assertEqual(self.platform_client.get("/api/account/app-delegations").json(), {"delegations": []})

    def test_caller_cannot_choose_user_or_security_claims(self):
        definition, _ = self.ready_definition()
        app = self.app()
        body = {"appId": app["id"], "workspaceId": self.workspace.id, "definitionId": definition["id"],
                "scopes": ["sessions:read"], "expiresInSeconds": 3600}
        for field, value in [("userId", str(self.other.id)), ("issuer", "caller"), ("audience", "caller"),
                             ("accessToken", "caller"), ("appDelegationId", "caller")]:
            response = self.send("post", "/api/account/app-delegations", {**body, field: value}, client=self.member_client)
            self.assertEqual(response.status_code, 400, response.content)
            self.assertEqual(response.json(), {"error": "delegation_request_invalid"})

    def test_bearer_cannot_manage_platform_or_assistant_configuration(self):
        _, _, issued = self.delegation(client=self.client)
        token = issued.json()["accessToken"]
        for path in ["/api/admin/business-apps", self.base + "/agent-definitions",
                     "/api/account/app-delegations", "/api/admin/mcp-bearer-credentials"]:
            response = Client().get(path, HTTP_AUTHORIZATION="Bearer " + token)
            self.assertEqual(response.status_code, 401, (path, response.content))

    def test_consent_rejects_empty_duplicate_unknown_scopes_and_out_of_range_expiry(self):
        definition, _ = self.ready_definition()
        app = self.app()
        body = {"appId": app["id"], "workspaceId": self.workspace.id, "definitionId": definition["id"],
                "scopes": ["sessions:read"], "expiresInSeconds": 3600}
        for invalid in [{"scopes": []}, {"scopes": ["sessions:read", "sessions:read"]},
                        {"scopes": ["credentials:manage"]}, {"expiresInSeconds": 299},
                        {"expiresInSeconds": 86401}]:
            response = self.send("post", "/api/account/app-delegations", {**body, **invalid}, client=self.member_client)
            self.assertEqual(response.status_code, 400, response.content)
            self.assertEqual(response.json(), {"error": "delegation_request_invalid"})
        self.assertFalse(UserAppDelegation.objects.exists())

    def test_cookie_and_bearer_never_fall_back_to_browser_identity(self):
        agent = Agent.objects.create(workspace=self.workspace, owner=self.member, name="Private")
        session = Session.objects.create(workspace=self.workspace, owner=self.member, agent=agent)
        for header in ["Bearer cwa_" + "a" * 43, "Bearer broken", "Basic ignored"]:
            response = self.member_client.get(f"/api/sessions/{session.id}", HTTP_AUTHORIZATION=header)
            self.assertEqual(response.status_code, 400, response.content)
            self.assertEqual(response.json(), {"error": "authentication_mixed"})

    def test_cookie_mutation_still_requires_csrf(self):
        agent = Agent.objects.create(workspace=self.workspace, owner=self.member, name="Private")
        browser = Client(enforce_csrf_checks=True)
        browser.force_login(self.member)
        response = self.send("post", self.base + "/sessions", {"agentId": agent.id, "operationId": "csrf-fixture"}, client=browser)
        self.assertEqual(response.status_code, 403)
        self.assertEqual(response.json(), {"error": "csrf_failed"})


class AppDelegationSessionFixture(AppDelegationFixture):
    def setUp(self):
        super().setUp()
        self.definition, self.application, issued = self.delegation()
        self.grant = UserAppDelegation.objects.get(id=issued.json()["delegation"]["id"])
        self.token = issued.json()["accessToken"]
        response = self.instance(self.definition, client=self.member_client)
        self.assertEqual(response.status_code, 201, response.content)
        self.agent = Agent.objects.get(id=response.json()["agent"]["id"])
        self.session = Session.objects.create(workspace=self.workspace, owner=self.member, agent=self.agent)
        self.bearer_client = Client()

    def api(self, method, path, payload=None):
        options = {"HTTP_AUTHORIZATION": "Bearer " + self.token}
        if payload is not None:
            options.update(data=json.dumps(payload), content_type="application/json")
        return getattr(self.bearer_client, method)(path, **options)


class AppDelegationUsageTests(AppDelegationSessionFixture, TestCase):
    def test_app_can_select_an_enabled_model_without_model_management_access(self):
        model = ModelConfig.objects.create(displayName="Fixture")
        response = self.api("get", "/api/models")
        self.assertEqual(response.status_code, 200, response.content)
        self.assertEqual(response.json(), self.member_client.get("/api/models").json())
        self.assertIn(model.id, [item["id"] for item in response.json()["models"]])
        self.assertEqual(self.api("get", "/api/admin/models").status_code, 401)

    def test_message_caller_cannot_choose_owner_or_remove_application_origin(self):
        payload = {"operationId": "forged-origin", "text": "Fixture", "modelConfigRef": "fixture"}
        for field, value in [("userId", str(self.other.id)), ("actingAppId", None), ("appDelegationId", None)]:
            response = self.api("post", self.base + f"/sessions/{self.session.id}/messages", {**payload, field: value})
            self.assertEqual(response.status_code, 400, response.content)
        self.assertFalse(AgentRun.objects.exists())
        self.assertFalse(HostedOperationReceipt.objects.exists())

    def test_app_uses_the_users_existing_instance_and_browser_session(self):
        response = self.api("post", self.base + f"/available-agent-definitions/{self.definition['id']}/instance", {})
        self.assertEqual(response.status_code, 200, response.content)
        self.assertEqual(response.json()["agent"]["id"], self.agent.id)
        app_response = self.api("get", f"/api/sessions/{self.session.id}")
        browser_response = self.member_client.get(f"/api/sessions/{self.session.id}")
        self.assertEqual(app_response.status_code, 200, app_response.content)
        self.assertEqual(app_response.json(), browser_response.json())
        self.assertEqual(Session.objects.get(id=self.session.id).owner_id, self.member.id)

    def test_agent_session_and_definition_lists_are_bounded_to_the_grant(self):
        private = Agent.objects.create(workspace=self.workspace, owner=self.member, name="Private")
        Session.objects.create(workspace=self.workspace, owner=self.member, agent=private)
        other_definition, _ = self.ready_definition()
        other = self.instance(other_definition, client=self.member_client).json()["agent"]
        Session.objects.create(workspace=self.workspace, owner=self.member, agent_id=other["id"])
        for path, field, identity in [(self.base + "/agents", "agents", "id"),
                                     (self.base + "/sessions", "sessions", "agentId")]:
            response = self.api("get", path)
            self.assertEqual(response.status_code, 200, response.content)
            self.assertEqual([item[identity] for item in response.json()[field]], [self.agent.id])
        available = self.api("get", self.base + "/available-agent-definitions")
        self.assertEqual(available.status_code, 200, available.content)
        self.assertEqual([item["definitionId"] for item in available.json()["definitions"]], [self.definition["id"]])

    def test_same_user_private_or_other_assistant_and_other_owner_are_inaccessible(self):
        self.assertEqual(self.api("get", f"/api/sessions/{self.session.id}").status_code, 200)
        private = Agent.objects.create(workspace=self.workspace, owner=self.member, name="Private")
        other_definition, _ = self.ready_definition()
        other_agent = self.instance(other_definition, client=self.member_client).json()["agent"]
        other_client = Client()
        other_client.force_login(self.other)
        foreign_agent = self.instance(self.definition, client=other_client).json()["agent"]
        for owner, agent_id in [(self.member, private.id), (self.member, other_agent["id"]), (self.other, foreign_agent["id"])]:
            session = Session.objects.create(workspace=self.workspace, owner=owner, agent_id=agent_id)
            response = self.api("get", f"/api/sessions/{session.id}")
            self.assertEqual(response.status_code, 404, response.content)

    def test_scope_denial_precedes_mutation_and_invalid_bearer_has_no_fallback(self):
        payload = {"appId": self.application["id"], "workspaceId": self.workspace.id, "definitionId": self.definition["id"],
                   "scopes": ["sessions:read"], "expiresInSeconds": 3600}
        issued = self.send("post", "/api/account/app-delegations", payload, client=self.member_client)
        self.token = issued.json()["accessToken"]
        self.assertEqual(self.api("get", f"/api/sessions/{self.session.id}").status_code, 200)
        denied = self.api("post", self.base + "/sessions", {"agentId": self.agent.id, "operationId": "denied"})
        self.assertEqual(denied.status_code, 403, denied.content)
        self.assertEqual(denied.json(), {"error": "delegation_scope_forbidden"})
        self.assertFalse(HostedOperationReceipt.objects.filter(operationId="denied").exists())
        self.token = "cwa_" + "a" * 43
        invalid = self.api("get", f"/api/sessions/{self.session.id}")
        self.assertEqual(invalid.status_code, 401, invalid.content)
        self.assertEqual(invalid.json(), {"error": "delegation_invalid"})

    def test_current_expiry_issuer_audience_and_app_status_are_required(self):
        path = f"/api/sessions/{self.session.id}"
        self.assertEqual(self.api("get", path).status_code, 200)
        User.objects.filter(id=self.member.id).update(is_active=False)
        self.assertEqual(self.api("get", path).status_code, 403)
        User.objects.filter(id=self.member.id).update(is_active=True)
        for field, invalid in [("expires_at", timezone.now() - timedelta(seconds=1)),
                               ("issuer", "another-issuer"), ("audience", "another-api")]:
            previous = getattr(self.grant, field)
            UserAppDelegation.objects.filter(id=self.grant.id).update(**{field: invalid})
            response = self.api("get", path)
            self.assertEqual(response.status_code, 403, (field, response.content))
            UserAppDelegation.objects.filter(id=self.grant.id).update(**{field: previous})
        self.assertEqual(self.send("patch", f"/api/admin/business-apps/{self.application['id']}",
                                  {"status": "revoked"}, client=self.platform_client).status_code, 200)
        self.assertEqual(self.api("get", path).status_code, 403)

    def test_membership_replacement_does_not_restore_delegation_and_owner_can_revoke(self):
        self.assertEqual(self.api("get", f"/api/sessions/{self.session.id}").status_code, 200)
        self.membership.delete()
        WorkspaceMembership.objects.create(workspace=self.workspace, user=self.member)
        self.assertEqual(self.api("get", f"/api/sessions/{self.session.id}").status_code, 403)
        response = self.member_client.delete(f"/api/account/app-delegations/{self.grant.id}")
        self.assertEqual(response.status_code, 204, response.content)
        self.grant.refresh_from_db()
        self.assertIsNotNone(self.grant.revoked_at)
        self.assertTrue(Session.objects.filter(id=self.session.id).exists())

    def test_definition_withdrawal_and_grant_revocation_reject_new_requests(self):
        path = f"/api/sessions/{self.session.id}"
        self.assertEqual(self.api("get", path).status_code, 200)
        self.assertEqual(self.availability(self.definition, "none").status_code, 200)
        self.assertEqual(self.api("get", path).status_code, 403)
        self.assertEqual(self.availability(self.definition).status_code, 200)
        self.assertEqual(self.api("get", path).status_code, 200)
        self.assertEqual(self.member_client.delete(f"/api/account/app-delegations/{self.grant.id}").status_code, 204)
        self.assertEqual(self.api("get", path).status_code, 403)
        self.assertEqual(self.member_client.get(path).status_code, 200)

    def test_operation_replay_is_shared_with_browser_and_remains_assistant_scoped(self):
        payload = {"agentId": self.agent.id, "operationId": "shared-session"}
        app_response = self.api("post", self.base + "/sessions", payload)
        self.assertEqual(app_response.status_code, 201, app_response.content)
        browser_response = self.send("post", self.base + "/sessions", payload, client=self.member_client)
        self.assertEqual(app_response.json(), browser_response.json())
        receipt = HostedOperationReceipt.objects.get(operationId="shared-session")
        self.assertEqual(receipt.user_id, self.member.id)
        self.assertEqual(receipt.acting_app_id, self.application["id"])
        self.assertEqual(receipt.app_delegation_id, self.grant.id)
        read = self.api("get", self.base + "/operations/createSession/shared-session")
        self.assertEqual(read.json(), app_response.json())
        private = Agent.objects.create(workspace=self.workspace, owner=self.member, name="Private")
        foreign_payload = {"agentId": private.id, "operationId": "foreign-assistant"}
        self.assertEqual(self.send("post", self.base + "/sessions", foreign_payload, client=self.member_client).status_code, 201)
        foreign = self.api("post", self.base + "/sessions", {"agentId": self.agent.id, "operationId": "foreign-assistant"})
        self.assertEqual(foreign.status_code, 404, foreign.content)

    def test_deleted_receipts_keep_browser_semantics_within_the_granted_assistant(self):
        payload = {"agentId": self.agent.id, "operationId": "deleted-delegated-session"}
        accepted = self.api("post", self.base + "/sessions", payload)
        self.assertEqual(accepted.status_code, 201, accepted.content)
        session_id = accepted.json()["sessionId"]
        self.assertEqual(self.member_client.delete(f"/api/sessions/{session_id}").status_code, 200)
        path = self.base + "/operations/createSession/" + payload["operationId"]
        browser = self.member_client.get(path)
        delegated = self.api("get", path)
        self.assertEqual(browser.status_code, 410, browser.content)
        self.assertEqual(delegated.status_code, 410, delegated.content)
        self.assertEqual(delegated.json(), browser.json())
        self.assertEqual(self.api("post", self.base + "/sessions", payload).status_code, 410)

        private = Agent.objects.create(workspace=self.workspace, owner=self.member, name="Private")
        foreign = self.send("post", self.base + "/sessions",
            {"agentId": private.id, "operationId": "deleted-private-session"}, client=self.member_client)
        self.assertEqual(foreign.status_code, 201, foreign.content)
        self.assertEqual(self.member_client.delete(f"/api/sessions/{foreign.json()['sessionId']}").status_code, 200)
        self.assertEqual(self.api("get", self.base + "/operations/createSession/deleted-private-session").status_code, 404)
        self.assertEqual(self.api("post", self.base + "/sessions",
            {"agentId": self.agent.id, "operationId": "deleted-private-session"}).status_code, 404)

    @patch("app_core.http.workspaces.request_execution_profile", return_value=PROFILE)
    @patch("app_core.http.workspaces.schedule_agent_run_lifecycle", return_value="inserted")
    def test_submit_keeps_existing_owner_session_and_audits_acting_app(self, _schedule, _profile):
        model = ModelConfig.objects.create(displayName="Fixture")
        path = self.base + f"/sessions/{self.session.id}/messages"
        payload = {"operationId": "delegated-message", "text": "Use primary evidence.", "modelConfigRef": model.id, "attachmentRefs": []}
        accepted = self.api("post", path, payload)
        self.assertEqual(accepted.status_code, 202, accepted.content)
        self.assertEqual(accepted.json()["sessionId"], self.session.id)
        run = AgentRun.objects.get(id=accepted.json()["agentRunId"])
        self.assertEqual(run.user_id, self.member.id)
        self.assertEqual(run.acting_app_id, self.application["id"])
        self.assertEqual(run.app_delegation_id, self.grant.id)
        self.assertEqual(self.send("post", path, payload, client=self.member_client).json(), accepted.json())
        conflict = self.api("post", path, {**payload, "text": "different digest"})
        self.assertEqual(conflict.status_code, 409, conflict.content)
        self.assertEqual(conflict.json(), {"error": "operation_conflict"})
