import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { flushSync } from "react-dom";
import "../../src/globals.css";
import { i18n } from "../../src/i18n";
import { configureApi } from "../../src/api";
import { TranscriptBlockRow } from "../../src/chat/TranscriptBlockContent";
import { createTranscriptViewStore } from "../../src/chat/transcriptViewStore";
import { clearTranscriptContentRangeCache } from "../../src/chat/transcriptContentRanges";
import type { TranscriptBlock } from "../../src/chat/transcriptContract";

const parameters = new URLSearchParams(location.search);
configureApi({ apiBaseUrl: location.origin });
document.documentElement.dataset.theme = parameters.get("theme") ?? "light";
void i18n.changeLanguage(parameters.get("language") ?? "en");
const host = document.getElementById("host")!;
host.className = "workspaceTranscriptBlocks";
host.style.padding = "24px";
const root = createRoot(host);
const store = createTranscriptViewStore();
const encoder = new TextEncoder();
const parts = ["第一段 **完整**\n" + "中".repeat(21000), "文".repeat(21000), "结".repeat(6000) + "\n原文末尾"];
const rawReference = parts.join("");
const reference = { refId: "session-event:long:modelMarkdown", revision: "1", byteLength: String(encoder.encode(rawReference).byteLength) };
let clipboardError = false;
const copied: string[] = [];
Object.defineProperty(navigator, "clipboard", { configurable: true, value: {
  async writeText(text: string) {
    if (clipboardError) throw new DOMException("Clipboard unavailable", "NotAllowedError");
    copied.push(text);
  },
} });
const requests: { url: URL; signal: AbortSignal; resolve(value: Response): void }[] = [];
window.fetch = ((input: RequestInfo | URL, options?: RequestInit) => {
  const url = new URL(String(input), location.href);
  if (!url.pathname.endsWith("/transcript/content")) throw new Error(`Unexpected request: ${url.pathname}`);
  return new Promise<Response>((resolve, reject) => {
    const signal = options?.signal as AbortSignal;
    signal.addEventListener("abort", () => reject(new DOMException("Aborted", "AbortError")), { once: true });
    requests.push({ url, signal, resolve });
  });
}) as typeof fetch;

function block(id: string, sequence: number, body: TranscriptBlock["body"], atMs: number | null = 1791095700000): TranscriptBlock {
  return { blockId: id, blockRevision: "1", orderKey: { sourceSequence: String(sequence), ordinal: 0 }, body,
    presentation: atMs === null ? null : { sourceType: body.kind === "userText" ? "user_message" : body.kind === "notice" ? "phase_event" : "assistant_message", observedAtMs: atMs,
      agentRunId: "fixture-run", displayTarget: null, durationMs: null, operation: null } };
}
const inline = (text: string) => ({ inlineContent: text, sourceRef: null });
const initial = [
  block("user", 1, { kind: "userText", content: inline("你好") }),
  block("answer", 2, { kind: "assistantText", content: inline("**回答**\n完整原文"), status: "completed" }),
  block("stage", 3, { kind: "notice", noticeType: "review", content: inline("阶段总结 **原文**"), status: "running" }),
  block("reasoning", 4, { kind: "reasoning", requestId: "private-request", content: inline("内部思考不可复制"), status: "completed" }),
  block("running", 5, { kind: "assistantText", content: inline("流式未完成"), status: "running" }),
  block("no-time", 6, { kind: "assistantText", content: inline("无时间历史消息"), status: "completed" }, null),
];
function page(blocks: TranscriptBlock[], highWater = "20", olderCursor: string | null = null) {
  return { schema: "transcript.page.v1", sessionId: "fixture-session", projectionVersion: "transcript.projection.v1", projectionGeneration: "generation-1",
    sourceHighWater: highWater, blocks, olderCursor, hasOlder: olderCursor !== null, resumeCursors: [{ streamId: "workspace-transcript.v1", cursor: highWater }] };
}
let epoch = store.openTail(page(initial));
function render() { flushSync(() => root.render(<StrictMode>{store.getListSnapshot().blockIds.map(id => <TranscriptBlockRow key={id} store={store} blockId={id} />)}</StrictMode>)); }
render();

function row(id: string) { return host.querySelector<HTMLElement>(`[data-block-id="${id}"]`)!; }
function resolve(index: number, failure = false) {
  const request = requests[index];
  if (failure) { request.resolve(Response.json({ error: "forbidden" }, { status: 403 })); return; }
  const start = request.url.searchParams.get("offset")!;
  let offset = 0;
  const content = parts.find(part => { if (String(offset) === start) return true; offset += encoder.encode(part).byteLength; return false; });
  if (content === undefined) throw new Error(`Unknown offset ${start}`);
  const end = Number(start) + encoder.encode(content).byteLength;
  request.resolve(Response.json({ schema: "transcript.content.range.v1", sessionId: "fixture-session", projectionVersion: "transcript.projection.v1",
    projectionGeneration: "generation-1", refId: reference.refId, revision: "1", byteLength: reference.byteLength,
    startOffset: start, endOffset: String(end), content, hasMore: end < Number(reference.byteLength) }));
}
Object.assign(window, { messageActionsFixture: {
  ready: true, row, copied, requests, rawReference, parts, resolve,
  clipboardFailure(value: boolean) { clipboardError = value; },
  completeRunning() {
    const next = { ...initial[4], blockRevision: "2", body: { kind: "assistantText", content: inline("流式完成 **完整回答**"), status: "completed" } };
    flushSync(() => store.applyPatchPage({ schema: "transcript.patch.page.v1", sessionId: "fixture-session", projectionVersion: "transcript.projection.v1", projectionGeneration: "generation-1",
      throughSourceHighWater: "21", patches: [{ schema: "transcript.patch.v1", sessionId: "fixture-session", projectionVersion: "transcript.projection.v1", projectionGeneration: "generation-1",
        sourceHighWater: "21", streamId: "workspace-transcript.v1", appliedCursor: "21", upserts: [next], removals: [] }], nextSourceHighWater: "21", hasMore: false }, epoch));
  },
  loadHistory() { flushSync(() => store.prependPage(page([block("history", 0, { kind: "assistantText", content: inline("较早 **历史原文**"), status: "completed" }, 1791009300000)]), epoch)); render(); },
  reference() { clearTranscriptContentRangeCache(); epoch = store.openTail(page([block("long", 1, { kind: "assistantText", content: { inlineContent: null, sourceRef: reference }, status: "completed" })])); render(); },
  replace() { flushSync(() => { store.clear(); }); render(); },
} });
