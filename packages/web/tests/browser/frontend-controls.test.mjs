import assert from "node:assert/strict";
import test from "node:test";
import { mkdtemp, realpath, rm } from "node:fs/promises";
import { basename, dirname, join } from "node:path";
import { tmpdir } from "node:os";
import { fileURLToPath } from "node:url";
import { createServer } from "vite";
import { runBrowser, waitFor } from "./browserHarness.mjs";

test("shared controls retain Web composer proportions, floating menus, draft and confirmation", async () => {
  const server = await createServer({ root: fileURLToPath(new URL("../..", import.meta.url)), server: { host: "127.0.0.1", port: 0 } });
  const profiles = [];
  try {
    await server.listen();
    for (const scenario of [{ width: 1280, theme: "light" }, { width: 390, theme: "dark", reduced: true }]) {
      const profile = await mkdtemp(join(tmpdir(), "centaeris-frontend-controls-"));
      profiles.push(profile);
      await runBrowser(`${server.resolvedUrls.local[0]}tests/browser/frontend-controls.html?theme=${scenario.theme}`, scenario, profile, async ({ evaluate, frame, press, send }) => {
        const click = async selector => {
          const point = await evaluate(`(() => { const bounds=document.querySelector(${JSON.stringify(selector)}).getBoundingClientRect(); return {x:bounds.left+bounds.width/2,y:bounds.top+bounds.height/2}; })()`);
          await send("Input.dispatchMouseEvent", { type: "mousePressed", button: "left", clickCount: 1, ...point });
          await send("Input.dispatchMouseEvent", { type: "mouseReleased", button: "left", clickCount: 1, ...point });
          await frame();
        };
        await waitFor(() => evaluate("Boolean(document.querySelector('.workspaceComposerThinking button'))"), "composer controls");
        const proportions = await evaluate(`(() => {
          const add = document.querySelector('.workspaceComposerIconButton');
          const effort = document.querySelector('.workspaceComposerThinking button');
          const pendingIcon = document.querySelector('.workspaceComposerControl.isPending svg');
          return { add: add.getBoundingClientRect().width, effort: effort.getBoundingClientRect().width,
            effortIcon: effort.querySelector('svg').getBoundingClientRect().width, pendingIcon: pendingIcon.getBoundingClientRect().width };
        })()`);
        assert.equal(proportions.effort, proportions.add, "effort retains the same reserved width as adjacent icon controls");
        assert.equal(proportions.effortIcon, proportions.pendingIcon, "effort retains the supporting-control icon scale");
        await click(".workspaceComposerModel button");
        await waitFor(() => evaluate("Boolean(document.querySelector('[role=menu]'))"), "model menu");
        const floating = await evaluate(`(() => {
          const trigger = document.querySelector('.workspaceComposerModel button').getBoundingClientRect();
          const menu = document.querySelector('[role=menu]').getBoundingClientRect();
          return { visible: menu.width > 0 && menu.height > 0, above: menu.bottom <= trigger.top,
            contained: menu.left >= 0 && menu.right <= innerWidth && menu.top >= 0 };
        })()`);
        assert.deepEqual(floating, { visible: true, above: true, contained: true });
        await click("[role=menuitemradio]:last-child");
        await waitFor(() => evaluate("!document.querySelector('[role=menu]')"), "choice closes only the model menu");
        await click(".workspaceComposerThinking button");
        await waitFor(() => evaluate("Boolean(document.querySelector('[role=menu]'))"), "separate effort menu");
        await press("Escape");
        await waitFor(() => evaluate("document.activeElement === document.querySelector('.workspaceComposerThinking button')"), "focus returns to effort trigger");
        assert.deepEqual(await evaluate("frontendControlsFixture.choices"), ["b"]);
        assert.equal(await evaluate("document.querySelector('.workspaceComposer textarea').value"), "Retained Web draft");
        await click(".workspaceMessages button");
        await waitFor(() => evaluate("Boolean(document.querySelector('[role=dialog]'))"), "confirmation");
        await frame();
        const dialog = await evaluate(`(() => {
          const bounds = document.querySelector('[role=dialog]').getBoundingClientRect();
          return { visible: bounds.width > 0 && bounds.height > 0,
            centered: Math.abs(bounds.left + bounds.width / 2 - innerWidth / 2) <= 1,
            contained: bounds.left >= 0 && bounds.right <= innerWidth };
        })()`);
        assert.deepEqual(dialog, { visible: true, centered: true, contained: true });
        await press("Escape");
        await waitFor(() => evaluate("!document.querySelector('[role=dialog]')"), "confirmation dismissal");
        assert.deepEqual(await evaluate("frontendControlsFixture.failures"), []);
      });
    }
  } finally {
    await server.close();
    for (const profile of profiles) {
      const resolved = await realpath(profile);
      assert.equal(dirname(resolved).toLowerCase(), (await realpath(tmpdir())).toLowerCase());
      assert.ok(basename(resolved).startsWith("centaeris-frontend-controls-"));
      await rm(resolved, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 });
    }
  }
});
