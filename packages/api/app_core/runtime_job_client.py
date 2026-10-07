import json
import urllib.error
import urllib.request

from django.conf import settings


def get_runtime_job(job_id: str) -> dict | None:
    request = urllib.request.Request(
        f"{settings.RUNTIME_URL.rstrip('/')}/internal/jobs/{job_id}",
        headers={"X-Internal-Token": settings.INTERNAL_API_TOKEN},
        method="GET",
    )
    try:
        with urllib.request.urlopen(request, timeout=settings.RUNTIME_CONTROL_TIMEOUT_SECONDS) as response:
            body = json.loads(response.read())
    except urllib.error.HTTPError as error:
        error.close()
        if error.code == 404:
            return None
        raise RuntimeError("runtime_job_query_failed") from error
    except (urllib.error.URLError, TimeoutError, json.JSONDecodeError) as error:
        raise RuntimeError("runtime_job_query_failed") from error
    if set(body) != {"job"} or not isinstance(body["job"], dict):
        raise RuntimeError("runtime_job_response_invalid")
    return body["job"]


def schedule_runtime_job(payload: dict) -> dict:
    request = urllib.request.Request(
        f"{settings.RUNTIME_URL.rstrip('/')}/internal/jobs/schedule",
        data=json.dumps(payload, separators=(",", ":")).encode(),
        headers={"Content-Type": "application/json", "X-Internal-Token": settings.INTERNAL_API_TOKEN},
        method="POST",
    )
    try:
        with urllib.request.urlopen(request, timeout=settings.RUNTIME_START_TIMEOUT_SECONDS) as response:
            body = json.loads(response.read())
    except (urllib.error.HTTPError, urllib.error.URLError, json.JSONDecodeError) as error:
        if isinstance(error, urllib.error.HTTPError):
            error.close()
        raise RuntimeError("runtime_job_schedule_failed") from error
    if set(body) != {"disposition", "job"} or body["disposition"] not in {"inserted", "existing"}:
        raise RuntimeError("runtime_job_schedule_response_invalid")
    if not isinstance(body["job"], dict) or body["job"].get("jobId") != payload.get("jobId"):
        raise RuntimeError("runtime_job_schedule_binding_mismatch")
    return body
