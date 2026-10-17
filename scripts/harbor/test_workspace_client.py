"""Wire-level tests for the public Workspace client; no model or Docker calls."""
import json
import socket
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from workspace_client import WorkspaceClient, WorkspaceError, SubmissionUncertain


class WorkspaceClientTests(unittest.TestCase):
    def setUp(self):
        self.requests = []
        self.receipts = {}
        self.lose_ack = False
        self.stream_items = []
        self.raw_stream = None
        self.preview = {"schema": "session.preview.v1", "sessionId": "session", "runFact": None,
                        "outputs": [], "hasMore": False, "nextAfterArtifactId": None}
        self.redirect = False
        owner = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_GET(self):
                owner.requests.append(("GET", self.path, dict(self.headers), None))
                if self.path == "/api/csrf":
                    self.reply({"csrfToken": "csrf-value"}, cookie="csrftoken=csrf-value; Path=/")
                elif owner.redirect:
                    self.send_response(302)
                    self.send_header("Location", "/credential-collector")
                    self.end_headers()
                elif "/operations/" in self.path:
                    operation = self.path.rsplit("/", 1)[1]
                    if operation in owner.receipts:
                        self.reply(owner.receipts[operation])
                    else:
                        self.reply({"error": "operation_not_found"}, 404)
                elif self.path.endswith("/events"):
                    self.send_response(200)
                    self.send_header("Content-Type", "text/event-stream")
                    self.end_headers()
                    if owner.raw_stream is not None:
                        self.wfile.write(owner.raw_stream)
                        return
                    for cursor, item in owner.stream_items:
                        self.wfile.write((f"id: {cursor}\ndata: {json.dumps(item)}\n\n").encode())
                elif self.path == "/api/sessions/session/preview?limit=1":
                    self.reply(owner.preview)
                else:
                    self.reply({"error": "forbidden secret=do-not-log"}, 403)

            def do_POST(self):
                body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                owner.requests.append(("POST", self.path, dict(self.headers), body))
                if self.path == "/api/login":
                    self.reply({"user": {"id": 1}}, cookie="sessionid=session-value; Path=/")
                    return
                if self.path.endswith("/messages"):
                    receipt = {"operationId": body["operationId"], "command": "submitMessage",
                               "status": "accepted", "sessionId": "session", "agentRunId": "run", "turnId": "turn"}
                else:
                    receipt = {"operationId": body["operationId"], "command": "createSession",
                               "status": "accepted", "sessionId": "session", "agentRunId": None, "turnId": None}
                owner.receipts[body["operationId"]] = receipt
                if owner.lose_ack:
                    self.connection.shutdown(socket.SHUT_RDWR)
                    self.connection.close()
                else:
                    self.reply(receipt, 202 if receipt["agentRunId"] else 201)

            def reply(self, payload, status=200, cookie=None):
                body = json.dumps(payload).encode()
                self.send_response(status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                if cookie:
                    self.send_header("Set-Cookie", cookie)
                self.end_headers()
                self.wfile.write(body)

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.client = WorkspaceClient(f"http://127.0.0.1:{self.server.server_port}", request_timeout=1)
        self.addCleanup(self.server.server_close)
        self.addCleanup(self.server.shutdown)

    def test_login_uses_csrf_cookie_and_forwards_exact_instruction(self):
        self.client.login("test@example.test", "fake-password")
        self.client.create_session("workspace", "agent", "create-op")
        text = "  Exact task\nwith unicode \u03bb\n"
        result = self.client.submit_message("workspace", "session", "agent", "message-op", text, "model", "high")
        self.assertEqual(result["agentRunId"], "run")
        post = self.requests[-1]
        self.assertEqual(post[3], {"operationId": "message-op", "text": text, "agentId": "agent",
                                   "modelConfigRef": "model", "thinkingMode": "high", "attachmentRefs": []})
        self.assertEqual(post[2]["X-Csrftoken"], "csrf-value")
        self.assertIn("sessionid=session-value", post[2]["Cookie"])

    def test_pre_admission_failure_is_read_without_any_session_events(self):
        self.assertEqual(self.stream_items, [])
        fact = {"agentRunId": "run", "status": "failed", "sourceType": "preAdmissionFailure",
                "eventId": None, "createdAtMs": 123}
        self.preview["runFact"] = fact
        self.assertEqual(self.client.run_fact("session", "run"), fact)
        self.assertEqual([(method, path) for method, path, _, _ in self.requests],
                         [("GET", "/api/sessions/session/preview?limit=1")])

    def test_run_fact_requires_exact_session_run_and_authoritative_terminal_source(self):
        fact = {"agentRunId": "run", "status": "failed", "sourceType": "preAdmissionFailure",
                "eventId": None, "createdAtMs": 123}
        for changes in ({"agentRunId": "other"}, {"status": "completed"},
                        {"sourceType": "hostedRun"}, {"sourceType": "live"},
                        {"sourceType": "sessionEvent"}, {"eventId": "invented"}):
            with self.subTest(changes=changes):
                self.preview["runFact"] = {**fact, **changes}
                with self.assertRaises(WorkspaceError):
                    self.client.run_fact("session", "run")
        self.preview.update(sessionId="other", runFact=fact)
        with self.assertRaisesRegex(WorkspaceError, "run_fact_identity_invalid"):
            self.client.run_fact("session", "run")

    def test_run_fact_pending_and_all_supported_committed_terminal_states(self):
        self.assertIsNone(self.client.run_fact("session", "run"))
        for status, source, event in [("queued", "hostedRun", None), ("running", "hostedRun", None),
                                      ("completed", "sessionEvent", "event"), ("failed", "sessionEvent", "event"),
                                      ("cancelled", "sessionEvent", "event"),
                                      ("cancelled", "preAdmissionCancellation", None)]:
            with self.subTest(status=status, source=source):
                self.preview["runFact"] = {"agentRunId": "run", "status": status, "sourceType": source,
                                           "eventId": event, "createdAtMs": 123}
                self.assertEqual(self.client.run_fact("session", "run"), self.preview["runFact"])

    def test_lost_ack_queries_same_operation_without_second_post(self):
        self.lose_ack = True
        result = self.client.create_session("workspace", "agent", "stable")
        self.assertEqual(result["sessionId"], "session")
        self.assertEqual(sum(method == "POST" for method, *_ in self.requests), 1)
        self.assertEqual(self.requests[-1][1], "/api/workspaces/workspace/operations/createSession/stable")

    def test_missing_receipt_after_lost_ack_is_explicit_uncertainty(self):
        self.lose_ack = True
        original = self.client.operation
        self.client.operation = lambda *args: None
        with self.assertRaises(SubmissionUncertain):
            self.client.create_session("workspace", "agent", "stable")
        self.client.operation = original
        self.assertEqual(sum(method == "POST" for method, *_ in self.requests), 1)

    def test_error_does_not_expose_arbitrary_response_body(self):
        with self.assertRaises(WorkspaceError) as caught:
            self.client.session("session")
        self.assertNotIn("do-not-log", str(caught.exception))
        self.assertEqual(caught.exception.status, 403)

    def test_stream_terminal_requires_committed_matching_identity_and_resumes_cursor(self):
        self.stream_items = [("cursor-2", {"schema": "session.stream.item.v1", "kind": "committed",
            "agentRunId": "run", "sourceSequence": 2, "event": {"type": "agent_run_completed",
                "sequence": 2, "sessionId": "session", "agentRunId": "run"}})]
        result = self.client.read_run_events("session", "run", "cursor-1")
        self.assertEqual(result, {"cursor": "cursor-2", "sourceSequence": 2, "status": "completed"})
        self.assertEqual(self.requests[-1][2]["Last-Event-Id"], "cursor-1")
        self.stream_items[0][1]["event"]["sessionId"] = "other"
        with self.assertRaises(WorkspaceError):
            self.client.read_run_events("session", "run")

    def test_live_overlay_cannot_finish_run_or_advance_durable_cursor(self):
        self.stream_items = [("", {"schema": "session.stream.item.v1", "kind": "live", "agentRunId": "run",
                                   "text": "done", "status": "completed"})]
        self.assertEqual(self.client.read_run_events("session", "run"),
                         {"cursor": "0-0", "sourceSequence": 0, "status": "running"})

    def test_partial_event_disconnect_replays_from_previous_durable_cursor(self):
        self.raw_stream = b'id: cursor-2\ndata: {"schema":"session.stream.item.v1"'
        self.assertEqual(self.client.read_run_events("session", "run", "cursor-1", after_sequence=1),
                         {"cursor": "cursor-1", "sourceSequence": 1, "status": "running"})

    def test_stream_backwards_sequence_and_oversize_record_are_rejected(self):
        self.stream_items = [("cursor-2", {"schema": "session.stream.item.v1", "kind": "committed",
            "agentRunId": "run", "sourceSequence": 2, "event": {"type": "agent_run_completed",
                "sequence": 2, "sessionId": "session", "agentRunId": "run"}})]
        with self.assertRaisesRegex(WorkspaceError, "stream_identity_invalid"):
            self.client.read_run_events("session", "run", "cursor-2", after_sequence=2)
        self.raw_stream = b"data: " + b"x" * 1025 + b"\n\n"
        self.client.max_event_bytes = 1024
        with self.assertRaisesRegex(WorkspaceError, "stream_event_too_large"):
            self.client.read_run_events("session", "run")

    def test_credentials_cannot_be_redirected_to_another_endpoint(self):
        self.redirect = True
        with self.assertRaisesRegex(WorkspaceError, "redirect_rejected"):
            self.client.session("session")
        self.assertEqual(len(self.requests), 1)

    def test_receipt_identity_is_checked_when_reconciling(self):
        self.receipts["stable"] = {"operationId": "stable", "command": "submitMessage", "status": "accepted",
                                    "sessionId": "session", "agentRunId": "run", "turnId": "turn"}
        with self.assertRaisesRegex(WorkspaceError, "operation_receipt_invalid"):
            self.client.operation("workspace", "createSession", "stable")

    def test_remote_plaintext_origin_and_url_credentials_are_rejected(self):
        for url in ("http://192.0.2.1", "https://user:password@example.test", "https://example.test/?key=fake"):
            with self.assertRaises(ValueError):
                WorkspaceClient(url)


if __name__ == "__main__":
    unittest.main()
