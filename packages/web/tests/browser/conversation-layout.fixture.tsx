import { useRef, useState } from "react";
import { createRoot } from "react-dom/client";
import { flushSync } from "react-dom";
import { WorkspaceComposer } from "../../src/chat/WorkspaceComposer";
import { WorkspaceContextPanel } from "../../src/components/WorkspaceContextPanel";
import { i18n } from "../../src/i18n";
import { waitForLayout } from "./layoutGeometry";

// Production layout classes and components, isolated from backend and routing.
// This fixture never submits text or reads an application Session.
const requests: string[] = [];
const failures: string[] = [];
window.addEventListener("error", event => failures.push(event.message));
window.addEventListener("unhandledrejection", event => failures.push(String(event.reason)));
window.fetch = async input => { requests.push(String(input)); throw new Error("Synthetic layout must not request application data"); };
document.documentElement.dataset.theme = new URLSearchParams(location.search).get("theme") || "light";
await i18n.changeLanguage("en");
const noop = () => {};
function SessionLayout({ sidebar = true, preview = false, initialWidth = null }: { sidebar?: boolean; preview?: boolean; initialWidth?: number | null }) {
  const [open, setOpen] = useState(preview);
  const [width, setWidth] = useState<number | null>(initialWidth);
  const [draft, setDraft] = useState("Synthetic unsent text");
  const fileInputRef = useRef<HTMLInputElement>(null);
  return <main className={`workspaceWorkbench ${sidebar ? "withSidebar" : "withoutSidebar"} ${open ? "withContextPanel withFilePreview" : ""}`} style={{ "--browser-panel-width": width === null ? undefined : `${width}px` } as React.CSSProperties}>
    <div className="workspaceSidebarSlot">{sidebar ? <aside className="workspaceSidebar shSidebar">Synthetic navigation</aside> : null}</div>
    <header className="workspaceTopbar">Synthetic Session<button className="fixtureTogglePreview" type="button" onClick={() => setOpen(value => !value)}>Toggle synthetic preview</button></header>
    <section className="workspaceChatColumn"><div className="workspaceConversationPlane">
      <div className="workspaceMessages"><div className="workspaceTranscriptBlocks"><article className="workspaceTranscriptAssistant"><p>{"A complete synthetic reply.\n".repeat(60)}</p></article></div></div>
      <WorkspaceComposer fileInputRef={fileInputRef} isHome={false} onSubmit={(event: React.FormEvent) => event.preventDefault()} pendingAttachments={[]} pendingUploadFiles={[]} draft={draft} onDraftChange={setDraft} enterStartsNewLine={false} activeAgentName="Synthetic Agent" workspaceAvailable sending={false} loadingHistory={false} hasActiveAgentRun={false} uploadingAttachment={false} onUploadAttachment={noop} sessionId={null} currentModel={null} thinkingMode="" onThinkingModeChange={noop} modelGroups={[]} modelId="" onModelIdChange={noop} onCancelActiveAgentRun={noop} />
    </div></section>
    <WorkspaceContextPanel panel={open ? { mode: "filePreview", status: "ready", displayName: "Synthetic evidence.txt", originLabel: "Session", preview: { kind: "text", content: "Synthetic evidence.\n".repeat(100), contentType: "text/plain" } } : { mode: "closed" }} browserWidthPx={width} onBrowserWidthChange={setWidth} onClose={() => setOpen(false)} onReturn={() => setOpen(false)} />
  </main>;
}
const root = createRoot(document.getElementById("fixture")!);
const results: { name: string; actual: unknown; expected: unknown }[] = [];
const check = (name: string, actual: unknown, expected: unknown) => results.push({ name, actual, expected });
const settle = () => waitForLayout([".workspaceChatColumn", ".workspaceContextPanel", ".workspaceTranscriptBlocks", ".workspaceComposer"]);
const rect = (selector: string) => document.querySelector(selector)!.getBoundingClientRect();
const clearOf = (left: DOMRect, right: DOMRect) => left.right <= right.left || left.left >= right.right || left.bottom <= right.top || left.top >= right.bottom;
if (new URLSearchParams(location.search).has("review")) {
  flushSync(() => root.render(<SessionLayout sidebar={new URLSearchParams(location.search).get("sidebar") !== "hidden"} preview />));
  await settle();
  check("synthetic resize surface ready", true, true);
} else try {
  for (const sidebar of [true, false]) {
    const label = sidebar ? "visible navigation" : "hidden navigation";
    flushSync(() => root.render(<SessionLayout key={`${sidebar}-closed`} sidebar={sidebar} />)); await settle();
    const closedBody = rect(".workspaceTranscriptBlocks"); const closedComposer = rect(".workspaceComposer");
    flushSync(() => root.render(<SessionLayout key={`${sidebar}-open`} sidebar={sidebar} preview />)); await settle();
    const panel = rect(".workspaceContextPanel"); const body = rect(".workspaceTranscriptBlocks"); const composer = rect(".workspaceComposer");
    const chat = rect(".workspaceChatColumn"); const messages = rect(".workspaceMessages");
    const width = Number.parseFloat(getComputedStyle(document.documentElement).getPropertyValue("--workspace-sidebar-width")) || 270;
    check(`${label}: default preview matches navigation width`, Math.round(panel.width), width);
    check(`${label}: message and composer share their center`, Math.abs(body.left + body.width / 2 - composer.left - composer.width / 2) < 1, true);
    check(`${label}: message is centered in its available region`, Math.abs(body.left + body.width / 2 - chat.left - chat.width / 2) < 1, true);
    check(`${label}: composer is centered in its available region`, Math.abs(composer.left + composer.width / 2 - chat.left - chat.width / 2) < 1, true);
    check(`${label}: preview leaves the input unobscured`, clearOf(composer, panel), true);
    const visibleBody = new DOMRect(body.left, Math.max(body.top, messages.top), body.width, Math.max(0, Math.min(body.bottom, messages.bottom) - Math.max(body.top, messages.top)));
    check(`${label}: preview leaves visible conversation unobscured`, clearOf(visibleBody, panel), true);
    check(`${label}: no page-wide horizontal overflow`, document.documentElement.scrollWidth <= innerWidth + 1, true);
    const stacked = panel.bottom <= chat.top + 1;
    check(`${label}: desktop preview remains beside the conversation`, stacked, innerWidth < (sidebar ? 1170 : 900));
    if (stacked) {
      check(`${label}: stacked preview is at most half screen high`, panel.height <= innerHeight / 2 + 1, true);
      check(`${label}: stacking preserves message width`, Math.abs(body.width - closedBody.width) < 1, true);
      check(`${label}: stacking preserves composer width`, Math.abs(composer.width - closedComposer.width) < 1, true);
    } else {
      const left = sidebar && innerWidth > 760 ? rect(".workspaceSidebarSlot").right : 0;
      const availableCenter = (left + panel.left) / 2;
      check(`${label}: wide conversation centers between both sidebars`, Math.round(body.left + body.width / 2), Math.round(availableCenter));
      check(`${label}: wide composer centers between both sidebars`, Math.round(composer.left + composer.width / 2), Math.round(availableCenter));
      check(`${label}: wide preview uses available reading width up to its maximum`, Math.round(body.width), Math.min(820, Math.round(chat.width - 32)));
      check(`${label}: conversation keeps a usable minimum width`, chat.width >= 480, true);
    }
    const separator = document.querySelector<HTMLElement>('[role="separator"]')!;
    check(`${label}: resize reports the actual default width`, Number(separator.getAttribute("aria-valuenow")), panel.width);
    check(`${label}: resize minimum follows navigation width`, Number(separator.getAttribute("aria-valuemin")), width);
    if (innerWidth > 1400) {
      separator.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowLeft", bubbles: true, cancelable: true })); await settle();
      check(`${label}: an intentional keyboard resize adds sixteen pixels`, rect(".workspaceContextPanel").width, panel.width + 16);
      check(`${label}: resize reports the actual changed width`, Number(document.querySelector('[role="separator"]')!.getAttribute("aria-valuenow")), panel.width + 16);
    }
    (document.querySelector(".workspaceContextPanelClose") as HTMLButtonElement).click(); await settle();
    check(`${label}: closing releases preview content`, document.querySelector(".documentTextPreview"), null);
    check(`${label}: closing restores the conversation width`, Math.abs(rect(".workspaceTranscriptBlocks").width - closedBody.width) < 1, true);
    check(`${label}: opening and closing preserve the unsent draft`, document.querySelector<HTMLTextAreaElement>(".workspaceComposer textarea")!.value, "Synthetic unsent text");
    flushSync(() => root.render(<SessionLayout key={`${sidebar}-preferred`} sidebar={sidebar} preview initialWidth={760} />)); await settle();
    const preferredPanel = rect(".workspaceContextPanel");
    if (preferredPanel.right <= rect(".workspaceChatColumn").left || preferredPanel.left >= rect(".workspaceChatColumn").right) check(`${label}: saved width preserves a usable conversation region`, rect(".workspaceChatColumn").width >= 480, true);
    check(`${label}: saved width reports its clipped display geometry`, Number(document.querySelector('[role="separator"]')!.getAttribute("aria-valuenow")), preferredPanel.width);
    check(`${label}: saved width leaves the input unobscured`, clearOf(rect(".workspaceComposer"), preferredPanel), true);
    (document.querySelector(".workspaceContextPanelClose") as HTMLButtonElement).click(); await settle();
    (document.querySelector(".fixtureTogglePreview") as HTMLButtonElement).click(); await settle();
    check(`${label}: closing and reopening preserve chosen preview width`, rect(".workspaceContextPanel").width, preferredPanel.width);
    check(`${label}: closing and reopening preserve the draft`, document.querySelector<HTMLTextAreaElement>(".workspaceComposer textarea")!.value, "Synthetic unsent text");
  }
  check("layout performs no application requests", requests, []);
  check("layout has no asynchronous failures", failures, []);
  check("all conversation layout checks completed", true, true);
} catch (error) { check("layout fixture completed", error instanceof Error ? error.message : String(error), "success"); }
document.getElementById("results")!.textContent = JSON.stringify(results);
