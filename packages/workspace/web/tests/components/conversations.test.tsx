// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { createMemoryRouter, Link, RouterProvider } from "react-router";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { ConversationHistory } from "../../src/shell/ConversationHistory";

beforeEach(() => { vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} }); });
afterEach(() => { cleanup(); vi.unstubAllGlobals(); });
test("history opens on demand and selects a conversation without replacing the work page", async () => {
  const user = userEvent.setup();
  const sessions = [{ id: "research", title: "调研记录", origin: "user", projectId: "", isPinned: false }, { id: "notes", title: "随手笔记", origin: "user", projectId: "", isPinned: true }];
  const onOpenSession = vi.fn();
  const onStartNewChat = vi.fn();
  const sessionProps = { sessions, projects: [], ready: true, onCreateProject: vi.fn(), renderSessionRow: (session: { id: string, title: string }) => <Link key={session.id} data-session-id={session.id} to={`/w/demo/sessions/${session.id}`}>{session.title}</Link> };
  const router = createMemoryRouter([{path:"/w/demo/work",element:<><h1>工作首页</h1><ConversationHistory sessionProps={sessionProps} onOpenSession={onOpenSession} onStartNewChat={onStartNewChat}>历史</ConversationHistory></>}],{initialEntries:["/w/demo/work"]});
  render(<RouterProvider router={router}/>);
  expect(screen.queryByText("调研记录")).toBeNull();
  await user.click(screen.getByRole("button",{name:"会话历史"}));
  expect(await screen.findByRole("link", { name: "随手笔记" })).toBeTruthy();
  expect(screen.queryByRole("textbox")).toBeNull();
  await user.click(screen.getByRole("link",{name:"调研记录"}));
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
