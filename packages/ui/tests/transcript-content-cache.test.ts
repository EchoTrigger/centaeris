import { afterEach, expect, test, vi } from "vitest";
import { clearTranscriptContentRangeCache, loadTranscriptContentRange, subscribeTranscriptContentRangeCache, transcriptContentRangeCacheBytes, TRANSCRIPT_CONTENT_RANGE_BYTES, TRANSCRIPT_CONTENT_CACHE_MAX_BYTES } from "../src/components/chat/transcriptContentRanges";
import { getTranscriptContentRange, TRANSCRIPT_PROJECTION_VERSION_V1, type TranscriptContentRangeV1 } from "../src/lib/chatBridge";

vi.mock("../src/lib/chatBridge", () => ({ getTranscriptContentRange: vi.fn(), TRANSCRIPT_PROJECTION_VERSION_V1: "transcript.projection.v1" }));
afterEach(() => { clearTranscriptContentRangeCache(); vi.resetAllMocks(); });
const identity = (sessionId = "one", revision = "1", byteLength = "6") => ({ sessionId, projectionGeneration: "generation", reference: { refId: "content", revision, byteLength } });
function response(owner = identity(), offset = "0", content = "\u7ee7\u7eed") : TranscriptContentRangeV1 {
  const endOffset = String(BigInt(offset) + BigInt(new TextEncoder().encode(content).byteLength));
  return { schema: "transcript.content.range.v1", sessionId: owner.sessionId, projectionVersion: TRANSCRIPT_PROJECTION_VERSION_V1, projectionGeneration: owner.projectionGeneration, refId: owner.reference.refId, revision: owner.reference.revision, byteLength: owner.reference.byteLength, startOffset: offset, endOffset, content, hasMore: BigInt(endOffset) < BigInt(owner.reference.byteLength) };
}

test("content cache counts UTF-8 bytes, shares reads only within exact session identities and unsubscribes observers", async () => {
  vi.mocked(getTranscriptContentRange).mockImplementation(async request => response(identity(request.sessionId, request.revision, request.byteLength)));
  const notify = vi.fn();
  const unsubscribe = subscribeTranscriptContentRangeCache(notify);
  await loadTranscriptContentRange(identity(), "0");
  expect(transcriptContentRangeCacheBytes()).toBe(6);
  expect(notify).toHaveBeenCalledTimes(1);
  await loadTranscriptContentRange(identity(), "0");
  expect(getTranscriptContentRange).toHaveBeenCalledTimes(1);
  expect(notify).toHaveBeenCalledTimes(1);
  await loadTranscriptContentRange(identity("two"), "0");
  await loadTranscriptContentRange(identity("one", "2"), "0");
  expect(getTranscriptContentRange).toHaveBeenCalledTimes(3);
  expect(transcriptContentRangeCacheBytes()).toBe(18);
  unsubscribe();
  clearTranscriptContentRangeCache();
  expect(notify).toHaveBeenCalledTimes(3);
  expect(transcriptContentRangeCacheBytes()).toBe(0);
});

test("clearing a view invalidates pending content reads and cannot repopulate the cache", async () => {
  let finish!: (page: TranscriptContentRangeV1) => void;
  vi.mocked(getTranscriptContentRange).mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
  const read = loadTranscriptContentRange(identity(), "0");
  const rejected = expect(read).rejects.toThrow("transcript content view was cleared");
  clearTranscriptContentRangeCache();
  finish(response());
  await rejected;
  expect(transcriptContentRangeCacheBytes()).toBe(0);
});

test("each content reference retains four pages and evicted pages are read again", async () => {
  const owner = identity("one", "1", String(6 * TRANSCRIPT_CONTENT_RANGE_BYTES));
  vi.mocked(getTranscriptContentRange).mockImplementation(async request => response(owner, request.offset, "a".repeat(TRANSCRIPT_CONTENT_RANGE_BYTES)));
  for (let index = 0; index < 5; index++) await loadTranscriptContentRange(owner, String(index * TRANSCRIPT_CONTENT_RANGE_BYTES));
  expect(transcriptContentRangeCacheBytes()).toBe(4 * TRANSCRIPT_CONTENT_RANGE_BYTES);
  await loadTranscriptContentRange(owner, "0");
  expect(getTranscriptContentRange).toHaveBeenCalledTimes(6);
  expect(transcriptContentRangeCacheBytes()).toBe(4 * TRANSCRIPT_CONTENT_RANGE_BYTES);
});


test("content retained across sessions stays within the shared cache byte budget", async () => {
  const content = "a".repeat(TRANSCRIPT_CONTENT_RANGE_BYTES);
  vi.mocked(getTranscriptContentRange).mockImplementation(async request => response(identity(request.sessionId, "1", String(TRANSCRIPT_CONTENT_RANGE_BYTES)), "0", content));
  const pages = TRANSCRIPT_CONTENT_CACHE_MAX_BYTES / TRANSCRIPT_CONTENT_RANGE_BYTES;
  for (let index = 0; index <= pages; index++) {
    await loadTranscriptContentRange(identity(String(index), "1", String(TRANSCRIPT_CONTENT_RANGE_BYTES)), "0");
  }
  expect(transcriptContentRangeCacheBytes()).toBe(TRANSCRIPT_CONTENT_CACHE_MAX_BYTES);
  await loadTranscriptContentRange(identity("0", "1", String(TRANSCRIPT_CONTENT_RANGE_BYTES)), "0");
  expect(getTranscriptContentRange).toHaveBeenCalledTimes(pages + 2);
  expect(transcriptContentRangeCacheBytes()).toBe(TRANSCRIPT_CONTENT_CACHE_MAX_BYTES);
});


test("overlapping readers share an in-flight content range and failure permits a fresh read", async () => {
  let finish!: (page: TranscriptContentRangeV1) => void;
  vi.mocked(getTranscriptContentRange).mockImplementation(() => new Promise(resolve => { finish = resolve; }));
  const first = loadTranscriptContentRange(identity(), "0");
  const second = loadTranscriptContentRange(identity(), "0");
  expect(getTranscriptContentRange).toHaveBeenCalledTimes(1);
  finish(response());
  expect(await first).toEqual(await second);
  expect(transcriptContentRangeCacheBytes()).toBe(6);
  clearTranscriptContentRangeCache();
  vi.mocked(getTranscriptContentRange).mockRejectedValueOnce(new Error("disconnected"));
  const failed = loadTranscriptContentRange(identity(), "0");
  const sharedFailure = loadTranscriptContentRange(identity(), "0");
  await expect(failed).rejects.toThrow("disconnected");
  await expect(sharedFailure).rejects.toThrow("disconnected");
  vi.mocked(getTranscriptContentRange).mockResolvedValueOnce(response());
  expect(await loadTranscriptContentRange(identity(), "0")).toEqual(response());
  expect(getTranscriptContentRange).toHaveBeenCalledTimes(3);
});

test("clearing shared reads cannot erase or accept the replacement request", async () => {
  const requests: ((page: TranscriptContentRangeV1) => void)[] = [];
  vi.mocked(getTranscriptContentRange).mockImplementation(() => new Promise(resolve => { requests.push(resolve); }));
  const stale = loadTranscriptContentRange(identity(), "0");
  const rejected = expect(stale).rejects.toThrow("transcript content view was cleared");
  clearTranscriptContentRangeCache();
  const replacement = loadTranscriptContentRange(identity(), "0");
  requests[0](response());
  await rejected;
  const shared = loadTranscriptContentRange(identity(), "0");
  expect(getTranscriptContentRange).toHaveBeenCalledTimes(2);
  requests[1](response());
  expect(await replacement).toEqual(await shared);
});
