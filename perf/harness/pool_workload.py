"""Bounded synthetic HTTP/SSE workload; only used inside the isolated perf stack.

The controller must additionally enforce a child-process deadline (210 seconds).
No response bodies, credentials, cookies or request payloads enter evidence files.
"""
import json
import math
from datetime import datetime, timezone
from pathlib import Path
import threading
import time
import urllib.error
import urllib.request
import uuid


API = "http://127.0.0.1:18000"
TERMINALS = {"agent_run_completed", "agent_run_failed", "agent_run_interrupted"}


def read_chunk(response, deadline, size):
    remaining = deadline - time.monotonic()
    if remaining <= 0:
        raise TimeoutError("HTTP response deadline exceeded")
    # urllib exposes HTTPResponse's buffered socket; bound its next blocking read
    # by the remaining total deadline, including streams that never reach EOF.
    sock = getattr(getattr(getattr(response, "fp", None), "raw", None), "_sock", None)
    if sock is not None:
        sock.settimeout(remaining)
    return response.read1(size)


def validate_result(accepted, observations, durable, observers):
    ids = {item["agentRunId"] for item in accepted}
    if not ids or len(ids) != len(accepted):
        raise ValueError("accepted run identities absent or duplicated")
    expected = {(identity, index) for identity in ids for index in range(observers)}
    actual = {(item["agentRunId"], item["observer"]) for item in observations}
    if actual != expected or len(observations) != len(expected) or any(
            item.get("terminal") != "agent_run_completed" for item in observations):
        raise ValueError("observer terminal accounting failed")
    if {item["id"] for item in durable} != ids or len(durable) != len(ids) or any(
            item.get("status") != "completed" or not item.get("completedAt") for item in durable):
        raise ValueError("durable run completion accounting failed")


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


class Client:
    def __init__(self, records):
        self.records = records
        self.cookies = {}
        self.csrf = None
        self.opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())

    def request(self, method, path, payload=None):
        if not path.startswith("/api/") or "?" in path or "#" in path:
            raise ValueError("only explicit local API paths are permitted")
        body = json.dumps(payload).encode() if payload is not None else None
        headers = {"Cookie": "; ".join(f"{k}={v}" for k, v in self.cookies.items())}
        if body is not None:
            headers["Content-Type"] = "application/json"
        if self.csrf and method != "GET":
            headers["X-CSRFToken"] = self.csrf
        return urllib.request.Request(API + path, data=body, method=method, headers=headers)

    @staticmethod
    def read(response, deadline, limit=1024 * 1024):
        result = bytearray()
        while time.monotonic() < deadline:
            part = read_chunk(response, deadline, min(8192, limit + 1 - len(result)))
            if not part:
                return bytes(result)
            result.extend(part)
            if len(result) > limit:
                raise ValueError("response exceeded byte budget")
        raise TimeoutError("HTTP response deadline exceeded")

    def call(self, method, path, payload=None, expected=(200,)):
        request = self.request(method, path, payload)
        start = time.monotonic()
        record = {"method": method, "path": path, "status": None,
                  "startedAtUtc": datetime.now(timezone.utc).isoformat()}
        try:
            try:
                response = self.opener.open(request, timeout=30)
            except urllib.error.HTTPError as error:
                response = error
            with response:
                record["status"] = response.status
                for header in response.headers.get_all("Set-Cookie") or []:
                    name, value = header.split(";", 1)[0].split("=", 1)
                    self.cookies[name.strip()] = value.strip()
                if response.status not in expected:
                    raise RuntimeError(f"HTTP status {response.status}")
                return json.loads(self.read(response, start + 30) or b"{}")
        except Exception as error:
            record["errorType"] = type(error).__name__
            raise
        finally:
            record["latencyMs"] = round((time.monotonic() - start) * 1000, 3)
            self.records.append(record)

    def observe(self, accepted, index, outcomes, stop):
        run_id = accepted["agentRunId"]
        path = f"/api/sessions/{accepted['sessionId']}/agent-runs/{run_id}/events"
        start = time.monotonic()
        record = {"method": "GET", "path": path, "status": None,
                  "startedAtUtc": datetime.now(timezone.utc).isoformat()}
        outcome = {"agentRunId": run_id, "observer": index, "terminal": None}
        try:
            request = self.request("GET", path)
            request.add_header("Accept", "text/event-stream")
            with self.opener.open(request, timeout=30) as response:
                record["status"] = response.status
                if response.status != 200:
                    raise RuntimeError("SSE HTTP failure")
                pending = b""
                total = 0
                while time.monotonic() - start < 30:
                    chunk = read_chunk(response, start + 30, 8192)
                    if not chunk:
                        break
                    total += len(chunk)
                    if total > 8 * 1024 * 1024:
                        raise ValueError("SSE byte budget exceeded")
                    pending += chunk
                    while b"\n" in pending:
                        line, pending = pending.split(b"\n", 1)
                        if not line.startswith(b"data:"):
                            continue
                        event = json.loads(line[5:].strip())
                        event_type = (event.get("event") or event).get("type")
                        if event_type in TERMINALS:
                            outcome["terminal"] = event_type
                            if event_type != "agent_run_completed":
                                stop.set()
                            return
                raise TimeoutError("SSE ended or exceeded deadline without terminal")
        except Exception as error:
            if isinstance(error, urllib.error.HTTPError):
                record["status"] = error.code
            outcome["errorType"] = type(error).__name__
            stop.set()
        finally:
            record["latencyMs"] = round((time.monotonic() - start) * 1000, 3)
            self.records.append(record)
            outcomes.append(outcome)


def setup(stack, client):
    client.csrf = client.call("GET", "/api/csrf")["csrfToken"]
    client.call("POST", "/api/login", {
        "email": stack.values["BOOTSTRAP_SUPERADMIN_EMAIL"],
        "password": stack.values["BOOTSTRAP_SUPERADMIN_PASSWORD"]})
    client.csrf = client.call("GET", "/api/csrf")["csrfToken"]
    provider = client.call("POST", "/api/admin/model-providers", {
        "displayName": "pool-validation-" + uuid.uuid4().hex,
        "api": "openai-responses", "apiBase": "https://mock-model:9999/v1",
        "secret": "sk-perf-dummy"}, expected=(200, 201))["provider"]["id"]
    stack.compose(["exec", "-T", "api", "python", "manage.py", "configure_model_quota",
                   "--domain-id", "pool-" + provider, "--max-concurrent", "2",
                   "--provider-id", provider, "--enable"], timeout=15)
    model = client.call("POST", "/api/admin/models", {
        "providerId": provider, "modelName": "perf-paced", "displayName": "Pool Paced",
        "contextTokens": 200000, "maxOutputTokens": 8192, "enabled": True}, expected=(200, 201))
    model_id = (model.get("model") or model)["id"]
    workspace_id = client.call("GET", "/api/workspaces")["workspaces"][0]["id"]
    agent_id = client.call("GET", f"/api/workspaces/{workspace_id}/agents")["agents"][0]["id"]
    return workspace_id, agent_id, model_id


def durable_runs(stack, accepted):
    if not accepted:
        return []
    ids = ",".join("'" + item["agentRunId"].replace("'", "''") + "'" for item in accepted)
    return json.loads(stack.sql(
        'SELECT coalesce(json_agg(r),\'[]\'::json) FROM (SELECT id,status,"completedAt" '
        f'FROM app_core_agentrun WHERE id IN ({ids})) r'))


def run_workload(stack, output, seconds=60, interval=5, observers=3, stop=None):
    if not (0 < seconds <= 60 and interval > 0 and math.ceil(seconds / interval) <= 12
            and 1 <= observers <= 3):
        raise ValueError("workload exceeds bounded arrival/observer budget")
    output = Path(output)
    output.mkdir(parents=True, exist_ok=True)
    stop = stop if stop is not None else threading.Event()
    result = {"ok": False, "requests": [], "accepted": [], "observations": [], "durable": []}
    threads = []
    started = time.monotonic()
    try:
        client = Client(result["requests"])
        workspace, agent, model = setup(stack, client)
        began = time.monotonic()
        for arrival in range(math.ceil(seconds / interval)):
            remaining = began + arrival * interval - time.monotonic()
            if remaining > 0:
                stop.wait(remaining)
            if stop.is_set() or time.monotonic() - began >= seconds:
                break
            active = {run_id for run_id, thread in threads if thread.is_alive()}
            if len(active) >= 2:
                continue
            operation_id = uuid.uuid4().hex
            accepted = client.call("POST", f"/api/workspaces/{workspace}/sessions/new/messages", {
                "operationId": operation_id, "agentId": agent, "modelConfigRef": model,
                "text": "Call your tool once, then finish the pool validation run.",
                "attachmentRefs": []}, expected=(202,))
            entry = {"agentRunId": accepted["agentRunId"], "sessionId": accepted["sessionId"],
                     "operationId": operation_id}
            result["accepted"].append(entry)
            for index in range(observers):
                thread = threading.Thread(target=client.observe,
                    args=(entry, index, result["observations"], stop), daemon=True)
                threads.append((entry["agentRunId"], thread))
                thread.start()
    except Exception as error:
        result["errorType"] = type(error).__name__
        stop.set()
    finally:
        deadline = min(started + 180, time.monotonic() + 120)
        for _, thread in threads:
            thread.join(max(0, deadline - time.monotonic()))
        try:
            result["durable"] = durable_runs(stack, result["accepted"])
            validate_result(result["accepted"], result["observations"], result["durable"], observers)
            result["ok"] = not stop.is_set() and not any(t.is_alive() for _, t in threads)
        except Exception as error:
            result.setdefault("errorType", type(error).__name__)
        result["elapsedSeconds"] = round(time.monotonic() - started, 3)
        (output / "workload.json").write_text(json.dumps(result, indent=2), encoding="utf-8")
    return result
