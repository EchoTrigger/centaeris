// @vitest-environment jsdom
import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { useAgentPresentation } from "../../src/agent-chat/useAgentPresentation";
import { ShellPage } from "../../src/shell/ShellPage";
import { ApiError, apiJson } from "../../src/api";
import { streamWorkspacePresentation } from "../../src/chat/workspacePresentationTransport";

vi.mock("../../src/i18n", () => ({ t: (key: string) => key, useTranslation: () => ({ t: (key: string) => key }) }));
vi.mock("../../src/api", async original => ({ ...await original<object>(), apiJson: vi.fn() }));
vi.mock("../../src/chat/workspacePresentationTransport", () => ({ streamWorkspacePresentation: vi.fn() }));
vi.mock("react-router", async original => ({ ...await original<object>(), useLocation: () => ({ state: null }),
  useRouteLoaderData: () => ({ workspace: { id: "workspace" }, agents: [{ id: "agent", workspaceId: "workspace" }] }),
}));
vi.mock("../../src/shell/ShellSidebar", () => ({ ShellSidebar: ({ recentNavigation }: { recentNavigation?: { sessions: { id: string; title: string }[] } }) =>
  <nav aria-label="Recent conversations">{recentNavigation?.sessions.map(session => <span key={session.id}>{session.title}</span>)}</nav> }));
function View() {
  const presentation = useAgentPresentation("workspace", "agent", () => {});
  return <ShellPage activeAgent={{ id: "agent", workspaceId: "workspace" }} showRecentSessions presentationNavigation={presentation.navigation} onNavigationRetry={presentation.retry}><p>Agent</p></ShellPage>;
}
const settle = async () => { await act(async () => { await Promise.resolve(); }); };
beforeEach(() => {
  vi.stubGlobal("matchMedia", () => ({ matches: false }));
  vi.mocked(apiJson).mockResolvedValue({ projects: [] });
});
afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.unstubAllGlobals(); vi.clearAllMocks(); });

test("losing presentation authority clears already displayed sidebar titles", async () => {
  let subscription!: Parameters<typeof streamWorkspacePresentation>[0];
  let disconnect!: (error: unknown) => void;
  vi.mocked(streamWorkspacePresentation).mockImplementation(options => {
    subscription = options; options.onConnection("running");
    return new Promise((_resolve, reject) => { disconnect = reject; options.signal.addEventListener("abort", () => reject(new DOMException("Aborted", "AbortError")), { once: true }); });
  });
  render(<View />); await settle();
  await act(async () => { await subscription.controller.acceptWithBackpressure({ cursor: null, item: {
    schema: "workspace.presentation.v1", workspaceId: "workspace", agentId: "agent", sessionId: null,
    kind: "navigation", snapshotId: "navigation", offset: 0, hasMore: false, sessions: [{
      id: "session", currentEnvironmentId: null, executionMode: "perRun", workspaceId: "workspace", agentId: "agent", projectId: null,
      title: "Previously authorized conversation", origin: "user", status: "active", deletedAt: null, isPinned: false, isUnread: false,
      isManuallyUnread: false, latestAttentionSequence: "0", readThroughSequence: "0", attentionRevision: "0",
      hasActiveAgentRun: false, updatedAt: "2026-10-23T00:00:00Z", initialInputOrigin: null,
    }],
  } }); });
  expect(screen.getByText("Previously authorized conversation")).toBeTruthy();
  await act(async () => { disconnect(new ApiError("forbidden", 403)); }); await settle();
  expect(screen.queryByText("Previously authorized conversation")).toBe(null);
  expect(streamWorkspacePresentation).toHaveBeenCalledTimes(1);
});
