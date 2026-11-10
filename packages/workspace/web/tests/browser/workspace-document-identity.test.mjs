import assert from "node:assert/strict";
import test from "node:test";
import { mkdtemp, realpath, rm } from "node:fs/promises";
import { basename, dirname, join } from "node:path";
import { tmpdir } from "node:os";
import { fileURLToPath } from "node:url";
import { createServer } from "vite";
import { runBrowser, waitFor } from "./browserHarness.mjs";

test("Workspace tab persists across Agent, Session and settings navigation and restores the platform head on exit", async () => {
  const server = await createServer({ root: fileURLToPath(new URL("../..", import.meta.url)),
    server: { host: "127.0.0.1", port: 0, strictPort: false }, optimizeDeps: { entries: ["tests/browser/workspace-document-identity.html"] },
  });
  const profiles = [];
  try {
    await server.listen();
    for (const withoutIcon of [false, true]) {
      const profile = await mkdtemp(join(tmpdir(), "centaeris-workspace-tab-")); profiles.push(profile);
      await runBrowser(`${server.resolvedUrls.local[0]}tests/browser/workspace-document-identity.html${withoutIcon ? "?withoutIcon" : ""}`, { width: 980 }, profile, async ({ evaluate, frame }) => {
        const click = async label => { await evaluate(`Array.from(document.querySelectorAll('button')).find(button => button.textContent===${JSON.stringify(label)}).click()`); await frame(); };
        const identity = () => evaluate(`({ title: document.title, icons: Array.from(document.querySelectorAll('link[rel="icon"]')).map(icon => ({ href: icon.getAttribute('href'), type: icon.getAttribute('type') })), apple: document.querySelector('link[rel="apple-touch-icon"]').getAttribute('href') })`);
        await waitFor(() => evaluate("Boolean(document.querySelector('button'))"), "identity controls");
        const original = await identity();
        await click("Open workspace");
        const initialHref = (await identity()).icons[0].href;
        assert.ok(initialHref.startsWith("data:image/svg+xml,"));
        assert.ok(decodeURIComponent(initialHref).includes(">R</text>"));
        const research = { title: "Research workspace", icons: [{ href: initialHref, type: "image/svg+xml" }], apple: "/centaeris-mark.png" };
        assert.deepEqual(await identity(), research);
        for (const page of ["Second Agent", "Session", "Settings", "Renamed Agent with custom avatar", "First Agent"]) {
          await click(page);
          assert.equal(await evaluate("document.querySelector('h1')?.textContent"), page);
          assert.deepEqual(await identity(), research, `${page} keeps the Workspace tab identity`);
        }
        await click("Rename workspace"); assert.deepEqual(await identity(), { ...research, title: "Renamed workspace" });
        await click("Switch workspace");
        const build = await identity();
        assert.equal(build.title, "Build workspace");
        assert.ok(decodeURIComponent(build.icons[0].href).includes(">B</text>"));
        assert.equal(build.icons.length, 1);
        await click("Leave workspace"); assert.deepEqual(await identity(), original);
        await click("Open workspace"); assert.deepEqual(await identity(), research);
        await click("Leave workspace"); assert.deepEqual(await identity(), original);
      });
    }
  } finally {
    await server.close();
    for (const profile of profiles) {
      const resolved = await realpath(profile); assert.equal(dirname(resolved).toLowerCase(), (await realpath(tmpdir())).toLowerCase()); assert.ok(basename(resolved).startsWith("centaeris-workspace-tab-"));
      await rm(resolved, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 });
    }
  }
});
