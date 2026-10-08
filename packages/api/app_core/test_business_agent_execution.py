"""Business branches retain native execution with an isolated persistent tree."""
import json
import hashlib
import tempfile
from unittest.mock import patch

from django.core.files.base import ContentFile
from django.core.files.storage import default_storage
from django.test import TransactionTestCase, override_settings

from . import models, test_agent_messages, test_agent_work, test_agent_work_consumption
from .agent_input_delivery import dispatch_agent_inputs
from .agent_inputs import accept_agent_input
from .agent_run_authorization_factory import create_agent_run_authorization
from .assets import captured_input_fields


class BusinessAgentExecutionTests(TransactionTestCase):
    serialized_rollback = True
    request_fact = test_agent_work.AgentWorkTests.request_fact
    post = test_agent_work.AgentWorkTests.post
    dependencies = test_agent_work.AgentWorkTests.dependencies
    validate_message = test_agent_messages.AgentMessageTests.validate
    child = test_agent_work_consumption.AgentWorkConsumptionTests.child
    terminal = test_agent_work_consumption.AgentWorkConsumptionTests.terminal
    publish = test_agent_work_consumption.AgentWorkConsumptionTests.publish
    notice = test_agent_work_consumption.AgentWorkConsumptionTests.notice
    consume = test_agent_work_consumption.AgentWorkConsumptionTests.consume
    runtime = test_agent_work_consumption.AgentWorkConsumptionTests.runtime

    def setUp(self):
        test_agent_work.AgentWorkTests.setUp(self)
        self.root, self.root_session, self.root_run = self.agent, self.source, self.run
        self.root.model_config = self.model
        self.root.instructions = "Original root instruction snapshot."
        self.root.save(update_fields=["model_config", "instructions"])
        self.application = models.BusinessApplication.objects.create(
            name="Synthetic branch business", created_by=self.user, status="active")
        self.branch = self.new_branch("external-user-one")
        self.agent, self.source = self.branch.agent, self.branch.session
        self.run = models.AgentRun.objects.create(workspace=self.workspace, session=self.source,
            user=self.user, modelConfig=self.model, prompt="Dispatch work", status="running",
            agent_instructions=self.root.instructions)
        create_agent_run_authorization(self.run, image_digest=test_agent_work.PROFILE["imageDigest"])
        self.work = self.source
        self.client.force_login(self.user)
        guard = patch("urllib.request.urlopen",
            side_effect=AssertionError("Synthetic tests require an explicit Runtime RPC fixture"))
        guard.start()
        self.addCleanup(guard.stop)

    def new_branch(self, subject):
        agent = models.Agent.objects.create(is_business_instance=True, workspace=self.workspace, owner=self.user,
            name=self.root.name, instructions=self.root.instructions, model_config=self.model)
        session = models.Session.objects.create(workspace=self.workspace, owner=self.user,
            agent=agent, origin="automation")
        models.AgentCoordinationSession.objects.create(agent=agent, session=session)
        return models.BusinessAgentBranch.objects.create(app=self.application,
            root_agent=self.root, business_user_id=subject, agent=agent, session=session)

    def validate_work(self, refs, **changes):
        from .test_agent_work_validation import AgentWorkValidationTests
        return AgentWorkValidationTests.validate(self, self.run, sessionRefs=refs, **changes)

    def test_branch_tools_reject_root_sibling_and_unbound_same_agent_sessions(self):
        sibling = self.new_branch("external-user-two")
        ordinary = models.Session.objects.create(workspace=self.workspace, owner=self.user, agent=self.agent)
        for forbidden in (self.root_session, sibling.session, ordinary):
            with self.subTest(session=forbidden.pk, tool="send_message"):
                self.assertEqual(self.validate_message(self.run, sessionRefs=[forbidden.pk]).status_code, 403)
            with self.subTest(session=forbidden.pk, tool="dispatch_work"):
                self.assertEqual(self.validate_work([forbidden.pk]).status_code, 403)
        self.assertEqual(self.validate_message(self.run, sessionRefs=[self.source.pk]).status_code, 200)
        self.assertEqual(self.validate_work([self.source.pk]).status_code, 200)

    def test_non_business_owner_native_tools_keep_existing_cross_session_authority(self):
        self.assertEqual(self.validate_message(self.root_run, sessionRefs=[self.source.pk]).status_code, 200)
        original = self.run
        self.run = self.root_run
        self.assertEqual(self.validate_work([self.source.pk]).status_code, 200)
        self.run = original

    def test_non_business_private_child_keeps_existing_current_agent_instruction_policy(self):
        self.agent, self.source, self.run = self.root, self.root_session, self.root_run
        models.Agent.objects.filter(pk=self.root.pk).update(instructions="Current ordinary private policy.")
        self.dependencies()
        response = self.post(self.request_fact())
        self.assertEqual(response.status_code, 201, response.content)
        child = models.AgentRun.objects.get(pk=response.json()["operation"]["agentRunId"])
        self.assertEqual(child.agent_instructions, "Current ordinary private policy.")

    def test_materializer_rechecks_committed_session_refs_before_creating_child(self):
        self.dependencies()
        self.args["session_refs"] = [self.root_session.pk]
        before = models.AgentWorkSession.objects.count(), models.Session.objects.count(), models.AgentRun.objects.count()
        response = self.post(self.request_fact())
        self.assertEqual(response.status_code, 403, response.content)
        self.assertEqual((models.AgentWorkSession.objects.count(), models.Session.objects.count(),
            models.AgentRun.objects.count()), before)

    def test_file_refs_retain_signed_current_session_scope_across_the_business_tree(self):
        sibling = self.new_branch("file-external-user")
        with tempfile.TemporaryDirectory(prefix="business-file-scope-") as storage, override_settings(MEDIA_ROOT=storage):
            content = b"Synthetic branch fixture"
            key = default_storage.save("fixture.txt", ContentFile(content))
            item = models.UserLibraryObject.objects.create(owner=self.user, objectKind="file",
                displayName="fixture.txt", contentType="text/plain", storageKey=key, status="ready",
                contentGeneration=1, sizeBytes=len(content), sha256="sha256:" + hashlib.sha256(content).hexdigest())
            links = [models.SessionAssetLink.objects.create(workspace=self.workspace, session=session,
                userLibraryObject=item, attachedBy=self.user, capturedDisplayName=item.displayName,
                capturedContentType=item.contentType, **captured_input_fields(item))
                for session in (self.root_session, sibling.session, self.source)]
            self.run = models.AgentRun.objects.create(workspace=self.workspace, session=self.source,
                user=self.user, modelConfig=self.model, prompt="Files", status="running",
                agent_instructions=self.root.instructions)
            create_agent_run_authorization(self.run, image_digest=test_agent_work.PROFILE["imageDigest"])
            for forbidden in links[:2]:
                self.assertEqual(self.validate_message(self.run, fileRefs=[forbidden.pk]).status_code, 403)
                self.assertEqual(self.validate_work([], fileRefs=[forbidden.pk]).status_code, 403)
            self.assertEqual(self.validate_message(self.run, fileRefs=[links[2].pk]).status_code, 200)
            self.assertEqual(self.validate_work([], fileRefs=[links[2].pk]).status_code, 200)
            self.dependencies()
            self.args["file_refs"] = [links[0].pk]
            before = models.AgentWorkSession.objects.count()
            self.assertEqual(self.post(self.request_fact()).status_code, 403)
            self.assertEqual(models.AgentWorkSession.objects.count(), before)

    def test_child_keeps_source_instruction_snapshot_and_is_the_only_admitted_work_reference(self):
        self.dependencies()
        models.Agent.objects.filter(pk=self.root.pk).update(instructions="Later root policy.")
        models.Agent.objects.filter(pk=self.agent.pk).update(instructions="Mutable branch data must not be inherited.")
        response = self.post(self.request_fact())
        self.assertEqual(response.status_code, 201, response.content)
        child = models.AgentRun.objects.get(pk=response.json()["operation"]["agentRunId"])
        self.assertEqual(child.agent_instructions, self.run.agent_instructions)
        self.assertEqual(child.session.agent_id, self.agent.pk)
        self.assertEqual(self.validate_message(self.run, sessionRefs=[child.session_id]).status_code, 200)
        self.assertEqual(self.validate_work([child.session_id]).status_code, 200)
        models.AgentWorkSession.objects.filter(session_id=child.session_id).delete()
        self.assertEqual(self.validate_message(self.run, sessionRefs=[child.session_id]).status_code, 403)
        self.assertEqual(self.validate_work([child.session_id]).status_code, 403)

    def test_new_input_run_reads_current_root_policy_and_busy_input_keeps_authorized_snapshot(self):
        models.AgentRun.objects.filter(pk=self.run.pk).update(status="completed")
        current = models.ModelConfig.objects.create(displayName="New root policy",
            thinkingMode="high", thinkingModes=["low", "high"])
        models.Agent.objects.filter(pk=self.root.pk).update(model_config=current, thinking_mode="low",
            instructions="Current root instruction.")
        with patch("app_core.agent_inputs._dispatch_after_commit"):
            accept_agent_input(self.user, self.agent.pk, "first-branch-input", "Retained exact body")
        with patch("app_core.runtime_client.request_execution_profile", return_value=test_agent_work.PROFILE), \
                patch("app_core.runtime_client.schedule_agent_run_lifecycle", return_value="inserted"):
            admitted = dispatch_agent_inputs(self.agent.pk)
            self.assertEqual((admitted.modelConfig_id, admitted.thinkingMode, admitted.agent_instructions),
                (current.pk, "low", "Current root instruction."))
            proof = admitted.authorization.digest, admitted.authorization.payload
            models.Agent.objects.filter(pk=self.root.pk).update(model_config=None,
                thinking_mode="", instructions="Future root instruction.")
            with patch("app_core.agent_inputs._dispatch_after_commit"):
                accept_agent_input(self.user, self.agent.pk, "second-branch-input", "Later body")
            self.assertEqual(dispatch_agent_inputs(self.agent.pk).pk, admitted.pk)
        admitted.refresh_from_db()
        self.assertEqual((admitted.authorization.digest, admitted.authorization.payload), proof)
        self.assertEqual(admitted.agent_instructions, "Current root instruction.")

    def test_idle_work_return_coordinator_snapshots_current_root_policy(self):
        notice = self.notice()
        current = models.ModelConfig.objects.create(displayName="Work return root policy")
        models.Agent.objects.filter(pk=self.root.pk).update(model_config=current,
            instructions="Current root instructions for return.")
        with self.runtime():
            response = self.consume(notice)
        self.assertEqual(response.status_code, 201, response.content)
        attempt = models.AgentWorkConsumeAttempt.objects.get()
        self.assertEqual((attempt.coordinator_run.modelConfig_id, attempt.coordinator_run.agent_instructions),
            (current.pk, "Current root instructions for return."))

    def test_owner_configuration_is_root_only_and_owner_list_hides_business_branches(self):
        self.assertEqual(self.client.patch(f"/api/agents/{self.agent.pk}",
            data=json.dumps({"instructions": "Forbidden branch override"}), content_type="application/json").status_code, 409)
        self.assertEqual(self.client.patch(f"/api/agents/{self.agent.pk}/model-settings",
            data=json.dumps({"schema": "agent.model_settings.update.v1", "modelConfigRef": None,
                "thinkingMode": None}), content_type="application/json").status_code, 409)
        response = self.client.get(f"/api/workspaces/{self.workspace.pk}/agents")
        self.assertEqual(response.status_code, 200, response.content)
        self.assertEqual([agent["id"] for agent in response.json()["agents"]], [self.root.pk])
        response = self.client.patch(f"/api/agents/{self.root.pk}",
            data=json.dumps({"name": "Root maintained name", "description": "Root maintained description",
                "instructions": "Root maintained policy", "avatarKind": "banana"}), content_type="application/json")
        self.assertEqual(response.status_code, 200, response.content)
        response = self.client.get(f"/api/agents/{self.agent.pk}")
        self.assertEqual(response.status_code, 200, response.content)
        record = response.json()["agent"]
        self.assertEqual((record["id"], record["name"], record["description"], record["instructions"], record["avatarKind"]),
            (self.agent.pk, "Root maintained name", "Root maintained description", "Root maintained policy", "banana"))

    def test_unavailable_root_never_falls_back_to_stale_branch_configuration(self):
        from django.utils import timezone
        models.Agent.objects.filter(pk=self.root.pk).update(status="deleted", deletedAt=timezone.now())
        response = self.client.get(f"/api/agents/{self.agent.pk}")
        self.assertEqual(response.status_code, 409, response.content)
        self.assertEqual(response.json()["error"], "agent_business_root_unavailable")

    def test_root_cannot_be_deleted_while_a_business_branch_has_active_work(self):
        models.AgentRun.objects.filter(pk=self.root_run.pk).update(status="completed")
        response = self.client.delete(f"/api/agents/{self.root.pk}")
        self.assertEqual(response.status_code, 409, response.content)
        self.root.refresh_from_db()
        self.assertEqual(self.root.status, "active")
