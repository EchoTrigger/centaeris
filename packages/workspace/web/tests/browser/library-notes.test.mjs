import test from "node:test";
import assert from "node:assert/strict";
import { mkdir, mkdtemp, realpath, rm } from "node:fs/promises";
import { dirname, basename, join } from "node:path";
import { fileURLToPath } from "node:url";
import { createServer } from "vite";
import { runBrowser, waitFor } from "./browserHarness.mjs";

async function fixture(inspect) {
  const root = fileURLToPath(new URL("../..", import.meta.url));
  const profiles = join(root, "../../.local");
  await mkdir(profiles, { recursive: true });
  const profile = await mkdtemp(join(profiles, "note-browser-"));
  const server = await createServer({ root, server: { port: 0, host: "127.0.0.1" } });
  try {
    await server.listen();
    await runBrowser(`${server.resolvedUrls.local[0]}tests/browser/library-notes.html`, { width: 360 }, profile,
      browser => inspect(browser, new URL(server.resolvedUrls.local[0]).origin));
  } finally {
    await server.close();
    const resolved = await realpath(profile);
    assert.equal(dirname(resolved), await realpath(profiles));
    assert.ok(basename(resolved).startsWith("note-browser-"));
    await rm(resolved, { recursive: true, force: true });
  }
}

test("real IndexedDB restores offline edits after reload and save controls remain usable when narrow", async () => {
  await fixture(async ({ evaluate, send, frame }) => {
      await waitFor(() => evaluate('Boolean(document.querySelector("textarea"))'), "note editor");
      assert.equal(await evaluate('document.querySelector("textarea").value'), "# Independent body heading\n\nOriginal body");
      await evaluate('sessionStorage.setItem("fixture.offline", "1"); document.querySelector("textarea").focus(); document.querySelector("textarea").select();');
      await send("Input.insertText", { text: "Offline draft 🌱" });
      await evaluate('[...document.querySelectorAll("button")].find(button => button.textContent === "Save now").click()');
      await waitFor(() => evaluate('document.body.textContent.includes("Sync failed")'), "offline save failure");
      await send("Page.reload");
      await waitFor(() => evaluate('document.body.textContent.includes("Local drafts are available.")'), "persisted draft recovery");
      await evaluate('[...document.querySelectorAll("button")].find(button => button.textContent.startsWith("Recover ")).click()');
      await waitFor(() => evaluate('document.querySelector("textarea").value === "Offline draft 🌱"'), "restored body");
      await waitFor(() => evaluate('document.body.textContent.includes("Sync failed")'), "restored pending operation");
      await evaluate('sessionStorage.setItem("fixture.offline", "0"); window.dispatchEvent(new Event("online"));');
      await waitFor(() => evaluate('document.body.textContent.includes("Saved on server")'), "server confirmation");
      const ids = await evaluate('JSON.parse(sessionStorage.getItem("fixture.noteCalls")).filter(call => call.method === "PUT").map(call => call.body.saveId)');
      assert.ok(ids.length >= 3);
      assert.equal(new Set(ids).size, 1, "uncertain operation identity survives reload");
      assert.equal(await evaluate('[...document.querySelectorAll("[role=status]")].filter(element => element.textContent === "Saved on server").length'), 1);
      await frame();
      assert.ok(await evaluate(() => {
        const save = [...document.querySelectorAll("button")].find(button => button.textContent === "Save now");
        const rect = save.getBoundingClientRect();
        const header = save.closest("header").getBoundingClientRect();
        return rect.left >= header.left && rect.right <= Math.min(header.right, innerWidth) && !save.disabled;
      }), "save control stays inside the visible header at narrow widths");
  });
});

test("quota failure blocks navigation until explicit choice or successful local persistence", async () => {
  await fixture(async ({ evaluate, send }, origin) => {
    await waitFor(() => evaluate('Boolean(document.querySelector("textarea"))'), "note editor");
    await send("Storage.overrideQuotaForOrigin", { origin, quotaSize: 1 });
    await evaluate('document.querySelector("textarea").focus(); document.querySelector("textarea").select();');
    await send("Input.insertText", { text: "Not durable yet" });
    await evaluate('[...document.querySelectorAll("button")].find(button => button.textContent === "Save now").click()');
    await waitFor(() => evaluate('document.body.textContent.includes("Local draft could not be saved")'), "quota failure");
    await evaluate('window.fixtureNavigate("/w/fixture/library/other-note")');
    await waitFor(() => evaluate('Boolean(document.querySelector("[role=dialog]"))'), "navigation protection");
    assert.equal(await evaluate('document.querySelector("textarea").value'), "Not durable yet");
    await evaluate('[...document.querySelector("[role=dialog]").querySelectorAll("button")].find(button => button.textContent === "Stay").click()');
    await waitFor(() => evaluate('!document.querySelector("[role=dialog]")'), "stay in editor");
    assert.equal(await evaluate('document.querySelector("textarea").value'), "Not durable yet");
    await send("Storage.overrideQuotaForOrigin", { origin });
    await evaluate('window.fixtureNavigate("/w/fixture/library/other-note")');
    await waitFor(() => evaluate('document.querySelector("textarea")?.value === "Other body"'), "safe navigation after draft persistence");
  });
});
