"""Harbor adapter contract tests, using fake execs and no model credentials."""
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

    async def test_failed_result_is_recorded_and_cancelled(self):
        with tempfile.TemporaryDirectory() as directory:
            agent = CentaerisAgent(logs_dir=Path(directory), model_name="mock",
                                   provider_id="custom.test", credential_env="TEST_MODEL_KEY", reasoning_effort="max",
                                   extra_env={"TEST_MODEL_KEY": "fake-test-key"})
            context, environment = AgentContext(), Environment("failed")
            with self.assertRaisesRegex(RuntimeError, "Centaeris run failed: failed"):
                await agent.run("task", environment, context)
            self.assertEqual(context.metadata["agentRunId"], "run")
            self.assertTrue(any(" cancel " in command for command, _ in environment.calls))


if __name__ == "__main__":
    unittest.main()
