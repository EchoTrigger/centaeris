"""Real hosted transcript capture on the owned isolated performance stack."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import threading
import time
from urllib.error import HTTPError
from urllib.parse import urlencode
import uuid

import control
from pool_workload import Client, durable_runs


class CaptureNotReady(RuntimeError):
    pass


def ready_page(client, session):
    deadline = time.monotonic() + 20
    while True:
        page = client.call("GET", f"/api/sessions/{session}/transcript", expected=(200, 409))
        if "error" not in page:
            return page
        if page["error"] != "transcript_projection_not_ready" or time.monotonic() >= deadline:
            raise RuntimeError("transcript page did not become ready")
        time.sleep(.2)


def query_content(client, session, query):
    request = client.request("GET", f"/api/sessions/{session}/transcript/content")
    request.full_url += "?" + urlencode(query)
    started = time.monotonic()
    record = {"method": "GET", "path": f"/api/sessions/{session}/transcript/content", "status": None}
    try:
        try:
            response = client.opener.open(request, timeout=10)
        except HTTPError as error:
            record["status"] = error.code
            with error:
                if error.code == 409 and json.loads(client.read(error, started + 10)).get(
                        "error") == "transcript_content_unavailable":
                    raise CaptureNotReady("capture has not become available") from error
            raise
        with response:
            record["status"] = response.status
            return json.loads(client.read(response, started + 10))
    finally:
        record["latencyMs"] = round((time.monotonic() - started) * 1000, 3)
        client.records.append(record)


def read_all(client, session, query):
    parts = []
    offset = 0
    for _ in range(8):
        page = query_content(client, session, {**query, "offset": str(offset)})
        content = page["content"].encode()
        end = int(page["endOffset"])
        if not content or len(content) > 65536 or end != offset + len(content):
            raise RuntimeError("invalid bounded UTF-8 continuation")
        parts.append(content)
        offset = end
        if not page["hasMore"]:
            data = b"".join(parts)
            if len(data) != int(query["byteLength"]):
                raise RuntimeError("incomplete historical content")
            return data, len(parts)
    raise RuntimeError("historical read exceeded eight-page budget")


def main(output):
    output = Path(output)
    output.mkdir(parents=True, exist_ok=False)
    stack = control.Stack()
    stack.preflight()
    if not stack.quiet():
        raise RuntimeError("capture validation requires an idle isolated stack")
    report = {"passed": False, "requests": [], "observations": []}
    (output / "manifest.json").write_text(json.dumps(stack.manifest(), indent=2))
    client = Client(report["requests"])
    try:
        client.csrf = client.call("GET", "/api/csrf")["csrfToken"]
        client.call("POST", "/api/login", {
            "email": stack.values["BOOTSTRAP_SUPERADMIN_EMAIL"],
            "password": stack.values["BOOTSTRAP_SUPERADMIN_PASSWORD"]})
        client.csrf = client.call("GET", "/api/csrf")["csrfToken"]
        provider = client.call("POST", "/api/admin/model-providers", {
            "displayName": "capture-" + uuid.uuid4().hex,
            "api": "openai-responses", "apiBase": "https://mock-model:9999/v1",
            "secret": "synthetic-capture"}, expected=(200, 201))["provider"]["id"]
        stack.compose(["exec", "-T", "api", "python", "manage.py", "configure_model_quota",
                       "--domain-id", "capture-" + provider, "--max-concurrent", "1",
                       "--provider-id", provider, "--enable"], timeout=20)
        model = client.call("POST", "/api/admin/models", {
            "providerId": provider, "modelName": "perf-transcript-capture",
            "displayName": "Transcript capture validation", "contextTokens": 200000,
            "maxOutputTokens": 8192, "enabled": True}, expected=(200, 201))
        workspace = client.call("GET", "/api/workspaces")["workspaces"][0]["id"]
        agent = client.call("GET", f"/api/workspaces/{workspace}/agents")["agents"][0]["id"]
        accepted = client.call("POST", f"/api/workspaces/{workspace}/sessions/new/messages", {
            "operationId": uuid.uuid4().hex, "agentId": agent,
            "modelConfigRef": (model.get("model") or model)["id"],
            "text": "Validate fixed historical text after source replacement and deletion.",
            "attachmentRefs": []}, expected=(202,))
        report["accepted"] = accepted
        client.observe(accepted, 0, report["observations"], threading.Event())
        if report["observations"][0].get("terminal") != "agent_run_completed":
            raise RuntimeError("capture run did not complete successfully")
        report["durable"] = durable_runs(stack, [accepted])
        if report["durable"][0]["status"] != "completed":
            raise RuntimeError("durable run was not completed")
        session = accepted["sessionId"]
        # IDs come from the accepted API receipt; quote before the read-only query.
        session_sql = session.replace("'", "''")
        events = json.loads(stack.sql("SELECT coalesce(json_agg(payload ORDER BY sequence),'[]'::json) "
            f"FROM app_core_sessionevent WHERE session_id='{session_sql}'"))
        tools = [event["payload"] for event in events if event.get("type") == "tool_result"]
        report["toolResults"] = [{k: tool.get(k) for k in
            ("callId", "toolName", "resultState", "outputByteLength", "fullOutputPath")} for tool in tools]
        if len(tools) != 3 or any(tool["resultState"] != "successWithOutput" for tool in tools):
            raise RuntimeError("capture workload requires three successful tool results")
        if "capture-mutated:1" not in tools[1]["modelContent"] or "capture-deleted:1" not in tools[2]["modelContent"]:
            raise RuntimeError("source replacement/deletion was not confirmed")
        first = tools[0]
        if not first.get("fullOutputPath") or first["outputByteLength"] <= 50 * 1024:
            raise RuntimeError("large result did not spill")
        page = ready_page(client, session)
        query = {"projectionGeneration": page["projectionGeneration"],
                 "refId": "tool-output:" + first["callId"], "revision": "2",
                 "byteLength": str(first["outputByteLength"])}
        deadline = time.monotonic() + 15
        while True:
            try:
                data, pages = read_all(client, session, query)
                break
            except CaptureNotReady:
                if time.monotonic() >= deadline:
                    raise
                time.sleep(.25)
        # The public bash result formats stdout and removes its trailing newline.
        expected = ("归档😀\n" * 10000).encode().rstrip(b"\n")
        if not data.endswith(expected):
            raise RuntimeError("historical content differs from generated text")
        report["content"] = {"bytes": len(data), "pages": pages,
                             "sha256": hashlib.sha256(data).hexdigest()}
        if not stack.quiet():
            raise RuntimeError("stack must be idle before service replacement")
        stack.compose(["up", "-d", "--no-build", "--no-deps", "--force-recreate",
                       "--wait", "api", "runtime"], timeout=180)
        stack.preflight()
        again, _ = read_all(client, session, query)
        if again != data:
            raise RuntimeError("historical content changed after service replacement")
        report["serviceReplacementPreservedBytes"] = True
        report["passed"] = True
    except Exception as error:
        report["failure"] = {"errorType": type(error).__name__}
        raise
    finally:
        (output / "report.json").write_text(json.dumps(report, indent=2), encoding="utf-8")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--evidence", required=True)
    parser.add_argument("--child", action="store_true", help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.child:
        main(args.evidence)
    else:
        subprocess.run([sys.executable, __file__, "--child", "--evidence", args.evidence],
                       check=True, timeout=360)
