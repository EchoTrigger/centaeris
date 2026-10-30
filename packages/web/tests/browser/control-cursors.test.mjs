import assert from "node:assert/strict";
import test from "node:test";
import { mkdtemp, realpath, rm } from "node:fs/promises";
import { basename, dirname, join } from "node:path";
import { tmpdir } from "node:os";
import { fileURLToPath, pathToFileURL } from "node:url";
import { createRequire } from "node:module";
import { createServer } from "vite";
import { runBrowser, waitFor } from "./browserHarness.mjs";

test("Web, desktop and shared unavailable controls use ordinary cursors", async () => {
  const resolveDesktop = createRequire(new URL("../../../ui/package.json", import.meta.url));
  const { default: tailwindcss } = await import(pathToFileURL(resolveDesktop.resolve("@tailwindcss/vite")));
  const profile = await mkdtemp(join(tmpdir(), "centaeris-control-cursors-"));
  const server = await createServer({ root: fileURLToPath(new URL("../..", import.meta.url)), cacheDir: join(profile, "vite-cache"), plugins: [tailwindcss()], server: { host: "127.0.0.1", port: 0, strictPort: false } });
  try {
    await server.listen();
    await runBrowser(`${server.resolvedUrls.local[0]}tests/browser/control-cursors.html`, { width: 1280 }, profile, async ({ evaluate }) => {
      await waitFor(() => evaluate("document.querySelectorAll('[data-cursor-sample]').length === 15"), "control samples");
      const cursors = await evaluate("Array.from(document.querySelectorAll('[data-cursor-sample]'), element => ({ sample: element.dataset.cursorSample, cursor: getComputedStyle(element).cursor }))");
      assert.deepEqual(cursors.filter(item => ["not-allowed", "no-drop"].includes(item.cursor)), []);
    });
  } finally {
    await server.close();
    const resolved = await realpath(profile);
    assert.equal(dirname(resolved).toLowerCase(), (await realpath(tmpdir())).toLowerCase());
    assert.ok(basename(resolved).startsWith("centaeris-control-cursors-"));
    await rm(resolved, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 });
  }
});
