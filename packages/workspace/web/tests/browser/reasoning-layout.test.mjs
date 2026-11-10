import assert from "node:assert/strict";
import test from "node:test";
import { mkdtemp, realpath, rm, writeFile } from "node:fs/promises";
import { basename, dirname, join } from "node:path";
import { tmpdir } from "node:os";
import { fileURLToPath } from "node:url";
import { createServer } from "vite";
import { runBrowser, waitFor } from "./browserHarness.mjs";

// The title must remain a compact disclosure. A longer preview may shrink and
// truncate, but must not displace its arrow or clip the preview from the left.
const geometry = kind => `(() => {
  const section = document.querySelector('[data-case="${kind}"]');
  const button = section.querySelector('button');
  const label = [...button.querySelectorAll('span')].find(element => !element.getAttribute('aria-hidden') && element.parentElement === button);
  const preview = button.querySelector('[aria-hidden="true"] span');
  const arrow = button.querySelector('svg:last-of-type');
  const bounds = element => { const { left, right, top, bottom, width, height } = element.getBoundingClientRect(); return { left, right, top, bottom, width, height }; };
  const text = preview?.firstChild;
  const first = text && document.createRange();
  if (first) { first.setStart(text, 0); first.setEnd(text, 1); }
  return { section: bounds(section), button: bounds(button), label: bounds(label), arrow: bounds(arrow),
    labelClipped: label.scrollWidth > label.clientWidth,
    preview: preview ? bounds(preview) : null, firstGlyph: first ? bounds(first) : null,
    previewOverflow: preview ? preview.scrollWidth > preview.clientWidth : false,
    ellipsis: preview ? getComputedStyle(preview).textOverflow : null,
    expanded: button.getAttribute('aria-expanded') };
})()`;

test("reasoning keeps title and arrow together while long previews truncate at their end", async () => {
  const server = await createServer({ root: fileURLToPath(new URL("../..", import.meta.url)), server: { host: "127.0.0.1", port: 0 } });
  const profiles = [];
  try {
    await server.listen();
    for (const scenario of [{ width: 1280, theme: "light", language: "zh-CN" }, { width: 390, theme: "dark", language: "en", reduced: true }]) {
      const profile = await mkdtemp(join(tmpdir(), "centaeris-reasoning-layout-"));
      profiles.push(profile);
      await runBrowser(`${server.resolvedUrls.local[0]}tests/browser/reasoning-layout.html?${new URLSearchParams(scenario)}`, scenario, profile, async ({ evaluate, frame, press, send }) => {
        await waitFor(() => evaluate("Boolean(document.querySelector('[data-case=long] button'))"), "reasoning disclosure");
        await evaluate("document.fonts.ready.then(() => true)");
        await frame();
        for (const kind of ["short", "long", "stream"]) {
          const value = await evaluate(geometry(kind));
          assert.equal(value.labelClipped, false, "the complete disclosure title remains readable");
          assert.ok(value.button.right <= value.section.right + 1, "the summary fits the transcript width");
          assert.ok(value.arrow.left >= value.label.right && value.arrow.left - value.label.right < value.label.height,
            "the disclosure arrow stays beside its title, independently of preview length");
          assert.ok(value.preview.left >= value.arrow.right, "preview text begins after the complete disclosure title and arrow");
          assert.ok(value.firstGlyph.left >= value.preview.left - 1 && value.firstGlyph.right <= value.preview.right + 1,
            "the first preview character remains visible instead of being clipped from the left");
          if (kind === "long") {
            assert.equal(value.previewOverflow, true, "a long preview is constrained to available width");
            assert.equal(value.ellipsis, "ellipsis", "the constrained preview communicates truncation at its end");
          } else {
            assert.ok(value.button.width < value.section.width, "short reasoning summaries use only their content width");
          }
        }
        const before = await evaluate(geometry("stream"));
        await evaluate("window.reasoningButton = document.querySelector('[data-case=stream] button'); reasoningLayoutFixture.append()");
        await frame();
        const after = await evaluate(geometry("stream"));
        assert.equal(await evaluate("window.reasoningButton === document.querySelector('[data-case=stream] button')"), true, "streaming retains the mounted disclosure");
        assert.equal(after.label.left, before.label.left, "streaming does not move the title");
        assert.equal(after.arrow.left, before.arrow.left, "streaming does not move the disclosure arrow");
        const collapsed = await evaluate(geometry("long"));
        const pointer = { x: collapsed.button.left + collapsed.button.width / 2, y: collapsed.button.top + collapsed.button.height / 2 };
        await send("Input.dispatchMouseEvent", { type: "mousePressed", button: "left", clickCount: 1, ...pointer });
        await send("Input.dispatchMouseEvent", { type: "mouseReleased", button: "left", clickCount: 1, ...pointer });
        await frame();
        assert.equal(await evaluate(`document.elementFromPoint(${pointer.x}, ${pointer.y})?.closest('button') === document.querySelector('[data-case=long] button')`), true,
          "opening retains the clicked disclosure area so the same mouse position can close it");
        const detail = await evaluate(`(() => {
          const section = document.querySelector('[data-case=long]');
          const button = section.querySelector('button');
          const body = section.querySelector('[role=region]');
          const bounds = body.getBoundingClientRect();
          return { expanded: button.getAttribute('aria-expanded'), text: body.textContent,
            below: bounds.top >= button.getBoundingClientRect().bottom,
            fits: bounds.left >= section.getBoundingClientRect().left && bounds.right <= section.getBoundingClientRect().right + 1,
            overflowingPage: document.documentElement.scrollWidth > innerWidth };
        })()`);
        assert.equal(detail.expanded, "true");
        assert.equal(detail.text, await evaluate("reasoningLayoutFixture.longText"));
        assert.equal(detail.below, true, "expanded reasoning opens below its summary");
        assert.equal(detail.fits, true, "expanded reasoning remains within the transcript width");
        assert.equal(detail.overflowingPage, false, "reasoning never widens the page");
        await press("Enter");
        assert.equal((await evaluate(geometry("long"))).expanded, "false", "keyboard activation closes the same disclosure");
        if (process.env.REASONING_LAYOUT_SCREENSHOT) {
          await evaluate("document.activeElement?.blur()");
          const { data } = await send("Page.captureScreenshot", { format: "png" });
          await writeFile(`${process.env.REASONING_LAYOUT_SCREENSHOT}-${scenario.width}.png`, Buffer.from(data, "base64"));
        }
        assert.deepEqual(await evaluate("reasoningLayoutFixture.failures"), []);
      });
    }
  } finally {
    await server.close();
    for (const profile of profiles) {
      const resolved = await realpath(profile);
      assert.equal(dirname(resolved).toLowerCase(), (await realpath(tmpdir())).toLowerCase());
      assert.ok(basename(resolved).startsWith("centaeris-reasoning-layout-"));
      await rm(resolved, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 });
    }
  }
});
