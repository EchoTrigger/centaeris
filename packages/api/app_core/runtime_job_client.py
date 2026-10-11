import json
import urllib.error

from django.conf import settings

from .runtime_http import request_json


def get_runtime_job(job_id: str) -> dict | None:
    try:
        body = request_json(
            f'/internal/jobs/{job_id}',
            timeout=settings.RUNTIME_CONTROL_TIMEOUT_SECONDS,
        )
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


def schedule_runtime_job(payload: dict, *, timeout=None) -> dict:
    try:
        body = request_json(
            '/internal/jobs/schedule',
            payload,
            timeout=settings.RUNTIME_START_TIMEOUT_SECONDS if timeout is None else timeout,
        )
    except (urllib.error.HTTPError, urllib.error.URLError, json.JSONDecodeError) as error:
        if isinstance(error, urllib.error.HTTPError):
            error.close()
        raise RuntimeError("runtime_job_schedule_failed") from error
    if set(body) != {"disposition", "job"} or body["disposition"] not in {"inserted", "existing"}:
        raise RuntimeError("runtime_job_schedule_response_invalid")
    if not isinstance(body["job"], dict) or body["job"].get("jobId") != payload.get("jobId"):
        raise RuntimeError("runtime_job_schedule_binding_mismatch")
    return body
