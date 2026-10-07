import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, realpath, rm, mkdir, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, dirname, basename } from "node:path";
import { fileURLToPath } from "node:url";
import { createServer } from "vite";
import { runBrowser, waitFor } from "./browserHarness.mjs";

for (const entry of [
  { name: "agent-chat-route", completed: "all Agent route checks completed", widths: [1280, 390, 820, 2048] },
  { name: "agent-model-settings", completed: "all Agent Settings checks completed", widths: [1280, 390] },
  { name: "conversation-layout", completed: "all conversation layout checks completed", widths: [1280, 390, 820, 2048] },
]) for (const theme of ["light", "dark"]) for (const width of entry.widths) test(`${entry.name} (${theme}, ${width}px)`, async () => {
  assert.ok(process.env.CHROME_BIN, "Set CHROME_BIN to a Chromium executable");
  const profile = await mkdtemp(join(tmpdir(), "centaeris-agent-route-"));
  const server = await createServer({ root: fileURLToPath(new URL("../..", import.meta.url)), server: { port: 0, host: "127.0.0.1", strictPort: false } });
  try {
    await server.listen();
    const url = `${server.resolvedUrls.local[0]}tests/browser/${entry.name}.html?theme=${theme}`;
    await runBrowser(url, { width }, profile, async ({ evaluate, send, frame, press, browserVersion }) => {
      const payload = await waitFor(() => evaluate('document.getElementById("results")?.textContent'), "route assertions");
      const results = JSON.parse(payload);
      assert.ok(results.some(result => result.name === entry.completed), JSON.stringify(results));
      assert.deepEqual(results.filter(result => JSON.stringify(result.actual) !== JSON.stringify(result.expected)), []);
      if (entry.name === "conversation-layout" && width === 2048) for (const sidebar of ["visible", "hidden"]) {
        await send("Page.navigate", { url: `${url}&review=1&sidebar=${sidebar}` });
        await waitFor(() => evaluate('document.getElementById("results")?.textContent.includes("synthetic resize surface ready")'), "synthetic resize surface");
        const geometry = () => evaluate('import("/tests/browser/layoutGeometry.ts").then(module => module.waitForLayout([".workspaceContextPanel", ".workspaceComposer", ".workspaceTranscriptBlocks"]))');
        const panelWidth = () => evaluate('document.querySelector(".workspaceContextPanel").getBoundingClientRect().width');
        await evaluate('document.querySelector("[role=separator]").focus()');
        await press("ArrowLeft"); await geometry();
        assert.equal(await panelWidth(), 286, "real keyboard resize expands the preview from its actual width");
        const handle = await evaluate('(() => { const rect = document.querySelector("[role=separator]").getBoundingClientRect(); return {x: rect.right - 2, y: rect.top + rect.height / 2}; })()');
        assert.equal(await evaluate(`document.elementFromPoint(${handle.x}, ${handle.y})?.getAttribute("role")`), "separator", "the production drag handle has an accessible hit target");
        await evaluate('window.fixturePointerEvents = []; window.fixturePointerErrors = []; window.addEventListener("error", event => window.fixturePointerErrors.push(event.message)); for (const type of ["pointerdown", "pointermove", "pointerup"]) document.querySelector("[role=separator]").addEventListener(type, event => {window.fixturePointerId=event.pointerId; window.fixturePointerEvents.push({type, x:event.clientX, buttons:event.buttons, captured:event.currentTarget.hasPointerCapture(event.pointerId)})})');
        await send("Input.dispatchMouseEvent", { type: "mouseMoved", x: handle.x, y: handle.y });
        await send("Input.dispatchMouseEvent", { type: "mousePressed", x: handle.x, y: handle.y, button: "left", buttons: 1, clickCount: 1 });
        await frame();
        assert.equal(await evaluate('document.querySelector("[role=separator]").hasPointerCapture(window.fixturePointerId)'), true, `drag captures the active pointer: ${JSON.stringify(await evaluate("window.fixturePointerErrors"))}`);
        await send("Input.dispatchMouseEvent", { type: "mouseMoved", x: handle.x - 64, y: handle.y, button: "left", buttons: 1 });
        await send("Input.dispatchMouseEvent", { type: "mouseReleased", x: handle.x - 64, y: handle.y, button: "left", clickCount: 1 });
        await geometry();
        assert.equal(await panelWidth(), 350, `real pointer drag preserves adjustable preview width: ${JSON.stringify(await evaluate("window.fixturePointerEvents"))}`);
        assert.equal(await evaluate('Number(document.querySelector("[role=separator]").getAttribute("aria-valuenow"))'), 350, "ARIA follows the rendered pointer-resized width");
        await evaluate('document.querySelector("[role=separator]").focus()');
        for (let index = 0; index < 8; index++) { await press("ArrowRight"); await geometry(); }
        assert.equal(await panelWidth(), 270, "keyboard resize stops at the navigation minimum");
        assert.equal(await evaluate('document.querySelector(".workspaceComposer textarea").value'), "Synthetic unsent text", "real resize retains the unsent draft");
        const dragStart = await evaluate('(() => { const rect = document.querySelector("[role=separator]").getBoundingClientRect(); return {x: rect.right - 2, y: rect.top + rect.height / 2}; })()');
        await send("Input.dispatchMouseEvent", { type: "mousePressed", ...dragStart, button: "left", buttons: 1, clickCount: 1 });
        await send("Input.dispatchMouseEvent", { type: "mouseMoved", x: dragStart.x - 1400, y: dragStart.y, button: "left", buttons: 1 });
        await send("Input.dispatchMouseEvent", { type: "mouseReleased", x: dragStart.x - 1400, y: dragStart.y, button: "left", clickCount: 1 });
        await geometry();
        const max = sidebar === "visible" ? 1298 : 1536;
        assert.equal(await panelWidth(), max, "oversized pointer drag preserves the minimum conversation region");
        await send("Emulation.setDeviceMetricsOverride", { width: 1280, height: 900, deviceScaleFactor: 1, mobile: false }); await geometry();
        assert.equal(await panelWidth(), sidebar === "visible" ? 530 : 800, "viewport shrink clips the preferred width to preserve reading space");
        assert.equal(await evaluate('document.querySelector(".workspaceChatColumn").getBoundingClientRect().width >= 480'), true, "viewport shrink retains usable conversation width");
        assert.equal(await evaluate('document.documentElement.scrollWidth <= innerWidth && document.querySelector(".workspaceComposer").getBoundingClientRect().bottom <= innerHeight'), true, "viewport shrink keeps the draft on screen without horizontal overflow");
        await send("Emulation.setDeviceMetricsOverride", { width, height: 900, deviceScaleFactor: 1, mobile: false }); await geometry();
        assert.equal(await panelWidth(), max, "expanding the viewport restores the chosen width");
        results.push({ name: `${sidebar} navigation: real keyboard/pointer resize and viewport shrink preserve width limits and draft`, actual: true, expected: true });
      }
      const evidence = process.env.AGENT_CHAT_EVIDENCE_DIR;
      if (evidence) {
        await mkdir(evidence, { recursive: true });
        await writeFile(join(evidence, `${entry.name}-${theme}-${width}-assertions.json`), JSON.stringify({ width, browserVersion, results }, null, 2));
        if (entry.name === "agent-chat-route") {
          await send("Page.navigate", { url: `${url}&review=1` });
          await waitFor(() => evaluate('Boolean(document.querySelector(".agentChatPreviewDock .agentChatOverviewHeader"))'), "review overview");
          await evaluate("document.fonts.ready.then(() => true)"); await frame();
          const { data } = await send("Page.captureScreenshot", { format: "png" });
          await writeFile(join(evidence, `${entry.name}-${theme}-${width}-overview.png`), Buffer.from(data, "base64"));
        } else if (entry.name === "conversation-layout") {
          await send("Page.navigate", { url: `${url}&review=1` });
          await waitFor(() => evaluate('document.getElementById("results")?.textContent.includes("synthetic resize surface ready")'), "Session layout review");
          const { data } = await send("Page.captureScreenshot", { format: "png" });
          await writeFile(join(evidence, `${entry.name}-${theme}-${width}-preview.png`), Buffer.from(data, "base64"));
        }
      }
    });
  } finally {
    await server.close();
    const resolved = await realpath(profile);
    assert.equal(dirname(resolved).toLowerCase(), (await realpath(tmpdir())).toLowerCase());
    assert.ok(basename(resolved).startsWith("centaeris-agent-route-"));
    await rm(resolved, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 });
  }
});
