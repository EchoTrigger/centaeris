import test from "node:test";
import assert from "node:assert/strict";
import { mkdir, mkdtemp, realpath, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, dirname, basename } from "node:path";
import { fileURLToPath } from "node:url";
import { createServer } from "vite";
import { runBrowser, waitFor } from "./browserHarness.mjs";

for (const scenario of [{ width: 1280, language: "en", theme: "light" }, { width: 390, language: "zh-CN", theme: "light" }, { width: 1280, language: "en", theme: "dark" }, { width: 390, language: "zh-CN", theme: "dark" }]) {
test(`application permanent Agent consent, rotation, temporary assistant grant and administrator lifecycle ${scenario.width} ${scenario.language} ${scenario.theme}`, async () => {
  assert.ok(process.env.CHROME_BIN, "Set CHROME_BIN to a Chromium executable");
  const profile = await mkdtemp(join(tmpdir(), "centaeris-app-delegations-"));
  const server = await createServer({ root: fileURLToPath(new URL("../..", import.meta.url)), cacheDir: fileURLToPath(new URL("../../../../.local/app-delegations-vite", import.meta.url)), optimizeDeps: { entries: ["tests/browser/app-delegations.html"], noDiscovery: true, include: ["react", "react/jsx-dev-runtime", "react-dom", "react-dom/client", "react-router", "i18next", "react-i18next", "lucide-react"] }, server: { port: 0, host: "127.0.0.1", strictPort: false } });
  try {
    await server.listen();
    const url = `${server.resolvedUrls.local[0]}tests/browser/app-delegations.html?previewLanguage=${scenario.language}&theme=${scenario.theme}`;
    await runBrowser(url, scenario, profile, async ({ evaluate, send, frame }) => {
      const results = await waitFor(() => evaluate("window.__appDelegationsComplete ? JSON.parse(document.getElementById('results').textContent) : null"), "application consent assertions");
      assert.ok(results.length >= 118, JSON.stringify(results));
      assert.deepEqual(results.filter(result => JSON.stringify(result.actual) !== JSON.stringify(result.expected)), []);
      await frame();
      if (process.env.APP_DELEGATIONS_EVIDENCE_DIR) {
        await mkdir(process.env.APP_DELEGATIONS_EVIDENCE_DIR, { recursive: true });
        const screenshot = await send("Page.captureScreenshot", { format: "png", captureBeyondViewport: true });
        await writeFile(join(process.env.APP_DELEGATIONS_EVIDENCE_DIR, `business-agent-settings-${scenario.width}-${scenario.language}-${scenario.theme}.png`), Buffer.from(screenshot.data, "base64"));
        await writeFile(join(process.env.APP_DELEGATIONS_EVIDENCE_DIR, `business-agent-settings-${scenario.width}-${scenario.language}-${scenario.theme}.json`), JSON.stringify({ source: "Synthetic-data fixture importing production AppDelegations", scenario, results }, null, 2));
        for (const tab of ["overview", "api", "credentials"]) {
          await evaluate(`document.querySelector('[data-tab="${tab}"]').click(); document.querySelector('.workspaceSettingsScroll').scrollTop = 0`);
          await frame();
          const detail = await send("Page.captureScreenshot", { format: "png", captureBeyondViewport: true });
          await writeFile(join(process.env.APP_DELEGATIONS_EVIDENCE_DIR, `business-agent-settings-${scenario.width}-${scenario.language}-${scenario.theme}-${tab}.png`), Buffer.from(detail.data, "base64"));
        }
        await evaluate(`document.querySelector('[data-action="back"]').click()`);
        await frame();
        await evaluate(`if (!document.querySelector('.appDelegationConsent').hidden) document.querySelector('[data-action="new-authorization"]').click(); document.querySelector('.workspaceSettingsScroll').scrollTop = 0`);
        await frame();
        const list = await send("Page.captureScreenshot", { format: "png", captureBeyondViewport: true });
        await writeFile(join(process.env.APP_DELEGATIONS_EVIDENCE_DIR, `business-agent-settings-${scenario.width}-${scenario.language}-${scenario.theme}-list.png`), Buffer.from(list.data, "base64"));
      }
    });
  } finally {
    await server.close();
    const resolvedProfile = await realpath(profile);
    assert.equal(dirname(resolvedProfile).toLowerCase(), (await realpath(tmpdir())).toLowerCase());
    assert.ok(basename(resolvedProfile).startsWith("centaeris-app-delegations-"));
    await rm(resolvedProfile, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 });
  }
});
}
