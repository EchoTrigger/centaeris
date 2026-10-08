import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, realpath, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, dirname, basename } from "node:path";
import { fileURLToPath } from "node:url";
import { createServer } from "vite";
import { runBrowser, waitFor } from "./browserHarness.mjs";

for (const width of [736, 320]) for (const theme of ["light", "dark"]) test(`error strip icon alignment (${width}px, ${theme})`, async () => {
  const profile = await mkdtemp(join(tmpdir(), "centaeris-error-strip-"));
  const server = await createServer({ root: fileURLToPath(new URL("../..", import.meta.url)), server: { port: 0, host: "127.0.0.1" } });
  try {
    await server.listen();
    await runBrowser(`${server.resolvedUrls.local[0]}tests/browser/model-error-strip.html?theme=${theme}`, { width }, profile, async ({ evaluate, frame }) => {
      await waitFor(() => evaluate('document.querySelectorAll(".workspaceRunError").length === 2'), "error strip render");
      await frame();
      const geometry = await evaluate(() => [...document.querySelectorAll(".workspaceRunError")].map(row => {
        const text = row.querySelector(".workspaceRunErrorText");
        const icon = row.querySelector("svg").getBoundingClientRect();
        const box = text.getBoundingClientRect();
        const lineHeight = parseFloat(getComputedStyle(text).lineHeight);
        return { offset: Math.abs(icon.y + icon.height / 2 - box.y - lineHeight / 2), iconWidth: icon.width,
          fits: row.getBoundingClientRect().right <= innerWidth && row.scrollWidth <= row.clientWidth,
          wraps: box.height > lineHeight + 1, radius: getComputedStyle(row).borderRadius };
      }));
      for (const item of geometry) {
        assert.ok(item.offset < 1, JSON.stringify(item));
        assert.equal(item.iconWidth, 20);
        assert.ok(item.fits);
        assert.ok(parseFloat(item.radius) >= 22);
      }
      if (width === 320) assert.ok(geometry[1].wraps, "long error wraps without moving the icon from the first line");
    });
  } finally {
    await server.close();
    const resolved = await realpath(profile);
    assert.equal(dirname(resolved).toLowerCase(), (await realpath(tmpdir())).toLowerCase());
    assert.ok(basename(resolved).startsWith("centaeris-error-strip-"));
    await rm(resolved, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 });
  }
});
