import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, realpath, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, dirname, basename } from "node:path";
import { fileURLToPath } from "node:url";
import { createServer } from "vite";
import { runBrowser, waitFor } from "./browserHarness.mjs";

for (const { width, locale } of [{ width: 1280, locale: "en-US" }, { width: 390, locale: "zh-CN" }]) for (const reduced of [false, true]) test(`chat presentation (${width}px, ${locale}, reduced motion: ${reduced})`, async () => {
  assert.ok(process.env.CHROME_BIN, "Set CHROME_BIN to a Chromium executable");
  const profile = await mkdtemp(join(tmpdir(), "centaeris-chat-ui-"));
  const server = await createServer({ root: fileURLToPath(new URL("../..", import.meta.url)), server: { port: 0, host: "127.0.0.1", strictPort: false } });
  try {
    await server.listen();
    const url = `${server.resolvedUrls.local[0]}tests/browser/chat-presentation.html?${new URLSearchParams({ locale })}`;
    await runBrowser(url, { width, reduced }, profile, async ({ evaluate }) => {
      const payload = await waitFor(() => evaluate('document.getElementById("results")?.textContent'), "chat presentation assertions");
      const results = JSON.parse(payload);
      assert.ok(results.length >= 95);
      assert.ok(results.some(result => result.name === "all chat presentation checks completed"));
      const failures = results.filter(result => JSON.stringify(result.actual) !== JSON.stringify(result.expected));
      assert.deepEqual(failures, []);
    });
  } finally {
    await server.close();
    const resolvedProfile = await realpath(profile);
    assert.equal(dirname(resolvedProfile).toLowerCase(), (await realpath(tmpdir())).toLowerCase());
    assert.ok(basename(resolvedProfile).startsWith("centaeris-chat-ui-"));
    await rm(resolvedProfile, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 });
  }
});

for (const { width, locale } of [{ width: 1280, locale: "en-US" }, { width: 390, locale: "zh-CN" }]) for (const theme of ["light", "dark"]) test(`Agent message Read (${theme}, ${width}px, ${locale})`, async () => {
  const profile = await mkdtemp(join(tmpdir(), "centaeris-message-ui-"));
  const server = await createServer({ root: fileURLToPath(new URL("../..", import.meta.url)), server: { port: 0, host: "127.0.0.1", strictPort: false } });
  try {
    await server.listen();
    const url = `${server.resolvedUrls.local[0]}tests/browser/agent-message-presentation.html?${new URLSearchParams({ locale, theme })}`;
    await runBrowser(url, { width }, profile, async ({ evaluate }) => {
      const payload = await waitFor(() => evaluate('document.getElementById("results")?.textContent'), "Agent Read presentation assertions");
      const results = JSON.parse(payload);
      assert.ok(results.some(result => result.name === "all Agent message presentation checks completed"));
      assert.deepEqual(results.filter(result => JSON.stringify(result.actual) !== JSON.stringify(result.expected)), []);
    });
  } finally {
    await server.close();
    const resolved = await realpath(profile);
    assert.equal(dirname(resolved).toLowerCase(), (await realpath(tmpdir())).toLowerCase());
    assert.ok(basename(resolved).startsWith("centaeris-message-ui-"));
    await rm(resolved, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 });
  }
});
