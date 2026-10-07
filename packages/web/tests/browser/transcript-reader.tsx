import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { flushSync } from "react-dom";
import { configureApi } from "../../src/api";
import { TranscriptReferencedContent } from "../../src/chat/TranscriptReferencedContent";
import { TranscriptContentReader } from "../../src/chat/transcriptContentReader";
import { clearTranscriptContentRangeCache } from "../../src/chat/transcriptContentRanges";
import { watchTranscriptLoads } from "../helpers/transcript-reader-watchdog.mjs";

const results: { name: string; actual: unknown; expected: unknown }[] = [];
const check = (name: string, actual: unknown, expected: unknown) => results.push({ name, actual, expected });
const host = document.getElementById("host")!;
const root = createRoot(host);
const watcher = watchTranscriptLoads(TranscriptContentReader);
const requests: { url: URL; signal: AbortSignal; resolve: (response: Response) => void; reject: (error: Error) => void }[] = [];
const originalFetch = window.fetch;
configureApi({ apiBaseUrl: location.origin });
window.fetch = (input, init) => new Promise((resolve, reject) => {
  requests.push({ url: new URL(String(input)), signal: init!.signal as AbortSignal, resolve, reject });
});
const settle = () => new Promise(resolve => setTimeout(resolve, 0));
async function until(predicate: () => boolean) {
  for (let turn = 0; turn < 100; turn++) {
    if (predicate()) return;
    await settle();
  }
  throw new Error("bounded component settlement timed out");
}
function render(refId: string, byteLength = "8") {
  flushSync(() => root.render(<StrictMode><TranscriptReferencedContent
    sessionId="test-session" projectionGeneration="generation-1"
    reference={{ refId, revision: "1", byteLength }} mode="plain" />
  </StrictMode>));
}
function complete(request: typeof requests[number], content: string) {
  const start = Number(request.url.searchParams.get("offset"));
  const byteLength = request.url.searchParams.get("byteLength")!;
  const end = start + new TextEncoder().encode(content).byteLength;
  request.resolve(Response.json({
    schema: "transcript.content.range.v1", sessionId: "test-session",
    projectionVersion: "transcript.projection.v1", projectionGeneration: "generation-1",
    refId: request.url.searchParams.get("refId"), revision: "1", byteLength,
    startOffset: String(start), endOffset: String(end), content, hasMore: end < Number(byteLength),
  }));
}
const originalTimeout = window.setTimeout;
const originalClearTimeout = window.clearTimeout;
const timers = new Map<number, { callback: () => void; delay: number }>();
const scheduledDelays: number[] = [];
let timerId = -1;
window.setTimeout = ((callback: () => void, delay?: number) => {
  if (delay !== undefined && [500, 1000, 2000, 4000, 8000].includes(delay)) {
    const id = timerId--;
    timers.set(id, { callback, delay });
    scheduledDelays.push(delay);
    return id;
  }
  return originalTimeout(callback, delay);
}) as typeof window.setTimeout;
window.clearTimeout = (id) => {
  if (id !== undefined && timers.delete(id)) return;
  originalClearTimeout(id);
};
function completePage(request: typeof requests[number]) {
  const start = Number(request.url.searchParams.get("offset"));
  complete(request, "a".repeat(Math.min(65536, Number(request.url.searchParams.get("byteLength")) - start)));
}
async function retryPage(delay: number) {
  const operation = watcher.operations.at(-1)!;
  await until(() => timers.size > 0 || operation.settled);
  const timer = timers.entries().next().value;
  if (!timer) throw new Error("automatic recovery did not schedule the failed page");
  check("bounded backoff delay", timer[1].delay, delay);
  const before = requests.length;
  timers.delete(timer[0]); timer[1].callback();
  await until(() => requests.length > before);
  return requests.at(-1)!;
}
try {
  render("unmount");
  await until(() => requests.length >= 2);
  check("StrictMode cancels the replaced effect", requests[0].signal.aborted, true);
  flushSync(() => root.render(null));
  check("unmount aborts the pending page", requests[1].signal.aborted, true);
  for (const request of requests) request.reject(new DOMException("Aborted", "AbortError"));
  await Promise.all(watcher.operations.map(operation => operation.pending));
  check("unmounted loadAll promises finish without spinning", watcher.operations.map(operation => operation.error), [null, null]);
  check("unmount leaves no content", host.textContent, "");

  const oldStart = requests.length;
  render("old-reference");
  await until(() => requests.length >= oldStart + 2);
  render("new-reference");
  await until(() => requests.length >= oldStart + 3);
  const oldRequests = requests.slice(oldStart, oldStart + 2);
  check("reference change aborts the old page", oldRequests.every(request => request.signal.aborted), true);
  complete(requests.at(-1)!, "new view");
  await until(() => host.textContent === "new view");
  for (const request of oldRequests) complete(request, "old view");
  await Promise.all(watcher.operations.map(operation => operation.pending));
  check("late old pages cannot contaminate the new view", host.textContent, "new view");
  check("reference-change loadAll promises finish without spinning", watcher.operations.slice(2).map(operation => operation.error), [null, null, null]);

  const recoveringStart = requests.length;
  render("transient", "200000");
  await until(() => requests.length > recoveringStart);
  completePage(requests.at(-1)!);
  await until(() => requests.length === recoveringStart + 2);
  requests.at(-1)!.reject(new TypeError("Failed to fetch"));
  await settle();
  check("transient failure has no Retry button", host.querySelectorAll("button").length, 0);
  check("transient failure has no alert", host.querySelectorAll('[role="alert"]').length, 0);
  (await retryPage(500)).resolve(Response.json({ error: "temporary" }, { status: 503 }));
  completePage(await retryPage(1000));
  for (let count = 4; count < 6; count++) {
    await until(() => requests.length > recoveringStart + count);
    completePage(requests.at(-1)!);
  }
  await watcher.operations.at(-1)!.pending;
  await until(() => host.textContent?.length === 200000);
  check("automatic recovery completes all pages through the component", host.textContent, "a".repeat(200000));
  check("failed page cursor is reused before advancing", requests.slice(recoveringStart).map(request => request.url.searchParams.get("offset")), ["0", "65536", "65536", "65536", "131072", "196608"]);

  const exhaustedStart = requests.length;
  const delayStart = scheduledDelays.length;
  render("exhausted", "200000");
  await until(() => requests.length > exhaustedStart);
  completePage(requests.at(-1)!);
  await until(() => requests.length === exhaustedStart + 2);
  requests.at(-1)!.reject(new TypeError("Failed to fetch"));
  for (const delay of [500, 1000, 2000, 4000, 8000]) {
    (await retryPage(delay)).resolve(Response.json({ error: "temporary" }, { status: 503 }));
  }
  await watcher.operations.at(-1)!.pending;
  await until(() => !host.querySelector('[role="status"]'));
  check("exhaustion preserves loaded content silently", host.textContent, "a".repeat(65536));
  check("exhaustion has no buttons or alert", host.querySelectorAll('button, [role="alert"]').length, 0);
  check("exhaustion uses initial attempt plus five retries", requests.length - exhaustedStart, 7);
  check("exhaustion uses the bounded delay sequence", scheduledDelays.slice(delayStart), [500, 1000, 2000, 4000, 8000]);
  window.dispatchEvent(new Event("online"));
  await settle();
  check("online cannot restart an exhausted reader", requests.length - exhaustedStart, 7);

  const cancelledStart = requests.length;
  render("cancel-backoff");
  await until(() => requests.length > cancelledStart);
  requests.at(-1)!.reject(new TypeError("Failed to fetch"));
  await until(() => timers.size === 1);
  render("replacement");
  await until(() => requests.length === cancelledStart + 2);
  check("reference change clears the old retry timer", timers.size, 0);
  complete(requests.at(-1)!, "new view");
  await Promise.all(watcher.operations.map(operation => operation.pending));
  await until(() => host.textContent === "new view");
  check("cancelled recovery cannot contaminate the replacement", host.textContent, "new view");

  const permanentStart = requests.length;
  render("forbidden");
  await until(() => requests.length > permanentStart);
  requests.at(-1)!.resolve(Response.json({ error: "forbidden" }, { status: 403 }));
  await watcher.operations.at(-1)!.pending;
  await until(() => Boolean(host.querySelector('[role="alert"]')));
  check("authorization failure remains unavailable", Boolean(host.querySelector('[role="alert"]')), true);
  check("permanent failure has no retry button or timer", [host.querySelectorAll("button").length, timers.size], [0, 0]);
} catch (error) {
  check("component exception", String(error), "no exception");
} finally {
  root.unmount();
  watcher.restore();
  window.fetch = originalFetch;
  window.setTimeout = originalTimeout;
  window.clearTimeout = originalClearTimeout;
  clearTranscriptContentRangeCache();
}
document.getElementById("results")!.textContent = JSON.stringify(results);
