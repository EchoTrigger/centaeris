import { createRoot } from "react-dom/client";
import { flushSync } from "react-dom";
import { useRef, useState } from "react";
import "../../src/globals.css";
import { configureApi } from "../../src/api";
import { i18n } from "../../src/i18n";
import { TranscriptBlockList } from "../../src/chat/TranscriptBlockList";
import { WorkspaceComposer } from "../../src/chat/WorkspaceComposer";
import { createTranscriptViewStore } from "../../src/chat/transcriptViewStore";
import { TranscriptToolOperationRegistry } from "../../src/chat/transcriptToolOperations";
import type { TranscriptBlock } from "../../src/chat/transcriptContract";
import type { SessionStreamEvent } from "../../src/chat/streamTypes";
import { waitForLayout } from "./layoutGeometry";

const parameters = new URLSearchParams(location.search);
const compact = parameters.has("compact");
const older = parameters.has("older");
let olderRequests = 0;
configureApi({ apiBaseUrl: location.origin });
document.documentElement.dataset.theme = parameters.get("theme") ?? "light";
await i18n.changeLanguage(parameters.get("language") ?? "en");
const failures: string[] = [];
const requests: string[] = [];
window.addEventListener("error", event => failures.push(event.message));
window.addEventListener("unhandledrejection", event => failures.push(String(event.reason)));
window.fetch = async input => {
  const url = new URL(String(input), location.href);
  requests.push(url.pathname);
  if (url.pathname === "/api/sessions/synthetic-disclosure/transcript/turn-metadata") return Response.json({ turns: [] });
  if (url.pathname === "/api/sessions/synthetic-disclosure/transcript/citations") return Response.json({ sessionId: "synthetic-disclosure", bindings: [] });
  throw new Error(`Unexpected disclosure fixture request: ${url.pathname}`);
};
const store = createTranscriptViewStore();
const registry = new TranscriptToolOperationRegistry();
for (const event of [
  { type: "tool_call", payload: { callId: "synthetic-call", toolName: "bash", normalizedInput: { command: "echo synthetic" } } },
  { type: "tool_result", payload: { callId: "synthetic-call", resultState: "succeeded", operations: [{ callId: "synthetic-call", toolName: "bash", kind: "command", status: "completed", resultState: "succeeded", outputPreview: Array.from({ length: compact ? 5 : 24 }, (_, i) => `Synthetic output ${i}`).join("\n") }] } },
]) registry.applyEvent(event as SessionStreamEvent);
const blocks: TranscriptBlock[] = Array.from({ length: parameters.has("noHistory") ? 0 : 12 }, (_, i) => ({ blockId: `history-${i}`, blockRevision: "1", orderKey: { sourceSequence: String(i + 1), ordinal: 0 }, body: { kind: "assistantText", status: "completed", content: { inlineContent: "Synthetic history.\n\n".repeat(4), sourceRef: null } } }));
blocks.push(
  { blockId: "synthetic-question", blockRevision: "1", orderKey: { sourceSequence: "13", ordinal: 0 }, body: { kind: "userText", content: { inlineContent: "Synthetic question", sourceRef: null } } },
  { blockId: "synthetic-reason", blockRevision: "1", orderKey: { sourceSequence: "14", ordinal: 0 }, body: { kind: "reasoning", requestId: "synthetic-request", status: "completed", content: { inlineContent: "Synthetic reasoning detail.\n\n".repeat(compact ? 3 : 15), sourceRef: null } } },
  { blockId: "synthetic-tool", blockRevision: "1", orderKey: { sourceSequence: "15", ordinal: 0 }, body: { kind: "tool", callId: "synthetic-call", toolName: "bash", status: "completed", summary: "Synthetic command", summaryRef: null, outputRef: null } },
  { blockId: "synthetic-answer", blockRevision: "1", orderKey: { sourceSequence: "16", ordinal: 0 }, body: { kind: "assistantText", status: "completed", content: { inlineContent: "Synthetic final answer.\n\nA complete local response.", sourceRef: null } } },
);
store.openTail({ schema: "transcript.page.v1", sessionId: "synthetic-disclosure", projectionVersion: "transcript.projection.v1", projectionGeneration: "generation-1", sourceHighWater: "16", blocks, olderCursor: older ? "synthetic-older" : null, hasOlder: older, resumeCursors: [{ streamId: "workspace-transcript.v1", cursor: "16" }] });
const root = createRoot(document.getElementById("fixture")!);
const draft = "保留的未发送草稿 — synthetic unsent text";
const noop = () => {};
function Session() {
  const [text, setText] = useState(draft);
  const input = useRef<HTMLInputElement>(null);
  return <main className="workspaceWorkbench withoutSidebar"><section className="workspaceChatColumn"><div className="workspaceConversationPlane">
    <TranscriptBlockList store={store} toolOperations={registry} sessionId="synthetic-disclosure" loadingHistory={false} loadingOlderHistory={false} onLoadOlderHistory={async () => { olderRequests++; }} />
    <WorkspaceComposer fileInputRef={input} isHome={false} onSubmit={event => event.preventDefault()}
      pendingAttachments={[]} pendingUploadFiles={[]} draft={text} onDraftChange={setText} enterStartsNewLine={false}
      activeAgentName="Synthetic Agent" workspaceAvailable sending={false} loadingHistory={false} hasActiveAgentRun={false}
      uploadingAttachment={false} onUploadAttachment={noop} sessionId={null} currentModel={null} thinkingMode=""
      onThinkingModeChange={noop} modelGroups={[]} modelId="" onModelIdChange={noop} onCancelActiveAgentRun={noop} />
  </div></section></main>;
}
flushSync(() => root.render(<Session />));
const selectors = { work: ".workspaceTranscriptTurn:last-child .workProgressSummary", reasoning: ".workspaceTranscriptTurn:last-child .workspaceReasoning button", group: ".workspaceTranscriptTurn:last-child .workspaceActivityGroupRecord > button", node: ".workspaceTranscriptTurn:last-child .agent-tool-node-summary" };
function region() { return document.querySelector<HTMLElement>(".workspaceMessages")!; }
function button(kind: keyof typeof selectors) { return document.querySelector<HTMLButtonElement>(selectors[kind])!; }
const settle = () => waitForLayout([".workspaceMessages", ".workspaceTranscriptBlocks", ".workspaceTranscriptTurn:last-child", ".workspaceTranscriptTurn:last-child .workProgressSummary"]);
const fixture = {
  ready: false, failures, requests, selectors, settle, draft,
  olderRequests: () => olderRequests,
  async prepare(kind: keyof typeof selectors) {
    if (kind !== "work" && button("work").getAttribute("aria-expanded") === "false") { button("work").click(); await settle(); }
    if (kind === "node" && button("group").getAttribute("aria-expanded") === "false") { button("group").click(); await settle(); }
    if (button(kind).getAttribute("aria-expanded") === "true") { button(kind).click(); await settle(); }
    document.querySelector<HTMLButtonElement>(".workspaceJumpToLatest")?.click();
    await settle();
  },
  geometry(kind: keyof typeof selectors) {
    const element = region(); const bounds = element.getBoundingClientRect(); const rect = button(kind).getBoundingClientRect();
    const detail = button(kind).nextElementSibling?.getBoundingClientRect();
    return { offset: rect.top - bounds.top, viewportY: rect.top, parentLineY: button("work").getBoundingClientRect().bottom,
      headerBottom: rect.bottom - bounds.top, detailTop: detail ? detail.top - bounds.top : null, detailHeight: detail?.height ?? 0,
      scrollTop: element.scrollTop, bottomDistance: element.scrollHeight - element.clientHeight - element.scrollTop,
      maxScroll: element.scrollHeight - element.clientHeight, expanded: button(kind).getAttribute("aria-expanded"),
      following: document.querySelector(".workspaceJumpToLatest") === null,
      draft: document.querySelector<HTMLTextAreaElement>(".workspaceComposer textarea")!.value };
  },
  alignReading(kind: keyof typeof selectors) { const element = region(); element.scrollTop += button(kind).getBoundingClientRect().top - element.getBoundingClientRect().top - 160; },
  track(kind: keyof typeof selectors) {
    const samples: { viewportY: number; parentLineY: number }[] = [];
    let frame = 0;
    const sample = () => { samples.push(fixture.geometry(kind)); frame = requestAnimationFrame(sample); };
    frame = requestAnimationFrame(sample);
    Object.assign(window, { disclosureFrames: { stop() { cancelAnimationFrame(frame); return samples; } } });
  },
};
Object.assign(window, { disclosureFixture: fixture });
await settle(); fixture.ready = true;
