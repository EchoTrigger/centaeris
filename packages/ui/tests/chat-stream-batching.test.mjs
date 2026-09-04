import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import path from "node:path";
import { test } from "vitest";

test("keeps chat stream updates batched", async () => {
  const rootDir = path.resolve(import.meta.dirname, "..");
  const chatBridgeSource = await readFile(
    path.join(rootDir, "src", "lib", "chatBridge.ts"),
    "utf8",
  );
  const chatAreaSource = await readFile(
    path.join(rootDir, "src", "components", "chat", "ChatArea.tsx"),
    "utf8",
  );
  const agentResultSource = await readFile(
    path.join(rootDir, "src", "components", "chat", "AgentResultStream.tsx"),
    "utf8",
  );
  const markdownContentSource = await readFile(
    path.join(rootDir, "src", "components", "chat", "MarkdownContent.tsx"),
    "utf8",
  );

  assert.doesNotMatch(
    chatBridgeSource,
    /DESKTOP_STREAM_MAX_BUFFERED_PAYLOADS|队列超过上限/,
    "desktop stream ingress must not destructively stop a valid stream at an arbitrary item count",
  );
  assert.match(
    chatBridgeSource,
    /appendCoalescedStreamPayload\(buffer, payload\)/,
    "the bridge-owned ordered queue must coalesce adjacent hot stream payloads",
  );
  assert.match(
    chatBridgeSource,
    /DESKTOP_STREAM_MAX_PAYLOADS_PER_FRAME\s*=\s*128/,
    "desktop stream drain must cap payloads per animation frame",
  );
  assert.match(
    chatBridgeSource,
    /DESKTOP_STREAM_MAX_REDUCE_MS_PER_FRAME\s*=\s*4/,
    "desktop stream drain must cap reducer time per animation frame",
  );
  assert.match(
    chatBridgeSource,
    /requestAnimationFrame\(drainBuffer\)/,
    "desktop stream drain must be frame scheduled, not microtask drained",
  );
  assert.doesNotMatch(
    chatBridgeSource,
    /queueMicrotask\(drainBuffer\)/,
    "desktop stream drain must not use microtask draining for hot streams",
  );
  assert.doesNotMatch(
    chatAreaSource,
    /createChatStreamScheduler|streamScheduler\.enqueue/,
    "ChatArea must not own a second ordered payload queue",
  );
  assert.match(
    chatAreaSource,
    /requestAnimationFrame\(\(\) => \{\s*assistantTurnUpdateFrameRef\.current = null;\s*flushAssistantTurnUpdates\(\)/,
    "visible assistant updates must be aligned to animation frames",
  );
  assert.doesNotMatch(
    chatAreaSource,
    /ASSISTANT_TURN_FLUSH_INTERVAL_MS\s*=\s*100/,
    "visible assistant updates must not be capped at 10 FPS",
  );
  assert.match(
    chatAreaSource,
    /pendingTextDeltas\.join\(""\)/,
    "text deltas received in one frame must be reduced into one turn update",
  );
  assert.match(
    agentResultSource,
    /<MarkdownContent[\s\S]*isStreaming={finalItem\.phase === "streaming"}/,
    "streaming and final answers must use the same Markdown renderer",
  );
  assert.match(
    agentResultSource,
    /id: `\${turn\.id}-answer-text`/,
    "streaming completion must not remount the answer tree",
  );
  assert.match(
    markdownContentSource,
    /const MarkdownBlock = memo/,
    "sealed Markdown blocks must retain their rendered subtree",
  );
  assert.doesNotMatch(
    agentResultSource,
    /streamingTextContent/,
    "streaming answers must not fall back to raw Markdown text",
  );
});
