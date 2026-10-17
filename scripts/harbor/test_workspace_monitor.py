"""Behavioral coverage for the read-only Workspace benchmark monitor."""
import importlib.util
import copy
import json
import os
from pathlib import Path
import tempfile
import threading
import subprocess
import unittest
from unittest.mock import patch
from urllib.error import HTTPError
from urllib.request import Request, build_opener, ProxyHandler

urlopen = build_opener(ProxyHandler({})).open


MODULE = Path(__file__).with_name("workspace_monitor.py")
spec = importlib.util.spec_from_file_location("workspace_monitor", MODULE)
monitor = importlib.util.module_from_spec(spec)
if MODULE.exists():
    spec.loader.exec_module(monitor)


class MonitorTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.manifest = {"schema": "centaeris.workspace_benchmark.job.v1", "jobId": "pilot",
            "title": "20-task pilot", "taskCount": 2, "trialCount": 2, "attemptsPerTask": 1, "maxConcurrent": 2,
            "tasks": [{"taskId": "ordinary", "memoryMiB": 4096}, {"taskId": "large", "memoryMiB": 8192}]}

    def tearDown(self):
        self.temp.cleanup()

    def prepare(self):
        (self.root / "manifest.json").write_text(json.dumps(self.manifest), encoding="utf-8")
        return monitor.JobProjection(self.root, clock=lambda: 1800000000)

    def event(self, sequence=1, **values):
        return {"schema": "centaeris.workspace_benchmark.event.v1", "sequence": sequence,
            "observedAt": "2027-01-15T08:00:00Z", "kind": "trial", "trialId": "ordinary-1",
            "payload": {"taskId": "ordinary", "attempt": 1, "status": "running", "phase": "agent",
                "slot": 1, "sessionId": "session-original", "agentRunId": "run-original",
                "agentStatus": "running", "verifierStatus": "notStarted", "resourceHolding": 1,
                "cleanupStatus": "notStarted"}, **values}

    def append(self, event):
        with (self.root / "events.jsonl").open("a", encoding="utf-8") as output:
            output.write(json.dumps(event) + "\n")

    def test_empty_directory_has_no_fabricated_run(self):
        state = monitor.JobProjection(self.root).snapshot()
        self.assertEqual(state["monitorStatus"], "noLiveRun")
        self.assertIsNone(state["job"])
        self.assertEqual(state["trials"], [])

    def output(self):
        return {"schema": "centaeris.workspaceBenchmarkOutput.v1", "jobId": "pilot",
            "observedAt": "2027-01-15T08:00:00Z", "error": None, "trials": [{
                "trialId": "ordinary-1", "sessionId": "session-original", "agentRunId": "run-original",
                "sourceSequence": 4, "truncated": False, "error": None,
                "entries": [{"id": "message", "kind": "Assistant", "text": "real output"}]}]}

    def test_output_is_optional_and_bound_to_current_job_run_and_session(self):
        projection = self.prepare()
        self.append(self.event())
        self.assertEqual(projection.snapshot()["outputs"], [])
        value = self.output()
        (self.root / "output.json").write_text(json.dumps(value), encoding="utf-8")
        self.assertEqual(projection.snapshot()["outputs"][0]["entries"][0]["text"], "real output")
        for part, key in ((value, "jobId"), (value["trials"][0], "sessionId"), (value["trials"][0], "agentRunId")):
            original = part[key]
            part[key] = "wrong"
            (self.root / "output.json").write_text(json.dumps(value), encoding="utf-8")
            state = projection.snapshot()
            self.assertEqual(state["outputs"], [])
            self.assertIsNotNone(state["outputError"])
            self.assertEqual(state["trials"][0]["status"], "running")
            self.assertNotEqual(state["monitorStatus"], "invalid")
            part[key] = original

    def test_initial_read_error_recovers_without_restart_or_output_identity_bypass(self):
        projection = self.prepare()
        self.append(self.event())
        (self.root / "output.json").write_text(json.dumps(self.output()), encoding="utf-8")
        with patch.object(projection, "_open", side_effect=PermissionError("private path must not leak")):
            unavailable = projection.snapshot()
        self.assertEqual(unavailable["monitorError"], "fileReadError")
        recovered = projection.snapshot()
        self.assertEqual(recovered["monitorStatus"], "live")
        self.assertIsNone(recovered["monitorError"])
        self.assertEqual(recovered["sequence"], 1)
        self.assertEqual(recovered["trials"][0]["status"], "running")
        self.assertEqual(recovered["outputs"][0]["entries"][0]["text"], "real output")
        self.assertNotIn("private path", json.dumps(unavailable))

    def test_event_read_error_defers_output_and_recovers_last_confirmed_state(self):
        projection = self.prepare()
        self.append(self.event())
        projection.snapshot()
        self.append(self.event(2))
        (self.root / "output.json").write_text(json.dumps(self.output()), encoding="utf-8")
        original = projection._open
        def open_file(name):
            if name == "events.jsonl": raise PermissionError("not public")
            return original(name)
        with patch.object(projection, "_open", side_effect=open_file):
            unavailable = projection.snapshot()
        self.assertEqual(unavailable["sequence"], 1)
        self.assertEqual(unavailable["trials"][0]["status"], "running")
        self.assertEqual(unavailable["outputs"], [])
        self.assertEqual(unavailable["outputError"], "lifecycleUnavailable")
        recovered = projection.snapshot()
        self.assertEqual(recovered["sequence"], 2)
        self.assertIsNone(recovered["monitorError"])
        self.assertIsNone(recovered["outputError"])
        # A different Run is still rejected after recovery.
        value = self.output()
        value["trials"][0]["agentRunId"] = "wrong-run"
        (self.root / "output.json").write_text(json.dumps(value), encoding="utf-8")
        self.assertEqual(projection.snapshot()["outputError"], "outputRunMismatch")

    def test_read_retry_cannot_skip_a_corrupt_event_or_advance_its_cursor(self):
        projection = self.prepare()
        self.append(self.event())
        self.append(self.event(3))
        for _ in range(2):
            state = projection.snapshot()
            self.assertEqual(state["monitorError"], "eventSequenceGap")
            self.assertEqual(state["sequence"], 1)

    def test_invalid_and_oversized_output_fail_independently(self):
        projection = self.prepare()
        self.append(self.event())
        value = self.output()
        value["trials"][0]["entries"][0]["text"] = "z" * 70000
        for wire in (json.dumps(value), "{" + " " * 1024 * 1024):
            (self.root / "output.json").write_text(wire, encoding="utf-8")
            state = projection.snapshot()
            self.assertEqual(state["outputs"], [])
            self.assertIsNotNone(state["outputError"])
            self.assertEqual(state["trials"][0]["status"], "running")

    def test_invalid_unicode_output_is_an_observation_error(self):
        projection = self.prepare()
        self.append(self.event())
        value = self.output()
        value["trials"][0]["entries"][0]["text"] = "\ud800"
        (self.root / "output.json").write_text(json.dumps(value), encoding="utf-8")
        state = projection.snapshot()
        self.assertEqual(state["outputs"], [])
        self.assertIsNotNone(state["outputError"])
        self.assertEqual(state["trials"][0]["status"], "running")

    def test_elapsed_anchors_survive_refresh_and_terminal_updates(self):
        projection = self.prepare()
        projection.clock = lambda: 1800001000
        self.append(self.event())
        self.assertEqual(projection.snapshot()["trials"][0]["startedAt"], "2027-01-15T08:00:00Z")
        terminal = self.event(2, observedAt="2027-01-15T08:05:00Z")
        terminal["payload"].update(status="failed", phase="cleanup", resourceHolding=1)
        self.append(terminal)
        later = self.event(3, observedAt="2027-01-15T08:06:00Z")
        later["payload"].update(status="failed", phase="finished", resourceHolding=0)
        self.append(later)
        row = projection.snapshot()["trials"][0]
        self.assertEqual(row["finishedAt"], terminal["observedAt"])
        self.assertEqual(monitor.JobProjection(self.root, clock=lambda: 1800001000).snapshot()["trials"][0]["startedAt"], row["startedAt"])

    def test_real_projection_separates_agent_and_verifier_and_resource_ownership(self):
        projection = self.prepare()
        self.append(self.event())
        state = projection.snapshot()
        self.assertEqual(state["job"]["title"], "20-task pilot")
        self.assertEqual(state["trials"][0]["agentRunId"], "run-original")
        self.assertEqual(state["trials"][0]["verifierStatus"], "notStarted")
        self.assertTrue(state["trials"][0]["resourceHolding"])
        self.assertEqual(len(state["slots"]), 2)
        second = self.event(2)
        second["payload"].update(agentStatus="completed", verifierStatus="failed", reward=0)
        self.append(second)
        state = projection.snapshot()
        self.assertEqual(len(state["trials"]), 1)
        self.assertEqual(state["trials"][0]["reward"], 0)
        self.assertEqual(state["trials"][0]["agentStatus"], "completed")

    def test_large_task_retains_exclusive_display_until_cleanup_releases(self):
        projection = self.prepare()
        event = self.event(trialId="large-1")
        event["payload"].update(taskId="large", phase="cleanup", agentStatus="completed", verifierStatus="passed")
        self.append(event)
        state = projection.snapshot()
        self.assertTrue(state["exclusiveTrial"])
        self.assertEqual(state["exclusiveTrial"], "large-1")

    def metrics(self):
        return {"observedAt": "2027-01-15T08:00:00Z", "usage": {
            "source": "committedProviderUsage", "scope": "mainAgent", "requests": 2,
            "inputTokens": 120, "outputTokens": 20, "promptCacheHitTokens": 90,
            "promptCacheMissTokens": 30, "promptCacheHitRate": 0.75, "complete": True},
            "memory": {"runtimeCurrentBytes": 1048576, "runtimePeakBytes": 2097152,
                "runtimeAnonBytes": None, "taskCurrentBytes": 0, "taskPeakBytes": 4096,
                "taskLimitBytes": 4294967296, "runtimeOomKills": None, "taskOomKills": 0}}

    def test_optional_trial_metrics_preserve_unknown_zero_and_latest_observation(self):
        projection = self.prepare()
        first = self.event()
        first["payload"]["metrics"] = self.metrics()
        self.append(first)
        state = projection.snapshot()
        self.assertNotEqual(state["monitorStatus"], "invalid")
        observed = state["trials"][0]["metrics"]
        self.assertIsNone(observed["memory"]["runtimeAnonBytes"])
        self.assertEqual(observed["memory"]["taskCurrentBytes"], 0)
        self.assertEqual(observed["usage"]["promptCacheHitRate"], 0.75)
        # Status-only updates retain the most recent sampled metrics, not history.
        self.append(self.event(2))
        self.assertEqual(projection.snapshot()["trials"][0]["metrics"], observed)
        updated = self.event(3)
        updated["payload"]["metrics"] = self.metrics()
        updated["payload"]["metrics"]["usage"]["inputTokens"] = None
        updated["payload"]["metrics"]["usage"]["complete"] = False
        updated["payload"]["metrics"]["usage"]["promptCacheHitRate"] = None
        self.append(updated)
        self.assertIsNone(projection.snapshot()["trials"][0]["metrics"]["usage"]["inputTokens"])

    def test_metrics_reject_invalid_types_unknown_fields_and_invented_cache_rates(self):
        base = self.metrics()
        mutations = [
            lambda m: m["memory"].update(taskCurrentBytes=True),
            lambda m: m["memory"].update(taskCurrentBytes=-1),
            lambda m: m["memory"].update(taskPeakBytes=2**53),
            lambda m: m["memory"].update(secret="never-display"),
            lambda m: m["usage"].update(source="contextEstimate"),
            lambda m: m["usage"].update(scope="allAgents"),
            lambda m: m["usage"].update(complete=1),
            lambda m: m["usage"].update(complete=False),
            lambda m: m["usage"].update(promptCacheHitRate=0.9),
            lambda m: m["usage"].update(promptCacheHitTokens=None),
            lambda m: m["usage"].update(promptCacheHitTokens=0, promptCacheMissTokens=0),
            lambda m: m.update(observedAt="not-time"),
            lambda m: m.update(history=[]),
        ]
        for mutate in mutations:
            with self.subTest(mutation=mutations.index(mutate)):
                metrics = copy.deepcopy(base)
                mutate(metrics)
                projection = self.prepare()
                event = self.event()
                event["payload"]["metrics"] = metrics
                (self.root / "events.jsonl").write_text(json.dumps(event) + "\n", encoding="utf-8")
                state = projection.snapshot()
                self.assertEqual(state["monitorStatus"], "invalid")
                self.assertNotIn("never-display", json.dumps(state))

    def test_unknown_metrics_are_nullable_and_do_not_fabricate_usage(self):
        projection = self.prepare()
        metrics = self.metrics()
        for key in metrics["memory"]:
            metrics["memory"][key] = None
        for key in metrics["usage"]:
            if key not in {"source", "scope", "complete"}:
                metrics["usage"][key] = None
        metrics["usage"]["complete"] = False
        event = self.event()
        event["payload"]["metrics"] = metrics
        self.append(event)
        state = projection.snapshot()
        self.assertNotEqual(state["monitorStatus"], "invalid")
        self.assertIsNone(state["trials"][0]["metrics"]["usage"]["promptCacheHitRate"])

    def test_malformed_and_unknown_events_are_monitor_errors_not_job_failure(self):
        projection = self.prepare()
        (self.root / "events.jsonl").write_text('{"broken":\n', encoding="utf-8")
        state = projection.snapshot()
        self.assertEqual(state["monitorStatus"], "invalid")
        self.assertEqual(state["jobStatus"], "unknown")
        (self.root / "events.jsonl").write_text(json.dumps(self.event(kind="secretDump")) + "\n", encoding="utf-8")
        state = projection.snapshot()
        self.assertEqual(state["monitorStatus"], "invalid")
        self.assertNotIn("secretDump", json.dumps(state))

    def test_unknown_manifest_fields_rejected_without_echoing_values(self):
        self.manifest["apiKey"] = "do-not-display-key"
        state = self.prepare().snapshot()
        self.assertEqual(state["monitorStatus"], "invalid")
        self.assertNotIn("do-not-display-key", json.dumps(state))

    def test_log_secrets_redacted_and_untrusted_markup_remains_data(self):
        projection = self.prepare()
        self.append(self.event(kind="log", payload={"message": '<script>alert(1)</script> Authorization: Bearer abc-secret COMMAND_GOAT_KEY=xyz-secret', "level": "error"}))
        state = projection.snapshot()
        text = json.dumps(state)
        self.assertNotIn("abc-secret", text)
        self.assertNotIn("xyz-secret", text)
        self.assertIn("<script>", state["logs"][0]["message"])

    def test_partial_lines_are_not_consumed_and_truncation_resets_cursor(self):
        projection = self.prepare()
        wire = json.dumps(self.event()).encode()
        (self.root / "events.jsonl").write_bytes(wire[:80])
        self.assertEqual(projection.snapshot()["trials"], [])
        with (self.root / "events.jsonl").open("ab") as output:
            output.write(wire[80:] + b"\n")
        self.assertEqual(len(projection.snapshot()["trials"]), 1)
        (self.root / "events.jsonl").write_bytes(b"")
        state = projection.snapshot()
        self.assertEqual(state["trials"], [])
        self.assertEqual(state["cursorResetCount"], 1)

    def test_rewritten_same_size_file_resets_instead_of_using_old_projection(self):
        projection = self.prepare()
        self.append(self.event())
        projection.snapshot()
        path = self.root / "events.jsonl"
        original = path.stat()
        replacement = self.event()
        replacement["payload"]["agentRunId"] = "run-replaced"
        path.write_text(json.dumps(replacement) + "\n", encoding="utf-8")
        # Rapid writes can share filesystem timestamps; detection must inspect
        # the committed event, including identities before its final 64 bytes.
        os.utime(path, ns=(original.st_atime_ns, original.st_mtime_ns))
        self.assertEqual(path.stat().st_size, original.st_size)
        self.assertEqual(path.stat().st_mtime_ns, original.st_mtime_ns)
        state = projection.snapshot()
        self.assertEqual(state["trials"][0]["agentRunId"], "run-replaced")
        self.assertEqual(state["cursorResetCount"], 1)

    def test_single_read_and_log_retention_are_bounded(self):
        projection = self.prepare()
        for sequence in range(1, 401):
            self.append(self.event(sequence, kind="log", payload={"message": "x" * 2000, "level": "info"}))
        first = projection.snapshot()
        self.assertLessEqual(first["readBytes"], monitor.READ_BUDGET)
        self.assertTrue(first["catchingUp"])
        for _ in range(10):
            last = projection.snapshot()
        self.assertFalse(last["catchingUp"])
        self.assertEqual(last["sequence"], 400)
        self.assertLessEqual(len(last["logs"]), 100)

    def test_append_and_partial_append_keep_the_committed_projection(self):
        projection = self.prepare()
        self.append(self.event())
        self.assertEqual(projection.snapshot()["sequence"], 1)
        wire = (json.dumps(self.event(2)) + "\n").encode()
        path = self.root / "events.jsonl"
        with path.open("ab") as output:
            output.write(wire[:80])
        for _ in range(2):
            state = projection.snapshot()
            self.assertEqual(state["sequence"], 1)
            self.assertEqual(state["cursorResetCount"], 0)
        with path.open("ab") as output:
            output.write(wire[80:])
        state = projection.snapshot()
        self.assertEqual(state["sequence"], 2)
        self.assertEqual(state["cursorResetCount"], 0)
        self.assertEqual(projection.snapshot()["readBytes"], 0)

    def test_atomic_log_replacement_resets_to_the_new_stream(self):
        projection = self.prepare()
        self.append(self.event())
        projection.snapshot()
        replacement = self.event()
        replacement["payload"]["agentRunId"] = "run-replaced"
        incoming = self.root / "incoming.jsonl"
        incoming.write_text(json.dumps(replacement) + "\n", encoding="utf-8")
        incoming.replace(self.root / "events.jsonl")
        state = projection.snapshot()
        self.assertEqual(state["trials"][0]["agentRunId"], "run-replaced")
        self.assertEqual(state["cursorResetCount"], 1)

    def test_stale_event_is_explicit_without_changing_trial_status(self):
        projection = self.prepare()
        self.append(self.event(observedAt="2020-01-01T00:00:00Z"))
        state = projection.snapshot()
        self.assertEqual(state["monitorStatus"], "stale")
        self.assertEqual(state["trials"][0]["status"], "running")

    def test_removed_manifest_cannot_leave_an_orphaned_live_projection(self):
        projection = self.prepare()
        self.append(self.event())
        self.assertEqual(len(projection.snapshot()["trials"]), 1)
        (self.root / "manifest.json").unlink()
        state = projection.snapshot()
        self.assertEqual(state["monitorStatus"], "noLiveRun")
        self.assertEqual(state["trials"], [])
        self.assertEqual(state["logs"], [])

    def test_identity_conflict_and_sequence_gap_are_rejected(self):
        projection = self.prepare()
        self.append(self.event())
        self.assertEqual(projection.snapshot()["sequence"], 1)
        event = self.event(3)
        self.append(event)
        state = projection.snapshot()
        self.assertEqual(state["monitorStatus"], "invalid")
        self.assertEqual(state["sequence"], 1)

    def test_wrong_field_types_and_deep_json_become_safe_observation_errors(self):
        projection = self.prepare()
        event = self.event()
        event["payload"]["status"] = {"private": "not-for-output"}
        self.append(event)
        state = projection.snapshot()
        self.assertEqual(state["monitorStatus"], "invalid")
        self.assertNotIn("not-for-output", json.dumps(state))
        (self.root / "events.jsonl").write_text("[" * 2000 + "0" + "]" * 2000 + "\n", encoding="utf-8")
        self.assertEqual(projection.snapshot()["monitorStatus"], "invalid")

    def test_fixed_files_cannot_escape_job_via_symlink(self):
        projection = self.prepare()
        with tempfile.TemporaryDirectory() as outside:
            target = Path(outside) / "secret"
            target.write_text("secret-outside")
            try:
                (self.root / "events.jsonl").symlink_to(target)
            except OSError:
                self.skipTest("symlinks not available to this account")
            state = projection.snapshot()
            self.assertEqual(state["monitorStatus"], "invalid")
            self.assertNotIn("secret-outside", json.dumps(state))

    def test_http_is_read_only_and_has_no_arbitrary_file_endpoint(self):
        projection = self.prepare()
        server = monitor.create_server(projection, port=0)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        url = "http://127.0.0.1:" + str(server.server_port)
        try:
            with urlopen(url + "/api/state") as response:
                self.assertEqual(response.status, 200)
                self.assertEqual(response.headers["Cache-Control"], "no-store")
            with urlopen(url + "/") as response:
                self.assertIn(b'lang="en"', response.read())
                self.assertIn("default-src 'self'", response.headers["Content-Security-Policy"])
            for path in ("/../../manifest.json", "/api/file?path=manifest.json", "/api/state?path=events.jsonl"):
                with self.assertRaises(HTTPError) as failure:
                    urlopen(url + path)
                self.assertEqual(failure.exception.code, 404)
            with self.assertRaises(HTTPError) as failure:
                urlopen(Request(url + "/api/state", data=b"{}", method="POST"))
            self.assertEqual(failure.exception.code, 405)
        finally:
            server.shutdown()
            server.server_close()
            thread.join()

    def test_http_recovers_from_transient_file_error_without_server_restart(self):
        projection = self.prepare()
        self.append(self.event())
        server = monitor.create_server(projection, port=0)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        url = "http://127.0.0.1:" + str(server.server_port) + "/api/state"
        try:
            with patch.object(projection, "_open", side_effect=PermissionError("private")):
                with urlopen(url) as response:
                    self.assertEqual(json.load(response)["monitorError"], "fileReadError")
            with urlopen(url) as response:
                state = json.load(response)
            self.assertEqual(state["monitorStatus"], "live")
            self.assertEqual(state["sequence"], 1)
            self.assertEqual(state["trials"][0]["agentRunId"], "run-original")
        finally:
            server.shutdown()
            server.server_close()
            thread.join()

    @unittest.skipUnless(os.environ.get("WORKSPACE_MONITOR_BROWSER") == "1", "optional browser integration")
    def test_browser_output_follow_timers_refresh_and_disconnect(self):
        projection = self.prepare()
        self.append(self.event())
        fixture = self.root / "browser-state.json"
        fixture.write_text(json.dumps(projection.snapshot()), encoding="utf-8")
        server = monitor.create_server(projection, port=0)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            result = subprocess.run([os.environ["NODE_BINARY"], str(Path(__file__).with_name("test_workspace_monitor_browser.cjs")),
                "http://127.0.0.1:" + str(server.server_port), str(fixture)],
                capture_output=True, text=True, timeout=50)
            self.assertEqual(result.returncode, 0, result.stderr)
        finally:
            server.shutdown()
            server.server_close()
            thread.join()

    @unittest.skipUnless(os.environ.get("WORKSPACE_MONITOR_BROWSER") == "1", "optional browser integration")
    def test_browser_displays_committed_metrics_without_inventing_unknown_values(self):
        projection = self.prepare()
        event = self.event()
        event["payload"]["metrics"] = self.metrics()
        self.append(event)
        server = monitor.create_server(projection, port=0)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        script = r"""
const {chromium} = require(process.env.PLAYWRIGHT_MODULE);
(async () => {
  const browser = await chromium.launch({headless:true, executablePath:process.env.CHROME_BINARY});
  try {
    const page = await browser.newPage({viewport:{width:390,height:844}});
    const errors = [];
    page.on('pageerror', error => errors.push(String(error)));
    await page.goto(process.argv[1]);
    await page.getByRole('heading', {name:'Committed provider usage', exact:true}).waitFor({timeout:4000});
    const facts = await page.locator('#usage-facts').innerText();
    for (const expected of ['120 / 20', '75.0%', '90 / 30', 'Complete']) {
      if (!facts.includes(expected)) throw Error('Missing committed metric: ' + expected);
    }
    const memory = await page.locator('#memory-facts').innerText();
    if (!memory.includes('Unknown') || !memory.includes('0 B')) throw Error('Unknown/zero conflated');
    if (!(await page.locator('#slots').innerText()).includes('Task memory: 0 B / 4.00 GiB')) throw Error('Slot memory missing');
    if (!(await page.locator('#metrics-note').innerText()).includes('excludes in-flight')) throw Error('Usage scope missing');
    await page.getByRole('button', {name:'large / 1', exact:true}).click();
    if (!(await page.locator('#metrics').innerText()).includes('Metrics not observed')) throw Error('Fabricated unobserved metrics');
    if (await page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth)) throw Error('Page overflow');
    if (await page.evaluate(() => getComputedStyle(document.body).backgroundColor) !== 'rgb(0, 0, 0)') throw Error('Background changed');
    if (errors.length) throw Error(errors.join('\n'));
    await page.close();
    const response = await fetch(process.argv[1] + '/api/state');
    if (response.status !== 200) throw Error('Page close stopped monitor');
  } finally { await browser.close(); }
})().catch(error => {console.error(error);process.exitCode=1;});
"""
        try:
            result = subprocess.run([os.environ["NODE_BINARY"], "-e", script,
                "http://127.0.0.1:" + str(server.server_port)], capture_output=True, text=True, timeout=30)
            self.assertEqual(result.returncode, 0, result.stderr)
        finally:
            server.shutdown()
            server.server_close()
            thread.join()


if __name__ == "__main__":
    unittest.main()
