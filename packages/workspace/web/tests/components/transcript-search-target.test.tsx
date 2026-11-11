// @vitest-environment jsdom
import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import userEvent from "@testing-library/user-event";
import { TranscriptBlockList } from "../../src/chat/TranscriptBlockList";
import { createTranscriptViewStore } from "../../src/chat/transcriptViewStore";
import { useTranscriptSearchTarget } from "../../src/chat/useTranscriptSearchTarget";
import type { createWorkspaceTranscriptTransport } from "../../src/chat/transcriptTransport";

vi.mock("../../src/i18n", () => ({ useTranslation: () => ({ t: (key: string) => key }) }));
vi.mock("../../src/chat/useTranscriptCitations", () => ({ useTranscriptCitations: () => ({ citations: new Map(), error: false }) }));
vi.mock("../../src/chat/useTranscriptTurnMetadata", () => ({ useTranscriptTurnMetadata: () => new Map() }));

beforeEach(() => {
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  vi.stubGlobal("CSS", { escape: (value: string) => value });
  vi.stubGlobal("matchMedia", () => ({ matches: true }));
});
afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

function page() {
  return { schema: "transcript.page.v1", sessionId: "session", projectionVersion: "transcript.projection.v1",
    projectionGeneration: "generation", sourceHighWater: "1000", olderCursor: null, hasOlder: false,
    resumeCursors: [{ streamId: "workspace-transcript.v1", cursor: "1000" }], blocks: [
      { blockId: "user", blockRevision: "1", orderKey: { sourceSequence: "1", ordinal: 0 }, body: { kind: "userText", content: { inlineContent: "权限问题", sourceRef: null } } },
      { blockId: "stage", blockRevision: "1", orderKey: { sourceSequence: "2", ordinal: 0 }, body: { kind: "notice", noticeType: "model_process_summary", status: "running", content: { inlineContent: "正在核对 **权限** 和 `权限`，见 [权限说明](/help)。", sourceRef: null } } },
      { blockId: "answer", blockRevision: "1", orderKey: { sourceSequence: "3", ordinal: 0 }, body: { kind: "assistantText", status: "completed", content: { inlineContent: "最终权限答复", sourceRef: null } } },
    ] };
}

test("pending history reserves the conversation without substituting loading or empty text", () => {
  const store = createTranscriptViewStore();
  const props = { store, sessionId: "session", loadingHistory: true, loadingOlderHistory: false,
    onLoadOlderHistory: async () => {}, emptyState: <p>Authoritative empty conversation</p> };
  const view = render(<TranscriptBlockList {...props} />);
  const region = screen.getByRole("region", { name: "virtualAgentRunList.conversationMessages" });
  expect(region.getAttribute("aria-busy")).toBe("true");
  expect(region.textContent).toBe("");
  view.rerender(<TranscriptBlockList {...props} loadingHistory={false} />);
  expect(region.getAttribute("aria-busy")).toBe("false");
  expect(screen.getByText("Authoritative empty conversation")).toBeTruthy();
});

test("a stage hit expands its process, highlights only the hit block and pauses latest reading", async () => {
  const store = createTranscriptViewStore();
  store.openTail(page());
  const reading = vi.fn();
  const returnLatest = vi.fn();
  const view = render(<TranscriptBlockList store={store} sessionId="session" loadingHistory={false} loadingOlderHistory={false}
    onLoadOlderHistory={async () => {}} onReadingLatestChange={reading}
    searchTarget={{ blockId: "stage", query: "权限", onReturnLatest: returnLatest }} />);
  const target = view.container.querySelector<HTMLElement>('[data-block-id="stage"]')!;
  await waitFor(() => expect(document.activeElement).toBe(target));
  expect(within(target).getAllByText("权限", { selector: "mark" })).toHaveLength(3);
  expect(within(target).getByRole("link", { name: "权限说明" }).getAttribute("href")).toBe("/help");
  expect(view.container.querySelector('[data-block-id="answer"] mark')).toBeNull();
  const disclosure = screen.getByRole("button", { expanded: true });
  expect(disclosure).toBeTruthy();
  expect(reading.mock.calls.every(([, latest]) => latest === false)).toBe(true);
  screen.getByRole("button", { name: "virtualAgentRunList.jumpToLatest" }).focus();
  const retried = createTranscriptViewStore();
  retried.openTail(page());
  view.rerender(<TranscriptBlockList store={retried} sessionId="session" loadingHistory={false} loadingOlderHistory={false}
    onLoadOlderHistory={async () => {}} onReadingLatestChange={reading}
    searchTarget={{ blockId: "stage", query: "权限", onReturnLatest: returnLatest }} />);
  await waitFor(() => expect(document.activeElement).toBe(view.container.querySelector('[data-block-id="stage"]')));
  await userEvent.click(screen.getByRole("button", { name: "virtualAgentRunList.jumpToLatest" }));
  expect(returnLatest).toHaveBeenCalledOnce();
});

test("a late target response does not replace a newer target window", async () => {
  let oldSignal!: AbortSignal;
  let finishOld!: (value: unknown) => void;
  const transport = { loadTarget: vi.fn((_session: string, event: string, signal: AbortSignal) => {
    if (event === "old") { oldSignal = signal; return new Promise(resolve => { finishOld = resolve; }); }
    return Promise.resolve({ page: page(), targetBlockId: "answer" });
  }) } as unknown as ReturnType<typeof createWorkspaceTranscriptTransport>;
  function Target({ event }: { event: string }) {
    const target = useTranscriptSearchTarget("session", event, transport);
    return <span>{target.blockId || "loading"}</span>;
  }
  const view = render(<Target event="old" />);
  view.rerender(<Target event="new" />);
  await screen.findByText("answer");
  expect(oldSignal.aborted).toBe(true);
  finishOld({ page: page(), targetBlockId: "stage" });
  await waitFor(() => expect(screen.queryByText("stage")).toBeNull());
});
