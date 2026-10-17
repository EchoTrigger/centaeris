"""Lifecycle contract tests; no Docker, database, credentials, or model requests."""
import asyncio
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

try:
    from harbor.models.task.config import EnvironmentConfig
    from harbor.models.trial.paths import TrialPaths
    from workspace_environment import WorkspaceEnvironment, OwnedEnvironment, CleanupObservation
    from workspace_agent import WorkspaceAgent
    from harbor.models.agent.context import AgentContext
except ModuleNotFoundError as error:
    if error.name != "harbor":
        raise
    WorkspaceEnvironment = None

from workspace_client import WorkspaceError


class Allocation:
    def __init__(self):
        self.released = False
        self.creation_started = False

    def mark_creation_started(self):
        self.creation_started = True

    async def release(self, *, cleanup_state):
        if cleanup_state != "absent" and not (cleanup_state == "neverCreated" and not self.creation_started):
            raise AssertionError("only proven absence releases admission")
        self.released = True


class PublicClient:
    request_timeout = 0.1

    def __init__(self):
        self.calls = []

    def create_session(self, workspace, agent, operation):
        self.calls.append(("create", operation))
        return {"sessionId": "session"}

    def session(self, session):
        return {"session": {"id": "session", "workspaceId": "workspace", "agentId": "agent",
                            "currentEnvironmentId": "environment", "status": "active"}}

    def submit_message(self, *args):
        self.calls.append(("submit", args))
        return {"operationId": args[3], "command": "submitMessage", "status": "accepted",
                "sessionId": "session", "agentRunId": "run", "turnId": "turn"}

    def run_environment(self, session, run):
        return {"sessionId": session, "agentRunId": run, "environmentId": "environment"}

    def run_fact(self, session, run):
        return None

    def read_run_events(self, session, run, cursor, *, after_sequence):
        return {"cursor": "cursor", "sourceSequence": 1, "status": "completed"}


class Provisioner:
    def __init__(self, profile):
        self.profile = profile
        self.calls = []
        self.files = {}
        self.physical = "present"
        self.debit = 1
        self.fail_retire = False

    def bind_profile(self, session, environment, profile, operation):
        self.calls.append(("bind", session, environment, operation))
        return {"environmentId": environment, "sessionId": session, "profileDigest": profile_digest(profile)}

    def observe_setup(self, session, environment, run):
        self.calls.append(("observe", run))
        return OwnedEnvironment("session", "environment", "run", "instance", "container", "host", 2,
                                self.profile["imageDigest"], profile_digest(self.profile), "waitingSetup", 1)

    def activate_setup(self, owned, operation):
        self.calls.append(("activate", owned.instance_id, operation))
        return "ready"

    def exec(self, owned, **request):
        self.calls.append(("exec", owned.instance_id, request))
        return {"returnCode": 0, "stdout": "same instance", "stderr": ""}

    def upload_file(self, owned, source, target):
        self.calls.append(("upload", owned.instance_id, target))
        self.files[target] = Path(source).read_bytes()

    def download_file(self, owned, source, target):
        self.calls.append(("download", owned.instance_id, source))
        Path(target).write_bytes(self.files[source])

    def upload_dir(self, owned, source, target):
        self.calls.append(("uploadDir", owned.instance_id, target))

    def download_dir(self, owned, source, target):
        self.calls.append(("downloadDir", owned.instance_id, source))

    def retire(self, owned, operation):
        self.calls.append(("retire", owned.instance_id, operation))
        if self.fail_retire:
            raise WorkspaceError("transport_unavailable")
        return "destroyed"

    def observe_cleanup(self, owned):
        self.calls.append(("cleanup", owned.instance_id))
        return CleanupObservation(owned.environment_id, owned.instance_id, self.physical, self.debit)


def profile_digest(profile):
    return "sha256:" + hashlib.sha256(json.dumps(profile, sort_keys=True, separators=(",", ":")).encode()).hexdigest()


@unittest.skipUnless(WorkspaceEnvironment, "requires Harbor 0.21.0")
class EnvironmentTests(unittest.IsolatedAsyncioTestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        environment_dir = self.root / "task" / "environment"
        environment_dir.mkdir(parents=True)
        (environment_dir / "Dockerfile").write_text("FROM scratch\n")
        (environment_dir.parent / "instruction.md").write_text("  Original task instruction\n", encoding="utf-8")
        self.profile = {"schema": "runtime.benchmark_execution_profile.v1", "imageDigest": "sha256:" + "a" * 64,
                        "workspaceRoot": "/app", "user": "0:0", "network": "publicInternet", "resources": {
                            "cpuMilli": 1000, "memoryBytes": 2 * 1024**3, "pidsLimit": 256,
                            "dataTmpfsBytes": 1024**3, "storageBytes": 10 * 1024**3}}
        self.client, self.allocation = PublicClient(), Allocation()
        self.provisioner = Provisioner(self.profile)
        self.arguments = dict(environment_dir=environment_dir, environment_name="task",
            session_id="harbor-environment", trial_paths=TrialPaths(self.root / "trial"),
            task_env_config=EnvironmentConfig(cpus=1, memory_mb=2048, storage_mb=10240),
            workspace_client=self.client, provisioner=self.provisioner, allocation=self.allocation,
            workspace_id="workspace", agent_id="agent", trial_id="trial", profile=self.profile,
            model_config_ref="model", thinking_mode="high", poll_interval=0)
        self.environment = WorkspaceEnvironment(**self.arguments)

    def filesystem_provisioner(self):
        bash = str(Path('C:/Program Files/Git/bin/bash.exe')) if os.name == 'nt' else shutil.which('bash')
        if not bash or not Path(bash).is_file():
            self.skipTest('requires Bash to exercise actual directory preparation')
        logs = self.root / 'container-logs'
        logs.mkdir()
        def execute(owned, **request):
            command = request['command'].replace('/logs/', logs.as_posix() + '/')
            result = subprocess.run([bash, '-c', command], capture_output=True, text=True)
            return {'returnCode': result.returncode, 'stdout': result.stdout, 'stderr': result.stderr}
        self.provisioner.exec = execute
        return logs

    async def test_cancelled_agent_blocks_verifier_until_process_quiescence_is_confirmed(self):
        await self.environment.start()
        await self.environment.activate_workspace_setup()
        self.environment.mark_agent_cancellation_pending("run")
        with self.assertRaisesRegex(WorkspaceError, "agent_quiescence_unconfirmed"):
            await self.environment.exec("verifier")
        self.client.run_fact = lambda session, run: {"agentRunId": run, "status": "cancelled"}
        original = self.provisioner.exec
        def quiesce(owned, **request):
            if "benchmark-quiesce" in request["command"]:
                self.assertEqual(request["user"], self.profile["user"])
                return {"returnCode": 0, "stdout": json.dumps({"schema": "runtime.benchmark.quiescence.v1", "quiescent": True}), "stderr": ""}
            return original(owned, **request)
        self.provisioner.exec = quiesce
        await self.environment.confirm_agent_cancellation("run")
        self.assertEqual((await self.environment.exec("verifier")).return_code, 0)
        self.assertFalse(self.allocation.released)

    async def test_uncertain_quiescence_blocks_verifier_but_keeps_retirement_available(self):
        await self.environment.start()
        await self.environment.activate_workspace_setup()
        self.environment.mark_agent_cancellation_pending("run")
        self.client.run_fact = lambda session, run: {"agentRunId": run, "status": "cancelled"}
        def unknown(*args, **kwargs):
            raise WorkspaceError("benchmark_exec_outcome_unknown")
        self.provisioner.exec = unknown
        with self.assertRaises(WorkspaceError):
            await self.environment.confirm_agent_cancellation("run")
        with self.assertRaisesRegex(WorkspaceError, "agent_quiescence_unconfirmed"):
            await self.environment.upload_dir(self.root, "/tests")
        self.assertFalse(self.allocation.released)
        self.provisioner.physical, self.provisioner.debit = "absent", 0
        await self.environment.stop()
        self.assertTrue(self.allocation.released)

    async def test_start_prepares_writable_harbor_logs_without_removing_existing_contents(self):
        logs = self.filesystem_provisioner()
        (logs / 'agent').mkdir()
        (logs / 'agent/previous.txt').write_text('retained diagnostic')
        await self.environment.start()
        for name in ('agent', 'verifier', 'artifacts'):
            self.assertTrue((logs / name).is_dir(), name)
            if os.name != 'nt':
                self.assertEqual((logs / name).stat().st_mode & 0o777, 0o777)
        self.assertEqual((logs / 'agent/previous.txt').read_text(), 'retained diagnostic')
        result = await self.environment.exec('printf verifier-marker > /logs/verifier/test-stdout.txt')
        self.assertEqual(result.return_code, 0)
        self.assertEqual((logs / 'verifier/test-stdout.txt').read_text(), 'verifier-marker')
        self.assertFalse(any(call[0] == 'activate' for call in self.provisioner.calls))

    async def test_failed_log_preparation_retains_cleanup_identity_and_prevents_activation(self):
        self.provisioner.exec = lambda owned, **request: {'returnCode': 1, 'stdout': '', 'stderr': 'permission denied'}
        with self.assertRaisesRegex(WorkspaceError, 'benchmark_log_directories_unavailable'):
            await self.environment.start()
        with self.assertRaisesRegex(WorkspaceError, 'workspace_setup_incomplete'):
            await self.environment.activate_workspace_setup()
        with self.assertRaisesRegex(WorkspaceError, 'workspace_setup_incomplete'):
            await self.environment.start()
        self.assertFalse(any(call[0] == 'activate' for call in self.provisioner.calls))
        self.provisioner.physical, self.provisioner.debit = 'absent', 0
        await self.environment.stop(delete=True)
        self.assertTrue(self.allocation.released)
        self.assertEqual([call[1] for call in self.provisioner.calls if call[0] == 'retire'], ['instance'])

    async def test_start_pre_admits_once_and_waits_before_any_model_work(self):
        await self.environment.start(force_build=False)
        self.assertEqual([call[0] for call in self.client.calls], ["create", "submit"])
        self.assertEqual(self.client.calls[1][1][4], "  Original task instruction\n")
        self.assertEqual(self.environment.workspace_binding.instance_id, "instance")
        self.assertFalse(any(call[0] == "activate" for call in self.provisioner.calls))
        self.assertTrue(self.allocation.creation_started)
        await self.environment.start(force_build=False)
        self.assertEqual(len(self.client.calls), 2)

    async def test_setup_stops_on_original_run_terminal_without_claiming_an_instance_or_replaying_submit(self):
        for status in ("completed", "failed", "cancelled"):
            with self.subTest(status=status):
                client, allocation = PublicClient(), Allocation()
                observations, notifications = [], []
                def run_fact(session, run):
                    observations.append((session, run))
                    return {"agentRunId": run, "status": status}
                client.run_fact = run_fact
                client.read_run_events = lambda *args, **kwargs: self.fail("pre-admission failure has no committed events")
                environment = WorkspaceEnvironment(**{**self.arguments,
                    "workspace_client": client, "allocation": allocation,
                    "setup_timeout_sec": 0.05, "workspace_event_callback": notifications.append})
                self.provisioner.observe_setup = lambda *args: None
                error = "benchmark_setup_agent_run_" + status
                with self.assertRaisesRegex(WorkspaceError, error):
                    await environment.start()
                with self.assertRaisesRegex(WorkspaceError, error):
                    await environment.start()
                self.assertEqual(observations, [("session", "run")])
                self.assertEqual([call[0] for call in client.calls], ["create", "submit"])
                self.assertIsNone(environment.workspace_binding)
                self.assertFalse(allocation.released)
                self.assertEqual(notifications, [{"sessionId": "session", "agentRunId": "run",
                    "environmentId": "environment", "phase": "setup", "status": "failed", "error": error}])
                with self.assertRaisesRegex(WorkspaceError, "cleanup_pending"):
                    await environment.stop(delete=True)
                self.assertFalse(allocation.released)
                self.assertFalse(any(call[0] in {"activate", "exec", "retire"} for call in self.provisioner.calls))

    async def test_setup_observes_original_run_while_waiting_then_uses_original_instance(self):
        observe = self.provisioner.observe_setup
        def observe_pending(*args):
            return None if len(reads) < 2 else observe(*args)
        reads = []
        def run_fact(session, run):
            reads.append((session, run))
            return {"agentRunId": run, "status": "running"}
        self.client.run_fact = run_fact
        self.provisioner.observe_setup = observe_pending
        await self.environment.start()
        self.assertEqual(reads, [("session", "run"), ("session", "run")])
        self.assertEqual(self.environment.workspace_binding.instance_id, "instance")
        self.assertEqual(sum(call[0] == "submit" for call in self.client.calls), 1)

    async def test_setup_public_observation_unavailable_does_not_invent_terminal_or_cleanup(self):
        observe = self.provisioner.observe_setup
        waiting = [True]
        def observe_pending(*args):
            return None if waiting.pop() else observe(*args)
        def unavailable(*args, **kwargs):
            waiting.append(False)
            raise WorkspaceError("transport_unavailable")
        self.provisioner.observe_setup = observe_pending
        self.client.run_fact = unavailable
        notifications = []
        self.environment.workspace_event_callback = notifications.append
        await self.environment.start()
        self.assertEqual(self.environment.workspace_binding.instance_id, "instance")
        self.assertFalse(self.allocation.released)
        self.assertFalse(any(item.get("status") == "failed" for item in notifications))

    async def test_agent_adopts_same_run_then_activates_without_second_prompt(self):
        await self.environment.start(force_build=False)
        agent = WorkspaceAgent(logs_dir=self.root / "agent", model_config_ref="model", thinking_mode="high")
        await agent.setup(self.environment)
        await agent.run("  Original task instruction\n", self.environment, AgentContext())
        self.assertEqual(sum(call[0] == "submit" for call in self.client.calls), 1)
        self.assertEqual(sum(call[0] == "activate" for call in self.provisioner.calls), 1)

    async def test_setup_verifier_and_artifacts_use_exact_owned_instance(self):
        await self.environment.start(force_build=False)
        result = await self.environment.exec("verify command", cwd="/app", user="1000", timeout_sec=120)
        self.assertEqual(result.stdout, "same instance")
        source, target = self.root / "source", self.root / "download"
        source.write_bytes(b"bytes")
        await self.environment.upload_file(source, "/tests/input")
        await self.environment.download_file("/tests/input", target)
        self.assertEqual(target.read_bytes(), b"bytes")
        self.assertEqual({call[1] for call in self.provisioner.calls if call[0] in {"exec", "upload", "download"}}, {"instance"})

    async def test_destroyed_response_without_physical_absence_never_releases_admission(self):
        await self.environment.start(force_build=False)
        with self.assertRaisesRegex(WorkspaceError, "cleanup_pending"):
            await self.environment.stop(delete=True)
        self.assertFalse(self.allocation.released)
        self.provisioner.physical, self.provisioner.debit = "absent", 0
        await self.environment.stop(delete=True)
        self.assertTrue(self.allocation.released)
        ids = [call[2] for call in self.provisioner.calls if call[0] == "retire"]
        self.assertEqual(len(set(ids)), 1)

    async def test_lost_delete_ack_reconciles_absence_and_zero_debit(self):
        notifications = []
        self.environment.workspace_event_callback = notifications.append
        await self.environment.start(force_build=False)
        self.provisioner.fail_retire = True
        self.provisioner.physical, self.provisioner.debit = "absent", 0
        await self.environment.stop(delete=True)
        self.assertTrue(self.allocation.released)
        self.assertEqual(notifications[-1]["cleanupStatus"], "absent")
        self.assertNotIn("status", notifications[-1])

    async def test_absent_container_with_live_debit_stays_pending(self):
        await self.environment.start(force_build=False)
        self.provisioner.physical = "absent"
        with self.assertRaisesRegex(WorkspaceError, "cleanup_pending"):
            await self.environment.stop(delete=True)
        self.assertFalse(self.allocation.released)

    async def test_keep_request_does_not_release_or_destroy(self):
        await self.environment.start(force_build=False)
        await self.environment.stop(delete=False)
        self.assertFalse(self.allocation.released)
        self.assertFalse(any(call[0] == "retire" for call in self.provisioner.calls))

    async def test_changed_instruction_or_model_is_rejected_before_activation(self):
        await self.environment.start(force_build=False)
        agent = WorkspaceAgent(logs_dir=self.root / "agent", model_config_ref="different-model")
        with self.assertRaisesRegex(WorkspaceError, "submission_binding_mismatch"):
            await agent.run("different task", self.environment, AgentContext())
        self.assertFalse(any(call[0] == "activate" for call in self.provisioner.calls))

    async def test_cleanup_before_any_create_can_release_without_physical_mutation(self):
        await self.environment.stop(delete=True)
        self.assertTrue(self.allocation.released)
        self.assertEqual(self.provisioner.calls, [])
        with self.assertRaisesRegex(WorkspaceError, "environment_destroyed"):
            await self.environment.start(force_build=False)

    async def test_cleanup_after_lost_start_ack_only_queries_original_identity(self):
        submit = self.client.submit_message
        receipt = []
        def lost(*args):
            receipt.append(submit(*args))
            raise WorkspaceError("transport_unavailable")
        self.client.submit_message = lost
        with self.assertRaises(WorkspaceError):
            await self.environment.start(force_build=False)
        self.client.operation = lambda workspace, command, operation: receipt[0]
        self.provisioner.physical, self.provisioner.debit = "absent", 0
        await self.environment.stop(delete=True)
        self.assertTrue(self.allocation.released)
        self.assertEqual(sum(call[0] == "submit" for call in self.client.calls), 1)

    async def test_cleanup_release_interruption_can_be_reconciled_again(self):
        await self.environment.start(force_build=False)
        self.provisioner.physical, self.provisioner.debit = "absent", 0
        release = self.allocation.release
        async def interrupted(**kwargs):
            raise asyncio.CancelledError()
        self.allocation.release = interrupted
        with self.assertRaises(asyncio.CancelledError):
            await self.environment.stop(delete=True)
        self.assertFalse(self.allocation.released)
        self.allocation.release = release
        await self.environment.stop(delete=True)
        self.assertTrue(self.allocation.released)

    async def test_ready_instance_is_rejected_before_harbor_can_mutate_setup(self):
        observe = self.provisioner.observe_setup
        def already_running(*args):
            from dataclasses import replace
            return replace(observe(*args), setup_state="ready")
        self.provisioner.observe_setup = already_running
        with self.assertRaisesRegex(WorkspaceError, "prepared_environment_binding_mismatch"):
            await self.environment.start(force_build=False)
        self.assertIsNone(self.environment.workspace_binding)
        self.assertFalse(self.allocation.released)

    async def test_exec_transport_failure_is_not_replayed(self):
        await self.environment.start(force_build=False)
        calls = []
        def fail(*args, **kwargs):
            calls.append(kwargs)
            raise WorkspaceError("transport_unavailable")
        self.provisioner.exec = fail
        with self.assertRaises(WorkspaceError):
            await self.environment.exec("write side effect")
        self.assertEqual(len(calls), 1)

    async def test_harbor_log_hints_are_copied_not_mounted_and_arbitrary_host_mounts_rejected(self):
        paths = self.arguments["trial_paths"]
        mount = {"type": "bind", "source": str(paths.agent_dir), "target": "/logs/agent"}
        environment = WorkspaceEnvironment(**self.arguments, mounts=[mount])
        self.assertFalse(environment.capabilities.mounted)
        await environment.start(force_build=False)
        self.assertFalse(any(call[0] == "mount" for call in self.provisioner.calls))
        with self.assertRaisesRegex(WorkspaceError, "task_mounts_or_environment_unsupported"):
            WorkspaceEnvironment(**self.arguments, mounts=[{**mount, "source": str(self.root.parent / "private")}])

    async def test_pre_admission_uses_harbors_canonical_canary_stripped_instruction(self):
        instruction = self.arguments["environment_dir"].parent / "instruction.md"
        instruction.write_text("<!-- benchmark canary marker -->\n\nExact task\n", encoding="utf-8")
        await self.environment.start(force_build=False)
        self.assertEqual(self.client.calls[1][1][4], "Exact task\n")


if __name__ == "__main__":
    unittest.main()
