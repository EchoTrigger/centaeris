import assert from "node:assert/strict";
import { access, readFile } from "node:fs/promises";
import path from "node:path";
import { test } from "vitest";

test("opens each durable Agent session once in the preview tab strip", async () => {
  const rootDir = path.resolve(import.meta.dirname, "..");
  const [appSource, resultSource, panelSource, previewSource, runtimeCoreSource] = await Promise.all([
    readFile(path.join(rootDir, "src", "App.tsx"), "utf8"),
    readFile(path.join(rootDir, "src", "components", "chat", "AgentResultStream.tsx"), "utf8"),
    readFile(path.join(rootDir, "src", "components", "SummaryPanel.tsx"), "utf8"),
    readFile(path.join(rootDir, "src", "components", "chat", "AgentSessionPreview.tsx"), "utf8"),
    readFile(path.join(rootDir, "src", "components", "chat", "chatRuntimeCore.ts"), "utf8"),
  ]);

  assert.match(appSource, /const tabId = `agent:\$\{normalizedSessionId\}`/);
  assert.match(appSource, /if \(tabs\.some\(\(tab\) => tab\.id === tabId\)\) return tabs/);
  assert.match(appSource, /setActivePanelTabId\(tabId\)/);
  assert.doesNotMatch(appSource, /onOpenSession=/);
  assert.match(resultSource, /onOpenAgentSession\?\.\(subagent\.childSessionId, title\)/);
  assert.match(panelSource, /agentTabs\.map\(\(tab\)/);
  assert.match(panelSource, /className="summaryPanelCollapse"/);
  assert.match(appSource, /onCollapse=\{\(\) => setIsFilePaneOpen\(false\)\}/);
  assert.doesNotMatch(appSource, /thinWorkspaceToolbar/);
  assert.match(previewSource, /snapshot\.activeReplay\?\.status === "queued"/);
  assert.match(previewSource, />重新加载<\/button>/);
  assert.doesNotMatch(runtimeCoreSource, /没有连上模型/);

  await assert.rejects(
    access(path.join(rootDir, "src", "components", "chat", "SubagentStatusRail.tsx")),
  );
});
