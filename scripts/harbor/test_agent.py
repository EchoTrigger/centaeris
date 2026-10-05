"""Harbor adapter contract tests, using fake execs and no model credentials."""
import asyncio
import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

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
        if " wait " in command:
            return SimpleNamespace(return_code=0 if self.status == "succeeded" else 1,
                                   stdout=json.dumps({"status": self.status, "sessionId": "session",
                                                      "agentRunId": "run", 'providerUsage': {'totals': {
                                                          'inputTokens': 120, 'outputTokens': 30,
                                                          'promptCacheHitTokens': 90}}, "modelBudget": {
                                                          "contextTokens": 500000, "maxOutputTokens": 64000}}))
        return SimpleNamespace(return_code=0, stdout="", stderr="")


@unittest.skipUnless(CentaerisAgent, "requires Harbor 0.21.0")
class AgentTests(unittest.IsolatedAsyncioTestCase):
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
