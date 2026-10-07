import assert from "node:assert/strict";
import { test } from "node:test";
import { loadRuntimeConfig } from "../../src/config.ts";

test("same-origin API mode follows each explicit browser entry point", async () => {
  const originalFetch = globalThis.fetch;
  const originalWindow = globalThis.window;
  try {
    globalThis.fetch = async () => new Response(JSON.stringify({ apiBaseUrl: "/" }));
    for (const origin of ["http://localhost:6767", "http://127.0.0.1:6767"]) {
      globalThis.window = { location: { origin } } as Window & typeof globalThis;
      assert.deepEqual(await loadRuntimeConfig(), { apiBaseUrl: origin });
    }
    globalThis.fetch = async () => new Response(JSON.stringify({ apiBaseUrl: "https://api.example.test/" }));
    assert.deepEqual(await loadRuntimeConfig(), { apiBaseUrl: "https://api.example.test" });
  } finally {
    globalThis.fetch = originalFetch;
    globalThis.window = originalWindow;
  }
});
