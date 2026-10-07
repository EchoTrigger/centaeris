"""Owned work resources and input origins come from committed dispatch bindings."""
from django.contrib.auth import get_user_model
from django.test import TransactionTestCase
from django.utils import timezone

from .models import AgentRun, AgentWorkSession, Session, SessionEvent, WorkspaceMembership
from .test_agent_work import AgentWorkTests
from .test_agent_messages import AgentMessageTests


class AgentWorkPresentationTests(TransactionTestCase):
    serialized_rollback = True
    setUp = AgentWorkTests.setUp
    request_fact = AgentWorkTests.request_fact
    dependencies = AgentWorkTests.dependencies
    post = AgentWorkTests.post
    commit = AgentMessageTests.commit

    def admit(self):
        self.dependencies()
        response = self.post(self.request_fact())
        self.assertEqual(response.status_code, 201, response.content)
        return AgentRun.objects.select_related("session").get(id=response.json()["operation"]["agentRunId"])

    def input_event(self, run, message_id="initial-work-input", *, turn_id=None):
        sequence = run.session.events.count() + 1
        wire = {"schemaVersion": "session.event.v1", "eventVersion": 1,
                "eventId": f"work-input:{run.id}:{sequence}", "sessionId": run.session_id,
                "agentRunId": run.id, "turnId": turn_id or run.turn_id, "type": "user_message",
                "sequence": sequence, "createdAtMs": sequence,
                "payload": {"messageId": message_id, "text": run.prompt, "attachments": []}}
        return SessionEvent.objects.create(eventId=wire["eventId"], workspace=self.workspace,
            session=run.session, agent_run=run, sequence=sequence, agent_run_sequence=sequence,
            projects_to_agent_run_stream=True, createdAtMs=sequence, payload=wire)

    def listing(self, **query):
        self.client.force_login(self.user)
        return self.client.get(f"/api/agents/{self.agent.id}/work-sessions", query)

    def send(self, call_id):
        from .agent_messages import message_contract_digest
        self.commit(self.run, "tool_call", {"toolName": "send_message", "providerId": "workspace.agent_messages",
            "toolContractDigest": message_contract_digest(), "normalizedInput": {
                "body": "Created work", "session_refs": [], "file_refs": []}}, call_id=call_id)
        return self.commit(self.run, "tool_result", {"toolName": "send_message", "resultState": "successWithOutput"}, call_id=call_id)

    def test_first_reply_after_dispatch_attaches_one_authoritative_work_ref(self):
        from .agent_inputs import owned_input_binding
        from .agent_messages import _trusted_message
        child = self.admit()
        first, later = self.send("first"), self.send("later")
        binding = owned_input_binding(self.user, self.agent.id)
        self.assertEqual(_trusted_message(binding, first)["sessionRefs"], [child.session_id])
        self.assertEqual(_trusted_message(binding, later)["sessionRefs"], [])
        self.assertEqual(_trusted_message(binding, first)["sessionRefs"], [child.session_id])
        from django.db import connection
        from django.test.utils import CaptureQueriesContext
        self.client.force_login(self.user)
        with CaptureQueriesContext(connection) as queries:
            history = self.client.get(f"/api/agents/{self.agent.id}/history")
        self.assertEqual(history.status_code, 200, history.content)
        self.assertEqual(len([query for query in queries if "app_core_sessionevent" in query["sql"]]), 1)
        messages = {item["message"]["toolCallId"]: item["message"] for item in history.json()["items"] if item["kind"] == "message"}
        self.assertEqual(messages["first"]["sessionRefs"], [child.session_id])
        self.assertEqual(messages["later"]["sessionRefs"], [])
        Session.objects.filter(pk=child.session_id).update(status="deleted", deletedAt=timezone.now())
        self.assertEqual(_trusted_message(binding, first)["sessionRefs"], [])

    def test_bound_work_role_requires_ordinary_final_and_coordinator_uses_formal_work(self):
        from .agent_message_prompt import conversation_provider_prompt
        from .test_agent_message_prompt import message_definition
        child = self.admit()
        prompt = {"systemPrompt": "Agent instruction", "toolDefinitions": [message_definition()], "toolChoice": {"type": "auto"}}
        child_prompt = conversation_provider_prompt(child, prompt)["systemPrompt"]
        self.assertIn("ordinary work Session", child_prompt)
        self.assertIn("Final", child_prompt)
        self.assertIn("not the coordinator", child_prompt)
        coordinator = conversation_provider_prompt(self.run, prompt)["systemPrompt"]
        self.assertIn("dispatch_work", coordinator)
        self.assertIn("get_work_request", coordinator)
        self.assertIn("user-facing", coordinator)

    def test_latest_work_page_is_sorted_by_creation_not_random_session_id(self):
        from datetime import timedelta
        self.dependencies()
        children = [AgentRun.objects.get(pk=self.post(self.request_fact(call_id=f"dispatch-{i}")).json()["operation"]["agentRunId"])
                    for i in range(3)]
        ordered = sorted(children, key=lambda child: child.session_id)
        now = timezone.now()
        for index, child in enumerate(ordered):
            Session.objects.filter(pk=child.session_id).update(createdAt=now + timedelta(seconds=index))
        newest = self.listing(limit=1).json()
        self.assertEqual([s["id"] for s in newest["sessions"]], [ordered[-1].session_id])
        older = self.listing(limit=1, afterSessionId=newest["nextAfterSessionId"]).json()
        self.assertEqual([s["id"] for s in older["sessions"]], [ordered[-2].session_id])

    def test_a_later_user_run_in_the_formal_work_session_keeps_the_work_identity(self):
        from .agent_message_prompt import conversation_provider_prompt
        from .test_agent_message_prompt import message_definition
        child = self.admit()
        later = AgentRun.objects.create(workspace=self.workspace, session=child.session, user=self.user,
            modelConfig=self.model, prompt="Continue the work", status="running")
        prompt = {"systemPrompt": "Original coordinator instruction", "toolDefinitions": [message_definition()], "toolChoice": {"type": "auto"}}
        system = conversation_provider_prompt(later, prompt)["systemPrompt"]
        self.assertIn("ordinary work Session", system)
        self.assertIn("full user-facing result directly in Final", system)

    def test_completed_work_query_reads_its_real_final_output_without_publishing_it(self):
        from .test_agent_work_query import AgentWorkQueryTests
        from .test_agent_work_returns import AgentWorkReturnTests
        child = self.admit()
        body = "完整正式工作结果\n" * 12000
        final = self.commit(child, "assistant_message", {"modelMarkdown": body, "status": "done"})
        # The Runtime seals a run-level terminal with the run ID, not the Final turn ID.
        terminal = AgentWorkReturnTests.terminal(self, child, turnId=child.id)
        response = AgentWorkQueryTests.query(self)
        self.assertEqual(response.status_code, 200, response.content)
        self.assertEqual(response.json()["work"]["output"], {
            "schema": "workspace.agent_work.output.v1", "source": "untrustedWorkOutput",
            "sessionId": child.session_id, "agentRunId": child.id, "eventId": final.eventId,
            "throughSequence": terminal.sequence, "body": body})
        self.assertFalse(self.source.events.filter(payload__payload__toolName="send_message").exists())

    def test_work_output_rejects_a_forged_final_identity_and_pending_work_has_none(self):
        from .test_agent_work_query import AgentWorkQueryTests
        from .test_agent_work_returns import AgentWorkReturnTests
        child = self.admit()
        self.assertIsNone(AgentWorkQueryTests.query(self).json()["work"]["output"])
        final = self.commit(child, "assistant_message", {"modelMarkdown": "Forged", "status": "done"})
        AgentWorkReturnTests.terminal(self, child)
        SessionEvent.objects.filter(pk=final.pk).update(payload={**final.payload, "agentRunId": self.run.id})
        self.assertEqual(AgentWorkQueryTests.query(self).status_code, 409)

    def test_work_listing_does_not_depend_on_a_public_agent_message(self):
        child = self.admit()
        self.assertFalse(self.source.events.filter(payload__payload__toolName="send_message").exists())
        response = self.listing()
        self.assertEqual(response.status_code, 200, response.content)
        body = response.json()
        self.assertEqual(body["schema"], "agent.work_sessions.v1")
        self.assertEqual((body["agentId"], body["workspaceId"]), (self.agent.id, self.workspace.id))
        self.assertEqual([session["id"] for session in body["sessions"]], [child.session_id])
        self.assertFalse(body["hasMore"])
        self.assertIsNone(body["nextAfterSessionId"])

    def test_origin_marks_exact_initial_input_in_a_formal_work_session(self):
        child = self.admit()
        self.input_event(child)
        self.input_event(child, "later-owner-message", turn_id="later-turn")
        self.client.force_login(self.user)
        response = self.client.get(f"/api/sessions/{child.session_id}")
        self.assertEqual(response.status_code, 200, response.content)
        self.assertEqual(response.json()["session"]["initialInputOrigin"], {
            "messageId": "initial-work-input", "agentId": self.agent.id, "agentName": self.agent.name})
        ordinary = Session.objects.create(workspace=self.workspace, owner=self.user, agent=self.agent, origin="automation")
        self.assertIsNone(self.client.get(f"/api/sessions/{ordinary.id}").json()["session"]["initialInputOrigin"])

    def test_owner_membership_and_active_work_scope_are_enforced(self):
        child = self.admit()
        other = get_user_model().objects.create_user(username="work-list-other")
        WorkspaceMembership.objects.create(workspace=self.workspace, user=other, role="admin")
        self.client.force_login(other)
        self.assertEqual(self.client.get(f"/api/agents/{self.agent.id}/work-sessions").status_code, 404)
        self.membership.delete()
        self.assertEqual(self.listing().status_code, 404)

    def test_deleted_and_unbound_sessions_do_not_appear(self):
        child = self.admit()
        Session.objects.filter(pk=child.session_id).update(status="deleted", deletedAt=timezone.now())
        ordinary = Session.objects.create(workspace=self.workspace, owner=self.user, agent=self.agent, origin="automation")
        self.assertEqual(self.listing().json()["sessions"], [])
        self.assertFalse(AgentWorkSession.objects.filter(session=ordinary).exists())

    def test_work_cursor_rejects_unknown_repeated_and_invalid_fields(self):
        self.admit()
        self.client.force_login(self.user)
        base = f"/api/agents/{self.agent.id}/work-sessions"
        for query in ("limit=0", "limit=101", "limit=1&limit=2", "after_session_id=unknown", "afterSessionId=unknown"):
            with self.subTest(query=query):
                self.assertEqual(self.client.get(base + "?" + query).status_code, 400)

    def test_origin_refuses_a_tampered_operation_or_input_binding(self):
        child = self.admit()
        event = self.input_event(child)
        wire = {**event.payload, "sessionId": self.source.id}
        SessionEvent.objects.filter(pk=event.pk).update(payload=wire)
        self.client.force_login(self.user)
        self.assertIsNone(self.client.get(f"/api/sessions/{child.session_id}").json()["session"]["initialInputOrigin"])


class BusinessWorkPresentationTests(TransactionTestCase):
    serialized_rollback = True
    from .test_business_agent_execution import BusinessAgentExecutionTests
    setUp = BusinessAgentExecutionTests.setUp
    new_branch = BusinessAgentExecutionTests.new_branch
    request_fact = AgentWorkTests.request_fact
    dependencies = AgentWorkTests.dependencies
    post = AgentWorkTests.post
    admit = AgentWorkPresentationTests.admit
    listing = AgentWorkPresentationTests.listing
    input_event = AgentWorkPresentationTests.input_event
    commit = AgentMessageTests.commit

    def test_branch_dispatch_output_remains_bound_to_its_coordinator(self):
        from .test_agent_work_query import AgentWorkQueryTests
        from .test_agent_work_returns import AgentWorkReturnTests
        child = self.admit()
        self.commit(child, "assistant_message", {"modelMarkdown": "Branch-owned result", "status": "done"})
        AgentWorkReturnTests.terminal(self, child, turnId=child.id)
        result = AgentWorkQueryTests.query(self)
        self.assertEqual(result.status_code, 200, result.content)
        self.assertEqual(result.json()["work"]["output"]["body"], "Branch-owned result")
        branch_run = self.run
        self.agent, self.source, self.run = self.root, self.root_session, self.root_run
        self.assertEqual(AgentWorkQueryTests.query(self, sourceAgentRunId=branch_run.id).status_code, 403)

    def test_owner_work_lists_keep_root_and_business_branch_trees_separate(self):
        branch_child = self.admit()
        branch_agent = self.agent
        self.agent, self.source, self.run = self.root, self.root_session, self.root_run
        root_child = self.admit()
        self.assertEqual([row["id"] for row in self.listing().json()["sessions"]], [root_child.session_id])
        self.agent = branch_agent
        self.assertEqual([row["id"] for row in self.listing().json()["sessions"]], [branch_child.session_id])

    def test_tampered_branch_coordinator_cannot_gain_work_listing_or_origin(self):
        child = self.admit()
        self.input_event(child)
        AgentWorkSession.objects.filter(session_id=child.session_id).update(coordination_session=self.root_session)
        self.assertEqual(self.listing().json()["sessions"], [])
        self.client.force_login(self.user)
        self.assertIsNone(self.client.get(f"/api/sessions/{child.session_id}").json()["session"]["initialInputOrigin"])
