"""Server timeline contract; real database race tests remain a separate gate."""
import json
from django.db import connection
from django.test import TestCase
from django.test.utils import CaptureQueriesContext

from . import test_agent_inputs, test_agent_messages
from .agent_history import encode_history_cursor
from .models import AgentInput


class AgentHistoryTests(TestCase):
    setUp = test_agent_messages.AgentMessageTests.setUp
    bind = test_agent_messages.AgentMessageTests.bind
    message_run = test_agent_messages.AgentMessageTests.message_run
    validate = test_agent_messages.AgentMessageTests.validate
    accepted_pair = test_agent_messages.AgentMessageTests.accepted_pair
    commit = test_agent_messages.AgentMessageTests.commit
    submit = test_agent_inputs.AgentInputTests.submit
    input_url = test_agent_inputs.AgentInputTests.input_url

    @property
    def history_url(self):
        return f"/api/agents/{self.agent.pk}/history"

    def output(self, run, body, call_id):
        return self.accepted_pair(run, body=body, call_changes={"callId": call_id},
            result_changes={"callId": call_id})

    def test_history_orders_persistence_across_both_domains_without_timestamp_sorting(self):
        session = self.bind()
        run = self.message_run(session)
        first = self.output(run, "Before input", "output-one")
        self.submit("z-input", "First input")
        self.submit("a-input", "Second input")
        second = self.output(run, "Generated before next input", "output-two")
        self.submit("last-input", "Third input")
        response = self.client.get(self.history_url)
        self.assertEqual(response.status_code, 200, response.content)
        page = response.json()
        self.assertEqual(set(page), {"schema", "agentId", "sessionId", "items", "nextCursor", "newestCursor", "hasMore"})
        self.assertEqual([row["kind"] for row in page["items"]], ["message", "input", "input", "message", "input"])
        self.assertEqual([row["input"]["inputId"] for row in page["items"] if row["kind"] == "input"],
            ["z-input", "a-input", "last-input"])
        self.assertEqual([row["message"]["id"] for row in page["items"] if row["kind"] == "message"],
            [first.eventId, second.eventId])
        self.assertEqual(list(AgentInput.objects.order_by("sequence").values_list("accepted_source_sequence", flat=True)),
            [first.sequence, first.sequence, second.sequence])
        self.assertFalse(page["hasMore"])
        self.assertEqual(page["nextCursor"], page["items"][0]["cursor"])
        self.assertEqual(page["newestCursor"], page["items"][-1]["cursor"])

    def test_latest_page_then_older_pages_and_independent_new_messages(self):
        self.bind()
        for index in range(5):
            self.submit(f"input-{index}", f"Message {index}")
        newest = self.client.get(self.history_url, {"limit": 2}).json()
        self.assertEqual([row["input"]["inputId"] for row in newest["items"]], ["input-3", "input-4"])
        older = self.client.get(self.history_url, {"beforeCursor": newest["nextCursor"], "limit": 2}).json()
        self.assertEqual([row["input"]["inputId"] for row in older["items"]], ["input-1", "input-2"])
        self.submit("input-new", "New while reading history")
        tail = self.client.get(self.history_url, {"afterCursor": newest["newestCursor"]}).json()
        self.assertEqual([row["input"]["inputId"] for row in tail["items"]], ["input-new"])
        oldest = self.client.get(self.history_url, {"beforeCursor": older["nextCursor"], "limit": 2}).json()
        self.assertEqual([row["input"]["inputId"] for row in oldest["items"]], ["input-0"])
        self.assertFalse(oldest["hasMore"])

    def test_replay_keeps_original_anchor_and_cursor_after_new_output_commits(self):
        session = self.bind()
        first = self.submit()
        initial = self.client.get(self.history_url).json()["items"][0]
        fact = AgentInput.objects.get()
        self.assertEqual(fact.accepted_source_sequence, 0)
        self.output(self.message_run(session), "Later output", "later")
        same = self.submit()
        self.assertEqual((first.status_code, same.status_code), (201, 200))
        self.assertEqual(first.json(), same.json())
        fact.refresh_from_db()
        self.assertEqual(fact.accepted_source_sequence, 0)
        self.assertEqual(self.client.get(self.history_url).json()["items"][0], initial)

    def test_bounded_pages_advance_backwards_over_ignored_output_and_keep_poll_cursor(self):
        session = self.bind()
        self.submit()
        self.accepted_pair(self.message_run(session), call_changes={"providerId": "foreign"})
        first = self.client.get(self.history_url, {"limit": 1}).json()
        self.assertEqual(first["items"], [])
        self.assertTrue(first["hasMore"])
        self.assertIsInstance(first["newestCursor"], str)
        second = self.client.get(self.history_url, {"limit": 1, "beforeCursor": first["nextCursor"]}).json()
        self.assertEqual([row["kind"] for row in second["items"]], ["input"])
        self.assertFalse(second["hasMore"])
        empty = self.client.get(self.history_url, {"afterCursor": first["newestCursor"]}).json()
        self.assertEqual(empty["items"], [])
        self.assertEqual(empty["newestCursor"], first["newestCursor"])
        self.assertFalse(empty["hasMore"])

    def test_page_uses_one_snapshot_for_inputs_outputs_and_matching_calls(self):
        session = self.bind()
        self.submit()
        self.output(self.message_run(session), "One output", "one")
        with CaptureQueriesContext(connection) as queries:
            response = self.client.get(self.history_url)
        self.assertEqual(response.status_code, 200, response.content)
        source_queries = [q["sql"] for q in queries if "app_core_sessionevent" in q["sql"]]
        self.assertEqual(len(source_queries), 1, source_queries)
        self.assertEqual([row["kind"] for row in response.json()["items"]], ["input", "message"])

    def test_history_rejects_aliases_duplicate_fields_and_wrong_session_cursor(self):
        self.bind()
        for query in ("after_cursor=x", "afterCursor=x&afterCursor=y", "limit=0", "limit=101",
                      "afterCursor=", "beforeCursor=", "afterCursor=x&beforeCursor=y", "sequence=1"):
            self.assertEqual(self.client.get(self.history_url + "?" + query).status_code, 400)
        wrong = encode_history_cursor(self.work.pk, (1, 0, 0))
        self.assertEqual(self.client.get(self.history_url, {"afterCursor": wrong}).status_code, 400)
        bad_positions = [(1, 0, 1), (0, 1, 0), (2_147_483_648, 0, 0)]
        session_id = self.bind().pk
        for position in bad_positions:
            self.assertEqual(self.client.get(self.history_url,
                {"afterCursor": encode_history_cursor(session_id, position)}).status_code, 400)

    def test_history_requires_owner_and_current_acl(self):
        self.bind()
        self.submit()
        self.client.force_login(self.other)
        self.assertEqual(self.client.get(self.history_url).status_code, 404)
        self.client.logout()
        self.assertEqual(self.client.get(self.history_url).status_code, 401)
        self.client.force_login(self.user)
        self.membership.delete()
        self.assertEqual(self.client.get(self.history_url).status_code, 404)

    def test_non_uptake_event_and_history_refresh_do_not_move_input_or_create_read(self):
        session = self.bind()
        self.submit()
        before = self.client.get(self.history_url).json()["items"]
        self.commit(self.message_run(session), "model_request_started", {"purpose": "main", "loopIndex": 0})
        after = self.client.get(self.history_url).json()["items"]
        self.assertEqual(after, before)
        self.assertIsNone(after[0]["input"]["read"])
