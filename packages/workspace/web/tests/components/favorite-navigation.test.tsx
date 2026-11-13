// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useSyncExternalStore } from "react";
import { FrontendProvider } from "@centaeris/ui/provider";
import { RouterProvider } from "react-router";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { i18n } from "../../src/i18n";
import type { streamWorkspacePresentation } from "../../src/chat/workspacePresentationTransport";
import type { TranscriptViewStore } from "../../src/chat/transcriptViewStore";
import { configureApi } from "../../src/api";
import { createRouter } from "../../src/router";

const sessions = ["first", "second"].map(id => ({ id, workspaceId: "demo", status: "active", title: `${id} favorite`, isPinned: true, origin: "user", projectId: null, currentEnvironmentId: null, executionMode: null, deletedAt: null, isUnread: false, updatedAt: "2026-11-12T00:00:00Z", hasActiveAgentRun: false, latestAttentionSequence: "0", readThroughSequence: "0", attentionRevision: "0", isManuallyUnread: false }));
type Subscription = Parameters<typeof streamWorkspacePresentation>[0];
const fixture = vi.hoisted(() => ({ subscriptions: [] as Subscription[], writes: [] as unknown[] }));
vi.mock("../../src/chat/workspacePresentationTransport", () => ({
  streamWorkspacePresentation: (options: Subscription) => {
    fixture.subscriptions.push(options);
    options.onConnection("running");
    return new Promise<void>(resolve => options.signal.addEventListener("abort", () => resolve(), { once: true }));
  },
}));
vi.mock("../../src/chat/TranscriptBlockList", () => ({
  TranscriptBlockList({ store }: { store: TranscriptViewStore }) {
    const snapshot = useSyncExternalStore(store.subscribeList, store.getListSnapshot, store.getListSnapshot);
    return <section aria-label="Selected chat">{snapshot.blockIds.map(id => {
      const block = store.getBlockSnapshot(id);
      return <p key={id}>{block?.body.kind === "userText" ? block.body.content.inlineContent : null}</p>;
    })}</section>;
  },
}));
vi.mock("../../src/work/WorkPrototypeRoute", () => ({ default: () => <h1>Work page</h1> }));
beforeEach(async () => {
  await i18n.changeLanguage("en");
  configureApi({ apiBaseUrl: window.location.origin });
  sessionStorage.clear(); localStorage.clear();
  vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });
  vi.stubGlobal("fetch", async (url: string, options?: RequestInit) => {
    const path = new URL(url).pathname;
    if (path === "/api/me") return Response.json({ user: { id: "owner", email: "owner@example.com", isStaff: false, isSuperuser: false } });
    if (path === "/api/workspaces") return Response.json({ workspaces: [{ id: "demo", name: "Demo", role: "owner", avatarUrl: null }] });
    if (path === "/api/models") return Response.json({ models: [] });
    if (path.endsWith("/session-projects")) return Response.json({ projects: [] });
    if (path === "/api/workspaces/demo/sessions") return Response.json({ sessions });
    if (path.endsWith("/assets")) return Response.json({ assets: [] });
    if (path.endsWith("/active-agent-run")) return Response.json({ schema: "workspace.transcript.active_agent_run.v1", sessionId: path.split("/")[3], agentRun: null, pendingSupplements: [] });
    if (path.endsWith("/context-usage")) return Response.json({ schema: "session.context_usage.v1", sessionId: path.split("/")[3], contextUsage: null });
    if (path === "/api/csrf") return Response.json({ csrfToken: "test-token" });
    const session = sessions.find(item => path === `/api/sessions/${item.id}`);
    if (session) {
      if (options?.method === "PATCH") {
        const patch = JSON.parse(String(options.body)); fixture.writes.push(patch);
        return Response.json({ session: { ...session, ...patch } });
      }
      return Response.json({ session });
    }
    throw new Error(`Unexpected request ${path}`);
  });
});
afterEach(() => { cleanup(); vi.unstubAllGlobals(); fixture.subscriptions.length = 0; fixture.writes.length = 0; });
async function hydrate(sessionId: string | null) {
  await waitFor(() => expect(fixture.subscriptions.some(item => item.sessionId === sessionId && !item.signal.aborted)).toBe(true));
  const subscription = fixture.subscriptions.findLast(item => item.sessionId === sessionId && !item.signal.aborted)!;
  await act(async () => {
    await subscription.controller.acceptWithBackpressure({ cursor: null, item: { schema: "workspace.presentation.v1", workspaceId: "demo", sessionId, kind: "navigation", snapshotId: sessionId || "draft", offset: 0, hasMore: false, sessions } });
    if (sessionId) await subscription.controller.acceptWithBackpressure({ cursor: null, item: { schema: "workspace.presentation.v1", workspaceId: "demo", sessionId, kind: "snapshot", page: {
      schema: "transcript.page.v1", sessionId, projectionVersion: "transcript.projection.v1", projectionGeneration: "generation", sourceHighWater: "1", olderCursor: null, hasOlder: false, resumeCursors: [{ streamId: "workspace-transcript.v1", cursor: "1" }],
      blocks: [{ blockId: `${sessionId}-message`, blockRevision: "1", orderKey: { sourceSequence: "1", ordinal: 0 }, body: { kind: "userText", content: { inlineContent: `Content of ${sessionId}`, sourceRef: null } } }],
    } } });
  });
}
test("favorites open the selected chat content from work and another chat", async () => {
  window.history.replaceState(null, "", "/w/demo/work");
  const router = createRouter();
  const user = userEvent.setup();
  render(<FrontendProvider><RouterProvider router={router} /></FrontendProvider>);
  await hydrate(null);
  await user.click(await screen.findByRole("link", { name: "first favorite" }));
  await hydrate("first");
  expect(await screen.findByText("Content of first")).toBeTruthy();
  expect(router.state.location.pathname).toBe("/w/demo/work");
  await user.click(screen.getByRole("link", { name: "second favorite" }));
  await hydrate("second");
  expect(await screen.findByText("Content of second")).toBeTruthy();
  expect(screen.queryByText("Content of first")).toBeNull();
  expect(router.state.location.pathname).toBe("/w/demo/work");
  const first = screen.getByRole("link", { name: "first favorite" });
  expect(first.getAttribute("href")).toBe("/w/demo/sessions/first");
  let intercepted: boolean | undefined;
  const inspect = (event: MouseEvent) => { intercepted = event.defaultPrevented; event.preventDefault(); };
  document.addEventListener("click", inspect, { once: true });
  fireEvent.click(first, { ctrlKey: true });
  expect(intercepted).toBe(false);
  expect(screen.getByText("Content of second")).toBeTruthy();
  await user.click(screen.getByRole("button", { name: "Unfavorite chat" }));
  await waitFor(() => expect(screen.queryByRole("link", { name: "second favorite" })).toBeNull());
  expect(fixture.writes).toEqual([{ isPinned: false }]);
  router.dispose();
});

