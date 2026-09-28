import { beforeEach, expect, test, vi } from "vitest";
import { getTranscriptPatches } from "../src/lib/chatBridge";
import { DesktopTranscriptView, loadTranscriptPage } from "../src/components/chat/transcriptPaging";
import { synchronizeSessionTranscript } from "../src/components/chat/sessionTranscriptSync";
import type { TranscriptPageV1, TranscriptPatchRpcResponseV1 } from "../src/lib/chatBridge";

vi.mock("../src/lib/chatBridge", async (original) => ({ ...await original<object>(), getTranscriptPatches: vi.fn() }));
vi.mock("../src/components/chat/transcriptPaging", async (original) => ({ ...await original<object>(), loadTranscriptPage: vi.fn() }));
const page: TranscriptPageV1 = {
  schema: "transcript.page.v1", sessionId: "s", projectionVersion: "transcript.projection.v1",
  projectionGeneration: "g", sourceHighWater: "12", blocks: [], olderCursor: "before-1", hasOlder: true,
  resumeCursors: [{ streamId: "session-jsonl.v1", cursor: "12" }],
};
const response: TranscriptPatchRpcResponseV1 = {
  schema: "transcript.patch.rpc.v1", projectionVersion: "transcript.projection.v1", projectionGeneration: "g",
  projectedSourceHighWater: "14", targetSourceHighWater: "14", nextSourceHighWater: "14",
  targetReached: true, hasMore: false, patches: [],
};
beforeEach(() => { vi.resetAllMocks(); vi.mocked(loadTranscriptPage).mockResolvedValue(page); vi.mocked(getTranscriptPatches).mockResolvedValue(response); });

test("returning to a cached chat uses its patch cursor and keeps older paging state", async () => {
  const cached = DesktopTranscriptView.open(page);
  const view = await synchronizeSessionTranscript("s", cached);
  expect(view).toBe(cached);
  expect(view.currentSourceHighWater).toBe("14");
  expect(view.hasOlder).toBe(true);
  expect(getTranscriptPatches).toHaveBeenCalledWith({ sessionId: "s", projectionGeneration: "g", afterSourceHighWater: "12" });
  expect(loadTranscriptPage).not.toHaveBeenCalled();
});
test("cancellation prevents a late patch response from advancing the cached cursor", async () => {
  const cached = DesktopTranscriptView.open(page);
  let cancelled = false;
  vi.mocked(getTranscriptPatches).mockImplementation(async () => { cancelled = true; return response; });
  await expect(synchronizeSessionTranscript("s", cached, () => cancelled)).rejects.toThrow("cancelled");
  expect(cached.currentSourceHighWater).toBe("12");
});
test("only an explicit Runtime generation invalidation permits a full reload", async () => {
  vi.mocked(getTranscriptPatches).mockRejectedValue(new Error("transcript projectionGeneration does not match the local Runtime generation"));
  await synchronizeSessionTranscript("s", DesktopTranscriptView.open(page));
  expect(loadTranscriptPage).toHaveBeenCalledTimes(1);
});
test("a malformed patch response is surfaced without silently reloading history", async () => {
  vi.mocked(getTranscriptPatches).mockResolvedValue({ ...response, projectionGeneration: "wrong" });
  await expect(synchronizeSessionTranscript("s", DesktopTranscriptView.open(page))).rejects.toThrow("identity");
  expect(loadTranscriptPage).not.toHaveBeenCalled();
});
