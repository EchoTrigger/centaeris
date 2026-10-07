import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { createMemoryRouter, RouterProvider, useLocation, useParams } from "react-router";
import { configureApi } from "../../src/api";
import { i18n, t } from "../../src/i18n";
import { AgentChatPageContent } from "../../src/routes/AgentChatRoute";
import { agentChatPath, workspaceChatKind } from "../../src/agent-chat/agentChatNavigation";
import "../../src/agent-chat/agent-chat.css";
import { waitForLayout } from "./layoutGeometry";

// The production bundle loads shared Workspace CSS after the Agent rules.
// Reorder the existing sheet so the route also covers that cascade boundary.
document.head.append(document.querySelector<HTMLLinkElement>('link[rel="stylesheet"][href="/src/globals.css"]')!);

// Synthetic API data only. This entry exercises the production readonly route,
// never a backend, user-input submission, main-loop uptake or Library resolver.
const nextHistoryCursor = "fixture-ignored-source/all+opaque=components";
const workspace = { id: "fixture-workspace", name: "Synthetic workspace", role: "owner" };
const agents = ["main", "worker", "empty", "invalid", "long"].map(id => ({
  id: `fixture-${id}`, workspaceId: workspace.id, name: `Synthetic ${id}`, description: "", instructions: "",
  avatarKind: "centaeris", status: "active", deletedAt: null, createdAt: "2026-09-30T14:00:00Z", updatedAt: "2026-09-30T14:00:00Z",
}));
const user = { id: "fixture-user", email: "fixture@example.invalid", isStaff: false, isSuperuser: false };
const requests: { path: string; method: string }[] = [];
const delayedHistory = { initial: true, older: false, newer: false, newerCount: 0, failOlder: false, olderReference: false, releaseInitial: null as (() => void) | null, releaseOlder: null as (() => void) | null, releaseNewer: null as (() => void) | null, releaseOlderReference: null as (() => void) | null };
const output = (id: string, sourceSequence: number, body: string, sessionRefs: string[] = []) => ({
  id, agentRunId: "fixture-run", turnId: "fixture-turn", toolCallId: `fixture-call-${id}`,
  sourceSequence, createdAtMs: Date.parse("2026-09-30T14:30:00Z") + sourceSequence * 60000,
  body, sessionRefs, fileRefs: [],
});
configureApi({ apiBaseUrl: location.origin } as Parameters<typeof configureApi>[0]);
window.fetch = async (input, init) => {
  const url = new URL(String(input), location.origin);
  const method = init?.method || "GET";
  requests.push({ path: `${url.pathname}${url.search}`, method });
  if (method !== "GET") throw new Error("Synthetic readonly Agent route must not mutate application data");
  const json = (body: unknown, status = 200) => Promise.resolve(new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } }));
  if (url.pathname === "/api/library") return json({ objects: [] });
  if (/^\/api\/sessions\/fixture-work-session\/outputs\/fixture-report\/(preview|download)$/.test(url.pathname)) return new Response("Synthetic output only.\n".repeat(64), { headers: { "Content-Type": "text/plain" } });
  if (url.pathname === `/api/workspaces/${workspace.id}/sessions`) return json({ sessions: [{ id: "fixture-work-session", workspaceId: workspace.id, agentId: "fixture-worker", title: "Synthetic work session", origin: "user", isPinned: false, projectId: null, status: "active", updatedAt: "2026-09-30T14:35:00Z" }] });
  if (url.pathname === `/api/workspaces/${workspace.id}/session-projects`) return json({ projects: [] });
  const model = url.pathname.match(/^\/api\/agents\/(fixture-[^/]+)\/model-settings$/);
  if (model) return json({ schema: "agent.model_settings.v1", agentId: model[1], modelConfigRef: null, thinkingMode: null, status: "unconfigured" });
  const binding = url.pathname.match(/^\/api\/agents\/(fixture-[^/]+)\/coordination-session$/);
  if (binding) return binding[1] === "fixture-empty" ? json({ error: "coordination_session_not_found" }, 404) : json({ schema: "agent.coordination_session.v1", agentId: binding[1], sessionId: `fixture-coordination-${binding[1]}` });
  const workSessions = url.pathname.match(/^\/api\/agents\/(fixture-[^/]+)\/work-sessions$/);
  if (workSessions) return json({ schema: "agent.work_sessions.v1", agentId: workSessions[1], workspaceId: workspace.id,
    sessions: workSessions[1] === "fixture-main" ? [{ id: "fixture-work-session", workspaceId: workspace.id, agentId: "fixture-worker", projectId: null, title: "Synthetic work session", origin: "automation", status: "active", deletedAt: null, isPinned: false, isUnread: false, hasActiveAgentRun: true, updatedAt: "2026-09-30T14:35:00Z", initialInputOrigin: { messageId: "fixture-initial-work", agentId: "fixture-main", agentName: "Synthetic main" } }] : [],
    nextAfterSessionId: null, hasMore: false });
  const inputPage = url.pathname.match(/^\/api\/agents\/(fixture-[^/]+)\/inputs$/);
  if (inputPage) return json({ schema: "agent.inputs.v1", agentId: inputPage[1], sessionId: `fixture-coordination-${inputPage[1]}`, inputs: [{ inputId: "fixture-input", sequence: 80, createdAtMs: Date.parse("2026-09-30T14:35:00Z"), body: "Synthetic exact input", attachments: [], read: null }], nextAfterSequence: null });
  const preview = url.pathname.match(/^\/api\/sessions\/([^/]+)\/preview$/);
  if (preview) {
    const sha256 = `sha256:${"a".repeat(64)}`;
    const review = new URLSearchParams(location.search).has("review");
    const after = url.searchParams.get("afterArtifactId");
    const workSession = preview[1] === "fixture-work-session";
    const indices = !workSession ? [] : review ? [0] : after ? [50] : Array.from({ length: 50 }, (_, index) => index);
    const outputs = indices.map(index => {
      const objectRef = index === 0 ? "fixture-report" : `fixture-report-${index}`;
      const outputPrefix = `/api/sessions/${preview[1]}/outputs/${objectRef}`;
      return { inputRef: null, agentRunId: "fixture-active-run", ownerKind: "artifact", objectRef, sourceVersion: "1", sha256, displayName: index === 0 ? "Synthetic report.txt" : `Synthetic output ${index}.txt`, contentType: "text/plain", sizeBytes: 14, ...Object.fromEntries(["preview", "download"].map(action => [`${action}Url`, `${outputPrefix}/${action}?${new URLSearchParams({ sourceVersion: "1", sha256, lang: "zh-CN" })}`])) };
    });
    const hasMore = workSession && !review && after === null;
    return json({ schema: "session.preview.v1", sessionId: preview[1], runFact: { agentRunId: "fixture-active-run", status: "running", sourceType: "hostedRun", eventId: null, createdAtMs: Date.parse("2026-09-30T14:35:00Z") }, outputs, nextAfterArtifactId: hasMore ? outputs.at(-1)!.objectRef : null, hasMore });
  }
  const match = url.pathname.match(/^\/api\/agents\/(fixture-[^/]+)\/history$/);
  if (match) {
    const agentId = match[1];
    if (agentId === "fixture-empty") return json({ error: "coordination_session_not_found" }, 404);
    if (agentId === "fixture-invalid") return json({ schema: "unknown" });
    const after = url.searchParams.get("afterCursor");
    const before = url.searchParams.get("beforeCursor");
    const main = agentId === "fixture-main";
    const review = new URLSearchParams(location.search).has("review");
    if (!review && before !== null && delayedHistory.failOlder) { delayedHistory.failOlder = false; return json({ error: "temporarily_unavailable" }, 503); }
    if (agentId === "fixture-long") {
      const start = before === null ? 51 : 1;
      const items = after === null ? Array.from({ length: 50 }, (_, index) => {
        const number = start + index;
        return { kind: "message", cursor: `fixture-long-position-${number}`, message: output(`fixture-long-output-${number}`, number, `Historical reply ${number}.\n\n` + "Long history reading paragraph.\n\n".repeat(number % 5 + 1)) };
      }) : [];
      const page = { schema: "agent.history.v1", agentId, sessionId: `fixture-coordination-${agentId}`, items, nextCursor: after ?? items[0].cursor, newestCursor: after ?? items.at(-1)!.cursor, hasMore: after === null && before === null };
      if (before !== null && delayedHistory.older) return new Promise<Response>(resolve => {
        delayedHistory.releaseOlder = () => { delayedHistory.older = false; delayedHistory.releaseOlder = null; void json(page).then(resolve); };
      });
      return json(page);
    }
    const longReply = "A complete committed reply." + (review ? "" : "\n\nA line in the complete reply.".repeat(36));
    const page = { schema: "agent.history.v1", agentId, sessionId: `fixture-coordination-${agentId}`,
      items: main ? after !== null ? [] : before === null ? [
        { kind: "input", cursor: "fixture-input-position", input: { inputId: "fixture-input", sequence: 80, createdAtMs: Date.parse("2026-09-30T14:35:00Z"), body: "Synthetic exact input", attachments: [], read: null } },
        { kind: "message", cursor: "fixture-output-position", message: output("fixture-output-1", 3, longReply, ["fixture-work-session"]) },
      ] : before === nextHistoryCursor ? [{ kind: "message", cursor: "fixture-older-output-position", message: output("fixture-output-older", 1, "An earlier independent whole reply." + (review ? "" : "\n\nEarlier reply line.".repeat(15)), ["fixture-older-work-session"]) }] : []
        : after !== null ? [] : [{ kind: "message", cursor: "fixture-worker-output-position", message: output("fixture-worker-output", 4, "A reply owned by the other Agent.") }],
      nextCursor: main ? after !== null ? "fixture-newest-tail-position" : before === null ? nextHistoryCursor : "fixture-older-output-position" : "fixture-worker-output-position",
      newestCursor: main ? after !== null ? "fixture-newest-tail-position" : before === null ? "fixture-output-position" : "fixture-older-output-position" : "fixture-worker-output-position",
      hasMore: main && after === null && before === null };
    if (main && !review && after === null && before === null && delayedHistory.initial) return new Promise<Response>(resolve => {
      const releasePrevious = delayedHistory.releaseInitial;
      delayedHistory.releaseInitial = () => {
        releasePrevious?.();
        delayedHistory.initial = false;
        delayedHistory.releaseInitial = null;
        void json(page).then(resolve);
      };
    });
    if (main && !review && before !== null && delayedHistory.older) return new Promise<Response>(resolve => {
      delayedHistory.releaseOlder = () => { delayedHistory.older = false; delayedHistory.releaseOlder = null; void json(page).then(resolve); };
    });
    if (main && !review && after !== null && delayedHistory.newer) return new Promise<Response>(resolve => {
      delayedHistory.releaseNewer = () => {
        delayedHistory.newer = false; delayedHistory.releaseNewer = null;
        const arrival = ++delayedHistory.newerCount;
        const cursor = arrival === 1 ? "fixture-arrival-position" : "fixture-following-position";
        void json({ ...page, items: [{ kind: "message", cursor, message: output(`fixture-output-arrival-${arrival}`, 4 + arrival, "A new reply arrived while reading.\n\n" + "New reply line.\n\n".repeat(12)) }], nextCursor: cursor, newestCursor: cursor, hasMore: true }).then(resolve);
      };
    });
    return json(page);
  }
  if (url.pathname === "/api/sessions/fixture-work-session") return json({ session: {
    id: "fixture-work-session", workspaceId: workspace.id, agentId: "fixture-worker", title: "Synthetic work session", origin: "user", isPinned: false, projectId: null, status: "active", hasActiveAgentRun: true,
  } });
  if (url.pathname === "/api/sessions/fixture-older-work-session") {
    const metadata = { session: { id: "fixture-older-work-session", workspaceId: workspace.id, agentId: "fixture-worker", title: "Synthetic older work session", origin: "user", isPinned: false, projectId: null, status: "active", hasActiveAgentRun: false } };
    if (delayedHistory.olderReference) return new Promise<Response>(resolve => {
      delayedHistory.releaseOlderReference = () => { delayedHistory.olderReference = false; delayedHistory.releaseOlderReference = null; void json(metadata).then(resolve); };
    });
    return json(metadata);
  }
  if (url.pathname === "/api/sessions/fixture-work-session/transcript") return json({
    schema: "transcript.page.v1", sessionId: "fixture-work-session", projectionVersion: "transcript.projection.v1",
    projectionGeneration: "fixture-generation", sourceHighWater: "0", blocks: [], olderCursor: null, hasOlder: false,
    resumeCursors: [],
  });
  if (url.pathname === "/api/sessions/fixture-work-session/transcript/active-agent-run") return json({
    schema: "workspace.transcript.active_agent_run.v1", sessionId: "fixture-work-session",
    agentRun: { agentRunId: "fixture-active-run", status: "running", streamCursor: "fixture-stream-cursor" },
  });
  throw new Error(`Unexpected synthetic route request: ${url.pathname}`);
};

function Entry() {
  const { agentId = "" } = useParams();
  const location = useLocation();
  return workspaceChatKind(agentId, location.search) === "agent"
    ? <AgentChatPageContent key={agentId} agentId={agentId} />
    : <p role="note" aria-label="Ordinary Session sentinel">Existing ordinary Session route selected.</p>;
}
const router = createMemoryRouter([{ id: "authenticated", loader: () => ({ user }), children: [{
  id: "workspace", path: "w/:workspaceId", loader: () => ({ workspace, workspaces: [workspace], agents }), children: [
    { path: "agents/:agentId", Component: Entry },
    { path: "app", element: <p role="note" aria-label="New Session sentinel">Existing workspace new-chat route selected.</p> },
  ],
}] }], { initialEntries: [agentChatPath(workspace.id, "fixture-main")] });
await i18n.changeLanguage("en");
document.documentElement.dataset.theme = new URLSearchParams(location.search).get("theme") || "light";
createRoot(document.getElementById("fixture")!).render(<StrictMode><RouterProvider router={router} /></StrictMode>);
const results: { name: string; actual: unknown; expected: unknown }[] = [];
const check = (name: string, actual: unknown, expected: unknown) => results.push({ name, actual, expected });
const pause = () => new Promise(resolve => setTimeout(resolve, 30));
async function waitFor(read: () => boolean) {
  for (let attempt = 0; attempt < 250; attempt++) { if (read()) return; await pause(); }
  throw new Error("Synthetic route did not reach the expected state");
}
const button = (label: string) => [...document.querySelectorAll<HTMLButtonElement>("button")].find(item => item.textContent?.trim() === label || item.getAttribute("aria-label") === label)!;
if (new URLSearchParams(location.search).has("review")) {
  await waitFor(() => Boolean(button("Synthetic work session")) && Boolean(document.querySelector("textarea")));
  button(t("agentChat.toggleOverview")).click();
} else try {
  await waitFor(() => Boolean(document.querySelector(".agentChatRouteReplies")) && delayedHistory.releaseInitial !== null);
  check("initial history request has no loading status message", document.querySelector(".agentChatContentWidth > [role=status]"), null);
  delayedHistory.releaseInitial!();
  await waitFor(() => Boolean(button("Synthetic work session")));
  const composer = document.querySelector<HTMLFormElement>(".agentInputComposer")!;
  const input = composer.querySelector("textarea")!;
  await waitFor(() => composer.querySelector("[role=status]")?.textContent === t("agentChat.modelRequired"));
  check("unconfigured model explains why sending is unavailable", composer.querySelector("[role=status]")?.textContent, t("agentChat.modelRequired"));
  check("input describes its model availability state", input.getAttribute("aria-describedby"), composer.querySelector("[role=status]")?.id);
  const reference = document.createElement("form");
  reference.className = "workspaceComposer";
  reference.innerHTML = '<textarea rows="2"></textarea><div class="workspaceComposerFooter"><button class="workspaceSendButton" type="button">↑</button></div>';
  document.body.append(reference);
  composer.style.transition = "none";
  reference.style.transition = "none";
  for (const [label, actual, expected, properties] of [
    ["container", composer, reference, ["padding", "gap", "borderRadius", "backgroundColor", "boxShadow"]],
    ["text input", input, reference.querySelector("textarea")!, ["minHeight", "padding", "lineHeight", "fontSize", "resize"]],
    ["send button", composer.querySelector('button[type="submit"]')!, reference.querySelector("button")!, ["width", "height", "padding", "borderRadius", "backgroundColor", "color"]],
  ] as const) {
    for (const property of properties) check(`Agent ${label} matches Session ${property}`, getComputedStyle(actual)[property], getComputedStyle(expected)[property]);
  }
  const footer = composer.querySelector<HTMLElement>(".workspaceComposerFooter")!;
  const availability = footer.querySelector<HTMLElement>("[role=status]")!;
  const sendButton = footer.querySelector<HTMLButtonElement>('button[type="submit"]')!;
  check("Agent composer has no additional block-start margin", getComputedStyle(composer).marginBlockStart, "0px");
  check("Agent composer has no additional block-end margin", getComputedStyle(composer).marginBlockEnd, "0px");
  check("Agent footer keeps its own control alignment", getComputedStyle(footer).justifyContent, "flex-end");
  check("Agent footer keeps a twelve-pixel status-to-send gap", Math.abs(sendButton.getBoundingClientRect().left - availability.getBoundingClientRect().right - 12) < 1, true);
  check("Agent send control stays aligned to the footer's right edge", Math.abs(sendButton.getBoundingClientRect().right - footer.getBoundingClientRect().right) < 1, true);
  const composerDock = composer.parentElement!;
  check("Agent composer uses its dock spacing without an extra bottom margin", Math.abs(composer.getBoundingClientRect().bottom + parseFloat(getComputedStyle(composerDock).paddingBottom) - composerDock.getBoundingClientRect().bottom) < 1, true);
  input.focus();
  check("Agent text focus has no nested outline", getComputedStyle(input).outlineStyle, "none");
  const sessionInput = reference.querySelector("textarea")!;
  sessionInput.focus();
  const sessionFocusShadow = getComputedStyle(reference).boxShadow;
  input.focus();
  check("Agent focus uses Session container emphasis", getComputedStyle(composer).boxShadow, sessionFocusShadow);
  check("Agent input starts at Session height", input.offsetHeight, sessionInput.offsetHeight);
  input.value = "A long message\n".repeat(40);
  check("long Agent input scrolls inside its text area", input.scrollHeight > input.clientHeight, true);
  input.value = "";
  input.blur();
  reference.remove();
  composer.style.removeProperty("transition");
  const exactDraft = "  unfinished\n中文 🐈 e\u0301  ";
  const typeMessage = (value: string) => {
    const field = document.querySelector<HTMLTextAreaElement>(".agentInputComposer textarea")!;
    Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")!.set!.call(field, value);
    field.dispatchEvent(new Event("input", { bubbles: true }));
  };
  typeMessage(exactDraft);
  await waitFor(() => JSON.parse(sessionStorage.getItem(`centaeris.agentInputDraft.v1:${JSON.stringify([user.id, workspace.id, "fixture-main"])}`) || "null")?.body === exactDraft);
  check("direct Agent route displays server-ordered input and committed output", document.querySelectorAll("article").length, 2);
  const workReference = button("Synthetic work session");
  const replyBubble = document.querySelector("article.agentChatMessage--agent")!;
  check("work Session attachment is outside the reply bubble", workReference.closest("article"), null);
  check("work Session attachment sits below the reply border", workReference.getBoundingClientRect().top > replyBubble.getBoundingClientRect().bottom, true);
  check("null Read has no receipt and composer has no model configuration link", Boolean(document.querySelector("article time")) || document.querySelectorAll(".agentInputComposer a").length !== 0, false);
  check("work Session is a compact rectangle", workReference.offsetWidth > workReference.offsetHeight && workReference.offsetHeight <= 40, true);
  check("work Session shows only its name", workReference.querySelector("svg"), null);
  check("server order is preserved despite timestamp and sequence differences", document.querySelector("article")?.textContent?.includes("Synthetic exact input"), true);
  const newChat = button(t("shellSidebar.newConversation"));
  check("top Session entry is named New Chat", newChat.textContent?.trim(), "New Chat");
  const sessionRow = document.querySelector<HTMLElement>(".shSessionSection .workspaceSessionButton")!;
  check("Session navigation uses a borderless row without link underline", getComputedStyle(sessionRow).textDecorationLine, "none");
  check("Session navigation shares the thirty-pixel row height", sessionRow.getBoundingClientRect().height, 30);
  check("sidebar has no duplicate bottom create button", document.querySelector(".shComposeChat"), null);
  const mainLink = [...document.querySelectorAll<HTMLAnchorElement>(".shAgentStrip a")].find(link => link.textContent === "Synthetic main")!;
  check("new chat precedes Agent avatar entries", Boolean(newChat.compareDocumentPosition(mainLink) & Node.DOCUMENT_POSITION_FOLLOWING), true);
  check("avatar link is direct Agent conversation", mainLink.getAttribute("href"), agentChatPath(workspace.id, "fixture-main"));
  check("queued-or-active metadata does not manufacture running Loader", Boolean(document.querySelector(".agentChatMessageGroup .lucide-loader")), false);
  const overviewToggle = button(t("agentChat.toggleOverview"));
  overviewToggle.focus(); overviewToggle.click();
  await waitFor(() => Boolean(document.querySelector(".agentChatOverviewHeader")) && Boolean(button("Synthetic report.txt")));
  check("Overview has no duplicate close icon", document.querySelector(".agentChatPreviewDock .agentChatPreviewClose"), null);
  document.querySelector(".agentChatSessionPreview")!.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }));
  await waitFor(() => !document.querySelector(".agentChatPreviewDock"));
  check("Escape closes Overview through the shared frame", overviewToggle.getAttribute("aria-expanded"), "false");
  overviewToggle.click();
  await waitFor(() => Boolean(document.querySelector(".agentChatOverviewHeader")));
  overviewToggle.click();
  await waitFor(() => !document.querySelector(".agentChatPreviewDock"));
  check("List toggle closes Overview", overviewToggle.getAttribute("aria-expanded"), "false");
  const outputRequests = () => requests.filter(item => new URL(item.path, location.origin).pathname === "/api/sessions/fixture-work-session/preview").length;
  const outputRequestsBeforePreview = outputRequests();
  const contentBeforePreview = document.querySelector(".agentChatContentWidth")!.getBoundingClientRect();
  const composerBeforePreview = composer.getBoundingClientRect();
  check("closed preview leaves message and composer widths aligned", Math.abs(contentBeforePreview.width - composerBeforePreview.width) < 1 ? true : {content:contentBeforePreview.width,composer:composerBeforePreview.width}, true);
  check("closed preview leaves message and composer left edges aligned", Math.abs(contentBeforePreview.left - composerBeforePreview.left) < 1 ? true : {content:contentBeforePreview.left,composer:composerBeforePreview.left}, true);
  button("Synthetic work session").focus(); button("Synthetic work session").click();
  await waitFor(() => Boolean(document.querySelector('aside[aria-label="Session preview"]')));
  await waitForLayout([".shMain", ".agentChatRoutePlane", ".agentChatConversationColumn", ".agentChatPreviewDock", ".agentChatContentWidth", ".agentInputComposer"]);
  check("selected conversation does not fetch an unrelated Outputs tab", outputRequests(), outputRequestsBeforePreview);
  check("session reference previews without navigating", router.state.location.pathname, agentChatPath(workspace.id, "fixture-main"));
  const previewBounds = document.querySelector(".agentChatPreviewDock")!.getBoundingClientRect();
  const planeBounds = document.querySelector(".agentChatRoutePlane")!.getBoundingClientRect();
  const contentWithPreview = document.querySelector(".agentChatContentWidth")!.getBoundingClientRect();
  const composerWithPreview = composer.getBoundingClientRect();
  check("floating conversation is capped above the actual composer", previewBounds.bottom <= composerWithPreview.top + 1, true);
  check("floating conversation uses a readable width", Math.abs(previewBounds.width - Math.min(360, planeBounds.width - 32)) < 1, true);
  check("opening preview preserves message width", contentWithPreview.width, contentBeforePreview.width);
  check("opening preview preserves message position", contentWithPreview.left, contentBeforePreview.left);
  check("opening preview preserves composer width", composerWithPreview.width, composerBeforePreview.width);
  check("opening preview preserves composer position", composerWithPreview.left, composerBeforePreview.left);
  check("preview leaves the input clear", previewBounds.bottom <= composerWithPreview.top, true);
  if (innerWidth > 760) {
    button(t("shellSidebar.hideSidebar")).click();
    await waitForLayout([".shMain", ".agentChatRoutePlane", ".agentChatConversationColumn", ".agentChatPreviewDock", ".agentChatContentWidth", ".agentInputComposer"]);
    const plane = document.querySelector(".agentChatRoutePlane")!.getBoundingClientRect();
    const panel = document.querySelector(".agentChatPreviewDock")!.getBoundingClientRect();
    const content = document.querySelector(".agentChatContentWidth")!.getBoundingClientRect();
    const editor = composer.getBoundingClientRect();
    check("hidden navigation: preview remains floating", Math.abs(panel.width - Math.min(360, plane.width - 32)) < 1, true);
    check("hidden navigation: messages stay centered in the full plane", Math.abs(content.left + content.width / 2 - plane.left - plane.width / 2) < 1, true);
    check("hidden navigation: input stays centered in the full plane", Math.abs(editor.left + editor.width / 2 - plane.left - plane.width / 2) < 1, true);
    check("hidden navigation: input remains clear", panel.bottom <= editor.top, true);
    check("hidden navigation: changing layout retains draft", input.value, exactDraft);
    button(t("appRoute.showSidebar")).click();
    await waitForLayout([".shMain", ".agentChatRoutePlane", ".agentChatConversationColumn", ".agentChatPreviewDock", ".agentChatContentWidth", ".agentInputComposer"]);
  }
  check("preview uses one inner content scroller", getComputedStyle(document.querySelector(".agentChatSessionPreview")!).overflowY, "hidden");
  input.focus();
  check("input stays focusable while preview is open", document.activeElement === input && !input.closest("[inert]"), true);
  check("preview keeps its complete Session title", document.querySelector(".agentChatPreviewTitle")?.textContent, "Synthetic work session");
  check("Session preview has no overview tabs", document.querySelector(".agentChatSessionPreview [role=tablist]"), null);
  check("Session preview has no Open Chat step", button(t("agentChat.openChat")), undefined);
  check("reference stays name-only", document.querySelector(".agentChatMessageGroup .agentChatSessionReference svg"), null);
  button("Synthetic main").click();
  await waitFor(() => Boolean(button("Synthetic report.txt")));
  check("overview owns the activity and outputs groups", [...document.querySelectorAll(".agentChatOverviewSectionTitle")].map(n => n.textContent), [t("agentChat.activity"), t("agentChat.outputs")]);
  check("overview fetches output metadata", outputRequests() > outputRequestsBeforePreview, true);
  button(t("agentChat.showMore")).click();
  await waitFor(() => Boolean(button("Synthetic output 50.txt")));
  const outputsBody = document.querySelector<HTMLElement>(".agentChatPreviewBody")!;
  check("opening next output page retains all fifty-one files", outputsBody.querySelectorAll(".agentFileReference").length, 51);
  button("Synthetic report.txt").focus(); button("Synthetic report.txt").click();
  await waitFor(() => document.querySelector(".agentFilePreview .documentTextPreview")?.textContent?.includes("Synthetic output only.") === true);
  const fileFrame = document.querySelector<HTMLElement>(".agentFilePreview")!;
  const textPreview = fileFrame.querySelector<HTMLElement>(".documentTextPreview")!;
  const filePanel = document.querySelector<HTMLElement>(".agentChatSessionPreview")!;
  check("file preview fits its floating frame", filePanel.scrollHeight <= filePanel.clientHeight + 1, true);
  check("file content scrolls inside its own viewport", textPreview.scrollHeight > textPreview.clientHeight, true);
  check("file frame has no oversized minimum height", getComputedStyle(fileFrame).minHeight, "0px");
  check("file preview does not add a second outer scroller", getComputedStyle(document.querySelector(".agentChatLibraryPreview")!).overflowY, "hidden");
  button(t("agentChat.returnToSession")).click();
  await waitFor(() => !document.querySelector(".agentFilePreview") && Boolean(document.querySelector(".agentChatConversationPreview")));
  button(t("agentChat.close")).click();
  await waitFor(() => !document.querySelector(".agentChatPreviewDock"));
  check("preview never changes current route", router.state.location.pathname, agentChatPath(workspace.id, "fixture-main"));
  check("preview keeps the exact Agent draft", document.querySelector<HTMLTextAreaElement>(".agentInputComposer textarea")?.value, exactDraft);
  const replies = document.querySelector<HTMLElement>(".agentChatRouteReplies")!;
  const latestPageBottomGap = Math.abs(replies.scrollHeight - replies.clientHeight - replies.scrollTop);
  check("latest page starts at the conversation bottom", latestPageBottomGap <= 2 ? 0 : Math.round(latestPageBottomGap), 0);
  check("history has no manual load-more button", button(t("agentChat.loadMore")), undefined);
  const visibleReadingAnchor = () => {
    const bounds = replies.getBoundingClientRect();
    return [...document.querySelectorAll<HTMLElement>("article.agentChatMessage--agent p")].find(element => {
      const rect = element.getBoundingClientRect();
      return element.textContent === "A line in the complete reply." && rect.top >= bounds.top && rect.bottom <= bounds.bottom;
    })!;
  };
  delayedHistory.failOlder = true;
  replies.scrollTop = 40;
  replies.dispatchEvent(new Event("scroll"));
  await waitFor(() => Boolean(document.querySelector(".agentChatContentWidth > [role=alert]")));
  check("an older-page failure retains the already loaded conversation", document.querySelectorAll("article").length, 2);
  delayedHistory.older = true;
  const requestsBeforeRetry = requests.length;
  button(t("agentChat.retry")).click();
  await waitFor(() => delayedHistory.releaseOlder !== null);
  check("seamless older history request has no loading status message", document.querySelector(".agentChatContentWidth > [role=status]"), null);
  const retryHistoryRequest = requests.slice(requestsBeforeRetry).find(item => new URL(item.path, location.origin).pathname.endsWith("/history"))!;
  check("retry repeats the exact older-page cursor", new URL(retryHistoryRequest.path, location.origin).searchParams.get("beforeCursor"), nextHistoryCursor);
  check("retry does not turn an older request into a latest-page replacement", new URL(retryHistoryRequest.path, location.origin).searchParams.get("afterCursor"), null);
  replies.scrollTop = 200;
  const olderReadingAnchor = visibleReadingAnchor();
  const olderReadingTop = olderReadingAnchor.getBoundingClientRect().top;
  delayedHistory.olderReference = true;
  delayedHistory.releaseOlder!();
  await waitFor(() => document.querySelectorAll("article").length === 3 && delayedHistory.releaseOlderReference !== null);
  check("older page prepends without reordering loaded messages", [...document.querySelectorAll("article")].map(item => item.textContent?.includes("earlier")), [true, false, false]);
  check("scrolling towards top sends the complete opaque older cursor", requests.some(item => new URL(item.path, location.origin).searchParams.get("beforeCursor") === nextHistoryCursor), true);
  check("prepending older page preserves the position selected during its request", Math.abs(olderReadingAnchor.getBoundingClientRect().top - olderReadingTop) < 1, true);
  replies.scrollTop += 80;
  replies.dispatchEvent(new Event("scroll"));
  const referenceReadingAnchor = visibleReadingAnchor();
  const referenceReadingTop = referenceReadingAnchor.getBoundingClientRect().top;
  const referenceReadingScroll = replies.scrollTop;
  const referenceMessageHeight = document.querySelector(".agentChatMessageList")!.getBoundingClientRect().height;
  delayedHistory.releaseOlderReference!();
  await waitFor(() => Boolean(button("Synthetic older work session")));
  const referenceReadingShift = referenceReadingAnchor.getBoundingClientRect().top - referenceReadingTop;
  check("hydrating an older Session reference preserves the reading position selected during metadata loading", Math.abs(referenceReadingShift) < 1 ? 0 : {
    shift: referenceReadingShift, beforeTop: referenceReadingTop, afterTop: referenceReadingAnchor.getBoundingClientRect().top,
    beforeScroll: referenceReadingScroll, afterScroll: replies.scrollTop,
    beforeMessageHeight: referenceMessageHeight, afterMessageHeight: document.querySelector(".agentChatMessageList")!.getBoundingClientRect().height,
  }, 0);
  delayedHistory.newer = true;
  replies.scrollTop = replies.scrollHeight - replies.clientHeight;
  await waitFor(() => delayedHistory.releaseNewer !== null);
  check("background tail history request has no loading status message", document.querySelector(".agentChatContentWidth > [role=status]"), null);
  replies.scrollTop = Math.max(200, replies.scrollTop - 320);
  const newerReadingAnchor = visibleReadingAnchor();
  const newerReadingTop = newerReadingAnchor.getBoundingClientRect().top;
  const newerReadingScroll = replies.scrollTop;
  delayedHistory.releaseNewer!();
  await waitFor(() => document.querySelectorAll("article").length === 4);
  check("newer page does not force a reader back to the bottom", replies.scrollTop, newerReadingScroll);
  check("newer page preserves the content selected during its request", Math.abs(newerReadingAnchor.getBoundingClientRect().top - newerReadingTop) < 1, true);
  check("polling uses newest cursor independently of older pagination", requests.some(item => new URL(item.path, location.origin).searchParams.get("afterCursor") === "fixture-output-position"), true);
  await waitFor(() => requests.some(item => new URL(item.path, location.origin).searchParams.get("afterCursor") === "fixture-arrival-position"));
  check("empty tail refresh preserves loaded rows", document.querySelectorAll("article").length, 4);
  delayedHistory.newer = true;
  replies.scrollTop = replies.scrollHeight - replies.clientHeight;
  await waitFor(() => delayedHistory.releaseNewer !== null);
  delayedHistory.releaseNewer!();
  await waitFor(() => document.querySelectorAll("article").length === 5);
  const followingBottomGap = Math.abs(replies.scrollHeight - replies.clientHeight - replies.scrollTop);
  check("a reader staying at the bottom follows newly committed replies", followingBottomGap <= 2 ? 0 : Math.round(followingBottomGap), 0);
  await router.navigate(agentChatPath(workspace.id, "fixture-long"));
  await waitFor(() => document.querySelectorAll("article").length === 50);
  const longReplies = document.querySelector<HTMLElement>(".agentChatRouteReplies")!;
  check("a full latest page starts at its actual bottom", Math.abs(longReplies.scrollHeight - longReplies.clientHeight - longReplies.scrollTop) <= 2, true);
  delayedHistory.older = true;
  longReplies.scrollTop = 40;
  longReplies.dispatchEvent(new Event("scroll"));
  await waitFor(() => delayedHistory.releaseOlder !== null);
  longReplies.scrollTop = 200;
  const longBounds = longReplies.getBoundingClientRect();
  const longReadingAnchor = [...document.querySelectorAll<HTMLElement>("article.agentChatMessage--agent p")].find(element => {
    const rect = element.getBoundingClientRect();
    return rect.top >= longBounds.top && rect.bottom <= longBounds.bottom;
  })!;
  const longReadingTop = longReadingAnchor.getBoundingClientRect().top;
  delayedHistory.releaseOlder!();
  await waitFor(() => document.querySelectorAll("article").length === 100);
  check("a long conversation keeps chronological order across pages", [...document.querySelectorAll("article")].map(item => item.querySelector("p")?.textContent), Array.from({ length: 100 }, (_, index) => `Historical reply ${index + 1}.`));
  const longReadingShift = Math.abs(longReadingAnchor.getBoundingClientRect().top - longReadingTop);
  check("a full earlier page preserves the reading position selected during its request", longReadingShift < 1 ? 0 : Math.round(longReadingShift), 0);
  longReplies.scrollTop = longReplies.scrollHeight - longReplies.clientHeight;
  await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)));
  check("long history still exposes its latest committed reply at the bottom", document.querySelectorAll("article").item(99).textContent?.includes("Historical reply 100."), true);
  const latestLongReply = document.querySelectorAll("article").item(99).getBoundingClientRect();
  check("long history keeps the latest reply inside its visible viewport", latestLongReply.top >= longReplies.getBoundingClientRect().top && latestLongReply.bottom <= longReplies.getBoundingClientRect().bottom, true);
  check("returning to the bottom keeps the long conversation scroll position", Math.abs(longReplies.scrollHeight - longReplies.clientHeight - longReplies.scrollTop) <= 2, true);
  await router.navigate(agentChatPath(workspace.id, "fixture-worker"));
  await waitFor(() => document.querySelector("article")?.textContent?.includes("owned by the other Agent") === true);
  check("switching Agent does not retain previous Agent outputs", document.querySelectorAll("article").length, 1);
  check("switching Agent does not inherit another Agent draft", document.querySelector<HTMLTextAreaElement>(".agentInputComposer textarea")?.value, "");
  typeMessage("worker draft");
  await router.navigate(agentChatPath(workspace.id, "fixture-empty"));
  await waitFor(() => document.querySelectorAll("article").length === 0 && document.querySelector(".agentInputComposer [role=status]")?.textContent === t("agentChat.modelRequired"));
  check("a bot with no history has its own empty draft", document.querySelector<HTMLTextAreaElement>(".agentInputComposer textarea")?.value, "");
  check("empty history stays quiet", document.body.textContent!.includes(t("agentChat.noHistory")) || Boolean(button(t("agentChat.refresh"))), false);
  check("missing coordination does not silently create a Session", requests.every(item => item.method === "GET"), true);
  await router.navigate(agentChatPath(workspace.id, "fixture-invalid"));
  await waitFor(() => document.body.textContent!.includes(t("agentChat.loadError")));
  check("unknown output schema shows error rather than production facts", document.querySelectorAll("article").length, 0);
  await router.navigate(agentChatPath(workspace.id, "fixture-main"));
  await waitFor(() => Boolean(button("Synthetic work session")));
  check("switching back to main Agent restores its own draft", document.querySelector<HTMLTextAreaElement>(".agentInputComposer textarea")?.value, exactDraft);
  button(t("shellSidebar.newConversation")).click();
  await waitFor(() => Boolean(document.querySelector('[aria-label="Ordinary Session sentinel"]')));
  check("top New Chat enters current bot Session draft", router.state.location.pathname + router.state.location.search, `/w/${workspace.id}/agents/fixture-main?new=1`);
  check("New Chat keeps conversation sidebar selected", router.state.location.state?.sidebarTab, "chat");
  check("all Agent route checks completed", true, true);
} catch (error) { check("route fixture completed", error instanceof Error ? error.message : String(error), "success"); }
finally { document.getElementById("results")!.textContent = JSON.stringify(results); }
