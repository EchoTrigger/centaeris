from ninja import Router, Status

from app_core.models import Session
from app_core.runtime_job_client import get_runtime_job
from app_core.workspace_access import workspace_membership_for

from .response_schema import COMMON_ERROR_RESPONSES, RuntimeJobEnvelope
from .security import session_auth


router = Router(tags=["runtime-jobs"], by_alias=True)


@router.get(
    "/jobs/{job_id}",
    auth=session_auth,
    response={200: RuntimeJobEnvelope} | COMMON_ERROR_RESPONSES,
)
def runtime_job_status(request, job_id: str):
    try:
        job = _require_authorized_runtime_job(request.user, job_id)
    except RuntimeError:
        return Status(503, {"error": "job_status_unavailable"})
    except LookupError:
        return Status(404, {"error": "job_not_found"})
    try:
        return {"job": _serialize_runtime_job(job)}
    except ValueError:
        return Status(409, {"error": "job_status_invalid"})


def _require_authorized_runtime_job(user, job_id: str) -> dict:
    job = get_runtime_job(job_id)
    if job is None:
        raise LookupError("job_not_found")
    session_id = job.get("sessionId")
    session = Session.objects.filter(id=session_id, owner=user).first()
    if session is None or workspace_membership_for(user, session.workspace_id) is None:
        raise LookupError("job_not_found")
    return job


def _serialize_runtime_job(job: dict) -> dict:
    job_id = str(job.get("jobId", ""))
    try:
        if not job_id.startswith("job_") or len(job_id) > 160:
            raise ValueError
    except (TypeError, AttributeError) as error:
        raise ValueError("job id invalid") from error
    status = str(job.get("status", ""))
    topics = {
        "queued": "等待处理",
        "leased": "准备处理",
        "running": "正在处理",
        "succeeded": "处理完成",
        "failed": "处理失败",
        "dead_lettered": "需要管理员处理",
        "cancelled": "已取消",
    }
    if status not in topics:
        raise ValueError("job status invalid")
    result_refs = [
        {"kind": "artifact", "id": value.removeprefix("artifact:")}
        for value in job.get("outputRefs", [])
        if isinstance(value, str)
        and value.startswith("artifact:")
        and len(value) <= 160
    ]
    raw_error = job.get("lastError")
    safe_error = (
        raw_error
        if isinstance(raw_error, str)
        and 0 < len(raw_error) <= 160
        and all(
            character.isascii()
            and (
                character.islower()
                or character.isdigit()
                or character in "_-.:"
            )
            for character in raw_error
        )
        else "job_failed"
    )
    return {
        "id": job_id,
        "status": status,
        "progressTopic": topics[status],
        "resultRefs": result_refs,
        "error": safe_error if status in {"failed", "dead_lettered"} else None,
    }
