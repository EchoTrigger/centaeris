"""Linux headless client for the existing Centaeris Runtime JSON-RPC protocol."""
from __future__ import annotations

import argparse
import asyncio
import json
import os
from pathlib import Path
import sys
import time
import tempfile
import uuid


def trial_profile(root: Path, attempt: str) -> Path:
    if not attempt or Path(attempt).name != attempt or attempt in (".", ".."):
        raise ValueError("invalid attempt identity")
    profile = root / attempt
    profile.mkdir(parents=True, exist_ok=False)
    return profile


def write_json(path: Path, value: dict) -> None:
    temporary = path.with_name(f".{path.name}.{uuid.uuid4().hex}.tmp")
    temporary.write_text(json.dumps(value, ensure_ascii=False), encoding="utf-8")
    os.replace(temporary, path)


def record_cleanup_errors(path: Path, errors: list) -> list:
    """Keep the finite cleanup steps observable without replacing their primary error."""
    values = [{"stage": stage, "type": type(error).__name__, "message": str(error)}
              for stage, error in errors]
    try:
        write_json(path, {"cleanupErrors": values})
    except Exception as error:
        try:
            print(f"Cleanup diagnostics unavailable: {type(error).__name__}: {error}", file=sys.stderr)
        except Exception:
            pass
    return values


def collect_provider_usage(profile: Path, session_id: str, agent_run_id: str) -> dict:
    """Export only Core's committed usage records, never configuration or secrets."""
    parent_paths = list((profile / 'sessions').rglob(session_id + '.jsonl'))
    if len(parent_paths) != 1:
        raise ValueError('expected exactly one authoritative session log')
    paths = parent_paths + list((profile / 'sessions').rglob('session-agent-*.jsonl'))
    fields = ('inputTokens', 'outputTokens', 'totalTokens',
              'promptCacheHitTokens', 'promptCacheMissTokens')
    turns = {}
    for path in paths:
        for line in path.read_text(encoding='utf-8').splitlines():
            event = json.loads(line)
            if event.get('type') != 'provider_usage':
                continue
            if path in parent_paths and event.get('agentRunId') != agent_run_id:
                continue
            if event.get('schemaVersion') != 'session.event.v1' or event.get('sessionId') != path.stem:
                raise ValueError('unsupported provider usage record')
            turn = (event['sessionId'], event['turnId'])
            if turn in turns:
                raise ValueError('duplicate committed provider usage turn')
            value = {field: event['payload'].get(field) for field in fields}
            if any(v is not None and (type(v) is not int or v < 0) for v in value.values()):
                raise ValueError('invalid provider token count')
            turns[turn] = value
    def summarize(rows):
        totals = {field: (sum(row[field] for row in rows)
                         if rows and all(row[field] is not None for row in rows) else None)
                  for field in fields}
        hit, miss = totals['promptCacheHitTokens'], totals['promptCacheMissTokens']
        totals['promptCacheHitRate'] = hit / (hit + miss) if hit is not None and miss is not None and hit + miss else None
        return {'nRequests': len(rows), 'totals': totals}

    def session_summary(identity):
        return {'sessionId': identity, **summarize([value for (session, _), value in turns.items() if session == identity])}

    return {**summarize(list(turns.values())),
            'mainAgent': session_summary(session_id),
            'subagents': [session_summary(identity) for identity in sorted({path.stem for path in paths} - {session_id})],
            'turns': [{'sessionId': session, 'turnId': turn, **value} for (session, turn), value in turns.items()]}


async def export_provider_usage(root, profile_path, identity, *, terminal_observed, status):
    artifact = {"usageCoverage": {
        "source": "committedProviderUsage", "terminalObserved": terminal_observed,
        "inFlightRequestMayBeMissing": not terminal_observed or status != "succeeded"}}
    try:
        if profile_path is None:
            raise ValueError("Runtime profilePath unavailable for committed usage export")
        artifact["providerUsage"] = await asyncio.wait_for(asyncio.to_thread(
            collect_provider_usage, Path(profile_path), identity["sessionId"], identity["agentRunId"]), 3)
    except Exception as error:
        artifact["usageExportError"] = {"type": type(error).__name__, "message": str(error)}
        if isinstance(error, asyncio.TimeoutError):
            # asyncio cancellation cannot stop a thread blocked in filesystem I/O.
            artifact["usageExportError"]["workerThreadMayContinue"] = True
    try:
        write_json(root / "provider-usage.json", artifact)
    except Exception as error:
        artifact["usageExportError"] = {"type": type(error).__name__, "message": str(error)}
    return artifact


async def cancel_and_observe_terminal(rpc, identity, reason):
    # Cancellation acknowledgement is not the committed terminal fact.
    await rpc.call("_centaeris/session/agent-runs/cancel", {**identity, "reason": reason}, timeout=6)
    while True:
        response = await rpc.call("_centaeris/session/agent-runs", {
            "sessionId": identity["sessionId"], "includeTerminal": True}, timeout=6)
        runs = [run for run in response["agentRuns"] if run["agentRunId"] == identity["agentRunId"]]
        if len(runs) != 1:
            raise ValueError("Runtime lost or duplicated the admitted AgentRun")
        status = runs[0]["status"]
        if status in ("succeeded", "failed", "cancelled"):
            return status
        if status not in ("running", "stalled"):
            raise ValueError(f"unknown AgentRun status: {status}")
        await asyncio.sleep(0.25)


class Rpc:
    def __init__(self, reader, writer, events, event_spool=None):
        self.reader, self.writer, self.events = reader, writer, events
        self.event_spool = event_spool if event_spool is not None else tempfile.TemporaryFile(mode="w+", encoding="utf-8")
        self.owns_event_spool = event_spool is None
        self.event_log_error = None
        self.mirror_unavailable = False
        self.event_spool_error = None
        self.unspooled_events = 0
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
                    self.record_event(frame)
            raise ConnectionError("Runtime connection closed")
        except Exception as error:
            self.failure = error
            for future in self.pending.values():
                if not future.done():
                    future.set_exception(error)
            self.pending.clear()

    def record_event(self, frame):
        line = json.dumps(frame, ensure_ascii=False) + "\n"
        try:
            self.event_spool.write(line)
            self.event_spool.flush()
        except OSError as error:
            self.event_spool_error = error
            self.unspooled_events += 1
        if not self.mirror_unavailable:
            try:
                self.events.write(line)
                self.events.flush()
            except OSError as error:
                self.event_log_error = error
                self.mirror_unavailable = True
                try:
                    print(f"Trajectory mirror unavailable (errno={error.errno}); RPC continues; persistence status is recorded", file=sys.stderr)
                except OSError:
                    # stderr may share the same failed host mount.
                    pass

    def restore_event_log(self):
        restored = False
        if self.event_log_error is not None and self.event_spool_error is None:
            try:
                self.event_spool.seek(0)
                self.events.seek(0)
                self.events.truncate()
                while chunk := self.event_spool.read(1024 * 1024):
                    self.events.write(chunk)
                self.events.flush()
                restored = True
                self.mirror_unavailable = False
            except OSError:
                pass
            finally:
                try:
                    self.event_spool.seek(0, 2)
                except OSError as error:
                    self.event_spool_error = error
        return {"mirrorErrorErrno": self.event_log_error.errno if self.event_log_error else None,
                "spoolErrorErrno": self.event_spool_error.errno if self.event_spool_error else None,
                "unspooledEvents": self.unspooled_events, "mirrorRestored": restored}

    def snapshot_event_spool(self, path):
        if self.event_spool_error is not None:
            return False
        try:
            self.event_spool.seek(0)
            with Path(path).open("w", encoding="utf-8") as output:
                while chunk := self.event_spool.read(1024 * 1024):
                    output.write(chunk)
            return True
        except OSError as error:
            self.event_spool_error = error
            return False
        finally:
            try:
                self.event_spool.seek(0, 2)
            except OSError as error:
                self.event_spool_error = error

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
        primary = None
        try:
            self.writer.close()
            await self.writer.wait_closed()
        except BaseException as error:
            primary = error
        self.task.cancel()
        try:
            try:
                await asyncio.gather(self.task, return_exceptions=True)
            finally:
                if self.owns_event_spool:
                    self.event_spool.close()
        except BaseException as cleanup:
            if primary is not None:
                raise primary from cleanup
            raise
        if primary is not None:
            raise primary


async def run_trial(rpc, instruction, cwd, model, provider, effort, timeout,
                    poll_interval=0.25, state_path=None, api_key=None, deadline_at_ms=None,
                    cleanup_errors=None):
    deadline = None
    if deadline_at_ms is not None:
        remaining = (deadline_at_ms - time.time() * 1000) / 1000
        if remaining <= 0:
            raise asyncio.TimeoutError("evaluation deadline expired before admission")
        deadline = time.monotonic() + remaining
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
    if deadline_at_ms is not None and time.monotonic() >= deadline:
        raise asyncio.TimeoutError("evaluation deadline expired before admission")
    prompt = await rpc.call("session/prompt", {"operationId": str(uuid.uuid4()),
                            "sessionId": session["id"], "message": instruction})
    identity = {"sessionId": session["id"], "agentRunId": prompt["agentRunId"]}
    if state_path:
        write_json(state_path, {**identity, "pid": os.getpid()})
    if deadline_at_ms is None:
        deadline = None if timeout is None else time.monotonic() + timeout
    try:
        while deadline is None or time.monotonic() < deadline:
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
            if cleanup_errors is not None:
                cleanup_errors.append(("cancelRun", cleanup))
            raise primary from cleanup
        raise


async def connect(endpoint, events, startup_timeout=30, event_spool=None):
    deadline = time.monotonic() + startup_timeout
    while True:
        try:
            reader, writer = await asyncio.open_unix_connection(endpoint, limit=16 * 1024 * 1024)
            rpc = Rpc(reader, writer, events, event_spool=event_spool)
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
                raise asyncio.TimeoutError("Runtime startup deadline exceeded")
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
    write_json(root / "endpoint.json", {"endpoint": endpoint, "pid": os.getpid(), "profilePath": str(profile)})
    with (root / "runtime.log").open("wb") as runtime_log, (root / "events.jsonl").open("w", encoding="utf-8") as events, (profile / "events.jsonl").open("w+", encoding="utf-8") as event_spool:
        runtime = await asyncio.create_subprocess_exec(args.runtime, "--runtime-server", env=env,
                                                       stdout=runtime_log, stderr=runtime_log)
        rpc = None
        primary = None
        cleanup_errors = []
        published_result = None
        try:
            rpc = await connect(endpoint, events, event_spool=event_spool)
            result = await run_trial(rpc, Path(args.instruction).read_text(encoding="utf-8"),
                                     args.cwd, args.model, args.provider, args.effort,
                                     args.timeout, state_path=root / "state.json",
                                     api_key=os.environ[args.credential_env],
                                     deadline_at_ms=args.deadline_at_ms,
                                     cleanup_errors=cleanup_errors)
            result["modelBudget"] = {"contextTokens": args.context_tokens,
                                     "maxOutputTokens": args.output_tokens}
            if result.get('sessionId'):
                result.update(await export_provider_usage(
                    root, profile, result,
                    terminal_observed=result["status"] in ("succeeded", "failed", "cancelled"),
                    status=result["status"]))
            health = rpc.restore_event_log()
            if health["mirrorErrorErrno"] is not None and not health["mirrorRestored"]:
                health["spoolSnapshotSaved"] = rpc.snapshot_event_spool(profile / "events-terminal.jsonl")
                health["spoolErrorErrno"] = rpc.event_spool_error.errno if rpc.event_spool_error else None
            result["trajectoryPersistence"] = health
            write_json(root / "result.json", result)
            published_result = result
            if result["status"] == "succeeded":
                # Keep the initialized connection and Runtime-owned task services alive
                # while Harbor verifies. Container teardown ultimately owns cleanup.
                if args.keep_alive:
                    await asyncio.Event().wait()
                else:
                    await asyncio.sleep(args.linger)
        except BaseException as error:
            primary = error
            raise
        finally:
            if rpc:
                try:
                    await rpc.call("runtime/shutdown", {}, timeout=12)
                except BaseException as error:
                    cleanup_errors.append(("shutdown", error))
                try:
                    await asyncio.wait_for(rpc.close(), 1)
                except BaseException as error:
                    cleanup_errors.append(("close", error))
            if runtime.returncode is None:
                try:
                    await asyncio.wait_for(runtime.wait(), 15)
                except asyncio.TimeoutError:
                    try:
                        runtime.kill()
                    except BaseException as error:
                        cleanup_errors.append(("killProcess", error))
                    try:
                        await asyncio.wait_for(runtime.wait(), 5)
                    except BaseException as error:
                        cleanup_errors.append(("reapProcess", error))
                except BaseException as error:
                    cleanup_errors.append(("waitProcess", error))
            if cleanup_errors:
                values = record_cleanup_errors(root / "cleanup-errors.json", cleanup_errors)
                if published_result is not None:
                    published_result["cleanupErrors"] = values
                    try:
                        write_json(root / "result.json", published_result)
                    except Exception as error:
                        cleanup_errors.append(("resultPersistence", error))
                        record_cleanup_errors(root / "cleanup-errors.json", cleanup_errors)
                failure = primary if primary is not None else cleanup_errors[0][1]
                secondary = next((error for _, error in cleanup_errors if error is not failure), None)
                if secondary is not None:
                    raise failure from secondary
                if primary is None:
                    raise failure


def wait_result(args):
    deadline = None if args.timeout is None else time.monotonic() + args.timeout
    result = Path(args.logs) / "result.json"
    while deadline is None or time.monotonic() < deadline:
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
    state_path = root / "state.json"
    identity = json.loads(state_path.read_text(encoding="utf-8")) if state_path.exists() else {}
    with (root / "cancel-events.jsonl").open("w", encoding="utf-8") as events:
        try:
            # Include initialize in the deadline. Debug Runtime initialization
            # hashes the executable and can take ten seconds on a 1-CPU task.
            rpc = await asyncio.wait_for(
                connect(endpoint["endpoint"], events, startup_timeout=2), timeout=15)
        except (asyncio.TimeoutError, FileNotFoundError, ConnectionRefusedError):
            result_path = root / "result.json"
            result = json.loads(result_path.read_text(encoding="utf-8")) if result_path.exists() else {}
            status = result.get("status")
            await export_provider_usage(root, endpoint.get("profilePath"), identity or result,
                                        terminal_observed=status in ("succeeded", "failed", "cancelled"),
                                        status=status)
            if status in ("succeeded", "failed", "cancelled", "timedOut"):
                return
            raise
        primary = None
        cleanup_errors = []
        status = None
        try:
            if identity:
                status = await asyncio.wait_for(cancel_and_observe_terminal(rpc, {
                    "sessionId": identity["sessionId"], "agentRunId": identity["agentRunId"]},
                    "harbor_cancelled"), 6)
        except BaseException as error:
            primary = error
        try:
            await export_provider_usage(root, endpoint.get("profilePath"), identity,
                                        terminal_observed=status is not None, status=status)
        except BaseException as error:
            cleanup_errors.append(("exportUsage", error))
        try:
            await rpc.call("runtime/shutdown", {}, timeout=6)
        except BaseException as error:
            cleanup_errors.append(("shutdown", error))
        try:
            await asyncio.wait_for(rpc.close(), 1)
        except BaseException as error:
            cleanup_errors.append(("close", error))
        if cleanup_errors:
            # run and cancel are separate CLI processes; their diagnostics have
            # separate destinations so either cleanup pass retains its evidence.
            record_cleanup_errors(root / "cancel-cleanup-errors.json", cleanup_errors)
            failure = primary if primary is not None else cleanup_errors[0][1]
            secondary = next((error for _, error in cleanup_errors if error is not failure), None)
            if secondary is not None:
                raise failure from secondary
            raise failure
        if primary is not None:
            raise primary


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
    parser.add_argument("--deadline-at-ms", type=int,
                        help="Absolute evaluation deadline, including setup elapsed before admission")
    parser.add_argument("--no-timeout", action="store_true",
                        help="Let the embedding trial orchestrator own the deadline")
    parser.add_argument("--keep-alive", action="store_true",
                        help="Retain successful Runtime services until external teardown")
    parser.add_argument("--linger", type=float, default=1200)
    args = parser.parse_args()
    if args.no_timeout:
        args.timeout = None
    if args.action == "wait":
        return wait_result(args)
    if args.action == "cancel":
        asyncio.run(cancel(args))
        return 0
    if not all((args.runtime, args.instruction, args.model, args.provider, args.credential_env)):
        parser.error("run requires --runtime, --instruction, --model, --provider and --credential-env")
    try:
        asyncio.run(supervise(args))
    except BaseException as error:
        root = Path(args.logs)
        result_path = root / "result.json"
        try:
            if result_path.exists():
                result = json.loads(result_path.read_text(encoding="utf-8"))
                if not isinstance(result, dict) or result.get("status") not in ("succeeded", "failed", "cancelled", "timedOut"):
                    raise ValueError("existing supervisor result has no supported terminal status")
            else:
                write_json(result_path, {"status": "failed", "error": str(error)})
        except Exception as persistence_error:
            record_cleanup_errors(root / "cleanup-errors.json", [("resultPersistence", persistence_error)])
            raise error from persistence_error
        raise
    return 0


if __name__ == "__main__":
    sys.exit(main())
