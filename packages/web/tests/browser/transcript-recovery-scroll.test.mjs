import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, realpath, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, dirname, basename } from "node:path";
import { fileURLToPath } from "node:url";
import { createServer } from "vite";
import { runBrowser, waitFor } from "./browserHarness.mjs";

for (const scenario of [
  { width: 1280, height: 900, theme: "light", language: "en" },
  { width: 390, height: 844, theme: "dark", language: "zh-CN", reduced: true },
]) test(`atomic transcript recovery preserves reading and latest (${scenario.width}px, ${scenario.theme})`, async () => {
  const profile = await mkdtemp(join(tmpdir(), "centaeris-transcript-recovery-"));
  const server = await createServer({ root: fileURLToPath(new URL("../..", import.meta.url)), server: { port: 0, host: "127.0.0.1", strictPort: false } });
  try {
    await server.listen();
    const url = `${server.resolvedUrls.local[0]}tests/browser/transcript-recovery-scroll.html?${new URLSearchParams({ language: scenario.language, theme: scenario.theme })}`;
    await runBrowser(url, scenario, profile, async ({ evaluate, send, frame }) => {
      await waitFor(() => evaluate("window.transcriptRecoveryFixture?.ready"), "synthetic recovery fixture");
      const geometry = () => evaluate("transcriptRecoveryFixture.geometry()");
      assert.ok((await geometry()).bottomDistance <= 2, "initial conversation follows latest");
      const point = await evaluate("(() => { const r = document.querySelector('.workspaceMessages').getBoundingClientRect(); return { x: r.left + r.width / 2, y: r.top + r.height / 2 }; })()");
      await send("Input.dispatchMouseEvent", { type: "mouseWheel", ...point, deltaX: 0, deltaY: -3000 });
      await waitFor(async () => !(await geometry()).following, "real upward wheel pauses following");
      await evaluate("transcriptRecoveryFixture.alignReading()");
      const before = await geometry();
      assert.equal(before.blockId, "user-12", "the reader is in the middle of an already loaded older page");
      assert.ok(Math.abs(before.offset) <= 1, `known reading offset: ${before.offset}`);
      assert.equal(before.draft, await evaluate("transcriptRecoveryFixture.draft"));
      await evaluate("transcriptRecoveryFixture.recover(2)");
      await evaluate("transcriptRecoveryFixture.settle()");
      const after = await geometry();
      console.log(JSON.stringify({ viewport: scenario.width, before, after }));
      assert.equal(after.generation, "generation-2");
      assert.equal(after.revision, "2");
      assert.equal(after.count, 48, "atomic replacement retains the complete loaded range");
      assert.ok(after.aboveHeight > before.aboveHeight + 100, "authoritative revision changes body height above the reader");
      assert.equal(after.blockId, before.blockId, "recovery preserves the actual first visible message");
      assert.ok(Math.abs(after.offset - before.offset) <= 1, `reading pixel anchor moved ${after.offset - before.offset}px`);
      assert.equal(after.draft, before.draft, "atomic recovery retains the unsent draft");
      assert.equal(after.following, false, "recovery does not force the reader to the latest message");
      await evaluate("document.querySelector('.workspaceJumpToLatest').click()");
      await waitFor(async () => Math.abs((await geometry()).bottomDistance) <= 2, "jump reaches latest");
      await frame();
      await evaluate("transcriptRecoveryFixture.settle()");
      const beforeLatest = await geometry();
      await evaluate("transcriptRecoveryFixture.recover(3)");
      await evaluate("transcriptRecoveryFixture.settle()");
      const latest = await geometry();
      console.log(JSON.stringify({ viewport: scenario.width, beforeLatest, latest }));
      assert.equal(latest.generation, "generation-3");
      assert.ok(latest.latestHeight > beforeLatest.latestHeight + 100, "latest recovery grows the actual final answer");
      assert.equal(latest.following, true, "recovery keeps latest following active");
      assert.ok(Math.abs(latest.bottomDistance) <= 2, `latest recovery leaves ${latest.bottomDistance}px to bottom`);
      assert.equal(latest.draft, before.draft);
      assert.deepEqual(await evaluate("transcriptRecoveryFixture.failures"), []);
      assert.deepEqual([...new Set(await evaluate("transcriptRecoveryFixture.requests"))], ["/api/sessions/synthetic-recovery/transcript/turn-metadata"], "fixture makes only intercepted synthetic metadata reads");
    });
  } finally {
    await server.close();
    const resolved = await realpath(profile);
    assert.equal(dirname(resolved).toLowerCase(), (await realpath(tmpdir())).toLowerCase());
    assert.ok(basename(resolved).startsWith("centaeris-transcript-recovery-"));
    await rm(resolved, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 });
  }
});
