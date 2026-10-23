"""Owner read commands preserve observable attention semantics across storage changes."""
from unittest.mock import Mock, patch

from django.test import SimpleTestCase

from .models import Session
from .session_attention import acknowledge_session_read, session_attention_snapshot, set_manual_session_unread


class SessionAttentionCommandCharacterizationTests(SimpleTestCase):
    def setUp(self):
        self.session = Session(id="attention-command", latestAttentionSequence=10,
            readThroughSequence=4, attentionRevision=3)
        self.session.save = Mock()
        refresh = patch("app_core.session_attention.refresh_session_attention", side_effect=lambda session: session)
        high_water = patch("app_core.session_attention.durable_session_high_water", return_value=41)
        refresh.start()
        high_water.start()
        self.addCleanup(refresh.stop)
        self.addCleanup(high_water.stop)

    def test_snapshot_distinguishes_automatic_attention_from_manual_choice(self):
        self.assertEqual(session_attention_snapshot(self.session), {
            "attentionRevision": "3", "latestAttentionSequence": "10", "readThroughSequence": "4",
            "isManuallyUnread": False, "isUnread": True,
        })
        self.session.readThroughSequence = 10
        self.assertFalse(session_attention_snapshot(self.session)["isUnread"])
        self.session.save.assert_not_called()
        set_manual_session_unread(self.session, True)
        self.assertTrue(session_attention_snapshot(self.session)["isUnread"])
        self.assertTrue(session_attention_snapshot(self.session)["isManuallyUnread"])
        self.assertEqual(self.session.readThroughSequence, 10)

    def test_explicit_read_clears_manual_mark_and_acknowledges_durable_history(self):
        set_manual_session_unread(self.session, True)
        self.session.save.reset_mock()
        set_manual_session_unread(self.session, False)
        self.assertEqual(session_attention_snapshot(self.session), {
            "attentionRevision": "5", "latestAttentionSequence": "10", "readThroughSequence": "41",
            "isManuallyUnread": False, "isUnread": False,
        })
        self.session.save.assert_called_once()

    def test_automatic_read_preserves_manual_mark_and_never_regresses_water(self):
        set_manual_session_unread(self.session, True)
        self.session.save.reset_mock()
        acknowledge_session_read(self.session, 10)
        self.assertEqual(session_attention_snapshot(self.session), {
            "attentionRevision": "5", "latestAttentionSequence": "10", "readThroughSequence": "10",
            "isManuallyUnread": True, "isUnread": True,
        })
        acknowledge_session_read(self.session, 7)
        self.assertEqual(self.session.readThroughSequence, 10)
        self.session.save.assert_called_once()

    def test_repeated_explicit_actions_do_not_write_or_increment_revision(self):
        for unread in (True, False):
            set_manual_session_unread(self.session, unread)
            expected = session_attention_snapshot(self.session)
            self.session.save.reset_mock()
            set_manual_session_unread(self.session, unread)
            self.assertEqual(session_attention_snapshot(self.session), expected)
            self.session.save.assert_not_called()
