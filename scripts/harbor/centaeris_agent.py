"""Harbor 0.21 installed-agent adapter; Core owns all agent behavior."""
from __future__ import annotations

import asyncio
import hashlib
import json
import os
from pathlib import Path
import shlex
import tempfile
import uuid
from urllib.parse import urlsplit, urlunsplit

from harbor.agents.installed.base import BaseInstalledAgent, with_prompt_template
from harbor.models.agent.context import AgentContext


class CentaerisAgent(BaseInstalledAgent):
    @staticmethod
    def name():
        return "centaeris"

    def __init__(self, *args, runtime_binary=None, provider_id=None, credential_env=None,
                 reasoning_effort=None, context_tokens=500_000, max_output_tokens=64_000,
                 **kwargs):
        super().__init__(*args, **kwargs)
        self.binary = Path(runtime_binary or os.environ.get("CENTAERIS_RUNTIME_BINARY", ""))
        self.provider = provider_id
        self.credential_env = credential_env or os.environ.get("CENTAERIS_BENCH_CREDENTIAL_ENV")
        self.effort = reasoning_effort
        self.context_tokens = int(context_tokens)
        self.output_tokens = int(max_output_tokens)
        if not 0 < self.output_tokens < self.context_tokens:
            raise ValueError("output budget must be positive and smaller than context budget")
        self.attempt = str(uuid.uuid4())
        self.remote = f"/installed-agent/centaeris-{self.attempt}"
        self.remote_logs = f"/logs/agent/centaeris-{self.attempt}"
        self._version = None

    def proxy_environment(self):
        sources = (self.extra_env, os.environ)
        def read(name):
            return next((source[key] for source in sources for key in (name, name.lower())
                         if source.get(key)), None)
        override = read("CENTAERIS_BENCH_PROXY_URL")
        env = {}
        for name in ("HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY"):
            value = override or read(name)
            if value:
                parsed = urlsplit(value)
                if parsed.hostname in ("localhost", "127.0.0.1", "::1"):
                    # Docker Desktop exposes the host through this DNS name.
                    # Preserve any credentials and the SOCKS remote-DNS scheme.
                    authority = parsed.netloc.rsplit("@", 1)
                    prefix = authority[0] + "@" if len(authority) == 2 else ""
                    host = "host.docker.internal" + (f":{parsed.port}" if parsed.port else "")
                    value = urlunsplit(parsed._replace(netloc=prefix + host))
                env[name] = env[name.lower()] = value
        bypass = read("NO_PROXY") or ""
        env["NO_PROXY"] = env["no_proxy"] = ",".join(filter(None, ["127.0.0.1,localhost,::1", bypass]))
        return env

    async def install(self, environment):
        if not self.binary.is_file():
            raise ValueError("set CENTAERIS_RUNTIME_BINARY to the prebuilt Linux runtime")
        with self.binary.open("rb") as binary:
            if binary.read(4) != b"\x7fELF":
                raise ValueError("Centaeris requires a Linux ELF binary, not the Windows executable")
            binary.seek(0)
            self._version = "sha256:" + hashlib.file_digest(binary, "sha256").hexdigest()
        result = await environment.exec(user="root", env=self.proxy_environment(), command=(
            "command -v python3 >/dev/null && command -v bash >/dev/null "
            "|| (apt-get update && apt-get install -y python3 bash ca-certificates)"))
        if result.return_code:
            raise RuntimeError("installing the headless client dependencies failed")
        result = await self.exec_as_root(environment, command=(
            f"mkdir -p {shlex.quote(self.remote)} {shlex.quote(self.remote_logs)}"))
        if result.return_code:
            raise RuntimeError("creating the isolated agent directories failed")
        if environment.default_user is not None:
            await self.exec_as_root(environment, command=(
                f"chown -R {shlex.quote(str(environment.default_user))} "
                f"{shlex.quote(self.remote)} {shlex.quote(self.remote_logs)}"))
        await environment.upload_file(self.binary, self.remote + "/centaeris-runtime")
        await environment.upload_file(Path(__file__).with_name("runner.py"), self.remote + "/runner.py")
        network_dir = '/installed-agent/centaeris-network'
        result = await self.exec_as_root(environment, command=f'mkdir -p {network_dir}')
        if result.return_code:
            raise RuntimeError('creating verifier network configuration failed')
        await environment.upload_file(Path(__file__).with_name('verifier.curlrc'), network_dir + '/.curlrc')
        result = await self.exec_as_root(environment, command=f'chmod a+rx {network_dir}; chmod a+r {network_dir}/.curlrc')
        if result.return_code:
            raise RuntimeError('setting verifier network configuration permissions failed')
        skills = Path(__file__).resolve().parents[2] / "system-skills"
        await environment.upload_dir(skills, self.remote + "/system-skills")
        result = await self.exec_as_root(environment, command=(
            f"chmod -R a+rX {shlex.quote(self.remote)}; "
            f"chmod a+x {shlex.quote(self.remote + '/centaeris-runtime')}"))
        if result.return_code:
            raise RuntimeError("setting installed runtime permissions failed")

    @with_prompt_template
    async def run(self, instruction, environment, context: AgentContext):
        if not self.credential_env:
            raise ValueError("credential_env is required")
        key = self.extra_env.get(self.credential_env) or os.environ.get(self.credential_env)
        if not key:
            raise ValueError(f"{self.credential_env} is required")
        if not self.model_name:
            raise ValueError("a model_name is required")
        if not self.provider:
            raise ValueError("provider_id is required")
        with tempfile.TemporaryDirectory() as temporary:
            instruction_file = Path(temporary) / "instruction.txt"
            instruction_file.write_text(instruction, encoding="utf-8")
            await environment.upload_file(instruction_file, self.remote + "/instruction.txt")
        await self.exec_as_root(environment, command=(
            "chmod a+r " + shlex.quote(self.remote + "/instruction.txt")))
        runner = ["python3", self.remote + "/runner.py", "run", "--logs", self.remote_logs,
                  "--runtime", self.remote + "/centaeris-runtime", "--attempt", self.attempt,
                  "--instruction", self.remote + "/instruction.txt", "--model", self.model_name,
                  "--provider", self.provider, "--credential-env", self.credential_env,
                  "--context-tokens", str(self.context_tokens), "--output-tokens", str(self.output_tokens),
                  "--cwd", "/app", "--no-timeout", "--keep-alive"]
        if self.effort is not None:
            runner.extend(["--effort", self.effort])
        # Secrets are passed as an exec environment, never interpolated into a command.
        launch = "nohup " + shlex.join(runner) + " > " + shlex.quote(self.remote_logs + "/supervisor.log") + " 2>&1 < /dev/null &"
        # BaseInstalledAgent._exec records env in debug logs; bypass it for secrets.
        launched = await environment.exec(command=launch, env={**self.proxy_environment(),
            self.credential_env: key,
            "CENTAERIS_SYSTEM_SKILLS_SOURCE": self.remote + "/system-skills"})
        if launched.return_code:
            raise RuntimeError("starting the headless supervisor failed")
        terminal_result = False
        try:
            result = await environment.exec(command=shlex.join([
                "python3", self.remote + "/runner.py", "wait", "--logs", self.remote_logs,
                "--no-timeout"]))
            if not result.stdout.strip():
                raise RuntimeError("headless supervisor returned no result")
            payload = json.loads(result.stdout)
            terminal_result = payload.get("status") in ("succeeded", "failed", "cancelled", "timedOut")
            usage = payload.get('providerUsage', {}).get('totals', {})
            context.n_input_tokens = usage.get('inputTokens')
            context.n_output_tokens = usage.get('outputTokens')
            context.n_cache_tokens = usage.get('promptCacheHitTokens')
            context.metadata = {"runtimeBuildId": self._version,
                                "modelBudget": payload.get("modelBudget"),
                                "sessionId": payload.get("sessionId"),
                                "agentRunId": payload.get("agentRunId")}
            context.metadata['providerUsage'] = payload.get('providerUsage')
            context.metadata['trajectoryPersistence'] = payload.get('trajectoryPersistence')
            health = context.metadata['trajectoryPersistence']
            if health and health.get('spoolSnapshotSaved'):
                recovered = self.logs_dir / ('centaeris-' + self.attempt) / 'events.recovered.jsonl'
                try:
                    recovered.parent.mkdir(parents=True, exist_ok=True)
                    await environment.download_file(
                        f"/tmp/centaeris-bench/{self.attempt}/events-terminal.jsonl", recovered)
                    health['recoveredArtifactPath'] = str(recovered.relative_to(self.logs_dir))
                except Exception as error:
                    # Artifact transport is independent of the committed AgentRun outcome.
                    health['recoveryErrorType'] = type(error).__name__
            if result.return_code or payload["status"] != "succeeded":
                diagnostic = payload.get("error") or payload.get("run", {}).get("error") or payload["status"]
                raise RuntimeError("Centaeris run failed: " + str(diagnostic))
        except BaseException as primary:
            if terminal_result:
                raise
            try:
                await asyncio.shield(self.exec_as_agent(environment, command=shlex.join([
                    "python3", self.remote + "/runner.py", "cancel", "--logs", self.remote_logs]),
                    timeout_sec=35))
            except BaseException as cleanup:
                if isinstance(primary, asyncio.CancelledError):
                    # Harbor must receive cancellation itself to enforce the task
                    # deadline. A BaseExceptionGroup would escape its per-trial
                    # handlers and cancel unrelated trials in the job TaskGroup.
                    context.metadata = dict(context.metadata or {})
                    context.metadata["cancellationCleanupError"] = {
                        "type": type(cleanup).__name__, "message": str(cleanup)}
                    primary.add_note("Cancellation cleanup failed: " + repr(cleanup))
                    raise primary from cleanup
                raise BaseExceptionGroup("agent run and cancellation failed", [primary, cleanup])
            raise
