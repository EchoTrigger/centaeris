import assert from "node:assert/strict";
import test from "node:test";
import { mkdtemp, realpath, rm } from "node:fs/promises";
import { basename, dirname, join } from "node:path";
import { tmpdir } from "node:os";
import { fileURLToPath } from "node:url";
import { createServer } from "vite";
import { runBrowser, waitFor } from "./browserHarness.mjs";

const png = Buffer.from("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+j6VQAAAAASUVORK5CYII=", "base64");

test("Agent tab follows saved name and avatar, rejects stale image completion and restores the original head", async () => {
  const server = await createServer({ root: fileURLToPath(new URL("../..", import.meta.url)),
    server: { host: "127.0.0.1", port: 0, strictPort: false }, optimizeDeps: { entries: ["tests/browser/agent-document-identity.html"] },
    plugins: [{ name: "synthetic-avatar-response", configureServer(vite) {
      vite.middlewares.use("/api/agents/fixture/avatar", (request, response) => {
        const version = new URL(request.url, "http://fixture.invalid").searchParams.get("v");
        const send = () => { response.statusCode = version === "missing" ? 404 : 200; response.setHeader("Content-Type", "image/png"); response.end(version === "missing" ? "" : png); };
        if (version === "slow") setTimeout(send, 250); else send();
      });
    } }],
  });
  const profiles = [];
  try {
    await server.listen();
    for (const withoutIcon of [false, true]) {
      const profile = await mkdtemp(join(tmpdir(), "centaeris-agent-tab-")); profiles.push(profile);
      await runBrowser(`${server.resolvedUrls.local[0]}tests/browser/agent-document-identity.html${withoutIcon ? "?withoutIcon" : ""}`, { width: 980 }, profile, async ({ evaluate, frame }) => {
        const click = async label => { await evaluate(`Array.from(document.querySelectorAll('button')).find(button => button.textContent===${JSON.stringify(label)}).click()`); await frame(); };
        const identity = () => evaluate(`({ title: document.title, icons: Array.from(document.querySelectorAll('link[rel="icon"]')).map(icon => ({ href: icon.getAttribute('href'), type: icon.getAttribute('type') })), apple: document.querySelector('link[rel="apple-touch-icon"]').getAttribute('href') })`);
        await waitFor(() => evaluate("Boolean(document.querySelector('button'))"), "identity controls");
        const original = await identity();
        await click("First Agent");
        assert.deepEqual(await identity(), { title: "Researcher · Centaeris", icons: [{ href: "/centaeris-mark.png", type: "image/png" }], apple: "/centaeris-mark.png" });
        await click("Rename Agent"); assert.equal((await identity()).title, "Renamed researcher · Centaeris");
        await click("Custom avatar");
        await waitFor(async () => (await identity()).icons[0]?.href.endsWith("?v=valid"), "saved custom avatar");
        assert.equal((await identity()).icons.length, 1, "using a custom avatar does not add duplicate favicon links");
        await click("Broken avatar");
        await waitFor(async () => (await identity()).icons[0]?.href === "/centaeris-mark.png", "failed custom avatar fallback");
        await click("Slow avatar"); await click("Second Agent");
        assert.equal((await identity()).title, "Builder · Centaeris");
        await new Promise(resolve => setTimeout(resolve, 350));
        assert.equal((await identity()).icons[0].href, "/agent-avatar-banana.png", "a prior Agent's late image never replaces the current Agent's favicon");
        await click("Leave Agent"); assert.deepEqual(await identity(), original, "leaving restores the original title, favicon and unrelated touch icon");
        await click("First Agent"); await click("New page mounts"); await click("New page unmounts");
        assert.equal((await identity()).title, "Researcher · Centaeris", "closing an overlaid page restores the still active Agent beneath it");
        await click("New page mounts"); await click("Leave Agent");
        assert.equal((await identity()).title, "Builder · Centaeris", "an old page cleanup cannot overwrite the newly mounted Agent");
        await click("New page unmounts"); assert.deepEqual(await identity(), original);
      });
    }
  } finally {
    await server.close();
    for (const profile of profiles) {
      const resolved = await realpath(profile); assert.equal(dirname(resolved).toLowerCase(), (await realpath(tmpdir())).toLowerCase()); assert.ok(basename(resolved).startsWith("centaeris-agent-tab-"));
      await rm(resolved, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 });
    }
  }
});
