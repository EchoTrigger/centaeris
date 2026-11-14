import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, realpath, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, dirname, basename } from "node:path";
import { fileURLToPath } from "node:url";
import { createServer } from "vite";
import { runBrowser, waitFor } from "./browserHarness.mjs";

const scenarios = [{ width: 1280, theme: "light" }, { width: 1280, theme: "dark" }, { width: 390, theme: "dark" }];
const longOnly = process.env.MARKDOWN_BROWSER_LONG_ONLY === "1";
for (const scenario of scenarios.filter(value => !longOnly || (value.width === 1280 && value.theme === "dark"))) test("Web Markdown acceptance (" + scenario.theme + ", " + scenario.width + "px)", async () => {
  const profile = await mkdtemp(join(tmpdir(), "centaeris-markdown-ui-"));
  const server = await createServer({ root: fileURLToPath(new URL("../..", import.meta.url)), server: { port: 0, host: "127.0.0.1", strictPort: false } });
  try {
    await server.listen();
    await runBrowser(server.resolvedUrls.local[0] + "tests/browser/markdown-content.html?theme=" + scenario.theme, scenario, profile, async ({ evaluate, send, frame, press }) => {
      const click = async expression => {
        const point = await evaluate("(() => { const element = " + expression + "; element.scrollIntoView({block:'center'}); const r=element.getBoundingClientRect(); return {x:r.x+r.width/2,y:r.y+r.height/2}; })()");
        await send("Input.dispatchMouseEvent", { type: "mousePressed", button: "left", clickCount: 1, ...point });
        await send("Input.dispatchMouseEvent", { type: "mouseReleased", button: "left", clickCount: 1, ...point });
        await frame();
      };
      const diagramButton = label => "Array.from(document.querySelectorAll('[aria-label=\"Diagram example\"] button')).find(button => button.textContent === " + JSON.stringify(label) + ")";
      const assertDiagramSize = async () => {
        const size = await evaluate(() => {
          const svg = document.querySelector('[aria-label="Diagram example"] [role=img] > svg');
          const container = svg.parentElement;
          const style = getComputedStyle(container);
          const bounds = svg.getBoundingClientRect();
          return { width: bounds.width, height: bounds.height,
            available: container.clientWidth - parseFloat(style.paddingLeft) - parseFloat(style.paddingRight),
            ratio: svg.viewBox.baseVal.height / svg.viewBox.baseVal.width };
        });
        assert.ok(size.available > 0 && Math.abs(size.width - size.available) <= 1, "the diagram fills its content area instead of inheriting icon dimensions");
        assert.ok(Math.abs(size.height - size.width * size.ratio) <= 1, "the diagram preserves its viewBox aspect ratio");
      };
      if (!longOnly) {
      try {
        await waitFor(() => evaluate("Boolean((document.querySelector('[aria-label=\"Diagram example\"] [role=img] svg') || document.querySelector('[aria-label=\"Diagram example\"] [role=status]')) && document.querySelectorAll('[aria-label=\"Math example\"] math').length === 2)"), "actual Mermaid and KaTeX rendering");
        assert.equal(await evaluate("Boolean(document.querySelector('[aria-label=\"Diagram example\"] [role=img] svg'))"), true, "a valid Mermaid graph renders an SVG");
      } catch (error) {
        const renderer = await evaluate(async () => {
          try {
            const module = await import("/src/chat/markdown/renderMermaid.ts");
            const svg = await module.renderMermaid("graph TD\n  A[Start] --> B[Done]", false, () => false);
            return { success: Boolean(svg) };
          } catch (cause) { return { error: String(cause), stack: cause.stack }; }
        });
        const diagnostics = await evaluate(() => ({
          failures: window.markdownFixture?.failures,
          mathCount: document.querySelectorAll('[aria-label="Math example"] math').length,
          diagram: document.querySelector('[aria-label="Diagram example"]')?.textContent,
          body: document.body.innerText.slice(0, 1800),
          vite: document.querySelector("vite-error-overlay")?.shadowRoot?.textContent.slice(0, 1800),
        }));
        throw new Error(error.message + ": " + JSON.stringify({ ...diagnostics, renderer }));
      }
      await evaluate("document.fonts.ready");
      await assertDiagramSize();
      assert.deepEqual(await evaluate(() => {
        const section = document.querySelector('[aria-label="AST example"]');
        return {
          nested: Boolean(section.querySelector("ul > li > ul > li > ul > li")),
          bold: section.querySelector("ul strong")?.textContent,
          reference: section.querySelector('a[href="https://example.com/docs"]')?.textContent,
          cell: section.querySelector("tbody td")?.textContent,
          tasks: [...section.querySelectorAll('input[type="checkbox"]')].map(input => input.checked),
          visual: section.textContent.includes("Run A") && section.textContent.includes("Run B"),
          htmlEscaped: section.textContent.includes("<script>window.markdownUnsafe = true</script>") && section.querySelector("script") === null,
          unsafe: window.markdownUnsafe === true,
        };
      }), { nested: true, bold: "bold", reference: "Guide", cell: "a|b", tasks: [true, false], visual: true, htmlEscaped: true, unsafe: false });
      await waitFor(() => evaluate(() => {
        const spans = [...document.querySelectorAll('[aria-label="Code example"] pre code span')];
        return new Set(spans.map(span => getComputedStyle(span).color)).size > 1;
      }), "actual static code syntax colors");
      assert.equal(await evaluate(() => document.querySelector('[aria-label="Code example"] pre code').textContent), 'const message = "流式🙂";\nconsole.log(message);');
      assert.equal(await evaluate(() => [...document.querySelectorAll('[aria-label="Code example"] mark')].map(mark => mark.textContent).join("")), "message =");
      await click("document.querySelector('[aria-label=\"Code example\"] button[aria-label]')");
      assert.equal(await evaluate("window.markdownFixture.copied.at(-1)"), 'const message = "流式🙂";\nconsole.log(message);');
      assert.deepEqual(await evaluate(() => [...document.querySelectorAll('[aria-label="Math example"] math annotation')].map(node => node.textContent)), ["E=mc^2", "\\frac{a}{b}"]);
      assert.equal(await evaluate(() => document.querySelector('[aria-label="Math example"]').textContent.includes("Cost $5 and $10.")), true);
      await click(diagramButton("Source"));
      assert.equal(await evaluate(() => document.querySelector('[aria-label="Diagram example"] pre code').textContent), "graph TD\n  A[Start] --> B[Done]");
      await click(diagramButton("Extend valid diagram"));
      await waitFor(() => evaluate(() => document.querySelector('[aria-label="Diagram example"] pre code')?.textContent.includes("C[Next]")), "source selection survives streamed content");
      assert.equal(await evaluate(() => [...document.querySelectorAll('[aria-label="Diagram example"] button')].find(button => button.textContent === "Source").getAttribute("aria-pressed")), "true");
      await click(diagramButton("Diagram"));
      await waitFor(() => evaluate(() => document.querySelector('[aria-label="Diagram example"] [role=img]')?.textContent.includes("Next")), "extended diagram");
      await click(diagramButton("Append incomplete diagram"));
      await frame();
      assert.equal(await evaluate(() => document.querySelector('[aria-label="Diagram example"] [role=img]')?.textContent.includes("Next")), true, "unfinished append retains its last valid diagram");
      await click(diagramButton("Finish invalid diagram"));
      await waitFor(() => evaluate(() => Boolean(document.querySelector('[aria-label="Diagram example"] [role=status]'))), "final Mermaid error");
      assert.equal(await evaluate(() => document.querySelector('[aria-label="Diagram example"] pre code').textContent), "graph TD\n  A[Start] --> B[Done]\n  B --> C[Next]\n  C --> D[");
      assert.equal(await evaluate(() => document.querySelector('[aria-label="Diagram example"] [role=img]') === null), true, "a stale valid graph cannot stand in for an invalid final result");
      await click(diagramButton("Replace diagram"));
      await waitFor(() => evaluate(() => document.querySelector('[aria-label="Diagram example"] [role=img]')?.textContent.includes("Replacement")), "replacement diagram");
      await assertDiagramSize();
      await evaluate(() => { const before = document.getElementById("before-return"); before.scrollIntoView({ block: "center" }); before.focus(); });
      await press("Tab");
      assert.equal(await evaluate(() => document.activeElement?.getAttribute("aria-label")), "Return to latest");
      const control = await evaluate(() => {
        const button = document.querySelector('button[aria-label="Return to latest"]');
        const style = getComputedStyle(button);
        const luminance = color => {
          const channels = color.match(/[\d.]+/g).slice(0, 3).map(value => Number(value) / 255).map(value => value <= .04045 ? value / 12.92 : ((value + .055) / 1.055) ** 2.4);
          return channels[0] * .2126 + channels[1] * .7152 + channels[2] * .0722;
        };
        const foreground = luminance(style.color), background = luminance(style.backgroundColor);
        const bounds = button.getBoundingClientRect();
        return { contrast: (Math.max(foreground, background) + .05) / (Math.min(foreground, background) + .05),
          focusVisible: button.matches(":focus-visible") && style.outlineStyle !== "none",
          outlined: style.borderTopStyle !== "none" && parseFloat(style.borderTopWidth) > 0,
          visible: bounds.width > 0 && bounds.height > 0 && bounds.left >= 0 && bounds.right <= innerWidth };
      });
      assert.ok(control.contrast >= 3, "return arrow is readable against its actual surface");
      assert.equal(control.focusVisible && control.outlined && control.visible, true);
      await press("Enter");
      assert.equal(await evaluate(() => document.querySelector('[aria-label="Return control example"] [role=status]').textContent), "Returned to latest");
      }
      if (scenario.width === 1280 && scenario.theme === "dark") {
        const longButton = label => "Array.from(document.querySelectorAll('[aria-label=\"Long response example\"] button')).find(button => button.textContent === " + JSON.stringify(label) + ")";
        await waitFor(() => evaluate("Boolean(document.querySelector('[aria-label=\"Long response example\"] button'))"), "long response controls");
        await click(longButton("Load long response"));
        await waitFor(() => evaluate("Boolean(document.querySelector('[aria-label=\"Long response example\"] [aria-busy=true]'))"), "large message parsing off the main thread");
        await evaluate(() => document.querySelector('[aria-label="Draft while reading"]').focus());
        await send("Input.insertText", { text: "Draft remains usable" });
        await frame();
        assert.equal(await evaluate(() => document.querySelector('[aria-label="Draft while reading"]').value), "Draft remains usable");
        await click(longButton("Record reading action"));
        assert.equal(await evaluate(() => document.querySelector('[aria-label="Reading action count"]').textContent), "1");
        await click(longButton("Finalize long response"));
        try {
          await waitFor(() => evaluate(() => {
            const region = document.querySelector('[aria-label="Long response example"]');
            return !region.querySelector('[aria-busy="true"]') && region.querySelector("h2")?.textContent === "Long response"
              && region.querySelector("strong")?.textContent === "Long response complete." && region.textContent.includes("Entity © &.");
          }), "complete one-MiB AST rendering");
        } catch (error) {
          const diagnostics = await evaluate(() => ({
            urls: markdownFixture.workerUrls, errors: markdownFixture.workerErrors,
            receipts: markdownFixture.workerReceipts, terminations: markdownFixture.workerTerminations,
            busy: Boolean(document.querySelector('[aria-label="Long response example"] [aria-busy=true]')),
            status: document.querySelector('[aria-label="Long response example"] [role=status]')?.textContent,
          }));
          throw new Error(error.message + ": " + JSON.stringify(diagnostics));
        }
        const timing = await evaluate(() => ({ ...markdownFixture.timings, completeMs: performance.now() - markdownFixture.timings.longStarted }));
        console.log("One-MiB browser fixture timing (includes first Worker startup): " + JSON.stringify(timing));
        const receipts = await evaluate("({ receipts: markdownFixture.workerReceipts, terminations: markdownFixture.workerTerminations })");
        assert.ok(receipts.receipts > 0, "the real parsing Worker delivered a result");
        await click(longButton("Start revised long response"));
        await waitFor(() => evaluate("Boolean(document.querySelector('[aria-label=\"Long response example\"] [aria-busy=true]'))"), "revised large parse");
        await click(longButton("Replace long response"));
        await waitFor(() => evaluate(() => document.querySelector('[aria-label="Long response example"] strong')?.textContent === "Corrected final response."), "short final correction");
        await waitFor(async () => {
          const current = await evaluate("({ receipts: markdownFixture.workerReceipts, terminations: markdownFixture.workerTerminations })");
          return current.receipts > receipts.receipts || current.terminations > receipts.terminations;
        }, "previous large parse completed or was cancelled");
        await frame();
        assert.equal(await evaluate(() => document.querySelector('[aria-label="Long response example"] strong').textContent), "Corrected final response.");
        assert.equal(await evaluate(() => document.querySelector('[aria-label="Draft while reading"]').value), "Draft remains usable");
      }
      assert.equal(await evaluate(() => document.documentElement.scrollWidth <= innerWidth), true, "reading content does not overflow the viewport");
      assert.deepEqual(await evaluate("window.markdownFixture.failures"), []);
    });
  } finally {
    await server.close();
    const resolved = await realpath(profile);
    assert.equal(dirname(resolved).toLowerCase(), (await realpath(tmpdir())).toLowerCase());
    assert.ok(basename(resolved).startsWith("centaeris-markdown-ui-"));
    await rm(resolved, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 });
  }
});
