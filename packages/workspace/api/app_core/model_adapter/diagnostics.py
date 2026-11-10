"""Keep exception identity and code locations without logging provider/user data."""

import hashlib
import json
import logging
from pathlib import Path
import traceback

from .common import safe_model_error_reason

logger = logging.getLogger(__name__)


def report_model_failure(agent_run_id, model_config_ref, stage, error):
    exceptions = []
    seen = set()
    cause = error
    while cause is not None and id(cause) not in seen and len(exceptions) < 4:
        seen.add(id(cause))
        exceptions.append({
            "type": type(cause).__name__,
            "module": type(cause).__module__,
            "messageDigest": hashlib.sha256(str(cause).encode("utf-8", errors="replace")).hexdigest(),
            "frames": [
                {"file": Path(frame.filename).name, "function": frame.name, "line": frame.lineno}
                for frame in traceback.extract_tb(cause.__traceback__)[-12:]
            ],
        })
        cause = cause.__cause__ or (None if cause.__suppress_context__ else cause.__context__)
    logger.error(json.dumps({
        "event": "model_request_failed",
        "agentRunId": agent_run_id,
        "modelConfigRef": model_config_ref,
        "stage": stage,
        "reasonType": safe_model_error_reason(error),
        "exceptions": exceptions,
    }, separators=(",", ":")))
