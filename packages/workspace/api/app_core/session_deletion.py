"""Runtime erasure must be acknowledged before the platform marks a purge complete."""
import json
import urllib.error

from django.conf import settings
from django.db import connection
from django.utils import timezone

from .resource_commit import require_durable_commit
from .runtime_http import request_json


def record_session_purge_intent(session):
    """Caller holds the Session admission lock; Runtime acts after commit."""
    if session.purgeRequestedAt is None:
        require_durable_commit(connection, RuntimeError("session_delete_intent_durability_required"))
        session.purgeRequestedAt = timezone.now()
        session.save(update_fields=["purgeRequestedAt", "updatedAt"])


def purge_session_runtime(session):
    try:
        receipt = request_json(
            "/internal/sessions/purge",
            {
                "schema": "runtime.session.purge.v1",
                "sessionId": session.id,
                "workspaceId": session.workspace_id,
                "ownerId": session.owner_id,
            },
            timeout=settings.RUNTIME_CONTROL_TIMEOUT_SECONDS,
        )
    except (urllib.error.HTTPError, urllib.error.URLError, TimeoutError, json.JSONDecodeError) as error:
        if isinstance(error, urllib.error.HTTPError):
            error.close()
        raise RuntimeError("session_purge_runtime_unavailable") from error
    if not isinstance(receipt, dict) or receipt.get("purged") is not True or receipt != {
        "schema": "runtime.session.purged.v1",
        "sessionId": session.id,
        "workspaceId": session.workspace_id,
        "purged": True,
    }:
        raise RuntimeError("session_purge_runtime_receipt_invalid")
