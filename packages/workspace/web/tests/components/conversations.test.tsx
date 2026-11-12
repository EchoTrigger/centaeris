// @vitest-environment jsdom
import { cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { createMemoryRouter, Link, RouterProvider } from "react-router";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { useState } from "react";
import { ConversationTabs } from "../../src/shell/ConversationTabs";
import { ConversationHistory } from "../../src/shell/ConversationHistory";

beforeEach(() => { vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} }); });
afterEach(() => { cleanup(); vi.unstubAllGlobals(); });
test("history opens on demand and selects a conversation without replacing the work page", async () => {
  const user = userEvent.setup();
  const sessions = [{ id: "research", title: "调研记录", origin: "user", projectId: "", isPinned: false }, { id: "notes", title: "随手笔记", origin: "user", projectId: "", isPinned: true }];
  const onOpenSession = vi.fn();
  const onStartNewChat = vi.fn();
  const sessionProps = { sessions, projects: [], ready: true, onCreateProject: vi.fn(), renderSessionRow: (session: { id: string, title: string }) => <Link key={session.id} data-session-id={session.id} to={`/w/demo/sessions/${session.id}`}>{session.title}</Link> };
  const router = createMemoryRouter([{path:"/w/demo/work",element:<><h1>工作首页</h1><ConversationHistory workspaceId="demo" currentSessionId="research" sessionProps={sessionProps} onOpenSession={onOpenSession} onStartNewChat={onStartNewChat}>历史</ConversationHistory></>}],{initialEntries:["/w/demo/work"]});
  render(<RouterProvider router={router}/>);
  expect(screen.queryByText("调研记录")).toBeNull();
  await user.click(screen.getByRole("button",{name:"会话历史"}));
  expect(await screen.findByRole("link", { name: "随手笔记" })).toBeTruthy();
  expect(screen.queryByRole("textbox")).toBeNull();
  expect(screen.getByRole("link", { name: /调研记录/ }).getAttribute("aria-current")).toBe("page");
  expect(screen.getByText("当前")).toBeTruthy();
  expect(within(screen.getByRole("navigation")).queryAllByRole("heading").map(heading => heading.textContent)).toEqual([]);
  await user.click(screen.getByRole("link",{name:/调研记录/}));
  expect(onOpenSession).toHaveBeenCalledWith(sessions[0]);
  expect(router.state.location.pathname).toBe("/w/demo/work");
  expect(screen.queryByRole("dialog")).toBeNull();
  await user.click(screen.getByRole("button",{name:"会话历史"}));
  await user.keyboard("{Escape}");
  expect(screen.queryByRole("dialog")).toBeNull();
  expect(document.activeElement).toBe(screen.getByRole("button",{name:"会话历史"}));
  await user.click(screen.getByRole("button",{name:"会话历史"}));
  await user.click(await screen.findByRole("button",{name:"新建会话"}));
  expect(onStartNewChat).toHaveBeenCalledOnce();
});


test("only the previous calendar week has a heading; recent and older chats stay outside it", async () => {
  const now = new Date(); const weekStart = new Date(now);
  weekStart.setHours(0, 0, 0, 0); weekStart.setDate(weekStart.getDate() - (weekStart.getDay() + 6) % 7);
  const previous = new Date(weekStart); previous.setDate(previous.getDate() - 1);
  const older = new Date(weekStart); older.setDate(older.getDate() - 8);
  const sessions = [{ id: "recent", title: "Recent note", updatedAt: now.toISOString() }, { id: "week", title: "Previous week note", updatedAt: previous.toISOString() }, { id: "old", title: "Older note", updatedAt: older.toISOString() }];
  const router = createMemoryRouter([{ path: "*", element: <ConversationHistory workspaceId="demo" sessionProps={{ sessions }} onStartNewChat={() => {}} onOpenSession={() => {}}>History</ConversationHistory> }]);
  render(<RouterProvider router={router} />);
  await userEvent.setup().click(screen.getByRole("button", { name: "会话历史" }));
  const lastWeek = screen.getByRole("region", { name: "上周" });
  expect(within(lastWeek).getAllByRole("link").map(link => link.textContent)).toEqual([expect.stringContaining("Previous week note")]);
  expect(within(lastWeek).queryByText("Recent note")).toBeNull();
  expect(within(lastWeek).queryByText("Older note")).toBeNull();
  expect(screen.getAllByRole("heading").map(heading => heading.textContent)).toEqual(["上周"]);
});


test("chat tabs select one window, minimize in place, and support keyboard selection", async () => {
  const sessions = [{ id: "first", title: "First chat" }, { id: "second", title: "Second chat" }];
  function Fixture() {
    const [activeId, setActiveId] = useState("first");
    const [expanded, setExpanded] = useState(false);
    return <><ConversationTabs sessions={sessions} activeId={activeId} expanded={expanded} panelId="chat-window" onSelect={session => { setActiveId(session.id); setExpanded(true); }} onMinimize={() => setExpanded(false)} />
      {expanded ? <section id="chat-window" role="tabpanel">{activeId}</section> : null}</>;
  }
  render(<Fixture />); const user = userEvent.setup();
  await user.click(screen.getByRole("tab", { name: "First chat" }));
  expect(screen.getByRole("tabpanel").textContent).toBe("first");
  await user.click(screen.getByRole("tab", { name: "Second chat" }));
  expect(screen.getAllByRole("tabpanel")).toHaveLength(1);
  expect(screen.getByRole("tabpanel").textContent).toBe("second");
  await user.click(screen.getByRole("tab", { name: "Second chat" }));
  expect(screen.queryByRole("tabpanel")).toBeNull();
  expect(screen.getAllByRole("tab")).toHaveLength(2);
  screen.getByRole("tab", { name: "First chat" }).focus();
  await user.keyboard("{ArrowRight}{Enter}");
  expect(screen.getByRole("tabpanel").textContent).toBe("second");
});
