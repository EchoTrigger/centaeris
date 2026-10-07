"""Agent policy controls new coordinators without rewriting source execution proof."""
from copy import deepcopy
import json
from unittest.mock import patch

from django.test import TestCase, TransactionTestCase

from . import test_agent_messages, test_agent_work_consumption
from .models import Agent, AgentCoordinationSession, AgentRun, AgentRunAuthorization, HostedOperationReceipt, ModelConfig, Session, WorkspaceMembership
from .runtime_client import build_agent_run_start
from .test_agent_work import PROFILE


class AgentCoordinatorModelAuthorityTests(TransactionTestCase):
    serialized_rollback = True
    setUp = test_agent_work_consumption.AgentWorkConsumptionTests.setUp
    request_fact = test_agent_work_consumption.AgentWorkConsumptionTests.request_fact
    dependencies = test_agent_work_consumption.AgentWorkConsumptionTests.dependencies
    post = test_agent_work_consumption.AgentWorkConsumptionTests.post
    child = test_agent_work_consumption.AgentWorkConsumptionTests.child
    terminal = test_agent_work_consumption.AgentWorkConsumptionTests.terminal
    publish = test_agent_work_consumption.AgentWorkConsumptionTests.publish
    notice = test_agent_work_consumption.AgentWorkConsumptionTests.notice
    consume = test_agent_work_consumption.AgentWorkConsumptionTests.consume
    attempts = test_agent_work_consumption.AgentWorkConsumptionTests.attempts
    counts = test_agent_work_consumption.AgentWorkConsumptionTests.counts
    runtime = test_agent_work_consumption.AgentWorkConsumptionTests.runtime

    def policy_model(self, name):
        return ModelConfig.objects.create(displayName=name, thinkingMode="high", thinkingModes=["low", "high"])

    def configure(self, model, mode="low"):
        self.agent.model_config, self.agent.thinking_mode = model, mode if model is not None else ""
        self.agent.save(update_fields=["model_config", "thinking_mode"])

    def test_new_coordinator_uses_agent_policy_and_preserves_independent_source_authorization(self):
        notice = self.notice()
        selected = self.policy_model("Owner selected coordinator")
        self.configure(selected)
        source_proof = deepcopy(self.run.authorization.payload), self.run.authorization.digest, self.run.authorization.signature
        # The old execution proof remains valid even when its model is no longer
        # available for a new execution; the new coordinator uses the owner policy.
        self.model_config = self.run.modelConfig
        self.model_config.enabled = False
        self.model_config.isCurrent = False
        self.model_config.save(update_fields=["enabled", "isCurrent"])
        with self.runtime():
            response = self.consume(notice)
        self.assertEqual(response.status_code, 201, response.content)
        run = self.attempts().get().coordinator_run
        self.assertEqual((run.modelConfig_id, run.thinkingMode), (selected.pk, "low"))
        self.assertEqual((run.authorization.payload["modelConfigRef"], run.authorization.payload["thinkingMode"]),
            (selected.pk, "low"))
        self.run.refresh_from_db()
        self.run.authorization.refresh_from_db()
        self.assertEqual(self.run.modelConfig_id, self.model_config.pk)
        self.assertEqual((self.run.authorization.payload, self.run.authorization.digest, self.run.authorization.signature), source_proof)
        self.assertEqual(build_agent_run_start(run)["initialInput"], self.attempts().get().input_binding["initialInput"])

    def test_active_run_keeps_snapshot_when_agent_policy_changes_or_is_cleared(self):
        notice = self.notice(idle=False)
        selected = self.policy_model("Future coordinator")
        self.configure(selected)
        before = self.counts()
        with self.runtime() as (inputs, schedules):
            accepted = None
            for index, model in enumerate((selected, None)):
                self.configure(model)
                response = self.consume(notice)
                self.assertEqual(response.status_code, 201 if index == 0 else 200, response.content)
                self.assertEqual(response.json()["disposition"], "accepted")
                if accepted is None:
                    accepted = response.json()
                self.assertEqual(response.json(), accepted)
            self.assertEqual(len(inputs), 1)
            self.assertFalse(schedules)
        self.run.refresh_from_db()
        self.assertEqual((self.run.modelConfig_id, self.run.thinkingMode, self.run.status), (self.model.pk, "", "running"))
        self.assertEqual(self.counts(), (before[0], before[1], before[2] + 1))
        self.assertEqual(self.attempts().get().coordinator_run_id, self.run.pk)

    def test_policy_is_rechecked_at_committed_admission_after_profile_rpc(self):
        notice = self.notice()
        selected = self.policy_model("Policy after RPC")
        with self.runtime(before_profile=lambda: self.configure(selected, "high")):
            response = self.consume(notice)
        self.assertEqual(response.status_code, 201, response.content)
        run = self.attempts().get().coordinator_run
        self.assertEqual((run.modelConfig_id, run.thinkingMode), (selected.pk, "high"))

    def test_clear_during_profile_rpc_leaves_notice_unspent_without_partial_run(self):
        notice = self.notice()
        before = self.counts()
        with self.runtime(before_profile=lambda: self.configure(None)):
            response = self.consume(notice)
        self.assertEqual(response.status_code, 409, response.content)
        self.assertEqual(response.json(), {"error": "agent_model_not_configured"})
        self.assertEqual(self.counts(), before)
        self.assertFalse(self.attempts().exists())

    def test_unconfigured_or_unavailable_idle_policy_cannot_fall_back_to_source_model(self):
        notice = self.notice()
        before = self.counts()
        unavailable = self.policy_model("Unavailable policy")
        unavailable.enabled = False
        unavailable.save(update_fields=["enabled"])
        with self.runtime():
            for selected, code in ((None, "agent_model_not_configured"), (unavailable, "agent_model_not_available")):
                self.configure(selected)
                response = self.consume(notice)
                self.assertEqual(response.status_code, 409, response.content)
                self.assertEqual(response.json(), {"error": code})
                self.assertEqual(self.counts(), before)
                self.assertFalse(self.attempts().exists())

    def test_valid_new_policy_cannot_launder_source_signature_mode_or_membership_tampering(self):
        notice = self.notice()
        selected = self.policy_model("Valid new policy")
        self.configure(selected)
        before = self.counts()
        authorization = self.run.authorization
        with patch("urllib.request.urlopen", side_effect=AssertionError("invalid source rejects before RPC")):
            AgentRunAuthorization.objects.filter(pk=authorization.pk).update(signature="invalid-signature")
            self.assertEqual(self.consume(notice).status_code, 403)
            AgentRunAuthorization.objects.filter(pk=authorization.pk).update(signature=authorization.signature)
            AgentRun.objects.filter(pk=self.run.pk).update(thinkingMode="high")
            self.assertEqual(self.consume(notice).status_code, 403)
            AgentRun.objects.filter(pk=self.run.pk).update(thinkingMode="")
            AgentRun.objects.filter(pk=self.run.pk).update(modelConfig=selected)
            self.assertEqual(self.consume(notice).status_code, 403)
            AgentRun.objects.filter(pk=self.run.pk).update(modelConfig=self.model)
            self.membership.delete()
            WorkspaceMembership.objects.create(workspace=self.workspace, user=self.user, role="owner")
            self.assertEqual(self.consume(notice).status_code, 403)
        self.assertEqual(self.counts(), before)
        self.assertFalse(self.attempts().exists())

    def test_accepted_coordinator_replay_and_rehydration_ignore_later_agent_policy_change(self):
        notice = self.notice()
        selected, later = self.policy_model("Admitted policy"), self.policy_model("Later policy")
        self.configure(selected)
        with self.runtime():
            accepted = self.consume(notice)
        self.assertEqual(accepted.status_code, 201, accepted.content)
        attempt = self.attempts().get()
        run = attempt.coordinator_run
        initial = deepcopy(attempt.input_binding["initialInput"])
        proof = deepcopy(run.authorization.payload), run.authorization.digest, run.authorization.signature
        before = self.counts()
        with patch("urllib.request.urlopen", side_effect=AssertionError("replay has no RPC")):
            for model in (later, None):
                self.configure(model)
                replay = self.consume(notice)
                self.assertEqual(replay.status_code, 200, replay.content)
                self.assertEqual(replay.json(), accepted.json())
                self.assertEqual(build_agent_run_start(run)["initialInput"], initial)
        run.refresh_from_db()
        run.authorization.refresh_from_db()
        self.assertEqual((run.modelConfig_id, run.thinkingMode), (selected.pk, "low"))
        self.assertEqual((run.authorization.payload, run.authorization.digest, run.authorization.signature), proof)
        self.assertEqual(self.counts(), before)

    def test_coordinator_effort_cannot_change_without_its_own_authorization_snapshot(self):
        notice = self.notice()
        selected = self.policy_model("Admitted policy")
        self.configure(selected)
        with self.runtime():
            response = self.consume(notice)
        self.assertEqual(response.status_code, 201, response.content)
        run = self.attempts().get().coordinator_run
        before = self.counts()
        AgentRun.objects.filter(pk=run.pk).update(thinkingMode="high")
        with patch("urllib.request.urlopen", side_effect=AssertionError("tampered replay has no RPC")):
            self.assertEqual(self.consume(notice).status_code, 400)
        self.assertEqual(self.counts(), before)


class AgentCoordinationEntryTests(TestCase):
    setUp = test_agent_messages.AgentMessageTests.setUp
    bind = test_agent_messages.AgentMessageTests.bind

    def models(self):
        selected = ModelConfig.objects.create(displayName="Agent policy")
        other = ModelConfig.objects.create(displayName="Ordinary work choice")
        self.agent.model_config = selected
        self.agent.save(update_fields=["model_config"])
        return selected, other

    def post_message(self, session, model, operation="message-entry"):
        return self.client.post(f"/api/workspaces/{self.workspace.pk}/sessions/{session.pk}/messages",
            content_type="application/json", data=json.dumps({
                "operationId": operation, "text": "Exact generic message", "modelConfigRef": model.pk}))

    def test_generic_entry_cannot_start_or_fork_coordination_even_with_matching_model(self):
        session = self.bind()
        selected, other = self.models()
        for state in ("idle", "active"):
            if state == "active":
                AgentRun.objects.create(workspace=self.workspace, user=self.user, session=session,
                    modelConfig=selected, prompt="Existing coordinator", status="running")
            before = AgentRun.objects.count(), HostedOperationReceipt.objects.count()
            with patch("app_core.http.workspaces.request_execution_profile", side_effect=AssertionError("reject before RPC")):
                for model in (selected, other):
                    response = self.post_message(session, model, "reserved-" + state + "-" + model.pk)
                    self.assertEqual(response.status_code, 409, response.content)
                    self.assertEqual(response.json(), {"error": "coordination_session_requires_agent_input"})
            self.assertEqual((AgentRun.objects.count(), HostedOperationReceipt.objects.count()), before)

    def test_binding_created_during_profile_rpc_is_rechecked_before_run_or_receipt(self):
        selected, other = self.models()
        agent = Agent.objects.create(workspace=self.workspace, owner=self.user, name="Race binding", model_config=selected)
        session = Session.objects.create(workspace=self.workspace, owner=self.user, agent=agent)
        before = AgentRun.objects.count(), HostedOperationReceipt.objects.count()
        def bind_during_rpc():
            AgentCoordinationSession.objects.create(agent=agent, session=session)
            return PROFILE
        with patch("app_core.http.workspaces.request_execution_profile", side_effect=bind_during_rpc):
            response = self.post_message(session, other)
        self.assertEqual(response.status_code, 409, response.content)
        self.assertEqual(response.json(), {"error": "coordination_session_requires_agent_input"})
        self.assertEqual((AgentRun.objects.count(), HostedOperationReceipt.objects.count()), before)

    def test_ordinary_work_session_keeps_explicit_model_selection_on_same_agent(self):
        self.bind()
        selected, other = self.models()
        with patch("app_core.http.workspaces.request_execution_profile", return_value=PROFILE), \
             patch("app_core.http.workspaces.schedule_agent_run_lifecycle"):
            response = self.post_message(self.work, other, "ordinary-work")
        self.assertEqual(response.status_code, 202, response.content)
        run = AgentRun.objects.get(pk=response.json()["agentRunId"])
        self.assertEqual(run.modelConfig_id, other.pk)
        self.assertNotEqual(run.modelConfig_id, selected.pk)
