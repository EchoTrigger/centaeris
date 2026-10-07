import { StrictMode, useState, type ReactNode } from "react";
import { createRoot } from "react-dom/client";
import { flushSync } from "react-dom";
import { AgentMessageBubble } from "../../src/agent-chat/AgentMessageBubble";
import { AgentMessageList } from "../../src/agent-chat/AgentMessageList";
import { AgentSessionPreviewShell } from "../../src/agent-chat/AgentSessionPreviewShell";
import type { AgentMessageView, AgentSessionReferenceView, AgentPreviewLoadView } from "../../src/agent-chat/agentChatViewTypes";
import { AttachmentCard } from "../../src/chat/AttachmentCard";
import { i18n } from "../../src/i18n";
import "../../src/agent-chat/agent-chat.css";

declare global {
  interface Window {
    agentChatFixtureRequests: string[];
    agentChatFixtureNavigations: string[];
  }
}

// Every identity and fact below is synthetic, fixed, and confined to this test entry.
const receivedAt = "2026-09-30T14:32:00Z";
const agentCreatedAt = "2026-09-30T14:33:00Z";
const referenceTime = "2026-09-30T18:00:00Z";
const running: AgentSessionReferenceView = Object.freeze({ sessionId: "fixture-session-running", title: "Check the cited sources", runState: "running" });
const completed: AgentSessionReferenceView = Object.freeze({ sessionId: "fixture-session-completed", title: "Prepare the summary", runState: "completed" });
const longSession: AgentSessionReferenceView = Object.freeze({ sessionId: "fixture-session-long", title: "A very long synthetic session title that remains understandable when the preview is narrow ".repeat(3), runState: "unknown" });
const sessions = [running, completed, longSession] as const;
const libraryReference = Object.freeze({ ownerKind: "userLibraryObject", ownerId: "fixture-library-object", generation: 7, sha256: `sha256:${"a".repeat(64)}` });
const libraryAttachment = Object.freeze({ displayName: "Synthetic sources.pdf", contentType: "application/pdf" });
const labels = Object.freeze({ preview: "Session preview", path: "Preview path", close: "Close preview", openChat: "Open chat", activity: "Activity", outputs: "Outputs", loading: "Loading preview", retry: "Retry", returnToSession: "Return to session preview" });
const calls: { action: string; value?: unknown }[] = [];
const record = (action: string, value?: unknown) => calls.push({ action, value });
type Scenario = "pending" | "read" | "late" | "new-input";
const user = (overrides: Partial<Extract<AgentMessageView, { role: "user" }>> = {}): Extract<AgentMessageView, { role: "user" }> => ({
  role: "user", messageId: "fixture-user-latest", authorLabel: "Latest user", text: "Check the sources and prepare a short summary.", isLatest: true, createdAt: "2026-09-30T14:31:00Z", ...overrides,
});
const agent = (overrides: Partial<Extract<AgentMessageView, { role: "agent" }>> = {}): Extract<AgentMessageView, { role: "agent" }> => ({
  role: "agent", messageId: "fixture-agent-latest", authorLabel: "Latest Agent", text: "The **summary** is ready. I am checking the source details.\n\nThis whole reply is a single published message.", isLatest: true, createdAt: agentCreatedAt, sessions: [running, completed], ...overrides,
});
const inputFact = (messageId = "fixture-user-latest", timestamp = receivedAt) => Object.freeze({ kind: "loopInput" as const, messageId, receivedAt: timestamp });

function FixtureHarness({ scenario = "read", load = { status: "ready" } }: { scenario?: Scenario; load?: AgentPreviewLoadView }) {
  const [selection, setSelection] = useState<AgentSessionReferenceView | null>(null);
  const [library, setLibrary] = useState(false);
  const preview = (sessionId: string) => {
    record("preview", sessionId);
    setSelection(sessions.find(item => item.sessionId === sessionId)!);
    setLibrary(false);
  };
  const close = () => { record("close"); setSelection(null); setLibrary(false); };
  const latest = user({ loopInputFact: scenario === "read" ? inputFact() : scenario === "late" ? inputFact("fixture-user-old") : undefined });
  const messages: readonly AgentMessageView[] = [
    user({ messageId: "fixture-user-old", authorLabel: "Older user", text: "An earlier question.", isLatest: false, createdAt: "2026-09-30T14:10:00Z", loopInputFact: inputFact("fixture-user-old") }),
    agent({ messageId: "fixture-agent-old", authorLabel: "Older Agent", text: "An earlier complete reply.", isLatest: false, createdAt: "2026-09-30T14:11:00Z", sessions: [] }),
    scenario === "new-input" ? user({ ...latest, isLatest: false, loopInputFact: inputFact() }) : latest,
    ...(scenario === "new-input" ? [user({ messageId: "fixture-user-new", authorLabel: "New user", text: "Please also compare the revisions.", createdAt: "2026-09-30T14:32:30Z" })] : []),
    agent(),
  ];
  return <>
    <p className="agentChatFixtureNote" role="note">Synthetic presentation scenarios. No backend, navigation, or real Read facts.</p>
    <main className="agentChatFixtureWorkbench agentChatRoutePlane">
      <div className="agentChatConversationColumn">
      <section className="agentChatRouteReplies" aria-label="Synthetic Agent conversation"><div className="agentChatContentWidth">
        <AgentMessageList messages={messages} referenceTime={referenceTime} readLabel="Read" onPreviewSession={preview} />
      </div></section>
      <div className="agentChatComposerDock"><form className="workspaceComposer agentInputComposer"><textarea rows={2} aria-label="Synthetic message input" name="message" autoComplete="off" placeholder="Message…" /></form></div>
      </div>
      {selection ? <div className="agentChatPreviewDock"><AgentSessionPreviewShell
        session={selection} agentLabel="Research Agent" labels={labels} load={load}
        conversation={<div><p>Synthetic complete conversation: checking the source details.</p><AttachmentCard attachment={libraryAttachment} onPreview={() => { record("library", libraryReference); setLibrary(true); }} /></div>}
        libraryPreview={library ? { title: libraryAttachment.displayName, content: <p>Synthetic Library preview slot. The host supplies its existing Library renderer.</p> } : undefined}
        onReturnToSession={() => { record("return-session"); setLibrary(false); }}
        onReturn={() => { record("return-agent"); setSelection(null); }}
        onClose={close}
        onRetry={() => record("retry", selection.sessionId)}
      /></div> : null}
    </main>
    <p className="agentChatFixtureStatus" role="status">{""}</p>
  </>;
}

function Review() {
  const [scenario, setScenario] = useState<Scenario>("read");
  return <><div className="agentChatFixtureControls"><label> Synthetic input scenario <select value={scenario} onChange={event => setScenario(event.target.value as Scenario)}>
    <option value="read">Matching synthetic loop input</option><option value="pending">No loop input</option><option value="late">Receipt for an older input</option><option value="new-input">Newer input without receipt</option>
  </select></label></div><FixtureHarness key={scenario} scenario={scenario} /></>;
}

const host = document.getElementById("fixture")!;
const root = createRoot(host);
const results: { name: string; actual: unknown; expected: unknown }[] = [];
function check(name: string, actual: unknown, expected: unknown) { results.push({ name, actual: Array.isArray(actual) ? [...actual] : actual, expected }); }
const render = (node: ReactNode) => flushSync(() => root.render(<StrictMode>{node}</StrictMode>));
const settle = async () => {
  await new Promise(resolve => setTimeout(resolve, 60));
  await new Promise<void>(resolve => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));
};
function message(label: string) {
  const element = host.querySelector<HTMLElement>(`article[aria-label="${label}"]`);
  if (!element) throw new Error(`Missing message: ${label}`);
  return element;
}
function button(label: string, scope: ParentNode = host) {
  const element = [...scope.querySelectorAll<HTMLButtonElement>("button")].find(item => item.getClientRects().length && (item.getAttribute("aria-label") === label || item.textContent?.trim() === label));
  if (!element) throw new Error(`Missing visible button: ${label}`);
  return element;
}
function panel() {
  const element = host.querySelector<HTMLElement>('[aria-label="Session preview"]');
  if (!element) throw new Error("Missing session preview");
  return element;
}
function key(element: HTMLElement, value: string, shiftKey = false) {
  const event = new KeyboardEvent("keydown", { key: value, shiftKey, bubbles: true, cancelable: true });
  element.dispatchEvent(event);
  return event;
}
function checkNoRead(label: string) {
  check(`${label}: no Read`, message(label).parentElement!.textContent!.includes("Read"), false);
  check(`${label}: no time`, message(label).parentElement!.querySelector("time"), null);
}
function reviewMessage(value: AgentMessageView) {
  render(<AgentMessageBubble message={value} readLabel="Read" locale="en-GB" timeZone="UTC" onPreviewSession={id => record("preview", id)} />);
}
const separatorTimes = () => [...host.querySelectorAll<HTMLTimeElement>("time")].filter(time => !time.closest("article"));
function reviewTimeline(messages: readonly AgentMessageView[], locale?: string, timeZone?: string, clock = referenceTime) {
  render(<AgentMessageList messages={messages} referenceTime={clock} readLabel="Read" locale={locale} timeZone={timeZone} onPreviewSession={id => record("preview", id)} />);
}
function timelineMessage(id: string, createdAt?: string) {
  return agent({ messageId: id, authorLabel: id, text: id, isLatest: false, createdAt, sessions: [] });
}

const query = new URLSearchParams(location.search);
await i18n.changeLanguage("en");
document.documentElement.dataset.theme = query.get("theme") === "dark" ? "dark" : "light";
const hrefBefore = location.href;
const storageBefore = JSON.stringify({ local: { ...localStorage }, session: { ...sessionStorage } });
if (query.has("review")) {
  render(<Review />);
} else try {
  reviewMessage(user()); checkNoRead("Latest user");
  reviewMessage(user({ loopInputFact: inputFact("fixture-user-old") })); checkNoRead("Latest user");
  reviewMessage(user({ loopInputFact: inputFact("fixture-user-latest", "invalid-time") })); checkNoRead("Latest user");
  reviewMessage(user({ loopInputFact: inputFact("fixture-user-latest", "") })); checkNoRead("Latest user");
  reviewMessage(user({ loopInputFact: inputFact("fixture-user-latest", "2026-02-30T14:32:00Z") })); checkNoRead("Latest user");
  reviewMessage(user({ loopInputFact: inputFact("fixture-user-latest", "2026-09-30T14:32:00") })); checkNoRead("Latest user");
  // A transport adapter cannot pass admission/accepted/handled as a loop-input fact.
  for (const kind of ["admission", "accepted", "handled"]) {
    reviewMessage(user({ loopInputFact: { ...inputFact(), kind } as unknown as Extract<AgentMessageView, { role: "user" }>["loopInputFact"] }));
    checkNoRead("Latest user");
  }
  reviewMessage(user({ loopInputFact: inputFact() }));
  check("matching latest input displays Read", message("Latest user").parentElement!.textContent!.includes("Read"), true);
  check("Read displays supplied input time", message("Latest user").parentElement!.querySelector("time")?.textContent, "14:32");
  check("Read retains supplied instant", message("Latest user").parentElement!.querySelector("time")?.dateTime, receivedAt);
  check("time has complete accessible date", Boolean(message("Latest user").parentElement!.querySelector("time")?.getAttribute("aria-label")?.includes("2026")), true);
  reviewMessage(user({ isLatest: false, loopInputFact: inputFact() })); checkNoRead("Latest user");
  reviewMessage(agent()); checkNoRead("Latest Agent");
  reviewMessage(agent({ createdAt: undefined })); checkNoRead("Latest Agent");
  reviewMessage(agent({ createdAt: "invalid-time" })); checkNoRead("Latest Agent");
  reviewMessage(agent({ isLatest: false })); checkNoRead("Latest Agent");
  const grouped = [timelineMessage("first", "2026-09-30T14:30:00Z"), timelineMessage("same group", "2026-09-30T14:49:59.999Z"), timelineMessage("boundary", "2026-09-30T15:09:59.999Z")];
  reviewTimeline(grouped, "en-US", "UTC");
  check("twenty-minute boundary creates exactly one next separator", separatorTimes().map(time => time.dateTime), [grouped[0].createdAt, grouped[2].createdAt]);
  check("messages in a group have no repeated bubble timestamps", host.querySelectorAll("article time").length, 0);
  const range = document.createRange(); range.selectNodeContents(separatorTimes()[0]);
  const textRect = range.getBoundingClientRect(); const listRect = separatorTimes()[0].parentElement!.getBoundingClientRect();
  check("time separator text is centered in conversation", Math.abs(textRect.left + textRect.width / 2 - listRect.left - listRect.width / 2) < 2, true);
  const historyBefore = separatorTimes().map(time => ({ instant: time.dateTime, label: time.textContent }));
  reviewMessage(user()); reviewTimeline(grouped, "en-US", "UTC");
  check("history replay preserves grouping and displayed canonical times", separatorTimes().map(time => ({ instant: time.dateTime, label: time.textContent })), historyBefore);
  const dated = [timelineMessage("older date", "2026-09-28T12:29:00Z"), timelineMessage("yesterday", "2026-09-29T12:29:00Z"), timelineMessage("today midnight", "2026-09-30T00:00:00Z"), timelineMessage("today noon", "2026-09-30T12:00:00Z"), timelineMessage("today afternoon", "2026-09-30T12:29:00Z")];
  reviewTimeline(dated, "en-US", "UTC");
  check("English date clock and period order includes noon and midnight", separatorTimes().map(time => time.textContent), ["Sep 28, 2026 12:29 PM", "Yesterday 12:29 PM", "Today 12:00 AM", "Today 12:00 PM", "Today 12:29 PM"]);
  reviewTimeline(dated, "zh-CN", "UTC");
  check("Chinese date clock and period order includes noon and midnight", separatorTimes().map(time => time.textContent), ["2026年9月28日 12:29 下午", "昨天 12:29 下午", "今天 12:00 上午", "今天 12:00 下午", "今天 12:29 下午"]);
  reviewTimeline([dated.at(-1)!], "en-GB", "UTC");
  check("English day period remains uppercase across browser language variants", separatorTimes()[0].textContent, "Today 12:29 PM");
  const midnight = [timelineMessage("before local midnight", "2026-10-01T06:59:30Z"), timelineMessage("after local midnight", "2026-10-01T07:00:30Z")];
  reviewTimeline(midnight, undefined, undefined, "2026-10-01T07:30:00Z");
  check("continuous messages keep one time group across local midnight", separatorTimes().map(time => time.dateTime), [midnight[0].createdAt]);
  const browserParts = new Intl.DateTimeFormat(undefined, { hour: "numeric", minute: "2-digit", hourCycle: "h12" }).formatToParts(new Date(midnight[0].createdAt!));
  const part = (type: Intl.DateTimeFormatPartTypes) => browserParts.find(item => item.type === type)!.value;
  const browserEnglish = /^en(?:-|$)/.test(new Intl.DateTimeFormat().resolvedOptions().locale);
  const period = browserEnglish ? part("dayPeriod").toLocaleUpperCase() : part("dayPeriod");
  check("default separator uses browser local clock and ends with localized day period", separatorTimes()[0].textContent!.endsWith(`${part("hour")}:${part("minute")} ${period}`), true);
  reviewTimeline([user({ createdAt: "2026-09-28T14:31:00Z", loopInputFact: inputFact() }), agent()], "en-GB", "UTC");
  check("separator message time does not replace matching Read fact time", message("Latest user").parentElement!.querySelector("time")?.dateTime, receivedAt);
  check("Read keeps its independent clock format", message("Latest user").parentElement!.querySelector("time")?.textContent, "14:32");
  check("Agent has no bubble timestamp when grouped", message("Latest Agent").querySelector("time"), null);
  render(<FixtureHarness key="initial" />); await settle();
  checkNoRead("Older user"); checkNoRead("Older Agent");
  const latestAgent = message("Latest Agent");
  check("Agent never displays Read", latestAgent.textContent!.includes("Read"), false);
  check("latest Agent has no bubble timestamp", latestAgent.querySelector("time"), null);
  check("whole Agent reply appears immediately", latestAgent.textContent!.includes("This whole reply is a single published message."), true);
  check("reply Session reference is name-only even while running", button(running.title, latestAgent.parentElement!).querySelector("svg"), null);
  check("running entry contains only its title text", button(running.title).textContent?.trim(), running.title);
  check("completed entry contains only its title", button(completed.title).textContent?.trim(), completed.title);
  check("completed entry has no Loader", button(completed.title).querySelector(".lucide-loader"), null);
  const referenceBox = button(running.title).getBoundingClientRect();
  const bodyText = [...latestAgent.querySelectorAll("p")].find(item => item.textContent!.includes("single published message"))!;
  check("reference sits below complete reply", referenceBox.top >= bodyText.getBoundingClientRect().bottom, true);
  check("reference is outside reply bubble", button(running.title).closest("article"), null);
  check("reference sits below reply border", referenceBox.top > latestAgent.getBoundingClientRect().bottom, true);
  check("attachment aligns with reply left edge", Math.abs(referenceBox.left - latestAgent.getBoundingClientRect().left) < 1, true);
  check("name-only reference has no status animation", button(running.title).getAnimations({ subtree: true }).length, 0);
  reviewMessage(agent({ sessions: [{ ...running, runState: "completed" }, completed] }));
  check("explicit completed fact removes running Loader", button(running.title).querySelector(".lucide-loader"), null);
  check("completed update retains title without state text", button(running.title).textContent?.trim(), running.title);
  render(<FixtureHarness key="new-input" scenario="new-input" />); await settle();
  checkNoRead("Latest user"); checkNoRead("New user");
  render(<FixtureHarness key="interaction" />); await settle(); calls.length = 0;
  const opener = button(running.title); opener.focus();
  check("reference is a native keyboard button", opener instanceof HTMLButtonElement, true);
  check("reference has visible keyboard focus", getComputedStyle(opener).outlineStyle !== "none", true);
  opener.click(); await settle();
  check("entry emits preview only", calls, [{ action: "preview", value: running.sessionId }]);
  check("preview retains its complete selected title", panel().querySelector(".agentChatPreviewTitle")?.textContent, running.title);
  check("preview directly shows its conversation", panel().textContent!.includes("Synthetic complete conversation"), true);
  check("selected Session has no overview tabs", panel().querySelector('[role="tablist"]'), null);
  check("selected Session has no Open chat step", [...panel().querySelectorAll("button")].some(b => b.textContent === labels.openChat), false);
  check("preview remains complementary at every viewport width", panel().getAttribute("role"), "complementary");
  check("preview has no modal semantics", panel().getAttribute("aria-modal"), null);
  check("preview fits viewport", panel().getBoundingClientRect().right <= innerWidth + 1 && panel().getBoundingClientRect().left >= -1, true);
  check("only matching latest user retains a bubble time after preview", host.querySelectorAll(".agentChatMessageFooter time").length, 1);
  const fileButton = [...panel().querySelectorAll<HTMLButtonElement>("button")].find(item => item.textContent?.includes(libraryAttachment.displayName))!;
  fileButton.focus(); fileButton.click(); await settle();
  check("Library identity passed through unchanged", calls.find(call => call.action === "library")?.value === libraryReference, true);
  check("file preview keeps its complete selected title", panel().querySelector(".agentChatPreviewTitle")?.textContent, libraryAttachment.displayName);
  button(labels.returnToSession).click(); await settle();
  check("Library return restores output trigger focus", document.activeElement === fileButton, true);
  check("file return restores the conversation", panel().textContent!.includes("Synthetic complete conversation"), true);
  check("file preview never opens another chat", calls.some(call => call.action === "open-chat"), false);
  button(labels.close).click(); await settle();
  check("close removes preview", host.querySelector('[aria-label="Session preview"]'), null);
  check("close restores reference focus", document.activeElement === opener, true);
  const completedOpener = button(completed.title); completedOpener.focus(); completedOpener.click(); await settle();
  check("second reference has its own selected title", panel().querySelector(".agentChatPreviewTitle")?.textContent === completed.title, true);
  button("Research Agent", panel()).click(); await settle();
  check("breadcrumb return removes preview", host.querySelector('[aria-label="Session preview"]'), null);
  check("breadcrumb return restores correct opener", document.activeElement === completedOpener, true);
  if (matchMedia("(max-width: 760px)").matches) {
    opener.focus(); opener.click(); await settle();
    const last = panel().querySelector<HTMLElement>(".agentChatConversationPreview")!;
    last.focus(); const forward = key(last, "Tab");
    check("narrow preview leaves native Tab navigation available", forward.defaultPrevented, false);
    check("narrow preview keeps conversation interactive", message("Latest user").closest("[inert]"), null);
    button(completed.title).focus();
    check("narrow preview allows focusing another conversation reference", document.activeElement === button(completed.title), true);
    button(labels.close, panel()).focus();
    key(document.activeElement as HTMLElement, "Escape"); await settle();
    check("Escape restores input entry focus", document.activeElement === opener, true);
    check("closing restores background interaction", message("Latest user").closest("[inert]"), null);
  }
  {
    render(<FixtureHarness key="direct-preview-switch" />); await settle();
    const firstReference = button(running.title);
    const secondReference = button(completed.title);
    const openCallsBefore = calls.filter(call => call.action === "open-chat").length;
    firstReference.focus(); firstReference.click(); await settle();
    secondReference.focus(); secondReference.click(); await settle();
    check("direct preview switch selects second session", panel().querySelector(".agentChatPreviewTitle")?.textContent === completed.title, true);
    check("direct preview switch emits second identity", calls.at(-1), { action: "preview", value: completed.sessionId });
    button(labels.close, panel()).click(); await settle();
    check("closing directly switched preview restores second reference focus", document.activeElement === secondReference, true);
    check("direct preview switch never opens chat", calls.filter(call => call.action === "open-chat").length, openCallsBefore);
  }
  for (const state of [{ status: "loading" }, { status: "error", error: "Synthetic preview unavailable" }] as const) {
    render(<FixtureHarness key={state.status} load={state} />); await settle();
    button(running.title).focus(); button(running.title).click(); await settle();
    check(`${state.status}: independent preview load state visible`, panel().textContent!.includes(state.status === "loading" ? labels.loading : state.error), true);
    if (state.status === "error") {
      check("preview error is announced", panel().querySelector('[role="alert"]')?.textContent!.includes(state.error), true);
      button(labels.retry).click(); await settle(); check("Retry emits callback only", calls.at(-1), { action: "retry", value: running.sessionId });
    }
    button(labels.close).click(); await settle();
  }
  reviewMessage(agent({ text: `A long complete reply with an unbroken word: ${"source".repeat(200)}\n\n\`\`\`text\n${"code".repeat(200)}\n\`\`\``, sessions: [longSession] })); await settle();
  check("long title is available to keyboard users", button(longSession.title).getAttribute("title"), longSession.title);
  check("long content does not widen the page", document.documentElement.scrollWidth <= innerWidth + 1, true);
  const beforeNormal = document.createElement("div"); beforeNormal.className = "workspaceUserMessage"; beforeNormal.textContent = "Ordinary Session sentinel"; host.append(beforeNormal);
  check("ordinary Session retains its existing border", getComputedStyle(beforeNormal).borderTopWidth, "0px");
  const swatch = document.createElement("span"); swatch.style.color = "var(--on-surface)"; host.append(swatch);
  check("message inherits active theme text", getComputedStyle(message("Latest Agent")).color, getComputedStyle(swatch).color);
  check("presentation made no application requests", window.agentChatFixtureRequests, []);
  check("presentation attempted no navigation", window.agentChatFixtureNavigations, []);
  check("presentation kept current URL", location.href, hrefBefore);
  check("presentation kept stored state", JSON.stringify({ local: { ...localStorage }, session: { ...sessionStorage } }), storageBefore);
  beforeNormal.remove(); swatch.remove();
  render(<Review />); await settle();
  check("all presentation checks completed", true, true);
} catch (error) {
  check("fixture completed", error instanceof Error ? error.message : String(error), "success");
} finally {
  document.getElementById("results")!.textContent = JSON.stringify(results);
}
