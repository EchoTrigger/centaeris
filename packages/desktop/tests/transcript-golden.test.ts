import { readFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { expect, test } from "vitest";

test("transcript golden scenario keeps its versioned semantic boundary", () => {
  const bytes = readFileSync(
    new URL("../../core/tests/fixtures/transcript_projection_v1/golden.json", import.meta.url),
  );
  const fixture = JSON.parse(bytes.toString("utf8"));

  expect(createHash("sha256").update(bytes).digest("hex")).toBe(
    "904971875d4f105d0a8ae735278017f3f6aab301549083ac3db7ce2f1568c8f7",
  );

  expect(fixture).toMatchObject({
    schema: "transcript.golden.v1",
    fixtureRevision: "2026-09-13.1",
    scenarioId: "reasoning-tool-answer",
  });
  expect(fixture.operations.map((operation: { kind: string }) => operation.kind)).toEqual([
    "userText",
    "reasoningReplace",
    "toolStart",
    "toolFinish",
    "assistantTextReplace",
    "terminal",
  ]);
  expect(fixture.expectedBlocks.map((block: { blockId: string }) => block.blockId)).toEqual([
    "user:1",
    "reasoning:req-1",
    "tool:call-1",
    "assistant:turn-1",
  ]);
});
