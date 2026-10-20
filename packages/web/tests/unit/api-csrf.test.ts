import assert from "node:assert/strict";
import { test } from "node:test";
import { ApiError, apiJson, clearCsrfToken, configureApi, jsonOptions } from "../../src/api.ts";

test("a CSRF rejection lets the next user attempt fetch a fresh token without replaying the rejected write", async t => {
  clearCsrfToken();
  configureApi({ apiBaseUrl: "https://app.example" });
  t.after(clearCsrfToken);
  let reads = 0;
  const writes: string[] = [];
  t.mock.method(globalThis, "fetch", async (url: string, options: RequestInit) => {
    if (url.endsWith("/api/csrf")) return Response.json({ csrfToken: ++reads === 1 ? "old-test-token" : "fresh-test-token" });
    writes.push(new Headers(options.headers).get("X-CSRFToken") || "");
    return writes.length === 1 ? Response.json({ error: "csrf_failed" }, { status: 403 }) : Response.json({ ok: true });
  });
  await assert.rejects(apiJson("/api/login", jsonOptions("POST", {})), error => error instanceof ApiError && error.status === 403 && error.message === "csrf_failed");
  assert.deepEqual(writes, ["old-test-token"]);
  assert.deepEqual(await apiJson("/api/login", jsonOptions("POST", {})), { ok: true });
  assert.deepEqual(writes, ["old-test-token", "fresh-test-token"]);
  assert.equal(reads, 2);
});

test("an ordinary permission rejection keeps the valid CSRF token for the next request", async t => {
  clearCsrfToken();
  configureApi({ apiBaseUrl: "https://app.example" });
  t.after(clearCsrfToken);
  let reads = 0;
  const writes: string[] = [];
  t.mock.method(globalThis, "fetch", async (url: string, options: RequestInit) => {
    if (url.endsWith("/api/csrf")) { reads++; return Response.json({ csrfToken: "valid-test-token" }); }
    writes.push(new Headers(options.headers).get("X-CSRFToken") || "");
    return writes.length === 1 ? Response.json({ error: "permission_denied" }, { status: 403 }) : Response.json({ ok: true });
  });
  await assert.rejects(apiJson("/api/protected-action", jsonOptions("POST", {})), error => error instanceof ApiError && error.message === "permission_denied");
  await apiJson("/api/protected-action", jsonOptions("POST", {}));
  assert.equal(reads, 1);
  assert.deepEqual(writes, ["valid-test-token", "valid-test-token"]);
});
