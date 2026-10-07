import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, realpath, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, dirname, basename } from "node:path";
import { fileURLToPath } from "node:url";
import { createServer } from "vite";
import { runBrowser, waitFor } from "./browserHarness.mjs";

for (const closing of [false, true]) test(closing ? "disclosure closure does not repaint released content" : "manual disclosures expand below their clicked headers", async () => {
const root = fileURLToPath(new URL("../..", import.meta.url));
const server = await createServer({ root, server: { host: "127.0.0.1", port: 0 } });
const profiles = [];
try {
  await server.listen();
  for (const scenario of [{ width: 1280, height: 900, theme: "light", language: "en" }, { width: 390, height: 844, theme: "dark", language: "zh-CN", reduced: true }]) {
    const profile = await mkdtemp(join(tmpdir(), "centaeris-disclosure-direction-"));
    profiles.push(profile);
    await runBrowser(`${server.resolvedUrls.local[0]}tests/browser/transcript-disclosure-direction.html?${new URLSearchParams({ theme: scenario.theme, language: scenario.language })}`, scenario, profile, async ({ evaluate, frame, send, press }) => {
      try { await waitFor(() => evaluate("window.disclosureFixture?.ready"), "local production disclosure fixture"); }
      catch (error) { console.log(JSON.stringify(await evaluate("({body:document.body.innerHTML, fixture:window.disclosureFixture?.failures})"))); throw error; }
      if (closing) {
        await evaluate("disclosureFixture.prepare('reasoning')");
        await evaluate("document.querySelector(disclosureFixture.selectors.reasoning).click()");
        await evaluate("disclosureFixture.settle()");
        const end = await evaluate(`new Promise(resolve => {
          const button = document.querySelector(disclosureFixture.selectors.reasoning);
          button.click();
          queueMicrotask(() => {
            const detail = button.nextElementSibling;
            const animation = detail.getAnimations()[0];
            if (animation) animation.finish();
            resolve({ hasAnimation: Boolean(animation), hidden: detail.hidden, inert: detail.inert, ariaHidden: detail.getAttribute('aria-hidden'), opacity: getComputedStyle(detail).opacity, height: detail.getBoundingClientRect().height });
          });
        })`);
        console.log(JSON.stringify({ width: scenario.width, end }));
        assert.ok(end.height <= 1, `finished close repaints ${end.height}px of removed detail before React releases it`);
        assert.ok(end.hidden || Number(end.opacity) === 0, "finished close cannot repaint the old content");
        await evaluate("disclosureFixture.settle()");
        assert.equal(await evaluate("document.querySelector(disclosureFixture.selectors.reasoning).nextElementSibling.childElementCount"), 0);
        if (!scenario.reduced) {
          const reversal = await evaluate(`new Promise(resolve => {
            const button = document.querySelector(disclosureFixture.selectors.reasoning);
            button.click();
            queueMicrotask(() => {
              const detail = button.nextElementSibling;
              const first = detail.getAnimations()[0]; first.currentTime = 80;
              const openingHeight = detail.getBoundingClientRect().height;
              button.click();
              queueMicrotask(() => {
                const second = detail.getAnimations()[0]; second.currentTime = 80;
                const closingHeight = detail.getBoundingClientRect().height;
                button.click();
                queueMicrotask(() => {
                  const currentHeight = detail.getBoundingClientRect().height;
                  resolve({ openingHeight, closingHeight, currentHeight, error: Math.abs(currentHeight - closingHeight) });
                });
              });
            });
          })`);
          console.log(JSON.stringify({ reversal }));
          assert.ok(reversal.error <= 1, "rapid reopening resumes the currently drawn height");
          await evaluate("disclosureFixture.settle()");
          assert.equal(await evaluate("document.querySelector(disclosureFixture.selectors.reasoning).getAttribute('aria-expanded')"), "true");
        }
        assert.deepEqual(await evaluate("disclosureFixture.failures"), []);
        return;
      }
      for (const kind of ["work", "reasoning", "group", "node"]) {
        for (const reading of [false, true]) {
          await evaluate(`disclosureFixture.prepare('${kind}')`);
          if (reading) {
            const point = await evaluate("(() => { const r = document.querySelector('.workspaceMessages').getBoundingClientRect(); return { x: r.left + r.width / 2, y: r.top + r.height / 2 }; })()");
            await send("Input.dispatchMouseEvent", { type: "mouseWheel", ...point, deltaX: 0, deltaY: -24 });
            await waitFor(() => evaluate("document.querySelector('.workspaceJumpToLatest') !== null"), "reader pauses following");
            await evaluate("disclosureFixture.settle()");
          }
          const before = await evaluate(`disclosureFixture.geometry('${kind}')`);
          assert.equal(before.expanded, "false");
          assert.equal(before.following, !reading);
          await evaluate(`(() => {
            const selectors = ['.workspaceAgentRunList', '.workspaceMessages', '.workspaceTranscriptBlocks',
              '.workspaceTranscriptTurn:last-child', disclosureFixture.selectors.work, '[data-block-id="synthetic-answer"]', '.workspaceComposer textarea'];
            const nodes = selectors.map(selector => document.querySelector(selector));
            const removed = [];
            const observer = new MutationObserver(records => {
              for (const record of records) for (const node of record.removedNodes) {
                for (let index = 0; index < nodes.length; index++) if (node === nodes[index] || node.contains(nodes[index])) removed.push(selectors[index]);
              }
            });
            observer.observe(document.getElementById('fixture'), { childList: true, subtree: true });
            window.disclosureOpeningIdentity = { selectors, nodes, removed, observer };
          })()`);
          await evaluate(`disclosureFixture.track('${kind}')`);
          await evaluate(`document.querySelector(disclosureFixture.selectors['${kind}']).focus({preventScroll:true})`);
          let pointer;
          if (scenario.reduced) await press("Enter");
          else {
            pointer = await evaluate(`(() => { const r = document.querySelector(disclosureFixture.selectors['${kind}']).getBoundingClientRect(); return { x: r.left + r.width / 2, y: r.top + r.height / 2 }; })()`);
            await send("Input.dispatchMouseEvent", { type: "mousePressed", button: "left", clickCount: 1, ...pointer });
            await send("Input.dispatchMouseEvent", { type: "mouseReleased", button: "left", clickCount: 1, ...pointer });
          }
          await frame(); await evaluate("disclosureFixture.settle()");
          const after = await evaluate(`disclosureFixture.geometry('${kind}')`);
          const samples = await evaluate("disclosureFrames.stop()");
          const drift = Math.max(...samples.map(sample => Math.abs(sample.viewportY - before.viewportY)), Math.abs(after.viewportY - before.viewportY));
          const parentDrift = Math.max(...samples.map(sample => Math.abs(sample.parentLineY - before.parentLineY)), Math.abs(after.parentLineY - before.parentLineY));
          const result = { width: scenario.width, kind, reading, before, after, movement: after.offset - before.offset, drift, parentDrift, frames: samples.length };
          console.log(JSON.stringify(result));
          assert.equal(after.expanded, "true");
          assert.ok(Math.abs(after.offset - before.offset) <= 1, `${kind} disclosure moved the clicked header ${after.offset - before.offset}px`);
          assert.ok(drift <= 1 && parentDrift <= 1, `${kind} opening keeps its header and work structure line fixed throughout every sampled animation frame`);
          assert.ok(after.detailHeight > 10, `${kind} opens visible detail`);
          assert.ok(after.detailTop >= after.headerBottom - 1, `${kind} detail opens below its header`);
          assert.equal(after.following, false, "explicit reading pauses bottom following");
          const identity = await evaluate(`(() => {
            const { selectors, nodes, removed, observer } = disclosureOpeningIdentity;
            observer.disconnect();
            return { stable: selectors.every((selector, index) => document.querySelector(selector) === nodes[index] && nodes[index].isConnected), removed };
          })()`);
          assert.equal(identity.stable, true, "opening retains mounted conversation roots, work title, final answer and draft input");
          assert.deepEqual(identity.removed, [], "opening never unmounts the conversation's key rows");
          if (pointer) {
            assert.equal(await evaluate(`document.elementFromPoint(${pointer.x}, ${pointer.y})?.closest('button') === document.querySelector(disclosureFixture.selectors['${kind}'])`), true, "the unchanged mouse position still hits the same disclosure");
            await send("Input.dispatchMouseEvent", { type: "mousePressed", button: "left", clickCount: 1, ...pointer });
            await send("Input.dispatchMouseEvent", { type: "mouseReleased", button: "left", clickCount: 1, ...pointer });
          } else await press("Enter");
          await evaluate("disclosureFixture.settle()");
          assert.equal(await evaluate(`document.querySelector(disclosureFixture.selectors['${kind}']).getAttribute('aria-expanded')`), "false", "a second activation at the original position closes the same disclosure");
        }
      }
      assert.deepEqual(await evaluate("disclosureFixture.failures"), []);
      assert.deepEqual([...new Set(await evaluate("disclosureFixture.requests"))].sort(), ["/api/sessions/synthetic-disclosure/transcript/citations", "/api/sessions/synthetic-disclosure/transcript/turn-metadata"]);
    });
  }
} finally {
  await server.close();
  for (const profile of profiles) {
    const resolved = await realpath(profile);
    assert.equal(dirname(resolved).toLowerCase(), (await realpath(tmpdir())).toLowerCase());
    assert.ok(basename(resolved).startsWith("centaeris-disclosure-direction-"));
    await rm(resolved, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 });
  }
}
});
