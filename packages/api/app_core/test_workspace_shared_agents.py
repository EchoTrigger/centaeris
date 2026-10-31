"""Workspace policy is shared while member identities and accepted Runs persist."""
import base64
import io
import json
from unittest.mock import patch

from django.contrib.auth.models import User
from django.test import TransactionTestCase
from PIL import Image

from .models import Agent, AgentDefinitionVersion, AgentRun, ModelConfig, Session, Workspace, WorkspaceMembership

PROFILE = {"schema": "runtime.execution_profile.v1", "imageDigest": "sha256:" + "a" * 64}


class WorkspaceSharedAgentTests(TransactionTestCase):
    serialized_rollback = True

    def setUp(self):
        self.admin = User.objects.create_user(username="shared-owner")
        self.member = User.objects.create_user(username="shared-member")
        self.workspace = Workspace.objects.create(name="Shared", createdBy=self.admin)
        WorkspaceMembership.objects.create(workspace=self.workspace, user=self.admin, role="owner")
        WorkspaceMembership.objects.create(workspace=self.workspace, user=self.member, role="member")
        self.model = ModelConfig.objects.create(displayName="Shared model", thinkingMode="high", thinkingModes=["low", "high"])
        self.legacy = Agent.objects.create(workspace=self.workspace, owner=self.admin, name="Chosen authority",
            instructions="Original shared instructions", model_config=self.model, thinking_mode="high")
        self.client.force_login(self.admin)
        self.base = f"/api/workspaces/{self.workspace.id}"
        for target, value in (("request_execution_profile", PROFILE), ("schedule_agent_run_lifecycle", "inserted")):
            patcher = patch("app_core.http.workspaces." + target, return_value=value)
            patcher.start()
            self.addCleanup(patcher.stop)

    def send(self, method, path, body=None):
        return getattr(self.client, method)(path, json.dumps(body or {}), content_type="application/json")

    def promote(self, **fields):
        response = self.send("post", f"/api/agents/{self.legacy.id}/promote", fields)
        self.assertEqual(response.status_code, 200, response.content)
        return response.json()["agent"]

    def member_instance(self):
        self.client.force_login(self.member)
        response = self.client.get(self.base + "/default-agent")
        self.assertEqual(response.status_code, 200, response.content)
        return response.json()["agent"]

    def submit(self, agent, operation, session="new", **fields):
        return self.send("post", self.base + f"/sessions/{session}/messages",
            {"agentId": agent["id"], "operationId": operation, "text": "Synthetic request", **fields})

    def test_explicit_promotion_preserves_selected_instance_sessions_and_unselected_personal_config(self):
        from copy import deepcopy
        from django.conf import settings
        from .agent_run_authorization_factory import create_agent_run_authorization
        from .runtime_contract import agent_run_binding_matches, verify_agent_run_authorization_signature

        old_model = ModelConfig.objects.create(displayName="Original member model", thinkingMode="low", thinkingModes=["low", "high"])
        target = Agent.objects.create(workspace=self.workspace, owner=self.member, name="Old personal default",
            instructions="Different", model_config=old_model, thinking_mode="low")
        session = Session.objects.create(workspace=self.workspace, owner=self.member, agent=target)
        source_session = Session.objects.create(workspace=self.workspace, owner=self.admin, agent=self.legacy)
        untouched = Agent.objects.create(workspace=self.workspace, owner=self.admin, name="Unrelated")
        old_runs = []
        original = {}
        for agent, owned_session in ((self.legacy, source_session), (target, session)):
            run = AgentRun.objects.create(workspace=self.workspace,
                session=owned_session, user=agent.owner, modelConfig=agent.model_config,
                thinkingMode=agent.thinking_mode, agent_instructions=agent.instructions,
                prompt="Accepted before explicit promotion", status="completed")
            grant = create_agent_run_authorization(run, image_digest=PROFILE["imageDigest"])
            self.assertIsNone(run.definition_version_id)
            verify_agent_run_authorization_signature(grant.payload, settings.AGENT_RUN_AUTHORIZATION_SIGNING_KEY, grant.signature)
            original[run.pk] = (run.definition_version_id, run.agent_instructions, run.modelConfig_id,
                run.thinkingMode, deepcopy(grant.payload), grant.digest, grant.signature)
            old_runs.append(run)

        shared = self.promote(bindAgentIds=[target.id])
        target.refresh_from_db()
        untouched.refresh_from_db()
        session.refresh_from_db()
        source_session.refresh_from_db()
        self.assertEqual(target.definition_id, shared["definitionId"])
        self.assertEqual((session.agent_id, source_session.agent_id), (target.id, self.legacy.id))
        self.assertEqual(target.instructions, "Different")
        self.assertIsNone(untouched.definition_id)
        member = self.member_instance()
        self.assertEqual(member["id"], target.id)
        self.assertEqual(member["instructions"], shared["instructions"])
        self.assertEqual(self.client.get(f"/api/agents/{target.id}/model-settings").json()["modelConfigRef"], self.model.id)

        continued = self.submit(member, "after-explicit-promotion", session.id)
        self.assertEqual(continued.status_code, 202, continued.content)
        new_run = AgentRun.objects.get(pk=continued.json()["agentRunId"])
        self.assertEqual((new_run.session_id, new_run.session.agent_id), (session.id, target.id))
        self.assertEqual(new_run.definition_version_id, shared["definitionVersionId"])
        self.assertEqual((new_run.agent_instructions, new_run.modelConfig_id, new_run.thinkingMode),
            (shared["instructions"], self.model.id, "high"))
        for run in old_runs:
            run.refresh_from_db()
            grant = run.authorization
            self.assertEqual((run.definition_version_id, run.agent_instructions, run.modelConfig_id,
                run.thinkingMode, grant.payload, grant.digest, grant.signature), original[run.pk])
            self.assertTrue(agent_run_binding_matches(grant.payload, run))
            verify_agent_run_authorization_signature(grant.payload, settings.AGENT_RUN_AUTHORIZATION_SIGNING_KEY, grant.signature)

    def test_legacy_configuration_requires_explicit_promotion(self):
        response = self.client.get(self.base + "/default-agent")
        self.assertEqual(response.status_code, 409, response.content)
        response = self.submit({"id": self.legacy.id}, "unpromoted", modelConfigRef=self.model.id)
        self.assertEqual(response.status_code, 409, response.content)
        self.assertEqual(response.json()["error"], "agent_configuration_promotion_required")
        self.assertFalse(AgentRun.objects.exists())

    def test_members_cannot_create_promote_edit_model_or_profile(self):
        self.promote()
        member = self.member_instance()
        for method, url, body in (
            ("post", self.base + "/agents", {"name": "Override"}),
            ("patch", f"/api/agents/{member['id']}", {"instructions": "Override"}),
            ("patch", f"/api/agents/{member['id']}/model-settings", {"schema": "agent.model_settings.update.v1", "modelConfigRef": self.model.id, "thinkingMode": "low"}),
        ):
            response = self.send(method, url, body)
            self.assertIn(response.status_code, [403, 404], response.content)
        personal = Agent.objects.create(workspace=self.workspace, owner=self.member, name="Legacy")
        response = self.send("post", f"/api/agents/{personal.id}/promote")
        self.assertEqual(response.status_code, 403, response.content)

    def test_shared_profile_model_avatar_and_plugins_publish_to_all_members(self):
        shared = self.promote()
        member = self.member_instance()
        self.client.force_login(self.admin)
        image = io.BytesIO()
        Image.new("RGB", (4, 4), "red").save(image, format="PNG")
        response = self.send("patch", f"/api/agents/{shared['id']}", {
            "name": "Updated shared name", "description": "Workspace policy", "instructions": "New instructions",
            "avatarDataBase64": base64.b64encode(image.getvalue()).decode(), "pluginNames": [],
        })
        self.assertEqual(response.status_code, 200, response.content)
        second = ModelConfig.objects.create(displayName="Second", thinkingMode="low", thinkingModes=["low", "high"])
        response = self.send("patch", f"/api/agents/{shared['id']}/model-settings", {
            "schema": "agent.model_settings.update.v1", "modelConfigRef": second.id, "thinkingMode": "low"})
        self.assertEqual(response.status_code, 200, response.content)
        self.client.force_login(self.member)
        observed = self.client.get(f"/api/agents/{member['id']}").json()["agent"]
        self.assertEqual(observed["name"], "Updated shared name")
        self.assertEqual(observed["pluginNames"], [])
        self.assertEqual(self.client.get(observed["avatarUrl"]).status_code, 200)
        self.assertEqual(self.client.get(f"/api/agents/{member['id']}/model-settings").json()["modelConfigRef"], second.id)
        self.assertNotEqual(observed["definitionVersionId"], shared["definitionVersionId"])

    def test_run_uses_selected_model_and_shared_version_and_existing_run_remains_frozen(self):
        from copy import deepcopy
        from django.conf import settings
        from .runtime_contract import agent_run_binding_matches, verify_agent_run_authorization_signature

        shared = self.promote()
        member = self.member_instance()
        second = ModelConfig.objects.create(displayName="Second", thinkingMode="low", thinkingModes=["low", "high"])
        first = self.submit(member, "first", modelConfigRef=second.id, thinkingMode="low")
        self.assertEqual(first.status_code, 202, first.content)
        run = AgentRun.objects.get(id=first.json()["agentRunId"])
        self.assertEqual((run.definition_version_id, run.modelConfig_id, run.thinkingMode, run.agent_instructions),
            (shared["definitionVersionId"], second.id, "low", shared["instructions"]))
        before = (deepcopy(run.authorization.payload), run.authorization.digest, run.authorization.signature)
        self.assertEqual((before[0]["modelConfigRef"], before[0]["thinkingMode"]), (second.id, "low"))
        self.assertTrue(agent_run_binding_matches(before[0], run))
        verify_agent_run_authorization_signature(before[0], settings.AGENT_RUN_AUTHORIZATION_SIGNING_KEY, before[2])
        AgentRun.objects.filter(pk=run.pk).update(status="completed")
        self.client.force_login(self.admin)
        edited = self.send("patch", f"/api/agents/{shared['id']}", {"instructions": "Updated shared instructions"})
        self.assertEqual(edited.status_code, 200, edited.content)
        self.client.force_login(self.member)
        accepted = self.submit(member, "second", first.json()["sessionId"], modelConfigRef=self.model.id, thinkingMode="low")
        self.assertEqual(accepted.status_code, 202, accepted.content)
        latest = AgentRun.objects.get(id=accepted.json()["agentRunId"])
        self.assertEqual((latest.session_id, latest.modelConfig_id, latest.thinkingMode), (run.session_id, self.model.id, "low"))
        self.assertEqual(latest.agent_instructions, "Updated shared instructions")
        self.assertNotEqual(latest.definition_version_id, run.definition_version_id)
        self.assertTrue(agent_run_binding_matches(latest.authorization.payload, latest))
        self.client.force_login(self.admin)
        owner = self.submit(shared, "owner-choice", modelConfigRef=second.id, thinkingMode="high")
        self.assertEqual(owner.status_code, 202, owner.content)
        owner_run = AgentRun.objects.get(id=owner.json()["agentRunId"])
        self.assertNotEqual(owner_run.session_id, run.session_id)
        self.assertEqual((owner_run.modelConfig_id, owner_run.thinkingMode), (second.id, "high"))
        run.refresh_from_db()
        self.assertEqual((run.modelConfig_id, run.thinkingMode), (second.id, "low"))
        self.assertEqual((run.authorization.payload, run.authorization.digest, run.authorization.signature), before)
        self.assertEqual(AgentDefinitionVersion.objects.get(id=run.definition_version_id).model_config_id, self.model.id)
        self.assertTrue(agent_run_binding_matches(run.authorization.payload, run))
        run.modelConfig = self.model
        with self.assertRaisesMessage(ValueError, "configuration snapshot is immutable"):
            run.save()

    def test_selected_model_does_not_require_a_configured_shared_default(self):
        shared = self.promote()
        cleared = self.send("patch", f"/api/agents/{shared['id']}/model-settings", {
            "schema": "agent.model_settings.update.v1", "modelConfigRef": None, "thinkingMode": None})
        self.assertEqual(cleared.status_code, 200, cleared.content)
        member = self.member_instance()
        accepted = self.submit(member, "explicit-without-default", modelConfigRef=self.model.id, thinkingMode="low")
        self.assertEqual(accepted.status_code, 202, accepted.content)
        run = AgentRun.objects.get(pk=accepted.json()["agentRunId"])
        self.assertEqual((run.modelConfig_id, run.thinkingMode), (self.model.id, "low"))
        self.assertIsNone(run.definition_version.model_config_id)

    def test_selected_model_rejects_unavailable_revisions_foreign_scope_and_unsupported_effort(self):
        from .models import ModelProvider
        self.promote()
        member = self.member_instance()
        other_workspace = Workspace.objects.create(name="Other model scope", createdBy=self.admin)
        provider = ModelProvider.objects.create(workspace=other_workspace, displayName="Private provider",
            api="openai-responses", apiBase="https://provider.example.test/v1")
        unavailable = [
            ModelConfig.objects.create(displayName="Disabled", enabled=False),
            ModelConfig.objects.create(displayName="Superseded", isCurrent=False),
            ModelConfig.objects.create(displayName="Foreign", provider=provider),
        ]
        for index, model in enumerate(unavailable):
            with self.subTest(model=model.displayName):
                denied = self.submit(member, f"unavailable-{index}", modelConfigRef=model.pk)
                self.assertEqual(denied.status_code, 409, denied.content)
                self.assertEqual(denied.json()["error"], "agent_model_not_available")
        denied = self.submit(member, "unsupported-effort", modelConfigRef=self.model.id, thinkingMode="medium")
        self.assertEqual(denied.status_code, 409, denied.content)
        self.assertEqual(denied.json()["error"], "model_thinking_mode_unsupported")
        self.assertFalse(AgentRun.objects.exists())

    def test_directory_exposes_same_shared_definition_with_distinct_member_identities(self):
        shared = self.promote()
        self.client.force_login(self.member)
        response = self.client.get(self.base + "/agents")
        self.assertEqual(response.status_code, 200, response.content)
        observed = response.json()["agents"][0]
        self.assertEqual(observed["definitionId"], shared["definitionId"])
        self.assertNotEqual(observed["id"], shared["id"])

    def test_new_agents_are_shared_and_member_defaults_do_not_copy_personal_models(self):
        original = self.promote()
        response = self.send("post", self.base + "/agents", {"name": "Second shared", "instructions": "Shared"})
        self.assertEqual(response.status_code, 201, response.content)
        self.assertIsNotNone(response.json()["agent"]["definitionId"])
        member = self.member_instance()
        self.assertEqual(member["definitionId"], original["definitionId"])

    def test_unbound_native_model_policy_requires_promotion(self):
        from .agent_model_settings import AgentModelSettingsError, configured_agent_model
        with self.assertRaisesMessage(AgentModelSettingsError, "agent_configuration_promotion_required"):
            configured_agent_model(self.legacy)

    def test_invitation_promotion_requirement_rolls_back_new_membership(self):
        from urllib.parse import parse_qs, urlparse
        invited = User.objects.create_user(username="invited@example.test", email="invited@example.test")
        issued = self.send("post", self.base + "/invitations", {"email": invited.email, "role": "member"})
        self.assertEqual(issued.status_code, 201, issued.content)
        token = parse_qs(urlparse(issued.json()["inviteUrl"]).fragment)["token"][0]
        self.client.force_login(invited)
        response = self.send("post", "/api/invitations/accept", {"token": token})
        self.assertEqual(response.status_code, 409, response.content)
        self.assertEqual(response.json()["error"], "agent_configuration_promotion_required")
        self.assertFalse(WorkspaceMembership.objects.filter(workspace=self.workspace, user=invited).exists())
        self.workspace.invitations.get().refresh_from_db()
        self.assertEqual(self.workspace.invitations.get().status, "pending")

    def test_shared_agent_lifecycle_cannot_delete_private_instance_or_member_history(self):
        shared = self.promote()
        member = self.member_instance()
        session = Session.objects.create(workspace=self.workspace, owner=self.member, agent_id=member["id"])
        for user, agent, status in ((self.member, member, 403),):
            self.client.force_login(user)
            for method, suffix in (("delete", ""), ("post", "/restore"), ("delete", "/trash")):
                response = self.send(method, f"/api/agents/{agent['id']}" + suffix)
                self.assertEqual(response.status_code, status, response.content)
        self.client.force_login(self.admin)
        for method, suffix in (("post", "/restore"), ("delete", "/trash")):
            self.assertEqual(self.send(method, f"/api/agents/{shared['id']}" + suffix).status_code, 409)
        self.assertEqual(self.send("delete", f"/api/agents/{shared['id']}").status_code, 200)
        session.refresh_from_db()
        self.assertEqual(session.status, "active")
        self.assertEqual(Agent.objects.filter(id__in=[shared["id"], member["id"]], status="active").count(), 2)

    def test_retired_profile_resolves_default_model_without_replacing_session_identity(self):
        from .agent_defaults import replace_retired_default
        from .agent_definitions import published_version_for_session
        shared = self.promote()
        self.legacy.refresh_from_db()
        session = Session.objects.create(workspace=self.workspace, owner=self.admin, agent=self.legacy)
        definition = self.legacy.definition
        definition.status = "disabled"
        definition.save(update_fields=["status"])
        self.workspace.refresh_from_db()
        replace_retired_default(self.workspace, self.admin, definition)
        membership = WorkspaceMembership.objects.select_related("workspace").get(workspace=self.workspace, user=self.admin)
        session.refresh_from_db()
        version = published_version_for_session(session, membership)
        self.assertEqual((version.name, version.instructions, version.avatar_digest), ("Centaeris", "", ""))
        self.assertEqual(version.plugin_activation["packages"], [])
        self.assertEqual((version.model_config_id, version.thinking_mode), (self.model.pk, "high"))
        self.assertNotEqual(version.definition_id, shared["definitionId"])
        self.assertEqual(session.agent_id, self.legacy.id)

    def test_retired_shared_profile_session_continues_with_default_frozen_policy(self):
        from .testing import fenced_start_fixture
        shared = self.promote()
        member = self.member_instance()
        first = self.submit(member, "before-retirement")
        self.assertEqual(first.status_code, 202, first.content)
        old = AgentRun.objects.get(id=first.json()["agentRunId"])
        original = (old.definition_version_id, old.agent_instructions, old.authorization.payload, old.authorization.digest, old.authorization.signature)
        AgentRun.objects.filter(pk=old.pk).update(status="completed")
        self.client.force_login(self.admin)
        deleted = self.send("delete", f"/api/agents/{shared['id']}")
        self.assertEqual(deleted.status_code, 200, deleted.content)
        self.workspace.refresh_from_db()
        default = self.workspace.default_agent_definition.published_version
        self.assertNotEqual(default.definition_id, shared["definitionId"])
        self.client.force_login(self.member)
        continued = self.submit(member, "after-retirement", first.json()["sessionId"])
        self.assertEqual(continued.status_code, 202, continued.content)
        new = AgentRun.objects.get(id=continued.json()["agentRunId"])
        self.assertEqual(new.session.agent_id, member["id"])
        self.assertEqual(new.definition_version_id, default.pk)
        self.assertEqual((new.modelConfig_id, new.thinkingMode, new.agent_instructions), (self.model.pk, "high", ""))
        self.assertEqual(new.authorization.payload["agentId"], member["id"])
        old.refresh_from_db()
        self.assertEqual((old.definition_version_id, old.agent_instructions, old.authorization.payload, old.authorization.digest, old.authorization.signature), original)
        self.assertEqual(fenced_start_fixture(new)["authorization"]["agentId"], member["id"])
        self.client.force_login(self.admin)
        denied = self.submit(member, "foreign-history", first.json()["sessionId"])
        self.assertEqual(denied.status_code, 404, denied.content)

    def test_retired_personal_profile_session_reads_keep_owner_and_membership_authority(self):
        from types import SimpleNamespace
        from asgiref.sync import async_to_sync
        from django.test import RequestFactory
        from django.utils import timezone
        from .http.presentation import PresentationLoader
        from .models import SessionProject
        from .presentation_stream import PresentationScope
        own = Session.objects.create(workspace=self.workspace, owner=self.admin, agent=self.legacy)
        other_agent = Agent.objects.create(workspace=self.workspace, owner=self.admin, name="Other own profile")
        sibling = Session.objects.create(workspace=self.workspace, owner=self.admin, agent=other_agent)
        for agent in (self.legacy, other_agent):
            SessionProject.objects.create(workspace=self.workspace, owner=self.admin, agent=agent, name=agent.name)
        self.legacy.status, self.legacy.deletedAt, self.legacy.purgedAt = "deleted", timezone.now(), timezone.now()
        self.legacy.save(update_fields=["status", "deletedAt", "purgedAt"])
        response = self.client.get(f"/api/sessions/{own.pk}")
        self.assertEqual(response.status_code, 200, response.content)
        response = self.send("patch", f"/api/sessions/{own.pk}", {"title": "Retained own Session"})
        self.assertEqual(response.status_code, 200, response.content)
        sessions = self.client.get(self.base + "/sessions").json()["sessions"]
        self.assertEqual({row["id"] for row in sessions}, {own.pk, sibling.pk})
        projects = self.client.get(self.base + "/session-projects")
        self.assertEqual(projects.status_code, 200, projects.content)
        self.assertEqual(len(projects.json()["projects"]), 2)
        request = RequestFactory().get("/")
        request.user, request.app_delegation, request.session = self.admin, None, SimpleNamespace(session_key="fixture")
        loader = PresentationLoader(request, PresentationScope(self.workspace.id, self.legacy.id, own.id))
        self.assertTrue(async_to_sync(loader.authorized)())
        self.assertEqual({row["id"] for row in async_to_sync(loader.navigation)()}, {own.pk, sibling.pk})
        self.client.force_login(self.member)
        self.assertEqual(self.client.get(f"/api/sessions/{own.pk}").status_code, 404)
        self.assertEqual(self.client.get(self.base + "/sessions").json()["sessions"], [])
        WorkspaceMembership.objects.filter(workspace=self.workspace, user=self.admin).delete()
        self.client.force_login(self.admin)
        self.assertEqual(self.client.get(f"/api/sessions/{own.pk}").status_code, 404)
        self.assertFalse(async_to_sync(loader.authorized)())

    def test_default_lookup_shares_workspace_pin_with_running_sessions(self):
        from concurrent.futures import ThreadPoolExecutor
        from django.db import close_old_connections, connection, transaction
        from django.test import Client
        self.promote()
        def lookup():
            close_old_connections()
            try:
                client = Client()
                client.force_login(self.member)
                return client.get(self.base + "/default-agent")
            finally:
                close_old_connections()
        with ThreadPoolExecutor(max_workers=1) as pool:
            with transaction.atomic():
                with connection.cursor() as cursor:
                    cursor.execute("SELECT id FROM app_core_workspace WHERE id=%s FOR SHARE", [self.workspace.id])
                response = pool.submit(lookup).result(timeout=3)
                self.assertEqual(response.status_code, 200, response.content)

    def test_retired_profile_directory_uses_default_while_session_identity_survives(self):
        shared = self.promote()
        member = self.member_instance()
        session = Session.objects.create(workspace=self.workspace, owner=self.member, agent_id=member["id"])
        self.client.force_login(self.admin)
        self.assertEqual(self.send("delete", f"/api/agents/{shared['id']}").status_code, 200)
        self.client.force_login(self.member)
        directory = self.client.get(self.base + "/agents")
        self.assertEqual(directory.status_code, 200, directory.content)
        self.assertEqual(len(directory.json()["agents"]), 1)
        self.assertNotEqual(directory.json()["agents"][0]["definitionId"], shared["definitionId"])
        self.assertEqual(self.client.get(f"/api/agents/{member['id']}").status_code, 404)
        current = self.client.get(f"/api/sessions/{session.pk}")
        self.assertEqual(current.status_code, 200, current.content)
        self.assertEqual(current.json()["session"]["agentId"], member["id"])
