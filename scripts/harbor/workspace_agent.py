"""Harbor Agent that submits work to Workspace's normal public admission API.

The custom Environment owns physical provisioning and verifier execution. This
adapter owns neither Core semantics nor a second local Runtime process.
"""
from __future__ import annotations

import asyncio
from dataclasses import asdict, dataclass
import hashlib
import json
import math
import os
from pathlib import Path
import tempfile
import time
import uuid

from harbor.agents.base import BaseAgent
from harbor.models.agent.context import AgentContext

from workspace_client import SubmissionUncertain, WorkspaceError


@dataclass(frozen=True)
class WorkspaceBinding:
    """Trusted controller provisioner's identity, never a task-supplied container ID."""
    workspace_id: str
    agent_id: str
    session_id: str
    environment_id: str
    instance_id: str
    trial_id: str

    def __post_init__(self):
        if any(not isinstance(value, str) or not value or len(value) > 160 for value in asdict(self).values()):
            raise ValueError("binding identities must be nonempty strings")

    def public(self):
        return {"workspaceId": self.workspace_id, "agentId": self.agent_id, "sessionId": self.session_id,
                "environmentId": self.environment_id, "instanceId": self.instance_id, "trialId": self.trial_id}


def message_operation_id(trial_id):
    return "tb-message-" + str(uuid.uuid5(uuid.NAMESPACE_URL, "centaeris-workspace:" + trial_id))


@dataclass(frozen=True)
class WorkspaceSubmission:
    """The original public acceptance made before setup, not a second prompt."""
    operation_id: str
    session_id: str
    agent_run_id: str
    turn_id: str
    instruction_sha256: str
    normalized_instruction_sha256: str
    model_config_ref: str
    thinking_mode: str | None


def _sha(value):
    return hashlib.sha256(value.encode("utf-8")).hexdigest()


def _write_state(path, state):
    path.parent.mkdir(parents=True, exist_ok=True)
    descriptor, temporary = tempfile.mkstemp(prefix=".workspace-state-", dir=path.parent)
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8") as output:
            json.dump(state, output, ensure_ascii=False, sort_keys=True)
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, path)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


class WorkspaceAgent(BaseAgent):
    SUPPORTS_RESUME = True

    @staticmethod
    def name():
        return "centaeris-workspace"

    def version(self):
        return "workspace-public-api-v1"

    def __init__(self, *args, model_config_ref, thinking_mode=None, agent_timeout_sec=None,
                 poll_interval=1, event_callback=None, **kwargs):
        super().__init__(*args, **kwargs)
        if not isinstance(model_config_ref, str) or not model_config_ref:
            raise ValueError("model_config_ref is required")
        for value in (poll_interval,) + (() if agent_timeout_sec is None else (agent_timeout_sec,)):
            if type(value) not in {int, float} or not math.isfinite(value):
                raise ValueError("timeouts must be finite numbers")
        if (agent_timeout_sec is not None and agent_timeout_sec <= 0) or not 0 <= poll_interval <= 30:
            raise ValueError("invalid timeout or poll interval")
        self.model_config_ref, self.thinking_mode = model_config_ref, thinking_mode
        self.agent_timeout_sec, self.poll_interval = agent_timeout_sec, poll_interval
        self.event_callback = event_callback

    def _ports(self, environment):
        binding = getattr(environment, "workspace_binding", None)
        client = getattr(environment, "workspace_client", None)
        if not isinstance(binding, WorkspaceBinding) or client is None:
            raise WorkspaceError("workspace_binding_required")
        if self.mcp_servers or self.skills_dir or self.load_trajectory:
            raise WorkspaceError("task_extensions_unsupported")
        return binding, client

    async def _verify_session(self, binding, client):
        value = await asyncio.to_thread(client.session, binding.session_id)
        session = value.get("session", {})
        expected = {"id": binding.session_id, "workspaceId": binding.workspace_id,
                    "agentId": binding.agent_id, "currentEnvironmentId": binding.environment_id, "status": "active"}
        if not isinstance(session, dict) or any(session.get(key) != value for key, value in expected.items()):
            raise WorkspaceError("workspace_binding_mismatch")

    async def setup(self, environment):
        binding, client = self._ports(environment)
        await self._verify_session(binding, client)

    async def resume(self, instruction, environment, context):
        if not (Path(self.logs_dir) / "workspace-state.json").is_file():
            raise WorkspaceError("resume_checkpoint_missing")
        await self.run(instruction, environment, context)

    def _notify(self, environment, binding, state):
        callback = self.event_callback or getattr(environment, "workspace_event_callback", None)
        if callback:
            callback({"sessionId": binding.session_id, "environmentId": binding.environment_id,
                      "instanceId": binding.instance_id, "agentRunId": state.get("agentRunId"),
                      "agentStatus": state["status"], "phase": "agent"})

    def _context(self, context, binding, state):
        context.metadata = {**(context.metadata or {}), "workspace": {
            **binding.public(), "operationId": state["operationId"], "agentRunId": state.get("agentRunId"),
            "agentStatus": state["status"], "sourceSequence": state["sourceSequence"],
            "instructionSha256": state["instructionSha256"],
            "normalizedInstructionSha256": state["normalizedInstructionSha256"],
            "usageCoverage": "not_collected"}}

    async def _confirm_cancellation(self, client, environment, binding, state):
        while True:
            fact = await asyncio.to_thread(client.run_fact, binding.session_id, state["agentRunId"])
            if fact is not None and fact["status"] in {"completed", "failed", "cancelled"}:
                if fact["agentRunId"] != state["agentRunId"]:
                    raise WorkspaceError("run_fact_identity_invalid")
                confirm = getattr(environment, "confirm_agent_cancellation", None)
                if confirm is not None:
                    await confirm(state["agentRunId"])
                state["status"] = fact["status"]
                state["cancellationStatus"] = "terminal"
                return
            await asyncio.sleep(max(0.02, min(self.poll_interval, 0.2)))

    async def run(self, instruction, environment, context: AgentContext):
        binding, client = self._ports(environment)
        if not isinstance(instruction, str) or not instruction.strip():
            raise ValueError("instruction must be nonempty text")
        await self._verify_session(binding, client)
        path = Path(self.logs_dir) / "workspace-state.json"
        operation = message_operation_id(binding.trial_id)
        accepted = getattr(environment, "workspace_submission", None)
        if accepted is not None and (not isinstance(accepted, WorkspaceSubmission)
                or accepted.operation_id != operation or accepted.session_id != binding.session_id
                or accepted.instruction_sha256 != _sha(instruction)
                or accepted.normalized_instruction_sha256 != _sha(instruction.strip())
                or accepted.model_config_ref != self.model_config_ref or accepted.thinking_mode != self.thinking_mode
                or not accepted.agent_run_id or not accepted.turn_id):
            raise WorkspaceError("submission_binding_mismatch")
        request_digest = _sha(json.dumps({"binding": binding.public(), "instruction": instruction,
            "modelConfigRef": self.model_config_ref, "thinkingMode": self.thinking_mode}, sort_keys=True, ensure_ascii=False))
        if path.exists():
            if path.stat().st_size > 16384:
                raise WorkspaceError("checkpoint_too_large")
            state = json.loads(path.read_text(encoding="utf-8"))
            if (not isinstance(state, dict) or state.get("schema") != "centaeris.workspace_benchmark.agent.v1"
                    or state.get("requestDigest") != request_digest or state.get("operationId") != operation
                    or state.get("binding") != binding.public()):
                raise WorkspaceError("checkpoint_binding_mismatch")
            required = {"schema", "binding", "operationId", "requestDigest", "instructionSha256",
                        "normalizedInstructionSha256", "status", "agentRunId", "cursor", "sourceSequence", "deadlineAt"}
            if (not required.issubset(state) or set(state) - required - {"cancellationStatus"}
                    or state["status"] not in {"prepared", "submission_uncertain", "running", "completed", "failed", "cancelled"}
                    or type(state["sourceSequence"]) is not int or state["sourceSequence"] < 0
                    or not isinstance(state["cursor"], str) or not 0 < len(state["cursor"]) <= 512
                    or state["instructionSha256"] != _sha(instruction)
                    or state["normalizedInstructionSha256"] != _sha(instruction.strip())
                    or (state["agentRunId"] is not None and (not isinstance(state["agentRunId"], str)
                        or not 0 < len(state["agentRunId"]) <= 160))
                    or (state["status"] in {"running", "completed", "failed", "cancelled"} and not state["agentRunId"])
                    or (state["deadlineAt"] is not None and (type(state["deadlineAt"]) not in {int, float}
                        or not math.isfinite(state["deadlineAt"]) or state["deadlineAt"] <= 0))
                    or state.get("cancellationStatus") not in {None, "requested", "terminal", "unknown"}):
                raise WorkspaceError("checkpoint_invalid")
        else:
            state = {"schema": "centaeris.workspace_benchmark.agent.v1", "binding": binding.public(),
                     "operationId": operation, "requestDigest": request_digest, "instructionSha256": _sha(instruction),
                     "normalizedInstructionSha256": _sha(instruction.strip()), "status": "prepared", "agentRunId": None,
                     "cursor": "0-0", "sourceSequence": 0,
                     "deadlineAt": None if self.agent_timeout_sec is None else time.time() + self.agent_timeout_sec}
            _write_state(path, state)
        self._context(context, binding, state)
        submission = None
        try:
            if (state.get("cancellationStatus") == "terminal" and state["deadlineAt"] is not None
                    and time.time() >= state["deadlineAt"]):
                raise TimeoutError("Workspace agent deadline exceeded")
            if accepted is not None:
                if state["agentRunId"] not in {None, accepted.agent_run_id}:
                    raise WorkspaceError("submission_binding_mismatch")
                if state["agentRunId"] is None:
                    state.update(agentRunId=accepted.agent_run_id, status="running")
                    _write_state(path, state)
            if state["status"] == "submission_uncertain":
                receipt = await asyncio.to_thread(client.operation, binding.workspace_id, "submitMessage", operation)
                if receipt is None:
                    raise SubmissionUncertain("submission_uncertain")
                if receipt["sessionId"] != binding.session_id:
                    raise WorkspaceError("operation_session_mismatch")
                state.update(agentRunId=receipt["agentRunId"], status="running")
                _write_state(path, state)
            if state["agentRunId"] is None:
                if state["deadlineAt"] is not None and time.time() >= state["deadlineAt"]:
                    raise TimeoutError("Workspace agent deadline exceeded")
                # Persist uncertainty before sending: process death cannot authorize
                # a second prompt, even if no receipt is immediately observable.
                state["status"] = "submission_uncertain"
                _write_state(path, state)
                submission = asyncio.create_task(asyncio.to_thread(client.submit_message,
                    binding.workspace_id, binding.session_id, binding.agent_id, operation,
                    instruction, self.model_config_ref, self.thinking_mode))
                receipt = await asyncio.shield(submission)
                state.update(agentRunId=receipt["agentRunId"], status="running")
                _write_state(path, state)
            run_id = state["agentRunId"]
            actual = await asyncio.to_thread(client.run_environment, binding.session_id, run_id)
            if (actual.get("sessionId") != binding.session_id or actual.get("agentRunId") != run_id
                    or actual.get("environmentId") != binding.environment_id):
                raise WorkspaceError("workspace_run_binding_mismatch")
            activate = getattr(environment, "activate_workspace_setup", None)
            if accepted is not None and state["status"] not in {"completed", "failed", "cancelled"}:
                if activate is None or await activate() != "ready":
                    raise WorkspaceError("workspace_setup_not_ready")
            while state["status"] not in {"completed", "failed", "cancelled"}:
                if state["deadlineAt"] is not None and time.time() >= state["deadlineAt"]:
                    raise TimeoutError("Workspace agent deadline exceeded")
                try:
                    update = await asyncio.to_thread(client.read_run_events, binding.session_id, run_id,
                        state["cursor"], after_sequence=state["sourceSequence"])
                except WorkspaceError as error:
                    if error.code != "transport_unavailable" and (error.status is None or error.status < 500):
                        raise
                else:
                    state.update(cursor=update["cursor"], sourceSequence=update["sourceSequence"], status=update["status"])
                    _write_state(path, state)
                    self._context(context, binding, state)
                    self._notify(environment, binding, state)
                if state["status"] == "running":
                    await asyncio.sleep(self.poll_interval)
            self._context(context, binding, state)
            if state["status"] != "completed":
                raise WorkspaceError("agent_run_" + state["status"])
        except BaseException:
            # Cancellation may race HTTP acceptance. Finish that bounded request
            # before attempting to cancel the original Run; never invent a Run ID.
            if submission is not None and state["agentRunId"] is None:
                try:
                    receipt = await asyncio.wait_for(asyncio.shield(submission), timeout=2 * client.request_timeout + 2)
                    state.update(agentRunId=receipt["agentRunId"], status="running")
                except BaseException:
                    pass
            if state["agentRunId"] and state["status"] not in {"completed", "failed", "cancelled"}:
                try:
                    pending = getattr(environment, "mark_agent_cancellation_pending", None)
                    if pending is not None:
                        pending(state["agentRunId"])
                    result = await asyncio.to_thread(client.cancel_run, binding.session_id, state["agentRunId"])
                    state["cancellationStatus"] = result["disposition"]
                    await asyncio.wait_for(self._confirm_cancellation(client, environment, binding, state),
                                           timeout=min(30, max(2, 2 * client.request_timeout + 2)))
                except BaseException:
                    state["cancellationStatus"] = "unknown"
            try:
                _write_state(path, state)
                self._context(context, binding, state)
                self._notify(environment, binding, state)
            except Exception:
                pass
            raise
