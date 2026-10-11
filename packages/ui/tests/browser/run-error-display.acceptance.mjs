import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, realpath, rm, mkdir, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, dirname, basename } from "node:path";
import { fileURLToPath } from "node:url";
import { createServer } from "vite";
import { runBrowser, waitFor } from "../../../web/tests/browser/browserHarness.mjs";

for (const width of [736, 320]) for (const theme of ["light", "dark"]) test(`run error wrapping and first-line icon alignment (${width}, ${theme})`, async () => {
  const profile = await mkdtemp(join(tmpdir(), "centaeris-desktop-error-"));
  const server = await createServer({ root: fileURLToPath(new URL("../..", import.meta.url)), server: { port: 0, host: "127.0.0.1" } });
  try {
    await server.listen();
    await runBrowser(`${server.resolvedUrls.local[0]}tests/browser/run-error-display.html?theme=${theme}`, { width }, profile, async ({ evaluate, frame, send }) => {
      await waitFor(() => evaluate('document.querySelectorAll("[role=status]").length === 3'), "run error rendering");
      for (const zoom of [1, 1.5]) {
        await evaluate(`document.body.style.zoom = ${zoom}`);
        await frame();
        const geometry = await evaluate(() => [...document.querySelectorAll("[role=status]")].map(row => {
          const text = row.lastElementChild;
          const icon = row.querySelector("svg").getBoundingClientRect();
          const box = text.getBoundingClientRect();
          const scale = Number(getComputedStyle(document.body).zoom);
          const lineHeight = parseFloat(getComputedStyle(text).lineHeight) * scale;
          return { offset: Math.abs(icon.y + icon.height / 2 - box.y - lineHeight / 2),
            fits: row.getBoundingClientRect().right <= innerWidth && row.scrollWidth <= row.clientWidth,
            square: Math.abs(icon.width - icon.height) < 1, wraps: box.height > lineHeight + 1,
            text: text.textContent, markup: text.children.length };
        }));
        for (const row of geometry) {
          assert.ok(row.offset < 1, "icon aligns to first line");
          assert.ok(row.fits, "error stays within the reading area");
          assert.ok(row.square, "wrapping does not distort the icon");
          assert.equal(row.markup, 0, "reason stays plain text");
        }
        assert.ok(geometry[0].text.includes("provider_timeout"));
        assert.ok(geometry[2].wraps, "long errors wrap");
        if (zoom === 1) {
          const output = fileURLToPath(new URL("../../../../test-results/run-error-display/", import.meta.url));
          await mkdir(output, { recursive: true });
          const shot = await send("Page.captureScreenshot", { format: "png" });
          await writeFile(join(output, `desktop-${theme}-${width}.png`), Buffer.from(shot.data, "base64"));
        }
      }
    });
  } finally {
    await server.close();
    const resolved = await realpath(profile);
    assert.equal(dirname(resolved).toLowerCase(), (await realpath(tmpdir())).toLowerCase());
    assert.ok(basename(resolved).startsWith("centaeris-desktop-error-"));
    await rm(resolved, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 });
  }
});
