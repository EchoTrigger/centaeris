"""Harbor adapter contract tests, using fake execs and no model credentials."""
import asyncio
import io
import json
from pathlib import Path
import shlex
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import PropertyMock, patch

try:
    from centaeris_agent import CentaerisAgent, AgentContext
except ModuleNotFoundError as error:
    if error.name != "harbor":
        raise
    CentaerisAgent = None


class Environment:
    def __init__(self, status="succeeded"):
        self.status = status
        self.calls = []
        self.uploads = []
        self.default_user = "1000"

    async def upload_file(self, source, target):
        self.uploads.append((target, Path(source).read_bytes()))

    async def upload_dir(self, source, target):
        self.uploads.append((target, b"directory"))

    async def exec(self, command, **kwargs):
        self.calls.append((command, kwargs))
        if command == "pwd -P":
            return SimpleNamespace(return_code=0, stdout="/app\n", stderr="")
        if command.endswith(" --runtime-server-endpoint"):
            return SimpleNamespace(return_code=0, stdout=json.dumps({"endpoint": "/tmp/preflight/runtime.sock"}), stderr="")
        if " wait " in command:
            return SimpleNamespace(return_code=0 if self.status == "succeeded" else 1,
                                   stdout=json.dumps({"status": self.status, "sessionId": "session",
                                                      "agentRunId": "run", 'providerUsage': {'totals': {
                                                          'inputTokens': 120, 'outputTokens': 30,
                                                          'promptCacheHitTokens': 90}}, "modelBudget": {
                                                          "contextTokens": 500000, "maxOutputTokens": 64000}}))
        return SimpleNamespace(return_code=0, stdout="", stderr="")


class PythonVersionEnvironment(Environment):
    def __init__(self, version):
        super().__init__()
        self.version = version

    async def exec(self, command, **kwargs):
        if "sys.version_info" not in command:
            return await super().exec(command, **kwargs)
        self.calls.append((command, kwargs))
        output = io.StringIO()
        with patch("sys.version_info", tuple(int(value) for value in self.version.split("."))), \
                patch("sys.version", self.version), patch("sys.stdout", output):
            try:
                exec(shlex.split(command)[2])
            except SystemExit as error:
                return SimpleNamespace(return_code=error.code, stdout=output.getvalue(), stderr="")
        raise AssertionError("interpreter preflight must provide an exit status")


@unittest.skipUnless(CentaerisAgent, "requires Harbor 0.21.0")
class AgentTests(unittest.IsolatedAsyncioTestCase):
    async def test_cancelled_attempt_recovers_committed_usage_without_changing_cancellation(self):
        class TimedEnvironment(Environment):
            async def exec(self, command, **kwargs):
                if " wait " in command:
                    raise asyncio.CancelledError()
                return await super().exec(command, **kwargs)

            async def download_file(self, source, destination):
                self_source = source.endswith("/provider-usage.json")
                if not self_source:
                    raise AssertionError("only the usage artifact may be recovered")
                Path(destination).write_text(json.dumps({
                    "providerUsage": {"nRequests": 2, "totals": {
                        "inputTokens": 100, "outputTokens": 20, "promptCacheHitTokens": 80}},
                    "usageCoverage": {"source": "committedProviderUsage", "terminalObserved": True,
                                      "inFlightRequestMayBeMissing": True}}))
        with tempfile.TemporaryDirectory() as directory:
            agent = CentaerisAgent(logs_dir=Path(directory), model_name="mock",
                                   provider_id="custom.test", credential_env="TEST_MODEL_KEY",
                                   extra_env={"TEST_MODEL_KEY": "fake-test-key"})
            context = AgentContext()
            with self.assertRaises(asyncio.CancelledError):
                await agent.run("task", TimedEnvironment(), context)
            self.assertEqual((context.n_input_tokens, context.n_output_tokens, context.n_cache_tokens), (100, 20, 80))
            self.assertTrue(context.metadata["usageCoverage"]["inFlightRequestMayBeMissing"])

    async def test_official_long_task_deadline_is_forwarded_without_a_fixed_timeout(self):
        with tempfile.TemporaryDirectory() as directory:
            trial = "long-task__attempt"
            logs = Path(directory) / trial / "agent"
            logs.mkdir(parents=True)
            manifest = Path(directory) / "private-deadlines.json"
            manifest.write_text(json.dumps({"schemaVersion": "centaeris.harbor.deadlines.v1",
                                           "trials": {trial: {"agentTimeoutSec": 7200}}}), encoding="utf-8")
            agent = CentaerisAgent(logs_dir=logs, model_name="mock", provider_id="custom.test",
                                   credential_env="TEST_MODEL_KEY", deadline_manifest_path=str(manifest),
                                   extra_env={"TEST_MODEL_KEY": "fake-test-key"})
            environment, context = Environment(), AgentContext()
            with patch("time.time", return_value=1000):
                await agent.run("task", environment, context)
            command = next(command for command, _ in environment.calls if command.startswith("nohup "))
            arguments = shlex.split(command)
            self.assertEqual(int(arguments[arguments.index("--deadline-at-ms") + 1]), 8_235_000)
            self.assertNotIn("--timeout", arguments)
            self.assertNotIn("--no-timeout", arguments)
            wait = next(command for command, _ in environment.calls if " wait " in command)
            self.assertIn("--no-timeout", shlex.split(wait))
            self.assertEqual(context.metadata["evaluationDeadlineAtMs"], 8_235_000)
            self.assertNotIn(str(manifest), command)
            self.assertNotIn("fake-test-key", command)

    async def test_missing_official_deadline_entry_prevents_model_launch(self):
        with tempfile.TemporaryDirectory() as directory:
            logs = Path(directory) / "missing-task__attempt" / "agent"
            logs.mkdir(parents=True)
            manifest = Path(directory) / "private-deadlines.json"
            manifest.write_text(json.dumps({"schemaVersion": "centaeris.harbor.deadlines.v1",
                                           "trials": {"different-task": {"agentTimeoutSec": 7200}}}), encoding="utf-8")
            agent = CentaerisAgent(logs_dir=logs, model_name="mock", provider_id="custom.test",
                                   credential_env="TEST_MODEL_KEY", deadline_manifest_path=str(manifest),
                                   extra_env={"TEST_MODEL_KEY": "fake-test-key"})
            environment = Environment()
            with self.assertRaisesRegex(ValueError, "deadline.*missing-task__attempt"):
                await agent.run("task", environment, AgentContext())
            self.assertFalse(any(command.startswith("nohup ") for command, _ in environment.calls))

    async def test_invalid_official_deadline_is_rejected_before_launch(self):
        for timeout in (0, -1, True, "7200", None, float("nan"), float("inf")):
            with self.subTest(timeout=timeout), tempfile.TemporaryDirectory() as directory:
                trial = "task__attempt"
                logs = Path(directory) / trial / "agent"
                logs.mkdir(parents=True)
                manifest = Path(directory) / "private-deadlines.json"
                manifest.write_text(json.dumps({"schemaVersion": "centaeris.harbor.deadlines.v1",
                                               "trials": {trial: {"agentTimeoutSec": timeout}}}), encoding="utf-8")
                agent = CentaerisAgent(logs_dir=logs, model_name="mock", provider_id="custom.test",
                                       credential_env="TEST_MODEL_KEY", deadline_manifest_path=str(manifest),
                                       extra_env={"TEST_MODEL_KEY": "fake-test-key"})
                environment = Environment()
                with self.assertRaisesRegex(ValueError, "agentTimeoutSec"):
                    await agent.run("task", environment, AgentContext())
                self.assertFalse(any(command.startswith("nohup ") for command, _ in environment.calls))

    async def test_deadline_manifest_rejects_unknown_schema_and_fields_before_reading_credentials(self):
        class CredentialsMustNotBeRead(dict):
            def get(self, name, *args):
                if name == "TEST_MODEL_KEY":
                    raise AssertionError("invalid deadline must fail before credentials are read")
                return super().get(name, *args)
        for invalid in (
            {"schemaVersion": "old.deadlines", "trials": {"task__attempt": {"agentTimeoutSec": 900}}},
            {"schemaVersion": "centaeris.harbor.deadlines.v1", "trials": {}, "timeoutSec": 900},
            {"schemaVersion": "centaeris.harbor.deadlines.v1", "trials": {
                "task__attempt": {"agentTimeoutSec": 900, "timeout": 900}}},
        ):
            with self.subTest(manifest=invalid), tempfile.TemporaryDirectory() as directory:
                logs = Path(directory) / "task__attempt" / "agent"
                logs.mkdir(parents=True)
                manifest = Path(directory) / "private-deadlines.json"
                manifest.write_text(json.dumps(invalid), encoding="utf-8")
                agent = CentaerisAgent(logs_dir=logs, model_name="mock", provider_id="custom.test",
                                       credential_env="TEST_MODEL_KEY", deadline_manifest_path=str(manifest))
                environment = Environment()
                with patch.object(CentaerisAgent, "extra_env", new_callable=PropertyMock,
                                  return_value=CredentialsMustNotBeRead({"TEST_MODEL_KEY": "fake-test-key"})), \
                        self.assertRaisesRegex(ValueError, "deadline"):
                    await agent.run("task", environment, AgentContext())
                self.assertEqual(environment.calls, [])

    async def test_deadline_identity_survives_cancellation(self):
        class CancelledEnvironment(Environment):
            async def exec(self, command, **kwargs):
                if " wait " in command:
                    raise asyncio.CancelledError()
                return await super().exec(command, **kwargs)
        with tempfile.TemporaryDirectory() as directory:
            trial = "task__attempt"
            logs = Path(directory) / trial / "agent"
            logs.mkdir(parents=True)
            manifest = Path(directory) / "private-deadlines.json"
            manifest.write_text(json.dumps({"schemaVersion": "centaeris.harbor.deadlines.v1",
                                           "trials": {trial: {"agentTimeoutSec": 900}}}), encoding="utf-8")
            agent = CentaerisAgent(logs_dir=logs, model_name="mock", provider_id="custom.test",
                                   credential_env="TEST_MODEL_KEY", deadline_manifest_path=str(manifest),
                                   extra_env={"TEST_MODEL_KEY": "fake-test-key"})
            context = AgentContext()
            with patch("time.time", return_value=1000), self.assertRaises(asyncio.CancelledError):
                await agent.run("task", CancelledEnvironment(), context)
            self.assertEqual(context.metadata["evaluationDeadlineAtMs"], 1_935_000)

    async def test_install_rejects_old_python_before_uploading_or_reading_credentials(self):
        class CredentialsMustNotBeRead(dict):
            def get(self, name, *args):
                if name == "TEST_MODEL_KEY":
                    raise AssertionError("interpreter preflight must precede credentials")
                return super().get(name, *args)
        with tempfile.TemporaryDirectory() as directory:
            binary = Path(directory) / "runtime"
            binary.write_bytes(b"\x7fELFtest")
            agent = CentaerisAgent(logs_dir=Path(directory), runtime_binary=str(binary),
                                   credential_env="TEST_MODEL_KEY")
            environment = PythonVersionEnvironment("3.8.20")
            with patch.object(CentaerisAgent, "extra_env", new_callable=PropertyMock,
                              return_value=CredentialsMustNotBeRead()), \
                    self.assertRaisesRegex(RuntimeError, "Python 3.9.*3.8.20"):
                await agent.install(environment)
            self.assertEqual(environment.uploads, [])
            self.assertFalse(any(command.startswith("nohup ") for command, _ in environment.calls))

    async def test_install_accepts_supported_task_python_versions(self):
        for version in ("3.9.2", "3.10.16", "3.12.10", "3.13.7"):
            with self.subTest(version=version), tempfile.TemporaryDirectory() as directory:
                binary = Path(directory) / "runtime"
                binary.write_bytes(b"\x7fELFtest")
                agent = CentaerisAgent(logs_dir=Path(directory), runtime_binary=str(binary))
                environment = PythonVersionEnvironment(version)
                await agent.install(environment)
                check = next(command for command, _ in environment.calls if "sys.version_info" in command)
                self.assertTrue(environment.uploads)

    async def test_install_rejects_an_actual_dynamic_loader_failure_before_model_launch(self):
        class IncompatibleEnvironment(Environment):
            async def exec(self, command, **kwargs):
                result = await super().exec(command, **kwargs)
                if command.endswith(" --runtime-server-endpoint"):
                    result.return_code = 127
                    result.stdout = None
                    result.stderr = "libc.so.6: version `GLIBC_2.34' not found"
                return result
        with tempfile.TemporaryDirectory() as directory:
            binary = Path(directory) / "runtime"
            binary.write_bytes(b"\x7fELFtest")
            agent = CentaerisAgent(logs_dir=Path(directory), runtime_binary=str(binary))
            environment = IncompatibleEnvironment()
            with self.assertRaisesRegex(RuntimeError, "preflight.*127.*GLIBC_2.34"):
                await agent.install(environment)
            self.assertFalse(any(command.startswith("nohup ") for command, _ in environment.calls))

    async def test_install_preflight_uses_an_isolated_profile_and_empty_environment(self):
        class CredentialsMustNotBeRead(dict):
            def get(self, name, *args):
                if name == "TEST_MODEL_KEY":
                    raise AssertionError("preflight must not read model credentials")
                return super().get(name, *args)
        with tempfile.TemporaryDirectory() as directory:
            binary = Path(directory) / "runtime"
            binary.write_bytes(b"\x7fELFtest")
            agent = CentaerisAgent(logs_dir=Path(directory), runtime_binary=str(binary),
                                   credential_env="TEST_MODEL_KEY")
            environment = Environment()
            with patch.object(CentaerisAgent, "extra_env", new_callable=PropertyMock,
                              return_value=CredentialsMustNotBeRead({"TEST_MODEL_KEY": "fake-test-key"})):
                await agent.install(environment)
            probes = [(command, options) for command, options in environment.calls
                      if command.endswith(" --runtime-server-endpoint")]
            self.assertEqual(len(probes), 1)
            command, options = probes[0]
            arguments = shlex.split(command)
            self.assertEqual(arguments[:2], ["env", "-i"])
            self.assertIn("CENTAERIS_DESKTOP_DATA_DIR=/tmp/centaeris-preflight-" + agent.attempt, arguments)
            self.assertEqual(arguments[-2], agent.remote + "/centaeris-runtime")
            self.assertNotIn("fake-test-key", command)
            self.assertNotIn("TEST_MODEL_KEY", command)
            self.assertNotIn("TEST_MODEL_KEY", options.get("env", {}))
            self.assertEqual(options["cwd"], "/")
            self.assertEqual(options["timeout_sec"], 30)
            chmod_index = next(index for index, (command, _) in enumerate(environment.calls)
                               if "chmod a+x" in command)
            probe_index = next(index for index, (command, _) in enumerate(environment.calls)
                               if command.endswith(" --runtime-server-endpoint"))
            self.assertLess(chmod_index, probe_index)

    async def test_session_uses_the_environment_working_directory(self):
        class WorkspaceEnvironment(Environment):
            async def exec(self, command, **kwargs):
                result = await super().exec(command, **kwargs)
                if command == "pwd -P":
                    result.stdout = "/workspace/project with spaces\n"
                return result
        with tempfile.TemporaryDirectory() as directory:
            agent = CentaerisAgent(logs_dir=Path(directory), model_name="mock",
                                   provider_id="custom.test", credential_env="TEST_MODEL_KEY",
                                   extra_env={"TEST_MODEL_KEY": "fake-test-key"})
            environment = WorkspaceEnvironment()
            await agent.run("task", environment, AgentContext())
            command = next(command for command, _ in environment.calls if command.startswith("nohup "))
            arguments = shlex.split(command)
            self.assertEqual(arguments[arguments.index("--cwd") + 1], "/workspace/project with spaces")
            probe = next(options for command, options in environment.calls if command == "pwd -P")
            self.assertNotIn("cwd", probe)

    async def test_working_directory_probe_failure_prevents_model_launch(self):
        class MissingDirectoryEnvironment(Environment):
            async def exec(self, command, **kwargs):
                if command == "pwd -P":
                    self.calls.append((command, kwargs))
                    return SimpleNamespace(return_code=1, stdout=None, stderr="Cannot resolve current directory")
                return await super().exec(command, **kwargs)
        with tempfile.TemporaryDirectory() as directory:
            agent = CentaerisAgent(logs_dir=Path(directory), model_name="mock",
                                   provider_id="custom.test", credential_env="TEST_MODEL_KEY",
                                   extra_env={"TEST_MODEL_KEY": "fake-test-key"})
            environment = MissingDirectoryEnvironment()
            with self.assertRaisesRegex(RuntimeError, "working directory.*Cannot resolve current directory"):
                await agent.run("task", environment, AgentContext())
            self.assertFalse(any(command.startswith("nohup ") for command, _ in environment.calls))

    async def test_missing_supervisor_stdout_preserves_transport_diagnostic_and_cancels(self):
        class EmptyResultEnvironment(Environment):
            async def exec(self, command, **kwargs):
                result = await super().exec(command, **kwargs)
                if " wait " in command:
                    result.stdout = missing_stdout
                    result.return_code = 139
                    result.stderr = "supervisor connection closed"
                return result
        for missing_stdout in (None, "", " \n"):
            with self.subTest(stdout=missing_stdout), tempfile.TemporaryDirectory() as directory:
                agent = CentaerisAgent(logs_dir=Path(directory), model_name="mock",
                                       provider_id="custom.test", credential_env="TEST_MODEL_KEY",
                                       extra_env={"TEST_MODEL_KEY": "fake-test-key"})
                environment = EmptyResultEnvironment()
                with self.assertRaisesRegex(RuntimeError, "no result.*139.*supervisor connection closed"):
                    await agent.run("task", environment, AgentContext())
                self.assertEqual(sum(" cancel " in command for command, _ in environment.calls), 1)

    async def test_deadline_cleanup_failure_preserves_cancellation_and_other_trial(self):
        class BrokenCleanupEnvironment(Environment):
            async def exec(self, command, **kwargs):
                if " wait " in command:
                    raise asyncio.CancelledError()
                if " cancel " in command:
                    raise RuntimeError("Command timed out after 20 seconds")
                return await super().exec(command, **kwargs)
        with tempfile.TemporaryDirectory() as directory:
            agent = CentaerisAgent(logs_dir=Path(directory), model_name="mock",
                                   provider_id="custom.test", credential_env="TEST_MODEL_KEY",
                                   extra_env={"TEST_MODEL_KEY": "fake-test-key"})
            context = AgentContext()
            completed = []
            async def other_trial():
                await asyncio.sleep(0)
                completed.append(True)
            async with asyncio.TaskGroup() as group:
                cancelled = group.create_task(agent.run("task", BrokenCleanupEnvironment(), context))
                group.create_task(other_trial())
            self.assertTrue(cancelled.cancelled())
            self.assertEqual(completed, [True])
            self.assertEqual(context.metadata["cancellationCleanupError"]["type"], "RuntimeError")
            self.assertIn("20 seconds", context.metadata["cancellationCleanupError"]["message"])

    async def test_bridge_accepts_an_explicit_provider_and_credential_variable(self):
        with tempfile.TemporaryDirectory() as directory:
            agent = CentaerisAgent(logs_dir=Path(directory), model_name="mock",
                                   provider_id="custom.test", credential_env="TEST_MODEL_KEY",
                                   reasoning_effort="medium",
                                   extra_env={"TEST_MODEL_KEY": "fake-test-key"})
            environment = Environment()
            await agent.run("task", environment, AgentContext())
            command, options = next(call for call in environment.calls if call[0].startswith("nohup "))
            self.assertIn("--provider custom.test", command)
            self.assertIn("--effort medium", command)
            self.assertIn("--credential-env TEST_MODEL_KEY", command)
            self.assertEqual(options["env"]["TEST_MODEL_KEY"], "fake-test-key")
            self.assertNotIn("fake-test-key", command)

    async def test_pilot_task_sources_have_a_progress_metric(self):
        from harbor.job import Job
        from harbor.models.job.config import JobConfig
        config = JobConfig.model_validate_json(Path(__file__).with_name("five-tasks.json").read_text())
        metrics = await Job._resolve_metrics(config, config.tasks)
        for task in config.tasks:
            bucket = task.source or "adhoc"
            self.assertTrue(metrics[bucket], f"missing progress metric for {bucket}")
            self.assertEqual(metrics[bucket][0].compute([{"reward": 1.0}])["mean"], 1.0)
        self.assertEqual(config.n_concurrent_trials, 2)
        self.assertTrue(config.environment.delete)

    async def test_host_socks_proxy_is_translated_for_container_and_loopback_bypassed(self):
        with tempfile.TemporaryDirectory() as directory, patch.dict("os.environ", {
                "HTTPS_PROXY": "socks5h://127.0.0.1:10808", "HTTP_PROXY": "socks5h://127.0.0.1:10808"}, clear=True):
            agent = CentaerisAgent(logs_dir=Path(directory), model_name="mock",
                                   provider_id="custom.test", credential_env="TEST_MODEL_KEY", reasoning_effort="max",
                                   extra_env={"TEST_MODEL_KEY": "fake-test-key"})
            environment = Environment()
            await agent.run("task", environment, AgentContext())
            _, options = next(call for call in environment.calls if call[0].startswith("nohup "))
            self.assertEqual(options["env"]["HTTPS_PROXY"], "socks5h://host.docker.internal:10808")
            self.assertIn("127.0.0.1", options["env"]["NO_PROXY"])

    async def test_installed_directories_are_created_as_root_and_owned_by_agent(self):
        with tempfile.TemporaryDirectory() as directory:
            binary = Path(directory) / "runtime"
            binary.write_bytes(b"\x7fELFtest")
            agent = CentaerisAgent(logs_dir=Path(directory), runtime_binary=str(binary))
            environment = Environment()
            await agent.install(environment)
            network = next(data for target, data in environment.uploads if target.endswith('/.curlrc'))
            self.assertIn(b'retry-all-errors', network)
            self.assertIn(b'retry-max-time = 120', network)
            command, options = next(call for call in environment.calls if "mkdir -p" in call[0])
            self.assertEqual(options.get("user"), "root")
            self.assertTrue(any("chown -R 1000" in command for command, _ in environment.calls))

    async def test_key_stays_in_exec_environment_and_budget_is_forwarded(self):
        with tempfile.TemporaryDirectory() as directory:
            agent = CentaerisAgent(logs_dir=Path(directory), model_name="mock",
                                   provider_id="custom.test", credential_env="TEST_MODEL_KEY", reasoning_effort="max",
                                   extra_env={"TEST_MODEL_KEY": "fake-test-key"})
            context, environment = AgentContext(), Environment()
            await agent.run("Solve task", environment, context)
            command, options = next(call for call in environment.calls if call[0].startswith("nohup "))
            self.assertNotIn("fake-test-key", command)
            self.assertEqual(options["env"]["TEST_MODEL_KEY"], "fake-test-key")
            self.assertIn("--context-tokens 500000 --output-tokens 64000", command)
            self.assertEqual(context.metadata["agentRunId"], "run")
            self.assertEqual((context.n_input_tokens, context.n_output_tokens, context.n_cache_tokens), (120, 30, 90))
            self.assertEqual(environment.uploads[0][1], b"Solve task")

    async def test_terminal_result_preserves_usage_coverage_and_export_failure(self):
        coverage = {"source": "committedProviderUsage", "terminalObserved": True,
                    "inFlightRequestMayBeMissing": False}
        error = {"type": "TimeoutError", "workerThreadMayContinue": True}
        cleanup = [{"stage": "close", "type": "ConnectionError", "message": "connection closed"}]
        class UsageEnvironment(Environment):
            async def exec(self, command, **kwargs):
                result = await super().exec(command, **kwargs)
                if " wait " in command:
                    payload = json.loads(result.stdout)
                    payload.update(usageCoverage=coverage, usageExportError=error, cleanupErrors=cleanup)
                    result.stdout = json.dumps(payload)
                return result
        for status in ("succeeded", "failed"):
            with tempfile.TemporaryDirectory() as directory:
                agent = CentaerisAgent(logs_dir=Path(directory), model_name="mock",
                                       provider_id="custom.test", credential_env="TEST_MODEL_KEY",
                                       extra_env={"TEST_MODEL_KEY": "fake-test-key"})
                context, environment = AgentContext(), UsageEnvironment(status)
                if status == "failed":
                    with self.assertRaisesRegex(RuntimeError, "Centaeris run failed: failed"):
                        await agent.run("task", environment, context)
                else:
                    await agent.run("task", environment, context)
                self.assertEqual(context.metadata["usageCoverage"], coverage)
                self.assertEqual(context.metadata["usageExportError"], error)
                self.assertEqual(context.metadata["cleanupErrors"], cleanup)
                self.assertEqual(context.n_input_tokens, 120)
                self.assertFalse(any(" cancel " in command for command, _ in environment.calls))

    async def test_trajectory_storage_failure_is_metadata_without_cancelling_success(self):
        health = {"mirrorErrorErrno": 5, "spoolErrorErrno": None,
                  "unspooledEvents": 0, "mirrorRestored": False, "spoolSnapshotSaved": True}
        recovery_fails = False
        class FailedMirrorEnvironment(Environment):
            async def download_file(self, source_path, target_path):
                if recovery_fails:
                    raise OSError(5, "Artifact transport failed")
                Path(target_path).write_text("{}\n", encoding="utf-8")

            async def exec(self, command, **kwargs):
                result = await super().exec(command, **kwargs)
                if " wait " in command:
                    payload = json.loads(result.stdout)
                    payload["trajectoryPersistence"] = health
                    result.stdout = json.dumps(payload)
                return result
        for recovery_fails in (False, True):
            with tempfile.TemporaryDirectory() as directory:
                agent = CentaerisAgent(logs_dir=Path(directory), model_name="mock",
                                       provider_id="custom.test", credential_env="TEST_MODEL_KEY",
                                       extra_env={"TEST_MODEL_KEY": "fake-test-key"})
                context, environment = AgentContext(), FailedMirrorEnvironment()
                await agent.run("task", environment, context)
                recorded = context.metadata["trajectoryPersistence"]
                self.assertEqual({key: recorded[key] for key in health}, health)
                if recovery_fails:
                    self.assertEqual(recorded["recoveryErrorType"], "OSError")
                else:
                    artifact = Path(directory) / recorded["recoveredArtifactPath"]
                    self.assertEqual(artifact.read_text(), "{}\n")
                self.assertFalse(any(" cancel " in command for command, _ in environment.calls))

    async def test_terminal_failure_is_recorded_without_redundant_cancellation(self):
        with tempfile.TemporaryDirectory() as directory:
            agent = CentaerisAgent(logs_dir=Path(directory), model_name="mock",
                                   provider_id="custom.test", credential_env="TEST_MODEL_KEY", reasoning_effort="max",
                                   extra_env={"TEST_MODEL_KEY": "fake-test-key"})
            context, environment = AgentContext(), Environment("failed")
            with self.assertRaisesRegex(RuntimeError, "Centaeris run failed: failed"):
                await agent.run("task", environment, context)
            self.assertEqual(context.metadata["agentRunId"], "run")
            self.assertFalse(any(" cancel " in command for command, _ in environment.calls))

    async def test_harbor_owns_deadline_and_completed_service_lifetime(self):
        with tempfile.TemporaryDirectory() as directory:
            agent = CentaerisAgent(logs_dir=Path(directory), model_name="mock",
                                   provider_id="custom.test", credential_env="TEST_MODEL_KEY",
                                   extra_env={"TEST_MODEL_KEY": "fake-test-key"})
            environment = Environment()
            await agent.run("task", environment, AgentContext())
            launch = next(command for command, _ in environment.calls if command.startswith("nohup "))
            wait, options = next(call for call in environment.calls if " wait " in call[0])
            self.assertIn("--no-timeout", launch)
            self.assertIn("--keep-alive", launch)
            self.assertIn("--no-timeout", wait)
            self.assertNotIn("timeout_sec", options)

    async def test_transport_failure_still_cancels_the_admitted_run(self):
        class BrokenEnvironment(Environment):
            async def exec(self, command, **kwargs):
                if " wait " in command:
                    raise ConnectionError("lost transport")
                return await super().exec(command, **kwargs)
        with tempfile.TemporaryDirectory() as directory:
            agent = CentaerisAgent(logs_dir=Path(directory), model_name="mock",
                                   provider_id="custom.test", credential_env="TEST_MODEL_KEY",
                                   extra_env={"TEST_MODEL_KEY": "fake-test-key"})
            environment = BrokenEnvironment()
            with self.assertRaisesRegex(ConnectionError, "lost transport"):
                await agent.run("task", environment, AgentContext())
            self.assertTrue(any(" cancel " in command for command, _ in environment.calls))

    async def test_terminal_provider_error_retains_its_diagnostic(self):
        class FailedEnvironment(Environment):
            async def exec(self, command, **kwargs):
                result = await super().exec(command, **kwargs)
                if " wait " in command:
                    payload = json.loads(result.stdout)
                    payload["run"] = {"status": "failed", "error": "model input rejected"}
                    result.stdout = json.dumps(payload)
                return result
        with tempfile.TemporaryDirectory() as directory:
            agent = CentaerisAgent(logs_dir=Path(directory), model_name="mock",
                                   provider_id="custom.test", credential_env="TEST_MODEL_KEY",
                                   extra_env={"TEST_MODEL_KEY": "fake-test-key"})
            with self.assertRaisesRegex(RuntimeError, "model input rejected"):
                await agent.run("task", FailedEnvironment("failed"), AgentContext())


if __name__ == "__main__":
    unittest.main()
