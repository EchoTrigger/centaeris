"""Committed source changes invalidate only the readers whose output can change."""
import json
from concurrent.futures import ThreadPoolExecutor
from threading import Barrier
from unittest import skipUnless
from unittest.mock import patch
from uuid import uuid4

import psycopg
from django.contrib.auth import get_user_model
from django.db import close_old_connections, connection, connections, transaction
from django.core.management import call_command
from django.test import SimpleTestCase, TransactionTestCase
from django.utils import timezone

from .models import AgentRun, ModelConfig, PresentationChange, PresentationChangeOutbox, Session, Workspace
from .presentation_changes import CHANNEL, PROJECTED_CHANNEL, drain_pending_changes, read_presentation_versions, presentation_change_keys, session_change_key
from .testing import create_session


@skipUnless(connection.vendor == "postgresql", "presentation triggers require PostgreSQL")
class PresentationChangeTests(TransactionTestCase):
    serialized_rollback = True

    def setUp(self):
        self.user = get_user_model().objects.create_user(username="presentation-change-owner")
        self.workspace = Workspace.objects.create(name="Presentation changes", createdBy=self.user)
        self.workspace.members.add(self.user)
        self.session = create_session(workspace=self.workspace, owner=self.user)
        self.model = ModelConfig.objects.create(displayName="Synthetic presentation")
        self.run = AgentRun.objects.create(workspace=self.workspace, session=self.session,
            user=self.user, modelConfig=self.model, prompt="Synthetic presentation")

    def versions(self, *, session=None):
        drain_pending_changes()
        return read_presentation_versions(self.workspace.pk, (session or self.session).pk)

    def event(self, kind, payload=None):
        # Runtime writes SQL directly. This intentionally bypasses model.save and
        # Django signals, including records excluded from the old per-Run stream.
        with connection.cursor() as cursor:
            cursor.execute("SELECT COALESCE(MAX(sequence),0)+1 FROM app_core_sessionevent WHERE session_id=%s", [self.session.pk])
            sequence = cursor.fetchone()[0]
            event_id = "presentation:" + uuid4().hex
            wire = {"type": kind, "payload": payload or {}}
            cursor.execute('INSERT INTO app_core_sessionevent ("eventId",workspace_id,session_id,agent_run_id,'
                'sequence,agent_run_sequence,session_level,projects_to_agent_run_stream,payload,"createdAtMs","insertedAt") '
                'VALUES(%s,%s,%s,%s,%s,%s,FALSE,%s,%s::jsonb,%s,clock_timestamp())',
                [event_id, self.workspace.pk, self.session.pk, self.run.pk, sequence, sequence,
                 kind not in {"checkpoint_ref", "model_request_started"}, json.dumps(wire), sequence])
        return event_id

    def assertChangedOnly(self, before, *kinds):
        after = self.versions()
        self.assertEqual({kind for kind in before if after[kind] != before[kind]}, set(kinds))
        for kind in kinds:
            self.assertGreater(after[kind], before[kind])

    def test_plain_reply_invalidates_state_without_catalog_or_context_reads(self):
        before = self.versions()
        self.event("assistant_message", {"modelMarkdown": "reply"})
        self.assertChangedOnly(before, "state")

    def test_question_and_terminal_events_invalidate_attention_navigation(self):
        for kind, payload in [("checkpoint_ref", {"status": "paused_question"}),
                              ("agent_run_completed", {"doneReason": "finalized"})]:
            with self.subTest(kind=kind):
                before = self.versions()
                self.event(kind, payload)
                self.assertChangedOnly(before, "state", "navigation", "work")

    def test_model_boundary_invalidates_context_and_main_uptake_history(self):
        for purpose, changed in [("compaction", ("state", "context")),
                                 ("main", ("state", "context", "history"))]:
            before = self.versions()
            self.event("model_request_started", {"purpose": purpose, "observations": []})
            self.assertChangedOnly(before, *changed)


    def test_session_metadata_and_read_fields_change_navigation_without_context(self):
        self.event("assistant_message")
        self.event("checkpoint_ref", {"status": "paused_question"})
        for fields in ({"title": "renamed"}, {"isPinned": True}, {"readThroughSequence": 1},
                       {"isManuallyUnread": True}, {"latestAttentionSequence": 2, "attentionRevision": 1}):
            before = self.versions()
            Session.objects.filter(pk=self.session.pk).update(**fields)
            self.assertChangedOnly(before, "navigation")

    def test_status_change_invalidates_exact_session_and_work_observers(self):
        before = self.versions()
        AgentRun.objects.filter(pk=self.run.pk).update(status="completed")
        self.assertChangedOnly(before, "state", "context", "navigation", "work")

    def test_new_run_invalidates_navigation_and_state_even_before_events(self):
        before = self.versions()
        AgentRun.objects.create(workspace=self.workspace, session=self.session, user=self.user,
            modelConfig=self.model, prompt="Second synthetic run")
        self.assertChangedOnly(before, "state", "context", "navigation", "work")


    def test_same_transaction_coalesces_and_rollback_never_advances_revision(self):
        before = self.versions()
        with transaction.atomic():
            for index in range(4):
                Session.objects.filter(pk=self.session.pk).update(title=f"title {index}")
            self.assertEqual(self.versions()["navigation"], before["navigation"] + 1)
        committed = self.versions()
        with self.assertRaisesRegex(RuntimeError, "rollback"):
            with transaction.atomic():
                Session.objects.filter(pk=self.session.pk).update(title="rolled back")
                self.event("assistant_message")
                raise RuntimeError("rollback")
        self.assertEqual(self.versions(), committed)

    def test_session_creation_wakes_workspace_navigation_only(self):
        before = self.versions()
        key = presentation_change_keys(self.workspace.pk)["navigation"]
        cfg = connection.settings_dict
        with psycopg.connect(dbname=cfg["NAME"], user=cfg["USER"], password=cfg["PASSWORD"],
                host=cfg["HOST"], port=cfg["PORT"], autocommit=True) as listener:
            listener.execute("LISTEN " + CHANNEL)
            with transaction.atomic():
                unrelated = create_session(workspace=self.workspace, owner=self.user)
                self.assertEqual(list(listener.notifies(timeout=.03, stop_after=1)), [])
            notices = list(listener.notifies(timeout=.15))
            self.assertIn(key, [notice.payload for notice in notices])
        self.assertChangedOnly(before, "navigation")
        self.assertEqual(self.versions(session=unrelated)["navigation"],
                         self.versions()["navigation"])

    def test_noop_writes_and_runtime_lease_churn_do_not_dirty_presentation(self):
        before = self.versions()
        Session.objects.filter(pk=self.session.pk).update(title=self.session.title)
        AgentRun.objects.filter(pk=self.run.pk).update(transitionReason="lease bookkeeping")
        self.assertEqual(self.versions(), before)

    def test_permission_changes_leave_display_versions_and_outbox_unchanged(self):
        before = self.versions()
        get_user_model().objects.filter(pk=self.user.pk).update(is_active=False)
        self.assertEqual(PresentationChangeOutbox.objects.count(), 0)
        self.assertEqual(self.versions(), before)

    def test_committed_notification_is_only_a_key_and_rollback_emits_nothing(self):
        cfg = connection.settings_dict
        with psycopg.connect(dbname=cfg["NAME"], user=cfg["USER"], password=cfg["PASSWORD"],
                host=cfg["HOST"], port=cfg["PORT"], autocommit=True) as listener:
            listener.execute("LISTEN " + CHANNEL)
            with transaction.atomic():
                Session.objects.filter(pk=self.session.pk).update(title="Private content must not be sent")
                self.assertEqual(list(listener.notifies(timeout=.03, stop_after=1)), [])
            notices = list(listener.notifies(timeout=.15, stop_after=1))
            self.assertEqual([notice.payload for notice in notices], [presentation_change_keys(self.workspace.pk)["navigation"]])
            with self.assertRaises(RuntimeError):
                with transaction.atomic():
                    self.event("assistant_message")
                    raise RuntimeError()
            self.assertEqual(list(listener.notifies(timeout=.03, stop_after=1)), [])
        self.assertGreater(self.versions()["navigation"], 0)

    def test_navigation_only_versions_do_not_bind_an_arbitrary_session(self):
        self.versions()
        before = read_presentation_versions(self.workspace.pk)
        self.event("assistant_message")
        drain_pending_changes()
        self.assertEqual(read_presentation_versions(self.workspace.pk), before)
        self.assertGreater(self.versions()["state"], 0)
        self.assertEqual(session_change_key(self.session.pk, "state"), f"session:{self.session.pk}:state")

    def test_relay_is_bounded_and_durable_when_wake_notifications_are_lost(self):
        self.versions()
        for index in range(4):
            self.event("assistant_message", {"modelMarkdown": str(index)})
        pending = PresentationChangeOutbox.objects.count()
        self.assertEqual(pending, 4)
        before = read_presentation_versions(self.workspace.pk, self.session.pk)
        self.assertEqual(drain_pending_changes(limit=2), 2)
        self.assertEqual(PresentationChangeOutbox.objects.count(), 2)
        after = read_presentation_versions(self.workspace.pk, self.session.pk)
        self.assertGreater(after["state"], before["state"])
        self.assertEqual(drain_pending_changes(limit=2), 2)
        self.assertEqual(drain_pending_changes(limit=2), 0)

    def test_failed_relay_rolls_back_both_versions_and_consumption(self):
        self.versions()
        self.event("assistant_message")
        before = read_presentation_versions(self.workspace.pk, self.session.pk)
        with self.assertRaises(RuntimeError):
            with transaction.atomic():
                self.assertEqual(drain_pending_changes(), 1)
                raise RuntimeError("relay crashed")
        self.assertEqual(read_presentation_versions(self.workspace.pk, self.session.pk), before)
        self.assertEqual(PresentationChangeOutbox.objects.count(), 1)
        self.assertEqual(drain_pending_changes(), 1)
        self.assertGreater(self.versions()["state"], before["state"])

    def test_source_transactions_do_not_deadlock_on_reversed_scope_order(self):
        self.versions()
        barrier = Barrier(2)
        def writer(first):
            close_old_connections()
            try:
                with transaction.atomic():
                    with connection.cursor() as cursor:
                        cursor.execute("SET LOCAL statement_timeout='5s'")
                    if first:
                        self.event("assistant_message")
                    else:
                        Session.objects.filter(pk=self.session.pk).update(title="other transaction")
                    barrier.wait(timeout=5)
                    if first:
                        Session.objects.filter(pk=self.session.pk).update(title="first transaction")
                    else:
                        AgentRun.objects.filter(pk=self.run.pk).update(status="completed")
                return True
            finally:
                connection.close()
        with ThreadPoolExecutor(max_workers=2) as pool:
            futures = [pool.submit(writer, first) for first in (True, False)]
            self.assertTrue(all(future.result(timeout=12) for future in futures))
        self.assertGreater(PresentationChangeOutbox.objects.count(), 0)
        self.assertGreater(drain_pending_changes(), 0)

    def test_concurrent_relays_consume_each_pending_row_once(self):
        self.versions()
        for index in range(6):
            Session.objects.filter(pk=self.session.pk).update(title=f"relay {index}")
            self.event("assistant_message")
        before = PresentationChangeOutbox.objects.count()
        barrier = Barrier(2)
        def relay():
            close_old_connections()
            try:
                barrier.wait(timeout=5)
                return drain_pending_changes(limit=8)
            finally:
                connection.close()
        with ThreadPoolExecutor(max_workers=2) as pool:
            results = [pool.submit(relay) for _ in range(2)]
            consumed = sum(future.result(timeout=12) for future in results)
        self.assertEqual(consumed, before)
        self.assertEqual(PresentationChangeOutbox.objects.count(), 0)
        self.assertGreater(PresentationChange.objects.count(), 0)

    def test_explicit_maintenance_is_bounded_and_rejects_invalid_limits(self):
        from io import StringIO
        from django.core.management.base import CommandError
        self.versions()
        self.event("assistant_message")
        self.event("assistant_message")
        output = StringIO()
        call_command("drain_presentation_changes", limit=1, stdout=output)
        self.assertEqual(PresentationChangeOutbox.objects.count(), 1)
        self.assertIn("Projected 1", output.getvalue())
        with self.assertRaises(CommandError):
            call_command("drain_presentation_changes", limit=4097, stdout=output)

    def test_model_capacity_changes_invalidate_only_relevant_context(self):
        before = self.versions()
        ModelConfig.objects.filter(pk=self.model.pk).update(contextTokens=self.model.contextTokens + 1)
        self.assertChangedOnly(before, "context")

    def test_source_coalesces_repeated_pending_changes_per_transaction(self):
        self.versions()
        with transaction.atomic():
            for index in range(4):
                self.event("assistant_message", {"modelMarkdown": str(index)})
            self.assertEqual(PresentationChangeOutbox.objects.count(), 1)
        self.assertEqual(drain_pending_changes(), 1)

    def test_membership_changes_keep_display_caches_and_session_removal_invalidates_views(self):
        before = self.versions()
        self.workspace.memberships.filter(user=self.user).update(role="admin")
        self.assertEqual(PresentationChangeOutbox.objects.count(), 0)
        self.assertEqual(self.versions(), before)
        Session.objects.filter(pk=self.session.pk).update(status="deleted", deletedAt=timezone.now())
        self.assertChangedOnly(before, "navigation", "state", "history", "work")

    def test_browser_logout_and_password_rotation_leave_display_invalidations_unchanged(self):
        from .models import BrowserLoginSession
        from .persistent_sessions import SessionStore
        login = SessionStore()
        login["_auth_user_id"] = str(self.user.pk)
        login.save()
        before = self.versions()
        BrowserLoginSession.objects.filter(pk=login.session_key).delete()
        self.assertEqual(PresentationChangeOutbox.objects.count(), 0)
        self.assertEqual(self.versions(), before)
        before = self.versions()
        get_user_model().objects.filter(pk=self.user.pk).update(password="synthetic-password-rotation")
        self.assertEqual(PresentationChangeOutbox.objects.count(), 0)
        self.assertEqual(self.versions(), before)

    def test_relay_commit_notifies_other_processes_but_empty_drain_does_not(self):
        self.versions()
        self.event("assistant_message")  # The new listener deliberately misses this source hint.
        cfg = connection.settings_dict
        key = session_change_key(self.session.pk, "state")
        before = read_presentation_versions(self.workspace.pk, self.session.pk)["state"]
        with psycopg.connect(dbname=cfg["NAME"], user=cfg["USER"], password=cfg["PASSWORD"],
                host=cfg["HOST"], port=cfg["PORT"], autocommit=True) as listener:
            listener.execute("LISTEN " + CHANNEL)
            listener.execute("LISTEN " + PROJECTED_CHANNEL)
            with transaction.atomic():
                self.assertEqual(drain_pending_changes(), 1)
                self.assertEqual(list(listener.notifies(timeout=.03, stop_after=1)), [])
                row = listener.execute("SELECT revision FROM app_core_presentationchange WHERE key=%s", [key]).fetchone()
                self.assertEqual(0 if row is None else row[0], before)
            notices = list(listener.notifies(timeout=.15, stop_after=1))
            self.assertEqual([(notice.channel, notice.payload) for notice in notices], [(PROJECTED_CHANNEL, key)])
            self.assertGreater(listener.execute("SELECT revision FROM app_core_presentationchange WHERE key=%s", [key]).fetchone()[0], before)
            self.assertEqual(drain_pending_changes(), 0)
            self.assertEqual(list(listener.notifies(timeout=.03, stop_after=1)), [])




@skipUnless(connection.vendor == "postgresql", "presentation triggers require PostgreSQL")
class PresentationChangeGcTests(TransactionTestCase):
    serialized_rollback = True

    def isolate_resource_collectors(self):
        from types import SimpleNamespace
        module = "app_core.management.commands.gc_deleted_resources."
        results = {"expire_trash": SimpleNamespace(sessions=0, sources=0, library_objects=0),
            "collect_note_history_gc": (0, 0),
            "collect_deleted_resource_gc": SimpleNamespace(planned=[], cleaned=[], blocked=[], failures=[]),
            "collect_workspace_snapshot_gc": SimpleNamespace(planned=[], cleaned=[], blocked=[], failures=[])}
        for name, value in results.items():
            self.enterContext(patch(module + name, return_value=value))

    def test_existing_gc_projects_one_bounded_batch_without_browser_readers(self):
        from io import StringIO
        self.isolate_resource_collectors()
        PresentationChangeOutbox.objects.bulk_create([PresentationChangeOutbox(key="session:gc-only:state", transactionId=index + 1)
            for index in range(4100)])
        output = StringIO()
        call_command("gc_deleted_resources", stdout=output)
        self.assertEqual(PresentationChangeOutbox.objects.count(), 4)
        self.assertGreater(PresentationChange.objects.get(key="session:gc-only:state").revision, 0)
        self.assertIn("4096", output.getvalue())

    def test_existing_gc_dry_run_retains_dirtiness_and_does_not_write_versions(self):
        from io import StringIO
        from django.test.utils import CaptureQueriesContext
        self.isolate_resource_collectors()
        PresentationChangeOutbox.objects.create(key="session:gc-dry:state", transactionId=1)
        with CaptureQueriesContext(connection) as captured:
            call_command("gc_deleted_resources", dry_run=True, stdout=StringIO())
        self.assertEqual(PresentationChangeOutbox.objects.count(), 1)
        self.assertFalse(PresentationChange.objects.exists())
        self.assertFalse(any(query["sql"].lstrip().upper().startswith(("INSERT", "UPDATE", "DELETE"))
            for query in captured.captured_queries))


class PresentationChangeBackendTests(SimpleTestCase):
    def test_display_scope_tracks_only_its_five_data_views(self):
        self.assertEqual(presentation_change_keys('workspace', 'session'), {
            'navigation': 'workspace:workspace:navigation', 'history': 'workspace:workspace:history',
            'work': 'workspace:workspace:work', 'state': 'session:session:state',
            'context': 'session:session:context',
        })

    def test_live_readers_reject_non_postgres_backends_before_querying(self):
        with patch.object(connections["default"], "vendor", "sqlite"):
            for operation in (lambda: read_presentation_versions("workspace"),
                              drain_pending_changes):
                with self.subTest(operation=operation):
                    with self.assertRaisesRegex(RuntimeError, "presentation_changes_require_postgresql"):
                        operation()
