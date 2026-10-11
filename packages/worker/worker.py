import json
import re
import os
import http.client
import signal
import socket
import sys
import threading
import time
import urllib.error
import urllib.request
import urllib.parse
import uuid
import observations
from contextlib import contextmanager
from datetime import datetime

RUNTIME_INTERNAL_URL = os.environ["RUNTIME_INTERNAL_URL"].rstrip("/")
API_INTERNAL_URL = os.environ["API_INTERNAL_URL"].rstrip("/")
INTERNAL_API_TOKEN = os.environ["INTERNAL_API_TOKEN"]
LEASE_MS = int(os.environ.get("WORKER_LEASE_MS", "60000"))
AGENT_RUN_LIFECYCLE_RECHECK_MS = int(os.environ.get("AGENT_RUN_LIFECYCLE_RECHECK_MS", "30000"))
RUNTIME_JOB_WAIT_RECHECK_MS = 5 * 60 * 1000
JOB_WAIT_MS = 20_000
JOB_WAIT_HTTP_TIMEOUT_SECONDS = 25
JOB_WAIT_FAILURE_BACKOFF_SECONDS = 1
OUTBOX_POLL_INTERVAL_SECONDS = 1
RECONCILE_INTERVAL_SECONDS = 5
CONTROL_PAGE_LIMIT = 100
WORKER_JOB_KINDS = ("agent_run.lifecycle", "worker.noop", "agent_work.return")
CONTROL_HTTP_TIMEOUT_SECONDS = int(os.environ.get("RUNTIME_HTTP_CONTROL_TIMEOUT_SECONDS", "5"))
if not 1 <= CONTROL_HTTP_TIMEOUT_SECONDS <= 1_000_000:
    raise ValueError("RUNTIME_HTTP_CONTROL_TIMEOUT_SECONDS must be between 1 and 1000000")
try:
    WORKER_SLOT_COUNT = int(os.environ.get("WORKER_SLOT_COUNT", "8"))
except ValueError:
    raise ValueError("WORKER_SLOT_COUNT must be an integer between 1 and 16") from None
if not 1 <= WORKER_SLOT_COUNT <= 16:
    raise ValueError("WORKER_SLOT_COUNT must be an integer between 1 and 16")
AGENT_RUN_WAITING_TRANSITION_REASONS = {
    "execution_recovery_checkpoint_committed",
    "question_wait",
    "runtime_job_wait",
    "session_record_commit_unavailable",
    "session_workspace_commit_unavailable",
    "session_workspace_resolve_unavailable",
}


class RuntimeJobCancelled(RuntimeError):
    pass


class DependencyUnavailable(RuntimeError):
    def __init__(self, reason, http_status=None):
        super().__init__(reason)
        self.http_status = http_status


class RuntimeStepFailed(RuntimeError):
    def __init__(self, reason, retryable, agent_run_id, http_status=None):
        super().__init__(reason)
        self.retryable = retryable
        self.agent_run_id = agent_run_id
        self.http_status = http_status


def runtime_request(path, body=None):
    return json_request(
        f"{RUNTIME_INTERNAL_URL}{path}",
        body,
        "X-Internal-Token",
        INTERNAL_API_TOKEN,
        "runtime_job_request_failed",
        timeout=(CONTROL_HTTP_TIMEOUT_SECONDS
                 if path == "/internal/jobs/heartbeat" or body is None else 10),
    )


def agent_run_step_request(body):
    try:
        return json_request(
            f"{RUNTIME_INTERNAL_URL}/agent-runs/step",
            body,
            "X-Internal-Token",
            INTERNAL_API_TOKEN,
            "agent_run_step_unavailable",
            timeout=None,
        )
    except RuntimeStepFailed as error:
        if error.agent_run_id != body.get("agentRunStart", {}).get("agentRunId"):
            raise RuntimeError("runtime_step_failure_response_invalid") from error
        raise


def runtime_teardown_request(body):
    return json_request(
        f"{RUNTIME_INTERNAL_URL}/agent-runs/teardown",
        body,
        "X-Internal-Token",
        INTERNAL_API_TOKEN,
        "sandbox_teardown_failed",
        timeout=30,
    )




def api_request(path, body, default_reason, *, timeout=10):
    return json_request(
        f"{API_INTERNAL_URL}{path}",
        body,
        "X-Internal-Token",
        INTERNAL_API_TOKEN,
        default_reason,
        timeout=timeout,
    )


def json_request(url, body, token_header, token, default_reason, timeout=10):
    path = urllib.parse.urlsplit(url).path
    route = "/internal/jobs/:id" if path.startswith("/internal/jobs/") and body is None else path
    with observations.measure("rpc", route=route):
        try:
            return _json_request(url, body, token_header, token, default_reason, timeout)
        except DependencyUnavailable as error:
            error.route = route
            raise


def _json_request(url, body, token_header, token, default_reason, timeout=10):
    method = "GET" if body is None else "POST"
    request = urllib.request.Request(
        url,
        data=None if body is None else json.dumps(body, separators=(",", ":")).encode(),
        headers={"Content-Type": "application/json", token_header: token},
        method=method,
    )
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            return json.loads(response.read())
    except urllib.error.HTTPError as error:
        status = error.code
        try:
            payload = json.loads(error.read())
            reason = payload.get("error", default_reason)
        except (json.JSONDecodeError, AttributeError):
            payload = None
            reason = default_reason
        error.close()
        if isinstance(payload, dict) and payload.get("schema") == "runtime.agent_run.step.failure.v1":
            if set(payload) != {
                "schema",
                "agentRunId",
                "failureClass",
                "retryable",
                "transitionReason",
                "error",
            } or not (
                isinstance(payload.get("agentRunId"), str)
                and payload["agentRunId"]
                and isinstance(payload.get("failureClass"), str)
                and payload["failureClass"]
                and isinstance(payload.get("retryable"), bool)
                and isinstance(payload.get("transitionReason"), str)
                and payload["transitionReason"]
                and payload.get("error") == "runtime_step_failed"
            ):
                raise RuntimeError("runtime_step_failure_response_invalid") from error
            raise RuntimeStepFailed(
                reason, payload["retryable"], payload["agentRunId"], http_status=status
            ) from error
        if status in {500, 502, 503, 504}:
            raise DependencyUnavailable(reason, http_status=status) from error
        failure = RuntimeError(reason)
        failure.http_status = status
        failure.http_error_code = (payload["error"]
                                   if isinstance(payload, dict) and set(payload) == {"error"}
                                   and isinstance(payload["error"], str) else None)
        raise failure from error
    except (
        urllib.error.URLError,
        TimeoutError,
        http.client.RemoteDisconnected,
        json.JSONDecodeError,
    ) as error:
        raise DependencyUnavailable(default_reason) from error


def now_ms():
    return time.time_ns() // 1_000_000


def heartbeat(job_id, lease_owner):
    try:
        runtime_request(
            "/internal/jobs/heartbeat",
            {
                "schema": "runtime.job.heartbeat.v1",
                "jobId": job_id,
                "leaseOwner": lease_owner,
                "heartbeatAtMs": now_ms(),
                "leaseMs": LEASE_MS,
            },
        )
    except RuntimeError as error:
        raise_if_job_cancelled(job_id, error, "job_heartbeat_rejected")


@contextmanager
def lease_heartbeats(job_id, lease_owner):
    heartbeat(job_id, lease_owner)
    stopped = threading.Event()
    errors = []

    def renew():
        while not stopped.wait(max(1, LEASE_MS // 3000)):
            try:
                heartbeat(job_id, lease_owner)
            except Exception as error:
                errors.append(error)
                return

    thread = threading.Thread(target=renew, daemon=True)
    thread.start()

    def require_healthy_lease():
        if errors:
            raise errors[0]

    try:
        yield require_healthy_lease
    finally:
        stopped.set()
        thread.join()


def complete_job(job_id, lease_owner, output_refs):
    try:
        runtime_request(
            "/internal/jobs/complete",
            {
                "schema": "runtime.job.complete.v1",
                "jobId": job_id,
                "leaseOwner": lease_owner,
                "completedAtMs": now_ms(),
                "outputRefs": output_refs,
            },
        )
    except RuntimeError as error:
        raise_if_job_cancelled(job_id, error, "job_complete_rejected")


def start_job(job_id, lease_owner):
    try:
        runtime_request(
            "/internal/jobs/start",
            {
                "schema": "runtime.job.start.v1",
                "jobId": job_id,
                "leaseOwner": lease_owner,
                "atMs": now_ms(),
            },
        )
    except RuntimeError as error:
        raise_if_job_cancelled(job_id, error, "job_start_rejected")


def yield_job(job_id, lease_owner, run_at_ms, transition_reason):
    try:
        runtime_request(
            "/internal/jobs/yield",
            {
                "schema": "runtime.job.yield.v1",
                "jobId": job_id,
                "leaseOwner": lease_owner,
                "yieldedAtMs": now_ms(),
                "runAtMs": run_at_ms,
                "transitionReason": transition_reason,
            },
        )
    except RuntimeError as error:
        raise_if_job_cancelled(job_id, error, "job_yield_rejected")


def raise_if_job_cancelled(job_id, error, rejected_reason):
    if str(error) != rejected_reason:
        raise error
    response = runtime_request(f"/internal/jobs/{job_id}")
    job = response.get("job") if isinstance(response, dict) else None
    if (
        not isinstance(job, dict)
        or job.get("jobId") != job_id
        or not isinstance(job.get("status"), str)
    ):
        raise RuntimeError("runtime_job_state_invalid") from error
    if job["status"] == "cancelled":
        raise RuntimeJobCancelled("job_cancelled_during_worker_lease") from error
    raise error


def fail_job(job, lease_owner, reason, retryable):
    retry_count = job.get("retryCount")
    max_retries = job.get("maxRetries")
    if (
        not isinstance(retry_count, int)
        or isinstance(retry_count, bool)
        or not isinstance(max_retries, int)
        or isinstance(max_retries, bool)
        or not 0 <= retry_count <= max_retries <= 10
    ):
        raise RuntimeError("runtime_job_retry_state_invalid")
    terminal = not retryable or retry_count + 1 > max_retries
    response = runtime_request(
        "/internal/jobs/fail",
        {
            "schema": "runtime.job.fail.v1",
            "jobId": job["jobId"],
            "leaseOwner": lease_owner,
            "failedAtMs": now_ms(),
            "error": reason,
            "retryable": retryable,
        },
    )
    disposition = response.get("disposition")
    expected_disposition = "failed" if not retryable else "dead_lettered" if terminal else "retry_scheduled"
    if disposition != expected_disposition:
        raise RuntimeError("runtime_job_fail_response_invalid")
    return terminal


def fail_claimed_job(job, lease_owner, reason, retryable):
    terminal = fail_job(job, lease_owner, reason, retryable)
    if terminal and job["jobKind"] == "agent_run.lifecycle":
        agent_run_id, _digest = agent_run_lifecycle_binding(job)
        transition_agent_run(
            agent_run_id,
            "failed",
            "agent_run_lifecycle_dead_lettered" if retryable else "runtime_step_failed",
        )
    return terminal


def transition_agent_run(agent_run_id, state, transition_reason):
    response = api_request(
        "/internal/agent-runs/transition",
        {
            "schema": "runtime.agent_run.transition.v1",
            "agentRunId": agent_run_id,
            "state": state,
            "transitionReason": transition_reason,
        },
        "agent_run_transition_unavailable",
    )
    if response != {"agentRunId": agent_run_id, "state": state} and not (
        state == "failed"
        and transition_reason == "agent_run_lifecycle_dead_lettered"
        and response == {"agentRunId": agent_run_id, "state": "completed"}
    ):
        raise RuntimeError("agent_run_transition_response_invalid")


def agent_run_lifecycle_binding(job):
    job_id = job.get("jobId")
    payload_ref = job.get("payloadRef")
    session_id = job.get("sessionId")
    idempotency_key = job.get("idempotencyKey")
    prefix = "agent_run.lifecycle:"
    if (
        not isinstance(job_id, str)
        or not job_id.startswith(prefix)
        or not isinstance(payload_ref, str)
        or not isinstance(session_id, str)
        or not session_id
        or not isinstance(idempotency_key, str)
    ):
        raise RuntimeError("agent_run_lifecycle_binding_invalid")
    agent_run_id = job_id[len(prefix) :]
    digest_prefix = f"agent_run.lifecycle:{agent_run_id}:"
    digest = (
        idempotency_key[len(digest_prefix) :]
        if idempotency_key.startswith(digest_prefix)
        else ""
    )
    if (
        not agent_run_id
        or payload_ref != f"record:agent_run:{agent_run_id}"
        or len(digest) != 71
        or not digest.startswith("sha256:")
        or any(character not in "0123456789abcdef" for character in digest[7:])
    ):
        raise RuntimeError("agent_run_lifecycle_binding_invalid")
    return agent_run_id, digest


def execute_agent_run_lifecycle_job(job, lease_owner, require_healthy_lease):
    agent_run_id, digest = agent_run_lifecycle_binding(job)
    resolved = api_request(
        "/internal/agent-run-lifecycle/resolve",
        {
            "schema": "runtime.agent_run_lifecycle.resolve.v1",
            "jobId": job["jobId"],
            "agentRunId": agent_run_id,
            "authorizationDigest": digest,
        },
        "agent_run_start_unavailable",
    )
    if (
        not isinstance(resolved, dict)
        or resolved.get("schema") != "runtime.agent_run_lifecycle.resolved.v1"
        or resolved.get("disposition") not in {"ready", "terminal"}
    ):
        raise RuntimeError("agent_run_lifecycle_resolve_response_invalid")
    agent_run_start = resolved.get("agentRunStart")
    if (
        not isinstance(agent_run_start, dict)
        or agent_run_start.get("agentRunId") != agent_run_id
        or agent_run_start.get("authorizationDigest") != digest
    ):
        raise RuntimeError("agent_run_lifecycle_resolve_response_invalid")
    if resolved["disposition"] == "terminal":
        if set(resolved) != {
            "schema",
            "disposition",
            "terminalState",
            "agentRunStart",
        } or resolved["terminalState"] not in {"completed", "failed", "cancelled"}:
            raise RuntimeError("agent_run_lifecycle_resolve_response_invalid")
        require_healthy_lease()
        finish_agent_run_lifecycle(job, lease_owner, agent_run_start, resolved["terminalState"])
        return False
    if set(resolved) != {"schema", "disposition", "agentRunStart"}:
        raise RuntimeError("agent_run_lifecycle_resolve_response_invalid")
    transition_agent_run(agent_run_id, "running", "agent_run_lifecycle_step_started")
    require_healthy_lease()
    result = agent_run_step_request(
        {
            "schema": "runtime.agent_run.step.v1",
            "jobId": job["jobId"],
            "leaseOwner": lease_owner,
            "agentRunStart": agent_run_start,
        }
    )
    require_healthy_lease()
    if (
        not isinstance(result, dict)
        or set(result)
        != ({"schema", "agentRunId", "disposition", "terminalState", "transitionReason"}
            | ({"retryAtMs"} if result.get("transitionReason") == "execution_recovery_checkpoint_committed" else set()))
        or result.get("schema") != "runtime.agent_run.step.result.v1"
        or result.get("agentRunId") != agent_run_id
        or result.get("disposition") not in {"waiting", "terminal"}
        or not isinstance(result.get("transitionReason"), str)
        or not result["transitionReason"]
    ):
        raise RuntimeError("agent_run_step_response_invalid")
    if result["transitionReason"] == "execution_recovery_checkpoint_committed" and (
        result["disposition"] != "waiting"
        or type(result.get("retryAtMs")) is not int
        or result["retryAtMs"] < 0
    ):
        raise RuntimeError("agent_run_step_response_invalid")
    if result["disposition"] == "waiting":
        if (
            result["terminalState"] is not None
            or result["transitionReason"] not in AGENT_RUN_WAITING_TRANSITION_REASONS
        ):
            raise RuntimeError("agent_run_step_response_invalid")
        transition_agent_run(agent_run_id, "running", result["transitionReason"])
        require_healthy_lease()
        yielded_at_ms = now_ms()
        if result["transitionReason"] == "execution_recovery_checkpoint_committed":
            # Runtime derives this deadline from durable recovery facts. Waiting
            # releases the execution slot; retries never sleep inside a worker.
            next_run_at_ms = max(yielded_at_ms, result["retryAtMs"])
        elif result["transitionReason"] == "runtime_job_wait":
            next_run_at_ms = yielded_at_ms + RUNTIME_JOB_WAIT_RECHECK_MS
        else:
            next_run_at_ms = yielded_at_ms + AGENT_RUN_LIFECYCLE_RECHECK_MS
        yield_job(
            job["jobId"],
            lease_owner,
            next_run_at_ms,
            result["transitionReason"],
        )
        return False
    if result["terminalState"] not in {"completed", "failed", "cancelled"}:
        raise RuntimeError("agent_run_step_response_invalid")
    require_healthy_lease()
    finish_agent_run_lifecycle(job, lease_owner, agent_run_start, result["terminalState"])
    return False


def finish_agent_run_lifecycle(job, lease_owner, agent_run_start, terminal_state):
    agent_run_id = agent_run_start["agentRunId"]
    transition_agent_run(
        agent_run_id,
        terminal_state,
        "agent_run_cancelled"
        if terminal_state == "cancelled"
        else "runtime_session_terminal_committed",
    )
    result = runtime_teardown_request(
        {
            "schema": "runtime.agent_run.teardown.v1",
            "jobId": job["jobId"],
            "leaseOwner": lease_owner,
            "agentRunStart": agent_run_start,
        }
    )
    if result != {
        "schema": "runtime.agent_run.teardown.result.v1",
        "agentRunId": agent_run_id,
        "status": "removed",
    }:
        raise RuntimeError("runtime_teardown_response_invalid")
    complete_job(job["jobId"], lease_owner, [])


def valid_runtime_job_id(value):
    return (
        isinstance(value, str)
        and 1 <= len(value) <= 160
        and all(
            character.isascii() and (character.isalnum() or character in "_-:.")
            for character in value
        )
    )


def valid_lease_owner(value):
    return (
        isinstance(value, str)
        and 16 <= len(value.encode("utf-8")) <= 160
        and all(ord(character) >= 32 and not 127 <= ord(character) < 160 for character in value)
    )


def claim_job(job_kind, worker_id):
    if job_kind not in WORKER_JOB_KINDS:
        raise RuntimeError("worker_job_kind_invalid")
    response = runtime_request(
        "/internal/jobs/claim",
        {
            "schema": "runtime.job.claim.v1",
            "workerId": worker_id,
            "jobId": None,
            "jobKind": job_kind,
            "nowMs": now_ms(),
            "leaseMs": LEASE_MS,
            "limit": 1,
        },
    )
    if not isinstance(response, dict) or set(response) != {"jobs"}:
        raise RuntimeError("runtime_job_claim_response_invalid")
    jobs = response["jobs"]
    if not isinstance(jobs, list) or len(jobs) > 1:
        raise RuntimeError("runtime_job_claim_response_invalid")
    if not jobs:
        observations.emit("claimResult", time.monotonic(), outcome="empty")
        return None
    job = jobs[0]
    if (
        not isinstance(job, dict)
        or not valid_runtime_job_id(job.get("jobId"))
        or job.get("jobKind") != job_kind
        or not valid_lease_owner(job.get("leaseOwner"))
        or job.get("status") != "leased"
    ):
        raise RuntimeError("runtime_job_claim_response_invalid")
    observations.emit("claimResult", time.monotonic(), outcome="claimed", jobId=job["jobId"])
    return job


def wait_for_jobs():
    response = json_request(
        f"{RUNTIME_INTERNAL_URL}/internal/jobs/wait",
        {
            "schema": "runtime.job.wait.v1",
            "jobKinds": list(WORKER_JOB_KINDS),
            "waitMs": JOB_WAIT_MS,
        },
        "X-Internal-Token",
        INTERNAL_API_TOKEN,
        "runtime_job_wait_unavailable",
        timeout=JOB_WAIT_HTTP_TIMEOUT_SECONDS,
    )
    if (
        not isinstance(response, dict)
        or set(response) != {"schema", "disposition", "nextRunAtMs"}
        or response.get("schema") != "runtime.job.wait.result.v1"
        or response.get("disposition") not in {"ready", "timeout"}
        or (
            response.get("disposition") == "ready"
            and response.get("nextRunAtMs") is None
        )
        or not (
            response.get("nextRunAtMs") is None
            or (
                isinstance(response.get("nextRunAtMs"), int)
                and not isinstance(response.get("nextRunAtMs"), bool)
            )
        )
    ):
        raise RuntimeError("runtime_job_wait_response_invalid")
    return response["disposition"]


def execute_claimed_job(job, lease_owner):
    with observations.context(jobId=job["jobId"]), observations.measure("slotHeld"):
        return _execute_claimed_job(job, lease_owner)


def _execute_claimed_job(job, lease_owner):
    try:
        start_job(job["jobId"], lease_owner)
        if job["jobKind"] not in WORKER_JOB_KINDS:
            raise RuntimeError("unknown_job_kind")
        with lease_heartbeats(job["jobId"], lease_owner) as require_healthy_lease:
            if job["jobKind"] == "worker.noop":
                output_refs = []
                completed = True
            elif job["jobKind"] == "agent_work.return":
                output_refs = []
                completed = execute_work_return_job(job, lease_owner, require_healthy_lease)
            else:
                output_refs = []
                completed = execute_agent_run_lifecycle_job(job, lease_owner, require_healthy_lease)
            if completed:
                require_healthy_lease()
                complete_job(job["jobId"], lease_owner, output_refs)
    except RuntimeJobCancelled as error:
        print(
            f"worker job stopped: jobId={job['jobId']}; transitionReason={error}",
            flush=True,
        )
    except DependencyUnavailable:
        fail_claimed_job(job, lease_owner, "dependency_unavailable", True)
    except RuntimeStepFailed as error:
        fail_claimed_job(job, lease_owner, str(error), error.retryable)
    except RuntimeError as error:
        reason = str(error)
        if job["jobKind"] == "agent_run.lifecycle":
            if reason == "agent_run_lifecycle_lease_lost":
                print(
                    f"run lifecycle stopped after lease loss: jobId={job['jobId']}",
                    flush=True,
                )
                return
            print(
                f"run lifecycle attempt failed: jobId={job['jobId']}; reason={reason}",
                file=sys.stderr,
                flush=True,
            )
            fail_claimed_job(job, lease_owner, "agent_run_lifecycle_failed", False)
            return
        if job["jobKind"] == "agent_work.return":
            fail_claimed_job(job, lease_owner, reason, False)
            return
        raise


def execute_work_return_job(job, lease_owner, require_healthy_lease):
    deadline = time.monotonic() + CONTROL_HTTP_TIMEOUT_SECONDS
    prefix = "agent_work.return:"
    identity = job["jobId"].removeprefix(prefix)
    if (not job["jobId"].startswith(prefix) or not valid_runtime_job_id(identity)
            or job.get("payloadRef") != "record:agent_work:" + identity
            or job.get("idempotencyKey") != job["jobId"]):
        raise RuntimeError("work_return_job_binding_invalid")
    require_healthy_lease()
    result = api_request("/internal/agent-work/returns/materialize",
        {"schema": "workspace.agent_work.return_materialize.v1", "workAgentRunId": identity},
        "work_return_materialize_unavailable", timeout=deadline - time.monotonic())
    require_healthy_lease()
    if (not isinstance(result, dict) or set(result) != {"schema", "disposition", "notice"}
            or result["schema"] != "workspace.agent_work.return_materialized.v1"
            or result["disposition"] not in {"delivered", "duplicate", "pending", "notWork"}):
        raise RuntimeError("work_return_materialize_invalid")
    if result["disposition"] in {"pending", "notWork"}:
        if result["notice"] is not None:
            raise RuntimeError("work_return_materialize_invalid")
        yield_job(job["jobId"], lease_owner, now_ms() + AGENT_RUN_LIFECYCLE_RECHECK_MS, "work_return_pending")
        return False
    if result["notice"] is None:
        raise RuntimeError("work_return_materialize_invalid")
    return True


def execute_next_job(slot_index):
    worker_id = f"worker:{socket.gethostname()}:{uuid.uuid4().hex}"
    for offset in range(len(WORKER_JOB_KINDS)):
        job_kind = WORKER_JOB_KINDS[(slot_index + offset) % len(WORKER_JOB_KINDS)]
        job = claim_job(job_kind, worker_id)
        if job is not None:
            execute_claimed_job(job, job["leaseOwner"])
            return True
    return False


def waiter_page_next(response, disposition, after):
    if (
        not isinstance(response, dict)
        or set(response) != {"disposition", "checked", "waiters", "next"}
        or response["disposition"] not in disposition
        or type(response["checked"]) is not int
        or not 0 <= response["checked"] <= 256
        or not isinstance(response["waiters"], list)
    ):
        raise RuntimeError("runtime_job_waiter_page_invalid")
    cursor = response["next"]
    if cursor is not None and (
        not isinstance(cursor, dict)
        or set(cursor) != {"checkpointId", "toolCallId"}
        or any(not isinstance(value, str) or not value.strip() for value in cursor.values())
        or cursor == after
        or response["checked"] == 0
    ):
        raise RuntimeError("runtime_job_waiter_cursor_invalid")
    return cursor


class TerminalDispatcher:
    def __init__(self):
        self.cursors = {}

    def __call__(self):
        return dispatch_terminal_once(self.cursors)


def dispatch_terminal_once(cursors=None):
    if cursors is None:
        cursors = {}
    events = runtime_request(
        "/internal/job-outbox/pending",
        {"schema": "runtime.job.outbox.pending.v1", "limit": 100},
    )["events"]
    for event in events:
        if (
            not isinstance(event, dict)
            or set(event) != {"jobId", "eventType", "publishedAtMs", "generation"}
            or not valid_runtime_job_id(event.get("jobId"))
            or event.get("eventType") != "runtime_job.terminal"
            or event.get("publishedAtMs") is not None
            or not isinstance(event.get("generation"), int)
            or isinstance(event.get("generation"), bool)
            or not 0 <= event["generation"] <= 4_294_967_295
        ):
            raise RuntimeError("runtime_job_outbox_event_invalid")
        key = (event["jobId"], event["generation"])
        after = cursors.get(key)
        wake = runtime_request(
            "/internal/job-outbox/wake-waiter",
            {
                "schema": "runtime.job.waiter_wake.v1",
                "jobId": event["jobId"],
                "generation": event["generation"],
                "after": after,
            },
        )
        next_cursor = waiter_page_next(wake, {"woken", "no_waiter"}, after)
        if next_cursor is not None:
            cursors[key] = next_cursor
            # A partial source fan-out remains pending. Process its next page on
            # the next control round; never acknowledge an incomplete delivery.
            continue
        published = runtime_request(
            "/internal/job-outbox/published",
            {
                "schema": "runtime.job.outbox.published.v1",
                "jobId": event["jobId"],
                "eventType": event["eventType"],
                "generation": event["generation"],
                "publishedAtMs": now_ms(),
            },
        )
        if published.get("disposition") not in {
            "published",
            "already_published",
            "stale",
        }:
            raise RuntimeError("runtime_job_outbox_publish_response_invalid")
        cursors.pop(key, None)
    visible = {(event["jobId"], event["generation"]) for event in events}
    for key in list(cursors):
        if key not in visible:
            del cursors[key]
    return len(events)




class LifecycleReconciler:
    def __init__(self):
        # A restart safely begins a new pass; reconciliation is idempotent.
        self.active_after = None
        self.dead_letter_after = None
        self.waiter_after = None

    def __call__(self):
        runtime = runtime_request(
            "/internal/jobs/reconcile",
            {"schema": "runtime.job.reconcile.v1", "nowMs": now_ms()},
        )
        lifecycle = api_request(
            "/internal/agent-run-lifecycle/reconcile",
            {"schema": "runtime.agent_run_lifecycle.reconcile.v1", "limit": CONTROL_PAGE_LIMIT,
             "activeAfter": self.active_after, "deadLetterAfter": self.dead_letter_after},
            "agent_run_lifecycle_reconcile_unavailable",
        )
        # Advance successful API pages even if the independent waiter pass fails.
        self.active_after = lifecycle["activeNext"]
        self.dead_letter_after = lifecycle["deadLetterNext"]
        waiters = runtime_request(
            "/internal/job-outbox/reconcile-waiters",
            {"schema": "runtime.job.waiters.reconcile.v1", "after": self.waiter_after},
        )
        self.waiter_after = waiter_page_next(waiters, {"reconciled"}, self.waiter_after)
        return {"runtimeJobs": runtime, "runLifecycle": lifecycle, "waiters": waiters}

def work_cursor(value):
    if value is None:
        return None
    if not isinstance(value, dict) or set(value) != {"insertedAt", "eventId"}:
        raise RuntimeError("agent_work_discovery_cursor_invalid")
    if not isinstance(value["eventId"], str) or not 1 <= len(value["eventId"]) <= 160 or not value["eventId"].strip():
        raise RuntimeError("agent_work_discovery_cursor_invalid")
    try:
        timestamp = datetime.fromisoformat(value["insertedAt"])
        if timestamp.utcoffset() is None:
            raise ValueError
    except (ValueError, TypeError):
        raise RuntimeError("agent_work_discovery_cursor_invalid") from None
    return timestamp, value["eventId"]


class WorkRequestReconciler:
    def __init__(self, stopped=None):
        self.after = self.through = None
        self.stopped = stopped or threading.Event()

    def __call__(self):
        if self.stopped.is_set():
            return
        deadline = time.monotonic() + RECONCILE_INTERVAL_SECONDS
        page = api_request("/internal/agent-work/discover",
            {"schema":"workspace.agent_work.discover.v1", "limit":CONTROL_PAGE_LIMIT,
             "after":self.after, "through":self.through}, "agent_work_discovery_unavailable",
            timeout=RECONCILE_INTERVAL_SECONDS)
        if not isinstance(page, dict) or set(page) != {"schema", "entries", "through", "next"} or page["schema"] != "workspace.agent_work.discovered.v1":
            raise RuntimeError("agent_work_discovery_invalid")
        entries = page["entries"]
        if not isinstance(entries, list) or len(entries) > CONTROL_PAGE_LIMIT:
            raise RuntimeError("agent_work_discovery_invalid")
        upper = work_cursor(page["through"])
        previous = work_cursor(self.after)
        if self.through is not None and work_cursor(self.through) != upper:
            raise RuntimeError("agent_work_discovery_cursor_invalid")
        for entry in entries:
            if not isinstance(entry, dict) or set(entry) != {"cursor", "sourceEventId"}:
                raise RuntimeError("agent_work_discovery_invalid")
            current = work_cursor(entry["cursor"])
            if current is None or upper is None or current > upper or previous is not None and current <= previous:
                raise RuntimeError("agent_work_discovery_cursor_invalid")
            if entry["sourceEventId"] is not None and entry["sourceEventId"] != entry["cursor"]["eventId"]:
                raise RuntimeError("agent_work_discovery_invalid")
            previous = current
        next_cursor = work_cursor(page["next"])
        if next_cursor is not None and (not entries or next_cursor != previous):
            raise RuntimeError("agent_work_discovery_cursor_invalid")
        self.through = page["through"]
        for entry in entries:
            remaining = deadline - time.monotonic()
            if remaining <= 0 or self.stopped.is_set():
                return
            # Advance every examined source, including errors/lost responses.
            # No unprocessed tail is carried in memory across control rounds.
            self.after = entry["cursor"]
            if entry["sourceEventId"] is not None:
                try:
                    api_request("/internal/agent-work/materialize",
                        {"schema":"workspace.agent_work.materialize.v1", "sourceEventId":entry["sourceEventId"]},
                        "agent_work_recovery_pending", timeout=min(10, remaining))
                except Exception as error:
                    print(f"work recovery pending: {type(error).__name__}", file=sys.stderr, flush=True)
        if page["next"] is None:
            self.after = self.through = None


def notice_cursor(value):
    if value is None:
        return None
    if not isinstance(value, str) or re.fullmatch(r"work-return:[a-f0-9]{64}", value) is None:
        raise RuntimeError("agent_work_consume_cursor_invalid")
    return value


class WorkReturnConsumer:
    """Accept notices once; the same scan binds retained pending acceptances."""
    def __init__(self, stopped=None):
        self.after = self.through = None
        self.stopped = stopped or threading.Event()

    def __call__(self):
        if self.stopped.is_set():
            return
        deadline = time.monotonic() + RECONCILE_INTERVAL_SECONDS
        page = api_request("/internal/agent-work/returns/consume/discover",
            {"schema": "workspace.agent_work.consume_discover.v1", "limit": CONTROL_PAGE_LIMIT,
             "after": self.after, "through": self.through}, "agent_work_consume_discovery_unavailable",
            timeout=RECONCILE_INTERVAL_SECONDS)
        if (not isinstance(page, dict) or set(page) != {"schema", "entries", "through", "next"}
                or page["schema"] != "workspace.agent_work.consume_discovered.v1"
                or not isinstance(page["entries"], list) or len(page["entries"]) > CONTROL_PAGE_LIMIT):
            raise RuntimeError("agent_work_consume_page_invalid")
        upper, next_cursor = notice_cursor(page["through"]), notice_cursor(page["next"])
        if self.through is not None and upper != self.through:
            raise RuntimeError("agent_work_consume_cursor_invalid")
        previous = self.after
        for entry in page["entries"]:
            if not isinstance(entry, dict) or set(entry) != {"cursor", "noticeId", "operationId"}:
                raise RuntimeError("agent_work_consume_page_invalid")
            current = notice_cursor(entry["cursor"])
            if (current is None or upper is None or current > upper
                    or previous is not None and current <= previous or entry["noticeId"] != current
                    or not isinstance(entry["operationId"], str)
                    or re.fullmatch(r"agent-work-consume:[a-f0-9]{64}", entry["operationId"]) is None):
                raise RuntimeError("agent_work_consume_page_invalid")
            previous = current
        if next_cursor is not None and (not page["entries"] or next_cursor != previous):
            raise RuntimeError("agent_work_consume_cursor_invalid")
        self.through = upper
        for entry in page["entries"]:
            remaining = deadline - time.monotonic()
            if remaining <= 0 or self.stopped.is_set():
                return
            # Denied and lost responses advance only this scanned row. Later
            # passes revisit unaccepted notices and accepted, unbound carriers.
            self.after = entry["cursor"]
            try:
                response = api_request("/internal/agent-work/returns/consume",
                    {"schema": "workspace.agent_work.consume.v1", "noticeId": entry["noticeId"],
                     "operationId": entry["operationId"]}, "agent_work_consume_pending", timeout=min(10, remaining))
                if (not isinstance(response, dict) or response.get("schema") != "workspace.agent_work.consumed.v1"
                        or response.get("noticeId") != entry["noticeId"]):
                    raise RuntimeError("agent_work_consume_response_invalid")
                if response.get("disposition") == "accepted":
                    operation = response.get("operation")
                    if (set(response) != {"schema", "disposition", "noticeId", "attemptId", "operation"}
                            or not isinstance(response["attemptId"], str) or not response["attemptId"].strip()
                            or not isinstance(operation, dict)
                            or not {"agentRunId", "turnId"} <= set(operation)):
                        raise RuntimeError("agent_work_consume_response_invalid")
                    run_id, turn_id = operation["agentRunId"], operation["turnId"]
                    if ((run_id is None) != (turn_id is None)
                            or run_id is not None and (not isinstance(run_id, str) or not run_id.strip()
                                or not isinstance(turn_id, str) or not turn_id.strip())):
                        raise RuntimeError("agent_work_consume_response_invalid")
                else:
                    raise RuntimeError("agent_work_consume_response_invalid")
            except Exception as error:
                print(f"work consume pending: {type(error).__name__}", file=sys.stderr, flush=True)
        if next_cursor is None:
            self.after = self.through = None


class WorkReturnAudit:
    """A shared, rate-limited repair page; Core owns per-work retries."""
    def __init__(self, stopped=None):
        self.stopped = stopped or threading.Event()

    def __call__(self):
        now = time.monotonic()
        if self.stopped.is_set():
            return
        deadline = now + CONTROL_HTTP_TIMEOUT_SECONDS
        work_deadline = now + CONTROL_HTTP_TIMEOUT_SECONDS * 0.8
        page = api_request("/internal/agent-work/returns/audit/claim",
            {"schema": "workspace.agent_work.return_audit.claim.v1", "limit": CONTROL_PAGE_LIMIT},
            "work_return_audit_unavailable", timeout=deadline - time.monotonic())
        if (not isinstance(page, dict) or page.get("schema") != "workspace.agent_work.return_audit.claimed.v1"
                or page.get("disposition") not in {"idle", "claimed"}):
            raise RuntimeError("work_return_audit_invalid")
        if page["disposition"] == "idle":
            if set(page) != {"schema", "disposition"}:
                raise RuntimeError("work_return_audit_invalid")
            return
        if (set(page) != {"schema", "disposition", "leaseOwner", "after", "through", "next", "entries"}
                or not valid_lease_owner(page["leaseOwner"])
                or not isinstance(page["entries"], list) or len(page["entries"]) > CONTROL_PAGE_LIMIT):
            raise RuntimeError("work_return_audit_invalid")
        after = page["after"]
        for cursor in (after, page["through"], page["next"]):
            if cursor is not None and (not isinstance(cursor, str) or not 1 <= len(cursor) <= 64):
                raise RuntimeError("work_return_audit_invalid")
        previous = after
        for entry in page["entries"]:
            if (not isinstance(entry, dict) or set(entry) != {"cursor", "workAgentRunId", "materialized"}
                    or not isinstance(entry["cursor"], str) or not 1 <= len(entry["cursor"]) <= 64
                    or not isinstance(entry["workAgentRunId"], str) or not 1 <= len(entry["workAgentRunId"]) <= 64
                    or type(entry["materialized"]) is not bool or page["through"] is None
                    or entry["cursor"] > page["through"] or previous is not None and entry["cursor"] <= previous):
                raise RuntimeError("work_return_audit_invalid")
            previous = entry["cursor"]
        if page["next"] is not None and (not page["entries"] or page["next"] != previous):
            raise RuntimeError("work_return_audit_invalid")
        for entry in page["entries"]:
            if self.stopped.is_set() or time.monotonic() >= work_deadline:
                break
            if not entry["materialized"]:
                try:
                    result = api_request("/internal/agent-work/returns/schedule",
                        {"schema": "workspace.agent_work.return_schedule.v1", "workAgentRunId": entry["workAgentRunId"]},
                        "work_return_schedule_unavailable", timeout=work_deadline - time.monotonic())
                    if (not isinstance(result, dict) or set(result) != {"schema", "disposition"}
                            or result["schema"] != "workspace.agent_work.return_scheduled.v1"
                            or result["disposition"] not in {"inserted", "existing", "delivered"}):
                        raise RuntimeError("work_return_schedule_invalid")
                except RuntimeError as error:
                    if (getattr(error, "http_status", None) == 409
                            and getattr(error, "http_error_code", None) == "agent_work_return_binding_rejected"):
                        # This exact rejection precedes enqueue. The source stays
                        # authoritative and is revisited on the next whole pass.
                        print(f"work return audit rejected: run={entry['workAgentRunId']} status=409",
                              file=sys.stderr, flush=True)
                    else:
                        # Authentication, unknown source and lost results cannot
                        # move past work whose durable schedule is unconfirmed.
                        print(f"work return audit pending: run={entry['workAgentRunId']} error={type(error).__name__}",
                              file=sys.stderr, flush=True)
                        break
            after = entry["cursor"]
        if self.stopped.is_set() or time.monotonic() >= deadline:
            return
        finished = api_request("/internal/agent-work/returns/audit/finish",
            {"schema": "workspace.agent_work.return_audit.finish.v1", "leaseOwner": page["leaseOwner"],
             "after": after, "complete": page["next"] is None and after == previous},
            "work_return_audit_finish_unavailable", timeout=deadline - time.monotonic())
        if finished != {"schema": "workspace.agent_work.return_audit.finished.v1", "disposition": "recorded"}:
            raise RuntimeError("work_return_audit_finish_invalid")


class WorkReturnReconciler:
    """Use the existing return control thread with an independent budget per pass."""
    def __init__(self, stopped=None):
        self.publisher = WorkReturnAudit(stopped)
        self.consumer = WorkReturnConsumer(stopped)

    def __call__(self):
        try:
            self.publisher()
        except Exception as error:
            print(f"work return delivery pending: {type(error).__name__}", file=sys.stderr, flush=True)
        self.consumer()


def run_loop(operation, interval_seconds, stopped=None):
    while stopped is None or not stopped.is_set():
        try:
            operation()
        except Exception as error:
            print(
                f"worker control loop failed: {type(error).__name__}",
                file=sys.stderr,
                flush=True,
            )
        if stopped is None:
            time.sleep(interval_seconds)
        else:
            stopped.wait(interval_seconds)


def run_job_loop(slot_index, stopped):
    with observations.context(slotIndex=slot_index):
        return _run_job_loop(slot_index, stopped)


def _run_job_loop(slot_index, stopped):
    while not stopped.is_set():
        try:
            worked = execute_next_job(slot_index)
            if not worked:
                wait_for_jobs()
        except Exception as error:
            print(
                f"worker job slot failed: slot={slot_index}; error={type(error).__name__}",
                file=sys.stderr,
                flush=True,
            )
            delay = 5 if str(error) in {"runtime_busy", "execution_claim_busy"} else JOB_WAIT_FAILURE_BACKOFF_SECONDS
            with observations.measure("backoff", requestedMs=delay * 1000,
                    route=getattr(error, "route", None),
                    errorCode=str(error) if str(error) in {"runtime_busy", "execution_claim_busy"} else "other"):
                stopped.wait(delay)


def run_worker_service():
    stopped = threading.Event()

    def stop_service(_signal_number, _frame):
        stopped.set()

    previous_handlers = {
        signal_number: signal.signal(signal_number, stop_service)
        for signal_number in (signal.SIGTERM, signal.SIGINT)
    }
    control_threads = [
        threading.Thread(
            target=run_loop,
            args=(TerminalDispatcher(), OUTBOX_POLL_INTERVAL_SECONDS, stopped),
            name="workspace-terminal-dispatcher",
        ),
        threading.Thread(
            target=run_loop,
            args=(LifecycleReconciler(), RECONCILE_INTERVAL_SECONDS, stopped),
            name="workspace-reconciler",
        ),
        threading.Thread(
            target=run_loop,
            args=(WorkRequestReconciler(stopped), RECONCILE_INTERVAL_SECONDS, stopped),
            name="workspace-work-reconciler",
        ),
        threading.Thread(
            target=run_loop,
            args=(WorkReturnReconciler(stopped), RECONCILE_INTERVAL_SECONDS, stopped),
            name="workspace-work-return-publisher",
        ),
    ]
    job_threads = [
        threading.Thread(
            target=run_job_loop,
            args=(slot_index, stopped),
            name=f"workspace-job-slot-{slot_index}",
            daemon=True,
        )
        for slot_index in range(WORKER_SLOT_COUNT)
    ]
    for thread in control_threads + job_threads:
        thread.start()
    try:
        stopped.wait()
    finally:
        stopped.set()
        for thread in control_threads:
            thread.join()
        for signal_number, handler in previous_handlers.items():
            signal.signal(signal_number, handler)


if __name__ == "__main__":
    command = sys.argv[1] if len(sys.argv) == 2 else ""
    if command == "serve":
        run_worker_service()
    else:
        raise SystemExit("usage: worker.py serve")
