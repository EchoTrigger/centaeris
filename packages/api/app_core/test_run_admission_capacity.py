"""Bound unfinished Run admission while preserving immutable operation replay."""
import json
import time
from concurrent.futures import ThreadPoolExecutor
from threading import Barrier
from django.conf import settings
from django.db import close_old_connections, connection
from django.test import Client, TransactionTestCase, override_settings
from django.utils import timezone
from . import models, test_agent_work_consumption, test_hosted_operations


@override_settings(EXECUTION_GLOBAL_QUEUE_LIMIT=1, EXECUTION_TENANT_QUEUE_LIMIT=4)
class ApiRunQueueCapacityTests(test_hosted_operations.OperationFixture, TransactionTestCase):
    serialized_rollback = True

    def test_runtime_capacity_wait_keeps_nonterminal_run_charged_and_replay_available(self):
        first = self.post()
        self.assertEqual(first.status_code, 202, first.content)
        run_id = first.json()["agentRunId"]
        transition = self.client.post("/internal/agent-runs/transition", content_type="application/json",
            HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN,
            data=json.dumps({"schema": "runtime.agent_run.transition.v1", "agentRunId": run_id,
                "state": "running", "transitionReason": "agent_run_lifecycle_step_started"}))
        self.assertEqual(transition.status_code, 200, transition.content)
        run = models.AgentRun.objects.get(pk=run_id)
        self.assertIsNotNone(run.startedAt)
        rejected = self.post(operationId="new-while-capacity-waits")
        self.assertEqual(rejected.status_code, 503, rejected.content)
        self.assertEqual(rejected.json(), {"error": "execution_queue_full"})
        self.assertEqual(models.AgentRun.objects.count(), 1)
        self.assertEqual(models.HostedOperationReceipt.objects.filter(command="submitMessage").count(), 1)
        run.refresh_from_db()
        self.assertEqual(run.status, "running")
        self.assertIsNone(run.completedAt)
        self.profile.side_effect = AssertionError("accepted replay must not contact Runtime")
        replay = self.post()
        self.assertEqual(replay.status_code, 202, replay.content)
        self.assertEqual(replay.json(), first.json())

    def test_terminal_history_releases_capacity_without_failed_replacement_run(self):
        first = self.post()
        self.assertEqual(first.status_code, 202, first.content)
        models.AgentRun.objects.filter(pk=first.json()["agentRunId"]).update(status="cancelled", completedAt=timezone.now())
        second = self.post(operationId="after-terminal")
        self.assertEqual(second.status_code, 202, second.content)
        self.assertEqual(list(models.AgentRun.objects.values_list("status", flat=True)).count("queued"), 1)
        self.assertEqual(models.AgentRun.objects.get(pk=first.json()["agentRunId"]).status, "cancelled")
        self.assertEqual(models.AgentRun.objects.count(), 2)
        self.assertFalse(models.AgentRun.objects.filter(status="failed").exists())

    @override_settings(EXECUTION_GLOBAL_QUEUE_LIMIT=4, EXECUTION_TENANT_QUEUE_LIMIT=1)
    def test_running_run_keeps_workspace_slot_without_blocking_another_workspace(self):
        first = self.post()
        self.assertEqual(first.status_code, 202, first.content)
        models.AgentRun.objects.filter(pk=first.json()["agentRunId"]).update(status="running", startedAt=timezone.now())
        rejected = self.post(operationId="same-workspace-full")
        self.assertEqual(rejected.status_code, 429, rejected.content)
        self.assertEqual(rejected.json(), {"error": "workspace_execution_queue_full"})
        other_workspace = models.Workspace.objects.create(name="Separate admission", createdBy=self.user)
        models.WorkspaceMembership.objects.create(workspace=other_workspace, user=self.user, role="owner")
        other_agent = models.Agent.objects.create(workspace=other_workspace, owner=self.user, name="Separate agent")
        accepted = self.client.post(f"/api/workspaces/{other_workspace.pk}/sessions/new/messages",
            content_type="application/json", data=json.dumps({"operationId": "other-workspace",
                "agentId": other_agent.pk, "text": "independent workspace slot", "modelConfigRef": self.model.pk}))
        self.assertEqual(accepted.status_code, 202, accepted.content)
        self.assertEqual(models.AgentRun.objects.count(), 2)
        self.assertEqual(models.HostedOperationReceipt.objects.filter(command="submitMessage").count(), 2)

    @override_settings(EXECUTION_GLOBAL_QUEUE_LIMIT=2)
    def test_cross_workspace_run_admissions_cannot_pass_one_remaining_slot(self):
        first = self.post()
        self.assertEqual(first.status_code, 202, first.content)
        models.AgentRun.objects.filter(pk=first.json()["agentRunId"]).update(status="running", startedAt=timezone.now())
        other_workspace = models.Workspace.objects.create(name="Other Run queue", createdBy=self.user)
        models.WorkspaceMembership.objects.create(workspace=other_workspace, user=self.user, role="owner")
        other_agent = models.Agent.objects.create(workspace=other_workspace, owner=self.user, name="Other Run")
        barrier = Barrier(2)

        def submit(target):
            workspace, agent = target
            try:
                client = Client()
                client.force_login(self.user)
                barrier.wait(timeout=5)
                deadline = time.monotonic() + 5
                body = json.dumps({"operationId": "next-run", "agentId": agent.pk,
                                   "text": "one remaining slot", "modelConfigRef": self.model.pk})
                while True:
                    response = client.post(f"/api/workspaces/{workspace.pk}/sessions/new/messages",
                        content_type="application/json", data=body)
                    result = response.json()
                    if (response.status_code != 503 or result != {"error": "execution_admission_busy"}
                            or time.monotonic() >= deadline):
                        return response.status_code, result
                    # Lock contention is not evidence that the remaining slot
                    # was enforced. Retry the exact identity until capacity is
                    # observed or the bounded contention allowance expires.
                    time.sleep(0.02)
            finally:
                close_old_connections()

        with ThreadPoolExecutor(max_workers=2) as pool:
            results = list(pool.map(submit, [(self.workspace, self.agent), (other_workspace, other_agent)]))
        self.assertEqual(sorted(status for status, _ in results), [202, 503], results)
        self.assertEqual([body for status, body in results if status == 503],
                         [{"error": "execution_queue_full"}], results)
        self.assertEqual(models.AgentRun.objects.count(), 2)
        self.assertEqual(models.HostedOperationReceipt.objects.filter(command="submitMessage").count(), 2)


class ApiWorkReturnQueueCapacityTests(TransactionTestCase):
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
    runtime = test_agent_work_consumption.AgentWorkConsumptionTests.runtime

    def test_full_run_queue_retains_work_return_receipt_then_binds_same_attempt(self):
        notice = self.notice()
        filler = models.AgentRun.objects.create(workspace=self.workspace, user=self.user,
            session=models.Session.objects.create(workspace=self.workspace, owner=self.user, agent=self.agent),
            modelConfig=self.model, prompt="one existing resource obligation")
        before = models.AgentRun.objects.count()
        with override_settings(EXECUTION_GLOBAL_QUEUE_LIMIT=1, EXECUTION_TENANT_QUEUE_LIMIT=4), self.runtime() as (inputs, schedules):
            first = self.consume(notice)
            self.assertEqual(first.status_code, 201, first.content)
            attempt = models.AgentWorkConsumeAttempt.objects.get()
            receipt_id, attempt_id = attempt.operation_id, attempt.pk
            self.assertIsNone(attempt.coordinator_run_id)
            self.assertIsNone(first.json()["operation"]["agentRunId"])
            self.assertEqual(models.AgentRun.objects.count(), before)
            replay = self.consume(notice)
            self.assertEqual(replay.status_code, 200, replay.content)
            self.assertEqual(replay.json(), first.json())
            models.AgentRun.objects.filter(pk=filler.pk).update(status="cancelled", completedAt=timezone.now())
            bound = self.consume(notice)
            self.assertEqual(bound.status_code, 200, bound.content)
        attempt.refresh_from_db()
        self.assertEqual((attempt.pk, attempt.operation_id), (attempt_id, receipt_id))
        self.assertIsNotNone(attempt.coordinator_run_id)
        self.assertEqual(models.AgentWorkConsumeAttempt.objects.count(), 1)
        self.assertEqual(models.AgentRun.objects.count(), before + 1)
        self.assertEqual((len(inputs), len(schedules)), (1, 1))
