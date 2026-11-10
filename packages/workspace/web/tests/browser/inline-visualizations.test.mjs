import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, realpath, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, dirname, basename } from "node:path";
import { fileURLToPath } from "node:url";
import { createServer } from "vite";
import { runBrowser, waitFor } from "./browserHarness.mjs";

for (const width of [1280, 390]) for (const theme of ["light", "dark"]) test(`shared visualizations (${theme}, ${width}px)`, async () => {
  const profile = await mkdtemp(join(tmpdir(), "centaeris-visualization-ui-"));
  const server = await createServer({ root: fileURLToPath(new URL("../..", import.meta.url)), server: { port: 0, host: "127.0.0.1", strictPort: false } });
  try {
    await server.listen();
    await runBrowser(`${server.resolvedUrls.local[0]}tests/browser/inline-visualizations.html?theme=${theme}`, { width }, profile, async ({ evaluate, send, frame }) => {
      await waitFor(() => evaluate('document.querySelectorAll("[data-example] .centaeris-visual").length === 6'), "six accepted Desktop visualization examples");
      assert.deepEqual(await evaluate(() => {
        const normalized = token => { const probe = document.createElement("span"); probe.style.color = `var(${token})`; document.body.append(probe); const color = getComputedStyle(probe).color; probe.remove(); return color; };
        return window.visualizationFixture.examples.flatMap(({ id, values }) => {
          const region = document.querySelector(`[data-example="${id}"]`);
          const root = region.querySelector(".centaeris-visual");
          const card = root.querySelector('[data-slot="card"]');
          const failures = [];
          for (const text of values) {
            if (!region.textContent.includes(text)) failures.push(`${id}: missing ${text}`);
          }
          if (getComputedStyle(root).color !== normalized("--on-surface")) failures.push(`${id}: foreground does not follow theme`);
          if (card && getComputedStyle(card).backgroundColor !== normalized("--surface-color")) failures.push(`${id}: paper does not follow theme`);
          if (root.scrollWidth > root.clientWidth + 1) failures.push(`${id}: horizontal content overflow`);
          return failures;
        });
      }), []);
      assert.equal(await evaluate('document.documentElement.scrollWidth <= innerWidth'), true, "all examples fit the available width");
      assert.equal(await evaluate(() => document.querySelector('[aria-label="Streaming visual"] [role=status]')?.getAttribute("aria-busy")), "true");
      assert.equal(await evaluate(() => document.querySelector('[aria-label="Streaming visual"]').textContent.includes("elements")), false, "unfinished JSON remains private");
      const nodes = await evaluate(() => {
        const sequence = document.querySelector('[data-example="delivery"] ol');
        const items = [...sequence.children];
        return items.map((item, index) => {
          const node = getComputedStyle(item, "::after");
          const guide = getComputedStyle(item, "::before");
          return { roundNode: node.width === node.height && parseFloat(node.width) > 0 && node.borderRadius === "50%", guide: index === items.length - 1 ? guide.content === "none" : parseFloat(guide.borderInlineStartWidth) > 0 };
        });
      });
      assert.ok(nodes.length > 1 && nodes.every(node => node.roundNode && node.guide), "ordered explanation retains nodes and a guide ending at the last heading");
      await evaluate(() => document.querySelector('[aria-label="Streaming visual"] button').click());
      await waitFor(() => evaluate(() => Boolean(document.querySelector('[aria-label="Streaming visual"] ol'))), "completed streamed visual");
      await evaluate(() => { window.completedVisual = document.querySelector('[aria-label="Streaming visual"] ol'); document.querySelectorAll('[aria-label="Streaming visual"] button')[1].click(); });
      await frame();
      assert.equal(await evaluate(() => window.completedVisual === document.querySelector('[aria-label="Streaming visual"] ol')), true, "appending prose keeps completed visual mounted");
      assert.equal(await evaluate(() => document.querySelector('[aria-label="Invalid visual"] script') === null && document.querySelector('[aria-label="Invalid visual"] details pre').textContent.includes("<script>bad()</script>")), true, "invalid data remains escaped and inspectable");
      assert.deepEqual(await evaluate('window.visualizationFixture.failures'), []);
      if (process.env.VISUALIZATION_SCREENSHOT_DIR && width === 1280) {
        const { data } = await send("Page.captureScreenshot", { format: "png", captureBeyondViewport: true, clip: { x: 0, y: 0, width, height: 1600, scale: 1 } });
        await writeFile(join(process.env.VISUALIZATION_SCREENSHOT_DIR, `visualizations-${theme}.png`), Buffer.from(data, "base64"));
      }
    });
  } finally {
    await server.close();
    const resolved = await realpath(profile);
    assert.equal(dirname(resolved).toLowerCase(), (await realpath(tmpdir())).toLowerCase());
    assert.ok(basename(resolved).startsWith("centaeris-visualization-ui-"));
    await rm(resolved, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 });
  }
});
