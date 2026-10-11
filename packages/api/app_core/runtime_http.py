"""Runtime JSON transport; callers own error policy and response validation."""
import json
import urllib.request

from django.conf import settings


def request_json(path: str, body: dict | None = None, *, timeout: float):
    headers = {"X-Internal-Token": settings.INTERNAL_API_TOKEN}
    if body is not None:
        headers["Content-Type"] = "application/json"
    request = urllib.request.Request(
        f"{settings.RUNTIME_URL.rstrip('/')}{path}",
        data=None if body is None else json.dumps(body, separators=(",", ":")).encode(),
        headers=headers,
        method="GET" if body is None else "POST",
    )
    with urllib.request.urlopen(request, timeout=timeout) as response:
        return json.loads(response.read())
