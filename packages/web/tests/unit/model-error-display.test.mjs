import assert from "node:assert/strict";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createJsxSubjectLoader } from "./jsxSubject.mjs";
import { ApiError, environment, nodes, renderer, subjectLoader } from "./componentHarness.mjs";
import { localizedModelError, modelErrorText } from "../../src/modelErrors.ts";
import { createTranscriptViewStore } from "../../src/chat/transcriptViewStore.ts";

const stub = code => `data:text/javascript;base64,${Buffer.from(code).toString("base64")}`;
const loader = createJsxSubjectLoader(new Map([
  ["../i18n", stub("export const t=key=>key; export const i18n={language:'en'}; export const useTranslation=()=>({t,i18n});")],
  ["./MarkdownContent", stub('import {createElement} from "' + import.meta.resolve("react") + '"; export const MarkdownContent=({text})=>createElement("span",null,text);')],
]));
const { TranscriptBlockRow } = await import(await loader(fileURLToPath(new URL("../../src/chat/TranscriptBlockContent.tsx", import.meta.url))));

function render(kind, sourceType, reason = "model_quota_domain_required") {
  const body = { kind, content: { inlineContent: reason, sourceRef: null },
    ...(kind === "notice" ? { noticeType: "run_boundary", status: "completed" } : kind === "assistantText" ? { status: "completed" } : {}) };
  const block = { blockId: "failure", blockRevision: "1", orderKey: { sourceSequence: "1", ordinal: 0 }, body,
    presentation: sourceType ? { sourceType, agentRunId: "run", observedAtMs: 1000, displayTarget: null, durationMs: null, operation: null } : null };
  const store = createTranscriptViewStore();
  store.openTail({ schema: "transcript.page.v1", sessionId: "session", projectionVersion: "transcript.projection.v1", projectionGeneration: "generation",
    sourceHighWater: "1", blocks: [block], olderCursor: null, hasOlder: false, resumeCursors: [{ streamId: "workspace-transcript.v1", cursor: "1" }] });
  return renderToStaticMarkup(createElement(TranscriptBlockRow, { store, blockId: "failure" }));
}

test("quota configuration failure is translated in trusted failed run notices", () => {
  assert.ok(render("notice", "agent_run_failed").includes("modelErrors.quotaRequired"));
});

test("failed and interrupted runs render a plain error strip with the original code", () => {
  for (const source of ["agent_run_failed", "agent_run_interrupted"]) {
    const html = render("notice", source, "provider_rate_limited");
    assert.match(html, /workspaceRunError/);
    assert.match(html, /role="status"/);
    assert.match(html, /aria-hidden="true"/);
    assert.match(html, /modelErrors.rateLimited · provider_rate_limited/);
    assert.doesNotMatch(html, /workspaceStageSummary|workspaceTerminalAnswer|<details|<button/);
  }
});

test("unclassified failures retain their code instead of the generic retry instruction", () => {
  for (const reason of ["model_adapter_failed", "provider_response_invalid", "prepared_prompt_new_validation_failure"]) {
    const html = render("notice", "agent_run_failed", reason);
    assert.match(html, /modelErrors.failureLabel/);
    assert.ok(html.includes(reason));
    assert.ok(!html.includes("modelErrors.failed"));
  }
  const raw = render("notice", "agent_run_failed", "exceeded retry limit, last status: 429 Too Many Requests");
  assert.ok(raw.includes("exceeded retry limit, last status: 429 Too Many Requests"));
});

test("ordinary content never acquires system error strip semantics", () => {
  for (const kind of ["userText", "assistantText"]) {
    assert.doesNotMatch(render(kind, "agent_run_failed", "provider_rate_limited"), /workspaceRunError/);
  }
  assert.doesNotMatch(render("notice", null), /workspaceRunError/);
});
test("failed delivery explains whether the Agent omitted its reply or exhausted context", () => {
  const t = key => key;
  for (const [reason, key] of [
    ["completion_tool_delivery_required", "modelErrors.replyNotDelivered"],
    ["completion_delivery_repair_prompt_too_large", "modelErrors.replyContextLimit"],
  ]) {
    assert.equal(localizedModelError(reason, t), key);
    assert.ok(render("notice", "agent_run_failed", reason).includes(key));
    assert.ok(render("assistantText", "agent_run_failed", reason).includes(reason));
  }
});
test("user, assistant and ordinary notice content is never rewritten as an error code", () => {
  for (const [kind, source] of [["userText", "agent_run_failed"], ["assistantText", "agent_run_failed"], ["notice", null]]) {
    assert.ok(render(kind, source).includes("model_quota_domain_required"));
  }
});

test("configuration reasons have useful product copy and unknown hosted codes have a translated fallback", () => {
  const t = key => key;
  assert.equal(localizedModelError("provider_request_rejected", t), "modelErrors.requestRejected");
  assert.equal(localizedModelError("model_thinking_mode_unsupported", t), "modelErrors.thinkingUnsupported");
  assert.equal(localizedModelError("prepared_prompt_new_validation_failure", t), "modelErrors.failed");
  const error = new ApiError("model_run_failed", 502);
  error.payload = { reasonType: "model_quota_domain_required" };
  assert.equal(modelErrorText(error, t), "modelErrors.quotaRequired");
});

test("model settings displays the translated quota error from the API", async () => {
  const { default: ModelSettings } = await import(await subjectLoader()(fileURLToPath(new URL("../../src/routes/ModelSettings.jsx", import.meta.url))));
  const error = new ApiError("model_quota_domain_required", 400);
  const subject = renderer(ModelSettings, { onClose() {}, onModelsChanged() {} }, environment({ request: async () => { throw error; } }));
  await subject.settle();
  assert.ok(nodes(subject.tree, node => node.props.children === "modelErrors.quotaRequired").length);
  subject.unmount();
});
