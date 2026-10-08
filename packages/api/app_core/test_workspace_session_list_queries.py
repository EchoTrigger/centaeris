"""Workspace list query growth and verified origin behavior on real PostgreSQL."""
from django.db import connection
from django.test import TransactionTestCase
from django.test.utils import CaptureQueriesContext

from .models import Session, SessionEvent, AgentCoordinationSession
from . import test_agent_work_presentation as presentation


class WorkspaceSessionListQueryTests(TransactionTestCase):
    serialized_rollback = True
    setUp = presentation.AgentWorkPresentationTests.setUp
    request_fact = presentation.AgentWorkPresentationTests.request_fact
    dependencies = presentation.AgentWorkPresentationTests.dependencies
    post = presentation.AgentWorkPresentationTests.post
    admit = presentation.AgentWorkPresentationTests.admit
    input_event = presentation.AgentWorkPresentationTests.input_event

    def listing(self):
        return self.client.get(f"/api/workspaces/{self.workspace.id}/sessions")

    def test_ordinary_session_list_queries_do_not_grow_with_each_session(self):
        self.client.force_login(self.user)
        Session.objects.create(workspace=self.workspace, owner=self.user, agent=self.agent)
        with CaptureQueriesContext(connection) as first_queries:
            first = self.listing()
        self.assertEqual(first.status_code, 200, first.content)
        self.assertEqual(len(first.json()["sessions"]), 1)
        for _ in range(12):
            Session.objects.create(workspace=self.workspace, owner=self.user, agent=self.agent)
        with CaptureQueriesContext(connection) as many_queries:
            many = self.listing()
        self.assertEqual(many.status_code, 200, many.content)
        self.assertEqual(len(many.json()["sessions"]), 13)
        self.assertTrue(all(row["initialInputOrigin"] is None for row in many.json()["sessions"]))
        self.assertEqual(len(many_queries), len(first_queries),
            f"queries grew from {len(first_queries)} to {len(many_queries)} for ordinary sessions")

    def test_list_preserves_verified_work_origin_and_rejects_tampering_and_revocation(self):
        child = self.admit()
        event = self.input_event(child)
        self.client.force_login(self.user)
        def origin():
            response = self.listing()
            self.assertEqual(response.status_code, 200, response.content)
            return next(row for row in response.json()["sessions"] if row["id"] == child.session_id)["initialInputOrigin"]
        expected = {"messageId": "initial-work-input", "agentId": self.agent.id, "agentName": self.agent.name}
        self.assertEqual(origin(), expected)
        SessionEvent.objects.filter(pk=event.pk).update(payload={**event.payload, "sessionId": self.source.id})
        self.assertIsNone(origin())
        SessionEvent.objects.filter(pk=event.pk).update(payload=event.payload)
        self.assertEqual(origin(), expected)
        AgentCoordinationSession.objects.filter(agent=self.agent).delete()
        self.assertIsNone(origin())
