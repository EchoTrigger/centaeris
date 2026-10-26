import assert from "node:assert/strict";
import test from "node:test";
import { mkdtemp, realpath, rm, writeFile } from "node:fs/promises";
import { basename, dirname, join } from "node:path";
import { tmpdir } from "node:os";
import { fileURLToPath } from "node:url";
import { createServer } from "vite";
import { runBrowser, waitFor } from "./browserHarness.mjs";

for (const scenario of [{ width: 1280, theme: "light" }, { width: 390, theme: "dark", contentOnly: true }]) {
  test(`workspace resource settings remain styled and usable (${scenario.theme}, ${scenario.width}px)`, async () => {
    const server = await createServer({
      root: fileURLToPath(new URL("../..", import.meta.url)),
      server: { host: "127.0.0.1", port: 0, strictPort: false },
      optimizeDeps: { entries: ["tests/browser/workspace-resource-settings.html"] },
      // No confirmation is open in this layout fixture; isolate its unrelated UI dependencies.
      plugins: [{
        name: "resource-settings-closed-dialog", enforce: "pre",
        resolveId(source) { if (source === "../components/ConfirmDialog") return "\0resource-settings-closed-dialog"; },
        load(id) { if (id === "\0resource-settings-closed-dialog") return "export const ConfirmDialog=()=>null;"; },
      }],
    });
    const profile = await mkdtemp(join(tmpdir(), "centaeris-resource-settings-"));
    try {
      await server.listen();
      const parameters = new URLSearchParams({ theme: scenario.theme, ...(scenario.contentOnly ? { contentOnly: "true" } : {}) });
      await runBrowser(`${server.resolvedUrls.local[0]}tests/browser/workspace-resource-settings.html?${parameters}`, scenario, profile, async ({ evaluate, frame, send }) => {
        await waitFor(() => evaluate("Boolean(document.querySelector('select option[value=synthetic]'))"), "workspace selector");
        await evaluate("(() => { const select=document.querySelector('select'); select.value='synthetic'; select.dispatchEvent(new Event('change',{bubbles:true})); })()");
        await waitFor(() => evaluate("Array.from(document.querySelectorAll('button')).some(button => button.textContent==='Plugins')"), "resource navigation");
        await evaluate("Array.from(document.querySelectorAll('button')).find(button => button.textContent==='Plugins').click()");
        await waitFor(() => evaluate("document.querySelector('[role=list]')?.children.length===24"), "workspace plugin catalog");
        await frame();
        if (process.env.CENTAERIS_TEST_SCREENSHOT_DIR) {
          const screenshot = await send("Page.captureScreenshot", { format: "png" });
          await writeFile(join(process.env.CENTAERIS_TEST_SCREENSHOT_DIR, `workspace-resource-settings-${scenario.theme}-${scenario.width}.png`), Buffer.from(screenshot.data, "base64"));
        }
        const layout = await evaluate(`(() => {
          const select=document.querySelector('select'), heading=document.querySelector('h1');
          const content=document.querySelector('.workspaceSettingsContent'), root=heading.closest('section');
          const upload=Array.from(document.querySelectorAll('button')).find(button => button.textContent.includes('Upload plugin'));
          const hints=Array.from(upload.parentElement.querySelectorAll('small'));
          const buttons=Array.from(document.querySelectorAll('button'));
          const active=buttons.find(button => button.textContent==='Plugins'), inactive=buttons.find(button => button.textContent==='Models');
          const bounds=element=>element.getBoundingClientRect();
          const text=document.createRange();text.selectNodeContents(upload);
          let scrollable=false;
          for(let element=document.querySelector('[role=list]');element && element!==document.body;element=element.parentElement){
            if(element.scrollHeight>element.clientHeight && /auto|scroll/.test(getComputedStyle(element).overflowY)){
              element.scrollTop=element.scrollHeight;scrollable=element.scrollTop>0;break;
            }
          }
          return {
            headingInset:bounds(heading).left>bounds(content).left,
            selectorSeparateFromHeading:bounds(select).top>=bounds(heading).bottom,
            selectedTabDistinct:getComputedStyle(active).backgroundColor!==getComputedStyle(inactive).backgroundColor || getComputedStyle(active).borderBottomColor!==getComputedStyle(inactive).borderBottomColor,
            pluginTabSelected:active.getAttribute('aria-pressed')==='true',
            hintsBelowButton:hints.length===2 && hints.every(hint=>bounds(hint).top>=bounds(upload).bottom),
            uploadTextStaysOnOneLine:text.getClientRects().length<=2,
            contained:root.scrollWidth<=root.clientWidth+1 && content.scrollWidth<=content.clientWidth+1,
            catalogCanScroll:scrollable,
          };
        })()`);
        assert.deepEqual(layout, {
          headingInset: true, selectorSeparateFromHeading: true, selectedTabDistinct: true,
          pluginTabSelected: true, hintsBelowButton: true, uploadTextStaysOnOneLine: true,
          contained: true, catalogCanScroll: true,
        }, "resource controls need readable spacing, selected state, stacked upload guidance and scroll access");
        await evaluate("Array.from(document.querySelectorAll('button')).find(button => button.textContent==='Models').click()");
        await waitFor(() => evaluate("Array.from(document.querySelectorAll('button')).some(button => button.textContent.includes('Add provider'))"), "model configuration panel");
        assert.equal(await evaluate("Array.from(document.querySelectorAll('button')).find(button => button.textContent==='Models').getAttribute('aria-pressed')"), "true");
        assert.deepEqual(await evaluate("workspaceResourceSettingsFixture.failures"), []);
      });
    } finally {
      await server.close();
      const resolved = await realpath(profile);
      assert.equal(dirname(resolved).toLowerCase(), (await realpath(tmpdir())).toLowerCase());
      assert.ok(basename(resolved).startsWith("centaeris-resource-settings-"));
      await rm(resolved, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 });
    }
  });
}
