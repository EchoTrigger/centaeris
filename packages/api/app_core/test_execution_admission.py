from datetime import timedelta
from types import SimpleNamespace
from unittest.mock import Mock, patch

from django.test import SimpleTestCase, override_settings
from django.utils import timezone

from app_core.http.internal import _expire_queued_run_locked


@override_settings(EXECUTION_QUEUE_WAIT_SECONDS=300)
class InitialQueueExpiryTests(SimpleTestCase):
    def run_state(self, *, status="queued", started_at=None, age=301):
        return SimpleNamespace(status=status, startedAt=started_at,
                               createdAt=timezone.now() - timedelta(seconds=age),
                               transitionReason="agent_run_created", save=Mock())

    @patch("app_core.http.internal.request_agent_run_cancellation")
    def test_expiry_requests_semantic_cancellation_without_inventing_terminal(self, cancel):
        run = self.run_state()
        self.assertTrue(_expire_queued_run_locked(run))
        cancel.assert_called_once_with(run)
        self.assertEqual(run.status, "queued")
        self.assertEqual(run.transitionReason, "execution_queue_expired")

    @patch("app_core.http.internal.request_agent_run_cancellation")
    def test_initial_deadline_excludes_started_and_logically_waiting_runs(self, cancel):
        for run in [self.run_state(age=299), self.run_state(status="running"),
                    self.run_state(started_at=timezone.now())]:
            self.assertFalse(_expire_queued_run_locked(run))
            run.save.assert_not_called()
        cancel.assert_not_called()

    @patch("app_core.http.internal.request_agent_run_cancellation",
           side_effect=RuntimeError("runtime_unavailable"))
    def test_unavailable_cancellation_does_not_mark_expiry_committed(self, _cancel):
        run = self.run_state()
        with self.assertRaises(RuntimeError):
            _expire_queued_run_locked(run)
        self.assertEqual(run.transitionReason, "agent_run_created")
        run.save.assert_not_called()
