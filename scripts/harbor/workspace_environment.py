"""Harbor's same-instance environment lifecycle through trusted control ports.

No Docker authority or maintenance credential is copied into a task container.
The provisioner port must be implemented by the independently authorized
benchmark controller; these contracts do not claim that HTTP host ports exist.
"""
from __future__ import annotations

import asyncio
from dataclasses import dataclass
import hashlib
import json
import math
from pathlib import Path, PurePosixPath
import re
import time
from typing import Protocol
import uuid

from harbor.environments.base import BaseEnvironment, ExecResult
from harbor.environments.capabilities import EnvironmentCapabilities, EnvironmentResourceCapabilities
from harbor.models.task.task import strip_canary
from harbor.models.trial.paths import EnvironmentPaths

from workspace_agent import WorkspaceBinding, WorkspaceSubmission, message_operation_id
from workspace_client import WorkspaceError


@dataclass(frozen=True)
class OwnedEnvironment:
    session_id: str
    environment_id: str
    agent_run_id: str
    instance_id: str
    container_id: str
    host_id: str
    controller_epoch: int
    image_digest: str
    profile_digest: str
    setup_state: str
    resource_holding: int

    def __post_init__(self):
        for value in (self.session_id, self.environment_id, self.agent_run_id, self.instance_id,
                      self.container_id, self.host_id):
            if not isinstance(value, str) or re.fullmatch(r"[A-Za-z0-9_.:-]{1,160}", value) is None:
                raise ValueError("invalid owned environment identity")
        if type(self.controller_epoch) is not int or self.controller_epoch < 0:
            raise ValueError("invalid controller epoch")
        if any(not isinstance(value, str) or re.fullmatch(r"sha256:[0-9a-f]{64}", value) is None
               for value in (self.image_digest, self.profile_digest)):
            raise ValueError("invalid image or profile digest")
        if self.setup_state not in {"waitingSetup", "ready"} or type(self.resource_holding) is not int or self.resource_holding != 1:
            raise ValueError("owned instance must hold its resource allocation")


@dataclass(frozen=True)
class CleanupObservation:
    environment_id: str
    instance_id: str
    physical_state: str
    resource_holding: int | None

    def __post_init__(self):
        if (not self.environment_id or not self.instance_id or self.physical_state not in {"present", "absent", "unknown"}
                or (self.resource_holding is not None and (type(self.resource_holding) is not int
                    or self.resource_holding not in {0, 1}))):
            raise ValueError("invalid cleanup observation")


class BenchmarkProvisioner(Protocol):
    """Controller-side ports; all operations verify ledger ownership and fence.

    Exec transport failures are unknown outcomes, never implicit replay. File
    methods must stream bounded binary data and reject unsafe archive extraction.
    Maintenance is independently authorized from the public user's run authority.
    """
    def bind_profile(self, session_id: str, environment_id: str, profile: dict, operation_id: str) -> dict: ...
    def observe_setup(self, session_id: str, environment_id: str, agent_run_id: str) -> OwnedEnvironment | None: ...
    def activate_setup(self, owned: OwnedEnvironment, operation_id: str) -> str: ...
    def exec(self, owned: OwnedEnvironment, **request) -> dict: ...
    def upload_file(self, owned: OwnedEnvironment, source: Path, target: str) -> None: ...
    def upload_dir(self, owned: OwnedEnvironment, source: Path, target: str) -> None: ...
    def download_file(self, owned: OwnedEnvironment, source: str, target: Path) -> None: ...
    def download_dir(self, owned: OwnedEnvironment, source: str, target: Path) -> None: ...
    def retire(self, owned: OwnedEnvironment, operation_id: str) -> str: ...
    def observe_cleanup(self, owned: OwnedEnvironment) -> CleanupObservation: ...


def _digest(profile):
    return "sha256:" + hashlib.sha256(json.dumps(profile, sort_keys=True, ensure_ascii=False,
        separators=(",", ":"), allow_nan=False).encode()).hexdigest()


def _remote_path(value):
    if (not isinstance(value, str) or not value.startswith("/") or value.startswith("//")
            or "\x00" in value or ".." in PurePosixPath(value).parts):
        raise ValueError("remote path must be absolute without traversal")
    return value


class WorkspaceEnvironment(BaseEnvironment):
    def __init__(self, *args, workspace_client, provisioner: BenchmarkProvisioner, allocation,
                 workspace_id, agent_id, trial_id, profile, model_config_ref, thinking_mode=None,
                 poll_interval=1, setup_timeout_sec=None, workspace_event_callback=None, **kwargs):
        self.workspace_client, self.provisioner, self.allocation = workspace_client, provisioner, allocation
        self.workspace_id, self.agent_id, self.trial_id = workspace_id, agent_id, trial_id
        self.profile = json.loads(json.dumps(profile, allow_nan=False))
        self.profile_digest = _digest(self.profile)
        self.model_config_ref, self.thinking_mode = model_config_ref, thinking_mode
        self.poll_interval, self.setup_timeout_sec = poll_interval, setup_timeout_sec
        self.workspace_event_callback = workspace_event_callback
        self.workspace_binding = None
        self.workspace_submission = None
        self._owned = None
        self._session_id = self._environment_id = self._agent_run_id = None
        self._creation_attempted = self._destroyed = False
        self._setup_activated = False
        self._setup_prepared = False
        self._setup_terminal_error = None
        self._agent_cancellation_pending = False
        self._lifecycle_lock = asyncio.Lock()
        super().__init__(*args, **kwargs)
        if (type(poll_interval) not in {int, float} or not math.isfinite(poll_interval) or not 0 <= poll_interval <= 30
                or (setup_timeout_sec is not None and (type(setup_timeout_sec) not in {int, float}
                    or not math.isfinite(setup_timeout_sec) or setup_timeout_sec <= 0))):
            raise ValueError("invalid setup timing")
        expected = {"cpuMilli": self.task_env_config.cpus * 1000,
                    "memoryBytes": self.task_env_config.memory_mb * 1024**2,
                    "storageBytes": self.task_env_config.storage_mb * 1024**2}
        resources = self.profile.get("resources", {})
        if any(resources.get(key) != value for key, value in expected.items()):
            raise WorkspaceError("task_resource_profile_mismatch")
        network = "disabled" if self._network_disabled else "publicInternet"
        if self.profile.get("network") != network:
            raise WorkspaceError("task_network_profile_mismatch")
        if self._persistent_env:
            raise WorkspaceError("task_mounts_or_environment_unsupported")
        # Harbor always supplies its three log-directory bind hints, including
        # to remote providers. We copy these artifacts through the owned port;
        # no controller host path is mounted into the task container.
        for mount in self._mounts:
            if (set(mount) != {"type", "source", "target"} or mount["type"] != "bind"
                    or mount["target"] not in {"/logs/agent", "/logs/verifier", "/logs/artifacts"}
                    or not Path(mount["source"]).resolve().is_relative_to(self.trial_paths.trial_dir.resolve())):
                raise WorkspaceError("task_mounts_or_environment_unsupported")

    @staticmethod
    def type():
        return "centaeris-workspace"

    @property
    def capabilities(self):
        return EnvironmentCapabilities(disable_internet=True)

    @classmethod
    def resource_capabilities(cls):
        return EnvironmentResourceCapabilities(cpu_limit=True, memory_limit=True)

    def _validate_definition(self):
        if not self.environment_dir.is_dir() or not (self.environment_dir.parent / "instruction.md").is_file():
            raise ValueError("pinned task environment and instruction are required")
        if any((self.environment_dir / name).exists() for name in ("docker-compose.yaml", "docker-compose.yml")):
            raise WorkspaceError("compose_tasks_unsupported")
        if not self.task_env_config.docker_image and not (self.environment_dir / "Dockerfile").is_file():
            raise ValueError("task image or Dockerfile is required")
        if any(type(value) is not int or value <= 0 for value in (
                self.task_env_config.cpus, self.task_env_config.memory_mb, self.task_env_config.storage_mb)):
            raise WorkspaceError("task_resources_required")

    def _operation(self, verb):
        return "tb-" + verb + "-" + str(uuid.uuid5(uuid.NAMESPACE_URL, "centaeris-workspace:" + self.trial_id))

    def _notify(self, **payload):
        if self.workspace_event_callback:
            self.workspace_event_callback(payload)

    def _check_owned(self, owned, *, waiting=False):
        if (not isinstance(owned, OwnedEnvironment) or owned.session_id != self._session_id
                or owned.environment_id != self._environment_id or owned.agent_run_id != self._agent_run_id
                or owned.image_digest != self.profile["imageDigest"] or owned.profile_digest != self.profile_digest
                or (waiting and owned.setup_state != "waitingSetup")):
            raise WorkspaceError("prepared_environment_binding_mismatch")
        if self._owned is not None and any(getattr(owned, key) != getattr(self._owned, key)
                for key in ("instance_id", "container_id", "host_id", "controller_epoch")):
            raise WorkspaceError("prepared_environment_identity_changed")
        return owned

    async def start(self, force_build=False):
        async with self._lifecycle_lock:
            if self._destroyed:
                raise WorkspaceError("environment_destroyed")
            if self._setup_terminal_error is not None:
                raise WorkspaceError(self._setup_terminal_error)
            if self._owned is not None:
                if not self._setup_prepared:
                    raise WorkspaceError("workspace_setup_incomplete")
                return
            if force_build:
                raise WorkspaceError("immutable_profile_rebuild_required")
            instruction_path = self.environment_dir.parent / "instruction.md"
            if instruction_path.stat().st_size > 1024 * 1024:
                raise WorkspaceError("task_instruction_too_large")
            # Match Harbor Task.instruction exactly, including its own canary
            # preprocessing, instead of exposing provenance markers to models.
            instruction = strip_canary(instruction_path.read_text(encoding="utf-8"))
            receipt = await asyncio.to_thread(self.workspace_client.create_session,
                self.workspace_id, self.agent_id, self._operation("session"))
            self._session_id = receipt["sessionId"]
            response = await asyncio.to_thread(self.workspace_client.session, self._session_id)
            session = response.get("session", {})
            if (session.get("id") != self._session_id or session.get("workspaceId") != self.workspace_id
                    or session.get("agentId") != self.agent_id or session.get("status") != "active"
                    or not isinstance(session.get("currentEnvironmentId"), str)):
                raise WorkspaceError("workspace_binding_mismatch")
            self._environment_id = session["currentEnvironmentId"]
            bound = await asyncio.to_thread(self.provisioner.bind_profile, self._session_id,
                self._environment_id, self.profile, self._operation("profile"))
            if (bound.get("sessionId") != self._session_id or bound.get("environmentId") != self._environment_id
                    or bound.get("profileDigest") != self.profile_digest):
                raise WorkspaceError("benchmark_profile_binding_mismatch")
            self.allocation.mark_creation_started()
            self._creation_attempted = True
            receipt = await asyncio.to_thread(self.workspace_client.submit_message, self.workspace_id,
                self._session_id, self.agent_id, message_operation_id(self.trial_id), instruction,
                self.model_config_ref, self.thinking_mode)
            self._agent_run_id = receipt["agentRunId"]
            self.workspace_submission = WorkspaceSubmission(receipt["operationId"], self._session_id,
                self._agent_run_id, receipt["turnId"], hashlib.sha256(instruction.encode()).hexdigest(),
                hashlib.sha256(instruction.strip().encode()).hexdigest(), self.model_config_ref, self.thinking_mode)
            timeout = self.setup_timeout_sec or self.task_env_config.build_timeout_sec
            deadline = time.monotonic() + timeout
            while time.monotonic() < deadline:
                observed = await asyncio.to_thread(self.provisioner.observe_setup, self._session_id,
                    self._environment_id, self._agent_run_id)
                if observed is not None:
                    self._owned = self._check_owned(observed, waiting=True)
                    self.workspace_binding = WorkspaceBinding(self.workspace_id, self.agent_id,
                        self._session_id, self._environment_id, self._owned.instance_id, self.trial_id)
                    self._notify(sessionId=self._session_id, agentRunId=self._agent_run_id,
                        environmentId=self._environment_id, instanceId=self._owned.instance_id,
                        containerId=self._owned.container_id, phase="setup", status="running", resourceHolding=1)
                    paths = EnvironmentPaths.for_os(self.os)
                    prepared = await self.ensure_dirs([paths.agent_dir, paths.verifier_dir, paths.artifacts_dir])
                    if prepared is None or prepared.return_code != 0:
                        raise WorkspaceError("benchmark_log_directories_unavailable")
                    await self._upload_environment_dir_after_start()
                    self._setup_prepared = True
                    return
                try:
                    fact = await asyncio.to_thread(self.workspace_client.run_fact, self._session_id, self._agent_run_id)
                except WorkspaceError as error:
                    if error.code != "transport_unavailable" and (error.status is None or error.status < 500):
                        raise
                    fact = None
                if fact is not None and fact["status"] in {"completed", "failed", "cancelled"}:
                    self._setup_terminal_error = "benchmark_setup_agent_run_" + fact["status"]
                    self._notify(sessionId=self._session_id, agentRunId=self._agent_run_id,
                        environmentId=self._environment_id, phase="setup", status="failed", error=self._setup_terminal_error)
                    raise WorkspaceError(self._setup_terminal_error)
                await asyncio.sleep(self.poll_interval)
            raise WorkspaceError("benchmark_setup_timeout")

    async def activate_workspace_setup(self):
        owned = self._require_owned()
        if not self._setup_prepared:
            raise WorkspaceError("workspace_setup_incomplete")
        if not self._setup_activated:
            status = await asyncio.to_thread(self.provisioner.activate_setup, owned, self._operation("activate"))
            if status != "ready":
                raise WorkspaceError("workspace_setup_not_ready")
            self._setup_activated = True
        return "ready"

    def _require_owned(self):
        if self._owned is None or self._destroyed:
            raise WorkspaceError("environment_not_available")
        return self._owned

    def mark_agent_cancellation_pending(self, agent_run_id):
        if agent_run_id != self._agent_run_id:
            raise WorkspaceError("workspace_run_binding_mismatch")
        self._agent_cancellation_pending = True

    async def confirm_agent_cancellation(self, agent_run_id):
        owned = self._require_owned()
        if agent_run_id != owned.agent_run_id or not self._agent_cancellation_pending:
            raise WorkspaceError("workspace_run_binding_mismatch")
        fact = await asyncio.to_thread(self.workspace_client.run_fact, owned.session_id, agent_run_id)
        if fact is None or fact.get("agentRunId") != agent_run_id or fact.get("status") not in {"completed", "failed", "cancelled"}:
            raise WorkspaceError("agent_quiescence_unconfirmed")
        # An independent maintenance command confirms silence even when the
        # execute controller's last cancellation acknowledgement was lost.
        value = await asyncio.to_thread(self.provisioner.exec, owned,
            command="exec /opt/centaeris/benchmark/execution_agent benchmark-quiesce",
            cwd="/", env={}, timeout_sec=5, user=self.profile["user"])
        try:
            receipt = json.loads(value["stdout"])
        except (KeyError, TypeError, ValueError):
            raise WorkspaceError("agent_quiescence_unconfirmed") from None
        if (value.get("returnCode") != 0 or receipt != {"schema": "runtime.benchmark.quiescence.v1", "quiescent": True}
                or type(receipt.get("quiescent")) is not bool):
            raise WorkspaceError("agent_quiescence_unconfirmed")
        self._agent_cancellation_pending = False

    def _require_verifier_handoff(self):
        if self._agent_cancellation_pending:
            raise WorkspaceError("agent_quiescence_unconfirmed")

    async def exec(self, command, cwd=None, env=None, timeout_sec=None, user=None):
        self._require_verifier_handoff()
        owned = self._require_owned()
        if not isinstance(command, str) or not command:
            raise ValueError("command must be nonempty text")
        if cwd is not None:
            _remote_path(cwd)
        if timeout_sec is not None and (type(timeout_sec) not in {int, float} or not math.isfinite(timeout_sec) or timeout_sec <= 0):
            raise ValueError("exec timeout must be positive")
        request = {"command": command, "cwd": cwd, "env": self._merge_env(env),
                   "timeout_sec": timeout_sec, "user": self._resolve_user(user)}
        # A transport failure propagates as an unknown outcome. Never rerun it.
        value = await asyncio.to_thread(self.provisioner.exec, owned, **request)
        if (not isinstance(value, dict) or set(value) != {"returnCode", "stdout", "stderr"}
                or type(value["returnCode"]) is not int
                or any(not isinstance(value[key], str) for key in ("stdout", "stderr"))
                or sum(len(value[key].encode()) for key in ("stdout", "stderr")) > 8 * 1024 * 1024):
            raise WorkspaceError("benchmark_exec_response_invalid")
        for stream in ("stdout", "stderr"):
            if value[stream]:
                callback = self._output_callback()
                if callback:
                    await callback(value[stream], stream)
        return ExecResult(return_code=value["returnCode"], stdout=value["stdout"], stderr=value["stderr"])

    async def upload_file(self, source_path, target_path):
        self._require_verifier_handoff()
        source = Path(source_path)
        if not source.is_file():
            raise ValueError("upload source must be a file")
        await asyncio.to_thread(self.provisioner.upload_file, self._require_owned(), source, _remote_path(target_path))

    async def upload_dir(self, source_dir, target_dir):
        self._require_verifier_handoff()
        source = Path(source_dir)
        if not source.is_dir():
            raise ValueError("upload source must be a directory")
        await asyncio.to_thread(self.provisioner.upload_dir, self._require_owned(), source, _remote_path(target_dir))

    async def download_file(self, source_path, target_path):
        self._require_verifier_handoff()
        await asyncio.to_thread(self.provisioner.download_file, self._require_owned(), _remote_path(source_path), Path(target_path))

    async def download_dir(self, source_dir, target_dir):
        self._require_verifier_handoff()
        await asyncio.to_thread(self.provisioner.download_dir, self._require_owned(), _remote_path(source_dir), Path(target_dir))

    async def stop(self, delete=True):
        async with self._lifecycle_lock:
            if self._destroyed or not delete:
                return
            if self._owned is None:
                if not self._creation_attempted:
                    await self.allocation.release(cleanup_state="neverCreated")
                    self._destroyed = True
                    return
                # Acceptance can finish after cancellation stopped awaiting its
                # HTTP response. Query its original identity, never POST again.
                if self._agent_run_id is None:
                    receipt = await asyncio.to_thread(self.workspace_client.operation, self.workspace_id,
                        "submitMessage", message_operation_id(self.trial_id))
                    if receipt is not None:
                        if receipt["sessionId"] != self._session_id:
                            raise WorkspaceError("operation_session_mismatch")
                        self._agent_run_id = receipt["agentRunId"]
                if self._agent_run_id is not None:
                    observed = await asyncio.to_thread(self.provisioner.observe_setup, self._session_id,
                        self._environment_id, self._agent_run_id)
                    if observed is not None:
                        self._owned = self._check_owned(observed)
                if self._owned is None:
                    raise WorkspaceError("cleanup_pending")
            owned = self._require_owned()
            retirement_error = None
            try:
                await asyncio.to_thread(self.provisioner.retire, owned, self._operation("retire"))
            except WorkspaceError as error:
                # Preserve only the bounded protocol diagnostic, never the
                # original exception text, response body or credential context.
                code = error.code if (isinstance(error.code, str)
                    and re.fullmatch(r"[a-z][a-z0-9_]{0,127}", error.code)) else "retirement_failed"
                status = error.status if type(error.status) is int and 100 <= error.status <= 599 else None
                retirement_error = WorkspaceError(code, status)
            observation = await asyncio.to_thread(self.provisioner.observe_cleanup, owned)
            if (not isinstance(observation, CleanupObservation) or observation.environment_id != owned.environment_id
                    or observation.instance_id != owned.instance_id or observation.physical_state != "absent"
                    or observation.resource_holding != 0):
                self._notify(cleanupStatus="pending", resourceHolding=1, phase="cleanup")
                if retirement_error is not None:
                    raise retirement_error from None
                raise WorkspaceError("cleanup_pending")
            await self.allocation.release(cleanup_state="absent")
            self._destroyed = True
            # Cleanup success is independent of the Agent/verifier outcome.
            self._notify(cleanupStatus="absent", resourceHolding=0, phase="cleanup")
