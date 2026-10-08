import { beforeEach, expect, test, vi } from "vitest";
import { invokeHost } from "../src/host/hostBridge";
import { DesktopTranscriptView } from "../src/components/chat/transcriptPaging";
import { synchronizeSessionTranscript } from "../src/components/chat/sessionTranscriptSync";

vi.mock("../src/host/hostBridge", () => ({
  invokeHost: vi.fn(), isNativeHostRuntime: () => true, listenHost: vi.fn(),
}));
const page = {
  schema: "transcript.page.v1", sessionId: "s", projectionVersion: "transcript.projection.v1",
  projectionGeneration: "g", sourceHighWater: "12", blocks: [], olderCursor: "before-1", hasOlder: true,
  resumeCursors: [{ streamId: "session-jsonl.v1", cursor: "12" }],
};
beforeEach(() => vi.resetAllMocks());

test("failed initial transcript page surfaces the error after one bounded RPC", async () => {
  const failure = new Error("page unavailable");
  vi.mocked(invokeHost).mockRejectedValue(failure);
  await expect(synchronizeSessionTranscript("s")).rejects.toBe(failure);
  expect(invokeHost.mock.calls).toEqual([["transcript/page", { request: { sessionId: "s" } }]]);
});

test("failed cached synchronization preserves its cursor and reconnect resumes patches", async () => {
  const cached = DesktopTranscriptView.open(page);
  const failure = new Error("connection interrupted");
  vi.mocked(invokeHost).mockRejectedValueOnce(failure).mockResolvedValueOnce({
    schema: "transcript.patch.rpc.v1", projectionVersion: page.projectionVersion,
    projectionGeneration: "g", projectedSourceHighWater: "14", targetSourceHighWater: "14",
    nextSourceHighWater: "14", targetReached: true, hasMore: false, patches: [],
  });
  await expect(synchronizeSessionTranscript("s", cached)).rejects.toBe(failure);
  expect(cached.currentSourceHighWater).toBe("12");
  expect(cached.hasOlder).toBe(true);
  await expect(synchronizeSessionTranscript("s", cached)).resolves.toBe(cached);
  expect(cached.currentSourceHighWater).toBe("14");
  expect(cached.hasOlder).toBe(true);
  expect(invokeHost.mock.calls).toEqual(Array(2).fill([
    "transcript/patches", { request: { sessionId: "s", projectionGeneration: "g", afterSourceHighWater: "12" } },
  ]));
});
