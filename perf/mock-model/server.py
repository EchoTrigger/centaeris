"""确定性 OpenAI Responses 方言 mock。

方言依据：packages/core/src/model/protocol_adapters/openai_responses.rs
  请求  POST {base}/responses
  事件集（适配器只认这 8 种，其余不发）：
    response.created / response.output_item.added /
    response.output_text.delta / response.function_call_arguments.delta /
    response.function_call_arguments.done / response.output_item.done /
    response.completed / response.incomplete

profile 由 model 名后缀决定：
  *instant    立即吐完（纯编排极限）
  *paced      固定 TTFT 300ms + 20 chunk × 50ms（真实流式生命周期）
  *marathon   第二次请求起 TTFT 500ms + 长 chunk 流（默认 300×1s，供 S1 挂观察者）
  *toolstorm  连续 4 轮 function_call（bash）再收尾（供 S4 沙箱 exec 风暴）

行为脚本（无状态、按请求内容判定）：
  input 里没有 function_call_output → 返回 function_call
  input 里有 function_call_output   → 按 profile 决定续调或收尾
usage 形状为标准 OpenAI Responses 结构，字段路径以 B1 冒烟实测为准。
"""

import json
import os
import ssl
import shlex
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

TOOL_NAME = os.environ.get("MOCK_TOOL_NAME", "bash")
TOOL_ARGS = os.environ.get("MOCK_TOOL_ARGS", '{"command":"echo perf"}')
PORT = 9999
TLS_PEM = "/opt/mock/tls.pem"
FINAL_TEXT = "Perf run complete. The deterministic mock has nothing further to add."
WAITING_MARKER = "restart-matrix parent waiting"
WAITING_CHILD_MARKER = "Restart matrix durable wait child"


def capture_command(rounds):
    scripts = [
        "import sys; sys.stdout.buffer.write(('归档😀\\n'*10000).encode('utf-8'))",
        "from pathlib import Path; files=list(Path('.agent-tool-results').rglob('*.log')); "
        "[p.write_bytes(b'B'*p.stat().st_size) for p in files]; print('capture-mutated:'+str(len(files)))",
        "from pathlib import Path; files=list(Path('.agent-tool-results').rglob('*.log')); "
        "[p.unlink() for p in files]; print('capture-deleted:'+str(len(files)))",
    ]
    return "python3 -c " + shlex.quote(scripts[rounds]) if rounds < len(scripts) else None


def sse(event_type: str, data: dict) -> bytes:
    return f"event: {event_type}\ndata: {json.dumps(data, separators=(',', ':'))}\n\n".encode()


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *args):  # 安静：负载下日志本身是压力源
        pass

    def emit_function_call(self, response_id: str):
        """输出一轮 function_call 事件并返回 (items, usage)。call_id 每次唯一——
        重复 call_id 会被 runtime 按模型输出契约 loud-fail。"""
        nonce = time.time_ns()
        fc_id = f"fc_{nonce}"
        call_id = f"call_{nonce}"
        call_item = {
            "type": "function_call", "id": fc_id, "call_id": call_id,
            "name": TOOL_NAME, "arguments": TOOL_ARGS,
        }
        self.wfile.write(sse("response.output_item.added", {
            "type": "response.output_item.added", "output_index": 0,
            "item": {"type": "function_call", "id": fc_id,
                     "call_id": call_id, "name": TOOL_NAME},
        }))
        self.wfile.write(sse("response.function_call_arguments.delta", {
            "type": "response.function_call_arguments.delta", "item_id": fc_id,
            "output_index": 0, "delta": TOOL_ARGS,
        }))
        self.wfile.write(sse("response.function_call_arguments.done", {
            "type": "response.function_call_arguments.done", "item_id": fc_id,
            "output_index": 0, "arguments": TOOL_ARGS,
        }))
        self.wfile.write(sse("response.output_item.done", {
            "type": "response.output_item.done", "output_index": 0, "item": call_item,
        }))
        usage = {
            "input_tokens": 480, "output_tokens": 16, "total_tokens": 496,
            "input_tokens_details": {"cached_tokens": 0},
            "output_tokens_details": {"reasoning_tokens": 0},
        }
        return [call_item], usage

    def emit_named_function_call(self, name: str, arguments: dict):
        nonce = time.time_ns()
        fc_id = f"fc_{nonce}"
        call_id = f"call_{nonce}"
        args = json.dumps(arguments, separators=(",", ":"))
        item = {"type": "function_call", "id": fc_id, "call_id": call_id,
                "name": name, "arguments": args}
        self.wfile.write(sse("response.output_item.added", {
            "type": "response.output_item.added", "output_index": 0,
            "item": {"type": "function_call", "id": fc_id,
                     "call_id": call_id, "name": name},
        }))
        self.wfile.write(sse("response.function_call_arguments.delta", {
            "type": "response.function_call_arguments.delta", "item_id": fc_id,
            "output_index": 0, "delta": args,
        }))
        self.wfile.write(sse("response.function_call_arguments.done", {
            "type": "response.function_call_arguments.done", "item_id": fc_id,
            "output_index": 0, "arguments": args,
        }))
        self.wfile.write(sse("response.output_item.done", {
            "type": "response.output_item.done", "output_index": 0, "item": item,
        }))
        return [item], {
            "input_tokens": 480, "output_tokens": 16, "total_tokens": 496,
            "input_tokens_details": {"cached_tokens": 0},
            "output_tokens_details": {"reasoning_tokens": 0},
        }

    @staticmethod
    def waiting_phase(request: dict):
        calls = {item.get("call_id"): item.get("name") for item in request.get("input", [])
                 if isinstance(item, dict) and item.get("type") == "function_call"}
        agent_ref = None
        for item in request.get("input", []):
            if not isinstance(item, dict) or item.get("type") != "function_call_output":
                continue
            tool_name = calls.get(item.get("call_id"))
            if tool_name == "task_output":
                return "complete", None
            if tool_name != "agent":
                continue
            raw = item.get("output")
            try:
                value = json.loads(raw) if isinstance(raw, str) else raw
            except json.JSONDecodeError:
                continue
            if isinstance(value, dict):
                details = value.get("details") if isinstance(value.get("details"), dict) else value
                output_ref = details.get("outputRef")
                if isinstance(output_ref, dict):
                    required = ("schema", "kind", "runtimeJobId", "childSessionId", "resultRef")
                    if all(key in output_ref for key in required):
                        agent_ref = {
                            "schema": output_ref["schema"], "kind": output_ref["kind"],
                            "runtime_job_id": output_ref["runtimeJobId"],
                            "child_session_id": output_ref["childSessionId"],
                            "result_ref": output_ref["resultRef"],
                        }
        return ("task_output", agent_ref) if agent_ref is not None else ("agent", None)

    @staticmethod
    def latest_user_text(request: dict):
        for item in reversed(request.get("input", [])):
            if (not isinstance(item, dict) or item.get("type") not in (None, "message") or
                    item.get("role") != "user"):
                continue
            content = item.get("content", "")
            if isinstance(content, str):
                return content
            texts = [part.get("text", "") for part in content
                     if isinstance(part, dict) and part.get("type") == "input_text"]
            return "\n".join(texts)
        return ""

    @classmethod
    def waiting_role(cls, request: dict, model: str):
        if not model.endswith("waiting"):
            return None
        latest_user = cls.latest_user_text(request)
        if WAITING_MARKER in latest_user:
            return "parent"
        if WAITING_CHILD_MARKER in latest_user:
            return "child"
        return None

    @staticmethod
    def streams(request: dict):
        return request.get("stream", False) is True

    def emit_final(self, response_id: str, model: str):
        """输出收尾 assistant message 并返回 (items, usage)。"""
        profile = "paced" if model.endswith("paced") else "instant"
        if model.endswith("marathon") or model.endswith("waitchild"):
            chunk_count = (30 if model.endswith("waitchild") else
                           int(os.environ.get("MOCK_MARATHON_CHUNKS", "300")))
            chunk_delay = float(os.environ.get("MOCK_MARATHON_CHUNK_DELAY", "1.0"))
            text = "marathon stream chunk"
            chunks = []
            self.wfile.write(sse("response.output_item.added", {
                "type": "response.output_item.added", "output_index": 0,
                "item": {"type": "message", "id": "msg_m"},
            }))
            self.wfile.flush()
            time.sleep(0.5)
            for index in range(chunk_count):
                chunk = f"{text} {index} "
                chunks.append(chunk)
                self.wfile.write(sse("response.output_text.delta", {
                    "type": "response.output_text.delta", "item_id": "msg_m",
                    "output_index": 0, "content_index": 0,
                    "delta": chunk,
                }))
                self.wfile.flush()
                time.sleep(chunk_delay)
            final_text = "".join(chunks)
            items = [{
                "type": "message", "id": "msg_m", "role": "assistant",
                "content": [{"type": "output_text", "text": final_text}],
            }]
            self.wfile.write(sse("response.output_item.done", {
                "type": "response.output_item.done", "output_index": 0, "item": items[0],
            }))
            usage = {
                "input_tokens": 1024, "output_tokens": chunk_count * 4,
                "total_tokens": 1024 + chunk_count * 4,
                "input_tokens_details": {"cached_tokens": 0},
                "output_tokens_details": {"reasoning_tokens": 0},
            }
        else:
            items = [{
                "type": "message", "id": "msg_1", "role": "assistant",
                "content": [{"type": "output_text", "text": FINAL_TEXT}],
            }]
            self.wfile.write(sse("response.output_item.added", {
                "type": "response.output_item.added", "output_index": 0,
                "item": {"type": "message", "id": "msg_1"},
            }))
            for index in range(20 if profile == "paced" else 1):
                if profile == "paced":
                    time.sleep(0.05)
                self.wfile.write(sse("response.output_text.delta", {
                    "type": "response.output_text.delta", "item_id": "msg_1",
                    "output_index": 0, "content_index": 0,
                    "delta": FINAL_TEXT if profile == "instant" else FINAL_TEXT[: 12] + f" chunk {index} ",
                }))
            self.wfile.write(sse("response.output_item.done", {
                "type": "response.output_item.done", "output_index": 0, "item": items[0],
            }))
            usage = {
                "input_tokens": 512, "output_tokens": 64, "total_tokens": 576,
                "input_tokens_details": {"cached_tokens": 0},
                "output_tokens_details": {"reasoning_tokens": 0},
            }
        return items, usage

    def do_POST(self):
        if not self.path.endswith("/responses"):
            self.send_error(404)
            return
        length = int(self.headers.get("content-length", 0))
        request = json.loads(self.rfile.read(length) or b"{}")
        model = request.get("model", "perf-instant")
        tool_rounds = sum(
            1 for item in request.get("input", [])
            if isinstance(item, dict) and item.get("type") == "function_call_output"
        )
        # 排障日志：stdin/stderr 由 docker logs 收集
        item_types = [item.get("type") for item in request.get("input", []) if isinstance(item, dict)]
        print(f"REQ model={model} tool_rounds={tool_rounds} input_types={item_types}", flush=True)

        waiting_role = self.waiting_role(request, model)
        if model.endswith("waiting"):
            print(f"WAIT role={waiting_role} latest_user={self.latest_user_text(request)[:160]!r}",
                  flush=True)
        if model.endswith("waiting") and waiting_role is None:
            self.send_error(422, "waiting profile role marker missing")
            return
        if not self.streams(request):
            if waiting_role != "child":
                self.send_error(422, "non-stream mock request requires waiting child")
                return
            time.sleep(float(os.environ.get("MOCK_WAITING_CHILD_DELAY", "30")))
            item = {"type": "message", "id": f"msg_{time.time_ns()}", "role": "assistant",
                    "content": [{"type": "output_text", "text": "ready"}]}
            body = json.dumps({
                "id": f"resp_{time.time_ns()}", "model": model, "status": "completed",
                "output": [item], "output_text": "ready",
                "usage": {"input_tokens": 128, "output_tokens": 1, "total_tokens": 129,
                          "input_tokens_details": {"cached_tokens": 0},
                          "output_tokens_details": {"reasoning_tokens": 0}},
            }, separators=(",", ":")).encode()
            self.send_response(200)
            self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(body)))
            self.send_header("connection", "close")
            self.close_connection = True
            self.end_headers()
            self.wfile.write(body)
            return

        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.send_header("cache-control", "no-cache")
        # SSE 无 Content-Length：必须显式关连接，否则 httpx/requests
        # 在 response.completed 之后等 EOF 直到 ReadTimeout
        self.send_header("connection", "close")
        self.close_connection = True
        self.end_headers()

        response_id = f"resp_{self.headers.get('x-request-id', str(time.time_ns()))}"
        self.wfile.write(sse("response.created", {
            "type": "response.created", "response": {"id": response_id, "model": model},
        }))

        waiting_parent = waiting_role == "parent"
        waiting_child = waiting_role == "child"
        waiting_phase, waiting_ref = self.waiting_phase(request) if waiting_parent else (None, None)
        if model.endswith("transcript-capture"):
            command = capture_command(tool_rounds)
            if command is None:
                items, usage = self.emit_final(response_id, "perf-instant")
            else:
                items, usage = self.emit_named_function_call("bash", {"command": command})
        elif waiting_child:
            items, usage = self.emit_final(response_id, "perf-waitchild")
        elif model.endswith("restart30") and tool_rounds == 0:
            time.sleep(float(os.environ.get("MOCK_RESTART_DELAY", "30")))
            items, usage = self.emit_function_call(response_id)
        elif waiting_parent and waiting_phase == "agent":
            items, usage = self.emit_named_function_call("agent", {
                "prompt": WAITING_CHILD_MARKER + ": return the word ready.",
                "description": "Restart matrix durable wait child",
                "budget": {"max_summary_chars": 200},
            })
        elif waiting_parent and waiting_phase == "task_output":
            items, usage = self.emit_named_function_call("task_output", {"output_ref": waiting_ref})
        elif waiting_parent and waiting_phase == "complete":
            items, usage = self.emit_final(response_id, "perf-instant")
        elif tool_rounds == 0 or (model.endswith("toolstorm") and tool_rounds < 4):
            items, usage = self.emit_function_call(response_id)
        elif model.endswith("paced"):
            time.sleep(0.3)
            items, usage = self.emit_final(response_id, model)
        else:
            items, usage = self.emit_final(response_id, model)

        completed = {
            "type": "response.completed",
            "response": {
                "id": response_id, "model": model, "status": "completed",
                "output": items, "usage": usage,
            },
        }
        self.wfile.write(sse("response.completed", completed))
        self.wfile.flush()


def main():
    server = ThreadingHTTPServer(("0.0.0.0", PORT), Handler)
    if os.path.exists(TLS_PEM):
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.load_cert_chain(TLS_PEM)
        server.socket = context.wrap_socket(server.socket, server_side=True)
    print(f"mock-model listening on {PORT} (tls={os.path.exists(TLS_PEM)})", flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
