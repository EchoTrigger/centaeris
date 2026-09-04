import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import path from "node:path";
import { test } from "vitest";

test("keeps chat rendering isolated", async () => {
  const rootDir = path.resolve(import.meta.dirname, "..");
  const chatAreaSource = await readFile(
    path.join(rootDir, "src", "components", "chat", "ChatArea.tsx"),
    "utf8",
  );
  const agentResultSource = await readFile(
    path.join(rootDir, "src", "components", "chat", "AgentResultStream.tsx"),
    "utf8",
  );
  const virtualListSource = await readFile(
    path.join(rootDir, "src", "components", "chat", "VirtualMessageList.tsx"),
    "utf8",
  );
  const chatComposerSource = await readFile(
    path.join(rootDir, "src", "components", "chat", "ChatComposer.tsx"),
    "utf8",
  );

  assert.match(
    chatAreaSource,
    /<VirtualMessageList[\s\S]*containerRef=\{messagesContainerRef\}/,
    "ChatArea must render messages through the virtualized message list",
  );
  assert.doesNotMatch(
    chatAreaSource,
    /messages\.map\(\(message,\s*index\)/,
    "ChatArea must not inline-render every message row",
  );
  assert.doesNotMatch(
    chatAreaSource,
    /const\s*\[\s*messages\s*,\s*setMessages\s*\]\s*=\s*useState/,
    "ChatArea must not own messages as React hot state",
  );
  assert.match(
    chatAreaSource,
    /useChatViewStore\.getState\(\)\.replaceMessages\(nextMessages\)/,
    "ChatArea must commit message changes directly to the normalized chat view store",
  );
  assert.match(
    chatAreaSource,
    /\.updateAssistantMessages\(options\.assistantMessages\)/,
    "hot stream updates must target only changed assistant messages",
  );
  assert.match(
    chatComposerSource,
    /export const ChatComposer = memo\(function ChatComposer/,
    "ChatComposer must be memoized so stream updates do not force composer renders",
  );
  assert.match(
    virtualListSource,
    /useVirtualizer\(\{/,
    "VirtualMessageList must use TanStack Virtual",
  );
  assert.match(
    virtualListSource,
    /useLayoutEffect\(\(\) => \{\s*onContentSizeChange\(\);\s*\}, \[onContentSizeChange, totalSize\]\);/,
    "virtual row measurements must keep the existing follow scheduler attached",
  );
  assert.match(
    chatAreaSource,
    /aria-label="回到最新"/,
    "ChatArea must provide an explicit way to resume following the latest message",
  );
  assert.match(
    chatAreaSource,
    /if \(queuedPrompt === undefined\) \{\s*setFollowingLatest\(true\);\s*\}\s*\n\s*let targetSession/,
    "direct user submission must resume the existing follow state before appending messages",
  );
  assert.match(
    chatAreaSource,
    /payload\.contextTokenEstimate[\s\S]*refreshContextUsage\(eventSessionId\)/,
    "request boundaries must refresh the canonical context snapshot without token polling",
  );
  assert.doesNotMatch(
    chatAreaSource,
    /usedTokens:\s*purpose === "main"|updatedAt:\s*purpose === "main"/,
    "request boundaries must not synthesize a partial context snapshot",
  );
  assert.doesNotMatch(
    chatAreaSource,
    /setInterval\([^)]*refreshContextUsage/,
    "context usage must not add a renderer polling loop",
  );
  assert.match(
    agentResultSource,
    /const TaskGroupTranscriptItem = memo/,
    "task groups must render through an isolated memoized component",
  );
  assert.match(
    agentResultSource,
    /useChatViewStore\(\s*useShallow\(\(state\) =>[\s\S]*state\.taskById\[taskId\]/,
    "task groups must subscribe to taskById by task id",
  );
  assert.match(
    agentResultSource,
    /const SubagentTranscriptTag = memo/,
    "subagent entry tags must render through an isolated memoized component",
  );
  assert.match(
    agentResultSource,
    /state\.subagentById\[entry\.id\]/,
    "subagent entry tags must subscribe to subagentById by subagent id",
  );
  assert.doesNotMatch(
    agentResultSource,
    /processSummaryOpen|liveToolItem|ProcessHeaderText/,
    "tools must stay in one flat timeline node instead of moving between live and history trees",
  );
  assert.match(
    agentResultSource,
    /const detail = isOpen\s*\? renderOperationDetail/,
    "collapsed operation details must not mount diff viewers or read spill files",
  );
  assert.doesNotMatch(
    chatAreaSource,
    /commitMessagesToView[\s\S]*?scheduleFollowLatestScroll\(\);[\s\S]*?scheduleVisibleSessionViewCachePersistRef/,
    "message commits must wait for the virtualizer measurement before following the tail",
  );
  assert.match(
    chatAreaSource,
    /finalAnswer: `\$\{nextTurn\.finalAnswer\}\$\{pendingTextDeltas\.join\(""\)\}`,[\s\S]*activity: null/,
    "assistant text output must hide the dynamic tail activity in the same batched update",
  );
});
