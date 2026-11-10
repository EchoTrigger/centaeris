import assert from "node:assert/strict";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createJsxSubjectLoader } from "./jsxSubject.mjs";
import { createTranscriptViewStore } from "../../src/chat/transcriptViewStore.ts";

const load = createJsxSubjectLoader(new Map());
const { TranscriptToolGroupCard } = await import(await load(fileURLToPath(new URL("../../src/chat/TranscriptBlockContent.tsx", import.meta.url))));
const block = status => ({ blockId: "tool", blockRevision: "1", orderKey: { sourceSequence: "3", ordinal: 0 },
  body: { kind: "tool", callId: "call", toolName: "bash", status,
    summary: "Probe", summaryRef: null, outputRef: null } });
function render(status, runEnded) {
  const store = createTranscriptViewStore();
  store.openTail({ schema: "transcript.page.v1", sessionId: "session", projectionVersion: "transcript.projection.v1",
    projectionGeneration: "generation", sourceHighWater: "3", blocks: [block(status)], hasOlder: false, olderCursor: null,
    resumeCursors: [{ streamId: "workspace-transcript.v1", cursor: "3" }] });
  return renderToStaticMarkup(createElement(TranscriptToolGroupCard, { store, blockIds: ["tool"], runEnded }));
}
test("a terminal Run displays unresolved tool results without claiming they are still running", () => {
  const markup = render("running", true);
  assert.match(markup, /结果未确认|results? unconfirmed|transcriptTool\.resultsUnconfirmed/i);
  assert.doesNotMatch(markup, /正在运行|Running|toolGroupTitle\.running/i);
});
test("an active Run keeps ordinary running progress", () => {
  const markup = render("running", false);
  assert.doesNotMatch(markup, /结果未确认|results? unconfirmed|transcriptTool\.resultsUnconfirmed/i);
});
test("a completed receipt keeps its original completion presentation after Run termination", () => {
  assert.equal(render("completed", true), render("completed", false));
});
