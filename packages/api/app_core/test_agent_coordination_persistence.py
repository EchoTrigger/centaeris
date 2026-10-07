"""Concurrent authenticated creation retains one private coordination binding."""
from concurrent.futures import ThreadPoolExecutor
from threading import Barrier

from django.contrib.auth import get_user_model
from django.db import connections
from django.test import Client, TransactionTestCase

from .models import Agent, AgentCoordinationSession, Session, Workspace, WorkspaceMembership


class AgentCoordinationPersistenceTests(TransactionTestCase):
    serialized_rollback = True


    def test_concurrent_authenticated_creation_has_one_binding_and_fresh_session(self):
        user = get_user_model().objects.create_user(username="concurrent-message-owner")
        workspace = Workspace.objects.create(name="Concurrent messages", createdBy=user)
        WorkspaceMembership.objects.create(workspace=workspace, user=user, role="owner")
        agent = Agent.objects.create(workspace=workspace, owner=user, name="Private")
        work = Session.objects.create(workspace=workspace, owner=user, agent=agent)
        barrier = Barrier(2)

        def create():
            try:
                client = Client()
                client.force_login(user)
                barrier.wait(timeout=10)
                response = client.post(f"/api/agents/{agent.id}/coordination-session", data="{}", content_type="application/json")
                return response.status_code, response.json()
            finally:
                connections.close_all()

        with ThreadPoolExecutor(max_workers=2) as workers:
            results = list(workers.map(lambda _: create(), range(2)))
        self.assertEqual(sorted(status for status, _ in results), [200, 201])
        self.assertEqual(results[0][1], results[1][1])
        self.assertEqual(AgentCoordinationSession.objects.filter(agent=agent).count(), 1)
        self.assertEqual(Session.objects.filter(agent=agent).count(), 2)
        self.assertNotEqual(results[0][1]["sessionId"], work.id)
