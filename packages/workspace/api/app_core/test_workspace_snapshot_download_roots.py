"""Historical checkpoint storage does not authorize recovery after it leaves Core roots."""
import json
from unittest.mock import patch

from django.conf import settings
from django.test import TransactionTestCase

from . import test_execution_publication_fencing as fixtures


class WorkspaceSnapshotDownloadRootTests(fixtures.ExecutionPublicationFixture, TransactionTestCase):
    serialized_rollback = True
    _checkpoint_download_body = fixtures.ExecutionPublicationFencingTests._checkpoint_download_body

    def download(self, body):
        response = self.client.post("/internal/agent-runs/execution-workspace/download",data=json.dumps(body),
            content_type="application/json",HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN)
        response.close()
        return response

    def test_historical_checkpoint_without_a_current_runtime_root_is_rejected(self):
        body, _, _ = self._checkpoint_download_body()
        self.workspace_root_manifest["roots"] = []
        response = self.download(body)
        self.assertEqual(response.status_code,409)
        self.assertEqual(response.json(),{"error":"execution_workspace_checkpoint_unreachable"})

    def test_changed_root_witness_cannot_open_a_recovery_object(self):
        body, _, _ = self._checkpoint_download_body()
        with patch("app_core.workspace_snapshot_roots.current_witness",return_value=(2,"sha256:"+"b"*64)):
            response = self.download(body)
        self.assertEqual(response.status_code,409)
        self.assertEqual(response.json(),{"error":"execution_workspace_roots_changed"})
