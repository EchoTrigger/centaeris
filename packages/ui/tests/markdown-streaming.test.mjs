import assert from "node:assert/strict";
import { test } from "vitest";
import {
  createMarkdownBlockProjection,
  updateMarkdownBlockProjection,
} from "../src/components/chat/MarkdownContent.tsx";

test("seals completed Markdown blocks without rebuilding earlier blocks", () => {
  let projection = updateMarkdownBlockProjection(
    createMarkdownBlockProjection(),
    "## 方案\n\n",
    false,
  );
  const headingBlock = projection.sealedBlocks[0];
  const sealedBlocks = projection.sealedBlocks;

  projection = updateMarkdownBlockProjection(
    projection,
    "## 方案\n\n- 第一项",
    false,
  );
  assert.equal(projection.sealedBlocks, sealedBlocks);
  assert.equal(projection.sealedBlocks[0], headingBlock);
  assert.equal(projection.sourceText.slice(projection.sealedEnd), "- 第一项");

  const finalized = updateMarkdownBlockProjection(
    projection,
    projection.sourceText,
    true,
  );
  assert.equal(finalized.sealedBlocks[0], headingBlock);
  assert.equal(finalized.sealedBlocks[1].id, projection.nextBlockId);
  assert.equal(finalized.sealedBlocks[1].text, "- 第一项");

  projection = updateMarkdownBlockProjection(
    projection,
    "## 方案\n\n- 第一项\n- 第二项\n\n",
    false,
  );
  assert.equal(projection.sealedBlocks[0], headingBlock);
  assert.deepEqual(
    projection.sealedBlocks.map((block) => block.text),
    ["## 方案\n\n", "- 第一项\n- 第二项\n\n"],
  );
});

test("keeps fenced code together and resets on text replacement", () => {
  let projection = updateMarkdownBlockProjection(
    createMarkdownBlockProjection(),
    "```rust\nfn main() {\n\n",
    false,
  );
  assert.equal(projection.sealedBlocks.length, 0);
  assert.equal(projection.inFence, true);

  projection = updateMarkdownBlockProjection(
    projection,
    "```rust\nfn main() {\n\n}\n```\n",
    false,
  );
  assert.equal(projection.sealedBlocks.length, 1);
  assert.equal(projection.inFence, false);

  const replaced = updateMarkdownBlockProjection(projection, "最终答案", true);
  assert.deepEqual(
    replaced.sealedBlocks.map((block) => block.text),
    ["最终答案"],
  );
  assert.notEqual(replaced.sealedBlocks[0], projection.sealedBlocks[0]);
});
