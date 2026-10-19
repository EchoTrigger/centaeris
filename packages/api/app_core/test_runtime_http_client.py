import json
import threading
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from types import SimpleNamespace
from unittest.mock import patch

from django.conf import settings
from django.test import SimpleTestCase

from app_core import runtime_client, runtime_job_client


class RuntimeHttpClientTests(SimpleTestCase):
    @classmethod
    def setUpClass(cls):
        super().setUpClass()

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                body = self.rfile.read(int(self.headers.get("Content-Length", 0)))
                self.server.requests.append((self.command, self.path, dict(self.headers), body))
                self.send_response(self.server.reply_status)
                self.send_header("Content-Length", str(len(self.server.reply_body)))
                self.end_headers()
                self.wfile.write(self.server.reply_body)

            do_POST = do_GET

            def log_message(self, *args):
                pass

        cls.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        cls.thread = threading.Thread(target=cls.server.serve_forever, daemon=True)
        cls.thread.start()
        cls.addClassCleanup(cls.server.server_close)
        cls.addClassCleanup(cls.thread.join, 5)
        cls.addClassCleanup(cls.server.shutdown)

    def setUp(self):
        self.server.requests = []
        self.override = self.settings(
            RUNTIME_URL=f"http://127.0.0.1:{self.server.server_port}/",
            RUNTIME_CONTROL_TIMEOUT_SECONDS=3,
            RUNTIME_START_TIMEOUT_SECONDS=7,
        )
        self.override.enable()
        self.addCleanup(self.override.disable)

    def reply(self, status, value):
        self.server.reply_status = status
        self.server.reply_body = value if isinstance(value, bytes) else json.dumps(value).encode()

    def test_get_and_post_preserve_wire_and_timeout(self):
        self.reply(200, {"job": {"jobId": "job_1"}})
        with patch("urllib.request.urlopen", wraps=urllib.request.urlopen) as opened:
            self.assertEqual(runtime_job_client.get_runtime_job("job_1"), {"jobId": "job_1"})
            self.assertEqual(opened.call_args.kwargs["timeout"], 3)
        method, path, headers, body = self.server.requests[-1]
        self.assertEqual((method, path, body), ("GET", "/internal/jobs/job_1", b""))
        self.assertEqual(headers["X-Internal-Token"], settings.INTERNAL_API_TOKEN)
        self.assertNotIn("Content-Type", headers)

        result = {"schema": "workspace.skill.catalog.result.v1", "skills": []}
        self.reply(200, result)
        with patch("urllib.request.urlopen", wraps=urllib.request.urlopen) as opened:
            self.assertEqual(runtime_client.request_workspace_skill_catalog({}), result)
            self.assertEqual(opened.call_args.kwargs["timeout"], 7)
        method, path, headers, body = self.server.requests[-1]
        self.assertEqual((method, path), ("POST", "/skills/catalog"))
        self.assertEqual(headers["Content-Type"], "application/json")
        self.assertEqual(body, b'{"schema":"workspace.skill.catalog.v1","pluginActivation":{}}')

    def test_schedule_preserves_caller_deadline_and_default_timeout(self):
        result = {"disposition": "inserted", "job": {"jobId": "job_1"}}
        self.reply(200, result)
        for options, expected in [({}, 7), ({"timeout": 0.25}, 0.25)]:
            with self.subTest(options=options), patch("urllib.request.urlopen", wraps=urllib.request.urlopen) as opened:
                self.assertEqual(runtime_job_client.schedule_runtime_job({"jobId": "job_1"}, **options), result)
                self.assertEqual(opened.call_args.kwargs["timeout"], expected)
            method, path, _, body = self.server.requests[-1]
            self.assertEqual((method, path, body), ("POST", "/internal/jobs/schedule", b'{"jobId":"job_1"}'))

    def test_operation_specific_http_errors_are_not_retried(self):
        run = SimpleNamespace(id="agent_1", session_id="session_1")
        for operation, status, payload, exception, message in [
            (lambda: runtime_client.request_workspace_skill_detail({}, "skill"), 404,
             {"error": "skill_not_found"}, LookupError, "skill_not_found"),
            (lambda: runtime_client.request_plugin_inspection("upload/package"), 400,
             {"error": "plugin_package_invalid"}, ValueError, "plugin_package_invalid"),
            (lambda: runtime_client.request_agent_run_supplement(run, "supplement", "hello"), 409,
             {"error": "queue_full"}, ValueError, "queue_full"),
            (lambda: runtime_job_client.schedule_runtime_job({"jobId": "job_1"}), 503,
             {"error": "busy"}, RuntimeError, "runtime_job_schedule_failed"),
        ]:
            with self.subTest(status=status, message=message), patch.object(runtime_client, "build_agent_run_control", return_value={}):
                self.reply(status, payload)
                before = len(self.server.requests)
                with self.assertRaisesRegex(exception, message):
                    operation()
                self.assertEqual(len(self.server.requests), before + 1)
        self.reply(404, {"error": "job_not_found"})
        self.assertIsNone(runtime_job_client.get_runtime_job("missing"))

    def test_invalid_json_and_timeout_keep_caller_error_policy(self):
        self.reply(200, b"{")
        with self.assertRaisesRegex(RuntimeError, "runtime_job_query_failed"):
            runtime_job_client.get_runtime_job("job_1")
        for operation, exception in [
            (lambda: runtime_job_client.get_runtime_job("job_1"), RuntimeError),
            (lambda: runtime_job_client.schedule_runtime_job({"jobId": "job_1"}), TimeoutError),
        ]:
            with self.subTest(exception=exception), patch("urllib.request.urlopen", side_effect=TimeoutError):
                with self.assertRaises(exception):
                    operation()
