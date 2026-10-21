import json
import hashlib
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
AGENT_RUN_EXECUTION_MODE = os.environ.get("AGENT_RUN_EXECUTION_MODE", "perRun")
if AGENT_RUN_EXECUTION_MODE not in {"perRun", "retained"}:
    raise ValueError("AGENT_RUN_EXECUTION_MODE must be perRun or retained")
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
    "execution_resident_capacity_wait",
    "execution_recovery_checkpoint_committed",
    "question_wait",
    "runtime_job_wait",
    "session_record_commit_unavailable",
    "session_workspace_commit_unavailable",
    "session_workspace_resolve_unavailable",
}
AGENT_RUN_SCHEDULED_WAIT_REASONS = {
    "execution_recovery_checkpoint_committed",
}


class RuntimeJobCancelled(RuntimeError):
    pass


class RuntimeLeaseLost(RuntimeError):
    def __init__(self):
        super().__init__("agent_run_lifecycle_lease_lost")


_lease_context = threading.local()


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


def runtime_request(path, body=None, *, timeout=None):
    return json_request(
        f"{RUNTIME_INTERNAL_URL}{path}",
        body,
        "X-Internal-Token",
        INTERNAL_API_TOKEN,
        "runtime_job_request_failed",
        timeout=(timeout if timeout is not None else
                 CONTROL_HTTP_TIMEOUT_SECONDS
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
        except (json.JSONDecodeError, AttributeError, http.client.IncompleteRead, OSError):
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
        http.client.IncompleteRead,
        OSError,
        json.JSONDecodeError,
    ) as error:
        raise DependencyUnavailable(default_reason) from error


def now_ms():
    return time.time_ns() // 1_000_000


def heartbeat(job_id, lease_owner, *, timeout=None):
    try:
        response = runtime_request(
            "/internal/jobs/heartbeat",
            {
                "schema": "runtime.job.heartbeat.v1",
                "jobId": job_id,
                "leaseOwner": lease_owner,
                "heartbeatAtMs": now_ms(),
                "leaseMs": LEASE_MS,
            },
            **({"timeout": timeout} if timeout is not None else {}),
        )
    except RuntimeError as error:
        # A confirmed 409 is authoritative loss. A second query must not
        # downgrade it into a temporary failure and revive this owner.
        if getattr(error, "http_status", None) == 409:
            raise RuntimeLeaseLost() from None
        raise
    if not isinstance(response, dict) or set(response) != {"ok"} or response["ok"] is not True:
        raise RuntimeError("runtime_job_heartbeat_response_invalid")


@contextmanager
def lease_heartbeats(job_id, lease_owner):
    if getattr(_lease_context, "guard", None) is not None:
        raise RuntimeError("worker_lease_guard_already_active")
    stopped = threading.Event()
    changed = threading.Condition()
    lease_seconds = LEASE_MS / 1000
    margin = min(1.0, lease_seconds / 10)
    state = {"deadline": 0.0, "status": "unconfirmed", "error": None, "attempt": 0, "renewing": False}
    owner_digest = hashlib.sha256(lease_owner.encode()).hexdigest()

    def observe(started, outcome, error=None):
        observations.emit("leaseHeartbeat", started, jobId=job_id,
            leaseOwnerSha256=owner_digest, attempt=state["attempt"], outcome=outcome,
            httpStatus=getattr(error, "http_status", None),
            errorCode=("temporary_unavailable" if isinstance(error, DependencyUnavailable)
                       else "fence_rejected" if isinstance(error, RuntimeLeaseLost)
                       else "invalid_response" if error is not None else None),
            confirmedRemainingMs=max(0, round((state["deadline"] - time.monotonic()) * 1000)))

    def lose(error=None):
        state["status"] = "lost"
        state["error"] = error if isinstance(error, RuntimeLeaseLost) else RuntimeLeaseLost()
        changed.notify_all()

    def attempt(initial=False):
        started = time.monotonic()
        with changed:
            if stopped.is_set() or state["status"] == "lost":
                return False
            if not initial and started >= state["deadline"]:
                lose()
                return False
            state["attempt"] += 1
            previous_deadline = None if initial else state["deadline"]
            budget = lease_seconds - margin if initial else state["deadline"] - started
            state["renewing"] = True
        try:
            try:
                heartbeat(job_id, lease_owner, timeout=min(CONTROL_HTTP_TIMEOUT_SECONDS, budget))
            except DependencyUnavailable as error:
                with changed:
                    if initial:
                        # No successfully confirmed window exists yet: dispatch is forbidden.
                        observe(started, "unconfirmed", error)
                        raise DependencyUnavailable("worker_heartbeat_unconfirmed", http_status=error.http_status) from None
                    if time.monotonic() >= state["deadline"]:
                        lose()
                        observe(started, "lost", error)
                    else:
                        state["status"] = "unconfirmed"
                        changed.notify_all()
                        observe(started, "unconfirmed", error)
                    return state["status"] != "lost"
            except Exception as error:
                with changed:
                    lose()
                    observe(started, "lost", error)
                return False
            with changed:
                if state["status"] == "lost":
                    return False
                completed = time.monotonic()
                if previous_deadline is not None and completed >= previous_deadline:
                    lose()
                    observe(started, "lost")
                    return False
                # The server renews for exactly leaseMs after obtaining its row lock.
                # Request-start + duration is a conservative lower bound. ACK arrival
                # and local wall-clock changes never grant an extra validity window.
                deadline = started + lease_seconds - margin
                if completed >= deadline:
                    lose()
                    observe(started, "lost")
                    return False
                state.update(deadline=deadline, status="healthy", error=None)
                changed.notify_all()
                observe(started, "renewed")
                return True
        finally:
            with changed:
                state["renewing"] = False
                changed.notify_all()

    def require_healthy_lease():
        with changed:
            while True:
                remaining = state["deadline"] - time.monotonic()
                if state["status"] in {"lost", "graceful_stop"} or remaining <= 0:
                    lose()
                    raise state["error"]
                if state["status"] == "healthy":
                    return
                # Only the single renewal thread can verify an uncertain ACK.
                # Callers cannot dispatch new work while that verification runs.
                changed.wait(remaining)

    if not attempt(initial=True):
        require_healthy_lease()

    def renew():
        while True:
            with changed:
                remaining = state["deadline"] - time.monotonic()
                delay = (min(0.25, max(0, remaining / 4)) if state["status"] == "unconfirmed"
                         else min(max(1, LEASE_MS // 3000), max(0, remaining / 2)))
            if stopped.wait(delay):
                return
            if not attempt():
                return

    thread = threading.Thread(target=renew, daemon=True)
    thread.start()

    def terminal_handoff():
        # Only the final complete/yield RPC uses this. Teardown, release and
        # maintenance retain their renewal guard for their entire duration.
        with changed:
            while True:
                remaining = state["deadline"] - time.monotonic()
                if state["status"] in {"lost", "graceful_stop"} or remaining <= 0:
                    lose()
                    raise state["error"]
                if state["status"] == "healthy" and not state["renewing"]:
                    # Only a confirmed, quiescent owner can stop its verifier.
                    # attempt() checks stopped under this same Condition lock.
                    stopped.set()
                    break
                changed.wait(remaining)
        thread.join()
        with changed:
            if state["status"] != "healthy" or time.monotonic() >= state["deadline"]:
                lose()
                raise state["error"]
            state["status"] = "graceful_stop"
            changed.notify_all()
            observe(time.monotonic(), "graceful_stop")

    _lease_context.guard = (job_id, lease_owner, terminal_handoff)
    try:
        yield require_healthy_lease
    finally:
        stopped.set()
        thread.join()
        with changed:
            if state["status"] not in {"lost", "graceful_stop"}:
                state["status"] = "graceful_stop"
                changed.notify_all()
                observe(time.monotonic(), "graceful_stop")
        _lease_context.guard = None


def _lease_terminal_handoff(job_id, lease_owner):
    guard = getattr(_lease_context, "guard", None)
    if guard is not None:
        if guard[:2] != (job_id, lease_owner):
            raise RuntimeError("worker_lease_guard_identity_mismatch")
        guard[2]()


def complete_job(job_id, lease_owner, output_refs):
    _lease_terminal_handoff(job_id, lease_owner)
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
    _lease_terminal_handoff(job_id, lease_owner)
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
    resolved = resolve_agent_run_lifecycle(job, lease_owner, agent_run_id, digest)
    if resolved["disposition"] in {"terminal", "released"}:
        require_healthy_lease()
        finish_environment_agent_run(job, lease_owner, agent_run_id, digest, resolved, require_healthy_lease)
        return False
    agent_run_start = resolved.get("agentRunStart")
    if (
        not isinstance(agent_run_start, dict)
        or agent_run_start.get("agentRunId") != agent_run_id
        or agent_run_start.get("authorizationDigest") != digest
        or agent_run_start.get("schema") != "workspace.agent_run.start.v1"
        or environment_controller_purpose(agent_run_start) != "execute"
    ):
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
            | ({"retryAtMs"} if result.get("transitionReason") in AGENT_RUN_SCHEDULED_WAIT_REASONS else set()))
        or result.get("schema") != "runtime.agent_run.step.result.v1"
        or result.get("agentRunId") != agent_run_id
        or result.get("disposition") not in {"waiting", "terminal"}
        or not isinstance(result.get("transitionReason"), str)
        or not result["transitionReason"]
    ):
        raise RuntimeError("agent_run_step_response_invalid")
    if result["transitionReason"] in AGENT_RUN_SCHEDULED_WAIT_REASONS and (
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
        if result["transitionReason"] in AGENT_RUN_SCHEDULED_WAIT_REASONS:
            # Runtime derives this deadline from its recovery boundary.
            # Waiting releases the slot; retries never sleep inside a worker.
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
    transition_agent_run(agent_run_id, result["terminalState"], "runtime_session_terminal_committed")
    resolved = resolve_agent_run_lifecycle(job, lease_owner, agent_run_id, digest)
    if resolved["disposition"] not in {"terminal", "released"} or resolved["terminalState"] != result["terminalState"]:
        raise RuntimeError("agent_run_lifecycle_resolve_response_invalid")
    finish_environment_agent_run(job, lease_owner, agent_run_id, digest, resolved, require_healthy_lease)
    return False


def environment_controller_purpose(envelope):
    authorization = envelope.get("environmentAuthorization")
    if not isinstance(authorization, dict):
        return None
    payload = authorization.get("payload")
    return payload.get("controllerPurpose") if isinstance(payload, dict) else None


def resolve_agent_run_lifecycle(job, lease_owner, agent_run_id, digest):
    resolved = api_request(
        "/internal/agent-run-lifecycle/resolve",
        {
            "schema": "runtime.agent_run_lifecycle.resolve.v1",
            "jobId": job["jobId"],
            "agentRunId": agent_run_id,
            "authorizationDigest": digest,
            "leaseOwner": lease_owner, "executionMode": AGENT_RUN_EXECUTION_MODE,
        },
        "agent_run_start_unavailable",
    )
    if (
        not isinstance(resolved, dict)
        or resolved.get("schema") != "runtime.agent_run_lifecycle.resolved.v1"
        or resolved.get("disposition") not in {"ready", "terminal", "released"}
    ):
        raise RuntimeError("agent_run_lifecycle_resolve_response_invalid")
    disposition = resolved["disposition"]
    expected = {"schema", "disposition"}
    if disposition == "ready":
        expected.add("agentRunStart")
    else:
        expected |= {"terminalState", "releaseReceiptId" if disposition == "released" else "agentRunCleanup"}
        if resolved.get("terminalState") not in {"completed", "failed", "cancelled"}:
            raise RuntimeError("agent_run_lifecycle_resolve_response_invalid")
    if set(resolved) != expected or (disposition == "released" and not valid_runtime_job_id(resolved["releaseReceiptId"])):
        raise RuntimeError("agent_run_lifecycle_resolve_response_invalid")
    return resolved


class RetainedCleanupPending(RuntimeError):
    """Cleanup is unconfirmed; retain the original terminal and retry this job."""


def finish_environment_agent_run(job, lease_owner, run_id, digest, resolved, require_healthy_lease):
    try:
        return _finish_environment_agent_run(job, lease_owner, run_id, digest, resolved, require_healthy_lease)
    except (RuntimeError, urllib.error.URLError, OSError) as error:
        if str(error) == "agent_run_lifecycle_lease_lost":
            raise
        # Never include untrusted response bodies or transport credentials.
        reason = str(error) if isinstance(error, RuntimeError) else "retained_cleanup_unavailable"
        raise RetainedCleanupPending(reason) from error


def _finish_environment_agent_run(job, lease_owner, run_id, digest, resolved, require_healthy_lease):
    if resolved["disposition"] == "terminal":
        cleanup = resolved["agentRunCleanup"]
        if (not isinstance(cleanup, dict) or set(cleanup) != {"schema", "agentRunId", "authorizationDigest",
                "authorizationSignature", "authorization", "environmentAuthorization"}
                or cleanup["schema"] != "workspace.agent_run.cleanup.v1"
                or cleanup["agentRunId"] != run_id or cleanup["authorizationDigest"] != digest
                or not isinstance(cleanup["environmentAuthorization"], dict)):
            raise RuntimeError("agent_run_lifecycle_resolve_response_invalid")
        grant = cleanup["environmentAuthorization"].get("payload")
        if (not isinstance(grant, dict) or grant.get("controllerPurpose") != "cleanup"
                or type(grant.get("controllerEpoch")) is not int or not 1 <= grant["controllerEpoch"] <= 2**63 - 1):
            raise RuntimeError("agent_run_lifecycle_resolve_response_invalid")
        require_healthy_lease()
        result = runtime_teardown_request({'schema': 'runtime.agent_run.teardown.v1',
            'jobId': job['jobId'], 'leaseOwner': lease_owner, 'agentRunCleanup': cleanup})
        if (not isinstance(result, dict) or set(result) != {'schema', 'agentRunId', 'status', 'cleanupReceiptId'}
                or result['schema'] != 'runtime.agent_run.teardown.result.v1' or result['agentRunId'] != run_id
                or result['status'] != 'cleanupConfirmed' or not valid_runtime_job_id(result['cleanupReceiptId'])):
            raise RuntimeError("runtime_teardown_response_invalid")
        require_healthy_lease()
        release = api_request('/internal/agent-run-lifecycle/release',
            {'schema': 'runtime.agent_run_lifecycle.release.v1', 'jobId': job['jobId'],
                'leaseOwner': lease_owner, 'agentRunId': run_id, 'authorizationDigest': digest,
                'controllerEpoch': grant['controllerEpoch'], 'cleanupReceiptId': result['cleanupReceiptId']},
            'agent_run_release_unavailable')
        if (not isinstance(release, dict) or set(release) != {'schema', 'agentRunId', 'releaseReceiptId'}
                or release['schema'] != 'runtime.agent_run_lifecycle.released.v1' or release['agentRunId'] != run_id
                or not valid_runtime_job_id(release['releaseReceiptId'])):
            raise RuntimeError('agent_run_release_response_invalid')
    require_healthy_lease()
    complete_job(job['jobId'], lease_owner, [])


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
    except RuntimeLeaseLost:
        print(f"worker lease stopped: jobId={job['jobId']}; transitionReason=lease_lost", flush=True)
        return
    except RuntimeJobCancelled as error:
        print(
            f"worker job stopped: jobId={job['jobId']}; transitionReason={error}",
            flush=True,
        )
    except RetainedCleanupPending:
        # The job state machine records retry/deadletter, but cleanup failure
        # cannot synthesize a different Core terminal or release residency.
        fail_job(job, lease_owner, "retained_cleanup_pending", True)
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
        self.cleanup_after = None
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
        cleanup_pending = api_request('/internal/agent-run-lifecycle/cleanup-reconcile',
            {'schema': 'runtime.agent_run_cleanup.reconcile.v1', 'limit': CONTROL_PAGE_LIMIT,
                'after': self.cleanup_after}, 'agent_run_cleanup_reconcile_unavailable')
        if (not isinstance(cleanup_pending, dict) or set(cleanup_pending) != {'schema', 'checked', 'pending', 'next'}
                or cleanup_pending['schema'] != 'runtime.agent_run_cleanup.reconciled.v1'
                or type(cleanup_pending['checked']) is not int or not 0 <= cleanup_pending['checked'] <= CONTROL_PAGE_LIMIT
                or not isinstance(cleanup_pending['pending'], list)
                or len(cleanup_pending['pending']) > cleanup_pending['checked']):
            raise RuntimeError('agent_run_cleanup_reconcile_response_invalid')
        for entry in cleanup_pending['pending']:
            if (not isinstance(entry, dict) or set(entry) != {'agentRunId', 'authorizationDigest', 'terminalState', 'jobStatus'}
                    or not valid_runtime_job_id(entry['agentRunId'])
                    or not isinstance(entry['authorizationDigest'], str) or not re.fullmatch(r'sha256:[0-9a-f]{64}', entry['authorizationDigest'])
                    or entry['terminalState'] not in {'completed', 'failed', 'cancelled'}
                    or entry['jobStatus'] not in {'queued', 'leased', 'running', 'succeeded', 'failed', 'dead_lettered', 'cancelled', 'missing', 'unknown'}):
                raise RuntimeError('agent_run_cleanup_reconcile_response_invalid')
        self.cleanup_after = cleanup_pending['next']
        waiters = runtime_request(
            "/internal/job-outbox/reconcile-waiters",
            {"schema": "runtime.job.waiters.reconcile.v1", "after": self.waiter_after},
        )
        self.waiter_after = waiter_page_next(waiters, {"reconciled"}, self.waiter_after)
        return {"runtimeJobs": runtime, "runLifecycle": lifecycle, "waiters": waiters,
            **({'cleanupPending': cleanup_pending} if cleanup_pending is not None else {})}

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


def claim_control_job(stage, worker_id):
    if stage not in {"advance", "cleanup"}:
        raise RuntimeError("control_stage_invalid")
    response = runtime_request("/internal/jobs/control/claim", {
        "schema": "runtime.job.control.claim.v1", "workerId": worker_id,
        "jobId": None, "jobKind": "agent_run.lifecycle", "nowMs": now_ms(),
        "leaseMs": LEASE_MS, "limit": 1, "stage": stage,
    })
    if not isinstance(response, dict) or set(response) != {"jobs"}:
        raise RuntimeError("control_claim_response_invalid")
    jobs = response["jobs"]
    if not isinstance(jobs, list) or len(jobs) > 1:
        raise RuntimeError("control_claim_response_invalid")
    if not jobs:
        return None
    job = jobs[0]
    if (not isinstance(job, dict) or job.get("jobKind") != "agent_run.lifecycle"
            or job.get("status") != "leased" or job.get("leasePurpose") != "control"
            or job.get("controlStage") != stage or not valid_runtime_job_id(job.get("jobId"))
            or not valid_lease_owner(job.get("leaseOwner"))):
        raise RuntimeError("control_claim_response_invalid")
    agent_run_lifecycle_binding(job)
    return job


def execute_control_job(job):
    run_id, digest = agent_run_lifecycle_binding(job)
    owner = job["leaseOwner"]
    try:
        runtime_request("/internal/jobs/control/start", {
            "schema": "runtime.job.control.start.v1", "jobId": job["jobId"],
            "leaseOwner": owner, "atMs": now_ms(),
        })
        with lease_heartbeats(job["jobId"], owner) as healthy:
            resolved = api_request("/internal/agent-run-lifecycle/control", {
                "schema": "runtime.agent_run_lifecycle.control.resolve.v1",
                "jobId": job["jobId"], "leaseOwner": owner,
                "agentRunId": run_id, "authorizationDigest": digest,
            }, "agent_run_control_unavailable")
            if (not isinstance(resolved, dict)
                    or set(resolved) != {"schema", "agentRunControl"}
                    or resolved["schema"] != "runtime.agent_run_lifecycle.control.resolved.v1"
                    or not isinstance(resolved["agentRunControl"], dict)
                    or resolved["agentRunControl"].get("agentRunId") != run_id):
                raise RuntimeError("agent_run_control_response_invalid")
            healthy()
            result = runtime_request("/agent-runs/control", {
                "schema": "runtime.agent_run.control.v1", "jobId": job["jobId"],
                "leaseOwner": owner, "agentRunControl": resolved["agentRunControl"],
            })
            if (not isinstance(result, dict)
                    or result.get("schema") != "runtime.agent_run.control.result.v1"
                    or result.get("agentRunId") != run_id
                    or result.get("terminalState") not in {"completed", "failed", "cancelled"}
                    or result.get("disposition") not in {"terminal", "released"}):
                raise RuntimeError("agent_run_control_response_invalid")
            healthy()
            transition_agent_run(run_id, result["terminalState"], "runtime_session_terminal_committed")
            if result["disposition"] == "released":
                if (set(result) != {"schema", "agentRunId", "terminalState", "disposition",
                                   "cleanupReceiptId", "releaseReceiptId"}
                        or not valid_runtime_job_id(result["cleanupReceiptId"])
                        or not valid_runtime_job_id(result["releaseReceiptId"])):
                    raise RuntimeError("agent_run_control_response_invalid")
                complete_job(job["jobId"], owner, [])
            else:
                if set(result) != {"schema", "agentRunId", "terminalState", "disposition"}:
                    raise RuntimeError("agent_run_control_response_invalid")
                # The same Job yields before the bounded slow-cleanup workers claim it.
                # Its authoritative terminal fact determines that lane's eligibility.
                if job["controlStage"] == "cleanup":
                    resolved = resolve_agent_run_lifecycle(job, owner, run_id, digest)
                    if resolved["disposition"] not in {"terminal", "released"}:
                        raise RuntimeError("control_execution_forbidden")
                    finish_environment_agent_run(job, owner, run_id, digest, resolved, healthy)
                else:
                    yield_job(job["jobId"], owner, now_ms(), "control_cleanup_pending")
    except RuntimeLeaseLost:
        return
    except (RuntimeError, urllib.error.URLError, OSError):
        # Control failures never manufacture a new business terminal or dead-letter
        # away the obligation. A later current lease retries the same Job.
        try:
            yield_job(job["jobId"], owner, now_ms() + 1000, "control_retry_pending")
        except (RuntimeError, urllib.error.URLError, OSError):
            # The persisted lease and durable intent survive an unavailable reply.
            return


def run_control_loop(stage, slot, stopped):
    worker_id = "worker:control:" + stage + ":" + str(slot) + ":" + uuid.uuid4().hex
    while not stopped.is_set():
        try:
            job = claim_control_job(stage, worker_id)
            if job is not None:
                execute_control_job(job)
                continue
        except (RuntimeError, urllib.error.URLError, OSError):
            pass
        stopped.wait(0.25)


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
    lifecycle_threads = [
        threading.Thread(target=run_control_loop, args=(stage, slot, stopped),
                         name=f"workspace-control-{stage}-{slot}", daemon=True)
        for stage in ("advance", "cleanup") for slot in range(2)
    ]
    for thread in control_threads + job_threads + lifecycle_threads:
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
