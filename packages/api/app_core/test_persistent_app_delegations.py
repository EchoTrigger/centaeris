"""Persistent user authority is independent from replaceable bearer credentials."""
import json
from concurrent.futures import ThreadPoolExecutor
from datetime import timedelta
from threading import Barrier, Event
from types import SimpleNamespace
from unittest.mock import patch

from django.db import close_old_connections
from django.test import Client, TestCase, TransactionTestCase
from django.utils import timezone

from .app_delegations import authenticate_delegation, require_request_delegation, require_run_delegation
from .models import Agent, AgentInput, AgentRun, BusinessAgentBranch, HostedOperationReceipt, ModelConfig, Session, UserAppDelegation, Workspace, WorkspaceMembership
from .test_agent_definitions import PROFILE
from .test_app_delegations import AppDelegationFixture, AppDelegationSessionFixture


NATIVE_SCOPES = ["assistant:use", "messages:submit", "sessions:read", "artifacts:read"]


class PersistentDelegationConsentTests(AppDelegationFixture, TestCase):
    def managed_payload(self):
        definition, _ = self.ready_definition()
        app = self.app()
        return {"appId": app["id"], "workspaceId": self.workspace.id,
                "definitionId": definition["id"], "scopes": ["sessions:read"]}

    def issue(self, payload):
        return self.send("post", "/api/account/app-delegations", payload, client=self.member_client)

    def test_explicit_null_is_persistent_and_omission_keeps_one_hour_default(self):
        payload = self.managed_payload()
        before = timezone.now()
        finite = self.issue(payload)
        self.assertEqual(finite.status_code, 201, finite.content)
        finite_grant = UserAppDelegation.objects.get(pk=finite.json()["delegation"]["id"])
        self.assertGreaterEqual(finite_grant.expires_at, before + timedelta(hours=1))
        permanent = self.issue({**payload, "expiresInSeconds": None})
        self.assertEqual(permanent.status_code, 201, permanent.content)
        metadata = permanent.json()["delegation"]
        self.assertIsNone(metadata["expiresAt"])
        self.assertEqual(metadata["credentialVersion"], 1)
        self.assertIsNone(metadata["agentId"])
        self.assertIsNone(metadata["agentName"])
        self.assertIsNone(UserAppDelegation.objects.get(pk=metadata["id"]).expires_at)
        finite_grant.refresh_from_db()
        self.assertIsNotNone(finite_grant.expires_at, "New consent never upgrades existing grants")

    def test_consent_requires_exactly_one_nonempty_target(self):
        payload = self.managed_payload()
        agent = Agent.objects.create(workspace=self.workspace, owner=self.member, name="Business Agent")
        for changes in ({"definitionId": None}, {"definitionId": None, "agentId": None},
                        {"agentId": agent.pk}, {"definitionId": ""}, {"definitionId": None, "agentId": ""}):
            with self.subTest(changes=changes):
                response = self.issue({**payload, **changes})
                self.assertEqual(response.status_code, 400, response.content)
                self.assertEqual(response.json(), {"error": "delegation_request_invalid"})

    def test_native_target_is_exact_and_preserves_user_owned_identity(self):
        app = self.app()
        agent = Agent.objects.create(workspace=self.workspace, owner=self.member, name="Business Agent")
        other = Agent.objects.create(workspace=self.workspace, owner=self.member, name="Another Agent")
        response = self.issue({"appId": app["id"], "workspaceId": self.workspace.pk,
            "agentId": agent.pk, "scopes": NATIVE_SCOPES, "expiresInSeconds": None})
        self.assertEqual(response.status_code, 201, response.content)
        metadata = response.json()["delegation"]
        self.assertEqual((metadata["agentId"], metadata["agentName"]), (agent.pk, agent.name))
        self.assertIsNone(metadata["definitionId"])
        self.assertIsNone(metadata["definitionName"])
        grant = authenticate_delegation(response.json()["accessToken"], "assistant:use")
        from .app_delegations import DelegationRejected, require_delegated_agent, require_delegated_session
        resolved = Client().post(f"/api/agents/{agent.pk}/business-branches/resolve",
            json.dumps({"businessUserId": "native-contract-user"}), content_type="application/json",
            HTTP_AUTHORIZATION="Bearer " + response.json()["accessToken"])
        self.assertEqual(resolved.status_code, 201, resolved.content)
        branch = BusinessAgentBranch.objects.get(pk=resolved.json()["branchId"])
        with self.assertRaises(DelegationRejected):
            require_delegated_agent(grant, agent.pk)
        self.assertEqual(require_delegated_agent(grant, branch.agent_id, business_branch=branch).pk, branch.agent_id)
        session = branch.session
        self.assertEqual(require_delegated_session(grant, session.pk, business_branch=branch).pk, session.pk)
        foreign = Session.objects.create(workspace=self.workspace, owner=self.member, agent=other)
        ordinary = Session.objects.create(workspace=self.workspace, owner=self.member, agent=branch.agent)
        with self.assertRaises(DelegationRejected):
            require_delegated_agent(grant, other.pk, business_branch=branch)
        with self.assertRaises(DelegationRejected):
            require_delegated_session(grant, foreign.pk, business_branch=branch)
        with self.assertRaises(DelegationRejected):
            require_delegated_session(grant, ordinary.pk, business_branch=branch)
        denied = Client().post(self.base + f"/sessions/{session.pk}/messages",
            json.dumps({"operationId": "native-generic-run", "text": "Fixture", "modelConfigRef": "unused"}),
            content_type="application/json", HTTP_AUTHORIZATION="Bearer " + response.json()["accessToken"],
            HTTP_X_CENTAERIS_BUSINESS_BRANCH_ID=branch.pk)
        self.assertEqual(denied.status_code, 403, denied.content)
        self.assertFalse(AgentRun.objects.exists())

    def test_native_consent_rejects_foreign_managed_and_inactive_targets_and_extra_scopes(self):
        app = self.app()
        native = Agent.objects.create(workspace=self.workspace, owner=self.member, name="Business Agent")
        foreign = Agent.objects.create(workspace=self.workspace, owner=self.other, name="Foreign Agent")
        definition, _ = self.ready_definition()
        managed = self.instance(definition, client=self.member_client).json()["agent"]
        other_workspace = Workspace.objects.create(name="Other", createdBy=self.member)
        WorkspaceMembership.objects.create(workspace=other_workspace, user=self.member, role="owner")
        elsewhere = Agent.objects.create(workspace=other_workspace, owner=self.member, name="Elsewhere")
        body = {"appId": app["id"], "workspaceId": self.workspace.pk, "agentId": native.pk,
                "scopes": NATIVE_SCOPES, "expiresInSeconds": None}
        for agent_id in (foreign.pk, managed["id"], elsewhere.pk):
            denied = self.issue({**body, "agentId": agent_id})
            self.assertEqual(denied.status_code, 404, denied.content)
        for scope in ("sessions:create", "attachments:write", "runs:cancel", "events:read"):
            denied = self.issue({**body, "scopes": [*NATIVE_SCOPES, scope]})
            self.assertEqual(denied.status_code, 400, denied.content)
        native.status, native.deletedAt = "deleted", timezone.now()
        native.save()
        self.assertEqual(self.issue(body).status_code, 404)
        self.assertFalse(UserAppDelegation.objects.exists())

    def test_native_input_origin_is_complete_immutable_and_bound_to_the_grant(self):
        app = self.app()
        agent = Agent.objects.create(workspace=self.workspace, owner=self.member, name="Business Agent")
        other = Agent.objects.create(workspace=self.workspace, owner=self.member, name="Another Agent")
        issued = self.issue({"appId": app["id"], "workspaceId": self.workspace.pk,
            "agentId": agent.pk, "scopes": NATIVE_SCOPES, "expiresInSeconds": None})
        self.assertEqual(issued.status_code, 201, issued.content)
        grant = UserAppDelegation.objects.get(pk=issued.json()["delegation"]["id"])
        resolved = Client().post(f"/api/agents/{agent.pk}/business-branches/resolve",
            json.dumps({"businessUserId": "native-input-user"}), content_type="application/json",
            HTTP_AUTHORIZATION="Bearer " + issued.json()["accessToken"])
        self.assertEqual(resolved.status_code, 201, resolved.content)
        branch = BusinessAgentBranch.objects.get(pk=resolved.json()["branchId"])
        base = dict(agent=branch.agent, session=branch.session, input_id="business-input", membership_ref=self.membership.pk,
                    sequence=1, accepted_source_sequence=0, body="业务输入", created_at_ms=1)
        input_fact = AgentInput.objects.create(**base, acting_app=grant.app, app_delegation=grant, credential_version=1)
        self.assertEqual((input_fact.acting_app_id, input_fact.app_delegation_id, input_fact.credential_version),
                         (grant.app_id, grant.pk, 1))
        input_fact.credential_version = 2
        with self.assertRaisesMessage(ValueError, "agent_input_is_immutable"):
            input_fact.save()
        for changes in ({"acting_app": grant.app}, {"app_delegation": grant}, {"credential_version": 1},
                        {"acting_app": grant.app, "app_delegation": grant, "credential_version": 0}):
            with self.assertRaises(ValueError):
                AgentInput.objects.create(**{**base, "input_id": "incomplete", "sequence": 2}, **changes)
        with self.assertRaises(ValueError):
            AgentInput.objects.create(**{**base, "agent": other, "input_id": "foreign", "sequence": 2},
                acting_app=grant.app, app_delegation=grant, credential_version=1)


class DelegationCredentialRotationTests(AppDelegationSessionFixture, TestCase):
    def rotate(self, version=1, client=None, **changes):
        return self.send("post", f"/api/account/app-delegations/{self.grant.pk}/rotate",
            {"expectedCredentialVersion": version, **changes}, client=client or self.member_client)

    def test_rotation_invalidates_old_token_and_keeps_grant_agent_history_and_finite_expiry(self):
        before = self.grant.expires_at
        rotated = self.rotate()
        self.assertEqual(rotated.status_code, 200, rotated.content)
        self.assertEqual(rotated["Cache-Control"], "no-store")
        data = rotated.json()
        self.assertEqual(data["delegation"]["id"], self.grant.pk)
        self.assertEqual(data["delegation"]["credentialVersion"], 2)
        self.assertNotEqual(data["accessToken"], self.token)
        self.assertEqual(self.api("get", f"/api/sessions/{self.session.pk}").status_code, 401)
        self.token = data["accessToken"]
        self.assertEqual(self.api("get", f"/api/sessions/{self.session.pk}").status_code, 200)
        self.grant.refresh_from_db()
        self.assertEqual(self.grant.expires_at, before)
        self.assertEqual(Session.objects.get(pk=self.session.pk).agent_id, self.agent.pk)
        listing = self.member_client.get("/api/account/app-delegations")
        self.assertNotIn(self.token, listing.content.decode())
        self.assertNotIn(self.grant.token_digest, listing.content.decode())
        from .models import AppDelegationAuditEvent
        events = list(AppDelegationAuditEvent.objects.filter(delegation=self.grant).order_by("created_at", "pk"))
        self.assertEqual([(event.action, event.credential_version, event.actor_id) for event in events],
                         [("issued", 1, self.member.pk), ("rotated", 2, self.member.pk)])
        self.assertNotIn("token_digest", [field.name for field in AppDelegationAuditEvent._meta.fields])
        events[-1].action = "revoked"
        with self.assertRaisesMessage(ValueError, "app_delegation_audit_is_immutable"):
            events[-1].save()

    def test_stale_version_and_invalid_rotation_do_not_change_credential(self):
        rotated = self.rotate()
        self.assertEqual(rotated.status_code, 200, rotated.content)
        self.assertEqual(self.rotate().status_code, 409)
        self.grant.refresh_from_db()
        digest = self.grant.token_digest
        for changes in ({"expectedCredentialVersion": 0}, {"expectedCredentialVersion": True},
                        {"expectedCredentialVersion": "2"}, {"expectedCredentialVersion": 2.0},
                        {"expectedCredentialVersion": None}, {"credentialVersion": 2}, {"expiresInSeconds": None}):
            denied = self.send("post", f"/api/account/app-delegations/{self.grant.pk}/rotate", changes,
                client=self.member_client)
            self.assertEqual(denied.status_code, 400, denied.content)
            self.assertEqual(denied.json(), {"error": "delegation_request_invalid"})
        self.grant.refresh_from_db()
        self.assertEqual((self.grant.credential_version, self.grant.token_digest), (2, digest))

    def test_plain_model_save_cannot_rotate_or_change_authority_policy(self):
        from .app_delegations import token_digest
        for field, value in (("token_digest", token_digest("cwa_" + "z" * 43)), ("credential_version", 2),
                             ("expires_at", None), ("scopes", ["sessions:read"])):
            self.grant.refresh_from_db()
            setattr(self.grant, field, value)
            with self.assertRaisesMessage(ValueError, "authority is immutable"):
                self.grant.save()

    def test_rotation_cannot_restore_expired_membership_or_application_authority(self):
        original_expiry = self.grant.expires_at
        UserAppDelegation.objects.filter(pk=self.grant.pk).update(expires_at=timezone.now() - timedelta(seconds=1))
        self.assertEqual(self.rotate().status_code, 403)
        UserAppDelegation.objects.filter(pk=self.grant.pk).update(expires_at=original_expiry)
        self.membership.delete()
        WorkspaceMembership.objects.create(workspace=self.workspace, user=self.member)
        self.assertEqual(self.rotate().status_code, 403)

    def test_digest_lookup_race_cannot_authenticate_rotated_credential(self):
        from . import app_delegations
        original = app_delegations.require_current_delegation
        def rotate_before_authority(*args, **kwargs):
            with patch.object(app_delegations, "require_current_delegation", original):
                self.assertEqual(self.rotate().status_code, 200)
            return original(*args, **kwargs)
        with patch.object(app_delegations, "require_current_delegation", side_effect=rotate_before_authority):
            with self.assertRaises(app_delegations.DelegationRejected) as denied:
                authenticate_delegation(self.token, "sessions:read")
        self.assertEqual((denied.exception.code, denied.exception.status), ("delegation_invalid", 401))

    def test_rotation_is_owner_browser_only_and_revoked_grants_cannot_rotate(self):
        self.assertEqual(self.rotate(client=self.platform_client).status_code, 404)
        bearer = Client().post(f"/api/account/app-delegations/{self.grant.pk}/rotate", "{}",
            content_type="application/json", HTTP_AUTHORIZATION="Bearer " + self.token)
        self.assertEqual(bearer.status_code, 401)
        csrf_client = Client(enforce_csrf_checks=True)
        csrf_client.force_login(self.member)
        self.assertEqual(self.rotate(client=csrf_client).status_code, 403)
        self.member_client.delete(f"/api/account/app-delegations/{self.grant.pk}")
        self.assertEqual(self.rotate().status_code, 403)
        self.member_client.delete(f"/api/account/app-delegations/{self.grant.pk}")
        from .models import AppDelegationAuditEvent
        self.assertEqual(AppDelegationAuditEvent.objects.filter(delegation=self.grant, action="revoked").count(), 1)

    def test_old_authenticated_request_is_rejected_after_rotation(self):
        authenticated = authenticate_delegation(self.token, "messages:submit")
        request = SimpleNamespace(app_delegation=authenticated,
            app_delegation_credential_version=authenticated.credential_version)
        self.assertEqual(self.rotate().status_code, 200)
        from .app_delegations import DelegationRejected
        with self.assertRaises(DelegationRejected) as denied:
            require_request_delegation(request, "messages:submit", session_id=self.session.pk)
        self.assertEqual((denied.exception.code, denied.exception.status), ("delegation_invalid", 401))

    def test_accepted_run_keeps_grant_authority_when_credential_rotates(self):
        with patch("app_core.http.workspaces.request_execution_profile", return_value=PROFILE), \
             patch("app_core.http.workspaces.schedule_agent_run_lifecycle", return_value="inserted"):
            model = ModelConfig.objects.create(displayName="Fixture")
            accepted = self.api("post", self.base + f"/sessions/{self.session.pk}/messages",
                {"operationId": "accepted-before-rotation", "text": "Fixture", "modelConfigRef": model.pk})
        self.assertEqual(accepted.status_code, 202, accepted.content)
        run = AgentRun.objects.get(pk=accepted.json()["agentRunId"])
        self.assertEqual(self.rotate().status_code, 200)
        require_run_delegation(run)
        run.refresh_from_db()
        self.assertEqual((run.acting_app_id, run.app_delegation_id), (self.application["id"], self.grant.pk))
        self.assertEqual(HostedOperationReceipt.objects.get(operationId="accepted-before-rotation").app_delegation_id,
                         self.grant.pk)


class DelegationRotationConcurrencyTests(AppDelegationSessionFixture, TransactionTestCase):
    serialized_rollback = True

    def test_one_concurrent_rotation_wins_and_only_its_new_token_authenticates(self):
        barrier = Barrier(2)
        def rotate():
            close_old_connections()
            try:
                client = Client()
                client.force_login(self.member)
                barrier.wait(timeout=10)
                return client.post(f"/api/account/app-delegations/{self.grant.pk}/rotate",
                    json.dumps({"expectedCredentialVersion": 1}), content_type="application/json")
            finally:
                close_old_connections()
        with ThreadPoolExecutor(max_workers=2) as pool:
            responses = list(pool.map(lambda _: rotate(), range(2)))
        self.assertEqual(sorted(response.status_code for response in responses), [200, 409])
        winner = next(response for response in responses if response.status_code == 200)
        self.assertEqual(authenticate_delegation(winner.json()["accessToken"], "sessions:read").credential_version, 2)
        from .models import AppDelegationAuditEvent
        self.assertEqual(AppDelegationAuditEvent.objects.filter(delegation=self.grant, action="rotated").count(), 1)

    def test_rotation_committed_during_runtime_preparation_rejects_old_request_acceptance(self):
        prepared, rotated = Event(), Event()
        model = ModelConfig.objects.create(displayName="Fixture")
        def profile():
            prepared.set()
            if not rotated.wait(timeout=10):
                raise AssertionError("rotation did not commit")
            return PROFILE
        def submit():
            close_old_connections()
            try:
                return self.api("post", self.base + f"/sessions/{self.session.pk}/messages",
                    {"operationId": "rotating-request", "text": "Fixture", "modelConfigRef": model.pk})
            finally:
                close_old_connections()
        with patch("app_core.http.workspaces.request_execution_profile", side_effect=profile), \
             patch("app_core.http.workspaces.schedule_agent_run_lifecycle") as schedule:
            with ThreadPoolExecutor(max_workers=1) as pool:
                future = pool.submit(submit)
                try:
                    self.assertTrue(prepared.wait(timeout=10))
                    response = self.send("post", f"/api/account/app-delegations/{self.grant.pk}/rotate",
                        {"expectedCredentialVersion": 1}, client=self.member_client)
                    self.assertEqual(response.status_code, 200, response.content)
                finally:
                    rotated.set()
                denied = future.result(timeout=20)
        self.assertEqual(denied.status_code, 401, denied.content)
        self.assertFalse(AgentRun.objects.exists())
        self.assertFalse(HostedOperationReceipt.objects.exists())
        schedule.assert_not_called()
