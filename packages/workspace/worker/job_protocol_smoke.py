import json
import time
import urllib.request
import uuid

import worker


RUN_SUFFIX = uuid.uuid4().hex[:12]
JOB_ID = f"job_lease_recovery_{RUN_SUFFIX}"


def get_job(job_id):
    request = urllib.request.Request(
        f"{worker.RUNTIME_INTERNAL_URL}/internal/jobs/{job_id}",
        headers={"X-Internal-Token": worker.INTERNAL_API_TOKEN},
        method="GET",
    )
    with urllib.request.urlopen(request, timeout=10) as response:
        return json.loads(response.read())["job"]


def expect_rejected(path, body):
    try:
        worker.runtime_request(path, body)
    except RuntimeError:
        return
    raise AssertionError(f"stale transition unexpectedly succeeded: {path}")


def main():
    schedule = {
        "schema": "runtime.job.schedule.v1",
        "jobId": JOB_ID,
        "jobKind": "worker.noop",
        "runAtMs": 0,
        "maxRetries": 0,
        "idempotencyKey": f"lease-recovery:{RUN_SUFFIX}",
        "sessionId": None,
        "payloadRef": None,
    }
    first = worker.runtime_request("/internal/jobs/schedule", schedule)
    duplicate = worker.runtime_request(
        "/internal/jobs/schedule", {**schedule, "jobId": f"job_lease_duplicate_{RUN_SUFFIX}"}
    )
    assert first["job"]["jobId"] == JOB_ID
    assert duplicate["disposition"] == "existing" and duplicate["job"]["jobId"] == JOB_ID

    worker_id = "worker:lease-recovery"
    claimed = worker.runtime_request("/internal/jobs/claim", {
        "schema": "runtime.job.claim.v1", "workerId": worker_id, "jobId": JOB_ID,
        "jobKind": "worker.noop",
        "nowMs": worker.now_ms(), "leaseMs": 1000, "limit": 1,
    })["jobs"]
    assert claimed[0]["jobId"] == JOB_ID
    old_owner = claimed[0]["leaseOwner"]
    worker.runtime_request("/internal/jobs/start", {
        "schema": "runtime.job.start.v1", "jobId": JOB_ID, "leaseOwner": old_owner, "atMs": worker.now_ms(),
    })
    time.sleep(1.05)
    worker.runtime_request("/internal/jobs/reconcile", {
        "schema": "runtime.job.reconcile.v1", "nowMs": worker.now_ms(),
    })
    reclaimed = worker.runtime_request("/internal/jobs/claim", {
        "schema": "runtime.job.claim.v1", "workerId": worker_id, "jobId": JOB_ID,
        "jobKind": "worker.noop",
        "nowMs": worker.now_ms(), "leaseMs": 1000, "limit": 1,
    })["jobs"]
    assert reclaimed[0]["jobId"] == JOB_ID
    new_owner = reclaimed[0]["leaseOwner"]
    assert new_owner != old_owner
    expect_rejected("/internal/jobs/heartbeat", {
        "schema": "runtime.job.heartbeat.v1", "jobId": JOB_ID, "leaseOwner": old_owner,
        "heartbeatAtMs": worker.now_ms(), "leaseMs": 1000,
    })
    expect_rejected("/internal/jobs/complete", {
        "schema": "runtime.job.complete.v1", "jobId": JOB_ID, "leaseOwner": old_owner,
        "completedAtMs": worker.now_ms(), "outputRefs": [],
    })
    worker.runtime_request("/internal/jobs/complete", {
        "schema": "runtime.job.complete.v1", "jobId": JOB_ID, "leaseOwner": new_owner,
        "completedAtMs": worker.now_ms(), "outputRefs": [],
    })
    assert get_job(JOB_ID)["status"] == "succeeded"
    expect_rejected("/internal/jobs/start", {
        "schema": "runtime.job.start.v1", "jobId": JOB_ID, "leaseOwner": new_owner,
        "atMs": worker.now_ms(),
    })
    print(json.dumps({
        "jobId": JOB_ID,
        "idempotent": True,
        "staleWorkerRejected": True,
        "terminalStartRejected": True,
        "status": "succeeded",
    }))


if __name__ == "__main__":
    main()
