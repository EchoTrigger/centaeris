"""Controlled old-Worker/native-Runtime comparison using the existing API fixture.

Requires two test executables and a disposable PostgreSQL with commit timestamps.
This measures coordination with a fixed model and an empty physical adapter; it
does not measure provider inference, real sandbox creation, or production capacity.
"""
import argparse
from collections import Counter
from datetime import datetime, timezone
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import random
import shutil
import subprocess
import sys
import tempfile
import tarfile
import threading
import time
import types
from unittest.mock import patch
from urllib.parse import unquote, urlsplit

ROOT = Path(__file__).resolve().parents[2]
BASELINE = "96a5dd5c950da44a01425e4be728276365eda424"
REPORT = {"schema": "runtime.lifecycle.comparison.v1", "cases": []}


def file_sha256(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def arguments():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline-executable", type=Path)
    parser.add_argument("--candidate-executable", type=Path)
    parser.add_argument("--build-executables", action="store_true")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--samples", type=int, default=30)
    parser.add_argument("--warmup", type=int, default=3)
    parser.add_argument("--model-ms", type=float, default=50)
    parser.add_argument("--case", action="append", choices=("baselineRun", "candidateRun", "baselineWake", "candidateWake"))
    return parser.parse_args()


def prepare_executables(destination):
    """Compile production versions with identical test-only waiter commands."""
    destination.mkdir(parents=True, exist_ok=True)

    def build(source, name):
        environment = {**os.environ, "CARGO_BUILD_JOBS": "2", "CARGO_TARGET_DIR": str(ROOT / "target")}
        command = ["cargo", "test", "--locked", "-p", "runtime_server", "--no-run", "--message-format=json"]
        print(f"Building {name} measurement executable", flush=True)
        process = subprocess.Popen(command, cwd=source, env=environment, stdout=subprocess.PIPE,
                                   text=True, encoding="utf-8")
        executable = None
        for line in process.stdout:
            event = json.loads(line)
            if event.get("reason") == "compiler-artifact" and event.get("target", {}).get("name") == "runtime_server":
                executable = event.get("executable") or executable
            if event.get("reason") == "compiler-message" and event.get("message", {}).get("level") == "error":
                print(event["message"]["rendered"], flush=True)
        if process.wait() or executable is None:
            raise RuntimeError(f"{name} measurement build failed")
        target = destination / (name + Path(executable).suffix)
        shutil.copyfile(executable, target)
        return target

    candidate = build(ROOT, "candidate")
    build_source = tempfile.TemporaryDirectory(prefix="centaeris-lifecycle-baseline-")
    temporary = Path(build_source.name)
    archive = temporary / "source.tar"
    with archive.open("wb") as stream:
        subprocess.run(["git", "archive", "--format=tar", BASELINE], cwd=ROOT, stdout=stream, check=True)
    source = temporary / "source"
    source.mkdir()
    with tarfile.open(archive) as package:
        package.extractall(source, filter="data")
    relative = Path("packages/workspace/runtime/src/environment_runtime_tests.rs")
    fixture = source / relative
    code = fixture.read_text(encoding="utf-8").replace("mod owned_execution;", "mod owned_execution;\nmod lifecycle_measurement;")
    # Use the same empty physical adapter registration in both versions. This
    # test-only fix lets cleanup before prepare select its owned host too.
    code = code.replace("assert!(owned_runs().lock().unwrap().insert(run.to_string()));",
                        "owned_runs().lock().unwrap().insert(run.to_string());")
    code = code.replace("    let value = production_fixture().unwrap();",
                        '    let value = production_fixture().unwrap();\n'
                        '    if let Some(run) = value["agentRunId"].as_str() { register_owned_run(run); }')
    current = (ROOT / relative).read_text(encoding="utf-8")
    start = current.index('        if let Some(wire) = command.strip_prefix("measure-waiter:")')
    end = current.index('        if let Some(wire) = command.strip_prefix("physical-pin:")')
    code = code.replace('        if let Some(wire) = command.strip_prefix("physical-pin:")',
                        current[start:end] + '        if let Some(wire) = command.strip_prefix("physical-pin:")')
    fixture.write_text(code, encoding="utf-8", newline="\n")
    helper = relative.parent / "environment_runtime_tests/lifecycle_measurement.rs"
    shutil.copyfile(ROOT / helper, source / helper)
    baseline = build(source, "baseline")
    return baseline, candidate, build_source


def load_worker(runtime_url, api_url, token):
    """Load the exact historical coordinator, without restoring its deleted package."""
    prefix = "packages/workspace/worker/"
    env = {"RUNTIME_INTERNAL_URL": runtime_url, "API_INTERNAL_URL": api_url,
           "INTERNAL_API_TOKEN": token, "WORKER_LEASE_MS": "60000"}
    modules = []
    with patch.dict(os.environ, env):
        for name in ("observations", "worker"):
            wire = subprocess.check_output(["git", "show", f"{BASELINE}:{prefix}{name}.py"], cwd=ROOT)
            module = types.ModuleType(name)
            module.__file__ = f"git:{BASELINE}/{prefix}{name}.py"
            sys.modules[name] = module
            exec(compile(wire, module.__file__, "exec"), module.__dict__)
            modules.append(module)
    return modules[-1]


def summary(samples, field):
    values = sorted(sample[field] for sample in samples)
    return {"samples": len(values), "p50Ms": values[math.ceil(len(values) * .5) - 1],
            "p95Ms": values[math.ceil(len(values) * .95) - 1],
            "meanMs": sum(values) / len(values), "minMs": values[0], "maxMs": values[-1]}


def define_cases(options):
    import psycopg
    from django.conf import settings
    from django.db import connection
    from django.test import LiveServerTestCase, override_settings
    from app_core import runtime_client
    from app_core.agent_run_authorization_factory import create_agent_run_authorization
    from app_core.model_adapter.common import zero_usage
    from app_core.models import AgentRun, Session
    from app_core.test_environment_runtime_fixture import EnvironmentRuntimeFixture

    class Comparison(EnvironmentRuntimeFixture, LiveServerTestCase):
        serialized_rollback = True

        def observer(self):
            database = connection.settings_dict
            observer = psycopg.connect(dbname=database["NAME"], user=database["USER"],
                                       password=database["PASSWORD"], host=database["HOST"],
                                       port=database["PORT"], autocommit=True)
            observer.execute("LISTEN runtime_job_ready_v1")
            self.addCleanup(observer.close)
            return observer

        def wait_row(self, observer, sql, params, ready, timeout=30):
            deadline = time.monotonic() + timeout
            while True:
                row = observer.execute(sql, params).fetchone()
                observed = time.perf_counter_ns()
                if ready(row):
                    return row, observed
                self.assertIsNone(self.runtime_process.poll(), "".join(self.runtime_lines[-100:]))
                self.assertLess(time.monotonic(), deadline, "".join(self.runtime_lines[-100:]))
                # Terminal/yield transitions notify after commit; bounded observation
                # also makes a failed notification explicit rather than hanging.
                for _ in observer.notifies(timeout=.25, stop_after=1):
                    break

        def historical(self, runtime_url, *, execution):
            worker = load_worker(runtime_url, self.live_server_url, settings.INTERNAL_API_TOKEN)
            self.worker_rpcs = []
            request = worker.json_request

            def record(url, body, *args, **kwargs):
                rpc = {"path": urlsplit(url).path, "target": "runtime" if url.startswith(runtime_url) else "api",
                       "startedNs": time.perf_counter_ns()}
                if isinstance(body, dict):
                    rpc["jobId"] = body.get("jobId") or body.get("agentRunStart", {}).get("agentRunId")
                elif rpc["path"].startswith("/internal/jobs/"):
                    rpc["jobId"] = unquote(rpc["path"].removeprefix("/internal/jobs/"))
                self.worker_rpcs.append(rpc)
                try:
                    return request(url, body, *args, **kwargs)
                finally:
                    rpc["finishedNs"] = time.perf_counter_ns()

            worker.json_request = record
            stopped = threading.Event()
            operations = [(worker.run_loop, (worker.TerminalDispatcher(), 1, stopped))]
            if execution:
                operations.extend([(worker.run_job_dispatcher, (stopped,)),
                                   (worker.run_control_dispatcher, ("advance", stopped)),
                                   (worker.run_control_dispatcher, ("cleanup", stopped))])
            threads = [threading.Thread(target=operation, args=args, daemon=True)
                       for operation, args in operations]
            for thread in threads:
                thread.start()

            def stop():
                stopped.set()
                if execution:
                    runtime_client.request_json("/internal/jobs/schedule", {
                        "schema": "runtime.job.schedule.v1", "jobId": "worker.noop:measurement-stop",
                        "jobKind": "worker.noop", "runAtMs": int(time.time() * 1000),
                        "maxRetries": 0, "idempotencyKey": "worker.noop:measurement-stop",
                        "sessionId": None, "payloadRef": None, "workspaceId": None}, timeout=5)
                for thread in threads:
                    thread.join(timeout=26)
                    self.assertFalse(thread.is_alive(), "historical coordinator did not stop")
            return stop

        def begin(self, mode):
            executable = options.baseline_executable if mode == "baseline" else options.candidate_executable
            with patch.dict(os.environ, {"CENTAERIS_TEST_RUNTIME_EXECUTABLE": str(executable),
                                        "WORKSPACE_PERF_OBSERVATIONS": "1", "RUNTIME_JOB_LEASE_MS": "60000"}):
                return self.start_runtime()

        def run_case(self, mode):
            case = {"kind": "ordinaryRun", "mode": mode, "samples": [], "warmup": options.warmup,
                    "outcome": "running", "modelDelayMs": options.model_ms}
            self.measurement_case = case
            REPORT["cases"].append(case)
            runtime_url = self.begin(mode)
            observer = self.observer()
            current = {}

            def response(_model, _request):
                current.setdefault("modelStartNs", time.perf_counter_ns())
                current["modelCalls"] = current.get("modelCalls", 0) + 1
                time.sleep(options.model_ms / 1000)
                current["modelReturnNs"] = time.perf_counter_ns()
                return {"text": "Controlled lifecycle latency measurement.", "toolCalls": [], "usage": zero_usage()}

            with override_settings(RUNTIME_URL=runtime_url), patch("app_core.model_adapter.fake_model_response", side_effect=response):
                stop = self.historical(runtime_url, execution=True) if mode == "baseline" else None
                if mode == "candidate":
                    self.runtime_command("start-lifecycle", "environment-runtime-lifecycle:")
                try:
                    for index in range(options.warmup + options.samples):
                        run = self.run if index == 0 else AgentRun.objects.create(
                            workspace=self.workspace, session=Session.objects.create(workspace=self.workspace, owner=self.user),
                            user=self.user, modelConfig=self.model, prompt="Measure ordinary lifecycle overhead.",
                            agent_instructions="Finish the requested objective.")
                        if index:
                            create_agent_run_authorization(run, image_digest="sha256:" + "a" * 64)
                        current.clear()
                        start = time.perf_counter_ns()
                        self.assertEqual(runtime_client.schedule_agent_run_lifecycle(run), "inserted")
                        job_id = "agent_run.lifecycle:" + run.pk
                        row, observed = self.wait_row(observer,
                            "SELECT status, retry_count FROM runtime.runtime_jobs WHERE job_id=%s", [job_id],
                            lambda row: row and row[0] in {"succeeded", "failed", "dead_lettered", "cancelled"})
                        self.assertEqual(row, ("succeeded", 0), "".join(self.runtime_lines[-100:]))
                        run.refresh_from_db()
                        facts = [line for line in self.runtime_lines if '"event":"agent_run_internal_failure"' in line]
                        self.assertEqual(run.status, "completed", json.dumps({"modelCalls": current.get("modelCalls", 0), "facts": facts}))
                        self.assert_lifecycle_cleanup(run)
                        self.assertEqual(current["modelCalls"], 1)
                        sample = {"index": index, "runId": run.pk, "jobId": job_id, **current,
                                  "dispatchToModelMs": (current["modelStartNs"] - start) / 1e6,
                                  "modelToObservedCompletionMs": (observed - current["modelReturnNs"]) / 1e6,
                                  "observedTotalMs": (observed - start) / 1e6,
                                  "actualModelMs": (current["modelReturnNs"] - current["modelStartNs"]) / 1e6,
                                  "startedNs": start, "completedNs": observed}
                        if index >= options.warmup:
                            case["samples"].append(sample)
                        print(f"{mode} Run {index}: {sample['dispatchToModelMs']:.2f}/{sample['modelToObservedCompletionMs']:.2f} ms", flush=True)
                finally:
                    if stop:
                        stop()
            measured = {identity for sample in case["samples"] for identity in (sample["jobId"], sample["runId"])}
            case["runtimePerJobRpcs"] = Counter(
                rpc["path"] if not rpc["path"].startswith("/internal/jobs/agent_run.lifecycle:") else "/internal/jobs/:id"
                for rpc in getattr(self, "worker_rpcs", []) if rpc["target"] == "runtime" and rpc.get("jobId") in measured)
            observed = []
            for line in self.runtime_lines:
                if line.startswith("{"):
                    try:
                        observed.append(json.loads(line))
                    except ValueError:
                        pass
            # The server independently observes these coordinator routes in both modes.
            # Scheduling, test observation and idle claims are outside this counter.
            control_routes = {"/agent-runs/step", "/agent-runs/teardown", "/agent-runs/finalize",
                              "/internal/jobs/start", "/internal/jobs/heartbeat", "/internal/jobs/complete",
                              "/internal/jobs/yield", "/internal/jobs/fail"}
            case["runtimeObservedControlRpcs"] = Counter(
                event["route"] for event in observed if event.get("phase") == "requestReleased"
                and event.get("jobId") in measured and event.get("route") in control_routes)
            case["workerRpcs"] = getattr(self, "worker_rpcs", [])
            case["summary"] = {field: summary(case["samples"], field) for field in
                               ("dispatchToModelMs", "modelToObservedCompletionMs", "observedTotalMs", "actualModelMs")}
            case["outcome"] = "passed"

        def wake_case(self, mode):
            case = {"kind": "terminalWake", "mode": mode, "samples": [], "warmup": options.warmup, "outcome": "running"}
            self.measurement_case = case
            REPORT["cases"].append(case)
            runtime_url = self.begin(mode)
            observer = self.observer()
            self.assertEqual(observer.execute("SHOW track_commit_timestamp").fetchone()[0], "on")
            delays = [random.Random(4700 + i).uniform(.005, .995) for i in range(options.warmup + options.samples)]
            with override_settings(RUNTIME_URL=runtime_url):
                stop = self.historical(runtime_url, execution=False) if mode == "baseline" else None
                if mode == "candidate":
                    self.runtime_command("start-terminal-delivery", "environment-runtime-terminal-delivery:")
                try:
                    for index, delay in enumerate(delays):
                        identity = self.runtime_command("measure-waiter:" + str(index), "environment-runtime-measure-waiter:")
                        time.sleep(delay)
                        self.runtime_command("measure-source-complete:" + identity["sourceJobId"],
                                             "environment-runtime-measure-source-complete:")
                        row, _ = self.wait_row(observer,
                            "SELECT EXTRACT(EPOCH FROM (pg_xact_commit_timestamp(e.xmin) - "
                            "pg_xact_commit_timestamp(s.xmin))) * 1000, p.status, p.run_at_ms <= s.updated_at_ms, "
                            "pg_xact_commit_timestamp(s.xmin), pg_xact_commit_timestamp(e.xmin) "
                            "FROM runtime.runtime_events e JOIN runtime.runtime_jobs s ON s.job_id=%s "
                            "JOIN runtime.runtime_jobs p ON p.job_id=e.task_id "
                            "WHERE e.task_id=%s AND e.event_type='runtime_job_wake_requested'",
                            [identity["sourceJobId"], identity["parentJobId"]], lambda row: row is not None)
                        self.assertEqual(row[1:3], ("queued", True))
                        sample = {"index": index, **identity, "arrivalJitterMs": delay * 1000,
                                  "sourceCommittedAt": row[3].isoformat(), "wakeCommittedAt": row[4].isoformat(),
                                  "commitToWakeMs": float(row[0])}
                        self.assertGreaterEqual(sample["commitToWakeMs"], 0)
                        if index >= options.warmup:
                            case["samples"].append(sample)
                        print(f"{mode} wake {index}: {sample['commitToWakeMs']:.3f} ms", flush=True)
                finally:
                    if stop:
                        stop()
            case["summary"] = {"commitToWakeMs": summary(case["samples"], "commitToWakeMs")}
            case["outcome"] = "passed"

        def test_a_baseline_run(self):
            self.run_case("baseline")

        def test_b_candidate_run(self):
            self.run_case("candidate")

        def test_c_baseline_wake(self):
            self.wake_case("baseline")

        def test_d_candidate_wake(self):
            self.wake_case("candidate")

    Comparison.__module__ = __name__
    globals()["Comparison"] = Comparison


def main():
    options = arguments()
    with tempfile.TemporaryDirectory(prefix="centaeris-lifecycle-binaries-") as directory:
        return run_comparison(options, Path(directory))


def run_comparison(options, binaries):
    build_source = None
    if options.build_executables:
        options.baseline_executable, options.candidate_executable, build_source = prepare_executables(binaries)
    elif options.baseline_executable is None or options.candidate_executable is None:
        raise SystemExit("Pass --build-executables or both exact test executables")
    options.baseline_executable = options.baseline_executable.resolve(strict=True)
    options.candidate_executable = options.candidate_executable.resolve(strict=True)
    options.output.parent.mkdir(parents=True, exist_ok=True)
    REPORT.update({"baselineRevision": BASELINE,
                   "candidateRevision": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(),
                   "candidateWorktreeChanges": subprocess.check_output(["git", "status", "--porcelain"], cwd=ROOT, text=True).splitlines(),
                   "candidateTrackedDiffSha256": hashlib.sha256(subprocess.check_output(["git", "diff", "HEAD"], cwd=ROOT)).hexdigest(),
                   "measurementStartedAtUtc": datetime.now(timezone.utc).isoformat(),
                   "measurementExtensions": {str(path): file_sha256(ROOT / path) for path in (
                       Path("perf/harness/lifecycle_comparison.py"),
                       Path("packages/workspace/runtime/src/environment_runtime_tests/lifecycle_measurement.rs"),
                       Path("packages/workspace/runtime/src/environment_runtime_tests.rs"),
                       Path("packages/workspace/runtime/src/lifecycle_executor.rs"))},
                   "executables": {mode: {"path": str(path), "sha256": file_sha256(path)}
                                   for mode, path in (("baseline", options.baseline_executable), ("candidate", options.candidate_executable))},
                   "testExitCode": None,
                   "host": platform.platform(), "logicalCpus": os.cpu_count(),
                   "rustc": subprocess.check_output(["rustc", "-Vv"], text=True).strip(),
                   "method": {"model": "fixed fake model; no real provider",
                              "physicalAdapter": "existing owned empty adapter; real authorization/allocation/cleanup receipts",
                              "ordinaryBoundary": "schedule HTTP begins -> model adapter begins; model adapter returns -> committed successful Job observed via PG notification/query",
                              "wakeBoundary": "source Job terminal transaction commit -> immutable wake event transaction commit, PostgreSQL commit timestamps",
                              "wakeEligibility": "parent remains queued; runAt <= source completion time; no parent executor in component measurement",
                              "testOnlyExtensions": "identical Core/store waiter seeding commands and initial owned physical adapter registration in both test executables; candidate starts only production outbox fast path for the wake component case",
                              "statistics": "nearest rank; warmup discarded; same deterministic arrival jitter; sequential A then B cases",
                              "unchangedPlatformReconciler": "disabled in both ordinary fixture cases",
                              "rpcCounter": "old Worker-to-Runtime calls bound to measured Jobs; separate server-observed coordinator routes; excludes idle claim and schedule/observation requests; zero does not mean all HTTP traffic is removed",
                              "notMeasured": ["real provider latency", "real sandbox creation", "production throughput", "whole-system CPU/RSS", "database query cost", "lost-notification latency"]}})
    with tempfile.TemporaryDirectory(prefix="centaeris-lifecycle-comparison-") as directory:
        previous_tempdir = tempfile.tempdir
        tempfile.tempdir = directory
        try:
            with patch.dict(os.environ, {"DJANGO_SETTINGS_MODULE": "api.test_settings", "TEMP": directory, "TMP": directory,
                                        "NO_PROXY": "127.0.0.1,localhost", "no_proxy": "127.0.0.1,localhost"}):
                sys.path.insert(0, str(ROOT / "packages/workspace/api"))
                import django
                django.setup()
                from django.test.runner import DiscoverRunner
                define_cases(options)
                class ReportingRunner(DiscoverRunner):
                    def run_suite(self, suite, **kwargs):
                        result = super().run_suite(suite, **kwargs)
                        for test, _ in result.failures + result.errors:
                            if hasattr(test, "measurement_case"):
                                test.measurement_case["outcome"] = "failed"
                        REPORT["testResults"] = {"executedCases": result.testsRun,
                            "passedCases": result.testsRun - len(result.failures) - len(result.errors) - len(result.skipped),
                            "failedCases": [test.id() for test, _ in result.failures],
                            "errorCases": [test.id() for test, _ in result.errors],
                            "skippedCases": [test.id() for test, _ in result.skipped]}
                        return result
                names = {"baselineRun": "test_a_baseline_run", "candidateRun": "test_b_candidate_run",
                         "baselineWake": "test_c_baseline_wake", "candidateWake": "test_d_candidate_wake"}
                labels = [__name__ + ".Comparison." + names[name] for name in (options.case or names)]
                status = ReportingRunner(verbosity=1, interactive=False).run_tests(labels)
                REPORT["testExitCode"] = status
        finally:
            tempfile.tempdir = previous_tempdir
            options.output.write_text(json.dumps(REPORT, indent=2), encoding="utf-8")
            if build_source is not None:
                build_source.cleanup()
    return status


if __name__ == "__main__":
    raise SystemExit(main())
