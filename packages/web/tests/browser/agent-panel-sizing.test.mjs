import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, realpath, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, dirname, basename } from "node:path";
import { fileURLToPath } from "node:url";
import { createServer } from "vite";
import { runBrowser, waitFor } from "./browserHarness.mjs";

for (const [width, height] of [[1280, 900], [390, 900], [1280, 480], [390, 480]]) test(`Agent panel natural height, references and collapse at ${width}x${height}`, async () => {
  const profile = await mkdtemp(join(tmpdir(), "centaeris-agent-panel-"));
  const server = await createServer({ root: fileURLToPath(new URL("../..", import.meta.url)), cacheDir: join(profile, "vite-cache"), server: { port: 0, host: "127.0.0.1", strictPort: false } });
  try {
    await server.listen();
    await runBrowser(`${server.resolvedUrls.local[0]}tests/browser/agent-panel-sizing.html`, { width, height, reduced: true }, profile, async ({ evaluate }) => {
      const results = JSON.parse(await waitFor(() => evaluate('document.getElementById("results")?.textContent'), "panel sizing assertions"));
      assert.ok(results.some(r => r.name === "all panel sizing checks completed"));
      assert.deepEqual(results.filter(r => JSON.stringify(r.actual) !== JSON.stringify(r.expected)), []);
    });
  } finally {
    await server.close();
    const resolved = await realpath(profile);
    assert.equal(dirname(resolved).toLowerCase(), (await realpath(tmpdir())).toLowerCase());
    assert.ok(basename(resolved).startsWith("centaeris-agent-panel-"));
    await rm(resolved, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 });
  }
});
