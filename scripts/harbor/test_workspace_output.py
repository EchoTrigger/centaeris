"""Read-only stream projection: identities, revisions, and bounded retention."""
import json
from pathlib import Path
import tempfile
import unittest
import test_workspace_client as client_tests

from workspace_output import OutputWindow, OutputClient, publish
from workspace_client import WorkspaceError


class OutputTests(unittest.TestCase):
    def setUp(self):
        self.window = OutputWindow("trial", "session", "run")

    def live(self, revision=1, text="hello", after=0):
        return {"schema": "session.stream.item.v1", "kind": "live", "agentRunId": "run",
            "messageId": "message", "turnId": "turn", "afterSequence": after,
            "revision": revision, "text": text}

    def committed(self, sequence=1, kind="assistant_message", payload=None):
        return {"schema": "session.stream.item.v1", "kind": "committed", "agentRunId": "run",
            "sourceSequence": sequence, "event": {"schemaVersion": "session.event.v1", "eventVersion": 1,
                "sequence": sequence, "sessionId": "session", "agentRunId": "run", "turnId": "turn",
                "eventId": "event-" + str(sequence), "createdAtMs": 1, "type": kind,
                "payload": payload or {"messageId": "message", "modelMarkdown": "final"}}}

    def test_live_snapshots_replace_and_committed_message_seals_overlay(self):
        self.window.apply(self.live())
        self.window.apply(self.live(2, "hello world"))
        self.window.apply(self.live(1, "old"))
        self.assertEqual([e["text"] for e in self.window.snapshot()["entries"]], ["hello world"])
        self.window.apply(self.committed())
        self.window.apply(self.live(3, "late"))
        self.assertEqual([e["text"] for e in self.window.snapshot()["entries"]], ["final"])

    def test_assistant_body_does_not_mark_run_terminal(self):
        self.window.apply(self.committed(payload={'messageId':'message','modelMarkdown':'Done','artifactRefs':[],'status':'done'}))
        self.assertEqual(self.window.snapshot()['entries'][0]['text'],'Done')
        self.assertFalse(self.window.terminal)
        self.window.apply(self.committed(2,'agent_run_completed',{'status':'completed'}))
        self.assertTrue(self.window.terminal)

    def test_reconnect_does_not_duplicate_committed_output(self):
        self.window.apply(self.committed())
        self.window.apply(self.committed())
        self.assertEqual(len(self.window.snapshot()["entries"]), 1)
        self.assertEqual(self.window.sequence, 1)

    def test_tool_arguments_and_results_remain_literal_and_secrets_are_redacted(self):
        self.window.apply(self.committed(kind="tool_call", payload={"callId": "call", "toolName": "bash",
            "normalizedInput": {"command": "<script>bad()</script> TOKEN=private-value"}}))
        self.window.apply(self.committed(2, "tool_result", {"callId": "call", "toolName": "bash",
            "modelContent": "ok", "resultState": "successWithOutput"}))
        result = json.dumps(self.window.snapshot())
        self.assertIn("<script>", result)
        self.assertIn("ok", result)
        self.assertNotIn("private-value", result)

    def test_window_is_bounded_in_records_and_utf8_bytes(self):
        for i in range(1, 101):
            self.window.apply(self.committed(i, payload={"messageId": str(i), "modelMarkdown": "界" * 10000}))
        value = self.window.snapshot()
        self.assertLessEqual(len(value["entries"]), 40)
        self.assertLessEqual(sum(len(e["text"].encode()) for e in value["entries"]), 65536)
        self.assertTrue(value["truncated"])
        self.assertIn("界", value["entries"][-1]["text"])

    def test_wrong_run_session_and_unsupported_schema_do_not_advance(self):
        for field, value in (("agentRunId", "other"), ("schema", "future")):
            item = self.committed()
            item[field] = value
            with self.assertRaises(ValueError): self.window.apply(item)
        item = self.committed()
        item["event"]["sessionId"] = "other"
        with self.assertRaises(ValueError): self.window.apply(item)
        self.assertEqual(self.window.sequence, 0)

    def test_phase_barrier_discards_stale_live_and_unknown_body_is_not_fabricated(self):
        self.window.apply(self.live())
        self.window.apply(self.committed(kind="phase_event", payload={"phase": "tools"}))
        self.window.apply(self.live(2, "late"))
        self.assertFalse(any("late" in e["text"] or e["kind"].endswith("streaming") for e in self.window.snapshot()["entries"]))
        self.window.apply(self.committed(2, payload={"messageId": "message", "textRef": {"id": "private"}}))
        self.assertIn("not available", self.window.snapshot()["entries"][-1]["text"])

    def test_output_client_cannot_issue_execution_or_maintenance_requests(self):
        client = OutputClient("http://127.0.0.1:18280", self.window, lambda: None)
        for method, path in (("POST", "/api/sessions/x/messages"), ("POST", "/internal/environments/retire"),
                             ("GET", "/api/sessions/x"), ("GET", "/api/sessions/other/agent-runs/run/events")):
            with self.assertRaises(WorkspaceError): client._request(method, path)
        self.assertEqual(client._request("GET", "/api/sessions/session/agent-runs/run/events").method, "GET")

    def test_publish_is_atomic_public_and_does_not_export_private_fields(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            publish(root, "job", [self.window], error="unavailable")
            value = json.loads((root / "output.json").read_text(encoding="utf-8"))
            self.assertEqual(value["jobId"], "job")
            self.assertEqual(value["error"], "unavailable")
            self.assertEqual(len(value["trials"]), 1)
            self.assertEqual(sorted(p.name for p in root.iterdir()), ["output.json"])

    def test_real_sse_login_revisions_commit_and_reconnect_are_read_only(self):
        fixture = client_tests.WorkspaceClientTests()
        fixture.setUp()
        try:
            client = OutputClient(fixture.client.base_url, self.window)
            client.login("test@example.test", "fake-password")
            fixture.stream_items = [(None, self.live()), (None, self.live(2, "expanded")),
                ("cursor-1", self.committed())]
            first = client.read_run_events("session", "run")
            self.assertEqual(first["cursor"], "cursor-1")
            self.assertEqual([e["text"] for e in self.window.snapshot()["entries"]], ["final"])
            fixture.stream_items = [("cursor-1", self.committed()),
                ("cursor-2", self.committed(2, "agent_run_completed"))]
            second = client.read_run_events("session", "run", first["cursor"], after_sequence=first["sourceSequence"])
            self.assertEqual(second["status"], "completed")
            self.assertEqual(len(self.window.snapshot()["entries"]), 1)
            self.assertTrue(self.window.terminal)
            self.assertEqual(fixture.requests[-1][2]["Last-Event-Id"], "cursor-1")
            self.assertEqual([(method, path) for method, path, _, _ in fixture.requests], [
                ("GET", "/api/csrf"), ("POST", "/api/login"), ("GET", "/api/csrf"),
                ("GET", "/api/sessions/session/agent-runs/run/events"),
                ("GET", "/api/sessions/session/agent-runs/run/events")])
        finally:
            fixture.doCleanups()


if __name__ == "__main__": unittest.main()
