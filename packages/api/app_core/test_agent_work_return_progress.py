"""Real worker/API/slow Runtime HTTP; source progress survives a page budget."""
import json
import os
from pathlib import Path
import subprocess
import sys
import threading
from collections import Counter
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from django.conf import settings
from django.core.signals import request_finished, request_started
from django.db import connection
from django.test import LiveServerTestCase, override_settings

from . import test_agent_work as fixture
from . import test_agent_work_return_runtime as runtime_fixture
from .models import AgentRun, AgentWorkReturn


class WorkReturnProgressTests(LiveServerTestCase):
    serialized_rollback = True
    setUp = fixture.AgentWorkTests.setUp
    request_fact = fixture.AgentWorkTests.request_fact
    dependencies = fixture.AgentWorkTests.dependencies
    post = fixture.AgentWorkTests.post
    commit_facts = runtime_fixture.WorkReturnRuntimeTests.commit_facts

    @override_settings(EXECUTION_TENANT_QUEUE_LIMIT=200, EXECUTION_GLOBAL_QUEUE_LIMIT=200)
    def test_slow_first_page_preserves_attempted_progress_and_reaches_later_terminal(self):
        self.dependencies()
        children = []
        for index in range(101):
            response = self.post(self.request_fact(call_id=f"progress-{index}"))
            self.assertEqual(response.status_code, 201, response.content)
            operation = response.json()["operation"]
            children.append(AgentRun.objects.get(pk=operation["agentRunId"]))
        children = list(AgentRun.objects.filter(pk__in=[child.pk for child in children]).order_by("session_id"))
        slow, terminal = children[:2], children[-1]
        self.commit_facts([("lifecycleFailure", child) for child in slow] + [("completed", terminal)])
        AgentRun.objects.filter(pk__in=[child.pk for child in slow]).update(
            status="failed", transitionReason="agent_run_lifecycle_dead_lettered")
        from .agent_work_returns import _bound_work
        for child in slow:
            bound, current = _bound_work(child.id)
            self.assertIsNotNone(bound)
            self.assertEqual((current.status, current.transitionReason), ("failed", "agent_run_lifecycle_dead_lettered"))
        with connection.cursor() as cursor:
            cursor.execute("SELECT job_id,job_kind,status,session_id,payload_ref,idempotency_key,updated_at_ms FROM runtime.runtime_jobs")
            jobs = {row[0]: dict(zip(("jobId", "jobKind", "status", "sessionId", "payloadRef", "idempotencyKey", "updatedAtMs"), row))
                    for row in cursor.fetchall()}

        release = threading.Event()
        requested = Counter()
        lock = threading.Lock()
        class RuntimeHandler(BaseHTTPRequestHandler):
            def do_GET(handler):
                if handler.headers.get("X-Internal-Token") != settings.INTERNAL_API_TOKEN:
                    handler.send_error(401); return
                job_id = handler.path.removeprefix("/internal/jobs/")
                with lock:
                    requested[job_id] += 1
                # Both production client timeouts stay at five seconds. This is
                # a real blocked socket read, not an immediate failure mock.
                release.wait(6)
                body = json.dumps({"job": jobs[job_id]}).encode()
                try:
                    handler.send_response(200)
                    handler.send_header("Content-Type", "application/json")
                    handler.send_header("Content-Length", str(len(body)))
                    handler.end_headers()
                    handler.wfile.write(body)
                except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError):
                    pass
            def log_message(handler, *args):
                pass

        runtime = ThreadingHTTPServer(("127.0.0.1", 0), RuntimeHandler)
        thread = threading.Thread(target=runtime.serve_forever)
        thread.start()
        # Track whole HTTP requests so timed-out page requests
        # finish before the disposable database and settings are restored.
        active, idle = 0, threading.Event()
        idle.set()
        def started(sender, **kwargs):
            nonlocal active
            with lock:
                active += 1; idle.clear()
        def finished(sender, **kwargs):
            nonlocal active
            with lock:
                active -= 1
                if active == 0: idle.set()
        request_started.connect(started, weak=False)
        request_finished.connect(finished, weak=False)

        script = """import json, time, worker
scan = worker.WorkReturnPublisher()
rounds = []
for index in range(6):
    start = time.monotonic()
    error = None
    try:
        scan()
    except worker.DependencyUnavailable as failure:
        error = type(failure).__name__
    rounds.append({'after': scan.after, 'through': scan.through, 'error': error, 'elapsed': time.monotonic()-start})
print('work-return-http-progress: ' + json.dumps(rounds))
"""
        try:
            with override_settings(RUNTIME_URL=f"http://127.0.0.1:{runtime.server_port}", RUNTIME_CONTROL_TIMEOUT_SECONDS=5):
                try:
                    result = subprocess.run([sys.executable, "-B", "-c", script],
                        cwd=Path(__file__).resolve().parents[2] / "worker",
                        env={**os.environ, "API_INTERNAL_URL": self.live_server_url, "RUNTIME_INTERNAL_URL": "http://127.0.0.1:1",
                            "INTERNAL_API_TOKEN": settings.INTERNAL_API_TOKEN, "NO_PROXY": "127.0.0.1,localhost"},
                        capture_output=True, text=True, encoding="utf-8", timeout=90)
                finally:
                    release.set()
                    self.assertTrue(idle.wait(20), "in-flight materialization did not finish")
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                rounds = json.loads(next(line.removeprefix("work-return-http-progress: ") for line in result.stdout.splitlines()
                    if line.startswith("work-return-http-progress: ")))
                snapshot = {"rounds": rounds, "slowRequests": dict(requested),
                    "terminalDelivered": AgentWorkReturn.objects.filter(child_run=terminal).exists()}
                print("work-return-http-progress-evidence: " + json.dumps(snapshot))
                self.assertEqual([rounds[0]["after"], rounds[1]["after"]], [slow[0].session_id, slow[1].session_id], snapshot)
                self.assertEqual(rounds[0]["through"], terminal.session_id)
                self.assertTrue(snapshot["terminalDelivered"], snapshot)
                self.assertTrue(all(requested["agent_run.lifecycle:" + child.id] >= 2 for child in slow), snapshot)
                self.assertEqual(AgentRun.objects.count(), 102)
                print("work-return-slow-http-later-terminal-progress-ok")
        finally:
            release.set()
            runtime.shutdown(); thread.join(5); runtime.server_close()
            request_started.disconnect(started)
            request_finished.disconnect(finished)
            self.assertFalse(thread.is_alive())
