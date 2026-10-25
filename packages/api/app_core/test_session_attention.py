"""Notification identity and observed-read boundaries over real HTTP and storage."""
import json
from concurrent.futures import ThreadPoolExecutor
from threading import Barrier

from django.contrib.auth.models import User
from django.db import close_old_connections, connection, connections
from django.test import Client, TestCase, TransactionTestCase
from django.test.utils import CaptureQueriesContext

from .http.serialization import serialize_session
from .models import AgentRun, ModelConfig, Session, SessionEvent, Workspace
from .session_event import project_committed_agent_run
from .testing import create_session
from .tests import append_completed, append_session_records, append_started, session_record


class SessionAttentionTests(TestCase):
    def setUp(self):
        self.user = User.objects.create_user(username="attention-owner")
        self.workspace = Workspace.objects.create(name="Attention", createdBy=self.user)
        self.workspace.members.add(self.user)
        self.session = create_session(workspace=self.workspace, owner=self.user)
        self.model = ModelConfig.objects.create(id="attention-model", displayName="Attention")
        self.run = self.new_run()
        self.client.force_login(self.user)

    def new_run(self):
        return AgentRun.objects.create(workspace=self.workspace, session=self.session,
            user=self.user, modelConfig=self.model, prompt="Synthetic request")

    def post_read(self, sequence):
        return self.client.post(f"/api/sessions/{self.session.id}/read",
            data=json.dumps({"throughSequence": sequence}), content_type="application/json")

    def mark_unread(self, unread):
        return self.client.patch(f"/api/sessions/{self.session.id}",
            data=json.dumps({"isUnread": unread}), content_type="application/json")

    def question(self):
        append_started(self.run)
        append_session_records(self.run, [session_record(self.run, 3, "checkpoint_ref", {
            "checkpointId": "checkpoint:question", "kind": "wait", "objectRef": "checkpoint:object",
            "status": "paused_question", "payloadSha256": "sha256:" + "0" * 64,
            "payloadByteLength": 42, "updatedAtMs": 3,
        })])

    def test_repeated_terminal_projection_does_not_restore_a_read_notification(self):
        append_started(self.run)
        append_completed(self.run)
        self.assertEqual(self.mark_unread(False).status_code, 200)
        project_committed_agent_run(self.run, "completed")
        self.session.refresh_from_db()
        self.assertFalse(serialize_session(self.session)["isUnread"])

    def test_current_view_acknowledges_only_applied_watermark_and_replay_is_harmless(self):
        append_started(self.run)
        append_completed(self.run)
        response = self.post_read("4")
        self.assertEqual(response.status_code, 200, response.content)
        self.assertEqual(response.json()["session"]["readThroughSequence"], "4")
        self.assertFalse(response.json()["session"]["isUnread"])
        repeated = self.post_read("2")
        self.assertEqual(repeated.status_code, 200)
        self.assertEqual(repeated.json()["session"]["readThroughSequence"], "4")
        project_committed_agent_run(self.run)
        self.session.refresh_from_db()
        self.assertFalse(serialize_session(self.session)["isUnread"])

    def test_old_ack_does_not_clear_newer_completion(self):
        append_started(self.run)
        append_completed(self.run)
        self.assertEqual(self.post_read("4").status_code, 200)
        second = self.new_run()
        append_started(second)
        append_completed(second)
        stale = self.post_read("4")
        self.assertEqual(stale.status_code, 200, stale.content)
        session = stale.json()["session"]
        self.assertEqual(session["latestAttentionSequence"], "8")
        self.assertEqual(session["readThroughSequence"], "4")
        self.assertTrue(session["isUnread"])

    def test_question_attention_is_available_before_terminal_and_ignores_stream_filter(self):
        self.question()
        self.session.refresh_from_db()
        serialized = serialize_session(self.session)
        self.assertTrue(serialized["isUnread"])
        self.assertEqual(serialized["latestAttentionSequence"], "3")
        self.assertFalse(SessionEvent.objects.get(session=self.session, sequence=3).projects_to_agent_run_stream)
        response = self.post_read("3")
        self.assertEqual(response.status_code, 200, response.content)
        self.assertFalse(response.json()["session"]["isUnread"])
        self.run.refresh_from_db()
        self.assertEqual(self.run.status, "queued")

    def test_plain_running_and_interim_assistant_text_do_not_create_attention(self):
        append_started(self.run)
        append_session_records(self.run, [session_record(self.run, 3, "assistant_message", {
            "messageId": "message:interim", "modelMarkdown": "Investigating", "artifactRefs": [], "status": "done",
        })])
        serialized = serialize_session(self.session)
        self.assertFalse(serialized["isUnread"])
        self.assertEqual(serialized["latestAttentionSequence"], "0")

    def test_failed_or_interrupted_terminal_without_answer_still_needs_attention(self):
        for kind in ["agent_run_failed", "agent_run_interrupted"]:
            run = self.new_run()
            append_started(run)
            payload = {"reasonType": "cancelled" if kind == "agent_run_interrupted" else "synthetic_failure",
                       "message": "Synthetic stop"}
            if kind == "agent_run_interrupted":
                payload["retryable"] = False
            append_session_records(run, [session_record(run, 3, kind, payload)])
            project_committed_agent_run(run)
            self.session.refresh_from_db()
            self.assertTrue(serialize_session(self.session)["isUnread"])
            latest = str(SessionEvent.objects.filter(session=self.session).latest("sequence").sequence)
            self.assertEqual(self.post_read(latest).status_code, 200)

    def test_automatic_ack_preserves_manual_unread_but_explicit_mark_read_clears_it(self):
        append_started(self.run)
        append_completed(self.run)
        self.assertEqual(self.mark_unread(True).status_code, 200)
        response = self.post_read("4")
        self.assertEqual(response.status_code, 200, response.content)
        self.assertTrue(response.json()["session"]["isManuallyUnread"])
        self.assertTrue(response.json()["session"]["isUnread"])
        cleared = self.mark_unread(False)
        self.assertFalse(cleared.json()["session"]["isManuallyUnread"])
        self.assertFalse(cleared.json()["session"]["isUnread"])
        self.assertEqual(cleared.json()["session"]["readThroughSequence"], "4")

    def test_ack_rejects_unknown_noncanonical_or_future_watermarks_without_mutation(self):
        append_started(self.run)
        for sequence in [3, True, "01", "-1", "1.0", "1e0", " 1", "1\n", "9223372036854775808", "3"]:
            with self.subTest(sequence=sequence):
                response = self.post_read(sequence)
                self.assertEqual(response.status_code, 400, response.content)
        self.session.refresh_from_db()
        self.assertEqual(self.session.readThroughSequence, 0)
        response = self.client.post(f"/api/sessions/{self.session.id}/read",
            data=json.dumps({"throughSequence": "1", "unknown": True}), content_type="application/json")
        self.assertEqual(response.status_code, 400)

    def test_session_listing_projects_pending_question_and_acknowledged_read_state(self):
        self.question()
        path = f"/api/workspaces/{self.workspace.id}/sessions"
        response = self.client.get(path)
        self.assertEqual(response.status_code, 200, response.content)
        session = response.json()["sessions"][0]
        self.assertEqual(session["latestAttentionSequence"], "3")
        self.assertTrue(session["isUnread"])
        self.assertEqual(self.post_read("3").status_code, 200)
        observed = self.client.get(path).json()["sessions"][0]
        self.assertFalse(observed["isUnread"])
        self.assertEqual(observed["readThroughSequence"], "3")

    def test_read_ack_requires_owner_and_current_membership(self):
        stranger = User.objects.create_user(username="attention-stranger")
        self.workspace.members.add(stranger)
        self.client.force_login(stranger)
        self.assertEqual(self.post_read("0").status_code, 404)
        self.client.force_login(self.user)
        self.workspace.members.remove(self.user)
        self.assertEqual(self.post_read("0").status_code, 404)
        self.client.logout()
        self.assertEqual(self.post_read("0").status_code, 401)

    def test_refresh_with_no_new_attention_does_not_write(self):
        from .session_attention import refresh_session_attention
        append_started(self.run)
        append_completed(self.run)
        self.session.refresh_from_db()
        with CaptureQueriesContext(connection) as captured:
            refresh_session_attention(self.session)
        self.assertFalse(any(query["sql"].lstrip().upper().startswith(("UPDATE", "INSERT", "DELETE"))
                             for query in captured.captured_queries))

    def test_attention_revision_changes_only_for_observable_notification_changes(self):
        append_started(self.run)
        append_completed(self.run)
        first = self.post_read("4").json()["session"]
        revision = int(first["attentionRevision"])
        self.assertGreater(revision, 0)
        project_committed_agent_run(self.run)
        old_ack = self.post_read("2").json()["session"]
        self.assertEqual(int(old_ack["attentionRevision"]), revision)
        marked = self.mark_unread(True).json()["session"]
        self.assertEqual(int(marked["attentionRevision"]), revision + 1)
        repeated = self.mark_unread(True).json()["session"]
        self.assertEqual(repeated["attentionRevision"], marked["attentionRevision"])
        automatic = self.post_read("4").json()["session"]
        self.assertEqual(automatic["attentionRevision"], marked["attentionRevision"])
        explicit = self.mark_unread(False).json()["session"]
        self.assertEqual(int(explicit["attentionRevision"]), revision + 2)


class SessionAttentionConcurrencyTests(TransactionTestCase):
    serialized_rollback = True
    setUp = SessionAttentionTests.setUp
    new_run = SessionAttentionTests.new_run

    def test_concurrent_http_acknowledgements_preserve_maximum_observed_cursor(self):
        append_started(self.run)
        append_completed(self.run)
        cookies = self.client.cookies.copy()
        barrier = Barrier(2)

        def acknowledge(sequence):
            close_old_connections()
            try:
                client = Client()
                client.cookies = cookies.copy()
                barrier.wait(timeout=5)
                response = client.post(f"/api/sessions/{self.session.id}/read",
                    data=json.dumps({"throughSequence": sequence}), content_type="application/json")
                return response.status_code, response.json()
            finally:
                connections.close_all()

        with ThreadPoolExecutor(max_workers=2) as executor:
            responses = list(executor.map(acknowledge, ["2", "4"]))
        self.assertEqual([status for status, _ in responses], [200, 200])
        self.session.refresh_from_db()
        final = serialize_session(self.session)
        self.assertEqual(final["readThroughSequence"], "4")
        self.assertFalse(final["isUnread"])
        self.assertFalse(final["isManuallyUnread"])
        self.assertEqual(int(final["attentionRevision"]),
                         max(int(body["session"]["attentionRevision"]) for _, body in responses))
