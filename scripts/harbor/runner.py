"""Linux headless client for the existing Centaeris Runtime JSON-RPC protocol."""
from __future__ import annotations

import argparse
import asyncio
import json
import os
from pathlib import Path
import sys
import time
import uuid


def trial_profile(root: Path, attempt: str) -> Path:
    if not attempt or Path(attempt).name != attempt or attempt in (".", ".."):
        raise ValueError("invalid attempt identity")
    profile = root / attempt
    profile.mkdir(parents=True, exist_ok=False)
    return profile


def write_json(path: Path, value: dict) -> None:
    temporary = path.with_suffix(".tmp")
    temporary.write_text(json.dumps(value, ensure_ascii=False), encoding="utf-8")
    temporary.replace(path)


class Rpc:
    def __init__(self, reader, writer, events):
        self.reader, self.writer, self.events = reader, writer, events
        self.pending = {}
        self.next_id = 0
        self.failure = None
        self.task = asyncio.create_task(self._read())

    async def _read(self):
        try:
            while line := await self.reader.readline():
                frame = json.loads(line)
                if "id" in frame:
                    future = self.pending.pop(frame["id"], None)
                    if future is not None and not future.done():
                        if "error" in frame:
                            future.set_exception(RuntimeError(json.dumps(frame["error"])))
                        else:
                            future.set_result(frame["result"])
                else:
                    self.events.write(json.dumps(frame, ensure_ascii=False) + "\n")
                    self.events.flush()
            raise ConnectionError("Runtime connection closed")
        except Exception as error:
            self.failure = error
            for future in self.pending.values():
                if not future.done():
                    future.set_exception(error)
            self.pending.clear()

    async def call(self, method, request, timeout=30):
        if self.failure:
            raise self.failure
        self.next_id += 1
        request_id = self.next_id
        future = asyncio.get_running_loop().create_future()
        self.pending[request_id] = future
        frame = {"jsonrpc": "2.0", "id": request_id, "method": method,
                 "params": {} if method == "runtime/shutdown" else {"request": request}}
        try:
            self.writer.write((json.dumps(frame) + "\n").encode())
            await self.writer.drain()
            return await asyncio.wait_for(future, timeout)
        finally:
            self.pending.pop(request_id, None)

    async def close(self):
        self.writer.close()
        await self.writer.wait_closed()
        self.task.cancel()
        await asyncio.gather(self.task, return_exceptions=True)


async def run_trial(rpc, instruction, cwd, model, provider, effort, timeout,
                    poll_interval=0.25, state_path=None, api_key=None):
    request = {"modelProviderId": provider, "model": model}
    if effort is not None:
        request["modelThinkingMode"] = effort
    if api_key is not None:
        # Runtime deliberately separates credential mutation from model settings.
        await rpc.call("agent_runtime_config_set", {
            "modelProviderId": provider, "modelApiKey": api_key})
    config = await rpc.call("agent_runtime_config_set", request)
    expected_fields = [("modelProviderId", provider), ("model", model)]
    if effort is not None:
        expected_fields.append(("modelThinkingMode", effort))
    for field, expected in expected_fields:
        if config.get(field) != expected:
            raise ValueError(f"Runtime did not apply {field}")
    session = await rpc.call("session/new", {"operationId": str(uuid.uuid4()),
                            "cwd": cwd, "title": "Terminal benchmark attempt"})
    prompt = await rpc.call("session/prompt", {"operationId": str(uuid.uuid4()),
                            "sessionId": session["id"], "message": instruction})
    identity = {"sessionId": session["id"], "agentRunId": prompt["agentRunId"]}
    if state_path:
        write_json(state_path, {**identity, "pid": os.getpid()})
    deadline = time.monotonic() + timeout
    try:
        while time.monotonic() < deadline:
            response = await rpc.call("_centaeris/session/agent-runs", {
                "sessionId": session["id"], "includeTerminal": True})
            runs = [run for run in response["agentRuns"]
                    if run["agentRunId"] == identity["agentRunId"]]
            if len(runs) != 1:
                raise ValueError("Runtime lost or duplicated the admitted AgentRun")
            status = runs[0]["status"]
            if status in ("succeeded", "failed", "cancelled"):
                transcript = await rpc.call("session/load", {"sessionId": session["id"]})
                return {**identity, "status": status, "run": runs[0], "transcript": transcript}
            if status not in ("running", "stalled"):
                raise ValueError(f"unknown AgentRun status: {status}")
            await asyncio.sleep(poll_interval)
        await rpc.call("_centaeris/session/agent-runs/cancel", {
            **identity, "reason": "benchmark_deadline"})
        return {**identity, "status": "timedOut"}
    except BaseException as primary:
        # Do not leave a model generation alive after host cancellation or a broken poll.
        try:
            await asyncio.shield(rpc.call("_centaeris/session/agent-runs/cancel", {
                **identity, "reason": "benchmark_client_failed"}))
        except BaseException as cleanup:
            raise BaseExceptionGroup("trial and cancellation failed", [primary, cleanup])
        raise


async def connect(endpoint, events, startup_timeout=30):
    deadline = time.monotonic() + startup_timeout
    while True:
        try:
            reader, writer = await asyncio.open_unix_connection(endpoint, limit=16 * 1024 * 1024)
            rpc = Rpc(reader, writer, events)
            descriptor = await rpc.call("initialize", {"clientKind": "tui",
                                                       "viewerId": str(uuid.uuid4())})
            expected = {"status": "ok", "protocol": "centaeris.runtime", "protocolVersion": 1,
                        "coreProtocolVersion": "1.0.0"}
            if any(descriptor.get(key) != value for key, value in expected.items()):
                await rpc.close()
                raise ValueError("unsupported Runtime initialize descriptor")
            return rpc
        except (FileNotFoundError, ConnectionRefusedError):
            if time.monotonic() >= deadline:
                raise TimeoutError("Runtime startup deadline exceeded")
            await asyncio.sleep(0.1)


async def supervise(args):
    root = Path(args.logs)
    root.mkdir(parents=True, exist_ok=True)
    # AF_UNIX paths are limited to 108 bytes on Linux; logs can have long names.
    profile = trial_profile(Path("/tmp/centaeris-bench"), args.attempt)
    env = {**os.environ, "CENTAERIS_DESKTOP_DATA_DIR": str(profile),
           "CENTAERIS_MODEL_CONTEXT_BUDGET_TOKENS": str(args.context_tokens),
           "CENTAERIS_MODEL_OUTPUT_BUDGET_TOKENS": str(args.output_tokens)}
    endpoint_process = await asyncio.create_subprocess_exec(
        args.runtime, "--runtime-server-endpoint", env=env, stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.PIPE)
    stdout, stderr = await asyncio.wait_for(endpoint_process.communicate(), 30)
    if endpoint_process.returncode:
        raise RuntimeError(stderr.decode())
    endpoint = json.loads(stdout)["endpoint"]
    write_json(root / "endpoint.json", {"endpoint": endpoint, "pid": os.getpid()})
    with (root / "runtime.log").open("wb") as runtime_log, (root / "events.jsonl").open("w", encoding="utf-8") as events:
        runtime = await asyncio.create_subprocess_exec(args.runtime, "--runtime-server", env=env,
                                                       stdout=runtime_log, stderr=runtime_log)
        rpc = None
        try:
            rpc = await connect(endpoint, events)
            result = await run_trial(rpc, Path(args.instruction).read_text(encoding="utf-8"),
                                     args.cwd, args.model, args.provider, args.effort,
                                     args.timeout, state_path=root / "state.json",
                                     api_key=os.environ[args.credential_env])
            result["modelBudget"] = {"contextTokens": args.context_tokens,
                                     "maxOutputTokens": args.output_tokens}
            write_json(root / "result.json", result)
            if result["status"] == "succeeded":
                # Keep the initialized connection and Runtime-owned task services alive
                # while Harbor verifies. Container teardown ultimately owns cleanup.
                await asyncio.sleep(args.linger)
        finally:
            try:
                if rpc:
                    try:
                        await rpc.call("runtime/shutdown", {}, timeout=12)
                    finally:
                        await rpc.close()
            finally:
                if runtime.returncode is None:
                    try:
                        await asyncio.wait_for(runtime.wait(), 15)
                    except TimeoutError:
                        runtime.kill()
                        await runtime.wait()


def wait_result(args):
    deadline = time.monotonic() + args.timeout
    result = Path(args.logs) / "result.json"
    while time.monotonic() < deadline:
        if result.exists():
            value = json.loads(result.read_text(encoding="utf-8"))
            print(json.dumps(value, ensure_ascii=False))
            return 0 if value["status"] == "succeeded" else 1
        time.sleep(0.2)
    raise TimeoutError("headless supervisor did not produce a result")


async def cancel(args):
    root = Path(args.logs)
    endpoint_path = root / "endpoint.json"
    if not endpoint_path.exists():
        return
    endpoint = json.loads(endpoint_path.read_text(encoding="utf-8"))
    with (root / "cancel-events.jsonl").open("w", encoding="utf-8") as events:
        rpc = await connect(endpoint["endpoint"], events, startup_timeout=2)
        try:
            if (root / "state.json").exists():
                state = json.loads((root / "state.json").read_text(encoding="utf-8"))
                await rpc.call("_centaeris/session/agent-runs/cancel", {
                    "sessionId": state["sessionId"], "agentRunId": state["agentRunId"],
                    "reason": "harbor_cancelled"})
            await rpc.call("runtime/shutdown", {}, timeout=12)
        finally:
            await rpc.close()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("action", choices=("run", "wait", "cancel"))
    parser.add_argument("--logs", required=True)
    parser.add_argument("--runtime")
    parser.add_argument("--instruction")
    parser.add_argument("--attempt", default=str(uuid.uuid4()))
    parser.add_argument("--cwd", default="/app")
    parser.add_argument("--model")
    parser.add_argument("--provider")
    parser.add_argument("--credential-env")
    parser.add_argument("--effort")
    parser.add_argument("--context-tokens", type=int, default=500_000)
    parser.add_argument("--output-tokens", type=int, default=64_000)
    parser.add_argument("--timeout", type=float, default=900)
    parser.add_argument("--linger", type=float, default=1200)
    args = parser.parse_args()
    if args.action == "wait":
        return wait_result(args)
    if args.action == "cancel":
        asyncio.run(cancel(args))
        return 0
    if not all((args.runtime, args.instruction, args.model, args.provider, args.credential_env)):
        parser.error("run requires --runtime, --instruction, --model, --provider and --credential-env")
    try:
        asyncio.run(supervise(args))
    except Exception as error:
        write_json(Path(args.logs) / "result.json", {"status": "failed", "error": str(error)})
        raise
    return 0


if __name__ == "__main__":
    sys.exit(main())
