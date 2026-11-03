"""Workspace Session list query growth on real PostgreSQL."""
from django.db import connection
from django.test import TransactionTestCase
from django.test.utils import CaptureQueriesContext
from .models import Session
from .session_record_test_fixtures import SessionRecordFixture

class WorkspaceSessionListQueryTests(SessionRecordFixture, TransactionTestCase):
    serialized_rollback = True
    def listing(self):
        return self.client.get(f"/api/workspaces/{self.workspace.id}/sessions")

    def test_ordinary_session_list_queries_do_not_grow_with_each_session(self):
        self.client.force_login(self.user)
        Session.objects.create(workspace=self.workspace, owner=self.user)
        with CaptureQueriesContext(connection) as first_queries:
            first = self.listing()
        self.assertEqual(first.status_code, 200, first.content)
        self.assertEqual(len(first.json()["sessions"]), 2)
        for _ in range(12):
            Session.objects.create(workspace=self.workspace, owner=self.user)
        with CaptureQueriesContext(connection) as many_queries:
            many = self.listing()
        self.assertEqual(many.status_code, 200, many.content)
        self.assertEqual(len(many.json()["sessions"]), 14)
        self.assertTrue(all("instructions" not in row for row in many.json()["sessions"]))
        self.assertEqual(len(many_queries), len(first_queries),
            f"queries grew from {len(first_queries)} to {len(many_queries)} for ordinary sessions")
