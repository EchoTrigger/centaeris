"""Real Harbor BaseAgent lifecycle tests with an isolated API dependency."""
import asyncio
import json
from pathlib import Path
import tempfile
import threading
from types import SimpleNamespace
import unittest
from unittest.mock import patch

try:
    from workspace_agent import WorkspaceAgent, WorkspaceBinding
    from harbor.models.agent.context import AgentContext
except ModuleNotFoundError as error:
    if error.name != "harbor":
        raise
    WorkspaceAgent = None

from workspace_client import SubmissionUncertain, WorkspaceError


class FakeClient:
    request_timeout = 0.1

    def __init__(self):
        self.calls = []
        self.statuses = ["completed"]
        self.failure = None

    def session(self, session_id):
        self.calls.append(("session", session_id))
        return {"session": {"id": session_id, "workspaceId": "workspace", "agentId": "agent",
                            "currentEnvironmentId": "environment", "status": "active"}}

    def run_environment(self, session_id, agent_run_id):
        self.calls.append(("environment", agent_run_id))
        return {"sessionId": session_id, "agentRunId": agent_run_id, "environmentId": "environment", "executionMode": "perRun"}

    def submit_message(self, *args):
        self.calls.append(("submit", args))
        return {"operationId": args[3], "command": "submitMessage", "status": "accepted",
                "sessionId": "session", "agentRunId": "run", "turnId": "turn"}

    def read_run_events(self, session_id, agent_run_id, cursor, *, after_sequence):
        self.calls.append(("events", cursor, after_sequence))
        if self.failure:
            raise self.failure
        status = self.statuses.pop(0)
        return {"cursor": f"cursor-{after_sequence + 1}", "sourceSequence": after_sequence + 1, "status": status}

    def cancel_run(self, session_id, agent_run_id):
        self.calls.append(("cancel", session_id, agent_run_id))
        return {"agentRunId": agent_run_id, "status": "running", "disposition": "requested"}

    def run_fact(self, session_id, agent_run_id):
        return {"agentRunId": agent_run_id, "status": "cancelled"}


@unittest.skipUnless(WorkspaceAgent, "requires Harbor 0.21.0")
class WorkspaceAgentTests(unittest.IsolatedAsyncioTestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.client = FakeClient()
        self.binding = WorkspaceBinding(workspace_id="workspace", agent_id="agent", session_id="session",
                                        environment_id="environment", instance_id="instance", trial_id="trial")
        self.environment = SimpleNamespace(workspace_client=self.client, workspace_binding=self.binding)
        self.agent = WorkspaceAgent(logs_dir=Path(self.temporary.name), model_config_ref="model", thinking_mode="high", poll_interval=0)

    async def test_completed_run_is_not_a_verifier_reward_and_original_text_is_submitted(self):
        instruction = "  Original instruction\n"
        context = AgentContext()
        await self.agent.setup(self.environment)
        await self.agent.run(instruction, self.environment, context)
        submitted = next(call[1] for call in self.client.calls if call[0] == "submit")
        self.assertEqual(submitted[4], instruction)
        self.assertEqual(submitted[5:], ("model", "high"))
        self.assertEqual(context.metadata["workspace"]["agentStatus"], "completed")
        self.assertNotIn("reward", context.metadata["workspace"])
        state = json.loads((Path(self.temporary.name) / "workspace-state.json").read_text())
        self.assertNotEqual(state["instructionSha256"], state["normalizedInstructionSha256"])
        self.assertNotIn(instruction, json.dumps(state))

    async def test_resume_checkpoint_does_not_resubmit_or_change_operation_identity(self):
        await self.agent.run("task", self.environment, AgentContext())
        original = json.loads((Path(self.temporary.name) / "workspace-state.json").read_text())
        await self.agent.resume("task", self.environment, AgentContext())
        self.assertEqual(sum(call[0] == "submit" for call in self.client.calls), 1)
        self.assertEqual(json.loads((Path(self.temporary.name) / "workspace-state.json").read_text())["operationId"], original["operationId"])
        with self.assertRaises(WorkspaceError):
            await self.agent.resume("changed task", self.environment, AgentContext())

    async def test_binding_mismatch_rejected_before_prompt(self):
        self.environment.workspace_binding = WorkspaceBinding("workspace", "agent", "session", "other", "instance", "trial")
        with self.assertRaises(WorkspaceError):
            await self.agent.run("task", self.environment, AgentContext())
        self.assertFalse(any(call[0] == "submit" for call in self.client.calls))

    async def test_cancellation_uses_original_run_and_preserves_primary_error(self):
        self.client.failure = asyncio.CancelledError()
        def failed_cancel(*args):
            self.client.calls.append(("cancel", *args))
            raise WorkspaceError("transport_unavailable")
        self.client.cancel_run = failed_cancel
        with self.assertRaises(asyncio.CancelledError):
            await self.agent.run("task", self.environment, AgentContext())
        self.assertIn(("cancel", "session", "run"), self.client.calls)
        state = json.loads((Path(self.temporary.name) / "workspace-state.json").read_text())
        self.assertEqual(state["cancellationStatus"], "unknown")

    async def test_timeout_waits_for_original_terminal_fact_and_quiescence_before_returning(self):
        self.client.failure = TimeoutError("original deadline")
        observations = iter([{"status": "running"}, {"status": "cancelled"}])
        order = []
        def fact(session, run):
            self.assertEqual((session, run), ("session", "run"))
            value = next(observations)
            order.append(value["status"])
            return {"agentRunId": run, **value}
        self.client.run_fact = fact
        self.environment.mark_agent_cancellation_pending = lambda run: order.append("pending")
        async def confirm(run):
            self.assertEqual(run, "run")
            self.assertEqual(order[-1], "cancelled")
            order.append("quiescent")
        self.environment.confirm_agent_cancellation = confirm
        context = AgentContext()
        with self.assertRaisesRegex(TimeoutError, "original deadline"):
            await self.agent.run("task", self.environment, context)
        self.assertEqual(order, ["pending", "running", "cancelled", "quiescent"])
        self.assertEqual(context.metadata["workspace"]["agentStatus"], "cancelled")
        self.assertEqual(sum(call[0] == "submit" for call in self.client.calls), 1)

    async def test_failed_canonical_run_is_not_reported_as_success(self):
        self.client.statuses = ["failed"]
        context = AgentContext()
        with self.assertRaisesRegex(WorkspaceError, "agent_run_failed"):
            await self.agent.run("task", self.environment, context)
        self.assertEqual(context.metadata["workspace"]["agentStatus"], "failed")
        self.assertFalse(any(call[0] == "cancel" for call in self.client.calls))

    async def test_untyped_binding_and_unconnected_mcp_are_rejected(self):
        self.environment.workspace_binding = {"containerId": "untrusted"}
        with self.assertRaisesRegex(WorkspaceError, "binding_required"):
            await self.agent.setup(self.environment)
        self.environment.workspace_binding = self.binding
        self.agent.mcp_servers = [object()]
        with self.assertRaisesRegex(WorkspaceError, "task_extensions_unsupported"):
            await self.agent.setup(self.environment)

    async def test_cancel_during_acceptance_waits_for_original_receipt_then_cancels_it(self):
        started, release = threading.Event(), threading.Event()
        submit = self.client.submit_message
        def blocked_submit(*args):
            started.set()
            if not release.wait(timeout=2):
                raise AssertionError("test did not release HTTP acceptance")
            return submit(*args)
        self.client.submit_message = blocked_submit
        task = asyncio.create_task(self.agent.run("task", self.environment, AgentContext()))
        self.assertTrue(await asyncio.to_thread(started.wait, 2))
        task.cancel()
        release.set()
        with self.assertRaises(asyncio.CancelledError):
            await task
        self.assertIn(("cancel", "session", "run"), self.client.calls)
        self.assertEqual(sum(call[0] == "submit" for call in self.client.calls), 1)

    async def test_unknown_submission_resume_only_queries_original_operation(self):
        original = self.client.submit_message
        captured = []
        def uncertain(*args):
            captured.append(args)
            raise SubmissionUncertain("submission_uncertain")
        self.client.submit_message = uncertain
        with self.assertRaises(SubmissionUncertain):
            await self.agent.run("task", self.environment, AgentContext())
        def receipt(*args):
            self.client.calls.append(("operation", args))
            return original(*captured[0])
        self.client.operation = receipt
        self.client.submit_message = lambda *args: self.fail("resume must not POST another prompt")
        await self.agent.resume("task", self.environment, AgentContext())
        self.assertEqual(len(captured), 1)

    async def test_expired_resumed_trial_cancels_instead_of_resetting_deadline(self):
        clock = [1000]
        self.client.statuses = ["running", "completed"]
        self.agent.agent_timeout_sec = 1
        original = self.client.read_run_events
        def advance_after_read(*args, **kwargs):
            value = original(*args, **kwargs)
            clock[0] = 1002
            return value
        self.client.read_run_events = advance_after_read
        with patch("workspace_agent.time", SimpleNamespace(time=lambda: clock[0])):
            with self.assertRaises(TimeoutError):
                await self.agent.run("task", self.environment, AgentContext())
            self.assertIn(("cancel", "session", "run"), self.client.calls)
            state = json.loads((Path(self.temporary.name) / "workspace-state.json").read_text())
            deadline = state["deadlineAt"]
            self.agent.agent_timeout_sec = 3600
            with self.assertRaises(TimeoutError):
                await self.agent.resume("task", self.environment, AgentContext())
        self.assertEqual(json.loads((Path(self.temporary.name) / "workspace-state.json").read_text())["deadlineAt"], deadline)

    async def test_checkpoint_unknown_fields_are_rejected_without_resubmission(self):
        await self.agent.run("task", self.environment, AgentContext())
        path = Path(self.temporary.name) / "workspace-state.json"
        state = json.loads(path.read_text())
        self.assertIsNone(state["deadlineAt"])
        state["inventedFlag"] = True
        path.write_text(json.dumps(state))
        with self.assertRaisesRegex(WorkspaceError, "checkpoint_invalid"):
            await self.agent.resume("task", self.environment, AgentContext())
        self.assertEqual(sum(call[0] == "submit" for call in self.client.calls), 1)


if __name__ == "__main__":
    unittest.main()
