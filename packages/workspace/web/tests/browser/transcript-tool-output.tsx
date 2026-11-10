import { createRoot } from "react-dom/client";
import { flushSync } from "react-dom";
import { configureApi } from "../../src/api";
import { TranscriptToolGroupCard } from "../../src/chat/TranscriptBlockContent";
import { TranscriptContentReader } from "../../src/chat/transcriptContentReader";
import { clearTranscriptContentRangeCache } from "../../src/chat/transcriptContentRanges";
import { createTranscriptViewStore } from "../../src/chat/transcriptViewStore";

const results: { name: string; actual: unknown; expected: unknown }[] = [];
const check = (name: string, actual: unknown, expected: unknown) => results.push({ name, actual, expected });
const trace: unknown[] = [];
const host = document.getElementById("host")!;
const root = createRoot(host);
const store = createTranscriptViewStore();
store.openTail({
  schema: "transcript.page.v1", sessionId: "test-session",
  projectionVersion: "transcript.projection.v1", projectionGeneration: "generation-1", sourceHighWater: "1",
  olderCursor: null, hasOlder: false, resumeCursors: [{ streamId: "workspace-transcript.v1", cursor: "1" }],
  blocks: [{ blockId: "tool:single-line", blockRevision: "1", orderKey: { sourceSequence: "1", ordinal: 0 },
    body: { kind: "tool", callId: "single-line", toolName: "bash", status: "completed", summary: "Single line output",
      summaryRef: null, outputRef: { refId: "tool-output:single-line", revision: "1", byteLength: "70000" } } }],
});
const requests: { url: URL; resolve: (response: Response) => void }[] = [];
const originalFetch = window.fetch;
const originalLoadMore = TranscriptContentReader.prototype.loadMore;
const observed = new WeakSet<TranscriptContentReader>();
let subscribed = false;
// Observe production calls without invoking loadMore or changing promise timing.
TranscriptContentReader.prototype.loadMore = function () {
  if (!observed.has(this)) {
    observed.add(this);
    const subscribe = this.subscribe;
    this.subscribe = (listener) => {
      subscribed = true;
      trace.push({ event: "subscribe" });
      return subscribe(listener);
    };
  }
  const state = this.getSnapshot();
  trace.push({ event: "loadMore", loaded: state.content.length, loading: state.loading,
    pending: Reflect.get(this, "pending") !== null });
  return originalLoadMore.call(this);
};
configureApi({ apiBaseUrl: location.origin });
window.fetch = (input) => new Promise(resolve => {
  const url = new URL(String(input));
  trace.push({ event: "request", offset: url.searchParams.get("offset") });
  requests.push({ url, resolve });
});
const settle = () => new Promise(resolve => setTimeout(resolve, 0));
async function until(predicate: () => boolean) {
  for (let turn = 0; turn < 100; turn++) {
    if (predicate()) return true;
    await settle();
  }
  return false;
}
function complete(request: typeof requests[number]) {
  const start = Number(request.url.searchParams.get("offset"));
  const end = Math.min(start + 65536, 70000);
  request.resolve(Response.json({
    schema: "transcript.content.range.v1", sessionId: "test-session",
    projectionVersion: "transcript.projection.v1", projectionGeneration: "generation-1",
    refId: "tool-output:single-line", revision: "1", byteLength: "70000",
    startOffset: String(start), endOffset: String(end), content: "a".repeat(end - start), hasMore: end < 70000,
  }));
}
try {
  flushSync(() => root.render(<TranscriptToolGroupCard store={store} blockIds={["tool:single-line"]} />));
  flushSync(() => (host.querySelector(".workspaceActivityGroup") as HTMLButtonElement).click());
  flushSync(() => (host.querySelector(".agent-operation-summary") as HTMLButtonElement).click());
  await until(() => requests.length === 1 && subscribed);
  for (const animation of host.getAnimations({ subtree: true })) animation.finish();
  await settle();
  check("output component is subscribed before the pending first response arrives", subscribed, true);
  check("first tool page starts at zero", requests[0]?.url.searchParams.get("offset"), "0");
  complete(requests[0]);
  await until(() => host.querySelector(".workspaceToolOutputScroll")?.textContent?.length === 65536);
  const output = host.querySelector(".workspaceToolOutputScroll") as HTMLPreElement;
  check("single-line output has a visible viewport without vertical scrolling", output.clientHeight > 0 && output.scrollHeight <= output.clientHeight + 1, true);
  const continued = await until(() => requests.length > 1);
  check("the component automatically requests the next page", continued, true);
  check("automatic tool continuation uses offset 65536", requests[1]?.url.searchParams.get("offset"), "65536");
  if (continued) complete(requests[1]);
  await until(() => output.textContent?.length === 70000);
  check("the tool viewport automatically receives the complete single line", output.textContent === "a".repeat(70000), true);
  check("there are exactly two page requests", requests.length, 2);
} catch (error) {
  check("tool component exception", String(error), "no exception");
} finally {
  root.unmount();
  window.fetch = originalFetch;
  TranscriptContentReader.prototype.loadMore = originalLoadMore;
  clearTranscriptContentRangeCache();
}
document.getElementById("results")!.textContent = JSON.stringify(results);
document.getElementById("trace")!.textContent = JSON.stringify(trace);
