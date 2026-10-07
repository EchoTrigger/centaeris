import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, realpath, rm, readFile, mkdir, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, dirname, basename } from "node:path";
import { fileURLToPath } from "node:url";
import { createServer } from "vite";
import { runBrowser, waitFor } from "./browserHarness.mjs";

// The fixture is a separate review entry; production routes never import it.
test("Agent presentation fixture remains outside production entry points", async () => {
  for (const path of ["../../src/main.tsx", "../../src/router.tsx", "../../src/routes/AppRoute.jsx", "../../src/routes/WorkspaceHomeRoute.jsx", "../../src/routes/AgentChatRoute.tsx"]) {
    const source = await readFile(new URL(path, import.meta.url), "utf8");
    assert.doesNotMatch(source, /agent-chat-presentation|tests\/browser\//);
  }
});

const scenarios = [
  { width: 1280, theme: "light", reduced: false, timeZone: "UTC", locale: "en-US" },
  { width: 1280, theme: "dark", reduced: true, timeZone: "America/Los_Angeles", locale: "en-GB" },
  { width: 390, theme: "light", reduced: true, timeZone: "Asia/Shanghai", locale: "zh-CN" },
  { width: 390, theme: "dark", reduced: false, timeZone: "UTC", locale: "zh-CN" },
];

async function runPresentation(url, scenario, profile) {
  return runBrowser(url, scenario, profile, async ({ send, evaluate, frame, press, browserVersion }) => {
    const payload = await waitFor(() => evaluate('document.getElementById("results")?.textContent'), "fixture assertions");
    const results = JSON.parse(payload);
    assert.ok(results.some(result => result.name === "all presentation checks completed"), JSON.stringify(results));
    assert.deepEqual(results.filter(result => JSON.stringify(result.actual) !== JSON.stringify(result.expected)), []);

    const evidence = process.env.AGENT_CHAT_EVIDENCE_DIR;
    const name = `${scenario.width < 760 ? "narrow" : "desktop"}-${scenario.theme}`;
    const screenshot = async suffix => {
      if (!evidence) return;
      await mkdir(evidence, { recursive: true });
      await evaluate("document.fonts.ready.then(() => true)"); await frame();
      const { data } = await send("Page.captureScreenshot", { format: "png" });
      await writeFile(join(evidence, `${name}-${suffix}.png`), Buffer.from(data, "base64"));
    };
    await screenshot("conversation");
    await evaluate(() => document.querySelector('button[title="Check the cited sources"]').focus());
    assert.equal(await evaluate("document.activeElement.title"), "Check the cited sources", "session reference receives keyboard focus");
    await press("Enter");
    assert.equal(await evaluate(() => Boolean(document.querySelector('aside[aria-label="Session preview"]'))), true, "Enter previews the session");
    await screenshot("preview");
    assert.equal(await evaluate('document.querySelector("aside [role=tablist]")'), null, "session directly shows a conversation");
    await evaluate('[...document.querySelectorAll("aside button")].find(button => button.textContent.includes("Synthetic sources.pdf")).focus()');
    if (scenario.width < 760) {
      await press("Tab");
      assert.equal(await evaluate('document.querySelector("aside").contains(document.activeElement)'), false, "real Tab leaves non-modal preview");
      await press("Tab", "Tab", 8);
      assert.equal(await evaluate('document.activeElement.textContent.includes("Synthetic sources.pdf")'), true, "real Shift+Tab returns to preview");
    }
    await press("Enter"); await screenshot("library-slot");
    await evaluate(() => document.querySelector('button[aria-label="Return to session preview"]').focus()); await press("Enter");
    assert.equal(await evaluate('document.activeElement.textContent.includes("Synthetic sources.pdf")'), true, "Library return restores output focus");
    await press("Escape");
    assert.equal(await evaluate('document.activeElement.getAttribute("title")'), "Check the cited sources", "Escape restores session reference focus");
    await screenshot("reference-focus");
    {
      await evaluate(() => document.querySelector('button[title="Check the cited sources"]').focus());
      await press("Enter");
      await evaluate(() => document.querySelector('button[title="Prepare the summary"]').focus());
      await press("Enter");
      assert.equal(await evaluate(() => document.querySelector("aside .agentChatPreviewTitle").textContent === "Prepare the summary"), true, "direct keyboard switch previews session B");
      await evaluate(() => document.querySelector('button[aria-label="Close preview"]').focus());
      await press("Enter");
      assert.equal(await evaluate("document.activeElement.title"), "Prepare the summary", "closing directly switched preview restores B reference focus");
      await screenshot("switched-reference-focus");
    }
    assert.deepEqual(await evaluate("window.agentChatFixtureRequests"), []);
    assert.deepEqual(await evaluate("window.agentChatFixtureNavigations"), []);
    if (evidence) await writeFile(join(evidence, `${name}-assertions.json`), JSON.stringify({ scenario, browserVersion, results, realKeyboard: "passed" }, null, 2));
  });
}

for (const scenario of scenarios) test(`Agent presentation ${JSON.stringify(scenario)}`, async () => {
  assert.ok(process.env.CHROME_BIN, "Set CHROME_BIN to a Chromium executable");
  const profile = await mkdtemp(join(tmpdir(), "centaeris-agent-chat-ui-"));
  const server = await createServer({
    root: fileURLToPath(new URL("../..", import.meta.url)),
    server: { port: 0, host: "127.0.0.1", strictPort: false },
  });
  try {
    await server.listen();
    const url = `${server.resolvedUrls.local[0]}tests/browser/agent-chat-presentation.html?theme=${scenario.theme}`;
    await runPresentation(url, scenario, profile);
  } finally {
    await server.close();
    const resolvedProfile = await realpath(profile);
    assert.equal(dirname(resolvedProfile).toLowerCase(), (await realpath(tmpdir())).toLowerCase());
    assert.ok(basename(resolvedProfile).startsWith("centaeris-agent-chat-ui-"));
    await rm(resolvedProfile, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 });
  }
});
