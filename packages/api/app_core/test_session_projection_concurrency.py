from concurrent.futures import ThreadPoolExecutor
from threading import Event
from unittest.mock import patch

from django.contrib.auth.models import User
from django.db import connection, connections, transaction
from django.test import TransactionTestCase

from . import session_event
from .models import AgentRun, ModelConfig, SessionEvent, Workspace, WorkspaceMembership
from .testing import create_session


class SessionProjectionConcurrencyTests(TransactionTestCase):
    serialized_rollback = True

    def test_projection_allows_runtime_session_append_and_foreign_key_commit(self):
        user = User.objects.create_user(username="projection-owner")
        workspace = Workspace.objects.create(name="Projection", createdBy=user)
        WorkspaceMembership.objects.create(workspace=workspace, user=user, role="owner")
        session = create_session(workspace=workspace, owner=user)
        model = ModelConfig.objects.create(id="projection-model", displayName="Projection")
        run = AgentRun.objects.create(workspace=workspace, session=session, user=user,
                                      modelConfig=model, prompt="read")
        for index, project in enumerate((session_event.citation_snapshot,
                                        session_event.rebuild_agent_run_citation_projection,
                                        session_event.project_committed_agent_run)):
            with self.subTest(projection=project.__name__):
                locked, release = Event(), Event()

                def paused_events(_run):
                    locked.set()
                    if not release.wait(10):
                        raise TimeoutError("Projection test release timed out")
                    return []

                def reader():
                    try:
                        return project(run)
                    finally:
                        connections.close_all()

                with patch.object(session_event, "_committed_events", paused_events):
                    with ThreadPoolExecutor(max_workers=1) as workers:
                        pending = workers.submit(reader)
                        try:
                            self.assertTrue(locked.wait(5), "Projection did not reach its read boundary")
                            # Runtime owns the Session row while appending events. Its
                            # deferred AgentRun FK must also commit while a reader runs.
                            with transaction.atomic():
                                with connection.cursor() as cursor:
                                    cursor.execute("SET LOCAL lock_timeout = '500ms'")
                                    cursor.execute("SELECT id FROM app_core_session WHERE id=%s FOR UPDATE", [session.id])
                                SessionEvent.objects.create(
                                    eventId=f"projection-event-{index}", workspace=workspace,
                                    session=session, agent_run=run, sequence=index + 1,
                                    agent_run_sequence=index + 1, payload={"type": "tool_result"},
                                    createdAtMs=1, projects_to_agent_run_stream=True,
                                )
                        finally:
                            release.set()
                            pending.result(timeout=10)
                self.assertTrue(SessionEvent.objects.filter(eventId=f"projection-event-{index}").exists())
