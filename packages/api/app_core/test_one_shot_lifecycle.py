"""One-shot deadlines and revocation use the existing durable cancel control."""
from datetime import datetime, timezone
from types import SimpleNamespace
from unittest.mock import Mock, patch

from django.test import SimpleTestCase

from .http.internal import _cancel_one_shot_run_if_required


class OneShotLifecycleTests(SimpleTestCase):
    def setUp(self):
        self.run = SimpleNamespace(status="running", transitionReason="", save=Mock(),
            authorization=SimpleNamespace(payload={"executionPolicy": {"purpose": "oneShot", "deadlineAtMs": 2000}}))
        self.cancel = self.enterContext(patch("app_core.http.internal.request_agent_run_cancellation"))
        self.current = self.enterContext(patch("app_core.http.internal.agent_run_membership_is_current", return_value=True))
        self.clock = self.enterContext(patch("app_core.http.internal.timezone.now", return_value=datetime.fromtimestamp(1, timezone.utc)))

    def test_deadline_requests_original_control_after_authority_expires(self):
        self.clock.return_value = datetime.fromtimestamp(2, timezone.utc)
        self.current.return_value = False
        self.assertTrue(_cancel_one_shot_run_if_required(self.run))
        self.cancel.assert_called_once_with(self.run, require_current_authority=False)
        self.assertEqual(self.run.transitionReason, "one_shot_deadline_expired")
        self.assertEqual(self.run.status, "running")

    def test_revocation_before_deadline_uses_original_control(self):
        self.current.return_value = False
        self.assertTrue(_cancel_one_shot_run_if_required(self.run))
        self.cancel.assert_called_once_with(self.run, require_current_authority=False)
        self.assertEqual(self.run.transitionReason, "one_shot_authority_revoked")

    def test_current_run_continues_until_deadline(self):
        self.assertFalse(_cancel_one_shot_run_if_required(self.run))
        self.cancel.assert_not_called()
        self.run.save.assert_not_called()

    def test_terminal_run_keeps_its_committed_outcome(self):
        self.run.status = "completed"
        self.current.return_value = False
        self.assertFalse(_cancel_one_shot_run_if_required(self.run))
        self.cancel.assert_not_called()
        self.run.save.assert_not_called()
