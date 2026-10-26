import assert from "node:assert/strict";
import test from "node:test";
import { mkdtemp, realpath, rm, writeFile } from "node:fs/promises";
import { basename, dirname, join } from "node:path";
import { tmpdir } from "node:os";
import { fileURLToPath } from "node:url";
import { createServer } from "vite";
import { runBrowser, waitFor } from "./browserHarness.mjs";

const png = Buffer.from("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGNgYGBgAAAABQABpfZFQAAAAABJRU5ErkJggg==", "base64");

test("avatar file selection previews locally and saves atomically with name, persisted mark and browser tab", async () => {
  const server = await createServer({ root: fileURLToPath(new URL("../..", import.meta.url)), server: { host: "127.0.0.1", port: 0, strictPort: false },
    optimizeDeps: { entries: ["tests/browser/agent-avatar-editor.html"] },
    plugins: [{ name: "saved-avatar-image", configureServer(vite) {
      vite.middlewares.use("/api/agents/fixture-agent/avatar", (_request, response) => { response.setHeader("Content-Type", "image/png"); response.end(png); });
    } }],
  });
  const profile = await mkdtemp(join(tmpdir(), "centaeris-avatar-editor-"));
  const image = join(profile, "synthetic-avatar.png"); await writeFile(image, png);
  try {
    await server.listen();
    await runBrowser(`${server.resolvedUrls.local[0]}tests/browser/agent-avatar-editor.html`, { width: 1280 }, profile, async ({ evaluate, frame, send }) => {
      const click = async label => { await evaluate(`Array.from(document.querySelectorAll('button')).find(button => button.textContent?.trim()===${JSON.stringify(label)} || button.getAttribute('aria-label')===${JSON.stringify(label)}).click()`); await frame(); };
      const writes = () => evaluate("avatarEditorFixture.requests.filter(request=>request.method==='PATCH')");
      const assertThemeTransparency = async selector => {
        for (const theme of ["light", "dark"]) {
          const appearance = await evaluate(`(() => {
            document.documentElement.dataset.theme=${JSON.stringify(theme)};
            const image=document.querySelector(${JSON.stringify(selector)});
            const canvas=document.createElement('canvas');canvas.width=canvas.height=1;
            const context=canvas.getContext('2d');context.drawImage(image,0,0,1,1);
            const mark=image.parentElement;
            const host=image.closest('[data-saved-agent]');
            if(host) host.style.background='var(--surface-variant)';
            const paintedBackground=element=>{
              for(let node=element;node;node=node.parentElement){
                const color=getComputedStyle(node).backgroundColor;
                if(color!=='transparent' && color!=='rgba(0, 0, 0, 0)')return color;
              }
              return 'transparent';
            };
            return {alpha:context.getImageData(0,0,1,1).data[3],backingMatchesPanel:paintedBackground(mark)===paintedBackground(mark.parentElement)};
          })()`);
          assert.deepEqual(appearance, {alpha:0,backingMatchesPanel:true}, `${theme} transparent avatars match their containing panel`);
        }
        await evaluate("document.documentElement.dataset.theme='light'");
      };
      const chooseFile = async () => {
        const { root } = await send("DOM.getDocument", { depth: 0 });
        const { nodeId } = await send("DOM.querySelector", { nodeId: root.nodeId, selector: 'input[type="file"]' });
        await send("DOM.setFileInputFiles", { nodeId, files: [image] }); await frame();
        await waitFor(() => evaluate("document.querySelector('[role=dialog] .shAgentHeroMark img')?.getAttribute('src')?.startsWith('data:image/png;base64,')"), "local avatar preview");
      };
      await waitFor(() => evaluate("document.title==='Private Agent · Centaeris'"), "initial saved identity");
      await click("Edit profile"); await chooseFile();
      await waitFor(() => evaluate("document.querySelector('[role=dialog] .shAgentHeroMark img')?.complete"), "transparent preview decoded");
      await assertThemeTransparency("[role=dialog] .shAgentHeroMark img");
      assert.equal(await evaluate("document.title"), "Private Agent · Centaeris", "unsaved image/name drafts do not change the saved tab identity");
      const name = await evaluate("(() => { const input=document.querySelector('input[name=agentName]'); input.focus(); input.select(); return true; })()");
      assert.equal(name, true); await send("Input.insertText", { text: "Uploaded portrait Agent" }); await frame();
      assert.deepEqual(await writes(), [], "preview and name changes do not issue a write before Save");
      if (process.env.CENTAERIS_TEST_SCREENSHOT_DIR) {
        const screenshot = await send("Page.captureScreenshot", { format: "png" });
        await writeFile(join(process.env.CENTAERIS_TEST_SCREENSHOT_DIR, "agent-avatar-editor-preview.png"), Buffer.from(screenshot.data, "base64"));
      }
      await click("Save profile");
      await waitFor(() => evaluate("!document.querySelector('[role=dialog]') && document.title==='Uploaded portrait Agent · Centaeris' && document.querySelector('link[rel=icon]')?.getAttribute('href')?.endsWith('?v=saved')"), "saved profile and favicon");
      const saved = await writes(); assert.equal(saved.length, 1); assert.equal(saved[0].path, "/api/agents/fixture-agent");
      assert.deepEqual(saved[0].body, { name: "Uploaded portrait Agent", description: "Synthetic avatar editor", instructions: "Keep the existing instructions.", avatarKind: "centaeris", avatarDataBase64: png.toString("base64") });
      assert.equal(await evaluate("document.querySelector('[data-saved-agent] img').getAttribute('src').endsWith('?v=saved')"), true);
      await assertThemeTransparency("[data-saved-agent] img");
      await click("Edit profile");
      assert.equal(await evaluate("document.querySelector('[role=dialog] .shAgentHeroMark img').getAttribute('src').endsWith('?v=saved')"), true, "reopening uses the persisted image rather than a stale file preview");
      await click("Restore default"); await click("Save profile");
      await waitFor(() => evaluate("!document.querySelector('[role=dialog]') && document.querySelector('link[rel=icon]')?.getAttribute('href')==='/centaeris-mark.png'"), "default restored in saved tab");
      const restored = await writes(); assert.equal(restored.length, 2); assert.equal(restored[1].body.avatarDataBase64, null);
      assert.equal(await evaluate("document.querySelector('[data-saved-agent] img').getAttribute('src')"), "/centaeris-mark.png");
      await click("Edit profile"); await chooseFile(); await click("Close"); await click("Discard changes");
      await waitFor(() => evaluate("!document.querySelector('[role=dialog]')"), "cancelled draft closes");
      assert.equal((await writes()).length, 2, "discarding an uploaded draft never changes the persisted profile");
      assert.equal(await evaluate("document.querySelector('link[rel=icon]').getAttribute('href')"), "/centaeris-mark.png");
      assert.deepEqual(await evaluate("avatarEditorFixture.failures"), []);
    });
  } finally {
    await server.close();
    const resolved = await realpath(profile); assert.equal(dirname(resolved).toLowerCase(), (await realpath(tmpdir())).toLowerCase()); assert.ok(basename(resolved).startsWith("centaeris-avatar-editor-"));
    await rm(resolved, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 });
  }
});
