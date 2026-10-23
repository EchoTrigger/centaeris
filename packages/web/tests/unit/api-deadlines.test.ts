import assert from "node:assert/strict";
import { createServer } from "node:http";
import { once } from "node:events";
import { test } from "node:test";
import { apiJson, apiResponse, clearCsrfToken, configureApi } from "../../src/api.ts";

async function server(t, handler) {
  const instance = createServer(handler);
  instance.listen(0, "127.0.0.1");
  await once(instance, "listening");
  const address = instance.address();
  configureApi({ apiBaseUrl: `http://127.0.0.1:${address.port}` });
  clearCsrfToken();
  t.after(() => { clearCsrfToken(); instance.closeAllConnections(); instance.close(); });
  return instance;
}

async function boundedFailure(work) {
  let guard;
  try {
    await assert.rejects(Promise.race([
      work,
      new Promise((_, reject) => { guard = setTimeout(() => reject(new Error("request remained pending past its deadline")), 500); }),
    ]), error => error?.name === "TimeoutError" || error?.name === "AbortError");
  } finally { clearTimeout(guard); }
}

test("request headers have a deadline and cancel a silent connection", async t => {
  await server(t, () => {});
  await boundedFailure(apiResponse("/silent", { timeoutMs: 30 }));
});

test("JSON consumption remains bounded after successful response headers", async t => {
  await server(t, (_request, response) => {
    response.writeHead(200, { "Content-Type": "application/json" });
    response.flushHeaders();
    response.write('{"unfinished":');
  });
  await boundedFailure(apiJson("/body", { timeoutMs: 30 }));
});

test("a caller can stop waiting for shared CSRF without cancelling another write", async t => {
  let releaseToken;
  let announceToken;
  const tokenRequested = new Promise(resolve => { announceToken = resolve; });
  const requests = [];
  await server(t, (request, response) => {
    requests.push(request.url);
    response.setHeader("Content-Type", "application/json");
    if (request.url === "/api/csrf") {
      releaseToken = () => response.end(JSON.stringify({ csrfToken: "synthetic-token" }));
      announceToken();
    } else response.end(JSON.stringify({ accepted: true }));
  });
  const cancelled = new AbortController();
  const first = boundedFailure(apiJson("/first", { method: "POST", body: "{}", signal: cancelled.signal }));
  await tokenRequested;
  const second = apiJson("/second", { method: "POST", body: "{}" });
  cancelled.abort();
  await first;
  releaseToken();
  assert.deepEqual(await second, { accepted: true });
  assert.deepEqual(requests, ["/api/csrf", "/second"]);
});

test("streaming requests can delegate lifetime to their heartbeat watchdog", async t => {
  const controller = new AbortController();
  await server(t, (_request, response) => {
    response.writeHead(200, { "Content-Type": "text/event-stream" });
    response.flushHeaders();
    response.write(": connected\n\n");
  });
  const response = await apiResponse("/stream", { timeoutMs: null, signal: controller.signal });
  const reader = response.body.getReader();
  assert.match(new TextDecoder().decode((await reader.read()).value), /connected/);
  const waiting = reader.read();
  controller.abort();
  await assert.rejects(waiting, { name: "AbortError" });
  reader.releaseLock();
});