import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, realpath, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, dirname, basename } from "node:path";
import { fileURLToPath } from "node:url";
import { createServer } from "vite";
import { runBrowser, waitFor } from "./browserHarness.mjs";

test("closing disclosures at the scroll boundary holds the structure line and mounted conversation", async () => {
  const root = fileURLToPath(new URL("../..", import.meta.url));
  const server = await createServer({ root, server: { host: "127.0.0.1", port: 0 } });
  const profiles = [];
  try {
    await server.listen();
    for (const scenario of [
      { width: 1280, height: 900, theme: "light", language: "en" },
      { width: 390, height: 844, theme: "dark", language: "zh-CN", reduced: true },
    ]) {
      const profile = await mkdtemp(join(tmpdir(), "centaeris-disclosure-stationary-"));
      profiles.push(profile);
      const query = new URLSearchParams({ theme: scenario.theme, language: scenario.language, compact: "1" });
      await runBrowser(`${server.resolvedUrls.local[0]}tests/browser/transcript-disclosure-direction.html?${query}`, scenario, profile,
        async ({ evaluate, frame, send, press }) => {
          await waitFor(() => evaluate("window.disclosureFixture?.ready"), "local disclosure fixture");
          const activate = async kind => {
            await evaluate(`document.querySelector(disclosureFixture.selectors['${kind}']).focus({preventScroll:true})`);
            if (scenario.reduced) await press("Enter");
            else {
              const point = await evaluate(`(() => { const r = document.querySelector(disclosureFixture.selectors['${kind}']).getBoundingClientRect(); return { x: r.left + r.width / 2, y: r.top + r.height / 2 }; })()`);
              await send("Input.dispatchMouseEvent", { type: "mousePressed", button: "left", clickCount: 1, ...point });
              await send("Input.dispatchMouseEvent", { type: "mouseReleased", button: "left", clickCount: 1, ...point });
            }
          };
          for (const kind of ["work", "reasoning", "group", "node"]) {
            await evaluate(`disclosureFixture.prepare('${kind}')`);
            await activate(kind);
            await evaluate("disclosureFixture.settle()");
            const regionPoint = await evaluate("(() => { const r = document.querySelector('.workspaceMessages').getBoundingClientRect(); return { x: r.left + r.width / 2, y: r.top + r.height / 2 }; })()");
            await send("Input.dispatchMouseEvent", { type: "mouseWheel", ...regionPoint, deltaX: 0, deltaY: 10000 });
            await waitFor(() => evaluate(`disclosureFixture.geometry('${kind}').bottomDistance <= 1`), "expanded conversation scroll end");
            await evaluate("disclosureFixture.settle()");
            const before = await evaluate(`disclosureFixture.geometry('${kind}')`);
            assert.equal(before.expanded, "true");
            assert.ok(before.detailHeight > 20, "collapse removes enough content to exercise native scroll clamping");
            assert.ok(before.scrollTop > before.maxScroll - before.detailHeight + 10, "reader crossed the collapsed scroll boundary");
            await evaluate(`(() => {
              const selectors = ['#fixture', '.workspaceAgentRunList', '.workspaceMessages', '.workspaceTranscriptBlocks',
                '.workspaceTranscriptTurn:last-child', disclosureFixture.selectors.work,
                '[data-block-id="synthetic-question"]', '[data-block-id="synthetic-answer"]', '.workspaceComposer textarea'];
              const watched = selectors.map(selector => document.querySelector(selector));
              const removed = [];
              const observer = new MutationObserver(records => {
                for (const record of records) for (const node of record.removedNodes) {
                  for (let i = 0; i < watched.length; i++) if (node === watched[i] || node.contains(watched[i])) removed.push(selectors[i]);
                }
              });
              observer.observe(document.getElementById('fixture'), { childList: true, subtree: true });
              window.disclosureIdentity = { selectors, watched, removed, observer };
            })()`);
            await evaluate(`disclosureFixture.track('${kind}')`);
            await activate(kind);
            await frame();
            await evaluate("disclosureFixture.settle()");
            const samples = await evaluate("disclosureFrames.stop()");
            const after = await evaluate(`disclosureFixture.geometry('${kind}')`);
            const identity = await evaluate(`(() => {
              const { selectors, watched, removed, observer } = disclosureIdentity;
              observer.disconnect();
              return { stable: selectors.map((selector, index) => ({ selector, same: document.querySelector(selector) === watched[index], connected: watched[index]?.isConnected })), removed };
            })()`);
            const movement = after.parentLineY - before.parentLineY;
            const maximumMovement = Math.max(...samples.map(sample => Math.abs(sample.parentLineY - before.parentLineY)), Math.abs(movement));
            const maximumHeaderMovement = Math.max(...samples.map(sample => Math.abs(sample.viewportY - before.viewportY)), Math.abs(after.viewportY - before.viewportY));
            console.log(JSON.stringify({ width: scenario.width, kind, before, after, movement, maximumMovement, maximumHeaderMovement, frames: samples.length, identity }));
            assert.equal(after.expanded, "false");
            assert.ok(maximumMovement <= 1, `${kind} close moved the work structure line ${maximumMovement}px`);
            assert.ok(maximumHeaderMovement <= 1, `${kind} close keeps its clicked header fixed throughout the animation`);
            assert.equal(after.detailHeight, 0, "closed detail no longer occupies visible layout");
            assert.equal(after.following, false, "manual closure pauses automatic following");
            assert.equal(after.draft, await evaluate("disclosureFixture.draft"));
            assert.ok(identity.stable.every(node => node.same && node.connected), "conversation roots, title, answer and composer retain their DOM identities");
            assert.deepEqual(identity.removed, [], "disclosure did not remove or recreate conversation roots or key rows");
            await evaluate("document.querySelector('.workspaceJumpToLatest').click()");
            await evaluate("disclosureFixture.settle()");
            const jumped = await evaluate(`disclosureFixture.geometry('${kind}')`);
            assert.equal(jumped.following, true, "jump explicitly resumes following");
            assert.ok(jumped.bottomDistance <= 1, "jump removes the temporary disclosure tail and reaches the real end");
            assert.ok(jumped.scrollTop < after.scrollTop - 10, "jump can reclaim the tail held during collapse");
          }
          assert.deepEqual(await evaluate("disclosureFixture.failures"), []);
        });
    }
  } finally {
    await server.close();
    for (const profile of profiles) {
      const resolved = await realpath(profile);
      assert.equal(dirname(resolved).toLowerCase(), (await realpath(tmpdir())).toLowerCase());
      assert.ok(basename(resolved).startsWith("centaeris-disclosure-stationary-"));
      await rm(resolved, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 });
    }
  }
});

test("nested output scrolling keeps the outer disclosure anchor until the reader scrolls the conversation", async () => {
  const profile = await mkdtemp(join(tmpdir(), "centaeris-disclosure-stationary-"));
  const server = await createServer({ root: fileURLToPath(new URL("../..", import.meta.url)), server: { host: "127.0.0.1", port: 0 } });
  try {
    await server.listen();
    await runBrowser(`${server.resolvedUrls.local[0]}tests/browser/transcript-disclosure-direction.html?noHistory=1&older=1`,
      { width: 1280, height: 900 }, profile, async ({ evaluate, send }) => {
        await waitFor(() => evaluate("window.disclosureFixture?.ready"), "nested local disclosure fixture");
        await evaluate(`disclosureFixture.prepare('node')`);
        await evaluate(`document.querySelector(disclosureFixture.selectors.reasoning).click(); document.querySelector(disclosureFixture.selectors.node).click()`);
        await evaluate("disclosureFixture.settle()");
        const wheelInside = async (selector, direction) => {
          const point = await evaluate(`(() => {
            const inner = document.querySelector('${selector}').getBoundingClientRect();
            const outer = document.querySelector('.workspaceMessages').getBoundingClientRect();
            const top = Math.max(inner.top, outer.top), bottom = Math.min(inner.bottom, outer.bottom);
            if (bottom <= top) throw new Error('Inner output must be visible to receive the wheel');
            return { x: inner.left + inner.width / 2, y: (top + bottom) / 2 };
          })()`);
          await send("Input.dispatchMouseEvent", { type: "mouseWheel", ...point, deltaX: 0, deltaY: direction });
        };
        await evaluate("document.querySelector('.workspaceMessages').scrollTop = 0");
        for (const selector of [".workspaceReasoningBody", ".agent-tool-output-block"]) {
          await evaluate(`document.querySelector('${selector}').scrollTop = 80`);
          const before = await evaluate("disclosureFixture.geometry('node')");
          await wheelInside(selector, -24);
          await waitFor(() => evaluate(`document.querySelector('${selector}').scrollTop < 80`), "wheel scrolls the inner output");
          const after = await evaluate("disclosureFixture.geometry('node')");
          assert.equal(after.scrollTop, before.scrollTop, "inner reading does not scroll the conversation");
          assert.equal(after.parentLineY, before.parentLineY, "inner reading keeps the work structure line in place");
          assert.equal(await evaluate("disclosureFixture.olderRequests()"), 0, "inner upward scrolling at outer top never requests history");
        }
        await evaluate("document.querySelector('.workspaceJumpToLatest').click()");
        await evaluate("disclosureFixture.settle()");
        await evaluate("document.querySelector('.workspaceReasoningBody').scrollTop = 100");
        const beforeClose = await evaluate("disclosureFixture.geometry('node')");
        await evaluate("disclosureFixture.track('node'); document.querySelector(disclosureFixture.selectors.node).click()");
        assert.equal(await evaluate("document.querySelector(disclosureFixture.selectors.node).nextElementSibling.getAnimations().some(animation => animation.playState === 'running')"), true, "inner wheel overlaps a real close animation");
        await wheelInside(".workspaceReasoningBody", -24);
        await waitFor(() => evaluate("document.querySelector('.workspaceReasoningBody').scrollTop < 100"), "inner reader moves during closure");
        await evaluate("disclosureFixture.settle()");
        const afterClose = await evaluate("disclosureFixture.geometry('node')");
        const frames = await evaluate("disclosureFrames.stop()");
        const drift = Math.max(...frames.map(sample => Math.abs(sample.parentLineY - beforeClose.parentLineY)), Math.abs(afterClose.parentLineY - beforeClose.parentLineY));
        console.log(JSON.stringify({ nestedWheel: true, beforeClose, afterClose, frames: frames.length, drift }));
        assert.ok(drift <= 1, "inner wheel does not release the outer close floor or move the work structure line");
        assert.equal(afterClose.scrollTop, beforeClose.scrollTop);
        const innerTop = await evaluate("document.querySelector('.workspaceReasoningBody').scrollTop");
        await wheelInside(".workspaceReasoningBody", 24);
        await waitFor(() => evaluate(`document.querySelector('.workspaceReasoningBody').scrollTop > ${innerTop}`), "inner scrolling remains available after closure");
        assert.equal((await evaluate("disclosureFixture.geometry('node')")).parentLineY, afterClose.parentLineY, "inner wheel after closure retains the held viewport");
        const point = await evaluate("(() => { const r = document.querySelector('.workspaceMessages').getBoundingClientRect(); return { x: r.right - 5, y: r.top + 40 }; })()");
        await send("Input.dispatchMouseEvent", { type: "mouseWheel", ...point, deltaX: 0, deltaY: -80 });
        await waitFor(() => evaluate(`disclosureFixture.geometry('node').scrollTop < ${afterClose.scrollTop}`), "actual outer wheel releases the disclosure anchor");
        await evaluate("disclosureFixture.settle()");
        const reading = await evaluate("disclosureFixture.geometry('node')");
        assert.ok(reading.parentLineY > afterClose.parentLineY + 10, "the user can move the conversation after explicit outer reading");
        assert.equal(reading.following, false);
        assert.equal(reading.draft, await evaluate("disclosureFixture.draft"));
        assert.deepEqual(await evaluate("disclosureFixture.failures"), []);
      });
  } finally {
    await server.close();
    const resolved = await realpath(profile);
    assert.equal(dirname(resolved).toLowerCase(), (await realpath(tmpdir())).toLowerCase());
    assert.ok(basename(resolved).startsWith("centaeris-disclosure-stationary-"));
    await rm(resolved, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 });
  }
});
