// @vitest-environment jsdom
import { beforeEach, expect, test } from "vitest";
import { clearBrowserChatState, createChatDraft, readChatLayout, writeChatLayout } from "../../src/shell/workspaceChatState";

beforeEach(() => sessionStorage.clear());

test("layout and composer drafts survive recreation within their user and workspace", () => {
  const layout = { dockedSessionIds: ["one", "two"], activeSessionId: "two", floating: true, lastCatalog: "/w/work/work" };
  writeChatLayout("alice", "work", layout);
  expect(readChatLayout("alice", "work")).toEqual(layout);
  expect(readChatLayout("bob", "work").dockedSessionIds).toEqual([]);
  expect(readChatLayout("alice", "other").dockedSessionIds).toEqual([]);
  const first = createChatDraft("alice", "work", "one");
  first.setDraft("Keep my draft");
  expect(createChatDraft("alice", "work", "one").getSnapshot()).toBe("Keep my draft");
  expect(createChatDraft("alice", "work", "two").getSnapshot()).toBe("");
  expect(createChatDraft("bob", "work", "one").getSnapshot()).toBe("");
  expect(createChatDraft("alice", "other", "one").getSnapshot()).toBe("");
  first.setDraft("");
  expect(createChatDraft("alice", "work", "one").getSnapshot()).toBe("");
});

test("logout clears chat layout and drafts without clearing other browser preferences", () => {
  sessionStorage.setItem("unrelated", "keep");
  writeChatLayout("alice", "work", { dockedSessionIds: ["one"], activeSessionId: "one", floating: false, lastCatalog: "" });
  createChatDraft("alice", "work", "one").setDraft("Private draft");
  clearBrowserChatState();
  expect(readChatLayout("alice", "work").dockedSessionIds).toEqual([]);
  expect(createChatDraft("alice", "work", "one").getSnapshot()).toBe("");
  expect(sessionStorage.getItem("unrelated")).toBe("keep");
});
