"""Hosted input facts survive replay and restart without claiming model uptake."""
from concurrent.futures import ThreadPoolExecutor
from threading import Barrier
import json

from django.contrib.auth import get_user_model
from django.db import connection, connections
from django.test import Client, TestCase, TransactionTestCase
from django.test.utils import CaptureQueriesContext

from .models import Agent, AgentInput, AgentRun, SessionEvent, Workspace, WorkspaceMembership
from . import test_agent_messages


class AgentInputTests(TestCase):
    setUp = test_agent_messages.AgentMessageTests.setUp
    bind = test_agent_messages.AgentMessageTests.bind
    message_run = test_agent_messages.AgentMessageTests.message_run
    commit = test_agent_messages.AgentMessageTests.commit

    @property
    def input_url(self):
        return f"/api/agents/{self.agent.pk}/inputs"

    def submit(self, fact_id="input-one", body="  Exact input\nUnicode: 中文  ", **changes):
        return self.client.post(self.input_url, data=json.dumps({"schema": "agent.input.submit.v1", "attachmentRefs": [],
            "inputId": fact_id, "body": body, **changes}), content_type="application/json")

    def test_input_acceptance_preserves_exact_text_id_and_server_metadata_without_read_or_run(self):
        session = self.bind()
        before = SessionEvent.objects.count()
        response = self.submit()
        self.assertEqual(response.status_code, 201, response.content)
        fact = response.json()["input"]
        self.assertEqual(set(response.json()), {"schema", "agentId", "sessionId", "input"})
        self.assertEqual(set(fact), {"inputId", "sequence", "createdAtMs", "body", "read", "attachments"})
        self.assertEqual(response.json()["schema"], "agent.input.accepted.v1")
        self.assertEqual(response.json()["sessionId"], session.pk)
        self.assertEqual((fact["inputId"], fact["sequence"], fact["body"]),
            ("input-one", 1, "  Exact input\nUnicode: 中文  "))
        self.assertIsInstance(fact["createdAtMs"], int)
        self.assertIsNone(fact["read"])
        self.assertEqual(AgentInput.objects.get().membership_ref, self.membership.pk)
        self.assertFalse(AgentRun.objects.exists(), "This preparation only certifies the input fact")
        self.assertEqual(SessionEvent.objects.count(), before)

    def test_same_id_replays_same_fact_and_changed_content_conflicts_without_another_row(self):
        self.bind()
        first = self.submit()
        same = self.submit()
        changed = self.submit(body="Different content")
        self.assertEqual((first.status_code, same.status_code, changed.status_code), (201, 200, 409))
        self.assertEqual(first.json(), same.json())
        self.assertEqual(changed.json(), {"error": "agent_input_conflict"})
        self.assertEqual(AgentInput.objects.count(), 1)
        self.assertEqual(AgentInput.objects.get().body, first.json()["input"]["body"])

    def test_history_reopens_original_body_in_server_order_without_duplicate_cursor_rows(self):
        self.bind()
        first = self.submit("z-input", "First accepted text").json()["input"]
        second = self.submit("a-input", "Second accepted text").json()["input"]
        fresh = Client()
        fresh.force_login(self.user)
        with CaptureQueriesContext(connection) as queries:
            page = fresh.get(self.input_url, {"limit": 1})
        self.assertFalse(any(q["sql"].lstrip().upper().startswith(("INSERT", "UPDATE", "DELETE")) for q in queries))
        self.assertEqual(page.status_code, 200, page.content)
        self.assertEqual(page.json()["inputs"], [first])
        self.assertEqual(page.json()["nextAfterSequence"], first["sequence"])
        next_page = fresh.get(self.input_url, {"afterSequence": first["sequence"], "limit": 1}).json()
        self.assertEqual(next_page["inputs"], [second])
        self.assertIsNone(next_page["nextAfterSequence"])
        self.assertEqual([item["sequence"] for item in (first, second)], [1, 2])

    def test_input_and_history_require_current_owner_and_acl(self):
        self.bind()
        self.submit()
        self.client.force_login(self.other)
        self.assertEqual(self.submit().status_code, 404)
        self.assertEqual(self.client.get(self.input_url).status_code, 404)
        self.client.logout()
        self.assertEqual(self.submit().status_code, 401)
        self.assertEqual(self.client.get(self.input_url).status_code, 401)
        self.client.force_login(self.user)
        self.membership.delete()
        self.assertEqual(self.submit().status_code, 404)
        self.assertEqual(self.client.get(self.input_url).status_code, 404)
        WorkspaceMembership.objects.create(workspace=self.workspace, user=self.user, role="owner")
        self.assertEqual(self.submit().status_code, 403, "An old input identity cannot regain execution authority")
        self.assertEqual(AgentInput.objects.count(), 1)

    def test_transport_aliases_run_selection_and_invalid_text_are_rejected(self):
        self.bind()
        for changes in ({"input_id": "alias"}, {"agentRunId": "chosen-run"}, {"sessionId": self.work.pk},
                        {"read": True}, {"createdAtMs": 1}, {"sequence": 1}, {"body": "  "},
                        {"body": "nul\0text"}, {"body": "😀" * 16_385}, {"inputId": "a" * 65}):
            with self.subTest(changes=changes):
                self.assertEqual(self.submit(**changes).status_code, 400)
        self.assertFalse(AgentInput.objects.exists())
        for query in ("after_sequence=1", "afterSequence=0&afterSequence=1", "limit=0", "limit=101", "agentRunId=chosen"):
            self.assertEqual(self.client.get(self.input_url + "?" + query).status_code, 400)

    def test_input_fact_is_insert_only_and_original_bytes_survive_a_new_instance(self):
        self.bind()
        self.submit()
        fact = AgentInput.objects.get()
        fact.body = "rewrite"
        with self.assertRaisesMessage(ValueError, "agent_input_is_immutable"):
            fact.save()
        self.assertEqual(AgentInput.objects.get().body, "  Exact input\nUnicode: 中文  ")

    def test_acceptance_and_non_uptake_session_events_do_not_establish_read(self):
        session = self.bind()
        self.submit()
        run = self.message_run(session)
        self.commit(run, "model_request_started", {"purpose": "main", "loopIndex": 0})
        self.commit(run, "assistant_message", {"text": "ACK"})
        fact = self.client.get(self.input_url).json()["inputs"][0]
        self.assertIsNone(fact["read"])
        self.assertEqual(AgentInput.objects.count(), 1)


class AgentInputConcurrencyTests(TransactionTestCase):
    serialized_rollback = True

    def setUp(self):
        self.user = get_user_model().objects.create_user(username="input-race-owner")
        self.workspace = Workspace.objects.create(name="Input race", createdBy=self.user)
        WorkspaceMembership.objects.create(workspace=self.workspace, user=self.user, role="owner")
        self.agent = Agent.objects.create(workspace=self.workspace, owner=self.user, name="Input race")
        self.client.force_login(self.user)
        self.client.post(f"/api/agents/{self.agent.pk}/coordination-session", data="{}", content_type="application/json")

    def race(self, ids):
        barrier = Barrier(2)
        def submit(input_id):
            try:
                client = Client()
                client.force_login(self.user)
                barrier.wait(timeout=10)
                response = client.post(f"/api/agents/{self.agent.pk}/inputs", content_type="application/json",
                    data=json.dumps({"schema": "agent.input.submit.v1", "attachmentRefs": [], "inputId": input_id, "body": "same text"}))
                return response.status_code, response.json()
            finally:
                connections.close_all()
        with ThreadPoolExecutor(max_workers=2) as pool:
            return list(pool.map(submit, ids))

    def test_same_id_race_has_one_fact_and_one_server_sequence(self):
        results = self.race(["same-input", "same-input"])
        self.assertEqual(sorted(status for status, _ in results), [200, 201], results)
        self.assertEqual(results[0][1], results[1][1])
        self.assertEqual(AgentInput.objects.count(), 1)

    def test_distinct_input_race_commits_both_once_in_one_server_order(self):
        results = self.race(["first-input", "second-input"])
        self.assertEqual([status for status, _ in results], [201, 201], results)
        self.assertEqual(list(AgentInput.objects.order_by("sequence").values_list("sequence", flat=True)), [1, 2])
        self.assertFalse(AgentRun.objects.exists())
