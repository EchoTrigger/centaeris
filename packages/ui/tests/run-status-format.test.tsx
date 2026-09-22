import { expect, test } from "vitest";
import { formatActiveDuration } from "../src/components/chat/runActivity";

test("active duration omits zero units without padding", () => {
  expect(formatActiveDuration(0)).toBe("");
  expect(formatActiveDuration(20_000)).toBe("20s");
  expect(formatActiveDuration(80_000)).toBe("1m 20s");
  expect(formatActiveDuration(3_600_000)).toBe("1h");
  expect(formatActiveDuration(3_601_000)).toBe("1h 1s");
});

test("historical process, supplements and final preserve turn grouping", async () => {
  const { transcriptMessageGroups } = await import("../src/components/chat/transcriptMessageGroups");
  const messages = [
    { id: "u1", role: "user" },
    { id: "reason", role: "assistant", turn: {} },
    { id: "supplement", role: "assistant", turn: {} },
    { id: "final", role: "assistant", turn: {} },
    { id: "u2", role: "user" },
    { id: "live", role: "assistant", turn: { agentRunId: "run:2" } },
  ] as import("../src/components/chat/types").ChatMessage[];
  expect(transcriptMessageGroups(messages.map((message) => message.id), (id) => messages.find((message) => message.id === id)))
    .toEqual([["u1"], ["reason", "supplement", "final"], ["u2"], ["live"]]);
});
