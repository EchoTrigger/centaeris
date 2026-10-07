import { useState } from "react";
import { createRoot } from "react-dom/client";
import { flushSync } from "react-dom";
import { configureApi } from "../../src/api";
import { i18n } from "../../src/i18n";
import { AgentOverviewPanel } from "../../src/agent-chat/AgentOverviewPanel";
import { AgentFilePreview } from "../../src/agent-chat/AgentPreviewContent";
import { AgentPreviewPanel } from "../../src/agent-chat/AgentPreviewPanel";
import { AgentSessionPreviewShell } from "../../src/agent-chat/AgentSessionPreviewShell";
import { AgentMessageList } from "../../src/agent-chat/AgentMessageList";
import { TranscriptBlockRow } from "../../src/chat/TranscriptBlockContent";
import { createTranscriptViewStore } from "../../src/chat/transcriptViewStore";
import type { AgentSummary } from "../../src/router";
import type { ReferencedSession } from "../../src/agent-chat/agentOutputFeed";
import "../../src/agent-chat/agent-chat.css";

// Isolated production components with synthetic read-only preview responses.
// No user route, submission, Runtime, container or model participates.
await i18n.changeLanguage("en");
document.documentElement.dataset.theme = "light";
configureApi({ apiBaseUrl: location.origin });
document.head.append(document.querySelector<HTMLLinkElement>('link[href="/src/globals.css"]')!);
const requests: string[] = [];
window.fetch = async (input, init) => {
  const url = new URL(String(input), location.href);
  if ((init?.method ?? "GET") !== "GET") throw new Error("Synthetic preview reads only");
  if (url.pathname === "/api/sessions/synthetic-0/outputs/synthetic-file/preview") {
    requests.push(url.pathname);
    return new Response("%PDF-1.4\n%%EOF", { headers: { "Content-Type": "application/pdf" } });
  }
  if (!/^\/api\/sessions\/synthetic-\d+\/preview$/.test(url.pathname)) throw new Error(`Unexpected synthetic request: ${url.pathname}`);
  requests.push(url.pathname);
  return Response.json({ schema: "session.preview.v1", sessionId: url.pathname.split("/")[3], runFact: null, outputs: [], nextAfterArtifactId: null, hasMore: false });
};
const agent = { id: "synthetic-agent", name: "Synthetic Agent", avatarKind: "centaeris" } as AgentSummary;
const sessions: ReferencedSession[] = Array.from({ length: 40 }, (_, i) => ({ sessionId: `synthetic-${i}`, title: `Work ${i}: a complete independently opened Session`, agentId: agent.id, workspaceId: "synthetic-workspace", runState: "unknown" }));
const noop = () => {};
const digest = `sha256:${"a".repeat(64)}`;
const file = { inputRef: null, agentRunId: "synthetic-run", ownerKind: "artifact" as const, objectRef: "synthetic-file", sourceVersion: "1", sha256: digest, displayName: "Synthetic file.pdf", contentType: "application/pdf", sizeBytes: 14,
  previewUrl: `/api/sessions/synthetic-0/outputs/synthetic-file/preview?sourceVersion=1&sha256=${digest}`, downloadUrl: `/api/sessions/synthetic-0/outputs/synthetic-file/download?sourceVersion=1&sha256=${digest}` };
const origin = { messageId: "synthetic-initial-work-input", agentId: "source-agent", agentName: "Synthetic Source Agent" };
const store = createTranscriptViewStore();
store.openTail({ schema: "transcript.page.v1", sessionId: "synthetic-work-session", projectionVersion: "transcript.projection.v1", projectionGeneration: "generation", sourceHighWater: "2", blocks: [
  { blockId: origin.messageId, blockRevision: "1", orderKey: { sourceSequence: "1", ordinal: 0 }, body: { kind: "userText", content: { inlineContent: "A work objective", sourceRef: null } } },
  { blockId: "synthetic-human-follow-up", blockRevision: "1", orderKey: { sourceSequence: "2", ordinal: 0 }, body: { kind: "userText", content: { inlineContent: "A manual follow-up", sourceRef: null } } },
], olderCursor: null, hasOlder: false, resumeCursors: [{ streamId: "workspace-transcript.v1", cursor: "2" }] });
const labels = { preview: "Session preview", path: "Preview path", close: "Close preview", openChat: "Open chat", activity: "Activity", outputs: "Outputs", loading: "Loading", retry: "Retry", returnToSession: "Return to session" };
const longMessage = "Long conversation message for comparing available reading width. ".repeat(30);
function Surface({ count = 0, mode = "overview" }: { count?: number; mode?: "overview" | "activity" | "file" }) {
  const [open, setOpen] = useState(true);
  return <main className="shMain agentChatMain">
    <header className="agentChatRouteHeader"><span>Synthetic Agent</span><button type="button" className="agentChatPanelToggle" onClick={() => setOpen(v => !v)}>List</button></header>
    <div className="agentChatRoutePlane">
      <div className="agentChatConversationColumn"><section className="agentChatRouteReplies"><div className="agentChatContentWidth">
        <AgentMessageList messages={[
          { role: "user", messageId: "long-input", authorLabel: "You", text: longMessage, isLatest: false },
          { role: "agent", messageId: "reply-a", authorLabel: "Agent", text: longMessage, isLatest: false, sessions: [sessions[0]] },
          { role: "agent", messageId: "reply-b", authorLabel: "Agent", text: "Reply B", isLatest: false, sessions: [sessions[1]] },
          { role: "agent", messageId: "reply-c", authorLabel: "Agent", text: "Reply C deliberately refers again to A", isLatest: true, sessions: [sessions[0]] },
        ]} referenceTime="2026-10-05T00:00:00Z" readLabel="Read" onPreviewSession={noop} />
        <div className="workspaceTranscriptBlocks"><TranscriptBlockRow store={store} blockId={origin.messageId} initialInputOrigin={origin} /><TranscriptBlockRow store={store} blockId="synthetic-human-follow-up" initialInputOrigin={origin} /></div>
      </div></section><div className="agentChatComposerDock"><form className="workspaceComposer agentInputComposer"><textarea aria-label="Synthetic draft" defaultValue="Preserved draft" /></form></div></div>
      {open ? <div className="agentChatPreviewDock"><AgentPreviewPanel label="Agent preview" focusKey={mode} onClose={() => setOpen(false)}>
        {mode === "overview" ? <AgentOverviewPanel agent={agent} sessions={sessions.slice(0, count)} onPreviewSession={noop} onPreviewFile={noop} onRunState={noop} />
          : <AgentSessionPreviewShell embedded session={sessions[0]} agentLabel={agent.name} labels={labels} load={{ status: "ready" }}
            conversation={<div className="agentChatRouteActivity"><div className="workspaceAgentRunList" style={{ overflow: "auto" }}>{Array.from({ length: 40 }, (_, i) => <p key={i}>Actual transcript reading space {i}</p>)}</div><button className="workspaceJumpToLatest" type="button" aria-label="Return to latest">↓</button></div>}
            libraryPreview={mode === "file" ? { title: file.displayName, content: <AgentFilePreview file={file} /> } : undefined}
            onReturnToSession={noop} onReturn={noop} onClose={() => setOpen(false)} />}
      </AgentPreviewPanel></div> : null}
    </div>
    <div className="sessionWidthReference" aria-hidden="true" style={{ position: "absolute", insetInline: 0, insetBlockStart: 0, visibility: "hidden", pointerEvents: "none" }}>
      <div className="workspaceMessages"><div className="workspaceTranscriptBlocks"><div className="workspaceUserMessageStack"><div className="workspaceUserMessage">{longMessage}</div></div></div></div>
      <div className="workspaceComposer" />
    </div>
  </main>;
}
const root = createRoot(document.getElementById("fixture")!);
const results: { name: string; actual: unknown; expected: unknown }[] = [];
const check = (name: string, actual: unknown, expected: unknown) => results.push({ name, actual, expected });
const settle = async () => { await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))); await document.fonts.ready; };
const bounds = (selector: string) => document.querySelector(selector)!.getBoundingClientRect();
flushSync(() => root.render(<Surface />)); await settle();
const emptyHeight = bounds(".agentChatSessionPreview").height;
check("empty overview fits its actual header and content", emptyHeight < 200 && emptyHeight < bounds(".agentChatComposerDock").top - bounds(".agentChatSessionPreview").top, true);
check("preview is flat without its former shadow", getComputedStyle(document.querySelector(".agentChatSessionPreview")!).boxShadow, "none");
check("preview keeps the established rounded frame", getComputedStyle(document.querySelector(".agentChatSessionPreview")!).borderRadius, "16px");
check("preview uses a readable floating width", Math.round(bounds(".agentChatPreviewDock").width), Math.min(360, innerWidth - 32));
const contentBeforeClose = bounds(".agentChatContentWidth");
const composerBeforeClose = bounds(".agentChatComposerDock");
check("Agent reading column matches ordinary Session width", contentBeforeClose.width, bounds(".sessionWidthReference .workspaceTranscriptBlocks").width);
check("Agent reading column matches ordinary Session left edge", contentBeforeClose.left, bounds(".sessionWidthReference .workspaceTranscriptBlocks").left);
check("Agent composer matches ordinary Session width", bounds(".agentInputComposer").width, bounds(".sessionWidthReference .workspaceComposer").width);
check("Agent user bubble matches ordinary Session width and proportion", bounds(".agentChatMessageGroup--user").width, bounds(".sessionWidthReference .workspaceUserMessageStack").width);
check("long Agent reply leaves room in the full reading column", bounds(".agentChatMessageGroup--agent").width, Math.min(760, contentBeforeClose.width));
check("preview never reserves reading-column space", getComputedStyle(document.querySelector(".agentChatConversationColumn")!).marginRight, "0px");
flushSync(() => root.render(<Surface count={1} />)); await settle();
check("one work grows the overview from its empty height", bounds(".agentChatSessionPreview").height > emptyHeight + 20, true);
const overviewRow = document.querySelector(".agentChatOverviewActivity > .agentChatSessionReference")!.getBoundingClientRect();
const overviewSection = bounds(".agentChatOverviewActivity");
check("overview Session title fills its content width", overviewRow.width, overviewSection.width);
check("overview Session title aligns to content left edge", overviewRow.left, overviewSection.left);
check("overview Session title aligns to content right edge", overviewRow.right, overviewSection.right);
flushSync(() => root.render(<Surface count={40} />)); await settle();
const showMore = [...document.querySelectorAll<HTMLButtonElement>(".agentChatPreviewDock button")].find(b => b.textContent === "Show more")!;
showMore.click(); await settle();
const panel = bounds(".agentChatSessionPreview"); const body = document.querySelector<HTMLElement>(".agentChatPreviewBody")!;
check("many works cap the overview above the composer", panel.bottom <= bounds(".agentChatComposerDock").top, true);
check("many works scroll their content instead of growing the panel", body.scrollHeight > body.clientHeight, true);
for (const mode of ["activity", "file"] as const) {
  flushSync(() => root.render(<Surface mode={mode} />)); await settle();
  check(`${mode} stays within its available viewport`, bounds(".agentChatSessionPreview").bottom <= bounds(".agentChatComposerDock").top, true);
  const close = bounds(".agentChatPreviewClose");
  check(`${mode} leaves its close control visible`, close.top >= bounds(".agentChatSessionPreview").top && close.bottom <= bounds(".agentChatSessionPreview").bottom, true);
  if (mode === "activity") check("selected activity keeps useful reading space", bounds(".agentChatRouteActivity").height >= Math.min(100, innerHeight / 10), true);
  if (mode === "activity") {
    const jump = document.querySelector<HTMLElement>(".workspaceJumpToLatest")!;
    check("preview Return to latest keeps shared circular shape", getComputedStyle(jump).borderRadius, "50%");
    check("preview Return to latest keeps shared 32px square dimensions", [jump.getBoundingClientRect().width, jump.getBoundingClientRect().height], [32, 32]);
  }
  if (mode === "file") {
    check("selected file keeps useful reading space", bounds("iframe").height >= Math.min(100, innerHeight / 10), true);
    check("selected file stays visible inside the capped panel", bounds("iframe").bottom <= bounds(".agentChatSessionPreview").bottom, true);
  }
  const composer = bounds(".agentChatComposerDock"); const preview = bounds(".agentChatPreviewDock");
  check(`${mode} leaves the composer clear`, preview.right <= composer.left || preview.left >= composer.right || preview.bottom <= composer.top || preview.top >= composer.bottom, true);
}
const groups = [...document.querySelectorAll<HTMLElement>(".agentChatMessageGroup--agent")];
const sender = document.querySelector<HTMLElement>(".workspaceUserMessageOrigin")!;
const initialBubble = sender.parentElement!.querySelector(".workspaceUserMessage")!;
check("the source Agent labels only its authoritative initial input", document.querySelectorAll(".workspaceUserMessageOrigin").length, 1);
check("the source label keeps the exact requested text", sender.textContent, "Send By Synthetic Source Agent");
check("the source label stays outside its input bubble", Boolean(sender.closest(".workspaceUserMessage")), false);
check("the source label sits above the input bubble", sender.getBoundingClientRect().bottom <= initialBubble.getBoundingClientRect().top, true);
check("the source label shares the input bubble's right edge", Math.abs(sender.getBoundingClientRect().right - initialBubble.getBoundingClientRect().right) < 1, true);
check("each reply keeps only its own Session attachment", groups.map(g => g.querySelector(".agentChatSessionReference")!.textContent), [sessions[0].title, sessions[1].title, sessions[0].title]);
for (const group of groups) {
  const article = group.querySelector("article")!; const refs = group.querySelector(".agentChatSessionReferences")!;
  check("references stay outside their own reply bubble", Boolean(refs.closest("article")), false);
  check("references sit below their own reply bubble", refs.getBoundingClientRect().top >= article.getBoundingClientRect().bottom, true);
  check("references align with their reply's left edge", Math.abs(refs.getBoundingClientRect().left - article.getBoundingClientRect().left) < 1, true);
}
document.querySelector<HTMLButtonElement>(".agentChatPreviewClose")!.click(); await settle();
check("closing releases the panel", document.querySelector(".agentChatPreviewDock"), null);
check("closing releases the reserved reading margin", getComputedStyle(document.querySelector(".agentChatConversationColumn")!).marginRight, "0px");
check("closing leaves message position unchanged", bounds(".agentChatContentWidth").left, contentBeforeClose.left);
check("closing leaves message width unchanged", bounds(".agentChatContentWidth").width, contentBeforeClose.width);
check("closing leaves composer position unchanged", bounds(".agentChatComposerDock").top, composerBeforeClose.top);
const title = document.querySelector<HTMLElement>(".agentChatSessionTitle")!;
const reference = title.closest("button")!;
check("Session titles stay on one line", getComputedStyle(title).whiteSpace, "nowrap");
check("Session titles fade instead of using ellipsis", getComputedStyle(title).maskImage.includes("linear-gradient") && getComputedStyle(title).textOverflow === "clip", true);
check("Session attachment width is half the longest bubble", Math.abs(reference.getBoundingClientRect().width - Math.min(760, contentBeforeClose.width) / 2) < 1, true);
check("conversation scrollbar is hidden without disabling scrolling", getComputedStyle(document.querySelector(".agentChatRouteReplies")!).scrollbarWidth, "none");
check("Agent header matches ordinary Session height", bounds(".agentChatRouteHeader").height, 40);
check("closing preserves the draft", document.querySelector<HTMLTextAreaElement>("textarea")!.value, "Preserved draft");
check("no horizontal document overflow", document.documentElement.scrollWidth <= innerWidth + 1, true);
check("preview requests remain scoped synthetic reads", requests.every(path => path.endsWith("/preview")), true);
check("all panel sizing checks completed", true, true);
document.getElementById("results")!.textContent = JSON.stringify(results);
