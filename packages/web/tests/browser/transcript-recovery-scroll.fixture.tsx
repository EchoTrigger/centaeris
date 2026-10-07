import { useRef, useState } from "react";
import { createRoot } from "react-dom/client";
import { flushSync } from "react-dom";
import "../../src/globals.css";
import { configureApi } from "../../src/api";
import { i18n } from "../../src/i18n";
import { TranscriptBlockList } from "../../src/chat/TranscriptBlockList";
import { WorkspaceComposer } from "../../src/chat/WorkspaceComposer";
import { createTranscriptViewStore } from "../../src/chat/transcriptViewStore";
import type { TranscriptBlock } from "../../src/chat/transcriptContract";
import { waitForLayout } from "./layoutGeometry";

// Real transcript, store, composer and styles; all records and metadata are local.
const parameters = new URLSearchParams(location.search);
configureApi({ apiBaseUrl: location.origin });
document.documentElement.dataset.theme = parameters.get("theme") ?? "light";
await i18n.changeLanguage(parameters.get("language") ?? "en");
const requests: string[] = [];
const failures: string[] = [];
window.addEventListener("error", event => failures.push(event.message));
window.addEventListener("unhandledrejection", event => failures.push(String(event.reason)));
window.fetch = async input => {
  const url = new URL(String(input), location.href);
  requests.push(url.pathname);
  if (url.pathname === "/api/sessions/synthetic-recovery/transcript/turn-metadata") return Response.json({ turns: [] });
  throw new Error(`Unexpected synthetic recovery request: ${url.pathname}`);
};
const store = createTranscriptViewStore();
const draft = "尚未发送的恢复验收草稿 — synthetic unsent text";
const noop = () => {};
function blocks(generation: number): TranscriptBlock[] {
  return Array.from({ length: 48 }, (_, index) => {
    const sequence = index + 1;
    const turn = Math.ceil(sequence / 2);
    const user = sequence % 2 === 1;
    const paragraph = `Synthetic answer ${turn}. 中文历史正文。 This paragraph belongs to the same authoritative message after recovery.`;
    const paragraphs = generation > 1 && turn === 11 ? 18 : generation > 2 && turn === 24 ? 10 : 2;
    const text = user ? `Synthetic question ${turn} / 中文问题 ${turn}` :
      Array.from({ length: paragraphs }, () => paragraph).join("\n\n");
    return { blockId: `${user ? "user" : "answer"}-${turn}`, blockRevision: String(generation),
      orderKey: { sourceSequence: String(sequence), ordinal: 0 },
      body: user ? { kind: "userText", content: { inlineContent: text, sourceRef: null } } :
        { kind: "assistantText", status: "completed", content: { inlineContent: text, sourceRef: null } } };
  });
}
function page(items: TranscriptBlock[], cursor: string | null, generation: number) {
  return { schema: "transcript.page.v1", sessionId: "synthetic-recovery", projectionVersion: "transcript.projection.v1",
    projectionGeneration: `generation-${generation}`, sourceHighWater: String(48 + generation), blocks: items,
    olderCursor: cursor, hasOlder: cursor !== null,
    resumeCursors: [{ streamId: "workspace-transcript.v1", cursor: String(48 + generation) }] };
}
const initial = blocks(1);
const epoch = store.openTail(page(initial.slice(32), "older-1", 1));
store.prependPage(page(initial.slice(16, 32), "older-2", 1), epoch);
store.prependPage(page(initial.slice(0, 16), null, 1), epoch);
function Session() {
  const [text, setText] = useState(draft);
  const input = useRef<HTMLInputElement>(null);
  return <main className="workspaceWorkbench withoutSidebar">
    <header className="workspaceTopbar">Synthetic recovery Session</header>
    <section className="workspaceChatColumn"><div className="workspaceConversationPlane">
      <TranscriptBlockList store={store} sessionId="synthetic-recovery" loadingHistory={false} loadingOlderHistory={false} onLoadOlderHistory={async () => { throw new Error("Recovery fixture already loaded its bounded older range"); }} />
      <WorkspaceComposer fileInputRef={input} isHome={false} onSubmit={event => event.preventDefault()}
        pendingAttachments={[]} pendingUploadFiles={[]} draft={text} onDraftChange={setText} enterStartsNewLine={false}
        activeAgentName="Synthetic Agent" workspaceAvailable sending={false} loadingHistory={false} hasActiveAgentRun={false}
        uploadingAttachment={false} onUploadAttachment={noop} sessionId={null} currentModel={null} thinkingMode=""
        onThinkingModeChange={noop} modelGroups={[]} modelId="" onModelIdChange={noop} onCancelActiveAgentRun={noop} />
    </div></section>
  </main>;
}
const root = createRoot(document.getElementById("fixture")!);
flushSync(() => root.render(<Session />));
function region() { return document.querySelector<HTMLElement>(".workspaceMessages")!; }
function row(id: string) { return region().querySelector<HTMLElement>(`[data-block-id="${id}"]`)!; }
function geometry() {
  const element = region();
  const bounds = element.getBoundingClientRect();
  const visible = [...element.querySelectorAll<HTMLElement>("[data-block-id]")]
    .find(node => { const rect = node.getBoundingClientRect(); return rect.bottom > bounds.top && rect.top < bounds.bottom; });
  return { blockId: visible?.dataset.blockId ?? null, offset: visible ? visible.getBoundingClientRect().top - bounds.top : null,
    targetOffset: row("user-12").getBoundingClientRect().top - bounds.top,
    scrollTop: element.scrollTop, bottomDistance: element.scrollHeight - element.clientHeight - element.scrollTop,
    aboveHeight: row("answer-11").getBoundingClientRect().height,
    latestHeight: row("answer-24").getBoundingClientRect().height,
    draft: document.querySelector<HTMLTextAreaElement>(".workspaceComposer textarea")!.value,
    generation: store.getListSnapshot().projectionGeneration, revision: store.getBlockSnapshot("answer-11")!.blockRevision,
    count: store.getListSnapshot().blockIds.length, following: document.querySelector(".workspaceJumpToLatest") === null };
}
const settle = () => waitForLayout([".workspaceMessages", ".workspaceTranscriptBlocks", "[data-block-id=answer-11]", "[data-block-id=user-12]", ".workspaceComposer"]);
const fixture = {
  ready: false, requests, failures, draft, geometry, settle,
  async alignReading() {
    // Following has already been paused by a real Chromium wheel event.
    for (let attempt = 0; attempt < 3; attempt++) {
      const element = region();
      element.scrollTop += row("user-12").getBoundingClientRect().top - element.getBoundingClientRect().top;
      await settle();
    }
  },
  recover(generation: number) {
    const authoritative = blocks(generation);
    store.openTail(page(authoritative.slice(32), "older-1", generation), [
      page(authoritative.slice(16, 32), "older-2", generation), page(authoritative.slice(0, 16), null, generation),
    ]);
  },
};
Object.assign(window, { transcriptRecoveryFixture: fixture });
await settle();
fixture.ready = true;
