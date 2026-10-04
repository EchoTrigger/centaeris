"""Linux Runtime acceptance with a loopback model; never uses provider credentials."""
import asyncio
import io
import json
import os
from pathlib import Path
import tempfile
import unittest

from runner import connect, run_trial


@unittest.skipUnless(os.name == "posix" and os.environ.get("CENTAERIS_RUNTIME_BINARY"),
                     "requires a built Linux Runtime")
class NativeTrialTests(unittest.IsolatedAsyncioTestCase):
    async def test_budget_and_background_service_survive_completed_agent(self):
        requests = []
        proxy_domains = []
        with tempfile.TemporaryDirectory(prefix="cb-") as directory:
            root = Path(directory)
            fifo = root / "service-control"
            os.mkfifo(fifo)

            async def model(reader, writer):
                try:
                    head = await reader.readuntil(b"\r\n\r\n")
                    length = next(int(line.split(b":", 1)[1]) for line in head.split(b"\r\n")
                                  if line.lower().startswith(b"content-length:"))
                    request = json.loads(await reader.readexactly(length))
                    requests.append(request)
                    if len(requests) == 1:
                        delta = {"tool_calls": [{"index": 0, "id": "service-start", "type": "function",
                                 "function": {"name": "process_start", "arguments": json.dumps({
                                     "program": "bash", "args": ["-c", 'read -r value < "$1"',
                                                                   "service", str(fifo)],
                                     "timeout_ms": 30000})}}]}
                        finish = "tool_calls"
                    else:
                        delta, finish = {"content": "Service is ready for verification."}, "stop"
                    frames = [{"id": "mock", "choices": [{"index": 0, "delta": delta,
                               "finish_reason": None}]},
                              {"id": "mock", "choices": [{"index": 0, "delta": {},
                               "finish_reason": finish}], "usage": {
                                   "prompt_tokens": 10, "completion_tokens": 10, "total_tokens": 20}}]
                    body = "".join("data: " + json.dumps(frame) + "\n\n" for frame in frames)
                    body += "data: [DONE]\n\n"
                    writer.write(("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n"
                                  f"Content-Length: {len(body.encode())}\r\nConnection: close\r\n\r\n"
                                  + body).encode())
                    await writer.drain()
                finally:
                    writer.close()
                    await writer.wait_closed()

            async def socks(reader, writer):
                upstream = None
                try:
                    version, count = await reader.readexactly(2)
                    self.assertEqual(version, 5)
                    await reader.readexactly(count)
                    writer.write(b"\x05\x00")
                    await writer.drain()
                    header = await reader.readexactly(4)
                    self.assertEqual(header, b"\x05\x01\x00\x03")
                    length = (await reader.readexactly(1))[0]
                    proxy_domains.append((await reader.readexactly(length)).decode())
                    port = int.from_bytes(await reader.readexactly(2), "big")
                    upstream_reader, upstream = await asyncio.open_connection("127.0.0.1", port)
                    writer.write(b"\x05\x00\x00\x01\x7f\x00\x00\x01\x00\x00")
                    await writer.drain()
                    async def pump(source, destination):
                        while chunk := await source.read(65536):
                            destination.write(chunk)
                            await destination.drain()
                    pumps = [asyncio.create_task(pump(reader, upstream)),
                             asyncio.create_task(pump(upstream_reader, writer))]
                    await asyncio.wait(pumps, return_when=asyncio.FIRST_COMPLETED)
                    for task in pumps:
                        task.cancel()
                    await asyncio.gather(*pumps, return_exceptions=True)
                finally:
                    if upstream:
                        upstream.close()
                        await upstream.wait_closed()
                    writer.close()
                    await writer.wait_closed()

            async with (await asyncio.start_server(model, "127.0.0.1", 0) as server,
                        await asyncio.start_server(socks, "127.0.0.1", 0) as proxy):
                proxy_url = f"socks5h://127.0.0.1:{proxy.sockets[0].getsockname()[1]}"
                env = {**os.environ, "CENTAERIS_DESKTOP_DATA_DIR": str(root),
                       "CENTAERIS_MODEL_CONTEXT_BUDGET_TOKENS": "500000",
                       "CENTAERIS_MODEL_OUTPUT_BUDGET_TOKENS": "64000",
                       "NO_PROXY": "127.0.0.1,localhost,::1", "no_proxy": "127.0.0.1,localhost,::1",
                       "HTTP_PROXY": proxy_url, "http_proxy": proxy_url,
                       "HTTPS_PROXY": proxy_url, "https_proxy": proxy_url,
                       "ALL_PROXY": proxy_url, "all_proxy": proxy_url}
                binary = os.environ["CENTAERIS_RUNTIME_BINARY"]
                endpoint_process = await asyncio.create_subprocess_exec(
                    binary, "--runtime-server-endpoint", env=env,
                    stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE)
                stdout, stderr = await endpoint_process.communicate()
                self.assertEqual(endpoint_process.returncode, 0, stderr.decode())
                endpoint = json.loads(stdout)["endpoint"]
                with (root / "runtime.log").open("wb") as log:
                    runtime = await asyncio.create_subprocess_exec(
                        binary, "--runtime-server", env=env, stdout=log, stderr=log)
                    rpc = None
                    try:
                        rpc = await connect(endpoint, io.StringIO())
                        port = server.sockets[0].getsockname()[1]
                        await rpc.call("agent_runtime_config_set", {"customModelProviders": [{
                            "providerId": "custom.benchmark-mock", "name": "Benchmark mock",
                            "baseUrl": f"http://benchmark-model.invalid:{port}", "api": "openai-completions",
                            "models": [{"model": "mock", "displayName": "Mock", "contextTokens": "1000000",
                                        "maxOutputTokens": "384000", "supportsVision": False}]}]})
                        result = await run_trial(rpc, "Start the background verification service.",
                                                 str(root), "mock", "custom.benchmark-mock", "max", 25,
                                                 api_key="local-mock-only")
                        self.assertEqual(result["status"], "succeeded", result)
                        self.assertGreaterEqual(len(requests), 2)
                        self.assertEqual(proxy_domains, ["benchmark-model.invalid"] * len(requests))
                        for request in requests:
                            self.assertEqual(request["max_tokens"], 64000)
                            self.assertEqual(request["reasoning_effort"], "max")
                        processes = await rpc.call("process_session_list", {"sessionId": result["sessionId"]})
                        self.assertEqual(len(processes["processes"]), 1)
                        self.assertIsNone(runtime.returncode)
                        # A nonblocking FIFO writer can open only while the service is alive.
                        descriptor = os.open(fifo, os.O_WRONLY | os.O_NONBLOCK)
                        os.close(descriptor)
                    finally:
                        if rpc:
                            await rpc.call("runtime/shutdown", {}, timeout=12)
                            await rpc.close()
                        try:
                            await asyncio.wait_for(runtime.wait(), 15)
                        except TimeoutError:
                            runtime.kill()
                            await runtime.wait()


if __name__ == "__main__":
    unittest.main()
