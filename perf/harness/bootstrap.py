"""B1 引导脚本：从零登录到一次 mock 支撑的完整 AgentRun。

流程：CSRF → 登录 → 建 provider(mock) → 建模型(perf-instant) →
     找 workspace/agent → 发消息(sessionId=new) → 订阅 SSE 到终态。

用法：python bootstrap.py
环境变量：API_BASE（默认 http://localhost:18000）
每步失败即打印响应体并退出 1。
"""

import json
import os
import sys
import subprocess
import urllib.error
import urllib.request
import uuid

API = os.environ.get("API_BASE", "http://localhost:18000")
EMAIL = os.environ.get("PERF_ADMIN_EMAIL", "perf-admin@localhost.invalid")
PASSWORD = os.environ.get("PERF_ADMIN_PASSWORD")
MOCK_API_BASE = "https://mock-model:9999/v1"
PERF_API_CONTAINER = "centaeris-perf-api-1"

# 手动 cookie 管理：Django 下发的 Secure cookie，urllib 的 cookiejar
# 拒绝在 http 上回发（浏览器对 localhost 有豁免，cookiejar 没有）。
cookies: dict[str, str] = {}


def cookie_header() -> str:
    return "; ".join(f"{name}={value}" for name, value in cookies.items())


def call(method: str, path: str, payload=None, csrf: str | None = None, timeout: int = 30):
    body = json.dumps(payload).encode() if payload is not None else None
    request = urllib.request.Request(API + path, data=body, method=method)
    if body:
        request.add_header("content-type", "application/json")
    if cookies:
        request.add_header("Cookie", cookie_header())
    if csrf:
        request.add_header("X-CSRFToken", csrf)
    try:
        response = urllib.request.urlopen(request, timeout=timeout)
    except urllib.error.HTTPError as error:
        response = error
    for header in response.headers.get_all("Set-Cookie") or []:
        pair = header.split(";", 1)[0]
        if "=" in pair:
            name, value = pair.split("=", 1)
            cookies[name.strip()] = value.strip()
    return response.status, json.loads(response.read() or b"{}")


def step(name: str, status: int, body, ok=(200, 201, 202)):
    if status not in ok:
        print(f"FAIL {name}: {status} {json.dumps(body)[:400]}")
        sys.exit(1)
    print(f"ok   {name}: {status}")
    return body


def read_terminal(stream):
    for raw in stream:
        line = raw.decode("utf-8").strip()
        if not line.startswith("data:"):
            continue
        item = json.loads(line[5:].strip())
        event_type = (item.get("event") or item).get("type")
        if event_type in {"agent_run_completed", "agent_run_failed", "agent_run_interrupted"}:
            return event_type
    raise RuntimeError("Event stream ended without a terminal event")


def run_bounded_smoke():
    # A process deadline also bounds a continuously active nonterminal stream.
    return subprocess.run([sys.executable, __file__, "--smoke-child"], timeout=180).returncode


def bind_mock_quota(provider_id: str) -> None:
    # Hosted model attempts require an explicit credential-to-quota binding.
    subprocess.run(
        ["docker", "exec", PERF_API_CONTAINER, "python", "manage.py",
         "configure_model_quota", "--domain-id", f"perf-smoke-{provider_id}",
         "--max-concurrent", "1", "--provider-id", provider_id, "--enable"],
        check=True, timeout=15, capture_output=True, text=True,
    )


def main():
    if not PASSWORD:
        print("需要 PERF_ADMIN_PASSWORD（与 .env 的 BOOTSTRAP_SUPERADMIN_PASSWORD 一致）")
        sys.exit(1)

    status, body = call("GET", "/api/csrf")
    csrf = step("csrf", status, body)["csrfToken"]

    status, body = call("POST", "/api/login", {"email": EMAIL, "password": PASSWORD}, csrf)
    user = step("login", status, body)["user"]
    print(f"     user: {user['id']}")

    # 登录会轮换 CSRF token（rotate_token），后续写操作必须用新 token
    status, body = call("GET", "/api/csrf")
    csrf = step("csrf-post-login", status, body)["csrfToken"]

    status, body = call(
        "POST",
        "/api/admin/model-providers",
        {
            "displayName": "perf-mock",
            "api": "openai-responses",
            "apiBase": MOCK_API_BASE,
            "secret": "sk-perf-dummy",
        },
        csrf,
    )
    body = step("provider", status, body, ok=(200, 201))
    provider_id = body["provider"]["id"]
    bind_mock_quota(provider_id)

    status, body = call(
        "POST",
        "/api/admin/models",
        {
            "providerId": provider_id,
            "modelName": "perf-instant",
            "displayName": "Perf Instant Mock",
            "contextTokens": 200000,
            "maxOutputTokens": 8192,
            "enabled": True,
        },
        csrf,
    )
    body = step("model", status, body, ok=(200, 201))
    model_id = (body.get("model") or body)["id"]

    status, body = call("GET", "/api/workspaces")
    body = step("workspaces", status, body, ok=(200,))
    workspaces = body["workspaces"] if isinstance(body, dict) else body
    workspace = workspaces[0]
    print(f"     workspace: {workspace['id']}")

    status, body = call(
        "GET", f"/api/workspaces/{workspace['id']}/agents"
    )
    body = step("agents", status, body, ok=(200,))
    agents = body.get("agents") or []
    if not agents:
        print("FAIL: 没有可用 agent（bootstrap 应该播种默认 agent）")
        sys.exit(1)
    agent = agents[0]
    print(f"     agent: {agent['id']}")

    status, body = call(
        "POST",
        f"/api/workspaces/{workspace['id']}/sessions/new/messages",
        {
            "operationId": uuid.uuid4().hex,
            "agentId": agent["id"],
            "text": "Run the perf smoke: call your tool once, then finish.",
            "attachmentRefs": [],
            "modelConfigRef": model_id,
        },
        csrf,
    )
    body = step("message", status, body)
    session_id = body["sessionId"]
    agent_run_id = body["agentRunId"]
    print(f"     session: {session_id}  run: {agent_run_id}")

    with urllib.request.urlopen(
        urllib.request.Request(
            f"{API}/api/sessions/{session_id}/agent-runs/{agent_run_id}/events",
            headers={"Cookie": cookie_header(), "Accept": "text/event-stream"},
        ), timeout=60,
    ) as stream:
        terminal = read_terminal(stream)

    if terminal == "agent_run_completed":
        print(f"终态到达: {terminal}")
        return
    print(f"FAIL: 终态异常: {terminal}")
    sys.exit(1)


if __name__ == "__main__":
    if sys.argv[1:] == ["--smoke-child"]:
        main()
    else:
        try:
            sys.exit(run_bounded_smoke())
        except subprocess.TimeoutExpired:
            print("FAIL: smoke exceeded its 180-second deadline")
            sys.exit(1)
