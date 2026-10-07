import { createRoot } from "react-dom/client";
import { AgentMessageList } from "../../src/agent-chat/AgentMessageList";
import type { AgentMessageView } from "../../src/agent-chat/agentChatViewTypes";
import { i18n, t } from "../../src/i18n";
import "../../src/agent-chat/agent-chat.css";

// Exercise the production message renderer with synthetic presentation facts.
// No route, API, input submission or model request participates in this fixture.
const query = new URLSearchParams(location.search);
const locale = query.get("locale") || "en-US";
await i18n.changeLanguage(locale);
document.documentElement.dataset.theme = query.get("theme") || "light";
document.head.append(document.querySelector<HTMLLinkElement>('link[rel="stylesheet"][href="/src/globals.css"]')!);
const user = { role: "user" as const, messageId: "synthetic-input", authorLabel: "You", text: "Hi", isLatest: true };
const fact = { kind: "loopInput" as const, messageId: user.messageId, receivedAt: "2026-10-04T21:45:00.000Z" };
const scenarios: readonly [string, AgentMessageView][] = [
  ["unread", user],
  ["read", { ...user, loopInputFact: fact }],
  ["older", { ...user, isLatest: false, loopInputFact: fact }],
  ["mismatched", { ...user, loopInputFact: { ...fact, messageId: "another-input" } }],
  ["content-free", { ...user, text: "", loopInputFact: fact }],
  ["long", { ...user, text: "A message with a long word.\n" + "long-word".repeat(50), loopInputFact: fact }],
  ["reference-only", { role: "agent", messageId: "synthetic-reference", authorLabel: "Agent", text: "", isLatest: true, sessions: [{ sessionId: "synthetic-work", title: "Synthetic Session without reply text", runState: "unknown" }] }],
];
createRoot(document.getElementById("fixture")!).render(<>{scenarios.map(([id, message]) => <section id={id} key={id} style={{ marginBlockEnd: 24 }}>
  <AgentMessageList messages={[message]} referenceTime="2026-10-04T21:46:00.000Z" readLabel={t("agentChat.read")} locale={locale} timeZone="UTC" onPreviewSession={() => {}} />
</section>)}</>);
await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)));
await document.fonts.ready;
const results: { name: string; actual: unknown; expected: unknown }[] = [];
const check = (name: string, actual: unknown, expected: unknown) => results.push({ name, actual, expected });
const read = document.querySelector("#read article")!;
const receipt = document.querySelector<HTMLElement>("#read .agentChatMessageTime")!;
const readBounds = read.getBoundingClientRect();
const receiptBounds = receipt.getBoundingClientRect();
check("Read is outside the user's bordered reply", Boolean(receipt.closest("article")), false);
check("Read sits below the user's reply", receiptBounds.top > readBounds.bottom, true);
check("Read shares the right edge of the user's reply", Math.abs(receiptBounds.right - readBounds.right) < 1, true);
check("Read has the compact message-group gap", Math.abs(receiptBounds.top - readBounds.bottom - 8) < 1, true);
check("Read does not enlarge a short user's bubble", Math.abs(readBounds.width - document.querySelector("#unread article")!.getBoundingClientRect().width) < 1, true);
check("message body uses Session's fourteen-pixel type", getComputedStyle(read).fontSize, "14px");
check("Read uses Session's twelve-pixel metadata type", getComputedStyle(receipt).fontSize, "12px");
check("Read keeps the localized label", receipt.textContent?.startsWith(t("agentChat.read")), true);
check("Read keeps its authoritative receivedAt", receipt.querySelector("time")!.getAttribute("datetime"), fact.receivedAt);
check("Read retains the accessible full receipt instant", receipt.querySelector("time")!.getAttribute("aria-label"), receipt.querySelector("time")!.getAttribute("title"));
for (const id of ["unread", "older", "mismatched"]) check(`${id} does not invent a Read receipt`, Boolean(document.querySelector(`#${id} .agentChatMessageTime`)), false);
check("a receipt with no visible body still stays outside its bubble", Boolean(document.querySelector("#content-free .agentChatMessageTime")?.closest("article")), false);
check("a reference-only reply retains its Session attachment", document.querySelector("#reference-only .agentChatSessionReference")?.textContent, "Synthetic Session without reply text");
check("a reference-only reply does not manufacture user Read", Boolean(document.querySelector("#reference-only .agentChatMessageTime")), false);
check("long user text stays inside the available message width", document.querySelector("#long article")!.getBoundingClientRect().width <= document.getElementById("long")!.getBoundingClientRect().width, true);
check("message presentation does not introduce horizontal document scrolling", document.documentElement.scrollWidth <= innerWidth, true);
check("all Agent message presentation checks completed", true, true);
document.getElementById("results")!.textContent = JSON.stringify(results);
