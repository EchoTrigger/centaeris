// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { TranscriptBlockList } from "../../src/chat/TranscriptBlockList";
import { createTranscriptViewStore } from "../../src/chat/transcriptViewStore";
import { SessionReadController, type SessionAttention } from "../../src/shell/sessionReadState";

vi.mock("../../src/i18n", () => ({ useTranslation: () => ({ t: (key: string) => key }) }));
vi.mock("../../src/chat/useTranscriptCitations", () => ({ useTranscriptCitations: () => ({ citations: new Map(), error: false }) }));
vi.mock("../../src/chat/useTranscriptTurnMetadata", () => ({ useTranscriptTurnMetadata: () => new Map() }));
vi.mock("../../src/chat/WorkProgress", () => ({ WorkProgress: ({ children }: { children: React.ReactNode }) => <div>{children}</div> }));
vi.mock("../../src/chat/TranscriptBlockContent", () => ({
  TranscriptBlockRow: () => <div data-block-id="answer"><p>Completed answer</p><button type="button" aria-expanded="false">Inspect details</button></div>,
  TranscriptLiveTail: () => null,
  TranscriptToolGroupCard: () => null,
}));

const geometry = { height: 707, contentEnd: 708 };
const resizeCallbacks = new Set<ResizeObserverCallback>();
beforeEach(() => {
  geometry.height = 707; geometry.contentEnd = 708;
  vi.stubGlobal("ResizeObserver", class {
    constructor(callback: ResizeObserverCallback) { this.callback = callback; resizeCallbacks.add(callback); }
    callback: ResizeObserverCallback;
    observe() {}
    disconnect() { resizeCallbacks.delete(this.callback); }
  });
  vi.stubGlobal("matchMedia", () => ({ matches: true }));
  vi.spyOn(Element.prototype, "clientHeight", "get").mockImplementation(function () {
    return this.classList.contains("workspaceMessages") ? geometry.height : 0;
  });
  vi.spyOn(Element.prototype, "getBoundingClientRect").mockImplementation(function () {
    const scroll = document.querySelector<HTMLElement>(".workspaceMessages");
    if (this.classList.contains("workspaceMessages")) return DOMRect.fromRect({ height: geometry.height });
    if (this.classList.contains("workspaceTranscriptBlocks")) return DOMRect.fromRect({
      y: -(scroll?.scrollTop ?? 0), height: geometry.contentEnd,
    });
    return DOMRect.fromRect({ height: 30 });
  });
});
afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.unstubAllGlobals(); resizeCallbacks.clear(); });
const resize = () => act(() => { for (const callback of resizeCallbacks) callback([], {} as ResizeObserver); });

function fixture(options: { hasOlder?: boolean; onLoadOlderHistory?: () => Promise<void> } = {}) {
  const store = createTranscriptViewStore();
  store.openTail({ schema: "transcript.page.v1", sessionId: "session", projectionVersion: "transcript.projection.v1",
    projectionGeneration: "generation", sourceHighWater: "18", olderCursor: options.hasOlder ? "older" : null, hasOlder: options.hasOlder ?? false,
    resumeCursors: [{ streamId: "workspace-transcript.v1", cursor: "18" }], blocks: [{
      blockId: "answer", blockRevision: "1", orderKey: { sourceSequence: "18", ordinal: 0 },
      body: { kind: "assistantText", content: { inlineContent: "Completed answer", sourceRef: null }, status: "completed" },
    }] });
  let session: SessionAttention = { id: "session", workspaceId: "workspace", agentId: "agent", isUnread: false,
    isManuallyUnread: false, latestAttentionSequence: "0", readThroughSequence: "0", attentionRevision: "0" };
  let readingLatest = false;
  let visible = true;
  const sent: string[] = [];
  const controller = new SessionReadController({
    send: async (record, throughSequence) => {
      sent.push(throughSequence);
      return { ...record, readThroughSequence: throughSequence, isUnread: record.isManuallyUnread,
        attentionRevision: String(BigInt(record.attentionRevision) + 1n) };
    }, onAcknowledged: record => { session = record; },
  });
  const observe = () => controller.observe({ session, visible, readingLatest, appliedSourceHighWater: "18" });
  const view = render(<TranscriptBlockList store={store} sessionId="session" loadingHistory={false} loadingOlderHistory={false}
    onLoadOlderHistory={options.onLoadOlderHistory ?? (async () => { throw new Error("unexpected history request"); })}
    onReadingLatestChange={(_sessionId, value) => { readingLatest = value; observe(); }} />);
  const messages = view.container.querySelector<HTMLElement>(".workspaceMessages")!;
  return { sent, messages, controller, store, getSession: () => session, reading: () => readingLatest,
    attention: (manual = false) => { session = { ...session, isUnread: true, latestAttentionSequence: "18",
      attentionRevision: "1", isManuallyUnread: manual }; observe(); },
    hide: () => { visible = false; observe(); } };
}

test("visible latest content is read while automatic following stays paused, including fractional bottom positions", async () => {
  const f = fixture();
  fireEvent.click(screen.getByRole("button", { name: "Inspect details" }));
  expect(screen.getByRole("button", { name: "virtualAgentRunList.jumpToLatest" })).toBeTruthy();
  f.messages.scrollTop = 0.6666666865;
  resize();
  await act(async () => { f.attention(); });
  expect(f.sent).toEqual(["18"]);
  expect(f.getSession().isUnread).toBe(false);
  // Reading recognition must not resume auto-following or move the reader.
  expect(screen.getByRole("button", { name: "virtualAgentRunList.jumpToLatest" })).toBeTruthy();
  expect(f.messages.scrollTop).toBeCloseTo(0.6666666865);
  f.controller.dispose();
});

test("paused reading only becomes read when content shrink makes the latest reply visible", async () => {
  const f = fixture();
  fireEvent.click(screen.getByRole("button", { name: "Inspect details" }));
  geometry.contentEnd = 1400; f.messages.scrollTop = 200; resize();
  await act(async () => { f.attention(); });
  expect(f.sent).toEqual([]);
  geometry.contentEnd = 708; f.messages.scrollTop = 0.6666666865; resize();
  await act(async () => {});
  expect(f.sent).toEqual(["18"]);
  f.controller.dispose();
});

test("visible geometry never overrides hidden-page or manual-unread choices", async () => {
  const f = fixture();
  fireEvent.click(screen.getByRole("button", { name: "Inspect details" }));
  await act(async () => { f.attention(true); });
  resize(); await act(async () => {});
  expect(f.sent).toEqual([]);
  f.hide();
  await act(async () => { f.attention(false); });
  resize(); await act(async () => {});
  expect(f.sent).toEqual([]);
  f.controller.dispose();
});


const upwardInputs: Record<string, (element: HTMLElement) => void> = {
  wheel: element => { fireEvent.wheel(element, { deltaY: -12 }); },
  touch: element => {
    fireEvent.touchStart(element, { touches: [{ clientY: 100 }] });
    fireEvent.touchMove(element, { touches: [{ clientY: 140 }] });
  },
  keyboard: element => { fireEvent.keyDown(element, { key: "ArrowUp" }); },
};
const allInputs = { ...upwardInputs, "downward wheel": (element: HTMLElement) => { fireEvent.wheel(element, { deltaY: 12 }); } };
const jumpButton = () => screen.queryByRole("button", { name: "virtualAgentRunList.jumpToLatest" });
const growStream = (f: ReturnType<typeof fixture>) => {
  act(() => {
    geometry.contentEnd = 1400;
    f.store.applyLiveOverlay({ messageId: "live-answer", turnId: "turn", afterSourceHighWater: "18", revision: 1,
      text: "New streamed answer continues", reasoning: null }, f.store.getListSnapshot().viewEpoch);
  });
  resize();
};

test.each(Object.entries(allInputs))("%s input without outer movement keeps following future streamed output", (_kind, input) => {
  geometry.contentEnd = 300;
  const f = fixture();
  expect(f.messages.scrollTop).toBe(0);
  input(f.messages);
  fireEvent.scroll(f.messages);
  expect(jumpButton()).toBeNull();
  growStream(f);
  expect(jumpButton()).toBeNull();
  expect(Math.abs(geometry.contentEnd - f.messages.clientHeight - f.messages.scrollTop)).toBeLessThanOrEqual(2);
  f.controller.dispose();
});

test.each(Object.entries(upwardInputs))("actual upward %s movement still pauses following", (_kind, input) => {
  geometry.contentEnd = 1400;
  const f = fixture();
  input(f.messages);
  f.messages.scrollTop -= 0.5;
  fireEvent.scroll(f.messages);
  expect(jumpButton()).toBeTruthy();
  const position = f.messages.scrollTop;
  geometry.contentEnd = 1800; resize();
  expect(f.messages.scrollTop).toBe(position);
  f.controller.dispose();
});

test.each(Object.entries(upwardInputs))("top-edge %s intent can load real older history without existing movement", async (_kind, input) => {
  geometry.contentEnd = 300;
  const loadOlder = vi.fn(async () => {});
  const f = fixture({ hasOlder: true, onLoadOlderHistory: loadOlder });
  await act(async () => { input(f.messages); });
  expect(loadOlder).toHaveBeenCalledTimes(1);
  expect(jumpButton()).toBeTruthy();
  f.controller.dispose();
});

test.each(Object.entries(upwardInputs))("nested output consumes %s input without detaching the outer conversation", (_kind, input) => {
  const f = fixture();
  const output = document.createElement("div");
  output.style.overflowY = "auto";
  Object.defineProperties(output, { clientHeight: { value: 100 }, scrollHeight: { value: 300 } });
  output.scrollTop = 40;
  f.messages.append(output);
  input(output);
  output.scrollTop = 30;
  fireEvent.scroll(output);
  expect(jumpButton()).toBeNull();
  growStream(f);
  expect(jumpButton()).toBeNull();
  expect(Math.abs(geometry.contentEnd - f.messages.clientHeight - f.messages.scrollTop)).toBeLessThanOrEqual(2);
  f.controller.dispose();
});


test("compositor-applied wheel movement is compared with the last observed position, before the passive wheel handler", () => {
  geometry.contentEnd = 7000;
  const f = fixture();
  const previous = f.messages.scrollTop;
  // Chromium can apply passive-wheel scrolling before delivering the wheel
  // listener. Capturing scrollTop in that listener loses the actual movement.
  f.messages.scrollTop = previous - 3000;
  fireEvent.wheel(f.messages, { deltaY: -3000 });
  fireEvent.scroll(f.messages);
  expect(jumpButton()).toBeTruthy();
  const position = f.messages.scrollTop;
  geometry.contentEnd += 400; resize();
  expect(f.messages.scrollTop).toBe(position);
  f.controller.dispose();
});


test("programmatic content shrink updates the input baseline without detaching following", () => {
  geometry.contentEnd = 1400;
  const f = fixture();
  geometry.contentEnd = 1000; resize();
  const position = f.messages.scrollTop;
  expect(position).toBe(geometry.contentEnd - f.messages.clientHeight);
  fireEvent.wheel(f.messages, { deltaY: -12 });
  fireEvent.scroll(f.messages);
  expect(jumpButton()).toBeNull();
  expect(f.messages.scrollTop).toBe(position);
  f.controller.dispose();
});
