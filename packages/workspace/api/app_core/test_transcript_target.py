from unittest.mock import patch

from django.test import TestCase

from .models import Session
from .session_record_test_fixtures import SessionRecordFixture


class TranscriptTargetTests(SessionRecordFixture, TestCase):
    def test_visible_targets_use_the_current_view_and_core_block_identity(self):
        run = self.message_run(self.session)
        for event_type, payload in (
            ("user_message", {"text": "用户权限问题"}),
            ("phase_event", {"stage": "model_process_summary", "message": "正在核对权限"}),
            ("assistant_message", {"modelMarkdown": "权限答复"}),
        ):
            event = self.commit(run, event_type, {"messageId": f"message-{event_type}", **payload})
            block_id = "notice:" + event.pk if event_type == "phase_event" else event.payload["payload"]["messageId"]
            page = {"schema": "transcript.page.v1", "blocks": [{"blockId": block_id, "orderKey": {"sourceSequence": str(event.sequence), "ordinal": 0}}]}
            with patch("app_core.http.workspaces.request_transcript_page", return_value=page) as read:
                response = self.client.get(f"/api/sessions/{self.session.pk}/transcript", {"anchorEventId": event.pk})
                self.assertEqual(response.status_code, 200)
                self.assertEqual(response.json(), {"schema": "workspace.transcript.target.v1", "targetBlockId": block_id, "page": page})
                self.assertEqual(read.call_args.args[0]["anchorSequence"], str(event.sequence))
                self.assertIsNone(read.call_args.args[0]["olderCursor"])

    def test_target_scope_and_query_contract_are_enforced_before_runtime_reads(self):
        run = self.message_run(self.session)
        event = self.commit(run, "user_message", {"text": "权限"})
        hidden = self.commit(run, "phase_event", {"stage": "model_process_state", "message": "隐藏状态"})
        other_session = Session.objects.create(workspace=self.workspace, owner=self.user)
        with patch("app_core.http.workspaces.request_transcript_page") as read:
            for session_id, params, status in (
                (other_session.pk, {"anchorEventId": event.pk}, 404),
                (self.session.pk, {"anchorEventId": hidden.pk}, 404),
                (self.session.pk, {"anchorEventId": "missing"}, 404),
                (self.session.pk, {"anchorEventId": event.pk, "olderCursor": "cursor"}, 400),
                (self.session.pk, {"anchorEventId": [event.pk, event.pk]}, 400),
                (self.session.pk, {"anchorEventId": ""}, 400),
                (self.session.pk, {"anchorEventId": event.pk, "sourceHighWater": "0", "projectionGeneration": "generation"}, 400),
            ):
                self.assertEqual(self.client.get(f"/api/sessions/{session_id}/transcript", params).status_code, status)
            self.client.force_login(self.other)
            self.assertEqual(self.client.get(f"/api/sessions/{self.session.pk}/transcript", {"anchorEventId": event.pk}).status_code, 404)
            read.assert_not_called()
