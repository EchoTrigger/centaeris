import { expect, test } from "vitest";
import { buildTranscriptFinalItem } from "../src/components/chat/agentTranscriptModel";

test("final transcript identity stays stable across stream completion", () => {
  const streaming = buildTranscriptFinalItem({
    id: "turn-one",
    finalAnswer: "Answer",
    isStreaming: true,
  });
  const completed = buildTranscriptFinalItem({
    id: "turn-one",
    finalAnswer: "Answer",
    isStreaming: false,
  });

  expect(streaming).toMatchObject({
    id: "turn-one-answer-text",
    phase: "streaming",
    text: "Answer",
    waterfall: {
      section: "final",
      groupId: "turn:turn-one:final",
      displayRole: "assistant_final_streaming",
    },
  });
  expect(completed).toMatchObject({
    id: "turn-one-answer-text",
    phase: "final",
    text: "Answer",
    waterfall: {
      section: "final",
      groupId: "turn:turn-one:final",
      displayRole: "assistant_final",
    },
  });
});

test("blank final answers do not create transcript items", () => {
  expect(buildTranscriptFinalItem({
    id: "turn-one",
    finalAnswer: "  ",
    isStreaming: false,
  })).toBeNull();
});
