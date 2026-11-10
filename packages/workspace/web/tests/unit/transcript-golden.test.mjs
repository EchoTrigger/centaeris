import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import test from "node:test";

test("transcript golden scenario keeps its versioned semantic boundary", () => {
  const bytes = readFileSync(new URL("../fixtures/transcript-golden-v1.json", import.meta.url));
  const fixture = JSON.parse(bytes.toString("utf8"));

  assert.equal(
    createHash("sha256").update(bytes).digest("hex"),
    "904971875d4f105d0a8ae735278017f3f6aab301549083ac3db7ce2f1568c8f7",
  );

  assert.equal(fixture.schema, "transcript.golden.v1");
  assert.equal(fixture.fixtureRevision, "2026-09-13.1");
  assert.equal(fixture.scenarioId, "reasoning-tool-answer");
  assert.deepEqual(fixture.operations.map(({ kind }) => kind), [
    "userText",
    "reasoningReplace",
    "toolStart",
    "toolFinish",
    "assistantTextReplace",
    "terminal",
  ]);
  assert.deepEqual(fixture.expectedBlocks.map(({ blockId }) => blockId), [
    "user:1",
    "reasoning:req-1",
    "tool:call-1",
    "assistant:turn-1",
  ]);
});
