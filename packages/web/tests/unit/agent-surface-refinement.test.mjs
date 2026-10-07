import assert from "node:assert/strict";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { renderToStaticMarkup } from "react-dom/server";
import { environment, nodes, renderer, subjectLoader } from "./componentHarness.mjs";

const harness = new URL("./componentHarness.mjs", import.meta.url).href;
const dataUrl = value => `data:text/javascript;base64,${Buffer.from(value).toString("base64")}`;
const load = subjectLoader({ extraOverrides: [["react", dataUrl(`export * from ${JSON.stringify(import.meta.resolve("react"))}; export {useState,useRef,useMemo,useCallback,useEffect,useLayoutEffect} from ${JSON.stringify(harness)}; export const useId=()=>"preview";`)]] });
const source = name => fileURLToPath(new URL(`../../src/${name}`, import.meta.url));
const { AgentOverviewPanel } = await import(await load(source("agent-chat/AgentOverviewPanel.tsx")));
const { AgentSessionPreviewShell } = await import(await load(source("agent-chat/AgentSessionPreviewShell.tsx")));
const { ShellSidebar } = await import(await load(source("shell/ShellSidebar.jsx")));

test("background work refresh keeps the overview free of visible loading text", () => {
  const env = environment();
  const subject = renderer(AgentOverviewPanel, { agent: env.agents[0], sessions: [{ sessionId: "work", title: "Work", runState: "unknown" }], revision: "poll:1", pagination: { loading: true, hasMore: false, error: false }, onPreviewSession() {}, onPreviewFile() {}, onRunState() {} }, env);
  assert.equal(nodes(subject.render(), node => node.props.role === "status").length, 0);
  subject.unmount();
});

test("a selected Session shows one conversation without overview tabs or an Open Chat step", () => {
  const subject = renderer(AgentSessionPreviewShell, { embedded: true, session: { sessionId: "work", title: "Work", runState: "unknown" }, agentLabel: "Agent", labels: { preview: "Preview", path: "Path", close: "Close", openChat: "Open chat", activity: "Activity", outputs: "Outputs", loading: "Loading", retry: "Retry", returnToSession: "Return" }, load: { status: "ready" }, activeTab: "activity", activity: "Complete conversation", conversation: "Complete conversation", outputs: () => "Outputs", onPreviewLibraryObject() {}, onReturnToSession() {}, onTabChange() {}, onReturn() {}, onClose() {}, onOpenChat() {} }, environment());
  const tree = subject.render();
  assert.equal(nodes(tree, node => node.props.role === "tablist").length, 0);
  const markup = renderToStaticMarkup(tree);
  assert.ok(markup.includes("Complete conversation"));
  assert.equal(markup.includes("Open chat"), false);
  subject.unmount();
});

test("sidebar highlights the Agent only on its own page, and uses an ordinary navigation row", () => {
  for (const search of ["", "?sessionId=work", "?new=1"]) {
    const env = environment({ location: { pathname: "/w/workspace/agents/agent", search, state: null } });
    const subject = renderer(ShellSidebar, { workspace: env.workspace, agents: env.agents, activeAgent: env.agents[0], initialTab: "chat" }, env);
    const node = nodes(subject.render(), value => value.type.name === "ConversationTab")[0];
    const pane = renderer(node.type, node.props, env);
    const link = nodes(pane.render(), value => value.props.to === "/w/workspace/agents/agent")[0];
    assert.equal(link.props["aria-current"], search === "" ? "page" : undefined);
    assert.ok(link.props.className.split(" ").includes("shRow"));
    pane.unmount(); subject.unmount();
  }
});
