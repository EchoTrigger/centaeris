"""Read committed Runtime receipts at the direct-store fixture's HTTP seam."""
from contextlib import contextmanager
import urllib.error
from unittest.mock import patch

from django.db import connection


def persisted_cleanup_receipt(body):
    """Adapt a real committed receipt to the Runtime's public read response."""
    if (not isinstance(body, dict)
            or set(body) != {"schema", "agentRunId", "authorizationDigest", "cleanupReceiptId"}
            or body["schema"] != "runtime.agent_run.cleanup_receipt.get.v1"):
        raise ValueError("cleanup_receipt_fixture_request_invalid")
    with connection.cursor() as cursor:
        cursor.execute(
            "SELECT receipt_id,agent_run_id,environment_id,authorization_digest,controller_epoch,"
            "allocation_kind,instance_id,cleanup_complete,file_protection FROM runtime.agent_run_cleanup_receipts "
            "WHERE agent_run_id=%s AND authorization_digest=%s AND receipt_id=%s",
            [body["agentRunId"], body["authorizationDigest"], body["cleanupReceiptId"]],
        )
        row = cursor.fetchone()
    if row is None:
        raise urllib.error.HTTPError("http://runtime-fixture/internal/agent-runs/cleanup-receipt",
                                     404, "cleanup receipt not found", {}, None)
    receipt_id, run, environment, digest, epoch, kind, instance, complete, protection = row
    if protection != "deferred":
        raise ValueError("cleanup_receipt_fixture_protection_invalid")
    allocation = {"kind": kind}
    if instance is not None:
        allocation["instanceId"] = instance
    return {"schema": "runtime.agent_run.cleanup_receipt.v1", "receiptId": receipt_id,
            "agentRunId": run, "environmentId": environment, "authorizationDigest": digest,
            "controllerEpoch": epoch, "allocation": allocation, "cleanupComplete": complete}


@contextmanager
def persisted_runtime_cleanup_receipts():
    """Keep production receipt validation and lifecycle release unchanged."""
    from . import runtime_client
    original = runtime_client.request_json

    def request(path, body=None, *, timeout):
        if path == "/internal/agent-runs/cleanup-receipt":
            return persisted_cleanup_receipt(body)
        return original(path, body, timeout=timeout)

    with patch("app_core.runtime_client.request_json", side_effect=request):
        yield
