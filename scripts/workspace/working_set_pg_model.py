"""Deterministic local model and bounded setup barrier for the PostgreSQL A/B.

Only provider output is simulated. Inputs, compaction commits, tools and Run
completion remain Core/Host responsibilities. No credentials or bodies are logged.
"""
from __future__ import annotations

import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import math
import os
from pathlib import Path
import re
import ssl
import threading
import uuid

SUMMARY = ("# Goal\n\nFollow the next synthetic performance task exactly.\n\n"
           "## Context\n\nThe supplied archival inputs are synthetic benchmark data. "
           "They establish no tool execution, successful output, or cleanup fact.\n\n"
           "## Next steps\n\nUse actual tool results to answer the current task.")
SETUP_REPLY = "Working-set setup step observed."
READY_REPLY = "WORKING_SET_READY"
MAX_BODY = 2 * 1024 * 1024
IMPLEMENTATION_SHA256 = hashlib.sha256(Path(__file__).read_bytes()).hexdigest()


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False)


def text(message):
    content = message.get("content", "")
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        return "\n".join(part.get("text", "") for part in content if isinstance(part, dict))
    return ""


def boundary(request):
    """Preserve exact text; normalize only opaque tool-call IDs by their order."""
    calls, projected = {}, []
    for message in request["messages"]:
        row = {"role": message["role"], "content": text(message)}
        for call in message.get("tool_calls", []):
            calls.setdefault(call["id"], "call:" + str(len(calls)))
            row.setdefault("toolCalls", []).append({"id": calls[call["id"]],
                "function": call["function"]})
        if "tool_call_id" in message:
            row["toolCallId"] = calls[message["tool_call_id"]]
        projected.append(row)
    contents = [text(message) for message in request["messages"]]
    return {"messageCount": len(contents),
            "contentBytes": sum(len(value.encode()) for value in contents),
            "messageContentBytes": [len(value.encode()) for value in contents],
            "messageSemanticSha256": ["sha256:" + hashlib.sha256(canonical(value).encode()).hexdigest()
                                      for value in projected],
            "semanticSha256": "sha256:" + hashlib.sha256(canonical(projected).encode()).hexdigest(),
            "toolsSha256": "sha256:" + hashlib.sha256(canonical({key: request.get(key)
                for key in ("tools", "tool_choice", "max_tokens")}).encode()).hexdigest(),
            "wireBytes": len(canonical(request).encode()),
            "historicalMarkers": sum(value.count("WORKING_SET_ARCHIVE ") for value in contents),
            "paddingMarkers": sum(value.count("WORKING_SET_PADDING") for value in contents)}


def summary_response(request):
    messages = request["messages"]
    if not any(text(message).startswith("[model_compaction_prompt_v1]") for message in messages):
        return None
    if len(messages) != 1 or request.get("tools") or request.get("tool_choice", "none") != "none":
        raise ValueError("invalid Core compaction shape")
    match = re.search(r"<conversation>\n(.*)\n</conversation>\s*$", text(messages[0]), re.DOTALL)
    if match is None:
        raise ValueError("missing Core conversation")
    records = [json.loads(line) for line in match[1].splitlines() if line.strip()]
    if not records or any(not isinstance(row, dict) or row.get("role") not in
            {"system", "user", "assistant", "tool"} or not isinstance(row.get("content"), str)
            for row in records):
        raise ValueError("invalid Core conversation records")
    # This summary makes no claim about command success or source progress.
    return SUMMARY


class State:
    def __init__(self):
        self.condition = threading.Condition()
        self.history = None
        self.main_calls = self.summary_calls = 0
        self.pending = None
        self.released = set()
        self.cancelled = False
        self.boundaries = []

    def configure(self, history):
        with self.condition:
            if self.history is not None or history not in (1000, 10000):
                raise ValueError("one explicit history configuration required")
            self.history = history

    def observe(self, request, phase, sample=None, round_index=None):
        with self.condition:
            if self.history is None or self.cancelled:
                raise ValueError("model not admitted")
            if phase == "summary":
                self.summary_calls += 1
                if self.summary_calls > math.ceil(self.history / 8) + 2 + 2 * (120 + 90):
                    raise ValueError("summary call budget exceeded")
            else:
                self.main_calls += 1
                if self.main_calls > math.ceil(self.history / 8) + 3 + 2 * (120 + 90):
                    raise ValueError("main call budget exceeded")
            item = {"schema": "runtime.working_set.model_boundary.v1",
                    "position": len(self.boundaries) + 1, "phase": phase,
                    "sampleId": sample, "round": round_index, **boundary(request)}
            self.boundaries.append(item)
            return item

    def gate(self, observed, kind, indices):
        with self.condition:
            if self.pending is not None:
                raise ValueError("concurrent setup generation")
            self.pending = {"id": observed["position"], "kind": kind,
                            "historyIndices": indices, "boundary": observed}
            self.condition.notify_all()
            ready = self.condition.wait_for(lambda: observed["position"] in self.released
                                            or self.cancelled, timeout=20)
            self.pending = None
            if not ready or self.cancelled:
                self.cancelled = True
                raise ValueError("bounded setup barrier stopped")
        return READY_REPLY if kind == "seal" else SETUP_REPLY


STATE = State()


class Control(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def reply(self, value):
        body = canonical(value).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        with STATE.condition:
            if self.path == "/state":
                self.reply({"pending": STATE.pending, "mainCalls": STATE.main_calls,
                            "summaryCalls": STATE.summary_calls, "cancelled": STATE.cancelled})
            elif self.path == "/boundaries":
                self.reply({"records": STATE.boundaries})
            else:
                self.send_error(404)

    def do_POST(self):
        try:
            size = int(self.headers.get("Content-Length", "0"))
            if not 0 < size <= 4096:
                raise ValueError("invalid control body")
            value = json.loads(self.rfile.read(size))
            with STATE.condition:
                if self.path == "/configure" and set(value) == {"history"}:
                    STATE.configure(value["history"])
                elif self.path == "/release" and set(value) == {"gateId"}:
                    if STATE.pending is None or STATE.pending["id"] != value["gateId"]:
                        raise ValueError("stale setup gate")
                    STATE.released.add(value["gateId"])
                    STATE.condition.notify_all()
                elif self.path == "/cancel" and value == {"cancel": True}:
                    STATE.cancelled = True
                    STATE.condition.notify_all()
                else:
                    raise ValueError("invalid control operation")
            self.reply({"ok": True, "implementationSha256": IMPLEMENTATION_SHA256})
        except (ValueError, TypeError, KeyError):
            self.send_error(422, "working-set control rejected")


class Model(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        if self.path != "/v1/chat/completions":
            self.send_error(404)
            return
        try:
            size = int(self.headers.get("Content-Length", "0"))
            if not 0 < size <= MAX_BODY:
                raise ValueError("model request exceeds bound")
            request = json.loads(self.rfile.read(size))
            summary = summary_response(request)
            call = None
            if summary is not None:
                STATE.observe(request, "summary")
                answer = summary
            else:
                messages = request["messages"]
                last_user = max(index for index, message in enumerate(messages) if message["role"] == "user")
                prompt = text(messages[last_user])
                match = re.fullmatch(
                    r"Synthetic performance task\. Invoke bash exactly 1 time\(s\), sequentially, with the following exact command\(s\)\. "
                    r"Do not combine commands, add other tool calls, or publish artifacts\. Use timeout_ms=10000 for each bash call\. "
                    r"Command 1:\n(printf 'PERF_EXPERIENCE_([a-f0-9]{32})_TOOL_OK\\n')"
                    r"\nAfter checking the tool outputs, reply with exactly PERF_EXPERIENCE_\2_TOOL_OK", prompt)
                if match:
                    replies = [message for message in messages[last_user + 1:] if message["role"] == "tool"]
                    answer = "PERF_EXPERIENCE_" + match[2] + "_TOOL_OK"
                    STATE.observe(request, "measurement", match[2], len(replies))
                    if not replies:
                        names = {tool["function"]["name"] for tool in request.get("tools", [])}
                        if "bash" not in names:
                            raise ValueError("bash not advertised")
                        call = {"id": "call-" + uuid.uuid4().hex, "type": "function",
                                "function": {"name": "bash", "arguments": canonical({"command": match[1], "timeout_ms": 10000})}}
                    elif len(replies) != 1 or answer not in text(replies[0]):
                        raise ValueError("actual tool result missing")
                elif prompt.startswith(("WORKING_SET_SETUP", "WORKING_SET_ARCHIVE ",
                                        "WORKING_SET_PADDING", "WORKING_SET_SEAL")):
                    observed = STATE.observe(request, "setup")
                    kind = "seal" if prompt.startswith("WORKING_SET_SEAL") else "padding" if prompt.startswith("WORKING_SET_PADDING") else "history" if prompt.startswith("WORKING_SET_ARCHIVE ") else "initial"
                    indices = [int(value) for value in re.findall(r"WORKING_SET_ARCHIVE (\d{6})", prompt)]
                    answer = STATE.gate(observed, kind, indices)
                else:
                    raise ValueError("unknown deterministic prompt")
        except (ValueError, KeyError, TypeError, StopIteration):
            self.send_error(422, "working-set model contract rejected")
            return
        identifier = "chatcmpl-" + uuid.uuid4().hex
        message = {"role": "assistant", **({"tool_calls": [call]} if call else {"content": answer})}
        reason = "tool_calls" if call else "stop"
        self.send_response(200)
        streaming = request.get("stream", False)
        self.send_header("Content-Type", "text/event-stream" if streaming else "application/json")
        self.send_header("Connection", "close")
        self.end_headers()
        self.close_connection = True
        # The fixture has no tokenizer. Omit usage rather than fabricate measured input.
        try:
            if not streaming:
                self.wfile.write(canonical({"id": identifier, "object": "chat.completion", "created": 0,
                    "model": request.get("model"), "choices": [{"index": 0, "message": message,
                    "finish_reason": reason}]}).encode())
            else:
                delta = dict(message)
                if call:
                    delta["tool_calls"] = [{"index": 0, **call}]
                for choice in [{"index": 0, "delta": delta, "finish_reason": None},
                               {"index": 0, "delta": {}, "finish_reason": reason}]:
                    self.wfile.write(("data: " + canonical({"id": identifier, "object": "chat.completion.chunk",
                        "model": request.get("model"), "choices": [choice]}) + "\n\n").encode())
                self.wfile.write(b"data: [DONE]\n\n")
            self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError):
            pass


if __name__ == "__main__":
    threading.Thread(target=ThreadingHTTPServer(("0.0.0.0", 8081), Control).serve_forever, daemon=True).start()
    server = ThreadingHTTPServer(("0.0.0.0", 8080), Model)
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain(os.environ["MOCK_TLS_CERT"], os.environ["MOCK_TLS_KEY"])
    server.socket = context.wrap_socket(server.socket, server_side=True)
    server.serve_forever()
