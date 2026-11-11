"""Invoke Django business reconciliation; Runtime owns leased execution and wakeups."""
import http.client
import json
import os
import re
import signal
import sys
import threading
import urllib.error
import urllib.request

PAGE_LIMIT = 100
INTERVAL_SECONDS = 5


def _cursor(value):
    return value is None or (
        isinstance(value, dict) and set(value) == {"createdAt", "id"}
        and all(isinstance(part, str) and part for part in value.values())
    )


def _identifier(value):
    return isinstance(value, str) and re.fullmatch(r"[A-Za-z0-9_.:-]{1,160}", value) is not None


class Reconciler:
    def __init__(self, post):
        self.post = post
        self.active_after = None
        self.dead_letter_after = None
        self.cleanup_after = None

    def once(self):
        active = self.post("/internal/agent-run-lifecycle/reconcile", {
            "schema": "runtime.agent_run_lifecycle.reconcile.v1", "limit": PAGE_LIMIT,
            "activeAfter": self.active_after, "deadLetterAfter": self.dead_letter_after,
        })
        if (not isinstance(active, dict)
                or set(active) != {"scheduled", "terminalized", "pending", "activeNext", "deadLetterNext"}
                or any(type(active[key]) is not int or active[key] < 0 for key in ("scheduled", "terminalized", "pending"))
                or not _cursor(active["activeNext"]) or not _cursor(active["deadLetterNext"])):
            raise ValueError("platform_lifecycle_reconciliation_invalid")
        # A later cleanup failure must not discard a successfully visited page.
        self.active_after = active["activeNext"]
        self.dead_letter_after = active["deadLetterNext"]
        cleanup = self.post("/internal/agent-run-lifecycle/cleanup-reconcile", {
            "schema": "runtime.agent_run_cleanup.reconcile.v1", "limit": PAGE_LIMIT,
            "after": self.cleanup_after,
        })
        if (not isinstance(cleanup, dict) or set(cleanup) != {"schema", "checked", "pending", "next"}
                or cleanup["schema"] != "runtime.agent_run_cleanup.reconciled.v1"
                or type(cleanup["checked"]) is not int or not 0 <= cleanup["checked"] <= PAGE_LIMIT
                or not isinstance(cleanup["pending"], list) or len(cleanup["pending"]) > cleanup["checked"]
                or not _cursor(cleanup["next"])):
            raise ValueError("platform_cleanup_reconciliation_invalid")
        for entry in cleanup["pending"]:
            if (not isinstance(entry, dict)
                    or set(entry) != {"agentRunId", "authorizationDigest", "terminalState", "jobStatus"}
                    or not _identifier(entry["agentRunId"])
                    or not isinstance(entry["authorizationDigest"], str)
                    or re.fullmatch(r"sha256:[0-9a-f]{64}", entry["authorizationDigest"]) is None
                    or not isinstance(entry["terminalState"], str)
                    or entry["terminalState"] not in {"completed", "failed", "cancelled"}
                    or not isinstance(entry["jobStatus"], str)
                    or entry["jobStatus"] not in {"queued", "leased", "running", "succeeded", "failed",
                                                "dead_lettered", "cancelled", "missing", "unknown"}):
                raise ValueError("platform_cleanup_reconciliation_invalid")
        self.cleanup_after = cleanup["next"]


class InternalApi:
    def __init__(self, url, token):
        self.url = url.rstrip("/")
        self.token = token
        self.opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))

    def post(self, path, body):
        request = urllib.request.Request(self.url + path,
            data=json.dumps(body, separators=(",", ":")).encode(),
            headers={"Content-Type": "application/json", "X-Internal-Token": self.token}, method="POST")
        try:
            with self.opener.open(request, timeout=10) as response:
                return json.loads(response.read())
        except urllib.error.HTTPError as error:
            error.close()
            raise RuntimeError("platform_reconciliation_unavailable") from None


def run(reconciler, stopped):
    while not stopped.is_set():
        try:
            reconciler.once()
        except (ValueError, RuntimeError, urllib.error.URLError, OSError, http.client.HTTPException):
            # Responses, URLs and transport exception text may contain secrets.
            print("Platform reconciliation unavailable", file=sys.stderr, flush=True)
        stopped.wait(INTERVAL_SECONDS)


def main():
    api = InternalApi(os.environ["API_INTERNAL_URL"], os.environ["INTERNAL_API_TOKEN"])
    stopped = threading.Event()
    for number in (signal.SIGTERM, signal.SIGINT):
        signal.signal(number, lambda *_: stopped.set())
    run(Reconciler(api.post), stopped)


if __name__ == "__main__":
    main()
