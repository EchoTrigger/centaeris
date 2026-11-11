import copy
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import io
import json
import os
import threading
import urllib.error
from unittest.mock import patch

from django.test import SimpleTestCase

from .platform_reconciler import InternalApi, Reconciler, run


class PlatformReconcilerTests(SimpleTestCase):
    cursor = {"createdAt": "2026-11-11T00:00:00Z", "id": "agent_run_fixture"}

    @staticmethod
    def active(next_cursor=None):
        return {"scheduled": 0, "terminalized": 0, "pending": 0,
                "activeNext": next_cursor, "deadLetterNext": None}

    @staticmethod
    def cleanup(next_cursor=None):
        return {"schema": "runtime.agent_run_cleanup.reconciled.v1", "checked": 0,
                "pending": [], "next": next_cursor}

    def test_active_page_progress_survives_cleanup_failure_and_restart_starts_a_new_pass(self):
        calls = []
        fail_cleanup = True

        def post(path, body):
            nonlocal fail_cleanup
            calls.append((path, copy.deepcopy(body)))
            if path.endswith("/reconcile"):
                return self.active(self.cursor if len(calls) == 1 else None)
            if fail_cleanup:
                fail_cleanup = False
                raise RuntimeError("unavailable")
            return self.cleanup()

        reconciler = Reconciler(post)
        with self.assertRaises(RuntimeError):
            reconciler.once()
        reconciler.once()
        Reconciler(post).once()
        active = [body for path, body in calls if path.endswith("/reconcile")]
        self.assertIsNone(active[0]["activeAfter"])
        self.assertEqual(active[1]["activeAfter"], self.cursor)
        self.assertIsNone(active[2]["activeAfter"])

    def test_cleanup_page_is_retried_from_its_last_successful_cursor(self):
        bodies = []
        pages = iter([self.cleanup(self.cursor), RuntimeError("offline"), self.cleanup()])

        def post(path, body):
            if path.endswith("/reconcile"):
                return self.active()
            bodies.append(copy.deepcopy(body))
            result = next(pages)
            if isinstance(result, Exception):
                raise result
            return result

        reconciler = Reconciler(post)
        reconciler.once()
        with self.assertRaises(RuntimeError):
            reconciler.once()
        reconciler.once()
        self.assertEqual([body["after"] for body in bodies], [None, self.cursor, self.cursor])

    def test_unsupported_response_does_not_advance_a_cursor(self):
        for response in (self.active() | {"unexpected": True}, self.active() | {"scheduled": True},
                         self.active({"id": "missing-date"})):
            reconciler = Reconciler(lambda *_: response)
            with self.subTest(response=response), self.assertRaises(ValueError):
                reconciler.once()
            self.assertIsNone(reconciler.active_after)

    def test_failed_control_plane_request_logs_no_secret_or_exception_text(self):
        stopped = threading.Event()

        def post(*_):
            stopped.set()
            raise urllib.error.URLError("private-token-and-url")

        output = io.StringIO()
        with patch("sys.stderr", output):
            run(Reconciler(post), stopped)
        self.assertEqual(output.getvalue(), "Platform reconciliation unavailable\n")

    def test_internal_api_request_reaches_its_target_with_proxy_environment_present(self):
        received = []

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self):
                received.append((self.path, self.headers["X-Internal-Token"],
                                 json.loads(self.rfile.read(int(self.headers["Content-Length"])))))
                body = b'{"checked":0}'
                self.send_response(200)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, *_):
                pass

        server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever)
        thread.start()
        try:
            with patch.dict(os.environ, {"HTTP_PROXY": "http://127.0.0.1:1", "HTTPS_PROXY": "http://127.0.0.1:1",
                                         "ALL_PROXY": "http://127.0.0.1:1", "NO_PROXY": ""}):
                client = InternalApi(f"http://127.0.0.1:{server.server_port}", "fixture-token")
                self.assertEqual(client.post("/internal/reconcile", {"limit": 100}), {"checked": 0})
            self.assertEqual(received, [("/internal/reconcile", "fixture-token", {"limit": 100})])
        finally:
            server.shutdown()
            server.server_close()
            thread.join()
