import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, rm, realpath } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, dirname, basename } from "node:path";
import { fileURLToPath } from "node:url";
import { createServer } from "vite";
import { runBrowser, waitFor } from "./browserHarness.mjs";

for (const scenario of [{ width: 1280, theme: "light", locale: "en" }, { width: 390, theme: "dark", locale: "zh-CN" }]) test(`Agent attachments reach accepted input and safe preview at ${scenario.width}`, async () => {
  const profile = await mkdtemp(join(tmpdir(), "centaeris-agent-attachments-"));
  const server = await createServer({ root: fileURLToPath(new URL("../..", import.meta.url)), cacheDir: join(profile, "vite-cache"), server: { port: 0, host: "127.0.0.1", strictPort: false }, plugins: [{ name: "isolated-attachment-image", configureServer(server) { server.middlewares.use((request, response, next) => {
    if (!request.url?.startsWith("/api/agents/") || !request.url.includes("/attachments/image/preview")) return next();
    response.setHeader("Content-Type", "image/png");
    response.end(Buffer.from("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+a4ZsAAAAASUVORK5CYII=", "base64"));
  }); } }] });
  try {
    await server.listen();
    await runBrowser(`${server.resolvedUrls.local[0]}tests/browser/agent-input-attachments.html?theme=${scenario.theme}&locale=${scenario.locale}`, { ...scenario, reduced: true }, profile, async ({ evaluate, press }) => {
      await waitFor(() => evaluate('document.querySelector("input[type=file]") && !document.querySelector("input[type=file]").disabled'), "attachment control");
      assert.equal(await evaluate('document.querySelector("button[type=submit]").disabled'), true);
      await evaluate(() => { const input = document.querySelector("input[type=file]"); const transfer = new DataTransfer(); transfer.items.add(new File(["fixture image"], "鍥剧墖.png", { type: "image/png" })); input.files = transfer.files; input.dispatchEvent(new Event("change", { bubbles: true })); });
      await waitFor(() => evaluate('document.querySelector(".agentInputComposer .attachmentCard img")?.complete && !document.querySelector("button[type=submit]").disabled'), "uploaded image preview and image-only send");
      assert.match(await evaluate('document.querySelector(".attachmentCard img").src'), /\/api\/agents\/fixture-agent\/attachments\/image\/preview\?lang=(en|zh-CN)$/);
      await evaluate('document.querySelector(".attachmentCardContent").click()');
      await waitFor(() => evaluate('document.querySelector("[role=dialog] > img")?.src.startsWith("blob:")'), "captured image preview");
      await waitFor(() => evaluate('Boolean(document.activeElement.closest("[role=dialog]"))'), "preview receives keyboard focus");
      await press("Escape");
      await waitFor(() => evaluate('!document.querySelector("[role=dialog]")'), "close preview");
      await evaluate('document.querySelector("button[type=submit]").click()');
      await waitFor(() => evaluate('window.attachmentFixture.posts.length === 1 && Boolean(document.querySelector(".agentChatMessage .attachmentCard img"))'), "accepted input attachment");
      const post = await evaluate('window.attachmentFixture.posts[0]'); assert.equal(post.body, ""); assert.deepEqual(post.attachmentRefs, ["image"]);
      assert.equal(await evaluate('document.querySelector("button[type=submit]").disabled'), true);
      assert.match(await evaluate('document.querySelector(".agentChatMessage .attachmentCard img").src'), /\/inputs\/input%3A[^/]+\/attachments\/image\/preview\?lang=(en|zh-CN)$/);
      const materialLabel = scenario.locale === "en" ? "Choose from library" : "浠庢潗鏂欏簱閫夋嫨";
      assert.equal(await evaluate(`Boolean(document.querySelector('button[aria-label=${JSON.stringify(materialLabel)}]'))`), false, "the unfinished library choice is hidden like the main Session composer");
      assert.equal(await evaluate('Boolean(document.querySelector(".agentAttachmentPicker"))'), false);
      assert.equal(await evaluate('document.documentElement.scrollWidth <= innerWidth'), true, "attachments fit narrow viewport");
    });
  } finally {
    await server.close(); const resolved = await realpath(profile);
    assert.equal(dirname(resolved).toLowerCase(), (await realpath(tmpdir())).toLowerCase()); assert.ok(basename(resolved).startsWith("centaeris-agent-attachments-"));
    await rm(resolved, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 });
  }
});
