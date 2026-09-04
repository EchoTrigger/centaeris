import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import path from "node:path";
import { test } from "vitest";
import { recoverQueuedPromptAfterStop } from "../src/components/chat/chatAreaModel.ts";

test("stop recovers the queued prompt without losing an existing draft", () => {
  assert.equal(recoverQueuedPromptAfterStop("排队输入", ""), "排队输入");
  assert.equal(
    recoverQueuedPromptAfterStop("排队输入", "正在输入"),
    "排队输入\n\n正在输入",
  );
});

test("stop clears the queue before closing the active stream", async () => {
  const source = await readFile(
    path.resolve(
      import.meta.dirname,
      "..",
      "src",
      "components",
      "chat",
      "ChatArea.tsx",
    ),
    "utf8",
  );
  const stopHandler = source.slice(
    source.indexOf("const handleStopActiveAgentRun"),
    source.indexOf("const applyDurableTurnMessageIds"),
  );
  assert.ok(stopHandler.indexOf('setQueuedNextPromptText("")') >= 0);
  assert.ok(
    stopHandler.indexOf('setQueuedNextPromptText("")') <
      stopHandler.indexOf("closeActiveStream()"),
  );
  assert.match(
    stopHandler,
    /setInputValue\(recoverQueuedPromptAfterStop\(queuedPrompt, inputValue\)\)/,
  );
});
