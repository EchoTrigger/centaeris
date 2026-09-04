import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "vitest";

const dialogUrl = new URL("../src/components/SkillsDialog.tsx", import.meta.url);
const bridgeUrl = new URL("../src/lib/chatBridge.ts", import.meta.url);
const appUrl = new URL("../src/App.tsx", import.meta.url);

test("Skills uses explicit typed sources and keeps Plugin on its own surface", async () => {
  const [dialogSource, bridgeSource, appSource] = await Promise.all([
    readFile(dialogUrl, "utf8"),
    readFile(bridgeUrl, "utf8"),
    readFile(appUrl, "utf8"),
  ]);

  assert.match(dialogSource, /Add skill location/);
  assert.match(dialogSource, /Catalog directory/);
  assert.match(dialogSource, /SKILL\.md/);
  assert.match(dialogSource, /source\.scope === "workspace" \|\| source\.scope === "user"/);
  assert.doesNotMatch(dialogSource, /skills\.sh|marketplace|install skill/i);
  assert.match(bridgeSource, /"skill\/source\/add"/);
  assert.match(bridgeSource, /"skill\/catalog"/);
  assert.match(bridgeSource, /"skill\/detail"/);
  assert.match(appSource, /<SkillsDialog workspaceRoot=/);
  assert.doesNotMatch(appSource, /activeModal === "skills" && activeWorkspaceRoot/);
  assert.doesNotMatch(appSource, /<PluginsDialog[^>]+kind=/);
});
