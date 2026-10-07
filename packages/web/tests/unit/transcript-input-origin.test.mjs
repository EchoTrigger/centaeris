import assert from "node:assert/strict";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createJsxSubjectLoader } from "./jsxSubject.mjs";
import { createTranscriptViewStore } from "../../src/chat/transcriptViewStore.ts";

const stub = code => `data:text/javascript;base64,${Buffer.from(code).toString("base64")}`;
const loader = createJsxSubjectLoader(new Map([
  ["../i18n", stub('export const useTranslation=()=>({t:(key, values)=>key==="transcript.sendByAgent"?`Send By ${values.agentName}`:key});')],
  ["./useTranscriptCitations", stub('export const useTranscriptCitations=()=>({citations:new Map(),error:false});')],
  ["./useTranscriptTurnMetadata", stub('export const useTranscriptTurnMetadata=()=>new Map();')],
]));
const { TranscriptBlockList } = await import(await loader(fileURLToPath(new URL("../../src/chat/TranscriptBlockList.tsx", import.meta.url))));
const origin = { messageId: "initial-work-input", agentId: "source-agent", agentName: "Synthetic Agent" };
const blocks = [
  ["initial-work-input", "userText", "Delegated objective"],
  ["first-answer", "assistantText", "First answer"],
  ["human-follow-up", "userText", "A later manual input"],
  ["second-answer", "assistantText", "Second answer"],
].map(([blockId, kind, text], i) => ({ blockId, blockRevision: "1", orderKey: { sourceSequence: String(i + 1), ordinal: 0 },
  body: { kind, content: { inlineContent: text, sourceRef: null }, ...(kind === "assistantText" ? { status: "completed" } : {}) } }));
function render(initialInputOrigin = null) {
  const store = createTranscriptViewStore();
  store.openTail({ schema: "transcript.page.v1", sessionId: "work-session", projectionVersion: "transcript.projection.v1", projectionGeneration: "generation", sourceHighWater: "4", blocks, olderCursor: null, hasOlder: false, resumeCursors: [{ streamId: "workspace-transcript.v1", cursor: "4" }] });
  return renderToStaticMarkup(createElement(TranscriptBlockList, { store, sessionId: "work-session", loadingHistory: false, loadingOlderHistory: false, onLoadOlderHistory: async () => {}, initialInputOrigin }));
}
test("only the authoritative initial user message shows Send By AgentName outside the bubble", () => {
  const markup = render(origin);
  assert.equal((markup.match(/Send By Synthetic Agent/g) ?? []).length, 1);
  assert.match(markup, /data-block-id="initial-work-input"[^>]*><span class="workspaceUserMessageOrigin">Send By Synthetic Agent<\/span><div class="workspaceUserMessage">Delegated objective<\/div>/);
  assert.doesNotMatch(markup, /data-block-id="human-follow-up"[^>]*><span/);
});
test("manual Sessions, missing initial messages and assistant IDs never manufacture an Agent sender", () => {
  for (const value of [null, { ...origin, messageId: "unloaded-input" }, { ...origin, messageId: "first-answer" }]) assert.doesNotMatch(render(value), /Send By/);
});
