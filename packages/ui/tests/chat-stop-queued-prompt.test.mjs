import assert from "node:assert/strict";
import { test } from "vitest";
import { recoverQueuedPromptAfterStop } from "../src/components/chat/chatAreaModel.ts";

test("stop recovers the queued prompt without losing an existing draft", () => {
  assert.equal(recoverQueuedPromptAfterStop("排队输入", ""), "排队输入");
  assert.equal(
    recoverQueuedPromptAfterStop("排队输入", "正在输入"),
    "排队输入\n\n正在输入",
  );
});
